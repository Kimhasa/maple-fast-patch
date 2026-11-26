//! Moves the built files into the install folder, and puts everything back if that fails.
//!
//! Nothing is deleted while applying: whatever a new file replaces is moved into the backup
//! folder first. Each step is written to a journal before it happens, so a run that dies
//! halfway (crash, power loss) can be undone the next time the app starts.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::busy::when_free;
use crate::error::{Error, Result, blocked_unless_empty};
use crate::plan::{self, Action, Plan};
use crate::preflight::{self, Problem};
use crate::stage::{self, Checked, Dirs};

/// Journal line kinds.
///
/// `B <slot> <name>`: what was at `name` is being moved to backup slot `slot`.
/// `N <name>`: a new file is being placed at `name`, where nothing was.
/// `D <name>`: the folder `name` is being created.
const BACKED_UP: &str = "B";
const PLACED_NEW: &str = "N";
const MADE_DIR: &str = "D";

/// What [`recover`] found left over from an earlier run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leftover {
    /// Nothing.
    None,
    /// Built files that the next run of the same patch can reuse.
    Staged,
    /// An apply had stopped halfway; the install folder was put back as it was.
    RolledBack,
}

/// What an apply tells of while it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step<'a> {
    /// The entry of this name is being worked on.
    Begins(&'a str),
    /// The new file of this name is in its place for good. This is told of every new file
    /// once the apply is finished, and of none before: an apply that fails puts every file
    /// back.
    Placed(&'a str),
}

/// Whether an apply was cut off in this install folder and is still to be undone. Until it
/// is, files of the folder lie among the backups, and the folder may be short of the very
/// things an install folder is known by.
pub(crate) fn cut_off(dirs: &Dirs) -> bool {
    dirs.journal().exists() && !dirs.committed().exists()
}

/// Files in the install folder that this plan will replace or delete.
pub(crate) fn targets(plan: &Plan, checked: &Checked, dirs: &Dirs) -> Vec<PathBuf> {
    plan.entries
        .iter()
        .filter(|entry| checked.builds(entry) || entry.action == Action::Delete)
        .filter_map(|entry| Some(dirs.install.join(plan::relative_path(&entry.name).ok()?)))
        .filter(|path| path.is_file())
        .collect()
}

/// Existing folders inside the install folder that this plan will write into or delete from.
pub(crate) fn target_dirs(plan: &Plan, dirs: &Dirs) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    for entry in &plan.entries {
        let Ok(relative) = plan::relative_path(&entry.name) else { continue };
        let target = dirs.install.join(relative);
        if let Some(existing) = target.ancestors().skip(1).find(|dir| dir.is_dir()) {
            found.insert(existing.to_path_buf());
        }
    }
    found.into_iter().collect()
}

/// Applies the staged files. Returns how many files were placed or removed. `on_step` hears
/// of every entry as its turn comes, and of every new file once all of them are in place
/// for good.
///
/// On failure the install folder is restored before the error is returned.
pub fn apply(plan: &Plan, checked: &Checked, dirs: &Dirs, on_step: &mut dyn FnMut(Step)) -> Result<usize> {
    // Every file this run decided to build has to be there. Only those files are applied,
    // never whatever else happens to lie in the staging folder.
    for entry in plan.entries.iter().filter(|entry| checked.builds(entry)) {
        let staged = dirs.staged().join(plan::relative_path(&entry.name)?);
        if !staged.is_file() {
            return Err(Error::file("find the built file", &staged, io::ErrorKind::NotFound.into()));
        }
    }
    // Looked at once more at the last moment: building takes minutes, long enough for a
    // folder to have been swapped for a link that leads out of the install folder.
    guard_work(dirs)?;
    blocked_unless_empty(preflight::check_inside_install(&dirs.install, &target_dirs(plan, dirs)))?;

    let journal = open_journal(dirs)?;
    let mut applying = Applying { dirs, journal, next_slot: 0, fuse: None, placed: Vec::new() };

    let outcome = applying.entries(plan, checked, on_step).and_then(|count| {
        // From this point the patch counts as applied, even if cleanup is interrupted.
        let committed = dirs.committed();
        File::create(&committed).and_then(|file| file.sync_all()).map_err(|e| Error::file("create", &committed, e))?;
        Ok(count)
    });
    let placed = std::mem::take(&mut applying.placed);
    drop(applying);

    match outcome {
        Ok(count) => {
            // Said only now, and before the work folder goes: until the apply was finished,
            // a failure would have put every one of these files back.
            for name in &placed {
                on_step(Step::Placed(name));
            }
            clean_up(dirs);
            Ok(count)
        }
        Err(err) => {
            let _ = fs::remove_file(dirs.committed());
            // The reason the apply failed is what the user needs; a failed restore is worse
            // and takes its place.
            roll_back(dirs)?;
            Err(err)
        }
    }
}

/// Starts an apply and abandons it partway, leaving the install folder exactly as a crash or
/// power cut at that moment would. For tests of [`recover`].
///
/// `fuse` counts the moments at which it can die: before each journal line is written, and
/// again after it is written but before the step it describes happens. Returns `true` when
/// the fuse outlasted every step, in which case it dies just before the apply is marked
/// finished.
#[doc(hidden)]
pub fn apply_then_crash(plan: &Plan, checked: &Checked, dirs: &Dirs, fuse: usize) -> Result<bool> {
    let journal = open_journal(dirs)?;
    let mut applying = Applying { dirs, journal, next_slot: 0, fuse: Some(fuse), placed: Vec::new() };
    match applying.entries(plan, checked, &mut |_| {}) {
        Ok(_) => Ok(true),
        Err(Error::Stopped) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Starts the journal of a new apply. No other program can delete or rename it while the
/// apply holds it open: without the journal a failed apply could not be undone.
fn open_journal(dirs: &Dirs) -> Result<File> {
    let path = dirs.journal();
    let mut options = OpenOptions::new();
    options.create_new(true).append(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Others may read it; nobody may write to, move or delete it.
        options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
    }
    options.open(&path).map_err(|e| Error::file("create", &path, e))
}

/// Refuses a work folder that is, or holds, a link to somewhere else. Everything in it is
/// moved into the install folder or back, so it has to be the plain folder the engine made.
pub(crate) fn guard_work(dirs: &Dirs) -> Result<()> {
    for dir in [dirs.work(), dirs.staged(), dirs.backup()] {
        if fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(Error::Blocked(vec![Problem::OutsideInstall { dir }]));
        }
    }
    Ok(())
}

/// Refuses a path that a link has led out of the install folder. Undoing an apply moves
/// files to the places its journal names; a planted link in one of those places must not
/// turn that into moving files anywhere on the PC.
fn guard_inside(dirs: &Dirs, path: &Path) -> Result<()> {
    let existing = path.ancestors().skip(1).find(|dir| dir.exists()).unwrap_or(&dirs.install);
    blocked_unless_empty(preflight::check_inside_install(&dirs.install, &[existing.to_path_buf()]))
}

struct Applying<'a> {
    dirs: &'a Dirs,
    journal: File,
    next_slot: usize,
    /// Test hook: moments left before the apply is abandoned. See [`apply_then_crash`].
    fuse: Option<usize>,
    /// The new files that are in place so far, by the names the patch gives them.
    placed: Vec<String>,
}

impl Applying<'_> {
    fn entries(&mut self, plan: &Plan, checked: &Checked, on_step: &mut dyn FnMut(Step)) -> Result<usize> {
        let mut count = 0;
        for entry in &plan.entries {
            let relative = plan::relative_path(&entry.name)?;
            let target = self.dirs.install.join(&relative);

            match &entry.action {
                Action::CreateDir => self.make_dirs(&relative)?,
                Action::Delete => {
                    if stage::deletes(&entry.name, &target) {
                        on_step(Step::Begins(&entry.name));
                        self.move_aside(&entry.name, &target)?;
                        count += 1;
                    }
                }
                Action::Create { .. } | Action::Rebuild { .. } => {
                    if !checked.builds(entry) {
                        continue;
                    }
                    on_step(Step::Begins(&entry.name));
                    if let Some(parent) = relative.parent() {
                        self.make_dirs(parent)?;
                    }
                    if target.is_dir() {
                        // A file never takes the place of a folder and everything in it.
                        let in_the_way = io::Error::new(io::ErrorKind::AlreadyExists, "a folder has this name");
                        return Err(Error::file("replace", &target, in_the_way));
                    }
                    if target.exists() {
                        self.move_aside(&entry.name, &target)?;
                    } else {
                        self.record(&format!("{PLACED_NEW}\t{}", entry.name))?;
                    }
                    let staged = self.dirs.staged().join(&relative);
                    when_free(|| fs::rename(&staged, &target)).map_err(|e| Error::file("place", &target, e))?;
                    self.placed.push(entry.name.clone());
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Creates the folder at `relative` and any missing parents, one level at a time so each
    /// can be removed again.
    fn make_dirs(&mut self, relative: &Path) -> Result<()> {
        let mut path = PathBuf::new();
        for part in relative.components() {
            path.push(part);
            let dir = self.dirs.install.join(&path);
            if dir.is_dir() {
                continue;
            }
            self.record(&format!("{MADE_DIR}\t{}", path.display()))?;
            fs::create_dir(&dir).map_err(|e| Error::file("create", &dir, e))?;
        }
        Ok(())
    }

    /// Moves a file or folder out of the install folder into its own backup slot.
    fn move_aside(&mut self, name: &str, target: &Path) -> Result<()> {
        let slot = self.next_slot;
        self.next_slot += 1;
        let backup = self.dirs.backup();
        fs::create_dir_all(&backup).map_err(|e| Error::file("create", &backup, e))?;
        self.record(&format!("{BACKED_UP}\t{slot}\t{name}"))?;
        clear_read_only(target);
        let slot = backup.join(slot.to_string());
        when_free(|| fs::rename(target, &slot)).map_err(|e| Error::file("replace", target, e))
    }

    /// The journal line is on disk before the step it describes happens.
    fn record(&mut self, line: &str) -> Result<()> {
        self.burn()?;
        writeln!(self.journal, "{line}")
            .and_then(|()| self.journal.sync_data())
            .map_err(|e| Error::io("write the apply journal", e))?;
        self.burn()
    }

    fn burn(&mut self) -> Result<()> {
        match &mut self.fuse {
            Some(0) => Err(Error::Stopped),
            Some(left) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }
}

/// Takes the read-only mark off a file, which would otherwise keep it from being moved or
/// replaced. A file that is not there, or cannot be changed, is left to the step that follows
/// to report.
pub(crate) fn clear_read_only(path: &Path) {
    if let Ok(meta) = fs::metadata(path) {
        let mut permissions = meta.permissions();
        if permissions.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            let _ = fs::set_permissions(path, permissions);
        }
    }
}

/// Removes the work folder of a finished apply, with everything in it: the patch is in
/// place, so the patch file that was downloaded into it has done its work, and an install
/// folder that was half patched no longer is.
///
/// The journal goes first and the marker that says "finished" last. A journal without the
/// marker reads as an apply that was cut off and gets undone, so the marker stays for as
/// long as the journal cannot be removed; the next start tries again.
///
/// The same holds for the marker of a half-patched install folder. Left behind without
/// "finished" beside it, it would say that this patch is still to be finished, and keep
/// every other patch out.
fn clean_up(dirs: &Dirs) {
    for file in [dirs.journal(), dirs.half_patched()] {
        if when_free(|| fs::remove_file(&file)).is_err() && file.exists() {
            return;
        }
    }
    let work = dirs.work();
    if let Ok(children) = fs::read_dir(&work) {
        for child in children.flatten() {
            let path = child.path();
            if path == dirs.committed() {
                continue;
            }
            let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        }
    }
    let _ = fs::remove_file(dirs.committed());
    let _ = fs::remove_dir(&work);
}

/// Looks for the work folder of an earlier run and brings the install folder to a known
/// state: finished runs are cleaned up, half-applied ones are undone.
pub fn recover(dirs: &Dirs) -> Result<Leftover> {
    if !dirs.work().exists() {
        return Ok(Leftover::None);
    }
    if dirs.committed().exists() {
        clean_up(dirs);
        return Ok(Leftover::None);
    }
    if dirs.journal().exists() {
        roll_back(dirs)?;
        return Ok(Leftover::RolledBack);
    }
    // Patch files the app downloaded into the work folder are nothing a run left behind.
    if !preflight::holds_work(&dirs.work()) {
        return Ok(Leftover::None);
    }
    Ok(Leftover::Staged)
}

/// Undoes every journaled step, newest first. New files go back to the staging folder so the
/// next run does not have to build them again.
fn roll_back(dirs: &Dirs) -> Result<()> {
    guard_work(dirs)?;
    let journal_path = dirs.journal();
    let journal = match fs::read_to_string(&journal_path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::file("read", &journal_path, e)),
    };
    // A last line without its line break was cut off while being written. Its step never
    // started, because a step only starts once its line is safely on disk.
    let complete = journal.rfind('\n').map_or("", |end| &journal[..=end]);
    // Each line is cut from the journal once it is undone. If undoing stops halfway (a file
    // is held open, the power goes), the next attempt carries on from there instead of
    // undoing the same steps twice against a folder that has already changed back.
    let shorten = |len: usize| -> Result<()> {
        let cut = || {
            let file = OpenOptions::new().write(true).open(&journal_path)?;
            file.set_len(len as u64)?;
            file.sync_data()
        };
        when_free(cut).map_err(|e| Error::file("write", &journal_path, e))
    };

    let mut end = complete.len();
    for line in complete.lines().rev() {
        let start = end - line.len() - 1;
        let mut fields = line.split('\t');
        match (fields.next(), fields.next(), fields.next()) {
            (Some(BACKED_UP), Some(slot), Some(name))
                if !slot.is_empty() && slot.bytes().all(|b| b.is_ascii_digit()) =>
            {
                let backup = dirs.backup().join(slot);
                // The line may have been written without the move having happened.
                if backup.exists() {
                    let relative = plan::relative_path(name)?;
                    let target = dirs.install.join(&relative);
                    guard_inside(dirs, &target)?;
                    unplace(&target, &dirs.staged().join(&relative))?;
                    when_free(|| fs::rename(&backup, &target)).map_err(|e| Error::file("restore", &target, e))?;
                }
            }
            (Some(PLACED_NEW), Some(name), None) => {
                let relative = plan::relative_path(name)?;
                let target = dirs.install.join(&relative);
                guard_inside(dirs, &target)?;
                unplace(&target, &dirs.staged().join(&relative))?;
            }
            (Some(MADE_DIR), Some(name), None) => {
                // Only removes the folder if it is empty again, which it is when everything
                // placed inside it has been undone.
                let dir = dirs.install.join(plan::relative_path(name)?);
                guard_inside(dirs, &dir)?;
                let _ = fs::remove_dir(dir);
            }
            _ => return Err(Error::corrupt(format!("unreadable line in the apply journal: {line}"))),
        }
        shorten(start)?;
        end = start;
    }

    when_free(|| fs::remove_file(&journal_path)).map_err(|e| Error::file("remove", &journal_path, e))?;
    let _ = fs::remove_dir_all(dirs.backup());
    Ok(())
}

/// Takes whatever now stands at `target` out of the way so the original can return. A new
/// file goes back to the staging folder.
fn unplace(target: &Path, staged: &Path) -> Result<()> {
    if target.is_dir() {
        // Everything placed inside has already been undone, so the folder is empty.
        return fs::remove_dir(target).map_err(|e| Error::file("remove", target, e));
    }
    if !target.is_file() {
        return Ok(());
    }
    if staged.exists() {
        return when_free(|| fs::remove_file(target)).map_err(|e| Error::file("remove", target, e));
    }
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::file("create", parent, e))?;
    }
    when_free(|| fs::rename(target, staged)).map_err(|e| Error::file("restore", target, e))
}
