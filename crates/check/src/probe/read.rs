//! How fast the game's drive hands over a large file.
//!
//! One file is read from its start, past Windows' file cache so that it is the drive that
//! answers, and then the same bytes once more. On a disk of the PC itself the two take
//! about as long. Where the second time is many times faster, something below Windows kept
//! the bytes: that is what a seat without a disk does with what it fetched from its server.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use mfp_engine::storage;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN};

use super::io_error;
use crate::facts::{Fact, Reads, Unknown};

/// How much is read in one go.
pub const SAMPLE: u64 = 256 << 20;
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

    let first = pass(&open()?, piece, bytes, stop)?;
    let again = pass(&open()?, piece, bytes, stop)?;
    let seek_ms = storage::look(sample.parent().unwrap_or(sample), Some(sample)).seek_ms;
    Ok(Reads { bytes, first: bytes as f64 / first, again: bytes as f64 / again, seek_ms })
}

/// Reads the first `bytes` of a file and returns the seconds that took.
fn pass(file: &File, piece: &mut [u8], bytes: u64, stop: &AtomicBool) -> Fact<f64> {
    use std::os::windows::fs::FileExt;

    let started = Instant::now();
    let mut done = 0u64;
    while done < bytes {
        if stop.load(Ordering::Relaxed) {
            return Err(Unknown::Skipped);
        }
        match file.seek_read(piece, done) {
            Ok(0) => return Err(Unknown::NoAnswer),
            Ok(count) => done += count as u64,
            Err(err) => return Err(io_error(&err)),
        }
    }
    // A read that took no time at all would make the speed a number without meaning.
    Ok(started.elapsed().as_secs_f64().max(1e-6))
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

        let small = dir.path().join("small.bin");
        File::create(&small).unwrap().set_len(LEAST - 1).unwrap();
        assert_eq!(time(&small, &stop), Err(Unknown::Skipped));
        assert!(matches!(time(&dir.path().join("missing.bin"), &stop), Err(Unknown::Windows(_))));
        // A stop ends it without a figure.
        stop.store(true, Ordering::Relaxed);
        assert_eq!(time(&large, &stop), Err(Unknown::Skipped));
    }
}
