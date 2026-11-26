//! How a run went, as figures and as the text a tester can copy and send.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mfp_engine::{Outcome, Stage, preflight};
use tauri::{AppHandle, State};

use crate::records::Left;
use crate::state::Patcher;
#[cfg(windows)]
use crate::wide::to_wide;
use crate::wording::{APP_NAME, drive_letter, size_label, speed, whole_gb};

/// How a patch went, for the text a tester can copy once it is done.
///
/// It is kept for the patch being applied in this session: a run that was paused or failed
/// and is taken up again adds to the same figures.
#[derive(Clone, Default)]
pub(crate) struct Measure {
    /// The patch these figures are of, as a run names it, and the install folder.
    pub(crate) id: String,
    pub(crate) install: PathBuf,
    /// What the patch is called in the measurement, and how big its file is.
    pub(crate) label: String,
    pub(crate) size: u64,
    /// How long each step was at work, in the order of [`STEPS`].
    pub(crate) steps: [Duration; 6],
    /// Bytes of the patch file that were downloaded during those steps.
    pub(crate) downloaded: u64,
    /// How many times the patch was paused, in this session and before it.
    pub(crate) pauses: u32,
    /// Game files the runs before the one at work changed for good, in this session and
    /// before it. A run that is cut off has changed some only if it replaces files as it
    /// builds them.
    pub(crate) files: usize,
    /// Part of the work was done before the app was started, and is not in the figures.
    pub(crate) carried: bool,
    /// What the engine found out about the game's drive; see [`mfp_engine::Event::Storage`].
    pub(crate) storage: Option<Storage>,
    /// Size of the game's drive and what was free there when the measuring began, in bytes.
    pub(crate) space: Option<(u64, u64)>,
}

impl Measure {
    /// Counts on from what an unfinished run of the patch left on record, when the measuring
    /// begins after it: the app was closed in between, or cut off. The record has the
    /// counts. How long each step took then, it does not have, and the measurement says so.
    pub(crate) fn take_up(&mut self, left: &Left) {
        self.carried = true;
        (self.files, self.pauses) = (left.files, left.pauses);
    }

    /// Adds a run that ended unfinished to the counts: the files it had put in place for good
    /// by then (`placed`), and the pause, if that is how it ended (`paused`).
    pub(crate) fn add_unfinished(&mut self, placed: usize, paused: bool) {
        self.files += placed;
        self.pauses += u32::from(paused);
    }

    /// Every file the patch changed, now that the run that finished it is done:
    /// `by_last_run` is what the engine says that run changed, the files it put in place
    /// and the files it removed. The runs before it only put files in place. So the sum is
    /// what one run of the whole patch reports, and the files the last run was told of one
    /// by one are in the engine's figure, not added a second time.
    pub(crate) fn files_in_all(&self, by_last_run: usize) -> usize {
        self.files + by_last_run
    }
}

/// What the engine found out about the drive the game is on, and how many threads it set
/// to work there.
#[derive(Clone)]
pub(crate) struct Storage {
    pub(crate) file_system: String,
    /// How long the drive took to get to a place on itself, when the engine tried that.
    pub(crate) seek_ms: Option<f64>,
    /// The engine takes it for a hard disk.
    pub(crate) slow: bool,
    pub(crate) readers: usize,
    pub(crate) checkers: usize,
}

/// The steps of a run in order, as the measurement names them.
pub(crate) const STEPS: [(Stage, &str); 6] = [
    (Stage::Download, "패치 파일 다운로드"),
    (Stage::VerifyPatch, "패치 파일 검사"),
    (Stage::VerifyInstall, "게임 파일 검사"),
    (Stage::Trial, "패치 사전 확인"),
    (Stage::Rebuild, "새 게임 파일 생성"),
    (Stage::Apply, "게임 파일 반영"),
];

/// Size of the drive `dir` is on, in bytes. What is free there the engine tells; how big the
/// drive is, it has no call for.
pub(crate) fn drive_size(dir: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let dir = to_wide(dir);
        let mut total = 0u64;
        // SAFETY: the path is a NUL-terminated buffer, and the one figure asked for is
        // written to a variable that outlives the call.
        let read = unsafe { GetDiskFreeSpaceExW(dir.as_ptr(), std::ptr::null_mut(), &mut total, std::ptr::null_mut()) };
        (read != 0).then_some(total)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        None
    }
}

/// The measurement of a finished run, as text to copy and send: how long each step took,
/// and what kind of PC and drive it ran on.
///
/// It is made to be passed on, so it names the game's drive by its letter and nothing else
/// of this PC: no folder, no user name, no computer name.
pub(crate) fn report_text(app: &AppHandle, measure: &Measure, seconds: u64, outcome: &Outcome) -> String {
    let took = if seconds >= 60 { format!("{}분 {}초", seconds / 60, seconds % 60) } else { format!("{seconds}초") };
    let mut lines = vec![
        format!("{APP_NAME} {} 측정 결과", app.package_info().version),
        format!("패치: {} ({}, 변경 파일 {}개)", measure.label, size_label(measure.size), outcome.files_changed),
        format!("소요 시간: {took}"),
    ];
    lines.extend(step_lines(measure));
    lines.extend(notes(measure, outcome.immediate));

    let machine = preflight::machine();
    lines.push(format!(
        "{} · {} ({}코어 {}스레드) · RAM {}",
        machine.os,
        machine.cpu,
        machine.cores,
        machine.threads,
        whole_gb(machine.ram_bytes)
    ));
    let storage = measure.storage.as_ref();
    let named: Vec<String> = [drive_letter(&measure.install), storage.map(|storage| storage.file_system.clone())]
        .into_iter()
        .flatten()
        .collect();
    // Whether it is a hard disk is the engine's guess from how long the drive took. Where
    // it did not try the drive, there is no guess to pass on.
    let seek = storage.and_then(|storage| {
        let kind = if storage.slow { "HDD" } else { "SSD" };
        Some(format!("탐색 {:.1} ms ({kind}로 추정)", storage.seek_ms?))
    });
    let drive: Vec<String> = [
        (!named.is_empty()).then(|| named.join(" ")),
        seek,
        measure.space.map(|(total, _)| format!("전체 {}", whole_gb(total))),
        measure.space.map(|(_, free)| format!("여유 {}", whole_gb(free))),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !drive.is_empty() {
        lines.push(format!("게임 드라이브: {}", drive.join(" · ")));
    }
    if let Some(storage) = storage {
        lines.push(format!("읽기 스레드 {} · 검사 스레드 {}", storage.readers, storage.checkers));
    }
    lines.join("\n")
}

/// The lines of a measurement that say how long each step took, over every run of the
/// patch in this session.
///
/// A step no run of the patch went through has no line: the download of a file that was
/// picked, or of one that was on this PC already when the first of these runs began. A
/// step that an earlier run went through keeps its time, whatever the last run did.
fn step_lines(measure: &Measure) -> Vec<String> {
    let went_through = STEPS.iter().zip(measure.steps).filter(|(_, took)| !took.is_zero());
    went_through
        .map(|((stage, name), took)| {
            let seconds = took.as_secs_f64();
            let speed = match measure.downloaded {
                bytes if *stage == Stage::Download && bytes > 0 => format!(" ({})", speed(bytes, seconds)),
                _ => String::new(),
            };
            format!("- {name}: {seconds:.1}초{speed}")
        })
        .collect()
}

/// What a measurement says about how the patch was run, on one line: how many times it was
/// paused, that part of it was done before the app was started, and that game files were
/// replaced as they were built (`immediate`). `None` for a patch run in one go the safe way.
fn notes(measure: &Measure, immediate: bool) -> Option<String> {
    let notes: Vec<String> = [
        (measure.pauses > 0).then(|| format!("일시정지 {}회", measure.pauses)),
        measure.carried.then(|| "이전 실행분 포함".to_owned()),
        immediate.then(|| "즉시 패치".to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!notes.is_empty()).then(|| notes.join(" · "))
}

/// How the last run that finished went, as text for the clipboard; see [`report_text`].
#[tauri::command]
pub(crate) fn run_report(patcher: State<Patcher>, server: String) -> Option<String> {
    patcher.site_named(&server)?.report.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an unfinished run with these counts leaves on record.
    fn record(files: usize, pauses: u32) -> Left {
        let active = Duration::from_secs(68);
        Left { patch: "00420to00421.patch".to_owned(), percent: 69.0, active, files, pauses }
    }

    /// Paused once, then the app was closed. Paused twice more after it was started again.
    #[test]
    fn pauses_are_counted_over_every_run_of_a_patch() {
        let mut first = Measure::default();
        first.add_unfinished(0, true);

        let mut measure = Measure::default();
        measure.take_up(&record(first.files, first.pauses));
        measure.add_unfinished(0, true);
        measure.add_unfinished(0, true);
        assert_eq!(notes(&measure, false).as_deref(), Some("일시정지 3회 · 이전 실행분 포함"));
    }

    /// A patch that puts 716 files in place and removes 2, of which one run reports 718.
    /// 470 files were in place when the app was cut off. The next run was paused with 120
    /// more in place, and the run that finished the patch put the last 126 in place and
    /// removed the 2.
    #[test]
    fn files_are_counted_over_every_run_of_a_patch() {
        let mut measure = Measure::default();
        measure.take_up(&record(470, 0));
        measure.add_unfinished(120, true);
        assert_eq!(measure.files_in_all(126 + 2), 718);
        // A patch that was never cut off is counted by its one run alone.
        assert_eq!(Measure::default().files_in_all(718), 718);
    }

    #[test]
    fn a_run_that_failed_or_was_turned_down_is_not_a_pause() {
        let mut measure = Measure::default();
        measure.add_unfinished(3, false);
        assert_eq!((measure.files, measure.pauses), (3, 0));
        assert_eq!(notes(&measure, false), None);
    }

    /// The times of the steps as a run adds them: the step at `at` of [`STEPS`] took
    /// `seconds`.
    fn took(measure: &mut Measure, at: usize, seconds: f64) {
        measure.steps[at] += Duration::from_secs_f64(seconds);
    }

    #[test]
    fn a_step_no_run_of_the_patch_went_through_has_no_line() {
        // A run of a patch file that was on this PC already: nothing was downloaded, and
        // the run adds no time for a download (see `went_through` in the patching).
        let mut measure = Measure::default();
        took(&mut measure, 1, 2.04);
        took(&mut measure, 2, 31.5);
        took(&mut measure, 4, 93.0);
        took(&mut measure, 5, 4.0);
        let lines = [
            "- 패치 파일 검사: 2.0초",
            "- 게임 파일 검사: 31.5초",
            "- 새 게임 파일 생성: 93.0초",
            "- 게임 파일 반영: 4.0초",
        ];
        assert_eq!(step_lines(&measure), lines);
        assert!(step_lines(&Measure::default()).is_empty());
    }

    /// The first run downloaded the patch file and was paused while it built. The run that
    /// finished the patch found the file here and fetched nothing.
    #[test]
    fn a_step_an_earlier_run_went_through_keeps_its_time() {
        let mut measure = Measure { downloaded: 1_288_490_189, ..Measure::default() };
        took(&mut measure, 0, 14.0);
        took(&mut measure, 1, 2.0);
        took(&mut measure, 4, 40.0);
        measure.add_unfinished(0, true);
        // The second run: the download adds nothing, the other steps add what they took.
        took(&mut measure, 1, 2.0);
        took(&mut measure, 4, 53.0);
        took(&mut measure, 5, 4.0);
        let lines = step_lines(&measure);
        assert_eq!(lines[0], "- 패치 파일 다운로드: 14.0초 (87.8 MB/s)");
        assert_eq!(lines[1..], ["- 패치 파일 검사: 4.0초", "- 새 게임 파일 생성: 93.0초", "- 게임 파일 반영: 4.0초"]);
    }

    #[test]
    fn a_patch_run_in_one_go_notes_only_the_way_it_was_applied() {
        assert_eq!(notes(&Measure::default(), false), None);
        assert_eq!(notes(&Measure::default(), true).as_deref(), Some("즉시 패치"));
    }
}
