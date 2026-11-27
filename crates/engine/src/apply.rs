//! Moves the built files into the install folder, and puts everything back if that fails.
//!
//! Nothing is deleted while applying: whatever a new file replaces is moved aside first,
//! into the work folder. Each step is written to a journal before it happens, so a run that
//! dies halfway (crash, power loss) can be undone the next time the app starts.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::busy::when_free;
use crate::error::{Error, Result, blocked_unless_empty};
use crate::plan::{self, Action, Plan};
use crate::preflight::{self, Problem};
use crate::restore;
use crate::stage::{self, Checked, Dirs};

/// Journal line kinds.
///
/// `M <step> <name>`: what was at `name` is being moved aside, to `aside\<step>\<name>`.
/// `N <name>`: a new file is being placed at `name`, where nothing was.
/// `D <name>`: the folder `name` is being created.
///
/// A line of any other kind makes the journal one that cannot be read: see [`read_journal`].
const MOVED_ASIDE: &str = "M";
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
    /// An apply had stopped halfway and its journal could not be read. The install folder
    /// was put back from what the apply had moved aside, and then checked against the patch
    /// file: for every file that patch has a say about, it is the version before the patch.
    Restored,
    /// An apply had stopped halfway and its journal could not be read. Everything it had
    /// moved aside is back in its place, but the folder is not vouched for as the version
    /// before the patch. Either that could not be checked: the patch file was not at hand,
    /// or the folder is half patched and never was that version. Or it was checked, and a
    /// file is the new one with no original to put in its place, while everything the
    /// patch still needs is in order: such a folder cannot go back, and the patch can be
    /// applied to it as it is. The check of the originals that the next run makes is what
    /// decides, as it does for every run.
    PutBack,
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
/// is, files of the folder lie where the apply moved them aside, and the folder may be short
/// of the very things an install folder is known by.
///
/// The journal says so, and so does anything that still lies where an apply moves things
/// aside: a journal can be lost. So does the note of an attempt to put the folder back that
/// found it could not.
pub(crate) fn cut_off(dirs: &Dirs) -> bool {
    let traces = dirs.journal().exists() || restore::holds_files(&dirs.aside()) || dirs.cannot_restore().exists();
    traces && !dirs.committed().exists()
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

    begin_fresh(dirs)?;
    let journal = open_journal(dirs)?;
    let mut applying = Applying { dirs, journal, next_step: 0, fuse: Fuse(None), placed: Vec::new() };

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
            roll_back(dirs, &mut Fuse(None))?;
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
    begin_fresh(dirs)?;
    let journal = open_journal(dirs)?;
    let mut applying = Applying { dirs, journal, next_step: 0, fuse: Fuse(Some(fuse)), placed: Vec::new() };
    match applying.entries(plan, checked, &mut |_| {}) {
        Ok(_) => Ok(true),
        Err(Error::Stopped) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Undoes an apply that was cut off and abandons the undoing partway, leaving the install
/// folder exactly as a crash or power cut at that moment would. For tests of [`recover`].
///
/// `fuse` counts the moments at which it can die: before a step is undone, between the two
/// moves of a step that takes two, after a step is undone and before its line is cut from
/// the journal, and after the last line, before the journal goes. Returns `true` when the
/// fuse outlasted the undoing, which is then complete.
#[doc(hidden)]
pub fn undo_then_crash(dirs: &Dirs, fuse: usize) -> Result<bool> {
    match roll_back(dirs, &mut Fuse(Some(fuse))) {
        Ok(()) => Ok(true),
        Err(Error::Stopped) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Puts an install folder back without its journal and abandons that partway, leaving the
/// folder exactly as a crash or power cut at that moment would. For tests of [`recover`].
///
/// `patch` is where the patch file may be, as for [`recover_with`]. `fuse` counts the moments
/// at which it can die: before each thing that is moved, between the two moves that putting
/// one file back can take, and before the traces of the apply are removed. Returns `true`
/// when the fuse outlasted it all.
#[doc(hidden)]
pub fn restore_then_crash(dirs: &Dirs, patch: Option<&Path>, fuse: usize) -> Result<bool> {
    match restore::restore(dirs, patch, "cut off for a test".to_owned(), &mut Fuse(Some(fuse))) {
        Ok(_) => Ok(true),
        Err(Error::Stopped) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Makes sure that nothing of an apply before this one lies in the work folder: what it
/// moved aside, and its mark of being finished. An apply that was cut off has been undone
/// by now, and one that finished has been cleaned up after, so whatever is still there
/// could not be removed at the time. It has to go before this apply moves anything aside:
/// what lies there is taken for this apply's own if it is cut off.
fn begin_fresh(dirs: &Dirs) -> Result<()> {
    for left in [dirs.aside(), dirs.committed()] {
        stage::remove(&left)?;
    }
    Ok(())
}

/// Test hook: the moments left before the work in hand is abandoned, as a crash would
/// abandon it. `None` outside of tests: nothing is abandoned.
pub(crate) struct Fuse(pub(crate) Option<usize>);

impl Fuse {
    /// One moment at which a crash could come has passed, or the crash comes.
    pub(crate) fn burn(&mut self) -> Result<()> {
        match &mut self.0 {
            Some(0) => Err(Error::Stopped),
            Some(left) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }
}

/// Starts the journal of a new apply. No other program can delete or rename it while the
/// apply holds it open: without the journal a failed apply could not be undone.
///
/// The work folder is made here if it is not there. It is there whenever something was
/// built into it, but an apply may have nothing built to place: a patch that only deletes,
/// or one whose new files the immediate way has all put in place already.
fn open_journal(dirs: &Dirs) -> Result<File> {
    let (work, path) = (dirs.work(), dirs.journal());
    fs::create_dir_all(&work).map_err(|e| Error::file("create", &work, e))?;
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

/// Refuses a work folder that is, or holds anywhere in it, a link to somewhere else.
///
/// Whatever is in the work folder is the engine's to write, to move into the install folder
/// and back, and to delete. Through a link all of that would be done to a folder somewhere
/// else on the PC: its files deleted as work nobody needs, gigabytes built into it. So the
/// work folder has to be the plain folder the engine made, all the way down. This is looked
/// at before anything in it is touched, and again before each step that takes a while.
pub(crate) fn guard_work(dirs: &Dirs) -> Result<()> {
    let outside = |dir: PathBuf| Err(Error::Blocked(vec![Problem::OutsideInstall { dir }]));
    let work = dirs.work();
    if is_link(&work) {
        return outside(work);
    }
    let aside = dirs.aside();
    let mut folders = vec![work];
    while let Some(folder) = folders.pop() {
        // What cannot be listed is not there, or is no folder.
        let Ok(children) = fs::read_dir(&folder) else { continue };
        for child in children.flatten() {
            let path = child.path();
            match child.file_type() {
                Ok(kind) if kind.is_symlink() => return outside(path),
                // What was moved aside is not looked into: the folder of a step, which the
                // apply makes itself the moment it uses it. It is moved back as it is, or
                // removed, without any link in it being followed, and a folder of the
                // install that was moved aside may well hold links of its own.
                Ok(kind) if kind.is_dir() && folder != aside => folders.push(path),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Whether `path` is a link to somewhere else, of whatever kind, and whether or not what it
/// leads to is there.
pub(crate) fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Refuses a path that a link has led out of the install folder. Undoing an apply moves
/// files to the places its journal names; a planted link in one of those places must not
/// turn that into moving files anywhere on the PC.
pub(crate) fn guard_inside(dirs: &Dirs, path: &Path) -> Result<()> {
    let existing = path.ancestors().skip(1).find(|dir| dir.exists()).unwrap_or(&dirs.install);
    blocked_unless_empty(preflight::check_inside_install(&dirs.install, &[existing.to_path_buf()]))
}

struct Applying<'a> {
    dirs: &'a Dirs,
    journal: File,
    next_step: usize,
    /// See [`apply_then_crash`].
    fuse: Fuse,
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
                        self.move_aside(&entry.name, &relative, &target)?;
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
                        self.move_aside(&entry.name, &relative, &target)?;
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

    /// Moves a file or folder out of the install folder, to a place of its own that says
    /// where it came from: `aside\<step>\<name>`. See [`Dirs::aside`].
    fn move_aside(&mut self, name: &str, relative: &Path, target: &Path) -> Result<()> {
        let aside = self.dirs.aside();
        fs::create_dir_all(&aside).map_err(|e| Error::file("create", &aside, e))?;
        // The folder of a step is made here and now, by this apply. One of that number
        // that is there already is not this apply's, and nothing is moved into it.
        let (step, folder) = loop {
            let step = self.next_step;
            self.next_step += 1;
            let folder = aside.join(step.to_string());
            match fs::create_dir(&folder) {
                Ok(()) => break (step, folder),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::file("create", &folder, e)),
            }
        };
        self.record(&format!("{MOVED_ASIDE}\t{step}\t{name}"))?;
        let kept = folder.join(relative);
        if let Some(parent) = kept.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::file("create", parent, e))?;
        }
        clear_read_only(target);
        when_free(|| fs::rename(target, &kept)).map_err(|e| Error::file("replace", target, e))
    }

    /// The journal line is on disk before the step it describes happens.
    fn record(&mut self, line: &str) -> Result<()> {
        self.fuse.burn()?;
        writeln!(self.journal, "{line}")
            .and_then(|()| self.journal.sync_data())
            .map_err(|e| Error::io("write the apply journal", e))?;
        self.fuse.burn()
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
            let _ = stage::remove(&path);
        }
    }
    // The mark also stays for as long as anything that was moved aside is still there.
    // Without the mark beside it, that would read as an apply that was cut off, and be
    // put back over the new files.
    if fs::symlink_metadata(dirs.aside()).is_ok() {
        return;
    }
    let _ = fs::remove_file(dirs.committed());
    let _ = fs::remove_dir(&work);
}

/// Looks for the work folder of an earlier run and brings the install folder to a known
/// state: finished runs are cleaned up, half-applied ones are undone.
pub fn recover(dirs: &Dirs) -> Result<Leftover> {
    recover_with(dirs, None)
}

/// [`recover`] by a caller who knows where a patch file is: the one a run was given, say.
///
/// An apply that was cut off is undone from its journal. Where the journal cannot be read,
/// or is gone, the folder is put back from what the apply moved aside, which says by itself
/// where it belongs, and is then checked against the patch that was being applied: see
/// [`Leftover::Restored`] and [`Leftover::PutBack`]. That patch is looked for among the
/// patch files downloaded into the work folder, and at `patch`. It is told from any other
/// by the id the work folder was made for, so a file that is another patch is never used.
///
/// Where the folder cannot be put back this way either, this fails with
/// [`Problem::UnreadableJournal`], and says why in its `detail`.
pub fn recover_with(dirs: &Dirs, patch: Option<&Path>) -> Result<Leftover> {
    // Every use of an install folder begins here, so this is where a work folder that
    // leads somewhere else is turned away before anything is done to it.
    guard_work(dirs)?;
    if !dirs.work().exists() {
        return Ok(Leftover::None);
    }
    if dirs.committed().exists() {
        clean_up(dirs);
        return Ok(Leftover::None);
    }
    if cut_off(dirs) {
        let trouble = match read_journal(dirs)? {
            Journal::Lines(_) => {
                roll_back(dirs, &mut Fuse(None))?;
                // A journal that had lost lines leaves what those lines moved aside.
                if !restore::holds_files(&dirs.aside()) {
                    return Ok(Leftover::RolledBack);
                }
                "the journal lacked steps of the apply".to_owned()
            }
            Journal::Gone => "the journal is gone".to_owned(),
            Journal::Unreadable(detail) => detail,
        };
        return restore::restore(dirs, patch, trouble, &mut Fuse(None));
    }
    // Patch files the app downloaded into the work folder are nothing a run left behind.
    if !preflight::holds_work(&dirs.work()) {
        return Ok(Leftover::None);
    }
    Ok(Leftover::Staged)
}

/// What a line of the journal says an apply had begun to do.
enum Begun {
    /// What stood at this place in the install folder is moved aside, into the folder of
    /// this step.
    MovedAside { step: String, place: PathBuf },
    /// A new file is placed here, where nothing was.
    PlacedNew { place: PathBuf },
    /// This folder is made.
    MadeDir { place: PathBuf },
}

/// What one line of the journal says, or `None` for a line that cannot be undone: one of a
/// kind an apply does not write, with more or less to it than an apply writes, or naming a
/// place that is not inside the install folder.
fn read_line(line: &str) -> Option<Begun> {
    let mut fields = line.split('\t');
    let place = |name: &str| plan::relative_path(name).ok();
    let number = |text: &str| (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.to_owned());
    match (fields.next()?, fields.next(), fields.next(), fields.next()) {
        (MOVED_ASIDE, Some(step), Some(name), None) => {
            Some(Begun::MovedAside { step: number(step)?, place: place(name)? })
        }
        (PLACED_NEW, Some(name), None, None) => Some(Begun::PlacedNew { place: place(name)? }),
        (MADE_DIR, Some(name), None, None) => Some(Begun::MadeDir { place: place(name)? }),
        _ => None,
    }
}

/// The journal of an apply that was cut off, as far as it can be read.
enum Journal {
    /// There is none.
    Gone,
    /// What each line says, oldest first, with where in the file the line begins.
    Lines(Vec<(usize, Begun)>),
    /// A line of it cannot be read, or it is not text: what is wrong, for a log.
    Unreadable(String),
}

/// Reads the journal of an apply that was cut off.
///
/// The whole journal is read before anything is undone. One line that cannot be read makes
/// it a journal that cannot be read, and none of it is acted on: an undo that stops at such
/// a line would leave the folder further from both the old version and the new one than it
/// was.
///
/// A last line without its line break was cut off while being written. Its step never
/// started, because a step only starts once its line is safely on disk, so whatever stands
/// there is no part of the journal and says nothing against it.
fn read_journal(dirs: &Dirs) -> Result<Journal> {
    let path = dirs.journal();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Journal::Gone),
        // Not being able to open the file is no fault of what is in it.
        Err(e) => return Err(Error::file("read", &path, e)),
    };
    let complete = bytes.iter().rposition(|byte| *byte == b'\n').map_or(&[][..], |end| &bytes[..=end]);
    let Ok(complete) = std::str::from_utf8(complete) else {
        return Ok(Journal::Unreadable("the journal is not text".to_owned()));
    };

    let mut lines = Vec::new();
    let mut start = 0;
    for (number, line) in complete.split_terminator('\n').enumerate() {
        let Some(begun) = read_line(line) else {
            // A line of garbage can be as long as the file. A log needs its beginning.
            let shown: String = line.chars().take(120).collect();
            return Ok(Journal::Unreadable(format!("line {} of the journal: {shown:?}", number + 1)));
        };
        lines.push((start, begun));
        start += line.len() + 1;
    }
    Ok(Journal::Lines(lines))
}

/// What is told of an apply that was cut off in this install folder and cannot be undone:
/// its journal cannot be read, and the folder cannot be put back without it either. Only
/// looks: for telling of it before anything is asked of the folder.
///
/// A journal that cannot be read is nothing to tell of by itself. The next start puts the
/// folder back from what the apply moved aside, as it would have from the journal. This
/// answers only where that is known to be out of reach: putting the folder back was tried
/// and failed. Whether it will work cannot be seen without trying, so a folder that has not
/// been tried is not told of here; the try is what [`recover`] does.
///
/// A work folder that is a link is not looked into. It is refused for what it is.
pub(crate) fn unreadable_journal(dirs: &Dirs) -> Option<Problem> {
    if is_link(&dirs.work()) || !cut_off(dirs) {
        return None;
    }
    let trouble = match read_journal(dirs) {
        Ok(Journal::Unreadable(detail)) => detail,
        Ok(Journal::Gone) => "the journal is gone".to_owned(),
        _ => return None,
    };
    let why = restore::out_of_reach(dirs)?;
    Some(Problem::UnreadableJournal { dir: dirs.work(), detail: format!("{trouble}; {why}") })
}

/// Whether the journal of the apply that was cut off in this install folder is one that
/// cannot be read, or is gone.
pub(crate) fn journal_cannot_be_read(dirs: &Dirs) -> bool {
    matches!(read_journal(dirs), Ok(Journal::Unreadable(_) | Journal::Gone))
}

/// Undoes every journaled step, newest first. New files go back to the staging folder so the
/// next run does not have to build them again. For `fuse` see [`undo_then_crash`].
fn roll_back(dirs: &Dirs, fuse: &mut Fuse) -> Result<()> {
    guard_work(dirs)?;
    let lines = match read_journal(dirs)? {
        Journal::Lines(lines) => lines,
        Journal::Gone => return Ok(()),
        Journal::Unreadable(detail) => {
            return Err(Error::Blocked(vec![Problem::UnreadableJournal { dir: dirs.work(), detail }]));
        }
    };
    let journal_path = dirs.journal();
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

    for (start, begun) in lines.into_iter().rev() {
        fuse.burn()?;
        match &begun {
            Begun::MovedAside { step, place } => {
                let kept = dirs.aside().join(step).join(place);
                // The line may have been written without the move having happened. A link
                // that was moved aside is there even if what it leads to is not.
                if fs::symlink_metadata(&kept).is_ok() {
                    let target = dirs.install.join(place);
                    guard_inside(dirs, &target)?;
                    unplace(&target, &dirs.staged().join(place))?;
                    fuse.burn()?;
                    when_free(|| fs::rename(&kept, &target)).map_err(|e| Error::file("restore", &target, e))?;
                }
            }
            Begun::PlacedNew { place } => {
                let target = dirs.install.join(place);
                guard_inside(dirs, &target)?;
                unplace(&target, &dirs.staged().join(place))?;
            }
            Begun::MadeDir { place } => {
                // Only removes the folder if it is empty again, which it is when everything
                // placed inside it has been undone.
                let dir = dirs.install.join(place);
                guard_inside(dirs, &dir)?;
                let _ = fs::remove_dir(dir);
            }
        }
        fuse.burn()?;
        // This also cuts off a last line that was never finished.
        shorten(start)?;
    }

    fuse.burn()?;
    when_free(|| fs::remove_file(&journal_path)).map_err(|e| Error::file("remove", &journal_path, e))?;
    // What is left of the steps are the folders they were kept in. Anything more than
    // that is a file the journal did not know of, which is not thrown away.
    if !restore::holds_files(&dirs.aside()) {
        let _ = fs::remove_dir_all(dirs.aside());
    }
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
