//! The dialog the game's launcher opens with when it is started by itself, and pressing its
//! 게임 실행 for the user.
//!
//! The launcher that sits in the install folder does not start the game at once. Started
//! bare, it first shows a dialog of its own: a few lines of text and two buttons, 홈페이지
//! 바로가기 and 게임 실행. Whoever pressed 게임 실행 in this app, or set the app to start the
//! game after a patch, has said what they want: the app presses the launcher's 게임 실행 too.
//!
//! This is done on somebody's PC, beside whatever else they have open, so it is done
//! narrowly. One button is pressed, once: that of a dialog of the very process this app
//! started, told by what it is and what its two buttons say ([`to_press`], which only looks
//! at a description). It is pressed by a message that says the button was pressed, never by
//! moving the pointer or by typing: the pointer and the keyboard are the user's. No window
//! of another program is asked anything. A dialog that looks any other way is left alone,
//! and stays for the user, as it does when the time is up.
//!
//! A launcher that runs with administrator rights takes no messages from a program that
//! does not. Where the launcher asks for those rights and the app has none, the app does not
//! try to get round that: it has a copy of itself started with them, by Windows' own prompt,
//! which starts the launcher and presses the button and is gone again ([`Helper`]). The
//! user answers one prompt either way: that copy's, where it would have been the launcher's.

/// The class Windows gives its own dialog boxes.
const DIALOG: &str = "#32770";
/// The class of a button.
const BUTTON: &str = "Button";
/// What the two buttons of the launcher's dialog say: the one that is pressed, and the one
/// beside it that the dialog is known by.
const START: &str = "게임 실행";
const HOMEPAGE: &str = "홈페이지 바로가기";

/// How many times the launcher's windows are looked at, and how long the watch rests
/// between two looks: a few times a second, for twenty seconds.
const LOOKS: u32 = 80;
const EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// A top-level window, as much of it as the rule goes by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    /// The process it belongs to.
    process: u32,
    class: String,
    /// It is shown.
    visible: bool,
    /// What is in it, in the order Windows lists it.
    controls: Vec<Control>,
}

/// One control of a window.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Control {
    class: String,
    /// What it says. Only asked of a button: nothing goes by what anything else says.
    text: String,
}

/// The button to press among `windows`, for the launcher whose process is `launcher`: the
/// place of its dialog among them, and of the button among the dialog's controls. `None`
/// where there is nothing to press, which is whenever anything is not as it is known.
///
/// The dialog is a window of exactly that process, of the class of Windows' own dialogs,
/// shown, with one button that says exactly 게임 실행 and one that says exactly 홈페이지
/// 바로가기. Where the buttons stand among the controls, and what else the dialog holds,
/// makes no difference: the lines of text are controls too. A button that says nearly that
/// (a space more, another word) is another button, and a dialog with two buttons to choose
/// from, or two such dialogs, is not the one that is known. Nothing is pressed on a guess.
fn to_press(windows: &[Seen], launcher: u32) -> Option<(usize, usize)> {
    // The one button of `window` that says `text`, if there is exactly one.
    let button = |window: &Seen, text: &str| {
        let says = |control: &&Control| control.class.eq_ignore_ascii_case(BUTTON) && control.text == text;
        let mut found = window.controls.iter().enumerate().filter(|(_, control)| says(control));
        let (at, _) = found.next()?;
        found.next().is_none().then_some(at)
    };
    let mut dialogs = windows.iter().enumerate().filter_map(|(at, window)| {
        let known = window.process == launcher && window.class == DIALOG && window.visible;
        let start = button(window, START).filter(|_| known)?;
        button(window, HOMEPAGE).map(|_| (at, start))
    });
    let dialog = dialogs.next()?;
    dialogs.next().is_none().then_some(dialog)
}

/// How the watch for the launcher's dialog ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// Its 게임 실행 was pressed.
    Pressed,
    /// The dialog was there, and Windows did not let the press through: the error it gave.
    /// Access denied (5) is a launcher that runs with rights this app does not have.
    Refused(u32),
    /// The dialog did not show: within the time, or because the launcher ended without it
    /// (`gone`).
    NotFound { gone: bool },
    /// The copy of the app that was to start the launcher with administrator rights did not
    /// start it, or did not say how it ended.
    NotStarted,
}

/// What a copy of the app that presses the button for another ends with when Windows did
/// not let the press through: this, and Windows' error on top of it.
const REFUSED: u32 = 1000;
/// The errors of Windows that are told apart in what such a copy ends with.
const ERRORS: u32 = 100_000;

impl Ended {
    /// What the log says of it. Nothing in it is of this PC or its user.
    pub(crate) fn line(self) -> String {
        match self {
            Ended::Pressed => "pressed the start button of the launcher's dialog".to_owned(),
            Ended::Refused(error) => {
                format!(
                    "Windows did not let the press of the launcher's start button through (error {error}); left to the user"
                )
            }
            Ended::NotFound { gone: false } => "the launcher's dialog was not found within 20 s".to_owned(),
            Ended::NotFound { gone: true } => "the launcher ended without its dialog being found".to_owned(),
            Ended::NotStarted => "the launcher was not started with administrator rights".to_owned(),
        }
    }

    /// What a copy of the app that pressed the button for another ends with, for that one
    /// to read; see [`Ended::of_code`].
    pub(crate) fn code(self) -> u32 {
        match self {
            Ended::Pressed => 0,
            Ended::NotFound { gone: false } => 1,
            Ended::NotFound { gone: true } => 2,
            Ended::NotStarted => 3,
            Ended::Refused(error) => REFUSED + error.min(ERRORS - 1),
        }
    }

    /// How the watch ended, by what the copy of the app that kept it ended with. Anything
    /// that copy does not end with by itself, as when it was ended from outside, says
    /// nothing of the launcher.
    pub(crate) fn of_code(code: u32) -> Ended {
        match code {
            0 => Ended::Pressed,
            1 => Ended::NotFound { gone: false },
            2 => Ended::NotFound { gone: true },
            code if (REFUSED..REFUSED + ERRORS).contains(&code) => Ended::Refused(code - REFUSED),
            _ => Ended::NotStarted,
        }
    }
}

/// The windows of this PC, as the watch asks about them: Windows itself in the app (see
/// [`OnScreen`]), and a made-up screen in the tests.
trait Screen {
    /// The top-level windows of the launcher's process as they are now.
    fn windows(&mut self) -> Vec<Seen>;

    /// Presses the control at `control` of the window at `window`, both as the last look
    /// listed them. Says so with Windows' error where Windows does not let it through.
    fn press(&mut self, window: usize, control: usize) -> Result<(), u32>;

    /// Whether the launcher has ended.
    fn launcher_gone(&mut self) -> bool;

    /// Rests between two looks.
    fn rest(&mut self);
}

/// Looks for the dialog of the launcher whose process is `launcher` on `screen` and presses
/// its 게임 실행, once; see [`to_press`]. Says how that ended.
///
/// It stops at the first of these: the button was pressed; Windows did not let the press
/// through, which is not tried again; the launcher has ended; the time is up.
fn watch(launcher: u32, screen: &mut dyn Screen) -> Ended {
    for look in 1..=LOOKS {
        if let Some((window, button)) = to_press(&screen.windows(), launcher) {
            return match screen.press(window, button) {
                Ok(()) => Ended::Pressed,
                Err(error) => Ended::Refused(error),
            };
        }
        if screen.launcher_gone() {
            return Ended::NotFound { gone: true };
        }
        if look < LOOKS {
            screen.rest();
        }
    }
    Ended::NotFound { gone: false }
}

/// The launcher this app has started: its process, kept open for as long as its dialog is
/// looked for, so that its number stands for no other process meanwhile.
pub(crate) struct Launcher {
    process: u32,
    #[cfg(windows)]
    open: std::os::windows::io::OwnedHandle,
}

impl Launcher {
    /// The launcher that was started as `child`. It is not waited for, and goes on by
    /// itself when this is dropped.
    pub(crate) fn spawned(child: std::process::Child) -> Launcher {
        Launcher {
            process: child.id(),
            #[cfg(windows)]
            open: child.into(),
        }
    }

    /// The launcher whose process Windows handed over as `process` when it started it with
    /// its prompt for rights. `None` where it handed over none: such a launcher is not
    /// known by anything, and its dialog is left alone.
    ///
    /// # Safety
    ///
    /// `process` is null or a handle of a process that nothing else closes: it is closed
    /// when this is dropped.
    #[cfg(windows)]
    pub(crate) unsafe fn handed_over(process: windows_sys::Win32::Foundation::HANDLE) -> Option<Launcher> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        if process.is_null() {
            return None;
        }
        // SAFETY: the caller hands over a handle that is this one's to close.
        let open = unsafe { OwnedHandle::from_raw_handle(process) };
        // SAFETY: the handle is open, and the call only reads the number of its process.
        let number = unsafe { windows_sys::Win32::System::Threading::GetProcessId(open.as_raw_handle()) };
        (number != 0).then_some(Launcher { process: number, open })
    }

    /// Looks for the dialog this launcher opens with and presses its 게임 실행; see
    /// [`watch`]. It takes up to twenty seconds: whoever asks does so on a thread that may.
    pub(crate) fn answer(&self) -> Ended {
        watch(self.process, &mut OnScreen { launcher: self, controls: Vec::new() })
    }
}

/// How long the copy of the app that starts the launcher with administrator rights is
/// waited for: the twenty seconds its watch takes at the most, and time to start in.
const HELPER_ENDS_WITHIN: std::time::Duration = std::time::Duration::from_secs(40);

/// A copy of this app that was started with administrator rights to start the launcher and
/// press its 게임 실행, by Windows' own prompt. It shows nothing and is gone when that is
/// settled; what it ends with says how ([`Ended::code`]).
pub(crate) struct Helper {
    #[cfg(windows)]
    open: std::os::windows::io::OwnedHandle,
}

impl Helper {
    /// The copy whose process Windows handed over as `process` when it started it. `None`
    /// where it handed over none.
    ///
    /// # Safety
    ///
    /// `process` is null or a handle of a process that nothing else closes: it is closed
    /// when this is dropped.
    #[cfg(windows)]
    pub(crate) unsafe fn handed_over(process: windows_sys::Win32::Foundation::HANDLE) -> Option<Helper> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        // SAFETY: the caller hands over a handle that is this one's to close.
        (!process.is_null()).then(|| Helper { open: unsafe { OwnedHandle::from_raw_handle(process) } })
    }

    /// Waits for the copy to be gone and says how its watch ended. A copy that is not gone
    /// in time is left to itself, and nothing is known of the launcher.
    pub(crate) fn answer(&self) -> Ended {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
            use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
            let (process, mut code) = (self.open.as_raw_handle(), 0);
            // SAFETY: the handle is open, and the second call writes one number.
            let ended = unsafe {
                WaitForSingleObject(process, HELPER_ENDS_WITHIN.as_millis() as u32) == WAIT_OBJECT_0
                    && GetExitCodeProcess(process, &mut code) != 0
            };
            if ended { Ended::of_code(code) } else { Ended::NotStarted }
        }
        #[cfg(not(windows))]
        {
            let _ = HELPER_ENDS_WITHIN;
            Ended::NotStarted
        }
    }
}

/// What this app started to get the game going: the launcher itself, or a copy of the app
/// that starts it with administrator rights.
pub(crate) enum Started {
    Launcher(Launcher),
    Helper(Helper),
}

impl Started {
    /// Sees the launcher's dialog answered and says how that ended; see
    /// [`Launcher::answer`] and [`Helper::answer`]. It takes well under a minute at the
    /// most: whoever asks does so on a thread that may.
    pub(crate) fn answer(&self) -> Ended {
        match self {
            Started::Launcher(launcher) => launcher.answer(),
            Started::Helper(helper) => helper.answer(),
        }
    }
}

/// A window as Windows knows it.
#[cfg(windows)]
type Handle = windows_sys::Win32::Foundation::HWND;
#[cfg(not(windows))]
type Handle = ();

/// The screen of this PC: the thin layer of calls into Windows under [`watch`].
///
/// Only the windows of the launcher's own process are looked into. Of any other window
/// nothing is asked but which process it belongs to.
struct OnScreen<'a> {
    launcher: &'a Launcher,
    /// The controls of each window of the last look, as Windows knows them.
    controls: Vec<Vec<Handle>>,
}

/// Where there is no Windows there is no launcher of the game and no dialog of it.
#[cfg(not(windows))]
impl Screen for OnScreen<'_> {
    fn windows(&mut self) -> Vec<Seen> {
        self.controls.clear();
        Vec::new()
    }

    fn press(&mut self, _: usize, _: usize) -> Result<(), u32> {
        Err(0)
    }

    fn launcher_gone(&mut self) -> bool {
        let _ = (self.launcher.process, EVERY);
        true
    }

    fn rest(&mut self) {}
}

#[cfg(windows)]
impl Screen for OnScreen<'_> {
    fn windows(&mut self) -> Vec<Seen> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumChildWindows, EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
        };
        let mut top_level: Vec<Handle> = Vec::new();
        // SAFETY: the callback is handed the list it adds to, which outlives the call.
        unsafe { EnumWindows(Some(collect), &raw mut top_level as isize) };
        self.controls.clear();
        let mut seen = Vec::new();
        for window in top_level {
            let mut process = 0;
            // SAFETY: the call only writes the number of the window's process.
            unsafe { GetWindowThreadProcessId(window, &mut process) };
            if process != self.launcher.process {
                continue;
            }
            let class = class_of(window);
            let mut inside: Vec<Handle> = Vec::new();
            // Only a dialog is looked into.
            if class == DIALOG {
                // SAFETY: as above.
                unsafe { EnumChildWindows(window, Some(collect), &raw mut inside as isize) };
            }
            let controls = inside.iter().map(|&control| {
                let class = class_of(control);
                // Only a button is asked what it says.
                let text = if class.eq_ignore_ascii_case(BUTTON) { text_of(control) } else { String::new() };
                Control { class, text }
            });
            // SAFETY: the call takes a window handle and reads one bit of its state.
            let visible = unsafe { IsWindowVisible(window) } != 0;
            seen.push(Seen { process, class, visible, controls: controls.collect() });
            self.controls.push(inside);
        }
        seen
    }

    /// The press is a message put into the queue of the dialog: the one a button sends the
    /// window it stands in when it has been pressed. Nothing waits for the launcher to take
    /// it, and neither the pointer nor the keyboard is touched.
    ///
    /// The button itself is not told to act pressed (`BM_CLICK`): a button does that only
    /// in a dialog that is the active window, and whether the launcher's is, is not this
    /// app's to say. Only a button that has no number of its own, or stands in no window,
    /// is told so, since there is nothing else to tell.
    fn press(&mut self, window: usize, control: usize) -> Result<(), u32> {
        use windows_sys::Win32::Foundation::ERROR_INVALID_WINDOW_HANDLE;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            BM_CLICK, BN_CLICKED, GetDlgCtrlID, GetParent, PostMessageW, WM_COMMAND,
        };
        let Some(&button) = self.controls.get(window).and_then(|controls| controls.get(control)) else {
            return Err(ERROR_INVALID_WINDOW_HANDLE);
        };
        // SAFETY: both calls take a window handle and read what Windows keeps of it.
        let (stands_in, number) = unsafe { (GetParent(button), GetDlgCtrlID(button)) };
        let posted = if stands_in.is_null() || number <= 0 {
            // SAFETY: the call takes a window handle and a message without pointers in it.
            unsafe { PostMessageW(button, BM_CLICK, 0, 0) }
        } else {
            // Which button it was in the low half, and that it was pressed in the high one.
            let pressed = (number as u32 & 0xFFFF) | (BN_CLICKED << 16);
            // SAFETY: as above; the button's handle is passed as a number, as the message has it.
            unsafe { PostMessageW(stands_in, WM_COMMAND, pressed as usize, button as isize) }
        };
        if posted != 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error().raw_os_error().map_or(0, |error| error as u32))
    }

    fn launcher_gone(&mut self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: the handle is open, and the call does not wait.
        unsafe { WaitForSingleObject(self.launcher.open.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }

    fn rest(&mut self) {
        std::thread::sleep(EVERY);
    }
}

/// Adds `window` to the list `list` points to. For Windows to call with each window it
/// lists.
#[cfg(windows)]
unsafe extern "system" fn collect(
    window: Handle,
    list: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::core::BOOL {
    // SAFETY: `list` is the pointer to the list that whoever asked Windows handed over.
    unsafe { (*(list as *mut Vec<Handle>)).push(window) };
    1
}

/// The class of `window`.
#[cfg(windows)]
fn class_of(window: Handle) -> String {
    let mut name = [0u16; 256];
    // SAFETY: the buffer is as long as the call is told, and outlives it.
    let length = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW(window, name.as_mut_ptr(), name.len() as i32)
    };
    String::from_utf16_lossy(&name[..length.clamp(0, name.len() as i32) as usize])
}

/// What the button `button` says: the text Windows keeps for it, which is had without
/// asking the launcher anything, and where Windows keeps none, what the button itself
/// answers. A button that does not answer within a fifth of a second says nothing.
#[cfg(windows)]
fn text_of(button: Handle) -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowTextW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_GETTEXT,
    };
    let mut text = [0u16; 128];
    // SAFETY: the buffer is as long as the call is told, and outlives it.
    let mut length = unsafe { GetWindowTextW(button, text.as_mut_ptr(), text.len() as i32) } as usize;
    if length == 0 {
        let mut answered = 0usize;
        // SAFETY: as above; the button writes no more than the length it is given.
        let sent = unsafe {
            SendMessageTimeoutW(
                button,
                WM_GETTEXT,
                text.len(),
                text.as_mut_ptr() as isize,
                SMTO_ABORTIFHUNG,
                200,
                &mut answered,
            )
        };
        length = if sent == 0 { 0 } else { answered };
    }
    String::from_utf16_lossy(&text[..length.min(text.len())])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process of the launcher this app started, and that of some other program.
    const LAUNCHER: u32 = 4242;
    const OTHER: u32 = 777;

    fn control(class: &str, text: &str) -> Control {
        Control { class: class.to_owned(), text: text.to_owned() }
    }

    fn button(text: &str) -> Control {
        control("Button", text)
    }

    /// A line of the dialog's text. The watch does not ask what it says.
    fn line() -> Control {
        control("Static", "")
    }

    /// A dialog of `process` that is shown, with `controls`.
    fn dialog(process: u32, controls: Vec<Control>) -> Seen {
        Seen { process, class: "#32770".to_owned(), visible: true, controls }
    }

    /// The dialog as the launcher shows it: its lines of text, then the two buttons.
    fn the_dialog(process: u32) -> Seen {
        dialog(process, vec![line(), line(), line(), button("홈페이지 바로가기"), button("게임 실행")])
    }

    /// What stands on the screen, by a word for it, and the window and the button that is
    /// pressed there, if any.
    type Case = (&'static str, Vec<Seen>, Option<(usize, usize)>);

    /// Which window and which button is pressed, for every way the windows can stand.
    #[test]
    fn only_the_start_button_of_the_started_launchers_own_dialog_is_pressed() {
        let main_window = Seen { class: "MapleStoryLauncher".to_owned(), ..dialog(LAUNCHER, Vec::new()) };
        let table: Vec<Case> = vec![
            ("the launcher's dialog", vec![the_dialog(LAUNCHER)], Some((0, 4))),
            ("nothing is open", vec![], None),
            // Only the process this app started is the launcher.
            ("the same dialog of another process", vec![the_dialog(OTHER)], None),
            ("that, beside the launcher's own", vec![the_dialog(OTHER), the_dialog(LAUNCHER)], Some((1, 4))),
            ("the launcher's other window beside it", vec![main_window, the_dialog(LAUNCHER)], Some((1, 4))),
            // The test world's launcher is another program with a process of its own. One
            // that this app did not start is as much another program as any.
            ("the test world's launcher, started by somebody else", vec![the_dialog(OTHER + 1)], None),
            // Both buttons have to be there.
            ("only 게임 실행", vec![dialog(LAUNCHER, vec![line(), button("게임 실행")])], None),
            ("only 홈페이지 바로가기", vec![dialog(LAUNCHER, vec![line(), button("홈페이지 바로가기")])], None),
            ("no button at all", vec![dialog(LAUNCHER, vec![line(), line()])], None),
            // Where they stand makes no difference, nor what else is there.
            (
                "the buttons in another order",
                vec![dialog(LAUNCHER, vec![button("게임 실행"), line(), button("홈페이지 바로가기")])],
                Some((0, 0)),
            ),
            (
                "a third button beside them",
                vec![dialog(LAUNCHER, vec![button("닫기"), button("홈페이지 바로가기"), button("게임 실행")])],
                Some((0, 2)),
            ),
            (
                "the class of a button written otherwise",
                vec![dialog(LAUNCHER, vec![control("BUTTON", "홈페이지 바로가기"), control("button", "게임 실행")])],
                Some((0, 1)),
            ),
            // A button that says nearly that is another button.
            (
                "a space after 게임 실행",
                vec![dialog(LAUNCHER, vec![button("홈페이지 바로가기"), button("게임 실행 ")])],
                None,
            ),
            (
                "a space before it",
                vec![dialog(LAUNCHER, vec![button("홈페이지 바로가기"), button(" 게임 실행")])],
                None,
            ),
            ("no space in it", vec![dialog(LAUNCHER, vec![button("홈페이지 바로가기"), button("게임실행")])], None),
            (
                "a key marked in it",
                vec![dialog(LAUNCHER, vec![button("홈페이지 바로가기"), button("게임 실행(&S)")])],
                None,
            ),
            (
                "another word for it",
                vec![dialog(LAUNCHER, vec![button("홈페이지 바로가기"), button("게임 시작")])],
                None,
            ),
            ("another word beside it", vec![dialog(LAUNCHER, vec![button("홈페이지"), button("게임 실행")])], None),
            (
                "a space more beside it",
                vec![dialog(LAUNCHER, vec![button("홈페이지  바로가기"), button("게임 실행")])],
                None,
            ),
            // What says so has to be a button.
            (
                "a line of text that says 게임 실행",
                vec![dialog(LAUNCHER, vec![control("Static", "게임 실행"), button("홈페이지 바로가기")])],
                None,
            ),
            (
                "a link that says 홈페이지 바로가기",
                vec![dialog(LAUNCHER, vec![control("SysLink", "홈페이지 바로가기"), button("게임 실행")])],
                None,
            ),
            // Nothing is pressed where there is a choice to make.
            (
                "two buttons that say 게임 실행",
                vec![dialog(LAUNCHER, vec![button("게임 실행"), button("홈페이지 바로가기"), button("게임 실행")])],
                None,
            ),
            (
                "two buttons that say 홈페이지 바로가기",
                vec![dialog(
                    LAUNCHER,
                    vec![button("홈페이지 바로가기"), button("홈페이지 바로가기"), button("게임 실행")],
                )],
                None,
            ),
            ("two such dialogs", vec![the_dialog(LAUNCHER), the_dialog(LAUNCHER)], None),
            // A window of another kind, and one that is not shown.
            (
                "the buttons in a window that is no dialog",
                vec![Seen { class: "Afx:400000".to_owned(), ..the_dialog(LAUNCHER) }],
                None,
            ),
            ("a dialog that is not shown", vec![Seen { visible: false, ..the_dialog(LAUNCHER) }], None),
            (
                "that, with the dialog shown beside it",
                vec![Seen { visible: false, ..the_dialog(LAUNCHER) }, the_dialog(LAUNCHER)],
                Some((1, 4)),
            ),
        ];
        for (what, windows, pressed) in table {
            assert_eq!(to_press(&windows, LAUNCHER), pressed, "{what}");
        }
        // The test world's launcher, started by this app, is the launcher: its dialog is
        // told by the same two buttons.
        assert_eq!(to_press(&[the_dialog(OTHER + 1)], OTHER + 1), Some((0, 4)));
    }

    /// A made-up screen: what each look finds, in order, with the last of them standing
    /// from then on. It notes what was asked of it.
    struct MadeUp {
        looks: Vec<Vec<Seen>>,
        /// What a press comes to.
        lets_through: Result<(), u32>,
        /// After how many looks the launcher has ended, if it ends.
        gone_after: Option<usize>,
        looked: usize,
        rested: usize,
        pressed: Vec<(usize, usize)>,
    }

    impl MadeUp {
        fn showing(looks: Vec<Vec<Seen>>) -> MadeUp {
            MadeUp { looks, lets_through: Ok(()), gone_after: None, looked: 0, rested: 0, pressed: Vec::new() }
        }
    }

    impl Screen for MadeUp {
        fn windows(&mut self) -> Vec<Seen> {
            self.looked += 1;
            let at = (self.looked - 1).min(self.looks.len().saturating_sub(1));
            self.looks.get(at).cloned().unwrap_or_default()
        }

        fn press(&mut self, window: usize, control: usize) -> Result<(), u32> {
            self.pressed.push((window, control));
            self.lets_through
        }

        fn launcher_gone(&mut self) -> bool {
            self.gone_after.is_some_and(|after| self.looked >= after)
        }

        fn rest(&mut self) {
            self.rested += 1;
        }
    }

    #[test]
    fn the_button_is_pressed_once_as_soon_as_the_dialog_is_there_and_nothing_after_it() {
        // The launcher takes three looks to come up: nothing, its window, then its dialog.
        let coming_up = vec![vec![], vec![dialog(LAUNCHER, vec![line()])], vec![the_dialog(LAUNCHER)]];
        let mut screen = MadeUp::showing(coming_up);
        assert_eq!(watch(LAUNCHER, &mut screen), Ended::Pressed);
        assert_eq!((screen.looked, screen.rested), (3, 2));
        assert_eq!(screen.pressed, [(0, 4)]);

        // There at the first look: pressed at once, without a rest.
        let mut screen = MadeUp::showing(vec![vec![the_dialog(OTHER), the_dialog(LAUNCHER)]]);
        assert_eq!(watch(LAUNCHER, &mut screen), Ended::Pressed);
        assert_eq!((screen.looked, screen.rested, screen.pressed), (1, 0, vec![(1, 4)]));
    }

    /// The launcher runs with rights the app does not have, and Windows turns the message
    /// away. That is the end of it: the dialog stays for the user.
    #[test]
    fn a_press_windows_does_not_let_through_is_not_tried_again() {
        let mut screen = MadeUp { lets_through: Err(5), ..MadeUp::showing(vec![vec![the_dialog(LAUNCHER)]]) };
        assert_eq!(watch(LAUNCHER, &mut screen), Ended::Refused(5));
        assert_eq!((screen.looked, screen.pressed.len()), (1, 1));
    }

    #[test]
    fn a_dialog_that_does_not_show_is_given_up_when_the_time_is_up_or_the_launcher_has_ended() {
        // Something else is open all the while, the same dialog of another program among
        // it: twenty seconds of looks, and nothing is pressed.
        let other = vec![the_dialog(OTHER), dialog(LAUNCHER, vec![button("확인")])];
        let mut screen = MadeUp::showing(vec![other.clone()]);
        assert_eq!(watch(LAUNCHER, &mut screen), Ended::NotFound { gone: false });
        assert_eq!((screen.looked, screen.rested), (80, 79));
        assert!(screen.pressed.is_empty());
        assert_eq!(EVERY * LOOKS, std::time::Duration::from_secs(20));

        // The launcher ends after its fifth look: nothing is waited for any longer.
        let mut screen = MadeUp { gone_after: Some(5), ..MadeUp::showing(vec![other]) };
        assert_eq!(watch(LAUNCHER, &mut screen), Ended::NotFound { gone: true });
        assert_eq!((screen.looked, screen.rested, screen.pressed.len()), (5, 4, 0));

        // A dialog that is there in the launcher's last moment is still pressed.
        let mut screen = MadeUp { gone_after: Some(1), ..MadeUp::showing(vec![vec![the_dialog(LAUNCHER)]]) };
        assert_eq!(watch(LAUNCHER, &mut screen), Ended::Pressed);
    }

    #[test]
    fn the_log_has_a_line_for_each_end_and_nothing_of_the_pc_in_it() {
        assert_eq!(Ended::Pressed.line(), "pressed the start button of the launcher's dialog");
        assert_eq!(
            Ended::Refused(5).line(),
            "Windows did not let the press of the launcher's start button through (error 5); left to the user"
        );
        assert_eq!(Ended::NotFound { gone: false }.line(), "the launcher's dialog was not found within 20 s");
        assert_eq!(Ended::NotFound { gone: true }.line(), "the launcher ended without its dialog being found");
        assert_eq!(Ended::NotStarted.line(), "the launcher was not started with administrator rights");
    }

    /// The copy that presses the button for another says how that ended in what it ends
    /// with, and the other reads the same out of it.
    #[test]
    fn what_a_copy_of_the_app_ends_with_says_how_its_watch_ended() {
        let ends = [
            Ended::Pressed,
            Ended::NotFound { gone: false },
            Ended::NotFound { gone: true },
            Ended::NotStarted,
            Ended::Refused(0),
            Ended::Refused(5),
            Ended::Refused(1400),
        ];
        for ended in ends {
            assert_eq!(Ended::of_code(ended.code()), ended, "{ended:?}");
        }
        assert_eq!(Ended::Pressed.code(), 0);
        // A copy that Windows ended, or that is still there, says nothing of the launcher.
        for code in [4, 259, 999, 101_000, 0xC000_0005, u32::MAX] {
            assert_eq!(Ended::of_code(code), Ended::NotStarted, "{code}");
        }
        // An error beyond those that are told apart is still one Windows did not let through.
        assert_eq!(Ended::of_code(Ended::Refused(u32::MAX).code()), Ended::Refused(99_999));
    }
}
