//! Reads what a patch file will do, without doing it.
//!
//! The decompressed contents are a list of entries. Each starts with a file name ended by a
//! byte that says what happens to that file: `0` create, `1` rebuild from the old file,
//! `2` delete. Newer patch files put a table of original-file checksums in front of the list;
//! older ones store the original checksum inside each rebuild entry.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::container::{PatchFile, PatchStream};
use crate::error::{Error, Result};
use crate::preflight;

/// Windows `MAX_PATH`; file names in a patch file never exceed it.
const MAX_NAME: u64 = 260;
/// Entries, and original files listed, in one patch file. Real patches have about a
/// thousand; the limit keeps a made-up file from filling memory with millions.
const MAX_ENTRIES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Original checksums are stored per rebuild entry, and a rebuild copies only from the
    /// file it replaces.
    PerEntry,
    /// A checksum table comes first, and a rebuild may copy from any original file.
    Table,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CreateDir,
    Create {
        len: u64,
        crc: u32,
    },
    Rebuild {
        /// Checksum the file must have before the patch, when the patch file states one.
        old_crc: Option<u32>,
        new_crc: u32,
        new_len: u64,
        /// Original files this rebuild copies from.
        sources: BTreeSet<String>,
    },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Name as written in the patch file, with `\` separators.
    pub name: String,
    pub action: Action,
}

impl Entry {
    /// Size of the new file this entry writes; zero for an entry that writes none.
    pub fn new_len(&self) -> u64 {
        match &self.action {
            Action::Create { len, .. } => *len,
            Action::Rebuild { new_len, .. } => *new_len,
            Action::CreateDir | Action::Delete => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub layout: Layout,
    /// Checksums of original files from the leading table (empty for [`Layout::PerEntry`]).
    pub originals: BTreeMap<String, u32>,
    pub entries: Vec<Entry>,
    /// Size of the decompressed contents.
    pub contents_len: u64,
}

impl Plan {
    /// Bytes of new files the patch writes, which is what the work folder must hold.
    pub fn new_bytes(&self) -> u64 {
        self.entries.iter().map(Entry::new_len).sum()
    }
}

/// One instruction inside a rebuild entry.
///
/// Public as part of reading a patch file entry by entry, for tools that look inside one.
#[derive(Debug)]
pub enum Op {
    /// The next `len` bytes of the patch contents go into the new file. The handler must
    /// consume exactly that many bytes.
    Literal {
        len: u64,
    },
    Fill {
        byte: u8,
        len: u64,
    },
    Copy {
        from: String,
        start: u64,
        len: u64,
    },
}

/// What stands at the start of an entry.
///
/// Public as part of reading a patch file entry by entry, for tools that look inside one.
pub enum Header {
    CreateDir,
    Create { len: u64, crc: u32 },
    Rebuild { old_crc: Option<u32>, new_crc: u32 },
    Delete,
}

/// Reads the whole patch file once and lists everything it will do.
pub fn scan(patch: &PatchFile, stop: &AtomicBool) -> Result<Plan> {
    let (mut stream, layout, originals) = open_contents(patch)?;
    let mut entries = Vec::new();
    // Windows treats names that differ only in case as one file, so two entries for one
    // file are refused in either spelling.
    let mut seen = std::collections::HashSet::new();

    while let Some((name, header)) = read_header(&mut stream, layout)? {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let action = match header {
            Header::CreateDir => Action::CreateDir,
            Header::Delete => Action::Delete,
            Header::Create { len, crc } => {
                stream.skip(len)?;
                Action::Create { len, crc }
            }
            Header::Rebuild { old_crc, new_crc } => {
                let mut new_len = 0u64;
                let mut sources = BTreeSet::new();
                read_ops(&mut stream, layout, &name, |op, stream| {
                    match op {
                        Op::Literal { len } => {
                            stream.skip(len)?;
                            new_len += len;
                        }
                        Op::Fill { len, .. } => new_len += len,
                        Op::Copy { from, len, .. } => {
                            new_len += len;
                            sources.insert(from);
                        }
                    }
                    Ok(())
                })?;
                let old_crc = old_crc.or_else(|| originals.get(&name).copied());
                Action::Rebuild { old_crc, new_crc, new_len, sources }
            }
        };
        if !seen.insert(name.to_ascii_lowercase().replace('/', "\\")) {
            return Err(Error::corrupt(format!("{name} appears twice")));
        }
        if entries.len() >= MAX_ENTRIES {
            return Err(Error::corrupt("more entries than any patch has"));
        }
        entries.push(Entry { name, action });
    }

    Ok(Plan { layout, originals, entries, contents_len: stream.position() })
}

/// Opens the contents and works out which layout they use.
///
/// This is where reading a patch file entry by entry starts: [`next_entry`] gives each entry in
/// turn and [`read_ops`] the instructions of one that rebuilds a file. Public for tools that
/// look inside a patch file.
pub fn open_contents(patch: &PatchFile) -> Result<(PatchStream, Layout, BTreeMap<String, u32>)> {
    let mut stream = patch.stream()?;
    match read_table(&mut stream) {
        Ok(originals) => Ok((stream, Layout::Table, originals)),
        // No table: the bytes just read were the first entry. Start over from a new stream.
        Err(Error::CorruptPatch(_)) => Ok((patch.stream()?, Layout::PerEntry, BTreeMap::new())),
        Err(other) => Err(other),
    }
}

fn read_table(stream: &mut PatchStream) -> Result<BTreeMap<String, u32>> {
    let count = stream.read_len()?;
    if count > MAX_ENTRIES as u64 {
        return Err(Error::corrupt("more original files listed than any patch has"));
    }
    let mut originals = BTreeMap::new();
    for _ in 0..count {
        let name = stream.read_string(MAX_NAME)?;
        check_name(&name)?;
        let crc = stream.read_u32()?;
        if originals.insert(name, crc).is_some() {
            return Err(Error::corrupt("an original file is listed twice"));
        }
    }
    Ok(originals)
}

/// Reads the next entry's name and header, or `None` at the end of the contents.
pub(crate) fn read_header(stream: &mut PatchStream, layout: Layout) -> Result<Option<(String, Header)>> {
    let mut name = Vec::new();
    let kind = loop {
        match stream.next_byte()? {
            None if name.is_empty() => return Ok(None),
            None => return Err(Error::corrupt("patch contents end in the middle of a file name")),
            Some(byte @ 0..=2) => break byte,
            Some(byte) => name.push(byte),
        }
        if name.len() as u64 > MAX_NAME {
            return Err(Error::corrupt("file name is too long"));
        }
    };
    if !name.is_ascii() {
        return Err(Error::corrupt("file name is not ASCII"));
    }
    let name = String::from_utf8(name).expect("ASCII is valid UTF-8");
    check_name(&name)?;

    let header = match kind {
        // A name without an extension is a folder to create.
        0 if !has_extension(&name) => Header::CreateDir,
        0 => {
            let len = stream.read_len()?;
            let crc = stream.read_u32()?;
            Header::Create { len, crc }
        }
        1 => {
            let old_crc = match layout {
                Layout::PerEntry => Some(stream.read_u32()?),
                Layout::Table => None,
            };
            Header::Rebuild { old_crc, new_crc: stream.read_u32()? }
        }
        _ => Header::Delete,
    };
    Ok(Some((name, header)))
}

/// What a later pass over the contents answers when they are not what the first pass read:
/// the file was swapped or damaged in between.
pub(crate) fn changed() -> Error {
    Error::corrupt("patch file changed while it was being used")
}

/// Reads the next entry's name and header on a later pass over the contents, and makes sure
/// it is the entry the plan has in that place.
///
/// Public as part of reading a patch file entry by entry, for tools that look inside one.
pub fn next_entry(stream: &mut PatchStream, layout: Layout, expected: &str) -> Result<(String, Header)> {
    match read_header(stream, layout)? {
        Some((name, header)) if name == expected => Ok((name, header)),
        _ => Err(changed()),
    }
}

/// Reads the instructions of a rebuild entry and hands each to `handle`.
///
/// Public as part of reading a patch file entry by entry, for tools that look inside one.
pub fn read_ops(
    stream: &mut PatchStream,
    layout: Layout,
    entry_name: &str,
    mut handle: impl FnMut(Op, &mut PatchStream) -> Result<()>,
) -> Result<()> {
    loop {
        let command = stream.read_u32()?;
        let op = match command >> 28 {
            _ if command == 0 => return Ok(()),
            0x8 => Op::Literal { len: u64::from(command & 0x0fff_ffff) },
            0xC => Op::Fill { byte: command as u8, len: u64::from((command & 0x0fff_ff00) >> 8) },
            0x0..=0x7 => {
                let start = stream.read_len()?;
                let from = match layout {
                    Layout::Table => {
                        let from = stream.read_string(MAX_NAME)?;
                        check_name(&from)?;
                        from
                    }
                    Layout::PerEntry => entry_name.to_owned(),
                };
                Op::Copy { from, start, len: u64::from(command) }
            }
            _ => return Err(Error::corrupt(format!("unknown instruction {command:#010x} in {entry_name}"))),
        };
        handle(op, stream)?;
    }
}

fn has_extension(name: &str) -> bool {
    let last = name.rsplit(['\\', '/']).next().unwrap_or(name);
    matches!(last.rfind('.'), Some(dot) if dot + 1 < last.len())
}

/// Rejects names that would reach outside the install folder.
fn check_name(name: &str) -> Result<()> {
    relative_path(name).map(drop)
}

/// Turns a name from the patch file into a path relative to the install folder.
///
/// The patch file decides which files get written and deleted, so its names are not trusted:
/// absolute paths, drive letters and `..` are refused.
pub fn relative_path(name: &str) -> Result<PathBuf> {
    let unsafe_name = || Error::UnsafePath(name.to_owned());
    let trimmed = name.strip_suffix(['\\', '/']).unwrap_or(name);
    if trimmed.is_empty() {
        return Err(unsafe_name());
    }
    let mut path = PathBuf::new();
    for part in trimmed.split(['\\', '/']) {
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.contains(':')
            // Short names such as `MAPLE-~1` are a second spelling of a long name; one would
            // get past every check made on names, including the one on the engine's own files.
            || part.contains('~')
            || is_device_name(part)
            || part.ends_with([' ', '.'])
            || part.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'));
        if bad {
            return Err(unsafe_name());
        }
        path.push(part);
    }
    // The engine's own files in the install folder are not the patch's to touch: a patch
    // that could remove the apply journal could make a failed run impossible to undo.
    let first = trimmed.split(['\\', '/']).next().unwrap_or_default();
    if first.eq_ignore_ascii_case(preflight::WORK_DIR_NAME) || first.eq_ignore_ascii_case(preflight::LOCK_FILE_NAME) {
        return Err(unsafe_name());
    }
    Ok(path)
}

/// Whether Windows may take this path component for a device instead of a file: `NUL`,
/// `COM1` and the like, with or without an extension.
fn is_device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix)
            .is_some_and(|rest| rest.len() == 1 && rest != "0" && rest.bytes().all(|b| b.is_ascii_digit()))
    };
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

/// Whether a name from the patch file names a folder: those end in a separator.
pub fn names_folder(name: &str) -> bool {
    name.ends_with(['\\', '/'])
}

/// A name in the form used to compare names: Windows treats `Data\A.wz` and `data/a.wz` as
/// the same file.
///
/// Public for tools that look inside a patch file, where one file comes under both spellings.
pub fn name_key(name: &str) -> String {
    name.trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase()
}

/// Reads only the table of original-file checksums at the front of a patch file.
///
/// Works on a file that holds just the beginning of a patch, which makes it cheap to ask
/// "is this patch made for my install?" before downloading all of it. Returns `None` when the
/// table cannot be read: the file is cut off inside the table, or uses the older layout.
pub(crate) fn originals_of(patch: &PatchFile) -> Result<Option<BTreeMap<String, u32>>> {
    match read_table(&mut patch.stream()?) {
        Ok(originals) => Ok(Some(originals)),
        Err(Error::CorruptPatch(_)) => Ok(None),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_become_relative_paths() {
        assert_eq!(relative_path("MapleStory.exe").unwrap(), PathBuf::from("MapleStory.exe"));
        assert_eq!(
            relative_path(r"Data\Skill\Skill_000.wz").unwrap(),
            ["Data", "Skill", "Skill_000.wz"].iter().collect::<PathBuf>()
        );
        // A trailing separator marks a folder.
        assert_eq!(relative_path(r"Data\Old\").unwrap(), ["Data", "Old"].iter().collect::<PathBuf>());
    }

    #[test]
    fn names_that_leave_the_install_folder_are_refused() {
        for name in [
            r"..\Windows\System32\evil.dll",
            r"Data\..\..\evil.dll",
            r"C:\Windows\evil.dll",
            r"\Windows\evil.dll",
            r"\\server\share\evil.dll",
            "Data//x.wz",
            r"Data\x.wz:stream",
            r"Data\.\x.wz",
            "",
            r"\",
        ] {
            assert!(matches!(relative_path(name), Err(Error::UnsafePath(_))), "{name:?} was accepted");
        }
    }

    #[test]
    fn device_names_are_refused() {
        for name in ["NUL", "nul.txt", r"Data\CON", r"Data\com1.wz", r"Data\LPT9", r"aux\x.wz", "PRN.tar.gz"] {
            assert!(matches!(relative_path(name), Err(Error::UnsafePath(_))), "{name:?} was accepted");
        }
        for name in [r"Data\Console.wz", r"Data\Null.wz", r"Data\COM10.wz", r"Data\Com.wz", "LPT.txt"] {
            assert!(relative_path(name).is_ok(), "{name:?} was refused");
        }
    }

    #[test]
    fn names_of_the_engines_own_files_are_refused() {
        for name in [
            r".maple-fast-patch\applying.log",
            r".Maple-Fast-Patch\committed",
            ".maple-fast-patch/new/Data/x.wz",
            ".maple-fast-patch",
            ".maple-fast-patch.lock",
            r".MAPLE-FAST-PATCH.LOCK\",
        ] {
            assert!(matches!(relative_path(name), Err(Error::UnsafePath(_))), "{name:?} was accepted");
        }
        // Nor the short name Windows gives the work folder, or any other short name.
        for name in [r"MAPLE-~1\applying.log", "MAPLE-~1.LOC", r"Data\SKILL_~1.WZ"] {
            assert!(matches!(relative_path(name), Err(Error::UnsafePath(_))), "{name:?} was accepted");
        }
        // A folder that merely starts the same way is an ordinary name.
        assert!(relative_path(r".maple-fast-patch-notes\readme.txt").is_ok());
        assert!(relative_path(r"Data\.maple-fast-patch\x.wz").is_ok());
    }

    #[test]
    fn extension_decides_between_file_and_folder() {
        assert!(has_extension(r"Data\Base\Base_000.wz"));
        assert!(has_extension("MapleStory.exe"));
        assert!(!has_extension(r"Data\Packs"));
        assert!(!has_extension(r"Data.v2\Packs"));
        assert!(!has_extension("Folder."));
    }
}
