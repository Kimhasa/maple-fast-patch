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
use crate::storage::{self, Profile, Volume};

/// The steps of a run, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Download,
    VerifyPatch,
    VerifyInstall,
    /// Only in the immediate way, and only before its first game file is replaced: every
    /// new file is built once and kept nowhere, to see that the patch can be built from the
    /// files that are there.
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
    /// An earlier run had stopped while applying; the install folder was restored first.
    Restored,
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
    /// Whether the player allows the immediate way (see [`immediate`]), in which new files
    /// replace the originals while others are still being built and a run that stops partway
    /// cannot be undone. Allowing it does not choose it: with room for the safe way, the
    /// safe way is taken, and it is not begun on a drive that is not NTFS. An install folder
    /// that is already half patched goes on the immediate way whatever this says.
    pub immediate: bool,
    /// Who decides how the drive is read.
    pub storage: storage::Choice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Files placed, replaced or removed. Zero means the install was already up to date.
    pub files_changed: usize,
    /// Whether this run went the immediate way: it moved new files into place as they were
    /// built. False for a run that went the safe way from start to finish.
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
pub fn recover(install_dir: &Path) -> Result<Leftover> {
    let dirs = Dirs::new(install_dir);
    if !dirs.work().exists() {
        return Ok(Leftover::None);
    }
    let _lock = InstallLock::acquire(&dirs)?;
    apply::recover(&dirs)
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
pub fn discard_work(install_dir: &Path) -> Result<()> {
    let dirs = Dirs::new(install_dir);
    if !dirs.work().exists() {
        return Ok(());
    }
    let _lock = InstallLock::acquire(&dirs)?;
    apply::recover(&dirs)?;
    stage::remove_work(&dirs.work())
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
    if apply::recover(&dirs)? == Leftover::RolledBack {
        on_event(Event::Restored);
    }
    blocked_unless_empty(programs.check_install_dir(&dirs.install))?;

    // What was built for a half-patched install folder exists nowhere else, so nothing but
    // the patch that began there may work on it. That one goes on the immediate way: the
    // safe way has no promise left to keep, and needs the room that was not there.
    let pinned = stage::half_patched(&dirs);
    let (patch, plan) = match (&pinned, &options.source) {
        (Some(id), Source::Url { url, dest }) => obtain_pinned(id, url, dest, &dirs, disk.free, stop, on_event)?,
        _ => obtain_patch(&options.source, disk.free, stop, on_event)?,
    };
    if pinned.as_ref().is_some_and(|id| *id != patch.id()) {
        return Err(Error::Blocked(vec![Problem::LeftoverWork { dir: dirs.work() }]));
    }
    let half_patched = pinned.is_some();
    blocked_unless_empty(preflight::check_inside_install(&dirs.install, &apply::target_dirs(&plan, &dirs)))?;
    if !options.immediate && !half_patched {
        require_room_for_certain(&patch, &plan, &dirs, disk.free, on_event)?;
    }

    let originals = stage::originals_to_check(&plan, &dirs.install);
    let (volume, profile) = look_at_drive(options.storage, &originals, &dirs, disk.file_system, on_event);

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

    // With room for the safe way, the safe way it is: it can be undone and costs nothing
    // more. The immediate way is for when that room is not there and the player allowed it.
    let work = dirs.work();
    let to_build = stage::bytes_to_build(&plan, &checked, &dirs);
    let safe_fits = to_build == 0 || {
        let need = SpaceNeed { dir: work.clone(), bytes: to_build };
        preflight::check_space_with(&[need], SPACE_MARGIN, disk.free).is_empty()
    };
    // It begins only where a file moved over another is known to survive a power cut whole.
    // A folder that is half patched already goes on wherever it is: stopping there helps
    // nobody.
    let allowed = options.immediate && !safe_fits;
    let immediate = half_patched || (allowed && volume.takes_immediate());

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
        require_space(&work, to_build, disk.free, on_event).map_err(|err| match err {
            // The way that needs less room was allowed and not taken. The answer says why.
            Error::Blocked(mut problems) if allowed => {
                problems.push(Problem::ImmediateNeedsNtfs { file_system: volume.file_system.clone() });
                Error::Blocked(problems)
            }
            other => other,
        })?;
        build_all(&profile, &patch, &plan, &checked, &dirs, stop, on_event)?;
        (checked, 0)
    };

    let applied = apply_built(programs, &plan, &checked, &dirs, stop, on_event)?;
    Ok(Outcome { files_changed: placed_early + applied, immediate })
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

/// Stops the run, before any game file is read, when it is already certain that the new
/// files cannot fit. Checking the game files takes minutes on a hard disk, and the answer
/// after it would be the same. Says nothing when they may fit: the exact check follows.
fn require_room_for_certain(
    patch: &PatchFile,
    plan: &Plan,
    dirs: &Dirs,
    free: &dyn Fn(&Path) -> io::Result<u64>,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<()> {
    let work = dirs.work();
    let ours = stage::staged_is_ours(patch, dirs);
    // Work of another patch is thrown away before the exact check. How much room that
    // gives back is not known here, so nothing is certain.
    if !ours && preflight::holds_work(&work) {
        return Ok(());
    }
    let bytes = stage::bytes_certain_to_build(plan, dirs, ours);
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
) -> (Volume, Profile) {
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
    (volume, profile)
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
