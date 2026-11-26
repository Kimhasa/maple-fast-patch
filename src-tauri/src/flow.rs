//! The flow of a site: what is known of its install folder and of the patches for it, the
//! run that is on its screen, and the rules by which the app goes from one to the next by
//! itself.
//!
//! Everything here is plain data and rules over it. Nothing asks the window, the clock or
//! the drive: whoever calls says what time it is and what was found. The desk is what
//! calls (see [`crate::desk`]), and [`crate::shown`] is what the screen is made of.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use mfp_kms::notice::Minutes;
use serde::Serialize;

use crate::offer::{CheckEnd, Checked};
use crate::state::{Availability, CheckKind, Status, Step, Target};

/// How often the install folder is looked at, and how often while nobody can see the window
/// and nothing is in the way that might go away.
const LOOK_EVERY: Duration = Duration::from_secs(3);
const LOOK_EVERY_UNSEEN: Duration = Duration::from_secs(30);
/// How long after a check that had to give up the next one is made.
const RETRY: Duration = Duration::from_secs(5);
/// A download that brings nothing for this long is waiting for the server.
const QUIET: Duration = Duration::from_secs(4);
/// How long the button that starts the game rests once the game was started.
const LAUNCH_REST: Duration = Duration::from_secs(5);

/// Where a site stands with its run. This is the one place that says so: the screen, the
/// buttons and what the app does by itself all go by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Phase {
    /// No run is on the screen.
    Ready,
    /// A run was asked for and is about to begin: it waits for a check to finish.
    Starting,
    Running,
    /// The run at work was asked to stop and has not stopped yet.
    Stopping,
    /// The run was stopped and can be taken up again. What it was working on stays on screen.
    Paused,
    /// The run failed or was turned down. What it was working on stays on screen.
    Failed,
    /// The run has finished, and how it went is on the screen.
    Done,
}

impl Phase {
    /// A run is at work.
    pub(crate) fn at_work(self) -> bool {
        matches!(self, Phase::Starting | Phase::Running | Phase::Stopping)
    }

    /// A run that stopped before it was done is on the screen.
    pub(crate) fn halted(self) -> bool {
        matches!(self, Phase::Paused | Phase::Failed)
    }
}

/// What the screen says once of a run that failed or was turned down.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Failure {
    /// Counts the failures of the site. The page shows each once, by this.
    pub(crate) id: u64,
    /// The headline: that the patch failed, or, for a run that was turned down, what was
    /// in the way.
    pub(crate) title: String,
    pub(crate) message: Option<String>,
    pub(crate) details: Vec<String>,
}

/// The run that is on a site's screen: the one at work, or one that has ended and was not
/// put away yet.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Run {
    /// The patch file the user picked for it; `None` for the patch from the server.
    pub(crate) source: Option<String>,
    /// The version a patch from the server leads to.
    pub(crate) label: Option<String>,
    /// The stop was asked for by closing the window; the app goes once the run has stopped.
    pub(crate) leaving: bool,
    /// How far the bar has come, 0 to 100. It never goes back within a patch.
    pub(crate) percent: f64,
    /// The step the run is at; `None` before it has said.
    pub(crate) step: Option<Step>,
    /// Bytes done and total within that step, and how many of them this run worked for.
    pub(crate) done: u64,
    pub(crate) total: u64,
    pub(crate) worked: u64,
    /// What the run is working on, as in `생성중: Map001.wz`.
    pub(crate) activity: String,
    pub(crate) failure: Option<Failure>,
    /// What the step is called while it is at work.
    #[serde(skip)]
    pub(crate) status: &'static str,
    /// The run was turned down by a check, as opposed to having failed at its work.
    #[serde(skip)]
    pub(crate) refused: bool,
    /// Of a finished run: how long the patch took, in seconds, and how many files it changed.
    #[serde(skip)]
    pub(crate) took: u64,
    #[serde(skip)]
    pub(crate) files: usize,
}

/// A paused or failed run that was put away for the ready screen: what it was applying and
/// how far it had come. Taking the same patch up again from there carries on on screen
/// where it stopped, instead of looking like a new run.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Left {
    source: Option<String>,
    label: Option<String>,
    percent: f64,
    activity: String,
}

/// Where a run is, as it says so up to a dozen times a second.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Progress {
    /// Overall progress, 0 to 100.
    pub(crate) percent: f64,
    pub(crate) step: Option<Step>,
    pub(crate) status: &'static str,
    /// Bytes done and total within the current step.
    pub(crate) done: u64,
    pub(crate) total: u64,
    /// Bytes of `done` this run worked for itself. The rest was found done by an earlier run,
    /// and says nothing about how fast the step is going.
    pub(crate) worked: u64,
    /// What is being worked on, the way the game's own patcher shows it: `생성중: Map001.wz`.
    /// Empty for a step that has named no file yet.
    pub(crate) activity: String,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunEnd {
    /// The patch is applied: how long it took over all its runs, in seconds, and how many
    /// files it changed.
    Done {
        seconds: u64,
        files: usize,
    },
    /// Paused, by the user or by the window being closed. What was done is kept.
    Stopped,
    /// Turned down by a check: something is in the way, or the patch file is not one the
    /// folder can take. Nothing the run did went wrong.
    Refused {
        message: String,
        details: Vec<String>,
    },
    Failed {
        message: String,
        details: Vec<String>,
    },
}

/// Why a run was not begun.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Refusal {
    pub(crate) kind: RefusalKind,
    /// What is in the way, a sentence each. The first is the one to lead with.
    pub(crate) messages: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RefusalKind {
    /// A check made before a patch may start has not passed.
    Blocked,
    /// A run is at work already.
    Busy,
    /// This version of the app is too old to patch with.
    Outdated,
    /// There is no install folder, or no patch for it.
    Nothing,
    /// The file that was picked is no patch file.
    NotPatch,
}

impl Refusal {
    pub(crate) fn new(kind: RefusalKind, message: &str) -> Refusal {
        Refusal { kind, messages: vec![message.to_owned()] }
    }

    pub(crate) fn busy() -> Refusal {
        Refusal::new(RefusalKind::Busy, "이미 패치가 진행 중입니다.")
    }
}

/// The patch a run is of: a patch file on this PC, or the one the server has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Patch {
    File(String),
    Server(Target),
}

/// The patch a run is begun of, and how it stands on the screen as it begins.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pick {
    pub(crate) patch: Patch,
    /// Where the bar stands and what the line above it says before the run has said
    /// anything: what a run of the same patch had come to, when one was left unfinished.
    percent: f64,
    activity: String,
}

impl Pick {
    /// The patch file the user picked, when the run is of one.
    pub(crate) fn file(&self) -> Option<&str> {
        match &self.patch {
            Patch::File(file) => Some(file),
            Patch::Server(_) => None,
        }
    }

    /// The patch from the server, when the run is of that.
    pub(crate) fn target(&self) -> Option<&Target> {
        match &self.patch {
            Patch::File(_) => None,
            Patch::Server(target) => Some(target),
        }
    }
}

/// What the app does by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Nothing,
    /// Start the patch that is waiting: `file` when it is one the user had picked.
    StartPatch {
        file: Option<String>,
    },
    StartGame,
    /// The game was to be started after a patch, and is not: another patch is waiting, or
    /// that could not be ruled out.
    DropGameStart,
}

/// What the rules go by that is not the site's own: what the user has set, what the app as
/// a whole is doing, and what time it is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct World {
    pub(crate) auto_patch: bool,
    pub(crate) auto_start: bool,
    /// This version of the app is too old to patch with; a newer one has to be got first.
    pub(crate) outdated: bool,
    /// A run is at work, for this client or for another.
    pub(crate) run_at_work: bool,
    /// Whether the game can be played, as the lamp says; `None` while that is not known.
    pub(crate) open: Option<bool>,
    pub(crate) now: Instant,
}

/// What a site knows, and what it has told the page.
pub(crate) struct Flow {
    /// What the last look at the install folder found.
    pub(crate) status: Option<Status>,
    /// What the last check found. `None` until one has answered, and again whenever what
    /// was found no longer holds: another folder, a patch that was applied, a check that
    /// was asked for anew.
    pub(crate) answer: Option<Availability>,
    /// The newest version the server was last seen to have for this folder. It outlives an
    /// answer that is put aside.
    pub(crate) latest: Option<String>,
    /// When the server last answered a check, in milliseconds since 1970.
    pub(crate) checked_at: Option<u64>,
    /// Counts the install folders the site has had. A check is about the folder it began
    /// under, and its answer is of no use once that has changed.
    pub(crate) folder_epoch: u64,
    /// A check is at work.
    pub(crate) checking: bool,
    /// When the next check is due.
    pub(crate) next_check: Instant,
    /// How many checks in a row could not ask the server.
    pub(crate) failures: u32,
    /// Counts what the page was told. What it is told later has the higher number.
    pub(crate) rev: u64,
    pub(crate) phase: Phase,
    pub(crate) run: Option<Run>,
    /// The patch file the user picked for the run that is on screen or was last left
    /// unfinished. A run that is taken up again from the ready screen is of this file.
    source: Option<String>,
    left: Option<Left>,
    /// The patches that are the user's to start for the rest of the session: a run of each
    /// was paused, failed or turned down. Without this they would begin again by
    /// themselves every time the ready screen came back. Starting the app afresh gives
    /// them another turn.
    hands_off: BTreeSet<String>,
    /// A patch was finished and the game has not been started since: it is to be started
    /// by itself if the user wants that.
    pub(crate) game_due: bool,
    /// How many failures the site has shown.
    failures_shown: u64,
    /// When the run at work last said anything.
    heard_at: Option<Instant>,
    /// When the game was last started.
    launched_at: Option<Instant>,
    /// When the install folder was last looked at.
    looked_at: Option<Instant>,
    /// A look is due at once, whenever the last one was.
    look_now: bool,
    /// Something was asked of the desk or changed under it since its last round.
    pub(crate) woken: bool,
    /// Whether there was nothing to hurry for at the desk's last round; see [`calm`].
    calm: Option<bool>,
    /// What the page was last told, as it was sent.
    pub(crate) told: Option<String>,
}

impl Flow {
    /// What a site knows as the app starts: nothing, and a check is due.
    pub(crate) fn new(now: Instant) -> Flow {
        Flow {
            status: None,
            answer: None,
            latest: None,
            checked_at: None,
            folder_epoch: 0,
            checking: false,
            next_check: now,
            failures: 0,
            rev: 0,
            phase: Phase::Ready,
            run: None,
            source: None,
            left: None,
            hands_off: BTreeSet::new(),
            game_due: false,
            failures_shown: 0,
            heard_at: None,
            launched_at: None,
            looked_at: None,
            look_now: false,
            woken: false,
            calm: None,
            told: None,
        }
    }

    /// Notes that the desk is not to rest through its usual second.
    pub(crate) fn wake(&mut self) {
        self.woken = true;
    }

    /// Everything the rules of the screen and of what the app does by itself go by.
    pub(crate) fn facts<'a>(&'a self, world: &World) -> Facts<'a> {
        Facts {
            status: self.status.as_ref(),
            answer: self.answer.as_ref(),
            phase: self.phase,
            run: self.run.as_ref(),
            source: self.source.as_deref(),
            hands_off: &self.hands_off,
            game_due: self.game_due,
            auto_patch: world.auto_patch,
            auto_start: world.auto_start,
            outdated: world.outdated,
            run_at_work: world.run_at_work,
            open: world.open,
            quiet: self.quiet(world.now),
            launching: self.launched_at.is_some_and(|at| world.now.saturating_duration_since(at) < LAUNCH_REST),
        }
    }

    /// Whether the run at work has said nothing for a while.
    pub(crate) fn quiet(&self, now: Instant) -> bool {
        self.heard_at.is_some_and(|at| now.saturating_duration_since(at) > QUIET)
    }

    /// The run that was put away for the ready screen, if one was.
    pub(crate) fn left(&self) -> Option<&Left> {
        self.left.as_ref()
    }

    /// Whether the folder is to be looked at and a check to begin; see [`look_due`] and
    /// [`check_due`]. `running` says that this site's run is at work, `run_at_work` that
    /// any is.
    pub(crate) fn look_is_due(&self, now: Instant, running: bool, unseen: bool) -> bool {
        let since = self.looked_at.map(|at| now.saturating_duration_since(at));
        look_due(since, self.look_now, running, unseen, self.status.as_ref().is_some_and(Status::blocked))
    }

    pub(crate) fn check_is_due(&self, now: Instant, run_at_work: bool) -> bool {
        check_due(now, self.next_check, self.checking, run_at_work, self.askable())
    }

    /// Whether there is anything to ask Nexon about for the site: the last look found a
    /// game folder (see [`Status::askable`]). For a client that is not installed nothing is
    /// asked of any server, neither for its patches nor for its maintenance.
    pub(crate) fn askable(&self) -> bool {
        self.status.as_ref().is_some_and(Status::askable)
    }

    /// Takes in what a look at the install folder found. Says whether the folder is another
    /// one than at the look before.
    ///
    /// Another folder may be at another version: what the server was asked about the one
    /// before is put aside, and a check is due. So it is for a folder that has only now
    /// become one to ask about. For a folder there is nothing to ask about, the answer is
    /// that there is nothing, without a check. A run that was on the screen was of the
    /// folder before, and goes with it.
    pub(crate) fn take_look(&mut self, status: Status, now: Instant) -> bool {
        let before = self.status.as_ref();
        let another = before.is_some_and(|before| before.install_dir != status.install_dir);
        let to_ask_about_now = before.is_some_and(|before| !before.askable()) && status.askable();
        if another {
            self.folder_epoch += 1;
            (self.latest, self.checked_at, self.failures) = (None, None, 0);
            (self.source, self.left) = (None, None);
            if !self.phase.at_work() {
                (self.phase, self.run) = (Phase::Ready, None);
            }
        }
        if another || to_ask_about_now {
            self.answer = None;
            self.next_check = now;
        }
        if !status.askable() {
            self.answer = Some(Availability::nothing());
        }
        self.status = Some(status);
        self.looked_at = Some(now);
        self.look_now = false;
        another
    }

    /// Takes in what a check found that began when the folder count was `epoch`, and sets
    /// the time of the next one. Says whether the answer was taken.
    ///
    /// `calm` says whether there is nothing to hurry for (see [`calm`]), `at` is the time in
    /// milliseconds since 1970, and `own` draws this copy's own delay; see [`next_check_in`].
    pub(crate) fn take_answer(
        &mut self,
        epoch: u64,
        checked: Checked,
        calm: Option<bool>,
        now: Instant,
        at: u64,
        own: &dyn Fn(u64) -> u64,
    ) -> bool {
        self.checking = false;
        // The check was about a folder the site has left since. The one for the folder it
        // has now is due already.
        if epoch != self.folder_epoch {
            return false;
        }
        let Checked { answer, end } = checked;
        let wait = match end {
            // It has no answer of its own. What it handed back is what was known before.
            CheckEnd::Stopped => {
                self.next_check = now + RETRY;
                return false;
            }
            CheckEnd::NotYet => RETRY,
            CheckEnd::Answered => {
                self.failures = 0;
                next_check_in(calm, 0, own)
            }
            CheckEnd::Failed => {
                self.failures += 1;
                next_check_in(calm, self.failures, own)
            }
        };
        self.next_check = now + wait;
        if answer.error.is_none() && answer.latest.is_some() {
            self.checked_at = Some(at);
        }
        if answer.latest.is_some() {
            self.latest = answer.latest.clone();
        }
        self.answer = Some(answer);
        // What the folder is at and what can be continued in it depend on what was found.
        self.look_now = true;
        true
    }

    /// Notes whether there is nothing to hurry for now; see [`calm`]. A maintenance that has
    /// begun or ended since the last round makes a check due at once, and so do the hours in
    /// which a client's patches come out: a patch comes out then, and the pace of the
    /// checks is another from here on. Says whether that is so.
    pub(crate) fn see_pace(&mut self, calm: Option<bool>, now: Instant) -> bool {
        let turned = matches!((self.calm, calm), (Some(true) | None, Some(false)) | (Some(false), Some(true)));
        self.calm = calm;
        if turned {
            self.next_check = now;
            self.look_now = true;
        }
        turned
    }

    /// The user asks for a check after one that failed: what was found is put aside, and a
    /// check is due at once.
    pub(crate) fn ask_again(&mut self, now: Instant) {
        if self.status.as_ref().is_some_and(Status::askable) {
            self.answer = None;
        }
        self.next_check = now;
    }

    /// What a run left in the folder was thrown away: the folder is looked at and the
    /// server asked at once. `half_patched` says that the run that was to be finished is
    /// given up with it, so that the patch that was offered for it no longer holds.
    pub(crate) fn leftovers_discarded(&mut self, half_patched: bool, now: Instant) {
        if half_patched && self.status.as_ref().is_some_and(Status::askable) {
            self.answer = None;
        }
        self.left = None;
        self.next_check = now;
        self.look_now = true;
    }

    /// A run of `pick` begins: it takes the screen, and waits for its turn at the folder.
    /// A game that was to be started after the patch before waits for this one instead.
    pub(crate) fn begin(&mut self, pick: &Pick, now: Instant) {
        self.phase = Phase::Starting;
        self.source = pick.file().map(str::to_owned);
        self.left = None;
        self.game_due = false;
        self.heard_at = Some(now);
        self.run = Some(Run {
            source: self.source.clone(),
            label: pick.target().map(|target| target.label.clone()),
            leaving: false,
            percent: pick.percent,
            step: None,
            done: 0,
            total: 0,
            worked: 0,
            activity: pick.activity.clone(),
            failure: None,
            status: "",
            refused: false,
            took: 0,
            files: 0,
        });
    }

    /// The run that was waiting has the folder to itself and is at work. A run that was
    /// asked to stop meanwhile goes on being one that is stopping.
    pub(crate) fn at_work(&mut self) {
        if self.phase == Phase::Starting {
            self.phase = Phase::Running;
        }
    }

    /// Takes in where the run at work says it is, and hands back what the page is told.
    /// The figure on the bar does not go back: steps that are gone through again after a
    /// pause go quickly, and the bar waits for them where it stood.
    pub(crate) fn hear(&mut self, mut progress: Progress, now: Instant) -> Option<Progress> {
        let run = self.run.as_mut().filter(|_| self.phase.at_work())?;
        progress.percent = progress.percent.max(run.percent);
        run.percent = progress.percent;
        (run.step, run.status) = (progress.step, progress.status);
        (run.done, run.total, run.worked) = (progress.done, progress.total, progress.worked);
        run.activity.clone_from(&progress.activity);
        self.heard_at = Some(now);
        Some(progress)
    }

    /// The run at work was asked to stop: by the user (a pause), or, with `leaving`, by the
    /// window being closed, in which case the app goes once the run has stopped. Says
    /// whether there was a run to ask.
    pub(crate) fn stop(&mut self, leaving: bool) -> bool {
        let Some(run) = self.run.as_mut().filter(|_| self.phase.at_work()) else { return false };
        run.leaving |= leaving;
        self.phase = Phase::Stopping;
        true
    }

    /// Takes in how the run on the screen ended.
    ///
    /// A run that is done leaves its summary on the screen, and what the server had for the
    /// folder before no longer holds. A run that did not get there makes its patch the
    /// user's to start for the rest of the session. What it was working on stays on the
    /// screen, to be gone on with from there, with two exceptions that go back to the ready
    /// screen: a run that never got going has nothing to show, and a run of a file that a
    /// partly new folder does not take would only be tried again from here, where the
    /// ready screen offers the patch that is to be finished.
    pub(crate) fn end(&mut self, end: RunEnd, now: Instant) {
        self.heard_at = None;
        self.next_check = now;
        self.look_now = true;
        let Some(mut run) = self.run.take() else {
            self.phase = Phase::Ready;
            return;
        };
        run.leaving = false;
        if let RunEnd::Done { seconds, files } = end {
            (run.percent, run.took, run.files, run.failure) = (100.0, seconds, files, None);
            (self.phase, self.run, self.source) = (Phase::Done, Some(run), None);
            (self.answer, self.game_due) = (None, true);
            return;
        }
        if let Some(patch) = run.source.clone().or_else(|| run.label.clone()) {
            self.hands_off.insert(patch);
        }
        let stopped = end == RunEnd::Stopped;
        run.refused = matches!(end, RunEnd::Refused { .. });
        run.failure = match end {
            // Nothing was tried and nothing failed. What is in the way is the headline
            // itself, as it is when the ready screen says what is in the way.
            RunEnd::Refused { message, details } => {
                let mut lines = message.lines();
                let title = lines.next().unwrap_or_default().to_owned();
                let rest = lines.collect::<Vec<_>>().join("\n");
                Some((title, (!rest.is_empty()).then_some(rest), details))
            }
            RunEnd::Failed { message, details } => Some(("패치에 실패했습니다.".to_owned(), Some(message), details)),
            RunEnd::Done { .. } | RunEnd::Stopped => None,
        }
        .map(|(title, message, details)| {
            self.failures_shown += 1;
            Failure { id: self.failures_shown, title, message, details }
        });
        // A folder that a run left partly new takes the rest of that one patch only, and
        // the look made before this run names the file of it.
        let foreign =
            self.status.as_ref().is_some_and(|status| status.half_patched && run.source != status.resume_file);
        let never = run.step.is_none() && run.activity.is_empty();
        if never || (foreign && !stopped) {
            self.phase = Phase::Ready;
            // A file that was turned down is not offered again there either.
            if foreign || run.refused {
                self.source = None;
            }
            // Only what there is to say of it is kept.
            self.run = run.failure.is_some().then_some(run);
        } else {
            self.phase = if stopped { Phase::Paused } else { Phase::Failed };
            self.run = Some(run);
        }
    }

    /// The user puts the run that ended away and goes back to the ready screen. A run that
    /// was not finished is remembered, for when the same patch is taken up again.
    pub(crate) fn dismiss(&mut self) {
        if self.phase.at_work() {
            return;
        }
        let run = self.run.take();
        self.left = run.filter(|_| self.phase.halted()).map(|run| Left {
            source: run.source,
            label: run.label,
            percent: run.percent,
            activity: run.activity,
        });
        self.phase = Phase::Ready;
    }

    /// A patch is not to be started by itself again in this session: it was, and no run
    /// came of it.
    pub(crate) fn keep_hands_off(&mut self, patch: String) {
        self.hands_off.insert(patch);
    }

    /// The game was started: by the button, or by the app itself.
    pub(crate) fn launched(&mut self, now: Instant) {
        self.game_due = false;
        self.launched_at = Some(now);
    }
}

/// Everything the screen and what the app does by itself follow from, for one site at one
/// moment.
pub(crate) struct Facts<'a> {
    /// What the last look at the install folder found.
    pub(crate) status: Option<&'a Status>,
    /// What the last check found, while it holds.
    pub(crate) answer: Option<&'a Availability>,
    pub(crate) phase: Phase,
    pub(crate) run: Option<&'a Run>,
    /// The patch file the user picked for the run that is on screen or was last left
    /// unfinished.
    pub(crate) source: Option<&'a str>,
    pub(crate) hands_off: &'a BTreeSet<String>,
    pub(crate) game_due: bool,
    pub(crate) auto_patch: bool,
    pub(crate) auto_start: bool,
    pub(crate) outdated: bool,
    pub(crate) run_at_work: bool,
    pub(crate) open: Option<bool>,
    /// The run at work has said nothing for a while.
    pub(crate) quiet: bool,
    /// The game was started a moment ago; the button rests.
    pub(crate) launching: bool,
}

impl<'a> Facts<'a> {
    /// The install folder is there and is a game folder. Only then is there anything to patch.
    pub(crate) fn game_folder(&self) -> bool {
        self.status.is_some_and(Status::game_folder)
    }

    /// The patch for this install. What was found for a folder that is gone does not count.
    pub(crate) fn target(&self) -> Option<&'a Target> {
        self.answer.filter(|_| self.game_folder())?.target.as_ref()
    }

    /// Something is in the way of a patch: one of the checks has not passed.
    pub(crate) fn blocked(&self) -> bool {
        self.status.is_some_and(Status::blocked)
    }

    /// The patch file of a run that was left unfinished and can be continued, when it was
    /// one the user had picked. A folder that a run left partly new takes the rest of that
    /// one patch only, so there the file is the one on record, whatever else was tried in
    /// the meantime.
    pub(crate) fn resume_file(&self) -> Option<&'a str> {
        let status = self.status.filter(|status| status.resumable)?;
        let on_record = status.resume_file.as_deref();
        if status.half_patched { on_record } else { self.source.or(on_record) }
    }

    pub(crate) fn can_start(&self) -> bool {
        self.status.is_some() && (self.target().is_some() || self.resume_file().is_some())
    }

    /// No install folder was found and none was picked: the game is not installed.
    pub(crate) fn not_installed(&self) -> bool {
        self.status.is_some_and(|status| status.install_dir.is_none() && !status.custom_dir)
    }

    /// The game is open, which blocks patching and makes starting it again pointless.
    pub(crate) fn game_running(&self) -> bool {
        self.status
            .is_some_and(|status| status.checks.iter().any(|check| !check.ok && check.kind == CheckKind::Running))
    }

    /// Nothing stands in the way except, at most, the game being open.
    fn clear(&self) -> bool {
        self.status.is_none_or(|status| status.checks.iter().all(|check| check.ok || check.quiet))
    }

    /// The server has not answered for this folder yet. An answer names the newest version
    /// or says why it could not; one that does neither came from a moment when the folder
    /// was being changed, and counts as none.
    pub(crate) fn unanswered(&self) -> bool {
        self.answer.is_none_or(|answer| self.game_folder() && answer.latest.is_none() && answer.error.is_none())
    }

    /// The server was asked and has nothing newer: the button starts the game instead.
    pub(crate) fn up_to_date(&self) -> bool {
        let no_error = self.answer.is_some_and(|answer| answer.error.is_none());
        let nothing_to_apply = self.target().is_none() && self.resume_file().is_none();
        self.status.is_some() && !self.unanswered() && no_error && nothing_to_apply && self.clear()
    }

    /// A run just finished and what comes next is still being asked; the button waits.
    pub(crate) fn settling(&self) -> bool {
        self.phase == Phase::Done && self.answer.is_none()
    }

    /// The check could not be made; the button asks again instead of starting anything.
    pub(crate) fn ask_again(&self) -> bool {
        let failed = self.answer.is_some_and(|answer| answer.error.is_some());
        failed && self.target().is_none() && self.resume_file().is_none()
    }

    /// A run of a file the user picked waits to be continued. The server has no say in that.
    pub(crate) fn own_run(&self) -> bool {
        self.phase == Phase::Ready && self.resume_file().is_some()
    }

    /// The patch that is waiting to be applied, by what it goes by when it is kept from
    /// starting by itself: the file the user picked, or the version the server's leads to.
    pub(crate) fn waiting(&self) -> Option<&'a str> {
        self.resume_file().or_else(|| self.target().map(|target| target.label.as_str()))
    }
}

/// What the app does by itself now, if anything.
///
/// The game is started only when all of this holds: a patch was finished in this session
/// and the user wants the game started after one; this site has no run at work; the game
/// is not closed for maintenance, as far as that is known; and the server has answered
/// since the patch was finished. If that answer names another patch, or says that it could
/// not ask, the game is not started and no longer waits to be.
///
/// A patch is started only when all of this holds: the user wants patches applied without
/// being asked; no run is on the screen but, at most, one that is done; no run is at work
/// for any client; there is a patch for the folder, or a run to continue; nothing is in
/// the way, the game being open included; this version of the app may still patch; the
/// patch has not had its turn in this session (see [`Flow::end`]); and the folder is not
/// one that is patched at the user's press the first time (see [`Status::by_press_only`]).
pub(crate) fn decide(facts: &Facts) -> Decision {
    if facts.game_due && facts.auto_start && !facts.phase.at_work() && facts.open != Some(false) {
        // What was found before the patch was put aside when it finished. So an answer is
        // one that was given since.
        if let Some(answer) = facts.answer {
            let free = answer.target.is_none() && answer.error.is_none();
            return if free { Decision::StartGame } else { Decision::DropGameStart };
        }
    }
    let rests = matches!(facts.phase, Phase::Ready | Phase::Done);
    // A folder of a client this app was never tried on is patched at the user's own press
    // the first time. From then on it is the app's to patch like any other.
    let by_press = facts.status.is_some_and(|status| status.by_press_only);
    let free = rests && !by_press && !facts.run_at_work && !facts.outdated && facts.can_start() && !facts.blocked();
    if facts.auto_patch && free && facts.waiting().is_some_and(|waiting| !facts.hands_off.contains(waiting)) {
        return Decision::StartPatch { file: facts.resume_file().map(str::to_owned) };
    }
    Decision::Nothing
}

/// The patch a run is to be begun of, when the user or the app itself asks for one: `file`
/// when a patch file was picked for it, and otherwise what the screen offers. Or why none
/// is begun.
///
/// With a run that stopped on the screen, the patch is that run's. From the ready screen
/// it is the run that was left unfinished, when there is one, and else the patch the
/// server has. `left` is the run that was last put away: the same patch taken up again
/// stands on the screen where that one stood.
pub(crate) fn pick(facts: &Facts, file: Option<&str>, left: Option<&Left>) -> Result<Pick, Refusal> {
    // What is known of the folder is from before the run that is at work, and holds for
    // nothing until that run has ended.
    if facts.phase.at_work() {
        return Err(Refusal::busy());
    }
    if facts.outdated {
        return Err(Refusal::new(RefusalKind::Outdated, "최신 버전으로 업데이트해 주세요."));
    }
    let Some(status) = facts.status.filter(|status| status.install_dir.is_some()) else {
        return Err(Refusal::new(RefusalKind::Nothing, "설치 경로를 찾을 수 없습니다."));
    };
    if facts.blocked() {
        let messages = status.checks.iter().filter(|check| !check.ok).map(|check| check.message.clone()).collect();
        return Err(Refusal { kind: RefusalKind::Blocked, messages });
    }
    let halted = facts.run.filter(|_| facts.phase.halted());
    let file = match (file, halted) {
        (Some(file), _) => Some(file),
        (None, Some(run)) => run.source.as_deref(),
        (None, None) => facts.resume_file(),
    };
    let patch = match (file, facts.target()) {
        (Some(file), _) => Patch::File(file.to_owned()),
        (None, Some(target)) => Patch::Server(target.clone()),
        (None, None) => return Err(Refusal::new(RefusalKind::Nothing, "적용할 패치가 없습니다.")),
    };
    let label = match &patch {
        Patch::File(_) => None,
        Patch::Server(target) => Some(target.label.as_str()),
    };
    let same = |source: &Option<String>, of: &Option<String>| source.as_deref() == file && of.as_deref() == label;
    // What is on the screen stays, for the run that is gone on with from there. Otherwise
    // the bar begins where a run of this patch had come to, also when the app was closed
    // in between and only its record still knows.
    let (percent, activity) = match (halted.filter(|run| same(&run.source, &run.label)), left) {
        (Some(run), _) => (run.percent, run.activity.clone()),
        (None, Some(left)) if same(&left.source, &left.label) => (left.percent, left.activity.clone()),
        _ => {
            let on_record = status.resumable && file == status.resume_file.as_deref();
            (on_record.then_some(status.resume_percent).flatten().unwrap_or(0.0), String::new())
        }
    };
    Ok(Pick { patch, percent, activity })
}

/// The hours of the week in which a client's patches come out with no maintenance to
/// announce them: the weekday, counted from Monday as 0, and the minutes of the day they
/// begin and end at, in Korean time.
#[derive(Debug)]
pub(crate) struct PatchHours {
    weekday: i64,
    begin: Minutes,
    end: Minutes,
}

/// When the test world's patches go up: on Thursdays between 15:00 and 17:30, about a
/// quarter of an hour before the test world opens, and usually with no notice on its board.
pub(crate) const TEST_WORLD_PATCH_HOURS: PatchHours = PatchHours { weekday: 3, begin: 15 * 60, end: 17 * 60 + 30 };

impl PatchHours {
    /// Whether `now`, a moment in Korean time, is within these hours: from their first
    /// minute on, and no longer at the minute they end at.
    pub(crate) fn hold(&self, now: Minutes) -> bool {
        // The first day of 1970 was a Thursday.
        let weekday = (now.div_euclid(1440) + 3).rem_euclid(7);
        weekday == self.weekday && (self.begin..self.end).contains(&now.rem_euclid(1440))
    }
}

/// Whether there is nothing to hurry for with the checks of a client: `Some(false)` while
/// its patches may come out at any moment, `Some(true)` at any other time, and `None` while
/// Nexon's word on its maintenance could not be read.
///
/// Patches come out during a maintenance (`underway`, as far as that could be read), and,
/// for a client that has them, in its patch hours.
pub(crate) fn calm(underway: Option<bool>, patch_hours: bool) -> Option<bool> {
    if patch_hours { Some(false) } else { underway.map(|underway| !underway) }
}

/// How long after a check the next one is due.
///
/// `calm` says whether there is nothing to hurry for (see [`calm`]). `failures` is how many
/// checks in a row, this one included, could not ask the server. `own` draws a number of
/// seconds up to the one it is given, which differs from one copy of the app to the next,
/// so that they do not all ask in the same second.
///
/// A patch comes out during a maintenance, so that is when the server is asked often: every
/// half minute. At any other time there is nothing to hurry for. A server that cannot be
/// asked is not helped by being asked more: each wait is then twice the one before.
pub(crate) fn next_check_in(calm: Option<bool>, failures: u32, own: &dyn Fn(u64) -> u64) -> Duration {
    const UNDERWAY: u64 = 30;
    const SELDOM: u64 = 10 * 60;
    const UNREADABLE: u64 = 2 * 60;
    /// The longest wait while checks fail during a maintenance.
    const SLOWEST_UNDERWAY: u64 = 5 * 60;
    let underway = calm == Some(false);
    let seconds = match (failures, calm) {
        (0, Some(false)) => UNDERWAY + own(10),
        (0, Some(true)) => SELDOM + own(60),
        (0, None) => UNREADABLE,
        (failed, _) => (UNDERWAY << (failed - 1).min(5)).min(if underway { SLOWEST_UNDERWAY } else { SELDOM }),
    };
    Duration::from_secs(seconds)
}

/// Whether the install folder is to be looked at now.
///
/// `since` is how long ago it was last looked at, `asked` that a look was asked for.
/// Nothing is looked at while the site's run is at work (`running`): the run has the folder
/// to itself, and what is on the screen is the run. A window nobody can see (`unseen`)
/// needs its figures seldom, unless something is in the way of a patch (`blocked`): that
/// may go away at any moment, and whatever waits for it is to notice soon.
fn look_due(since: Option<Duration>, asked: bool, running: bool, unseen: bool, blocked: bool) -> bool {
    if running {
        return false;
    }
    match since {
        None => true,
        Some(_) if asked => true,
        Some(since) => since >= if unseen && !blocked { LOOK_EVERY_UNSEEN } else { LOOK_EVERY },
    }
}

/// Whether a check is to begin now: its time has come, none is at work, no run is, and the
/// folder is one there is something to ask about.
fn check_due(now: Instant, next_check: Instant, checking: bool, running: bool, askable: bool) -> bool {
    !checking && !running && askable && now >= next_check
}

/// What becomes of a run when the window is asked to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnClose {
    /// The window is put away, and whatever is at work goes on.
    Hide,
    /// The run at work is asked to stop first; the app goes when it has.
    StopThenLeave,
    /// The window closes, and the app with it.
    Leave,
}

/// What closing the window does: `background` says that the user wants the app to stay at
/// work next to the clock, `run_at_work` that a run is at work.
///
/// Closing during a run would cut the run off. It is asked to stop first, and an apply
/// that has begun is finished.
pub(crate) fn on_close(background: bool, run_at_work: bool) -> OnClose {
    match (background, run_at_work) {
        (true, _) => OnClose::Hide,
        (false, true) => OnClose::StopThenLeave,
        (false, false) => OnClose::Leave,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::made::{
        errored, game_at, game_open, half_patched, no_game_at, no_install, patch_to, resumable, up_to_date,
    };

    fn ended(answer: Availability, end: CheckEnd) -> Checked {
        Checked { answer, end }
    }

    /// Takes an answer in with the game open and no delay of this copy's own.
    fn take(flow: &mut Flow, epoch: u64, checked: Checked, now: Instant) -> bool {
        flow.take_answer(epoch, checked, Some(true), now, 1_764_000_000_000, &|_| 0)
    }

    fn seconds(of: u64) -> Duration {
        Duration::from_secs(of)
    }

    /// The world as it is on a quiet day: nothing set, nothing at work, the game open.
    fn world(now: Instant) -> World {
        World { auto_patch: false, auto_start: false, outdated: false, run_at_work: false, open: Some(true), now }
    }

    /// A site that has looked at its game folder and was told of the patch to 1.2.422.
    fn with_a_patch(now: Instant) -> Flow {
        let mut flow = Flow::new(now);
        flow.take_look(game_at(r"C:\Games\Maple"), now);
        take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Answered), now);
        flow
    }

    /// Begins the run a start asks for, the way the app does.
    fn begin(flow: &mut Flow, file: Option<&str>, now: Instant) -> Result<(), Refusal> {
        let pick = pick(&flow.facts(&world(now)), file, flow.left())?;
        flow.begin(&pick, now);
        flow.at_work();
        Ok(())
    }

    fn says(step: Step, percent: f64, activity: &str) -> Progress {
        Progress {
            percent,
            step: Some(step),
            status: "새 게임 파일 생성 중",
            done: 5,
            total: 10,
            worked: 5,
            activity: activity.to_owned(),
        }
    }

    fn failed(message: &str) -> RunEnd {
        RunEnd::Failed { message: message.to_owned(), details: Vec::new() }
    }

    fn refused(message: &str) -> RunEnd {
        RunEnd::Refused { message: message.to_owned(), details: Vec::new() }
    }

    // The pace of the looks and the checks.

    #[test]
    fn the_pace_of_the_checks_is_the_owners_table() {
        let (none, most) = (|_: u64| 0, |most: u64| most);
        let table: [(Option<bool>, u32, u64, u64); 12] = [
            // A maintenance is underway: every half minute, and up to ten seconds of its own.
            (Some(false), 0, 30, 40),
            // No maintenance: every ten minutes, and up to a minute of its own.
            (Some(true), 0, 600, 660),
            // Nexon's word on it could not be read.
            (None, 0, 120, 120),
            // Checks that fail: each wait twice the one before.
            (Some(false), 1, 30, 30),
            (Some(false), 2, 60, 60),
            (Some(false), 4, 240, 240),
            (Some(false), 5, 300, 300),
            (Some(false), 30, 300, 300),
            (Some(true), 1, 30, 30),
            (Some(true), 5, 480, 480),
            (Some(true), 6, 600, 600),
            (None, 60, 600, 600),
        ];
        for (open, failures, soonest, latest) in table {
            let what = format!("open {open:?}, {failures} failed");
            assert_eq!(next_check_in(open, failures, &none), seconds(soonest), "{what}");
            assert_eq!(next_check_in(open, failures, &most), seconds(latest), "{what}");
        }
    }

    /// The owner decided on half a minute during a maintenance and seldom otherwise.
    #[test]
    fn the_server_is_never_asked_more_often_than_every_half_minute() {
        for open in [Some(true), Some(false), None] {
            for failures in 0..40 {
                assert!(next_check_in(open, failures, &|_| 0) >= seconds(30), "open {open:?}, {failures} failed");
            }
        }
    }

    #[test]
    fn the_folder_is_looked_at_every_three_seconds_and_seldom_while_nobody_sees_it() {
        let (seen, unseen, clear, blocked) = (false, true, false, true);
        // Never looked at, or asked to: at once.
        assert!(look_due(None, false, false, unseen, clear));
        assert!(look_due(Some(seconds(0)), true, false, unseen, clear));
        // By the clock.
        assert!(!look_due(Some(seconds(2)), false, false, seen, clear));
        assert!(look_due(Some(seconds(3)), false, false, seen, clear));
        // A window that is put away or made small.
        assert!(!look_due(Some(seconds(29)), false, false, unseen, clear));
        assert!(look_due(Some(seconds(30)), false, false, unseen, clear));
        // Something is in the way that whoever waits for it is to see go.
        assert!(look_due(Some(seconds(3)), false, false, unseen, blocked));
        // Not while a run is at work, whoever asks.
        for asked in [false, true] {
            assert!(!look_due(Some(seconds(60)), asked, true, seen, clear));
            assert!(!look_due(None, asked, true, seen, clear));
        }
    }

    #[test]
    fn a_check_begins_when_its_time_has_come_and_nothing_else_is_at_work() {
        let due = Instant::now();
        assert!(check_due(due, due, false, false, true));
        assert!(check_due(due + seconds(1), due, false, false, true));
        assert!(!check_due(due - seconds(1), due, false, false, true));
        // One check at a time, none during a run, and none for a folder with nothing to ask about.
        assert!(!check_due(due, due, true, false, true));
        assert!(!check_due(due, due, false, true, true));
        assert!(!check_due(due, due, false, false, false));
    }

    #[test]
    fn a_check_is_due_as_the_app_starts_and_begins_once_the_folder_is_known() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        assert!(flow.look_is_due(start, false, false));
        assert!(!flow.check_is_due(start, false));
        assert!(!flow.take_look(game_at(r"C:\Games\Maple"), start));
        assert!(flow.check_is_due(start, false) && !flow.check_is_due(start, true));
        assert_eq!((flow.folder_epoch, flow.answer.is_none()), (0, true));
    }

    /// The epoch rule: a check is about the folder it began under.
    #[test]
    fn an_answer_about_a_folder_the_site_has_left_is_dropped() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        let begun_under = flow.folder_epoch;
        flow.checking = true;
        // The user picks another folder while the check is at work.
        assert!(flow.take_look(game_at(r"D:\Copy\Maple"), start + seconds(1)));
        assert_eq!(flow.folder_epoch, begun_under + 1);

        let late = start + seconds(4);
        assert!(!take(&mut flow, begun_under, ended(patch_to("1.2.422"), CheckEnd::Answered), late));
        assert!(flow.answer.is_none() && flow.latest.is_none() && flow.checked_at.is_none());
        // The check for the folder it has now is still due, and can begin: the old one is over.
        assert!(flow.check_is_due(late, false));

        let now_under = flow.folder_epoch;
        assert!(take(&mut flow, now_under, ended(up_to_date("1.2.421"), CheckEnd::Answered), late));
        assert_eq!(flow.latest.as_deref(), Some("1.2.421"));
    }

    #[test]
    fn another_folder_puts_aside_what_was_found_and_the_same_folder_keeps_it() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Failed), start);
        assert_eq!(flow.failures, 1);

        // The same folder, looked at again: nothing changes but the look.
        assert!(!flow.take_look(game_at(r"C:\Games\Maple"), start + seconds(3)));
        assert!(flow.answer.is_some() && flow.latest.is_some() && flow.next_check > start + seconds(3));

        // Another one, or none where there was one.
        for other in [game_at(r"D:\Copy\Maple"), no_install()] {
            let mut flow = Flow::new(start);
            flow.take_look(game_at(r"C:\Games\Maple"), start);
            take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Failed), start);
            let now = start + seconds(3);
            assert!(flow.take_look(other, now));
            assert_eq!((flow.folder_epoch, flow.failures, flow.next_check), (1, 0, now));
            assert!(flow.latest.is_none() && flow.checked_at.is_none());
            assert!(flow.answer.as_ref().is_none_or(|answer| answer.target.is_none() && answer.latest.is_none()));
        }
    }

    /// The screen tells "nothing to ask" from "not asked yet" by there being an answer.
    #[test]
    fn a_folder_with_nothing_to_ask_about_has_its_answer_without_a_check() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(no_game_at(r"D:\Empty"), start);
        let answer = flow.answer.as_ref().expect("the answer that there is nothing");
        assert!(answer.target.is_none() && answer.latest.is_none() && answer.error.is_none());
        assert!(!flow.check_is_due(start + seconds(60), false));

        // The game is installed into it: the same folder, and now one to ask about.
        let now = start + seconds(90);
        assert!(!flow.take_look(game_at(r"D:\Empty"), now));
        assert!(flow.answer.is_none());
        assert_eq!((flow.folder_epoch, flow.next_check), (0, now));
    }

    #[test]
    fn an_answer_sets_the_time_of_the_next_check_by_how_the_check_ended() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(game_at(r"C:\Games\Maple"), start);

        // The server could not be asked: what stood before stands with the reason, and
        // the waits grow.
        for (failed, wait) in [(1, 30), (2, 60), (3, 120)] {
            flow.checking = true;
            assert!(take(&mut flow, 0, ended(errored("서버 요청 실패"), CheckEnd::Failed), start));
            assert_eq!((flow.failures, flow.next_check, flow.checking), (failed, start + seconds(wait), false));
        }
        assert_eq!(flow.checked_at, None);
        assert!(flow.answer.as_ref().is_some_and(|answer| answer.error.is_some()));

        // The first answer brings the pace back, and says when the server was last heard.
        assert!(take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Answered), start));
        assert_eq!((flow.failures, flow.next_check), (0, start + seconds(600)));
        assert_eq!((flow.checked_at, flow.latest.as_deref()), (Some(1_764_000_000_000), Some("1.2.422")));

        // A failure after that keeps the newest version that was seen, and the time it was.
        assert!(take(&mut flow, 0, ended(errored("서버 요청 실패"), CheckEnd::Failed), start));
        assert_eq!((flow.checked_at, flow.latest.as_deref()), (Some(1_764_000_000_000), Some("1.2.422")));

        // During a maintenance the next check comes in half a minute.
        flow.take_answer(0, ended(up_to_date("1.2.422"), CheckEnd::Answered), Some(false), start, 0, &|_| 0);
        assert_eq!(flow.next_check, start + seconds(30));
    }

    #[test]
    fn a_check_that_gave_up_is_made_again_shortly_and_leaves_what_was_found() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);

        let now = start + seconds(700);
        flow.checking = true;
        assert!(!take(&mut flow, 0, ended(Availability::nothing(), CheckEnd::Stopped), now));
        assert_eq!((flow.next_check, flow.failures, flow.checking), (now + seconds(5), 0, false));
        assert!(flow.answer.as_ref().is_some_and(|answer| answer.target.is_some()));

        // A folder that is still being put back says so, and is asked about again as soon.
        assert!(take(&mut flow, 0, ended(errored("이전 패치 복구 대기"), CheckEnd::NotYet), now));
        assert_eq!(flow.next_check, now + seconds(5));
        assert!(flow.answer.as_ref().is_some_and(|answer| answer.error.is_some()));
    }

    #[test]
    fn a_maintenance_that_begins_or_ends_makes_a_check_due_at_once() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        // The first word of the lamp is no turn of it when the game is open.
        assert!(!flow.see_pace(Some(true), start));
        take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
        assert_eq!(flow.next_check, start + seconds(600));

        let begins = start + seconds(200);
        assert!(flow.see_pace(Some(false), begins));
        assert_eq!(flow.next_check, begins);
        // While nothing changes nothing is due.
        take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), begins);
        assert!(!flow.see_pace(Some(false), begins + seconds(1)));
        assert!(flow.next_check > begins + seconds(1));

        let ends = begins + seconds(3600);
        assert!(flow.see_pace(Some(true), ends));
        assert_eq!(flow.next_check, ends);
        // Word that cannot be read for a while is neither.
        assert!(!flow.see_pace(None, ends + seconds(1)));
        assert!(!flow.see_pace(Some(true), ends + seconds(2)));
        // A maintenance that is found underway when word comes back is one that began.
        flow.see_pace(None, ends + seconds(3));
        assert!(flow.see_pace(Some(false), ends + seconds(4)));
    }

    #[test]
    fn asking_again_puts_the_answer_aside_and_makes_a_check_due() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        take(&mut flow, 0, ended(errored("서버 요청 실패"), CheckEnd::Failed), start);
        let now = start + seconds(2);
        flow.ask_again(now);
        assert!(flow.answer.is_none());
        assert_eq!(flow.next_check, now);
        // Where there is nothing to ask about, the answer that says so stays.
        let mut flow = Flow::new(start);
        flow.take_look(no_game_at(r"D:\Empty"), start);
        flow.ask_again(now);
        assert!(flow.answer.is_some());
    }

    // The run and its phases.

    #[test]
    fn a_run_goes_from_ready_through_starting_to_running_and_is_done() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        assert_eq!(flow.phase, Phase::Ready);
        let pick = pick(&flow.facts(&world(start)), None, None).unwrap();
        assert_eq!((pick.file(), pick.target().map(|target| target.label.as_str())), (None, Some("1.2.422")));

        flow.begin(&pick, start);
        assert_eq!(flow.phase, Phase::Starting);
        let run = flow.run.as_ref().unwrap();
        assert_eq!(
            (run.source.as_deref(), run.label.as_deref(), run.step, run.percent),
            (None, Some("1.2.422"), None, 0.0)
        );
        flow.at_work();
        assert_eq!(flow.phase, Phase::Running);

        flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start + seconds(30));
        flow.end(RunEnd::Done { seconds: 95, files: 718 }, start + seconds(95));
        assert_eq!(flow.phase, Phase::Done);
        let run = flow.run.as_ref().unwrap();
        assert_eq!((run.percent, run.took, run.files), (100.0, 95, 718));
        // What the server had for the folder before no longer holds; it is asked at once,
        // and the game is to be started if the user wants that.
        assert!(flow.answer.is_none() && flow.game_due);
        assert_eq!(flow.next_check, start + seconds(95));
        assert!(flow.look_is_due(start + seconds(95), false, false));
        // Nothing is hands-off after a patch that went through.
        assert!(flow.hands_off.is_empty());
    }

    #[test]
    fn a_second_start_finds_the_first_at_work() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        let at_work = World { run_at_work: true, ..world(start) };
        assert_eq!(decide(&flow.facts(&World { auto_patch: true, ..at_work })), Decision::Nothing);
        // Whoever asks, and for whichever patch: the folder is the run's until it has ended.
        let asked = |flow: &Flow, file| pick(&flow.facts(&world(start)), file, None).map_err(|refusal| refusal.kind);
        assert_eq!(asked(&flow, None), Err(RefusalKind::Busy));
        assert_eq!(asked(&flow, Some(r"D:\patches\other.patch")), Err(RefusalKind::Busy));
        flow.stop(false);
        assert_eq!(asked(&flow, None), Err(RefusalKind::Busy));
        flow.end(RunEnd::Stopped, start);
        assert!(asked(&flow, None).is_ok());
    }

    #[test]
    fn a_pause_stops_the_run_and_leaves_what_it_was_working_on_on_the_screen() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start + seconds(30));

        assert!(flow.stop(false));
        assert_eq!(flow.phase, Phase::Stopping);
        assert!(!flow.run.as_ref().unwrap().leaving);
        flow.end(RunEnd::Stopped, start + seconds(31));
        assert_eq!(flow.phase, Phase::Paused);
        let run = flow.run.as_ref().unwrap();
        assert_eq!((run.percent, run.activity.as_str(), run.failure.as_ref()), (61.5, "생성중: Map001.wz", None));
        // There is nothing to stop any more.
        assert!(!flow.stop(false));
        assert_eq!(flow.phase, Phase::Paused);
    }

    /// Closing the window during a run, when the app is not set to stay next to the clock.
    #[test]
    fn closing_during_a_run_stops_it_first_and_the_app_goes_when_it_has_stopped() {
        assert_eq!(on_close(false, true), OnClose::StopThenLeave);
        assert_eq!(on_close(false, false), OnClose::Leave);
        // With the app set to stay at work, closing only puts the window away.
        assert_eq!((on_close(true, true), on_close(true, false)), (OnClose::Hide, OnClose::Hide));

        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        assert!(flow.stop(true));
        assert_eq!(flow.phase, Phase::Stopping);
        assert!(flow.run.as_ref().unwrap().leaving);
        // A pause that was asked for before stays one that leaves once the window is closed.
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.stop(false);
        flow.stop(true);
        assert!(flow.run.as_ref().unwrap().leaving);
    }

    #[test]
    fn a_run_that_was_asked_to_stop_before_it_was_at_work_stays_one_that_is_stopping() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        let pick = pick(&flow.facts(&world(start)), None, None).unwrap();
        flow.begin(&pick, start);
        flow.stop(false);
        flow.at_work();
        assert_eq!(flow.phase, Phase::Stopping);
    }

    #[test]
    fn a_run_that_never_got_going_returns_to_the_ready_screen() {
        let start = Instant::now();
        // Stopped before it had said anything: there is nothing to show of it.
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.end(RunEnd::Stopped, start);
        assert_eq!((flow.phase, flow.run.as_ref()), (Phase::Ready, None));

        // Failed or turned down before its first step: back to the ready screen, with what
        // there is to say of it.
        for (end, title) in [
            (failed("파일 처리 중 오류가 발생했습니다."), "패치에 실패했습니다."),
            (refused("이미 패치가 진행 중입니다."), "이미 패치가 진행 중입니다."),
        ] {
            let mut flow = with_a_patch(start);
            begin(&mut flow, None, start).unwrap();
            flow.end(end, start);
            assert_eq!(flow.phase, Phase::Ready);
            assert_eq!(
                flow.run.as_ref().and_then(|run| run.failure.as_ref()).map(|failure| failure.title.as_str()),
                Some(title)
            );
        }

        // A run that had said so much as its step stays on the screen.
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::VerifyPatch, 30.0, ""), start);
        flow.end(failed("패치 파일 검사에 실패했습니다."), start);
        assert_eq!(flow.phase, Phase::Failed);
    }

    #[test]
    fn a_failure_is_said_once_each_with_what_went_wrong_under_its_headline() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::Rebuild, 50.0, "생성중: Map001.wz"), start);
        let details = vec!["Map001.wz".to_owned()];
        flow.end(
            RunEnd::Failed {
                message: "새 게임 파일 검사에 실패했습니다.".to_owned(), details: details.clone()
            },
            start,
        );
        let first = flow.run.as_ref().unwrap().failure.clone().unwrap();
        let said = Some("새 게임 파일 검사에 실패했습니다.".to_owned());
        assert_eq!(first, Failure { id: 1, title: "패치에 실패했습니다.".to_owned(), message: said, details });

        // Tried again and turned down: what is in the way is the headline, the figures under it.
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::VerifyInstall, 50.0, "검사중: Map001.wz"), start);
        let no_room = "디스크 공간이 부족합니다.\nC: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)";
        flow.end(refused(no_room), start);
        assert_eq!(flow.phase, Phase::Failed);
        let run = flow.run.as_ref().unwrap();
        let second = run.failure.as_ref().unwrap();
        assert!(run.refused);
        assert_eq!((second.id, second.title.as_str()), (2, "디스크 공간이 부족합니다."));
        assert_eq!(second.message.as_deref(), Some("C: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)"));
    }

    #[test]
    fn the_bar_never_goes_back_within_a_patch() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        let told = flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start).unwrap();
        assert_eq!(told.percent, 61.5);
        flow.end(RunEnd::Stopped, start);

        // Taken up again from where it stands on the screen: the steps before are gone
        // through once more, and the figure waits for them.
        begin(&mut flow, None, start).unwrap();
        let run = flow.run.as_ref().unwrap();
        assert_eq!((run.percent, run.activity.as_str(), run.step), (61.5, "생성중: Map001.wz", None));
        let told = flow.hear(says(Step::VerifyInstall, 33.0, "검사중: Base.wz"), start).unwrap();
        assert_eq!((told.percent, told.step), (61.5, Some(Step::VerifyInstall)));
        assert_eq!(flow.run.as_ref().unwrap().activity, "검사중: Base.wz");
        let told = flow.hear(says(Step::Rebuild, 70.25, "생성중: Skill.wz"), start).unwrap();
        assert_eq!(told.percent, 70.25);
    }

    #[test]
    fn a_run_that_was_put_away_is_taken_up_where_it_stood() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start);
        flow.end(RunEnd::Stopped, start);
        flow.dismiss();
        assert_eq!((flow.phase, flow.run.as_ref()), (Phase::Ready, None));

        begin(&mut flow, None, start).unwrap();
        let run = flow.run.as_ref().unwrap();
        assert_eq!((run.percent, run.activity.as_str()), (61.5, "생성중: Map001.wz"));
        // It is remembered for that patch and no other, and once.
        flow.end(RunEnd::Stopped, start);
        flow.dismiss();
        begin(&mut flow, Some(r"D:\patches\00421to00422.patch"), start).unwrap();
        assert_eq!(flow.run.as_ref().unwrap().percent, 0.0);
        assert!(flow.left().is_none());
    }

    #[test]
    fn the_bar_is_restored_after_a_restart_from_what_the_run_left_on_record() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        // The app was closed with the patch from the server at 69 percent.
        flow.take_look(resumable(game_at(r"C:\Games\Maple"), None, Some(69.25)), start);
        take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Answered), start);
        begin(&mut flow, None, start).unwrap();
        assert_eq!(flow.run.as_ref().unwrap().percent, 69.25);

        // A file the user had picked, likewise, and only that file.
        let file = r"D:\patches\00421to00422.patch";
        let mut flow = Flow::new(start);
        flow.take_look(resumable(game_at(r"C:\Games\Maple"), Some(file), Some(12.5)), start);
        let picked = pick(&flow.facts(&world(start)), None, None).unwrap();
        assert_eq!((picked.file(), picked.percent), (Some(file), 12.5));
        let other = pick(&flow.facts(&world(start)), Some(r"D:\patches\other.patch"), None).unwrap();
        assert_eq!(other.percent, 0.0);
    }

    #[test]
    fn a_done_run_is_put_away_without_a_trace() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.end(RunEnd::Done { seconds: 95, files: 718 }, start);
        flow.dismiss();
        assert_eq!((flow.phase, flow.run.as_ref(), flow.left()), (Phase::Ready, None, None));
        // The game is still to be started after it.
        assert!(flow.game_due);
        // A run at work is not put away.
        begin(&mut flow, Some(r"D:\patches\a.patch"), start).unwrap();
        flow.dismiss();
        assert_eq!(flow.phase, Phase::Running);
    }

    #[test]
    fn a_half_patched_folder_resumes_only_the_file_on_record() {
        let start = Instant::now();
        let on_record = r"D:\patches\00420to00421.patch";
        let mut flow = Flow::new(start);
        flow.take_look(half_patched(game_at(r"C:\Games\Maple"), Some(on_record), Some(41.0)), start);
        // The user tried another file in the meantime, and it is the one still in mind.
        flow.source = Some(r"D:\patches\other.patch".to_owned());
        let facts = flow.facts(&world(start));
        assert_eq!(facts.resume_file(), Some(on_record));
        assert_eq!(pick(&facts, None, None).unwrap().file(), Some(on_record));

        // In a whole folder with work left in it, the file that was last picked is gone on with.
        let mut flow = Flow::new(start);
        flow.take_look(resumable(game_at(r"C:\Games\Maple"), Some(on_record), Some(41.0)), start);
        flow.source = Some(r"D:\patches\other.patch".to_owned());
        assert_eq!(flow.facts(&world(start)).resume_file(), Some(r"D:\patches\other.patch"));
    }

    #[test]
    fn a_wrong_file_that_is_refused_returns_to_the_ready_screen_of_the_half_patched_folder() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(half_patched(game_at(r"C:\Games\Maple"), None, Some(41.0)), start);
        take(&mut flow, 0, ended(patch_to("1.2.421"), CheckEnd::Answered), start);
        let wrong = r"D:\patches\00421to00422.patch";
        begin(&mut flow, Some(wrong), start).unwrap();
        // It got as far as the check of the patch file before the engine turned it down.
        flow.hear(says(Step::VerifyPatch, 4.0, "검사중: 00421to00422.patch"), start);
        flow.end(refused("진행 중인 패치와 다른 패치 파일입니다."), start);

        assert_eq!(flow.phase, Phase::Ready);
        assert_eq!(flow.source, None);
        let failure = flow.run.as_ref().and_then(|run| run.failure.as_ref()).unwrap();
        assert_eq!(
            (failure.title.as_str(), failure.message.as_deref()),
            ("진행 중인 패치와 다른 패치 파일입니다.", None)
        );
        // The patch to finish is still the one that is offered, and may still start by itself.
        let facts = flow.facts(&World { auto_patch: true, ..world(start) });
        assert_eq!(facts.resume_file(), None);
        let offered = pick(&facts, None, None).unwrap();
        assert_eq!(offered.target().map(|target| target.label.as_str()), Some("1.2.421"));
        assert!(flow.hands_off.contains(wrong) && !flow.hands_off.contains("1.2.421"));

        // The right patch, paused there, stays on the screen like any other.
        let mut flow = Flow::new(start);
        flow.take_look(half_patched(game_at(r"C:\Games\Maple"), None, Some(41.0)), start);
        take(&mut flow, 0, ended(patch_to("1.2.421"), CheckEnd::Answered), start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::Rebuild, 60.0, "생성중: Map001.wz"), start);
        flow.end(failed("파일 처리 중 오류가 발생했습니다."), start);
        assert_eq!(flow.phase, Phase::Failed);
    }

    #[test]
    fn a_folder_that_is_changed_takes_the_run_on_its_screen_with_it() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, Some(r"D:\patches\a.patch"), start).unwrap();
        flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start);
        flow.end(RunEnd::Stopped, start);
        assert!(flow.take_look(game_at(r"D:\Copy\Maple"), start));
        assert_eq!(
            (flow.phase, flow.run.as_ref(), flow.source.as_deref(), flow.left()),
            (Phase::Ready, None, None, None)
        );
    }

    // What a start asks for.

    #[test]
    fn a_start_is_turned_down_with_what_is_in_the_way() {
        let start = Instant::now();
        let flow = with_a_patch(start);
        // Too old a version of the app patches nothing, whatever else holds.
        let outdated = pick(&flow.facts(&World { outdated: true, ..world(start) }), None, None).unwrap_err();
        assert_eq!(
            (outdated.kind, outdated.messages),
            (RefusalKind::Outdated, vec!["최신 버전으로 업데이트해 주세요.".to_owned()])
        );

        let mut none = Flow::new(start);
        assert_eq!(pick(&none.facts(&world(start)), None, None).unwrap_err().kind, RefusalKind::Nothing);
        none.take_look(no_install(), start);
        let no_folder = pick(&none.facts(&world(start)), None, None).unwrap_err();
        assert_eq!(
            (no_folder.kind, no_folder.messages),
            (RefusalKind::Nothing, vec!["설치 경로를 찾을 수 없습니다.".to_owned()])
        );

        // Every check that has not passed says so, the quiet ones included.
        let mut playing = with_a_patch(start);
        playing.take_look(game_open(game_at(r"C:\Games\Maple")), start);
        let blocked = pick(&playing.facts(&world(start)), None, None).unwrap_err();
        assert_eq!(blocked.kind, RefusalKind::Blocked);
        assert_eq!(blocked.messages, ["게임 관련 프로그램이 실행 중입니다.\n해당 프로그램을 종료해 주세요."]);

        // A folder the server has nothing for, and nothing left to go on with.
        let mut up = Flow::new(start);
        up.take_look(game_at(r"C:\Games\Maple"), start);
        take(&mut up, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
        let nothing = pick(&up.facts(&world(start)), None, None).unwrap_err();
        assert_eq!(
            (nothing.kind, nothing.messages),
            (RefusalKind::Nothing, vec!["적용할 패치가 없습니다.".to_owned()])
        );
        // A file that was picked needs no patch from the server.
        assert!(pick(&up.facts(&world(start)), Some(r"D:\patches\a.patch"), None).is_ok());
    }

    #[test]
    fn a_run_that_stopped_is_gone_on_with_by_its_own_patch() {
        let start = Instant::now();
        let file = r"D:\patches\00421to00422.patch";
        let mut flow = with_a_patch(start);
        begin(&mut flow, Some(file), start).unwrap();
        flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start);
        flow.end(failed("파일 처리 중 오류가 발생했습니다."), start);
        // The button under a run that stopped asks for no file: the run's own is meant, not
        // the patch the server has.
        let again = pick(&flow.facts(&world(start)), None, flow.left()).unwrap();
        assert_eq!((again.file(), again.target(), again.percent), (Some(file), None, 61.5));
    }

    // What the app does by itself.

    /// A site with a patch waiting, and the user wanting patches applied without being asked.
    fn wanting(now: Instant) -> (Flow, World) {
        (with_a_patch(now), World { auto_patch: true, ..world(now) })
    }

    #[test]
    fn a_patch_starts_by_itself_when_the_user_wants_that_and_nothing_is_in_the_way() {
        let start = Instant::now();
        let (flow, world) = wanting(start);
        assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch { file: None });
        // Not without the setting.
        assert_eq!(decide(&flow.facts(&World { auto_patch: false, ..world })), Decision::Nothing);
    }

    #[test]
    fn no_patch_starts_by_itself_while_a_run_is_on_the_screen_or_at_work_anywhere() {
        let start = Instant::now();
        let (mut flow, world) = wanting(start);
        // A run for another client.
        assert_eq!(decide(&flow.facts(&World { run_at_work: true, ..world })), Decision::Nothing);
        // This site's own, at every moment of it.
        begin(&mut flow, None, start).unwrap();
        for phase in [Phase::Starting, Phase::Running, Phase::Stopping, Phase::Paused, Phase::Failed] {
            flow.phase = phase;
            assert_eq!(decide(&flow.facts(&world)), Decision::Nothing, "{phase:?}");
        }
    }

    #[test]
    fn no_patch_starts_by_itself_without_a_patch_or_with_something_in_the_way() {
        let start = Instant::now();
        let world = World { auto_patch: true, ..world(start) };
        // The server has not answered, or has nothing.
        let mut flow = Flow::new(start);
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
        take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);

        // The game is open: a check that only shows when it fails, and counts all the same.
        let mut flow = with_a_patch(start);
        flow.take_look(game_open(game_at(r"C:\Games\Maple")), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
        // It is closed again: the patch starts.
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch { file: None });
        // A version of the app that is too old patches nothing by itself either.
        assert_eq!(decide(&flow.facts(&World { outdated: true, ..world })), Decision::Nothing);
    }

    #[test]
    fn a_patch_that_was_paused_or_failed_is_the_users_to_start_for_the_rest_of_the_session() {
        let start = Instant::now();
        for end in [RunEnd::Stopped, failed("파일 처리 중 오류가 발생했습니다."), refused("디스크 공간이 부족합니다.")]
        {
            let (mut flow, world) = wanting(start);
            begin(&mut flow, None, start).unwrap();
            flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start);
            flow.end(end.clone(), start);
            flow.dismiss();
            assert_eq!(flow.phase, Phase::Ready);
            assert_eq!(decide(&flow.facts(&world)), Decision::Nothing, "{end:?}");
            // The user starts it, and it goes through: the next patch has its own turn.
            begin(&mut flow, None, start).unwrap();
            flow.end(RunEnd::Done { seconds: 60, files: 10 }, start);
            take(&mut flow, 0, ended(patch_to("1.2.423"), CheckEnd::Answered), start);
            assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch { file: None }, "{end:?}");
        }
        // A new session knows nothing of it.
        let (fresh, world) = wanting(start);
        assert_eq!(decide(&fresh.facts(&world)), Decision::StartPatch { file: None });
    }

    #[test]
    fn a_patch_stays_hands_off_when_a_run_of_another_file_ends() {
        let start = Instant::now();
        let (mut flow, world) = wanting(start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::Rebuild, 61.5, "생성중: Map001.wz"), start);
        flow.end(RunEnd::Stopped, start);
        flow.dismiss();
        begin(&mut flow, Some(r"D:\patches\a.patch"), start).unwrap();
        flow.hear(says(Step::VerifyPatch, 2.0, "검사중: a.patch"), start);
        flow.end(failed("패치 파일 검사에 실패했습니다."), start);
        flow.dismiss();
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
        assert!(flow.hands_off.contains("1.2.422") && flow.hands_off.contains(r"D:\patches\a.patch"));
    }

    #[test]
    fn a_run_that_was_left_unfinished_when_the_app_was_closed_is_taken_up_by_itself() {
        let start = Instant::now();
        let file = r"D:\patches\00421to00422.patch";
        let mut flow = Flow::new(start);
        flow.take_look(resumable(game_at(r"C:\Games\Maple"), Some(file), Some(12.5)), start);
        let world = World { auto_patch: true, ..world(start) };
        // The server has no say in that: its answer is not waited for.
        assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch { file: Some(file.to_owned()) });
        flow.keep_hands_off(file.to_owned());
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    }

    #[test]
    fn the_next_patch_starts_by_itself_from_the_screen_of_the_one_that_is_done() {
        let start = Instant::now();
        let (mut flow, world) = wanting(start);
        begin(&mut flow, None, start).unwrap();
        flow.end(RunEnd::Done { seconds: 60, files: 10 }, start);
        assert_eq!(flow.phase, Phase::Done);
        // Not before the server has said what there is for the folder as it is now.
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
        take(&mut flow, 0, ended(patch_to("1.2.423"), CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch { file: None });
    }

    /// What the page shows is none of the app's business: a failure that is still being
    /// read does not keep the next patch waiting.
    #[test]
    fn a_patch_does_not_wait_for_what_the_screen_is_saying_of_another() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(half_patched(game_at(r"C:\Games\Maple"), None, Some(41.0)), start);
        take(&mut flow, 0, ended(patch_to("1.2.421"), CheckEnd::Answered), start);
        begin(&mut flow, Some(r"D:\patches\wrong.patch"), start).unwrap();
        flow.hear(says(Step::VerifyPatch, 4.0, "검사중: wrong.patch"), start);
        flow.end(refused("진행 중인 패치와 다른 패치 파일입니다."), start);
        assert!(flow.run.as_ref().is_some_and(|run| run.failure.is_some()));
        assert_eq!(
            decide(&flow.facts(&World { auto_patch: true, ..world(start) })),
            Decision::StartPatch { file: None }
        );
    }

    /// A site whose patch has just gone through, with the user wanting the game started.
    fn patched(now: Instant) -> (Flow, World) {
        let mut flow = with_a_patch(now);
        begin(&mut flow, None, now).unwrap();
        flow.end(RunEnd::Done { seconds: 60, files: 10 }, now);
        (flow, World { auto_start: true, ..world(now) })
    }

    #[test]
    fn the_game_is_started_after_a_patch_once_the_server_says_there_is_no_other() {
        let start = Instant::now();
        let (mut flow, world) = patched(start);
        // What the server had before the patch says nothing; its answer is waited for.
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
        take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::StartGame);
        // Once.
        flow.launched(start);
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    }

    #[test]
    fn the_game_is_not_started_without_a_patch_or_without_the_setting() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&World { auto_start: true, ..world(start) })), Decision::Nothing);

        let (mut flow, world) = patched(start);
        take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&World { auto_start: false, ..world })), Decision::Nothing);
        // It still waits to be, should the user turn the setting on.
        assert!(flow.game_due);
    }

    #[test]
    fn the_game_waits_for_a_maintenance_to_end_and_not_for_a_lamp_that_cannot_be_read() {
        let start = Instant::now();
        let (mut flow, world) = patched(start);
        take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&World { open: Some(false), ..world })), Decision::Nothing);
        assert!(flow.game_due);
        assert_eq!(decide(&flow.facts(&World { open: Some(true), ..world })), Decision::StartGame);
        // Not knowing is no reason to wait: then the launcher is the one to ask.
        assert_eq!(decide(&flow.facts(&World { open: None, ..world })), Decision::StartGame);
    }

    #[test]
    fn the_game_is_not_started_when_another_patch_waits_or_that_cannot_be_ruled_out() {
        let start = Instant::now();
        for answer in [patch_to("1.2.423"), errored("서버 요청 실패")] {
            let (mut flow, world) = patched(start);
            let end = if answer.error.is_some() { CheckEnd::Failed } else { CheckEnd::Answered };
            take(&mut flow, 0, ended(answer, end), start);
            assert_eq!(decide(&flow.facts(&world)), Decision::DropGameStart);
            // With that given up, the patch is what is decided on: it starts if the user
            // wants patches applied, and the game is due again when it is done.
            flow.game_due = false;
            let then = decide(&flow.facts(&World { auto_patch: true, ..world }));
            let patch_waits = flow.answer.as_ref().is_some_and(|answer| answer.target.is_some());
            assert_eq!(then == Decision::StartPatch { file: None }, patch_waits);
        }
    }

    #[test]
    fn the_game_is_not_started_while_this_site_has_a_run_at_work() {
        let start = Instant::now();
        let (mut flow, world) = patched(start);
        take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
        begin(&mut flow, Some(r"D:\patches\a.patch"), start).unwrap();
        // A run that begins takes the game's turn: it is due again when the patch is done.
        assert!(!flow.game_due);
        // Nor would it be started under a run, were it still due.
        flow.game_due = true;
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    }

    #[test]
    fn a_download_that_brings_nothing_is_quiet_after_four_seconds_and_the_button_rests_five() {
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::Download, 3.0, "다운로드중: 00421to00422.patch"), start);
        assert!(!flow.facts(&world(start + seconds(4))).quiet);
        assert!(flow.facts(&world(start + seconds(5))).quiet);
        flow.hear(says(Step::Download, 3.5, "다운로드중: 00421to00422.patch"), start + seconds(6));
        assert!(!flow.facts(&world(start + seconds(7))).quiet);

        flow.launched(start);
        assert!(flow.facts(&world(start + seconds(4))).launching);
        assert!(!flow.facts(&world(start + seconds(5))).launching);
    }

    /// A client that is not installed costs nothing: its patch server is not asked, and
    /// the watch on its maintenance, which goes by the same word, reads nothing.
    #[test]
    fn nothing_is_asked_of_any_server_for_a_client_that_is_not_installed() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        // Before the first look nothing is known, and nothing is asked.
        assert!(!flow.askable() && !flow.check_is_due(start, false));
        for not_there in [no_install(), no_game_at(r"D:\Empty")] {
            flow.take_look(not_there, start);
            assert!(!flow.askable());
            for later in [0, 60, 3600, 86_400] {
                assert!(!flow.check_is_due(start + seconds(later), false), "after {later} s");
            }
        }
        // The folder is looked at all the same, so that a game that gets installed is found.
        assert!(flow.look_is_due(start + seconds(3), false, false));

        // Once the game is there, both are asked.
        flow.take_look(game_at(r"C:\Games\MapleT"), start + seconds(3));
        assert!(flow.askable() && flow.check_is_due(start + seconds(3), false));
    }

    // The test world.

    /// A moment in Korean time in the week of Thursday 2026-11-26, which is 20783 days after
    /// the first of 1970: `days` after that Thursday, at `hour`:`minute`.
    fn of_the_week(days: i64, hour: i64, minute: i64) -> Minutes {
        (20_783 + days) * 1440 + hour * 60 + minute
    }

    #[test]
    fn the_test_worlds_patches_are_looked_for_often_on_thursday_afternoons() {
        let hours = &TEST_WORLD_PATCH_HOURS;
        // From 15:00 on, and no longer at 17:30.
        assert!(!hours.hold(of_the_week(0, 14, 59)));
        assert!(hours.hold(of_the_week(0, 15, 0)));
        assert!(hours.hold(of_the_week(0, 17, 29)));
        assert!(!hours.hold(of_the_week(0, 17, 30)));
        // On Thursdays, and on no other day of the week.
        for days in [-3, -2, -1, 1, 2, 3] {
            assert!(!hours.hold(of_the_week(days, 16, 0)), "{days} days from Thursday");
        }
        assert!(hours.hold(of_the_week(7, 16, 0)) && hours.hold(of_the_week(-7, 16, 0)));

        // In those hours the server is asked as during a maintenance; outside them as at
        // any other time.
        let none = |_: u64| 0;
        assert_eq!(next_check_in(calm(Some(false), true), 0, &none), seconds(30));
        assert_eq!(next_check_in(calm(Some(false), false), 0, &none), seconds(600));
    }

    #[test]
    fn there_is_something_to_hurry_for_during_a_maintenance_and_in_a_clients_patch_hours() {
        assert_eq!(calm(Some(false), false), Some(true));
        assert_eq!(calm(Some(true), false), Some(false));
        // Whatever is known of a maintenance, the patch hours are the patch hours.
        for underway in [Some(false), Some(true), None] {
            assert_eq!(calm(underway, true), Some(false), "{underway:?}");
        }
        // Outside them, what could not be read is not known.
        assert_eq!(calm(None, false), None);

        // The hours beginning and ending make a check due at once, as a maintenance does.
        let start = Instant::now();
        let mut flow = with_a_patch(start);
        assert!(!flow.see_pace(calm(Some(false), false), start));
        assert!(flow.see_pace(calm(Some(false), true), start + seconds(1)));
        assert_eq!(flow.next_check, start + seconds(1));
        assert!(flow.see_pace(calm(Some(false), false), start + seconds(2)));
    }

    /// A client this app was never tried on is patched at the user's own press the first
    /// time, whatever is set.
    #[test]
    fn a_folder_that_is_the_users_to_patch_first_is_not_patched_by_itself() {
        let start = Instant::now();
        let mut flow = Flow::new(start);
        flow.take_look(Status { by_press_only: true, ..game_at(r"C:\Games\MapleT") }, start);
        take(&mut flow, 0, ended(patch_to("1.2.208"), CheckEnd::Answered), start);
        let wanting = World { auto_patch: true, ..world(start) };
        assert_eq!(decide(&flow.facts(&wanting)), Decision::Nothing);
        // The press is there to be made.
        assert!(pick(&flow.facts(&wanting), None, None).is_ok());

        // Once this app has patched the folder, the look says so, and the next patch is
        // the app's to start.
        flow.take_look(game_at(r"C:\Games\MapleT"), start);
        assert_eq!(decide(&flow.facts(&wanting)), Decision::StartPatch { file: None });
    }
}
