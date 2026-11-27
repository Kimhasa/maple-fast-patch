//! The desk of a site: what looks at the client's install folder, asks the server for
//! patches, and does what the app does by itself, for as long as the app is open. The page
//! is told what the screen says and draws it.
//!
//! Nothing here waits for the page. A window that is put away has its clocks slowed, and a
//! page that has stopped has none: what the desk does goes on whatever becomes of either.
//! The rules it goes by are in [`crate::flow`], and what the screen says in [`crate::shown`].

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mfp_kms::notice;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::diagnosis::{DiagnosisView, diagnose_if_due, shown_of};
use crate::flow::{Decision, Flow, Phase, Progress, RefusalKind, Run, World, calm, decide};
use crate::game::start_game;
use crate::maintenance::{Lamp, own_delay};
use crate::offer::availability;
use crate::patching::{Wanted, begin};
use crate::ready::{heal_begins, look, put_back, read_begins, read_patch};
use crate::settings::Settings;
use crate::shown::{Button, Row, shown};
use crate::state::{Patcher, Site, Status};
use crate::update::Update;

/// How long the desk rests between two rounds when nothing wakes it.
const ROUND: Duration = Duration::from_secs(1);

/// What the page is told about one client: everything its screen says, as it is to be drawn.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteView {
    /// Which client: `live`, `test`.
    server: &'static str,
    /// Counts what the page was told about this client. The page keeps what has the highest
    /// number and drops what comes with a lower one.
    rev: u64,
    /// What the game is called.
    title: &'static str,
    current: Option<String>,
    latest: Option<String>,
    /// The address the user is given to begin from when they state where the patch is.
    patch_address: String,
    /// When the server last answered a check, in milliseconds since 1970.
    checked_at: Option<u64>,
    lamp: Option<Lamp>,
    rows: Vec<Row>,
    custom_dir: bool,
    half_patched: bool,
    /// Removing what a run left cannot be made good by this app: the folder is partly new,
    /// or an apply that was cut off in it cannot be undone. The user is asked first.
    discard_asks: bool,
    phase: Phase,
    run: Option<Run>,
    percent: Option<f64>,
    status: String,
    status_bad: bool,
    busy: bool,
    summary: Option<String>,
    button: Button,
    /// The client whose run keeps this one from starting: a run is at work, and it is that
    /// client's.
    locked: Option<&'static str>,
    /// The diagnosis of the PC, for the page of its own. Nothing for a client without a
    /// folder: there is no tab for it there.
    diagnosis: Option<DiagnosisView>,
}

impl SiteView {
    /// The view without what a run at work changes many times a second: its figures and the
    /// file it is on. Those reach the page as the run says them (see [`run_said`]), and the
    /// whole view only when something else in it has changed.
    fn steady(&self) -> String {
        let mut steady = self.clone();
        steady.rev = 0;
        if self.phase.at_work() {
            steady.percent = None;
            if let Some(run) = &mut steady.run {
                (run.done, run.total, run.worked) = (0, 0, 0);
                run.activity.clear();
            }
        }
        serde_json::to_string(&steady).unwrap_or_default()
    }
}

/// What the page is told that is not of one client: what the user has set, word of a newer
/// version of the app, and which client a run is at work for.
#[derive(Clone, Serialize)]
pub(crate) struct AppWord {
    settings: Settings,
    update: Option<Update>,
    running: Option<&'static str>,
}

/// Everything the page shows, as it stands now.
#[derive(Serialize)]
pub(crate) struct AppView {
    sites: Vec<SiteView>,
    #[serde(flatten)]
    app: AppWord,
}

/// Where a run is, with the client it is a run for.
#[derive(Clone, Serialize)]
struct ProgressWord {
    server: &'static str,
    #[serde(flatten)]
    progress: Progress,
}

fn app_word(patcher: &Patcher) -> AppWord {
    AppWord {
        settings: *patcher.settings.lock().unwrap(),
        update: patcher.update.lock().unwrap().clone(),
        running: patcher.running().map(|site| site.client.server.key()),
    }
}

/// What the screen of a site says, and what its rules go by, that is not the site's own.
pub(crate) struct Outside {
    pub(crate) world: World,
    lamp: Option<Lamp>,
    /// What the page is told of the site's diagnosis, and the line its own page shows of
    /// one that found no ordinary PC; see [`crate::diagnosis`].
    diagnosis: Option<DiagnosisView>,
    seat: Option<Row>,
    /// The client whose run keeps this one from starting.
    locked: Option<&'static str>,
}

/// What is outside `site` now; see [`Outside`]. Gathered before the site's flow is held, so
/// that nothing else is asked for under it.
pub(crate) fn outside(patcher: &Patcher, site: &Site) -> Outside {
    let settings = *patcher.settings.lock().unwrap();
    let told = site.told.lock().unwrap().clone();
    // Whether the app's home was asked is read before what it said: once it was asked, its
    // word is in place (see `watch_updates`), and is not read as it was a moment before.
    let update_asked = patcher.update_asked.load(Ordering::SeqCst);
    let (diagnosis, seat) = shown_of(patcher, site);
    let checking_files = diagnosis.as_ref().is_some_and(DiagnosisView::checking);
    let world = World {
        auto_patch: settings.auto_patch,
        auto_start: settings.auto_start,
        outdated: patcher.update.lock().unwrap().as_ref().is_some_and(|update| update.required),
        // A check of the game's files holds the folder as a run does: nothing is begun
        // in it by itself meanwhile.
        run_at_work: patcher.turn_taken() || checking_files,
        open: told.open,
        sees_open: site.inspection().is_some(),
        checking_files,
        update_asked,
        now: Instant::now(),
    };
    let locked = patcher.holding(site).map(|other| other.client.server.key());
    Outside { world, lamp: told.lamp, locked, diagnosis, seat }
}

/// Whether there is nothing to hurry for with the checks of `site` now; see [`calm`].
fn calm_of(site: &Site) -> Option<bool> {
    let underway = site.told.lock().unwrap().underway;
    calm(underway, site.patch_hours().is_some_and(|hours| hours.hold(notice::now())))
}

/// What the screen of `site` says now.
fn site_view(site: &Site, flow: &Flow, outside: &Outside) -> SiteView {
    let shown = shown(&flow.facts(&outside.world));
    SiteView {
        server: site.client.server.key(),
        rev: flow.rev,
        title: site.client.title,
        current: shown.current,
        latest: shown.latest,
        patch_address: shown.address,
        checked_at: flow.checked_at,
        lamp: outside.lamp.clone(),
        // What the diagnosis says of a PC that is no ordinary one stands last: it is
        // for knowing, and nothing in the way.
        rows: shown.rows.into_iter().chain(outside.seat.clone()).collect(),
        custom_dir: flow.status.as_ref().is_some_and(|status| status.custom_dir),
        half_patched: flow.status.as_ref().is_some_and(|status| status.half_patched),
        discard_asks: flow.status.as_ref().is_some_and(Status::discard_asks),
        phase: flow.phase,
        run: flow.run.clone(),
        percent: shown.percent,
        status: shown.status,
        status_bad: shown.status_bad,
        busy: shown.busy,
        summary: shown.summary,
        button: shown.button,
        locked: outside.locked,
        diagnosis: outside.diagnosis.clone(),
    }
}

/// Everything the page shows, for a page that has just opened or was not looked at for a
/// while. From then on it is told of every change.
#[tauri::command(async)]
pub(crate) fn snapshot(patcher: State<Patcher>) -> AppView {
    let view = |site: &Site| {
        let outside = outside(&patcher, site);
        site_view(site, &site.flow.lock().unwrap(), &outside)
    };
    AppView { sites: patcher.sites.iter().map(view).collect(), app: app_word(&patcher) }
}

/// Asks the server again at once, for the user who does not want to wait after a check that
/// failed. What was found before is put aside.
#[tauri::command(async)]
pub(crate) fn check_again(app: AppHandle, patcher: State<Patcher>, server: String) {
    let Some(site) = patcher.site_named(&server) else { return };
    site.forget_found();
    update(&app, &patcher, site, |flow| flow.ask_again(Instant::now()));
}

/// Puts the run that ended away and shows the ready screen again.
#[tauri::command(async)]
pub(crate) fn dismiss_run(app: AppHandle, patcher: State<Patcher>, server: String) {
    let Some(site) = patcher.site_named(&server) else { return };
    update(&app, &patcher, site, Flow::dismiss);
}

/// Looks at the install folder of `site` now, takes in what is found and tells the page.
///
/// The desk does this by its clock. Whoever begins a run or changes the folder does it for
/// the moment at which what is known must not be a few seconds old.
pub(crate) fn look_now(app: &AppHandle, patcher: &Patcher, site: &Site) -> Status {
    let _one_at_a_time = site.looking.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let status = look(site, patcher.running_for(site), patcher.settings.lock().unwrap().immediate);
    update(app, patcher, site, |flow| {
        if flow.take_look(status.clone(), Instant::now()) {
            // What was found for the folder before is of no use here, and a check that is
            // still about it gives up. Said before the desk is woken: the check it then
            // begins for this folder is not the one that is told to stop.
            site.forget_found();
            site.check_stop.store(true, Ordering::SeqCst);
        }
    });
    status
}

/// Changes what `site` knows, tells the page if its screen says something else for it, and
/// wakes the desk so that what follows from the change does not wait for the next round.
pub(crate) fn update<T>(app: &AppHandle, patcher: &Patcher, site: &Site, with: impl FnOnce(&mut Flow) -> T) -> T {
    let outside = outside(patcher, site);
    let mut flow = site.flow.lock().unwrap();
    let outcome = with(&mut flow);
    tell(app, site, &mut flow, &outside);
    flow.wake();
    drop(flow);
    site.woken.notify_all();
    outcome
}

/// Tells the page what the screen of `site` says, if that is anything else than it was last
/// told. Said while the site's flow is held, so that what is told comes in the order it is
/// counted in.
fn tell(app: &AppHandle, site: &Site, flow: &mut Flow, outside: &Outside) {
    let mut view = site_view(site, flow, outside);
    let steady = view.steady();
    if flow.told.as_ref() != Some(&steady) {
        flow.told = Some(steady);
        flow.rev += 1;
        view.rev = flow.rev;
        let _ = app.emit("site", view);
    }
}

/// Tells the page what is not of one client, when that has changed: a setting, word of a
/// newer version, a run that has begun or ended. Every desk is woken with it: what a
/// screen says and what the app does by itself go by the same, and a run of one client
/// shows on the screen of the other.
pub(crate) fn tell_app(app: &AppHandle, patcher: &Patcher) {
    let _ = app.emit("app", app_word(patcher));
    patcher.sites.iter().for_each(Site::wake_desk);
}

/// Takes in where the run at work for `site` says it is, and tells the page. The whole
/// screen is told as well when its words change with it: at a new step (`urgent`), and when
/// a download that had gone quiet brings something again.
pub(crate) fn run_said(app: &AppHandle, patcher: &Patcher, site: &Site, progress: Progress, urgent: bool) {
    let now = Instant::now();
    let was_quiet = {
        let mut flow = site.flow.lock().unwrap();
        let was_quiet = flow.quiet(now);
        let Some(progress) = flow.hear(progress, now) else { return };
        let _ = app.emit("progress", ProgressWord { server: site.client.server.key(), progress });
        was_quiet
    };
    if urgent || was_quiet {
        let outside = outside(patcher, site);
        tell(app, site, &mut site.flow.lock().unwrap(), &outside);
    }
}

/// Opens the desk of every site: a thread each, which goes round once a second, or at once
/// when it is woken.
pub(crate) fn open_desks(app: &AppHandle) {
    for place in 0..app.state::<Patcher>().sites.len() {
        let app = app.clone();
        std::thread::spawn(move || {
            let patcher = app.state::<Patcher>();
            let site = &patcher.sites[place];
            loop {
                round(&app, &patcher, place);
                let flow = site.flow.lock().unwrap();
                let (mut flow, _) = site.woken.wait_timeout_while(flow, ROUND, |flow| !flow.woken).unwrap();
                flow.woken = false;
            }
        });
    }
}

/// One round of the desk of the site at `place`: a look at the install folder, a folder to
/// put back, a patch file to read and a check of the server, each if it is due; then what
/// the app does by itself; then what the screen says, which also changes with the time
/// alone.
///
/// Nothing is held while a look or a check is made. A look holds only what keeps two looks
/// apart, and a check works on a thread of its own, so that the looks go on while the
/// server is slow. So do putting a folder back and reading a patch file, which take as long
/// as the files are large.
fn round(app: &AppHandle, patcher: &Patcher, place: usize) {
    let site = &patcher.sites[place];
    // Asked before anything is held: the window answers from the thread it lives on.
    let unseen = app
        .get_webview_window("main")
        .is_none_or(|window| !window.is_visible().unwrap_or(true) || window.is_minimized().unwrap_or(false));
    let calm = calm_of(site);
    let looks = {
        let mut flow = site.flow.lock().unwrap();
        let now = Instant::now();
        flow.see_pace(calm, now);
        flow.look_is_due(now, patcher.running_for(site), unseen)
    };
    if looks {
        look_now(app, patcher, site);
    }
    heal_if_due(app, patcher, place);
    read_patch_if_due(app, patcher, place);
    diagnose_if_due(app, patcher, place);
    let begins = {
        let mut flow = site.flow.lock().unwrap();
        let due = flow.check_is_due(Instant::now(), patcher.running().is_some());
        flow.checking |= due;
        due.then_some(flow.folder_epoch)
    };
    if let Some(epoch) = begins {
        let app = app.clone();
        std::thread::spawn(move || {
            let patcher = app.state::<Patcher>();
            let site = &patcher.sites[place];
            let checked = availability(&app, &patcher, site);
            let calm = calm_of(site);
            let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
            update(&app, &patcher, site, |flow| flow.take_answer(epoch, checked, calm, Instant::now(), at, &own_delay));
        });
    }
    act(app, patcher, site);
    let outside = outside(patcher, site);
    tell(app, site, &mut site.flow.lock().unwrap(), &outside);
}

/// Has the folder of the site at `place` put back, when an apply was cut off in it and an
/// attempt is due; see [`heal_begins`]. That comes before anything is said of the folder.
///
/// It is done on a thread of its own. Without the record of the apply it takes as long as
/// a check of the game files, and the desk goes on meanwhile: the looks find the folder
/// being put back, and the screen says that this is waited for. A window that is closed
/// meanwhile closes: every move brings the folder nearer to what it was, and the next
/// start of the app carries on from wherever this one came to.
fn heal_if_due(app: &AppHandle, patcher: &Patcher, place: usize) {
    let site = &patcher.sites[place];
    // Only a folder the last look found to be in that state is looked into for it.
    let seen = site.flow.lock().unwrap().status.as_ref().and_then(Status::to_put_back).map(PathBuf::from);
    let Some(install) = seen else { return };
    if !heal_begins(site, &install, patcher.running_for(site), Instant::now()) {
        return;
    }
    // An app that is about to leave and start itself again begins nothing that leaving
    // would cut off; see `rights`. Looked at once the turn is taken, as that side looks for
    // a turn once it has said so: one of the two sees the other.
    if patcher.restarting.load(Ordering::SeqCst) || site.diagnosis.lock().unwrap().checking() {
        site.heal.lock().unwrap().at_work = false;
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let patcher = app.state::<Patcher>();
        let site = &patcher.sites[place];
        put_back(site, &install);
        // The folder is another one from here on: it is looked at and asked about at once.
        look_now(&app, &patcher, site);
        update(&app, &patcher, site, |flow| flow.ask_again(Instant::now()));
    });
}

/// Has what the patch file on this PC does read, when the last look found one that has not
/// been read; see [`read_patch`]. The line for the room then says what a run of it needs,
/// by the way of patching that is chosen.
///
/// It is done on a thread of its own, since the whole file is read once, and not beside a
/// run, which reads the same file and has the drive to itself.
fn read_patch_if_due(app: &AppHandle, patcher: &Patcher, place: usize) {
    let site = &patcher.sites[place];
    if patcher.running().is_some() {
        return;
    }
    let unread = site.flow.lock().unwrap().status.as_ref().and_then(|status| status.unread_patch.clone());
    let Some(file) = unread.map(PathBuf::from) else { return };
    if !read_begins(site, &file) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let patcher = app.state::<Patcher>();
        let site = &patcher.sites[place];
        read_patch(site, &file);
        look_now(&app, &patcher, site);
    });
}

/// Takes the start of the game that is due for `site`, and says whether it was there to
/// take: not while the app is about to leave and start itself again, when the game stays
/// due for the copy that takes over. Taken, it is no longer due.
///
/// The app says that it is leaving before it reads what is due to hand that over (see
/// [`crate::rights::start_again_with_rights`]), and reads it under the same lock as this
/// is taken under. So the game is started by this copy or by the next, and never by both.
///
/// Nor does the app start the game by itself into a check of its files that the user
/// asked for (see [`crate::diagnosis`]): the game stays due until the check has ended.
fn claims_game_start(patcher: &Patcher, site: &Site) -> bool {
    let mut flow = site.flow.lock().unwrap();
    if patcher.restarting.load(Ordering::SeqCst) || site.diagnosis.lock().unwrap().checking() {
        return false;
    }
    flow.game_due = false;
    true
}

/// Does what the app does by itself for `site` now, if anything; see [`decide`].
///
/// Nobody asked for what is done here, so nobody is told when it does not come about: it
/// goes to the log.
fn act(app: &AppHandle, patcher: &Patcher, site: &Site) {
    loop {
        let (decision, waiting) = {
            let world = outside(patcher, site).world;
            let flow = site.flow.lock().unwrap();
            let facts = flow.facts(&world);
            (decide(&facts), facts.waiting().map(str::to_owned))
        };
        match decision {
            Decision::Nothing => return,
            // The patch is decided on next: it starts in this same round if the user wants
            // patches applied without being asked.
            Decision::DropGameStart => {
                update(app, patcher, site, |flow| flow.game_due = false);
                site.log("not starting the game by itself: another patch is waiting, or that could not be ruled out");
            }
            Decision::LeaveGameToUser => {
                update(app, patcher, site, |flow| flow.game_due = false);
                site.log("not starting the game by itself: whether it is open cannot be seen for this client");
            }
            Decision::StartGame => {
                // An app that is about to leave and start itself again starts no game: it
                // stays due, and the copy that takes over is told so; see `rights`.
                if !claims_game_start(patcher, site) {
                    return;
                }
                match start_game(app, patcher, site) {
                    Ok(()) => site.log("started the game after the patch"),
                    Err(err) => {
                        update(app, patcher, site, |flow| flow.game_due = false);
                        site.log(&format!("could not start the game after the patch: {err}"));
                    }
                }
                return;
            }
            Decision::StartPatch => {
                if let Err(refusal) = begin(app, patcher, site, Wanted::Offered) {
                    // Something in the way shows on the screen, and the patch starts when it
                    // is gone. Anything else would be met again every second: the patch is
                    // left to the user.
                    if !matches!(refusal.kind, RefusalKind::Blocked | RefusalKind::Busy) {
                        update(app, patcher, site, |flow| flow.keep_hands_off(waiting.unwrap_or_default()));
                    }
                    site.log(&format!("did not start the patch by itself: {}", refusal.messages.join(" / ")));
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;
    use crate::flow::{Refusal, RunEnd, pick};
    use crate::harness::StandIns;
    use crate::made::{
        Registered, errored, game_at, game_open, no_game_at, no_install, patch_to, resumable, up_to_date,
    };
    use crate::offer::{CheckEnd, Checked};
    use crate::state::{Availability, Step};

    fn world(now: Instant) -> World {
        World {
            auto_patch: false,
            auto_start: false,
            outdated: false,
            run_at_work: false,
            open: Some(true),
            sees_open: true,
            checking_files: false,
            update_asked: true,
            now,
        }
    }

    /// What is outside a site at `now` with nothing going on around it.
    fn seen(now: Instant) -> Outside {
        Outside { world: world(now), lamp: None, locked: None, diagnosis: None, seat: None }
    }

    /// The live game's site with its folder looked at and the patch to 1.2.422 on the server.
    fn site_with_a_patch(now: Instant) -> Site {
        let site = Site::of(&mfp_kms::LIVE, PathBuf::from("data"), StandIns::default(), Registered::nowhere());
        {
            let mut flow = site.flow.lock().unwrap();
            flow.take_look(game_at(r"C:\Games\Maple"), now);
            let (epoch, checked) =
                (flow.folder_epoch, Checked { answer: patch_to("1.2.422"), end: CheckEnd::Answered });
            assert!(flow.take_answer(epoch, checked, Some(true), now, 1_795_000_000_000, &|_| 0));
        }
        site
    }

    /// Begins the run of the patch the screen offers, and has it at work.
    fn begin_run(flow: &mut Flow, now: Instant) {
        let picked = pick(&flow.facts(&world(now)), None, flow.left()).expect("a patch to begin");
        flow.begin(&picked, now);
        flow.at_work();
    }

    fn said(percent: f64, step: Step, status: &'static str, done: u64, activity: &str) -> Progress {
        Progress { percent, step: Some(step), status, done, total: 8, worked: done, activity: activity.to_owned() }
    }

    fn names(value: &Value) -> BTreeSet<&str> {
        value.as_object().expect("an object").keys().map(String::as_str).collect()
    }

    /// The page draws what it is told by these names. Whoever changes one changes the page.
    #[test]
    fn the_page_is_told_of_a_site_under_the_names_it_draws_by() {
        let now = Instant::now();
        let site = site_with_a_patch(now);
        let mut flow = site.flow.lock().unwrap();
        let view = serde_json::to_value(site_view(&site, &flow, &seen(now))).unwrap();
        let all = [
            "server",
            "rev",
            "title",
            "current",
            "latest",
            "patchAddress",
            "checkedAt",
            "lamp",
            "rows",
            "customDir",
            "halfPatched",
            "discardAsks",
            "phase",
            "run",
            "percent",
            "status",
            "statusBad",
            "busy",
            "summary",
            "button",
            "locked",
            "diagnosis",
        ];
        assert_eq!(names(&view), BTreeSet::from(all));
        assert_eq!(view["server"], json!("live"));
        assert_eq!(view["title"], json!("메이플스토리"));
        assert_eq!(view["phase"], json!("ready"));
        assert_eq!((&view["current"], &view["latest"]), (&json!("1.2.421"), &json!("1.2.422")));
        assert_eq!(view["checkedAt"], json!(1_795_000_000_000u64));
        for nothing in ["run", "summary", "locked", "lamp"] {
            assert_eq!(view[nothing], Value::Null, "{nothing}");
        }
        assert_eq!((&view["status"], &view["percent"]), (&json!("새 클라이언트 패치 있음"), &json!(0.0)));
        assert_eq!((&view["statusBad"], &view["busy"]), (&json!(false), &json!(false)));
        assert_eq!((&view["customDir"], &view["halfPatched"]), (&json!(false), &json!(false)));
        let folder =
            json!({ "kind": "folder", "label": "설치 경로", "ok": true, "detail": r"C:\Games\Maple", "message": "" });
        let patch = json!({ "kind": "patchFile", "label": "패치 파일", "ok": true, "detail": "00421to00422.patch · 1,229 MB", "message": "" });
        assert_eq!(view["patchAddress"], json!("https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch"));
        let rows = view["rows"].as_array().expect("the lines of the ready screen");
        assert_eq!((rows.first(), rows.last()), (Some(&folder), Some(&patch)));
        assert_eq!(view["button"], json!({ "kind": "download", "enabled": true, "primary": true }));

        // With a run on the screen that failed: the run, and what its dialog says once.
        begin_run(&mut flow, now);
        flow.hear(said(40.0, Step::VerifyInstall, "게임 파일 검사 중", 1, "검사 중: Map001.wz"), now);
        let (message, details) = ("새 게임 파일 검사에 실패했습니다.".to_owned(), vec!["Map001.wz".to_owned()]);
        flow.end(RunEnd::Failed { message, details }, now);
        let view = serde_json::to_value(site_view(&site, &flow, &seen(now))).unwrap();
        let failure = json!({
            "id": 1,
            "title": "패치에 실패했습니다.",
            "message": "새 게임 파일 검사에 실패했습니다.",
            "details": ["Map001.wz"],
        });
        // Of the run it is told what it draws by, and nothing the backend keeps for itself:
        // not the address or the version of its patch, nor that the app goes once it stops.
        // How far it has come is the figure of the screen.
        let run = json!({
            "asking": false,
            "step": "verifyInstall",
            "done": 1,
            "total": 8,
            "worked": 1,
            "activity": "검사 중: Map001.wz",
            "failure": failure,
        });
        assert_eq!((&view["phase"], &view["run"]), (&json!("failed"), &run));
        assert_eq!(view["percent"], json!(40.0));
        assert_eq!(view["button"], json!({ "kind": "retry", "enabled": true, "primary": true }));
    }

    #[test]
    fn a_run_at_work_is_told_whole_only_when_more_than_its_figures_change() {
        let now = Instant::now();
        let site = site_with_a_patch(now);
        let mut flow = site.flow.lock().unwrap();
        begin_run(&mut flow, now);
        let told = |flow: &Flow, outside: &Outside| site_view(&site, flow, outside).steady();
        flow.hear(said(10.0, Step::Download, "패치 파일 다운로드 중", 2, "다운로드 중: 00421to00422.patch"), now);
        let downloading = told(&flow, &seen(now));
        // The figures and the file it is on reach the page as the run says them.
        flow.hear(said(20.0, Step::Download, "패치 파일 다운로드 중", 5, "다운로드 중: 00421to00422.patch"), now);
        assert_eq!(told(&flow, &seen(now)), downloading);
        // A download that has gone quiet is another word in the footer.
        assert_ne!(told(&flow, &seen(now + Duration::from_secs(5))), downloading);
        // So is another step.
        flow.hear(said(30.0, Step::VerifyPatch, "패치 파일 검사 중", 0, ""), now);
        assert_ne!(told(&flow, &seen(now)), downloading);

        // Once the run has stopped, where it stands is part of what its screen says. The
        // count of what was told is never part of it.
        assert!(flow.stop(false));
        flow.end(RunEnd::Stopped, now);
        let mut view = site_view(&site, &flow, &seen(now));
        let paused = view.steady();
        view.rev += 1;
        assert_eq!(view.steady(), paused);
        view.percent = Some(99.0);
        assert_ne!(view.steady(), paused);
    }

    #[test]
    fn a_run_says_where_it_is_and_the_app_what_is_not_of_one_client_under_the_pages_names() {
        let progress = said(12.5, Step::Download, "패치 파일 다운로드 중", 1, "다운로드 중: 00421to00422.patch");
        let word = serde_json::to_value(ProgressWord { server: "live", progress }).unwrap();
        // What the step is called is the footer's word, which comes with the screen.
        let expected = json!({
            "server": "live",
            "percent": 12.5,
            "step": "download",
            "done": 1,
            "total": 8,
            "worked": 1,
            "activity": "다운로드 중: 00421to00422.patch",
        });
        assert_eq!(word, expected);

        let patcher = Patcher::new(PathBuf::from("data"));
        let view = serde_json::to_value(AppView { sites: Vec::new(), app: app_word(&patcher) }).unwrap();
        assert_eq!(names(&view), BTreeSet::from(["sites", "settings", "update", "running"]));
        assert_eq!((&view["update"], &view["running"]), (&Value::Null, &Value::Null));
        assert!(patcher.begin_run(&patcher.sites[0]));
        assert_eq!(serde_json::to_value(app_word(&patcher)).unwrap()["running"], json!("live"));
    }

    #[test]
    fn a_start_that_is_turned_down_says_which_way_under_the_pages_names() {
        let busy = json!({ "kind": "busy", "messages": ["이미 패치가 진행 중입니다."] });
        assert_eq!(serde_json::to_value(Refusal::busy()).unwrap(), busy);
        let kinds = [
            (RefusalKind::Blocked, "blocked"),
            (RefusalKind::Busy, "busy"),
            (RefusalKind::Nothing, "nothing"),
            (RefusalKind::Address, "address"),
            (RefusalKind::Ask, "ask"),
        ];
        // An address the app cannot vouch for: the one message is the question.
        let question = "메이플스토리 패치 주소가 아닙니다.\n이 주소로 패치하시겠습니까?";
        let asked = json!({ "kind": "ask", "messages": [question] });
        let refusal = Refusal::new(RefusalKind::Ask, &crate::wording::asks_about_address("메이플스토리"));
        assert_eq!(serde_json::to_value(refusal).unwrap(), asked);
        for (kind, name) in kinds {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
        }
    }

    /// While Windows asks whether the app may start again with administrator rights, the
    /// game that is due is not started: it stays due, for the copy that takes over, or for
    /// this one if the user says no.
    #[test]
    fn a_game_that_is_due_is_not_started_by_an_app_that_is_about_to_leave() {
        let site = Site::of(&mfp_kms::LIVE, PathBuf::from("data"), StandIns::default(), Registered::nowhere());
        let patcher = Patcher::of(PathBuf::from("data"), vec![site]);
        let site = &patcher.sites[0];
        let due = || site.flow.lock().unwrap().game_due;
        site.flow.lock().unwrap().game_due = true;

        patcher.restarting.store(true, Ordering::SeqCst);
        assert!(!claims_game_start(&patcher, site) && due());
        // What the copy that takes over is told is what was left due.
        assert!(site.flow.lock().unwrap().remembered().game_due);

        patcher.restarting.store(false, Ordering::SeqCst);
        assert!(claims_game_start(&patcher, site) && !due());
    }

    #[test]
    fn a_site_is_told_whose_run_keeps_it_from_starting() {
        let stand_in = |client| Site::of(client, PathBuf::from("data"), StandIns::default(), Registered::nowhere());
        let patcher = Patcher::of(PathBuf::from("data"), vec![stand_in(&mfp_kms::LIVE), stand_in(&mfp_kms::TEST)]);
        let (live, test) = (&patcher.sites[0], &patcher.sites[1]);
        let locked = |site: &Site| {
            let outside = outside(&patcher, site);
            let view = site_view(site, &site.flow.lock().unwrap(), &outside);
            serde_json::to_value(view).unwrap()["locked"].clone()
        };
        assert_eq!((locked(live), locked(test)), (Value::Null, Value::Null));

        assert!(patcher.begin_run(live));
        assert_eq!((locked(live), locked(test)), (Value::Null, json!("live")));
        // Both know that a run is at work: neither asks the server, and neither starts one.
        assert!(outside(&patcher, live).world.run_at_work && outside(&patcher, test).world.run_at_work);
        assert_eq!(serde_json::to_value(app_word(&patcher)).unwrap()["running"], json!("live"));

        patcher.end_run();
        assert!(patcher.begin_run(test));
        assert_eq!((locked(live), locked(test)), (json!("test"), Value::Null));
        assert_eq!(serde_json::to_value(app_word(&patcher)).unwrap()["running"], json!("test"));
    }

    /// Something that becomes of a site, as its desk, a check or a run brings it about.
    type Event = Box<dyn Fn(&mut Flow, Instant)>;

    fn event(does: impl Fn(&mut Flow, Instant) + 'static) -> Event {
        Box::new(does)
    }

    /// A look at the folder that finds `status`.
    fn look(status: fn() -> Status) -> Event {
        event(move |flow, now| {
            flow.take_look(status(), now);
        })
    }

    /// A check that ends as `end` with `answer`, begun for the folder the site has, or
    /// with `stale` for the one it had before.
    fn check(answer: fn() -> Availability, end: CheckEnd, stale: bool) -> Event {
        event(move |flow, now| {
            let epoch = if stale { flow.folder_epoch.wrapping_sub(1) } else { flow.folder_epoch };
            flow.take_answer(epoch, Checked { answer: answer(), end }, Some(true), now, 1_795_000_000_000, &|_| 0);
        })
    }

    /// How many of [`events`] are looks at the folder. They come first.
    const LOOKS: usize = 6;

    /// Everything that changes what a site knows of the newest version or of its answer.
    fn events() -> Vec<Event> {
        const HERE: &str = r"C:\Games\Maple";
        vec![
            look(|| game_at(HERE)),
            look(|| game_at(r"D:\Copy\Maple")),
            look(|| game_open(game_at(HERE))),
            look(no_install),
            look(|| no_game_at(HERE)),
            look(|| resumable(game_at(HERE), None, Some(12.5))),
            check(|| up_to_date("1.2.421"), CheckEnd::Answered, false),
            check(|| up_to_date("1.2.421"), CheckEnd::Answered, true),
            check(|| up_to_date("1.2.421"), CheckEnd::Stopped, false),
            check(|| patch_to("1.2.422"), CheckEnd::Answered, false),
            check(|| errored("서버 요청 실패"), CheckEnd::Failed, false),
            check(
                || Availability { latest: Some("1.2.421".to_owned()), ..errored("패치 확인 불가") },
                CheckEnd::Failed,
                false,
            ),
            check(Availability::nothing, CheckEnd::Answered, false),
            event(|flow, now| flow.ask_again(now)),
            event(|flow, now| flow.leftovers_discarded(true, now)),
            event(|flow, now| {
                if let Ok(picked) = pick(&flow.facts(&world(now)), None, flow.left()) {
                    flow.begin(&picked, now);
                    flow.at_work();
                }
            }),
            event(|flow, now| flow.end(RunEnd::Done { seconds: 60, files: 10 }, now)),
            event(|flow, now| flow.end(RunEnd::Stopped, now)),
            event(|flow, _| flow.dismiss()),
        ]
    }

    /// The page names the newest version on one line and says in its footer whether the
    /// install is at it. Whatever becomes of a site, and in whatever order, the footer
    /// never says that the install is up to date under a line with no version to name.
    ///
    /// Every order of up to four things that can become of a site is gone through, the
    /// first of them a look: nothing is asked or begun before one. What the site keeps is
    /// of one moment throughout: an answer that names the newest version is never kept
    /// without that version. The screen would hold even if it were not, since the footer
    /// goes by the version the page has to show.
    #[test]
    fn the_footer_never_calls_the_install_up_to_date_over_a_page_with_no_newest_version() {
        let now = Instant::now();
        let site = Site::of(&mfp_kms::LIVE, PathBuf::from("data"), StandIns::default(), Registered::nowhere());
        let events = events();
        for length in 1..=4u32 {
            for order in 0..LOOKS * events.len().pow(length - 1) {
                let mut picks = vec![order % LOOKS];
                let mut rest = order / LOOKS;
                for _ in 1..length {
                    picks.push(rest % events.len());
                    rest /= events.len();
                }
                let mut flow = Flow::new(now);
                picks.iter().for_each(|&at| events[at](&mut flow, now));

                let names = flow.answer.as_ref().is_some_and(|answer| answer.latest.is_some());
                assert!(!names || flow.latest.is_some(), "an answer without its newest version after {picks:?}");
                let view = site_view(&site, &flow, &seen(now));
                // The figure follows the footer: it is at its end when the footer says so.
                let figure_full = view.phase == Phase::Ready && view.percent == Some(100.0);
                assert_eq!(view.status == "최신 버전", figure_full, "after {picks:?}");
                assert!(!figure_full || view.latest.is_some(), "up to date with no newest version after {picks:?}");
            }
        }
    }
}
