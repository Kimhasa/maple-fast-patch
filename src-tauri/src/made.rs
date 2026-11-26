//! Made-up looks at an install folder and answers of the server, for the tests of the rules
//! that go by them.

use crate::flow::Run;
use crate::state::{Availability, Check, CheckKind, Status, Target};

/// What a look finds at `dir`, with the checks it made.
pub(crate) fn folder(dir: Option<&str>, checks: Vec<Check>) -> Status {
    Status {
        install_dir: dir.map(str::to_owned),
        version: None,
        checks,
        custom_dir: false,
        leftover: None,
        resumable: false,
        resume_file: None,
        resume_percent: None,
        half_patched: false,
        by_press_only: false,
    }
}

/// What a look finds of the game installed at `dir`, at version 1.2.421, with nothing in
/// the way of a patch.
pub(crate) fn game_at(dir: &str) -> Status {
    let checks = vec![
        Check::passed(CheckKind::Folder, dir),
        Check::passed(CheckKind::Running, "").quiet(),
        Check::passed(CheckKind::Writable, "").quiet(),
        Check::passed(CheckKind::Space, "160.0 GB"),
    ];
    Status { version: Some("1.2.421".to_owned()), ..folder(Some(dir), checks) }
}

/// The same folder with `check` in the place of the check of its kind.
pub(crate) fn with_check(mut status: Status, check: Check) -> Status {
    match status.checks.iter_mut().find(|known| known.kind == check.kind) {
        Some(known) => *known = check,
        None => status.checks.push(check),
    }
    status
}

/// The same folder with the game open in it.
pub(crate) fn game_open(status: Status) -> Status {
    let message = "게임 관련 프로그램이 실행 중입니다.\n해당 프로그램을 종료해 주세요.";
    with_check(status, Check::failed(CheckKind::Running, "MapleStory.exe", message).quiet())
}

/// The same folder on a drive without the room for the patch file.
pub(crate) fn short_of_room(status: Status) -> Status {
    let message = "디스크 공간이 부족합니다.\nC: 여유 공간을 3.1 GB 이상 확보해 주세요. (현재 1.2 GB)";
    with_check(status, Check::failed(CheckKind::Space, "C: 필요 3.1 GB · 여유 1.2 GB", message))
}

/// What a look finds of a folder at `dir` that the game is not in.
pub(crate) fn no_game_at(dir: &str) -> Status {
    let not_one =
        Check::failed(CheckKind::Folder, format!("{dir} · 설치 경로 아님"), "메이플스토리 설치 경로가 아닙니다.");
    Status { custom_dir: true, ..folder(Some(dir), vec![not_one]) }
}

/// What a look finds when the game is not installed and no folder was picked.
pub(crate) fn no_install() -> Status {
    let none = Check::failed(CheckKind::Folder, "찾을 수 없음", "메이플스토리 설치 경로를 찾을 수 없습니다.");
    folder(None, vec![none])
}

/// The same folder with a run left unfinished in it, which can be continued: of `file` when
/// the user had picked one, and as far along as `percent`.
pub(crate) fn resumable(status: Status, file: Option<&str>, percent: Option<f64>) -> Status {
    Status {
        resumable: true,
        resume_file: file.map(str::to_owned),
        resume_percent: percent,
        leftover: Some("12.3 GB".to_owned()),
        ..status
    }
}

/// The same folder left partly new by a run that replaced game files as it built them.
pub(crate) fn half_patched(status: Status, file: Option<&str>, percent: Option<f64>) -> Status {
    Status { half_patched: true, version: None, ..resumable(status, file, percent) }
}

/// The patch from the server that leads to `label`.
pub(crate) fn target(label: &str) -> Target {
    Target {
        label: label.to_owned(),
        size: 1_288_490_189,
        url: format!("https://maplestory.dn.nexoncdn.co.kr/Patch/00422/{label}.patch"),
        file_name: "00421to00422.patch".to_owned(),
        version: 422,
        minor: None,
    }
}

/// The answer of a check that found the patch to `label` for the install.
pub(crate) fn patch_to(label: &str) -> Availability {
    Availability { target: Some(target(label)), latest: Some(label.to_owned()), error: None }
}

/// The answer of a check that found the install at the newest version, `latest`.
pub(crate) fn up_to_date(latest: &str) -> Availability {
    Availability { target: None, latest: Some(latest.to_owned()), error: None }
}

/// The answer of a check that could not be made, for the reason `error`.
pub(crate) fn errored(error: &str) -> Availability {
    Availability { error: Some(error.to_owned()), ..Availability::nothing() }
}

/// A run of the patch from the server to `label`, or of the file `source`, that has said
/// nothing yet.
pub(crate) fn run_of(source: Option<&str>, label: Option<&str>) -> Run {
    Run {
        source: source.map(str::to_owned),
        label: label.map(str::to_owned),
        leaving: false,
        percent: 0.0,
        step: None,
        done: 0,
        total: 0,
        worked: 0,
        activity: String::new(),
        failure: None,
        status: "",
        refused: false,
        took: 0,
        files: 0,
    }
}
