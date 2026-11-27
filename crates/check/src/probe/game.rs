//! What an install folder holds, apart from what its files are compared with: the version
//! the game program carries, the last line of the game's own patcher, how many files there
//! are, and whether this app has been at work in the folder.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mfp_engine::preflight::{LOCK_FILE_NAME, WORK_DIR_NAME};
use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};

use super::{io_error, last_error};
use crate::facts::{Fact, Game, Unknown};
use crate::wide::to_wide;

/// The file the game's own patcher writes its steps to.
const PATCHER_LOG: &str = "Patcher.log";
/// How much of the end of that file is read.
const LOG_TAIL: u64 = 1 << 20;
/// A folder with more entries than this is not walked to its end: it is no game folder.
const MOST_ENTRIES: u64 = 200_000;
/// The longest a folder is walked. A game folder is listed in well under a second, also
/// on a seat that gets it from a server; a drive that takes longer is not waited for.
const WALK_TIME: Duration = Duration::from_secs(4);

/// What walking a folder found.
#[derive(Debug, Clone, Default)]
pub struct Walked {
    /// How many files, and their sizes added up.
    pub files: u64,
    pub bytes: u64,
    /// The largest file, with its size.
    pub largest: Option<(PathBuf, u64)>,
    /// Every file by its path inside the folder, in the form names are compared in.
    pub keys: BTreeSet<String>,
    /// Why the walk ended early, if it did.
    pub cut_short: Option<Unknown>,
}

/// Lists every file under `install` with its size, reading none. Links to other folders are
/// not followed. Setting `stop` ends it at the next folder, as taking too long does.
pub fn walk(install: &Path, stop: &AtomicBool) -> Walked {
    walk_within(install, WALK_TIME, stop)
}

/// As [`walk`], given up when it has taken longer than `within`.
fn walk_within(install: &Path, within: Duration, stop: &AtomicBool) -> Walked {
    let started = Instant::now();
    let mut walked = Walked::default();
    let mut entries = 0u64;
    let mut todo = vec![install.to_path_buf()];
    while let Some(dir) = todo.pop() {
        if started.elapsed() >= within || stop.load(Ordering::Relaxed) {
            walked.cut_short = Some(Unknown::NoAnswer);
            return walked;
        }
        let children = match fs::read_dir(&dir) {
            Ok(children) => children,
            Err(err) => {
                walked.cut_short.get_or_insert(io_error(&err));
                continue;
            }
        };
        for child in children.flatten() {
            entries += 1;
            if entries > MOST_ENTRIES {
                walked.cut_short = Some(Unknown::NoAnswer);
                return walked;
            }
            let Ok(kind) = child.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                todo.push(child.path());
                continue;
            }
            let Ok(meta) = child.metadata() else { continue };
            walked.files += 1;
            walked.bytes += meta.len();
            let path = child.path();
            if let Ok(inside) = path.strip_prefix(install) {
                walked.keys.insert(crate::known::key(&inside.to_string_lossy()));
            }
            if walked.largest.as_ref().is_none_or(|(_, largest)| meta.len() > *largest) {
                walked.largest = Some((path, meta.len()));
            }
        }
    }
    walked
}

/// What the folder holds, given what walking it found. `program` is the game's program.
pub fn look(install: &Path, program: &str, walked: &Walked) -> Game {
    Game {
        file_version: file_version(&install.join(program)),
        patcher_log: fs::File::open(install.join(PATCHER_LOG)).ok().and_then(last_logged_version),
        files: match walked.cut_short {
            Some(why) if walked.files == 0 => Err(why),
            _ => Ok((walked.files, walked.bytes)),
        },
        app_traces: install.join(WORK_DIR_NAME).exists() || install.join(LOCK_FILE_NAME).exists(),
    }
}

/// The four numbers of the version a program file carries.
pub fn file_version(program: &Path) -> Fact<[u16; 4]> {
    let name = to_wide(program);
    let mut ignored = 0u32;
    // SAFETY: the name ends in a zero, and the number that is written is not used.
    let len = unsafe { GetFileVersionInfoSizeW(name.as_ptr(), &mut ignored) };
    if len == 0 {
        return Err(last_error());
    }
    // The block is read as 32-bit numbers below, so it is kept in a buffer aligned for them.
    let mut block = vec![0u32; (len as usize).div_ceil(4)];
    // SAFETY: the buffer has room for `len` bytes.
    if unsafe { GetFileVersionInfoW(name.as_ptr(), 0, len, block.as_mut_ptr().cast()) } == 0 {
        return Err(last_error());
    }
    let root = to_wide("\\");
    let mut fixed: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut fixed_len = 0u32;
    // SAFETY: the block was filled in by the call above, and the answer is a pointer into
    // it with its length.
    if unsafe { VerQueryValueW(block.as_ptr().cast(), root.as_ptr(), &mut fixed, &mut fixed_len) } == 0
        || fixed.is_null()
    {
        return Err(Unknown::NoAnswer);
    }
    // The fixed part begins with a mark, a layout version, and the two halves of the file
    // version.
    const MARK: u32 = 0xFEEF_04BD;
    if fixed_len < 16 {
        return Err(Unknown::NoAnswer);
    }
    // SAFETY: Windows says the answer is `fixed_len` bytes long, which is at least the 16
    // read here; they lie inside `block`, which is still alive.
    let words = unsafe { std::slice::from_raw_parts(fixed.cast::<u8>(), 16) };
    let word = |at: usize| u32::from_ne_bytes([words[at], words[at + 1], words[at + 2], words[at + 3]]);
    if word(0) != MARK {
        return Err(Unknown::NoAnswer);
    }
    let (high, low) = (word(8), word(12));
    Ok([(high >> 16) as u16, high as u16, (low >> 16) as u16, low as u16])
}

/// The last version the game's patcher wrote to its log: five digits, an underscore and two
/// digits, as in `00421_04`. Nothing else of the log is kept.
fn last_logged_version(mut log: fs::File) -> Option<String> {
    let len = log.metadata().ok()?.len();
    log.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL))).ok()?;
    let mut tail = Vec::new();
    log.take(LOG_TAIL).read_to_end(&mut tail).ok()?;
    last_version_in(&tail)
}

fn last_version_in(text: &[u8]) -> Option<String> {
    let digits =
        |from: usize, count: usize| text.get(from..from + count).is_some_and(|run| run.iter().all(u8::is_ascii_digit));
    let digit_at = |at: Option<usize>| at.and_then(|at| text.get(at)).is_some_and(u8::is_ascii_digit);
    (0..text.len().saturating_sub(7))
        .rev()
        .find(|at| {
            // Exactly five digits, the underscore, exactly two digits.
            digits(*at, 5)
                && text[at + 5] == b'_'
                && digits(at + 6, 2)
                && !digit_at(at.checked_sub(1))
                && !digit_at(Some(at + 8))
        })
        .map(|at| String::from_utf8_lossy(&text[at..at + 8]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_version_of_the_patchers_log_is_found_and_nothing_else_is_taken() {
        let log = "[2026-11-24] 00421_03 -> 00421_04 start C:\\Users\\someone\\x\r\n패치 00421_04 완료\r\nlast line";
        assert_eq!(last_version_in(log.as_bytes()).as_deref(), Some("00421_04"));
        assert_eq!(last_version_in(b"00406_06 then 00406_07").as_deref(), Some("00406_07"));
        // Longer runs of digits are something else, and so is a log without a version.
        assert_eq!(last_version_in(b"id 1234500421_04 and 00421_045"), None);
        assert_eq!(last_version_in(b"nothing here"), None);
        assert_eq!(last_version_in(b""), None);
        assert_eq!(last_version_in(b"00421_04").as_deref(), Some("00421_04"));
    }

    #[test]
    fn folder_is_walked_without_reading_and_its_largest_file_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path();
        fs::create_dir_all(install.join("Data").join("Map")).unwrap();
        fs::write(install.join("MapleStory.exe"), vec![1u8; 300]).unwrap();
        fs::write(install.join("Data").join("Base.wz"), vec![2u8; 1000]).unwrap();
        fs::write(install.join("Data").join("Map").join("Map_000.wz"), vec![3u8; 5000]).unwrap();
        let log = "old 00421_03\r\nnew 00421_04\r\n";
        fs::write(install.join("Patcher.log"), log).unwrap();

        let walked = walk(install, &AtomicBool::new(false));
        assert_eq!((walked.files, walked.bytes), (4, 300 + 1000 + 5000 + log.len() as u64));
        assert_eq!(
            walked.largest.as_ref().map(|(path, len)| (path.file_name().unwrap().to_str().unwrap(), *len)),
            Some(("Map_000.wz", 5000))
        );
        assert!(walked.keys.contains(r"data\map\map_000.wz") && walked.keys.contains("maplestory.exe"));
        assert!(walked.cut_short.is_none());

        let game = look(install, "MapleStory.exe", &walked);
        assert_eq!(game.files, Ok((4, walked.bytes)));
        assert_eq!(game.patcher_log.as_deref(), Some("00421_04"));
        assert!(!game.app_traces);
        // A file of made-up bytes carries no version, and that is an error with a number.
        assert!(matches!(game.file_version, Err(Unknown::Windows(_))), "{:?}", game.file_version);

        fs::create_dir(install.join(".maple-fast-patch")).unwrap();
        assert!(look(install, "MapleStory.exe", &walk(install, &AtomicBool::new(false))).app_traces);
    }

    #[test]
    fn version_of_a_real_program_is_read() {
        // Windows' own shell carries the build of Windows as the third number.
        let shell = crate::probe::system::windows_dir().join("explorer.exe");
        let version = file_version(&shell).expect("the version of explorer.exe could not be read");
        assert!(version[2] >= 10_000, "{version:?}");
        assert!(matches!(file_version(Path::new(r"C:\no\such\program.exe")), Err(Unknown::Windows(_))));
    }

    #[test]
    fn folder_that_is_not_there_is_an_empty_walk_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let walked = walk(&dir.path().join("gone"), &AtomicBool::new(false));
        assert_eq!(walked.files, 0);
        assert!(matches!(walked.cut_short, Some(Unknown::Windows(_))));
        assert!(look(&dir.path().join("gone"), "MapleStory.exe", &walked).files.is_err());
    }

    #[test]
    fn walk_that_takes_too_long_is_given_up_with_what_it_has() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("MapleStory.exe"), b"the game").unwrap();
        let going = AtomicBool::new(false);
        assert_eq!((walk(dir.path(), &going).files, walk(dir.path(), &going).cut_short), (1, None));
        // With no time at all, nothing is listed, and that it was cut short is noted.
        let walked = walk_within(dir.path(), Duration::ZERO, &going);
        assert_eq!((walked.files, walked.cut_short), (0, Some(Unknown::NoAnswer)));
        // A stop ends it in the same way.
        let walked = walk(dir.path(), &AtomicBool::new(true));
        assert_eq!((walked.files, walked.cut_short), (0, Some(Unknown::NoAnswer)));
    }
}
