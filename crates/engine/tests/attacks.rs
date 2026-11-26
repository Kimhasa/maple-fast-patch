//! Attacks on the engine by patch files that lie.
//!
//! Each test is an attack that was tried and stays in the suite. What has to hold under every
//! one of them: no panic and no hang; a run that fails leaves the install folder exactly as
//! it was; nothing outside the install folder is ever touched; a wrong or damaged patch is
//! refused and never half applied; and every refusal is an [`Error`].
//!
//! The file of a patch carries a checksum over itself, which catches damage on the way. It
//! does not catch a file that was made to lie, so nearly every attack here puts the right
//! checksum on its file: what is tried is the reading of the contents, not the checksum.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mfp_engine::container::PatchFile;
use mfp_engine::crc;
use mfp_engine::plan::{self, Layout};
use mfp_engine::run::{Disk, half_patched, run_on_disk};
use mfp_engine::{Error, Event, Options, Outcome, Source, Stage};
use support::{Drive, GO, LAYOUTS, LIVE, Op, PatchBuilder, copy, files_under, on_disk, run, with_header, wrap};

const A: &str = r"Data\A.wz";
const B: &str = r"Data\B.wz";
const C: &str = r"Data\C.wz";
const GONE: &str = r"Data\Old\Gone.wz";
const FRESH: &str = r"Data\New\Fresh.wz";

/// The files of a folder by name.
type Files = BTreeMap<String, Vec<u8>>;

/// Runs `attack` on a thread of its own and gives up on it after `seconds`: an engine that
/// hangs fails the test and does not hang it. A panic in the engine fails the test too, and
/// says what was being tried.
fn bounded<T: Send + 'static>(seconds: u64, what: &str, attack: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || drop(done.send(catch_unwind(AssertUnwindSafe(attack)))));
    match result.recv_timeout(Duration::from_secs(seconds)) {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => panic!("{what}: the engine panicked: {}", said_by(panic)),
        Err(_) => panic!("{what}: no answer after {seconds} s"),
    }
}

/// What a panic said.
fn said_by(panic: Box<dyn std::any::Any + Send>) -> String {
    let said = panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).into()));
    said.unwrap_or_default()
}

/// Bytes that differ from place to place, so that a copy from the wrong place shows.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|at| (at as u8).wrapping_mul(31).wrapping_add(seed).wrapping_add((at >> 8) as u8)).collect()
}

/// A name in the form names are compared in: Windows takes `Data\A.wz` and `data/a.wz` for
/// the same file.
fn key(name: &str) -> String {
    name.trim_end_matches(['\\', '/']).replace('/', "\\").to_ascii_lowercase()
}

/// `files` by the form of their names that names are compared in.
fn by_key(files: &Files) -> Files {
    files.iter().map(|(name, contents)| (key(name), contents.clone())).collect()
}

/// The names of the files that are not the same in `before` and `after`.
fn differences(before: &Files, after: &Files) -> Vec<String> {
    let names = before.keys().chain(after.keys()).filter(|name| before.get(*name) != after.get(*name));
    names.cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect()
}

// ---------------------------------------------------------------------------------------
// A second reader of patch contents, written for these tests and sharing nothing with the
// engine's. When the engine takes a patch that was tampered with, this says what that patch
// means, and the install folder has to be exactly that: the patch was applied whole, as the
// other patch it had become, or it was not applied at all.
// ---------------------------------------------------------------------------------------

/// Where the second reader is in the contents, and where it found every number.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
    /// Where each 32-bit number stands: the counts, lengths, checksums and instructions.
    fields: Vec<usize>,
}

impl Cursor<'_> {
    fn take(&mut self, len: usize) -> Option<Range<usize>> {
        let end = self.at.checked_add(len).filter(|end| *end <= self.bytes.len())?;
        let range = self.at..end;
        self.at = end;
        Some(range)
    }

    fn number(&mut self) -> Option<u32> {
        let at = self.at;
        let range = self.take(4)?;
        self.fields.push(at);
        Some(u32::from_le_bytes(self.bytes[range].try_into().unwrap()))
    }

    fn text(&mut self) -> Option<String> {
        let len = self.number()?;
        let range = self.take(len as usize)?;
        String::from_utf8(self.bytes[range].to_vec()).ok()
    }
}

/// One instruction of a rebuild, as the second reader read it.
enum Step {
    Data(Range<usize>),
    Fill(u8, usize),
    Copy { from: String, start: usize, len: usize },
}

/// What an entry says is to happen to the file of its name.
enum Says {
    Folder,
    File { crc: u32, data: Range<usize> },
    Rebuild { old_crc: Option<u32>, new_crc: u32, steps: Vec<Step> },
    Delete,
}

/// Patch contents as the second reader read them.
#[derive(Default)]
struct Reading {
    table: Vec<(String, u32)>,
    entries: Vec<(String, Says)>,
    /// Where the entries begin, and where each of them ends.
    ends: Vec<usize>,
    fields: Vec<usize>,
    /// Whether the contents were read to their end. If not, `entries` are those before the
    /// place that could not be read.
    whole: bool,
}

/// Whether a name that ends in the byte for "create" is a file, by the rule of the format:
/// it has an extension.
fn has_extension(name: &str) -> bool {
    let last = name.rsplit(['\\', '/']).next().unwrap_or(name);
    last.contains('.') && !last.ends_with('.')
}

fn read_entry(cursor: &mut Cursor, layout: Layout) -> Option<(String, Says)> {
    let start = cursor.at;
    let kind_at = start + cursor.bytes[start..].iter().position(|byte| *byte <= 2)?;
    let name = String::from_utf8(cursor.bytes[start..kind_at].to_vec()).ok()?;
    cursor.at = kind_at + 1;
    let says = match cursor.bytes[kind_at] {
        0 if !has_extension(&name) => Says::Folder,
        0 => {
            let len = cursor.number()?;
            let crc = cursor.number()?;
            Says::File { crc, data: cursor.take(len as usize)? }
        }
        1 => {
            let old_crc = match layout {
                Layout::PerEntry => Some(cursor.number()?),
                Layout::Table => None,
            };
            let new_crc = cursor.number()?;
            let mut steps = Vec::new();
            loop {
                let command = cursor.number()?;
                match command >> 28 {
                    _ if command == 0 => break,
                    0x8 => steps.push(Step::Data(cursor.take((command & 0x0fff_ffff) as usize)?)),
                    0xC => steps.push(Step::Fill(command as u8, ((command >> 8) & 0xf_ffff) as usize)),
                    0x0..=0x7 => {
                        let start = cursor.number()? as usize;
                        let from = match layout {
                            Layout::Table => cursor.text()?,
                            Layout::PerEntry => name.clone(),
                        };
                        steps.push(Step::Copy { from, start, len: command as usize });
                    }
                    _ => return None,
                }
            }
            Says::Rebuild { old_crc, new_crc, steps }
        }
        _ => Says::Delete,
    };
    Some((name, says))
}

/// Reads `contents` as far as they can be read in `layout`.
fn read_again(contents: &[u8], layout: Layout) -> Reading {
    let mut cursor = Cursor { bytes: contents, at: 0, fields: Vec::new() };
    let mut reading = Reading::default();
    if layout == Layout::Table {
        let Some(count) = cursor.number() else { return reading };
        for _ in 0..count {
            let row = cursor.text().and_then(|name| Some((name, cursor.number()?)));
            let Some(row) = row else { return reading };
            reading.table.push(row);
        }
    }
    reading.ends.push(cursor.at);
    while cursor.at < contents.len() {
        let Some(entry) = read_entry(&mut cursor, layout) else { break };
        reading.entries.push(entry);
        reading.ends.push(cursor.at);
    }
    let end = *reading.ends.last().unwrap();
    reading.whole = end == contents.len();
    reading.fields = cursor.fields.into_iter().filter(|field| *field < end).collect();
    reading
}

/// What the patch `contents`, read in `layout`, make of the files `before`. `None` when,
/// read this way, they are no patch that can be put on these files: something is cut short,
/// a checksum is wrong, a copy reaches for what is not there.
fn meaning(contents: &[u8], layout: Layout, before: &Files) -> Option<Files> {
    let reading = read_again(contents, layout);
    if !reading.whole {
        return None;
    }
    let old = by_key(before);
    let mut files = old.clone();
    for (name, says) in &reading.entries {
        match says {
            Says::Folder => {}
            Says::File { crc, data } => {
                let data = &contents[data.clone()];
                if crc::update(0, data) != *crc {
                    return None;
                }
                // An entry for a file of no bytes writes nothing.
                if !data.is_empty() {
                    files.insert(key(name), data.to_vec());
                }
            }
            // A name that ends like a folder's removes a folder, any other name a file.
            Says::Delete if name.ends_with(['\\', '/']) => {
                let below = format!("{}\\", key(name));
                files.retain(|file, _| !file.starts_with(&below));
            }
            Says::Delete => drop(files.remove(&key(name))),
            Says::Rebuild { old_crc, new_crc, steps } => {
                let mut new = Vec::new();
                for step in steps {
                    match step {
                        Step::Data(range) => new.extend(&contents[range.clone()]),
                        Step::Fill(byte, len) => new.resize(new.len() + len, *byte),
                        Step::Copy { from, start, len } => {
                            // Copies are from the files as they were, each of which has to
                            // be the file the patch was made for.
                            let source = old.get(&key(from))?;
                            let expected = match layout {
                                Layout::Table => reading.table.iter().find(|(listed, _)| listed == from)?.1,
                                Layout::PerEntry => (*old_crc)?,
                            };
                            if crc::update(0, source) != expected {
                                return None;
                            }
                            new.extend(source.get(*start..start.checked_add(*len)?)?);
                        }
                    }
                }
                if crc::update(0, &new) != *new_crc {
                    return None;
                }
                files.insert(key(name), new);
            }
        }
    }
    Some(files)
}

// ---------------------------------------------------------------------------------------
// The install folder that is attacked.
// ---------------------------------------------------------------------------------------

/// What a made-up drive has free for the runs that go the immediate way: less than the new
/// files of [`Target::prelude`] take together, and more than the largest of them.
const ROOM: u64 = 150_000;

/// An install folder to attack, and a folder beside it that nothing may ever touch.
struct Target {
    _temp: tempfile::TempDir,
    install: PathBuf,
    outside: PathBuf,
    patch: PathBuf,
    /// The files of the install before any attack, and those of the folder beside it.
    before: Files,
    beside: Files,
}

impl Target {
    fn new() -> Target {
        let temp = tempfile::tempdir().unwrap();
        let (install, outside) = (temp.path().join("Maple"), temp.path().join("outside"));
        let files = [
            ("MapleStory.exe", pattern(64, 1)),
            (A, pattern(120_000, 2)),
            (B, pattern(80_000, 3)),
            (C, pattern(30_000, 4)),
            (GONE, pattern(500, 5)),
        ];
        for (name, contents) in &files {
            let path = on_disk(&install, name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"nothing may touch this").unwrap();
        let patch = temp.path().join("attack.patch");
        let (before, beside) = (files_under(&install), files_under(&outside));
        Target { _temp: temp, install, outside, patch, before, beside }
    }

    /// An honest patch for this install with one entry of every kind and few bytes, for the
    /// attacks that change it byte by byte: a folder, a whole file, a rebuild that copies,
    /// fills and brings new bytes, and a delete.
    fn honest(&self, layout: Layout) -> PatchBuilder {
        let from_other = match layout {
            Layout::Table => copy(B, 0, 500),
            Layout::PerEntry => copy(A, 2000, 500),
        };
        let ops = [copy(A, 100, 1000), Op::Literal(pattern(30, 9)), Op::Fill(7, 300), from_other];
        let mut builder = PatchBuilder::new(layout);
        builder.original(A, &self.before[A]).original(B, &self.before[B]);
        builder.create_dir(r"Data\New").create(FRESH, &pattern(40, 8));
        builder.rebuild(A, &self.before[A], &self.built(&ops), &ops).delete(GONE);
        builder
    }

    /// An honest patch that replaces two files large enough to tell the two ways of
    /// patching apart: on a drive with [`ROOM`] free it can only go the immediate way, which
    /// puts the first new file in its place before the second is built. An attack adds its
    /// lie behind these entries, where the immediate way would come to it too late if it
    /// had not looked first.
    fn prelude(&self, layout: Layout) -> PatchBuilder {
        let for_a = [copy(A, 1000, 100_000), Op::Fill(1, 5000)];
        let for_b = [copy(B, 500, 70_000), Op::Literal(pattern(100, 6))];
        let mut builder = PatchBuilder::new(layout);
        builder.original(A, &self.before[A]).original(B, &self.before[B]).original(C, &self.before[C]);
        builder.rebuild(A, &self.before[A], &self.built(&for_a), &for_a);
        builder.rebuild(B, &self.before[B], &self.built(&for_b), &for_b);
        builder
    }

    /// What `ops` make out of the files of the install.
    fn built(&self, ops: &[Op]) -> Vec<u8> {
        let mut new = Vec::new();
        for op in ops {
            match op {
                Op::Literal(data) => new.extend(data),
                Op::Fill(byte, len) => new.resize(new.len() + *len as usize, *byte),
                Op::Copy(from, start, len) => new.extend(&self.before[from][*start as usize..(*start + *len) as usize]),
            }
        }
        new
    }

    /// The files of the install now, the engine's own work folder and lock file aside.
    fn files(&self) -> Files {
        let mut files = files_under(&self.install);
        files.retain(|name, _| !name.starts_with(".maple-fast-patch"));
        files
    }

    fn options(&self) -> Options {
        Options {
            install_dir: self.install.clone(),
            source: Source::File(self.patch.clone()),
            immediate: false,
            storage: Default::default(),
        }
    }

    /// Whether the engine reads the patch file `file` at all, before any run. A file that is
    /// refused here is refused by a run at the same place, before the run has looked at the
    /// install folder.
    fn reads(&self, file: &[u8], what: &str) -> bool {
        fs::write(&self.patch, file).unwrap();
        let path = self.patch.clone();
        let read = move || {
            let patch = PatchFile::open(&path)?;
            patch.verify(&GO, |_| {})?;
            plan::scan(&patch, &GO).map(drop)
        };
        bounded(60, what, read).is_ok()
    }

    /// Runs the patch file `file` on the install, the safe way.
    fn attempt(&self, file: &[u8], what: &str) -> Result<Outcome, Error> {
        fs::write(&self.patch, file).unwrap();
        let options = self.options();
        let result = bounded(60, what, move || run(&options, &GO, &mut |_| {}));
        self.judge(&result, what);
        result
    }

    /// Runs the patch file `file` on the install the immediate way, on a drive that is said
    /// to be NTFS and to have [`ROOM`] free. Also returns whether the run got as far as its
    /// trial, which only the immediate way has.
    fn attempt_immediately(&self, file: &[u8], what: &str) -> (Result<Outcome, Error>, bool) {
        fs::write(&self.patch, file).unwrap();
        let options = Options { immediate: true, ..self.options() };
        let drive = Drive::with_room(&self.install, ROOM);
        let (result, tried) = bounded(60, what, move || {
            let free = |_: &Path| -> io::Result<u64> { Ok(drive.free()) };
            let mut tried = false;
            let disk = Disk { free: &free, slack: 0, file_system: Some("NTFS") };
            let result = run_on_disk(&LIVE, &options, &disk, &GO, &mut |event| {
                tried |= matches!(event, Event::Stage(Stage::Trial));
            });
            (result, tried)
        });
        self.judge(&result, what);
        (result, tried)
    }

    /// However a run ended, nothing beside the install folder may have been touched. And a
    /// run that failed on a patch that lies must leave the install folder as it was, in
    /// either way of patching: such a patch is never half applied.
    fn judge(&self, result: &Result<Outcome, Error>, what: &str) {
        assert!(files_under(&self.outside) == self.beside, "{what}: something beside the install folder was touched");
        if let Err(err) = result {
            let changed = differences(&self.before, &self.files());
            assert!(changed.is_empty(), "{what}: the run failed ({err}) and changed {changed:?}");
            assert!(!half_patched(&self.install), "{what}: the run failed ({err}) and left the folder half patched");
        }
    }

    /// Runs a patch file made of `contents`, the safe way, and says whether the run went
    /// through. A run that goes through must have done exactly what the second reader says
    /// these contents mean.
    fn try_contents(&self, contents: &[u8], what: &str) -> bool {
        if self.attempt(&wrap(contents), what).is_err() {
            return false;
        }
        let after = by_key(&self.files());
        let said: Vec<Files> = LAYOUTS.iter().filter_map(|layout| meaning(contents, *layout, &self.before)).collect();
        assert!(said.contains(&after), "{what}: the run went through, and the folder is not what the patch says");
        true
    }

    /// [`Target::try_contents`] for the many attacks that change an honest patch a little.
    /// Every changed file is read by the engine. A run on the install is made with each one
    /// the engine reads, and with every seventh of the others, to see that a run refuses
    /// them as well. The install is put back after a run that went through.
    fn strike(&self, contents: &[u8], what: &str, tally: &mut Tally) {
        let sampled = (tally.refused + tally.accepted).is_multiple_of(7);
        if (self.reads(&wrap(contents), what) || sampled) && self.try_contents(contents, what) {
            tally.accepted += 1;
            self.reset();
        } else {
            tally.refused += 1;
        }
    }

    /// Puts the install back as it was before any attack, and clears out the work folder.
    fn reset(&self) {
        for entry in fs::read_dir(&self.install).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { fs::remove_dir_all(path).unwrap() } else { fs::remove_file(path).unwrap() }
        }
        for (name, contents) in &self.before {
            let path = on_disk(&self.install, name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }
}

/// How many attacks of a kind were refused, and how many were taken as another patch.
#[derive(Default)]
struct Tally {
    refused: usize,
    accepted: usize,
}

#[test]
fn honest_patches_of_the_attacks_apply_in_both_ways() {
    for layout in LAYOUTS {
        let target = Target::new();
        let honest = target.honest(layout).contents();
        assert!(target.try_contents(&honest, "the honest patch"), "{layout:?}");
        let after = target.files();
        assert!(after.contains_key(FRESH) && !after.contains_key(GONE) && after[A] != target.before[A], "{layout:?}");

        // The larger one goes the immediate way on the small drive, and gets there.
        target.reset();
        let prelude = target.prelude(layout).contents();
        let (result, tried) = target.attempt_immediately(&wrap(&prelude), "the honest prelude");
        assert!(result.is_ok() && tried, "{layout:?}: {result:?}, trial: {tried}");
        assert!(by_key(&target.files()) == meaning(&prelude, layout, &target.before).unwrap(), "{layout:?}");
    }
}

/// The values a byte is changed to.
fn other_values(old: u8) -> Vec<u8> {
    let mut values =
        vec![0x00, 0x01, 0x02, 0xFF, old ^ 0x01, old ^ 0x20, old ^ 0x80, old.wrapping_add(1), old.wrapping_sub(1)];
    values.sort_unstable();
    values.dedup();
    values.retain(|value| *value != old);
    values
}

/// Changes every byte of the contents of an honest patch, one at a time, to each of a
/// handful of other values. The checksum over the file is put right every time.
fn change_every_byte(layout: Layout) {
    let target = Target::new();
    let honest = target.honest(layout).contents();
    let mut tally = Tally::default();
    for at in 0..honest.len() {
        for value in other_values(honest[at]) {
            let mut contents = honest.clone();
            contents[at] = value;
            target.strike(&contents, &format!("{layout:?}, byte {at} set to {value:#04x}"), &mut tally);
        }
    }
    // A changed letter in a name makes a patch for another file, which is a whole patch
    // still. Everything else breaks a length, a checksum or the form.
    assert!(tally.refused > 3 * tally.accepted && tally.accepted > 0, "{} / {}", tally.refused, tally.accepted);
}

#[test]
fn every_single_byte_change_is_refused_or_is_another_whole_patch_table_layout() {
    change_every_byte(Layout::Table);
}

#[test]
fn every_single_byte_change_is_refused_or_is_another_whole_patch_per_entry_layout() {
    change_every_byte(Layout::PerEntry);
}

#[test]
fn every_count_length_checksum_and_instruction_at_its_edges_is_refused_or_is_another_whole_patch() {
    for layout in LAYOUTS {
        let target = Target::new();
        let honest = target.honest(layout).contents();
        let len = honest.len() as u32;
        let fields = read_again(&honest, layout).fields;
        assert!(fields.len() >= 10, "{layout:?}: the second reader found {} numbers", fields.len());
        let mut tally = Tally::default();
        for field in fields {
            let old = u32::from_le_bytes(honest[field..field + 4].try_into().unwrap());
            let edges = [0, 1, 2, 260, 261, 100_000, 100_001, 1_000_000_000, 0x7FFF_FFFE, 0x7FFF_FFFF, 0x8000_0000];
            let around = [0xFFFF_FFFF, len - 1, len, len + 1, old.wrapping_add(1), old.wrapping_sub(1), old | 1 << 31];
            for value in edges.into_iter().chain(around).filter(|value| *value != old) {
                let mut contents = honest.clone();
                contents[field..field + 4].copy_from_slice(&value.to_le_bytes());
                target.strike(&contents, &format!("{layout:?}, the number at {field} set to {value:#x}"), &mut tally);
            }
        }
        assert!(tally.refused > 10 * tally.accepted, "{layout:?}: {} / {}", tally.refused, tally.accepted);
    }
}

#[test]
fn patch_cut_off_anywhere_is_refused_or_is_a_whole_shorter_patch() {
    for layout in LAYOUTS {
        let target = Target::new();
        let honest = target.honest(layout).contents();
        let ends = read_again(&honest, layout).ends;

        // The contents cut at every length, compressed whole and under the right checksum.
        // What ends where an entry ends is a shorter patch, and nothing else is a patch.
        // Nothing at all is a patch as well: one that does nothing.
        let mut taken = Vec::new();
        for len in 0..honest.len() {
            let mut tally = Tally { refused: len, accepted: 0 };
            target.strike(&honest[..len], &format!("{layout:?}, contents cut at {len}"), &mut tally);
            if tally.accepted == 1 {
                taken.push(len);
            }
        }
        let mut between: Vec<usize> = ends.iter().copied().filter(|end| *end < honest.len()).collect();
        between.insert(0, 0);
        between.dedup();
        assert_eq!(taken, between, "{layout:?}: only the cuts between two entries make a patch");

        // The compressed stream cut at every length, under the right checksum. A stream
        // that just stops reads as contents that end there, so this is the same attack from
        // further out. Whatever is taken has to be one of the shorter patches again.
        let whole = wrap(&honest);
        let payload = &whole[16..];
        let shorter: Vec<Files> =
            ends.iter().filter_map(|end| meaning(&honest[..*end], layout, &target.before)).collect();
        for len in 0..payload.len() {
            let what = format!("{layout:?}, compressed stream cut at {len}");
            if target.attempt(&with_header(&payload[..len]), &what).is_ok() {
                assert!(shorter.contains(&by_key(&target.files())), "{what}: taken, and not as a shorter patch");
                target.reset();
            }
        }

        // The file cut at every length, as a download that stopped would leave it. Its
        // checksum no longer fits, and it is refused wherever it ends.
        for len in 0..whole.len() {
            let what = format!("{layout:?}, file cut at {len}");
            assert!(target.attempt(&whole[..len], &what).is_err(), "{what}: taken");
        }
    }
}

#[test]
fn bytes_after_the_end_are_refused_inside_the_contents_and_passed_over_behind_them() {
    for layout in LAYOUTS {
        let target = Target::new();
        let honest = target.honest(layout).contents();
        let said = meaning(&honest, layout, &target.before).unwrap();

        // More contents behind the last entry are the start of another entry, which is cut
        // short or names nothing that can be a file.
        for extra in [&b"x"[..], b"Data\\X.wz", &[0xFF; 9], &[1], &[2], &[0, 0, 0, 0]] {
            let what = format!("{layout:?}, contents followed by {extra:?}");
            assert!(target.attempt(&wrap(&[&honest[..], extra].concat()), &what).is_err(), "{what}: taken");
        }

        // Bytes behind the end of the compressed stream are not contents. The checksum over
        // the file covers them, so they were there when the patch was made: the patch is
        // the one in front of them, and is applied as that.
        let file = with_header(&[&wrap(&honest)[16..], &pattern(300, 1)[..]].concat());
        target.attempt(&file, "bytes behind the compressed stream").unwrap();
        assert!(by_key(&target.files()) == said, "{layout:?}: the patch in front of the extra bytes was not applied");
    }
}

#[test]
fn wrong_checksum_of_the_file_is_refused_whatever_the_contents() {
    for layout in LAYOUTS {
        let target = Target::new();
        let honest = target.honest(layout).bytes();
        // Every byte of the header and of the compressed stream, changed without putting
        // the checksum right, as damage on the way would.
        let said = meaning(&target.honest(layout).contents(), layout, &target.before).unwrap();
        for at in 0..honest.len() {
            let mut file = honest.clone();
            file[at] ^= 0x10;
            let what = format!("{layout:?}, byte {at} of the file changed");
            if target.attempt(&file, &what).is_ok() {
                // The four bytes that number the version of the form are not looked at, and
                // what follows them is the patch it was.
                assert!((8..12).contains(&at) && by_key(&target.files()) == said, "{what}: taken");
                target.reset();
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Names.
// ---------------------------------------------------------------------------------------

/// Names that reach out of the install folder, or that Windows reads as something other
/// than the plain file they seem to name. `outside` is a folder beside the install folder.
fn hostile_names(outside: &Path) -> Vec<String> {
    let keep = outside.join("keep.txt").to_string_lossy().into_owned();
    let drive_less = keep.split_once(':').map_or(keep.clone(), |(_, rest)| rest.to_owned());
    let mut names: Vec<String> = [
        // Up and out.
        r"..\outside\keep.txt",
        r"Data\..\..\outside\keep.txt",
        "../outside/keep.txt",
        r"Data\..\MapleStory.exe",
        r".\MapleStory.exe",
        r"Data\.\A.wz",
        "..",
        ".",
        // From the root of a drive, of this drive, of another PC, of the device namespace.
        r"C:\Windows\evil.dll",
        r"C:evil.dll",
        r"\Windows\evil.dll",
        "/Windows/evil.dll",
        r"\\server\share\evil.dll",
        r"\\?\C:\Windows\evil.dll",
        r"\\.\C:\Windows\evil.dll",
        r"\\.\PhysicalDrive0",
        r"\??\C:\Windows\evil.dll",
        // Devices.
        "CON",
        "NUL",
        "nul.wz",
        r"Data\COM1",
        r"Data\com1.wz",
        r"Data\LPT9.wz",
        r"AUX\x.wz",
        "CONIN$",
        "CONOUT$",
        // Other streams of a file.
        r"Data\A.wz:hidden",
        r"Data\A.wz::$DATA",
        r"Data:$I30:$INDEX_ALLOCATION",
        // What Windows strips, so that two names are one file.
        r"Data\A.wz.",
        r"Data\A.wz ",
        r"Data.\A.wz",
        r"Data \A.wz",
        r"Data\A.wz...",
        // The short name of a file is a second name for it.
        r"Data\A~1.wz",
        r"MAPLE-~1\applying.log",
        // Characters no file name has.
        r"Data\A?.wz",
        r"Data\*.wz",
        r"Data\A<B.wz",
        r"Data\A>B.wz",
        r"Data\A|B.wz",
        "Data\\A\"B.wz",
        "Data\\A\tB.wz",
        "Data\\A\nB.wz",
        "Data\\A\x7FB.wz",
        "Data\\A\x03B.wz",
        // Empty parts.
        r"Data\\A.wz",
        r"\Data\A.wz",
        // The engine's own files in the install folder.
        r".maple-fast-patch\applying.log",
        r".maple-fast-patch\committed",
        r".Maple-Fast-Patch\new\Data\A.wz",
        ".maple-fast-patch/patch",
        ".maple-fast-patch",
        ".maple-fast-patch.lock",
        ".MAPLE-FAST-PATCH.LOCK",
    ]
    .map(String::from)
    .to_vec();
    // The folder beside the install by its full name, with and without the drive.
    names.extend([keep.clone(), drive_less, format!(r"\\?\{keep}"), keep.replace('\\', "/")]);
    names
}

/// Every place in the contents of a patch that carries a name, with `name` in it: an entry
/// of each kind, the table of originals and a copy instruction.
fn contents_naming(layout: Layout, name: &str) -> Vec<(&'static str, Vec<u8>)> {
    let entry = |kind: u8, rest: &[u8]| {
        let mut builder = PatchBuilder::new(layout);
        builder.raw(name.as_bytes()).raw(&[kind]).raw(rest);
        builder.contents()
    };
    let data = pattern(20, 1);
    let whole_file = [&20u32.to_le_bytes()[..], &crc::update(0, &data).to_le_bytes(), &data].concat();
    let old_crc: &[u8] = if layout == Layout::PerEntry { &[0; 4] } else { &[] };
    let literal = [old_crc, &crc::update(0, &data).to_le_bytes(), &(0x8000_0014u32).to_le_bytes(), &data, &[0; 4]];
    let mut places = vec![
        ("an entry that creates", entry(0, &whole_file)),
        ("an entry that rebuilds", entry(1, &literal.concat())),
        ("an entry that deletes", entry(2, &[])),
    ];
    if layout == Layout::Table {
        let mut listed = PatchBuilder::new(layout);
        listed.original(name, b"anything").delete(GONE);
        places.push(("the table of originals", listed.contents()));

        let mut copied = PatchBuilder::new(layout);
        let new = pattern(10, 2);
        copied.original(A, &pattern(120_000, 2)).rebuild(A, &[], &new, &[copy(name, 0, 10)]);
        places.push(("a copy instruction", copied.contents()));
    }
    places
}

#[test]
fn hostile_names_are_refused_wherever_a_patch_carries_a_name() {
    for layout in LAYOUTS {
        let target = Target::new();
        for name in hostile_names(&target.outside) {
            for (place, contents) in contents_naming(layout, &name) {
                let what = format!("{layout:?}, {name:?} in {place}");
                // The run is made even if the reading refuses: these are the names that
                // would do harm, and the harm would be done by a run.
                let result = target.attempt(&wrap(&contents), &what);
                assert!(result.is_err(), "{what}: taken");
            }
        }
        // Not a file was made for them either, in the install folder or in its work folder.
        assert!(files_under(&target.install) == target.before, "{layout:?}: something was left in the install folder");
    }
}

#[test]
fn names_too_long_for_a_file_are_refused() {
    for layout in LAYOUTS {
        let target = Target::new();
        let long = |len: usize| format!("{}.wz", "n".repeat(len - 3));
        // 255 is the longest a single name can be on NTFS. One more, and no file of that
        // name can be made, however short the whole path is.
        let names = [
            long(256),
            long(260),
            format!(r"{}\x", "d".repeat(256)),
            long(261),
            long(300),
            long(32_767),
            long(32_768),
            long(70_000),
        ];
        for name in names {
            for (place, contents) in contents_naming(layout, &name) {
                let what = format!("{layout:?}, a name of {} characters in {place}", name.len());
                assert!(target.attempt(&wrap(&contents), &what).is_err(), "{what}: taken");
            }
        }
        // The longest names that can be files are taken: 255 in one part, 260 in all.
        for name in [long(255), format!(r"Data\Sub\{}", long(251))] {
            let data = pattern(20, 1);
            let mut builder = PatchBuilder::new(layout);
            builder.create(&name, &data);
            target.attempt(&builder.bytes(), "a name as long as a name can be").unwrap();
            assert!(
                fs::read(on_disk(&target.install, &name)).unwrap() == data,
                "{layout:?}: {} characters",
                name.len()
            );
            target.reset();
        }
    }
}

// ---------------------------------------------------------------------------------------
// Entries that lie.
// ---------------------------------------------------------------------------------------

/// The start of a rebuild entry as it stands in the contents: the name, the byte that says
/// "rebuild", and the checksums.
fn rebuild_head(layout: Layout, name: &str, old: &[u8], new_crc: u32) -> Vec<u8> {
    let mut head = name.as_bytes().to_vec();
    head.push(1);
    if layout == Layout::PerEntry {
        head.extend(crc::update(0, old).to_le_bytes());
    }
    head.extend(new_crc.to_le_bytes());
    head
}

/// A copy instruction as it stands in the contents.
fn copy_bytes(layout: Layout, from: &str, start: u32, len: u32) -> Vec<u8> {
    let mut bytes = [len.to_le_bytes(), start.to_le_bytes()].concat();
    if layout == Layout::Table {
        bytes.extend((from.len() as u32).to_le_bytes());
        bytes.extend(from.as_bytes());
    }
    bytes
}

/// An instruction that brings `data` as it stands in the contents.
fn literal_bytes(data: &[u8]) -> Vec<u8> {
    [&(0x8000_0000 | data.len() as u32).to_le_bytes()[..], data].concat()
}

/// An entry that creates the file `name` out of `data`, as it stands in the contents.
fn create_bytes(name: &str, data: &[u8]) -> Vec<u8> {
    [name.as_bytes(), &[0], &(data.len() as u32).to_le_bytes(), &crc::update(0, data).to_le_bytes(), data].concat()
}

/// The end of the instructions of a rebuild.
const END: [u8; 4] = [0; 4];

/// Patches that begin honestly ([`Target::prelude`]) and then lie, each with what the lie
/// is. Every one of them has to be refused, in both ways of patching, with the install
/// folder left as it was.
fn lies(target: &Target, layout: Layout) -> Vec<(String, Vec<u8>)> {
    let old = &target.before[C];
    let good = pattern(50, 7);
    let good_crc = crc::update(0, &good);
    let mut lies: Vec<(String, Vec<u8>)> = Vec::new();
    let mut lie = |what: &str, parts: &[&[u8]]| {
        let mut builder = target.prelude(layout);
        builder.raw(&parts.concat());
        lies.push((what.to_owned(), builder.contents()));
    };
    let head = |new_crc: u32| rebuild_head(layout, C, old, new_crc);
    let copied = |start: usize, len: usize| crc::update(0, &old[start..start + len]);

    // Copies that reach for what is not there.
    lie("a copy that runs past the end of the original", &[&head(0), &copy_bytes(layout, C, 29_000, 2000), &END]);
    lie("a copy that ends one byte past the end", &[&head(0), &copy_bytes(layout, C, 29_000, 1001), &END]);
    lie("a copy that begins past the end", &[&head(0), &copy_bytes(layout, C, 40_000, 10), &END]);
    lie("a copy from as far as numbers go", &[&head(0), &copy_bytes(layout, C, 0x7FFF_FFFF, 10), &END]);
    lie("a copy as long as numbers go", &[&head(0), &copy_bytes(layout, C, 0, 0x7FFF_FFFF), &END]);
    lie("a copy from a negative place", &[&head(0), &copy_bytes(layout, C, 0x8000_0000, 10), &END]);
    if layout == Layout::Table {
        let from = |name: &str| copy_bytes(layout, name, 0, 10);
        lie("a copy from a file the table does not list", &[&head(0), &from("MapleStory.exe"), &END]);
        lie("a copy from a file that is nowhere", &[&head(0), &from(r"Data\Nowhere.wz"), &END]);
        lie("a copy from a folder", &[&head(0), &from(r"Data\Old"), &END]);
        lie("a copy from a name of 261 characters", &[&head(0), &from(&"n".repeat(261)), &END]);
    }

    // Checksums that are wrong for what they are over.
    lie(
        "a new file that does not have the checksum promised",
        &[&head(copied(0, 1000) ^ 1), &copy_bytes(layout, C, 0, 1000), &END],
    );
    lie(
        "a whole file that does not have the checksum promised",
        &[b"Data\\N.wz\0", &50u32.to_le_bytes(), &(good_crc ^ 1).to_le_bytes(), &good],
    );
    if layout == Layout::PerEntry {
        let wrong_old = rebuild_head(layout, C, b"another file", copied(0, 1000));
        lie(
            "an original that is not the file the entry was made for",
            &[&wrong_old, &copy_bytes(layout, C, 0, 1000), &END],
        );
    }

    // Lengths that disagree with what is there.
    lie("new bytes promised beyond the end of the contents", &[&head(good_crc), &0x8000_1388u32.to_le_bytes(), &good]);
    lie("the most new bytes an instruction can promise", &[&head(good_crc), &0x8FFF_FFFFu32.to_le_bytes(), &good]);
    lie("instructions that never end", &[&head(copied(0, 10)), &copy_bytes(layout, C, 0, 10)]);
    lie(
        "a whole file longer than the contents",
        &[b"Data\\N.wz\0", &5000u32.to_le_bytes(), &good_crc.to_le_bytes(), &good],
    );
    lie(
        "a whole file of the greatest length",
        &[b"Data\\N.wz\0", &0x7FFF_FFFFu32.to_le_bytes(), &good_crc.to_le_bytes(), &good],
    );
    lie(
        "a whole file of a negative length",
        &[b"Data\\N.wz\0", &0xFFFF_FFFFu32.to_le_bytes(), &good_crc.to_le_bytes(), &good],
    );
    lie(
        "a whole file shorter than the bytes that follow",
        &[b"Data\\N.wz\0", &40u32.to_le_bytes(), &crc::update(0, &good[..40]).to_le_bytes(), &good],
    );
    for unknown in [0x9000_0001u32, 0xA000_0010, 0xB123_4567, 0xD000_0001, 0xE000_0001, 0xF000_0000, 0xFFFF_FFFF] {
        lie(&format!("the unknown instruction {unknown:#x}"), &[&head(good_crc), &unknown.to_le_bytes(), &END]);
    }

    // One name for two things.
    let file = |name: &str| create_bytes(name, &good);
    let from_new_bytes =
        |name: &str| [&rebuild_head(layout, name, &[], good_crc)[..], &literal_bytes(&good), &END].concat();
    lie("the same file twice", &[&file(r"Data\N.wz"), &file(r"Data\N.wz")]);
    lie("the same file in two spellings of case", &[&file(r"Data\N.wz"), &file(r"DATA\n.WZ")]);
    lie("the same file with both kinds of separator", &[&file(r"Data\Sub\N.wz"), &file("Data/Sub/N.wz")]);
    lie("a file created and deleted", &[&file(r"Data\N.wz"), b"Data\\N.wz\x02"]);
    lie("a file, and then a file in the folder of that name", &[&file(r"Data\N.wz"), &file(r"Data\N.wz\x.wz")]);
    lie(
        "a file in a folder, and then a file of the name of that folder",
        &[&file(r"Data\N.wz\x.wz"), &file(r"Data\N.wz")],
    );
    lie("a file, and then a folder below it", &[&file(r"Data\N.wz"), b"Data\\N.wz\\Sub\0"]);
    lie("a folder, and then a file of its name", &[b"Data\\Fresh\\\0", &from_new_bytes(r"Data\Fresh")]);
    lie(
        "a file of the name of a folder, in another spelling of case",
        &[&file(r"Data\N.wz\x.wz"), &from_new_bytes(r"data\n.WZ")],
    );
    lie("a file named the way folders are", &[&from_new_bytes("Data\\N.wz\\")]);
    lie("a file twice, once named the way folders are", &[&file(r"Data\N.wz"), &from_new_bytes("Data\\N.wz\\")]);

    // Names the install folder has for the other kind of thing.
    lie("a file where the install has a folder", &[&from_new_bytes(r"Data\Old")]);
    lie("a file in a folder where the install has a file", &[&file(r"Data\C.wz\x.wz")]);
    lie("a folder where the install has a file", &[b"Data\\C.wz\\Sub\0"]);
    // A delete in front that seems to clear the place and does not: it names the other
    // kind of thing, and a delete only ever removes the kind it names.
    lie(
        "a folder where the install has a file, behind a delete of a folder",
        &[b"Data\\C.wz\\\x02", b"Data\\C.wz\\Sub\0"],
    );
    lie(
        "a file where the install has a folder, behind a delete of a file",
        &[b"Data\x02", &from_new_bytes(r"Data\Old")],
    );
    lie(
        "a file below a file of the install, behind a delete of a folder",
        &[b"Data\\C.wz\\\x02", &file(r"Data\C.wz\x.wz")],
    );
    lies
}

#[test]
fn patches_that_begin_honestly_and_then_lie_are_refused_whole_in_both_ways() {
    // Every lie is tried, and those that get through are told of together at the end.
    let mut through = Vec::new();
    for layout in LAYOUTS {
        let target = Target::new();
        for (what, contents) in lies(&target, layout) {
            let file = wrap(&contents);
            for way in ["safe", "immediate"] {
                let said = format!("{layout:?}, the {way} way, {what}");
                // Nothing is left of the run before, which may have built what this one
                // would then not need to build.
                target.reset();
                let attempt = || {
                    let result = match way {
                        "safe" => target.attempt(&file, &said),
                        _ => target.attempt_immediately(&file, &said).0,
                    };
                    let err = result.err().unwrap_or_else(|| panic!("{said}: taken"));
                    // It is refused for what it is, and not for want of room on the small
                    // drive. Only the lie that asks for two gigabytes may be.
                    let for_room = format!("{err:?}").contains("Space");
                    assert!(!for_room || what == "a copy as long as numbers go", "{said}: {err}");
                    // A copy that reaches past the end of an original that was checked is
                    // the patch lying, and is told of as that.
                    let past_the_end = what.starts_with("a copy that") || what.ends_with("as numbers go");
                    assert!(!past_the_end || for_room || matches!(err, Error::CorruptPatch(_)), "{said}: {err}");
                };
                if let Err(panic) = catch_unwind(AssertUnwindSafe(attempt)) {
                    through.push(said_by(panic));
                }
            }
        }
    }
    assert!(through.is_empty(), "{} lies were not refused whole:\n{}", through.len(), through.join("\n"));
}

#[test]
fn copy_past_the_end_of_an_original_that_was_checked_is_refused_as_a_lie_of_the_patch() {
    // The original is the very file the patch was made for: its checksum says so. A copy
    // that reads past its end asks for bytes that file never had. That is the patch lying,
    // not the disk failing, and it is said before anything is built, in either way.
    for layout in LAYOUTS {
        let target = Target::new();
        let len = target.before[C].len() as u32;
        for (start, count) in [(len - 1000, 2000), (len - 1000, 1001), (len, 1), (len + 4096, 10), (0, len + 1)] {
            let mut builder = target.prelude(layout);
            builder.raw(
                &[&rebuild_head(layout, C, &target.before[C], 0)[..], &copy_bytes(layout, C, start, count), &END]
                    .concat(),
            );
            let file = wrap(&builder.contents());
            let what = format!("{layout:?}, a copy of {count} at {start} of {len}");
            for immediate in [false, true] {
                target.reset();
                let result = match immediate {
                    false => target.attempt(&file, &what),
                    true => target.attempt_immediately(&file, &what).0,
                };
                assert!(
                    matches!(&result, Err(Error::CorruptPatch(detail)) if detail.contains(C)),
                    "{what}: {result:?}"
                );
                // Not a file was built for it.
                assert!(!target.install.join(".maple-fast-patch").join("new").exists(), "{what}: something was built");
            }
        }
        // A copy that ends exactly where the original ends is every other copy.
        target.reset();
        let ops = [copy(C, len as usize - 1000, 1000)];
        let mut builder = target.prelude(layout);
        builder.rebuild(C, &target.before[C], &target.built(&ops), &ops);
        assert!(target.try_contents(&builder.contents(), "a copy up to the last byte"), "{layout:?}");
    }
}

#[test]
fn patch_that_deletes_the_game_program_is_refused_and_one_that_replaces_it_is_not() {
    for layout in LAYOUTS {
        let target = Target::new();
        // A folder without the game's own program is no folder of the game: the next run
        // would not take it. No patch of the game deletes that program.
        for name in ["MapleStory.exe", "maplestory.EXE"] {
            let mut builder = target.honest(layout);
            builder.delete(name);
            for immediate in [false, true] {
                target.reset();
                let what = format!("{layout:?}, a delete of {name}, immediate {immediate}");
                let result = match immediate {
                    false => target.attempt(&builder.bytes(), &what),
                    true => target.attempt_immediately(&builder.bytes(), &what).0,
                };
                assert!(matches!(result, Err(Error::UnknownForm(_))), "{what}: {result:?}");
            }
        }

        // A new program in the place of the old one is what every patch brings, and a
        // delete of a folder of that name removes no file.
        target.reset();
        let new_program = pattern(100, 9);
        let mut builder = target.honest(layout);
        builder.create("MapleStory.exe", &new_program).delete("MapleStory.exe\\");
        assert!(target.try_contents(&builder.contents(), "a new game program"), "{layout:?}");
        assert!(target.files()["MapleStory.exe"] == new_program, "{layout:?}: the game program was not replaced");
    }
}

#[test]
fn original_that_the_table_has_another_checksum_for_is_refused_in_both_ways() {
    let target = Target::new();
    let ops = [copy(C, 0, 1000)];
    let mut builder = target.prelude(Layout::Table);
    builder.rebuild(C, &[], &target.built(&ops), &ops);
    let mut contents = builder.contents();
    // The checksum the table has for the third original, which is the last number of the
    // table: the patch is for another version of that file.
    let at = read_again(&contents, Layout::Table).ends[0] - 4;
    contents[at] ^= 1;
    let file = wrap(&contents);
    let named = |result: Result<Outcome, Error>| match result {
        Err(Error::OriginalMismatch(files)) => files,
        other => panic!("{other:?}"),
    };
    assert_eq!(named(target.attempt(&file, "the safe way")), [C]);
    assert_eq!(named(target.attempt_immediately(&file, "the immediate way").0), [C]);
}

// ---------------------------------------------------------------------------------------
// Bombs: small files that promise a great deal.
// ---------------------------------------------------------------------------------------

/// Compresses `bytes` by themselves into a piece of a compressed stream. Such a piece can
/// stand anywhere in a stream, any number of times, which is how a few kilobytes come to
/// hold gigabytes.
fn piece(bytes: &[u8]) -> Vec<u8> {
    let mut compress = flate2::Compress::new(flate2::Compression::best(), false);
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 1000 + 1024);
    compress.compress_vec(bytes, &mut out, flate2::FlushCompress::Sync).unwrap();
    assert_eq!(compress.total_in(), bytes.len() as u64, "the piece did not fit");
    out
}

/// A patch file whose compressed stream is `pieces` one after the other.
fn file_of_pieces(pieces: &[&[u8]]) -> Vec<u8> {
    // A stream begins with two bytes that say what it is, and ends with an empty last block.
    with_header(&[&[0x78, 0x01][..], &pieces.concat(), &[0x03, 0x00]].concat())
}

/// A megabyte that says the same thing over and over, compressed into one piece.
fn megabyte_of(unit: &[u8]) -> Vec<u8> {
    piece(&unit.repeat((1 << 20) / unit.len()))
}

#[test]
fn file_that_inflates_to_more_than_the_drive_holds_is_refused_before_a_byte_is_written() {
    // A whole file of 256 MB of zeros, with the right checksum: a patch of a few hundred
    // kilobytes that is honest about being a bomb.
    const MEGABYTES: usize = 256;
    let target = Target::new();
    let zeros = vec![0u8; 1 << 20];
    let crc = (0..MEGABYTES).fold(0, |crc, _| crc::update(crc, &zeros));
    let head = [&b"Data\\Bomb.wz\0"[..], &((MEGABYTES as u32) << 20).to_le_bytes(), &crc.to_le_bytes()].concat();
    let megabyte = megabyte_of(&[0]);
    let mut pieces: Vec<&[u8]> = vec![&megabyte[..]; MEGABYTES];
    let head = piece(&head);
    pieces.insert(0, &head);
    let file = file_of_pieces(&pieces);
    assert!(file.len() < 1 << 20, "the bomb is {} bytes", file.len());
    fs::write(&target.patch, &file).unwrap();

    // The drive has 64 MB free. Both ways of patching need the room for the file.
    for immediate in [false, true] {
        let options = Options { immediate, ..target.options() };
        let what = format!("the bomb, immediate: {immediate}");
        let install = target.install.clone();
        let (result, most) = bounded(120, &what, move || {
            let free = |_: &Path| -> io::Result<u64> { Ok(64 << 20) };
            let disk = Disk { free: &free, slack: 0, file_system: Some("NTFS") };
            let mut most = 0;
            let result = run_on_disk(&LIVE, &options, &disk, &GO, &mut |_| {
                most = most.max(support::tree_size(&install.join(".maple-fast-patch")));
            });
            (result, most)
        });
        target.judge(&result, &what);
        assert!(
            matches!(&result, Err(Error::Blocked(problems)) if format!("{problems:?}").contains("NotEnoughSpace")),
            "{result:?}"
        );
        assert!(most < 1 << 20, "{what}: {most} bytes were written to the work folder");
    }
}

/// A patch file with one rebuild entry that holds `megabytes` MB of instructions, each of
/// which brings no bytes: about a quarter of a million of them to the megabyte.
fn bomb_of_instructions(megabytes: usize) -> Vec<u8> {
    let head = piece(&rebuild_head(Layout::PerEntry, r"Data\Bomb.wz", &[], 0));
    let megabyte = megabyte_of(&0x8000_0000u32.to_le_bytes());
    let end = piece(&END);
    let mut pieces: Vec<&[u8]> = vec![&megabyte[..]; megabytes];
    pieces.insert(0, &head);
    pieces.push(&end);
    file_of_pieces(&pieces)
}

#[test]
fn entry_of_endless_instructions_can_be_paused_while_it_is_read() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bomb.patch");
    fs::write(&path, bomb_of_instructions(16)).unwrap();
    let patch = PatchFile::open(&path).unwrap();

    // Read to its end it is a patch like any other, if a slow one to read.
    let begun = Instant::now();
    let plan = plan::scan(&patch, &GO).unwrap();
    let whole = begun.elapsed();
    assert_eq!((plan.entries.len(), plan.new_bytes()), (1, 0));

    // A pause that comes while the one entry is being read ends the reading there, and
    // does not wait for the end of the entry.
    let stop = AtomicBool::new(false);
    let begun = Instant::now();
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(whole / 20);
            stop.store(true, Ordering::Relaxed);
        });
        plan::scan(&patch, &stop)
    });
    assert!(matches!(result, Err(Error::Stopped)), "{result:?}");
    assert!(begun.elapsed() < whole / 2, "paused after {:?} of the {whole:?} the reading takes", begun.elapsed());
}

#[test]
fn patch_that_names_more_originals_to_copy_from_than_any_patch_does_is_refused() {
    // Every rebuild keeps the names of the files it copies from. Five hundred and one
    // entries that each copy from two thousand files make a list of a million names out of
    // a patch file of a megabyte or two: what is kept has to have an end.
    let layout = Layout::Table;
    let sources: Vec<String> = (0..2000).map(|i| format!("s{i:04}.wz")).collect();
    let row = |name: &String| [&(name.len() as u32).to_le_bytes()[..], name.as_bytes(), &[0; 4]].concat();
    let table: Vec<u8> = 2000u32.to_le_bytes().into_iter().chain(sources.iter().flat_map(row)).collect();
    let copies: Vec<u8> = sources.iter().flat_map(|name| copy_bytes(layout, name, 0, 1)).chain(END).collect();
    let (table, copies) = (piece(&table), piece(&copies));
    let heads: Vec<Vec<u8>> = (0..501).map(|i| piece(&rebuild_head(layout, &format!("e{i:03}.wz"), &[], 0))).collect();
    let mut pieces: Vec<&[u8]> = vec![&table];
    for head in &heads {
        pieces.extend([&head[..], &copies[..]]);
    }
    let file = file_of_pieces(&pieces);
    assert!(file.len() < 4 << 20, "the patch is {} bytes", file.len());

    let target = Target::new();
    let result = target.attempt(&file, "a million names to copy from");
    assert!(matches!(result, Err(Error::UnknownForm(_))), "{result:?}");
}

#[test]
fn table_that_promises_a_billion_originals_is_refused_at_once() {
    let target = Target::new();
    for count in [1_000_000_000u32, 0x7FFF_FFFF, 100_001, 0xFFFF_FFFF] {
        let begun = Instant::now();
        let contents = [&count.to_le_bytes()[..], &9u32.to_le_bytes(), b"Data\\A.wz", &[0; 4]].concat();
        let what = format!("a table of {count} originals");
        assert!(target.attempt(&wrap(&contents), &what).is_err(), "{what}: taken");
        assert!(begun.elapsed() < Duration::from_secs(5), "{what}: refused after {:?}", begun.elapsed());
    }
}

#[test]
fn patch_of_as_many_entries_as_a_patch_may_have_does_not_keep_the_engine_for_minutes() {
    // Half of the hundred thousand delete, and the others come behind them. Whether an entry
    // comes after a delete of its name, or of a folder above it, is asked for every one of
    // them. Asked of every delete in turn, that is billions of questions.
    let layout = Layout::Table;
    let target = Target::new();
    let mut builder = target.prelude(layout);
    for i in 0..49_999 {
        builder.delete(&format!("d{i:05}.wz"));
    }
    for i in 0..49_999 {
        builder.create(&format!("c{i:05}.wz"), &[]);
    }
    let contents = builder.contents();
    let file = wrap(&contents);
    // None of the deletes names a file that is there, and a file of no bytes is not written:
    // what the patch does is what its first two entries do.
    let said = meaning(&contents, layout, &target.before).unwrap();
    assert!(said == meaning(&target.prelude(layout).contents(), layout, &target.before).unwrap());

    target.attempt(&file, "a hundred thousand entries, the safe way").unwrap();
    assert!(by_key(&target.files()) == said, "the safe way");
    target.reset();
    let (result, tried) = target.attempt_immediately(&file, "a hundred thousand entries, the immediate way");
    assert!(result.is_ok() && tried, "{result:?}, trial: {tried}");
    assert!(by_key(&target.files()) == said, "the immediate way");
}

// ---------------------------------------------------------------------------------------
// The patch files of the real game. These two are not part of the suite: the files are
// large and lie on one PC. They are only read. To run them:
//
//     set MFP_REAL_PATCHES=<a folder with .patch files>
//     cargo test --release -p mfp-engine --test attacks real_patch -- --ignored --nocapture
// ---------------------------------------------------------------------------------------

/// The patch files in the folder that `MFP_REAL_PATCHES` names.
fn real_patches() -> Vec<PathBuf> {
    let dir = std::env::var_os("MFP_REAL_PATCHES").expect("MFP_REAL_PATCHES names the folder with the patch files");
    let mut found: Vec<PathBuf> = fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    found.retain(|path| path.extension().is_some_and(|extension| extension == "patch"));
    found.sort();
    assert!(!found.is_empty(), "no .patch file in the folder");
    found
}

/// The layout of a real patch file and the first `len` bytes of its contents.
fn start_of_contents(path: &Path, len: usize) -> (Layout, Vec<u8>) {
    let patch = PatchFile::open(path).unwrap();
    let (_, layout, _) = plan::open_contents(&patch).unwrap();
    let mut stream = patch.stream().unwrap();
    let mut start = vec![0u8; len];
    stream.read_exact(&mut start).unwrap();
    (layout, start)
}

/// A patch file of `contents` that are stored as they are, which is quick to make of a
/// quarter of a megabyte thousands of times over.
fn stored(contents: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::none());
    encoder.write_all(contents).unwrap();
    with_header(&encoder.finish().unwrap())
}

#[test]
#[ignore = "reads the patch files of the real game: see the note above"]
fn real_patch_tables_and_headers_withstand_every_change() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("changed.patch");
    let reads = |file: &[u8], what: &str| {
        fs::write(&path, file).unwrap();
        let path = path.clone();
        let read = move || {
            let patch = PatchFile::open(&path)?;
            patch.verify(&GO, |_| {})?;
            plan::scan(&patch, &GO).map(drop)
        };
        bounded(120, what, read).is_ok()
    };

    for real in real_patches() {
        let name = real.file_name().unwrap().to_string_lossy().into_owned();

        // The header of the file itself, on a copy of its first megabyte: whatever is
        // changed there, the copy is never taken for a whole patch file.
        let mut first = vec![0u8; 1 << 20];
        io::Read::read_exact(&mut fs::File::open(&real).unwrap(), &mut first).unwrap();
        for at in 0..32 {
            for value in other_values(first[at]) {
                let mut file = first.clone();
                file[at] = value;
                assert!(
                    !reads(&file, &format!("{name}, byte {at} of the file")),
                    "{name}: byte {at} set to {value:#04x}"
                );
            }
        }

        // The table and the first entries, cut where an entry ends: a patch of its own,
        // with the names, counts and instructions the real builder wrote.
        let (layout, start) = start_of_contents(&real, 256 << 10);
        let reading = read_again(&start, layout);
        let Some(end) = reading.ends.last().copied() else { panic!("{name}: the table does not fit in the start") };
        let base = &start[..end];
        assert!(reads(&stored(base), &name), "{name}: its own table and first entries are not read");

        // Every number of the first kilobytes at its edges: the table, and the entries
        // that follow it.
        let len = base.len() as u32;
        let (mut refused, mut taken) = (0, 0);
        let mut strike = |contents: &[u8], what: &str| match reads(&stored(contents), what) {
            true => taken += 1,
            false => refused += 1,
        };
        let entries_at = reading.ends[0];
        let near = |field: &usize| *field < 8 << 10 || (entries_at..entries_at + (8 << 10)).contains(field);
        for field in reading.fields.iter().copied().filter(near) {
            let old = u32::from_le_bytes(base[field..field + 4].try_into().unwrap());
            let edges = [0, 1, 260, 261, 100_001, 1_000_000_000, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF, len, len + 1];
            for value in edges.into_iter().chain([old.wrapping_add(1), old.wrapping_sub(1)]) {
                let mut contents = base.to_vec();
                contents[field..field + 4].copy_from_slice(&value.to_le_bytes());
                strike(&contents, &format!("{name}, the number at {field} set to {value:#x}"));
            }
        }
        // Every byte of the first kilobyte, and those around where the entries begin.
        let around = entries_at.saturating_sub(256)..(entries_at + 256).min(base.len());
        for at in (0..base.len().min(1024)).chain(around) {
            for value in other_values(base[at]) {
                let mut contents = base.to_vec();
                contents[at] = value;
                strike(&contents, &format!("{name}, byte {at} of the contents set to {value:#04x}"));
            }
        }
        // And cut short at every third length up to the first entries.
        for cut in (0..(entries_at + 512).min(base.len())).step_by(3) {
            strike(&base[..cut], &format!("{name}, contents cut at {cut}"));
        }
        println!(
            "{name}: {layout:?}, table of {}, {} entries in the first {end} bytes; {refused} changes refused, {taken} \
             read as another patch",
            reading.table.len(),
            reading.entries.len()
        );
    }
}

#[test]
#[ignore = "reads the patch files of the real game: see the note above"]
fn real_patch_files_keep_far_inside_the_limits_a_patch_is_held_to() {
    for real in real_patches() {
        let name = real.file_name().unwrap().to_string_lossy().into_owned();
        let begun = Instant::now();
        let plan = plan::scan(&PatchFile::open(&real).unwrap(), &GO).unwrap();
        let sources = |entry: &plan::Entry| match &entry.action {
            plan::Action::Rebuild { sources, .. } => sources.len(),
            _ => 0,
        };
        let named: usize = plan.entries.iter().map(sources).sum();
        let most = plan.entries.iter().map(sources).max().unwrap_or(0);
        let names = || plan.entries.iter().map(|entry| &entry.name).chain(plan.originals.keys());
        let parts = |name: &String| name.split(['\\', '/']).map(str::len).collect::<Vec<_>>();
        let longest = names().map(String::len).max().unwrap_or(0);
        let longest_part = names().flat_map(parts).max().unwrap_or(0);
        let deepest = names().map(|name| parts(name).len()).max().unwrap_or(0);
        println!(
            "{name}: {:?}, {} entries, {} originals listed, {named} names of originals to copy from (at most {most} \
             in one entry), longest name {longest}, longest part {longest_part}, deepest {deepest}, contents of {} MB, \
             read in {:?}",
            plan.layout,
            plan.entries.len(),
            plan.originals.len(),
            plan.contents_len >> 20,
            begun.elapsed()
        );
        // The limits are 100,000 entries, 1,000,000 names to copy from, and 255 characters
        // in a part of a name. A real patch should not come within a tenth of any.
        assert!(plan.entries.len() < 10_000 && plan.originals.len() < 10_000, "{name}");
        assert!(named < 100_000 && longest_part < 100, "{name}");
    }
}
