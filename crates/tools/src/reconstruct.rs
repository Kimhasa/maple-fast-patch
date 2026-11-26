//! Makes a folder that a patch file can be applied to, out of an install that already has
//! the patch.
//!
//! A patch can only be tried on the version it was made for, and after patch day that version
//! is gone from the PC. But the patched files show what the old ones held: wherever the patch
//! says "copy these bytes from the old file", the new file has those very bytes at a known
//! place. Collecting them puts each old file back together as far as the patch ever reads it.
//! What the patch never reads is left as zeros, and four bytes are added at the end so the
//! file carries the checksum the patch expects of it.
//!
//! Applying the real patch to such a folder runs every instruction of the patch through the
//! engine, at full size, and must reproduce the patched install byte for byte.
//!
//! Some new files are no longer on disk as the patch made them, because later patches
//! changed them again. What their old files held cannot be recovered. For those entries the
//! unknown bytes are chosen so that the checksums still come out, which lets the run finish;
//! their results are stand-ins, and are reported as such.
//!
//! This is a test tool. It only reads the patched install.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mfp_engine::container::{PatchFile, PatchStream};
use mfp_engine::crc::{self, POLY};
use mfp_engine::error::{Error, Result};
use mfp_engine::plan::{self, Action, Header, Layout, Op, Plan};
use mfp_engine::preflight::{DATA_DIR_NAME, Programs};
use mfp_engine::stage::{self, Fingerprint};
use mfp_engine::storage::Profile;

const CHUNK: usize = 4 << 20;

/// What the patch's originals turned out to be, before anything is written.
#[derive(Debug, Default)]
pub struct Survey {
    /// Originals still on disk exactly as the patch expects them, and their total size.
    pub intact: usize,
    pub intact_bytes: u64,
    /// Originals that have to be put back together.
    pub to_recover: usize,
    /// Rebuild entries whose new file is on disk as the patch made it.
    pub rebuilds_on_disk: usize,
    /// Rebuild entries whose new file was changed again later; they get stand-ins.
    pub stand_ins: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub survey: Survey,
    /// Bytes of originals recovered from the new files.
    pub recovered_bytes: u64,
    /// Bytes of originals that no instruction reads; left as zeros.
    pub unread_bytes: u64,
    /// Recovered originals that came out with the checksum the patch expects, untouched:
    /// every byte of them was recovered, and correctly.
    pub exact: usize,
    /// Recovered originals that had four bytes added to carry the expected checksum.
    pub adjusted: usize,
    /// Copy instructions used, and those that read bytes another instruction had already
    /// supplied (the two must agree).
    pub copies: u64,
    pub copies_overlapping: u64,
    /// Places where two instructions disagree about what an original held. Must be empty.
    pub conflicts: Vec<String>,
    /// Stand-in entries for which no free bytes were found; the run will fail on them.
    pub unsolved: Vec<String>,
    /// Files and folders made only so that the patch's delete entries have something to remove.
    pub placeholders: Vec<String>,
}

/// Byte ranges of one file, kept merged and in order.
#[derive(Debug, Default)]
pub(crate) struct Ranges(BTreeMap<u64, u64>);

impl Ranges {
    /// Adds `start..end` and returns the parts of it that were already there.
    pub(crate) fn add(&mut self, start: u64, end: u64) -> Vec<(u64, u64)> {
        if start >= end {
            return Vec::new();
        }
        // Ranges are disjoint, so in order of start they are also in order of end: walking
        // down from the last one that starts at or before `end`, those that reach `start`
        // are the ones that touch the new range.
        let touching: Vec<(u64, u64)> = self
            .0
            .range(..=end)
            .rev()
            .take_while(|(_, range_end)| **range_end >= start)
            .map(|(s, e)| (*s, *e))
            .collect();
        let mut seen = Vec::new();
        let (mut merged_start, mut merged_end) = (start, end);
        for (range_start, range_end) in touching {
            let (from, to) = (range_start.max(start), range_end.min(end));
            if from < to {
                seen.push((from, to));
            }
            merged_start = merged_start.min(range_start);
            merged_end = merged_end.max(range_end);
            self.0.remove(&range_start);
        }
        self.0.insert(merged_start, merged_end);
        seen.sort_unstable();
        seen
    }

    pub(crate) fn total(&self) -> u64 {
        self.0.iter().map(|(start, end)| end - start).sum()
    }

    /// The parts of `start..end` that are not in here.
    fn missing(&self, start: u64, end: u64) -> Vec<(u64, u64)> {
        let mut gaps = Vec::new();
        let mut at = start;
        for (&range_start, &range_end) in self.0.range(..end) {
            if range_end <= at {
                continue;
            }
            if range_start > at {
                gaps.push((at, range_start.min(end)));
            }
            at = at.max(range_end);
            if at >= end {
                break;
            }
        }
        if at < end {
            gaps.push((at, end));
        }
        gaps
    }
}

/// An original being put back together.
struct Old {
    path: PathBuf,
    file: File,
    /// The bytes recovered so far.
    known: Ranges,
    /// The end of the furthest byte any instruction reads.
    extent: u64,
}

/// The polynomial that gives 1 when multiplied by x.
const X_INVERSE: u32 = (POLY >> 1) | 0x8000_0000;

/// Product of two polynomials modulo the checksum's polynomial. Bit 31 stands for x^31.
fn multiply(a: u32, b: u32) -> u32 {
    let mut product = 0u32;
    for bit in (0..32).rev() {
        let carry = product & 0x8000_0000 != 0;
        product <<= 1;
        if carry {
            product ^= POLY;
        }
        if (b >> bit) & 1 == 1 {
            product ^= a;
        }
    }
    product
}

/// x to the power of minus `exponent`.
fn x_to_minus(mut exponent: u64) -> u32 {
    let (mut result, mut base) = (1u32, X_INVERSE);
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = multiply(result, base);
        }
        base = multiply(base, base);
        exponent >>= 1;
    }
    result
}

/// The four bytes that, written over four zero bytes which are followed by `after` more
/// bytes, change a file's checksum from `now` to `wanted`.
///
/// The checksum of a file is the file, read as one long polynomial, times x^32, modulo the
/// checksum's polynomial. That is linear: changing bytes changes the checksum by the
/// checksum of the change alone.
fn bytes_for(now: u32, wanted: u32, after: u64) -> [u8; 4] {
    multiply(now ^ wanted, x_to_minus(8 * after + 32)).to_be_bytes()
}

/// The four bytes that, added at the end of a file whose checksum is `now`, make it `wanted`.
fn tail_for(now: u32, wanted: u32) -> [u8; 4] {
    (now ^ multiply(wanted, x_to_minus(32))).to_be_bytes()
}

/// What is known about the patched install before any reconstruction.
struct Sorted {
    /// Originals still on disk as the patch expects them.
    intact: BTreeSet<String>,
    /// Entries whose new file is on disk as the patch made it.
    on_disk: BTreeSet<String>,
    survey: Survey,
}

fn sort_out(plan: &Plan, patched: &Path, stop: &AtomicBool) -> Result<Sorted> {
    if plan.layout != Layout::Table {
        return Err(Error::corrupt("only patch files that list their originals can be reconstructed"));
    }
    let mut names: BTreeSet<String> = plan.originals.keys().cloned().collect();
    names.extend(plan.entries.iter().map(|entry| entry.name.clone()));
    let files = stage::files_present(names, patched);
    let prints = stage::fingerprint_all(&files, patched, Profile::fast().checkers, stop, &|_, _| {}, &|_, _| {})?;

    let mut sorted = Sorted { intact: BTreeSet::new(), on_disk: BTreeSet::new(), survey: Survey::default() };
    for (name, expected) in &plan.originals {
        match prints.get(name) {
            Some(print) if print.crc == *expected => {
                sorted.intact.insert(name.clone());
                sorted.survey.intact += 1;
                sorted.survey.intact_bytes += print.len;
            }
            _ => sorted.survey.to_recover += 1,
        }
    }
    for entry in &plan.entries {
        let Some(wanted) = Fingerprint::promised(entry) else { continue };
        if prints.get(&entry.name) == Some(&wanted) {
            sorted.on_disk.insert(entry.name.clone());
            if matches!(entry.action, Action::Rebuild { .. }) {
                sorted.survey.rebuilds_on_disk += 1;
            }
        } else if matches!(entry.action, Action::Rebuild { .. }) {
            sorted.survey.stand_ins.push(entry.name.clone());
        }
    }
    Ok(sorted)
}

/// Looks at the patched install and says what a reconstruction would involve. Writes nothing.
pub fn survey(plan: &Plan, patched: &Path, stop: &AtomicBool) -> Result<Survey> {
    Ok(sort_out(plan, patched, stop)?.survey)
}

/// Builds in `out` a folder the patch can be applied to. `out` must not exist yet.
///
/// With `link`, originals that are still on disk unchanged are hard-linked instead of
/// copied, which needs `out` to be on the same drive as `patched`.
pub fn reconstruct(
    patch: &PatchFile,
    plan: &Plan,
    patched: &Path,
    out: &Path,
    link: bool,
    stop: &AtomicBool,
    on_step: &mut dyn FnMut(&str),
) -> Result<Report> {
    if out.exists() {
        return Err(Error::file("create", out, std::io::ErrorKind::AlreadyExists.into()));
    }
    on_step("checking which files the install still has");
    let Sorted { intact, on_disk, survey } = sort_out(plan, patched, stop)?;
    let mut report = Report { survey, ..Report::default() };
    let stand_ins: BTreeSet<String> = report.survey.stand_ins.iter().cloned().collect();
    let in_out = |name: &str| -> Result<PathBuf> { Ok(out.join(plan::relative_path(name)?)) };
    let make_parent = |path: &Path| -> Result<()> {
        match path.parent() {
            Some(parent) => fs::create_dir_all(parent).map_err(|e| Error::file("create", parent, e)),
            None => Ok(()),
        }
    };

    on_step("taking over the originals that are unchanged");
    for name in &intact {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let (from, to) = (patched.join(plan::relative_path(name)?), in_out(name)?);
        make_parent(&to)?;
        let taken = if link { fs::hard_link(&from, &to) } else { fs::copy(&from, &to).map(drop) };
        taken.map_err(|e| Error::file("copy", &from, e))?;
    }

    // First pass: every copy instruction of a rebuild whose result is on disk tells what a
    // stretch of an original held.
    on_step("recovering the originals from the new files");
    let mut olds: HashMap<String, Old> = HashMap::new();
    let open_old = |olds: &mut HashMap<String, Old>, name: &str| -> Result<()> {
        if olds.contains_key(name) {
            return Ok(());
        }
        let path = in_out(name)?;
        make_parent(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::file("create", &path, e))?;
        olds.insert(name.to_owned(), Old { path, file, known: Ranges::default(), extent: 0 });
        Ok(())
    };
    for name in plan.originals.keys().filter(|name| !intact.contains(*name)) {
        open_old(&mut olds, name)?;
    }
    // Stretches of originals that only stand-in entries read: (start, end) per original.
    let mut wanted_by_stand_ins: HashMap<String, Vec<(u64, u64)>> = HashMap::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut existing = vec![0u8; CHUNK];

    let (mut stream, layout, _) = plan::open_contents(patch)?;
    for entry in &plan.entries {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let (name, header) = plan::next_entry(&mut stream, layout, &entry.name)?;
        match header {
            Header::Create { len, .. } => stream.skip(len)?,
            Header::Rebuild { .. } => {
                let mut new_file = match on_disk.contains(&name) {
                    true => {
                        let path = patched.join(plan::relative_path(&name)?);
                        Some((File::open(&path).map_err(|e| Error::file("open", &path, e))?, path))
                    }
                    false => None,
                };
                let mut offset = 0u64;
                plan::read_ops(&mut stream, layout, &name, |op, stream| {
                    let len = match &op {
                        Op::Literal { len } | Op::Fill { len, .. } | Op::Copy { len, .. } => *len,
                    };
                    match op {
                        Op::Literal { len } => stream.skip(len)?,
                        Op::Fill { .. } => {}
                        Op::Copy { from, .. } if intact.contains(&from) => {}
                        Op::Copy { from, start, len } => {
                            // A copy source the patch does not list is not expected, but
                            // would simply be one more original to put together.
                            open_old(&mut olds, &from)?;
                            let old = olds.get_mut(&from).expect("just opened");
                            old.extent = old.extent.max(start + len);
                            match &mut new_file {
                                None => wanted_by_stand_ins.entry(from).or_default().push((start, start + len)),
                                Some((file, path)) => {
                                    report.copies += 1;
                                    let overlaps = old.known.add(start, start + len);
                                    if !overlaps.is_empty() {
                                        report.copies_overlapping += 1;
                                    }
                                    let agreed = bring_back(
                                        (file, path.as_path(), offset),
                                        old,
                                        (start, len),
                                        &overlaps,
                                        &mut buffer,
                                        &mut existing,
                                    )?;
                                    if !agreed && report.conflicts.len() < 50 {
                                        report
                                            .conflicts
                                            .push(format!("{from} at {start}, {len} bytes (read by {name})"));
                                    }
                                    report.recovered_bytes += len;
                                }
                            }
                        }
                    }
                    offset += len;
                    Ok(())
                })?;
            }
            Header::CreateDir | Header::Delete => {}
        }
    }
    // Overlapping copies were counted once per instruction; what was recovered is the union.
    report.recovered_bytes = olds.values().map(|old| old.known.total()).sum();
    for old in olds.values_mut() {
        old.file.set_len(old.extent).map_err(|e| Error::file("write", &old.path, e))?;
    }

    // Second pass: entries whose result is not on disk. Their originals' unknown bytes are
    // zeros so far; four of them, read by this entry alone, are set so that the file the
    // engine builds from them has the checksum the patch expects.
    if !stand_ins.is_empty() {
        on_step("making stand-ins for files that later patches changed");
        let mut free = free_stretches(&wanted_by_stand_ins, &olds);
        let (mut stream, layout, _) = plan::open_contents(patch)?;
        for entry in &plan.entries {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Stopped);
            }
            let (name, header) = plan::next_entry(&mut stream, layout, &entry.name)?;
            match header {
                Header::Create { len, .. } => stream.skip(len)?,
                Header::Rebuild { new_crc, .. } if stand_ins.contains(&name) => {
                    let solved = stand_in(
                        &mut stream,
                        layout,
                        &name,
                        new_crc,
                        patched,
                        &intact,
                        &mut olds,
                        &mut free,
                        &mut buffer,
                    )?;
                    if !solved {
                        report.unsolved.push(name);
                    }
                }
                Header::Rebuild { .. } => plan::read_ops(&mut stream, layout, &name, |op, stream| match op {
                    Op::Literal { len } => stream.skip(len),
                    _ => Ok(()),
                })?,
                Header::CreateDir | Header::Delete => {}
            }
        }
    }

    // Last: each recovered original has to carry the checksum the patch expects of it.
    on_step("giving the recovered originals their checksums");
    for (name, old) in &mut olds {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        report.unread_bytes += old.extent - old.known.total();
        old.file.sync_all().map_err(|e| Error::file("write", &old.path, e))?;
        // Files that are only ever copied into, never listed, have no checksum to carry.
        let Some(&wanted) = plan.originals.get(name) else { continue };
        let now = stage::fingerprint(&old.path, stop, &|_| {})?.crc;
        if now == wanted {
            report.exact += 1;
            continue;
        }
        let tail = tail_for(now, wanted);
        if crc::update(now, &tail) != wanted {
            return Err(Error::corrupt(format!("could not give {name} its checksum")));
        }
        old.file
            .seek(SeekFrom::End(0))
            .and_then(|_| old.file.write_all(&tail))
            .map_err(|e| Error::file("write", &old.path, e))?;
        report.adjusted += 1;
    }
    drop(olds);

    // Something for the delete entries to delete.
    for entry in plan.entries.iter().filter(|entry| entry.action == Action::Delete) {
        let path = in_out(&entry.name)?;
        if path.exists() {
            continue;
        }
        if plan::names_folder(&entry.name) {
            fs::create_dir_all(&path).map_err(|e| Error::file("create", &path, e))?;
        } else {
            make_parent(&path)?;
            fs::write(&path, b"placeholder").map_err(|e| Error::file("create", &path, e))?;
        }
        report.placeholders.push(entry.name.clone());
    }
    Ok(report)
}

/// Makes the folder [`reconstruct`] built pass as an install folder of the client whose
/// programs are `programs`. The engine only works in a game folder, which it knows by the
/// game's program and its data folder. A patch that touches neither leaves them to be
/// supplied here, the program out of the patched install.
pub fn pass_as_install(programs: &Programs, patched: &Path, out: &Path) -> Result<()> {
    let program = out.join(programs.game);
    if !program.exists() {
        let from = patched.join(programs.game);
        fs::copy(&from, &program).map_err(|e| Error::file("copy", &from, e))?;
    }
    let data = out.join(DATA_DIR_NAME);
    fs::create_dir_all(&data).map_err(|e| Error::file("create", &data, e))
}

/// Copies `len` bytes of a new file, from `offset`, to where they came from in an original.
/// Returns whether the stretches that had already been recovered held the same bytes.
fn bring_back(
    (new_file, new_path, offset): (&mut File, &Path, u64),
    old: &mut Old,
    (start, len): (u64, u64),
    overlaps: &[(u64, u64)],
    buffer: &mut [u8],
    existing: &mut [u8],
) -> Result<bool> {
    let read_error = |e| Error::file("read", new_path, e);
    new_file.seek(SeekFrom::Start(offset)).map_err(read_error)?;
    let mut agreed = true;
    let mut done = 0u64;
    while done < len {
        let count = (len - done).min(buffer.len() as u64) as usize;
        new_file.read_exact(&mut buffer[..count]).map_err(read_error)?;
        let (chunk_start, chunk_end) = (start + done, start + done + count as u64);
        let old_error = |e| Error::file("write", &old.path, e);
        for &(from, to) in overlaps {
            let (from, to) = (from.max(chunk_start), to.min(chunk_end));
            if from >= to {
                continue;
            }
            let was = &mut existing[..(to - from) as usize];
            old.file.seek(SeekFrom::Start(from)).and_then(|_| old.file.read_exact(was)).map_err(old_error)?;
            let (a, b) = ((from - chunk_start) as usize, (to - chunk_start) as usize);
            agreed &= *was == buffer[a..b];
        }
        old.file
            .seek(SeekFrom::Start(chunk_start))
            .and_then(|_| old.file.write_all(&buffer[..count]))
            .map_err(old_error)?;
        done += count as u64;
    }
    Ok(agreed)
}

/// Stretches of the originals that exactly one stand-in instruction reads and that hold no
/// recovered bytes: free to be given any value without touching another file's contents.
fn free_stretches(
    wanted: &HashMap<String, Vec<(u64, u64)>>,
    olds: &HashMap<String, Old>,
) -> HashMap<String, Vec<(u64, u64)>> {
    let mut free = HashMap::new();
    for (name, stretches) in wanted {
        let mut edges: Vec<(u64, i32)> = stretches.iter().flat_map(|&(start, end)| [(start, 1), (end, -1)]).collect();
        edges.sort_unstable();
        let mut once = Vec::new();
        let (mut readers, mut since) = (0, 0u64);
        for (at, change) in edges {
            if readers == 1 && at > since {
                once.push((since, at));
            }
            readers += change;
            since = at;
        }
        let known = &olds[name].known;
        let unknown: Vec<(u64, u64)> = once.into_iter().flat_map(|(start, end)| known.missing(start, end)).collect();
        free.insert(name.clone(), unknown);
    }
    free
}

/// Goes through one stand-in entry the way the engine will, works out the checksum of what
/// the engine would build, and sets four free bytes of an original so that it becomes
/// `wanted`. Returns `false` when the entry reads no free bytes.
#[allow(clippy::too_many_arguments)]
fn stand_in(
    stream: &mut PatchStream,
    layout: Layout,
    name: &str,
    wanted: u32,
    patched: &Path,
    intact: &BTreeSet<String>,
    olds: &mut HashMap<String, Old>,
    free: &mut HashMap<String, Vec<(u64, u64)>>,
    buffer: &mut [u8],
) -> Result<bool> {
    let mut now = 0u32;
    let mut offset = 0u64;
    // The original, the place in it, and the place in the new file of the four bytes.
    let mut spot: Option<(String, u64, u64)> = None;
    plan::read_ops(stream, layout, name, |op, stream| {
        let len = match &op {
            Op::Literal { len } | Op::Fill { len, .. } | Op::Copy { len, .. } => *len,
        };
        match op {
            Op::Literal { len } => {
                let mut left = len;
                while left > 0 {
                    let count = left.min(buffer.len() as u64) as usize;
                    stream.read_exact(&mut buffer[..count])?;
                    now = crc::update(now, &buffer[..count]);
                    left -= count as u64;
                }
            }
            Op::Fill { byte, len } => {
                let count = len.min(buffer.len() as u64) as usize;
                buffer[..count].fill(byte);
                let mut left = len;
                while left > 0 {
                    let count = left.min(buffer.len() as u64) as usize;
                    now = crc::update(now, &buffer[..count]);
                    left -= count as u64;
                }
            }
            Op::Copy { from, start, len } => {
                let path = match olds.get(&from) {
                    Some(old) => old.path.clone(),
                    None if intact.contains(&from) => patched.join(plan::relative_path(&from)?),
                    None => return Err(Error::corrupt(format!("{name} copies from {from}, which is not an original"))),
                };
                let read_error = |e| Error::file("read", &path, e);
                let mut file = File::open(&path).map_err(read_error)?;
                file.seek(SeekFrom::Start(start)).map_err(read_error)?;
                let mut left = len;
                while left > 0 {
                    let count = left.min(buffer.len() as u64) as usize;
                    file.read_exact(&mut buffer[..count]).map_err(read_error)?;
                    now = crc::update(now, &buffer[..count]);
                    left -= count as u64;
                }
                if spot.is_none() {
                    let stretches = free.get_mut(&from).map(Vec::as_mut_slice).unwrap_or_default();
                    let here = stretches.iter_mut().find_map(|(free_start, free_end)| {
                        let (a, b) = ((*free_start).max(start), (*free_end).min(start + len));
                        (a + 4 <= b).then(|| {
                            // The stretch is used up, so that no second entry sets it.
                            *free_end = *free_start;
                            a
                        })
                    });
                    if let Some(at) = here {
                        spot = Some((from, at, offset + (at - start)));
                    }
                }
            }
        }
        offset += len;
        Ok(())
    })?;

    if now == wanted {
        return Ok(true);
    }
    let Some((from, at, in_new)) = spot else { return Ok(false) };
    let old = olds.get_mut(&from).expect("a free stretch belongs to an original being recovered");
    let bytes = bytes_for(now, wanted, offset - (in_new + 4));
    old.file
        .seek(SeekFrom::Start(at))
        .and_then(|_| old.file.write_all(&bytes))
        .and_then(|()| old.file.flush())
        .map_err(|e| Error::file("write", &old.path, e))?;
    old.known.add(at, at + 4);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small deterministic random numbers.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn bytes(&mut self, len: usize) -> Vec<u8> {
            (0..len).map(|_| self.next() as u8).collect()
        }
    }

    #[test]
    fn ranges_merge_and_report_what_was_already_there() {
        let mut ranges = Ranges::default();
        assert!(ranges.add(10, 20).is_empty());
        assert!(ranges.add(30, 40).is_empty());
        assert_eq!(ranges.add(15, 35), [(15, 20), (30, 35)]);
        assert_eq!(ranges.0.iter().map(|(s, e)| (*s, *e)).collect::<Vec<_>>(), [(10, 40)]);
        // Touching without overlapping merges but reports nothing.
        assert!(ranges.add(40, 50).is_empty());
        assert!(ranges.add(0, 10).is_empty());
        assert_eq!(ranges.total(), 50);
        assert_eq!(ranges.add(5, 6), [(5, 6)]);
        assert!(ranges.add(7, 7).is_empty());
    }

    #[test]
    fn missing_parts_are_what_no_range_covers() {
        let mut ranges = Ranges::default();
        ranges.add(10, 20);
        ranges.add(30, 40);
        assert_eq!(ranges.missing(0, 50), [(0, 10), (20, 30), (40, 50)]);
        assert_eq!(ranges.missing(12, 18), []);
        assert_eq!(ranges.missing(15, 35), [(20, 30)]);
        assert_eq!(ranges.missing(40, 45), [(40, 45)]);
        assert_eq!(Ranges::default().missing(3, 9), [(3, 9)]);
    }

    #[test]
    fn x_inverse_undoes_a_multiplication_by_x() {
        assert_eq!(multiply(X_INVERSE, 2), 1);
        assert_eq!(multiply(x_to_minus(32), x_to_minus(8)), x_to_minus(40));
        for value in [1u32, 0x8000_0000, 0xDEAD_BEEF, POLY] {
            assert_eq!(multiply(multiply(value, 2), X_INVERSE), value);
        }
    }

    #[test]
    fn four_bytes_at_the_end_give_a_file_any_checksum() {
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        for len in [0usize, 1, 3, 4, 17, 4096, 100_003] {
            let mut data = rng.bytes(len);
            let wanted = rng.next() as u32;
            let tail = tail_for(crc::update(0, &data), wanted);
            data.extend(tail);
            assert_eq!(crc::update(0, &data), wanted, "length {len}");
        }
    }

    #[test]
    fn four_bytes_in_the_middle_give_a_file_any_checksum() {
        let mut rng = Rng(0x0F0F_1234_5678_9ABC);
        for (len, at) in [(4usize, 0usize), (10, 0), (10, 6), (5000, 1234), (200_000, 199_996), (200_000, 7)] {
            let mut data = rng.bytes(len);
            data[at..at + 4].fill(0);
            let wanted = rng.next() as u32;
            let bytes = bytes_for(crc::update(0, &data), wanted, (len - at - 4) as u64);
            data[at..at + 4].copy_from_slice(&bytes);
            assert_eq!(crc::update(0, &data), wanted, "length {len}, at {at}");
        }
    }

    #[test]
    fn free_stretches_are_read_once_and_not_yet_known() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old");
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).unwrap();
        let mut known = Ranges::default();
        known.add(100, 120);
        let olds = HashMap::from([("a".to_owned(), Old { path, file, known, extent: 400 })]);
        // 0..200 and 150..300 are read; 150..200 by both, 100..120 is already known.
        let wanted = HashMap::from([("a".to_owned(), vec![(0, 200), (150, 300)])]);
        let free = free_stretches(&wanted, &olds);
        assert_eq!(free["a"], [(0, 100), (120, 150), (200, 300)]);
    }
}
