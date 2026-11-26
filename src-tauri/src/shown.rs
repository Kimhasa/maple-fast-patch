//! What the screen of a site says: the lines of the ready screen, the big figure, the word
//! in the footer, the line under a finished run, and the one button at the bottom right.
//!
//! All of it follows from the [`Facts`] of the site, and the page draws it as it comes. The
//! words are the ones the screen has always used.

use serde::Serialize;

use crate::flow::{Facts, Phase};
use crate::state::{CheckKind, Step};

/// What the footer says while the app is too old to patch with. A start is turned down with
/// the same words.
const UPDATE_FIRST: &str = "최신 버전으로 업데이트해 주세요.";

/// Which of the lines of the ready screen a row is. The page goes by this for what a row
/// offers beside its words: picking a folder, deleting what a run left, picking a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RowKind {
    Folder,
    Running,
    Writable,
    Space,
    Recovery,
    /// What a paused or interrupted run left in the install folder.
    Leftover,
    /// The patch file the server has for the install.
    PatchFile,
}

impl From<CheckKind> for RowKind {
    fn from(kind: CheckKind) -> RowKind {
        match kind {
            CheckKind::Folder => RowKind::Folder,
            CheckKind::Running => RowKind::Running,
            CheckKind::Writable => RowKind::Writable,
            CheckKind::Space => RowKind::Space,
            CheckKind::Recovery => RowKind::Recovery,
        }
    }
}

/// One line of the ready screen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Row {
    pub(crate) kind: RowKind,
    pub(crate) label: &'static str,
    /// A check that passed or did not; `None` for a line that is no check.
    pub(crate) ok: Option<bool>,
    /// What was found, in a few words for the line.
    pub(crate) detail: String,
    /// For a check that did not pass: what is wrong, as the sentence a dialog shows.
    pub(crate) message: String,
}

/// What the one button at the bottom right does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ButtonKind {
    /// Opens the page the game is downloaded from: 게임 다운로드.
    Install,
    /// Starts the patch: 다운로드.
    Download,
    /// Goes on with a run that was left unfinished: 패치 이어하기.
    Resume,
    /// Tries a run again that failed or was turned down: 다시 시도.
    Retry,
    /// Asks the server again after a check that failed: 다시 시도.
    CheckAgain,
    /// Starts the game: 게임 실행.
    Launch,
    /// The game is open: 게임 실행 중.
    GameRunning,
    /// Pauses the run at work: 일시정지.
    Pause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Button {
    pub(crate) kind: ButtonKind,
    pub(crate) enabled: bool,
    /// Drawn as the thing to do next.
    pub(crate) primary: bool,
}

/// What the screen of a site says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    /// The installed version. Right after a run it is the version that run patched to.
    pub(crate) current: Option<String>,
    /// What the ready screen lists, in order.
    pub(crate) rows: Vec<Row>,
    /// What the big figure and the bar show, 0 to 100. Nothing where there is no game
    /// folder to have a figure of.
    pub(crate) percent: Option<f64>,
    /// The footer's word, whether it is bad news, and whether something is being waited for.
    pub(crate) status: String,
    pub(crate) status_bad: bool,
    pub(crate) busy: bool,
    /// The line under a finished run.
    pub(crate) summary: Option<String>,
    pub(crate) button: Button,
}

/// What the screen says, given everything that is known of the site.
pub(crate) fn shown(facts: &Facts) -> Shown {
    let ready = facts.phase == Phase::Ready;
    // The status says so, to explain the buttons it holds back. A run at work goes on
    // saying its step.
    let held_back = facts.outdated && !facts.phase.at_work();
    // The status is bad news: something is in the way, the check failed, or the run did.
    let troubled = match facts.own_run() {
        true => facts.blocked(),
        false => {
            let failed = facts.answer.is_some_and(|answer| answer.error.is_some());
            facts.answer.is_some() && !facts.up_to_date() && (facts.blocked() || failed)
        }
    };
    Shown {
        current: current(facts),
        rows: rows(facts),
        percent: percent(facts),
        status: if held_back { UPDATE_FIRST.to_owned() } else { status(facts) },
        status_bad: held_back || (ready && troubled) || facts.phase == Phase::Failed,
        // Something is being waited for: the first answer from the server, or a run to stop.
        busy: (ready && facts.unanswered() && !facts.not_installed() && !facts.own_run() && !held_back)
            || facts.phase == Phase::Stopping,
        summary: summary(facts),
        button: button(facts),
    }
}

fn current(facts: &Facts) -> Option<String> {
    let patched_to = facts.run.filter(|_| facts.phase == Phase::Done).and_then(|run| run.label.clone());
    patched_to.or_else(|| facts.status.and_then(|status| status.version.clone()))
}

/// The lines of the ready screen: the checks, a quiet one only when it has failed, then
/// what a run left, then the patch file.
fn rows(facts: &Facts) -> Vec<Row> {
    let Some(status) = facts.status else { return Vec::new() };
    let mut rows: Vec<Row> = status
        .checks
        .iter()
        .filter(|check| !(check.quiet && check.ok))
        .map(|check| Row {
            kind: check.kind.into(),
            label: check.label,
            ok: Some(check.ok),
            detail: check.detail.clone(),
            message: check.message.clone(),
        })
        .collect();
    if let Some(leftover) = &status.leftover {
        let (label, detail) = ("이전 패치 작업", leftover.clone());
        rows.push(Row { kind: RowKind::Leftover, label, ok: None, detail, message: String::new() });
    }
    if facts.game_folder() {
        let target = facts.target();
        rows.push(Row {
            kind: RowKind::PatchFile,
            label: "패치 파일",
            ok: target.map(|_| true),
            detail: target.map(|target| megabytes(target.size)).unwrap_or_default(),
            message: String::new(),
        });
    }
    rows
}

/// A size in whole megabytes, as the screen writes it: `1,229 MB`.
fn megabytes(bytes: u64) -> String {
    let whole = ((bytes as f64 / (1u64 << 20) as f64).round() as u64).to_string();
    let mut grouped = String::new();
    for (at, digit) in whole.chars().enumerate() {
        if at > 0 && (whole.len() - at).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped} MB")
}

/// On the ready screen the bar shows a finished state, or how far an unfinished run came.
/// With a run on the screen it shows the run. A folder that was looked at and is no game
/// folder has no figure: nothing is installed there to be at any point of anything.
fn percent(facts: &Facts) -> Option<f64> {
    match (facts.phase, facts.run) {
        (Phase::Ready, _) | (_, None) => {
            if facts.status.is_some() && !facts.game_folder() {
                return None;
            }
            let left = facts.status.filter(|status| status.resumable && facts.can_start());
            Some(if facts.up_to_date() { 100.0 } else { left.and_then(|status| status.resume_percent).unwrap_or(0.0) })
        }
        (_, Some(run)) => Some(run.percent),
    }
}

fn status(facts: &Facts) -> String {
    let run = facts.run;
    let word = match facts.phase {
        Phase::Paused => "일시정지",
        Phase::Failed if run.is_some_and(|run| run.refused) => "패치 불가",
        Phase::Failed => "패치 실패",
        Phase::Stopping if run.is_some_and(|run| run.leaving) => "종료 대기",
        Phase::Stopping => "일시정지 대기",
        // A download that has brought nothing for a while.
        Phase::Starting | Phase::Running if facts.quiet && run.is_some_and(|run| run.step == Some(Step::Download)) => {
            "다운로드 응답 대기"
        }
        Phase::Starting | Phase::Running => run.map_or("", |run| run.status),
        Phase::Done => "패치 완료",
        Phase::Ready if facts.not_installed() => "설치 경로 미확인",
        Phase::Ready if facts.own_run() => match facts.blocked() {
            true => "패치 불가",
            false => "중단된 패치 있음",
        },
        Phase::Ready if facts.unanswered() => "최신 버전 확인 중",
        Phase::Ready if facts.up_to_date() => "최신 버전",
        Phase::Ready => match facts.answer.and_then(|answer| answer.error.as_deref()) {
            Some(error) => error,
            None if facts.blocked() => "패치 불가",
            None if facts.status.is_some_and(|status| status.resumable) && facts.can_start() => "중단된 패치 있음",
            None if facts.target().is_none() => "패치 확인 불가",
            None => "새 패치 있음",
        },
    };
    word.to_owned()
}

/// How the finished run went, and after it what the server says comes next: nothing, or
/// another patch. That is said by the answer there is, so a patch that comes out while the
/// summary is up takes the place of "nothing", and the button changes with it.
fn summary(facts: &Facts) -> Option<String> {
    let run = facts.run.filter(|_| facts.phase == Phase::Done)?;
    let took = match run.took {
        seconds if seconds >= 60 => format!("{}분 {}초", seconds / 60, seconds % 60),
        seconds => format!("{seconds}초"),
    };
    let went = format!("소요 시간 {took} · 변경 파일 {}개", run.files);
    Some(match (facts.answer.filter(|answer| answer.error.is_none()), facts.target()) {
        (None, _) => went,
        (Some(_), Some(next)) => format!("{went} · 다음 패치 {}", next.label),
        (Some(_), None) => format!("{went} · 추가 패치 없음"),
    })
}

/// The one button at the bottom right. What it does follows from where things stand.
fn button(facts: &Facts) -> Button {
    let (ready, done) = (facts.phase == Phase::Ready, facts.phase == Phase::Done);
    if ready && facts.not_installed() {
        return Button { kind: ButtonKind::Install, enabled: true, primary: true };
    }
    if ready && facts.ask_again() {
        return Button { kind: ButtonKind::CheckAgain, enabled: true, primary: false };
    }
    if (ready && facts.up_to_date()) || (done && facts.target().is_none()) {
        // The game is open, was just started, or what comes next is still being asked.
        let resting = facts.game_running() || facts.launching || facts.settling();
        let kind = if facts.game_running() { ButtonKind::GameRunning } else { ButtonKind::Launch };
        return Button { kind, enabled: !resting, primary: !resting };
    }
    if ready || done {
        // A run that was left unfinished is taken up again; anything else begins with the
        // download. What is in the way does not disable the button: pressing it says what.
        let resuming = ready && facts.status.is_some_and(|status| status.resumable);
        let kind = if resuming { ButtonKind::Resume } else { ButtonKind::Download };
        let able = facts.can_start() && !facts.outdated;
        return Button { kind, enabled: able, primary: able && !facts.blocked() };
    }
    if facts.phase.halted() {
        let kind = if facts.phase == Phase::Paused { ButtonKind::Resume } else { ButtonKind::Retry };
        return Button { kind, enabled: !facts.outdated, primary: !facts.outdated };
    }
    // An apply has begun and runs to its end; it cannot be paused. Nor can a run that was
    // asked to stop be asked again.
    let applying = facts.run.is_some_and(|run| run.step == Some(Step::Apply));
    Button { kind: ButtonKind::Pause, enabled: facts.phase != Phase::Stopping && !applying, primary: false }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::flow::{Failure, Run};
    use crate::made::{
        errored, game_at, game_open, half_patched, no_game_at, no_install, patch_to, resumable, run_of, short_of_room,
        up_to_date,
    };
    use crate::state::{Availability, Check, Status};

    /// Everything a screen is made from, as a test sets it up.
    struct Scene {
        status: Option<Status>,
        answer: Option<Availability>,
        phase: Phase,
        run: Option<Run>,
        source: Option<String>,
        outdated: bool,
        quiet: bool,
        launching: bool,
    }

    impl Scene {
        /// The ready screen of `status` with the server having said `answer`.
        fn ready(status: Status, answer: Option<Availability>) -> Scene {
            Scene {
                status: Some(status),
                answer,
                phase: Phase::Ready,
                run: None,
                source: None,
                outdated: false,
                quiet: false,
                launching: false,
            }
        }

        /// A run of the patch to 1.2.422 on the screen, in `phase`.
        fn with_run(phase: Phase, change: impl FnOnce(&mut Run)) -> Scene {
            let mut run = run_of(None, Some("1.2.422"));
            (run.percent, run.step, run.status) = (61.5, Some(Step::Rebuild), "새 게임 파일 생성 중");
            (run.done, run.total, run.activity) = (5, 10, "생성중: Map001.wz".to_owned());
            change(&mut run);
            Scene { phase, run: Some(run), ..Scene::ready(game_at(r"C:\Nexon\Maple"), Some(patch_to("1.2.422"))) }
        }

        /// The screen of a run that is done: 95 seconds, 718 files.
        fn done(answer: Option<Availability>) -> Scene {
            let mut scene = Scene::with_run(Phase::Done, |run| (run.percent, run.took, run.files) = (100.0, 95, 718));
            scene.answer = answer;
            scene
        }

        fn shown(&self) -> Shown {
            let hands_off = BTreeSet::new();
            shown(&Facts {
                status: self.status.as_ref(),
                answer: self.answer.as_ref(),
                phase: self.phase,
                run: self.run.as_ref(),
                source: self.source.as_deref(),
                hands_off: &hands_off,
                game_due: false,
                auto_patch: false,
                auto_start: false,
                outdated: self.outdated,
                run_at_work: self.phase.at_work(),
                open: Some(true),
                quiet: self.quiet,
                launching: self.launching,
            })
        }

        /// The footer: its word, whether it is bad news, and whether the spinner turns.
        fn footer(&self) -> (String, bool, bool) {
            let shown = self.shown();
            (shown.status, shown.status_bad, shown.busy)
        }

        fn button(&self) -> (ButtonKind, bool, bool) {
            let Button { kind, enabled, primary } = self.shown().button;
            (kind, enabled, primary)
        }
    }

    fn game() -> Status {
        game_at(r"C:\Nexon\Maple")
    }

    fn word(of: &str, bad: bool, busy: bool) -> (String, bool, bool) {
        (of.to_owned(), bad, busy)
    }

    // The footer's word, in the order the screen has always decided it.

    #[test]
    fn an_app_that_is_too_old_says_so_in_the_footer_except_over_a_run_at_work() {
        for phase in [Phase::Ready, Phase::Paused, Phase::Failed, Phase::Done] {
            let scene = Scene { outdated: true, ..Scene::with_run(phase, |_| ()) };
            assert_eq!(scene.footer(), word("최신 버전으로 업데이트해 주세요.", true, false), "{phase:?}");
        }
        // Before the server has answered it is the reason nothing is offered, not a wait.
        let waiting = Scene { outdated: true, ..Scene::ready(game(), None) };
        assert_eq!(waiting.footer(), word("최신 버전으로 업데이트해 주세요.", true, false));
        for phase in [Phase::Starting, Phase::Running] {
            let scene = Scene { outdated: true, ..Scene::with_run(phase, |_| ()) };
            assert_eq!(scene.footer(), word("새 게임 파일 생성 중", false, false), "{phase:?}");
        }
    }

    #[test]
    fn a_paused_run_says_paused() {
        assert_eq!(Scene::with_run(Phase::Paused, |_| ()).footer(), word("일시정지", false, false));
    }

    #[test]
    fn a_run_that_was_turned_down_says_that_it_cannot_patch() {
        assert_eq!(Scene::with_run(Phase::Failed, |run| run.refused = true).footer(), word("패치 불가", true, false));
    }

    #[test]
    fn a_run_that_failed_says_so() {
        assert_eq!(Scene::with_run(Phase::Failed, |_| ()).footer(), word("패치 실패", true, false));
    }

    #[test]
    fn a_run_that_is_stopping_waits_for_the_pause_or_for_the_app_to_go() {
        assert_eq!(Scene::with_run(Phase::Stopping, |_| ()).footer(), word("일시정지 대기", false, true));
        assert_eq!(Scene::with_run(Phase::Stopping, |run| run.leaving = true).footer(), word("종료 대기", false, true));
    }

    #[test]
    fn a_download_that_brings_nothing_waits_for_the_server() {
        let downloading = |run: &mut Run| (run.step, run.status) = (Some(Step::Download), "패치 파일 다운로드 중");
        let quiet = Scene { quiet: true, ..Scene::with_run(Phase::Running, downloading) };
        assert_eq!(quiet.footer(), word("다운로드 응답 대기", false, false));
        assert_eq!(Scene::with_run(Phase::Running, downloading).footer(), word("패치 파일 다운로드 중", false, false));
        // Any other step is at work on this PC and waits for no one.
        let building = Scene { quiet: true, ..Scene::with_run(Phase::Running, |_| ()) };
        assert_eq!(building.footer(), word("새 게임 파일 생성 중", false, false));
        // Nor does a run that is stopping say it.
        let stopping = Scene { quiet: true, ..Scene::with_run(Phase::Stopping, downloading) };
        assert_eq!(stopping.footer(), word("일시정지 대기", false, true));
    }

    #[test]
    fn a_run_at_work_says_its_step_and_nothing_before_the_first() {
        assert_eq!(Scene::with_run(Phase::Running, |_| ()).footer(), word("새 게임 파일 생성 중", false, false));
        let begun = |run: &mut Run| (run.step, run.status) = (None, "");
        assert_eq!(Scene::with_run(Phase::Starting, begun).footer(), word("", false, false));
    }

    #[test]
    fn a_finished_run_says_that_the_patch_is_done() {
        assert_eq!(Scene::done(None).footer(), word("패치 완료", false, false));
    }

    #[test]
    fn a_game_that_is_not_installed_says_that_the_folder_is_not_known() {
        // The answer where there is nothing to ask about, and before there is any.
        assert_eq!(
            Scene::ready(no_install(), Some(Availability::nothing())).footer(),
            word("설치 경로 미확인", true, false)
        );
        assert_eq!(Scene::ready(no_install(), None).footer(), word("설치 경로 미확인", false, false));
    }

    #[test]
    fn a_run_of_a_picked_file_that_waits_says_that_a_patch_was_left_off() {
        let file = r"D:\patches\00421to00422.patch";
        let waits = Scene::ready(resumable(game(), Some(file), Some(41.0)), None);
        // The server has no say in that: no answer is waited for.
        assert_eq!(waits.footer(), word("중단된 패치 있음", false, false));
        let blocked = Scene::ready(game_open(resumable(game(), Some(file), Some(41.0))), None);
        assert_eq!(blocked.footer(), word("패치 불가", true, false));
    }

    #[test]
    fn a_folder_the_server_has_not_answered_for_says_that_it_is_asking() {
        assert_eq!(Scene::ready(game(), None).footer(), word("최신 버전 확인 중", false, true));
        // An answer that names no version and no reason came while the folder was being changed.
        assert_eq!(
            Scene::ready(game(), Some(Availability::nothing())).footer(),
            word("최신 버전 확인 중", false, true)
        );
        // Before the first look there is nothing to say either.
        let nothing = Scene { status: None, ..Scene::ready(game(), None) };
        assert_eq!(nothing.footer(), word("최신 버전 확인 중", false, true));
    }

    #[test]
    fn an_install_with_nothing_newer_on_the_server_says_newest_version() {
        assert_eq!(Scene::ready(game(), Some(up_to_date("1.2.421"))).footer(), word("최신 버전", false, false));
        // The game being open is in nobody's way then.
        assert_eq!(
            Scene::ready(game_open(game()), Some(up_to_date("1.2.421"))).footer(),
            word("최신 버전", false, false)
        );
    }

    #[test]
    fn a_check_that_could_not_be_made_says_why() {
        assert_eq!(Scene::ready(game(), Some(errored("서버 요청 실패"))).footer(), word("서버 요청 실패", true, false));
        // Also beside a patch that was found before the server went quiet.
        let stale = Availability { error: Some("서버 요청 실패".to_owned()), ..patch_to("1.2.422") };
        assert_eq!(Scene::ready(game(), Some(stale)).footer(), word("서버 요청 실패", true, false));
    }

    #[test]
    fn something_in_the_way_of_a_patch_says_that_it_cannot_patch() {
        let playing = Scene::ready(game_open(game()), Some(patch_to("1.2.422")));
        assert_eq!(playing.footer(), word("패치 불가", true, false));
        let no_room = Scene::ready(short_of_room(game()), Some(patch_to("1.2.422")));
        assert_eq!(no_room.footer(), word("패치 불가", true, false));
        // Nothing to apply and something in the way all the same: not up to date, and bad.
        let idle = Scene::ready(short_of_room(game()), Some(up_to_date("1.2.421")));
        assert_eq!(idle.footer(), word("패치 불가", true, false));
    }

    #[test]
    fn a_run_of_the_servers_patch_that_was_left_off_says_so() {
        let left = Scene::ready(resumable(game(), None, Some(69.25)), Some(patch_to("1.2.422")));
        assert_eq!(left.footer(), word("중단된 패치 있음", false, false));
    }

    /// What a run left lies in a folder the server has nothing for, and no file to go on
    /// with: the install is at the newest version all the same. This is also why the word
    /// for "no patch can be made out" is never said: every way to it leads through one of
    /// the words before it.
    #[test]
    fn work_left_in_a_folder_that_is_up_to_date_does_not_keep_it_from_saying_so() {
        let stranded = Scene::ready(resumable(game(), None, Some(69.25)), Some(up_to_date("1.2.421")));
        assert_eq!(stranded.footer(), word("최신 버전", false, false));
    }

    #[test]
    fn a_patch_on_the_server_says_that_there_is_a_new_one() {
        assert_eq!(Scene::ready(game(), Some(patch_to("1.2.422"))).footer(), word("새 패치 있음", false, false));
    }

    #[test]
    fn a_folder_that_is_no_game_folder_says_that_it_cannot_patch() {
        let empty = Scene::ready(no_game_at(r"D:\Empty"), Some(Availability::nothing()));
        assert_eq!(empty.footer(), word("패치 불가", true, false));
    }

    // The big figure.

    #[test]
    fn the_figure_of_the_ready_screen_is_full_when_up_to_date_and_where_a_run_was_left_otherwise() {
        assert_eq!(Scene::ready(game(), Some(up_to_date("1.2.421"))).shown().percent, Some(100.0));
        assert_eq!(Scene::ready(game(), Some(patch_to("1.2.422"))).shown().percent, Some(0.0));
        assert_eq!(Scene::ready(game(), None).shown().percent, Some(0.0));
        let left = Scene::ready(resumable(game(), None, Some(69.25)), Some(patch_to("1.2.422")));
        assert_eq!(left.shown().percent, Some(69.25));
        // Work that nothing goes on with is not what the figure is of, nor has a record
        // without a figure one.
        let stranded = Scene::ready(resumable(game(), None, Some(69.25)), Some(up_to_date("1.2.421")));
        assert_eq!(stranded.shown().percent, Some(100.0));
        let unknown = Scene::ready(resumable(game(), None, None), Some(patch_to("1.2.422")));
        assert_eq!(unknown.shown().percent, Some(0.0));
    }

    /// Where no game is installed there is nothing to be at any point of: the page draws a
    /// dash for it, for the live game and the test world alike.
    #[test]
    fn a_folder_that_is_no_game_folder_has_no_figure() {
        assert_eq!(Scene::ready(no_install(), Some(Availability::nothing())).shown().percent, None);
        assert_eq!(Scene::ready(no_game_at(r"D:\Empty"), Some(Availability::nothing())).shown().percent, None);
        // Before the folder has been looked at, nothing is known of it either way.
        let unseen = Scene { status: None, ..Scene::ready(game(), None) };
        assert_eq!(unseen.shown().percent, Some(0.0));
    }

    #[test]
    fn the_figure_of_a_run_is_the_runs_and_a_finished_one_is_full() {
        for phase in [Phase::Starting, Phase::Running, Phase::Stopping, Phase::Paused, Phase::Failed] {
            assert_eq!(Scene::with_run(phase, |_| ()).shown().percent, Some(61.5), "{phase:?}");
        }
        assert_eq!(Scene::done(None).shown().percent, Some(100.0));
        // A run that only left something to say of itself is not on the ready screen's bar.
        let said = Scene::with_run(Phase::Ready, |run| run.percent = 4.0);
        assert_eq!(said.shown().percent, Some(0.0));
    }

    // The versions.

    #[test]
    fn the_installed_version_is_the_one_the_look_found_and_after_a_run_the_one_it_patched_to() {
        assert_eq!(Scene::ready(game(), None).shown().current.as_deref(), Some("1.2.421"));
        assert_eq!(Scene::done(None).shown().current.as_deref(), Some("1.2.422"));
        // A run of a picked file names no version; the look's stands.
        let file = Scene::with_run(Phase::Done, |run| run.label = None);
        assert_eq!(file.shown().current.as_deref(), Some("1.2.421"));
        // While a run is at work or stopped it is still the old one.
        assert_eq!(Scene::with_run(Phase::Paused, |_| ()).shown().current.as_deref(), Some("1.2.421"));
        assert_eq!(Scene::ready(half_patched(game(), None, None), None).shown().current, None);
    }

    // The lines of the ready screen.

    fn kinds(shown: &Shown) -> Vec<RowKind> {
        shown.rows.iter().map(|row| row.kind).collect()
    }

    #[test]
    fn the_ready_screen_lists_the_checks_that_show_then_the_patch_file() {
        let shown = Scene::ready(game(), Some(patch_to("1.2.422"))).shown();
        assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::PatchFile]);
        let folder = &shown.rows[0];
        assert_eq!((folder.label, folder.ok, folder.detail.as_str()), ("설치 경로", Some(true), r"C:\Nexon\Maple"));
        let space = &shown.rows[1];
        assert_eq!((space.label, space.ok, space.detail.as_str()), ("여유 공간", Some(true), "160.0 GB"));
        // The patch file with its size, in whole megabytes.
        let file = &shown.rows[2];
        assert_eq!((file.label, file.ok, file.detail.as_str()), ("패치 파일", Some(true), "1,229 MB"));
    }

    #[test]
    fn a_quiet_check_is_listed_only_when_it_has_failed_with_what_a_dialog_says_of_it() {
        let shown = Scene::ready(game_open(game()), Some(patch_to("1.2.422"))).shown();
        assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Running, RowKind::Space, RowKind::PatchFile]);
        let running = &shown.rows[1];
        assert_eq!(
            (running.label, running.ok, running.detail.as_str()),
            ("관련 프로그램 실행 중", Some(false), "MapleStory.exe")
        );
        assert_eq!(running.message, "게임 관련 프로그램이 실행 중입니다.\n해당 프로그램을 종료해 주세요.");
    }

    #[test]
    fn what_a_run_left_is_listed_after_the_checks_and_before_the_patch_file() {
        let shown = Scene::ready(resumable(game(), None, Some(41.0)), Some(patch_to("1.2.422"))).shown();
        assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::Leftover, RowKind::PatchFile]);
        let left = &shown.rows[2];
        assert_eq!((left.label, left.ok, left.detail.as_str()), ("이전 패치 작업", None, "12.3 GB"));
    }

    #[test]
    fn the_patch_file_line_is_empty_without_a_patch_and_missing_without_a_game_folder() {
        let none = Scene::ready(game(), Some(up_to_date("1.2.421"))).shown();
        let file = none.rows.last().unwrap();
        assert_eq!((file.kind, file.ok, file.detail.as_str()), (RowKind::PatchFile, None, ""));

        let empty = Scene::ready(no_game_at(r"D:\Empty"), Some(Availability::nothing())).shown();
        assert_eq!(kinds(&empty), [RowKind::Folder]);
        assert_eq!(
            (empty.rows[0].ok, empty.rows[0].message.as_str()),
            (Some(false), "메이플스토리 설치 경로가 아닙니다.")
        );
        // Before the first look there is nothing to list.
        assert!(Scene { status: None, ..Scene::ready(game(), None) }.shown().rows.is_empty());
    }

    #[test]
    fn a_cut_off_apply_that_could_not_be_undone_is_a_line_of_its_own() {
        let cut_off = Check::failed(CheckKind::Recovery, "실패", "이전 패치를 복구하지 못했습니다.");
        let mut status = game();
        status.checks.push(cut_off);
        let shown = Scene::ready(status, Some(errored("이전 패치 복구 대기"))).shown();
        assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::Recovery, RowKind::PatchFile]);
        assert_eq!((shown.status.as_str(), shown.status_bad), ("이전 패치 복구 대기", true));
    }

    #[test]
    fn sizes_are_written_in_whole_megabytes_with_the_thousands_set_apart() {
        assert_eq!(megabytes(0), "0 MB");
        assert_eq!(megabytes(524_287), "0 MB");
        assert_eq!(megabytes(524_288), "1 MB");
        assert_eq!(megabytes(999 << 20), "999 MB");
        assert_eq!(megabytes(1_288_490_189), "1,229 MB");
        assert_eq!(megabytes(34_359_738_368), "32,768 MB");
        assert_eq!(megabytes(1_234_567 << 20), "1,234,567 MB");
    }

    // The line under a finished run.

    #[test]
    fn a_finished_run_says_how_long_it_took_and_how_many_files_it_changed() {
        assert_eq!(Scene::done(None).shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개"));
        let quick = Scene::with_run(Phase::Done, |run| (run.took, run.files) = (59, 3));
        let quick = Scene { answer: None, ..quick };
        assert_eq!(quick.shown().summary.as_deref(), Some("소요 시간 59초 · 변경 파일 3개"));
        // No other screen has the line.
        for phase in [Phase::Ready, Phase::Running, Phase::Paused, Phase::Failed] {
            assert_eq!(Scene::with_run(phase, |_| ()).shown().summary, None, "{phase:?}");
        }
    }

    /// What comes next is said the moment the server says it, and the button with it.
    #[test]
    fn the_summary_and_the_button_change_together_when_the_next_answer_arrives() {
        // The patch has just finished: the server is being asked, and the button waits.
        let settling = Scene::done(None);
        assert_eq!(settling.shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개"));
        assert_eq!(settling.button(), (ButtonKind::Launch, false, false));

        let nothing_more = Scene::done(Some(up_to_date("1.2.422")));
        assert_eq!(
            nothing_more.shown().summary.as_deref(),
            Some("소요 시간 1분 35초 · 변경 파일 718개 · 추가 패치 없음")
        );
        assert_eq!(nothing_more.button(), (ButtonKind::Launch, true, true));

        // Another patch comes out while the summary is up.
        let another = Scene::done(Some(patch_to("1.2.423")));
        assert_eq!(
            another.shown().summary.as_deref(),
            Some("소요 시간 1분 35초 · 변경 파일 718개 · 다음 패치 1.2.423")
        );
        assert_eq!(another.button(), (ButtonKind::Download, true, true));
    }

    #[test]
    fn a_check_that_fails_after_a_run_adds_nothing_to_the_summary() {
        let failed = Scene::done(Some(errored("서버 요청 실패")));
        assert_eq!(failed.shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개"));
        assert_eq!(failed.button(), (ButtonKind::Launch, true, true));
    }

    // The button.

    #[test]
    fn a_game_that_is_not_installed_offers_its_download() {
        assert_eq!(
            Scene::ready(no_install(), Some(Availability::nothing())).button(),
            (ButtonKind::Install, true, true)
        );
        // A folder that was picked and is not there is another matter: there the folder is the trouble.
        let picked = Status { custom_dir: true, ..no_install() };
        assert_eq!(Scene::ready(picked, Some(Availability::nothing())).button(), (ButtonKind::Download, false, false));
    }

    #[test]
    fn a_check_that_could_not_be_made_offers_to_ask_again() {
        assert_eq!(
            Scene::ready(game(), Some(errored("서버 요청 실패"))).button(),
            (ButtonKind::CheckAgain, true, false)
        );
        // With a patch that was found before, or a run to go on with, that is offered instead.
        let stale = Availability { error: Some("서버 요청 실패".to_owned()), ..patch_to("1.2.422") };
        assert_eq!(Scene::ready(game(), Some(stale)).button(), (ButtonKind::Download, true, true));
        let file = r"D:\patches\00421to00422.patch";
        let own = Scene::ready(resumable(game(), Some(file), None), Some(errored("서버 요청 실패")));
        assert_eq!(own.button(), (ButtonKind::Resume, true, true));
    }

    #[test]
    fn an_install_that_is_up_to_date_offers_the_game() {
        assert_eq!(Scene::ready(game(), Some(up_to_date("1.2.421"))).button(), (ButtonKind::Launch, true, true));
    }

    #[test]
    fn a_game_that_is_open_says_so_on_the_button_and_one_just_started_lets_it_rest() {
        let playing = Scene::ready(game_open(game()), Some(up_to_date("1.2.421")));
        assert_eq!(playing.button(), (ButtonKind::GameRunning, false, false));
        let started = Scene { launching: true, ..Scene::ready(game(), Some(up_to_date("1.2.421"))) };
        assert_eq!(started.button(), (ButtonKind::Launch, false, false));
    }

    #[test]
    fn a_patch_offers_its_download_and_what_is_in_the_way_only_dims_the_button() {
        assert_eq!(Scene::ready(game(), Some(patch_to("1.2.422"))).button(), (ButtonKind::Download, true, true));
        // Pressing it says what is in the way.
        let playing = Scene::ready(game_open(game()), Some(patch_to("1.2.422")));
        assert_eq!(playing.button(), (ButtonKind::Download, true, false));
    }

    #[test]
    fn nothing_to_start_and_an_app_that_is_too_old_disable_the_button() {
        assert_eq!(Scene::ready(game(), None).button(), (ButtonKind::Download, false, false));
        assert_eq!(Scene { status: None, ..Scene::ready(game(), None) }.button(), (ButtonKind::Download, false, false));
        let outdated = Scene { outdated: true, ..Scene::ready(game(), Some(patch_to("1.2.422"))) };
        assert_eq!(outdated.button(), (ButtonKind::Download, false, false));
    }

    #[test]
    fn a_run_that_was_left_unfinished_offers_to_go_on_from_the_ready_screen() {
        let left = Scene::ready(resumable(game(), None, Some(69.25)), Some(patch_to("1.2.422")));
        assert_eq!(left.button(), (ButtonKind::Resume, true, true));
        // Work that nothing goes on with leaves the button to the game.
        let stranded = Scene::ready(resumable(game(), None, Some(69.25)), Some(up_to_date("1.2.421")));
        assert_eq!(stranded.button(), (ButtonKind::Launch, true, true));
        // While the server has not said, going on is offered and cannot be pressed.
        let unanswered = Scene::ready(resumable(game(), None, Some(69.25)), None);
        assert_eq!(unanswered.button(), (ButtonKind::Resume, false, false));
    }

    #[test]
    fn a_finished_run_offers_the_next_patch_as_a_download_never_as_one_to_go_on_with() {
        assert_eq!(Scene::done(Some(patch_to("1.2.423"))).button(), (ButtonKind::Download, true, true));
        let mut left_over = Scene::done(Some(patch_to("1.2.423")));
        left_over.status = Some(resumable(game(), None, None));
        assert_eq!(left_over.button(), (ButtonKind::Download, true, true));
    }

    #[test]
    fn a_run_that_stopped_offers_to_go_on_and_one_that_failed_to_try_again() {
        assert_eq!(Scene::with_run(Phase::Paused, |_| ()).button(), (ButtonKind::Resume, true, true));
        assert_eq!(Scene::with_run(Phase::Failed, |_| ()).button(), (ButtonKind::Retry, true, true));
        assert_eq!(Scene::with_run(Phase::Failed, |run| run.refused = true).button(), (ButtonKind::Retry, true, true));
        for phase in [Phase::Paused, Phase::Failed] {
            let outdated = Scene { outdated: true, ..Scene::with_run(phase, |_| ()) };
            assert!(matches!(outdated.button(), (_, false, false)), "{phase:?}");
        }
    }

    #[test]
    fn a_run_at_work_offers_the_pause_until_it_is_stopping_or_applying() {
        assert_eq!(Scene::with_run(Phase::Running, |_| ()).button(), (ButtonKind::Pause, true, false));
        assert_eq!(Scene::with_run(Phase::Starting, |run| run.step = None).button(), (ButtonKind::Pause, true, false));
        assert_eq!(Scene::with_run(Phase::Stopping, |_| ()).button(), (ButtonKind::Pause, false, false));
        // An apply runs to its end.
        let applying = Scene::with_run(Phase::Running, |run| run.step = Some(Step::Apply));
        assert_eq!(applying.button(), (ButtonKind::Pause, false, false));
    }

    #[test]
    fn a_failure_that_is_still_to_be_read_changes_nothing_on_the_ready_screen() {
        let failure = Failure {
            id: 1,
            title: "진행 중인 패치와 다른 패치 파일입니다.".to_owned(),
            message: None,
            details: Vec::new(),
        };
        let mut said = Scene::ready(half_patched(game(), None, Some(41.0)), Some(patch_to("1.2.421")));
        let quiet = said.shown();
        said.run = Some(Run { failure: Some(failure), ..run_of(Some(r"D:\patches\wrong.patch"), None) });
        assert_eq!(said.shown(), quiet);
        // The half-patched folder's own screen: the bar where the run was left, and going on.
        assert_eq!((quiet.status.as_str(), quiet.percent), ("중단된 패치 있음", Some(41.0)));
        assert_eq!(said.button(), (ButtonKind::Resume, true, true));
    }
}
