//! The backend of the app: the commands the page calls, and what runs by itself while the
//! window is open. This file starts it all; the modules do the work.

mod address;
mod clock;
mod desk;
mod diagnosis;
mod flow;
mod game;
mod harness;
mod launcher;
mod log;
#[cfg(test)]
mod made;
mod maintenance;
mod measure;
mod offer;
mod patching;
mod ready;
mod records;
mod rights;
#[cfg(test)]
mod scratch;
mod settings;
mod shown;
mod signed;
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
use rights::Handover;
use state::Patcher;
use update::watch_updates;
use window::{
    Fitted, ask_for_webview, bring_back, closed, make_window, open_window, settle_tray, settle_when_at_rest,
    show_at_the_latest,
};

pub fn run() {
    speak_up_on_failure();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // A copy that was started only to start the game's launcher with administrator rights
    // does that and is gone: it is not the app, and opens nothing; see `game`.
    if let Some(ended) = game::asked_to_press(&arguments) {
        std::process::exit(ended.code() as i32);
    }
    #[cfg(windows)]
    {
        if tauri::webview_version().is_err() {
            ask_for_webview();
            return;
        }
    }
    // A copy that was started to take over from another, with administrator rights, is told
    // so by the one it replaces; see `rights`.
    let handed = Handover::told(&arguments);
    // Everything the app keeps is in one folder: the usual one, or the one it was started
    // with (see `harness`). A copy that takes over keeps the folder of the one it replaces.
    let (named, beside) = match &handed {
        Some(handed) => (Some(handed.data.clone()), handed.beside),
        None => {
            let named = harness::data_dir(std::env::var_os(harness::DATA_DIR));
            let beside = named.is_some();
            (named, beside)
        }
    };
    // The copy that takes over waits here until the one it replaces is gone: from then on
    // it is the app, and there was no moment with two.
    rights::take_over(handed.as_ref(), beside);
    // A start by the user while the app is open with administrator rights, which this
    // start has none of: the window of that copy is brought back, and this one leaves.
    // Windows keeps such a start from reaching that copy in the way a second start
    // otherwise does, so it is asked here, before anything else is set up.
    if handed.is_none() && !beside && !rights::elevated() && rights::ask_to_show() {
        return;
    }
    let mut builder = tauri::Builder::default();
    // Starting the app a second time brings the window that is already open to the front.
    // Not so for a copy that was given a data folder of its own: that one is there to run
    // beside the usual one, and beside others like it.
    if !beside {
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
        .setup(move |app| {
            let handle = app.handle().clone();
            let data = match named {
                Some(named) => named,
                None => handle.path().app_local_data_dir()?,
            };
            keep_log_in(&data);
            // The first line of every start says which version it is.
            log_line(&format!("started {}", handle.package_info().version));
            let elevated = rights::elevated();
            if elevated {
                log_line("running with administrator rights");
            }
            let mut patcher = Patcher::new(data);
            patcher.version = handle.package_info().version.to_string();
            // Only a build made for trying the app out has anything to say here.
            for site in &patcher.sites {
                if let Some(told) = site.stand_ins_told() {
                    log_line(&format!("{}: {told}", site.client.server.key()));
                }
            }
            // A copy that takes over goes on with the session of the one it replaces: it is
            // told what that one remembered of it. It was started for administrator rights
            // and does not ask for them again, whether it got them or not.
            if let Some(handed) = &handed {
                handed.taken_up(&patcher);
                patcher.rights_declined.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // The game is to be started by itself, which takes administrator rights where
            // nobody is there to answer Windows' prompt: the app starts itself again with
            // them now, before it has a window, and this copy leaves. If the user says no,
            // it goes on as it is and does not ask again in this session.
            if rights::start_again_with_rights(&patcher) {
                handle.cleanup_before_exit();
                std::process::exit(0);
            }
            // What answers the page is in place before there is a page to ask.
            app.manage(patcher);
            // The window is made here and not by Tauri before this: see `make_window`. It
            // opens at the size it was left at, and a screen that is smaller than the
            // window gets a smaller window.
            make_window(&handle, elevated)?;
            open_window(&handle);
            // With administrator rights, a start of the app without them cannot reach this
            // copy as a second start does: it is listened for in a way that it can.
            if elevated && !beside {
                let app = handle.clone();
                rights::listen_for_starts(move || bring_back(&app));
            }
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
            rights::restart_with_rights,
            diagnosis::diagnose_again,
            diagnosis::ask_file_check,
            diagnosis::start_file_check,
            diagnosis::stop_file_check,
            update::open_bug_report_page,
            measure::run_report,
            update::open_release_page,
            window::page_drawn,
            window::answer_close
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        // A check of the game's files that is at work when the app closes is stopped, and
        // the app goes when it has stopped.
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event
                && let Some(patcher) = app.try_state::<Patcher>()
            {
                diagnosis::stop_checks(&patcher);
            }
        });
}
