//! The three things the diagnostic writes onto a PC's drives, and nothing else of it
//! writes there.
//!
//! - An empty file that is made and removed at once, to see that the install folder takes
//!   writes.
//! - The write probe: four files of 256 MB of bytes that cannot be made smaller, each
//!   written and made safe on the drive before the next, and all removed again.
//! - The write limit test: as many files of 1 GB as a main patch writes, each made safe on
//!   the drive before the next, and all removed again.
//!
//! The last two are one piece of work, [`fill`], with different sizes, and are only done
//! when the user said yes. What every fixed drive of the PC has free, and what memory
//! Windows has left, is noted before, after every step, after the removal and once more a
//! few seconds later. That shows where a PC keeps what it writes (on the drive it was
//! written to, on another, in memory) and whether it gives the room back.
//!
//! The writing looks at its own readings before every step and ends itself, by the rules
//! of [`halts`], before the PC is brought to where it would stop: a seat in a PC room may
//! keep what it writes in memory, or on a disk it shares with the other seats, and a test
//! that runs until something breaks would break more than itself. It does not end itself
//! because the writing slows down to what the wired line carries: that is what a seat
//! looks like that takes its writes into memory first and then passes them on to its
//! shop's server, and such a seat takes a patch.
//!
//! On the game's drive the files go into the install folder. On any other volume they go
//! into a folder of the diagnostic's own, [`OWN_FOLDER`], which [`fill_elsewhere`] makes
//! under the root of the volume and removes again.
//!
//! Every file carries a name no game file has. The files of [`fill`] are opened so that
//! Windows removes them by itself when the program ends, however it ends; and a file or a
//! folder that was left all the same (the power went) is removed by the next run.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_DELETE_ON_CLOSE};

use super::{system, volume};
use crate::facts::{DriveKind, Halt, Landing, Memory, Moved, Place, Step, WriteTest};
pub use crate::facts::{LIMIT_FLOOR, MEMORY_FLOOR, MEMORY_OWN, OWN_FOLDER};

/// What the names of all these files begin and end with.
const PREFIX: &str = ".mfp-check-";
const SUFFIX: &str = ".tmp";
/// What the write probe writes in all, and in how many files. A reading after each file
/// tells memory that goes down with the writing from memory that moved for another reason,
/// which one reading cannot.
pub const PROBE_BYTES: u64 = 1 << 30;
pub const PROBE_STEPS: u64 = 4;
/// What the write probe leaves free on the drive it writes to: a volume with less than a
/// probe file and this is not written to.
pub const PROBE_FLOOR: u64 = 1 << 30;
/// The size of one file of the write limit test.
pub const LIMIT_STEP: u64 = 1 << 30;
/// A step is too slow when it is written at less than this share of what the wired line
/// carries: far under the speed of writes that go to a shop's server over that line. Two
/// such steps in a row end the writing.
pub const SLOW_SHARE: f64 = 0.2;
/// The most that counts as too slow, in bytes a second, whatever the line carries, and
/// what counts where there is no wired line: 24 MB/s, a fifth of a 1 Gbps line. A disk of
/// the PC itself that writes faster than this is never taken for a stalled one because
/// the PC happens to be on a fast line.
pub const SLOW_AT_MOST: u64 = 24 << 20;
/// The seconds a step may take for each gigabyte it writes, and the least any step is
/// given. A step that takes longer is the last.
pub const STEP_SECONDS_PER_GB: f64 = 180.0;
pub const STEP_SECONDS_AT_LEAST: f64 = 30.0;
/// How long after the removal memory is looked at once more. Memory that was only on its
/// way back is back by then, and memory that is still gone was kept.
pub const SETTLE: Duration = Duration::from_secs(3);
/// Files are written in pieces of this size.
const PIECE: usize = 4 << 20;
/// A file is made safe on the drive after every so many bytes, and not only at its end:
/// making a whole gigabyte safe at once can take many seconds on a slow drive, and a stop
/// is to be heard within about one.
const SAFE_EVERY: u64 = 64 << 20;
/// The right to write to a file, as Windows numbers it.
const GENERIC_WRITE: u32 = 0x4000_0000;

fn name(what: &str) -> String {
    format!("{PREFIX}{what}-{}{SUFFIX}", std::process::id())
}

/// The folders the diagnostic may make its own folder under on the volume `letter` (`D:`),
/// in the order they are tried: the root of the volume, and the folder Windows keeps
/// temporary files in where that is on the volume.
pub fn parents_on(letter: &str) -> Vec<(PathBuf, Place)> {
    let root = format!("{letter}\\");
    let mut parents = vec![(PathBuf::from(&root), Place::Root)];
    let temp = std::env::temp_dir();
    if volume::root_of(&temp).is_ok_and(|of_temp| of_temp.eq_ignore_ascii_case(&root)) {
        parents.push((temp, Place::Temp));
    }
    parents
}

/// Removes what an earlier run of the diagnostic left under `parent`: its own folder, with
/// the files of its own in it. How many files and folders were removed, and how many could
/// not be.
pub fn remove_leftover_folder(parent: &Path) -> (usize, usize) {
    let folder = parent.join(OWN_FOLDER);
    if !folder.is_dir() {
        return (0, 0);
    }
    let (removed, left) = remove_leftovers(&folder);
    // A folder that holds anything else is not emptied, and stays.
    match fs::remove_dir(&folder) {
        Ok(()) => (removed + 1, left),
        Err(_) => (removed, left + 1),
    }
}

/// Removes what an earlier run left on any fixed volume of the PC outside an install
/// folder: see [`remove_leftover_folder`].
pub fn remove_leftovers_elsewhere() -> (usize, usize) {
    let fixed = volume::letters().into_iter().filter(|(_, kind)| *kind == DriveKind::Fixed);
    fixed.flat_map(|(letter, _)| parents_on(&letter)).fold((0, 0), |(removed, left), (parent, _)| {
        let (more_removed, more_left) = remove_leftover_folder(&parent);
        (removed + more_removed, left + more_left)
    })
}

/// Removes files an earlier run of the diagnostic left in the folder: how many were
/// removed, and how many could not be.
pub fn remove_leftovers(install: &Path) -> (usize, usize) {
    let Ok(children) = fs::read_dir(install) else { return (0, 0) };
    let (mut removed, mut left) = (0, 0);
    for child in children.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        let ours = name.starts_with(PREFIX) && name.ends_with(SUFFIX);
        if ours && child.file_type().is_ok_and(|kind| kind.is_file()) {
            match fs::remove_file(child.path()) {
                Ok(()) => removed += 1,
                Err(_) => left += 1,
            }
        }
    }
    (removed, left)
}

/// Makes an empty file in the folder and removes it again.
pub fn empty_file(install: &Path) -> WriteTest {
    let probe = install.join(name("write-test"));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Err(err) => WriteTest::Failed(err.raw_os_error()),
        Ok(file) => {
            drop(file);
            match fs::remove_file(&probe) {
                Ok(()) => WriteTest::Passed,
                Err(err) => WriteTest::LeftBehind(err.raw_os_error()),
            }
        }
    }
}

/// SplitMix64: bytes that no drive, driver or server can store in less room than they take.
struct Noise(u64);

impl Noise {
    fn fill(&mut self, buffer: &mut [u8]) {
        for chunk in buffer.chunks_mut(8) {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }
}

/// The volumes whose free room is watched: every fixed drive, and the one written to.
fn watched(folder: &Path) -> Vec<(String, bool)> {
    let target = volume::root_of(folder).unwrap_or_default();
    let mut roots: Vec<(String, bool)> = volume::letters()
        .into_iter()
        .filter(|(_, kind)| *kind == DriveKind::Fixed)
        .map(|(letter, _)| (format!("{letter}\\"), false))
        .collect();
    match roots.iter_mut().find(|(root, _)| root.eq_ignore_ascii_case(&target)) {
        Some(written_to) => written_to.1 = true,
        None if !target.is_empty() => roots.push((target, true)),
        None => {}
    }
    roots
}

/// How much is written into a folder, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    /// What the names of the files carry: which of the two pieces of work wrote them.
    pub what: &'static str,
    /// Bytes to write in all.
    pub total: u64,
    /// Bytes to a file. Each file is made safe on the drive before the next is begun.
    pub step: u64,
    /// What the drive written to must have left after every step, and any other volume
    /// that goes down with the writing. Zero asks for nothing.
    pub floor: u64,
    /// What must be left of memory where memory goes down with the writing. Zero asks
    /// for nothing.
    pub memory_floor: u64,
    /// Bytes a second under which a step is too slow: two such steps in a row end the
    /// writing. Zero asks for nothing.
    pub slow_below: u64,
    /// How long after the removal memory is looked at once more.
    pub settle: Duration,
}

impl Fill {
    /// The write probe: `bytes` in [`PROBE_STEPS`] files, leaving [`PROBE_FLOOR`] free on
    /// the drive.
    pub fn probe(bytes: u64, settle: Duration) -> Fill {
        let step = bytes.div_ceil(PROBE_STEPS).max(1);
        Fill { what: "write-probe", total: bytes, step, floor: PROBE_FLOOR, memory_floor: 0, slow_below: 0, settle }
    }

    /// The write limit test: `total` bytes in files of `step`, leaving [`LIMIT_FLOOR`] free
    /// on the drive and `memory_floor` of memory, on a PC without a wired line.
    pub fn limit(total: u64, step: u64, memory_floor: u64, settle: Duration) -> Fill {
        let slow_below = SLOW_AT_MOST;
        Fill { what: "write-limit", total, step: step.max(1), floor: LIMIT_FLOOR, memory_floor, slow_below, settle }
    }

    /// The same on a PC whose wired line carries `line` bits a second: a step is too slow
    /// from [`SLOW_SHARE`] of that down, and never from more than [`SLOW_AT_MOST`].
    pub fn on_line(self, line: Option<u64>) -> Fill {
        let slow_below = match line {
            Some(bits) => ((bits as f64 / 8.0 * SLOW_SHARE) as u64).min(SLOW_AT_MOST),
            None => SLOW_AT_MOST,
        };
        Fill { slow_below, ..self }
    }

    /// The memory that has to be available for the writing to begin: what is to be left,
    /// and two steps. Whether memory goes down with the writing shows after two steps, and
    /// a PC with less would be under what is to be left by then. Zero asks for nothing.
    pub fn memory_to_begin(&self) -> u64 {
        if self.memory_floor == 0 { 0 } else { self.memory_floor.saturating_add(self.step.saturating_mul(2)) }
    }
}

/// The seconds a step of `bytes` may take.
pub fn seconds_allowed(bytes: u64) -> f64 {
    (bytes as f64 / (1u64 << 30) as f64 * STEP_SECONDS_PER_GB).max(STEP_SECONDS_AT_LEAST)
}

/// What was read off the PC before the writing began.
#[derive(Debug, Clone, Copy)]
pub struct Start<'a> {
    /// What each watched volume had free, and which of them is the one written to.
    pub free: &'a [Option<u64>],
    pub written_to: Option<usize>,
    pub memory: Option<Memory>,
}

/// Whether the writing ends itself before a step of `next` bytes, and why: by what was
/// read before it began and after each of the `steps` written so far. Changes nothing and
/// reads nothing.
///
/// - The last step took longer than a step may ([`seconds_allowed`]).
/// - The step would leave the drive written to with less free than the plan's floor.
/// - Another volume lost at least half of what was written, and the step would leave that
///   one with less than the floor: the PC keeps what it writes there.
/// - At least two steps are written, at least half of what was written is gone from the
///   memory that was available, and the step would leave less of it than the plan's memory
///   floor: the PC keeps what it writes in memory. Little memory alone does not say so. A
///   PC with a game running has little to begin with, and its writes go to its disk.
/// - The last two steps were both written at less than the plan's slowest speed. A step
///   at the speed of the wired line is far above that: writes that a seat passes on to
///   its shop's server are no reason to end. A step so small that the slowest speed that
///   passes writes it in under a second is not timed at all: making a file safe on the
///   drive takes its time whatever is in the file, and says nothing of the drive's speed.
pub fn halts(plan: &Fill, start: &Start, steps: &[Step], next: u64) -> Option<Halt> {
    let written: u64 = steps.iter().map(|step| step.bytes).sum();
    let last = steps.last();
    if last.is_some_and(|last| last.seconds > seconds_allowed(last.bytes)) {
        return Some(Halt::TimedOut);
    }
    let free = last.map_or(start.free, |last| &last.free[..]);
    let free_at = |at: usize| free.get(at).copied().flatten();
    if plan.floor > 0 {
        let left = next.saturating_add(plan.floor);
        if start.written_to.and_then(free_at).is_some_and(|free| free < left) {
            return Some(Halt::Drive);
        }
        let took_the_writes = |at: usize| match (start.free[at], free_at(at)) {
            (Some(before), Some(now)) => written > 0 && before.saturating_sub(now) >= written / 2 && now < left,
            _ => false,
        };
        if (0..start.free.len()).any(|at| Some(at) != start.written_to && took_the_writes(at)) {
            return Some(Halt::Elsewhere);
        }
    }
    if let (Some(before), Some(now)) = (start.memory, last.and_then(|last| last.memory)) {
        let gone = before.available.saturating_sub(now.available);
        let in_step = steps.len() >= 2 && gone >= written / 2;
        if plan.memory_floor > 0 && in_step && now.available < next.saturating_add(plan.memory_floor) {
            return Some(Halt::Memory);
        }
    }
    if let [.., one, other] = steps {
        let timed = |step: &&Step| step.bytes >= plan.slow_below;
        let too_slow =
            |step: &&Step| timed(step) && (step.bytes as f64 / step.seconds.max(1e-6)) < plan.slow_below as f64;
        if plan.slow_below > 0 && [one, other].iter().all(too_slow) {
            return Some(Halt::Slow);
        }
    }
    None
}

/// A file being written, and where it is.
struct Held {
    file: File,
    path: PathBuf,
}

/// Makes a file that Windows removes when its last handle closes, which is also what
/// happens when the program is ended from outside. Where that is not to be had, a plain
/// file, which is removed by hand.
fn hold(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    let self_removing = OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(GENERIC_WRITE | DELETE)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path);
    self_removing.or_else(|_| OpenOptions::new().write(true).create_new(true).open(path))
}

/// As [`fill`], on a volume without an install folder: makes [`OWN_FOLDER`] under the
/// first of `parents` that takes it, writes into that, and removes it with its files.
///
/// `Err` when none of them takes a folder, with the Windows error number of the first.
pub fn fill_elsewhere(
    parents: &[(PathBuf, Place)],
    plan: Fill,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<Landing, Option<i32>> {
    let mut refused = None;
    for (parent, place) in parents {
        let folder = parent.join(OWN_FOLDER);
        match fs::create_dir(&folder) {
            Ok(()) => {}
            // One that an earlier run left.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && folder.is_dir() => {}
            Err(err) => {
                refused = refused.or(Some(err.raw_os_error()));
                continue;
            }
        }
        let mut landing = fill(&folder, plan, stop, on_progress);
        landing.place = *place;
        landing.deleted &= fs::remove_dir(&folder).is_ok() || !folder.exists();
        return Ok(landing);
    }
    Err(refused.flatten())
}

/// Writes files into the install folder as `plan` says and removes them all, and notes
/// what was free where before, after every step, after the removal, and once more after
/// the plan's settle time. `on_progress` hears how many bytes are written.
///
/// The writing ends at the first error, when [`halts`] says so, and when `stop` is set,
/// which also ends the waiting. The files are removed however it ends.
pub fn fill(install: &Path, plan: Fill, stop: &AtomicBool, on_progress: &mut dyn FnMut(u64)) -> Landing {
    let roots = watched(install);
    let free_now = || -> Vec<Option<u64>> { roots.iter().map(|(root, _)| volume::free(root).ok()).collect() };
    let stopped = || stop.load(Ordering::Relaxed);

    let before = free_now();
    let memory_before = system::memory();
    let start = Start {
        free: &before,
        written_to: roots.iter().position(|(_, written_to)| *written_to),
        memory: memory_before,
    };
    let started = Instant::now();
    let mut noise = Noise(0x4D46_5043 ^ u64::from(std::process::id()) ^ started.elapsed().as_nanos() as u64);
    let mut piece = vec![0u8; PIECE];
    let (mut written, mut error, mut halted) = (0u64, None, None);
    let mut held: Vec<Held> = Vec::new();
    let mut steps: Vec<Step> = Vec::new();

    'steps: while written < plan.total && !stopped() {
        let step = plan.step.min(plan.total - written);
        // What the PC has left is read off it after each step, not worked out from what
        // was written: on a seat the two need not move together.
        halted = halts(&plan, &start, &steps, step);
        if halted.is_some() {
            break;
        }
        let path = install.join(name(&format!("{}-{}", plan.what, held.len() + 1)));
        let mut file = match hold(&path) {
            Ok(file) => file,
            Err(err) => {
                error = err.raw_os_error().or(Some(0));
                break;
            }
        };
        let step_started = Instant::now();
        let mut done = 0u64;
        let mut failed = None;
        // A step that takes longer than it may is left where it is, and is the last.
        while done < step && !stopped() && step_started.elapsed().as_secs_f64() <= seconds_allowed(step) {
            let len = PIECE.min((step - done) as usize);
            noise.fill(&mut piece[..len]);
            if let Err(err) = file.write_all(&piece[..len]) {
                failed = err.raw_os_error().or(Some(0));
                break;
            }
            done += len as u64;
            on_progress(written + done);
            if done.is_multiple_of(SAFE_EVERY)
                && done < step
                && let Err(err) = file.sync_all()
            {
                failed = err.raw_os_error().or(Some(0));
                break;
            }
        }
        // Made safe on the drive, so that what is measured is the drive and not memory.
        // Not when it was stopped: what is about to be removed need not be made safe.
        if !stopped()
            && let Err(err) = file.sync_all()
        {
            failed = failed.or(err.raw_os_error()).or(Some(0));
        }
        written += done;
        held.push(Held { file, path });
        if done > 0 {
            steps.push(Step {
                bytes: done,
                seconds: step_started.elapsed().as_secs_f64(),
                free: free_now(),
                memory: system::memory(),
            });
        }
        if failed.is_some() {
            error = failed;
            break 'steps;
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let after_write = free_now();
    let memory_after_write = system::memory();

    let made_any = !held.is_empty();
    let mut deleted = true;
    for Held { file, path } in held {
        drop(file);
        deleted &= remove(&path);
    }
    // A drive may take a moment to hand the room back. It is not waited for after a stop.
    let mut after_delete = free_now();
    for _ in 0..10 {
        if stopped() {
            break;
        }
        let back = after_delete.iter().zip(&before).all(|(now, before)| match (now, before) {
            (Some(now), Some(before)) => now + written / 2 >= *before,
            _ => true,
        });
        if back {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
        after_delete = free_now();
    }
    let memory_after_delete = system::memory();
    // Nothing was written: there is nothing to wait for.
    let waiting = Instant::now();
    while written > 0 && waiting.elapsed() < plan.settle && !stopped() {
        std::thread::sleep(Duration::from_millis(100).min(plan.settle));
    }
    let waited = waiting.elapsed().as_secs_f64();
    let memory_later = if written > 0 { system::memory() } else { None };
    let later = if written > 0 { free_now() } else { vec![None; roots.len()] };

    let volumes = roots
        .iter()
        .enumerate()
        .map(|(at, (root, written_to))| Moved {
            letter: root.trim_end_matches('\\').to_owned(),
            written_to: *written_to,
            before: before[at],
            after_write: after_write[at],
            after_delete: after_delete[at],
            later: later[at],
        })
        .collect();
    Landing {
        place: Place::Install,
        written,
        wanted: plan.total,
        seconds,
        error,
        halted,
        steps,
        volumes,
        memory: [memory_before, memory_after_write, memory_after_delete, memory_later],
        waited,
        // With no file made there was none to remove.
        deleted: deleted || !made_any,
    }
}

/// Removes a file if it is still there, and says whether it is gone.
fn remove(path: &Path) -> bool {
    for _ in 0..5 {
        if !path.exists() {
            return true;
        }
        if fs::remove_file(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    fn quick(bytes: u64) -> Fill {
        Fill::probe(bytes, Duration::from_millis(300))
    }

    /// A write limit test that asks nothing of memory: what is available on the PC the
    /// tests run on is not theirs to count on.
    fn limit(total: u64, step: u64, settle: Duration) -> Fill {
        Fill::limit(total, step, 0, settle)
    }

    fn files_in(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn empty_file_is_made_and_removed_and_a_missing_folder_takes_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(empty_file(dir.path()), WriteTest::Passed);
        assert_eq!(files_in(dir.path()), 0);
        // "The system cannot find the path specified."
        assert_eq!(empty_file(&dir.path().join("gone")), WriteTest::Failed(Some(3)));
    }

    #[test]
    fn probe_files_are_written_one_after_the_other_and_gone_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        let mut heard = Vec::new();
        let bytes = 18 * MIB;
        let landing = fill(dir.path(), quick(bytes), &stop, &mut |written| heard.push(written));
        assert_eq!((landing.written, landing.wanted, landing.error), (bytes, bytes, None));
        assert!(landing.deleted && landing.halted.is_none());
        assert_eq!(files_in(dir.path()), 0, "a probe file was left behind");
        assert_eq!(heard.last(), Some(&bytes));
        assert!(landing.seconds > 0.0);
        // Four files, with what was free and what memory there was read after each.
        assert_eq!(landing.steps.iter().map(|step| step.bytes).collect::<Vec<_>>(), [9 * MIB / 2; 4]);
        assert!(landing.steps.iter().all(|step| step.memory.is_some() && step.free.len() == landing.volumes.len()));
        // The drive it was written to is among those watched, once, with its room read.
        let written_to: Vec<&Moved> = landing.volumes.iter().filter(|volume| volume.written_to).collect();
        assert_eq!(written_to.len(), 1);
        assert!(
            written_to[0].before.is_some()
                && written_to[0].after_write.is_some()
                && written_to[0].after_delete.is_some()
                && written_to[0].later.is_some()
        );
        assert!(written_to[0].letter.ends_with(':'), "{:?}", written_to[0].letter);
        assert_eq!((landing.place, landing.written_to()), (Place::Install, Some(written_to[0].letter.as_str())));
        // Memory was read at all four moments, the last of them after the wait.
        assert!(landing.memory.iter().all(Option::is_some), "{:?}", landing.memory);
        assert!(landing.waited >= 0.3 && landing.waited < 3.0, "{}", landing.waited);
        // The probe as it is run: a gigabyte in four files, and a floor on the drive only.
        let probe = Fill::probe(PROBE_BYTES, SETTLE);
        assert_eq!((probe.total, probe.step, probe.floor, probe.memory_floor), (GIB, 256 * MIB, PROBE_FLOOR, 0));
        assert_eq!(probe.memory_to_begin(), 0);
    }

    #[test]
    fn limit_test_writes_file_after_file_and_notes_what_was_free_after_each() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("MapleStory.exe"), b"the game").unwrap();
        let stop = AtomicBool::new(false);
        let plan = limit(22 * MIB, 8 * MIB, Duration::from_millis(200));
        // While it writes, its files are in the folder under names of its own.
        let mut most_files = 0;
        let mut names = Vec::new();
        let landing = fill(dir.path(), plan, &stop, &mut |_| {
            let here: Vec<String> = fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .map(|file| file.file_name().to_string_lossy().into_owned())
                .collect();
            if here.len() > most_files {
                most_files = here.len();
                names = here;
            }
        });
        assert_eq!((landing.written, landing.wanted, landing.error), (22 * MIB, 22 * MIB, None));
        assert!(landing.deleted && landing.halted.is_none());
        // Three files: two whole steps and what was left.
        assert_eq!(landing.steps.iter().map(|step| step.bytes).collect::<Vec<_>>(), [8 * MIB, 8 * MIB, 6 * MIB]);
        assert_eq!(most_files, 4, "{names:?}");
        assert!(
            names
                .iter()
                .filter(|name| *name != "MapleStory.exe")
                .all(|name| name.starts_with(".mfp-check-write-limit-"))
        );
        for step in &landing.steps {
            assert!(step.seconds > 0.0 && step.memory.is_some());
            assert_eq!(step.free.len(), landing.volumes.len());
        }
        // Nothing of it is left, and what was in the folder is as it was.
        assert_eq!(files_in(dir.path()), 1);
        assert_eq!(fs::read(dir.path().join("MapleStory.exe")).unwrap(), b"the game");
    }

    #[test]
    fn writing_ends_before_a_step_that_would_leave_less_than_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        let free = volume::free(&volume::root_of(dir.path()).unwrap()).unwrap();
        // A floor just above what is free now: not even the first step may be written.
        let tight = Fill { floor: free + 64 * MIB, ..limit(16 * MIB, 8 * MIB, Duration::ZERO) };
        let landing = fill(dir.path(), tight, &stop, &mut |_| {});
        assert_eq!((landing.written, landing.error, landing.halted), (0, None, Some(Halt::Drive)));
        assert!(landing.steps.is_empty() && landing.deleted);
        assert_eq!(files_in(dir.path()), 0);
        // A floor the drive has room for does not stop it.
        let roomy = Fill { floor: free.saturating_sub(4096 * MIB).min(64 * MIB), ..tight };
        let landing = fill(dir.path(), roomy, &stop, &mut |_| {});
        assert_eq!((landing.written, landing.halted), (16 * MIB, None));
        assert_eq!(files_in(dir.path()), 0);
    }

    /// A step of a gigabyte written in `seconds`, after which `available` memory was left
    /// and the two watched volumes had `free`.
    fn step(seconds: f64, available: u64, free: [u64; 2]) -> Step {
        Step {
            bytes: GIB,
            seconds,
            free: free.map(Some).to_vec(),
            memory: Some(Memory { available, cache: 0, nonpaged: 0, commit: 0 }),
        }
    }

    #[test]
    fn writing_ends_itself_by_what_it_read_after_each_step() {
        // The test as it is run: a gigabyte a step, four left on a drive and of memory.
        let plan = Fill::limit(73 * GIB / 2, LIMIT_STEP, MEMORY_FLOOR, SETTLE);
        assert_eq!((plan.floor, plan.memory_floor, plan.memory_to_begin()), (LIMIT_FLOOR, MEMORY_FLOOR, 6 * GIB));
        // The game's drive is the first of the two volumes, with 300 GB free; the other
        // has 32. Memory: 20 GB available.
        let free = [Some(300 * GIB), Some(32 * GIB)];
        let memory = |available: u64| Some(Memory { available, cache: 0, nonpaged: 0, commit: 0 });
        let start = Start { free: &free, written_to: Some(0), memory: memory(20 * GIB) };
        let ends = |steps: &[Step]| halts(&plan, &start, steps, GIB);
        // A step on a PC whose writes go to the drive they are written to, `at` steps in.
        let clean = |at: u64| step(2.5, 20 * GIB - 300 * MIB, [(300 - at) * GIB, 32 * GIB]);
        let run = |steps: u64| (1..=steps).map(clean).collect::<Vec<Step>>();

        // Before anything is written, and all the way through a clean run.
        assert_eq!(ends(&[]), None);
        assert!((1..=36).all(|steps| ends(&run(steps)).is_none()));

        // Memory goes down by what is written, step after step: the writing ends before
        // the step that would leave less than four gigabytes of it.
        let in_memory = |steps: u64| -> Vec<Step> {
            (1..=steps).map(|at| step(2.5, (20 - at) * GIB, [(300 - at) * GIB, 32 * GIB])).collect()
        };
        assert!((1..=15).all(|steps| ends(&in_memory(steps)).is_none()));
        assert_eq!(ends(&in_memory(16)), Some(Halt::Memory));
        // One step says nothing yet, however little memory is left after it.
        let little = Start { memory: memory(5 * GIB), ..start };
        let one = [step(2.5, 4 * GIB, [299 * GIB, 32 * GIB])];
        assert_eq!(halts(&plan, &little, &one, GIB), None);
        let two = [one[0].clone(), step(2.5, 3 * GIB, [298 * GIB, 32 * GIB])];
        assert_eq!(halts(&plan, &little, &two, GIB), Some(Halt::Memory));

        // Memory that is low and moves about, and does not go down with what is written:
        // a PC with a game running, whose writes go to its disk.
        let noisy: Vec<Step> = [4500, 4200, 4700, 4100, 4400, 4000, 4300, 4600, 4100, 4500]
            .iter()
            .zip(1..)
            .map(|(available, at)| step(2.5, available * MIB, [(300 - at) * GIB, 32 * GIB]))
            .collect();
        assert!((1..=noisy.len()).all(|steps| halts(&plan, &little, &noisy[..steps], GIB).is_none()));
        // A plan that asks nothing of memory is not ended by it, and neither is a run
        // whose memory could not be read.
        let no_floor = Fill { memory_floor: 0, ..plan };
        assert_eq!(halts(&no_floor, &start, &in_memory(18), GIB), None);
        assert_eq!(halts(&plan, &Start { memory: None, ..start }, &in_memory(18), GIB), None);

        // The other volume goes down by what is written: a seat that keeps its writes on
        // a disk of its own. The writing ends before that disk is left with under four.
        let on_the_other = |steps: u64| -> Vec<Step> {
            (1..=steps).map(|at| step(2.5, 20 * GIB, [(300 - at) * GIB, (32 - at) * GIB])).collect()
        };
        assert!((1..=27).all(|steps| ends(&on_the_other(steps)).is_none()));
        assert_eq!(ends(&on_the_other(28)), Some(Halt::Elsewhere));
        // A volume that is nearly full and did not move is left out of it.
        let full = [Some(300 * GIB), Some(2 * GIB)];
        let beside_a_full_one = Start { free: &full, ..start };
        let untouched: Vec<Step> = (1..=10).map(|at| step(2.5, 20 * GIB, [(300 - at) * GIB, 2 * GIB])).collect();
        assert_eq!(halts(&plan, &beside_a_full_one, &untouched, GIB), None);

        // The drive written to would be left with less than the floor.
        let tight = [Some(8 * GIB), Some(32 * GIB)];
        let on_a_tight_drive = Start { free: &tight, ..start };
        let filling = |steps: u64| -> Vec<Step> {
            (1..=steps).map(|at| step(2.5, 20 * GIB, [(8 - at) * GIB, 32 * GIB])).collect()
        };
        assert_eq!(halts(&plan, &on_a_tight_drive, &filling(3), GIB), None);
        assert_eq!(halts(&plan, &on_a_tight_drive, &filling(4), GIB), Some(Halt::Drive));
        assert_eq!(halts(&plan, &Start { free: &[Some(4 * GIB), None], ..start }, &[], GIB), Some(Halt::Drive));

        // The seat is on a 1 Gbps line, which carries 119 MB/s: a step is too slow under
        // a fifth of that, 23.8 MB/s.
        let on_a_gigabit = plan.on_line(Some(1_000_000_000));
        assert_eq!(on_a_gigabit.slow_below, 25_000_000);
        let ends = |steps: &[Step]| halts(&on_a_gigabit, &start, steps, GIB);
        let at_speed = |megabytes: f64, at: u64| step(1024.0 / megabytes, 20 * GIB, [(300 - at) * GIB, 32 * GIB]);
        // Three steps at 414 MB/s, and then every step at 105 MB/s, which is what the
        // line carries: the seat has passed its writes on to its shop's server. That is
        // no reason to end, however long it stays so.
        let mut handed_over = run(3);
        handed_over.extend((4..=36).map(|at| at_speed(105.0, at)));
        assert!((1..=36).all(|steps| ends(&handed_over[..steps]).is_none()));
        // A drive that slows to a tenth of what it began at and is still above that, a
        // disk whose fast part is full, goes on too.
        let mut slower = run(10);
        slower.extend((11..=20).map(|at| at_speed(40.0, at)));
        assert_eq!(ends(&slower), None);
        // Far under what the line carries: 15 MB/s. One such step may be a hiccup; the
        // second in a row ends the writing.
        let mut stalling = run(5);
        stalling.push(at_speed(15.0, 6));
        assert_eq!(ends(&stalling), None);
        stalling.push(at_speed(15.0, 7));
        assert_eq!(ends(&stalling), Some(Halt::Slow));
        // A slow step, a fast one, and a slow one again are not two in a row.
        let hiccups = [clean(1), at_speed(15.0, 2), clean(3), at_speed(15.0, 4)];
        assert_eq!(ends(&hiccups), None);
        // On a 100 Mbps line, which carries 11.9 MB/s, steps at 10 MB/s are the line's
        // speed and not too slow; and no line is ever fast enough to make more than
        // 24 MB/s too slow, nor is the lack of one.
        let on_a_slow_line = plan.on_line(Some(100_000_000));
        let slow: Vec<Step> = (1..=6).map(|at| at_speed(10.0, at)).collect();
        assert_eq!((on_a_slow_line.slow_below, halts(&on_a_slow_line, &start, &slow, GIB)), (2_500_000, None));
        assert_eq!(ends(&slow), Some(Halt::Slow));
        assert_eq!(plan.on_line(Some(10_000_000_000)).slow_below, SLOW_AT_MOST);
        assert_eq!((plan.slow_below, plan.on_line(None).slow_below), (SLOW_AT_MOST, SLOW_AT_MOST));
        // The probe is never ended for being slow.
        assert_eq!(Fill::probe(PROBE_BYTES, SETTLE).slow_below, 0);
        // Nor is anything for steps too small to time: two megabytes that took a second
        // each are the time it takes to make a file safe, not the speed of the drive.
        let small = |seconds: f64| Step { bytes: 2 * MIB, seconds, ..clean(1) };
        assert_eq!(halts(&plan, &start, &[small(1.0), small(1.0), small(1.0)], 2 * MIB), None);
        let timed = |seconds: f64| Step { bytes: SLOW_AT_MOST, seconds, ..clean(1) };
        assert_eq!(halts(&plan, &start, &[timed(2.0), timed(2.0)], 2 * MIB), Some(Halt::Slow));

        // A step that took longer than a step may is the last: three minutes a gigabyte.
        let mut stalled = run(5);
        stalled.push(step(181.0, 20 * GIB, [294 * GIB, 32 * GIB]));
        assert_eq!(ends(&stalled), Some(Halt::TimedOut));
        assert_eq!((seconds_allowed(GIB), seconds_allowed(8 * MIB), seconds_allowed(2 * GIB)), (180.0, 30.0, 360.0));
    }

    #[test]
    fn write_that_is_stopped_or_cannot_begin_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(true);
        let stopped = fill(dir.path(), Fill::probe(64 * MIB, SETTLE), &stop, &mut |_| {});
        assert_eq!((stopped.written, stopped.error, stopped.deleted), (0, None, true));
        assert_eq!(files_in(dir.path()), 0);
        // With nothing written there is nothing to wait for, and no last reading.
        assert!(stopped.waited < 1.0 && stopped.memory[3].is_none(), "{stopped:?}");
        assert!(stopped.volumes.iter().all(|volume| volume.later.is_none()));

        // Stopped in the middle of the second file: both are removed, and what was
        // written until then is counted.
        let stop = AtomicBool::new(false);
        let plan = limit(32 * MIB, 8 * MIB, Duration::from_secs(30));
        let cut = fill(dir.path(), plan, &stop, &mut |written| {
            if written >= 12 * MIB {
                stop.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!((cut.written, cut.error, cut.halted), (12 * MIB, None, None));
        assert_eq!(cut.steps.iter().map(|step| step.bytes).collect::<Vec<_>>(), [8 * MIB, 4 * MIB]);
        assert!(cut.deleted && cut.waited < 5.0);
        assert_eq!(files_in(dir.path()), 0);

        // Stopped in the middle of a long file: it is left at the next piece, without
        // being made safe first, and is gone within a moment. What the writing heard of
        // its progress never went back.
        let stop = AtomicBool::new(false);
        let plan = limit(256 * MIB, 256 * MIB, Duration::from_secs(30));
        let mut heard = Vec::new();
        let began = Instant::now();
        let mut stopped_at = None;
        let cut = fill(dir.path(), plan, &stop, &mut |written| {
            heard.push(written);
            if written >= 96 * MIB && stopped_at.is_none() {
                stop.store(true, Ordering::Relaxed);
                stopped_at = Some(began.elapsed());
            }
        });
        let after_stop = began.elapsed() - stopped_at.expect("the writing did not get that far");
        assert_eq!((cut.written, cut.error, cut.halted), (96 * MIB, None, None));
        assert!(cut.deleted && files_in(dir.path()) == 0);
        assert!(after_stop < Duration::from_secs(2), "{after_stop:?}");
        assert!(heard.windows(2).all(|pair| pair[0] < pair[1]), "{heard:?}");

        let nowhere = fill(&dir.path().join("gone"), Fill::probe(MIB, SETTLE), &AtomicBool::new(false), &mut |_| {});
        assert_eq!((nowhere.written, nowhere.error), (0, Some(3)));
        assert!(nowhere.deleted && nowhere.steps.is_empty());
    }

    #[test]
    fn volume_without_an_install_folder_is_written_to_in_a_folder_that_is_made_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        let (root, temp) = (dir.path().join("root"), dir.path().join("temp"));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&temp).unwrap();
        let parents = [(root.clone(), Place::Root), (temp.clone(), Place::Temp)];
        // While it writes, its files are in a folder of its own under the root.
        let mut seen = Vec::new();
        let landing = fill_elsewhere(&parents, quick(4 * MIB), &stop, &mut |_| {
            seen = fs::read_dir(root.join(OWN_FOLDER))
                .unwrap()
                .flatten()
                .map(|file| file.file_name().to_string_lossy().into_owned())
                .collect();
        })
        .unwrap();
        assert_eq!(
            (landing.place, landing.written, landing.error, landing.deleted),
            (Place::Root, 4 * MIB, None, true)
        );
        assert!(seen.len() == 4 && seen.iter().all(|name| name.starts_with(".mfp-check-write-probe-")), "{seen:?}");
        assert_eq!((files_in(&root), files_in(&temp)), (0, 0));

        // A root that takes no folder: the folder for temporary files is used, and said.
        let no_root = [(dir.path().join("gone"), Place::Root), (temp.clone(), Place::Temp)];
        let landing = fill_elsewhere(&no_root, quick(MIB), &stop, &mut |_| {}).unwrap();
        assert_eq!((landing.place, landing.written, landing.deleted), (Place::Temp, MIB, true));
        assert_eq!(files_in(&temp), 0);
        // Nowhere to write: the error number of the root. "The system cannot find the
        // path specified."
        assert_eq!(fill_elsewhere(&no_root[..1], quick(MIB), &stop, &mut |_| {}), Err(Some(3)));
        assert_eq!(fill_elsewhere(&[], quick(MIB), &stop, &mut |_| {}), Err(None));

        // A folder an earlier run left is used, and removed with what it left in it by the
        // next start, where a folder that holds something else stays.
        fs::create_dir(root.join(OWN_FOLDER)).unwrap();
        let landing = fill_elsewhere(&parents, quick(MIB), &stop, &mut |_| {}).unwrap();
        assert!(landing.deleted && files_in(&root) == 0);
        for parent in [&root, &temp] {
            fs::create_dir(parent.join(OWN_FOLDER)).unwrap();
            fs::write(parent.join(OWN_FOLDER).join(".mfp-check-write-limit-3-4242.tmp"), b"left").unwrap();
        }
        fs::write(temp.join(OWN_FOLDER).join("notes.txt"), b"somebody's").unwrap();
        assert_eq!(remove_leftover_folder(&root), (2, 0));
        assert_eq!(remove_leftover_folder(&temp), (1, 1));
        assert_eq!((files_in(&root), files_in(&temp.join(OWN_FOLDER))), (0, 1));
        assert_eq!(remove_leftover_folder(&root), (0, 0));
        // The places tried on a volume begin with its root.
        assert_eq!(parents_on("Q:")[0], (PathBuf::from(r"Q:\"), Place::Root));
    }

    #[test]
    fn files_an_earlier_run_left_are_removed_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        for name in
            [".mfp-check-write-probe-1-4242.tmp", ".mfp-check-write-test-7.tmp", ".mfp-check-write-limit-37-4242.tmp"]
        {
            fs::write(dir.path().join(name), b"left").unwrap();
        }
        for other in ["MapleStory.exe", ".mfp-check-notes.txt", "mfp-check-write-probe-1.tmp", ".maple-fast-patch.lock"]
        {
            fs::write(dir.path().join(other), b"keep").unwrap();
        }
        // A folder of that name is not a file this program wrote.
        fs::create_dir(dir.path().join(".mfp-check-folder.tmp")).unwrap();
        assert_eq!(remove_leftovers(dir.path()), (3, 0));
        assert_eq!(files_in(dir.path()), 5);
        assert_eq!(remove_leftovers(dir.path()), (0, 0));
        assert_eq!(remove_leftovers(&dir.path().join("gone")), (0, 0));
    }

    #[test]
    fn noise_does_not_repeat() {
        let mut noise = Noise(1);
        let (mut first, mut second) = (vec![0u8; 4099], vec![0u8; 4099]);
        noise.fill(&mut first);
        noise.fill(&mut second);
        assert_ne!(first, second);
        // About as many different bytes as chance gives: nothing a drive could squeeze.
        let distinct = first.iter().collect::<std::collections::BTreeSet<_>>().len();
        assert!(distinct > 250, "{distinct}");
    }
}
