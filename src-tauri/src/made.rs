//! Made-up looks at an install folder and answers of the server, for the tests of the rules
//! that go by them, and the registry of a made-up PC for the tests that need a site.

use std::path::{Path, PathBuf};

use crate::flow::Run;
use crate::state::{Availability, Check, CheckKind, Registry, Status, Target};

/// What the registry of a made-up PC says of a client's install. A test hands it to the
/// site it makes (see [`Site::of`](crate::state::Site::of)), which then asks it in Windows'
/// place: no test looks at the registry of the PC it runs on, or at the folder a real game
/// is in, and none comes out another way for what is installed there.
pub(crate) struct Registered(Option<(PathBuf, u32)>);

impl Registered {
    /// No install of the client is registered, and the registry gives no version: a PC
    /// the game was never installed on, or one whose game is a copy the registry knows
    /// nothing of.
    pub(crate) fn nowhere() -> Box<Registered> {
        Box::new(Registered(None))
    }

    /// The client's install is registered at `dir`, with `version` as what the registry
    /// gives for it: the install nobody had to pick.
    pub(crate) fn at(dir: &Path, version: u32) -> Box<Registered> {
        Box::new(Registered(Some((dir.to_path_buf(), version))))
    }
}

impl Registry for Registered {
    fn install_dir(&self) -> Option<PathBuf> {
        self.0.as_ref().map(|(dir, _)| dir.clone())
    }

    fn version(&self) -> Option<u32> {
        self.0.as_ref().map(|(_, version)| *version)
    }
}

/// Addresses the user gives for a patch, each another than that of the patch a check finds
/// (see [`target`]): the tests of the rules for a run by address tell them apart by these.
pub(crate) const GIVEN: &str = "https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00420to00422.patch";
pub(crate) const GIVEN_OTHER: &str = "https://maplestory.dn.nexoncdn.co.kr/Patch/00424/00420to00424.patch";
pub(crate) const GIVEN_ANOTHER: &str = "https://maplestory.dn.nexoncdn.co.kr/Patch/00423/00420to00423.patch";
pub(crate) const GIVEN_WRONG: &str = "https://maplestory.dn.nexoncdn.co.kr/Patch/00425/00420to00425.patch";
pub(crate) const GIVEN_BEFORE: &str = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";

/// What a look finds at `dir`, with the checks it made.
pub(crate) fn folder(dir: Option<&str>, checks: Vec<Check>) -> Status {
    Status {
        install_dir: dir.map(str::to_owned),
        version: None,
        next_address: None,
        checks,
        custom_dir: false,
        leftover: None,
        resumable: false,
        resume_address: None,
        resume_percent: None,
        half_patched: false,
        unreadable_journal: false,
        putting_back: false,
        unread_patch: None,
        by_press_only: false,
        room: None,
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
fn with_check(mut status: Status, check: Check) -> Status {
    match status.checks.iter_mut().find(|known| known.kind == check.kind) {
        Some(known) => *known = check,
        None => status.checks.push(check),
    }
    status
}

/// The same folder with the game open in it.
pub(crate) fn game_open(status: Status) -> Status {
    with_check(status, Check::game_open("메이플스토리"))
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

/// The same folder with a run left unfinished in it, which can be continued: of the patch
/// at `address` when the user had given one, and as far along as `percent`.
pub(crate) fn resumable(status: Status, address: Option<&str>, percent: Option<f64>) -> Status {
    Status {
        resumable: true,
        resume_address: address.map(str::to_owned),
        resume_percent: percent,
        leftover: Some("12.3 GB".to_owned()),
        ..status
    }
}

/// The same folder left partly new by a run that replaced game files as it built them.
pub(crate) fn half_patched(status: Status, address: Option<&str>, percent: Option<f64>) -> Status {
    Status { half_patched: true, version: None, ..resumable(status, address, percent) }
}

/// The same folder with an apply cut off in it whose record cannot be read: nothing to go on
/// with, and what the run left still there.
pub(crate) fn journal_unreadable(status: Status) -> Status {
    let message = "이전 패치 작업 기록이 손상되었습니다.\n공식 런처로 게임을 복구해 주세요.";
    let status = with_check(status, Check::failed(CheckKind::Recovery, "작업 기록 손상", message));
    Status { unreadable_journal: true, leftover: Some("12.3 GB".to_owned()), version: None, ..status }
}

/// The patch from the server that leads to `label`.
pub(crate) fn target(label: &str) -> Target {
    Target {
        label: label.to_owned(),
        size: 1_288_490_189,
        url: "https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch".to_owned(),
        file_name: "00421to00422.patch".to_owned(),
        version: 422,
        minor: None,
    }
}

/// The answer of a check that found the patch to `label` for the install.
pub(crate) fn patch_to(label: &str) -> Availability {
    Availability { target: Some(target(label)), latest: Some(label.to_owned()), error: None }
}

/// The answer of a check that found a minor patch for the install, which leads to minor
/// `minor` of the version it is at: `label` is that version with the minor behind it.
pub(crate) fn minor_patch_to(label: &str, minor: u32) -> Availability {
    let target = Target { version: 421, minor: Some(minor), ..target(label) };
    Availability { target: Some(target), latest: Some(label.to_owned()), error: None }
}

/// The answer of a check that found the install at the newest version, `latest`.
pub(crate) fn up_to_date(latest: &str) -> Availability {
    Availability { target: None, latest: Some(latest.to_owned()), error: None }
}

/// The answer of a check that could not be made, for the reason `error`.
pub(crate) fn errored(error: &str) -> Availability {
    Availability { error: Some(error.to_owned()), ..Availability::nothing() }
}

/// A run of the patch from the server to `label`, or of the one at the address `source`,
/// that has said nothing yet.
pub(crate) fn run_of(source: Option<&str>, label: Option<&str>) -> Run {
    Run {
        source: source.map(str::to_owned),
        label: label.map(str::to_owned),
        leaving: false,
        asking: false,
        asked_at: None,
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
