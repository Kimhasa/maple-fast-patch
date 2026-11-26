//! The app's window, its icon next to the clock, and the notifications and boxes it shows.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, LogicalSize, Manager};
use tauri_plugin_notification::NotificationExt;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MessageBoxW};

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

/// The size the window is laid out at, in logical pixels: the page is made for it.
const LAID_OUT: (f64, f64) = (1128.0, 602.0);
/// What is left free on every side of a window that was made smaller to fit its screen.
const MARGIN: f64 = 8.0;
/// How long the window has to have stood still before it is fitted to the screen it is on.
const AT_REST: Duration = Duration::from_millis(250);

/// By how much the whole window is made smaller, so that it fits a screen with a work area
/// of `work` (width and height, in logical pixels): 1 where it fits as it is laid out, and
/// never more than that.
///
/// A laptop screen at a large scale has fewer logical pixels than the window is wide or
/// high, and a window that cannot be resized would hang over its edge, the one button with
/// it. The window is then made smaller as a whole, by one factor for its size and for the
/// page in it, so that the page stays as it is laid out.
pub(crate) fn fit(work: (f64, f64)) -> f64 {
    if work.0 >= LAID_OUT.0 && work.1 >= LAID_OUT.1 {
        return 1.0;
    }
    let room = |work: f64, laid_out: f64| (work - 2.0 * MARGIN) / laid_out;
    // A screen less than half the window's size is no screen this is for.
    room(work.0, LAID_OUT.0).min(room(work.1, LAID_OUT.1)).clamp(0.5, 1.0)
}

/// How the window was last fitted to its screen, and what it takes to fit it again when
/// it has been moved.
pub(crate) struct Fitted {
    /// The factor the window is at; see [`fit`].
    factor: Mutex<f64>,
    /// Counts the times the window was moved or came to another scale.
    moves: AtomicU64,
    /// Something is waiting for the window to come to rest.
    waiting: AtomicBool,
}

impl Default for Fitted {
    fn default() -> Fitted {
        Fitted { factor: Mutex::new(1.0), moves: AtomicU64::new(0), waiting: AtomicBool::new(false) }
    }
}

/// Fits the window to the screen it is on; see [`fit`]. With `centred` it is put in the
/// middle of that screen as well, as it is when the app starts.
pub(crate) fn fit_window(app: &AppHandle, centred: bool) {
    let Some(window) = app.get_webview_window("main") else { return };
    let Ok(Some(monitor)) = window.current_monitor() else { return };
    let (scale, area) = (monitor.scale_factor(), monitor.work_area().size);
    let on_screen = (f64::from(area.width) / scale, f64::from(area.height) / scale);
    let factor = fit(work_area().unwrap_or(on_screen));
    let fitted = app.state::<Fitted>();
    let before = std::mem::replace(&mut *fitted.factor.lock().unwrap(), factor);
    if (before - factor).abs() < 0.001 {
        return;
    }
    let _ = window.set_size(LogicalSize::new(LAID_OUT.0 * factor, LAID_OUT.1 * factor));
    let _ = window.set_zoom(factor);
    if centred {
        let _ = window.center();
    }
}

/// Has the window fitted to the screen it is on once it has come to rest there: it was
/// moved, or the scale of its screen has changed. While it is being dragged from one screen
/// to the next it is left as it is.
pub(crate) fn fit_when_at_rest(app: &AppHandle) {
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
            fit_window(&app, false);
            fitted.waiting.store(false, Ordering::SeqCst);
            // It was moved again in that very moment, and nobody else is waiting for it.
            if fitted.moves.load(Ordering::SeqCst) == seen || fitted.waiting.swap(true, Ordering::SeqCst) {
                break;
            }
        }
    });
}

const TRAY: &str = "main";

/// Puts the app's icon next to the clock, or takes it away again. While it is there, closing
/// the window only puts the window away.
///
/// The icon is made once, the first time it is wanted, and from then on only shown and
/// hidden. Tauri keeps the handler of an icon's menu for good, also after the icon is
/// removed: an icon made anew each time the setting is switched on would leave one more
/// handler behind, and 열기 and 종료 would then run once for each of them.
pub(crate) fn show_tray(app: &AppHandle, wanted: bool) {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    if let Some(tray) = app.tray_by_id(TRAY) {
        let _ = tray.set_visible(wanted);
        return;
    }
    if !wanted {
        return;
    }
    let menu = MenuItem::with_id(app, "open", "열기", true, None::<&str>).and_then(|open| {
        let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
        Menu::with_items(app, &[&open, &quit])
    });
    let Ok(menu) = menu else { return };
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
    let _ = tray.build(app);
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

/// Shows the window, in front: for the first time, or again.
pub(crate) fn bring_back(app: &AppHandle) {
    SHOWN.store(true, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Ends the app for good. A run that is at work is asked to stop first, and the app goes
/// when it has: the same as closing the window does without the icon.
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

    /// The work area of a screen of `pixels` at `scale` percent, in logical pixels, with
    /// the taskbar of Windows 11 along its lower edge.
    fn work_area(pixels: (f64, f64), scale: f64) -> (f64, f64) {
        (pixels.0 * 100.0 / scale, pixels.1 * 100.0 / scale - 48.0)
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
}
