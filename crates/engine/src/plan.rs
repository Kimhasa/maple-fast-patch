//! Reads what a patch file will do, without doing it.
//!
//! The decompressed contents are a list of entries. Each starts with a file name ended by a
//! byte that says what happens to that file: `0` create, `1` rebuild from the old file,
//! `2` delete. Newer patch files put a table of original-file checksums in front of the list;
//! older ones store the original checksum inside each rebuild entry.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::container::{HEADER_LEN, PatchFile, PatchStream};
use crate::error::{Error, Result};
use crate::preflight;

/// Windows `MAX_PATH`; file names in a patch file never exceed it.
const MAX_NAME: u64 = 260;
/// The longest one part of a name can be: NTFS gives no file or folder a longer name,
/// however short the whole path is.
const MAX_PART: usize = 255;
/// Entries, and original files listed, in one patch file. Real patches have about a
/// thousand; the limit keeps a made-up file from filling memory with millions.
const MAX_ENTRIES: usize = 100_000;
/// Original files named as what a rebuild copies from, counted over all the rebuilds of one
/// patch file. Real patches name a few thousand. Every name is kept, and a made-up file of a
/// few megabytes can name hundreds of millions.
const MAX_SOURCES: usize = 1_000_000;

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
    /// For each original file that a rebuild copies from, where the copy that reaches
    /// furthest into it ends: the least that original has to be long. An original that is
    /// the file the patch was made for and is shorter than this shows a patch that lies.
    pub reach: BTreeMap<String, u64>,
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
    let mut seen = HashSet::new();
    // What the plan has grown to, which a made-up file must not be able to drive up
    // without end.
    let (mut sources_named, mut new_bytes) = (0usize, 0u64);
    let mut reach: BTreeMap<String, u64> = BTreeMap::new();

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
                let (new_len, copied) = scan_ops(&mut stream, layout, &name, MAX_SOURCES - sources_named, stop)?;
                sources_named += copied.len();
                for (source, end) in &copied {
                    let furthest = reach.entry(source.clone()).or_default();
                    *furthest = (*furthest).max(*end);
                }
                let old_crc = old_crc.or_else(|| originals.get(&name).copied());
                Action::Rebuild { old_crc, new_crc, new_len, sources: copied.into_keys().collect() }
            }
        };
        if !seen.insert(name.to_ascii_lowercase().replace('/', "\\")) {
            return Err(Error::corrupt(format!("{name} appears twice")));
        }
        if entries.len() >= MAX_ENTRIES {
            return Err(Error::corrupt("more entries than any patch has"));
        }
        let entry = Entry { name, action };
        new_bytes =
            new_bytes.checked_add(entry.new_len()).ok_or_else(|| Error::corrupt("new files beyond any drive"))?;
        entries.push(entry);
    }
    refuse_file_that_is_a_folder(&entries)?;

    Ok(Plan { layout, originals, entries, contents_len: stream.position(), reach })
}

/// Reads the instructions of one rebuild for what the plan keeps of them: how long the new
/// file is, and which original files it is copied from, of which there may be `most`, each
/// with where the copy that reaches furthest into it ends.
///
/// One entry can hold as many instructions as the patch file has room for, so a pause is
/// looked for at every one of them.
fn scan_ops(
    stream: &mut PatchStream,
    layout: Layout,
    name: &str,
    most: usize,
    stop: &AtomicBool,
) -> Result<(u64, BTreeMap<String, u64>)> {
    let mut new_len = 0u64;
    let mut sources: BTreeMap<String, u64> = BTreeMap::new();
    read_ops(stream, layout, name, |op, stream| {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let len = match op {
            Op::Literal { len } => {
                stream.skip(len)?;
                len
            }
            Op::Fill { len, .. } => len,
            Op::Copy { from, start, len } => {
                // Both are under 2^32, so their sum is a number.
                let furthest = sources.entry(from).or_default();
                *furthest = (*furthest).max(start + len);
                if sources.len() > most {
                    return Err(Error::corrupt("copies from more original files than any patch does"));
                }
                len
            }
        };
        new_len = new_len.checked_add(len).ok_or_else(|| Error::corrupt(format!("{name} is longer than any file")))?;
        Ok(())
    })?;
    Ok((new_len, sources))
}

/// Refuses a patch that needs one name to be a file and a folder at once: a file it writes,
/// and a folder it makes or puts something into. Nothing can be done in an order that gives
/// it both. The safe way would find out while it applies and undo everything; the immediate
/// way would find out with the first of the two in its place for good.
fn refuse_file_that_is_a_folder(entries: &[Entry]) -> Result<()> {
    let writes_file = |entry: &&Entry| matches!(entry.action, Action::Create { .. } | Action::Rebuild { .. });
    let files: HashSet<String> = entries.iter().filter(writes_file).map(|entry| name_key(&entry.name)).collect();
    for entry in entries.iter().filter(|entry| entry.action != Action::Delete) {
        let key = name_key(&entry.name);
        // The folders this entry needs: every one above it, and itself if it makes one.
        let itself = (entry.action == Action::CreateDir).then_some(key.as_str());
        if let Some(both) = folders_above(&key).chain(itself).find(|folder| files.contains(*folder)) {
            return Err(Error::corrupt(format!("{both} is a file and a folder in one patch")));
        }
    }
    Ok(())
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

fn read_table<R: Read>(stream: &mut PatchStream<R>) -> Result<BTreeMap<String, u32>> {
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
fn read_header(stream: &mut PatchStream, layout: Layout) -> Result<Option<(String, Header)>> {
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
            // A name that ends the way a folder's does would be a second spelling of the
            // file's name, and get one file past the check that no file comes twice.
            if names_folder(&name) {
                return Err(Error::corrupt(format!("{name} is rebuilt, and named the way a folder is")));
            }
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
            || part.len() > MAX_PART
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

/// The folders above a name that is in the form of [`name_key`], nearest first: `data\skill`
/// and `data` for `data\skill\a.wz`.
///
/// What is asked about a name and everything above it is asked this way, of a set, and not
/// of every other entry in turn: a patch may have a hundred thousand entries.
pub(crate) fn folders_above(key: &str) -> impl Iterator<Item = &str> {
    key.rmatch_indices('\\').map(|(at, _)| &key[..at])
}

/// What the first bytes of a patch file tell of the original files it was built from.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// The table of original-file checksums, whole. It may name nothing: such patches exist.
    Table(BTreeMap<String, u32>),
    /// The table could not be read to its end: the bytes stop before it does. More of the
    /// file may hold the rest.
    CutOff,
    /// What stands where the table would be is none, however much of the file is read: a
    /// patch file of the older layout begins with its first entry.
    NoTable,
}

/// Reads only the table of original-file checksums at the front of a patch file, out of the
/// first bytes of that file, in memory.
///
/// That makes it cheap to ask "is this patch made for my install?" before downloading all
/// of it: the beginning of the file is fetched, and nothing is written for it. The table is
/// read the way [`open_contents`] reads it from a whole file.
///
/// Fails with [`Error::NotAPatchFile`] when the bytes are a header's worth and not how a
/// patch file begins, and with [`Error::UnsafePath`] when the table names a file outside
/// the install folder.
pub fn read_start(bytes: &[u8]) -> Result<Start> {
    // Before the header is there, nothing says what this is.
    if (bytes.len() as u64) < HEADER_LEN {
        return Ok(Start::CutOff);
    }
    let mut stream = PatchStream::of_start(bytes)?;
    match read_table(&mut stream) {
        Ok(originals) => Ok(Start::Table(originals)),
        // Running out of bytes is the one thing that more bytes can mend.
        Err(Error::CorruptPatch(_)) if stream.ran_out() => Ok(Start::CutOff),
        Err(Error::CorruptPatch(_)) => Ok(Start::NoTable),
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

    #[test]
    fn part_of_a_name_longer_than_a_file_name_can_be_is_refused() {
        let part = |len: usize| "n".repeat(len);
        assert!(relative_path(&part(255)).is_ok());
        assert!(relative_path(&format!(r"Data\{}.wz", part(251))).is_ok());
        for name in [part(256), format!(r"Data\{}", part(256)), format!(r"{}\x.wz", part(256))] {
            assert!(
                matches!(relative_path(&name), Err(Error::UnsafePath(_))),
                "{} characters were accepted",
                name.len()
            );
        }
    }

    /// The beginning of a patch file that holds `contents`, compressed as a real one is.
    fn start_of(contents: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut packed = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        packed.write_all(contents).unwrap();
        let mut bytes = b"WzPatch\x1A".to_vec();
        bytes.extend([0u8; 8]);
        bytes.extend(packed.finish().unwrap());
        bytes
    }

    /// The contents of a patch file whose table names `files`, with an entry behind the
    /// table as in a real file.
    fn table_of(files: &[(&str, u32)]) -> Vec<u8> {
        let mut contents = (files.len() as i32).to_le_bytes().to_vec();
        for (name, crc) in files {
            contents.extend((name.len() as i32).to_le_bytes());
            contents.extend(name.as_bytes());
            contents.extend(crc.to_le_bytes());
        }
        contents.extend(b"Data\\Next.wz\x02");
        contents
    }

    #[test]
    fn table_is_read_from_the_first_bytes_of_a_patch_file_as_from_the_whole_file() {
        let files: Vec<(String, u32)> = (0..400).map(|at| (format!(r"Data\Map\Map_{at:03}.wz"), at * 7919)).collect();
        let named: Vec<(&str, u32)> = files.iter().map(|(name, crc)| (name.as_str(), *crc)).collect();
        let bytes = start_of(&table_of(&named));
        let Start::Table(table) = read_start(&bytes).unwrap() else { panic!("no table was read") };
        assert_eq!(table.len(), 400);
        assert_eq!(table[r"Data\Map\Map_007.wz"], 7 * 7919);

        // The same bytes as a file on disk give the same table.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whole.patch");
        std::fs::write(&path, &bytes).unwrap();
        let (_, layout, originals) = open_contents(&PatchFile::open(&path).unwrap()).unwrap();
        assert_eq!((layout, originals), (Layout::Table, table));

        // Cut off anywhere before the table ends, more bytes may mend it: before the
        // header is whole, right behind it, and inside the table.
        for cut in [0, 10, 16, 17, 40, bytes.len() / 2] {
            assert_eq!(read_start(&bytes[..cut]).unwrap(), Start::CutOff, "cut at {cut}");
        }
        // A table that names nothing is a table too, and so is one whose compressed bytes
        // come without the two bytes zlib puts in front.
        assert_eq!(read_start(&start_of(&table_of(&[]))).unwrap(), Start::Table(BTreeMap::new()));
        let mut bare = bytes[..16].to_vec();
        bare.extend(&bytes[18..]);
        assert!(matches!(read_start(&bare), Ok(Start::Table(table)) if table.len() == 400));
    }

    #[test]
    fn first_bytes_without_a_table_are_told_from_those_that_are_cut_off() {
        // The older layout begins with the name of its first entry, and no more bytes
        // turn that into a table.
        let older = start_of(b"Data\\Base\\Base_000.wz\x01\x00\x00\x00\x00");
        assert_eq!(read_start(&older).unwrap(), Start::NoTable);
        // Nor do they for a table that names a file twice.
        assert_eq!(read_start(&start_of(&table_of(&[("a.wz", 1), ("a.wz", 2)]))).unwrap(), Start::NoTable);
        // What does not begin as a patch file does is none, and a name that leads out of
        // the install folder is refused as it is in a whole file.
        assert!(matches!(read_start(&[0u8; 64]), Err(Error::NotAPatchFile)));
        assert!(matches!(
            read_start(b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xFF\xFF\x00\x00"),
            Err(Error::NotAPatchFile)
        ));
        for name in [r"..\Windows\evil.dll", r"C:\Windows\evil.dll", r".maple-fast-patch\x", "NUL"] {
            let bytes = start_of(&table_of(&[("MapleStory.exe", 1), (name, 2)]));
            assert!(matches!(read_start(&bytes), Err(Error::UnsafePath(_))), "{name:?} was accepted");
        }
    }

    #[test]
    fn folders_above_a_name_come_nearest_first() {
        assert_eq!(folders_above(r"data\skill\a.wz").collect::<Vec<_>>(), [r"data\skill", "data"]);
        assert_eq!(folders_above("a.wz").count(), 0);
    }

    #[test]
    fn name_that_is_a_file_and_a_folder_in_one_patch_is_refused() {
        let entry = |name: &str, action: Action| Entry { name: name.to_owned(), action };
        let file = |name: &str| entry(name, Action::Create { len: 1, crc: 0 });
        let refused = |entries: &[Entry]| matches!(refuse_file_that_is_a_folder(entries), Err(Error::CorruptPatch(_)));
        // A file and something below it, in either order and in any spelling.
        assert!(refused(&[file(r"Data\A.wz"), file(r"Data\A.wz\b.wz")]));
        assert!(refused(&[file(r"data/a.WZ/b.wz"), file(r"Data\A.wz")]));
        assert!(refused(&[file(r"Data\A.wz"), entry(r"Data\A.wz\Sub", Action::CreateDir)]));
        // A folder that is made, and a file of its name.
        assert!(refused(&[entry(r"Data\New\", Action::CreateDir), file(r"Data\New")]));
        // A delete below a file takes nothing that the file needs, and a folder with files
        // in it is what every patch has.
        assert!(!refused(&[file(r"Data\A.wz"), entry(r"Data\A.wz\b.wz", Action::Delete)]));
        assert!(!refused(&[entry(r"Data\New", Action::CreateDir), file(r"Data\New\a.wz"), file(r"Data\New\b.wz")]));
    }
}
