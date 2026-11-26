//! The backend of the app: the commands the page calls, and what runs by itself while the
//! window is open. This file starts it all; the modules do the work.

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
use flow::{OnClose, on_close};
use log::{keep_log_in, log_line, speak_up_on_failure};
use maintenance::watch_maintenance;
use state::Patcher;
use update::watch_updates;
use window::{Fitted, ask_for_webview, bring_back, fit_when_at_rest, fit_window, show_at_the_latest, show_tray};

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
            let background = patcher.settings.lock().unwrap().background;
            // The window is there by now. Nothing it asks for is answered before this
            // returns, so what answers it is in place in time.
            app.manage(patcher);
            // A screen that is smaller than the window gets a smaller window.
            fit_window(&handle, true);
            // The window is made hidden, and shows when its page has drawn; see `window`.
            show_at_the_latest(&handle);
            show_tray(&handle, background);
            // From here on the app looks after the game by itself, whatever becomes of the
            // window.
            open_desks(&handle);
            watch_updates(handle.clone());
            watch_maintenance(&handle);
            Ok(())
        })
        // Closing the window during a run would cut it off. The run is asked to stop first
        // (an apply that has begun is finished), and the app exits when it has.
        .on_window_event(|window, event| {
            // The window may have landed on another screen, or its screen has another scale.
            if matches!(event, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::ScaleFactorChanged { .. }) {
                fit_when_at_rest(window.app_handle());
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let patcher = window.state::<Patcher>();
                let background = patcher.settings.lock().unwrap().background;
                match on_close(background, patcher.running().is_some()) {
                    // With the icon next to the clock, closing the window puts it away and
                    // nothing stops.
                    OnClose::Hide => {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                    // A run that has ended in this very moment is not waited for: the window
                    // closes.
                    OnClose::StopThenLeave => {
                        if patching::stop(window.app_handle(), &patcher, true) {
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
            patching::stop_patch,
            ready::set_install_dir,
            ready::discard_leftovers,
            patching::foreign_origin,
            game::launch_game,
            game::open_download_page,
            settings::set_settings,
            measure::run_report,
            update::open_release_page,
            window::page_drawn
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
