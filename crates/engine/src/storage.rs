//! Finds out what kind of drive a patch works on, and chooses how to read from it.
//!
//! An SSD answers many requests at once, so several threads read at the same time and the
//! file cache is left out of it. A hard disk has one arm: threads that read in different
//! places send it back and forth, and without the file cache Windows does not read ahead.
//! The settings for an SSD were measured on an NVMe drive. Which settings are best on a hard
//! disk has not been measured yet, so what is chosen for one is cautious, is reported, and
//! can be overridden.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::time::Instant;

/// Most reader threads fetching copies on a fast drive. Measured on an NVMe drive, the
/// copies of an 8 GB rebuild took 19 s with one thread, 10 s with two, 6 s with four and 4 s
/// with eight; the gain then flattens while the drive still has new files to write.
const MAX_READERS: usize = 8;

/// Most threads checksumming files at once on a fast drive.
const MAX_CHECKERS: usize = 4;

/// A file has to be at least this long for the time between two reads in it to say
/// something about the drive: in a short file every place is near every other.
const PROBE_MIN_LEN: u64 = 64 << 20;
/// How many reads the probe times.
const PROBE_READS: u64 = 20;
/// The size of one probe read. Reads past the file cache are whole sectors, and 4096 bytes
/// is a whole number of sectors on every drive in use.
const PROBE_READ: usize = 4096;
/// A drive whose reads at places far apart take longer than this has to move something to
/// get there. An SSD answers in about 0.2 ms, a hard disk in 8 ms or more.
const SLOW_SEEK_MS: f64 = 2.0;

/// Who decides how the drive is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// The engine, from what [`look`] finds.
    #[default]
    Auto,
    /// As for an SSD, whatever was found.
    Fast,
    /// As for a hard disk, whatever was found.
    Slow,
}

impl Choice {
    /// How a drive that looks like `volume` is read under this choice.
    pub fn profile(self, volume: &Volume) -> Profile {
        match self {
            Choice::Auto => Profile::for_volume(volume),
            Choice::Fast => Profile::fast(),
            Choice::Slow => Profile::slow(),
        }
    }
}

/// What [`look`] found out about a drive.
#[derive(Debug, Clone, PartialEq)]
pub struct Volume {
    /// The name of the file system, such as `NTFS` or `exFAT`. Empty when it cannot be told.
    pub file_system: String,
    /// The median time of a read at a place far from the last one, in milliseconds. `None`
    /// when it was not measured.
    pub seek_ms: Option<f64>,
    /// Whether the drive takes so long to get from one place to another that it is read
    /// the cautious way.
    pub slow: bool,
}

impl Volume {
    /// Whether the immediate way of patching may begin on this drive. That way puts a new
    /// file in the place of an old one by one rename and counts on the file there being
    /// wholly the old one or wholly the new one after a power cut, with the renames of a
    /// run surviving in the order they were made. NTFS keeps a journal that gives this.
    /// exFAT and FAT32 keep none, and of any other file system nothing is known here.
    pub fn takes_immediate(&self) -> bool {
        self.file_system.eq_ignore_ascii_case("NTFS")
    }
}

/// Looks at the drive `dir` is on. `sample` is a large file on that drive to time reads in;
/// without one, or when it cannot be read, the drive is taken to be fast.
pub fn look(dir: &Path, sample: Option<&Path>) -> Volume {
    let seek_ms = sample.and_then(seek_time);
    Volume { file_system: file_system(dir), seek_ms, slow: seek_ms.is_some_and(|ms| ms > SLOW_SEEK_MS) }
}

/// How the files on a drive are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// Threads that fetch the copies of a rebuild from the original files.
    pub readers: usize,
    /// Threads that checksum files at the same time.
    pub checkers: usize,
    /// Whether the originals a rebuild copies from are read past the file cache.
    pub unbuffered: bool,
}

impl Profile {
    /// For a drive that answers many requests at once. Measured on an NVMe drive: see
    /// `MAX_READERS`, and reading past the file cache took an 8 GB rebuild from 19 s to
    /// 10 s.
    pub fn fast() -> Profile {
        let threads = std::thread::available_parallelism().ok().map(|n| n.get());
        Profile {
            readers: threads.unwrap_or(4).clamp(2, MAX_READERS),
            checkers: threads.unwrap_or(2).min(MAX_CHECKERS),
            unbuffered: true,
        }
    }

    /// For a drive that has to travel between places. Files are checked one after the
    /// other, two threads fetch copies so that the drive can at least pick the nearer of
    /// two requests, and reads go through the file cache with Windows told to read ahead.
    /// Not measured on a hard disk yet.
    pub fn slow() -> Profile {
        Profile { readers: 2, checkers: 1, unbuffered: false }
    }

    /// The way to read a drive that looks like `volume`.
    fn for_volume(volume: &Volume) -> Profile {
        if volume.slow { Profile::slow() } else { Profile::fast() }
    }
}

/// The median time, in milliseconds, of single reads at places far apart in `sample`, each
/// past the file cache so that it is the drive that answers. `None` when the file is too
/// short to have places far apart, or cannot be read that way.
fn seek_time(sample: &Path) -> Option<f64> {
    let len = fs::metadata(sample).ok().filter(|meta| meta.is_file())?.len();
    if len < PROBE_MIN_LEN {
        return None;
    }
    let file = open_unbuffered(sample).ok()?;
    // A read past the file cache needs a buffer at an aligned address. One is found inside
    // a buffer twice as long, without any unsafe code.
    let mut buffer = vec![0u8; 2 * PROBE_READ];
    let pad = buffer.as_ptr().align_offset(PROBE_READ);
    if pad >= PROBE_READ {
        return None;
    }
    let sector = &mut buffer[pad..pad + PROBE_READ];

    let mut times = Vec::new();
    for turn in 0..PROBE_READS {
        // The same places every time, so that two looks at one drive agree, and not in
        // order: a hard disk has to travel between each two of them.
        let slot = turn * 7 % PROBE_READS;
        let offset = len / (PROBE_READS + 1) * (slot + 1) / PROBE_READ as u64 * PROBE_READ as u64;
        let started = Instant::now();
        if read_at(&file, sector, offset).ok()? != PROBE_READ {
            return None;
        }
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    Some(times[times.len() / 2])
}

/// Opens a file for reads that bypass the file cache, where the system has such a thing.
fn open_unbuffered(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING);
    }
    options.open(path)
}

/// Reads at a place in a file without moving a file pointer.
pub(crate) fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(windows)]
    return std::os::windows::fs::FileExt::seek_read(file, buffer, offset);
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::read_at(file, buffer, offset);
}

/// The name of the file system `dir` is on, or nothing when Windows does not say.
#[cfg(windows)]
fn file_system(dir: &Path) -> String {
    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};

    let path = crate::wide::to_wide(dir);
    // The root of the volume is no longer than the full form of the path it is found from.
    let mut root = vec![0u16; path.len() + 260];
    // Windows documents 261 as the most a file system name takes.
    let mut name = [0u16; 261];
    // SAFETY: `path` ends in a zero, both buffers are passed with their true lengths and
    // outlive the calls, and the pointers that are null are for answers not asked for.
    let found = unsafe {
        GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
            && GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                name.as_mut_ptr(),
                name.len() as u32,
            ) != 0
    };
    if !found {
        return String::new();
    }
    crate::wide::from_wide(&name)
}

#[cfg(not(windows))]
fn file_system(_dir: &Path) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(seek_ms: Option<f64>) -> Volume {
        Volume { file_system: "NTFS".into(), seek_ms, slow: seek_ms.is_some_and(|ms| ms > SLOW_SEEK_MS) }
    }

    #[test]
    fn drive_is_read_the_cautious_way_only_when_it_was_found_slow() {
        // Not measured, an SSD, and right at the line: all read the fast way.
        for seek_ms in [None, Some(0.05), Some(0.2), Some(2.0)] {
            assert_eq!(Profile::for_volume(&volume(seek_ms)), Profile::fast(), "{seek_ms:?}");
        }
        for seek_ms in [2.01, 8.0, 15.0] {
            assert_eq!(Profile::for_volume(&volume(Some(seek_ms))), Profile::slow(), "{seek_ms}");
        }
    }

    #[test]
    fn choice_overrides_what_was_found() {
        let (ssd, disk) = (volume(Some(0.1)), volume(Some(12.0)));
        assert_eq!(Choice::default(), Choice::Auto);
        assert_eq!(Choice::Auto.profile(&ssd), Profile::fast());
        assert_eq!(Choice::Auto.profile(&disk), Profile::slow());
        assert_eq!(Choice::Fast.profile(&disk), Profile::fast());
        assert_eq!(Choice::Slow.profile(&ssd), Profile::slow());
    }

    #[test]
    fn fast_way_is_what_was_measured_and_the_slow_way_is_cautious() {
        let fast = Profile::fast();
        assert!((2..=MAX_READERS).contains(&fast.readers), "{fast:?}");
        assert!((1..=MAX_CHECKERS).contains(&fast.checkers), "{fast:?}");
        assert!(fast.unbuffered);
        assert_eq!(Profile::slow(), Profile { readers: 2, checkers: 1, unbuffered: false });
    }

    #[test]
    fn small_file_tells_nothing_about_the_drive() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.bin");
        fs::write(&small, vec![7u8; 100_000]).unwrap();
        let missing = dir.path().join("missing.bin");

        for sample in [None, Some(small.as_path()), Some(dir.path()), Some(missing.as_path())] {
            let found = look(dir.path(), sample);
            assert_eq!(found.seek_ms, None, "{sample:?}");
            assert!(!found.slow, "{sample:?}");
        }
    }

    #[test]
    fn large_file_is_timed() {
        let dir = tempfile::tempdir().unwrap();
        let large = dir.path().join("large.bin");
        // Only the length is set, which takes no time and no room worth speaking of.
        File::create(&large).unwrap().set_len(PROBE_MIN_LEN).unwrap();

        let found = look(dir.path(), Some(&large));
        let seek_ms = found.seek_ms.expect("a file of the least length was not timed");
        assert!(seek_ms >= 0.0 && seek_ms.is_finite(), "{seek_ms}");
        assert_eq!(found.slow, seek_ms > SLOW_SEEK_MS);
        // One byte less and there is nothing to time.
        File::options().write(true).open(&large).unwrap().set_len(PROBE_MIN_LEN - 1).unwrap();
        assert_eq!(look(dir.path(), Some(&large)).seek_ms, None);
    }

    #[cfg(windows)]
    #[test]
    fn file_system_of_a_real_folder_has_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let name = file_system(dir.path());
        assert!(!name.is_empty() && name.is_ascii(), "{name:?}");
        assert_eq!(look(dir.path(), None).file_system, name);
    }

    #[test]
    fn immediate_way_may_begin_on_ntfs_and_on_nothing_else() {
        let on = |file_system: &str| Volume { file_system: file_system.into(), seek_ms: None, slow: false };
        assert!(on("NTFS").takes_immediate() && on("ntfs").takes_immediate());
        // Not where no journal keeps a rename whole, and not where the name is unknown.
        for other in ["exFAT", "FAT32", "FAT", "ReFS", "UDF", "NTFS5", ""] {
            assert!(!on(other).takes_immediate(), "{other:?}");
        }
    }
}
