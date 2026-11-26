//! Opens a patch file and gives access to its compressed contents.
//!
//! A patch file is a block that starts with `WzPatch\x1A`, a version number and a checksum,
//! followed by compressed data. The block is either the whole file (`.patch`) or sits near the
//! end of a self-patching `.exe`, where a trailer says where it is.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::DeflateDecoder;

use crate::crc;
use crate::error::{Error, Result};

const MAGIC: &[u8; 8] = b"WzPatch\x1A";
pub(crate) const HEADER_LEN: u64 = 16;
const TRAILER_MARK: u64 = 0xf2f7_fbf3;

#[derive(Debug, Clone)]
pub struct PatchFile {
    path: PathBuf,
    block_offset: u64,
    block_len: u64,
    stored_crc: u32,
}

impl PatchFile {
    /// Locates the patch block and checks its signature. Does not read the whole file.
    pub fn open(path: &Path) -> Result<PatchFile> {
        let mut file = File::open(path).map_err(|e| Error::file("open", path, e))?;
        let file_len = file.metadata().map_err(|e| Error::file("inspect", path, e))?.len();
        let read = |e| Error::file("read", path, e);

        let mut start = [0u8; 2];
        if file_len < HEADER_LEN || file.read_exact(&mut start).is_err() {
            return Err(Error::NotAPatchFile);
        }

        let is_exe = &start == b"MZ";
        let (block_offset, block_len) = match trailer(&mut file, file_len, is_exe).map_err(read)? {
            Some(found) => found,
            None if is_exe => return Err(Error::NotAPatchFile),
            None => (0, file_len),
        };
        if block_len < HEADER_LEN {
            return Err(Error::NotAPatchFile);
        }

        let mut header = [0u8; HEADER_LEN as usize];
        file.seek(SeekFrom::Start(block_offset)).map_err(read)?;
        file.read_exact(&mut header).map_err(read)?;
        if &header[..8] != MAGIC {
            return Err(Error::NotAPatchFile);
        }
        let stored_crc = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);

        Ok(PatchFile { path: path.to_path_buf(), block_offset, block_len, stored_crc })
    }

    /// Identifies this patch file among others: its stored checksum and size.
    pub fn id(&self) -> String {
        id_of(self.stored_crc, self.block_len)
    }

    /// The id of a patch file that is `len` bytes long and begins with `start`, for telling
    /// which file a server has before all of it is fetched. `None` when that is not how a
    /// patch file begins: one that sits inside an `.exe` has its header further in.
    pub(crate) fn id_from_start(start: &[u8], len: u64) -> Option<String> {
        let header = start.get(..HEADER_LEN as usize)?;
        let stored_crc = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
        (&header[..8] == MAGIC).then(|| id_of(stored_crc, len))
    }

    /// Bytes covered by [`PatchFile::verify`].
    pub fn payload_len(&self) -> u64 {
        self.block_len - HEADER_LEN
    }

    /// Reads the whole compressed payload and compares its checksum with the one in the header.
    /// A mismatch means the download is incomplete or damaged.
    pub fn verify(&self, stop: &AtomicBool, mut on_bytes: impl FnMut(u64)) -> Result<()> {
        let mut payload = self.payload()?;
        let mut buffer = vec![0u8; 1 << 20];
        let mut checksum = 0u32;
        let mut done = 0u64;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Stopped);
            }
            let count = payload.read(&mut buffer).map_err(|e| Error::file("read", &self.path, e))?;
            if count == 0 {
                break;
            }
            checksum = crc::update(checksum, &buffer[..count]);
            done += count as u64;
            on_bytes(done);
        }
        if done != self.payload_len() {
            return Err(Error::corrupt("patch file is shorter than its header says"));
        }
        if checksum != self.stored_crc {
            return Err(Error::corrupt(format!(
                "payload checksum {checksum:08x} does not match the header's {:08x}",
                self.stored_crc
            )));
        }
        Ok(())
    }

    /// Starts decompressing from the beginning. Every pass over the contents opens a fresh
    /// stream; there is no rewinding to get wrong.
    pub fn stream(&self) -> Result<PatchStream> {
        let mut payload = self.payload()?;
        // The payload normally starts with a two-byte zlib header. If those bytes do not form
        // one, they are data.
        let mut zlib = [0u8; 2];
        let has_header = match payload.read_exact(&mut zlib) {
            Ok(()) => zlib[0] == 0x78 && (u16::from(zlib[0]) << 8 | u16::from(zlib[1])) % 31 == 0,
            Err(_) => false,
        };
        if !has_header {
            payload = self.payload()?;
        }
        Ok(PatchStream { inner: BufReader::with_capacity(1 << 16, DeflateDecoder::new(payload)), position: 0 })
    }

    fn payload(&self) -> Result<io::Take<BufReader<File>>> {
        let open = |e| Error::file("open", &self.path, e);
        let mut file = File::open(&self.path).map_err(open)?;
        file.seek(SeekFrom::Start(self.block_offset + HEADER_LEN)).map_err(open)?;
        Ok(BufReader::with_capacity(1 << 20, file).take(self.payload_len()))
    }
}

/// The id of the patch block that carries `stored_crc` and is `block_len` bytes long.
fn id_of(stored_crc: u32, block_len: u64) -> String {
    format!("{stored_crc:08x}-{block_len}")
}

/// Reads the trailer that marks an embedded patch block: `(offset, length)` of the block.
fn trailer(file: &mut File, file_len: u64, try_short_form: bool) -> io::Result<Option<(u64, u64)>> {
    // Short form (exe only): block length, notice length and marker as three 32-bit values.
    if try_short_form && file_len >= 12 {
        let mut tail = [0u8; 12];
        file.seek(SeekFrom::Start(file_len - 12))?;
        file.read_exact(&mut tail)?;
        let word = |i: usize| u64::from(u32::from_le_bytes([tail[i], tail[i + 1], tail[i + 2], tail[i + 3]]));
        if word(8) == TRAILER_MARK {
            return Ok(block_before(file_len, 12, word(0), word(4)));
        }
    }
    // Long form: the same three values as 64-bit.
    if file_len >= 24 {
        let mut tail = [0u8; 24];
        file.seek(SeekFrom::Start(file_len - 24))?;
        file.read_exact(&mut tail)?;
        let word = |i: usize| u64::from_le_bytes(tail[i..i + 8].try_into().unwrap());
        if word(16) == TRAILER_MARK {
            return Ok(block_before(file_len, 24, word(0), word(8)));
        }
    }
    Ok(None)
}

fn block_before(file_len: u64, trailer_len: u64, block_len: u64, notice_len: u64) -> Option<(u64, u64)> {
    let end = file_len.checked_sub(trailer_len)?.checked_sub(notice_len)?;
    Some((end.checked_sub(block_len)?, block_len))
}

/// The decompressed contents of a patch file, read front to back.
pub struct PatchStream {
    inner: BufReader<DeflateDecoder<io::Take<BufReader<File>>>>,
    position: u64,
}

impl PatchStream {
    /// Bytes read so far.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// The next byte, or `None` at the end of the contents.
    pub(crate) fn next_byte(&mut self) -> Result<Option<u8>> {
        let mut byte = [0u8; 1];
        loop {
            match self.inner.read(&mut byte) {
                Ok(0) => return Ok(None),
                Ok(_) => {
                    self.position += 1;
                    return Ok(Some(byte[0]));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(decode_error(e)),
            }
        }
    }

    pub fn read_exact(&mut self, buffer: &mut [u8]) -> Result<()> {
        self.inner.read_exact(buffer).map_err(decode_error)?;
        self.position += buffer.len() as u64;
        Ok(())
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32> {
        let mut bytes = [0u8; 4];
        self.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// Reads a length that the format stores as a signed 32-bit number; negative is invalid.
    pub(crate) fn read_len(&mut self) -> Result<u64> {
        let value = self.read_u32()? as i32;
        u64::try_from(value).map_err(|_| Error::corrupt(format!("negative length {value}")))
    }

    /// Reads a length-prefixed ASCII string no longer than `max` bytes.
    pub(crate) fn read_string(&mut self, max: u64) -> Result<String> {
        let len = self.read_len()?;
        if len > max {
            return Err(Error::corrupt(format!("string of {len} bytes exceeds the limit of {max}")));
        }
        let mut bytes = vec![0u8; len as usize];
        self.read_exact(&mut bytes)?;
        if !bytes.is_ascii() {
            return Err(Error::corrupt("file name is not ASCII"));
        }
        Ok(String::from_utf8(bytes).expect("ASCII is valid UTF-8"))
    }

    /// Reads and discards `count` bytes.
    pub fn skip(&mut self, count: u64) -> Result<()> {
        let copied = io::copy(&mut (&mut self.inner).take(count), &mut io::sink()).map_err(decode_error)?;
        self.position += copied;
        if copied != count {
            return Err(Error::corrupt("patch contents end in the middle of a file"));
        }
        Ok(())
    }
}

fn decode_error(source: io::Error) -> Error {
    match source.kind() {
        io::ErrorKind::UnexpectedEof => Error::corrupt("patch contents end unexpectedly"),
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
            Error::corrupt(format!("compressed data is invalid: {source}"))
        }
        _ => Error::io("read patch file", source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_told_from_the_start_of_a_file_is_the_id_of_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.patch");
        let mut bytes = MAGIC.to_vec();
        bytes.extend(2u32.to_le_bytes());
        bytes.extend(0xDEAD_BEEFu32.to_le_bytes());
        bytes.extend([7u8; 100]);
        std::fs::write(&path, &bytes).unwrap();

        let whole = PatchFile::open(&path).unwrap().id();
        assert_eq!(whole, "deadbeef-116");
        assert_eq!(PatchFile::id_from_start(&bytes[..16], 116), Some(whole.clone()));
        // More than the header tells no more than the header.
        assert_eq!(PatchFile::id_from_start(&bytes, 116), Some(whole));
        // The same beginning of a longer file is another file.
        assert_eq!(PatchFile::id_from_start(&bytes[..16], 117).as_deref(), Some("deadbeef-117"));
        // Less than a header says nothing, and neither does the start of a program, which
        // carries its patch further in.
        assert_eq!(PatchFile::id_from_start(&bytes[..15], 116), None);
        assert_eq!(PatchFile::id_from_start(b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xFF\xFF\x00\x00", 116), None);
    }
}
