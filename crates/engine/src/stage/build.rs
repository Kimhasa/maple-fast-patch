//! Builds the new files: patch data and pieces of original files, written in the order the
//! patch file gives, checked against the promised checksum and moved to the staged files.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use super::BUFFER;
use super::originals::{Checked, Fingerprint, fingerprint_all};
use super::work::{Dirs, prepare_work, remove_unfinished};
use crate::busy::when_free;
use crate::container::{PatchFile, PatchStream};
use crate::crc;
use crate::error::{Error, Result};
use crate::plan::{self, Action, Header, Layout, Op, Plan};
use crate::storage::{Profile, read_at};

/// The most that is fetched from an original file in one read. Longer copies are fetched as
/// several pieces, so one long copy keeps several reader threads busy.
const PIECE: usize = BUFFER;

/// How many bytes of a new file may sit in memory waiting for their turn to be written:
/// pieces being fetched from originals, pieces fetched, and patch data read ahead of them.
///
/// A rebuild is slow because each copy waits for the drive (about 0.2 ms on an NVMe drive,
/// measured), not because of the amount of data, so several copies are fetched at once. The
/// window has to hold one full piece per reader thread (8 x 1 MiB) with room to spare for
/// pieces that arrived before an earlier, slower one. 32 MiB does that, and it is a fixed
/// amount whatever the size of the patch: together with the read and write buffers a rebuild
/// stays under 40 MiB, which a PC with 8 GB does not notice.
const WINDOW: usize = 32 << 20;

/// What one waiting part is counted as on top of its bytes, so that a patch made of a
/// million empty instructions cannot queue a million of them either.
const PART_COST: usize = 64;

/// What the buffer of a fetched piece may hold beyond the piece itself: a read past the
/// file cache starts and ends on a [`SECTOR`] boundary and needs an aligned buffer.
const PIECE_SLACK: usize = 3 * SECTOR;

/// Offsets and lengths of reads that bypass the file cache are multiples of this, and so is
/// the address of the buffer. Drives use 512 or 4096 bytes per sector and 4096 serves both.
/// A drive that wants something else refuses the read, which then goes the ordinary way.
const SECTOR: usize = 4096;

/// Bytes of finished files that may wait to be synced before the next finished file has to
/// wait as well. Syncing happens off the writing thread so that the next file can be built
/// meanwhile; this keeps the building from running far ahead of what is safely on disk. A
/// single file larger than this is still accepted when nothing else is waiting.
const UNSYNCED_CAP: u64 = 256 << 20;

/// How often a thread that waits for another looks whether that one has failed.
const POLL: Duration = Duration::from_millis(100);

/// Builds every new file into the work folder, checking each against the checksum in the
/// patch file. Files already built from this same patch file by an earlier, interrupted run
/// are kept when they check out.
///
/// `on_progress` hears the file, how many more bytes of the new files are done, and whether
/// they are of a file that was kept rather than built now.
///
/// A run does this through [`stage_with`]. This one is here for tests and for the test tools.
#[doc(hidden)]
pub fn stage(
    patch: &PatchFile,
    plan: &Plan,
    checked: &Checked,
    dirs: &Dirs,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(&str, u64, bool),
) -> Result<()> {
    stage_with(&Profile::fast(), patch, plan, checked, dirs, stop, on_progress)
}

/// [`stage`] on a drive that is read the way `profile` says. What is built is the same.
pub fn stage_with(
    profile: &Profile,
    patch: &PatchFile,
    plan: &Plan,
    checked: &Checked,
    dirs: &Dirs,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(&str, u64, bool),
) -> Result<()> {
    // Everything that is built is written into the work folder, which is looked at once
    // more before that begins: it must not have become a way out of the install folder.
    crate::apply::guard_work(dirs)?;
    prepare_work(patch, dirs)?;
    // Whatever an earlier run was still writing when it was cut off is not a finished file.
    remove_unfinished(dirs);
    Build { profile, patch, plan, checked, dirs, stop }.run(false, on_progress)
}

/// Builds every new file that `checked` leaves to be built, the way [`stage_with`] does, and
/// keeps none of them: each is checked against the checksum the patch file promises and goes
/// nowhere. Nothing is written, in the work folder or anywhere else.
///
/// This tells whether a patch can be built from the files that are there. The immediate way
/// asks before it replaces the first of them, because from then on there is no way back.
/// `on_progress` hears the file and how many more bytes of the new files have been tried.
pub(crate) fn try_build(
    profile: &Profile,
    patch: &PatchFile,
    plan: &Plan,
    checked: &Checked,
    dirs: &Dirs,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(&str, u64),
) -> Result<()> {
    Build { profile, patch, plan, checked, dirs, stop }.run(true, &mut |name, bytes, _| on_progress(name, bytes))
}

/// What a build is given to work with.
struct Build<'a> {
    profile: &'a Profile,
    patch: &'a PatchFile,
    plan: &'a Plan,
    checked: &'a Checked,
    dirs: &'a Dirs,
    stop: &'a AtomicBool,
}

impl Build<'_> {
    /// Builds every file that is still to be built: into the work folder, or for a `trial`
    /// to nowhere. A trial does not look at the staged files either. Whoever asks for one
    /// has counted those that are good as done.
    fn run(&self, trial: bool, on_progress: &mut dyn FnMut(&str, u64, bool)) -> Result<()> {
        let Build { profile, patch, plan, checked, dirs, stop } = *self;
        let (stream, layout, _) = plan::open_contents(patch)?;
        if layout != plan.layout {
            return Err(plan::changed());
        }
        let kept = match trial {
            true => BTreeSet::new(),
            false => verify_staged(plan, checked, dirs, profile.checkers, stop, &mut |name, bytes| {
                on_progress(name, bytes, true)
            })?,
        };

        let flusher = Flusher::default();
        let quit = AtomicBool::new(false);
        let (jobs, waiting_jobs) = mpsc::channel::<Job>();
        let waiting_jobs = Mutex::new(waiting_jobs);
        let (finished, waiting_files) = mpsc::channel::<Finished>();
        // Copies only occur in rebuilds; a patch of whole files needs no reader threads.
        let rebuilds = |entry: &plan::Entry| {
            matches!(entry.action, Action::Rebuild { .. }) && checked.builds(entry) && !kept.contains(&entry.name)
        };
        let readers = if plan.entries.iter().any(rebuilds) { profile.readers.max(1) } else { 0 };

        let built = std::thread::scope(|scope| {
            // Both sending ends are owned in here, so that every way out of this block closes
            // them. That is what ends the threads: no reader and no flusher outlives `stage`,
            // and none still holds a file open when it returns.
            let (jobs, finished) = (jobs, finished);
            let _quit = RaiseOnDrop(&quit);
            let thread = |name: &str| std::thread::Builder::new().name(name.to_owned());
            // Fewer readers than wanted still do the work; none at all cannot.
            let (mut started, mut refused) = (0, None);
            for _ in 0..readers {
                match thread("mfp-reader").spawn_scoped(scope, || read_jobs(&waiting_jobs, &quit, profile.unbuffered)) {
                    Ok(_) => started += 1,
                    Err(e) => refused = Some(e),
                }
            }
            if let (0, Some(e)) = (started, refused) {
                return Err(Error::io("start a thread to read the original files", e));
            }
            let readers = started;
            thread("mfp-flusher")
                .spawn_scoped(scope, || flusher.run(waiting_files))
                .map_err(|e| Error::io("start a thread to finish the new files", e))?;

            let builder = Builder {
                plan,
                checked,
                dirs,
                stop,
                layout,
                trial,
                kept: &kept,
                fetch: Fetch { jobs: &jobs, readers, install: &dirs.install, known: HashMap::new() },
                done: Done { flusher: &flusher, line: &finished },
            };
            builder.run(stream, &mut |name, bytes| on_progress(name, bytes, false))
        });

        // Every thread has ended. A pause, or the builder noticing that the flusher failed,
        // gives way to what the flusher has to report; any other error is the one to show.
        match (built, flusher.failure()) {
            (Err(err), _) if !matches!(err, Error::Stopped) => Err(err),
            (_, Some(err)) => Err(err),
            (built, None) => built,
        }
    }
}

/// Sets a flag when it goes out of scope, however that happens.
struct RaiseOnDrop<'a>(&'a AtomicBool);

impl Drop for RaiseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Checks the files an earlier, interrupted run of this patch already built, `checkers` at
/// a time, and returns the names of those that are exactly what the patch file promises.
/// Each of them is reported as done here; the others are built again.
pub(crate) fn verify_staged(
    plan: &Plan,
    checked: &Checked,
    dirs: &Dirs,
    checkers: usize,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(&str, u64),
) -> Result<BTreeSet<String>> {
    let staged_dir = dirs.staged();
    let mut wanted = BTreeMap::new();
    for entry in plan.entries.iter().filter(|entry| checked.builds(entry)) {
        let Some(print) = Fingerprint::promised(entry) else { continue };
        let staged = staged_dir.join(plan::relative_path(&entry.name)?);
        // A file of another size is not read at all.
        if fs::metadata(staged).is_ok_and(|meta| meta.is_file() && meta.len() == print.len) {
            wanted.insert(entry.name.clone(), print);
        }
    }
    let mut kept = BTreeSet::new();
    if wanted.is_empty() {
        return Ok(kept);
    }

    let files: Vec<(String, u64)> = wanted.iter().map(|(name, print)| (name.clone(), print.len)).collect();
    let (checked_file, checked_files) = mpsc::channel::<(String, Fingerprint)>();
    std::thread::scope(|scope| {
        // The checking runs beside this thread, which alone may call `on_progress`. The
        // sending end moves into the checking thread and closes when that thread is done.
        let checking = scope.spawn(move || {
            let on_file = |name: &str, print: Fingerprint| drop(checked_file.send((name.to_owned(), print)));
            fingerprint_all(&files, &staged_dir, checkers, stop, &|_, _| {}, &on_file).map(drop)
        });
        for (name, print) in checked_files {
            if wanted.get(&name) == Some(&print) {
                on_progress(&name, print.len);
                kept.insert(name);
            }
        }
        checking.join().expect("the thread that checks staged files panicked")
    })?;
    Ok(kept)
}

/// What building needs to know, gathered so that it can be handed around in one piece.
struct Builder<'a> {
    plan: &'a Plan,
    checked: &'a Checked,
    dirs: &'a Dirs,
    stop: &'a AtomicBool,
    layout: Layout,
    /// Whether the files are built to nowhere, only to see that they can be built.
    trial: bool,
    /// Staged files that [`verify_staged`] found complete. They are already reported.
    kept: &'a BTreeSet<String>,
    fetch: Fetch<'a>,
    done: Done<'a>,
}

impl Builder<'_> {
    /// Goes through the patch contents once more and builds every file that is not there
    /// yet. Runs on the calling thread: it alone parses, checksums, writes and reports.
    fn run(mut self, mut stream: PatchStream, on_progress: &mut dyn FnMut(&str, u64)) -> Result<()> {
        let (plan, checked, stop, layout) = (self.plan, self.checked, self.stop, self.layout);
        let trial = self.trial;
        let staged_dir = self.dirs.staged();
        let mut buffer = vec![0u8; BUFFER];

        for (index, entry) in plan.entries.iter().enumerate() {
            let (name, header) = plan::next_entry(&mut stream, layout, &entry.name)?;
            let staged = staged_dir.join(plan::relative_path(&name)?);
            // Looked at for every entry, so that a file that could not be synced or moved
            // ends the run soon and not only after everything else was built.
            self.done.flusher.check()?;

            // The contents are read a second time here. Everything the plan promised about
            // this entry has to still be true, or the staged file would not be what was
            // planned.
            match (header, &entry.action) {
                (Header::CreateDir, Action::CreateDir) | (Header::Delete, Action::Delete) => {}
                (Header::Create { len, crc }, Action::Create { len: planned_len, crc: planned_crc }) => {
                    if (len, crc) != (*planned_len, *planned_crc) {
                        return Err(plan::changed());
                    }
                    if len == 0 {
                        continue;
                    }
                    if !checked.builds(entry) {
                        stream.skip(len)?;
                        on_progress(&name, len);
                        continue;
                    }
                    if self.kept.contains(&name) {
                        stream.skip(len)?;
                        continue;
                    }
                    let building = (!trial).then(|| self.dirs.building(index));
                    let built = Output::open(building.as_deref(), &staged).and_then(|mut out| {
                        copy_from_patch(&mut stream, &mut out, len, &mut buffer, stop, &mut |n| on_progress(&name, n))?;
                        out.finish(&name, Fingerprint { crc, len }, &self.done)
                    });
                    discard_unless_built(building.as_deref(), built)?;
                }
                (Header::Rebuild { new_crc, .. }, Action::Rebuild { new_crc: planned_crc, new_len, .. }) => {
                    if new_crc != *planned_crc {
                        return Err(plan::changed());
                    }
                    if !checked.builds(entry) {
                        skip_ops(&mut stream, layout, &name, stop)?;
                        on_progress(&name, *new_len);
                        continue;
                    }
                    if self.kept.contains(&name) {
                        skip_ops(&mut stream, layout, &name, stop)?;
                        continue;
                    }
                    let building = (!trial).then(|| self.dirs.building(index));
                    let fetch = &mut self.fetch;
                    let built = Output::open(building.as_deref(), &staged).and_then(|mut out| {
                        let mut ahead = Ahead::default();
                        let mut turn =
                            Turn { out: &mut out, buffer: &mut buffer, stop, report: &mut |n| on_progress(&name, n) };
                        plan::read_ops(&mut stream, layout, &name, |op, stream| {
                            if stop.load(Ordering::Relaxed) {
                                return Err(Error::Stopped);
                            }
                            match op {
                                Op::Literal { len } => ahead.literal(stream, len, &mut turn),
                                Op::Fill { byte, len } => ahead.fill(byte, len, &mut turn),
                                Op::Copy { from, start, len } => ahead.copy(&mut *fetch, &from, start, len, &mut turn),
                            }
                        })?;
                        ahead.write_all(&mut turn)?;
                        out.finish(&name, Fingerprint { crc: new_crc, len: *new_len }, &self.done)
                    });
                    discard_unless_built(building.as_deref(), built)?;
                }
                _ => return Err(plan::changed()),
            }
        }
        Ok(())
    }
}

/// Removes the file an entry was being written to when building it failed or was paused.
/// The file is closed by then: it was either handed on complete or dropped. A trial has no
/// such file.
fn discard_unless_built(building: Option<&Path>, built: Result<()>) -> Result<()> {
    if let (Some(building), Err(_)) = (building, &built) {
        let _ = fs::remove_file(building);
    }
    built
}

/// Passes over the instructions of a rebuild that is not built now. A pause is looked for at
/// every one of them: an entry can hold as many as the patch file has room for.
fn skip_ops(stream: &mut PatchStream, layout: Layout, name: &str, stop: &AtomicBool) -> Result<()> {
    plan::read_ops(stream, layout, name, |op, stream| match op {
        _ if stop.load(Ordering::Relaxed) => Err(Error::Stopped),
        Op::Literal { len } => stream.skip(len),
        Op::Fill { .. } | Op::Copy { .. } => Ok(()),
    })
}

fn copy_from_patch(
    stream: &mut PatchStream,
    out: &mut Output,
    mut len: u64,
    buffer: &mut [u8],
    stop: &AtomicBool,
    report: &mut dyn FnMut(u64),
) -> Result<()> {
    while len > 0 {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let count = len.min(buffer.len() as u64) as usize;
        stream.read_exact(&mut buffer[..count])?;
        out.write(&buffer[..count])?;
        report(count as u64);
        len -= count as u64;
    }
    Ok(())
}

/// One part of a new file whose turn to be written has not come yet.
enum Part {
    /// Bytes from the patch file.
    Data(Vec<u8>),
    Fill {
        byte: u8,
        len: u64,
    },
    /// Bytes of an original file that a reader thread is fetching or has fetched.
    Copy(mpsc::Receiver<Result<Piece>>),
}

/// Where a part goes when its turn has come: into the new file, which checksums what it is
/// given, with progress reported for bytes written and not for bytes merely read.
struct Turn<'a> {
    out: &'a mut Output,
    buffer: &'a mut [u8],
    stop: &'a AtomicBool,
    report: &'a mut dyn FnMut(u64),
}

/// The parts of one new file that are known but not written yet, oldest first.
///
/// Copies are what a rebuild waits for, so they are asked for as soon as the instruction is
/// read and written when their turn comes. Whatever lies between two copies in the new file
/// has to wait in line with them. Everything is written in exactly the order of the
/// instructions, by the one thread that reads them.
#[derive(Default)]
struct Ahead {
    parts: VecDeque<(Part, usize)>,
    /// What the waiting parts count as, which never exceeds [`WINDOW`].
    held: usize,
}

impl Ahead {
    fn literal(&mut self, stream: &mut PatchStream, mut len: u64, turn: &mut Turn) -> Result<()> {
        while len > 0 {
            if self.parts.is_empty() {
                // Nothing is in line, so the rest goes straight from the patch file to the
                // new file.
                return copy_from_patch(stream, turn.out, len, turn.buffer, turn.stop, turn.report);
            }
            let count = len.min(PIECE as u64) as usize;
            self.make_room(count + PART_COST, turn)?;
            let mut data = vec![0u8; count];
            stream.read_exact(&mut data)?;
            self.add(Part::Data(data), count + PART_COST, turn)?;
            len -= count as u64;
        }
        Ok(())
    }

    fn fill(&mut self, byte: u8, len: u64, turn: &mut Turn) -> Result<()> {
        self.make_room(PART_COST, turn)?;
        self.add(Part::Fill { byte, len }, PART_COST, turn)
    }

    fn copy(&mut self, fetch: &mut Fetch, from: &str, start: u64, len: u64, turn: &mut Turn) -> Result<()> {
        let source = fetch.source(from)?;
        let (mut at, mut left) = (start, len);
        while left > 0 {
            let count = left.min(PIECE as u64) as usize;
            let cost = count + PIECE_SLACK + PART_COST;
            // Room is made before the piece is asked for: what is being fetched counts.
            self.make_room(cost, turn)?;
            self.add(Part::Copy(fetch.ask(&source, at, count)?), cost, turn)?;
            at += count as u64;
            left -= count as u64;
        }
        Ok(())
    }

    /// Writes the oldest parts, waiting for them if need be, until `more` fits in the window.
    fn make_room(&mut self, more: usize, turn: &mut Turn) -> Result<()> {
        while self.held + more > WINDOW && self.write_oldest(turn, true)? {}
        Ok(())
    }

    /// Puts a part at the end of the line, then writes whatever is ready at its front.
    fn add(&mut self, part: Part, cost: usize, turn: &mut Turn) -> Result<()> {
        self.parts.push_back((part, cost));
        self.held += cost;
        while self.write_oldest(turn, false)? {}
        Ok(())
    }

    /// Writes everything in line. A new file is complete only after this.
    fn write_all(&mut self, turn: &mut Turn) -> Result<()> {
        while self.write_oldest(turn, true)? {}
        Ok(())
    }

    /// Writes the oldest part and says whether there was one to write. A copy that has not
    /// arrived yet is waited for if `wait` is set and left in line otherwise.
    fn write_oldest(&mut self, turn: &mut Turn, wait: bool) -> Result<bool> {
        let arrived = match self.parts.front() {
            None => return Ok(false),
            Some((Part::Copy(arrives), _)) => Some(if wait {
                arrives.recv().map_err(|_| readers_gone())??
            } else {
                match arrives.try_recv() {
                    Ok(piece) => piece?,
                    Err(mpsc::TryRecvError::Empty) => return Ok(false),
                    Err(mpsc::TryRecvError::Disconnected) => return Err(readers_gone()),
                }
            }),
            Some(_) => None,
        };
        if turn.stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let (part, cost) = self.parts.pop_front().expect("looked at above");
        self.held -= cost;
        match (part, arrived) {
            (Part::Fill { byte, len }, _) => turn.out.fill(byte, len, turn.buffer, turn.report)?,
            (Part::Data(data), _) => {
                turn.out.write(&data)?;
                (turn.report)(data.len() as u64);
            }
            (Part::Copy(_), piece) => {
                let piece = piece.expect("fetched above");
                turn.out.write(piece.bytes())?;
                (turn.report)(piece.len as u64);
            }
        }
        Ok(true)
    }
}

/// A piece was asked for and no reader thread will answer. In a run that is still going
/// that only happens when a reader thread panicked.
fn readers_gone() -> Error {
    Error::io("read the original files", io::Error::other("a reader thread stopped"))
}

/// An original file that copies are taken from.
struct Source {
    /// Tells the files apart for the reader threads, which each keep their own handles.
    id: usize,
    path: PathBuf,
}

/// A piece of an original file for a reader thread to fetch.
struct Job {
    source: Arc<Source>,
    start: u64,
    len: usize,
    reply: mpsc::Sender<Result<Piece>>,
}

/// A fetched piece: `len` bytes at `at` in `buf`.
struct Piece {
    buf: Vec<u8>,
    at: usize,
    len: usize,
}

impl Piece {
    fn bytes(&self) -> &[u8] {
        &self.buf[self.at..self.at + self.len]
    }
}

/// The building thread's side of fetching: it names the pieces, the reader threads get them.
struct Fetch<'a> {
    jobs: &'a mpsc::Sender<Job>,
    /// How many reader threads there are to answer.
    readers: usize,
    install: &'a Path,
    known: HashMap<String, Arc<Source>>,
}

impl Fetch<'_> {
    fn source(&mut self, name: &str) -> Result<Arc<Source>> {
        if let Some(source) = self.known.get(name) {
            return Ok(source.clone());
        }
        let source = Arc::new(Source { id: self.known.len(), path: self.install.join(plan::relative_path(name)?) });
        self.known.insert(name.to_owned(), source.clone());
        Ok(source)
    }

    /// Asks for `len` bytes at `start` of an original. The answer comes when a reader thread
    /// has them, or has found that they cannot be read.
    fn ask(&self, source: &Arc<Source>, start: u64, len: usize) -> Result<mpsc::Receiver<Result<Piece>>> {
        // Without readers the answer would never come.
        if self.readers == 0 {
            return Err(readers_gone());
        }
        let (reply, arrives) = mpsc::channel();
        self.jobs.send(Job { source: source.clone(), start, len, reply }).map_err(|_| readers_gone())?;
        Ok(arrives)
    }
}

/// A reader thread: fetches the pieces asked for until nobody asks any more.
///
/// Each reader opens the originals itself. Windows runs the reads on one handle one after
/// the other, so readers sharing a handle would be no faster than one (measured).
/// `unbuffered` says how they are opened: see [`Handle::open`].
fn read_jobs(jobs: &Mutex<mpsc::Receiver<Job>>, quit: &AtomicBool, unbuffered: bool) {
    let mut open: HashMap<usize, Handle> = HashMap::new();
    loop {
        // The lock is held while waiting. That is fine: the others have nothing to do
        // either, and the next job goes to whoever holds it.
        let job = jobs.lock().unwrap().recv();
        let Ok(job) = job else { return };
        // After a failure or a pause nobody waits for what is still in line.
        if quit.load(Ordering::Relaxed) {
            continue;
        }
        let _ = job.reply.send(fetch(&mut open, &job, unbuffered));
    }
}

/// An original file as one reader thread has it open.
struct Handle {
    file: File,
    /// Whether reads on this handle bypass the file cache, which makes them subject to the
    /// rules [`read_direct`] follows.
    direct: bool,
}

impl Handle {
    /// Opens an original for reading: past the file cache where that is asked for and
    /// possible, through it otherwise.
    ///
    /// A rebuild reads every original it copies from about once, in pieces all over the
    /// file, while tens of gigabytes of new files go through the same cache. Measured on an
    /// SSD with four readers, an 8 GB rebuild took 19 s reading through the cache and 10 s
    /// past it.
    ///
    /// A hard disk is another matter: past the cache Windows reads nothing ahead, and every
    /// piece is a trip of its own. There the cache is used, with Windows told to read ahead
    /// as it does for a file read from front to back. Whether that suits the copies of a
    /// rebuild has not been measured on a hard disk yet.
    fn open(path: &Path, unbuffered: bool) -> io::Result<Handle> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN};
            if !unbuffered {
                let file = fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_SEQUENTIAL_SCAN).open(path)?;
                return Ok(Handle { file, direct: false });
            }
            // Not every file system offers this. Where it does not, the ordinary way does.
            if let Ok(file) = fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_NO_BUFFERING).open(path) {
                return Ok(Handle { file, direct: true });
            }
        }
        // Only Windows has the two ways to choose between.
        #[cfg(not(windows))]
        let _ = unbuffered;
        Ok(Handle { file: File::open(path)?, direct: false })
    }
}

fn fetch(open: &mut HashMap<usize, Handle>, job: &Job, unbuffered: bool) -> Result<Piece> {
    let path = &job.source.path;
    let handle = match open.entry(job.source.id) {
        Entry::Occupied(found) => found.into_mut(),
        Entry::Vacant(place) => place.insert(Handle::open(path, unbuffered).map_err(|e| Error::file("open", path, e))?),
    };
    if handle.direct {
        if let Ok(piece) = read_direct(&handle.file, job.start, job.len) {
            return Ok(piece);
        }
        // Reading past the cache is only a faster way to the same bytes. Whatever went
        // wrong, the ordinary way has the last word: it gets the bytes, or the error to
        // report. This reader then stays with it for this file.
        *handle = Handle { file: File::open(path).map_err(|e| Error::file("open", path, e))?, direct: false };
    }
    // Running past the end of an original means it is not the file the patch expects.
    read_cached(&handle.file, job.start, job.len).map_err(|e| Error::file("read", path, e))
}

/// Reads `len` bytes at `start` through the file cache.
fn read_cached(file: &File, start: u64, len: usize) -> io::Result<Piece> {
    let mut buf = vec![0u8; len];
    let mut done = 0;
    while done < len {
        match read_at(file, &mut buf[done..], start + done as u64) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => done += count,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Piece { buf, at: 0, len })
}

/// Reads `len` bytes at `start` from a file opened to bypass the file cache.
///
/// Such a file is only read in whole sectors, at offsets that are multiples of the sector
/// size, into a buffer whose address is one too. So the read covers the wanted bytes from
/// the sector boundary before them to the one after them, and the piece says where in the
/// buffer the wanted bytes are. The buffer is allocated one sector longer than needed, so
/// that an aligned address can be found inside it without any unsafe code.
///
/// A file rarely ends on a sector boundary. Reading the last, partial sector is asked for
/// as a whole sector like any other and returns the bytes there are; a short read is
/// therefore the end of the file, and is only an error if the wanted bytes are not all in.
fn read_direct(file: &File, start: u64, len: usize) -> io::Result<Piece> {
    let sector = SECTOR as u64;
    let from = start / sector * sector;
    let to = (start + len as u64).div_ceil(sector) * sector;
    let (span, skip) = ((to - from) as usize, (start - from) as usize);

    let mut buf = vec![0u8; span + SECTOR];
    let pad = buf.as_ptr().align_offset(SECTOR);
    if pad >= SECTOR {
        return Err(io::Error::other("no aligned buffer"));
    }
    let mut got = 0;
    while got < skip + len {
        let count = match read_at(file, &mut buf[pad + got..pad + span], from + got as u64) {
            Ok(count) => count,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        got += count;
        // Anything but a whole number of sectors was the end of the file, and the next
        // read could not start on a boundary anyway.
        if count == 0 || got % SECTOR != 0 {
            break;
        }
    }
    if got < skip + len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Piece { buf, at: pad + skip, len })
}

/// A new file that is complete and verified, on its way to the staged files.
struct Finished {
    file: File,
    building: PathBuf,
    path: PathBuf,
    len: u64,
}

/// Where the building thread hands finished files on.
struct Done<'a> {
    flusher: &'a Flusher,
    line: &'a mpsc::Sender<Finished>,
}

/// Makes finished files safe on disk and then moves them among the staged files, on a
/// thread of its own, so that the next file is built while the last one is synced.
///
/// The order within one file stays what it was: complete, checksum verified, synced, and
/// only then renamed into `new`. A file under `new` is always whole and on disk.
#[derive(Default)]
struct Flusher {
    /// Bytes of finished files handed over and not yet synced.
    waiting: Mutex<u64>,
    eased: Condvar,
    /// The first file that could not be synced or moved.
    failure: Mutex<Option<Error>>,
    /// Set once there is a failure, and when the flusher thread ends.
    failed: AtomicBool,
}

impl Flusher {
    /// Takes a finished file. Returns at once, unless more than [`UNSYNCED_CAP`] would then
    /// be waiting: in that case it returns when enough of it has been synced.
    fn take(&self, line: &mpsc::Sender<Finished>, done: Finished) -> Result<()> {
        let mut waiting = self.waiting.lock().unwrap();
        while *waiting > 0 && *waiting + done.len > UNSYNCED_CAP && !self.failed.load(Ordering::Relaxed) {
            waiting = self.eased.wait_timeout(waiting, POLL).unwrap().0;
        }
        self.check()?;
        *waiting += done.len;
        drop(waiting);
        line.send(done).map_err(|_| Error::Stopped)
    }

    /// Fails once a file could not be synced or moved. The error returned is a stand-in:
    /// [`stage`] replaces it with the one the flusher recorded.
    fn check(&self) -> Result<()> {
        if self.failed.load(Ordering::Relaxed) { Err(Error::Stopped) } else { Ok(()) }
    }

    fn failure(&self) -> Option<Error> {
        self.failure.lock().unwrap().take()
    }

    /// The flusher thread: runs until the building thread closes the line, which it does
    /// when it is done or has given up. Files already handed over are finished either way,
    /// so a pause keeps everything that was complete.
    fn run(&self, line: mpsc::Receiver<Finished>) {
        // Nobody may wait for a flusher that is gone.
        let _ended = RaiseOnDrop(&self.failed);
        for Finished { file, building, path, len } in line {
            let synced = file.sync_all();
            drop(file);
            let moved = synced.map_err(|e| Error::file("write", &building, e)).and_then(|()| {
                // A stale file from an earlier attempt may sit where this one goes.
                let _ = fs::remove_file(&path);
                when_free(|| fs::rename(&building, &path)).map_err(|e| Error::file("rename", &building, e))
            });
            if let Err(err) = moved {
                let _ = fs::remove_file(&building);
                self.failure.lock().unwrap().get_or_insert(err);
                self.failed.store(true, Ordering::Relaxed);
            }
            *self.waiting.lock().unwrap() -= len;
            self.eased.notify_all();
        }
    }
}

/// A new file being written. It only moves to its place among the staged files once
/// complete and verified, so a staged file is always whole.
///
/// In a trial nothing is written: the bytes are checksummed and go nowhere.
struct Output {
    path: PathBuf,
    building: PathBuf,
    /// The file being written. `None` in a trial.
    writer: Option<BufWriter<File>>,
    print: Fingerprint,
}

impl Output {
    /// The output of an entry: the file `building`, or nothing at all when there is none.
    fn open(building: Option<&Path>, path: &Path) -> Result<Output> {
        let Some(building) = building else {
            let print = Fingerprint { crc: 0, len: 0 };
            return Ok(Output { path: PathBuf::new(), building: PathBuf::new(), writer: None, print });
        };
        Output::create(building, path)
    }

    fn create(building: &Path, path: &Path) -> Result<Output> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::file("create", parent, e))?;
        }
        let file = File::create(building).map_err(|e| Error::file("create", building, e))?;
        Ok(Output {
            path: path.to_path_buf(),
            building: building.to_path_buf(),
            writer: Some(BufWriter::with_capacity(BUFFER, file)),
            print: Fingerprint { crc: 0, len: 0 },
        })
    }

    fn write(&mut self, data: &[u8]) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer.write_all(data).map_err(|e| Error::file("write", &self.building, e))?;
        }
        self.print.crc = crc::update(self.print.crc, data);
        self.print.len += data.len() as u64;
        Ok(())
    }

    fn fill(&mut self, byte: u8, mut len: u64, buffer: &mut [u8], report: &mut dyn FnMut(u64)) -> Result<()> {
        let chunk = len.min(buffer.len() as u64) as usize;
        buffer[..chunk].fill(byte);
        while len > 0 {
            let count = len.min(chunk as u64) as usize;
            self.write(&buffer[..count])?;
            report(count as u64);
            len -= count as u64;
        }
        Ok(())
    }

    /// Checks the finished file against what the patch file promises and hands it on to be
    /// synced and moved among the staged files. A file that does not check out goes nowhere,
    /// and neither does the output of a trial.
    fn finish(self, name: &str, wanted: Fingerprint, done: &Done) -> Result<()> {
        let Output { path, building, writer, print } = self;
        let file = match writer {
            Some(writer) => Some(writer.into_inner().map_err(|e| Error::file("write", &building, e.into_error()))?),
            None => None,
        };
        if print != wanted {
            return Err(Error::NewFileMismatch(name.to_owned()));
        }
        match file {
            Some(file) => done.flusher.take(done.line, Finished { file, building, path, len: print.len }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes that differ from place to place, so a read from the wrong offset shows.
    fn pattern(len: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    /// Offsets worth trying in a file of `size` bytes: its start and end, and every sector
    /// boundary near them with the bytes around it.
    fn edges(size: usize) -> Vec<usize> {
        let mut edges = vec![0, 1, 2, SECTOR - 1, SECTOR, SECTOR + 1, 2 * SECTOR - 1, 2 * SECTOR, 2 * SECTOR + 1];
        let last_boundary = size / SECTOR * SECTOR;
        for near in [size, last_boundary, last_boundary.saturating_sub(SECTOR)] {
            edges.extend([near.saturating_sub(2), near.saturating_sub(1), near, near + 1, near + 2]);
        }
        edges.extend([size / 2, size / 3, size + SECTOR, size + SECTOR + 1, size + 3 * SECTOR]);
        edges.sort();
        edges.dedup();
        edges
    }

    #[test]
    fn reads_past_the_file_cache_return_exactly_the_bytes_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let sizes = [
            1,
            2,
            511,
            512,
            513,
            4095,
            4096,
            4097,
            8191,
            8192,
            8193,
            12_411,
            65_536,
            70_001,
            PIECE + 1,
            PIECE + SECTOR + 17,
        ];
        let mut reads = 0;
        for size in sizes {
            let contents = pattern(size);
            let path = dir.path().join(format!("{size}.bin"));
            fs::write(&path, &contents).unwrap();
            let handle = Handle::open(&path, true).unwrap();
            // On Windows this must be the real thing: a handle that bypasses the cache and
            // refuses reads that are not aligned. Elsewhere the same code reads an ordinary
            // file, which checks the arithmetic.
            assert_eq!(handle.direct, cfg!(windows), "the temp folder does not allow reads past the cache");
            let cached = File::open(&path).unwrap();

            let edges = edges(size);
            for &start in &edges {
                for &end in &edges {
                    if end <= start || end - start > PIECE {
                        continue;
                    }
                    let len = end - start;
                    let what = format!("{len} bytes at {start} of {size}");
                    let direct = read_direct(&handle.file, start as u64, len);
                    let ordinary = read_cached(&cached, start as u64, len);
                    if end <= size {
                        let direct = direct.unwrap_or_else(|e| panic!("{what}: {e}"));
                        assert!(direct.bytes() == &contents[start..end], "{what}: wrong bytes past the cache");
                        assert!(
                            ordinary.unwrap().bytes() == &contents[start..end],
                            "{what}: wrong bytes through the cache"
                        );
                    } else {
                        // Asking for more than the file has is an error both ways.
                        assert_eq!(direct.err().map(|e| e.kind()), Some(io::ErrorKind::UnexpectedEof), "{what}");
                        assert_eq!(ordinary.err().map(|e| e.kind()), Some(io::ErrorKind::UnexpectedEof), "{what}");
                    }
                    reads += 1;
                }
            }
        }
        assert!(reads > 2000, "only {reads} reads were tried");
    }

    #[test]
    fn a_whole_piece_is_read_past_the_cache_at_any_offset() {
        // The longest read there is, starting at every offset within a sector and ending
        // at, before and after a boundary, the last one at the very end of the file.
        let dir = tempfile::tempdir().unwrap();
        let size = 3 * PIECE + 1234;
        let contents = pattern(size);
        let path = dir.path().join("big.bin");
        fs::write(&path, &contents).unwrap();
        let handle = Handle::open(&path, true).unwrap();
        for start in (0..SECTOR + 2).step_by(97).chain([SECTOR - 1, SECTOR, size - PIECE, size - PIECE - 1]) {
            let piece = read_direct(&handle.file, start as u64, PIECE).unwrap();
            assert!(piece.bytes() == &contents[start..start + PIECE], "a piece at {start}");
            assert!(piece.buf.len() <= PIECE + PIECE_SLACK, "a piece at {start} took {} bytes", piece.buf.len());
        }
        assert!(read_direct(&handle.file, (size - PIECE + 1) as u64, PIECE).is_err());
    }

    fn job(path: &Path, start: u64, len: usize) -> (Job, mpsc::Receiver<Result<Piece>>) {
        let (reply, arrives) = mpsc::channel();
        (Job { source: Arc::new(Source { id: 0, path: path.to_path_buf() }), start, len, reply }, arrives)
    }

    #[test]
    fn read_that_fails_past_the_cache_is_done_the_ordinary_way() {
        let dir = tempfile::tempdir().unwrap();
        let contents = pattern(10_000);
        let path = dir.path().join("original.bin");
        fs::write(&path, &contents).unwrap();

        // A handle that is said to bypass the cache but cannot be read at all stands in for
        // a drive that refuses such reads.
        let unreadable = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let mut open = HashMap::from([(0, Handle { file: unreadable, direct: true })]);
        let (first, _) = job(&path, 4000, 3000);
        assert!(fetch(&mut open, &first, true).unwrap().bytes() == &contents[4000..7000]);
        assert!(!open[&0].direct, "the reader did not switch to the ordinary way");
        let (second, _) = job(&path, 9999, 1);
        assert!(fetch(&mut open, &second, true).unwrap().bytes() == &contents[9999..]);

        // What the ordinary way cannot read either is reported as the read error it is.
        let (beyond, _) = job(&path, 9000, 2000);
        let err = fetch(&mut open, &beyond, true).err().unwrap();
        assert!(
            matches!(&err, Error::Io { what, source } if what.starts_with("read ") && source.kind() == io::ErrorKind::UnexpectedEof),
            "{err}"
        );

        // And so is a read beyond the end on a handle that does bypass the cache.
        let mut open = HashMap::new();
        let err = fetch(&mut open, &beyond, true).err().unwrap();
        assert!(
            matches!(&err, Error::Io { what, source } if what.starts_with("read ") && source.kind() == io::ErrorKind::UnexpectedEof),
            "{err}"
        );

        let (missing, _) = job(&dir.path().join("missing.bin"), 0, 1);
        let mut open = HashMap::new();
        let err = fetch(&mut open, &missing, true).err().unwrap();
        assert!(
            matches!(&err, Error::Io { what, source } if what.starts_with("open ") && source.kind() == io::ErrorKind::NotFound),
            "{err}"
        );
    }

    #[test]
    fn reader_threads_answer_every_job_and_end_when_nobody_asks_any_more() {
        let dir = tempfile::tempdir().unwrap();
        let contents = pattern(300_000);
        let path = dir.path().join("original.bin");
        fs::write(&path, &contents).unwrap();

        let (jobs, waiting_jobs) = mpsc::channel::<Job>();
        let waiting_jobs = Mutex::new(waiting_jobs);
        let quit = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let jobs = jobs;
            for _ in 0..4 {
                scope.spawn(|| read_jobs(&waiting_jobs, &quit, true));
            }
            let source = Arc::new(Source { id: 0, path: path.clone() });
            let mut asked = Vec::new();
            for i in 0..500usize {
                let (start, len) = ((i * 577) % 290_000, 1 + (i * 131) % 9000);
                let (reply, arrives) = mpsc::channel();
                jobs.send(Job { source: source.clone(), start: start as u64, len, reply }).unwrap();
                asked.push((start, len, arrives));
            }
            for (start, len, arrives) in asked {
                let piece = arrives.recv().unwrap().unwrap();
                assert!(piece.bytes() == &contents[start..start + len], "{len} bytes at {start}");
            }

            // Once told to quit, a reader lets jobs go unanswered instead of reading for
            // nobody. The one who asked sees the line closed, not silence.
            quit.store(true, Ordering::Relaxed);
            let (reply, arrives) = mpsc::channel();
            jobs.send(Job { source, start: 0, len: 10, reply }).unwrap();
            assert!(arrives.recv().is_err());
            // Leaving the scope drops `jobs`; the scope only returns if every reader ends.
        });
    }

    #[test]
    fn flusher_moves_files_in_place_and_reports_the_first_that_fails() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("new");
        fs::create_dir_all(&staged).unwrap();
        let flusher = Flusher::default();
        let (line, waiting) = mpsc::channel::<Finished>();
        let finished = |name: &str, target: PathBuf| {
            let building = dir.path().join(name);
            fs::write(&building, name).unwrap();
            let file = fs::OpenOptions::new().append(true).open(&building).unwrap();
            Finished { file, building, path: target, len: name.len() as u64 }
        };
        // A stale file where the new one goes is replaced.
        fs::write(staged.join("a"), b"stale").unwrap();
        flusher.take(&line, finished("building-1.tmp", staged.join("a"))).unwrap();
        // A file that cannot be moved to its place: the folder does not exist.
        flusher.take(&line, finished("building-2.tmp", dir.path().join("missing").join("b"))).unwrap();
        // The third is in line behind the one that will fail before the flusher starts. Handed
        // over while it runs, it would be refused whenever the failure happened to come first.
        flusher.take(&line, finished("building-3.tmp", staged.join("c"))).unwrap();
        std::thread::scope(|scope| {
            let line = line;
            scope.spawn(|| flusher.run(waiting));
            // The failure is seen without waiting for the end.
            while flusher.check().is_ok() {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(flusher.take(&line, finished("building-4.tmp", staged.join("d"))).is_err());
        });
        assert_eq!(fs::read(staged.join("a")).unwrap(), b"building-1.tmp");
        assert_eq!(
            fs::read(staged.join("c")).unwrap(),
            b"building-3.tmp",
            "a file after the failed one was not finished"
        );
        assert!(!staged.join("d").exists());
        assert!(!dir.path().join("building-1.tmp").exists() && !dir.path().join("building-2.tmp").exists());
        let err = flusher.failure().expect("the failure was not recorded");
        assert!(
            matches!(&err, Error::Io { what, .. } if what.starts_with("rename ") && what.ends_with("building-2.tmp")),
            "{err}"
        );
        assert_eq!(*flusher.waiting.lock().unwrap(), 0);
    }

    #[test]
    fn finished_files_wait_when_too_much_is_not_synced_yet() {
        let dir = tempfile::tempdir().unwrap();
        let flusher = Flusher::default();
        let (line, waiting) = mpsc::channel::<Finished>();
        let finished = |name: &str, len: u64| {
            let building = dir.path().join(format!("{name}.tmp"));
            let file = File::create(&building).unwrap();
            Finished { file, building, path: dir.path().join(name), len }
        };
        // Nobody syncs yet. A file larger than the cap is taken when nothing waits, and a
        // small one after it has to wait.
        flusher.take(&line, finished("huge", UNSYNCED_CAP + 1)).unwrap();
        std::thread::scope(|scope| {
            let line = line;
            let (second_line, flusher, finished) = (line.clone(), &flusher, &finished);
            // The thread says when its clock runs. One that started late on a busy PC would
            // otherwise measure a shorter wait than the one it is made to sit through.
            let (clock_runs, begun) = mpsc::channel();
            let waited = scope.spawn(move || {
                let started = std::time::Instant::now();
                clock_runs.send(()).unwrap();
                flusher.take(&second_line, finished("small", 1)).unwrap();
                started.elapsed()
            });
            begun.recv().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            assert!(!waited.is_finished(), "a file was taken while more than the cap was waiting");
            // The flusher starts, syncs the first file, and the second is let in.
            let run = scope.spawn(move || flusher.run(waiting));
            assert!(waited.join().unwrap() >= Duration::from_millis(250));
            drop(line);
            run.join().unwrap();
        });
        assert!(dir.path().join("huge").exists() && dir.path().join("small").exists());
        assert!(flusher.failure().is_none());
    }

    #[test]
    fn originals_are_read_through_the_file_cache_when_that_is_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let contents = pattern(70_001);
        let path = dir.path().join("original.bin");
        fs::write(&path, &contents).unwrap();
        assert!(!Handle::open(&path, false).unwrap().direct);

        let mut open = HashMap::new();
        for (start, len) in [(0usize, 1usize), (4095, 2), (65_000, 5_001), (0, 70_001)] {
            let (wanted, _) = job(&path, start as u64, len);
            assert!(
                fetch(&mut open, &wanted, false).unwrap().bytes() == &contents[start..start + len],
                "{len} at {start}"
            );
        }
        assert!(!open[&0].direct);
        let (beyond, _) = job(&path, 70_000, 2);
        let err = fetch(&mut open, &beyond, false).err().unwrap();
        assert!(matches!(&err, Error::Io { source, .. } if source.kind() == io::ErrorKind::UnexpectedEof), "{err}");
    }

    #[test]
    fn what_waits_to_be_written_never_exceeds_the_window() {
        // A copy that takes its time holds up everything behind it. Twice the window of
        // patch data follows; it may only pile up as far as the window allows, then has to
        // wait for the copy, and everything comes out in the order it went in.
        let dir = tempfile::tempdir().unwrap();
        let building = dir.path().join("building-0.tmp");
        let mut out = Output::create(&building, &dir.path().join("new").join("x")).unwrap();
        let mut buffer = vec![0u8; BUFFER];
        let stop = AtomicBool::new(false);
        let mut written = 0u64;
        let mut ahead = Ahead::default();
        let mut most = 0;
        {
            let mut turn = Turn { out: &mut out, buffer: &mut buffer, stop: &stop, report: &mut |n| written += n };
            let (reply, arrives) = mpsc::channel::<Result<Piece>>();
            // The copy arrives when it is told that the line behind it is full, and not after
            // some time: on a busy PC the line could take longer to fill than any time chosen.
            let (full, filled) = mpsc::channel::<()>();
            let slow = std::thread::spawn(move || {
                let _ = filled.recv();
                let _ = reply.send(Ok(Piece { buf: vec![0xEE; 300], at: 100, len: 100 }));
            });
            ahead.add(Part::Copy(arrives), 100 + PIECE_SLACK + PART_COST, &mut turn).unwrap();
            for i in 0..64u8 {
                if ahead.held + PIECE + PART_COST > WINDOW {
                    let _ = full.send(());
                }
                ahead.make_room(PIECE + PART_COST, &mut turn).unwrap();
                ahead.add(Part::Data(vec![i; PIECE]), PIECE + PART_COST, &mut turn).unwrap();
                assert!(ahead.held <= WINDOW, "{} bytes in line after part {i}", ahead.held);
                most = most.max(ahead.held);
            }
            ahead.write_all(&mut turn).unwrap();
            slow.join().unwrap();
        }
        assert!(most > WINDOW - 2 * PIECE, "the line never filled up: {most} bytes at most");
        assert!(ahead.parts.is_empty() && ahead.held == 0);
        assert_eq!(written, 100 + 64 * PIECE as u64);

        out.writer.as_mut().unwrap().flush().unwrap();
        let bytes = fs::read(&building).unwrap();
        assert!(bytes[..100].iter().all(|byte| *byte == 0xEE));
        for (i, part) in bytes[100..].chunks(PIECE).enumerate() {
            assert!(part.len() == PIECE && part.iter().all(|byte| *byte == i as u8), "part {i} is out of place");
        }
        assert_eq!(out.print, Fingerprint { crc: crc::update(0, &bytes), len: bytes.len() as u64 });
    }
}
