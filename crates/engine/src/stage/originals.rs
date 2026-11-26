//! What is in the install folder before a run changes it: the fingerprints of its files, and
//! whether the originals are the files the patch was made for.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::BUFFER;
use crate::crc;
use crate::error::{Error, Result};
use crate::plan::{self, Action, Layout, Plan};
use crate::storage::Profile;

/// Checksum and size of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub crc: u32,
    pub len: u64,
}

impl Fingerprint {
    /// What the patch file promises the new file of an entry to be. `None` for an entry that
    /// writes no file.
    pub fn promised(entry: &plan::Entry) -> Option<Fingerprint> {
        match &entry.action {
            Action::Create { len, crc } => Some(Fingerprint { crc: *crc, len: *len }),
            Action::Rebuild { new_crc, new_len, .. } => Some(Fingerprint { crc: *new_crc, len: *new_len }),
            Action::CreateDir | Action::Delete => None,
        }
    }
}

pub fn fingerprint(path: &Path, stop: &AtomicBool, on_bytes: &(dyn Fn(u64) + Sync)) -> Result<Fingerprint> {
    let mut file = File::open(path).map_err(|e| Error::file("open", path, e))?;
    let mut buffer = vec![0u8; BUFFER];
    let mut result = Fingerprint { crc: 0, len: 0 };
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let count = file.read(&mut buffer).map_err(|e| Error::file("read", path, e))?;
        if count == 0 {
            return Ok(result);
        }
        result.crc = crc::update(result.crc, &buffer[..count]);
        result.len += count as u64;
        on_bytes(count as u64);
    }
}

/// What checking the install folder against the plan found.
#[derive(Debug, Default, Clone)]
pub struct Checked {
    /// Entries whose file already is the new version; nothing to build or move for them.
    pub up_to_date: BTreeSet<String>,
}

impl Checked {
    /// Whether this entry gets a new file built and moved into place.
    pub fn builds(&self, entry: &plan::Entry) -> bool {
        match entry.action {
            Action::Create { len, .. } => len > 0 && !self.up_to_date.contains(&entry.name),
            Action::Rebuild { .. } => !self.up_to_date.contains(&entry.name),
            Action::CreateDir | Action::Delete => false,
        }
    }
}

/// Files [`check_originals`] will read, with their sizes, for progress reporting.
pub(crate) fn originals_to_check(plan: &Plan, install: &Path) -> Vec<(String, u64)> {
    let mut names = BTreeSet::new();
    for entry in &plan.entries {
        if matches!(entry.action, Action::Create { len, .. } if len > 0) {
            names.insert(entry.name.clone());
        }
        if let Action::Rebuild { sources, .. } = &entry.action {
            names.insert(entry.name.clone());
            names.extend(sources.iter().cloned());
        }
    }
    files_present(names, install)
}

/// Those of `names` that are files under `root`, with their sizes, in the order given.
pub fn files_present(names: impl IntoIterator<Item = String>, root: &Path) -> Vec<(String, u64)> {
    names
        .into_iter()
        .filter_map(|name| {
            let path = root.join(plan::relative_path(&name).ok()?);
            Some((name, fs::metadata(path).ok().filter(|m| m.is_file())?.len()))
        })
        .collect()
}

/// Confirms that every original file a rebuild copies from is exactly the file the patch was
/// made for. Reports all files that differ, not only the first. Where they are all the right
/// files and a copy reads past the end of one, it is the patch that is refused. `on_progress` hears, from
/// whichever thread is reading, which file is being read and how many more bytes of it were.
pub fn check_originals(
    plan: &Plan,
    install: &Path,
    stop: &AtomicBool,
    on_progress: &(dyn Fn(&str, u64) + Sync),
) -> Result<Checked> {
    check_originals_except(plan, install, &BTreeSet::new(), stop, on_progress)
}

/// [`check_originals`] for an install folder that may be half patched (see [`pin`](super::pin)).
///
/// `built` names the entries whose new file is already built and waits among the staged
/// files, checked against its checksum. What such a file was built from is no longer needed,
/// and may be gone: an original it copied from may itself have been replaced since. So the
/// originals of those entries are not asked for. Without any, this is [`check_originals`].
///
/// A run does this through [`check_originals_with`]. This one is public for tests.
#[doc(hidden)]
pub fn check_originals_except(
    plan: &Plan,
    install: &Path,
    built: &BTreeSet<String>,
    stop: &AtomicBool,
    on_progress: &(dyn Fn(&str, u64) + Sync),
) -> Result<Checked> {
    check_originals_with(&Profile::fast(), plan, install, built, stop, on_progress)
}

/// [`check_originals_except`] on a drive that is read the way `profile` says.
pub fn check_originals_with(
    profile: &Profile,
    plan: &Plan,
    install: &Path,
    built: &BTreeSet<String>,
    stop: &AtomicBool,
    on_progress: &(dyn Fn(&str, u64) + Sync),
) -> Result<Checked> {
    let files = originals_to_check(plan, install);
    let found = fingerprint_all(&files, install, profile.checkers, stop, on_progress, &|_, _| {})?;

    let mut checked = Checked::default();
    let mut differing = BTreeSet::new();
    // The first original that is the right file and is shorter than a copy from it needs.
    let mut lies_about = None;
    // What the entries so far delete, as name keys: everything they name, and of that the
    // folders.
    let (mut deleted, mut deleted_folders) = (HashSet::new(), HashSet::new());
    for entry in &plan.entries {
        let key = plan::name_key(&entry.name);
        if entry.action == Action::Delete {
            if plan::names_folder(&entry.name) {
                deleted_folders.insert(key.clone());
            }
            deleted.insert(key);
            continue;
        }
        // A file that already is the new version needs no work. Unless an earlier entry
        // deletes it, or the folder it is in: then it is gone by the time this entry's turn
        // comes, and has to be put there again.
        let removed_first =
            deleted.contains(&key) || plan::folders_above(&key).any(|above| deleted_folders.contains(above));
        let in_place =
            !removed_first && found.get(&entry.name).is_some_and(|print| Some(*print) == Fingerprint::promised(entry));
        if let Action::Create { .. } = entry.action {
            if in_place {
                checked.up_to_date.insert(entry.name.clone());
            }
            continue;
        }
        let Action::Rebuild { old_crc, sources, .. } = &entry.action else { continue };

        if in_place {
            checked.up_to_date.insert(entry.name.clone());
            continue;
        }
        if built.contains(&entry.name) {
            continue;
        }
        for source in sources {
            let expected = match plan.layout {
                Layout::Table => plan.originals.get(source).copied(),
                Layout::PerEntry => *old_crc,
            };
            let Some(expected) = expected else {
                return Err(Error::corrupt(format!("no original checksum for {source}")));
            };
            match found.get(source) {
                // This is the very file the patch was made for. A copy that reads past its
                // end asks for bytes that file never had: the patch is at fault, not the disk.
                Some(print) if print.crc == expected => {
                    if lies_about.is_none() && plan.reach.get(source).is_some_and(|reach| *reach > print.len) {
                        lies_about = Some(source);
                    }
                }
                _ => {
                    differing.insert(source.clone());
                }
            }
        }
    }

    // Files that differ come first: with them in the folder there is no telling yet what
    // the patch would make of the right ones.
    if !differing.is_empty() {
        return Err(Error::OriginalMismatch(differing.into_iter().collect()));
    }
    match lies_about {
        Some(source) => Err(Error::corrupt(format!("a copy reads past the end of {source}"))),
        None => Ok(checked),
    }
}

/// Fingerprints several files under `root`, `checkers` of them at a time. Reading is the slow
/// part and runs well in parallel on an SSD, while a hard disk is better read one file after
/// the other: see [`Profile`]. `on_progress` hears which file is being read and how many more
/// bytes of it were; `on_file` hears of each file as it is done. Both are called from
/// whichever thread did the work.
pub fn fingerprint_all(
    files: &[(String, u64)],
    root: &Path,
    checkers: usize,
    stop: &AtomicBool,
    on_progress: &(dyn Fn(&str, u64) + Sync),
    on_file: &(dyn Fn(&str, Fingerprint) + Sync),
) -> Result<BTreeMap<String, Fingerprint>> {
    let next = AtomicUsize::new(0);
    let found = Mutex::new(BTreeMap::new());
    let failure = Mutex::new(None);
    let workers = checkers.clamp(1, files.len().max(1));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while failure.lock().unwrap().is_none() {
                    let Some((name, _)) = files.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                    let path = match plan::relative_path(name) {
                        Ok(relative) => root.join(relative),
                        Err(err) => return *failure.lock().unwrap() = Some(err),
                    };
                    match fingerprint(&path, stop, &|bytes| on_progress(name, bytes)) {
                        Ok(print) => {
                            found.lock().unwrap().insert(name.clone(), print);
                            on_file(name, print);
                        }
                        Err(err) => return *failure.lock().unwrap() = Some(err),
                    }
                }
            });
        }
    });

    match failure.into_inner().unwrap() {
        Some(err) => Err(err),
        None => Ok(found.into_inner().unwrap()),
    }
}
