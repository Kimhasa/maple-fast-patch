//! Writes made-up patch files, and the numbers they are made from.
//!
//! This file is shared: the tests of the engine use it through `support`, and the tools
//! include it for the made-up world they build to try the app against. It must not need
//! anything but the engine and what compresses a patch file.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::Path;

use mfp_engine::crc;
use mfp_engine::plan::Layout;

/// Small deterministic random number generator, so a failing seed can be replayed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// A number from 0 up to, but not including, `limit`.
    pub fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(len + 8);
        while bytes.len() < len {
            bytes.extend(self.next().to_le_bytes());
        }
        bytes.truncate(len);
        bytes
    }
}

/// One instruction of a rebuild entry.
pub enum Op {
    Literal(Vec<u8>),
    Fill(u8, u32),
    Copy(String, u32, u32),
}

/// The instruction that copies `len` bytes at `start` of the original `from`.
pub fn copy(from: &str, start: usize, len: usize) -> Op {
    Op::Copy(from.to_owned(), start as u32, len as u32)
}

/// Writes patch files in the layout the game's patch builder produces. Every method that
/// adds something gives the builder back, so that a small patch can be written in one go.
pub struct PatchBuilder {
    layout: Layout,
    originals: Vec<(String, u32)>,
    body: Vec<u8>,
}

impl PatchBuilder {
    pub fn new(layout: Layout) -> PatchBuilder {
        PatchBuilder { layout, originals: Vec::new(), body: Vec::new() }
    }

    /// Lists an original file in the checksum table (table layout only).
    pub fn original(&mut self, name: &str, contents: &[u8]) -> &mut Self {
        if !self.originals.iter().any(|(listed, _)| listed == name) {
            self.originals.push((name.to_owned(), crc::update(0, contents)));
        }
        self
    }

    /// How many original files the checksum table lists so far.
    pub fn originals(&self) -> usize {
        self.originals.len()
    }

    pub fn create(&mut self, name: &str, contents: &[u8]) -> &mut Self {
        self.body.extend(name.as_bytes());
        self.body.push(0);
        self.body.extend((contents.len() as u32).to_le_bytes());
        self.body.extend(crc::update(0, contents).to_le_bytes());
        self.body.extend(contents);
        self
    }

    pub fn create_dir(&mut self, name: &str) -> &mut Self {
        self.body.extend(name.as_bytes());
        self.body.push(0);
        self
    }

    pub fn delete(&mut self, name: &str) -> &mut Self {
        self.body.extend(name.as_bytes());
        self.body.push(2);
        self
    }

    /// Adds bytes to the contents exactly as given, for what no real patch builder writes.
    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.body.extend(bytes);
        self
    }

    pub fn rebuild(&mut self, name: &str, old: &[u8], new: &[u8], ops: &[Op]) -> &mut Self {
        self.body.extend(name.as_bytes());
        self.body.push(1);
        if self.layout == Layout::PerEntry {
            self.body.extend(crc::update(0, old).to_le_bytes());
        }
        self.body.extend(crc::update(0, new).to_le_bytes());
        for op in ops {
            match op {
                Op::Literal(data) => {
                    self.body.extend((0x8000_0000 | data.len() as u32).to_le_bytes());
                    self.body.extend(data);
                }
                Op::Fill(byte, len) => self.body.extend((0xC000_0000 | len << 8 | u32::from(*byte)).to_le_bytes()),
                Op::Copy(from, start, len) => {
                    self.body.extend(len.to_le_bytes());
                    self.body.extend(start.to_le_bytes());
                    if self.layout == Layout::Table {
                        self.body.extend((from.len() as u32).to_le_bytes());
                        self.body.extend(from.as_bytes());
                    }
                }
            }
        }
        self.body.extend(0u32.to_le_bytes());
        self
    }

    /// Writes the patch file to `path`.
    pub fn write(&self, path: &Path) {
        fs::write(path, self.bytes()).unwrap();
    }

    pub fn bytes(&self) -> Vec<u8> {
        wrap(&self.contents())
    }

    /// The contents of the patch file before they are compressed: the table of originals,
    /// where the layout has one, and the entries.
    pub fn contents(&self) -> Vec<u8> {
        let mut contents = Vec::new();
        if self.layout == Layout::Table {
            contents.extend((self.originals.len() as u32).to_le_bytes());
            for (name, checksum) in &self.originals {
                contents.extend((name.len() as u32).to_le_bytes());
                contents.extend(name.as_bytes());
                contents.extend(checksum.to_le_bytes());
            }
        }
        contents.extend(&self.body);
        contents
    }
}

/// Makes a patch file of `contents`, whatever they are: compressed, and behind a header
/// with the checksum that is right for them. A test that changes the contents by hand gets
/// a file that passes every check made on the file itself, so that what is tried is the
/// reading of the contents.
pub fn wrap(contents: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(contents).unwrap();
    with_header(&encoder.finish().unwrap())
}

/// Puts the header of a patch file in front of `payload`, which is taken as it is, with the
/// checksum that is right for it.
pub fn with_header(payload: &[u8]) -> Vec<u8> {
    let mut file = b"WzPatch\x1A".to_vec();
    file.extend(2u32.to_le_bytes());
    file.extend(crc::update(0, payload).to_le_bytes());
    file.extend(payload);
    file
}
