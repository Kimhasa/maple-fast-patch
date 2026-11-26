//! Puts an install folder back to what it was before an apply that was cut off, without
//! the journal of that apply: for when the journal cannot be read, or is gone.
//!
//! The journal is only the record of the undo. What the undo needs is still on disk: an
//! apply moves every original aside under the name it had (`aside\<step>\<name>`, see
//! [`Dirs::aside`]), so what lies there says by itself where it belongs. Putting it all
//! back, newest step first, takes the folder to where it was when the apply began.
//!
//! What the folder cannot say is which new files the apply put where nothing was, and
//! whether what was moved aside is undamaged. The patch file answers both, where it is at
//! hand: it names the files it creates, and states the checksums of the old files. With it
//! the folder is checked after it was put back, and only then called restored.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::apply::{self, Fuse, Leftover};
use crate::busy::when_free;
use crate::container::PatchFile;
use crate::error::{Error, Result};
use crate::plan::{self, Action, Entry, Layout, Plan};
use crate::preflight::Problem;
use crate::stage::{self, Dirs, Fingerprint};

/// Never set. Putting a folder back is not paused: it is short of both versions until it
/// is done.
static GO_ON: AtomicBool = AtomicBool::new(false);

/// One thing that an apply moved aside, or a folder on the way to it.
struct Kept {
    /// Where it lies now, in the folder of its step.
    path: PathBuf,
    /// Where it belongs, from the install folder.
    place: PathBuf,
    /// Whether it is a plain folder. Anything else is moved back as the one thing it is,
    /// a link too, which is not looked into.
    folder: bool,
}

/// Whether anything but empty folders lies below `dir`. Links are not followed: a link is
/// something that lies there.
pub(crate) fn holds_files(dir: &Path) -> bool {
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(children) = fs::read_dir(&folder) else { continue };
        for child in children.flatten() {
            match child.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(child.path()),
                _ => return true,
            }
        }
    }
    false
}

/// Why this install folder cannot be put back without its journal, where that is known
/// without trying: an attempt before this one found out and left a note of it, or what was
/// moved aside lies where earlier versions put it, numbered and without names.
pub(crate) fn out_of_reach(dirs: &Dirs) -> Option<String> {
    if let Ok(why) = fs::read_to_string(dirs.cannot_restore()) {
        return Some(why);
    }
    let nameless = "what the apply moved aside carries no names: an earlier version moved it";
    holds_files(&dirs.backup()).then(|| nameless.to_owned())
}

/// Puts the install folder back from what the apply moved aside, and says how far that
/// can be vouched for. `trouble` is what is wrong with the journal, for the error this
/// ends in where the folder cannot be put back. For `patch` see [`apply::recover_with`],
/// for `fuse` [`apply::restore_then_crash`].
///
/// Nothing is deleted on the way. An original goes back by being moved over what stands
/// in its place, and a new file that is in the way goes back among the built files, where
/// the next run finds it. Each move leaves the folder nearer to the old version, so being
/// cut off at any of them leaves a folder that the next attempt carries on from, and doing
/// it all twice is doing it once.
pub(crate) fn restore(dirs: &Dirs, patch: Option<&Path>, trouble: String, fuse: &mut Fuse) -> Result<Leftover> {
    let cannot = |why: &str| {
        Error::Blocked(vec![Problem::UnreadableJournal { dir: dirs.work(), detail: format!("{trouble}; {why}") }])
    };
    if let Some(why) = out_of_reach(dirs) {
        return Err(cannot(&why));
    }
    // A folder that is half patched was never wholly the old version, and nothing here can
    // make it that: what the immediate way put in place early took the place of its
    // original for good. What can be undone is the apply that was cut off, which leaves
    // the folder half patched as it was, for its own patch to finish.
    let whole_before = stage::half_patched(dirs).is_none();
    let plan = patch_at_hand(dirs, patch);
    let kept = list_kept(dirs);

    // Before anything is moved: what was moved aside has to be what the patch says the old
    // files are. A backup that is not is put nowhere.
    if let Some(plan) = &plan
        && let Some(why) = first_wrong_backup(plan, &kept)?
    {
        return Err(give_up(dirs, &cannot, &why));
    }
    put_back(dirs, &kept, fuse)?;
    if let (Some(plan), true) = (&plan, whole_before) {
        take_out_what_the_patch_made(dirs, plan, fuse)?;
    }
    let outcome = match (&plan, whole_before) {
        (Some(plan), true) => match hold_against_the_patch(dirs, plan)? {
            Found::OldVersion => Leftover::Restored,
            // Not the old version in every file, and nothing more can be put back. But
            // the patch still fits the folder, and applying it is the way to a folder
            // that is one version again: the new one. Nothing stands in the way of that.
            Found::NotAllOld => Leftover::PutBack,
            Found::Unfit(why) => return Err(give_up(dirs, &cannot, &why)),
        },
        _ => Leftover::PutBack,
    };

    // The traces of the apply go last, the journal first among them: until it is gone,
    // the next start comes here again and finds nothing left to do.
    fuse.burn()?;
    stage::remove(&dirs.journal())?;
    fuse.burn()?;
    stage::remove(&dirs.aside())?;
    Ok(outcome)
}

/// Notes in the work folder why this install folder cannot be put back, so that the next
/// look does not have to read everything again to find out, and returns the error to end
/// with. A note that cannot be written costs the next look that reading, and nothing more.
fn give_up(dirs: &Dirs, cannot: &dyn Fn(&str) -> Error, why: &str) -> Error {
    let _ = fs::write(dirs.cannot_restore(), why);
    cannot(why)
}

/// The plan of the patch whose apply was cut off, if that patch file is on this PC where it
/// can be found: at `hint`, or among the patch files downloaded into the work folder. It
/// is told from any other patch by the id the work folder was made for, and read only if
/// its own checksum says that it is whole.
fn patch_at_hand(dirs: &Dirs, hint: Option<&Path>) -> Option<Plan> {
    let id = stage::made_for(dirs)?;
    let downloaded = fs::read_dir(dirs.download()).into_iter().flatten().flatten().map(|file| file.path());
    hint.map(Path::to_path_buf).into_iter().chain(downloaded).find_map(|path| {
        let patch = PatchFile::open(&path).ok().filter(|patch| patch.id() == id)?;
        patch.verify(&GO_ON, |_| {}).ok()?;
        plan::scan(&patch, &GO_ON).ok()
    })
}

/// Everything that lies where the apply moved things aside, in the order it is put back:
/// the newest step first, since what a later step moved may stand where an earlier one took
/// something from, and within a step the folders before what is in them.
///
/// Only the numbered folders of steps are looked at. A link is listed as the one thing it
/// is and not looked into, whatever it leads to.
fn list_kept(dirs: &Dirs) -> Vec<Kept> {
    let mut steps: Vec<(u64, PathBuf)> = fs::read_dir(dirs.aside())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|step| step.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|step| Some((step.file_name().to_str()?.parse().ok()?, step.path())))
        .collect();
    steps.sort_by_key(|step| std::cmp::Reverse(step.0));

    let mut kept = Vec::new();
    for (_, step) in steps {
        let mut folders = vec![step.clone()];
        let mut found = Vec::new();
        while let Some(folder) = folders.pop() {
            for child in fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = child.path();
                let Ok(place) = path.strip_prefix(&step).map(Path::to_path_buf) else { continue };
                let folder = child.file_type().is_ok_and(|kind| kind.is_dir());
                if folder {
                    folders.push(path.clone());
                }
                found.push(Kept { path, place, folder });
            }
        }
        // Folders first, the ones nearer the top before those below them.
        found.sort_by_key(|kept| (!kept.folder, kept.place.components().count()));
        kept.extend(found);
    }
    kept
}

/// What the patch knows of the file of one name: the checksum it had before the patch,
/// where the patch states one, and what it is after.
#[derive(Default, Clone, Copy)]
struct Known {
    old: Option<u32>,
    new: Option<Fingerprint>,
}

/// What the patch knows of each file it names, by the name in the form names are compared
/// in.
fn known_files(plan: &Plan) -> HashMap<String, Known> {
    let mut known: HashMap<String, Known> = HashMap::new();
    for (name, crc) in &plan.originals {
        known.entry(plan::name_key(name)).or_default().old = Some(*crc);
    }
    for entry in &plan.entries {
        let of_entry = known.entry(plan::name_key(&entry.name)).or_default();
        of_entry.new = Fingerprint::promised(entry);
        if let Action::Rebuild { old_crc: Some(crc), .. } = &entry.action {
            of_entry.old = Some(*crc);
        }
    }
    known
}

/// The first file among what was moved aside that is not the file the patch says stood in
/// its place, with what is wrong with it. Files the patch states no checksum for cannot be
/// judged and pass.
///
/// A file that is the new version of its name passes as well: a step that moved a whole
/// folder aside took along whatever earlier steps had put into it.
fn first_wrong_backup(plan: &Plan, kept: &[Kept]) -> Result<Option<String>> {
    let known = known_files(plan);
    for item in kept.iter().filter(|item| !item.folder && !apply::is_link(&item.path)) {
        let Some(of_file) = known.get(&plan::name_key(&item.place.to_string_lossy())) else { continue };
        let Some(old) = of_file.old else { continue };
        let found = stage::fingerprint(&item.path, &GO_ON, &|_| {})?;
        if found.crc != old && Some(found) != of_file.new {
            return Ok(Some(format!(
                "what was moved aside from {} is not the file the patch was made for",
                item.place.display()
            )));
        }
    }
    Ok(None)
}

/// Moves everything in `kept` back to its place in the install folder.
fn put_back(dirs: &Dirs, kept: &[Kept], fuse: &mut Fuse) -> Result<()> {
    for item in kept {
        fuse.burn()?;
        let target = dirs.install.join(&item.place);
        apply::guard_inside(dirs, &target)?;
        let standing = fs::symlink_metadata(&target).ok();
        if standing.as_ref().is_some_and(|meta| meta.file_type().is_symlink()) {
            return Err(Error::Blocked(vec![Problem::OutsideInstall { dir: target }]));
        }
        let (is_folder, is_file) =
            (standing.as_ref().is_some_and(|m| m.is_dir()), standing.is_some_and(|m| m.is_file()));
        match (item.folder, is_folder, is_file) {
            // The folder is there.
            (true, true, _) => {}
            // A folder belongs here. A file that stands in its place came with the patch.
            (true, false, _) => {
                if is_file {
                    set_aside_new(dirs, &target, &item.place)?;
                    fuse.burn()?;
                }
                fs::create_dir_all(&target).map_err(|e| Error::file("create", &target, e))?;
            }
            // A file belongs here, or a link. A folder that stands in its place was made
            // by the patch, with everything in it, and is taken apart first.
            (false, ..) => {
                if is_folder {
                    take_apart_new_folder(dirs, &target, &item.place)?;
                } else if is_file && !dirs.staged().join(&item.place).exists() {
                    // The new file is kept for the next run before the old one takes its
                    // place again. Where a copy of it is among the built files already,
                    // the old one simply goes over it.
                    set_aside_new(dirs, &target, &item.place)?;
                }
                fuse.burn()?;
                when_free(|| fs::rename(&item.path, &target)).map_err(|e| Error::file("restore", &target, e))?;
            }
        }
    }
    Ok(())
}

/// Moves a new file out of the install folder, back among the built files. If its like is
/// there already, it stays where it is for the original to be moved over.
fn set_aside_new(dirs: &Dirs, target: &Path, place: &Path) -> Result<()> {
    let staged = dirs.staged().join(place);
    if staged.exists() {
        return when_free(|| fs::remove_file(target)).map_err(|e| Error::file("remove", target, e));
    }
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::file("create", parent, e))?;
    }
    when_free(|| fs::rename(target, &staged)).map_err(|e| Error::file("restore", target, e))
}

/// Takes apart a folder that stands where a file was before the patch. Nothing in it is
/// older than the apply: its files go back among the built files, and the folders, empty
/// then, are removed. A link in it is not followed, and stops this.
fn take_apart_new_folder(dirs: &Dirs, folder: &Path, place: &Path) -> Result<()> {
    let children = fs::read_dir(folder).map_err(|e| Error::file("read", folder, e))?;
    for child in children.flatten() {
        let (path, below) = (child.path(), place.join(child.file_name()));
        match child.file_type() {
            Ok(kind) if kind.is_symlink() => return Err(Error::Blocked(vec![Problem::OutsideInstall { dir: path }])),
            Ok(kind) if kind.is_dir() => take_apart_new_folder(dirs, &path, &below)?,
            _ => set_aside_new(dirs, &path, &below)?,
        }
    }
    fs::remove_dir(folder).map_err(|e| Error::file("remove", folder, e))
}

/// Takes out of the install folder what the patch made where nothing was: its new files
/// that stand in the folder though no original of theirs was moved aside, and the folders
/// it makes, where they are empty. The files go back among the built files.
///
/// This is where the patch file is needed. The folder does not say which of its files came
/// with the apply; the patch names the files it makes, and a file of that name that is
/// byte for byte the new one is taken for the apply's. If it was there before the apply,
/// it is among the built files now, and the next run of the patch puts it back.
fn take_out_what_the_patch_made(dirs: &Dirs, plan: &Plan, fuse: &mut Fuse) -> Result<()> {
    // An entry for a whole file of no bytes writes nothing.
    let writes_file = |entry: &Entry| match entry.action {
        Action::Create { len, .. } => len > 0,
        Action::Rebuild { .. } => true,
        Action::CreateDir | Action::Delete => false,
    };
    let known = known_files(plan);
    // Folders that may be the patch's own: the ones it makes, and those its new files are in.
    let mut folders: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in &plan.entries {
        let place = plan::relative_path(&entry.name)?;
        let target = dirs.install.join(&place);
        if entry.action == Action::CreateDir {
            folders.extend(place.ancestors().filter(|above| !above.as_os_str().is_empty()).map(Path::to_path_buf));
        }
        // A file that the patch states an old checksum for had an original. If its new
        // version stands there now and nothing of it was moved aside, that original is
        // lost, and the new file is the only one there is: it stays, and the check that
        // follows says that the folder is not the old version.
        let had_an_original = known.get(&plan::name_key(&entry.name)).is_some_and(|of_file| of_file.old.is_some());
        if !writes_file(entry) || had_an_original {
            continue;
        }
        let Some(promised) = Fingerprint::promised(entry) else { continue };
        let is_new = fs::symlink_metadata(&target).is_ok_and(|meta| meta.is_file() && meta.len() == promised.len)
            && stage::fingerprint(&target, &GO_ON, &|_| {})? == promised;
        if is_new {
            apply::guard_inside(dirs, &target)?;
            fuse.burn()?;
            set_aside_new(dirs, &target, &place)?;
            folders
                .extend(place.ancestors().skip(1).filter(|above| !above.as_os_str().is_empty()).map(Path::to_path_buf));
        }
    }
    // The deepest first. A folder with anything in it is not removed, which is how one
    // that was there before the patch stays.
    for folder in folders.iter().rev() {
        let _ = fs::remove_dir(dirs.install.join(folder));
    }
    Ok(())
}

/// What the install folder is, held against the patch, once everything was put back.
enum Found {
    /// The version before the patch, for every file the patch has a say about.
    OldVersion,
    /// Not wholly that version: a file is the new one already, or is not what the patch
    /// says it was. But every original the patch still needs is the right file, so the
    /// patch can be applied to the folder as it is.
    NotAllOld,
    /// Neither: the patch cannot be applied to what is there. Why not.
    Unfit(String),
}

/// Holds the install folder against the patch. It is the version before the patch when
///
/// - nothing that was moved aside is still waiting to be put back;
/// - every original that a rebuild copies from is the file the patch was made for;
/// - no file is the new version already;
/// - every file the patch replaces or deletes, and states the old checksum of, has it.
///
/// Files the patch does not name are not looked at: no apply touches them. And a file the
/// patch only deletes or replaces whole, without stating what it was, cannot be checked
/// against anything. It was moved aside and moved back, and never written.
fn hold_against_the_patch(dirs: &Dirs, plan: &Plan) -> Result<Found> {
    if holds_files(&dirs.aside()) {
        return Ok(Found::Unfit("not everything that was moved aside could be put back".to_owned()));
    }
    let checked = match stage::check_originals(plan, &dirs.install, &GO_ON, &|_, _| {}) {
        Ok(checked) => checked,
        Err(Error::OriginalMismatch(files)) => {
            return Ok(Found::Unfit(format!("{} is not the file from before the patch", files.join(", "))));
        }
        Err(Error::CorruptPatch(detail)) => return Ok(Found::Unfit(detail)),
        Err(other) => return Err(other),
    };
    if !checked.up_to_date.is_empty() {
        return Ok(Found::NotAllOld);
    }

    // The files that entries replace or delete, with the checksum the patch states for
    // each. A rebuild that copies from the file it replaces was checked just now.
    let mut stated: Vec<(String, u32)> = Vec::new();
    for entry in &plan.entries {
        let old = match &entry.action {
            Action::Rebuild { sources, .. } if sources.contains(&entry.name) => None,
            Action::Rebuild { old_crc, .. } => *old_crc,
            Action::Create { .. } => plan.originals.get(&entry.name).copied(),
            Action::Delete if plan.layout == Layout::Table && !plan::names_folder(&entry.name) => {
                plan.originals.get(&entry.name).copied()
            }
            Action::Delete | Action::CreateDir => None,
        };
        stated.extend(old.map(|crc| (entry.name.clone(), crc)));
    }
    let present = stage::files_present(stated.iter().map(|(name, _)| name.clone()), &dirs.install);
    let found = stage::fingerprint_all(&present, &dirs.install, 2, &GO_ON, &|_, _| {}, &|_, _| {})?;
    let all_old = stated.iter().all(|(name, crc)| found.get(name).map(|print| print.crc) == Some(*crc));
    Ok(if all_old { Found::OldVersion } else { Found::NotAllOld })
}
