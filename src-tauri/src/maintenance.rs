//! The watch on the game's maintenance: the server status on the screen, and the word that
//! a maintenance is over.

use std::time::{Duration, Instant};

use mfp_engine::download;
use mfp_kms::notice;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::state::{Patcher, Site};
use crate::window::notify;

/// Whether a client's game can be played right now, as the page shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Lamp {
    /// No maintenance is underway.
    open: bool,
    /// The maintenance underway has been extended.
    extended: bool,
    /// When the maintenance underway is planned to end, as a clock shows it: `10:00`. Left
    /// out once that time has passed and the maintenance goes on.
    until: Option<String>,
    /// The game is within one of its operating periods. The test world has times when it
    /// is not: it is closed altogether then, and that is all there is to say of it.
    season: bool,
}

/// What a site knows of when its game is closed, as last read from what Nexon publishes.
/// Nothing of it is known until something could be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Told {
    /// What the page shows of it.
    pub(crate) lamp: Option<Lamp>,
    /// Whether the game can be played.
    pub(crate) open: Option<bool>,
    /// Whether a maintenance is underway, which is when a patch comes out.
    pub(crate) underway: Option<bool>,
}

impl Told {
    /// What is known at `now`, going by a reading of what Nexon last said. `in_season` is
    /// what the client's board says of its operating periods, and `seen_into` whether
    /// Nexon's service lists this client's maintenance.
    ///
    /// That a game can be played is only said where it can be seen: of a client the service
    /// does not list, the page is shown nothing while nothing is known to be in the way.
    fn at(reading: &notice::Reading, now: notice::Minutes, in_season: bool, seen_into: bool) -> Told {
        let underway = reading.underway.as_ref();
        let lamp = match underway {
            _ if !in_season => Some(Lamp { open: false, extended: false, until: None, season: false }),
            Some(closed) => Some(Lamp {
                open: false,
                extended: closed.extended,
                until: (closed.ends > now).then(|| notice::clock(closed.ends)),
                season: true,
            }),
            None => seen_into.then_some(Lamp { open: true, extended: false, until: None, season: true }),
        };
        Told { lamp, open: Some(underway.is_none() && in_season), underway: Some(underway.is_some()) }
    }
}

/// How often Nexon is asked: seldom, and often while a maintenance is underway.
const SELDOM: u64 = 10 * 60;
const OFTEN: u64 = 30;
/// The longest wait between two rounds of a maintenance while the rounds fail.
const SLOWEST: u64 = 5 * 60;
/// An account that cannot be read at the moment stands for this long.
const STANDS: Duration = Duration::from_secs(20 * 60);

/// How many seconds lie between two rounds of asking Nexon, before this copy's own delay
/// is added: `soon` says that a maintenance is underway, `failed_rounds` how many rounds in
/// a row could not read one of the two accounts.
///
/// During a maintenance every copy asks every half minute. A server that cannot answer is
/// not helped by that: while rounds fail, each wait is twice the one before, up to five
/// minutes. The first round that works brings the pace back.
fn round_wait(soon: bool, failed_rounds: u32) -> u64 {
    match failed_rounds {
        failed if soon && failed > 0 => (OFTEN << (failed - 1).min(4)).min(SLOWEST),
        _ if soon => OFTEN,
        _ => SELDOM,
    }
}

/// What Nexon last said about the game's maintenance, by each of its two accounts, and when
/// each was read.
#[derive(Default)]
struct Heard {
    planned: Option<(Instant, Vec<notice::Planned>)>,
    board: Option<(Instant, Board)>,
}

/// What a client's board says: its notices, and whether its page says that the game is
/// closed between two of its operating periods.
struct Board {
    notices: Vec<notice::Notice>,
    out_of_season: bool,
}

/// Follows the maintenance of every client's game for as long as the app is open: for the
/// lamp on the screen and for the word that it is over. A thread for each client.
///
/// Only what Nexon publishes for everyone is read; the game's servers are not asked. It is
/// read seldom, and often only while a maintenance is underway. Between two readings what
/// was read is held against the clock every second: the hours of a maintenance are listed
/// before it begins, and the lamp turns at the minute they say.
pub(crate) fn watch_maintenance(app: &AppHandle) {
    for place in 0..app.state::<Patcher>().sites.len() {
        let app = app.clone();
        std::thread::spawn(move || watch(&app, place));
    }
}

/// Follows the maintenance of the game of the site at `place`.
fn watch(app: &AppHandle, place: usize) {
    let patcher = app.state::<Patcher>();
    let site = &patcher.sites[place];
    let mut watch = notice::Watch::default();
    let mut heard = Heard::default();
    let mut unread = false;
    // Rounds in a row in which one of the two accounts could not be read, and whether
    // the rounds have been set further apart for it.
    let mut failed_rounds: u32 = 0;
    let mut slowed = false;
    loop {
        // A client that is not installed has no maintenance anyone here waits for: nothing
        // is read for it, and what was read for a folder that is gone is put aside.
        if !site.flow.lock().unwrap().askable() {
            if std::mem::take(&mut *site.told.lock().unwrap()) != Told::default() {
                site.wake_desk();
            }
            (watch, heard) = (notice::Watch::default(), Heard::default());
            (unread, failed_rounds, slowed) = (false, 0, false);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        // The two accounts of the game's maintenance: Nexon's inspection service, where
        // it lists this client, and the client's notice board.
        let asked =
            site.inspection().map(|(url, service)| download::soap(url, service.action, service.request, 1 << 20));
        let read = download::page(site.board(), 1 << 20);
        let not_asked = asked.as_ref().and_then(|asked| asked.as_ref().err());
        let failure = not_asked.or(read.as_ref().err()).map(ToString::to_string);
        if let (Some(failure), false) = (&failure, unread) {
            let which = if not_asked.is_some() { "from the inspection service" } else { "from the notice board" };
            site.log(&format!("could not read the maintenance information {which}: {failure}"));
        }
        unread = failure.is_some();
        failed_rounds = if unread { failed_rounds.saturating_add(1) } else { 0 };
        let first = heard.planned.is_none() && heard.board.is_none();
        if let Some(Ok(answer)) = asked {
            heard.planned = Some((Instant::now(), notice::read_inspection(&answer)));
        }
        if let Ok(page) = read {
            let board = Board { notices: notice::read_board(&page), out_of_season: notice::out_of_season(&page) };
            heard.board = Some((Instant::now(), board));
        }
        if first {
            let listed = heard.planned.as_ref().map(|(_, planned)| planned.len());
            let on_board = heard.board.as_ref().map(|(_, board)| board.notices.len());
            match (site.inspection().is_some(), listed, on_board) {
                (true, Some(listed), Some(on_board)) => site
                    .log(&format!("maintenance information read: {listed} listed by the service, {on_board} notices")),
                // The service does not list this client: its board is all there is to read.
                (false, _, Some(on_board)) => site.log(&format!("maintenance information read: {on_board} notices")),
                _ => {}
            }
        }

        let mut soon = hold_against_clock(app, site, &mut watch, &heard);
        // Every copy of the app waits a little longer by an amount of its own, so that
        // they do not all ask at the same moment.
        let (pace, spread) = if soon { (OFTEN, 10) } else { (SELDOM, 60) };
        let wait = round_wait(soon, failed_rounds);
        if (wait > pace) != slowed {
            slowed = wait > pace;
            let how = if slowed { "less often while it cannot be read" } else { "at the usual pace again" };
            site.log(&format!("asking for the maintenance information {how}"));
        }
        let mut wait = wait + own_delay(spread);
        let mut waited = 0;
        while waited < wait && site.flow.lock().unwrap().askable() {
            std::thread::sleep(Duration::from_secs(1));
            waited += 1;
            let was_underway = soon;
            soon = hold_against_clock(app, site, &mut watch, &heard);
            // A maintenance that was listed has begun by the clock. From here on Nexon
            // is asked at the pace of a maintenance, not when the long wait is over.
            if soon && !was_underway {
                wait = wait.min(waited + round_wait(true, failed_rounds) + own_delay(10));
            }
        }
    }
}

/// Holds what Nexon last said against the clock: sets what `site` knows of its game's
/// maintenance, and says so when a maintenance has been extended or is over. Nexon is not
/// asked. Says whether a maintenance is underway.
fn hold_against_clock(app: &AppHandle, site: &Site, watch: &mut notice::Watch, heard: &Heard) -> bool {
    let planned = heard.planned.as_ref().filter(|(at, _)| at.elapsed() < STANDS).map(|(_, found)| found.as_slice());
    let board = heard.board.as_ref().filter(|(at, _)| at.elapsed() < STANDS).map(|(_, board)| board);
    if planned.is_none() && board.is_none() {
        if std::mem::take(&mut *site.told.lock().unwrap()) != Told::default() {
            site.wake_desk();
        }
        return false;
    }
    let now = notice::now();
    let notices = board.map(|board| board.notices.as_slice());
    let reading = watch.read(planned.unwrap_or_default(), notices.unwrap_or_default(), now);
    let in_season = !board.is_some_and(|board| board.out_of_season);
    let told = Told::at(&reading, now, in_season, site.inspection().is_some());
    let before = std::mem::replace(&mut *site.told.lock().unwrap(), told.clone());
    if before != told {
        match &reading.underway {
            _ if !in_season => site.log("closed between two operating periods"),
            Some(closed) => {
                let until = notice::clock(closed.ends);
                site.log(&format!("maintenance underway until {until}: {}", closed.title));
            }
            None => site.log("no maintenance underway"),
        }
        // The desk shows the lamp, and asks the server by what it says.
        site.wake_desk();
    }
    let alert = app.state::<Patcher>().settings.lock().unwrap().maintenance_alert;
    if let Some(ends) = reading.extended {
        let until = notice::clock(ends);
        site.log(&format!("maintenance extended until {until}"));
        if alert {
            announce(app, site, &format!("점검이 {until}까지 연장되었습니다."));
        }
    }
    if reading.ended {
        site.log("maintenance over");
        if alert {
            announce(app, site, "점검이 완료되었습니다.");
        }
    }
    reading.soon
}

/// A number of seconds up to `most` that differs from one copy of the app and one moment to
/// the next.
pub(crate) fn own_delay(most: u64) -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (u64::from(now.subsec_nanos()) ^ u64::from(std::process::id()).wrapping_mul(2_654_435_761)) % (most + 1)
}

/// Tells the user something about the maintenance of the game of `site`: a Windows
/// notification, the taskbar button flashing until they come back to the window, and one
/// sound. The sound is heard even while Windows holds notifications back, as it does during
/// a full-screen video or game.
fn announce(app: &AppHandle, site: &Site, text: &str) {
    notify(app, site, text);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Critical));
    }
    #[cfg(windows)]
    // SAFETY: the call takes a constant and plays one of the system's sounds.
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK;
        MessageBeep(MB_ICONASTERISK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minutes since 1970 of a time of day in Korea, on a day of the tests' own.
    fn at(hour: i64, minute: i64) -> notice::Minutes {
        20_000 * 1440 + hour * 60 + minute
    }

    /// What the inspection service lists ahead of a patch day, read once at ten to four.
    fn listed() -> Vec<notice::Planned> {
        let text = "[패치중] 11/26(목) ver1.2.422 클라이언트 패치(04:00~10:00)".to_owned();
        vec![notice::Planned { begins: at(4, 0), ends: at(10, 0), text }]
    }

    /// What the live game's site knows by a reading: it is always in season, and Nexon's
    /// service lists it.
    fn live(reading: &notice::Reading, now: notice::Minutes) -> Told {
        Told::at(reading, now, true, true)
    }

    fn open() -> Lamp {
        Lamp { open: true, extended: false, until: None, season: true }
    }

    /// Nexon is asked once. What it said is held against the clock from then on.
    #[test]
    fn a_maintenance_that_was_listed_closes_the_game_at_its_minute_without_asking_again() {
        let listed = listed();
        let mut watch = notice::Watch::default();
        for minute in [50, 59] {
            let reading = watch.read(&listed, &[], at(3, minute));
            assert!(!reading.soon, "03:{minute}");
            let told = Told { lamp: Some(open()), open: Some(true), underway: Some(false) };
            assert_eq!(live(&reading, at(3, minute)), told);
        }
        let reading = watch.read(&listed, &[], at(4, 0));
        assert!(reading.soon);
        let closed = Lamp { open: false, extended: false, until: Some("10:00".to_owned()), season: true };
        let told = Told { lamp: Some(closed), open: Some(false), underway: Some(true) };
        assert_eq!(live(&reading, at(4, 0)), told);
    }

    /// The service alone says nothing of a maintenance being over, so its hours are what
    /// there is to go by, and the watch says that it is over once.
    #[test]
    fn a_maintenance_that_only_the_service_lists_is_over_at_its_minute() {
        let listed = listed();
        let mut watch = notice::Watch::default();
        assert!(watch.read(&listed, &[], at(9, 59)).underway.is_some());
        let reading = watch.read(&listed, &[], at(10, 0));
        assert!(reading.ended && !reading.soon);
        assert_eq!(live(&reading, at(10, 0)).open, Some(true));
        // Held against the clock again a second later, it is not over a second time.
        assert!(!watch.read(&listed, &[], at(10, 0)).ended);
    }

    /// The test world is not listed by Nexon's service: its board is all there is, and
    /// that says when it is closed, not that it is open.
    #[test]
    fn of_a_game_that_cannot_be_seen_into_only_what_closes_it_is_shown() {
        let check = notice::Notice {
            id: 195, title: "11/26(목) 전체 테스트월드 채널 점검 (15:50~16:10)".to_owned()
        };
        // 2026-11-26, the day in the notice, is 20783 days after the first of 1970.
        let now = |hour: i64, minute: i64| 20_783 * 1440 + hour * 60 + minute;
        let mut watch = notice::Watch::default();

        // Nothing is in the way as far as its board says. That is known, and not shown.
        let reading = watch.read(&[], std::slice::from_ref(&check), now(15, 0));
        let told = Told::at(&reading, now(15, 0), true, false);
        assert_eq!(told, Told { lamp: None, open: Some(true), underway: Some(false) });

        // A maintenance its board announces shows like any other.
        let reading = watch.read(&[], std::slice::from_ref(&check), now(15, 50));
        let closed = Lamp { open: false, extended: false, until: Some("16:10".to_owned()), season: true };
        let told = Told::at(&reading, now(15, 50), true, false);
        assert_eq!(told, Told { lamp: Some(closed), open: Some(false), underway: Some(true) });

        // Between two operating periods it is closed altogether: that is what shows,
        // whatever else its board says, and it is no maintenance.
        let off = Lamp { open: false, extended: false, until: None, season: false };
        for reading in [watch.read(&[], &[check], now(15, 55)), notice::Reading::default()] {
            let told = Told::at(&reading, now(15, 55), false, false);
            assert_eq!((told.lamp, told.open), (Some(off.clone()), Some(false)));
        }
        assert_eq!(Told::at(&notice::Reading::default(), now(15, 55), false, false).underway, Some(false));
    }

    /// What the page is told of the lamp, by the names it draws it by.
    #[test]
    fn the_lamp_is_told_under_the_names_the_page_draws_it_by() {
        let off = Lamp { open: false, extended: false, until: None, season: false };
        let told = serde_json::json!({ "open": false, "extended": false, "until": null, "season": false });
        assert_eq!(serde_json::to_value(off).unwrap(), told);
        let told = serde_json::json!({ "open": true, "extended": false, "until": null, "season": true });
        assert_eq!(serde_json::to_value(open()).unwrap(), told);
    }

    #[test]
    fn nexon_is_asked_often_during_a_maintenance_and_less_often_while_that_fails() {
        assert_eq!(round_wait(false, 0), 600);
        assert_eq!(round_wait(true, 0), 30);
        // Outside a maintenance a round that failed changes nothing: it is asked seldom anyway.
        assert_eq!(round_wait(false, 3), 600);
        let waits: Vec<u64> = (1..=7).map(|failed| round_wait(true, failed)).collect();
        assert_eq!(waits, [30, 60, 120, 240, 300, 300, 300]);
    }
}
