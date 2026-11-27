//! Starting the game, and opening the pages it is downloaded from.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::desk::update;
use crate::launcher::{Ended, Launcher, Started};
use crate::records::{install_dir, is_registered_install};
use crate::rights::elevated;
use crate::signed::{Held, Unvouched, in_its_folder, plain, vouched};
use crate::state::{Patcher, Site};
#[cfg(windows)]
use crate::wide::to_wide;
use crate::wording::{NO_FOLDER, drive_letter, not_the_games_folder};

/// How long the launcher is given to come up before this app goes, where it is set to go
/// once it has started the game.
const COMES_UP: Duration = Duration::from_millis(1500);

/// Starts the game of `site`, for the button and for the app starting it by itself. With
/// the game started, the app goes when the user wants that.
///
/// The launcher that is started from the install folder asks once more before it starts
/// the game, in a dialog of its own. That is answered for the user; see
/// [`Started::answer`]. The app does not go before that is settled one way or the other,
/// which is well under a minute at the most.
pub(crate) fn start_game(app: &AppHandle, patcher: &Patcher, site: &Site) -> Result<(), String> {
    let started_with = launch(app, site)?;
    // It is not started a second time by itself, and the button rests for a moment.
    update(app, patcher, site, |flow| flow.launched(Instant::now()));
    let leaves = patcher.settings.lock().unwrap().exit_after_start && patcher.running().is_none();
    if started_with.is_none() && !leaves {
        return Ok(());
    }
    let (app, place, started) = (app.clone(), patcher.place(site), Instant::now());
    std::thread::spawn(move || {
        let patcher = app.state::<Patcher>();
        if let Some(started_with) = started_with {
            let ended = started_with.answer();
            if let Some(site) = place.and_then(|place| patcher.sites.get(place)) {
                site.log(&ended.line());
            }
        }
        // The launcher gets a moment to come up before this window goes.
        std::thread::sleep(COMES_UP.saturating_sub(started.elapsed()));
        if goes(leaves, patcher.settings.lock().unwrap().exit_after_start, patcher.running().is_some()) {
            app.exit(0);
        }
    });
    Ok(())
}

/// Whether the app goes now that it has started the game: `set` says that it was set to
/// when the game was started, `still_set` that it still is, and `run_at_work` that a patch
/// run is at work by now.
///
/// The app may be twenty seconds behind the start by the time it gets here. Whoever has
/// switched the setting off since does not want it to go, and a run that was begun since is
/// not cut off for it.
fn goes(set: bool, still_set: bool, run_at_work: bool) -> bool {
    set && still_set && !run_at_work
}

/// Starts the game of the client the page calls `server`.
#[tauri::command(async)]
pub(crate) fn launch_game(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or(NO_FOLDER)?;
    start_game(&app, &patcher, site)
}

/// How the game in an install folder is started.
#[derive(Debug, PartialEq, Eq)]
enum Start {
    /// With the launcher that sits in the install folder.
    Launcher,
    /// The way Nexon's desktop shortcut starts it, by the address Nexon's own software
    /// answers to.
    Shortcut,
    /// Not at all: there is nothing to start it with.
    Nothing,
    /// Not at all: there is only the address, and this app runs with administrator rights.
    Unchecked,
}

/// How the game in an install folder is to be started: `has_launcher` says whether the
/// folder holds the client's launcher, `registered` whether it is the folder the Nexon
/// installer registered, of a client whose shortcut is known to work, and `elevated`
/// whether this app runs with administrator rights.
///
/// Nexon's shortcut starts the install that is registered, whatever folder is on the
/// screen. So it is for that folder and no other: for a copy, or for an install that was
/// made up to try the app out, it would start another game than the one that is shown.
///
/// With administrator rights the app starts a launcher it has checked (see
/// [`crate::signed`]) and nothing else. An address is answered by whatever program is set
/// down for it on this PC, which any program of the user's can change and nothing here can
/// check: that program would be handed the rights.
fn way_to_start(has_launcher: bool, registered: bool, elevated: bool) -> Start {
    match (has_launcher, registered) {
        (true, _) => Start::Launcher,
        (false, true) if elevated => Start::Unchecked,
        (false, true) => Start::Shortcut,
        (false, false) => Start::Nothing,
    }
}

/// The kinds of drive, as Windows numbers them (`GetDriveTypeW`), that are part of this PC:
/// one that can be taken out, one that cannot, and one that is kept in memory. A drive
/// letter that stands for a folder another computer shares is none of them.
const DRIVES_OF_THIS_PC: [u32; 3] = [2, 3, 6];

/// Whether what really is at `real` is on a drive of this PC. `kind_of` says which kind of
/// drive Windows takes a drive letter (`C:`) for. A path without a drive letter is that of
/// a folder another computer shares.
fn on_this_pc(real: &Path, kind_of: &dyn Fn(&str) -> u32) -> bool {
    drive_letter(real).is_some_and(|drive| DRIVES_OF_THIS_PC.contains(&kind_of(&drive)))
}

/// Which kind of drive Windows takes the drive letter `drive` (`C:`) for.
fn drive_kind(drive: &str) -> u32 {
    #[cfg(windows)]
    {
        let root = to_wide(format!("{drive}\\"));
        // SAFETY: the pointer is to a NUL-terminated buffer that outlives the call.
        unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) }
    }
    #[cfg(not(windows))]
    {
        let _ = drive;
        0
    }
}

/// What is said when the game's launcher could not be started, whatever kept it.
const NOT_STARTED: &str = "게임 실행기를 시작하지 못했습니다.";

/// Whether the game in `dir` may be started, for the client of `site`: asked at the moment
/// it is to be started, not taken from what a look found some seconds before.
///
/// The folder has to be this client's game folder, and what is started has to be on this
/// PC, by where its path really leads. This app starts no program that another computer
/// hands it over the network, whatever the folder it lies in is called.
fn may_start(site: &Site, dir: &Path) -> Result<(), String> {
    if !site.programs().check_install_dir(dir).is_empty() {
        return Err(not_the_games_folder(site.client.title));
    }
    let launcher = dir.join(site.client.launcher);
    let started = if launcher.is_file() { launcher.as_path() } else { dir };
    if !started.canonicalize().is_ok_and(|real| on_this_pc(&real, &drive_kind)) {
        site.log("not starting the game: its folder is not on a drive of this PC");
        return Err(NOT_STARTED.to_owned());
    }
    Ok(())
}

/// Starts the game with the launcher that sits in the install folder. Falls back to the way
/// Nexon's desktop shortcut starts it when the folder has no launcher.
///
/// Hands on what it started, where Windows says which process that is: the dialog the
/// launcher opens with is then answered for the user.
fn launch(app: &AppHandle, site: &Site) -> Result<Option<Started>, String> {
    let dir = install_dir(site).ok_or(NO_FOLDER)?;
    may_start(site, &dir)?;
    let launcher = dir.join(site.client.launcher);
    // What Nexon's software does with the address of a client this app was never tried on
    // is not known. Nothing is guessed there: such a client is started by its own launcher
    // or not at all.
    let by_shortcut = is_registered_install(site, &dir) && !site.untried();
    let elevated = elevated();
    match way_to_start(launcher.is_file(), by_shortcut, elevated) {
        Start::Launcher => start_launcher(site, &dir, elevated),
        Start::Shortcut => app
            .opener()
            .open_url(site.client.launch_url(), None::<&str>)
            .or_else(|_| app.opener().open_url(site.client.game_page, None::<&str>))
            .map(|()| None)
            .map_err(|_| "게임 실행 페이지를 열 수 없습니다.".to_owned()),
        Start::Nothing => Err("게임 실행기를 찾을 수 없습니다.".to_owned()),
        Start::Unchecked => {
            site.log("not starting the game with administrator rights: the folder has no launcher to check");
            Err(NOT_STARTED.to_owned())
        }
    }
}

/// Starts the launcher that sits in the install folder `dir`, and hands on the process
/// that was started.
///
/// An app that runs with administrator rights (`elevated`) hands them on to it, so it
/// starts only the launcher Windows vouches for as Nexon's; see [`start_vouched`]. Without
/// them the launcher is started as it is, and runs with the user's own rights.
///
/// A launcher that asks for more rights than this app runs with is not started so: Windows
/// asks the user first, and what then runs with those rights takes no messages from this
/// app, so that its dialog could not be answered. For a launcher Windows vouches for, a
/// copy of this app is started with the rights instead, by the same prompt, which starts
/// the launcher and answers its dialog; see [`start_helper`]. Any other launcher is started
/// the way a double click starts it, and its dialog is the user's to answer. The log says
/// which way it went.
fn start_launcher(site: &Site, dir: &Path, elevated: bool) -> Result<Option<Started>, String> {
    let name = site.client.launcher;
    if elevated {
        return start_vouched(name, dir).map(|launcher| launcher.map(Started::Launcher)).map_err(|why| {
            if let Some(why) = why {
                site.log(&format!("not starting the launcher with administrator rights: {}", why.line()));
            }
            NOT_STARTED.to_owned()
        });
    }
    let launcher = dir.join(name);
    match std::process::Command::new(&launcher).current_dir(dir).spawn() {
        Ok(child) => Ok(Some(Started::Launcher(Launcher::spawned(child)))),
        Err(err) if err.raw_os_error() == Some(ELEVATION_REQUIRED) => match checked(name, dir) {
            Ok(_) => {
                site.log(
                    "the launcher asks for administrator rights: started by a copy of this app that is given them",
                );
                start_helper(site, dir)
            }
            Err(why) => {
                let why = why.line();
                site.log(&format!(
                    "the launcher asks for administrator rights, and {why}: started through Windows' own prompt"
                ));
                open_in_folder(&launcher, dir).map(|launcher| launcher.map(Started::Launcher))
            }
        },
        Err(_) => Err(NOT_STARTED.to_owned()),
    }
}

/// The launcher called `name` in `dir`, checked and held: the file, the path it is started
/// by, and the folder it is started in. Why not, where Windows does not vouch for it.
///
/// The file is opened once and held from then on, so that nobody changes it, removes it or
/// renames it or a folder above it (see [`Held`]). What is held is asked where it really
/// is: it has to lie in the folder itself under the launcher's name, on a drive of this PC,
/// and a file that a link leads to does not. Its signature is checked by that open file.
fn checked(name: &str, dir: &Path) -> Result<(Held, PathBuf, PathBuf), Unvouched> {
    let unreadable = |err: std::io::Error| Unvouched::CheckFailed(err.raw_os_error().map_or(0, |error| error as u32));
    let held = Held::open(&dir.join(name)).map_err(unreadable)?;
    let folder = dir.canonicalize().map_err(unreadable)?;
    let here = in_its_folder(held.real(), &folder, name) && on_this_pc(held.real(), &drive_kind);
    let (program, folder) =
        here.then(|| plain(held.real()).zip(plain(&folder))).flatten().ok_or(Unvouched::Elsewhere)?;
    vouched(true, || held.signature())?;
    Ok((held, program, folder))
}

/// Starts the launcher called `name` in `dir` for an app that runs with administrator
/// rights: only where Windows vouches that the file is Nexon's, and that very file. Why
/// not, where it is not started for what the check came to; `None` where Windows did not
/// start what was checked.
///
/// The file is checked and held ([`checked`]), and then started by the path it really has
/// while it is still held: what Windows starts is what was checked. The launcher's dialog
/// is answered in the process that was started so, and in no other.
fn start_vouched(name: &str, dir: &Path) -> Result<Option<Launcher>, Option<Unvouched>> {
    let (held, program, folder) = checked(name, dir)?;
    let started = std::process::Command::new(&program).current_dir(&folder).spawn();
    // Windows has the file open itself by now, or has not started it.
    drop(held);
    started.map(|child| Some(Launcher::spawned(child))).map_err(|_| None)
}

/// What Windows says when a program is started that asks for more rights than the one
/// starting it has (`ERROR_ELEVATION_REQUIRED`).
const ELEVATION_REQUIRED: i32 = 740;

/// What a copy of the app that is started to start a launcher is told: the client whose
/// launcher it is, by what the client goes by (`--press-launcher=live`), and the folder
/// the launcher is in.
const PRESS: &str = "--press-launcher=";
const FOLDER: &str = "--folder=";

/// Starts a copy of this app with administrator rights that starts the launcher in `dir`
/// and answers its dialog. Windows asks the user first, as it would for the launcher
/// itself. Hands on that copy; none where the user said no to the prompt, which is not an
/// error worth a message.
///
/// The copy takes nothing on trust from whoever started it: it checks the launcher itself
/// before it starts it; see [`asked_to_press`].
fn start_helper(site: &Site, dir: &Path) -> Result<Option<Started>, String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_CANCELLED};
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        use crate::launcher::Helper;
        let Ok(program) = std::env::current_exe() else { return Err(NOT_STARTED.to_owned()) };
        let told = [format!("{PRESS}{}", site.client.server.key()), format!("{FOLDER}{}", dir.display())];
        let (program, told, verb) = (to_wide(program), to_wide(crate::rights::command_line(&told)), to_wide("runas"));
        // SAFETY: the structure is plain data, and every bit pattern of it is valid.
        let mut asked: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        asked.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        // The process is handed over, the call returns when it has been started, and it
        // shows nothing of its own when it has not.
        asked.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        (asked.lpVerb, asked.lpFile, asked.lpParameters) = (verb.as_ptr(), program.as_ptr(), told.as_ptr());
        asked.nShow = SW_SHOWNORMAL;
        // SAFETY: every pointer is to a NUL-terminated buffer that outlives the call.
        if unsafe { ShellExecuteExW(&mut asked) } == 0 {
            let error = std::io::Error::last_os_error().raw_os_error().map_or(0, |error| error as u32);
            return match error {
                ERROR_CANCELLED | ERROR_ACCESS_DENIED => Ok(None),
                _ => Err(NOT_STARTED.to_owned()),
            };
        }
        // SAFETY: the handle is the one Windows has just handed over, and nothing else has it.
        Ok(unsafe { Helper::handed_over(asked.hProcess) }.map(Started::Helper))
    }
    #[cfg(not(windows))]
    {
        let _ = (site, dir, PRESS, FOLDER);
        Err("Windows에서만 실행할 수 있습니다.".into())
    }
}

/// What a copy of the app does that was started to start a launcher with administrator
/// rights: `arguments` are those it was started with. `None` where they ask for no such
/// thing, which is every other start of the app. Otherwise how it ended, which is all the
/// copy is there for: it opens no window and is gone with that as what it ends with.
///
/// Anybody can start the app with these arguments, and Windows' prompt then asks the user
/// in this app's name. So the copy does one thing with the rights and takes nobody's word
/// for it: it starts the launcher of a client it knows, from the folder it is told, only
/// where Windows vouches that the file is Nexon's ([`start_vouched`]), and presses the one
/// button of that launcher's own dialog ([`Launcher::answer`]).
pub(crate) fn asked_to_press(arguments: &[String]) -> Option<Ended> {
    let said = |what: &str| arguments.iter().find_map(|argument| argument.strip_prefix(what));
    let client = said(PRESS)?;
    let pressed = || {
        let client = [&mfp_kms::LIVE, &mfp_kms::TEST].into_iter().find(|known| known.server.key() == client)?;
        let dir = said(FOLDER).filter(|folder| !folder.is_empty()).map(PathBuf::from)?;
        let launcher = start_vouched(client.launcher, &dir).ok()??;
        Some(launcher.answer())
    };
    Some(pressed().unwrap_or(Ended::NotStarted))
}

/// Starts `program` the way a double click does, with `folder` as its working folder.
/// Windows asks for administrator rights itself when the program needs them. Hands on the
/// process it started, where Windows says which it is; none where the user said no to the
/// prompt, which is not an error worth a message.
fn open_in_folder(program: &Path, folder: &Path) -> Result<Option<Launcher>, String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_CANCELLED};
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let (program, folder, verb) = (to_wide(program), to_wide(folder), to_wide("open"));
        // SAFETY: the structure is plain data, and every bit pattern of it is valid.
        let mut asked: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        asked.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        // The process is handed over, and the call returns when it has been started.
        asked.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        (asked.lpVerb, asked.lpFile, asked.lpDirectory) = (verb.as_ptr(), program.as_ptr(), folder.as_ptr());
        asked.nShow = SW_SHOWNORMAL;
        // SAFETY: every pointer is to a NUL-terminated buffer that outlives the call.
        if unsafe { ShellExecuteExW(&mut asked) } == 0 {
            let error = std::io::Error::last_os_error().raw_os_error().map_or(0, |error| error as u32);
            return match error {
                ERROR_CANCELLED | ERROR_ACCESS_DENIED => Ok(None),
                _ => Err(NOT_STARTED.to_owned()),
            };
        }
        // SAFETY: the handle is the one Windows has just handed over, and nothing else has it.
        Ok(unsafe { Launcher::handed_over(asked.hProcess) })
    }
    #[cfg(not(windows))]
    {
        let _ = (program, folder);
        Err("Windows에서만 실행할 수 있습니다.".into())
    }
}

/// Opens the official page that offers the game's installer, for when the game is not
/// installed.
#[tauri::command]
pub(crate) fn open_download_page(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), String> {
    let failed = || "다운로드 페이지를 열 수 없습니다.".to_owned();
    let site = patcher.site_named(&server).ok_or_else(failed)?;
    app.opener().open_url(site.client.download_page, None::<&str>).map_err(|_| failed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::StandIns;
    use crate::made::Registered;
    use crate::scratch::Scratch;

    /// The game is started from the folder as it is at that moment. A folder that has
    /// stopped being the client's game folder since it was last looked at is not started
    /// from, whatever program lies in it under the launcher's name.
    #[test]
    fn the_game_is_only_started_from_a_folder_that_is_the_clients_game_folder_at_that_moment() {
        let scratch = Scratch::new("may-start");
        let site = Site::of(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::default(), Registered::nowhere());
        let dir = scratch.0.join("game");
        std::fs::create_dir_all(dir.join("Data")).unwrap();
        std::fs::write(dir.join("GameLauncher.exe"), b"a launcher").unwrap();
        assert_eq!(may_start(&site, &dir), Err("메이플스토리 설치 경로가 아닙니다.".to_owned()));
        // The other client's folder is none of this client's either.
        std::fs::write(dir.join("MapleStoryT.exe"), b"the test world").unwrap();
        assert!(may_start(&site, &dir).is_err());

        std::fs::write(dir.join("MapleStory.exe"), b"the game").unwrap();
        assert_eq!(may_start(&site, &dir), Ok(()));
        // Without a launcher it is the folder itself that has to be on this PC.
        std::fs::remove_file(dir.join("GameLauncher.exe")).unwrap();
        assert_eq!(may_start(&site, &dir), Ok(()));
        // The game was removed since.
        std::fs::remove_file(dir.join("MapleStory.exe")).unwrap();
        assert!(may_start(&site, &dir).is_err());
    }

    #[test]
    fn nothing_is_started_from_a_folder_another_computer_shares() {
        let kinds = |drive: &str| match drive {
            "C:" | "D:" => 3,
            "E:" => 2,
            "R:" => 6,
            // A drive letter that stands for a share, a disc, and a letter no drive has.
            "Z:" => 4,
            "F:" => 5,
            _ => 1,
        };
        // As Windows gives the path a folder really has, and as a path is usually written.
        for local in [r"\\?\C:\Nexon\Maple\GameLauncher.exe", r"D:\Games\Maple", r"E:\Maple", r"R:\Maple"] {
            assert!(on_this_pc(Path::new(local), &kinds), "{local}");
        }
        let shared = [
            r"\\?\UNC\nas\games\Maple\GameLauncher.exe",
            r"\\nas\games\Maple",
            r"Z:\Maple\GameLauncher.exe",
            r"\\?\Z:\Maple",
            r"F:\Maple",
            r"Q:\Maple",
        ];
        for elsewhere in shared {
            assert!(!on_this_pc(Path::new(elsewhere), &kinds), "{elsewhere}");
        }
        // What Windows itself says of the drive this test runs on.
        let here = std::env::temp_dir().canonicalize().unwrap();
        assert!(on_this_pc(&here, &drive_kind));
    }

    /// Set to go once the game is started, the app goes when the launcher's dialog is
    /// settled, which may be twenty seconds on. What has become of the setting and of the
    /// app's one run by then counts.
    #[test]
    fn the_app_goes_after_the_start_only_while_it_is_still_set_to_and_no_run_is_at_work() {
        assert!(goes(true, true, false));
        // Not set when the game was started: switching it on afterwards is for the next start.
        assert!(!goes(false, true, false) && !goes(false, false, false));
        // Switched off since.
        assert!(!goes(true, false, false));
        // A run was begun since: it is not cut off.
        assert!(!goes(true, true, true) && !goes(true, false, true));
        // A launcher that is quick is still given its moment to come up.
        assert_eq!(COMES_UP, Duration::from_millis(1500));
    }

    #[test]
    fn a_folder_with_its_launcher_is_started_by_that_launcher() {
        for elevated in [false, true] {
            assert_eq!(way_to_start(true, true, elevated), Start::Launcher);
            // A copy, or a made-up install with its stand-in for the launcher.
            assert_eq!(way_to_start(true, false, elevated), Start::Launcher);
        }
    }

    #[test]
    fn nexons_shortcut_is_for_the_registered_install_and_for_no_other_folder() {
        assert_eq!(way_to_start(false, true, false), Start::Shortcut);
        // Whatever the folder holds, the address is not opened for it unless it is the
        // registered one: a folder without a launcher is then not started at all.
        for (has_launcher, elevated) in [(true, false), (false, false), (true, true), (false, true)] {
            assert_ne!(way_to_start(has_launcher, false, elevated), Start::Shortcut);
        }
        assert_eq!(way_to_start(false, false, false), Start::Nothing);
        assert_eq!(way_to_start(false, false, true), Start::Nothing);
    }

    /// With administrator rights the app starts a launcher it has checked, and nothing
    /// else: whatever answers to Nexon's address would be handed the rights unchecked.
    #[test]
    fn with_administrator_rights_the_game_is_never_started_by_its_address() {
        assert_eq!(way_to_start(false, true, true), Start::Unchecked);
        for (has_launcher, registered) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_ne!(way_to_start(has_launcher, registered, true), Start::Shortcut);
        }
    }

    /// With administrator rights, a file that is only called like the launcher is not
    /// started: not one that nobody signed, not one that cannot be held for the check, and
    /// not where there is none. What is tried here is no program, so that nothing could
    /// start even if the check let it through.
    #[cfg(windows)]
    #[test]
    fn with_administrator_rights_a_launcher_windows_does_not_vouch_for_is_not_started() {
        let scratch = Scratch::new("vouched-start");
        let site = Site::of(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::default(), Registered::nowhere());
        let dir = scratch.0.join("game");
        std::fs::create_dir_all(&dir).unwrap();
        let refused = |dir: &Path| start_vouched(site.client.launcher, dir).err().flatten();
        // No launcher in the folder, and no folder.
        assert_eq!(refused(&dir), Some(Unvouched::CheckFailed(2)));
        assert_eq!(refused(&scratch.0.join("missing")), Some(Unvouched::CheckFailed(3)));

        let launcher = dir.join("GameLauncher.exe");
        std::fs::write(&launcher, b"a launcher").unwrap();
        let why = refused(&dir);
        assert!(matches!(why, Some(Unvouched::NotSigned | Unvouched::CheckFailed(_))), "{why:?}");
        // What the user is told is the one sentence, whatever kept the launcher from starting.
        assert_eq!(start_launcher(&site, &dir, true).err(), Some("게임 실행기를 시작하지 못했습니다.".to_owned()));

        // Somebody has it open for writing and could change it after the check.
        let writer = std::fs::OpenOptions::new().write(true).open(&launcher).unwrap();
        assert_eq!(refused(&dir), Some(Unvouched::CheckFailed(32)));
        drop(writer);
    }

    /// The copy that is started to start a launcher does so only when it is told a client
    /// it knows and a folder, and never starts a file Windows does not vouch for. What is
    /// tried here is no program, so that nothing could start even if the check let it
    /// through.
    #[cfg(windows)]
    #[test]
    fn a_copy_started_for_the_launcher_starts_nothing_but_a_launcher_windows_vouches_for() {
        let told = |arguments: &[&str]| {
            asked_to_press(&arguments.iter().map(|argument| (*argument).to_owned()).collect::<Vec<_>>())
        };
        // Every other start of the app is not such a copy, whatever else it is told.
        assert_eq!(told(&[]), None);
        assert_eq!(told(&["--replaces=1", r"--data=E:\old", r"--folder=C:\game", "--game-due=live"]), None);
        // Without a folder there is nothing to start.
        assert_eq!(told(&["--press-launcher=live"]), Some(Ended::NotStarted));
        assert_eq!(told(&["--press-launcher=live", "--folder="]), Some(Ended::NotStarted));

        let scratch = Scratch::new("copy-for-the-launcher");
        let dir = scratch.0.join("game");
        std::fs::create_dir_all(&dir).unwrap();
        for launcher in ["GameLauncher.exe", "GameLauncherT.exe", "other.exe"] {
            std::fs::write(dir.join(launcher), b"a launcher").unwrap();
        }
        let folder = format!("--folder={}", dir.display());
        // A client there is none of, a program by its path, and a file that is only called
        // like the launcher: none is started.
        assert_eq!(told(&["--press-launcher=other", &folder]), Some(Ended::NotStarted));
        assert_eq!(told(&["--press-launcher=", &folder]), Some(Ended::NotStarted));
        assert_eq!(told(&[r"--press-launcher=..\game\other.exe", &folder]), Some(Ended::NotStarted));
        assert_eq!(told(&["--press-launcher=live", &folder]), Some(Ended::NotStarted));
        assert_eq!(told(&["--press-launcher=test", &folder]), Some(Ended::NotStarted));
    }
}
