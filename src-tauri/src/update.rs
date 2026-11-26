//! Word of a newer version of this app.

use std::time::Duration;

use mfp_engine::download;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::desk::tell_app;
use crate::log::log_line;
use crate::maintenance::own_delay;
use crate::state::Patcher;

/// Word of a newer version of this app than the one running.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Update {
    /// The newer version, as its home writes it: `0.2.0`.
    latest: String,
    /// The version running is older than the oldest one that is still to be used.
    pub(crate) required: bool,
    /// A line that goes with it, when the home gives one.
    message: Option<String>,
}

/// Where this app's home says which version is the newest, and where a newer one is to be
/// had. Both are fixed here: nothing that is read from the network decides what is opened.
const NOTICE_URL: &str = "https://raw.githubusercontent.com/Kimhasa/maple-fast-patch/HEAD/notice.json";
const RELEASES_URL: &str = "https://github.com/Kimhasa/maple-fast-patch/releases/latest";

/// A version as the notice may give it: up to three numbers of up to five digits each, with
/// dots between them. Anything else is not taken for a version.
fn plain_version(text: &str) -> Option<[u64; 3]> {
    let mut version = [0; 3];
    let mut given = text.split('.');
    for (part, digits) in version.iter_mut().zip(&mut given) {
        if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
            return None;
        }
        *part = digits.parse().ok()?;
    }
    given.next().is_none().then_some(version)
}

/// What the notice file says to someone running version `running`: that there is a newer
/// one, or nothing.
///
/// The file is data and is read as nothing else: two versions and a line of text, each of
/// which may be missing. A version that is not plain numbers voids the whole file. The text
/// is cut to what fits a line and stripped of everything that is not to be seen, and goes
/// to the screen as text.
fn read_notice(page: &str, running: [u64; 3]) -> Result<Option<Update>, String> {
    #[derive(serde::Deserialize)]
    struct Notice {
        latest: Option<String>,
        minimum: Option<String>,
        message: Option<String>,
    }
    const MESSAGE_MOST: usize = 120;
    let notice: Notice = serde_json::from_str(page).map_err(|err| err.to_string())?;
    // A version with the text it was given as, or nothing where the file gives none.
    let version = |given: Option<String>| -> Result<Option<([u64; 3], String)>, String> {
        let Some(text) = given else { return Ok(None) };
        let version = plain_version(&text).ok_or("a version that is not plain numbers")?;
        Ok(Some((version, text)))
    };
    let (latest, minimum) = (version(notice.latest)?, version(notice.minimum)?);
    let required = minimum.as_ref().is_some_and(|(minimum, _)| running < *minimum);
    let message = notice
        .message
        .map(|text| {
            // Line breaks go with the control characters: the two that Unicode has of its own
            // are not counted among those.
            let seen =
                text.chars().filter(|letter| !letter.is_control() && !matches!(*letter, '\u{2028}' | '\u{2029}'));
            seen.take(MESSAGE_MOST).collect::<String>().trim().to_owned()
        })
        .filter(|text| !text.is_empty());
    // The version to get is the newer of the two, should the file name an oldest one that
    // is newer than its newest.
    let newest = [latest, minimum].into_iter().flatten().max_by_key(|(version, _)| *version);
    Ok(newest.filter(|(version, _)| running < *version).map(|(_, latest)| Update { latest, required, message }))
}

/// Looks once in a while whether there is a newer version of this app: soon after the app
/// is started, and then once a day for as long as it stays open. A look that failed is
/// made again half an hour later.
///
/// That is all it does. Nothing is downloaded or installed; the screen gets a line that
/// says so, and the user decides.
pub(crate) fn watch_updates(app: AppHandle) {
    const DAY: u64 = 24 * 60 * 60;
    const AGAIN: u64 = 30 * 60;
    std::thread::spawn(move || {
        let patcher = app.state::<Patcher>();
        let running = &app.package_info().version;
        let running = [running.major, running.minor, running.patch];
        let mut unread = false;
        // Not all copies of the app ask in the same second after they are started.
        let mut wait = own_delay(60);
        loop {
            std::thread::sleep(Duration::from_secs(wait));
            // Not during a run, which has the line and the drive to itself. It is looked at
            // again in a minute.
            if patcher.running().is_some() {
                wait = 60;
                continue;
            }
            let read = download::page(NOTICE_URL, 16 << 10).map_err(|err| err.to_string());
            let read = read.and_then(|page| read_notice(&page, running));
            wait = if read.is_ok() { DAY } else { AGAIN };
            match read {
                Ok(update) => {
                    let before = std::mem::replace(&mut *patcher.update.lock().unwrap(), update.clone());
                    // The page shows it, and a version that is too old to patch with
                    // changes what every screen says and offers.
                    if before != update {
                        tell_app(&app, &patcher);
                    }
                }
                // Nothing of this shows on the screen, and the log has it once.
                Err(why) if !unread => {
                    unread = true;
                    log_line(&format!("could not read whether there is a newer version: {why}"));
                }
                Err(_) => {}
            }
        }
    });
}

/// Opens the page a newer version of this app is downloaded from.
#[tauri::command]
pub(crate) fn open_release_page(app: AppHandle) -> Result<(), String> {
    app.opener().open_url(RELEASES_URL, None::<&str>).map_err(|_| "다운로드 페이지를 열 수 없습니다.".to_owned())
}
