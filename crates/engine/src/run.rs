//! One patch run from start to finish.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::apply::{self, Leftover, Step};
use crate::busy;
use crate::container::{HEADER_LEN, PatchFile};
use crate::download;
use crate::error::{Error, Result, blocked_unless_empty};
use crate::immediate;
use crate::plan::{self, Plan};
use crate::preflight::{self, Problem, Programs, SPACE_MARGIN, SpaceNeed};
use crate::stage::{self, Checked, Dirs};
use crate::storage::{self, Profile};

/// The steps of a run, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Download,
    VerifyPatch,
    VerifyInstall,
    /// Only in the immediate way, only before its first game file is replaced, and only
    /// where the drive has not the room to build every new file before the first is placed:
    /// every new file is built once and kept nowhere, to see that the patch can be built
    /// from the files that are there. A run with room for all of them has no such step. It
    /// builds them all before it places any, which shows the same.
    Trial,
    Rebuild,
    Apply,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// A step began.
    Stage(Stage),
    /// Bytes done and total within the current step.
    Progress { done: u64, total: u64 },
    /// The file being worked on.
    File(String),
    /// A new file that an earlier, interrupted run had already built was checked and is kept:
    /// its name and size. The size is part of the next `Progress`, though nothing was built.
    Kept { name: String, bytes: u64 },
    /// A new file has taken its place in the install folder and stays there: the name the
    /// patch gives it. Never sent for a file the patch deletes or for a folder.
    ///
    /// It is only sent of what cannot be taken back. A file that the immediate way puts in
    /// place ahead of the others is named as soon as it is there. The files of the apply at
    /// the end are named together when that apply is finished, before the work folder is
    /// removed: until then a failure puts every one of them back, and of an apply that
    /// failed none is named.
    ///
    /// So over all the runs it takes to finish one patch, no file is named twice and none
    /// is named that is not in place. The other way round has one gap: a process that dies
    /// in the instant after a file is in place for good and before this is sent has not
    /// named it, and the next run finds it in place and has nothing to say of it.
    Placed { name: String },
    /// Space the next step needs on a drive, and what is free there. Sent before the download
    /// and again, with the exact figure, once the patch file has been read.
    Space { needed: u64, available: u64 },
    /// An earlier run had stopped while applying; the install folder was put back first.
    /// How it was put back is one of three: [`Leftover::RolledBack`], from the journal of
    /// that apply; [`Leftover::Restored`], without the journal, and then proven against the
    /// patch file to be the version before the patch; or [`Leftover::PutBack`], without the
    /// journal and without that proof. It is what [`recover`] returns of such a folder, and
    /// is told there.
    Restored(Leftover),
    /// What was found out about the drive the game is on, and how it is read because of
    /// that. Sent once, before the game files are checked. See [`storage`].
    Storage { file_system: String, seek_ms: Option<f64>, slow: bool, readers: usize, checkers: usize, unbuffered: bool },
}

pub enum Source {
    /// A patch file already on this PC.
    File(PathBuf),
    /// A patch file to download to `dest`.
    Url { url: String, dest: PathBuf },
}

pub struct Options {
    pub install_dir: PathBuf,
    pub source: Source,
    /// Which way of patching the player chose. With this set, the run goes the immediate way
    /// (see [`immediate`]) from its start, whatever room the drive has: new files replace
    /// the originals while others are still being built, and a run that stops partway
    /// cannot be undone, only finished. Without it the run goes the safe way and nothing
    /// else: it is refused for room where the new files do not fit next to the old ones.
    ///
    /// The choice is not bent in either direction. A run that was asked to go the immediate
    /// way on a drive that is not NTFS is refused with
    /// [`Problem::ImmediateNeedsNtfs`], before anything is fetched or read: it does not
    /// quietly go the safe way. The one thing that outweighs the choice is a folder that is
    /// half patched already, which goes on the immediate way whatever this says.
    pub immediate: bool,
    /// Who decides how the drive is read.
    pub storage: storage::Choice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Files placed, replaced or removed. Zero means the install was already up to date.
    pub files_changed: usize,
    /// Whether this run went the immediate way: that way was chosen, or the folder was half
    /// patched. It is so also when the run found nothing to put in place ahead of the
    /// others. False for a run that went the safe way from start to finish.
    pub immediate: bool,
}

/// How a run learns what is free on a drive: from the drive, or from a made-up one in tests.
#[doc(hidden)]
pub struct Disk<'a> {
    /// Free bytes on the drive that holds a folder.
    pub free: &'a dyn Fn(&Path) -> io::Result<u64>,
    /// Room the immediate way asks for at its start beyond the least it can work with:
    /// [`immediate::SLACK`], or less to make a test's drive a tight one.
    pub slack: u64,
    /// The file system the drive is said to have, for a test of a drive of another kind
    /// than the one its folder is on. `None` asks the drive.
    pub file_system: Option<&'a str>,
}

/// Held for the length of a run so that two runs never work on the same install folder at
/// once, whether from one app or two.
///
/// Public for tests, which hold it the way a run that is going on does.
#[doc(hidden)]
pub struct InstallLock {
    path: PathBuf,
    file: Option<File>,
}

impl InstallLock {
    pub fn acquire(dirs: &Dirs) -> Result<InstallLock> {
        let path = dirs.lock();
        let mut options = fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // No sharing: a second open fails for as long as this handle lives, and the
            // system releases it even if the app crashes.
            options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_NONE);
        }
        match options.open(&path) {
            Ok(file) => Ok(InstallLock { path, file: Some(file) }),
            Err(err) if busy::held(&err) => Err(Error::Blocked(vec![Problem::AlreadyRunning])),
            Err(err) => Err(Error::file("lock", &path, err)),
        }
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        // The file can only be deleted once its handle is closed.
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

/// Restores the install folder if an earlier run was cut off while applying. Safe to call at
/// any time; does nothing while another run holds the folder.
///
/// What comes back says how the folder was put back:
///
/// - [`Leftover::RolledBack`]: from the journal of the apply, step by step. The folder is as
///   it was before that apply.
/// - [`Leftover::Restored`]: the journal could not be read, or was gone. The folder was put
///   back from what the apply had moved aside, and then checked against the patch file,
///   which was found among the downloads in the work folder: for every file that patch has
///   a say about, the folder is the version before the patch. Nothing needs to be asked of
///   anyone; the patch can simply be applied again.
/// - [`Leftover::PutBack`]: as before, but the folder is not vouched for as the old
///   version. The check could not be made: the patch file was not among the downloads, or
///   the folder is half patched and never was the old version. Or it was made and found a
///   file that is the new one, with no original to put in its place, while every original
///   the patch still needs is the right file. Everything the apply had moved aside is
///   back; the run that comes next checks every original it needs before it changes
///   anything, as every run does, and where the patch still fits it simply applies.
///
/// It fails with [`Problem::UnreadableJournal`] where the journal cannot be read and the
/// folder cannot be put back without it either: what was moved aside is not what the patch
/// says the old files are, or the check afterwards found that the patch cannot be applied
/// to what is there, or an earlier version moved things aside without their names. The
/// `detail` says which.
/// Nothing but [`discard_work_as_it_is`] clears such a folder. A file held by another
/// program or a link in the way fail as themselves, and the next call tries again.
///
/// Putting a folder back without its journal reads the patch file and the originals, which
/// takes as long as the check of the game files at the start of a run.
pub fn recover(install_dir: &Path) -> Result<Leftover> {
    let dirs = Dirs::new(install_dir);
    if !dirs.work().exists() {
        return Ok(Leftover::None);
    }
    let _lock = InstallLock::acquire(&dirs)?;
    apply::recover(&dirs)
}

/// Whether an apply was cut off in this install folder and is still to be undone, which is
/// what [`recover`] does. True also where the journal of that apply is gone and only what
/// it moved aside tells of it.
pub fn cut_off(install_dir: &Path) -> bool {
    apply::cut_off(&Dirs::new(install_dir))
}

/// Whether the install folder is half patched: a run in the immediate way moved new files
/// into place and did not finish. The folder is then partly the old version and partly the
/// new one, and only the patch that began can finish it. See [`immediate`].
pub fn half_patched(install_dir: &Path) -> bool {
    stage::half_patched(&Dirs::new(install_dir)).is_some()
}

/// Deletes everything a paused or interrupted run left in the install folder, for a run that
/// will not be continued. A run that was cut off while applying is undone first, so the
/// install folder ends up as it was before that run.
///
/// Patch files downloaded into the work folder go with it, and so does the marker of a
/// half-patched install folder. That folder is not put back: what the immediate way has
/// replaced stays replaced, and with the staged files gone no patch can finish it.
///
/// A work folder that is a link to somewhere else is no work of the engine's. The link is
/// taken away, and what it leads to is left as it is.
///
/// An apply that cannot be undone, neither from its journal nor from what it moved aside,
/// leaves its work folder here: this fails with [`Problem::UnreadableJournal`] and removes
/// nothing, so that nothing clears such a folder by accident. See [`discard_work_as_it_is`].
pub fn discard_work(install_dir: &Path) -> Result<()> {
    discard(install_dir, false)
}

/// [`discard_work`] for an install folder whose apply was cut off and cannot be undone: its
/// journal cannot be read, and the folder could not be put back from what the apply moved
/// aside either ([`Problem::UnreadableJournal`]). The last resort, for a caller who has
/// been told what that leaves and asks for it all the same.
///
/// Wherever [`discard_work`] can do its work, this does exactly that work: the apply is
/// undone first, from its journal or, where that cannot be read, from what it moved aside.
/// Only where neither can be done is the apply passed over, and then the work folder is
/// removed as it is, with nothing in it trusted or acted on:
///
/// - Nothing outside the work folder is touched. No file of the game is moved, written or
///   removed.
/// - No link is followed. A link in the work folder goes as the link it is, and so does a
///   work folder that is one.
/// - Everything in the work folder goes: the new files that were built and not yet placed,
///   the patch files downloaded into it, the marker of a half-patched folder, and what the
///   apply had moved aside. Those are files of the game, and for a file the apply replaced
///   or deleted they are the only copy of the old version.
///
/// The install folder is then, for good, what it was when this was called. If nothing
/// could be put back, that is what the apply left when it was cut off, and each file that
/// the patch replaces, creates or deletes is in one of these states, with nothing on this
/// PC to say which:
///
/// - as it was before the patch, where the apply had not come to it;
/// - as the patch makes it: the new file in place, or the file gone;
/// - missing although the patch does not delete it. That can be one file at most, the one
///   that was being replaced at the moment the apply was cut off: its old version was
///   among what was moved aside, and its new version among the built files. If that one
///   file is the game's own program, the folder is no longer taken for a folder of the
///   game at all.
///
/// If putting back got as far as moving the originals back and then found a file that is
/// not the old one, every file is the old one again but those the `detail` of the problem
/// named. Files the patch does not name are as they were, and no file is half written:
/// files are only ever moved into place whole.
///
/// So the folder may be neither the old version nor the new one. A later run goes by the
/// files themselves, as every run does. A patch goes through if each file it still has to
/// build can be built from files that are there and are the ones it was made for; files
/// that are the new version already are left alone. Any other patch is refused with
/// [`Error::OriginalMismatch`] and changes nothing. The patch that was cut off may be
/// either: with the built files gone, it can no longer build a file from an original that
/// its own apply has replaced. Where no patch fits, the game's own launcher or a new
/// install is what puts the folder right.
///
/// Fails like [`discard_work`] while a run holds the folder, and for whatever else keeps a
/// journal that can be read from being undone: a file held open, a link in the way.
pub fn discard_work_as_it_is(install_dir: &Path) -> Result<()> {
    discard(install_dir, true)
}

/// Removes the work folder of `install_dir`. An apply that was cut off is undone first. If
/// it cannot be, its journal being one that cannot be read, `as_it_is` says to remove the
/// work folder anyway.
fn discard(install_dir: &Path, as_it_is: bool) -> Result<()> {
    let dirs = Dirs::new(install_dir);
    let work = dirs.work();
    if fs::symlink_metadata(&work).is_err() {
        return Ok(());
    }
    let _lock = InstallLock::acquire(&dirs)?;
    // Only an apply that was cut off has to be undone. Everything else in the work folder
    // simply goes, and removing a folder follows no link that may be in it.
    if !apply::is_link(&work) && apply::cut_off(&dirs) {
        let unread = apply::journal_cannot_be_read(&dirs);
        match apply::recover(&dirs) {
            // Turned away: the folder cannot be put back, or a link in the work folder
            // keeps anything from being tried. A journal that can be read is not passed
            // over for that; it is undone once what is in its way is gone.
            Err(Error::Blocked(_)) if as_it_is && unread => {}
            undone => {
                undone?;
            }
        }
    }
    stage::remove_work(&work)
}

/// Runs a patch on the install folder of the client whose programs are `programs`. A folder
/// that is not that client's is refused, and that client's programs are the ones that must
/// not be running. Setting `stop` ends the run with [`Error::Stopped`] at the next safe
/// point; everything finished so far is kept, and calling `run_for` again continues from
/// there.
pub fn run_for(
    programs: &Programs,
    options: &Options,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Outcome> {
    run_on(programs, options, &preflight::free_space, stop, on_event)
}

/// [`run_for`] on a drive whose free space is whatever `free` says. For tests.
#[doc(hidden)]
pub fn run_on(
    programs: &Programs,
    options: &Options,
    free: &dyn Fn(&Path) -> io::Result<u64>,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Outcome> {
    run_on_disk(programs, options, &Disk { free, slack: immediate::SLACK, file_system: None }, stop, on_event)
}

/// [`run_on`] with the room the immediate way asks for beyond its least chosen as well, so
/// that a test can make the drive exactly as tight as it wants.
#[doc(hidden)]
pub fn run_on_disk(
    programs: &Programs,
    options: &Options,
    disk: &Disk,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Outcome> {
    let dirs = Dirs::new(&options.install_dir);
    // A folder that is not this client's is refused before anything in it is touched: it is
    // not locked, and nothing in it is restored. The one exception is a folder in which an
    // apply was cut off. The game's own program may be among the files that apply had moved
    // aside, so only undoing it shows whose folder this is.
    let problems = programs.check_install_dir(&dirs.install);
    if !problems.is_empty() && !apply::cut_off(&dirs) {
        return Err(Error::Blocked(problems));
    }
    let _lock = InstallLock::acquire(&dirs)?;
    // Restoring comes before the check that counts: a run that died while replacing the
    // game's own executable leaves a folder that would not pass it.
    // The patch file this run was given may be the one whose apply was cut off. A folder
    // that has to be put back without its journal is checked against it.
    let given = match &options.source {
        Source::File(path) => path,
        Source::Url { dest, .. } => dest,
    };
    let undone = apply::recover_with(&dirs, Some(given))?;
    if matches!(undone, Leftover::RolledBack | Leftover::Restored | Leftover::PutBack) {
        on_event(Event::Restored(undone));
    }
    blocked_unless_empty(programs.check_install_dir(&dirs.install))?;

    // What was built for a half-patched install folder exists nowhere else, so nothing but
    // the patch that began there may work on it. That one goes on the immediate way: the
    // safe way has no promise left to keep.
    let pinned = stage::half_patched(&dirs);
    // The player chose the immediate way, and the drive either takes it or it does not.
    // That is said now, before the patch file is fetched and the game files are read.
    if options.immediate && pinned.is_none() {
        require_drive_that_takes_immediate(&dirs, disk.file_system)?;
    }
    let (patch, plan) = match (&pinned, &options.source) {
        (Some(id), Source::Url { url, dest }) => obtain_pinned(id, url, dest, &dirs, disk.free, stop, on_event)?,
        _ => obtain_patch(&options.source, disk.free, stop, on_event)?,
    };
    if pinned.as_ref().is_some_and(|id| *id != patch.id()) {
        return Err(Error::Blocked(vec![Problem::LeftoverWork { dir: dirs.work() }]));
    }
    let half_patched = pinned.is_some();
    refuse_patch_that_deletes_the_game(programs, &plan)?;
    blocked_unless_empty(preflight::check_inside_install(&dirs.install, &apply::target_dirs(&plan, &dirs)))?;
    // A folder that is half patched is not turned away on what sizes say. Going on is all
    // that helps it, and the check that is made once its files are read asks it for the
    // least only.
    if !half_patched {
        let to_spare = options.immediate.then_some(disk.slack);
        require_room_for_certain(&patch, &plan, &dirs, to_spare, disk.free, on_event)?;
    }

    let originals = stage::originals_to_check(&plan, &dirs.install);
    let profile = look_at_drive(options.storage, &originals, &dirs, disk.file_system, on_event);

    on_event(Event::Stage(Stage::VerifyInstall));
    // Only in a half-patched install folder can a file be built while what it was built
    // from is gone. Those files are found first, so that their originals are not asked for.
    let built = if half_patched { Some(immediate::built_before(&profile, &patch, &plan, &dirs, stop)?) } else { None };
    let none_built = BTreeSet::new();
    let checked =
        verify_install(&profile, &plan, &dirs, &originals, built.as_ref().unwrap_or(&none_built), stop, on_event)?;

    blocked_unless_empty(preflight::check_writable(&dirs.install))?;
    // Files built from another patch go before the space is measured: they are of no use
    // to this run and may be most of what fills the drive.
    stage::discard_foreign_work(&patch, &dirs)?;

    // Which way it goes is the player's choice, made before the run, and holds for every
    // run, whatever room the drive has. A folder that is half patched already goes on the
    // immediate way whatever was chosen, and wherever it is: the safe way has no promise
    // left to keep there, and stopping helps nobody.
    let immediate = half_patched || options.immediate;

    let (checked, placed_early) = if immediate {
        let job = immediate::Job {
            programs,
            patch: &patch,
            plan: &plan,
            dirs: &dirs,
            profile: &profile,
            free: disk.free,
            slack: disk.slack,
            stop,
        };
        immediate::build_and_place(&job, checked, built, on_event)?
    } else {
        let to_build = stage::bytes_to_build(&plan, &checked, &dirs);
        require_space(&dirs.work(), to_build, disk.free, on_event)?;
        build_all(&profile, &patch, &plan, &checked, &dirs, stop, on_event)?;
        (checked, 0)
    };

    let applied = apply_built(programs, &plan, &checked, &dirs, stop, on_event)?;
    Ok(Outcome { files_changed: placed_early + applied, immediate })
}

/// Refuses a patch that deletes the game's own program. No patch of the game does: a folder
/// without that program is no folder of this client any more, which the next run, and this
/// engine's own look at the folder, would turn away. A patch that puts a new program in the
/// place of the old one is every other patch, and is not meant.
fn refuse_patch_that_deletes_the_game(programs: &Programs, plan: &Plan) -> Result<()> {
    let game = plan::name_key(programs.game);
    let deletes_it = |entry: &&plan::Entry| {
        entry.action == plan::Action::Delete && !plan::names_folder(&entry.name) && plan::name_key(&entry.name) == game
    };
    match plan.entries.iter().find(deletes_it) {
        Some(entry) => Err(Error::UnknownForm(format!("the patch deletes {}, the game's own program", entry.name))),
        None => Ok(()),
    }
}

/// Opens the patch file a run was given, downloading it first if it was given as an address.
fn obtain_patch(
    source: &Source,
    free: &dyn Fn(&Path) -> io::Result<u64>,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<(PatchFile, Plan)> {
    let (url, dest) = match source {
        Source::File(path) => return open_patch(path, stop, on_event),
        Source::Url { url, dest } => (url, dest),
    };
    fetch(url, dest, free, stop, on_event)?;
    match open_patch(dest, stop, on_event) {
        // A download that does not check out is thrown away and fetched once more before
        // giving up. One that is whole and still cannot be read is kept: fetching it again
        // would bring the same file.
        Err(Error::CorruptPatch(_) | Error::NotAPatchFile) => {
            download::discard(dest);
            fetch(url, dest, free, stop, on_event)?;
            // Only a file that is itself bad is thrown away; a pause or a read error says
            // nothing against the download.
            open_patch(dest, stop, on_event).inspect_err(|err| {
                if matches!(err, Error::CorruptPatch(_) | Error::NotAPatchFile) {
                    download::discard(dest);
                }
            })
        }
        other => other,
    }
}

/// Gets the patch file for an install folder that is half patched. Only the patch file that
/// began there can finish it, and `pinned` is the id of that one.
///
/// The copy on this PC comes first. While it is the file that began, the server is not
/// asked at all: by now it may have another file under the same address, or none, or be
/// out of reach, and none of that may cost the one file that can finish the folder. Only
/// without that copy is the server asked. What it has is told from its beginning, so that
/// another file is refused before it is fetched onto a drive that is short of room already.
fn obtain_pinned(
    pinned: &str,
    url: &str,
    dest: &Path,
    dirs: &Dirs,
    free: &dyn Fn(&Path) -> io::Result<u64>,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<(PatchFile, Plan)> {
    if PatchFile::open(dest).is_ok_and(|local| local.id() == pinned) {
        match open_patch(dest, stop, on_event) {
            // Damaged where it lies. The server may still have it whole.
            Err(Error::CorruptPatch(_) | Error::NotAPatchFile) => {}
            other => return other,
        }
    }
    // A marker that could not be read names no patch file. Nothing is fetched for it.
    if pinned.is_empty() {
        return Err(Error::Blocked(vec![Problem::LeftoverWork { dir: dirs.work() }]));
    }
    let other = |offered: String| Error::PinnedToOtherPatch { pinned: pinned.to_owned(), offered };
    let remote = download::probe_patiently(url, download::Patience::default(), stop)?;
    let start = download::fetch_prefix(url, HEADER_LEN)?;
    if let Some(offered) = PatchFile::id_from_start(&start, remote.len).filter(|offered| offered != pinned) {
        return Err(other(offered));
    }
    let source = Source::Url { url: url.to_owned(), dest: dest.to_path_buf() };
    let (patch, plan) = obtain_patch(&source, free, stop, on_event)?;
    // What could not be told from its beginning is told now.
    if patch.id() != pinned {
        return Err(other(patch.id()));
    }
    Ok((patch, plan))
}

/// Downloads the patch file, asking first how big it is so that a drive without the room
/// for it is not filled.
fn fetch(
    url: &str,
    dest: &Path,
    free: &dyn Fn(&Path) -> io::Result<u64>,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<()> {
    on_event(Event::Stage(Stage::Download));
    // The question and the file go through one session, and so over one connection.
    let session = download::Session::new();
    let patience = download::Patience::default();
    let remote = session.probe_patiently(url, patience, stop)?;
    let missing = remote.len.saturating_sub(download::bytes_on_disk(dest, &remote));
    require_space(dest.parent().unwrap_or(dest), missing, free, on_event)?;
    session.fetch(url, dest, remote, patience, stop, &mut |done, total| on_event(Event::Progress { done, total }))?;
    Ok(())
}

/// Opens a patch file, checks that it is whole, and reads what it will do.
///
/// A file that fails before or at the checksum is damaged, or no patch file at all. One that
/// passes the checksum is what its maker made; if it still cannot be read, the trouble is
/// the form it is in and not this copy of it, and the error says so.
fn open_patch(path: &Path, stop: &AtomicBool, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<(PatchFile, Plan)> {
    on_event(Event::Stage(Stage::VerifyPatch));
    let patch = PatchFile::open(path)?;
    let total = patch.payload_len();
    patch.verify(stop, |done| on_event(Event::Progress { done, total }))?;
    let plan = plan::scan(&patch, stop).map_err(|err| match err {
        Error::CorruptPatch(detail) => Error::UnknownForm(detail),
        other => other,
    })?;
    Ok((patch, plan))
}

/// Stops the run, before any game file is read, when it is already certain that the drive
/// has not the room for the way the run goes. Checking the game files takes minutes on a
/// hard disk, and the answer after it would be the same. Says nothing when the room may be
/// there: the exact check follows.
///
/// `to_spare` is for the immediate way, the room it asks for beyond the least it can work
/// with. `None` is the safe way.
fn require_room_for_certain(
    patch: &PatchFile,
    plan: &Plan,
    dirs: &Dirs,
    to_spare: Option<u64>,
    free: &dyn Fn(&Path) -> io::Result<u64>,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<()> {
    let work = dirs.work();
    // Work of another patch is thrown away before the exact check. How much room that
    // gives back is not known here, so nothing is certain.
    let ours = stage::staged_is_ours(patch, dirs);
    if !ours && preflight::holds_work(&work) {
        return Ok(());
    }
    // What a run of this patch was still writing when it was cut off is no file of
    // anyone's. It goes before the room is looked at: on a drive with little room, the
    // room it takes would be counted as missing.
    if ours {
        stage::remove_unfinished(dirs);
    }
    let (safe, immediate) = room_by_sizes(patch, plan, dirs, to_spare.unwrap_or(0));
    let bytes = if to_spare.is_some() { immediate } else { safe };
    let need = SpaceNeed { dir: work.clone(), bytes };
    if bytes == 0 || preflight::check_space_with(&[need], SPACE_MARGIN, free).is_empty() {
        return Ok(());
    }
    require_space(&work, bytes, free, on_event)
}

/// Looks at the drive the game is on, chooses how it is read, and reports both. A file
/// system that is `pretended` takes the place of the one found.
fn look_at_drive(
    choice: storage::Choice,
    originals: &[(String, u64)],
    dirs: &Dirs,
    pretended: Option<&str>,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Profile {
    // The largest original stands for the drive: reads far apart in it show how long the
    // drive takes to get from one place to another.
    let sample = originals
        .iter()
        .max_by_key(|(_, len)| *len)
        .and_then(|(name, _)| plan::relative_path(name).ok())
        .map(|relative| dirs.install.join(relative));
    let mut volume = storage::look(&dirs.install, sample.as_deref());
    if let Some(pretended) = pretended {
        volume.file_system = pretended.to_owned();
    }
    let profile = choice.profile(&volume);
    on_event(Event::Storage {
        file_system: volume.file_system.clone(),
        seek_ms: volume.seek_ms,
        slow: volume.slow,
        readers: profile.readers,
        checkers: profile.checkers,
        unbuffered: profile.unbuffered,
    });
    profile
}

/// Stops a run for which the immediate way was chosen on a drive where that way is not
/// begun: one that is not NTFS, where it is not known that a file moved over another
/// survives a power cut whole. A file system that is `pretended` takes the place of the
/// one found.
///
/// The run is not sent the safe way in its place. The player chose, and a run that goes
/// another way than was chosen would say nothing of it until it stood refused for room, or
/// was through.
fn require_drive_that_takes_immediate(dirs: &Dirs, pretended: Option<&str>) -> Result<()> {
    let mut volume = storage::look(&dirs.install, None);
    if let Some(pretended) = pretended {
        volume.file_system = pretended.to_owned();
    }
    if volume.takes_immediate() {
        return Ok(());
    }
    Err(Error::Blocked(vec![Problem::ImmediateNeedsNtfs { file_system: volume.file_system }]))
}

/// What each way of patching asks the drive of the install folder to have free before it
/// builds anything, the margin that has to stay free included. Zero where there is nothing
/// to build. These are the figures a run reports in [`Event::Space`] and, where the drive
/// has less, in [`Problem::NotEnoughSpace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    /// For the safe way: every new file that is still to be built, next to the files it
    /// takes the place of.
    pub safe: u64,
    /// For the immediate way: the most it has standing next to the files of the folder at
    /// any one moment, and a little to spare so that it does not build one file at a time.
    /// Never more than `safe`.
    pub immediate: u64,
}

impl Room {
    /// The room that a run asks for. `chosen` is [`Options::immediate`]; a folder that is
    /// `half_patched` (see [`half_patched`]) goes the immediate way whatever was chosen.
    pub fn of_run(&self, chosen: bool, half_patched: bool) -> u64 {
        if chosen || half_patched { self.immediate } else { self.safe }
    }
}

/// Works out what a run of this patch on this install folder will ask the drive to have
/// free, for either way of patching, without running anything: see [`Room`].
///
/// It is told from the patch file, which says how long every new file is, and from the
/// sizes of the files in the folder. No file of the game is read and nothing is changed, so
/// this is quick, and can be asked before the player starts the patch. What it needs is
/// the patch file on this PC: before that is fetched, nothing knows how long the new files
/// are.
///
/// A file that has the size of its new version is taken for that version, as it is when a
/// run that was paused has put it there or built it. Where such a file is in truth still
/// the old one, the run finds out when it reads it and asks for that much more: the figure
/// here is then short by the length of that file, and never too high. Where a file of that
/// size is in place and another waits among the files this patch has built, the built one
/// is taken for the new version, and the one in place for the file it is there to replace.
pub fn room_needed(patch: &PatchFile, plan: &Plan, install_dir: &Path) -> Room {
    let dirs = Dirs::new(install_dir);
    // A folder that is half patched is asked for the least only: see `immediate`.
    let to_spare = if stage::half_patched(&dirs).is_some() { 0 } else { immediate::SLACK };
    let (safe, immediate) = room_by_sizes(patch, plan, &dirs, to_spare);
    let with_margin = |bytes: u64| if bytes == 0 { 0 } else { bytes.saturating_add(SPACE_MARGIN) };
    Room { safe: with_margin(safe), immediate: with_margin(immediate) }
}

/// The bytes each way of patching is certain to need room for, told from the sizes of files
/// alone: for the safe way, and for the immediate way with `to_spare` on top of its least.
/// This is what a run goes by before it has read the game files, and what is told of a
/// patch without a run. A file that has the size of its new version counts as that version.
fn room_by_sizes(patch: &PatchFile, plan: &Plan, dirs: &Dirs, to_spare: u64) -> (u64, u64) {
    let ours = stage::staged_is_ours(patch, dirs);
    let (in_place, built) = stage::told_by_sizes(plan, dirs, ours);
    let safe = stage::bytes_certain_to_build(plan, dirs, ours);
    (safe, immediate::room_to_start(plan, &in_place, &built, &dirs.install, to_spare))
}

/// Checks the game files against what the patch was made for, reporting each file once and
/// the bytes as they are read. `built` is for a half-patched install folder: see
/// [`stage::check_originals_except`].
fn verify_install(
    profile: &Profile,
    plan: &Plan,
    dirs: &Dirs,
    originals: &[(String, u64)],
    built: &BTreeSet<String>,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Checked> {
    let total: u64 = originals.iter().map(|(_, len)| len).sum();
    // Several threads report here; the count, the files named so far and the callback stay
    // under one lock so the figures arrive in order. A file is named once, when the first of
    // it has been read: the threads take turns, and naming whichever spoke last would send
    // the same few names back and forth.
    let progress = Mutex::new((0u64, BTreeSet::new(), on_event));
    let checked = stage::check_originals_with(profile, plan, &dirs.install, built, stop, &|name, bytes| {
        let mut guard = progress.lock().unwrap();
        if !guard.1.contains(name) {
            guard.1.insert(name.to_owned());
            (guard.2)(Event::File(name.to_owned()));
        }
        guard.0 += bytes;
        let done = guard.0;
        (guard.2)(Event::Progress { done, total });
    })?;
    Ok(checked)
}

/// The safe way: builds every new file into the work folder, where files that an earlier
/// run built are kept. The install folder is only read.
fn build_all(
    profile: &Profile,
    patch: &PatchFile,
    plan: &Plan,
    checked: &Checked,
    dirs: &Dirs,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<()> {
    on_event(Event::Stage(Stage::Rebuild));
    let total = plan.new_bytes();
    let mut done = 0u64;
    let mut current = String::new();
    stage::stage_with(profile, patch, plan, checked, dirs, stop, &mut |name, bytes, kept| {
        if kept {
            on_event(Event::Kept { name: name.to_owned(), bytes });
        } else if current != name {
            current = name.to_owned();
            on_event(Event::File(current.clone()));
        }
        done += bytes;
        on_event(Event::Progress { done, total });
    })
}

/// Moves into place what is built and not there yet, removes what the patch deletes and
/// makes its folders, all under the journal that lets it be undone. Returns how many files
/// that were.
fn apply_built(
    programs: &Programs,
    plan: &Plan,
    checked: &Checked,
    dirs: &Dirs,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<usize> {
    // Checked again right before the install folder changes: a game started in the meantime
    // would hold files open and stop the apply halfway.
    let mut problems = programs.check_running(&dirs.install);
    problems.extend(preflight::check_locks(&apply::targets(plan, checked, dirs)));
    blocked_unless_empty(problems)?;
    // The last moment a pause can still take effect: an apply runs to its end.
    if stop.load(Ordering::Relaxed) {
        return Err(Error::Stopped);
    }

    on_event(Event::Stage(Stage::Apply));
    apply::apply(plan, checked, dirs, &mut |step| {
        on_event(match step {
            Step::Begins(name) => Event::File(name.to_owned()),
            Step::Placed(name) => Event::Placed { name: name.to_owned() },
        })
    })
}

/// Stops the run unless `dir`'s drive can take `bytes` more and still keep the safety margin.
/// Reports the figures either way.
fn require_space(
    dir: &Path,
    bytes: u64,
    free: &dyn Fn(&Path) -> io::Result<u64>,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<()> {
    if bytes == 0 {
        return Ok(());
    }
    if let Ok(available) = free(dir) {
        on_event(Event::Space { needed: bytes.saturating_add(SPACE_MARGIN), available });
    }
    let need = SpaceNeed { dir: dir.to_path_buf(), bytes };
    blocked_unless_empty(preflight::check_space_with(&[need], SPACE_MARGIN, free))
}
