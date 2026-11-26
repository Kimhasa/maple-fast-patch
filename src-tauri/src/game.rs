//! Starting the game, and opening the pages it is downloaded from.

use std::path::Path;
use std::time::{Duration, Instant};

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::desk::update;
use crate::records::{install_dir, is_registered_install};
use crate::state::{Patcher, Site};
#[cfg(windows)]
use crate::wide::to_wide;

/// Starts the game of `site`, for the button and for the app starting it by itself. With
/// the game started, the app goes when the user wants that.
pub(crate) fn start_game(app: &AppHandle, patcher: &Patcher, site: &Site) -> Result<(), String> {
    launch(app, site)?;
    // It is not started a second time by itself, and the button rests for a moment.
    update(app, patcher, site, |flow| flow.launched(Instant::now()));
    if patcher.settings.lock().unwrap().exit_after_start && patcher.running().is_none() {
        let app = app.clone();
        // The launcher gets a moment to come up before this window goes.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            app.exit(0);
        });
    }
    Ok(())
}

/// Starts the game of the client the page calls `server`.
#[tauri::command(async)]
pub(crate) fn launch_game(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or("설치 경로를 찾을 수 없습니다.")?;
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
    /// Not at all.
    Nothing,
}

/// How the game in an install folder is to be started: `has_launcher` says whether the
/// folder holds the client's launcher, `registered` whether it is the folder the Nexon
/// installer registered, of a client whose shortcut is known to work.
///
/// Nexon's shortcut starts the install that is registered, whatever folder is on the
/// screen. So it is for that folder and no other: for a copy, or for an install that was
/// made up to try the app out, it would start another game than the one that is shown.
fn way_to_start(has_launcher: bool, registered: bool) -> Start {
    match (has_launcher, registered) {
        (true, _) => Start::Launcher,
        (false, true) => Start::Shortcut,
        (false, false) => Start::Nothing,
    }
}

/// Starts the game with the launcher that sits in the install folder. Falls back to the way
/// Nexon's desktop shortcut starts it when the folder has no launcher.
fn launch(app: &AppHandle, site: &Site) -> Result<(), String> {
    let dir = install_dir(site).ok_or("설치 경로를 찾을 수 없습니다.")?;
    let launcher = dir.join(site.client.launcher);
    // What Nexon's software does with the address of a client this app was never tried on
    // is not known. Nothing is guessed there: such a client is started by its own launcher
    // or not at all.
    let by_shortcut = is_registered_install(site, &dir) && !site.untried();
    match way_to_start(launcher.is_file(), by_shortcut) {
        Start::Launcher => match std::process::Command::new(&launcher).current_dir(&dir).spawn() {
            Ok(_) => Ok(()),
            // The launcher asks for administrator rights; let Windows show its prompt.
            Err(err) if err.raw_os_error() == Some(740) => open_in_folder(&launcher, &dir),
            Err(_) => Err("게임 실행기를 시작하지 못했습니다.".to_owned()),
        },
        Start::Shortcut => app
            .opener()
            .open_url(site.client.launch_url(), None::<&str>)
            .or_else(|_| app.opener().open_url(site.client.game_page, None::<&str>))
            .map_err(|_| "게임 실행 페이지를 열 수 없습니다.".to_owned()),
        Start::Nothing => Err("게임 실행기를 찾을 수 없습니다.".to_owned()),
    }
}

/// Starts `program` the way a double click does, with `folder` as its working folder.
/// Windows asks for administrator rights itself when the program needs them.
fn open_in_folder(program: &Path, folder: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let (program, folder, verb) = (to_wide(program), to_wide(folder), to_wide("open"));
        // SAFETY: every pointer is to a NUL-terminated buffer that outlives the call.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                program.as_ptr(),
                std::ptr::null(),
                folder.as_ptr(),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 mean it started; the user declining the prompt is not an error
        // worth a message.
        match result as usize {
            code if code > 32 => Ok(()),
            5 => Ok(()),
            _ => Err("게임 실행기를 시작하지 못했습니다.".to_owned()),
        }
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

    #[test]
    fn a_folder_with_its_launcher_is_started_by_that_launcher() {
        assert_eq!(way_to_start(true, true), Start::Launcher);
        // A copy, or a made-up install with its stand-in for the launcher.
        assert_eq!(way_to_start(true, false), Start::Launcher);
    }

    #[test]
    fn nexons_shortcut_is_for_the_registered_install_and_for_no_other_folder() {
        assert_eq!(way_to_start(false, true), Start::Shortcut);
        // Whatever the folder holds, the address is not opened for it unless it is the
        // registered one: a folder without a launcher is then not started at all.
        for has_launcher in [true, false] {
            assert_ne!(way_to_start(has_launcher, false), Start::Shortcut);
        }
        assert_eq!(way_to_start(false, false), Start::Nothing);
    }
}
