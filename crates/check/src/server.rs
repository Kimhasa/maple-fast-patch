//! Asks Nexon's patch server what it has, and reads the start of patch files there.
//!
//! A patch file begins with the checksums of the files it was made from. That start is
//! fetched into memory and read there: nothing is written to this PC for it. With it the
//! folder's version can be told when the table built into this program does not know it,
//! and a patch newer than the table is taken into account.
//!
//! Nothing but Nexon's patch server is asked, and nothing is sent to it but the requests
//! for those files. The server is asked through a [`Shelf`], so that all of this can be
//! tried on a made-up one, without a network.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Instant;

use flate2::read::DeflateDecoder;
use mfp_engine::{Error, download, plan};
use mfp_kms::Client;

use crate::facts::{Traffic, Unknown};
use crate::known::{Sheet, State};

/// The start of a patch file is fetched in these sizes in turn until the table in it is whole.
const START_SIZES: [u64; 2] = [256 << 10, 4 << 20];
/// How much of the newest main patch is fetched to time the line.
pub const SAMPLE: u64 = 8 << 20;
/// How many main versions above the one it starts from the newest is looked for.
const MAINS_AHEAD: u32 = 20;
/// Minor versions are two digits in the patch addresses.
const LAST_MINOR: u32 = 99;
/// How many main versions a patch that skips versions leads over at most; see the app's
/// `offer.rs`.
const REACH: u32 = 5;
/// The most patch files the minors of one version are asked for.
const MOST_MINORS: u32 = 12;

/// What the diagnostic asks of the server a client's patch files are published on.
pub trait Shelf {
    /// How large the file at `url` is; `None` when the server says it has no such file.
    fn size_of(&self, url: &str) -> Result<Option<u64>, Unknown>;

    /// The first `len` bytes of the file at `url`, or all of it when it is shorter. `None`
    /// when the server has no such file.
    fn start_of(&self, url: &str, len: u64) -> Result<Option<Vec<u8>>, Unknown>;
}

/// Nexon's server, with a count of what was asked of it.
#[derive(Default)]
pub struct Nexon {
    session: download::Session,
    requests: Cell<u32>,
    received: Cell<u64>,
}

impl Nexon {
    pub fn new() -> Nexon {
        Nexon::default()
    }

    /// How many requests were made, and how many bytes of files came back.
    pub fn traffic(&self) -> Traffic {
        Traffic { requests: self.requests.get(), received: self.received.get() }
    }
}

/// Why the server gave no answer, without any of the words the failure came with: those
/// can name this PC's side of the connection.
fn refused(err: Error) -> Unknown {
    match err {
        Error::Download { status: Some(status), .. } => Unknown::Http(status),
        Error::Download { status: None, .. } => Unknown::NoConnection,
        Error::Io { source, .. } => {
            source.raw_os_error().map_or(Unknown::NoAnswer, |code| Unknown::Windows(code as u32))
        }
        _ => Unknown::NoAnswer,
    }
}

impl Shelf for Nexon {
    fn size_of(&self, url: &str) -> Result<Option<u64>, Unknown> {
        self.requests.set(self.requests.get() + 1);
        match self.session.probe(url) {
            Ok(remote) => Ok(Some(remote.len)),
            Err(Error::NotPublished) => Ok(None),
            Err(err) => Err(refused(err)),
        }
    }

    fn start_of(&self, url: &str, len: u64) -> Result<Option<Vec<u8>>, Unknown> {
        self.requests.set(self.requests.get() + 1);
        match download::fetch_prefix(url, len) {
            Ok(bytes) => {
                self.received.set(self.received.get() + bytes.len() as u64);
                Ok(Some(bytes))
            }
            Err(Error::NotPublished) => Ok(None),
            Err(err) => Err(refused(err)),
        }
    }
}

/// What the start of a patch file holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// The table of the files the patch starts from, whole.
    Table(BTreeMap<String, u32>),
    /// The bytes end before the table does.
    CutOff,
    /// This is no patch file with such a table.
    NoTable,
}

/// How a patch file begins, and the most files its table may name; as in the engine.
const MAGIC: &[u8; 8] = b"WzPatch\x1A";
const HEADER: usize = 16;
const MOST_FILES: i32 = 100_000;
const LONGEST_NAME: i32 = 260;

/// Reads the table of original files out of the first bytes of a patch file.
pub fn read_start(bytes: &[u8]) -> Start {
    if bytes.len() < HEADER + 2 {
        return Start::CutOff;
    }
    if &bytes[..8] != MAGIC {
        return Start::NoTable;
    }
    let mut payload = &bytes[HEADER..];
    // The compressed data normally begins with a two-byte zlib header.
    if payload[0] == 0x78 && (u16::from(payload[0]) << 8 | u16::from(payload[1])).is_multiple_of(31) {
        payload = &payload[2..];
    }
    let mut contents = DeflateDecoder::new(payload);
    // Running out of bytes is the one thing that more bytes can mend.
    let number = |contents: &mut DeflateDecoder<&[u8]>| -> Option<[u8; 4]> {
        let mut bytes = [0u8; 4];
        contents.read_exact(&mut bytes).ok().map(|()| bytes)
    };
    let Some(count) = number(&mut contents).map(i32::from_le_bytes) else { return Start::CutOff };
    if !(0..=MOST_FILES).contains(&count) {
        return Start::NoTable;
    }
    let mut table = BTreeMap::new();
    for _ in 0..count {
        let Some(len) = number(&mut contents).map(i32::from_le_bytes) else { return Start::CutOff };
        if !(0..=LONGEST_NAME).contains(&len) {
            return Start::NoTable;
        }
        let mut name = vec![0u8; len as usize];
        if contents.read_exact(&mut name).is_err() {
            return Start::CutOff;
        }
        let Some(crc) = number(&mut contents).map(u32::from_le_bytes) else { return Start::CutOff };
        // The names decide which files of the folder are read. One that could lead out of
        // the folder makes the whole table one that is not gone by.
        let Ok(name) = String::from_utf8(name) else { return Start::NoTable };
        if !name.is_ascii() || plan::relative_path(&name).is_err() || table.insert(name, crc).is_some() {
            return Start::NoTable;
        }
    }
    Start::Table(table)
}

/// The checksums of the files the patch at `url` starts from. `None` when the server has no
/// such patch, or the patch has no such table.
pub fn originals_of(url: &str, shelf: &dyn Shelf) -> Result<Option<BTreeMap<String, u32>>, Unknown> {
    for size in START_SIZES {
        let Some(bytes) = shelf.start_of(url, size)? else { return Ok(None) };
        match read_start(&bytes) {
            Start::Table(table) => return Ok(Some(table)),
            Start::NoTable => return Ok(None),
            // The whole file came and the table still has no end.
            Start::CutOff if (bytes.len() as u64) < size => return Ok(None),
            Start::CutOff => {}
        }
    }
    Ok(None)
}

/// The newest main version on the server, going up from `from`, and its newest minor.
pub fn newest(client: &Client, from: u32, shelf: &dyn Shelf) -> Result<State, Unknown> {
    let mut version = from;
    while version < from + MAINS_AHEAD && shelf.size_of(&client.patch_url(version, version + 1))?.is_some() {
        version += 1;
    }
    let mut minor = 1;
    while minor < LAST_MINOR && shelf.size_of(&client.minor_patch_url(version, minor, minor + 1))?.is_some() {
        minor += 1;
    }
    Ok(State { version, minor })
}

/// The sheets the server can add to the ones in `have`: what the patches to the newest
/// version start from. A state that is in `have` already is not asked for again.
///
/// - The main patches to `newest` from each of the few versions before it say what the last
///   minor of those versions is.
/// - The minor patches of `newest` say what the minors before the newest one are.
///
/// A patch the server does not have, or one that does not say what it starts from, adds
/// nothing. The first request the server does not answer ends the asking, with what was
/// learned until then.
pub fn sheets(client: &Client, newest: State, have: &[State], shelf: &dyn Shelf) -> (Vec<Sheet>, Result<(), Unknown>) {
    let mut found = Vec::new();
    let ask = |state: State, url: String, found: &mut Vec<Sheet>| -> Result<(), Unknown> {
        if have.contains(&state) || found.iter().any(|sheet: &Sheet| sheet.state == state) {
            return Ok(());
        }
        if let Some(originals) = originals_of(&url, shelf)?
            && !originals.is_empty()
        {
            found.push(Sheet::from_originals(state, &originals));
        }
        Ok(())
    };
    let asked = (|| {
        for version in (newest.version.saturating_sub(REACH)..newest.version).rev() {
            ask(State { version, minor: 0 }, client.patch_url(version, newest.version), &mut found)?;
        }
        // The patch from each minor straight to the newest, and where that names nothing,
        // the step to the minor after it.
        for at in (1..newest.minor).rev().take(MOST_MINORS as usize) {
            let state = State { version: newest.version, minor: at };
            ask(state, client.minor_patch_url(newest.version, at, newest.minor), &mut found)?;
            if at + 1 < newest.minor {
                ask(state, client.minor_patch_url(newest.version, at, at + 1), &mut found)?;
            }
        }
        Ok(())
    })();
    (found, asked)
}

/// Fetches a piece of the patch at `url` to time the line: the bytes that came, and the
/// seconds they took. The first request to a server takes the longest, so this is asked
/// after others were.
pub fn sample(url: &str, shelf: &dyn Shelf) -> Result<(u64, f64), Unknown> {
    let started = Instant::now();
    match shelf.start_of(url, SAMPLE)? {
        Some(bytes) if !bytes.is_empty() => Ok((bytes.len() as u64, started.elapsed().as_secs_f64())),
        _ => Err(Unknown::NoAnswer),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::io::Write;

    use super::*;

    /// The start of a patch file whose table names `files`, as the engine reads one.
    pub(crate) fn patch_start(files: &[(&str, u32)]) -> Vec<u8> {
        let mut contents = (files.len() as i32).to_le_bytes().to_vec();
        for (name, crc) in files {
            contents.extend((name.len() as i32).to_le_bytes());
            contents.extend(name.as_bytes());
            contents.extend(crc.to_le_bytes());
        }
        // Entries follow the table in a real file.
        contents.extend(b"Data\\Next.wz\x02");
        let mut packed = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        packed.write_all(&contents).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend(2u32.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(packed.finish().unwrap());
        bytes
    }

    /// A made-up shelf: the files on it by their addresses, and a note of what was asked.
    #[derive(Default)]
    pub(crate) struct Shelved {
        pub(crate) files: BTreeMap<String, Vec<u8>>,
        pub(crate) asked: RefCell<Vec<String>>,
        /// Every request from this one on is not answered.
        pub(crate) down_after: Option<usize>,
    }

    impl Shelved {
        fn answer(&self, what: String) -> Result<(), Unknown> {
            let mut asked = self.asked.borrow_mut();
            asked.push(what);
            if self.down_after.is_some_and(|limit| asked.len() > limit) { Err(Unknown::NoConnection) } else { Ok(()) }
        }
    }

    impl Shelf for Shelved {
        fn size_of(&self, url: &str) -> Result<Option<u64>, Unknown> {
            self.answer(format!("size {url}"))?;
            Ok(self.files.get(url).map(|file| file.len() as u64))
        }

        fn start_of(&self, url: &str, len: u64) -> Result<Option<Vec<u8>>, Unknown> {
            self.answer(format!("start {url}"))?;
            Ok(self.files.get(url).map(|file| file[..file.len().min(len as usize)].to_vec()))
        }
    }

    const LIVE: &Client = &mfp_kms::LIVE;

    #[test]
    fn table_at_the_start_of_a_patch_file_is_read_from_memory() {
        let bytes = patch_start(&[(r"Data\Base\Base_000.wz", 0x1a2b_3c4d), ("MapleStory.exe", 7)]);
        let Start::Table(table) = read_start(&bytes) else { panic!("no table was read") };
        assert_eq!(table.len(), 2);
        assert_eq!(table[r"Data\Base\Base_000.wz"], 0x1a2b_3c4d);
        // A table that names nothing is a table too: such patches exist.
        assert_eq!(read_start(&patch_start(&[])), Start::Table(BTreeMap::new()));
    }

    #[test]
    fn start_that_ends_inside_the_table_asks_for_more_and_anything_else_is_no_table() {
        let files: Vec<(String, u32)> = (0..400).map(|at| (format!(r"Data\Map\Map_{at:03}.wz"), at * 7919)).collect();
        let named: Vec<(&str, u32)> = files.iter().map(|(name, crc)| (name.as_str(), *crc)).collect();
        let bytes = patch_start(&named);
        assert!(matches!(read_start(&bytes), Start::Table(table) if table.len() == 400));
        for cut in [0, 10, 17, 40, bytes.len() / 2] {
            assert_eq!(read_start(&bytes[..cut]), Start::CutOff, "cut at {cut}");
        }
        // Not a patch file, and a patch file of the older form, which begins with a name.
        assert_eq!(read_start(&[0u8; 64]), Start::NoTable);
        let mut older = MAGIC.to_vec();
        older.extend([0u8; 8]);
        let mut packed = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        packed.write_all(b"Data\\Base\\Base_000.wz\x01\x00\x00\x00\x00").unwrap();
        older.extend(packed.finish().unwrap());
        assert_eq!(read_start(&older), Start::NoTable);
    }

    #[test]
    fn table_that_names_a_way_out_of_the_folder_is_not_gone_by() {
        for name in [r"..\Windows\evil.dll", r"C:\Windows\evil.dll", r"Data\..\..\x", r".maple-fast-patch\x", "NUL"] {
            assert_eq!(read_start(&patch_start(&[("MapleStory.exe", 1), (name, 2)])), Start::NoTable, "{name}");
        }
        // Nor one that names a file twice.
        assert_eq!(read_start(&patch_start(&[("a.wz", 1), ("a.wz", 2)])), Start::NoTable);
    }

    #[test]
    fn newest_version_is_found_by_following_the_patches_upwards() {
        let mut shelf = Shelved::default();
        for url in [
            LIVE.patch_url(421, 422),
            LIVE.patch_url(422, 423),
            LIVE.minor_patch_url(423, 1, 2),
            LIVE.minor_patch_url(423, 2, 3),
        ] {
            shelf.files.insert(url, patch_start(&[]));
        }
        assert_eq!(newest(LIVE, 421, &shelf), Ok(State { version: 423, minor: 3 }));
        // With nothing above the version it starts from, that one is the newest, at its
        // first minor.
        assert_eq!(newest(LIVE, 423, &Shelved::default()), Ok(State { version: 423, minor: 1 }));
        // A server that does not answer is no server without patches.
        let down = Shelved { down_after: Some(0), ..Shelved::default() };
        assert_eq!(newest(LIVE, 421, &down), Err(Unknown::NoConnection));
    }

    #[test]
    fn server_adds_the_states_its_patches_start_from() {
        let mut shelf = Shelved::default();
        shelf.files.insert(LIVE.patch_url(420, 421), patch_start(&[("MapleStory.exe", 420)]));
        shelf.files.insert(LIVE.patch_url(418, 421), patch_start(&[("MapleStory.exe", 418)]));
        // The jump from minor 1 names what it starts from; the one from minor 2 names
        // nothing, and the step from 2 to 3 does; the last step names nothing.
        shelf.files.insert(LIVE.minor_patch_url(421, 1, 4), patch_start(&[("MapleStory.exe", 1)]));
        shelf.files.insert(LIVE.minor_patch_url(421, 2, 4), patch_start(&[]));
        shelf.files.insert(LIVE.minor_patch_url(421, 2, 3), patch_start(&[("MapleStory.exe", 2)]));
        shelf.files.insert(LIVE.minor_patch_url(421, 3, 4), patch_start(&[]));

        let newest = State { version: 421, minor: 4 };
        let (found, asked) = sheets(LIVE, newest, &[], &shelf);
        assert_eq!(asked, Ok(()));
        let states: Vec<(u32, u32)> = found.iter().map(|sheet| (sheet.state.version, sheet.state.minor)).collect();
        assert_eq!(states, [(420, 0), (418, 0), (421, 2), (421, 1)]);
        assert_eq!(found[2].files["maplestory.exe"].1, crate::known::Expect::File { crc: 2, len: None });

        // What is known already is not asked for.
        shelf.asked.borrow_mut().clear();
        let have =
            [State { version: 420, minor: 0 }, State { version: 421, minor: 1 }, State { version: 421, minor: 2 }];
        let (found, _) = sheets(LIVE, newest, &have, &shelf);
        assert_eq!(found.iter().map(|sheet| sheet.state.version).collect::<Vec<_>>(), [418]);
        let asked = shelf.asked.borrow();
        assert!(
            !asked.iter().any(|asked| asked.contains("00420to00421") || asked.contains("Minor/01to04")),
            "{asked:?}"
        );
    }

    #[test]
    fn asking_ends_at_the_first_request_without_an_answer_and_keeps_what_it_has() {
        let mut shelf = Shelved { down_after: Some(1), ..Shelved::default() };
        shelf.files.insert(LIVE.patch_url(420, 421), patch_start(&[("MapleStory.exe", 420)]));
        let (found, asked) = sheets(LIVE, State { version: 421, minor: 4 }, &[], &shelf);
        assert_eq!(found.len(), 1);
        assert_eq!(asked, Err(Unknown::NoConnection));
    }

    #[test]
    fn table_longer_than_the_first_piece_is_fetched_again_in_a_longer_one() {
        // Names that do not compress well, so that the table outgrows the first piece.
        let mut seed = 1u64;
        let files: Vec<(String, u32)> = (0..30_000)
            .map(|at| {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                (format!(r"Data\{:016x}{:016x}\{at}.wz", seed, seed.rotate_left(17) ^ at as u64), at)
            })
            .collect();
        let named: Vec<(&str, u32)> = files.iter().map(|(name, crc)| (name.as_str(), *crc)).collect();
        let bytes = patch_start(&named);
        assert!(bytes.len() as u64 > START_SIZES[0], "the made-up table is too small to try this: {}", bytes.len());
        let mut shelf = Shelved::default();
        shelf.files.insert("https://example.invalid/a.patch".to_owned(), bytes);
        let table = originals_of("https://example.invalid/a.patch", &shelf).unwrap().unwrap();
        assert_eq!(table.len(), 30_000);
        assert_eq!(shelf.asked.borrow().len(), 2);
        // A file the server does not have is not an error.
        assert_eq!(originals_of("https://example.invalid/b.patch", &shelf), Ok(None));
    }

    /// Asks Nexon's own server, with a handful of small requests. Run by hand:
    /// `cargo test -p mfp-check --lib server -- --ignored --nocapture`.
    #[test]
    #[ignore = "asks Nexon's patch server"]
    fn nexons_server_says_what_its_patches_start_from() {
        use crate::known::{Expect, Table};

        let nexon = Nexon::new();
        let table = Table::built_in(mfp_kms::Server::Live);
        let newest = newest(LIVE, table.newest().version, &nexon).expect("the server did not answer");
        println!("newest on the server: {}", newest.label(LIVE));
        assert!(newest.order() >= table.newest().order());

        // What the patch to the table's version starts from, read here from the first bytes
        // of the file, is what the engine read out of the whole file for the table.
        let first = table.states[0];
        let url = LIVE.patch_url(first.version, table.states[1].version);
        let originals = originals_of(&url, &nexon).unwrap().expect("the main patch names nothing");
        let built_in = &table.sheets()[0];
        assert_eq!(originals.len(), built_in.files.len());
        for (name, crc) in &originals {
            let known = built_in.files.get(&crate::known::key(name)).map(|(_, expect)| *expect);
            assert_eq!(known, Some(Expect::File { crc: *crc, len: None }), "{name}");
        }

        let (found, asked) = sheets(LIVE, newest, &[], &nexon);
        assert_eq!(asked, Ok(()));
        for sheet in &found {
            println!("  {}: {} files", sheet.state.label(LIVE), sheet.files.len());
        }
        assert!(found.iter().any(|sheet| sheet.state == first));
        let traffic = nexon.traffic();
        println!("{} requests, {} KiB", traffic.requests, traffic.received >> 10);
    }

    #[test]
    fn line_is_timed_by_what_arrives() {
        let mut shelf = Shelved::default();
        shelf.files.insert("https://example.invalid/a.patch".to_owned(), vec![7u8; 1000]);
        let (bytes, seconds) = sample("https://example.invalid/a.patch", &shelf).unwrap();
        assert_eq!(bytes, 1000);
        assert!(seconds >= 0.0);
        assert_eq!(sample("https://example.invalid/b.patch", &shelf), Err(Unknown::NoAnswer));
    }
}
