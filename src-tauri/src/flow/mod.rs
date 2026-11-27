//! The flow of a site: what is known of its install folder and of the patches for it, the
//! run that is on its screen, and the rules by which the app goes from one to the next by
//! itself.
//!
//! Everything here is plain data and rules over it. Nothing asks the window, the clock or
//! the drive: whoever calls says what time it is and what was found. The desk is what
//! calls (see [`crate::desk`]), and [`crate::shown`] is what the screen is made of.
//!
//! What closing the window does to a run is in [`close`].

mod close;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use mfp_kms::notice::Minutes;
use serde::Serialize;

pub(crate) use self::close::{AtClose, CloseAnswer, OnClose, on_close};
use crate::offer::{CheckEnd, Checked};
use crate::state::{Availability, CheckKind, Status, Step, Target};
use crate::wording::NO_FOLDER;

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
///
/// The page is told of it what the page draws by. The rest is for the rules here and for
/// what the screen says (see [`crate::shown`]), and is not sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Run {
    /// The address the user gave for its patch; `None` for the patch a check found.
    #[serde(skip)]
    pub(crate) source: Option<String>,
    /// The version a patch from the server leads to.
    #[serde(skip)]
    pub(crate) label: Option<String>,
    /// The stop was asked for by closing the window; the app goes once the run has stopped.
    /// The footer's word says so.
    #[serde(skip)]
    pub(crate) leaving: bool,
    /// The window was closed on the run, and the user is being asked whether it is to go on
    /// with the app next to the clock. Until they answer, or the run is asked to stop.
    pub(crate) asking: bool,
    /// When they were asked; see [`on_close`].
    #[serde(skip)]
    pub(crate) asked_at: Option<Instant>,
    /// How far the bar has come, 0 to 100. It never goes back within a patch. The page has
    /// it as the figure of the screen, and with each word of where the run is.
    #[serde(skip)]
    pub(crate) percent: f64,
    /// The step the run is at; `None` before it has said.
    pub(crate) step: Option<Step>,
    /// Bytes done and total within that step, and how many of them this run worked for.
    pub(crate) done: u64,
    pub(crate) total: u64,
    pub(crate) worked: u64,
    /// What the run is working on, as in `생성 중: Map001.wz`.
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
    /// What the step is called while it is at work. The page has it from the footer's word,
    /// which is told whenever it changes, and is not sent it with every figure.
    #[serde(skip)]
    pub(crate) status: &'static str,
    /// Bytes done and total within the current step.
    pub(crate) done: u64,
    pub(crate) total: u64,
    /// Bytes of `done` this run worked for itself. The rest was found done by an earlier run,
    /// and says nothing about how fast the step is going.
    pub(crate) worked: u64,
    /// What is being worked on, the way the game's own patcher shows it: `생성 중: Map001.wz`.
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
    /// There is no install folder, or no patch for it.
    Nothing,
    /// The address the user gave is not one a patch is fetched from, or no patch is there.
    Address,
    /// The address the user gave is on a server of Nexon's and not among the client's own
    /// patch files, and the user has not said yes to it. The one message is the question.
    Ask,
}

impl Refusal {
    pub(crate) fn new(kind: RefusalKind, message: &str) -> Refusal {
        Refusal { kind, messages: vec![message.to_owned()] }
    }

    pub(crate) fn busy() -> Refusal {
        Refusal::new(RefusalKind::Busy, "이미 패치가 진행 중입니다.")
    }

    /// There is no install folder to begin a run in.
    pub(crate) fn no_folder() -> Refusal {
        Refusal::new(RefusalKind::Nothing, NO_FOLDER)
    }

    /// There is no patch to begin a run of.
    pub(crate) fn no_patch() -> Refusal {
        Refusal::new(RefusalKind::Nothing, "적용할 패치가 없습니다.")
    }
}

/// The patch a run is of: the one at an address the user gave, or the one a check found on
/// the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Patch {
    Given(String),
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
    /// The address the user gave, when the run is of the patch at one.
    pub(crate) fn address(&self) -> Option<&str> {
        match &self.patch {
            Patch::Given(address) => Some(address),
            Patch::Server(_) => None,
        }
    }

    /// The patch a check found, when the run is of that.
    pub(crate) fn target(&self) -> Option<&Target> {
        match &self.patch {
            Patch::Given(_) => None,
            Patch::Server(target) => Some(target),
        }
    }
}

/// What the app does by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Nothing,
    /// Start the patch from the server that is waiting.
    StartPatch,
    StartGame,
    /// The game was to be started after a patch, and is not: another patch is waiting, or
    /// that could not be ruled out.
    DropGameStart,
    /// The game was to be started after a patch, and is left to the user: whether it is
    /// open cannot be seen for this client, and it is not started on a guess.
    LeaveGameToUser,
}

/// What the rules go by that is not the site's own: what the user has set, what the app as
/// a whole is doing, and what time it is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct World {
    pub(crate) auto_patch: bool,
    pub(crate) auto_start: bool,
    /// This version of the app is older than the oldest its home still stands by. The app
    /// says so and begins no patch by itself; a patch the user starts, it starts.
    pub(crate) outdated: bool,
    /// A run is at work, for this client or for another.
    pub(crate) run_at_work: bool,
    /// Whether the game can be played, as the lamp says; `None` while that is not known.
    pub(crate) open: Option<bool>,
    /// Whether it can be seen at all that the game is open: Nexon's service lists this
    /// client's maintenance. Of a client it does not list, only what closes the game is
    /// ever known, and `open` never says that it can be played.
    pub(crate) sees_open: bool,
    /// The game's files are being checked in this site's folder, at the user's word (see
    /// [`crate::diagnosis`]): no patch is begun in it until that has ended.
    pub(crate) checking_files: bool,
    /// The app's home was asked whether this version may still patch, once at least since
    /// the app was started, whether or not it answered. Until then `outdated` says nothing.
    pub(crate) update_asked: bool,
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
    /// The address the user gave for the run that is on screen or was last left
    /// unfinished. A run that is taken up again from the ready screen is of the patch there.
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
            latest: self.latest.as_deref(),
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
            sees_open: world.sees_open,
            checking_files: world.checking_files,
            update_asked: world.update_asked,
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
    /// game folder, or one that is still to be put back (see [`Status::askable`]). For a
    /// client that is not installed nothing is asked of any server, neither for its patches
    /// nor for its maintenance.
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
        if self.askable() {
            self.answer = None;
        }
        self.next_check = now;
    }

    /// Something the look goes by has changed that is not in the folder: the way of
    /// patching the user chose, by which the room a run needs is judged. The folder is
    /// looked at again at once, so that its lines say so.
    pub(crate) fn look_soon(&mut self) {
        self.look_now = true;
    }

    /// What a run left in the folder was thrown away: the folder is looked at and the
    /// server asked at once. `half_patched` says that the run that was to be finished is
    /// given up with it, so that the patch that was offered for it no longer holds.
    pub(crate) fn leftovers_discarded(&mut self, half_patched: bool, now: Instant) {
        if half_patched && self.askable() {
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
        self.source = pick.address().map(str::to_owned);
        self.left = None;
        self.game_due = false;
        self.heard_at = Some(now);
        self.run = Some(Run {
            source: self.source.clone(),
            label: pick.target().map(|target| target.label.clone()),
            leaving: false,
            asking: false,
            asked_at: None,
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
        // Whether it is to go on is no longer a question.
        (run.asking, run.asked_at) = (false, None);
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
        (run.leaving, run.asking, run.asked_at) = (false, false, None);
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
            self.status.as_ref().is_some_and(|status| status.half_patched && run.source != status.resume_address);
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

    /// What the site remembers of this session; see [`Remembered`].
    pub(crate) fn remembered(&self) -> Remembered {
        Remembered { game_due: self.game_due, hands_off: self.hands_off.iter().cloned().collect() }
    }

    /// Takes up what a copy of the app remembered that this one has taken over from: the
    /// session goes on as if it had been one app all along.
    pub(crate) fn remember(&mut self, remembered: Remembered) {
        self.game_due |= remembered.game_due;
        self.hands_off.extend(remembered.hands_off);
    }
}

/// What a site remembers of this session that what the app does by itself goes by (see
/// [`decide`]), and that nothing on this PC says: starting the app afresh forgets it.
///
/// A copy of the app that takes over from another in the middle of a session is told it by
/// the one it replaces (see [`crate::rights`]). So the game is still started when it opens
/// after a patch, and a patch the user stopped does not begin again by itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Remembered {
    /// A patch was finished and the game has not been started since.
    pub(crate) game_due: bool,
    /// The patches that are the user's to start for the rest of the session.
    pub(crate) hands_off: Vec<String>,
}

/// Everything the screen and what the app does by itself follow from, for one site at one
/// moment.
pub(crate) struct Facts<'a> {
    /// What the last look at the install folder found.
    pub(crate) status: Option<&'a Status>,
    /// What the last check found, while it holds.
    pub(crate) answer: Option<&'a Availability>,
    /// The newest version the server was last seen to have for the folder, which is what
    /// the page shows as the newest.
    pub(crate) latest: Option<&'a str>,
    pub(crate) phase: Phase,
    pub(crate) run: Option<&'a Run>,
    /// The address the user gave for the run that is on screen or was last left
    /// unfinished.
    pub(crate) source: Option<&'a str>,
    pub(crate) hands_off: &'a BTreeSet<String>,
    pub(crate) game_due: bool,
    pub(crate) auto_patch: bool,
    pub(crate) auto_start: bool,
    pub(crate) outdated: bool,
    pub(crate) run_at_work: bool,
    pub(crate) open: Option<bool>,
    pub(crate) sees_open: bool,
    /// The game's files are being checked in the folder: no patch is begun in it meanwhile.
    pub(crate) checking_files: bool,
    pub(crate) update_asked: bool,
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

    /// Something is in the way of a patch: one of the checks has not passed, or the folder
    /// is still being put back.
    pub(crate) fn blocked(&self) -> bool {
        self.status.is_some_and(Status::blocked)
    }

    /// An apply that was cut off in the folder is being undone, or is about to be.
    pub(crate) fn putting_back(&self) -> bool {
        self.status.is_some_and(|status| status.putting_back)
    }

    /// The address of the patch of a run that was left unfinished and can be continued,
    /// when it was one the user had given. A folder that a run left partly new takes the
    /// rest of that one patch only, so there the address is the one on record, whatever
    /// else was tried in the meantime.
    pub(crate) fn resume_address(&self) -> Option<&'a str> {
        let status = self.status.filter(|status| status.resumable)?;
        let on_record = status.resume_address.as_deref();
        if status.half_patched { on_record } else { self.source.or(on_record) }
    }

    /// Whether a run of the patch at `address` is there to be gone on with: the run that
    /// stopped on the screen is of it, or the one that was left unfinished.
    pub(crate) fn continues(&self, address: &str) -> bool {
        let halted = self.run.filter(|_| self.phase.halted()).and_then(|run| run.source.as_deref());
        halted == Some(address) || self.resume_address() == Some(address)
    }

    pub(crate) fn can_start(&self) -> bool {
        self.status.is_some() && (self.target().is_some() || self.resume_address().is_some())
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
    ///
    /// Nor does an answer count for more than the page has to show. The page names the
    /// newest version on one line and says on another whether the install is at it, and
    /// the two are one statement: while there is no newest version to name, nothing is
    /// said of the install being at it either, and the server counts as still to answer.
    pub(crate) fn unanswered(&self) -> bool {
        self.answer.is_none_or(|answer| {
            let named = answer.latest.is_some() && self.latest.is_some();
            self.game_folder() && !named && answer.error.is_none()
        })
    }

    /// The server was asked and has nothing newer: the button starts the game instead.
    pub(crate) fn up_to_date(&self) -> bool {
        let no_error = self.answer.is_some_and(|answer| answer.error.is_none());
        let nothing_to_apply = self.target().is_none() && self.resume_address().is_none();
        self.status.is_some() && !self.unanswered() && no_error && nothing_to_apply && self.clear()
    }

    /// A run just finished and what comes next is still being asked; the button waits.
    pub(crate) fn settling(&self) -> bool {
        self.phase == Phase::Done && self.answer.is_none()
    }

    /// The check could not be made; the button asks again instead of starting anything.
    pub(crate) fn ask_again(&self) -> bool {
        let failed = self.answer.is_some_and(|answer| answer.error.is_some());
        failed && self.target().is_none() && self.resume_address().is_none()
    }

    /// A run of a patch the user gave the address of waits to be continued. What a check
    /// finds has no say in that.
    pub(crate) fn own_run(&self) -> bool {
        self.phase == Phase::Ready && self.resume_address().is_some()
    }

    /// The patch that is waiting to be applied, by what it goes by when it is kept from
    /// starting by itself: the address the user gave, or the version the one a check found
    /// leads to.
    pub(crate) fn waiting(&self) -> Option<&'a str> {
        self.resume_address().or_else(|| self.target().map(|target| target.label.as_str()))
    }
}

/// What the app does by itself now, if anything.
///
/// Nothing at all until the app's home was asked once whether this version may still patch
/// (see [`World::update_asked`]). A version that is too old begins no patch by itself, and
/// before it has asked it does not know whether it is one. Whether the home answered makes
/// no difference: the app does its work without it.
///
/// The game is started only when all of this holds: a patch was finished in this session
/// and the user wants the game started after one; this site has no run at work; the game
/// is known to be open; and the server has answered since the patch was finished. If that
/// answer names another patch, or says that it could not ask, the game is not started and
/// no longer waits to be.
///
/// Known to be open is what Nexon was read to say, and nothing less: a game that is closed
/// for maintenance is waited for, and so is one of which nothing is known for the moment
/// (nothing was read yet, or Nexon cannot be read). It is started the moment it is seen to
/// be open. Nobody is there to see the game started into a closed server, and the button is
/// there all the while for whoever knows better. Where it can never be seen that the game
/// is open (see [`World::sees_open`]) there is nothing to wait for: the start is left to the
/// user at once.
///
/// A patch is started only when all of this holds: the user wants patches applied without
/// being asked; no run is on the screen but, at most, one that is done; no run is at work
/// for any client; there is a patch from the server for the folder; nothing is in the way,
/// the game being open included; this version of the app may still patch; the patch has not
/// had its turn in this session (see [`Flow::end`]); the folder is not one that is patched
/// at the user's press the first time (see [`Status::by_press_only`]); and no run of a
/// patch the user gave the address of waits to be continued.
///
/// A run by an address the user gave is the user's to continue, in the session it was
/// begun in and in any later one. The app takes up by itself what it found itself, and
/// nothing that only a record says was once asked for.
pub(crate) fn decide(facts: &Facts) -> Decision {
    if !facts.update_asked {
        return Decision::Nothing;
    }
    if facts.game_due && facts.auto_start && !facts.phase.at_work() {
        if !facts.sees_open {
            return Decision::LeaveGameToUser;
        }
        // What was found before the patch was put aside when it finished. So an answer is
        // one that was given since.
        if let (Some(true), Some(answer)) = (facts.open, facts.answer) {
            let free = answer.target.is_none() && answer.error.is_none();
            return if free { Decision::StartGame } else { Decision::DropGameStart };
        }
    }
    let rests = matches!(facts.phase, Phase::Ready | Phase::Done);
    // A folder of a client this app was never tried on is patched at the user's own press
    // the first time. From then on it is the app's to patch like any other.
    let by_press = facts.status.is_some_and(|status| status.by_press_only);
    let own = facts.resume_address().is_some();
    let held = by_press || own || facts.run_at_work || facts.outdated || facts.blocked();
    let free = rests && !held && facts.can_start();
    if facts.auto_patch && free && facts.waiting().is_some_and(|waiting| !facts.hands_off.contains(waiting)) {
        return Decision::StartPatch;
    }
    Decision::Nothing
}

/// The patch a run is to be begun of, when the user or the app itself asks for one: the one
/// at `address` when the user gave an address, and otherwise what the screen offers. Or why
/// none is begun.
///
/// With a run that stopped on the screen, the patch is that run's. From the ready screen
/// it is the run that was left unfinished, when there is one, and else the patch a check
/// found. `left` is the run that was last put away: the same patch taken up again stands
/// on the screen where that one stood.
///
/// The address of the patch a check found is that patch, whoever names it: a run of it is
/// the app's own, to offer again and to take up again like any it found.
pub(crate) fn pick(facts: &Facts, address: Option<&str>, left: Option<&Left>) -> Result<Pick, Refusal> {
    // What is known of the folder is from before the run that is at work, and holds for
    // nothing until that run has ended.
    if facts.phase.at_work() {
        return Err(Refusal::busy());
    }
    let Some(status) = facts.status.filter(|status| status.install_dir.is_some()) else {
        return Err(Refusal::no_folder());
    };
    if facts.blocked() {
        let messages = status.checks.iter().filter(|check| !check.ok).map(|check| check.message.clone()).collect();
        return Err(Refusal { kind: RefusalKind::Blocked, messages });
    }
    let halted = facts.run.filter(|_| facts.phase.halted());
    let address = match (address, halted) {
        (Some(address), _) => Some(address),
        (None, Some(run)) => run.source.as_deref(),
        (None, None) => facts.resume_address(),
    };
    let address = address.filter(|address| facts.target().is_none_or(|found| found.url != *address));
    let patch = match (address, facts.target()) {
        (Some(address), _) => Patch::Given(address.to_owned()),
        (None, Some(target)) => Patch::Server(target.clone()),
        (None, None) => return Err(Refusal::no_patch()),
    };
    let label = match &patch {
        Patch::Given(_) => None,
        Patch::Server(target) => Some(target.label.as_str()),
    };
    let same = |source: &Option<String>, of: &Option<String>| source.as_deref() == address && of.as_deref() == label;
    // What is on the screen stays, for the run that is gone on with from there. Otherwise
    // the bar begins where a run of this patch had come to, also when the app was closed
    // in between and only its record still knows.
    let (percent, activity) = match (halted.filter(|run| same(&run.source, &run.label)), left) {
        (Some(run), _) => (run.percent, run.activity.clone()),
        (None, Some(left)) if same(&left.source, &left.label) => (left.percent, left.activity.clone()),
        _ => {
            let on_record = status.resumable && address == status.resume_address.as_deref();
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
fn next_check_in(calm: Option<bool>, failures: u32, own: &dyn Fn(u64) -> u64) -> Duration {
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
