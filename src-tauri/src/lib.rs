//! The backend of the app: the commands the page calls, and what runs by itself while the
//! window is open. This file starts it all; the modules do the work.

mod address;
mod clock;
mod desk;
mod flow;
mod game;
mod harness;
mod log;
#[cfg(test)]
mod made;
mod maintenance;
mod measure;
mod offer;
mod patching;
mod ready;
mod records;
#[cfg(test)]
mod scratch;
mod settings;
mod shown;
mod state;
mod update;
#[cfg(windows)]
mod wide;
mod window;
mod wording;

use tauri::Manager;

use desk::open_desks;
use flow::OnClose;
use log::{keep_log_in, log_line, speak_up_on_failure};
use maintenance::watch_maintenance;
use state::Patcher;
use update::watch_updates;
use window::{
    Fitted, ask_for_webview, bring_back, closed, open_window, settle_tray, settle_when_at_rest, show_at_the_latest,
};

pub fn run() {
    speak_up_on_failure();
    #[cfg(windows)]
    {
        if tauri::webview_version().is_err() {
            ask_for_webview();
            return;
        }
    }
    // Everything the app keeps is in one folder: the usual one, or the one it was started
    // with (see `harness`).
    let named = harness::data_dir(std::env::var_os(harness::DATA_DIR));
    let mut builder = tauri::Builder::default();
    // Starting the app a second time brings the window that is already open to the front.
    // Not so for a copy that was given a data folder of its own: that one is there to run
    // beside the usual one, and beside others like it.
    if named.is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _, _| {
            bring_back(app);
        }));
    }
    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        // How the window is fitted to its screen is known from the first moment: the window
        // may be moved before anything else is set up.
        .manage(Fitted::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let data = match named {
                Some(named) => named,
                None => handle.path().app_local_data_dir()?,
            };
            keep_log_in(&data);
            // The first line of every start says which version it is.
            log_line(&format!("started {}", handle.package_info().version));
            let patcher = Patcher::new(data);
            // Only a build made for trying the app out has anything to say here.
            for site in &patcher.sites {
                if let Some(told) = site.stand_ins_told() {
                    log_line(&format!("{}: {told}", site.client.server.key()));
                }
            }
            // The window is there by now. Nothing it asks for is answered before this
            // returns, so what answers it is in place in time.
            app.manage(patcher);
            // The window opens at the size it was left at, and a screen that is smaller
            // than the window gets a smaller window.
            open_window(&handle);
            // The window is made hidden, and shows when its page has drawn; see `window`.
            show_at_the_latest(&handle);
            settle_tray(&handle);
            // From here on the app looks after the game by itself, whatever becomes of the
            // window.
            open_desks(&handle);
            watch_updates(handle.clone());
            watch_maintenance(&handle);
            Ok(())
        })
        // Closing the window during a run would cut it off. The user is asked whether the
        // run is to go on without the window; a run that is to stop is asked to stop first
        // (an apply that has begun is finished), and the app exits when it has.
        .on_window_event(|window, event| {
            use tauri::WindowEvent::{Moved, Resized, ScaleFactorChanged};
            // The window may have landed on another screen, or its screen has another scale,
            // or the user has given it another size.
            if matches!(event, Moved(_) | Resized(_) | ScaleFactorChanged { .. }) {
                settle_when_at_rest(window.app_handle());
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                match closed(app) {
                    // With the icon next to the clock, closing the window puts it away and
                    // nothing stops.
                    OnClose::Hide => {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                    // The question is on the page. A window that was closed from the taskbar
                    // while it was made small comes up with it.
                    OnClose::Ask => {
                        api.prevent_close();
                        let _ = window.unminimize();
                        let _ = window.set_focus();
                    }
                    // A run that has ended in this very moment is not waited for: the window
                    // closes.
                    OnClose::StopThenLeave => {
                        if patching::stop(app, &app.state::<Patcher>(), true) {
                            api.prevent_close();
                        }
                    }
                    OnClose::Leave => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            desk::snapshot,
            desk::check_again,
            desk::dismiss_run,
            patching::start_patch,
            patching::start_patch_at,
            patching::confirm_patch_address,
            patching::stop_patch,
            ready::pick_install_dir,
            ready::reset_install_dir,
            ready::discard_leftovers,
            game::launch_game,
            game::open_download_page,
            settings::set_settings,
            measure::run_report,
            update::open_release_page,
            window::page_drawn,
            window::answer_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
