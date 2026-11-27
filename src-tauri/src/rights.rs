//! Administrator rights: whether the app runs with them, when it asks for them, and how it
//! starts itself again with them.
//!
//! The game's own program asks for administrator rights, and its launcher may. A program
//! that starts either without having them itself is met by Windows' prompt, and a launcher
//! that runs above this app takes no messages from it. That is no trouble while the user is
//! at the PC. It is one when the app starts the game by itself after a maintenance: nobody
//! is there to answer the prompt. An app that runs with administrator rights already hands
//! them on to what it starts, and there is no prompt at that moment.
//!
//! So the app asks for them while the user is there: when 패치 후 게임 자동 실행 is switched
//! on, and as it starts with that set. It asks by Windows' own prompt and in no other way,
//! and only after the user has said yes in the app or has the setting on. With them it is
//! another process: the app starts itself again and leaves, and the copy that replaces it
//! waits for it to be gone before it becomes the app (see [`Handover`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, State};

use crate::flow::Remembered;
use crate::log::log_line;
use crate::state::Patcher;

/// Where the app stands with administrator rights, and with what starting itself again
/// would cut off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Standing {
    /// It runs with them already: a PC whose every program does, or a copy that was started
    /// for them.
    pub(crate) elevated: bool,
    /// It asked Windows for them in this session and did not get them: the prompt was
    /// declined, or this copy is the one that was started for them and still has none.
    pub(crate) declined: bool,
    /// A run is at work, or stopping.
    pub(crate) run_at_work: bool,
    /// A folder is being put back after an apply that was cut off in it.
    pub(crate) putting_back: bool,
    /// The user is being asked something they have not answered yet.
    pub(crate) question_open: bool,
}

impl Standing {
    /// Whether the app may start itself again with administrator rights now: it has none,
    /// has not been turned down for them in this session, and leaving cuts nothing off.
    fn may_restart(self) -> bool {
        !(self.elevated || self.declined || self.run_at_work || self.putting_back || self.question_open)
    }

    /// Whether the user is asked if the app is to start again with administrator rights:
    /// 패치 후 게임 자동 실행 has gone from `was_set` to `set`. They are asked as they switch
    /// it on, where saying yes can be acted on at once. Nothing is asked of a setting that
    /// was on already, or of one that is switched off.
    pub(crate) fn asks(self, was_set: bool, set: bool) -> bool {
        !was_set && set && self.may_restart()
    }

    /// Whether the app starts itself again with administrator rights: as it starts, and
    /// when the user has said yes to being asked. Only with 패치 후 게임 자동 실행 on
    /// (`set`): it is what the rights are for.
    pub(crate) fn restarts(self, set: bool) -> bool {
        set && self.may_restart()
    }
}

/// What the copy of the app that replaces another is told by it, on its command line.
///
/// The one that leaves and the one that takes over are never the app at the same time. The
/// new one is told which process it replaces, and waits for that process to be gone before
/// it does anything: before it claims to be the one copy there is, before it reads a file
/// or opens a window. It is told where the data is as well, by its whole path: started with
/// another account's rights, it would look for the usual folder in another profile.
///
/// To the user it is the same session, so the new copy is also told what the old one
/// remembered of it and nothing on this PC says; see [`Remembered`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Handover {
    /// The process this copy replaces.
    pub(crate) replaces: u32,
    /// The data folder that process kept, which is this copy's from here on.
    pub(crate) data: PathBuf,
    /// That process ran beside the usual copy of the app, with a data folder that was
    /// named for it (see [`crate::harness`]): so does this one.
    pub(crate) beside: bool,
    /// What the site of each client remembered of the session, by what the client goes by
    /// (`live`). Only the sites that remembered anything are in it.
    pub(crate) remembered: BTreeMap<String, Remembered>,
}

/// What the things a copy is told are written as. A game that is due names its client
/// (`--game-due=live`), and a patch that is the user's to start names its client and
/// itself (`--hands-off=live=1.2.422`).
const REPLACES: &str = "--replaces=";
const DATA: &str = "--data=";
const BESIDE: &str = "--beside";
const GAME_DUE: &str = "--game-due=";
const HANDS_OFF: &str = "--hands-off=";

impl Handover {
    /// What this copy of the app hands over to the one that replaces it: itself as the
    /// process to wait for, its data folder, and what its sites remember of the session.
    fn of(patcher: &Patcher, beside: bool) -> Handover {
        let remembered = patcher.sites.iter().filter_map(|site| {
            let remembered = site.flow.lock().unwrap().remembered();
            (remembered != Remembered::default()).then(|| (site.client.server.key().to_owned(), remembered))
        });
        Handover {
            replaces: std::process::id(),
            data: std::path::absolute(&patcher.data).unwrap_or_else(|_| patcher.data.clone()),
            beside,
            remembered: remembered.collect(),
        }
    }

    /// What `arguments`, those a copy of the app was started with, hand over to it.
    /// Nothing, unless they say both which process it replaces and where the data is.
    pub(crate) fn told(arguments: &[String]) -> Option<Handover> {
        let said = |what: &str| arguments.iter().find_map(|argument| argument.strip_prefix(what));
        let mut remembered: BTreeMap<String, Remembered> = BTreeMap::new();
        for argument in arguments {
            if let Some(client) = argument.strip_prefix(GAME_DUE) {
                remembered.entry(client.to_owned()).or_default().game_due = true;
            } else if let Some((client, patch)) = argument.strip_prefix(HANDS_OFF).and_then(|of| of.split_once('=')) {
                remembered.entry(client.to_owned()).or_default().hands_off.push(patch.to_owned());
            }
        }
        Some(Handover {
            replaces: said(REPLACES)?.parse().ok()?,
            data: said(DATA).filter(|data| !data.is_empty()).map(PathBuf::from)?,
            beside: arguments.iter().any(|argument| argument == BESIDE),
            remembered,
        })
    }

    /// The arguments the copy that replaces this one is started with: those this one was
    /// started with (`own`), and after them what is handed over. What an earlier handover
    /// left among `own` is not handed on a second time.
    pub(crate) fn arguments(&self, own: &[String]) -> Vec<String> {
        let handed = |argument: &&String| {
            [REPLACES, DATA, GAME_DUE, HANDS_OFF].iter().any(|told| argument.starts_with(told)) || *argument == BESIDE
        };
        let mut arguments: Vec<String> = own.iter().filter(|argument| !handed(argument)).cloned().collect();
        arguments.push(format!("{REPLACES}{}", self.replaces));
        arguments.push(format!("{DATA}{}", self.data.display()));
        arguments.extend(self.beside.then(|| BESIDE.to_owned()));
        for (client, remembered) in &self.remembered {
            arguments.extend(remembered.game_due.then(|| format!("{GAME_DUE}{client}")));
            arguments.extend(remembered.hands_off.iter().map(|patch| format!("{HANDS_OFF}{client}={patch}")));
        }
        arguments
    }

    /// Has the sites of this copy take up what the copy it replaces remembered of the
    /// session. What is said of a client this copy does not look after is let go.
    pub(crate) fn taken_up(&self, patcher: &Patcher) {
        for (client, remembered) in &self.remembered {
            if let Some(site) = patcher.site_named(client) {
                site.flow.lock().unwrap().remember(remembered.clone());
            }
        }
    }
}

/// `arguments` as one line, written so that a Windows program takes it apart into the same
/// arguments again: one with a space or a quote in it stands in quotes, and a backslash
/// counts double only where a quote follows it.
pub(crate) fn command_line(arguments: &[String]) -> String {
    let quoted = |argument: &String| {
        let plain = !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\u{b}', '"']);
        if plain {
            return argument.clone();
        }
        let mut quoted = String::from('"');
        let mut backslashes = 0;
        for letter in argument.chars() {
            match letter {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.push_str(&"\\".repeat(2 * backslashes + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                other => {
                    quoted.push_str(&"\\".repeat(backslashes));
                    quoted.push(other);
                    backslashes = 0;
                }
            }
        }
        quoted.push_str(&"\\".repeat(2 * backslashes));
        quoted.push('"');
        quoted
    };
    arguments.iter().map(quoted).collect::<Vec<_>>().join(" ")
}

/// How long the copy that takes over waits for the one it replaces to be gone. That one
/// leaves the moment it has started this one, so this is for a PC that is slow about it.
const WAITS_FOR: Duration = Duration::from_secs(30);

/// This copy runs beside the usual one, with a data folder that was named for it. Said
/// once, as the app starts.
static BESIDE_THE_USUAL: AtomicBool = AtomicBool::new(false);

/// Takes in how this copy of the app was started: `handed` is what the copy it replaces
/// told it, if it replaces one, and `beside` says that it runs beside the usual copy.
///
/// A copy that replaces another waits here for that one to be gone; see [`Handover`].
pub(crate) fn take_over(handed: Option<&Handover>, beside: bool) {
    BESIDE_THE_USUAL.store(beside, Ordering::SeqCst);
    if let Some(handed) = handed {
        wait_for(handed.replaces, WAITS_FOR);
    }
}

/// Starts the app again with administrator rights and says whether the copy that takes
/// over was started: whoever asks then leaves at once, and the new copy waits for that.
///
/// Nothing is started unless the app may (see [`Standing::restarts`]): Windows' prompt is
/// then all the user sees. While the prompt is open nothing is begun that leaving would cut
/// off; see [`Patcher::restarting`]. A user who says no to the prompt is not asked again in
/// this session.
///
/// What is handed over is read once that is said. Whoever begins something looks the other
/// way round, taking its turn and then looking whether the app is leaving: so a game that
/// is due is started by this copy or by the one that takes over, and never by both.
pub(crate) fn start_again_with_rights(patcher: &Patcher) -> bool {
    patcher.restarting.store(true, Ordering::SeqCst);
    let set = patcher.settings.lock().unwrap().auto_start;
    let may = patcher.standing(elevated()).restarts(set);
    let handover = Handover::of(patcher, BESIDE_THE_USUAL.load(Ordering::SeqCst));
    let own: Vec<String> = std::env::args().skip(1).collect();
    let started = may && start_elevated(&command_line(&handover.arguments(&own)));
    if started {
        log_line("starting again with administrator rights");
    } else {
        patcher.restarting.store(false, Ordering::SeqCst);
    }
    if may && !started {
        patcher.rights_declined.store(true, Ordering::SeqCst);
        log_line("administrator rights were not given: going on without them");
    }
    started
}

/// Starts the app again with administrator rights: the user has said yes to being asked.
/// The app leaves when the copy that takes over has been started. Where it has not (they
/// said no to Windows' prompt, or something has come up that leaving would cut off) the
/// app goes on as it is, and nothing is said.
#[tauri::command(async)]
pub(crate) fn restart_with_rights(app: AppHandle, patcher: State<Patcher>) {
    if start_again_with_rights(&patcher) {
        app.exit(0);
    }
}

/// Whether this process runs with administrator rights: its token is an elevated one.
pub(crate) fn elevated() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut token = std::ptr::null_mut();
        // SAFETY: the call writes one handle, to a variable that outlives it.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let (size, mut written) = (std::mem::size_of::<TOKEN_ELEVATION>() as u32, 0);
        // SAFETY: the buffer is the structure the call fills in, as long as it is told.
        let read =
            unsafe { GetTokenInformation(token, TokenElevation, (&raw mut elevation).cast(), size, &mut written) };
        // SAFETY: the handle was opened above, and is closed once.
        unsafe { CloseHandle(token) };
        read != 0 && elevation.TokenIsElevated != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Starts this program again with administrator rights and the command line `arguments`.
/// Windows asks the user first. Says whether it was started: not when they said no, and
/// not when Windows could not start it, of which nothing is shown either.
fn start_elevated(arguments: &str) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        use crate::wide::to_wide;
        let Ok(program) = std::env::current_exe() else { return false };
        let (program, arguments, verb) = (to_wide(program), to_wide(arguments), to_wide("runas"));
        // SAFETY: the structure is plain data, and every bit pattern of it is valid.
        let mut asked: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        asked.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        // The call returns when the program has been started, and shows nothing of its own
        // when it has not.
        asked.fMask = SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        (asked.lpVerb, asked.lpFile, asked.lpParameters) = (verb.as_ptr(), program.as_ptr(), arguments.as_ptr());
        asked.nShow = SW_SHOWNORMAL;
        // SAFETY: every pointer is to a NUL-terminated buffer that outlives the call.
        unsafe { ShellExecuteExW(&mut asked) != 0 }
    }
    #[cfg(not(windows))]
    {
        let _ = arguments;
        false
    }
}

/// Waits for the process `process` to be gone, `at_most` so long. A process that is gone
/// already, or cannot be waited for, is not waited for.
fn wait_for(process: u32, at_most: Duration) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
        // SAFETY: the call takes a number and hands back a handle or none.
        let open = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, process) };
        if open.is_null() {
            return;
        }
        // SAFETY: the handle is open until it is closed below, once.
        unsafe {
            WaitForSingleObject(open, at_most.as_millis().min(u128::from(u32::MAX - 1)) as u32);
            CloseHandle(open);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (process, at_most);
    }
}

/// What a copy of the app that runs with administrator rights listens on for being asked
/// to show its window, in the session of the user who is logged on.
#[cfg(windows)]
const SHOW: &str = "Local\\com.maplefastpatch.app-show";

/// Has `show` called whenever the app is started again while this copy, which runs with
/// administrator rights, is open: the window is then brought back, as a second start
/// always did.
///
/// Windows lets a program without those rights neither see that this copy is the app nor
/// send its window a message, which is how a second start otherwise reaches the first. So
/// this copy keeps a signal that a program of the user who is logged on may raise, and
/// nothing else: raising it shows the window, and that is all it does.
pub(crate) fn listen_for_starts(show: impl Fn() + Send + 'static) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};

        use crate::wide::to_wide;
        // Administrators and Windows itself may do anything with it. Whoever is logged on
        // at this PC's screen may raise it and wait for it, and no more.
        let who = to_wide("D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;0x100002;;;IU)");
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: the text is NUL-terminated, and the call writes one pointer.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                who.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name = to_wide(SHOW);
        // SAFETY: the name is NUL-terminated and the attributes outlive the call. Without
        // a descriptor the signal is made as any is, and only a copy with these rights can
        // raise it.
        let signal = unsafe { CreateEventW(&attributes, 0, 0, name.as_ptr()) };
        // SAFETY: the descriptor was made above, or is null, and is freed once.
        unsafe { LocalFree(descriptor) };
        if signal.is_null() {
            return;
        }
        // A handle is a number, and this one is kept for as long as the app is open.
        let signal = signal as usize;
        std::thread::spawn(move || {
            // SAFETY: the handle stays open for as long as the process lives.
            while unsafe { WaitForSingleObject(signal as _, INFINITE) } == 0 {
                show();
            }
        });
    }
    #[cfg(not(windows))]
    {
        let _ = show;
    }
}

/// Asks the copy of the app that runs with administrator rights to show its window, where
/// there is one; see [`listen_for_starts`]. Says whether there is one: whoever asks is then
/// a second start of the app, and leaves.
pub(crate) fn ask_to_show() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
        use windows_sys::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
        use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

        use crate::wide::to_wide;
        let name = to_wide(SHOW);
        // SAFETY: the name is NUL-terminated and outlives the call.
        let signal = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if signal.is_null() {
            // It is there, and not this one's to raise: still the app, and still open.
            return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
        }
        // SAFETY: the calls take a constant and a handle that is open until it is closed
        // below, once. This start was the user's doing, so it may hand on its right to
        // come to the front, which the copy that shows its window needs.
        unsafe {
            AllowSetForegroundWindow(ASFW_ANY);
            SetEvent(signal);
            CloseHandle(signal);
        }
        true
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app as it usually stands: without administrator rights, not turned down for
    /// them, and with nothing at work.
    const USUAL: Standing =
        Standing { elevated: false, declined: false, run_at_work: false, putting_back: false, question_open: false };

    /// Every way the app can stand, by a word for it.
    fn standings() -> [(&'static str, Standing, bool); 8] {
        [
            ("as usual", USUAL, true),
            // A PC whose every program has them, or the copy that was started for them.
            ("with administrator rights already", Standing { elevated: true, ..USUAL }, false),
            ("turned down in this session", Standing { declined: true, ..USUAL }, false),
            ("a run at work or stopping", Standing { run_at_work: true, ..USUAL }, false),
            ("a folder being put back", Standing { putting_back: true, ..USUAL }, false),
            ("a question open", Standing { question_open: true, ..USUAL }, false),
            ("with rights, and a run at work", Standing { elevated: true, run_at_work: true, ..USUAL }, false),
            (
                "everything at once",
                Standing { elevated: true, declined: true, run_at_work: true, putting_back: true, question_open: true },
                false,
            ),
        ]
    }

    #[test]
    fn the_user_is_asked_as_they_switch_the_game_start_on_and_only_where_a_yes_can_be_acted_on() {
        for (what, standing, may) in standings() {
            // Switched on: asked, where the app may start again.
            assert_eq!(standing.asks(false, true), may, "{what}");
            // On already, switched off, or left off: nothing to ask.
            for (was_set, set) in [(true, true), (true, false), (false, false)] {
                assert!(!standing.asks(was_set, set), "{what}: {was_set} to {set}");
            }
        }
    }

    #[test]
    fn the_app_starts_again_with_rights_only_with_the_setting_on_and_never_over_something_at_work() {
        for (what, standing, may) in standings() {
            assert_eq!(standing.restarts(true), may, "{what}");
            // Without the setting there is nothing the rights are for.
            assert!(!standing.restarts(false), "{what}");
        }
    }

    fn words(of: &[&str]) -> Vec<String> {
        of.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn the_copy_that_takes_over_is_told_which_process_it_replaces_and_where_the_data_is() {
        let usual = Handover {
            replaces: 4242,
            data: PathBuf::from(r"D:\Kept\com.maplefastpatch.app"),
            beside: false,
            remembered: BTreeMap::new(),
        };
        let told = usual.arguments(&[]);
        assert_eq!(told, words(&["--replaces=4242", r"--data=D:\Kept\com.maplefastpatch.app"]));
        assert_eq!(Handover::told(&told), Some(usual.clone()));

        // A copy that runs beside the usual one says so, and its folder may have a space in it.
        let beside = Handover {
            replaces: 7,
            data: PathBuf::from(r"D:\Maple Fast Patch\data"),
            beside: true,
            remembered: BTreeMap::new(),
        };
        let told = beside.arguments(&words(&["--verbose"]));
        assert_eq!(told, words(&["--verbose", "--replaces=7", r"--data=D:\Maple Fast Patch\data", "--beside"]));
        assert_eq!(Handover::told(&told), Some(beside));

        // What an earlier handover left among the arguments is not handed on again.
        let earlier =
            ["--replaces=1", r"--data=E:\old", "--beside", "--game-due=test", "--verbose", "--hands-off=live=1.2.3"];
        let again = usual.arguments(&words(&earlier));
        assert_eq!(again, words(&["--verbose", "--replaces=4242", r"--data=D:\Kept\com.maplefastpatch.app"]));
    }

    /// What the sites remembered of the session is told by the client it is of, and comes
    /// out of the command line as it went in, whatever stands in the name of a patch.
    #[test]
    fn the_copy_that_takes_over_is_told_what_the_session_remembered() {
        let address = "https://download.example/patch/00421to00422.patch?a=1&b=2 3";
        let live = Remembered { game_due: true, hands_off: vec!["1.2.422".to_owned(), address.to_owned()] };
        let test = Remembered { game_due: true, hands_off: Vec::new() };
        let handover = Handover {
            replaces: 4242,
            data: PathBuf::from(r"D:\data"),
            beside: false,
            remembered: BTreeMap::from([("live".to_owned(), live), ("test".to_owned(), test)]),
        };
        let told = handover.arguments(&[]);
        let expected = [
            "--replaces=4242",
            r"--data=D:\data",
            "--game-due=live",
            "--hands-off=live=1.2.422",
            "--hands-off=live=https://download.example/patch/00421to00422.patch?a=1&b=2 3",
            "--game-due=test",
        ];
        assert_eq!(told, words(&expected));
        assert_eq!(Handover::told(&told), Some(handover));
        // The one with a space in it goes in quotes, and the rest as it is.
        assert!(command_line(&told).ends_with(
            r#"1.2.422 "--hands-off=live=https://download.example/patch/00421to00422.patch?a=1&b=2 3" --game-due=test"#
        ));

        // A version goes as it is shown, with the spaces and the Korean in its name.
        let shown = Remembered { game_due: false, hands_off: vec!["Ver 1.2.421 마이너버전(4)".to_owned()] };
        let handover = Handover {
            replaces: 4242,
            data: PathBuf::from(r"D:\data"),
            beside: false,
            remembered: BTreeMap::from([("live".to_owned(), shown)]),
        };
        let told = handover.arguments(&[]);
        assert!(command_line(&told).ends_with(r#" "--hands-off=live=Ver 1.2.421 마이너버전(4)""#));
        assert_eq!(Handover::told(&told), Some(handover));
    }

    /// From one copy of the app to the next: the game that was due is still due, the patch
    /// the user stopped is still theirs to start, and a site that remembered nothing is not
    /// spoken of.
    #[test]
    fn what_the_sites_remember_goes_from_the_copy_that_leaves_to_the_one_that_takes_over() {
        use crate::harness::StandIns;
        use crate::made::Registered;
        use crate::state::Site;
        let made = || {
            let site = |client| Site::of(client, PathBuf::from("data"), StandIns::default(), Registered::nowhere());
            Patcher::of(PathBuf::from("data"), vec![site(&mfp_kms::LIVE), site(&mfp_kms::TEST)])
        };
        let leaving = made();
        assert_eq!(Handover::of(&leaving, false).remembered, BTreeMap::new());
        {
            let mut live = leaving.sites[0].flow.lock().unwrap();
            live.game_due = true;
            live.keep_hands_off("1.2.422".to_owned());
        }
        let handover = Handover::of(&leaving, true);
        assert_eq!((handover.replaces, handover.beside), (std::process::id(), true));
        assert!(handover.data.is_absolute() && handover.data.ends_with("data"));
        let live = Remembered { game_due: true, hands_off: vec!["1.2.422".to_owned()] };
        assert_eq!(handover.remembered, BTreeMap::from([("live".to_owned(), live.clone())]));

        // Told on a command line, and taken up by the sites of the copy that reads it.
        let mut told = Handover::told(&handover.arguments(&[])).unwrap();
        assert_eq!(told, handover);
        // What is said of a client this copy does not look after is let go.
        told.remembered.insert("another".to_owned(), live.clone());
        let taking_over = made();
        told.taken_up(&taking_over);
        let remembers = |place: usize| taking_over.sites[place].flow.lock().unwrap().remembered();
        assert_eq!((remembers(0), remembers(1)), (live, Remembered::default()));
    }

    #[test]
    fn a_start_by_the_user_hands_nothing_over() {
        let none: [&[&str]; 7] = [
            &[],
            &["--verbose"],
            // Half of it is none of it.
            &["--replaces=4242"],
            &[r"--data=C:\data"],
            &["--replaces=4242", "--data="],
            // What is no process is none.
            &["--replaces=the-app", r"--data=C:\data"],
            &["--replaces=-1", r"--data=C:\data"],
        ];
        for arguments in none {
            assert_eq!(Handover::told(&words(arguments)), None, "{arguments:?}");
        }
    }

    /// The command line is taken apart again by the program it starts. What goes in has to
    /// come out as the same arguments, whatever stands in them.
    #[test]
    fn the_command_line_is_written_so_that_it_is_taken_apart_into_the_same_arguments() {
        let line = |arguments: &[&str]| command_line(&words(arguments));
        assert_eq!(line(&[]), "");
        assert_eq!(line(&["--replaces=4242", r"--data=C:\data"]), r"--replaces=4242 --data=C:\data");
        // A space puts the argument in quotes.
        assert_eq!(line(&[r"--data=D:\Maple Fast Patch\data"]), r#""--data=D:\Maple Fast Patch\data""#);
        // A backslash at the end of a quoted argument would take the closing quote with it.
        assert_eq!(line(&[r"--data=D:\Maple Fast Patch\"]), r#""--data=D:\Maple Fast Patch\\""#);
        // Without quotes around it, a backslash is only itself.
        assert_eq!(line(&[r"--data=D:\data\"]), r"--data=D:\data\");
        // A quote is written with a backslash, and backslashes before it twice.
        assert_eq!(line(&[r#"a"b"#]), r#""a\"b""#);
        assert_eq!(line(&[r#"a\"b"#]), r#""a\\\"b""#);
        assert_eq!(line(&[""]), r#""""#);
        assert_eq!(line(&["a b", "c"]), r#""a b" c"#);
    }
}
