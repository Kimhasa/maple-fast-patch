//! A patch run: starting and stopping it, and turning what the engine reports into what
//! the screen, the taskbar and the records show.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mfp_engine::apply::Leftover;
use mfp_engine::{Error, Event, Options, Outcome, Source, Stage, download, preflight, stage};
use tauri::window::{ProgressBarState, ProgressBarStatus};
use tauri::{AppHandle, Manager, State};

use crate::address::{NotTaken, Taken, file_at, take};
use crate::desk::{look_now, outside, run_said, tell_app, update};
use crate::flow::{Flow, Patch, Progress, Refusal, RefusalKind, RunEnd, pick};
use crate::log::{open_log, write_line};
use crate::measure::{Measure, STEPS, Storage, drive_size, report_text};
use crate::offer::{Published, adopt_checked, checks_its_folder, given_checks_its_folder, starts_from};
use crate::ready::{forget_what_was_known, work_left};
use crate::records::{
    Left, forget_left_run, install_mark, left_run, patch_at, record_minor, record_run_target, record_version, records,
    write_whole,
};
use crate::state::{Patcher, Site, Target};
use crate::window::notify;
use crate::wording::{asks_about_address, describe_error, describe_run_error, turned_down};

/// What a run is asked to be of.
pub(crate) enum Wanted {
    /// What the screen offers: the run that stopped or was left unfinished, and otherwise
    /// the patch a check found; see [`pick`].
    Offered,
    /// The patch at the address the user gave. `agreed` says that they were asked about an
    /// address the app cannot vouch for, and said yes to this one.
    At { address: String, agreed: bool },
}

/// The patch a run fetches and applies.
struct Fetch {
    url: String,
    /// What its file is kept as in the install folder while a run is at it.
    file_name: String,
    size: u64,
    /// What it is called in the log and in the measurement: the version a patch the app
    /// found leads to, and the name of the file at an address the user gave.
    label: String,
    /// The patch as a check describes it, when it is one of the client's own. Where it
    /// leads is then known, and is written down once it has been applied.
    known: Option<Target>,
    /// The user gave its address. Any other patch is one a check found.
    given: bool,
}

impl Fetch {
    /// The patch a check found.
    fn found(target: &Target) -> Fetch {
        Fetch {
            url: target.url.clone(),
            file_name: target.file_name.clone(),
            size: target.size,
            label: target.label.clone(),
            known: Some(target.clone()),
            given: false,
        }
    }

    /// The patch at `url`, an address the user gave, of which the server says that it is
    /// `size` bytes long.
    fn given(site: &Site, url: &str, size: u64) -> Fetch {
        match patch_at(site, url, size) {
            Some(target) => Fetch { label: file_at(url).to_owned(), given: true, ..Fetch::found(&target) },
            None => {
                let name = file_at(url).to_owned();
                Fetch { url: url.to_owned(), file_name: name.clone(), size, label: name, known: None, given: true }
            }
        }
    }

    /// What a run of this patch goes by when it is taken up again: the address the user
    /// gave, or the name the file of a patch the app found is kept under.
    fn id(&self) -> &str {
        if self.given { &self.url } else { &self.file_name }
    }
}

fn site_named<'a>(patcher: &'a Patcher, server: &str) -> Result<&'a Site, Refusal> {
    patcher.site_named(server).ok_or_else(Refusal::no_folder)
}

/// Starts a patch run of what the screen offers for the client the page calls `server`; see
/// [`pick`].
#[tauri::command(async)]
pub(crate) fn start_patch(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), Refusal> {
    begin(&app, &patcher, site_named(&patcher, &server)?, Wanted::Offered)
}

/// Starts a run of the patch at `address`, which the user gave, for the client the page
/// calls `server`.
///
/// Whether a patch is fetched from that address is decided here and not on the page; see
/// [`take`]. An address on a server of Nexon's that is not among the client's own patch
/// files is not begun: the start is turned down as [`RefusalKind::Ask`], for the page to
/// ask, and the address waits for [`confirm_patch_address`].
#[tauri::command(async)]
pub(crate) fn start_patch_at(
    app: AppHandle,
    patcher: State<Patcher>,
    server: String,
    address: String,
) -> Result<(), Refusal> {
    let site = site_named(&patcher, &server)?;
    // Whatever address was asked about before is no longer the one in question.
    site.asked_about.lock().unwrap().take();
    begin(&app, &patcher, site, Wanted::At { address, agreed: false })
}

/// Starts the run of the patch at the address the user was asked about, for the client the
/// page calls `server`: they have said yes.
///
/// The yes is for the one address that was asked about, which the backend has kept, and
/// is used up by this. Nothing the page says here names an address or stays said.
#[tauri::command(async)]
pub(crate) fn confirm_patch_address(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), Refusal> {
    let site = site_named(&patcher, &server)?;
    let asked_about = site.asked_about.lock().unwrap().take();
    let address = asked_about.ok_or_else(Refusal::no_patch)?;
    begin(&app, &patcher, site, Wanted::At { address, agreed: true })
}

/// What `address` is taken for as an address of a patch for the client of `site`, or the
/// sentence that says why it is not taken; see [`take`].
fn taken(site: &Site, address: &str) -> Result<Taken, Refusal> {
    take(address, &site.patch_files()).map_err(|why| {
        let sentence = match why {
            NotTaken::NotAnAddress => "패치 주소가 아닙니다.",
            NotTaken::NotNexons => "넥슨 패치 서버 주소가 아닙니다.",
        };
        Refusal::new(RefusalKind::Address, sentence)
    })
}

/// Whether a patch at `address` is fetched in a way nobody on the line can change. A build
/// made for trying the app out (`trying_out`) also fetches from a made-up server on this PC,
/// which speaks plain HTTP.
fn fetched_safely(address: &str, trying_out: bool) -> bool {
    trying_out || address.starts_with("https://")
}

/// Begins a run in the folder of `site`: for the user who asked for one, or for the app
/// itself (see [`crate::flow::decide`]). Says why not, when none is begun.
///
/// The folder is looked at first. What the desk last saw may be a few seconds old, and a
/// run must not begin by that.
pub(crate) fn begin(app: &AppHandle, patcher: &Patcher, site: &Site, wanted: Wanted) -> Result<(), Refusal> {
    if patcher.turn_taken() || site.diagnosis.lock().unwrap().checking() {
        return Err(Refusal::busy());
    }
    // An address is known from here on in the one spelling it is asked for in.
    let (address, agreed) = match &wanted {
        Wanted::At { address, agreed } => (Some(taken(site, address)?.url), *agreed),
        Wanted::Offered => (None, false),
    };
    // A folder that is still being put back after an apply that was cut off is at work:
    // nothing begins in it until it is back, and the footer says what is waited for.
    if look_now(app, patcher, site).putting_back {
        return Err(Refusal::busy());
    }
    // The patch and the folder it is for are taken from the same look, in one breath. A
    // folder that is changed in this very moment is not given the patch of the one before.
    let (picked, folder, continued) = {
        let world = outside(patcher, site).world;
        let flow = site.flow.lock().unwrap();
        let facts = flow.facts(&world);
        let picked = pick(&facts, address.as_deref(), flow.left())?;
        let folder =
            flow.status.as_ref().and_then(|status| status.install_dir.clone()).ok_or_else(Refusal::no_folder)?;
        let continued = picked.address().is_some_and(|address| facts.continues(address));
        (picked, folder, continued)
    };
    let (install_dir, place) = (PathBuf::from(&folder), patcher.place(site).ok_or_else(Refusal::no_folder)?);
    let fetch = match &picked.patch {
        Patch::Given(address) => {
            // The rule is held against every address a run is begun of, also one that is
            // gone on with: what a run left on record of itself is a file like any other.
            let taken = taken(site, address)?;
            // Nexon's, and not among this client's own patch files: the user is asked
            // first. A run that is gone on with was begun with that settled.
            if !(taken.own || agreed || continued) {
                *site.asked_about.lock().unwrap() = Some(taken.url);
                return Err(Refusal::new(RefusalKind::Ask, &asks_about_address(site.client.title)));
            }
            // The server is asked whether the file is there, and how large it is.
            let there = download::probe(&taken.url).map_err(|err| {
                let (message, details) = describe_error(site.client.title, &err);
                Refusal { kind: RefusalKind::Address, messages: std::iter::once(message).chain(details).collect() }
            })?;
            Fetch::given(site, &taken.url, there.len)
        }
        Patch::Server(target) => {
            let unchanged = || {
                let found = site.found_for(&install_dir).filter(|found| found.target.as_ref() == Some(target));
                found.is_some_and(|found| found.mark == install_mark(site, &install_dir))
            };
            // The patch was found for the folder as its files were at the last check, and
            // so was what proves which version the folder is at. Files that have changed
            // since are another folder to ask about: the server is asked first. A folder
            // that a run left partly new is not what any check found it as, and takes the
            // one patch that is on record for it.
            if !mfp_engine::run::half_patched(&install_dir) && !unchanged() {
                site.log("not starting: the folder is not as the last check found it. asking again");
                site.forget_found();
                update(app, patcher, site, |flow| flow.ask_again(Instant::now()));
                return Err(Refusal::busy());
            }
            if !fetched_safely(&target.url, cfg!(debug_assertions)) {
                return Err(Refusal::no_patch());
            }
            Fetch::found(target)
        }
    };
    // The one turn there is for a run is taken in the same breath as the run takes the
    // screen: a site's run is at work exactly while its phase says so.
    let begun = update(app, patcher, site, |flow| {
        // Not for a folder the site has left in the meantime.
        let here = flow.status.as_ref().is_some_and(|status| status.install_dir.as_deref() == Some(folder.as_str()));
        let mut free = here && patcher.begin_run(site);
        // An app that is about to leave and start itself again begins no run; see `rights`.
        // Looked at once the turn is taken, as that side looks for a run once it has said
        // so: one of the two sees the other, and no run is begun to be cut off.
        if free && patcher.restarting.load(Ordering::SeqCst) {
            patcher.end_run();
            free = false;
        }
        // Nor is a run begun in a folder whose game files are being checked at the user's
        // word; see `diagnosis`. Looked at once the turn is taken, as that side looks for a
        // run once it has said that it checks: one of the two gives way. A diagnosis that
        // is at work by itself is ended instead, and made anew when the run has ended.
        if free {
            let mut diagnosed = site.diagnosis.lock().unwrap();
            if diagnosed.checking() {
                patcher.end_run();
                free = false;
            } else {
                diagnosed.put_off();
            }
        }
        if free {
            patcher.stop.store(false, Ordering::SeqCst);
            // A check still at work gives up, so the run does not have to wait for it. So
            // does one of another client: nobody asks the server while a run is at work.
            // Said after the turn is taken, and in that order: a check that is only now
            // beginning looks for a run once it has cleared this, and finds it.
            patcher.sites.iter().for_each(|any| any.check_stop.store(true, Ordering::SeqCst));
            flow.begin(&picked, Instant::now());
        }
        free
    });
    if !begun {
        return Err(Refusal::busy());
    }
    tell_app(app, patcher);

    let app = app.clone();
    std::thread::spawn(move || run(&app, place, install_dir, fetch));
    Ok(())
}

/// A run, from its turn at the folder to its end. The site it is of is the one at `place`.
fn run(app: &AppHandle, place: usize, install_dir: PathBuf, fetch: Fetch) {
    let patcher = app.state::<Patcher>();
    let site = &patcher.sites[place];
    let stop = patcher.stop.clone();
    // A check that is still at work finishes first: it may be using the same files.
    let busy = site.busy.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    update(app, &patcher, site, Flow::at_work);
    let awake = StayAwake::start();

    let wishes = *patcher.settings.lock().unwrap();
    // The patch file is kept in the install folder. The game's drive has to have room for
    // the new game files anyway; the system drive may have none to spare.
    let dest = stage::Dirs::new(&install_dir).download().join(&fetch.file_name);
    let source = Source::Url { url: fetch.url.clone(), dest: dest.clone() };
    let options = Options { install_dir, source, immediate: wishes.immediate, storage: wishes.storage.choice() };
    let mut reporter = Reporter::new(app.clone(), place);
    reporter.download_to = dest.clone();
    reporter.patch = file_at(&fetch.url).to_owned();
    // A run of this same patch that was left unfinished is carried on: the bar starts
    // where it had come to and the time already spent counts, also after the app was
    // closed in between.
    let id = fetch.id().to_owned();
    // Whatever that run left, the patch file it downloaded included, is in the work
    // folder.
    let work = options.install_dir.join(preflight::WORK_DIR_NAME).is_dir();
    let recorded = left_run(site, &options.install_dir, &id);
    let left = recorded.clone().filter(|_| work);
    if let Some(left) = &left {
        reporter.floor = left.percent;
        reporter.before = left.active;
    }
    // A folder that a run left partly new takes only the rest of that same patch; the
    // engine turns a run of any other down. Such an attempt must not write over what
    // is known of the run to finish, or that patch could not be offered again. So in
    // such a folder only the run that is on record keeps records, and only its figures
    // are kept for the measurement.
    let half_patched = mfp_engine::run::half_patched(&options.install_dir);
    let keeps_records = !half_patched || recorded.is_some();
    // The figures of this patch so far, when a run of it was paused or failed earlier
    // in this session. Otherwise the measuring begins here. What an earlier session did
    // of the patch is then not in the time each step took, and the measurement says so;
    // the files it changed and the times it was paused are counted on from its record.
    let measured = if keeps_records { site.measure.lock().unwrap().take() } else { None };
    reporter.measure = match measured.filter(|kept| kept.id == id && kept.install == options.install_dir) {
        Some(kept) => kept,
        None => {
            let mut measure = Measure {
                id: id.clone(),
                install: options.install_dir.clone(),
                label: fetch.label.clone(),
                size: fetch.size,
                space: drive_size(&options.install_dir).zip(preflight::free_space(&options.install_dir).ok()),
                ..Measure::default()
            };
            if let Some(left) = &left {
                measure.take_up(left);
            }
            measure
        }
    };
    reporter.tried = half_patched;
    if !half_patched {
        record_run_target(site, &options.install_dir, fetch.known.as_ref().filter(|_| !fetch.given), &fetch.url);
    }
    if keeps_records {
        reporter.record = records(site, &options.install_dir).ok().map(|dir| (dir.join("run-left"), id));
    }
    reporter.log(&format!("start {} from {} in {}", fetch.label, fetch.url, options.install_dir.display()));
    if adopt_checked(site, &dest) {
        reporter.log("the patch file is the one the check had downloaded");
    }
    // A patch is only applied to a folder there is something to hold it against: the files
    // it names as what it starts from, or what is known of the folder's version. Whether
    // there is, is asked of the server where it is not known already; see
    // [`checks_its_folder`]. A folder that a run left partly new takes the rest of the patch
    // that run was begun with, and nothing is asked.
    let shelf = Published::of(site);
    let checks = match &fetch.known {
        _ if half_patched => Ok(true),
        Some(target) if !fetch.given => checks_its_folder(site, &options.install_dir, target, &shelf),
        known => {
            let from = known.as_ref().and_then(starts_from);
            given_checks_its_folder(site, &options.install_dir, &fetch.url, from, &shelf)
        }
    };
    let result = match checks {
        Ok(true) => mfp_engine::run_for(&site.programs(), &options, &stop, &mut |event| {
            // The run found an apply cut off, and put the folder back without being able to
            // vouch for it as the version before that patch. What the app had on record of
            // the folder is forgotten before this run adds to it.
            if matches!(event, Event::Restored(Leftover::PutBack)) {
                forget_what_was_known(site, &options.install_dir);
            }
            reporter.handle(event)
        }),
        // Nothing was fetched and nothing changed. The game's own launcher still brings
        // the game up to date, which is what a patch in a form this app does not take
        // comes to as well.
        Ok(false) => Err(Error::UnknownForm(
            "it names no file it starts from, and nothing proves which version the folder is at".to_owned(),
        )),
        Err(err) => Err(err),
    };
    reporter.time_step(None);
    let seconds = reporter.active().as_secs();
    // What the patch changed is counted over every run of it, as its time is.
    let result = result.map(|outcome| {
        let files_changed = reporter.measure.files_in_all(outcome.files_changed);
        Outcome { files_changed, ..outcome }
    });
    match &result {
        Ok(outcome) => {
            let way = if outcome.immediate { ", replaced as they were built" } else { "" };
            reporter.log(&format!("done, {} file(s) changed{way}", outcome.files_changed));
            forget_left_run(site, &options.install_dir);
            *site.report.lock().unwrap() = Some(report_text(app, &reporter.measure, seconds, outcome));
        }
        Err(err) => {
            reporter.log(&format!("ended: {err}"));
            // The run may be taken up again, in this session or another. What it adds to
            // the counts goes on record with where it came to.
            let placed = std::mem::take(&mut reporter.placed);
            reporter.measure.add_unfinished(placed, matches!(err, Error::Stopped));
            reporter.settle();
            if keeps_records {
                *site.measure.lock().unwrap() = Some(reporter.measure.clone());
            }
        }
    }
    // A run that was paused and left something to go on with keeps its figure on the
    // taskbar, marked as paused: what it built, or part of the patch file. That is what
    // the status calls a run that can be continued. Any other end takes the figure away.
    let partial = download::part_path(&dest).exists();
    let unfinished = partial || work_left(&options.install_dir);
    let rests = matches!(&result, Err(Error::Stopped)) && unfinished;
    taskbar_progress(app, rests.then(|| (ProgressBarStatus::Paused, reporter.percent())));

    let end = match result {
        Ok(outcome) => {
            // Where the patch left the install is remembered, when it is known where that
            // is: for a patch the app found, and for one of the client's own that the user
            // gave the address of.
            if let Some(target) = &fetch.known {
                record_version(site, &options.install_dir, target.version);
                if let Some(minor) = target.minor {
                    record_minor(site, &options.install_dir, target.version, minor);
                }
            }
            // The install changed; work out afresh what it can take next.
            site.forget_found();
            RunEnd::Done { seconds, files: outcome.files_changed }
        }
        Err(Error::Stopped) => RunEnd::Stopped,
        Err(err) => {
            let (message, details) = describe_run_error(site.client.title, &err, half_patched);
            match turned_down(&err) {
                true => RunEnd::Refused { message, details },
                false => RunEnd::Failed { message, details },
            }
        }
    };
    drop(awake);
    drop(busy);

    // Someone who is not looking at the window is told how it ended. Not someone who closed
    // the window on the run: the app goes.
    let leaving = || patcher.closing.load(Ordering::SeqCst);
    let away = app
        .get_webview_window("main")
        .is_some_and(|window| window.is_minimized().unwrap_or(false) || !window.is_focused().unwrap_or(true));
    let word = match &end {
        RunEnd::Done { .. } => Some("패치가 완료되었습니다."),
        RunEnd::Failed { .. } => Some("패치에 실패했습니다."),
        // A run that was turned down says what was in the way, as its dialog does.
        RunEnd::Refused { message, .. } => message.lines().next(),
        RunEnd::Stopped => None,
    };
    if let Some(word) = word.filter(|_| away && !leaving()) {
        notify(app, site, word);
    }
    // The run gives up its turn in the same breath as its end is taken in, so that nothing
    // is looked at, asked or begun in between by what was known before the run. The desk
    // left the folder alone meanwhile: it looks at it and asks the server at once, and
    // decides by how the run ended what the app does next.
    let leaves = update(app, &patcher, site, |flow| {
        patcher.end_run();
        // The window was closed during the run: now that it has stopped, the app can go,
        // and the screen stays as it is until then.
        let leaves = leaving();
        if !leaves {
            flow.end(end, Instant::now());
        }
        leaves
    });
    if leaves {
        app.exit(0);
        return;
    }
    tell_app(app, &patcher);
}

/// Keeps Windows from going to sleep for as long as it lives. The display may still turn
/// off.
struct StayAwake;

impl StayAwake {
    fn start() -> StayAwake {
        #[cfg(windows)]
        // SAFETY: the call takes flags only and affects this thread's power request.
        unsafe {
            use windows_sys::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
        }
        StayAwake
    }
}

impl Drop for StayAwake {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: as above; this withdraws the request made in `start` on the same thread.
        unsafe {
            use windows_sys::Win32::System::Power::{ES_CONTINUOUS, SetThreadExecutionState};
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

/// Asks the run that is at work for the client the page calls `server` to stop at the next
/// safe point: a pause. What is done so far is kept.
#[tauri::command]
pub(crate) fn stop_patch(app: AppHandle, patcher: State<Patcher>, server: String) {
    if patcher.site_named(&server).is_some_and(|site| patcher.running_for(site)) {
        stop(&app, &patcher, false);
    }
}

/// Asks the run at work to stop at the next safe point. With `leaving` the window was
/// closed on it, and the app goes once the run has stopped. Says whether there was a run to
/// ask: one that has just ended is not one.
pub(crate) fn stop(app: &AppHandle, patcher: &Patcher, leaving: bool) -> bool {
    let Some(site) = patcher.running() else { return false };
    update(app, patcher, site, |flow| {
        let asked = flow.stop(leaving);
        if asked {
            // Said while the run's end cannot be taken in, so the run finds it when it ends.
            patcher.closing.fetch_or(leaving, Ordering::SeqCst);
            patcher.stop.store(true, Ordering::SeqCst);
        }
        asked
    })
}

/// How long a file name stays on the activity line before the next one takes its place.
/// Hundreds of small files go by in a second, and a name that changes that fast cannot be read.
const NAME_HOLD: Duration = Duration::from_millis(400);
/// How long what a run has written down of itself may go without being written again. The
/// figure on the bar is written whenever it changes. This is for the time the run has taken,
/// which grows while the bar stands still.
const SAVE_EVERY: Duration = Duration::from_secs(2);

/// Whether a run has to write down how far it has come before the screen is told that the
/// bar stands at `told`: the whole percent on record is another one, or the record is old.
///
/// What a restart shows is what is on record, and the app may be cut off at any moment. A
/// record written by the clock alone was up to two seconds behind the screen by then, and
/// the bar came back lower than it had been seen.
fn record_due(on_record: Option<u64>, told: f64, age: Duration) -> bool {
    on_record != Some(told as u64) || age >= SAVE_EVERY
}

/// How many more game files are in place for good once the engine has said `event`.
///
/// The engine names each such file once, as placed, and that is all a run counts while it
/// goes: not a file it begins on, not one it finds built, not one it removes. What the
/// run removed is in the figure the engine gives when the run is done, and only there.
fn placed_by(event: &Event) -> usize {
    usize::from(matches!(event, Event::Placed { .. }))
}

/// The step the screen is told, which is the engine's own but for one case: the download of
/// a patch file that is on this PC whole already (`whole`). The engine then only asks the
/// server whether the file is still the one it has, and fetches nothing. A download that
/// brings nothing is not shown as one. The screen goes straight on to the check of the file,
/// which is what follows.
fn shown_step(stage: Stage, whole: bool) -> Stage {
    if stage == Stage::Download && whole { Stage::VerifyPatch } else { stage }
}

/// Whether a step that has ended is one the run went through, and has a time of its own in
/// the measurement. A download that found the patch file on this PC whole (`whole`) is
/// not: nothing was fetched, and the moment it took to ask the server is no time of a
/// download. The screen does not show it as one either; see [`shown_step`].
///
/// A step no run of a patch went through has no line in the measurement. One that an
/// earlier run of the same patch went through keeps the time it took then.
fn went_through(stage: Stage, whole: bool) -> bool {
    shown_step(stage, whole) == stage
}

/// The stretch of the bar a step has, as where it begins and how long it is, 0 to 100: its
/// share of the whole run, as a rough guide.
///
/// A trial takes its share out of the build's: it was timed at 17 s where the build took
/// 93 s, which is 8 of the build's 50. A run without one (`tried` is not set) leaves the
/// build all 50, so nothing about its figure changes.
fn stretch(stage: Stage, tried: bool) -> (f64, f64) {
    match stage {
        Stage::Download => (0.0, 30.0),
        Stage::VerifyPatch => (30.0, 3.0),
        Stage::VerifyInstall => (33.0, 12.0),
        Stage::Trial => (45.0, 8.0),
        Stage::Rebuild if tried => (53.0, 42.0),
        Stage::Rebuild => (45.0, 50.0),
        Stage::Apply => (95.0, 5.0),
    }
}

/// Where the bar stands, 0 to 100, with `part` of a step's stretch done. It is never behind
/// `floor`, where an unfinished run of the patch had come to: the steps before that point
/// are gone through again, and the bar waits there for them.
fn bar((start, span): (f64, f64), part: f64, floor: f64) -> f64 {
    (start + span * part.clamp(0.0, 1.0)).max(floor)
}

/// What a step is called on the screen while it is at work.
fn status_of(stage: Stage) -> &'static str {
    match stage {
        Stage::Download => "패치 파일 다운로드 중",
        Stage::VerifyPatch => "패치 파일 검사 중",
        Stage::VerifyInstall => "게임 파일 검사 중",
        Stage::Trial => "패치 사전 확인 중",
        Stage::Rebuild => "새 게임 파일 생성 중",
        Stage::Apply => "게임 파일 반영 중",
    }
}

/// Turns engine events into at most a dozen UI updates per second.
struct Reporter {
    app: AppHandle,
    /// The place of the run's site among the app's sites.
    place: usize,
    /// The run has said which step it is at. Until then it has no step.
    begun: bool,
    stage: Stage,
    done: u64,
    total: u64,
    /// Bytes of `done` this run did not work for: new files an earlier run had already built.
    kept: u64,
    /// What is being worked on right now, as the line above the bar words it.
    doing: Option<String>,
    /// That line as the screen has it, and since when. It follows `doing` at a pace that can
    /// be read.
    shown: Option<String>,
    shown_at: Instant,
    /// The patch file's name, as its address has it. The first two steps work on that file.
    patch: String,
    last_emit: Instant,
    /// Where the patch file is downloaded to.
    download_to: PathBuf,
    /// The download at work found the patch file there whole and has fetched nothing; see
    /// [`shown_step`].
    whole: bool,
    /// Where an unfinished run of this patch had come to. The bar does not go back behind
    /// it while the steps before are gone through again.
    floor: f64,
    /// Every new file is built once for nothing before the build proper: in this run, or in
    /// the run that left the folder partly new, which this one takes up without trying
    /// again. The build then has that much less of the bar, in both of them alike.
    tried: bool,
    /// How long this patch has been worked on: in earlier runs, and since this one began.
    before: Duration,
    started: Instant,
    /// Where the bar stood when the screen was last told. That figure is the one written
    /// down, so that what a restart shows is what was last seen.
    told: f64,
    /// Where this run writes down how far it has come, the patch it is a run of, when it
    /// last did so, and the whole percent it wrote then.
    record: Option<(PathBuf, String)>,
    saved_at: Instant,
    on_record: Option<u64>,
    /// Game files this run has put in place for good so far; see [`placed_by`].
    placed: usize,
    /// Record of this run, kept for troubleshooting.
    log: Option<std::fs::File>,
    /// How the patch has gone, for the measurement that can be copied when it is done.
    measure: Measure,
    /// The step that is at work, and since when.
    timing: Option<(Stage, Instant)>,
    /// How far the download had come when it last said, to count what arrives.
    downloaded_to: Option<u64>,
    /// The percent the app's button in the taskbar shows, once this run has put one there.
    on_taskbar: Option<u64>,
}

/// Shows how far a run is on the app's button in the taskbar, or takes that away again.
/// It is all someone who has minimised the window sees of a run.
pub(crate) fn taskbar_progress(app: &AppHandle, shown: Option<(ProgressBarStatus, u64)>) {
    let (status, progress) = match shown {
        Some((status, percent)) => (status, Some(percent)),
        None => (ProgressBarStatus::None, None),
    };
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_progress_bar(ProgressBarState { status: Some(status), progress });
    }
}

impl Reporter {
    fn new(app: AppHandle, place: usize) -> Reporter {
        let log = open_log();
        let now = Instant::now();
        Reporter {
            app,
            place,
            begun: false,
            stage: Stage::Download,
            done: 0,
            total: 0,
            kept: 0,
            doing: None,
            shown: None,
            shown_at: now,
            patch: String::new(),
            last_emit: now,
            download_to: PathBuf::new(),
            whole: false,
            floor: 0.0,
            tried: false,
            before: Duration::ZERO,
            started: now,
            told: 0.0,
            record: None,
            saved_at: now,
            on_record: None,
            placed: 0,
            log,
            measure: Measure::default(),
            timing: None,
            downloaded_to: None,
            on_taskbar: None,
        }
    }

    /// Writes a line of this run to the log, as the run's site writes its lines.
    fn log(&mut self, text: &str) {
        let line = self.app.state::<Patcher>().sites[self.place].said(text);
        if let Some(log) = &mut self.log {
            write_line(log, &line);
        }
    }

    /// Adds the time the step at work has taken to its total, and begins timing `next`.
    /// A step the run did not go through adds nothing; see [`went_through`].
    fn time_step(&mut self, next: Option<Stage>) {
        let now = Instant::now();
        if let Some((stage, since)) = self.timing.take()
            && went_through(stage, self.whole)
            && let Some(at) = STEPS.iter().position(|(step, _)| *step == stage)
        {
            self.measure.steps[at] += now - since;
        }
        self.timing = next.map(|stage| (stage, now));
    }

    fn handle(&mut self, event: Event) {
        self.placed += placed_by(&event);
        let urgent = match event {
            Event::Stage(stage) => {
                self.log(&format!("step {stage:?}"));
                self.begun = true;
                self.tried |= stage == Stage::Trial;
                self.time_step(Some(stage));
                self.downloaded_to = None;
                (self.stage, self.done, self.total, self.kept) = (stage, 0, 0, 0);
                self.whole = stage == Stage::Download && self.download_to.is_file();
                // The first two steps work on the patch file itself; the others name a game
                // file each as they go.
                let on_patch = matches!(stage, Stage::Download | Stage::VerifyPatch) && !self.patch.is_empty();
                self.doing = on_patch.then(|| self.line(&self.patch));
                self.shown = None;
                true
            }
            Event::Progress { done, total } => {
                let mut fetched = false;
                if self.stage == Stage::Download {
                    // The first figure of the step is what was on disk before it. Only what
                    // comes on top of that arrived now.
                    if let Some(before) = self.downloaded_to {
                        self.measure.downloaded += done.saturating_sub(before);
                    }
                    self.downloaded_to = Some(done);
                    // The file that was here is not the one the server has now, and is
                    // fetched after all. From here on the step shows for what it is.
                    fetched = self.whole && done < total;
                    if fetched {
                        self.whole = false;
                        self.doing = (!self.patch.is_empty()).then(|| self.line(&self.patch));
                        self.shown = None;
                    }
                }
                (self.done, self.total) = (done, total);
                fetched
            }
            Event::File(name) => {
                self.doing = Some(self.line(&name));
                false
            }
            // Found already built: it is being checked, not made.
            Event::Kept { name, bytes } => {
                self.kept += bytes;
                self.doing = Some(format!("검사 중: {name}"));
                false
            }
            Event::Placed { .. } => {
                // A file that went in ahead of the others stays there whatever becomes of
                // the run. It is on record at once, so that the count holds when the app is
                // cut off. Those of the apply are named together when it is finished: the
                // run is done the next moment, and its record goes with it.
                if self.stage != Stage::Apply {
                    self.save();
                }
                false
            }
            Event::Space { needed, available } => {
                self.log(&format!("space needed {needed} bytes, available {available} bytes"));
                false
            }
            Event::Restored(_) => {
                self.log("restored the install folder after an interrupted apply");
                false
            }
            Event::Storage { file_system, seek_ms, slow, readers, checkers, unbuffered } => {
                let seek = seek_ms.map_or("seek not measured".to_owned(), |ms| format!("seek {ms:.2} ms"));
                let (kind, reads) =
                    (if slow { "slow" } else { "fast" }, if unbuffered { "unbuffered" } else { "buffered" });
                self.log(&format!(
                    "storage {file_system}, {seek}, {kind}: {readers} readers, {checkers} checkers, {reads}"
                ));
                self.measure.storage = Some(Storage { file_system, seek_ms, slow, readers, checkers });
                false
            }
        };
        if urgent || self.last_emit.elapsed() >= Duration::from_millis(80) {
            self.last_emit = Instant::now();
            if (self.shown.is_none() || self.shown_at.elapsed() >= NAME_HOLD) && self.shown != self.doing {
                self.shown = self.doing.clone();
                self.shown_at = self.last_emit;
            }
            self.tell(urgent);
            // The taskbar shows the figure the screen shows, and is only told when that
            // changes: a whole percent at a time.
            let percent = self.percent();
            if self.on_taskbar != Some(percent) {
                self.on_taskbar = Some(percent);
                taskbar_progress(&self.app, Some((ProgressBarStatus::Normal, percent)));
            }
        }
    }

    /// Tells the site where the run is, and through it the screen. A figure the record does
    /// not have yet is written down first, and at once when `urgent`; see [`record_due`].
    fn tell(&mut self, urgent: bool) {
        let progress = self.snapshot();
        self.told = progress.percent;
        if urgent || record_due(self.on_record, self.told, self.saved_at.elapsed()) {
            self.save();
        }
        let patcher = self.app.state::<Patcher>();
        run_said(&self.app, &patcher, &patcher.sites[self.place], progress, urgent);
    }

    /// Where the bar stands as the screen writes it: in whole percent.
    fn percent(&self) -> u64 {
        self.standing().floor() as u64
    }

    /// The step as the screen is told it; see [`shown_step`].
    fn step(&self) -> Stage {
        shown_step(self.stage, self.whole)
    }

    /// The activity line for `name` in the current step, worded the way the game's own
    /// patcher words it: `생성 중: Map001.wz`.
    fn line(&self, name: &str) -> String {
        let doing = match self.step() {
            Stage::Download => "다운로드 중",
            Stage::VerifyPatch | Stage::VerifyInstall => "검사 중",
            Stage::Trial => "확인 중",
            Stage::Rebuild => "생성 중",
            Stage::Apply => "반영 중",
        };
        format!("{doing}: {name}")
    }

    /// Where the bar stands, 0 to 100.
    fn standing(&self) -> f64 {
        let step = self.step();
        // The figures of a download that is not shown as one are not those of the step that
        // is shown in its place, which has yet to begin.
        let part = if step != self.stage || self.total == 0 { 0.0 } else { self.done as f64 / self.total as f64 };
        bar(stretch(step, self.tried), part, self.floor)
    }

    fn snapshot(&self) -> Progress {
        let step = self.step();
        Progress {
            percent: self.standing(),
            step: self.begun.then(|| step.into()),
            status: if self.begun { status_of(step) } else { "" },
            done: self.done,
            total: self.total,
            worked: self.done.saturating_sub(self.kept),
            // A step that has named no file yet shows none, not the last one of the step before.
            activity: self.shown.clone().unwrap_or_default(),
        }
    }

    /// How long this patch has been worked on, pauses and restarts of the app not counted.
    fn active(&self) -> Duration {
        self.before + self.started.elapsed()
    }

    /// A run that ends unfinished says where it really came to. What the screen got last may
    /// be a moment old, and what is written down for the next run has to be the same figure.
    fn settle(&mut self) {
        self.shown = self.doing.clone();
        self.tell(true);
    }

    /// Writes down how far the run has come, for the next run of the same patch: where the
    /// screen was last told the bar stands, and what every run of the patch so far comes to
    /// in time, in files changed for good and in pauses.
    fn save(&mut self) {
        self.saved_at = Instant::now();
        let Some((path, patch)) = &self.record else { return };
        let left = Left {
            patch: patch.clone(),
            percent: self.told,
            active: self.active(),
            files: self.measure.files + self.placed,
            pauses: self.measure.pauses,
        };
        // A write that did not get through leaves the record as it was, and is made again
        // with the next figure the screen is told.
        self.on_record = write_whole(path, &left.written()).is_ok().then_some(self.told as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::made::Registered;

    fn begins(name: &str) -> Event {
        Event::File(name.to_owned())
    }

    fn placed(name: &str) -> Event {
        Event::Placed { name: name.to_owned() }
    }

    /// How many files a run has in place for good after `events`, as it counts them.
    fn in_place(events: &[Event]) -> usize {
        events.iter().map(placed_by).sum()
    }

    /// The safe way, paused while it builds: what an earlier run built is found built, the
    /// rest is begun on, and no file is in place before the run's last step is finished.
    #[test]
    fn a_run_that_is_cut_off_before_its_apply_is_finished_has_put_nothing_in_place() {
        let paused = [
            Event::Stage(Stage::VerifyInstall),
            begins("Base.wz"),
            Event::Stage(Stage::Rebuild),
            Event::Kept { name: "Base.wz".to_owned(), bytes: 4096 },
            begins("Map.wz"),
            Event::Progress { done: 6000, total: 9000 },
            begins("Skill.wz"),
        ];
        assert_eq!(in_place(&paused), 0);
        // Nor does an apply that failed and was undone: the engine names none of its files.
        let rolled_back = Event::Restored(mfp_engine::apply::Leftover::RolledBack);
        let undone = [Event::Stage(Stage::Apply), begins("Base.wz"), begins("Map.wz"), rolled_back];
        assert_eq!(in_place(&undone), 0);
    }

    /// The way that replaces files as it builds them: each file counts from the moment the
    /// engine says it is in place, and for nothing the engine says before.
    #[test]
    fn a_file_counts_once_the_engine_names_it_as_placed() {
        let mut run = vec![Event::Stage(Stage::Rebuild), begins("Skill.wz"), begins("Sound.wz"), begins("Skill.wz")];
        assert_eq!(in_place(&run), 0);
        run.push(placed("Skill.wz"));
        assert_eq!(in_place(&run), 1);
        run.extend([begins("Sound.wz"), placed("Sound.wz"), begins("Map.wz")]);
        assert_eq!(in_place(&run), 2);
    }

    /// A patch that puts four files in place and removes one. In one run the engine reports
    /// five. Cut off with one file in place and a second built, and then finished, it has
    /// to come to five as well.
    #[test]
    fn the_runs_of_a_patch_come_to_what_one_run_of_it_reports() {
        let removed = 1;
        // Two files go in ahead of the others, two in the apply, which names them together
        // when it is finished.
        let in_one_run = [
            Event::Stage(Stage::Rebuild),
            begins("a.wz"),
            begins("a.wz"),
            placed("a.wz"),
            begins("b.wz"),
            begins("b.wz"),
            placed("b.wz"),
            begins("c.wz"),
            begins("d.wz"),
            Event::Stage(Stage::Apply),
            begins("c.wz"),
            begins("d.wz"),
            begins("old.wz"),
            placed("c.wz"),
            placed("d.wz"),
        ];
        let reported = in_place(&in_one_run) + removed;
        assert_eq!(reported, 5);
        assert_eq!(Measure::default().files_in_all(reported), 5);

        let cut_off = [Event::Stage(Stage::Rebuild), begins("a.wz"), begins("a.wz"), placed("a.wz"), begins("b.wz")];
        let mut measure = Measure::default();
        measure.add_unfinished(in_place(&cut_off), false);
        // The run that finishes the patch finds the second file built and puts it in place
        // at once. The engine's figure for that run is what it placed and what it removed.
        let last_run = [
            Event::Stage(Stage::Rebuild),
            Event::Kept { name: "b.wz".to_owned(), bytes: 4096 },
            begins("b.wz"),
            placed("b.wz"),
            begins("c.wz"),
            begins("d.wz"),
            Event::Stage(Stage::Apply),
            begins("c.wz"),
            begins("d.wz"),
            begins("old.wz"),
            placed("c.wz"),
            placed("d.wz"),
        ];
        assert_eq!(measure.files_in_all(in_place(&last_run) + removed), reported);
    }

    /// A run that is continued with its patch file already here asks the server about the
    /// file and fetches nothing. That moment is not written down as a download.
    #[test]
    fn a_download_that_fetched_nothing_is_no_step_the_run_went_through() {
        assert!(!went_through(Stage::Download, true));
        assert!(went_through(Stage::Download, false));
        for stage in [Stage::VerifyPatch, Stage::VerifyInstall, Stage::Trial, Stage::Rebuild, Stage::Apply] {
            assert!(went_through(stage, false), "{stage:?}");
        }
    }

    #[test]
    fn a_patch_file_that_is_here_whole_shows_no_download() {
        let shown = shown_step(Stage::Download, true);
        assert_eq!(shown, Stage::VerifyPatch);
        assert_eq!(status_of(shown), "패치 파일 검사 중");
        // The bar is where the check of the file begins.
        assert_eq!(bar(stretch(shown, false), 0.0, 0.0), 30.0);
    }

    #[test]
    fn a_patch_file_that_is_fetched_shows_the_download_and_no_other_step_is_shown_as_another() {
        assert_eq!(shown_step(Stage::Download, false), Stage::Download);
        assert_eq!(status_of(Stage::Download), "패치 파일 다운로드 중");
        for stage in [Stage::VerifyPatch, Stage::VerifyInstall, Stage::Trial, Stage::Rebuild, Stage::Apply] {
            assert_eq!(shown_step(stage, true), stage);
        }
    }

    #[test]
    fn the_steps_of_a_run_lie_end_to_end_along_the_bar() {
        use Stage::{Apply, Download, Rebuild, Trial, VerifyInstall, VerifyPatch};
        // Without a trial, and with one.
        let runs: [(&[Stage], bool); 2] = [
            (&[Download, VerifyPatch, VerifyInstall, Rebuild, Apply], false),
            (&[Download, VerifyPatch, VerifyInstall, Trial, Rebuild, Apply], true),
        ];
        for (steps, tried) in runs {
            let mut end = 0.0;
            for step in steps {
                let (start, span) = stretch(*step, tried);
                assert!((start - end).abs() < 1e-9, "{step:?} begins at {start}, the step before ends at {end}");
                end = start + span;
            }
            assert!((end - 100.0).abs() < 1e-9, "the last step ends at {end}");
        }
    }

    #[test]
    fn the_bar_waits_where_an_unfinished_run_had_come_to() {
        let floor = 69.25;
        let rebuild = stretch(Stage::Rebuild, false);
        // The steps before that point are gone through again without the bar going back.
        assert_eq!(bar(stretch(Stage::VerifyPatch, false), 0.0, floor), floor);
        assert_eq!(bar(stretch(Stage::VerifyInstall, false), 1.0, floor), floor);
        assert_eq!(bar(rebuild, 0.3, floor), floor);
        // Past it the figure is the run's own, as it is in a run that takes nothing up.
        assert_eq!(bar(rebuild, 0.5, floor), 70.0);
        assert_eq!(bar(rebuild, 0.5, 0.0), 70.0);
        // A step never goes past its own end.
        assert_eq!(bar(stretch(Stage::Download, false), 1.5, 0.0), 30.0);
    }

    #[test]
    fn a_new_whole_percent_goes_on_record_at_once() {
        let just_written = Duration::from_millis(80);
        assert!(record_due(None, 0.0, just_written));
        assert!(record_due(Some(68), 69.0, just_written));
        // Within the same percent the record waits for the clock: only the time on it grows.
        assert!(!record_due(Some(69), 69.99, just_written));
        assert!(record_due(Some(69), 69.99, SAVE_EVERY));
    }

    fn live() -> Site {
        Site::of(&mfp_kms::LIVE, PathBuf::from("data"), crate::harness::StandIns::default(), Registered::nowhere())
    }

    /// Whether an address is one a patch is fetched from is decided here, where the run is
    /// begun, and the page is told in a sentence why one is not.
    #[test]
    fn an_address_that_is_not_taken_is_turned_down_with_the_sentence_for_it() {
        let site = live();
        let refused = |address: &str| {
            let refusal = taken(&site, address).expect_err(address);
            assert_eq!(refusal.kind, RefusalKind::Address, "{address:?}");
            refusal.messages
        };
        assert_eq!(refused("https://example.com/Patch/00421/00420to00421.patch"), ["넥슨 패치 서버 주소가 아닙니다."]);
        let no_address = [
            "",
            "00420to00421.patch",
            "ftp://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.exe",
        ];
        for address in no_address {
            assert_eq!(refused(address), ["패치 주소가 아닙니다."], "{address:?}");
        }

        // One that is taken is known from then on in the one spelling it is asked for in.
        let own = taken(&site, " HTTPS://MAPLESTORY.DN.NEXONCDN.CO.KR:443/Patch/00421/00420to00421.patch ").unwrap();
        assert_eq!(own.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch");
        assert!(own.own);
        // One that is given with http, as the links that are handed round are, is that
        // same address, and is what a run by it is on record under.
        let handed_round = taken(&site, "http://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch").unwrap();
        assert_eq!(handed_round, own);
        assert!(fetched_safely(&handed_round.url, false));
        // The other client's patches are Nexon's, and not this client's own.
        assert!(!taken(&site, &mfp_kms::TEST.patch_url(1207, 1208)).unwrap().own);

        // A build for trying the app out takes the made-up server it is pointed at. A build
        // that is given out is pointed at none, and takes no such address.
        let made_up = "http://127.0.0.1:18421/Patch/00901/00900to00901.patch";
        let trying_out = Site::of(
            &mfp_kms::LIVE,
            PathBuf::from("data"),
            crate::harness::StandIns::of_a_session(),
            Registered::nowhere(),
        );
        assert!(taken(&trying_out, made_up).unwrap().own);
        assert_eq!(refused(made_up), ["넥슨 패치 서버 주소가 아닙니다."]);
    }

    /// A patch the user gave the address of is called by the name of the file there. Where
    /// it is one of the client's own, it is otherwise the same patch a check would have
    /// found: kept under the same name, and written down as the version it leads to once
    /// it is applied. Of any other, where it leads is not known.
    #[test]
    fn a_patch_given_by_its_address_is_known_as_a_check_knows_it_when_it_is_one_of_the_clients_own() {
        let site = live();
        let files = "https://maplestory.dn.nexoncdn.co.kr";
        let main = Fetch::given(&site, &format!("{files}/Patch/00421/00420to00421.patch"), 1000);
        assert_eq!(
            (main.label.as_str(), main.file_name.as_str(), main.size),
            ("00420to00421.patch", "00420to00421.patch", 1000)
        );
        assert_eq!(main.known.as_ref().map(|target| (target.version, target.minor)), Some((421, None)));
        let minor = Fetch::given(&site, &format!("{files}/Patch/00421/Minor/03to04.patch"), 200);
        assert_eq!((minor.label.as_str(), minor.file_name.as_str()), ("03to04.patch", "00421-minor-03to04.patch"));
        assert_eq!(minor.known.as_ref().map(|target| (target.version, target.minor)), Some((421, Some(4))));

        for elsewhere in ["/PatchT/01208/01207to01208.patch", "/Patch/00421/00420to00421-fix.patch", "/Patch/a.patch"] {
            let other = Fetch::given(&site, &format!("{files}{elsewhere}"), 300);
            let name = elsewhere.rsplit('/').next().unwrap();
            assert_eq!((other.label.as_str(), other.file_name.as_str(), other.size), (name, name, 300), "{elsewhere}");
            assert!(other.known.is_none() && other.given, "{elsewhere}");
        }

        // A run by address goes by the address when it is taken up again, and a run of a
        // patch the app found by the name its file is kept under, as it always did.
        assert!(main.given && main.id() == main.url);
        let found = Fetch::found(main.known.as_ref().unwrap());
        assert!(!found.given && found.id() == "00420to00421.patch");
    }

    #[test]
    fn a_patch_is_fetched_over_https_and_nothing_else_except_by_a_build_for_trying_the_app_out() {
        let given_out = false;
        assert!(fetched_safely("https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch", given_out));
        let others = [
            "http://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch",
            "HTTP://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch",
            "ftp://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch",
            "file:///C:/patches/00421to00422.patch",
            r"\\nas\patches\00421to00422.patch",
            " https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch",
            "",
        ];
        for address in others {
            assert!(!fetched_safely(address, given_out), "{address:?}");
        }
        // A made-up server on this PC speaks plain HTTP, and only a build for trying the
        // app out is ever pointed at one.
        assert!(fetched_safely("http://127.0.0.1:18421/Patch/00901/00900to00901.patch", true));
        // Where the clients' patches are published is fetched from in any build.
        for server in mfp_kms::Server::ALL {
            let client = server.client();
            assert!(fetched_safely(&client.patch_url(1207, 1208), given_out));
            assert!(fetched_safely(&client.minor_patch_url(1208, 1, 2), given_out));
        }
    }
}
