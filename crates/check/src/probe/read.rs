//! How fast the game's drive hands over a large file.
//!
//! One file is read from its start, past Windows' file cache so that it is the drive that
//! answers, and then the same bytes once more. On a disk of the PC itself the two take
//! about as long. Where the second time is many times faster, something below Windows kept
//! the bytes: that is what a seat without a disk does with what it fetched from its server.
//!
//! Each of the two reads ends after [`SAMPLE`] bytes or after [`SAMPLE_TIME`], whichever
//! comes first, so that a slow drive costs the run no more than a fast one.
//!
//! A second read that is many times faster may also mean that the first one was held up:
//! the drive was busy with something else just then. So where it is, the bytes that follow
//! in the file, which were not read before, are read as well. A drive that keeps what it
//! fetched hands those over as slowly as the first ones; a drive that was only held up
//! hands them over as fast as the second read.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mfp_engine::storage;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN};

use super::io_error;
use crate::facts::{Fact, Reads, Unknown};
use crate::verdict::KEPT_BELOW;

/// The most that is read in one go.
pub const SAMPLE: u64 = 256 << 20;
/// The longest one go is given. It ends with the read that is under way when the time is
/// up.
pub const SAMPLE_TIME: Duration = Duration::from_secs(2);
/// A file shorter than this says too little about the drive.
const LEAST: u64 = 16 << 20;
/// The size of one read. Reads past the file cache are whole sectors, and this is a whole
/// number of sectors on every drive in use.
const PIECE: usize = 1 << 20;
/// What a buffer for such reads has to be aligned to.
const ALIGN: usize = 4096;

/// Times reads of `sample`, a large file on the drive in question. `Err` with
/// [`Unknown::Skipped`] when the file is too short to say anything, or when `stop` was set.
pub fn time(sample: &Path, stop: &AtomicBool) -> Fact<Reads> {
    time_within(sample, SAMPLE_TIME, stop)
}

/// As [`time`], with each of the two reads given `within`.
pub fn time_within(sample: &Path, within: Duration, stop: &AtomicBool) -> Fact<Reads> {
    use std::os::windows::fs::OpenOptionsExt;

    let len = std::fs::metadata(sample).map_err(|err| io_error(&err))?.len();
    let bytes = len.min(SAMPLE) / PIECE as u64 * PIECE as u64;
    if bytes < LEAST {
        return Err(Unknown::Skipped);
    }
    let open = || {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
            .open(sample)
            .map_err(|err| io_error(&err))
    };
    // A read past the file cache needs a buffer at an aligned address. One is found inside
    // a longer buffer, without any unsafe code.
    let mut buffer = vec![0u8; PIECE + ALIGN];
    let pad = buffer.as_ptr().align_offset(ALIGN);
    if pad >= ALIGN {
        return Err(Unknown::NoAnswer);
    }
    let piece = &mut buffer[pad..pad + PIECE];

    let (read, first) = pass(&open()?, piece, 0, bytes, within, stop)?;
    // The same bytes once more, and no more of them than came the first time.
    let (read_again, again) = pass(&open()?, piece, 0, read, within, stop)?;
    let (first, again) = (read as f64 / first, read_again as f64 / again);
    // Much faster the second time: the bytes after these, where the file has as many,
    // tell a drive that keeps what it fetched from one that was held up the first time.
    let fresh = if wants_fresh(first, again) && len >= read * 2 && read.is_multiple_of(PIECE as u64) {
        pass(&open()?, piece, read, read, within, stop).ok().map(|(read, seconds)| read as f64 / seconds)
    } else {
        None
    };
    let seek_ms = storage::look(sample.parent().unwrap_or(sample), Some(sample)).seek_ms;
    Ok(Reads { bytes: read, first, again, fresh, seek_ms })
}

/// Whether bytes that were not read before are read as well: when the second read was so
/// much faster than the first that it would count as a sign of a drive that keeps what it
/// fetched.
fn wants_fresh(first: f64, again: f64) -> bool {
    first > 0.0 && again / first >= KEPT_BELOW
}

/// Reads `bytes` of a file from `from` on, or as many of them as come `within` the time,
/// and returns how many were read and the seconds that took.
fn pass(file: &File, piece: &mut [u8], from: u64, bytes: u64, within: Duration, stop: &AtomicBool) -> Fact<(u64, f64)> {
    use std::os::windows::fs::FileExt;

    let started = Instant::now();
    let mut done = 0u64;
    while done < bytes && (done == 0 || started.elapsed() < within) {
        if stop.load(Ordering::Relaxed) {
            return Err(Unknown::Skipped);
        }
        match file.seek_read(piece, from + done) {
            Ok(0) => return Err(Unknown::NoAnswer),
            Ok(count) => done += count as u64,
            Err(err) => return Err(io_error(&err)),
        }
    }
    // A read that took no time at all would make the speed a number without meaning.
    Ok((done, started.elapsed().as_secs_f64().max(1e-6)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_file_is_timed_twice_and_a_small_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let large = dir.path().join("large.bin");
        // Only the length is set, which takes no time and no room worth speaking of.
        File::create(&large).unwrap().set_len(LEAST + 12_345).unwrap();
        let stop = AtomicBool::new(false);

        let reads = time(&large, &stop).expect("a file of the least length was not timed");
        assert_eq!(reads.bytes, LEAST);
        assert!(reads.first > 0.0 && reads.again > 0.0 && reads.first.is_finite() && reads.again.is_finite());

        // Two reads that took about as long: nothing else is read.
        assert_eq!(reads.fresh, None);
        assert!(wants_fresh(480.0, 1700.0) && !wants_fresh(1650.0, 1700.0) && !wants_fresh(0.0, 1700.0));
        // Bytes further on in the file are read from where they are.
        let file = File::open(&large).unwrap();
        let mut piece = vec![0u8; PIECE];
        let within = Duration::from_secs(5);
        assert_eq!(pass(&file, &mut piece, 8 << 20, 4 << 20, within, &stop).unwrap().0, 4 << 20);
        // With no time to read in, each of the two reads ends after its first piece.
        let short = time_within(&large, Duration::ZERO, &stop).unwrap();
        assert_eq!(short.bytes, PIECE as u64);
        assert!(short.first > 0.0 && short.again > 0.0);

        let small = dir.path().join("small.bin");
        File::create(&small).unwrap().set_len(LEAST - 1).unwrap();
        assert_eq!(time(&small, &stop), Err(Unknown::Skipped));
        assert!(matches!(time(&dir.path().join("missing.bin"), &stop), Err(Unknown::Windows(_))));
        // A stop ends it without a figure.
        stop.store(true, Ordering::Relaxed);
        assert_eq!(time(&large, &stop), Err(Unknown::Skipped));
    }
}
