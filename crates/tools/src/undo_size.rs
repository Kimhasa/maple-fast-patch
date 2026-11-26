//! What a way back to the old version would have to keep, and what room the immediate way
//! needs, worked out from a patch file. Nothing is written and no game file is read: of an
//! install folder only the lengths of its files are asked for.
//!
//! A way back that does not keep the old files whole keeps, of every file a patch replaces
//! or removes, the bytes that no new file holds. A copy instruction says where in a new file
//! a stretch of an old one lies, so that stretch can be fetched from there again. What no
//! copy reads has to be kept. A patch file does not say how long the old files were, only
//! how far its copies reach into them. From the patch alone the figure is therefore a lower
//! bound, and of a file that is replaced whole or deleted and that no copy reads, nothing is
//! known at all.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use mfp_engine::container::PatchFile;
use mfp_engine::error::{Error, Result};
use mfp_engine::plan::{self, Action, Entry, Header, Op, Plan};

use crate::reconstruct::Ranges;

/// What a patch does to a file that was there before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Built anew out of old files and patch data.
    Rebuilt,
    /// Replaced by a file that is in the patch whole.
    ReplacedWhole,
    /// Removed, by its own name or with the folder it is in.
    Deleted,
    /// Only read.
    Kept,
}

/// One file of the old version, as far as a patch file tells of it. For a delete that names
/// a folder, everything in that folder that the patch does not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Old {
    pub name: String,
    pub fate: Fate,
    /// The end of the furthest byte a copy reads. The file was at least this long.
    pub extent: u64,
    /// Bytes of it that at least one copy reads, and that lie in a new file because of it.
    pub covered: u64,
    /// How long it is in the old install, where one was given and holds it.
    pub len: Option<u64>,
}

impl Old {
    /// Whether the patch takes this file away, so that a way back has to bring it back.
    pub fn changed(&self) -> bool {
        self.fate != Fate::Kept
    }

    /// The least a way back has to keep of it, as far as the patch file alone tells.
    pub fn least_kept(&self) -> u64 {
        if self.changed() { self.extent - self.covered } else { 0 }
    }

    /// What a way back has to keep of it, when its length is known.
    pub fn kept(&self) -> Option<u64> {
        if self.changed() { self.len.map(|len| len.saturating_sub(self.covered)) } else { Some(0) }
    }
}

/// How many entries and instructions of each kind a patch file holds, and the bytes they
/// stand for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub folders: usize,
    pub deletes: usize,
    /// Files that are in the patch whole.
    pub whole: usize,
    pub whole_bytes: u64,
    /// Entries for a file of no length. A run builds nothing for them.
    pub empty: usize,
    pub rebuilds: usize,
    pub rebuilt_bytes: u64,
    pub copies: u64,
    pub copy_bytes: u64,
    pub literals: u64,
    pub literal_bytes: u64,
    pub fills: u64,
    pub fill_bytes: u64,
}

/// What [`measure`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measured {
    pub counts: Counts,
    /// Every file the patch replaces, removes or reads, in the order the patch names them:
    /// first those it changes, then those it only reads.
    pub olds: Vec<Old>,
}

impl Measured {
    /// The files the patch replaces or removes.
    pub fn changed(&self) -> impl Iterator<Item = &Old> {
        self.olds.iter().filter(|old| old.changed())
    }

    /// The least a way back has to keep in all, as far as the patch file alone tells.
    pub fn least_kept(&self) -> u64 {
        self.changed().map(Old::least_kept).sum()
    }

    /// What a way back has to keep in all, counting the files whose length is known. Files
    /// the old install does not hold are new with this patch, and nothing is kept for them.
    pub fn kept(&self) -> u64 {
        self.changed().filter_map(Old::kept).sum()
    }

    /// Files that are shorter in the install than the patch reads them: with any of these,
    /// the install is not the version the patch was made for.
    pub fn too_short(&self) -> usize {
        self.olds.iter().filter(|old| old.len.is_some_and(|len| len < old.extent)).count()
    }
}

/// What the copies of a patch read of one file.
struct Reads {
    name: String,
    ranges: Ranges,
    extent: u64,
}

/// Goes through a patch file once and says what it does to the old files. With
/// `old_install`, a folder that is still at the version the patch was made for, the lengths
/// of the files there are added.
pub fn measure(patch: &PatchFile, plan: &Plan, old_install: Option<&Path>, stop: &AtomicBool) -> Result<Measured> {
    let mut counts = Counts::default();
    let mut reads: HashMap<String, Reads> = HashMap::new();
    let (mut stream, layout, _) = plan::open_contents(patch)?;
    for entry in &plan.entries {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let (name, header) = plan::next_entry(&mut stream, layout, &entry.name)?;
        match header {
            Header::CreateDir => counts.folders += 1,
            Header::Delete => counts.deletes += 1,
            Header::Create { len: 0, .. } => counts.empty += 1,
            Header::Create { len, .. } => {
                stream.skip(len)?;
                counts.whole += 1;
                counts.whole_bytes += len;
            }
            Header::Rebuild { .. } => {
                counts.rebuilds += 1;
                counts.rebuilt_bytes += entry.new_len();
                plan::read_ops(&mut stream, layout, &name, |op, stream| {
                    match op {
                        Op::Literal { len } => {
                            stream.skip(len)?;
                            counts.literals += 1;
                            counts.literal_bytes += len;
                        }
                        Op::Fill { len, .. } => {
                            counts.fills += 1;
                            counts.fill_bytes += len;
                        }
                        Op::Copy { from, start, len } => {
                            counts.copies += 1;
                            counts.copy_bytes += len;
                            let read = reads.entry(plan::name_key(&from)).or_insert_with(|| Reads {
                                name: from,
                                ranges: Ranges::default(),
                                extent: 0,
                            });
                            read.ranges.add(start, start + len);
                            read.extent = read.extent.max(start + len);
                        }
                    }
                    Ok(())
                })?;
            }
        }
    }
    Ok(Measured { counts, olds: olds(plan, reads, old_install) })
}

/// Puts together what the entries of a patch do to each old file with what its copies read
/// of it.
fn olds(plan: &Plan, mut reads: HashMap<String, Reads>, old_install: Option<&Path>) -> Vec<Old> {
    let mut olds: Vec<Old> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut deleted_folders = Vec::new();
    for entry in &plan.entries {
        let fate = match entry.action {
            Action::Rebuild { .. } => Fate::Rebuilt,
            Action::Create { len, .. } if len > 0 => Fate::ReplacedWhole,
            Action::Delete => Fate::Deleted,
            Action::Create { .. } | Action::CreateDir => continue,
        };
        let key = plan::name_key(&entry.name);
        // The first entry that names a file says what becomes of the old one.
        if !seen.insert(key.clone()) {
            continue;
        }
        if fate == Fate::Deleted && plan::names_folder(&entry.name) {
            deleted_folders.push(olds.len());
        }
        let read = reads.remove(&key);
        olds.push(Old {
            name: entry.name.clone(),
            fate,
            extent: read.as_ref().map_or(0, |read| read.extent),
            covered: read.as_ref().map_or(0, |read| read.ranges.total()),
            len: None,
        });
    }
    // What is left is read and not named by an entry. A file like that stays, unless the
    // folder it is in is deleted.
    let mut only_read: Vec<Reads> = reads.into_values().collect();
    only_read.sort_by_key(|read| plan::name_key(&read.name));
    for read in only_read {
        let key = plan::name_key(&read.name);
        let gone = deleted_folders.iter().any(|&folder| below(&key, &plan::name_key(&olds[folder].name)));
        seen.insert(key);
        olds.push(Old {
            name: read.name,
            fate: if gone { Fate::Deleted } else { Fate::Kept },
            extent: read.extent,
            covered: read.ranges.total(),
            len: None,
        });
    }

    if let Some(install) = old_install {
        for (index, old) in olds.iter_mut().enumerate() {
            let Ok(relative) = plan::relative_path(&old.name) else { continue };
            let path = install.join(relative);
            old.len = if deleted_folders.contains(&index) {
                // Files in it that the patch reads have a line of their own.
                path.is_dir().then(|| tree_size(&path, install, &seen))
            } else {
                fs::metadata(&path).ok().filter(|meta| meta.is_file()).map(|meta| meta.len())
            };
        }
    }
    olds
}

/// Whether the name with the key `key` lies in the folder with the key `folder`.
fn below(key: &str, folder: &str) -> bool {
    key.strip_prefix(folder).is_some_and(|rest| rest.starts_with('\\'))
}

/// Bytes of all files under `dir`, a folder of `install`, except those named in `listed`.
fn tree_size(dir: &Path, install: &Path, listed: &BTreeSet<String>) -> u64 {
    let mut total = 0;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        let Ok(children) = fs::read_dir(&next) else { continue };
        for child in children.flatten() {
            let path = child.path();
            let Ok(meta) = fs::metadata(&path) else { continue };
            if meta.is_dir() {
                todo.push(path);
            } else if let Ok(relative) = path.strip_prefix(install)
                && !listed.contains(&plan::name_key(&relative.to_string_lossy()))
            {
                total += meta.len();
            }
        }
    }
    total
}

/// When a new file may take the place of its original ahead of the others, in the
/// immediate way of patching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// As soon as every other file that copies from the original is built.
    ConsumersBuilt,
    /// Only when every other file that copies from the original is in place itself. A file
    /// that is built and not in place can then always be built again, because nothing it
    /// was built from has been replaced. Files that copy from one another in a circle never
    /// get there, and wait for the apply at the end.
    ConsumersInPlace,
    /// As [`Rule::ConsumersInPlace`], but files that wait only for one another go into
    /// place together, one right after the other.
    ConsumersGoAlong,
}

/// The order in which the immediate way builds the new files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequence {
    /// The order of the patch file.
    AsInPatch,
    /// The first file, in the order of the patch file, that nothing would hold back once it
    /// is built; when there is none, the first that is not built yet.
    ConsumersFirst,
}

/// The room the immediate way needs under a rule and an order of building.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Floor {
    /// The most the install folder holds beyond what it held at the start, when one file is
    /// built at a time and everything that may be placed is placed after each.
    pub peak: u64,
    /// New files that are still waiting when everything is built, and their bytes. They go
    /// in at the end, through the apply that keeps a journal.
    pub left: usize,
    pub left_bytes: u64,
    /// The most files that went into place in one go because they waited for one another,
    /// under [`Rule::ConsumersGoAlong`].
    pub largest_group: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Unbuilt,
    Built,
    Placed,
}

/// The entries of a plan as the immediate way sees them while it works.
struct Sim<'a> {
    entries: &'a [Entry],
    state: Vec<State>,
    /// For each entry, whether it waits for the apply at the end whatever else happens.
    waits: Vec<bool>,
    /// For each entry, the other entries that copy from the file it replaces.
    consumers: Vec<Vec<usize>>,
}

impl Sim<'_> {
    fn unbuilt(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.entries.len()).filter(|&index| self.state[index] == State::Unbuilt)
    }

    /// Whether every consumer of an entry is at least as far as `rule` asks for.
    fn free(&self, index: usize, rule: Rule) -> bool {
        self.consumers[index].iter().all(|&consumer| match rule {
            Rule::ConsumersBuilt => self.state[consumer] != State::Unbuilt,
            Rule::ConsumersInPlace | Rule::ConsumersGoAlong => self.state[consumer] == State::Placed,
        })
    }

    /// The built files that may go into place now.
    fn ready(&self, rule: Rule) -> Vec<usize> {
        let built = |index: &usize| self.state[*index] == State::Built && !self.waits[*index];
        let mut ready: Vec<usize> = (0..self.entries.len()).filter(built).collect();
        if rule != Rule::ConsumersGoAlong {
            ready.retain(|&index| self.free(index, rule));
            return ready;
        }
        // The largest set in which every file has all its consumers in place or in the set.
        loop {
            let before = ready.len();
            let going: BTreeSet<usize> = ready.iter().copied().collect();
            ready.retain(|&index| {
                self.consumers[index]
                    .iter()
                    .all(|consumer| self.state[*consumer] == State::Placed || going.contains(consumer))
            });
            if ready.len() == before {
                return ready;
            }
        }
    }

    /// The file to build next.
    fn next(&self, rule: Rule, sequence: Sequence) -> Option<usize> {
        let first = self.unbuilt().next();
        if sequence == Sequence::AsInPatch {
            return first;
        }
        let no_wait = |index: &usize| !self.waits[*index];
        let free = self.unbuilt().filter(no_wait).find(|&index| self.free(index, rule));
        // A file whose consumers are all built may be the last one of a circle.
        let closing = || self.unbuilt().filter(no_wait).find(|&index| self.free(index, Rule::ConsumersBuilt));
        match rule {
            Rule::ConsumersGoAlong => free.or_else(closing).or(first),
            Rule::ConsumersBuilt | Rule::ConsumersInPlace => free.or(first),
        }
    }
}

/// Works out [`Floor`] for a folder in which nothing is built or in place yet. `old_len`
/// says how many bytes the file of that name gives back when a new one takes its place.
pub fn floor(plan: &Plan, old_len: &dyn Fn(&str) -> u64, rule: Rule, sequence: Sequence) -> Floor {
    let entries = &plan.entries[..];
    let builds = |entry: &Entry| match entry.action {
        Action::Create { len, .. } => len > 0,
        Action::Rebuild { .. } => true,
        Action::CreateDir | Action::Delete => false,
    };
    let keys: Vec<String> = entries.iter().map(|entry| plan::name_key(&entry.name)).collect();
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if builds(entry) {
            by_key.entry(keys[index].as_str()).or_default().push(index);
        }
    }
    let mut consumers = vec![Vec::new(); entries.len()];
    for (index, entry) in entries.iter().enumerate() {
        let Action::Rebuild { sources, .. } = &entry.action else { continue };
        for source in sources {
            for &fed in by_key.get(plan::name_key(source).as_str()).into_iter().flatten() {
                if fed != index && !consumers[fed].contains(&index) {
                    consumers[fed].push(index);
                }
            }
        }
    }
    let state = entries.iter().map(|entry| if builds(entry) { State::Unbuilt } else { State::Placed }).collect();
    let mut sim = Sim { entries, state, waits: waits_for_delete(plan, &keys), consumers };

    let (mut used, mut peak, mut largest_group) = (0i128, 0i128, 0);
    while let Some(next) = sim.next(rule, sequence) {
        sim.state[next] = State::Built;
        used += i128::from(entries[next].new_len());
        peak = peak.max(used);
        // Placing one file may be what another was waiting for.
        loop {
            let ready = sim.ready(rule);
            if ready.is_empty() {
                break;
            }
            largest_group = largest_group.max(ready.len());
            for index in ready {
                sim.state[index] = State::Placed;
                used -= i128::from(old_len(&entries[index].name));
            }
        }
    }
    let left = || (0..entries.len()).filter(|&index| sim.state[index] == State::Built);
    Floor {
        peak: peak.max(0) as u64,
        left: left().count(),
        left_bytes: left().map(|index| entries[index].new_len()).sum(),
        largest_group,
    }
}

/// For each entry, whether an earlier entry deletes its name or something above it. Such a
/// file is right only if it goes in after the delete, so it waits for the apply at the end.
fn waits_for_delete(plan: &Plan, keys: &[String]) -> Vec<bool> {
    let mut deleted: Vec<&str> = Vec::new();
    let mut waits = Vec::with_capacity(keys.len());
    for (entry, key) in plan.entries.iter().zip(keys) {
        waits.push(deleted.iter().any(|gone| key == gone || below(key, gone)));
        if entry.action == Action::Delete {
            deleted.push(key);
        }
    }
    waits
}
