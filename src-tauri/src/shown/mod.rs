//! What the screen of a site says: the lines of the ready screen, the big figure, the word
//! in the footer, the line under a finished run, and the one button at the bottom right.
//!
//! All of it follows from the [`Facts`] of the site, and the page draws it as it comes. The
//! words are the ones the screen has always used.

#[cfg(test)]
mod tests;

use serde::Serialize;

use crate::address::file_at;
use crate::flow::{Facts, Phase};
use crate::state::{CheckKind, Step};
use crate::wording::{PUT_BACK_FIRST, UPDATE_FIRST, megabytes, minutes_and_seconds};

/// Which of the lines of the ready screen a row is. The page goes by this for what a row
/// offers beside its words: picking a folder, deleting what a run left, stating where the
/// patch is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RowKind {
    Folder,
    Running,
    Writable,
    Space,
    Recovery,
    /// The way of patching that was chosen, where the drive does not take it. The page
    /// shows it on its own line for the choice and not among the lines listed here.
    Way,
    /// What a paused or interrupted run left in the install folder.
    Leftover,
    /// The patch file a check found on the server for the install.
    PatchFile,
    /// What the diagnosis says of the PC, where it is no ordinary one; see [`crate::diagnosis::row`].
    Diagnosis,
}

impl From<CheckKind> for RowKind {
    fn from(kind: CheckKind) -> RowKind {
        match kind {
            CheckKind::Folder => RowKind::Folder,
            CheckKind::Running => RowKind::Running,
            CheckKind::Writable => RowKind::Writable,
            CheckKind::Space => RowKind::Space,
            CheckKind::Recovery => RowKind::Recovery,
            CheckKind::Way => RowKind::Way,
        }
    }
}

/// One line of the ready screen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Row {
    pub(crate) kind: RowKind,
    pub(crate) label: String,
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
    /// Goes on with a run that was left unfinished: 이어하기.
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
    /// The newest version the server has for the folder, when that is known. What the
    /// footer says of the install being up to date goes by the same; see
    /// [`Facts::unanswered`].
    pub(crate) latest: Option<String>,
    /// The address the user is given to begin from when they state where the patch is
    /// themselves: that of the patch the app found, and otherwise where the patch from the
    /// installed version to the next one would be. Empty where neither is known.
    pub(crate) address: String,
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
    // The status is bad news: something is in the way, the check failed, or the run did.
    let troubled = match facts.own_run() {
        true => facts.blocked(),
        false => {
            let failed = facts.answer.is_some_and(|answer| answer.error.is_some());
            facts.answer.is_some() && !facts.up_to_date() && (facts.blocked() || failed)
        }
    };
    // An apply that was cut off is being undone. That is waited for, and is no bad news.
    let putting_back = ready && facts.putting_back();
    // An app that is too old says so where the screen rests with nothing else the matter.
    // It is something to know, and keeps nobody from anything: the buttons stay as they
    // are, and it is no bad news. What is in the way, a folder being put back, and a run
    // that is at work or has ended go on saying their own.
    let told_to_update = facts.outdated && ready && !troubled && !putting_back;
    Shown {
        current: current(facts),
        latest: facts.latest.map(str::to_owned),
        address: address(facts),
        rows: rows(facts),
        percent: percent(facts),
        status: if told_to_update { UPDATE_FIRST.to_owned() } else { status(facts) },
        status_bad: (ready && troubled && !putting_back) || facts.phase == Phase::Failed,
        // Something is being waited for: the first answer from the server, a folder to be
        // put back, or a run to stop.
        busy: (ready && facts.unanswered() && !facts.not_installed() && !facts.own_run() && !told_to_update)
            || putting_back
            || facts.phase == Phase::Stopping,
        summary: summary(facts),
        button: button(facts),
    }
}

fn address(facts: &Facts) -> String {
    let found = facts.target().map(|target| target.url.clone());
    found.or_else(|| facts.status.and_then(|status| status.next_address.clone())).unwrap_or_default()
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
            label: check.label.clone(),
            ok: Some(check.ok),
            detail: check.detail.clone(),
            message: check.message.clone(),
        })
        .collect();
    if let Some(leftover) = &status.leftover {
        let (label, detail) = ("이전 패치 작업".to_owned(), leftover.clone());
        rows.push(Row { kind: RowKind::Leftover, label, ok: None, detail, message: String::new() });
    }
    if facts.game_folder() {
        let target = facts.target();
        rows.push(Row {
            kind: RowKind::PatchFile,
            label: "패치 파일".to_owned(),
            ok: target.map(|_| true),
            // Which file it is, by the name it has on the server, and how large.
            detail: target
                .map(|target| format!("{} · {}", file_at(&target.url), megabytes(target.size)))
                .unwrap_or_default(),
            message: String::new(),
        });
    }
    rows
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
        Phase::Ready if facts.putting_back() => PUT_BACK_FIRST,
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
            None => match facts.target() {
                None => "패치 확인 불가",
                // Which kind of patch it is, is said with it: a new version, or a minor
                // patch within the one that is installed.
                Some(target) => return format!("새 {} 있음", target.kind()),
            },
        },
    };
    word.to_owned()
}

/// How the finished run went, and after it what the server says comes next: nothing, or
/// another patch. That is said by the answer there is, so a patch that comes out while the
/// summary is up takes the place of "nothing", and the button changes with it.
fn summary(facts: &Facts) -> Option<String> {
    let run = facts.run.filter(|_| facts.phase == Phase::Done)?;
    let went = format!("소요 시간 {} · 변경 파일 {}개", minutes_and_seconds(run.took), run.files);
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
    // A folder that is being put back is at work: nothing is begun or asked about until it
    // is back, and the button says so by being off, whichever it would be.
    let putting_back = ready && facts.putting_back();
    if ready && facts.ask_again() {
        return Button { kind: ButtonKind::CheckAgain, enabled: !putting_back, primary: false };
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
        // Nor is anything begun in a folder whose game files are being checked.
        let able = facts.can_start() && !putting_back && !facts.checking_files;
        return Button { kind, enabled: able, primary: able && !facts.blocked() };
    }
    if facts.phase.halted() {
        let kind = if facts.phase == Phase::Paused { ButtonKind::Resume } else { ButtonKind::Retry };
        let able = !facts.checking_files;
        return Button { kind, enabled: able, primary: able };
    }
    // An apply has begun and runs to its end; it cannot be paused. Nor can a run that was
    // asked to stop be asked again.
    let applying = facts.run.is_some_and(|run| run.step == Some(Step::Apply));
    Button { kind: ButtonKind::Pause, enabled: facts.phase != Phase::Stopping && !applying, primary: false }
}
