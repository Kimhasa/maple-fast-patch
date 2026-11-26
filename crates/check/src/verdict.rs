//! Says what kind of PC this looks like and whether a patch ahead of time is expected to
//! work on it, each with the facts it rests on.
//!
//! The rules are one function without side effects, [`verdict`], so that every one of them
//! can be tried on made-up facts. They are cautious on purpose: a verdict names a kind only
//! when the facts point one way, and says that it cannot tell when they do not.
//!
//! What decides is where the game's files are kept. On a PC with a disk of its own, what a
//! patch writes stays. On a PC room seat without one, everything the seat writes goes to a
//! hidden space of limited size and is gone at the next start; a main patch writes about
//! 34 GB, which mostly does not fit there, and the seat cannot see how much does. On a seat
//! with a disk and a program that puts the disk back at every start, the patch works and
//! whether it stays depends on a setting the seat cannot see either.
//!
//! The kind of PC, by the first of these that holds:
//!
//! 1. A program that serves seats without a disk runs: a seat without a disk.
//! 2. Windows or the game is on a disk that comes over the network: a seat without a disk.
//! 3. Both are on real disks and a restore program runs: a disk with a restore program.
//! 4. What the write probe wrote took room on another drive than the one it was written
//!    to: a seat without a disk.
//! 5. One of the two is on a made-up disk, or the game's drive reads the same bytes several
//!    times faster the second time, and a PC room's program runs: a seat without a disk.
//! 6. Both are on real disks, nothing of a PC room runs, nor anything whose name hints at
//!    one, and the second read was no faster than the first: an ordinary PC.
//! 7. Anything else: not told.
//!
//! Whether a main patch goes through, by the first of these that holds:
//!
//! 1. No install folder: not told.
//! 2. The folder takes no writes, or its files are known not to be Nexon's: unlikely.
//! 3. A seat without a disk: unlikely where the probe showed the drive the writes pile up
//!    on and that drive is too small, and not told otherwise, since the seat's limit
//!    cannot be seen.
//! 4. The room on the game's drive cannot be read: not told. Too little room: unlikely.
//! 5. The game is on a real disk and the folder took the empty file: expected.
//! 6. Anything else: not told. And what was expected is not told either where the files
//!    fit no known version and may be of a version this program does not know.
//!
//! Whether the patch stays after a restart follows the kind: expected on an ordinary PC,
//! not expected on a seat without a disk, and not told for the other two.

use crate::facts::{Bus, DriveKind, Facts, Landing, Programs, Volume, WriteTest};
use crate::known::State;
use crate::privacy::Scrub;
use crate::units::{gb, size};

/// Bytes of new files a main patch writes: 34.5 GiB for the patch to 1.2.421.
pub const MAIN_PATCH_WRITES: u64 = 69 << 29;
/// Room the game's drive needs for a main patch the safe way: the new files, the patch file
/// itself (about 1 GiB), and the 1 GiB the engine keeps free.
pub const ROOM_NEEDED: u64 = MAIN_PATCH_WRITES + (2 << 30);

/// How a drive is attached, as far as it matters for where writes end up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attached {
    /// A disk in or at the PC itself.
    Disk,
    /// A disk another computer hands over the network.
    Network,
    /// A disk that is really a file or a driver's making. A seat without a disk can look
    /// like this, and so can a virtual disk somebody mounted at home.
    Virtual,
    /// A folder another computer shares.
    Share,
    /// Windows names a way of attaching that both real and made-up disks use, or none.
    Unclear,
}

/// Words in the name a disk gives itself that mark it as made up, whatever bus it claims.
const MADE_UP: [&str; 8] = ["virtual", "vdisk", "iscsi", "diskless", "netdisk", "ccboot", "vhd", "ramdisk"];

/// Sorts a drive by how it is attached.
pub fn attached(kind: DriveKind, bus: Option<Bus>, model: &str) -> Attached {
    match kind {
        DriveKind::Remote => return Attached::Share,
        DriveKind::RamDisk | DriveKind::Disc | DriveKind::Other => return Attached::Unclear,
        DriveKind::Fixed | DriveKind::Removable => {}
    }
    let model = model.to_lowercase();
    let made_up = MADE_UP.iter().any(|word| model.contains(word));
    match bus.map(|bus| bus.0) {
        Some(9) => Attached::Network,
        Some(14 | 15) => Attached::Virtual,
        // ATAPI, ATA, 1394, SSA, Fibre Channel, USB, RAID, SAS, SATA, SD, MMC, storage
        // spaces, NVMe, SCM and UFS: ways a real disk is attached. A RAID controller is how
        // many home PCs present their one NVMe disk.
        Some(2..=8 | 10..=13 | 16..=19) if !made_up => Attached::Disk,
        _ => Attached::Unclear,
    }
}

/// One drive, as the verdict looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// `C:`, or nothing for a shared folder without a letter.
    pub letter: String,
    pub attached: Attached,
    /// How it is attached, in the words the report uses: `NVMe`, `iSCSI`.
    pub how: String,
    pub free: Option<u64>,
    pub read_only: bool,
}

impl Drive {
    /// What the verdict needs of a volume.
    pub fn of(volume: &Volume) -> Drive {
        let bus = volume.bus.ok();
        let how = match (volume.kind, bus) {
            (DriveKind::Fixed | DriveKind::Removable, Some(bus)) => bus.label(),
            (kind, _) => kind.label().to_owned(),
        };
        Drive {
            letter: volume.letter.clone(),
            attached: attached(volume.kind, bus, &volume.model),
            how,
            free: volume.free.ok(),
            read_only: volume.read_only,
        }
    }
}

/// Where the probe's writes showed up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    /// Bytes the probe wrote.
    pub written: u64,
    /// Another volume than the one written to lost about as much room: its letter and what
    /// is free on it now.
    pub elsewhere: Option<(String, Option<u64>)>,
}

impl Landed {
    /// Reads where a probe's writes landed out of what was free before and after. A volume
    /// that was not written to and lost at least half of what was written took the writes.
    pub fn of(landing: &Landing) -> Option<Landed> {
        if landing.written < landing.wanted / 2 || landing.written == 0 {
            return None;
        }
        let elsewhere = landing.volumes.iter().filter(|volume| !volume.written_to).find_map(|volume| {
            let lost = volume.before?.checked_sub(volume.after_write?)?;
            (lost >= landing.written / 2).then(|| (volume.letter.clone(), volume.after_write))
        });
        Some(Landed { written: landing.written, elsewhere })
    }
}

/// How the folder's files stand against what Nexon published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Files {
    NotCompared,
    /// The folder is at this state. `behind` says that the server has a newer minor of the
    /// same version, which the folder takes first. `unread` is how many of its files were
    /// not read, and are taken for right by their length or by their being there.
    At {
        label: String,
        behind: bool,
        unread: usize,
    },
    /// It could be at either of these.
    Either {
        labels: Vec<String>,
    },
    /// It fits no state, and every state it could be at is known. Against the nearest one,
    /// this many files are something else than they should be, and this many are not there.
    Differs {
        label: String,
        differs: usize,
        missing: usize,
    },
    /// It fits no state that is known, and it may be at one that is not: the server has a
    /// version newer than the table built into this program (`newer` names it), or the
    /// server could not be asked which version is the newest (`newer` is `None`). The
    /// counts are against the nearest known state, as for [`Files::Differs`].
    Unsettled {
        label: String,
        differs: usize,
        missing: usize,
        newer: Option<String>,
    },
}

/// The facts the verdict is worked out from.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    pub install_found: bool,
    pub system: Option<Drive>,
    pub game: Option<Drive>,
    /// The game is on the drive Windows is on.
    pub same_drive: bool,
    /// Names of programs that matter, already fit to show.
    pub programs: Programs,
    pub write_test: WriteTest,
    pub landed: Option<Landed>,
    /// How many times faster the same part of a large file was read the second time, with
    /// both reads going past Windows' own file cache. `None` when it was not timed.
    pub reread: Option<f64>,
    pub files: Files,
}

/// A second read this many times faster than the first means something below Windows kept
/// the bytes. A disk of the PC itself gives about one; this is far enough from that to be
/// no accident of timing.
pub const KEPT_BELOW: f64 = 3.0;

impl Evidence {
    /// The evidence in what was found. Names of programs go through `scrub` here, since the
    /// verdict quotes them.
    pub fn of(facts: &Facts, scrub: &Scrub) -> Evidence {
        let shown = |names: &[String]| names.iter().map(|name| scrub.text(name)).collect();
        let found = facts.programs.clone().unwrap_or_default();
        let programs = Programs {
            diskless: shown(&found.diskless),
            restore: shown(&found.restore),
            seat: shown(&found.seat),
            hints: shown(&found.hints),
            noted: shown(&found.noted),
            game: shown(&found.game),
        };
        let (system, game) = (facts.system_drive.as_ref().ok(), facts.game_drive.as_ref().ok());
        let client = facts.client.client();
        // The full comparison says more than the quick one, where there is one.
        let compared = match &facts.full {
            crate::facts::Asked::Done(full) if !full.cut_short => Some(full),
            _ => facts.quick.as_ref().ok(),
        };
        let newest = facts.shelf.as_ref().ok().map(|shelf| shelf.newest);
        let behind =
            |at: &State| newest.is_some_and(|newest| newest.version == at.version && newest.order() > at.order());
        let files = match compared {
            None => Files::NotCompared,
            Some(compared) => match (&compared.at, &compared.either[..], &compared.against) {
                (Some(at), _, _) => Files::At {
                    label: at.label(client),
                    behind: behind(at),
                    unread: compared.tally.same_length + compared.tally.unread,
                },
                (None, [_, ..], _) => {
                    Files::Either { labels: compared.either.iter().map(|state| state.label(client)).collect() }
                }
                (None, [], Some((nearest, _))) => {
                    let (differs, missing) = (compared.tally.differs.len(), compared.tally.missing.len());
                    let label = nearest.label(client);
                    // The table knows what the files are up to its newest state. A patch on
                    // the server only says what it starts from, so nothing knows the files
                    // of a version that came after the table was made.
                    let known_up_to = facts.tables[usize::from(facts.client == mfp_kms::Server::Test)];
                    match newest {
                        Some(newest) if newest.order() <= known_up_to.order() => {
                            Files::Differs { label, differs, missing }
                        }
                        Some(newest) => Files::Unsettled { label, differs, missing, newer: Some(newest.label(client)) },
                        None => Files::Unsettled { label, differs, missing, newer: None },
                    }
                }
                (None, [], None) => Files::NotCompared,
            },
        };
        Evidence {
            install_found: facts.install.is_some(),
            same_drive: matches!((system, game), (Some(system), Some(game)) if system.letter == game.letter && !game.letter.is_empty()),
            system: system.map(Drive::of),
            game: game.map(Drive::of),
            programs,
            write_test: facts.write_test,
            landed: match &facts.landing {
                crate::facts::Asked::Done(landing) => Landed::of(landing),
                _ => None,
            },
            reread: facts.reads.as_ref().ok().filter(|reads| reads.first > 0.0).map(|reads| reads.again / reads.first),
            files,
        }
    }
}

/// The kinds of PC the verdict tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A PC like the one at home: its own disk, and nothing that puts it back.
    Ordinary,
    /// A seat with a disk of its own and a program that puts the disk back at every start.
    DiskWithRestore,
    /// A seat without a disk of its own.
    Diskless,
    /// The facts do not decide.
    Unknown,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ordinary => "일반 PC로 보임",
            Kind::DiskWithRestore => "디스크가 있고 복구 프로그램이 도는 PC로 보임",
            Kind::Diskless => "노하드 좌석으로 보임",
            Kind::Unknown => "판단 불가",
        }
    }
}

/// What is expected of something that has not been tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outlook {
    Expected,
    Unlikely,
    /// The facts do not decide.
    Unknown,
}

/// The verdict: each answer with the facts it rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub kind: Kind,
    pub kind_facts: Vec<String>,
    /// Whether a main patch is expected to go through here.
    pub patch: Outlook,
    pub patch_facts: Vec<String>,
    /// Whether what the patch wrote is expected to be there after a restart.
    pub kept: Outlook,
    pub kept_facts: Vec<String>,
}

impl Verdict {
    /// What is said of the patch.
    pub fn patch_label(&self) -> &'static str {
        match self.patch {
            Outlook::Expected => "가능 예상",
            Outlook::Unlikely => "어려움",
            Outlook::Unknown => "판단 불가",
        }
    }

    /// What is said of a restart.
    pub fn kept_label(&self) -> &'static str {
        match self.kept {
            Outlook::Expected => "유지 예상",
            Outlook::Unlikely => "유지 안 됨 예상",
            Outlook::Unknown => "판단 불가",
        }
    }
}

/// Works out the verdict. Changes nothing and asks nothing: the same evidence gives the same
/// verdict.
pub fn verdict(evidence: &Evidence) -> Verdict {
    let (kind, kind_facts) = kind_of(evidence);
    let (patch, patch_facts) = patch_outlook(evidence, kind);
    let (kept, kept_facts) = kept_outlook(evidence, kind);
    Verdict { kind, kind_facts, patch, patch_facts, kept, kept_facts }
}

/// The drives of the evidence with what the report calls each: the two under one name where
/// the game is on the system drive.
fn drives(evidence: &Evidence) -> Vec<(&'static str, &Drive)> {
    match (&evidence.system, &evidence.game) {
        (Some(system), Some(_)) if evidence.same_drive => vec![("시스템·게임 드라이브", system)],
        (system, game) => system
            .iter()
            .map(|drive| ("시스템 드라이브", drive))
            .chain(game.iter().map(|drive| ("게임 드라이브", drive)))
            .collect(),
    }
}

fn named(role: &str, drive: &Drive) -> String {
    if drive.letter.is_empty() { role.to_owned() } else { format!("{role} {}", drive.letter) }
}

fn kind_of(evidence: &Evidence) -> (Kind, Vec<String>) {
    let programs = &evidence.programs;
    let drives = drives(evidence);
    let those = |attached: Attached| drives.iter().filter(move |(_, drive)| drive.attached == attached);
    let stated = |attached: Attached, what: &str| -> Vec<String> {
        those(attached).map(|(role, drive)| format!("{} {} {what}", named(role, drive), drive.how)).collect()
    };

    // 1. A program that serves seats without a disk is running.
    if !programs.diskless.is_empty() {
        let mut facts = vec![format!("노하드 프로그램 실행 중: {}", programs.diskless.join(", "))];
        facts.extend(stated(Attached::Network, "연결"));
        return (Kind::Diskless, facts);
    }
    // 2. Windows or the game is on a disk that comes over the network.
    if those(Attached::Network).next().is_some() {
        return (Kind::Diskless, stated(Attached::Network, "연결(네트워크 디스크)"));
    }
    let real = |drive: &Option<Drive>| drive.as_ref().is_some_and(|drive| drive.attached == Attached::Disk);
    let both_real = evidence.install_found && real(&evidence.game) && real(&evidence.system);
    // 3. Real disks, and a program that puts a disk back at every start.
    if both_real && !programs.restore.is_empty() {
        let mut facts = stated(Attached::Disk, "(실제 디스크)");
        facts.push(format!("복구 프로그램 실행 중: {}", programs.restore.join(", ")));
        return (Kind::DiskWithRestore, facts);
    }
    // 4. What was written to the game's drive took room on another one.
    if let Some((letter, _)) = evidence.landed.as_ref().and_then(|landed| landed.elsewhere.as_ref()) {
        let written = evidence.landed.as_ref().map_or(0, |landed| landed.written);
        return (Kind::Diskless, vec![format!("게임 폴더에 쓴 {}만큼 {letter} 여유가 줄어듦", size(written))]);
    }
    // 5. A made-up disk, or a disk with something below Windows that keeps what was read,
    // on a PC that a shop's programs run on.
    let shop: Vec<String> = programs.seat.iter().chain(&programs.restore).cloned().collect();
    let kept_below = evidence
        .reread
        .filter(|times| *times >= KEPT_BELOW)
        .map(|times| format!("같은 파일을 다시 읽으면 {times:.0}배 빠름(Windows 아래에서 읽은 내용을 보관함)"));
    if (those(Attached::Virtual).next().is_some() || kept_below.is_some()) && !shop.is_empty() {
        let mut facts = stated(Attached::Virtual, "디스크");
        facts.extend(kept_below);
        facts.push(format!("매장 프로그램 실행 중: {}", shop.join(", ")));
        return (Kind::Diskless, facts);
    }
    // 6. Real disks and nothing of a shop.
    if both_real && shop.is_empty() && programs.hints.is_empty() && kept_below.is_none() {
        let mut facts = stated(Attached::Disk, "(실제 디스크)");
        facts.push("매장 프로그램 없음".to_owned());
        return (Kind::Ordinary, facts);
    }
    // 7. The facts do not decide. What is in the way is said.
    let mut facts = Vec::new();
    if !evidence.install_found {
        facts.push("설치 폴더를 찾지 못함".to_owned());
    } else if evidence.game.is_none() {
        facts.push("게임 드라이브 확인 불가".to_owned());
    }
    if evidence.system.is_none() {
        facts.push("시스템 드라이브 확인 불가".to_owned());
    }
    facts.extend(stated(Attached::Virtual, "디스크(노하드 좌석인지 직접 만든 가상 디스크인지 구별 안 됨)"));
    facts.extend(stated(Attached::Share, "공유 폴더"));
    facts.extend(kept_below);
    facts.extend(stated(Attached::Unclear, "연결(실제 디스크인지 구별 안 됨)"));
    if both_real {
        facts.extend(stated(Attached::Disk, "(실제 디스크)"));
    }
    if !programs.seat.is_empty() {
        facts.push(format!("PC방 관리 프로그램 실행 중: {} · 복구 프로그램은 확인 안 됨", programs.seat.join(", ")));
    }
    if !programs.restore.is_empty() {
        facts.push(format!("복구 프로그램 실행 중: {}", programs.restore.join(", ")));
    }
    if !programs.hints.is_empty() {
        facts.push(format!("매장 프로그램일 수 있는 이름: {}", programs.hints.join(", ")));
    }
    (Kind::Unknown, facts)
}

fn patch_outlook(evidence: &Evidence, kind: Kind) -> (Outlook, Vec<String>) {
    let needed = format!("필요 약 {}", gb(ROOM_NEEDED));
    if !evidence.install_found {
        return (Outlook::Unknown, vec!["설치 폴더를 찾지 못함".to_owned()]);
    }
    // What stops a patch whatever the drive is.
    let mut stops = Vec::new();
    match evidence.write_test {
        WriteTest::Failed(Some(code)) => stops.push(format!("설치 폴더에 쓸 수 없음(오류 {code})")),
        WriteTest::Failed(None) => stops.push("설치 폴더에 쓸 수 없음".to_owned()),
        WriteTest::Passed | WriteTest::LeftBehind(_) | WriteTest::Skipped => {}
    }
    if evidence.game.as_ref().is_some_and(|game| game.read_only) {
        stops.push("게임 드라이브가 읽기 전용".to_owned());
    }
    if let Files::Differs { label, differs, missing } = &evidence.files {
        stops.push(format!("{label} 기준 다른 파일 {differs}개 · 없는 파일 {missing}개(패치가 원본 검사에서 멈춤)"));
    }
    if !stops.is_empty() {
        return (Outlook::Unlikely, stops);
    }

    let mut facts = Vec::new();
    let outlook = if kind == Kind::Diskless {
        match evidence.landed.as_ref().and_then(|landed| landed.elsewhere.as_ref()) {
            // The room the writes take is on a drive whose free space can be read.
            Some((letter, Some(free))) if *free < ROOM_NEEDED => {
                facts.push(format!("쓰기가 {letter}에 쌓임 · {letter} 여유 {} < {needed}", gb(*free)));
                Outlook::Unlikely
            }
            Some((letter, free)) => {
                let free = free.map_or("확인 불가".to_owned(), gb);
                facts.push(format!("쓰기가 {letter}에 쌓임 · {letter} 여유 {free} · {needed}"));
                facts.push("좌석의 실제 쓰기 한도는 좌석에서 보이지 않음".to_owned());
                Outlook::Unknown
            }
            None => {
                facts.push("좌석의 쓰기 한도가 좌석에서 보이지 않음(한도를 넘으면 좌석이 재부팅될 수 있음)".to_owned());
                Outlook::Unknown
            }
        }
    } else {
        match &evidence.game {
            None => {
                facts.push("게임 드라이브 확인 불가".to_owned());
                Outlook::Unknown
            }
            Some(Drive { free: None, .. }) => {
                facts.push("게임 드라이브 여유 공간 확인 불가".to_owned());
                Outlook::Unknown
            }
            Some(Drive { free: Some(free), .. }) if *free < ROOM_NEEDED => {
                facts.push(format!("게임 드라이브 여유 {} < {needed}", gb(*free)));
                Outlook::Unlikely
            }
            Some(Drive { free: Some(free), attached, how, .. }) => {
                let room = format!("게임 드라이브 여유 {} ≥ {needed}", gb(*free));
                match (attached, evidence.write_test) {
                    (Attached::Disk, WriteTest::Passed | WriteTest::LeftBehind(_)) => {
                        facts.push(format!("게임 드라이브 {how}(실제 디스크)"));
                        facts.push(room);
                        facts.push("쓰기 시험 성공".to_owned());
                        if kind == Kind::DiskWithRestore {
                            facts.push("복구 프로그램이 바뀐 내용을 얼마나 받아 주는지는 확인 불가".to_owned());
                        }
                        Outlook::Expected
                    }
                    (Attached::Disk, _) => {
                        facts.push(room);
                        facts.push("쓰기 시험을 하지 않음".to_owned());
                        Outlook::Unknown
                    }
                    _ => {
                        facts.push(room);
                        facts.push(
                            "게임 드라이브가 실제 디스크인지 알 수 없어 여유 공간이 쓰기 한도인지 확인 불가".to_owned(),
                        );
                        Outlook::Unknown
                    }
                }
            }
        }
    };
    match &evidence.files {
        Files::NotCompared => facts.push("파일 대조 안 함".to_owned()),
        Files::At { label, behind, unread } => {
            let mut fact = format!("파일이 {label}와 일치");
            if *unread > 0 {
                fact.push_str(&format!("(큰 파일 {unread}개는 내용을 읽지 않음)"));
            }
            if *behind {
                fact.push_str(" · 마이너 패치부터 필요");
            }
            facts.push(fact);
        }
        Files::Either { labels } => facts.push(format!("파일이 {} 중 하나와 일치", labels.join(", "))),
        Files::Differs { .. } => {}
        Files::Unsettled { label, differs, missing, newer } => {
            let counted = format!("{label} 기준 다른 파일 {differs}개 · 없는 파일 {missing}개");
            facts.push(match newer {
                Some(newer) => {
                    format!("서버 최신 {newer}의 파일은 이 도구의 표에 없음 · {counted}(새 버전이어서일 수 있음)")
                }
                None => format!("{counted} · 서버 최신 버전을 확인하지 못해 새 버전이어서인지 판단 불가"),
            });
            // Files that may or may not be Nexon's are no ground to expect a patch to go through.
            return (if outlook == Outlook::Expected { Outlook::Unknown } else { outlook }, facts);
        }
    }
    (outlook, facts)
}

fn kept_outlook(evidence: &Evidence, kind: Kind) -> (Outlook, Vec<String>) {
    let programs = &evidence.programs;
    match kind {
        Kind::Diskless => {
            (Outlook::Unlikely, vec!["노하드 좌석은 재부팅·로그아웃 때 좌석에서 쓴 내용을 지움".to_owned()])
        }
        Kind::DiskWithRestore => (
            Outlook::Unknown,
            vec![
                format!("복구 프로그램 실행 중: {}", programs.restore.join(", ")),
                "게임 드라이브가 복구 대상에서 빠져 있는지는 좌석에서 보이지 않음".to_owned(),
            ],
        ),
        Kind::Ordinary => (Outlook::Expected, vec!["복구 프로그램·노하드 흔적 없음".to_owned()]),
        Kind::Unknown => (Outlook::Unknown, vec!["PC 종류를 판단하지 못함".to_owned()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn drive(letter: &str, attached: Attached, how: &str, free_gib: u64) -> Drive {
        Drive { letter: letter.to_owned(), attached, how: how.to_owned(), free: Some(free_gib * GIB), read_only: false }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// A PC at home: Windows and the game on one NVMe disk with room, nothing of a shop
    /// running, the folder writable, and its files those of the newest version.
    fn home() -> Evidence {
        Evidence {
            install_found: true,
            system: Some(drive("C:", Attached::Disk, "NVMe", 200)),
            game: Some(drive("C:", Attached::Disk, "NVMe", 200)),
            same_drive: true,
            programs: Programs::default(),
            write_test: WriteTest::Passed,
            landed: None,
            reread: Some(0.96),
            files: Files::At { label: "1.2.421 (4)".to_owned(), behind: false, unread: 0 },
        }
    }

    /// A seat whose game disk comes over the network, next to a system disk of its own.
    fn seat_with_network_game_disk() -> Evidence {
        Evidence { game: Some(drive("D:", Attached::Network, "iSCSI", 855)), same_drive: false, ..home() }
    }

    #[test]
    fn home_pc_is_ordinary_takes_the_patch_and_keeps_it() {
        let verdict = verdict(&home());
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Expected, Outlook::Expected));
        assert_eq!(verdict.kind_facts, ["시스템·게임 드라이브 C: NVMe (실제 디스크)", "매장 프로그램 없음"]);
        assert!(verdict.patch_facts.contains(&"게임 드라이브 여유 200.0 GB ≥ 필요 약 36.5 GB".to_owned()));
        assert!(verdict.patch_facts.contains(&"파일이 1.2.421 (4)와 일치".to_owned()));
        // The game on a second real disk changes nothing.
        let two = Evidence { game: Some(drive("D:", Attached::Disk, "SATA", 100)), same_drive: false, ..home() };
        let verdict = super::verdict(&two);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Expected, Outlook::Expected));
        assert_eq!(
            verdict.kind_facts[..2],
            ["시스템 드라이브 C: NVMe (실제 디스크)", "게임 드라이브 D: SATA (실제 디스크)"]
        );
    }

    #[test]
    fn disk_that_comes_over_the_network_makes_a_diskless_seat() {
        for evidence in [
            seat_with_network_game_disk(),
            // Windows itself from the network, and both on one such disk.
            Evidence { system: Some(drive("C:", Attached::Network, "iSCSI", 30)), same_drive: false, ..home() },
            Evidence {
                system: Some(drive("C:", Attached::Network, "iSCSI", 30)),
                game: Some(drive("C:", Attached::Network, "iSCSI", 30)),
                ..home()
            },
        ] {
            let verdict = verdict(&evidence);
            assert_eq!(verdict.kind, Kind::Diskless, "{evidence:?}");
            // How much such a seat takes cannot be seen from the seat, whatever is free.
            assert_eq!(verdict.patch, Outlook::Unknown, "{evidence:?}");
            assert_eq!(verdict.kept, Outlook::Unlikely, "{evidence:?}");
        }
        let verdict = verdict(&seat_with_network_game_disk());
        assert_eq!(verdict.kind_facts, ["게임 드라이브 D: iSCSI 연결(네트워크 디스크)"]);
        assert!(verdict.patch_facts[0].contains("쓰기 한도"), "{:?}", verdict.patch_facts);
    }

    #[test]
    fn known_diskless_program_makes_a_diskless_seat_whatever_the_disks_claim() {
        let programs = Programs { diskless: names(&["PFGameDisk.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely));
        assert_eq!(verdict.kind_facts, ["노하드 프로그램 실행 중: PFGameDisk.exe"]);
    }

    #[test]
    fn real_disk_with_a_restore_program_takes_the_patch_and_may_lose_it() {
        let programs =
            Programs { restore: names(&["DFServ.exe"]), seat: names(&["pmclient.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!(verdict.kind, Kind::DiskWithRestore);
        assert_eq!(verdict.patch, Outlook::Expected);
        assert_eq!(verdict.kept, Outlook::Unknown);
        assert_eq!(verdict.kept_facts[0], "복구 프로그램 실행 중: DFServ.exe");
        assert!(verdict.patch_facts.iter().any(|fact| fact.contains("복구 프로그램이 바뀐 내용")));
    }

    #[test]
    fn made_up_disk_is_a_seat_only_with_a_shops_programs_beside_it() {
        let on_virtual =
            Evidence { game: Some(drive("D:", Attached::Virtual, "가상", 500)), same_drive: false, ..home() };
        // At home it may be a virtual disk somebody mounted: the facts do not decide.
        let alone = verdict(&on_virtual);
        assert_eq!((alone.kind, alone.patch, alone.kept), (Kind::Unknown, Outlook::Unknown, Outlook::Unknown));
        assert!(alone.kind_facts[0].starts_with("게임 드라이브 D: 가상 디스크"), "{:?}", alone.kind_facts);

        let programs = Programs { seat: names(&["pmclient.exe"]), ..Programs::default() };
        let in_a_shop = verdict(&Evidence { programs, ..on_virtual });
        assert_eq!((in_a_shop.kind, in_a_shop.kept), (Kind::Diskless, Outlook::Unlikely));
        assert_eq!(in_a_shop.kind_facts, ["게임 드라이브 D: 가상 디스크", "매장 프로그램 실행 중: pmclient.exe"]);
    }

    #[test]
    fn second_read_that_is_far_faster_than_the_first_keeps_a_pc_from_being_called_ordinary() {
        // A disk that names itself like a real one, and reads again 23 times as fast: what
        // a seat does that keeps what it fetched from its server in memory.
        let cached = Evidence { reread: Some(23.2), ..home() };
        let alone = verdict(&cached);
        assert_eq!((alone.kind, alone.patch, alone.kept), (Kind::Unknown, Outlook::Expected, Outlook::Unknown));
        assert!(
            alone
                .kind_facts
                .contains(&"같은 파일을 다시 읽으면 23배 빠름(Windows 아래에서 읽은 내용을 보관함)".to_owned()),
            "{:?}",
            alone.kind_facts
        );
        // With a PC room's program beside it, it is a seat without a disk.
        let programs = Programs { seat: names(&["pmclient.exe"]), ..Programs::default() };
        let in_a_shop = verdict(&Evidence { programs, ..cached });
        assert_eq!(
            (in_a_shop.kind, in_a_shop.patch, in_a_shop.kept),
            (Kind::Diskless, Outlook::Unknown, Outlook::Unlikely)
        );
        // About as fast, a little faster, and not timed at all: nothing is made of it.
        for reread in [Some(1.0), Some(2.9), None] {
            assert_eq!(verdict(&Evidence { reread, ..home() }).kind, Kind::Ordinary, "{reread:?}");
        }
    }

    #[test]
    fn shop_program_on_real_disks_leaves_the_kind_open_and_the_patch_possible() {
        // A seat with a disk: the program that manages the seat runs, and no restore
        // program this tool knows by name. One may run all the same.
        let programs = Programs { seat: names(&["pmclient.exe", "pmAgent.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Unknown, Outlook::Expected, Outlook::Unknown));
        assert!(
            verdict
                .kind_facts
                .iter()
                .any(|fact| fact.starts_with("PC방 관리 프로그램 실행 중: pmclient.exe, pmAgent.exe"))
        );
        assert!(verdict.kind_facts.contains(&"시스템·게임 드라이브 C: NVMe (실제 디스크)".to_owned()));
    }

    #[test]
    fn name_that_only_hints_at_a_shop_keeps_a_pc_from_being_called_ordinary() {
        let programs = Programs { hints: names(&["HDRestore.exe"]), ..Programs::default() };
        let verdict = verdict(&Evidence { programs, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Unknown, Outlook::Expected, Outlook::Unknown));
        assert!(verdict.kind_facts.contains(&"매장 프로그램일 수 있는 이름: HDRestore.exe".to_owned()));
        // A name that is known to be something else is no hint.
        let programs = Programs { noted: names(&["wireguard.exe", "ahcache"]), ..Programs::default() };
        assert_eq!(super::verdict(&Evidence { programs, ..home() }).kind, Kind::Ordinary);
    }

    #[test]
    fn drive_that_could_be_either_leaves_everything_open() {
        for unclear in [
            drive("D:", Attached::Unclear, "SCSI", 500),
            drive("", Attached::Share, "네트워크", 500),
            drive("R:", Attached::Unclear, "램 디스크", 500),
        ] {
            let verdict = verdict(&Evidence { game: Some(unclear.clone()), same_drive: false, ..home() });
            assert_eq!(
                (verdict.kind, verdict.patch, verdict.kept),
                (Kind::Unknown, Outlook::Unknown, Outlook::Unknown),
                "{unclear:?}"
            );
        }
        // Nor is anything said of a drive that could not be looked at.
        let verdict = verdict(&Evidence { game: None, ..home() });
        assert_eq!((verdict.kind, verdict.patch), (Kind::Unknown, Outlook::Unknown));
        assert_eq!(verdict.kind_facts, ["게임 드라이브 확인 불가"]);
    }

    #[test]
    fn nothing_is_said_of_a_patch_without_an_install_folder() {
        let verdict = verdict(&Evidence { install_found: false, game: None, files: Files::NotCompared, ..home() });
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Unknown, Outlook::Unknown, Outlook::Unknown));
        assert_eq!(verdict.patch_facts, ["설치 폴더를 찾지 못함"]);
        // A system disk from the network is a seat without a disk all the same.
        let seat = Evidence {
            install_found: false,
            game: None,
            system: Some(drive("C:", Attached::Network, "iSCSI", 30)),
            ..home()
        };
        assert_eq!(super::verdict(&seat).kind, Kind::Diskless);
    }

    #[test]
    fn too_little_room_makes_the_patch_unlikely() {
        let tight = Evidence { game: Some(drive("C:", Attached::Disk, "NVMe", 36)), ..home() };
        let verdict = verdict(&tight);
        assert_eq!((verdict.kind, verdict.patch, verdict.kept), (Kind::Ordinary, Outlook::Unlikely, Outlook::Expected));
        assert_eq!(verdict.patch_facts[0], "게임 드라이브 여유 36.0 GB < 필요 약 36.5 GB");
        // Exactly enough is enough.
        let enough = Drive { free: Some(ROOM_NEEDED), ..drive("C:", Attached::Disk, "NVMe", 0) };
        assert_eq!(super::verdict(&Evidence { game: Some(enough), ..home() }).patch, Outlook::Expected);
        // Room that cannot be read decides nothing.
        let unread = Drive { free: None, ..drive("C:", Attached::Disk, "NVMe", 0) };
        assert_eq!(super::verdict(&Evidence { game: Some(unread), ..home() }).patch, Outlook::Unknown);
    }

    #[test]
    fn folder_that_cannot_be_written_or_differs_from_nexons_files_stops_a_patch_anywhere() {
        let refused = verdict(&Evidence { write_test: WriteTest::Failed(Some(5)), ..home() });
        assert_eq!(refused.patch, Outlook::Unlikely);
        assert_eq!(refused.patch_facts, ["설치 폴더에 쓸 수 없음(오류 5)"]);

        let read_only = Drive { read_only: true, ..drive("C:", Attached::Disk, "NVMe", 200) };
        assert_eq!(verdict(&Evidence { game: Some(read_only), ..home() }).patch, Outlook::Unlikely);

        let files = Files::Differs { label: "1.2.421 (4)".to_owned(), differs: 3, missing: 1 };
        let differs = verdict(&Evidence { files: files.clone(), ..home() });
        assert_eq!((differs.kind, differs.patch, differs.kept), (Kind::Ordinary, Outlook::Unlikely, Outlook::Expected));
        assert_eq!(differs.patch_facts, ["1.2.421 (4) 기준 다른 파일 3개 · 없는 파일 1개(패치가 원본 검사에서 멈춤)"]);
        // On a seat without a disk too: this is known, where the write limit is not.
        let seat = verdict(&Evidence { files, ..seat_with_network_game_disk() });
        assert_eq!((seat.kind, seat.patch), (Kind::Diskless, Outlook::Unlikely));
    }

    #[test]
    fn folder_that_may_be_at_a_version_nothing_knows_leaves_the_patch_open() {
        let unsettled = |newer: Option<&str>| Files::Unsettled {
            label: "1.2.421 (4)".to_owned(),
            differs: 9,
            missing: 0,
            newer: newer.map(str::to_owned),
        };
        // The server has a minor the table was made before.
        let newer = verdict(&Evidence { files: unsettled(Some("1.2.421 (5)")), ..home() });
        assert_eq!((newer.kind, newer.patch, newer.kept), (Kind::Ordinary, Outlook::Unknown, Outlook::Expected));
        assert_eq!(
            newer.patch_facts.last().unwrap(),
            "서버 최신 1.2.421 (5)의 파일은 이 도구의 표에 없음 · 1.2.421 (4) 기준 다른 파일 9개 · 없는 파일 0개(새 버전이어서일 수 있음)"
        );
        // The server could not be asked.
        let unasked = verdict(&Evidence { files: unsettled(None), ..home() });
        assert_eq!(unasked.patch, Outlook::Unknown);
        assert!(
            unasked.patch_facts.last().unwrap().ends_with("서버 최신 버전을 확인하지 못해 새 버전이어서인지 판단 불가")
        );
        // What was unlikely for another reason stays unlikely.
        let tight = Evidence { game: Some(drive("C:", Attached::Disk, "NVMe", 10)), files: unsettled(None), ..home() };
        assert_eq!(verdict(&tight).patch, Outlook::Unlikely);
    }

    #[test]
    fn write_test_that_was_left_out_leaves_the_patch_open() {
        let verdict = verdict(&Evidence { write_test: WriteTest::Skipped, ..home() });
        assert_eq!((verdict.kind, verdict.patch), (Kind::Ordinary, Outlook::Unknown));
        assert!(verdict.patch_facts.contains(&"쓰기 시험을 하지 않음".to_owned()));
    }

    #[test]
    fn what_the_files_are_is_said_and_only_a_mismatch_decides() {
        let said = |files: Files| verdict(&Evidence { files, ..home() });
        let behind = said(Files::At { label: "1.2.421 (2)".to_owned(), behind: true, unread: 0 });
        assert_eq!(behind.patch, Outlook::Expected);
        assert!(behind.patch_facts.contains(&"파일이 1.2.421 (2)와 일치 · 마이너 패치부터 필요".to_owned()));
        let either = said(Files::Either { labels: names(&["1.2.421 (3)", "1.2.421 (4)"]) });
        assert_eq!(either.patch, Outlook::Expected);
        assert!(either.patch_facts.contains(&"파일이 1.2.421 (3), 1.2.421 (4) 중 하나와 일치".to_owned()));
        // The quick comparison goes by length for the large files, and that is said.
        let quick = said(Files::At { label: "1.2.421 (4)".to_owned(), behind: false, unread: 248 });
        assert_eq!(quick.patch, Outlook::Expected);
        assert!(quick.patch_facts.contains(&"파일이 1.2.421 (4)와 일치(큰 파일 248개는 내용을 읽지 않음)".to_owned()));
        let unread = said(Files::NotCompared);
        assert_eq!(unread.patch, Outlook::Expected);
        assert!(unread.patch_facts.contains(&"파일 대조 안 함".to_owned()));
    }

    #[test]
    fn writes_that_land_on_another_drive_make_a_seat_and_that_drives_room_the_limit() {
        // 512 MB went to D: and C: lost them: C: has 31 GB left.
        let landed = |free_gib: u64| {
            Some(Landed { written: 512 << 20, elsewhere: Some(("C:".to_owned(), Some(free_gib * GIB))) })
        };
        let two_disks = Evidence { game: Some(drive("D:", Attached::Disk, "SATA", 855)), same_drive: false, ..home() };
        let tight = verdict(&Evidence { landed: landed(31), ..two_disks.clone() });
        assert_eq!((tight.kind, tight.patch, tight.kept), (Kind::Diskless, Outlook::Unlikely, Outlook::Unlikely));
        assert_eq!(tight.kind_facts, ["게임 폴더에 쓴 512 MB만큼 C: 여유가 줄어듦"]);
        assert_eq!(tight.patch_facts[0], "쓰기가 C:에 쌓임 · C: 여유 31.0 GB < 필요 약 36.5 GB");
        // With room there, the seat's own limit is still not known.
        let roomy = verdict(&Evidence { landed: landed(120), ..two_disks.clone() });
        assert_eq!((roomy.kind, roomy.patch), (Kind::Diskless, Outlook::Unknown));
        // A probe whose writes stayed where they were written changes nothing.
        let stayed = Some(Landed { written: 512 << 20, elsewhere: None });
        assert_eq!(verdict(&Evidence { landed: stayed, ..two_disks }).kind, Kind::Ordinary);
    }

    #[test]
    fn where_a_probe_landed_is_read_from_what_was_free() {
        use crate::facts::Moved;
        let moved = |letter: &str, written_to: bool, before: u64, after: u64| Moved {
            letter: letter.to_owned(),
            written_to,
            before: Some(before),
            after_write: Some(after),
            after_delete: Some(before),
        };
        let probe = |volumes: Vec<Moved>, written: u64| Landing {
            written,
            wanted: 512 << 20,
            seconds: 1.0,
            error: None,
            volumes,
            memory: [None; 3],
            deleted: true,
        };
        let mib = 1u64 << 20;
        // Written to D:, and D: lost it: nothing landed elsewhere.
        let stayed = probe(
            vec![moved("C:", false, 5000 * mib, 4990 * mib), moved("D:", true, 9000 * mib, 8488 * mib)],
            512 * mib,
        );
        assert_eq!(Landed::of(&stayed), Some(Landed { written: 512 * mib, elsewhere: None }));
        // Written to D:, and C: lost it.
        let moved_away = probe(
            vec![moved("C:", false, 5000 * mib, 4488 * mib), moved("D:", true, 9000 * mib, 9000 * mib)],
            512 * mib,
        );
        assert_eq!(Landed::of(&moved_away).unwrap().elsewhere, Some(("C:".to_owned(), Some(4488 * mib))));
        // A volume that gained room, or whose room could not be read, took nothing.
        let mut odd = probe(vec![moved("C:", false, 5000 * mib, 5100 * mib)], 512 * mib);
        odd.volumes.push(Moved {
            letter: "E:".to_owned(),
            written_to: false,
            before: None,
            after_write: Some(1),
            after_delete: None,
        });
        assert_eq!(Landed::of(&odd).unwrap().elsewhere, None);
        // A probe that wrote next to nothing says nothing.
        assert_eq!(Landed::of(&probe(Vec::new(), 10 * mib)), None);
    }

    #[test]
    fn drives_are_sorted_by_how_they_are_attached() {
        let fixed = |bus: u32, model: &str| attached(DriveKind::Fixed, Some(Bus(bus)), model);
        assert_eq!(fixed(17, "SAMSUNG MZVL21T0HCLR-00BL2"), Attached::Disk);
        assert_eq!(fixed(11, "WDC WD10EZEX-08WN4A0"), Attached::Disk);
        assert_eq!(fixed(8, "NVMe Samsung SSD 980"), Attached::Disk);
        assert_eq!(attached(DriveKind::Removable, Some(Bus::USB), "Samsung PSSD T7"), Attached::Disk);
        assert_eq!(fixed(9, "MSFT Virtual HD"), Attached::Network);
        assert_eq!(fixed(14, ""), Attached::Virtual);
        assert_eq!(fixed(15, "Msft Virtual Disk"), Attached::Virtual);
        // A bus both kinds use, no bus at all, and a real bus under a name that gives the
        // disk away.
        assert_eq!(fixed(1, "Some Disk"), Attached::Unclear);
        assert_eq!(fixed(0, ""), Attached::Unclear);
        assert_eq!(attached(DriveKind::Fixed, None, "Some Disk"), Attached::Unclear);
        assert_eq!(fixed(11, "GManager VDisk"), Attached::Unclear);
        assert_eq!(fixed(10, "Msft Virtual Disk"), Attached::Unclear);
        // What is no disk of this PC at all.
        assert_eq!(attached(DriveKind::Remote, None, ""), Attached::Share);
        assert_eq!(attached(DriveKind::RamDisk, Some(Bus::SATA), ""), Attached::Unclear);
    }

    #[test]
    fn same_evidence_gives_the_same_verdict() {
        assert_eq!(verdict(&home()), verdict(&home()));
        assert_eq!(verdict(&seat_with_network_game_disk()), verdict(&seat_with_network_game_disk()));
    }
}
