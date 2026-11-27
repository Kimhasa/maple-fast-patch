//! The app's window, its icon next to the clock, and the notifications and boxes it shows.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tauri::{AppHandle, LogicalSize, Manager, State, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MessageBoxW};

use crate::desk::update;
use crate::flow::{AtClose, CloseAnswer, Flow, OnClose, on_close};
use crate::harness::work_area;
use crate::patching::stop;
use crate::state::{Patcher, Site};
#[cfg(windows)]
use crate::wide::to_wide;
use crate::wording::{APP_NAME, APP_TITLE};

/// Shows a Windows notification about the game of `site`. Best effort: there is nothing
/// to do when it fails.
pub(crate) fn notify(app: &AppHandle, site: &Site, text: &str) {
    let _ = app.notification().builder().title(site.heading()).body(text).show();
}

/// Lets the user pick a folder, in a dialog of Windows' own that belongs to the app's
/// window. `None` when they pick none. It is waited for here, so whoever asks does so on a
/// thread that is not the window's.
///
/// Every folder the app is given comes from here, picked by the user. The page names none:
/// it can ask for the dialog, and that is all it can do about which folder is used.
pub(crate) fn pick_folder(app: &AppHandle) -> Option<PathBuf> {
    let dialog = app.dialog().file();
    let dialog = match app.get_webview_window("main") {
        Some(window) => dialog.set_parent(&window),
        None => dialog,
    };
    dialog.blocking_pick_folder().and_then(|folder| folder.into_path().ok())
}

/// The size the window is laid out at, in logical pixels: the page is made for it, and the
/// window is not made smaller than this.
const LAID_OUT: (f64, f64) = (1128.0, 602.0);
/// The size the window opens at where none is on record, in logical pixels: a little more
/// than it is laid out at, for the room that gives the page. It is the size the window is
/// made at (`tauri.conf.json`).
const OPENS_AT: (f64, f64) = (1200.0, 650.0);
/// What is left free on every side of a window that was made smaller to fit its screen.
const MARGIN: f64 = 8.0;
/// How long the window has to have stood still before it is fitted to the screen it is on.
const AT_REST: Duration = Duration::from_millis(250);

/// By how much the whole window is made smaller, so that it fits a screen with a work area
/// of `work` (width and height, in logical pixels): 1 where it fits as it is laid out, and
/// never more than that.
///
/// A laptop screen at a large scale has fewer logical pixels than the window is wide or
/// high, and a window that cannot be made smaller than it is laid out would hang over its
/// edge, the one button with it. The window is then made smaller as a whole, by one factor
/// for its size and for the page in it, so that the page stays as it is laid out.
fn fit(work: (f64, f64)) -> f64 {
    if work.0 >= LAID_OUT.0 && work.1 >= LAID_OUT.1 {
        return 1.0;
    }
    let room = |work: f64, laid_out: f64| (work - 2.0 * MARGIN) / laid_out;
    // A screen less than half the window's size is no screen this is for.
    room(work.0, LAID_OUT.0).min(room(work.1, LAID_OUT.1)).clamp(0.5, 1.0)
}

/// The smallest size the window takes on a screen it is fitted to by `factor`, in logical
/// pixels: the size it is laid out at, made smaller as a whole where the screen asks for
/// that. The user makes the window larger than this by its edges, and never smaller.
fn smallest(factor: f64) -> (f64, f64) {
    (LAID_OUT.0 * factor, LAID_OUT.1 * factor)
}

/// The size the window opens at, in logical pixels, on a screen with a work area of `work`
/// that it is fitted to by `factor`: the size it was `left_at` when the app last ran, no
/// larger than the screen has room for and no smaller than the window gets there. With
/// nothing on record it is the size it is made at ([`OPENS_AT`]), within the same bounds:
/// on a screen the window had to be made smaller for, that is the smallest size.
fn opens_at(left_at: Option<(u32, u32)>, factor: f64, work: (f64, f64)) -> (f64, f64) {
    let least = smallest(factor);
    let (width, height) = match left_at {
        Some((width, height)) => (f64::from(width), f64::from(height)),
        None if factor < 1.0 => return least,
        None => OPENS_AT,
    };
    (width.min(work.0).max(least.0), height.min(work.1).max(least.1))
}

/// The size the window is given once it has come to rest on a screen it is fitted to by
/// `factor`, having been fitted by `before`: none while the factor stays what it was, so
/// that a size the user gave the window is left alone, and the smallest size of a screen
/// that asks for another factor.
fn refitted(before: f64, factor: f64) -> Option<(f64, f64)> {
    ((before - factor).abs() >= 0.001).then(|| smallest(factor))
}

/// What the size of the window is written down in, in the app's data folder: its width and
/// its height in logical pixels, and nothing else.
const SIZE_FILE: &str = "window-size";

/// The size `text` records: two whole numbers above zero, as in `1500 800`. Anything else
/// records none.
fn size_in(text: &str) -> Option<(u32, u32)> {
    let mut numbers = text.split_whitespace().map(|number| number.parse::<u32>().ok().filter(|&number| number > 0));
    match (numbers.next(), numbers.next(), numbers.next()) {
        (Some(Some(width)), Some(Some(height)), None) => Some((width, height)),
        _ => None,
    }
}

/// The size on record in `data`, the app's data folder, if one is and can be read.
fn size_left(data: &Path) -> Option<(u32, u32)> {
    size_in(&std::fs::read_to_string(data.join(SIZE_FILE)).ok()?)
}

fn write_size(data: &Path, size: (u32, u32)) -> std::io::Result<()> {
    std::fs::create_dir_all(data)?;
    std::fs::write(data.join(SIZE_FILE), format!("{} {}", size.0, size.1))
}

/// A size in logical pixels as it is written down: in whole pixels.
fn whole(size: (f64, f64)) -> (u32, u32) {
    (size.0.round() as u32, size.1.round() as u32)
}

/// How the window was last fitted to its screen, and what it takes to fit it again when
/// it has been moved.
pub(crate) struct Fitted {
    /// The factor the window is at; see [`fit`].
    factor: Mutex<f64>,
    /// The size of the window that is on record, or that it opened at. It is written down
    /// when it has become another.
    kept: Mutex<Option<(u32, u32)>>,
    /// Counts the times the window was moved, given another size or came to another scale.
    moves: AtomicU64,
    /// Something is waiting for the window to come to rest.
    waiting: AtomicBool,
}

impl Default for Fitted {
    fn default() -> Fitted {
        Fitted {
            factor: Mutex::new(1.0),
            kept: Mutex::default(),
            moves: AtomicU64::new(0),
            waiting: AtomicBool::new(false),
        }
    }
}

/// The window, and the work area of the screen it is on, in logical pixels.
fn on_its_screen(app: &AppHandle) -> Option<(WebviewWindow, (f64, f64))> {
    let window = app.get_webview_window("main")?;
    let monitor = window.current_monitor().ok()??;
    let (scale, area) = (monitor.scale_factor(), monitor.work_area().size);
    let on_screen = (f64::from(area.width) / scale, f64::from(area.height) / scale);
    Some((window, work_area().unwrap_or(on_screen)))
}

/// Gives the window the size `size` on a screen it is fitted to by `factor`. The smallest
/// size goes by the factor as well: on a screen that asks for a smaller window, the window
/// could not be made that small otherwise.
fn size_window(window: &WebviewWindow, factor: f64, size: (f64, f64)) {
    let least = smallest(factor);
    let _ = window.set_min_size(Some(LogicalSize::new(least.0, least.1)));
    let _ = window.set_size(LogicalSize::new(size.0, size.1));
    let _ = window.set_zoom(factor);
}

/// The folder, inside the one Tauri keeps for the app, in which a copy that runs with
/// administrator rights has its browser keep its files.
const WITH_RIGHTS: &str = "elevated";

/// Where the browser inside the window keeps its files, when that is not `usual`, the
/// folder Tauri has it keep them in: a copy that runs with administrator rights (`elevated`)
/// keeps them apart.
///
/// WebView2 runs one browser for a folder, whichever programs show pages out of it, and a
/// browser that was started without those rights is not one a program with them can share.
/// The copy that takes over with them (see [`crate::rights`]) would find the browser of the
/// copy it replaces still closing in that folder. In a folder of its own it finds nothing.
fn browser_folder(usual: &Path, elevated: bool) -> Option<PathBuf> {
    elevated.then(|| usual.join(WITH_RIGHTS))
}

/// Makes the app's window, hidden: [`open_window`] gives it its size, and it shows when
/// its page has drawn.
///
/// The app makes it itself once its start has come this far, and Tauri does not make it
/// before that (`create` is off for it in `tauri.conf.json`). A copy that starts itself
/// again with administrator rights and leaves never comes this far: it has no window that
/// could show for a moment, and no browser to close behind it.
pub(crate) fn make_window(app: &AppHandle, elevated: bool) -> tauri::Result<()> {
    let apart = app.path().app_local_data_dir().ok().and_then(|usual| browser_folder(&usual, elevated));
    for config in &app.config().app.windows {
        let mut window = WebviewWindowBuilder::from_config(app, config)?;
        if let Some(apart) = &apart {
            window = window.data_directory(apart.clone());
        }
        window.build()?;
    }
    Ok(())
}

/// Has the window as it opens: fitted to the screen it is on (see [`fit`]), at the size it
/// was left at when the app last ran (see [`opens_at`]), in the middle of that screen.
pub(crate) fn open_window(app: &AppHandle) {
    let Some((window, work)) = on_its_screen(app) else { return };
    let factor = fit(work);
    let size = opens_at(size_left(&app.state::<Patcher>().data), factor, work);
    let fitted = app.state::<Fitted>();
    *fitted.factor.lock().unwrap() = factor;
    // What is on record stays as it is until the window is given another size than this.
    *fitted.kept.lock().unwrap() = Some(whole(size));
    // The window is made at the size it opens at with nothing on record, in the middle of
    // its screen.
    if (factor - 1.0).abs() < 0.001 && size == OPENS_AT {
        return;
    }
    size_window(&window, factor, size);
    let _ = window.center();
}

/// Fits the window to the screen it has come to; see [`fit`] and [`refitted`].
fn fit_window(app: &AppHandle) {
    let Some((window, work)) = on_its_screen(app) else { return };
    let factor = fit(work);
    let before = std::mem::replace(&mut *app.state::<Fitted>().factor.lock().unwrap(), factor);
    if let Some(size) = refitted(before, factor) {
        size_window(&window, factor, size);
    }
}

/// Writes down the size the window has now, when it is another than is on record: it opens
/// at that size the next time. A window that is made small has none to write down.
fn keep_size(app: &AppHandle) {
    let (Some(patcher), Some(window)) = (app.try_state::<Patcher>(), app.get_webview_window("main")) else { return };
    if window.is_minimized().unwrap_or(true) {
        return;
    }
    let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) else { return };
    let size = whole((f64::from(size.width) / scale, f64::from(size.height) / scale));
    if size.0 == 0 || size.1 == 0 {
        return;
    }
    let fitted = app.state::<Fitted>();
    let mut kept = fitted.kept.lock().unwrap();
    if *kept != Some(size) && write_size(&patcher.data, size).is_ok() {
        *kept = Some(size);
    }
}

/// Looks after the window once it has come to rest: it was moved or given another size, or
/// the scale of its screen has changed. It is fitted to the screen it is on, and its size is
/// written down. While it is being dragged, by its bar or by an edge, it is left as it is.
pub(crate) fn settle_when_at_rest(app: &AppHandle) {
    let fitted = app.state::<Fitted>();
    fitted.moves.fetch_add(1, Ordering::SeqCst);
    if fitted.waiting.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let fitted = app.state::<Fitted>();
        loop {
            let mut seen = fitted.moves.load(Ordering::SeqCst);
            loop {
                std::thread::sleep(AT_REST);
                let moves = fitted.moves.load(Ordering::SeqCst);
                if moves == seen {
                    break;
                }
                seen = moves;
            }
            fit_window(&app);
            keep_size(&app);
            fitted.waiting.store(false, Ordering::SeqCst);
            // It was moved again in that very moment, and nobody else is waiting for it.
            if fitted.moves.load(Ordering::SeqCst) == seen || fitted.waiting.swap(true, Ordering::SeqCst) {
                break;
            }
        }
    });
}

const TRAY: &str = "main";

/// Has the app's icon next to the clock exactly while the app stays there, and says whether
/// it is there now; see [`Patcher::stays`]. While it is there, closing the window only puts
/// the window away.
///
/// Whoever changes what that goes by, the setting or a window put away for a run, calls
/// this once the change is made. The icon is then as the last of them left it, whichever
/// order they came in.
///
/// The icon is made once, the first time it is wanted, and from then on only shown and
/// hidden. Tauri keeps the handler of an icon's menu for good, also after the icon is
/// removed: an icon made anew each time the setting is switched on would leave one more
/// handler behind, and 열기 and 종료 would then run once for each of them.
pub(crate) fn settle_tray(app: &AppHandle) -> bool {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let wanted = app.state::<Patcher>().stays();
    if let Some(tray) = app.tray_by_id(TRAY) {
        return tray.set_visible(wanted).is_ok() && wanted;
    }
    if !wanted {
        return false;
    }
    let menu = MenuItem::with_id(app, "open", "열기", true, None::<&str>).and_then(|open| {
        let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
        Menu::with_items(app, &[&open, &quit])
    });
    let Ok(menu) = menu else { return false };
    let mut tray = TrayIconBuilder::with_id(TRAY)
        .tooltip(APP_NAME)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => bring_back(app),
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                bring_back(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app).is_ok()
}

/// How long the window waits for its page before it shows without it.
const SHOWN_AT_LATEST: Duration = Duration::from_millis(1500);
/// The window has been shown. It is made hidden, and shown when its page has something on
/// it: shown at once it would be a plain surface until the page has drawn.
static SHOWN: AtomicBool = AtomicBool::new(false);

/// The page has drawn what the backend first told it: the window shows.
#[tauri::command]
pub(crate) fn page_drawn(app: AppHandle) {
    show_first(&app);
}

/// Has the window shown at the latest, whatever becomes of its page: a page that is slow,
/// or never loads, still gets its window.
pub(crate) fn show_at_the_latest(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SHOWN_AT_LATEST);
        show_first(&app);
    });
}

/// Shows the window if it has not been shown yet.
fn show_first(app: &AppHandle) {
    if !SHOWN.load(Ordering::SeqCst) {
        bring_back(app);
    }
}

/// Shows the window, in front: for the first time, or again. An app that is still starting
/// and has made no window yet has none to show: that one shows when its page has drawn.
pub(crate) fn bring_back(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        SHOWN.store(true, Ordering::SeqCst);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    // A window that was put away for a run is why the icon was there. From here on the
    // icon goes by what is set again. The window comes first, so that there is no moment
    // with neither. An app that is only just starting has put nothing away yet.
    if app.try_state::<Patcher>().is_some_and(|patcher| patcher.brought_back()) {
        settle_tray(app);
    }
}

/// The window was asked to close. Says what that does; see [`on_close`]. Where the user is
/// to be asked first, the run says so from here on, and the page, which is told, asks.
pub(crate) fn closed(app: &AppHandle) -> OnClose {
    let patcher = app.state::<Patcher>();
    let stays = patcher.stays();
    match patcher.running() {
        // Decided in one breath with the run. One that ends in this very moment is not
        // asked about, and a question is open from the moment it is decided on.
        Some(site) => update(app, &patcher, site, |flow| flow.close(stays, Instant::now())),
        None => on_close(stays, AtClose::Idle),
    }
}

/// Takes the user's answer to the question that closing the window on a run opened; see
/// [`OnClose::Ask`] and [`CloseAnswer`].
#[tauri::command]
pub(crate) fn answer_close(app: AppHandle, patcher: State<Patcher>, answer: CloseAnswer) {
    // The question is over with any answer, and the page is told so with the run.
    let open = patcher.running().is_some_and(|site| update(&app, &patcher, site, Flow::answered));
    match answer.given(open) {
        CloseAnswer::Background => put_window_away(&app),
        CloseAnswer::Quit => quit(&app),
        CloseAnswer::Stay => {}
    }
}

/// Puts the window away for the run at work, which goes on: the app is next to the clock
/// until the window is brought back, whatever is set, and what is set is not changed.
///
/// The icon comes first. It is what the window is brought back with, so a window whose
/// icon could not be shown stays where it is.
fn put_window_away(app: &AppHandle) {
    let patcher = app.state::<Patcher>();
    patcher.put_away();
    if !settle_tray(app) {
        patcher.brought_back();
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

/// Ends the app for good. A run that is at work is asked to stop first, and the app goes
/// when it has: the same as closing the window again over the question does.
fn quit(app: &AppHandle) {
    if !stop(app, &app.state::<Patcher>(), true) {
        app.exit(0);
    }
}

/// Says that the WebView2 runtime is missing and offers Microsoft's page for it.
///
/// The window is drawn by that runtime. Windows 11 has it; on Windows 10 the installer of
/// this app puts it there, but the program file started by itself, without it, would only
/// close again without a word.
#[cfg(windows)]
pub(crate) fn ask_for_webview() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IDOK, MB_ICONWARNING, MB_OKCANCEL};
    const PAGE: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
    let text = "WebView2 런타임이 설치되어 있지 않습니다.\n확인을 누르면 다운로드 페이지로 이동합니다.";
    if message_box(text, MB_OKCANCEL | MB_ICONWARNING) == IDOK {
        let _ = tauri_plugin_opener::open_url(PAGE, None::<&str>);
    }
}

/// Shows one of Windows' own boxes with `text` under the app's name, and says how it was
/// answered. For when the app's window is not there to say it: before there is one, or
/// after the program has failed.
#[cfg(windows)]
pub(crate) fn message_box(text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (text, title) = (to_wide(text), to_wide(APP_TITLE));
    // SAFETY: both pointers are to NUL-terminated buffers that outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), style) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    /// The work area of a screen of `pixels` at `scale` percent, in logical pixels, with
    /// the taskbar of Windows 11 along its lower edge.
    fn work_area(pixels: (f64, f64), scale: f64) -> (f64, f64) {
        (pixels.0 * 100.0 / scale, pixels.1 * 100.0 / scale - 48.0)
    }

    /// The browser of a copy with administrator rights is never the one a copy without
    /// them has running, or has only just left behind: each keeps to a folder of its own.
    #[test]
    fn a_copy_with_administrator_rights_keeps_its_browsers_files_apart() {
        let usual = Path::new("local").join("com.maplefastpatch.app");
        assert_eq!(browser_folder(&usual, false), None);
        assert_eq!(browser_folder(&usual, true), Some(usual.join("elevated")));
    }

    #[test]
    fn a_screen_with_room_for_the_window_leaves_it_as_it_is_laid_out() {
        for (pixels, scale) in [
            ((1920.0, 1080.0), 100.0),
            ((1920.0, 1080.0), 125.0),
            ((1920.0, 1080.0), 150.0),
            ((1366.0, 768.0), 100.0),
            ((2560.0, 1440.0), 200.0),
        ] {
            assert_eq!(fit(work_area(pixels, scale)), 1.0, "{pixels:?} at {scale} %");
        }
        // Exactly the window's own size is room enough.
        assert_eq!(fit(LAID_OUT), 1.0);
    }

    #[test]
    fn a_screen_that_is_smaller_than_the_window_gets_a_smaller_window() {
        // 1920 × 1080 at 175 % is 1097 × 617 logical pixels, 569 high above the taskbar;
        // 1366 × 768 at 125 % is 1093 × 614, and 1600 × 900 at 150 % is 1067 × 600.
        let cases = [
            (((1920.0, 1080.0), 175.0), 0.9188),
            (((1366.0, 768.0), 125.0), 0.9143),
            (((1600.0, 900.0), 150.0), 0.8904),
        ];
        for ((pixels, scale), expected) in cases {
            let work = work_area(pixels, scale);
            let factor = fit(work);
            assert!((factor - expected).abs() < 0.0001, "{pixels:?} at {scale} %: {factor}");
            // The whole window is on the screen, with the margin on every side.
            assert!(LAID_OUT.0 * factor <= work.0 - 2.0 * MARGIN + 0.001, "{pixels:?} at {scale} %");
            assert!(LAID_OUT.1 * factor <= work.1 - 2.0 * MARGIN + 0.001, "{pixels:?} at {scale} %");
        }
        // Too narrow alone, and too low alone.
        assert!((fit((1000.0, 900.0)) - (1000.0 - 16.0) / 1128.0).abs() < 1e-9);
        assert!((fit((1600.0, 500.0)) - (500.0 - 16.0) / 602.0).abs() < 1e-9);
    }

    #[test]
    fn the_window_is_never_made_larger_and_never_smaller_than_half() {
        assert_eq!(fit((3840.0, 2112.0)), 1.0);
        assert_eq!(fit((400.0, 300.0)), 0.5);
        assert_eq!(fit((0.0, 0.0)), 0.5);
    }

    #[test]
    fn the_smallest_size_follows_the_factor_the_window_is_fitted_by() {
        assert_eq!(smallest(1.0), LAID_OUT);
        // 1920 × 1080 at 175 %: the window is made smaller there, and can be made that small.
        let work = work_area((1920.0, 1080.0), 175.0);
        let least = smallest(fit(work));
        assert!(least.0 < LAID_OUT.0 && least.1 < LAID_OUT.1);
        assert!(least.0 <= work.0 - 2.0 * MARGIN + 0.001 && least.1 <= work.1 - 2.0 * MARGIN + 0.001);
        // One factor for both sides: the page in it stays as it is laid out.
        assert!((least.0 / least.1 - LAID_OUT.0 / LAID_OUT.1).abs() < 1e-9);
    }

    #[test]
    fn a_window_at_rest_keeps_the_size_the_user_gave_it_unless_its_screen_asks_for_another_factor() {
        // An ordinary screen, however often the window is moved or given a size on it.
        assert_eq!(refitted(1.0, 1.0), None);
        assert_eq!(refitted(1.0, fit(work_area((2560.0, 1440.0), 100.0))), None);
        // The same small screen again.
        let small = fit(work_area((1920.0, 1080.0), 175.0));
        assert_eq!(refitted(small, small), None);
        // Onto a screen that asks for a smaller window, and back.
        assert_eq!(refitted(1.0, small), Some(smallest(small)));
        assert_eq!(refitted(small, 1.0), Some(LAID_OUT));
    }

    #[test]
    fn the_window_opens_at_the_size_it_was_left_at_within_what_its_screen_takes() {
        let full_hd = work_area((1920.0, 1080.0), 100.0);
        // Nothing on record: the size it is made at, a little more than it is laid out at.
        assert_eq!(opens_at(None, 1.0, full_hd), OPENS_AT);
        assert!(OPENS_AT.0 >= LAID_OUT.0 && OPENS_AT.1 >= LAID_OUT.1);
        // That fits the screens there are, the lowest of them with little to spare:
        // 1920 × 1080 at 150 % is 1280 × 672 above the taskbar.
        for (pixels, scale) in [((1366.0, 768.0), 100.0), ((1920.0, 1080.0), 125.0), ((1920.0, 1080.0), 150.0)] {
            let work = work_area(pixels, scale);
            assert_eq!(opens_at(None, fit(work), work), OPENS_AT, "{pixels:?} at {scale} %");
        }
        // On a screen with room for the window as it is laid out and no more, it is no
        // larger than the screen.
        assert_eq!(opens_at(None, 1.0, (1150.0, 640.0)), (1150.0, 640.0));
        assert_eq!(opens_at(None, 1.0, LAID_OUT), LAID_OUT);
        assert_eq!(opens_at(Some((1500, 800)), 1.0, full_hd), (1500.0, 800.0));
        // Never smaller than the smallest size, on either side alone.
        assert_eq!(opens_at(Some((800, 400)), 1.0, full_hd), LAID_OUT);
        assert_eq!(opens_at(Some((1500, 400)), 1.0, full_hd), (1500.0, 602.0));
        assert_eq!(opens_at(Some((1, 900)), 1.0, full_hd), (1128.0, 900.0));
        // Never larger than the work area of the screen it opens on.
        assert_eq!(opens_at(Some((3000, 2000)), 1.0, full_hd), full_hd);
        assert_eq!(opens_at(Some((u32::MAX, 700)), 1.0, full_hd), (1920.0, 700.0));

        // Left large on a large screen, and opened on one that asks for a smaller window.
        let small = work_area((1920.0, 1080.0), 175.0);
        let factor = fit(small);
        assert_eq!(opens_at(None, factor, small), smallest(factor));
        assert_eq!(opens_at(Some((1500, 800)), factor, small), small);
        // Left at the smallest size there, and opened on a large one.
        assert_eq!(opens_at(Some(whole(smallest(factor))), 1.0, full_hd), LAID_OUT);
        // A screen with no room for any window of this app gets the smallest there is.
        assert_eq!(opens_at(Some((1500, 800)), 0.5, (400.0, 300.0)), smallest(0.5));
    }

    #[test]
    fn the_record_of_the_size_is_two_whole_numbers_and_anything_else_is_none() {
        assert_eq!(size_in("1500 800"), Some((1500, 800)));
        assert_eq!(size_in(" 1128\t602\r\n"), Some((1128, 602)));
        let unreadable =
            ["", "1500", "1500 800 1", "0 800", "1500 0", "-1500 800", "1500.5 800", "1500x800", "wide high"];
        for text in unreadable {
            assert_eq!(size_in(text), None, "{text:?}");
        }
        // More than a number of pixels can be.
        assert_eq!(size_in("99999999999 800"), None);
        assert_eq!(whole((1499.6, 800.4)), (1500, 800));
    }

    #[test]
    fn the_size_is_written_down_as_two_numbers_and_a_record_that_is_missing_or_unreadable_is_none() {
        let scratch = Scratch::new("window-size");
        // The app's data folder is made by whatever is first written to it.
        let data = scratch.0.join("data");
        assert_eq!(size_left(&data), None);
        write_size(&data, (1500, 800)).unwrap();
        assert_eq!(std::fs::read_to_string(data.join("window-size")).unwrap(), "1500 800");
        assert_eq!(size_left(&data), Some((1500, 800)));
        std::fs::write(data.join("window-size"), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(size_left(&data), None);
    }
}
