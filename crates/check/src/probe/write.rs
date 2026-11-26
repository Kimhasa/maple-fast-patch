//! The two things the diagnostic writes into an install folder, and nothing else of it
//! writes there.
//!
//! - An empty file that is made and removed at once, to see that the folder takes writes.
//! - The probe file: a large file of bytes that cannot be made smaller, written, made safe
//!   on the drive, and removed again. What every drive of the PC has free is noted before,
//!   after the writing and after the removal, which shows where a seat keeps what it writes.
//!   This one is only written when the user said yes.
//!
//! Both carry names no game file has. The probe file is opened so that Windows removes it
//! by itself when the program ends, however it ends; and a file that was left all the same
//! (the power went) is removed by the next run.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_DELETE_ON_CLOSE};

use super::{system, volume};
use crate::facts::{DriveKind, Landing, Moved, WriteTest};

/// What the names of both files begin and end with.
const PREFIX: &str = ".mfp-check-";
const SUFFIX: &str = ".tmp";
/// The size of the probe file.
pub const PROBE_BYTES: u64 = 512 << 20;
/// The probe file is written in pieces of this size.
const PIECE: usize = 4 << 20;
/// The right to write to a file, as Windows numbers it.
const GENERIC_WRITE: u32 = 0x4000_0000;

fn name(what: &str) -> String {
    format!("{PREFIX}{what}-{}{SUFFIX}", std::process::id())
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
fn watched(install: &Path) -> Vec<(String, bool)> {
    let target = volume::root_of(install).unwrap_or_default();
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

/// Writes the probe file of `bytes` bytes into the folder and removes it, and notes what
/// was free where at each step. `on_progress` hears how many bytes are written.
///
/// Setting `stop` ends the writing early; the file is removed all the same.
pub fn landing(install: &Path, bytes: u64, stop: &AtomicBool, on_progress: &mut dyn FnMut(u64)) -> Landing {
    use std::os::windows::fs::OpenOptionsExt;

    let roots = watched(install);
    let free_now = || -> Vec<Option<u64>> { roots.iter().map(|(root, _)| volume::free(root).ok()).collect() };
    let path = install.join(name("write-probe"));

    let before = free_now();
    let memory_before = system::memory_free();
    // Windows removes a file opened this way when its last handle closes, which is also
    // what happens when the program is ended from outside.
    let self_removing = OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(GENERIC_WRITE | DELETE)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(&path);
    // Where that is not to be had, the file is removed by hand below.
    let opened = self_removing.or_else(|_| OpenOptions::new().write(true).create_new(true).open(&path));

    let started = Instant::now();
    let (mut written, mut error) = (0u64, None);
    let mut file = match opened {
        Ok(file) => Some(file),
        Err(err) => {
            error = err.raw_os_error().or(Some(0));
            None
        }
    };
    if let Some(file) = file.as_mut() {
        let mut noise = Noise(0x4D46_5043 ^ u64::from(std::process::id()) ^ started.elapsed().as_nanos() as u64);
        let mut piece = vec![0u8; PIECE];
        while written < bytes && !stop.load(Ordering::Relaxed) {
            let len = PIECE.min((bytes - written) as usize);
            noise.fill(&mut piece[..len]);
            if let Err(err) = file.write_all(&piece[..len]) {
                error = err.raw_os_error().or(Some(0));
                break;
            }
            written += len as u64;
            on_progress(written);
        }
        // Made safe on the drive, so that what is measured is the drive and not memory.
        if let Err(err) = file.sync_all() {
            error = error.or(err.raw_os_error()).or(Some(0));
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let after_write = free_now();
    let memory_after_write = system::memory_free();

    drop(file);
    let deleted = remove(&path);
    // A drive may take a moment to hand the room back.
    let mut after_delete = free_now();
    for _ in 0..10 {
        let back = roots.iter().zip(&after_delete).zip(&before).all(|((_, now), before)| match (now, before) {
            (Some(now), Some(before)) => now + written / 2 >= *before,
            _ => true,
        });
        if back {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        after_delete = free_now();
    }

    let volumes = roots
        .iter()
        .enumerate()
        .map(|(at, (root, written_to))| Moved {
            letter: root.trim_end_matches('\\').to_owned(),
            written_to: *written_to,
            before: before[at],
            after_write: after_write[at],
            after_delete: after_delete[at],
        })
        .collect();
    Landing {
        written,
        wanted: bytes,
        seconds,
        error,
        volumes,
        memory: [memory_before, memory_after_write, system::memory_free()],
        deleted,
    }
}

/// Removes the probe file if it is still there, and says whether it is gone.
fn remove(path: &Path) -> bool {
    for _ in 0..5 {
        if !path.exists() {
            return true;
        }
        if fs::remove_file(path).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    !path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_made_and_removed_and_a_missing_folder_takes_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(empty_file(dir.path()), WriteTest::Passed);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        // "The system cannot find the path specified."
        assert_eq!(empty_file(&dir.path().join("gone")), WriteTest::Failed(Some(3)));
    }

    #[test]
    fn probe_file_is_written_and_gone_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        let mut heard = Vec::new();
        let bytes = 9 << 20;
        let landing = landing(dir.path(), bytes, &stop, &mut |written| heard.push(written));
        assert_eq!((landing.written, landing.wanted, landing.error), (bytes, bytes, None));
        assert!(landing.deleted);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "the probe file was left behind");
        assert_eq!(heard, [4 << 20, 8 << 20, 9 << 20]);
        assert!(landing.seconds > 0.0);
        // The drive it was written to is among those watched, once, with its room read.
        let written_to: Vec<&Moved> = landing.volumes.iter().filter(|volume| volume.written_to).collect();
        assert_eq!(written_to.len(), 1);
        assert!(
            written_to[0].before.is_some()
                && written_to[0].after_write.is_some()
                && written_to[0].after_delete.is_some()
        );
        assert!(written_to[0].letter.ends_with(':'), "{:?}", written_to[0].letter);
    }

    #[test]
    fn probe_that_is_stopped_or_cannot_begin_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(true);
        let stopped = landing(dir.path(), 64 << 20, &stop, &mut |_| {});
        assert_eq!((stopped.written, stopped.error, stopped.deleted), (0, None, true));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);

        let nowhere = landing(&dir.path().join("gone"), 1 << 20, &AtomicBool::new(false), &mut |_| {});
        assert_eq!((nowhere.written, nowhere.error), (0, Some(3)));
        assert!(nowhere.deleted);
    }

    #[test]
    fn files_an_earlier_run_left_are_removed_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        for name in [".mfp-check-write-probe-4242.tmp", ".mfp-check-write-test-7.tmp"] {
            fs::write(dir.path().join(name), b"left").unwrap();
        }
        for other in ["MapleStory.exe", ".mfp-check-notes.txt", "mfp-check-write-probe-1.tmp", ".maple-fast-patch.lock"]
        {
            fs::write(dir.path().join(other), b"keep").unwrap();
        }
        // A folder of that name is not a file this program wrote.
        fs::create_dir(dir.path().join(".mfp-check-folder.tmp")).unwrap();
        assert_eq!(remove_leftovers(dir.path()), (2, 0));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 5);
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
