//! The immediate way of patching: new files take their place in the install folder while
//! others are still to be built.
//!
//! The safe way builds every new file first, so the drive has to hold all of them next to
//! the files they replace: 34.5 GiB for one real patch of 920 MB. Here the files are built a
//! round at a time. After each round, every new file whose original nothing still needs is
//! moved into place, and the room the original took is there for the next round.
//!
//! The price is that such a move is not undone. A run that stops partway leaves an install
//! folder that is partly the old version and partly the new one. The folder is marked as
//! such (see `stage::pin`), each of its files is wholly old or wholly new, and the same
//! patch can go on from any point. Nothing else can: the files that are built and not placed
//! yet are the only copy of what they hold, because originals they were built from have
//! been replaced.
//!
//! This way is the player's choice, made before a patch is started. With the choice made,
//! every run goes this way from its start, whatever room the drive has; without it, none
//! does. With room for everything, all the new files are built in one round and then
//! placed, and what is left of the difference to the safe way is that placing is not
//! undone.
//!
//! A file that may not be placed ahead of its turn, because an earlier entry deletes the
//! folder it goes into, waits among the staged files. It goes in at the end, with the
//! deletes and the new folders, through the apply that keeps a journal and can be undone.
//!
//! Because there is no way back, what can be known beforehand is found out beforehand. The
//! way is only begun on a drive where a file moved over another survives a power cut whole
//! (see `storage::Volume::takes_immediate`), and no file is replaced before every new file
//! has been built once, so that a patch that cannot be built is found while the folder is
//! still the old version. Where the drive has room for all the new files, they are built in
//! one round before any of them is placed, and that round shows it. Where it has not, the
//! files of one round are placed before those of the next are built, so a trial comes
//! first: every new file is built once and kept nowhere. And once a folder is half patched,
//! the patch file that began there is used from this PC for as long as it is there, whatever
//! the server has under its address by then (see `run::run_on_disk`).

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::apply;
use crate::busy::when_free;
use crate::container::PatchFile;
use crate::error::{Error, Result, blocked_unless_empty};
use crate::plan::{self, Action, Entry, Plan};
use crate::preflight::{self, Programs, SPACE_MARGIN, SpaceNeed};
use crate::run::{Event, Stage};
use crate::stage::{self, Checked, Dirs};
use crate::storage::Profile;

/// Room the immediate way asks for at its start beyond the least it can work with. With
/// exactly the least, every round would build a single file, and every round reads the patch
/// file from its beginning again, which takes about 3 s for a real patch.
///
/// It is not asked for beyond the room for everything that is to be built: see
/// [`room_to_start`].
///
/// Public for tests, which work out from it how much room a run asks for.
#[doc(hidden)]
pub const SLACK: u64 = 1 << 30;

/// What a run in the immediate way is given to work with.
///
/// Public for tests, which run [`build_and_place`] on its own.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct Job<'a> {
    /// The programs of the client the folder is for. None of them may be running when a
    /// file goes into place.
    pub programs: &'a Programs,
    pub patch: &'a PatchFile,
    pub plan: &'a Plan,
    pub dirs: &'a Dirs,
    pub profile: &'a Profile,
    /// Says how many bytes are free on the drive that holds a folder.
    pub free: &'a dyn Fn(&Path) -> io::Result<u64>,
    /// Room to ask for at the start beyond the least that works: [`SLACK`], or less to make
    /// a test's drive a tight one.
    pub slack: u64,
    pub stop: &'a AtomicBool,
}

/// Names of the entries whose new file an earlier run of this patch built and left among the
/// staged files, each checked against its checksum and not by its size alone. None when the
/// staged files are not this patch's.
///
/// In a half-patched install folder this has to be known before the originals are checked:
/// see [`stage::check_originals_except`].
///
/// Public for tests, which ask it what a run that was cut off has left.
#[doc(hidden)]
pub fn built_before(
    profile: &Profile,
    patch: &PatchFile,
    plan: &Plan,
    dirs: &Dirs,
    stop: &AtomicBool,
) -> Result<BTreeSet<String>> {
    if !stage::staged_is_ours(patch, dirs) {
        return Ok(BTreeSet::new());
    }
    stage::verify_staged(plan, &Checked::default(), dirs, profile.checkers, stop, &mut |_, _| {})
}

/// The most room the immediate way needs beyond what is on the drive now: the highest that
/// "bytes written, less the bytes of the originals replaced so far" gets when one file is
/// built at a time and everything that may be placed is placed after each.
///
/// `built` names the new files that are among the staged files already. They are written,
/// so they cost nothing more, and placing them gives the room of their originals back. The
/// sizes of the originals are read from `install` as it is now.
///
/// Public for tests, which give a made-up drive exactly this much room.
#[doc(hidden)]
pub fn space_needed(plan: &Plan, checked: &Checked, built: &BTreeSet<String>, install: &Path) -> u64 {
    let order = Order::new(plan);
    let mut checked = checked.clone();
    let builds = |entry: &&Entry| checked.builds(entry);
    let mut staged: BTreeSet<String> = plan
        .entries
        .iter()
        .filter(builds)
        .filter(|entry| built.contains(&entry.name))
        .map(|entry| entry.name.clone())
        .collect();
    let to_build: Vec<&Entry> =
        plan.entries.iter().filter(builds).filter(|entry| !staged.contains(&entry.name)).collect();

    let mut used: i64 = -place_on_paper(&order, &mut checked, &mut staged, install);
    let mut peak: i64 = 0;
    for entry in to_build {
        used += entry.new_len() as i64;
        peak = peak.max(used);
        staged.insert(entry.name.clone());
        used -= place_on_paper(&order, &mut checked, &mut staged, install);
    }
    peak as u64
}

/// The room the immediate way asks the drive for before it builds anything: the least it
/// can work with (see [`space_needed`]) and `slack` on top, but never more than the new files
/// that are still to be built take all together. With room for all of them there is one
/// round, and nothing left for the slack to spare. So this way never asks for more than
/// the safe way does, however small the patch. `built` is as for [`space_needed`].
pub(crate) fn room_to_start(
    plan: &Plan,
    checked: &Checked,
    built: &BTreeSet<String>,
    install: &Path,
    slack: u64,
) -> u64 {
    let still_to_build = |entry: &&Entry| checked.builds(entry) && !built.contains(&entry.name);
    let all: u64 = plan.entries.iter().filter(still_to_build).map(Entry::new_len).sum();
    space_needed(plan, checked, built, install).saturating_add(slack).min(all)
}

/// Does on paper what a pass of placing does: every staged file that may be placed counts as
/// placed. Returns the bytes of the originals that would be replaced.
fn place_on_paper(order: &Order, checked: &mut Checked, staged: &mut BTreeSet<String>, install: &Path) -> i64 {
    let mut freed = 0;
    for index in order.ready(checked, staged) {
        let entry = &order.plan.entries[index];
        let original =
            plan::relative_path(&entry.name).ok().and_then(|relative| fs::metadata(install.join(relative)).ok());
        freed += original.filter(|meta| meta.is_file()).map_or(0, |meta| meta.len()) as i64;
        staged.remove(&entry.name);
        checked.up_to_date.insert(entry.name.clone());
    }
    freed
}

/// Which built files may take their place in the install folder ahead of the others.
struct Order<'a> {
    plan: &'a Plan,
    /// The name of each entry in the form names are compared in.
    keys: Vec<String>,
    /// For each entry, the files it copies from, in the same form.
    sources: Vec<Vec<String>>,
    /// For each entry, whether an earlier entry deletes its name or something above it. A
    /// file like that is right only if it is placed after the delete, so it waits for the
    /// apply that goes through the entries in order.
    ///
    /// This is wider than what the check of the originals looks at. A delete that names a
    /// file counts for everything below that name too: if a file stands there, it has to go
    /// before a folder of that name can be made.
    waits: Vec<bool>,
}

impl<'a> Order<'a> {
    fn new(plan: &'a Plan) -> Order<'a> {
        let keys: Vec<String> = plan.entries.iter().map(|entry| plan::name_key(&entry.name)).collect();
        let sources = plan
            .entries
            .iter()
            .map(|entry| match &entry.action {
                Action::Rebuild { sources, .. } => sources.iter().map(|source| plan::name_key(source)).collect(),
                _ => Vec::new(),
            })
            .collect();
        let waits = waits_for_delete(plan, &keys);
        Order { plan, keys, sources, waits }
    }

    /// Entries, by their place in the plan, whose built file may be moved into place now:
    /// it is among the staged files, it does not wait for a delete, and no entry that is
    /// still to be built copies from the file it replaces.
    fn ready(&self, checked: &Checked, staged: &BTreeSet<String>) -> Vec<usize> {
        let entries = &self.plan.entries;
        let mut needed: HashSet<&str> = HashSet::new();
        for (index, entry) in entries.iter().enumerate() {
            if checked.builds(entry) && !staged.contains(&entry.name) {
                needed.extend(self.sources[index].iter().map(String::as_str));
            }
        }
        (0..entries.len())
            .filter(|&index| {
                let entry = &entries[index];
                checked.builds(entry)
                    && staged.contains(&entry.name)
                    && !self.waits[index]
                    && !needed.contains(self.keys[index].as_str())
            })
            .collect()
    }
}

/// See [`Order::waits`]. `keys` are the names of the entries in the form names are compared
/// in.
fn waits_for_delete(plan: &Plan, keys: &[String]) -> Vec<bool> {
    let mut deleted: HashSet<&str> = HashSet::new();
    let mut waits = Vec::with_capacity(keys.len());
    for (entry, key) in plan.entries.iter().zip(keys) {
        let itself = std::iter::once(key.as_str());
        waits.push(itself.chain(plan::folders_above(key)).any(|name| deleted.contains(name)));
        if entry.action == Action::Delete {
            deleted.insert(key);
        }
    }
    waits
}

/// Builds what is still to be built, a round at a time, and after each round moves into
/// place every new file that may go there already. Announces the `Rebuild` step and reports
/// its progress. Where there is more than one round, a trial of every file comes first, as
/// a step of its own.
///
/// `checked` is what the check of the originals found. `built` is the answer of
/// [`built_before`], where the run has asked for it already. Returns `checked` with every
/// file that is now in place counted as up to date, and how many files were placed. What is
/// then still among the staged files, and the deletes and the new folders, are left for
/// [`apply::apply`].
///
/// Public for tests, which run this part alone and look at the folder it leaves.
#[doc(hidden)]
pub fn build_and_place(
    job: &Job,
    checked: Checked,
    built: Option<BTreeSet<String>>,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<(Checked, usize)> {
    let Job { patch, plan, dirs, profile, stop, .. } = *job;
    let built = match built {
        Some(built) => built,
        None => built_before(profile, patch, plan, dirs, stop)?,
    };
    let staged: BTreeSet<String> = plan
        .entries
        .iter()
        .filter(|entry| checked.builds(entry) && built.contains(&entry.name))
        .map(|entry| entry.name.clone())
        .collect();
    let mut rounds =
        Rounds { job: *job, order: Order::new(plan), checked, staged, placed: 0, done: 0, total: plan.new_bytes() };
    rounds.remove_stale();
    rounds.refuse_what_cannot_be_placed()?;
    rounds.require_space(on_event)?;
    // Nothing is placed before every new file has been built once. One round of all of
    // them does that by itself; rounds of some of them need the trial before them.
    let everything = rounds.round_of_everything();
    if everything.is_none() {
        rounds.try_everything(on_event)?;
    }

    on_event(Event::Stage(Stage::Rebuild));
    rounds.report_what_is_done(on_event);
    if let Some(everything) = &everything {
        rounds.build(everything, on_event)?;
    }
    // Files an earlier run built may be free to go already, and each gives room back.
    rounds.place_ready(on_event)?;
    while let Some(round) = rounds.next_round()? {
        rounds.build(&round, on_event)?;
        rounds.place_ready(on_event)?;
    }
    Ok((rounds.checked, rounds.placed))
}

/// A run in the immediate way as it goes.
struct Rounds<'a> {
    job: Job<'a>,
    order: Order<'a>,
    /// What the check of the originals found, with every file counted as up to date from
    /// the moment it is placed.
    checked: Checked,
    /// Names of the entries whose new file is built and waits among the staged files.
    staged: BTreeSet<String>,
    /// How many files this run has moved into place.
    placed: usize,
    /// Bytes of new files done so far, out of `total`.
    done: u64,
    total: u64,
}

impl Rounds<'_> {
    /// Whether the entry's new file has yet to be built.
    fn to_build(&self, entry: &Entry) -> bool {
        self.checked.builds(entry) && !self.staged.contains(&entry.name)
    }

    /// Removes staged files that did not check out. Building their entries would write over
    /// them in the end; until then they take room that the rounds count on.
    fn remove_stale(&self) {
        let staged_dir = self.job.dirs.staged();
        for entry in self.job.plan.entries.iter().filter(|entry| self.to_build(entry)) {
            if let Ok(relative) = plan::relative_path(&entry.name) {
                let _ = fs::remove_file(staged_dir.join(relative));
            }
        }
    }

    /// Refuses, before anything is replaced, a patch with a file that could not be put in
    /// its place, or a folder that could not be made: a folder stands where the file goes,
    /// or a file stands where a folder has to be. The apply that can be undone finds such a
    /// thing as it goes. Here that would be after other files were replaced for good.
    ///
    /// What stands in the way does not count if an entry before it deletes it. A delete only
    /// removes the kind of thing it names, a file or a folder, so one that names the other
    /// kind clears nothing, though the entries behind it wait for it all the same.
    fn refuse_what_cannot_be_placed(&self) -> Result<()> {
        let in_the_way = |path: &Path, what: &'static str| {
            Error::file("replace", path, io::Error::new(io::ErrorKind::AlreadyExists, what))
        };
        // What the deletes so far take away, as name keys: files, and folders with
        // everything in them.
        let (mut files_gone, mut folders_gone) = (HashSet::new(), HashSet::new());
        for (entry, key) in self.job.plan.entries.iter().zip(&self.order.keys) {
            let target = self.job.dirs.install.join(plan::relative_path(&entry.name)?);
            if entry.action == Action::Delete {
                if stage::deletes(&entry.name, &target) {
                    let gone = if plan::names_folder(&entry.name) { &mut folders_gone } else { &mut files_gone };
                    gone.insert(key.as_str());
                }
                continue;
            }
            let folder = entry.action == Action::CreateDir;
            if !(folder || self.checked.builds(entry)) {
                continue;
            }
            let with_folders_above = |name| std::iter::once(name).chain(plan::folders_above(name));
            let goes_with_a_folder = |name| with_folders_above(name).any(|above| folders_gone.contains(above));
            // Each place of the name with what it is called, nearest first.
            let mut places = target.ancestors().zip(with_folders_above(key));
            if !folder {
                let (path, name) = places.next().expect("a name has at least one part");
                if path.is_dir() && !goes_with_a_folder(name) {
                    return Err(in_the_way(path, "a folder has this name"));
                }
            }
            // The folders that have to be there: those above, and the entry's own if it
            // makes one. The nearest thing that is there has to be a folder.
            if let Some((path, name)) = places.find(|(path, _)| path.exists())
                && !path.is_dir()
                && !files_gone.contains(name)
                && !goes_with_a_folder(name)
            {
                return Err(in_the_way(path, "a file has the name of this folder"));
            }
        }
        Ok(())
    }

    /// Stops the run unless the drive has the room this way of patching needs, and reports
    /// the figures either way. See [`space_needed`] and [`SLACK`].
    fn require_space(&self, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<()> {
        let Job { plan, dirs, free, .. } = self.job;
        if !plan.entries.iter().any(|entry| self.to_build(entry)) {
            return Ok(());
        }
        // An install folder that is half patched already cannot turn back, and going on
        // slowly is better for it than not going on. It is asked for the least only.
        let slack = if stage::half_patched(dirs).is_some() { 0 } else { self.job.slack };
        let bytes = room_to_start(plan, &self.checked, &self.staged, &dirs.install, slack);
        let work = dirs.work();
        if let Ok(available) = free(&work) {
            on_event(Event::Space { needed: bytes.saturating_add(SPACE_MARGIN), available });
        }
        blocked_unless_empty(preflight::check_space_with(&[SpaceNeed { dir: work, bytes }], SPACE_MARGIN, free))
    }

    /// Every file that is still to be built, as one round, where the drive has room for all
    /// of them as it is now and the install folder is still whole. `None` where it has not,
    /// where the folder is half patched, and where nothing is to be built.
    ///
    /// A run that builds this round before it places anything needs no trial: the round
    /// finds what a trial would find, and finds it while the install folder is still the old
    /// version. That holds for a run that goes on from an earlier one as well. The files
    /// that one left built were checked against their checksums, and they wait for this
    /// round like the rest.
    ///
    /// The drive is asked once, here, and the round is not cut down if the drive has less
    /// by the time it is built. A build that runs out of room fails with nothing placed. A
    /// smaller round would be placed before the files after it had ever been built.
    fn round_of_everything(&self) -> Option<BTreeSet<String>> {
        let Job { plan, dirs, free, .. } = self.job;
        if stage::half_patched(dirs).is_some() {
            return None;
        }
        let left: Vec<&Entry> = plan.entries.iter().filter(|entry| self.to_build(entry)).collect();
        let bytes = left.iter().fold(0u64, |sum, entry| sum.saturating_add(entry.new_len()));
        let room = free(&dirs.work()).ok()?.saturating_sub(SPACE_MARGIN);
        (!left.is_empty() && bytes <= room).then(|| left.iter().map(|entry| entry.name.clone()).collect())
    }

    /// Builds every file that is still to be built once, to nowhere, so that a patch that
    /// cannot be built is found while the install folder is still whole: an original that
    /// cannot be read or has got shorter since it was checked, a new file that comes out
    /// with another checksum than promised. The safe way finds these while it builds, before
    /// it touches anything, and so does a run that has [`Self::round_of_everything`]. In
    /// rounds of some files at a time, the first files are in place by then, for good.
    /// Announces the `Trial` step and reports its progress, which costs one more reading of
    /// the originals and no room.
    ///
    /// An install folder that is half patched already is not tried. There is no way back
    /// from there whatever a trial says, and files of it that are built can no longer be
    /// tried: what they were built from may be gone.
    fn try_everything(&self, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<()> {
        let Job { patch, plan, dirs, profile, stop, .. } = self.job;
        if stage::half_patched(dirs).is_some() {
            return Ok(());
        }
        let (tried, rest): (Vec<&Entry>, Vec<&Entry>) = plan.entries.iter().partition(|entry| self.to_build(entry));
        let total: u64 = tried.iter().map(|entry| entry.new_len()).sum();
        if total == 0 {
            return Ok(());
        }
        let tried: BTreeSet<&str> = tried.iter().map(|entry| entry.name.as_str()).collect();
        let done_already = Checked { up_to_date: rest.iter().map(|entry| entry.name.clone()).collect() };

        on_event(Event::Stage(Stage::Trial));
        let mut done = 0u64;
        let mut current = String::new();
        stage::try_build(profile, patch, plan, &done_already, dirs, stop, &mut |name, bytes| {
            // Every entry that is passed over is reported as done as well.
            if !tried.contains(name) {
                return;
            }
            if current != name {
                current = name.to_owned();
                on_event(Event::File(current.clone()));
            }
            done += bytes;
            on_event(Event::Progress { done, total });
        })
    }

    /// Reports what is done before anything is built: the new files that are in place
    /// already, as one figure, and each file an earlier run built and left staged, by name.
    fn report_what_is_done(&mut self, on_event: &mut (dyn FnMut(Event) + Send)) {
        let entries = &self.job.plan.entries;
        self.done = entries.iter().filter(|entry| !self.checked.builds(entry)).map(Entry::new_len).sum();
        on_event(Event::Progress { done: self.done, total: self.total });
        let kept: Vec<&Entry> = entries.iter().filter(|entry| self.staged.contains(&entry.name)).collect();
        for entry in kept {
            on_event(Event::Kept { name: entry.name.clone(), bytes: entry.new_len() });
            self.done += entry.new_len();
            on_event(Event::Progress { done: self.done, total: self.total });
        }
    }

    /// The names of the entries to build next: as many of those still to be built, in the
    /// order of the patch file, as fit in what is free on the drive with the margin left
    /// over. `None` when everything is built.
    ///
    /// What is free is asked of the drive for every round. It changes with every file that
    /// is placed, by the difference between the new file and its original, and a figure
    /// worked out once at the start would drift away from the drive.
    fn next_round(&self) -> Result<Option<BTreeSet<String>>> {
        let Job { plan, dirs, free, stop, .. } = self.job;
        let left: Vec<&Entry> = plan.entries.iter().filter(|entry| self.to_build(entry)).collect();
        let Some(first) = left.first().map(|entry| entry.new_len()) else { return Ok(None) };
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        // Without room for even the next file there is no going on.
        let work = dirs.work();
        blocked_unless_empty(preflight::check_space_with(
            &[SpaceNeed { dir: work.clone(), bytes: first }],
            SPACE_MARGIN,
            free,
        ))?;
        let room = free(&work).map_or(first, |available| available.saturating_sub(SPACE_MARGIN)).max(first);

        let mut round = BTreeSet::new();
        let mut sum = 0u64;
        for entry in left {
            sum = sum.saturating_add(entry.new_len());
            if sum > room {
                break;
            }
            round.insert(entry.name.clone());
        }
        Ok(Some(round))
    }

    /// Builds the files of one round into the work folder. The install folder is only read.
    ///
    /// Staging is told that everything outside the round is up to date, so it passes over
    /// those entries, comes back when the round is built, and a pause is the only thing
    /// that ends it early.
    fn build(&mut self, round: &BTreeSet<String>, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<()> {
        let Job { patch, plan, dirs, profile, stop, .. } = self.job;
        let outside: BTreeSet<String> =
            plan.entries.iter().filter(|entry| !round.contains(&entry.name)).map(|entry| entry.name.clone()).collect();
        let (total, mut done) = (self.total, self.done);
        let mut current = String::new();
        let built = stage::stage_with(
            profile,
            patch,
            plan,
            &Checked { up_to_date: outside },
            dirs,
            stop,
            &mut |name, bytes, _kept| {
                // Staging reports every entry it passes over as done, each time it is
                // called. Those were counted when they were done.
                if !round.contains(name) {
                    return;
                }
                if current != name {
                    current = name.to_owned();
                    on_event(Event::File(current.clone()));
                }
                done += bytes;
                on_event(Event::Progress { done, total });
            },
        );
        self.done = done;
        built?;
        self.staged.extend(round.iter().cloned());
        Ok(())
    }

    /// Moves into place every new file that may go there now. No thread of the build is
    /// alive by then, and no original is open.
    ///
    /// The checks are the ones made before an apply, made again here because each pass
    /// changes the install folder for good. A pause is looked for before every file, and
    /// stops the pass between two files: each file is in place or it is not.
    fn place_ready(&mut self, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<()> {
        let Job { programs, patch, plan, dirs, stop, .. } = self.job;
        let ready = self.order.ready(&self.checked, &self.staged);
        if ready.is_empty() {
            return Ok(());
        }
        let mut moves = Vec::with_capacity(ready.len());
        for index in ready {
            let entry = &plan.entries[index];
            let relative = plan::relative_path(&entry.name)?;
            moves.push((entry, dirs.staged().join(&relative), dirs.install.join(&relative)));
        }

        apply::guard_work(dirs)?;
        let mut problems = preflight::check_inside_install(&dirs.install, &apply::target_dirs(plan, dirs));
        problems.extend(programs.check_running(&dirs.install));
        let replaced: Vec<PathBuf> =
            moves.iter().map(|(_, _, target)| target.clone()).filter(|target| target.is_file()).collect();
        problems.extend(preflight::check_locks(&replaced));
        blocked_unless_empty(problems)?;
        // From the first file placed the install folder is neither the old version nor the
        // new one. That is on disk before the folder is.
        stage::pin(patch, dirs)?;

        for (entry, built, target) in moves {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Stopped);
            }
            on_event(Event::File(entry.name.clone()));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::file("create", parent, e))?;
            }
            apply::clear_read_only(&target);
            // One step takes the original away and puts the new file there: no moment has
            // neither, and none has half of one.
            when_free(|| fs::rename(&built, &target)).map_err(|e| Error::file("place", &target, e))?;
            self.staged.remove(&entry.name);
            self.checked.up_to_date.insert(entry.name.clone());
            self.placed += 1;
            // It is in place for good: nothing undoes a file that went in ahead of the others.
            on_event(Event::Placed { name: entry.name.clone() });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::plan::Layout;

    fn rebuild(name: &str, new_len: u64, sources: &[&str]) -> Entry {
        let sources = sources.iter().map(|source| (*source).to_owned()).collect();
        Entry { name: name.to_owned(), action: Action::Rebuild { old_crc: None, new_crc: 0, new_len, sources } }
    }

    fn create(name: &str, len: u64) -> Entry {
        Entry { name: name.to_owned(), action: Action::Create { len, crc: 0 } }
    }

    fn delete(name: &str) -> Entry {
        Entry { name: name.to_owned(), action: Action::Delete }
    }

    fn plan_of(entries: Vec<Entry>) -> Plan {
        Plan { layout: Layout::Table, originals: BTreeMap::new(), entries, contents_len: 0, reach: BTreeMap::new() }
    }

    /// An install folder that holds files of the given sizes.
    fn install(files: &[(&str, usize)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, len) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![0u8; *len]).unwrap();
        }
        dir
    }

    /// The least room for `entries` in `dir` when nothing is up to date and nothing built.
    fn least(entries: Vec<Entry>, dir: &tempfile::TempDir) -> u64 {
        space_needed(&plan_of(entries), &Checked::default(), &BTreeSet::new(), dir.path())
    }

    const A: &str = r"Data\A.wz";
    const B: &str = r"Data\B.wz";

    #[test]
    fn room_needed_is_the_highest_point_of_written_less_replaced() {
        let dir = install(&[("Data/A.wz", 40), ("Data/B.wz", 80)]);
        // A grows from 40 to 100 and B shrinks from 80 to 30, each built from itself.
        // A first: 100 written, 40 given back, then 30 more written: never above 100.
        assert_eq!(least(vec![rebuild(A, 100, &[A]), rebuild(B, 30, &[B])], &dir), 100);
        // B first: 30 written, 80 given back, and A's 100 only bring it to 50.
        assert_eq!(least(vec![rebuild(B, 30, &[B]), rebuild(A, 100, &[A])], &dir), 50);
        // A new file where there was none gives nothing back.
        assert_eq!(least(vec![create(r"Data\New.wz", 70), rebuild(B, 30, &[B])], &dir), 100);
        // A file that makes no new file needs no room, and neither does no file at all.
        assert_eq!(least(vec![delete(A), create(r"Data\Empty.wz", 0), create("Folder", 0)], &dir), 0);
        assert_eq!(least(Vec::new(), &dir), 0);
    }

    #[test]
    fn file_that_others_are_still_built_from_keeps_its_room_until_they_are_built() {
        let dir = install(&[("Data/A.wz", 50), ("Data/F.wz", 20), ("Data/U.wz", 10)]);
        let (f, u) = (r"Data\F.wz", r"Data\U.wz");
        // F is replaced whole, and U is built from the old F. The new F has to wait for U:
        // 50 for A and back to 0, then 20 and 60 together.
        let waiting = vec![rebuild(A, 50, &[A]), create(f, 20), rebuild(u, 60, &[f, u])];
        assert_eq!(least(waiting, &dir), 80);
        // The same when the patch spells the name it copies from another way.
        let other_spelling = vec![rebuild(A, 50, &[A]), create(f, 20), rebuild(u, 60, &["data/f.WZ", u])];
        assert_eq!(least(other_spelling, &dir), 80);
        // With U built from itself alone, F goes into place at once: 20 and back to 0,
        // then 60.
        let free_to_go = vec![rebuild(A, 50, &[A]), create(f, 20), rebuild(u, 60, &[u])];
        assert_eq!(least(free_to_go, &dir), 60);
        // A chain: F is built from A's original and U from F's, so each waits for the next.
        // A: 50, and it waits. F: 70, and A goes, which leaves 20. U: 80, and both go.
        let chain = vec![rebuild(A, 50, &[A]), rebuild(f, 20, &[A]), rebuild(u, 60, &[f])];
        assert_eq!(least(chain, &dir), 80);
    }

    #[test]
    fn file_under_a_name_that_is_deleted_first_waits_for_the_apply() {
        let dir = install(&[("Data/Old/h.wz", 30), ("Data/A.wz", 40)]);
        let h = r"Data\Old\h.wz";
        // Without the delete each file is placed as it is built: 30 and back, 40 and back.
        assert_eq!(least(vec![create(h, 30), rebuild(A, 40, &[A])], &dir), 40);
        // After a delete of its folder, or of a file with the folder's name, or of the
        // file itself, the new file stays staged to the end: 30, and 40 on top of it.
        for gone in [r"Data\Old\", r"Data\Old", "data/old/", r"Data\Old\H.wz"] {
            assert_eq!(least(vec![delete(gone), create(h, 30), rebuild(A, 40, &[A])], &dir), 70, "{gone}");
        }
        // A delete that comes later, or names something else, holds nothing back.
        assert_eq!(least(vec![create(h, 30), delete(r"Data\Old\"), rebuild(A, 40, &[A])], &dir), 40);
        assert_eq!(least(vec![delete(r"Data\Ol"), create(h, 30), rebuild(A, 40, &[A])], &dir), 40);
    }

    #[test]
    fn files_that_are_built_or_in_place_already_cost_no_more() {
        let dir = install(&[("Data/A.wz", 40), ("Data/B.wz", 80)]);
        let entries = plan_of(vec![rebuild(A, 100, &[A]), rebuild(B, 30, &[B])]);
        let (nothing, a_built) = (BTreeSet::new(), BTreeSet::from([A.to_owned()]));
        let a_in_place = Checked { up_to_date: a_built.clone() };

        assert_eq!(space_needed(&entries, &Checked::default(), &nothing, dir.path()), 100);
        // A is staged: placing it gives 40 back before B's 30 are written.
        assert_eq!(space_needed(&entries, &Checked::default(), &a_built, dir.path()), 0);
        // A is in place: only B is left.
        assert_eq!(space_needed(&entries, &a_in_place, &nothing, dir.path()), 30);
        // A staged file of an entry that is in place is nobody's business.
        assert_eq!(space_needed(&entries, &a_in_place, &a_built, dir.path()), 30);

        // A staged file that has to wait for another gives nothing back until then.
        let entries = plan_of(vec![rebuild(A, 100, &[A]), rebuild(B, 30, &[A, B])]);
        assert_eq!(space_needed(&entries, &Checked::default(), &a_built, dir.path()), 30);
    }
}
