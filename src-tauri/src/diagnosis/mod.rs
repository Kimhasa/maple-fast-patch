//! The diagnosis of the PC: what kind of PC this is, whether a main patch is expected to go
//! through on it, and whether what a patch wrote is still there after a restart.
//!
//! The app was made for a PC at home, and a seat in a PC room is another matter. Which one
//! this is, the diagnostic finds out (`mfp-check`, which is also a program of its own: both
//! say the same, since both ask the same library). The app has it look by itself, once for
//! each client that has an install folder, and shows what it said on a page of its own.
//!
//! By itself the app runs only the part of a diagnosis that asks nothing. It is told what
//! the app knows already, so it writes nothing and does not ask the server what it has:
//! whether the folder takes writes, what Nexon's server has, and whether the game is open,
//! in which case it reads nothing from the game's files either. The one thing it asks of
//! the server is how fast the line to it is: a piece of the newest patch is fetched and the
//! way there is timed, where the app knows what the server has. One further step is run
//! when the user asks for it, the check of the game's files, which only reads. The steps
//! that write are the other program's. What a diagnosis came to is kept in memory, while
//! the app is open.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mfp_check::{Kind, Outlook, Plain, Verdict};
use mfp_kms::Server;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::desk::update;
use crate::shown::{Row, RowKind};
use crate::state::{CheckKind, Found as Checked, Patcher, Site, Status};
use crate::wording::asks_to_check_files;

/// How long a diagnosis of a folder waits for the server's first answer about it, so that
/// it can be told what the server has. A folder nothing is asked about, or a server that
/// does not answer, does not keep it waiting longer.
const WAITS_FOR_ANSWER: Duration = Duration::from_secs(10);
/// How often at most the page is told how far a check of the game's files is.
const SAID_EVERY: Duration = Duration::from_millis(100);
/// How long the app waits, as it closes, for a check of the game's files to stop.
const STOPS_WITHIN: Duration = Duration::from_secs(5);

/// What a diagnosis is told, which is all of it things the app knows already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asked {
    pub(crate) install: PathBuf,
    pub(crate) server: Server,
    /// The user picked the folder; otherwise it is the one the Nexon installer registered.
    pub(crate) custom: bool,
    /// The game is open: nothing is read from its files.
    pub(crate) game_running: bool,
    /// What the app's own test of writing to the folder came to, where it made one.
    pub(crate) writable: Option<bool>,
    /// What a run of the patch that is waiting asks the drive to have free, the usual way
    /// and the immediate way, where the patch file is on this PC and has been read.
    pub(crate) room: Option<(u64, u64)>,
    /// What the last check found Nexon's server to have, where it is known for certain.
    pub(crate) shelf: Option<Shelf>,
    /// The line to Nexon's server is timed, where what the server has is known: how fast
    /// a piece of its newest patch comes, and how long the way there is. Nothing else is
    /// asked of the server.
    pub(crate) time_the_line: bool,
}

/// What Nexon's server has for a client, as the last check found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shelf {
    /// The newest version there, and its newest minor version.
    pub(crate) newest: (u32, u32),
    /// The patch the folder takes next, by Nexon's name for its file, and its size.
    pub(crate) patch: Option<(String, u64)>,
    /// That patch is a client patch, which brings a new version.
    pub(crate) patch_is_main: bool,
}

/// What a diagnosis is told about the folder `status` is of; `checked` is what the last
/// check found for it. `None` where there is no folder.
pub(crate) fn asked_of(server: Server, status: &Status, checked: Option<&Checked>) -> Option<Asked> {
    let check = |kind: CheckKind| status.checks.iter().find(|check| check.kind == kind);
    let shelf = |checked: &Checked| {
        let target = checked.target.as_ref();
        // Nexon's own name for the file is the end of its address.
        let named = |address: &str| address.rsplit('/').next().unwrap_or(address).to_owned();
        Some(Shelf {
            newest: checked.newest?,
            patch: target.map(|target| (named(&target.url), target.size)),
            patch_is_main: target.is_some_and(|target| target.minor.is_none()),
        })
    };
    Some(Asked {
        install: PathBuf::from(status.install_dir.as_ref()?),
        server,
        custom: status.custom_dir,
        game_running: check(CheckKind::Running).is_some_and(|open| !open.ok),
        writable: check(CheckKind::Writable).map(|writable| writable.ok),
        room: status.room,
        shelf: checked.and_then(shelf),
        // The report says how fast the line is, so the app's own diagnosis measures it.
        time_the_line: true,
    })
}

/// What a diagnosis came to: its verdict, and its whole report as text. The report's first
/// line names the app and its version, which the diagnostic is told when it is asked for a
/// diagnosis. Nothing in the report names the PC or the person at it; the diagnostic sees
/// to that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) verdict: Verdict,
    pub(crate) report: String,
}

/// A diagnosis as the diagnostic hands it back: what it came to, and what can be gone on
/// with from it. The app asks through this, so that what it does with a diagnosis is tried
/// on made-up ones.
pub(crate) trait Kept: Send + Sync {
    fn found(&self) -> Found;
    /// Whether the game's files can be checked from here, and about how many seconds that
    /// takes where the drive was timed. Not where the game was open when the diagnosis was
    /// made, where there is no folder, and where every file has been read already.
    fn check_takes(&self) -> Option<Option<f64>>;
    /// Checks the game's files: those up to 64 MB are read and held against what Nexon's
    /// patches say of them, the larger ones by their size. `said` hears how many bytes of
    /// how many are read. Hands back the diagnosis with what the check found added, or
    /// nothing where `stop` ended it first.
    fn check(&self, stop: &AtomicBool, said: &mut dyn FnMut(u64, u64)) -> Option<Arc<dyn Kept>>;
    /// The same diagnosis, told what a run of the patch that is waiting asks the drive to
    /// have free (`room`, the usual way and the immediate way), or that this is not known.
    /// Nothing is looked at again: what is said of the patch goes by these figures.
    fn with_room(&self, room: Option<(u64, u64)>) -> Arc<dyn Kept>;
}

/// One of the three answers of a verdict as the page shows it, on a line of its own: what
/// is asked, the answer, and the one reason that is said for it. Everything else an answer
/// rests on is in the report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Answer {
    title: String,
    answer: String,
    reason: String,
    tone: Tone,
}

/// How an answer is to be taken, which is the colour the page writes it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum Tone {
    /// Good news: a patch goes through with nothing it hangs on, or stays.
    #[serde(rename = "good")]
    Good,
    /// Something to know of: a patch goes through on a condition, or is lost at a restart.
    #[serde(rename = "caution")]
    Caution,
    /// Neither. What kind of PC this is, an answer that could not be had, and a patch that
    /// does not go through: the page says so and paints nothing, since it keeps nobody
    /// from trying.
    #[serde(rename = "")]
    Plain,
}

/// The answers the diagnostic gives of a patch that goes through, in its plain words:
/// outright, and by the immediate way alone.
const GOES: &str = "가능";
const GOES_AT_ONCE: &str = "즉시 패치로 가능";

/// The three answers of `verdict` in plain words, each with its name and its one reason:
/// whether a client patch goes through, what kind of PC this is, and what becomes of the
/// patch at a restart. The patch comes first: it is what the page is opened for. A player
/// reads these, and they are the diagnostic's own: the name, the answer and the reason of
/// every line come from its plain words and from nowhere else. The report keeps the exact
/// words, for whoever it is copied to.
///
/// How an answer is to be taken is said beside it ([`Tone`]). On an ordinary PC a patch
/// that goes through is good news; anywhere else it goes through as long as what its
/// reason names holds, and by the immediate way alone it does so on a condition too.
pub(crate) fn answers(verdict: &Verdict) -> Vec<Answer> {
    let answer = |plain: &Plain, tone: Tone| Answer {
        title: plain.name.to_owned(),
        answer: plain.answer.to_owned(),
        reason: plain.reason.clone(),
        tone,
    };
    let patch = match (verdict.patch_plain.answer, verdict.kind) {
        (GOES, Kind::Ordinary) => Tone::Good,
        (GOES | GOES_AT_ONCE, _) => Tone::Caution,
        _ => Tone::Plain,
    };
    let kept = match verdict.kept {
        Outlook::Expected => Tone::Good,
        Outlook::Unlikely => Tone::Caution,
        Outlook::Unknown => Tone::Plain,
    };
    vec![
        answer(&verdict.patch_plain, patch),
        answer(&verdict.kind_plain, Tone::Plain),
        answer(&verdict.kept_plain, kept),
    ]
}

/// A diagnosis in one line, for the log: the kind of PC, and what is expected of a main
/// patch and of a restart. Nothing else of a diagnosis is written down anywhere.
pub(crate) fn summary(verdict: &Verdict) -> String {
    let kind = match verdict.kind {
        Kind::Ordinary => "an ordinary PC",
        Kind::DiskWithRestore => "a disk that is put back at every start",
        Kind::Diskless => "a seat without a disk",
        Kind::Unknown => "a PC of no kind that can be told",
    };
    let outlook = |outlook: Outlook| match outlook {
        Outlook::Expected => "expected",
        Outlook::Unlikely => "unlikely",
        Outlook::Unknown => "not known",
    };
    format!(
        "diagnosis: {kind}; a main patch going through is {}, and its staying after a restart is {}",
        outlook(verdict.patch),
        outlook(verdict.kept)
    )
}

/// The line a client's own page shows of a diagnosis: only of a PC that is no ordinary
/// one, a seat without a disk or with a disk that is put back. It says which, in the plain
/// words the page of the diagnosis says it in, and is nothing in the way of a patch.
pub(crate) fn row(verdict: &Verdict) -> Option<Row> {
    matches!(verdict.kind, Kind::Diskless | Kind::DiskWithRestore).then(|| Row {
        kind: RowKind::Diagnosis,
        label: "PC 검사".to_owned(),
        ok: None,
        detail: verdict.kind.plain().to_owned(),
        message: String::new(),
    })
}

/// What the page is told of a client's diagnosis.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosisView {
    /// A diagnosis is at work.
    working: bool,
    /// The check of the game's files is at work. How far it is, the page hears as it goes.
    checking: bool,
    /// That check can be asked for now.
    can_check: bool,
    /// The verdict's three answers, and the whole report. Empty until a diagnosis has
    /// ended; while another is at work they are still those of the one before.
    answers: Vec<Answer>,
    report: String,
}

impl DiagnosisView {
    /// The check of the game's files is at work.
    pub(crate) fn checking(&self) -> bool {
        self.checking
    }
}

/// How far a check of the game's files is, with the client it is of.
#[derive(Clone, Serialize)]
struct CheckingWord {
    server: &'static str,
    done: u64,
    total: u64,
}

/// Where a site is with its diagnosis.
#[derive(Default)]
pub(crate) struct Diagnosed {
    /// The folder the site has, as the desk last saw it, and since when it has had it.
    seen: Option<(PathBuf, Instant)>,
    /// The folder the last diagnosis was begun of.
    begun_of: Option<PathBuf>,
    /// A diagnosis is at work: what it is told to stop by.
    at_work: Option<Arc<AtomicBool>>,
    /// The user asked for another.
    again: bool,
    /// The diagnosis as it stands, what it came to, and whether the game's files can be
    /// checked from it.
    kept: Option<Arc<dyn Kept>>,
    found: Option<Found>,
    checkable: bool,
    /// What a run of the patch that is waiting asks the drive to have free, as the desk
    /// last saw it. The diagnosis that stands is told it, whenever either is another.
    room: Option<(u64, u64)>,
    /// The check of the game's files is at work: what it is told to stop by.
    check: Option<Arc<AtomicBool>>,
}

impl Diagnosed {
    /// What the page is told. `None` for a site without a folder: there is nothing to
    /// diagnose, and the page has no tab for it. `free` says that the game is closed and
    /// nothing works in the folder, without which no check of its files is offered.
    ///
    /// The report is the diagnostic's, as it wrote it: its first line says the version of
    /// the app it is copied out of.
    pub(crate) fn view(&self, free: bool) -> Option<DiagnosisView> {
        self.seen.as_ref()?;
        let found = self.found.as_ref();
        Some(DiagnosisView {
            working: self.at_work.is_some(),
            checking: self.check.is_some(),
            can_check: free && self.may_check(),
            answers: found.map(|found| answers(&found.verdict)).unwrap_or_default(),
            report: found.map(|found| found.report.clone()).unwrap_or_default(),
        })
    }

    /// The line of the client's own page; see [`row`].
    pub(crate) fn row(&self) -> Option<Row> {
        row(&self.found.as_ref()?.verdict)
    }

    /// The check of the game's files is at work. No patch run begins in the folder then.
    pub(crate) fn checking(&self) -> bool {
        self.check.is_some()
    }

    /// The check can be begun as far as the diagnosis goes: there is one to go on from, it
    /// takes the check, and neither it nor a diagnosis is at work.
    fn may_check(&self) -> bool {
        self.kept.is_some() && self.checkable && self.at_work.is_none() && self.check.is_none()
    }

    /// The site has `folder` now, or none. What was found of another folder goes with it.
    fn see(&mut self, folder: Option<&Path>, now: Instant) {
        if self.seen.as_ref().map(|(seen, _)| seen.as_path()) == folder {
            return;
        }
        self.seen = folder.map(|folder| (folder.to_path_buf(), now));
        (self.kept, self.found, self.checkable, self.again) = (None, None, false, false);
        self.put_off();
        self.stop_check();
    }

    /// What a run of the patch that is waiting asks the drive to have free is `room` now.
    /// Says whether the diagnosis that stands was told so: it was where it had been told
    /// otherwise, and what the page says of the patch may be another line for it.
    fn room_is(&mut self, room: Option<(u64, u64)>) -> bool {
        if self.room == room {
            return false;
        }
        self.room = room;
        match self.kept.clone() {
            Some(kept) => self.take(kept),
            None => false,
        }
    }

    /// Ends the diagnosis that is at work, if one is, for it to be made anew later: a run
    /// is about to work in the folder, or the folder is another one.
    pub(crate) fn put_off(&mut self) {
        if let Some(stop) = &self.at_work {
            stop.store(true, Ordering::SeqCst);
            self.begun_of = None;
        }
    }

    /// Whether a diagnosis begins now; see [`begins`]. If so, it is at work from here on,
    /// and what it is told to stop by is handed back.
    fn begin(&mut self, quiet: bool, answered: bool, now: Instant) -> Option<Arc<AtomicBool>> {
        let (folder, since) = self.seen.clone()?;
        let at = At {
            at_work: self.at_work.is_some() || self.check.is_some(),
            diagnosed: self.begun_of.as_deref() == Some(folder.as_path()),
            again: self.again,
            quiet,
            answered,
            seen_for: now.saturating_duration_since(since),
        };
        if !begins(&at) {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        (self.begun_of, self.again, self.at_work) = (Some(folder), false, Some(stop.clone()));
        Some(stop)
    }

    /// The diagnosis that was told to stop by `stop` has ended, with `kept`, or with
    /// nothing where it was stopped or could not be made. Says whether it is taken: not
    /// that of a diagnosis that was put off, whose folder may be another by now.
    fn end(&mut self, stop: &Arc<AtomicBool>, kept: Option<Arc<dyn Kept>>) -> bool {
        if self.at_work.as_ref().is_some_and(|at_work| Arc::ptr_eq(at_work, stop)) {
            self.at_work = None;
        }
        match kept.filter(|_| !stop.load(Ordering::SeqCst)) {
            Some(kept) => self.take(kept),
            None => false,
        }
    }

    /// `kept` is the diagnosis from here on.
    fn take(&mut self, kept: Arc<dyn Kept>) -> bool {
        // Whatever made it, it is told the room a run needs as that is known now.
        let kept = kept.with_room(self.room);
        (self.found, self.checkable) = (Some(kept.found()), kept.check_takes().is_some());
        self.kept = Some(kept);
        true
    }

    /// Begins the check of the game's files, where it can be: see [`Diagnosed::may_check`],
    /// and `free` as for [`Diagnosed::view`]. Hands back the diagnosis to go on from and
    /// what the check is told to stop by.
    fn begin_check(&mut self, free: bool) -> Option<(Arc<dyn Kept>, Arc<AtomicBool>)> {
        let kept = self.kept.clone().filter(|_| free && self.may_check())?;
        let stop = Arc::new(AtomicBool::new(false));
        self.check = Some(stop.clone());
        Some((kept, stop))
    }

    /// The check that was told to stop by `stop` has ended, with the diagnosis it added
    /// to, or with nothing where it was stopped. Says whether that is the diagnosis from
    /// here on: a check that was stopped leaves what was there as it was.
    fn end_check(&mut self, stop: &Arc<AtomicBool>, kept: Option<Arc<dyn Kept>>) -> bool {
        let own = self.check.as_ref().is_some_and(|check| Arc::ptr_eq(check, stop));
        if own {
            self.check = None;
        }
        match kept.filter(|_| own && !stop.load(Ordering::SeqCst)) {
            Some(kept) => self.take(kept),
            None => false,
        }
    }

    /// Tells the check that is at work to stop, if one is. It is at work until it has.
    fn stop_check(&mut self) {
        if let Some(stop) = &self.check {
            stop.store(true, Ordering::SeqCst);
        }
    }
}

/// Where a site with a folder is, for the question whether a diagnosis of it begins.
#[derive(Clone, Copy, Debug)]
pub(crate) struct At {
    /// A diagnosis is at work, or the check of the game's files.
    pub(crate) at_work: bool,
    /// The last diagnosis that was begun, and not put off, was of the folder as it is.
    pub(crate) diagnosed: bool,
    /// The user asked for another.
    pub(crate) again: bool,
    /// Nothing works in the folder: no patch run, and nothing that puts it back.
    pub(crate) quiet: bool,
    /// The server has answered a check about the folder.
    pub(crate) answered: bool,
    /// How long the site has had the folder.
    pub(crate) seen_for: Duration,
}

/// Whether a diagnosis of a site's folder begins.
///
/// One begins by itself for a folder that has had none: once the server has answered about
/// the folder, so that the diagnosis can be told what the server has, or once that has
/// been waited for long enough. Another begins when the user asks for it, at once. One at a
/// time, and none while a run works in the folder or the folder is being put back: a
/// diagnosis reads a sample of the game's files, and that waits until the folder is quiet.
pub(crate) fn begins(at: &At) -> bool {
    let first = !at.diagnosed && (at.answered || at.seen_for >= WAITS_FOR_ANSWER);
    !at.at_work && at.quiet && (at.again || first)
}

#[cfg(windows)]
impl Kept for mfp_check::Diagnosis {
    fn found(&self) -> Found {
        Found { verdict: self.verdict(), report: self.report() }
    }

    fn check_takes(&self) -> Option<Option<f64>> {
        let offer = self.offer(mfp_check::Extra::Checksums, None);
        offer.refused.is_none().then_some(offer.seconds)
    }

    fn check(&self, stop: &AtomicBool, said: &mut dyn FnMut(u64, u64)) -> Option<Arc<dyn Kept>> {
        let checked = self.run(mfp_check::Extra::Checksums, None, stop, &mut |at| said(at.done, at.total));
        // A check that was stopped has found half of it. That is not shown for the whole.
        (!checked.stopped() && !stop.load(Ordering::SeqCst)).then(|| Arc::new(checked) as Arc<dyn Kept>)
    }

    fn with_room(&self, room: Option<(u64, u64)>) -> Arc<dyn Kept> {
        Arc::new(self.with_room(room.map(|(safe, immediate)| mfp_check::Room { safe, immediate })))
    }
}

/// Runs a diagnosis of what `asked` names and hands it back. `None` where it was told to
/// stop before it was done, and where there is no diagnostic to run. `version` is the
/// app's own, which the first line of the report says.
pub(crate) fn diagnose(asked: &Asked, version: &str, stop: &AtomicBool) -> Option<Arc<dyn Kept>> {
    #[cfg(windows)]
    {
        use mfp_check::facts::{How, PatchOnShelf, WriteTest};
        use mfp_check::known::State;
        use mfp_check::{Diagnosis, Head, OnShelf, Request};
        let mut request = Request::of(&asked.install, asked.server);
        request.head = Head::App(version.to_owned());
        request.how = if asked.custom { How::Given } else { How::Registry };
        request.game_running = asked.game_running;
        // The app has tried the folder itself, or has not: either way no file is made here.
        request.write_test = false;
        request.writable = asked.writable.map(|took| if took { WriteTest::Passed } else { WriteTest::Failed(None) });
        // The server is not asked what it has: the app has asked already, and says what
        // it was told. How fast the line to it is, nobody has measured: that is asked.
        request.online = false;
        request.shelf = asked.shelf.as_ref().map(|shelf| OnShelf {
            newest: State { version: shelf.newest.0, minor: shelf.newest.1 },
            patch: shelf.patch.clone().map(|(file_name, size)| PatchOnShelf { file_name, size, originals: None }),
            patch_is_next_main: shelf.patch_is_main,
        });
        request.time_the_line = asked.time_the_line;
        request.room = asked.room.map(|(safe, immediate)| mfp_check::Room { safe, immediate });
        let diagnosis = Diagnosis::start(&request, stop, &mut |_| {});
        (!diagnosis.stopped()).then(|| Arc::new(diagnosis) as Arc<dyn Kept>)
    }
    #[cfg(not(windows))]
    {
        let _ = (asked, version, stop);
        None
    }
}

/// Whether the folder of `site` is quiet, with no run at work in it and nothing putting it
/// back, and whether it is free, which is quiet with the game closed as well. A diagnosis
/// begins in a quiet folder, and the game's files are checked in a free one.
fn quiet_and_free(patcher: &Patcher, site: &Site) -> (bool, bool) {
    let (busy, open) = {
        let flow = site.flow.lock().unwrap();
        let status = flow.status.as_ref();
        let game = |status: &Status| status.checks.iter().any(|check| check.kind == CheckKind::Running && !check.ok);
        (flow.phase.at_work() || status.is_some_and(|status| status.putting_back), status.is_none_or(game))
    };
    let quiet = !(busy || patcher.running_for(site) || site.heal.lock().unwrap().at_work);
    (quiet, quiet && !open)
}

/// What a diagnosis of the folder of `site` is told, where it has one, and whether the
/// server has answered a check about that folder.
fn to_ask(site: &Site) -> (Option<Asked>, bool) {
    let (status, answered) = {
        let flow = site.flow.lock().unwrap();
        (flow.status.clone(), flow.answer.is_some())
    };
    let checked = status.as_ref().and_then(|status| site.found_for(Path::new(status.install_dir.as_ref()?)));
    (status.as_ref().and_then(|status| asked_of(site.client.server, status, checked.as_ref())), answered)
}

/// What the page is told of the diagnosis of `site`, and the line its own page shows of it.
pub(crate) fn shown_of(patcher: &Patcher, site: &Site) -> (Option<DiagnosisView>, Option<Row>) {
    let (_, free) = quiet_and_free(patcher, site);
    let diagnosed = site.diagnosis.lock().unwrap();
    (diagnosed.view(free), diagnosed.row())
}

/// Has the folder of the site at `place` diagnosed, when one is due; see [`begins`].
///
/// It is done on a thread of its own: it takes seconds, up to ten on a seat that reads its
/// game over the network, and a few more where the line to Nexon's server is slow, which
/// it times. The page is told when it begins and when it has ended, and the log gets its
/// one line.
pub(crate) fn diagnose_if_due(app: &AppHandle, patcher: &Patcher, place: usize) {
    let site = &patcher.sites[place];
    let ((asked, answered), (quiet, _)) = (to_ask(site), quiet_and_free(patcher, site));
    let told_room;
    let begun = {
        let mut diagnosed = site.diagnosis.lock().unwrap();
        let now = Instant::now();
        diagnosed.see(asked.as_ref().map(|asked| asked.install.as_path()), now);
        told_room = diagnosed.room_is(asked.as_ref().and_then(|asked| asked.room));
        diagnosed.begin(quiet, answered, now)
    };
    // What is said of the patch goes by the room a run needs: the page is told anew.
    if told_room {
        update(app, patcher, site, |_| ());
    }
    let (Some(asked), Some(stop)) = (asked, begun) else { return };
    // The page shows that one is at work.
    update(app, patcher, site, |_| ());
    let app = app.clone();
    std::thread::spawn(move || {
        let patcher = app.state::<Patcher>();
        let site = &patcher.sites[place];
        let kept = diagnose(&asked, &patcher.version, &stop);
        let line = kept.as_ref().map(|kept| summary(&kept.found().verdict));
        if site.diagnosis.lock().unwrap().end(&stop, kept)
            && let Some(line) = line
        {
            site.log(&line);
        }
        update(&app, &patcher, site, |_| ());
    });
}

/// Has the folder of the client the page calls `server` diagnosed again: the user asked.
/// It begins at the desk's next round, which this wakes, or when the folder is quiet.
#[tauri::command(async)]
pub(crate) fn diagnose_again(app: AppHandle, patcher: State<Patcher>, server: String) {
    let Some(site) = patcher.site_named(&server) else { return };
    site.diagnosis.lock().unwrap().again = true;
    update(&app, &patcher, site, |_| ());
}

/// What the user is asked before the game's files of the client the page calls `server`
/// are checked, with about how long that takes on this PC. `None` where the check cannot
/// be begun now: the page then asks nothing, and its button for the check is off.
#[tauri::command(async)]
pub(crate) fn ask_file_check(patcher: State<Patcher>, server: String) -> Option<String> {
    let site = patcher.site_named(&server)?;
    let (_, free) = quiet_and_free(&patcher, site);
    let kept = {
        let diagnosed = site.diagnosis.lock().unwrap();
        diagnosed.kept.clone().filter(|_| free && diagnosed.may_check())?
    };
    // Asked anew: how fast the drive reads was measured, and what is left to read is known.
    kept.check_takes().map(asks_to_check_files)
}

/// Begins the check of the game's files of the client the page calls `server`: the user
/// said yes to being asked. It is done on a thread of its own, and the page hears how far
/// it is. When it has ended, what it found is the diagnosis; stopped, it changes nothing.
///
/// No patch run begins in the folder while the check is at work, and the check does not
/// begin while one is. Each side says what it is about to do and then looks at the other,
/// so that one of the two gives way.
#[tauri::command(async)]
pub(crate) fn start_file_check(app: AppHandle, patcher: State<Patcher>, server: String) {
    let Some(site) = patcher.site_named(&server) else { return };
    let Some(place) = patcher.place(site) else { return };
    let (_, free) = quiet_and_free(&patcher, site);
    let Some((kept, stop)) = site.diagnosis.lock().unwrap().begin_check(free) else { return };
    if !quiet_and_free(&patcher, site).1 {
        site.diagnosis.lock().unwrap().end_check(&stop, None);
        return;
    }
    update(&app, &patcher, site, |_| ());
    std::thread::spawn(move || {
        let patcher = app.state::<Patcher>();
        let site = &patcher.sites[place];
        let server = site.client.server.key();
        let mut last: Option<Instant> = None;
        let checked = kept.check(&stop, &mut |done, total| {
            if last.is_none_or(|last| last.elapsed() >= SAID_EVERY) {
                last = Some(Instant::now());
                let _ = app.emit("checking", CheckingWord { server, done, total });
            }
        });
        let line = checked.as_ref().map(|checked| summary(&checked.found().verdict));
        if site.diagnosis.lock().unwrap().end_check(&stop, checked)
            && let Some(line) = line
        {
            site.log(&format!("the game's files were checked. {line}"));
        }
        update(&app, &patcher, site, |_| ());
    });
}

/// Stops the check of the game's files of the client the page calls `server`: the user
/// said so. What was found before the check stays as it was.
#[tauri::command]
pub(crate) fn stop_file_check(patcher: State<Patcher>, server: String) {
    if let Some(site) = patcher.site_named(&server) {
        site.diagnosis.lock().unwrap().stop_check();
    }
}

/// The app is closing: every check of the game's files is told to stop, and waited for.
/// A check only reads, so one that does not stop in time is left behind without harm.
pub(crate) fn stop_checks(patcher: &Patcher) {
    patcher.sites.iter().for_each(|site| site.diagnosis.lock().unwrap().stop_check());
    let asked = Instant::now();
    let at_work = || patcher.sites.iter().any(|site| site.diagnosis.lock().unwrap().checking());
    while at_work() && asked.elapsed() < STOPS_WITHIN {
        std::thread::sleep(Duration::from_millis(20));
    }
}
