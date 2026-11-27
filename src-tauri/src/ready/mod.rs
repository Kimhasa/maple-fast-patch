//! The install folder: the look at it that the ready screen is made from, with the checks
//! made before a patch may start, the folder the user picks, and what an unfinished run left
//! behind. Also what the desk has done for a look to be whole: a folder put back after an
//! apply that was cut off in it, and a patch file on this PC read for the room its run needs.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mfp_engine::apply::Leftover;
use mfp_engine::container::PatchFile;
use mfp_engine::preflight::{self, Problem, SpaceNeed};
use mfp_engine::run::Room;
use mfp_engine::storage::{self, Volume};
use mfp_engine::{Error, download, plan, stage};
use tauri::{AppHandle, State};

use crate::address::take;
use crate::desk::{look_now, update};
use crate::log::meet_install;
use crate::offer::full_label;
use crate::patching::taskbar_progress;
use crate::records::{
    Left, choose_dir, chosen_dir, forget_left_run, forget_version, found_unproven, install_dir, installed_version,
    kept_as, known_minor, left_record, patched_before, to_apply, unsettled,
};
use crate::state::{Check, CheckKind, Patcher, Site, Status, Target};
use crate::window::pick_folder;
use crate::wording::{
    self, NO_FOLDER, describe_error, describe_problem, drive_letter, gib, no_folder_of, not_the_games_folder, sentence,
    size_label,
};

/// What there is to see of the install folder of `site`: the folder, the checks made before
/// a patch may start there, and what an unfinished run left. `running` says that the site's
/// run is at work, which has the folder to itself, and `immediate` which way of patching
/// the user chose: the room a run needs is judged by it.
///
/// Whoever wants to know asks [`look_now`], which also keeps what is found.
pub(crate) fn look(site: &Site, running: bool, immediate: bool) -> Status {
    look_by(site, running, immediate, &room_of)
}

/// The look behind [`look`], with what a run of a patch file on this PC asks the drive to
/// have free told by `room_of`; see [`room_of`]. A test has no patch file to read that from,
/// and tells it in its place.
fn look_by(
    site: &Site,
    running: bool,
    immediate: bool,
    room_of: &dyn Fn(&Site, &Path, &Path) -> Option<Room>,
) -> Status {
    let install_dir = install_dir(site);
    // Whatever line of the log comes to have this folder in it names it as what it is.
    if let Some(dir) = &install_dir {
        meet_install(dir);
    }
    // What the game is called, for the sentences that name it.
    let game = site.client.title;
    let mut resumable = false;
    let mut left: Option<(Option<String>, f64)> = None;
    // A folder that is not a game folder has no version. Nor, yet, has a copy whose files
    // have not been looked at: the registry's version is that of another folder. Nor has a
    // folder of which a check has found that nothing proves its version.
    let game_dir = install_dir.as_deref().filter(|dir| site.programs().check_install_dir(dir).is_empty());
    // Nor has a folder that a run left partly new: it is at neither version until that run
    // is finished.
    let half_patched = install_dir.as_deref().is_some_and(mfp_engine::run::half_patched);
    // A run was cut off while applying, and the folder is still to be put back as it was:
    // until it is, it is neither version, and nothing is said of it.
    let cut_off = !running && install_dir.as_deref().is_some_and(mfp_engine::run::cut_off);
    // The record that run kept so that the apply could be undone cannot be read, and the
    // folder could not be put back without it either. It is never undone, and nothing
    // goes on over it: that is all there is to say of the folder until what the run left
    // has been removed. Such a folder may be a mix of two versions.
    let unreadable = install_dir.as_deref().filter(|_| !running).and_then(unreadable_journal);
    let whole = !half_patched && !cut_off && unreadable.is_none();
    let said = |dir: &&Path| whole && unsettled(site, dir).is_none() && !found_unproven(site, dir);
    let at = game_dir.filter(said).and_then(|dir| Some((dir, installed_version(site, dir)?)));
    let version =
        at.map(|(dir, version)| full_label(site.client, version, known_minor(site, dir, version).unwrap_or(1)));
    let next_address = at.map(|(_, version)| site.patch_url(version, version + 1));
    // Putting the folder back is not done here: it can take as long as a check of the
    // game files, and is begun by the desk; see [`heal_begins`]. What is seen here is how
    // that stands. While the last attempt has failed (a file is held open, say) the folder
    // is shown as not ready, and until one has been made, or while one is at work, as
    // being put back.
    let can_be_put_back = cut_off && unreadable.is_none();
    let not_restored = {
        let mut heal = site.heal.lock().unwrap();
        // What could not be read of the record, and nothing of where it is.
        if let (Some(Problem::UnreadableJournal { detail, .. }), false) = (&unreadable, heal.logged) {
            site.log(&format!("could not undo an interrupted apply: its record cannot be read ({detail})"));
            heal.logged = true;
        }
        heal.failed.as_ref().filter(|_| can_be_put_back).map(|(_, said)| said.clone())
    };
    let putting_back = can_be_put_back && not_restored.is_none();
    let way = Way { chosen: immediate, half_patched };
    let mut unread_patch = None;
    let mut room_needed = None;
    let mut checks = match &install_dir {
        None => vec![match chosen_dir(site) {
            Some(dir) => Check::failed(
                CheckKind::Folder,
                format!("{} · 찾을 수 없음", dir.display()),
                "선택한 설치 경로를 찾을 수 없습니다.",
            ),
            None => Check::failed(CheckKind::Folder, "찾을 수 없음", no_folder_of(game)),
        }],
        Some(dir) => {
            let target = to_apply(site, dir, half_patched);
            // A download that was paused can be gone on with as well. The engine does not
            // count patch files in the work folder as something a run left, so it is looked
            // for here.
            let part_of = |name: &str| download::part_path(&stage::Dirs::new(dir).download().join(name)).exists();
            // What the run that was left wrote down of itself. Of a patch the user gave the
            // address of, that is the address, and it is held against the rule for such
            // addresses like one that has just been given: a record is a file like any other.
            let on_record = left_record(site, dir);
            let given = on_record
                .as_ref()
                .map(|left| left.patch.as_str())
                .filter(|address| take(address, &site.patch_files()).is_ok_and(|taken| taken.url == *address));
            let partial_download = target.as_ref().is_some_and(|target| part_of(&target.file_name))
                || given.is_some_and(|address| part_of(&kept_as(site, address)));
            // Leftovers are not a problem: the next run restores or reuses them.
            // Not while the folder is still to be put back: nothing goes on before that.
            resumable = !running && !cut_off && (work_left(dir) || partial_download || half_patched);
            // What that run was applying and how far it had come, for as long as the files it
            // built are there: the rule the run itself goes by when it is continued. A patch
            // the app found has to be the one a check finds now.
            if resumable
                && dir.join(preflight::WORK_DIR_NAME).is_dir()
                && let Some(Left { patch, percent, .. }) = &on_record
            {
                if given.is_some() {
                    left = Some((Some(patch.clone()), *percent));
                } else if target.as_ref().is_some_and(|target| target.file_name == *patch) {
                    left = Some((None, *percent));
                }
            }
            if game_dir.is_some() {
                vec![
                    Check::passed(CheckKind::Folder, dir.display().to_string()),
                    running_check(game, site.programs().check_running(dir)),
                    Check::of(game, CheckKind::Writable, "", site.writable(dir)).quiet(),
                    {
                        // Once all of the patch file is here, what a run of it needs can be
                        // told, as soon as what the file does has been read.
                        let on_pc = target.as_ref().and_then(|target| patch_on_pc(dir, target));
                        let room = on_pc.as_ref().and_then(|file| room_of(site, dir, file));
                        unread_patch = on_pc.filter(|file| !running && room.is_none() && !is_read(site, file));
                        room_needed = room.as_ref().map(|room| (room.safe, room.immediate));
                        space_check(game, dir, target.as_ref(), room, way)
                    },
                ]
            } else {
                // Not this client's game folder: one without the game, or the other client's.
                // The other checks would be about a folder that has nothing to do with the
                // game (and one of them writes a test file there).
                let detail = format!("{} · 설치 경로 아님", dir.display());
                vec![Check::failed(CheckKind::Folder, detail, not_the_games_folder(game))]
            }
        }
    };
    // The immediate way chosen on a drive that does not take it: no run begins there that
    // way, however much room there is. That is a line of its own, and takes nothing from
    // the line for the room.
    if let Some(dir) = game_dir.filter(|_| way.chosen && !way.half_patched)
        && let Some(refused) = drive_refuses(&storage::look(dir, None), way)
    {
        checks.push(Check::of(game, CheckKind::Way, "", vec![refused]));
    }
    if let Some(problem) = &unreadable {
        checks.push(Check::failed(CheckKind::Recovery, describe_problem(problem), sentence(game, problem)));
    } else if let Some(said) = not_restored {
        checks.push(Check::failed(CheckKind::Recovery, "실패", said));
    }
    let leftover = if running { None } else { install_dir.as_deref().and_then(leftover_bytes).map(size_label) };
    let (resume_address, resume_percent) = match left {
        Some((file, percent)) => (file, Some(percent)),
        None => (None, None),
    };
    // A client this app was never tried on is patched at the user's press the first time.
    let by_press_only = site.untried() && install_dir.as_deref().is_some_and(|dir| !patched_before(site, dir));
    Status {
        install_dir: install_dir.map(|dir| dir.display().to_string()),
        version,
        next_address,
        custom_dir: chosen_dir(site).is_some(),
        leftover,
        checks,
        resumable,
        resume_address,
        resume_percent,
        half_patched,
        unreadable_journal: unreadable.is_some(),
        putting_back,
        unread_patch: unread_patch.map(|file| file.display().to_string()),
        by_press_only,
        room: room_needed,
    }
}

/// How long after an attempt to put a folder back has failed the next one is made: the pace
/// of the checks. Without the record of the apply each attempt reads the patch file again.
const HEAL_AGAIN: Duration = Duration::from_secs(30);

/// Whether an attempt to put a folder back is to begin: none is `at_work`, and the one
/// before it, if it failed, did so long enough ago (`failed` is how long).
fn heal_due(at_work: bool, failed: Option<Duration>) -> bool {
    !at_work && failed.is_none_or(|ago| ago >= HEAL_AGAIN)
}

/// Takes the turn to put `install` back, where an apply was cut off in it and an attempt is
/// due at `now`. Says whether it was taken: whoever takes it calls [`put_back`].
///
/// `running` says that the site's run is at work, which has the folder to itself and puts
/// it back as it begins. A folder that is known not to come back is not tried again; see
/// [`unreadable_journal`].
pub(crate) fn heal_begins(site: &Site, install: &Path, running: bool, now: Instant) -> bool {
    if running || !mfp_engine::run::cut_off(install) || unreadable_journal(install).is_some() {
        return false;
    }
    let mut heal = site.heal.lock().unwrap();
    let due = heal_due(heal.at_work, heal.failed.as_ref().map(|(at, _)| now.saturating_duration_since(*at)));
    heal.at_work |= due;
    due
}

/// Puts `install` back as it was before the apply that was cut off in it, and keeps for
/// the looks how that went. From the record of the apply this is a matter of a moment.
/// Where that record cannot be read the engine puts the folder back from what the apply
/// had moved aside, and holds it against the patch file, which takes as long as a check
/// of the game files: whoever calls this does so on a thread that may take that long.
pub(crate) fn put_back(site: &Site, install: &Path) {
    heal_ended(site, install, mfp_engine::run::recover(install));
}

/// Forgets what the app had on record of `install`: its version, its minor, and what the
/// last check found. For a folder that was put back after an apply that was cut off
/// without being vouched for as the version before the patch, by the app or by a run.
pub(crate) fn forget_what_was_known(site: &Site, install: &Path) {
    forget_version(site, install);
    site.forget_found();
    site.log("put the folder back after an interrupted apply; what it holds is worked out anew");
}

/// Takes in how an attempt to put `install` back ended.
///
/// A folder that is back as it was needs nothing said or done: what the app knew of it
/// holds again. One that is back but not vouched for as the version before the patch may
/// hold files of both versions. What the app had on record of it (its version, its minor,
/// what the last check found) was known of other files, and is forgotten: the next check
/// works the version out from the files, as for a folder nothing is known of, and the next
/// run holds the folder against its patch as every run does.
fn heal_ended(site: &Site, install: &Path, outcome: Result<Leftover, Error>) {
    match &outcome {
        Ok(Leftover::PutBack) => forget_what_was_known(site, install),
        Ok(Leftover::Restored) => site.log("restored the folder after an interrupted apply, without its record"),
        _ => {}
    }
    let mut heal = site.heal.lock().unwrap();
    heal.at_work = false;
    heal.failed = match outcome {
        Ok(_) => {
            heal.logged = false;
            None
        }
        Err(err) => {
            if !std::mem::replace(&mut heal.logged, true) {
                site.log(&format!("could not undo an interrupted apply: {err}"));
            }
            Some((Instant::now(), wording::not_restored(site.client.title, &err)))
        }
    };
}

/// Whether a run left work in `install` that a run can go on with. The work of an apply that
/// cannot be undone (see [`unreadable_journal`]) is none of that: nothing goes on over it.
pub(crate) fn work_left(install: &Path) -> bool {
    let left = preflight::check_leftovers(install);
    !left.is_empty() && !left.iter().any(|problem| matches!(problem, Problem::UnreadableJournal { .. }))
}

/// What is wrong with the record of an apply that was cut off in `install`, when that
/// record cannot be read and the folder cannot be put back without it either: the apply is
/// never undone. A record that cannot be read alone is not that; see [`put_back`]. Only
/// looks.
pub(crate) fn unreadable_journal(install: &Path) -> Option<Problem> {
    preflight::check_leftovers(install).into_iter().find(|problem| matches!(problem, Problem::UnreadableJournal { .. }))
}

/// The line for the game being open. It only ever shows when it is, and then asks for the
/// game to be closed; see [`Check::game_open`]. Which programs were found is not said.
fn running_check(game: &str, problems: Vec<Problem>) -> Check {
    match problems.iter().any(|problem| matches!(problem, Problem::GameRunning { .. })) {
        true => Check::game_open(game),
        false => Check::passed(CheckKind::Running, "").quiet(),
    }
}

/// The way of patching a run in a folder goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Way {
    /// The user chose the immediate way: every run replaces game files as it builds them.
    pub(crate) chosen: bool,
    /// A run left the folder partly new. It goes on the immediate way whatever is chosen,
    /// and on any drive: it was begun so.
    pub(crate) half_patched: bool,
}

/// Why no run begins on `volume` by the way that was chosen: the immediate way is not
/// begun on a drive that is not NTFS. The way out is the choice itself.
fn drive_refuses(volume: &Volume, way: Way) -> Option<Problem> {
    let refused = way.chosen && !way.half_patched && !volume.takes_immediate();
    refused.then(|| Problem::ImmediateNeedsNtfs { file_system: volume.file_system.clone() })
}

/// What a run is short of on the drive `letter` with `free` bytes free, when `room` is what
/// a run of its patch asks for either way: the figure of the way the run goes.
fn short_of_room(letter: &str, free: u64, room: Room, way: Way) -> Vec<Problem> {
    let needed = room.of_run(way.chosen, way.half_patched);
    if free >= needed {
        return Vec::new();
    }
    vec![Problem::NotEnoughSpace { volume: letter.to_owned(), needed, available: free }]
}

/// The patch file a run of `target` in `dir` applies, when all of it is on this PC: in
/// the place a run keeps it while it is at it.
fn patch_on_pc(dir: &Path, target: &Target) -> Option<PathBuf> {
    let dest = stage::Dirs::new(dir).download().join(&target.file_name);
    let whole = std::fs::metadata(&dest).is_ok_and(|meta| meta.is_file() && meta.len() == target.size);
    (whole && !download::part_path(&dest).exists()).then_some(dest)
}

/// Whether what the patch file `file` does has been read, or was found unreadable.
fn is_read(site: &Site, file: &Path) -> bool {
    let len = std::fs::metadata(file).map_or(0, |meta| meta.len());
    site.read.lock().unwrap().read.as_ref().is_some_and(|(of, long, _)| of == file && *long == len)
}

/// What a run of the patch file `file` asks the drive of `dir` to have free, either way.
/// Nothing until what the file does has been read; see [`read_patch`]. Only the sizes of
/// files are looked at here, so this is asked at every look.
fn room_of(site: &Site, dir: &Path, file: &Path) -> Option<Room> {
    let len = std::fs::metadata(file).ok()?.len();
    let read = site.read.lock().unwrap();
    let (_, _, plan) = read.read.as_ref().filter(|(of, long, _)| of == file && *long == len)?;
    let patch = PatchFile::open(file).ok()?;
    Some(mfp_engine::run::room_needed(&patch, plan.as_ref()?, dir))
}

/// Takes the turn to read what the patch file `file` does. Says whether it was taken:
/// whoever takes it calls [`read_patch`]. One file is read at a time, and none while a
/// check of the site was told to give up: a run is beginning, or the folder was left.
pub(crate) fn read_begins(site: &Site, file: &Path) -> bool {
    let mut read = site.read.lock().unwrap();
    if read.reading.is_some() || site.check_stop.load(Ordering::SeqCst) {
        return false;
    }
    read.reading = Some(file.to_path_buf());
    true
}

/// Reads what the patch file `file` does and keeps it for the looks, which tell from it
/// what a run of the patch asks the drive to have free. The whole file is read once, which
/// takes a while for a large one: whoever calls this does so on a thread that may. What
/// makes a check give up stops this as well, and the file is then read another time.
pub(crate) fn read_patch(site: &Site, file: &Path) {
    let len = std::fs::metadata(file).map_or(0, |meta| meta.len());
    let plan = PatchFile::open(file).and_then(|patch| plan::scan(&patch, &site.check_stop));
    let mut read = site.read.lock().unwrap();
    read.reading = None;
    if !matches!(plan, Err(Error::Stopped)) {
        read.read = Some((file.to_path_buf(), len, plan.ok()));
    }
}

/// Compares free space with what is known to be needed so far, by the way of patching the
/// run goes (`way`).
///
/// Before the patch file is on this PC, only its own size is known, so this checks that the
/// download fits. Once it is here and has been read, `room` is what a run of it asks for
/// either way, and the line says whether the drive has that: the figure of the way that is
/// chosen, so that choosing the other way changes the line. The run works the same out
/// again, before anything is built.
fn space_check(game: &str, dir: &Path, target: Option<&Target>, room: Option<Room>, way: Way) -> Check {
    let Ok(free) = preflight::free_space(dir) else {
        return Check::failed(CheckKind::Space, "확인 불가", "디스크 여유 공간을 확인할 수 없습니다.");
    };
    if let Some(room) = room {
        let letter = drive_letter(dir).unwrap_or_default();
        return Check::of(game, CheckKind::Space, gib(free), short_of_room(&letter, free, room, way));
    }
    // The download goes into the install folder, so the game's drive is the only one that
    // has to have room. What is already downloaded needs no more of it.
    let to_download = target.map_or(0, |target| {
        let dest = stage::Dirs::new(dir).download().join(&target.file_name);
        let size = |path: &Path| std::fs::metadata(path).map_or(0, |meta| meta.len());
        target.size.saturating_sub(size(&dest).max(size(&download::part_path(&dest))))
    });
    let short = match to_download {
        0 => Vec::new(),
        bytes => preflight::check_space(&[SpaceNeed { dir: dir.to_path_buf(), bytes }]),
    };
    Check::of(game, CheckKind::Space, gib(free), short)
}

/// What is said to whoever asks for the folder to be changed, or for what a run left in it to
/// be removed, while the site's run is at work there.
const RUN_AT_WORK: &str = "패치가 진행 중입니다.";

/// Lets the user pick the install folder of the client the page calls `server`, and uses
/// the folder they pick. Nothing changes when they pick none.
#[tauri::command(async)]
pub(crate) fn pick_install_dir(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or(NO_FOLDER)?;
    if patcher.running_for(site) {
        return Err(RUN_AT_WORK.into());
    }
    match pick_folder(&app) {
        Some(folder) => use_install_dir(&app, &patcher, site, Some(&folder.to_string_lossy())),
        None => Ok(()),
    }
}

/// Goes back from a folder the user picked to the one the Nexon installer recorded, for the
/// client the page calls `server`.
#[tauri::command(async)]
pub(crate) fn reset_install_dir(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or(NO_FOLDER)?;
    use_install_dir(&app, &patcher, site, None)
}

/// Uses `folder` as the install folder of `site`. `None` goes back to the one the Nexon
/// installer recorded.
fn use_install_dir(app: &AppHandle, patcher: &Patcher, site: &Site, folder: Option<&str>) -> Result<(), String> {
    if patcher.running_for(site) {
        return Err(RUN_AT_WORK.into());
    }
    choose_dir(site, folder).map_err(|_| "설치 경로를 저장하지 못했습니다.".to_owned())?;
    // An address the user was being asked about was given for the folder before.
    site.asked_about.lock().unwrap().take();
    // A run that was paused in the folder that was left has nothing to show for this one.
    taskbar_progress(app, None);
    // The folder is looked at before this returns, so that whoever asked finds the new one.
    // Another folder may be at another version: the look puts aside what was found for the
    // one before, and the server is asked about this one at once.
    look_now(app, patcher, site);
    Ok(())
}

/// Bytes in the work folder a paused or interrupted run left in the install folder.
fn leftover_bytes(install: &Path) -> Option<u64> {
    fn size(dir: &Path) -> u64 {
        let Ok(children) = std::fs::read_dir(dir) else { return 0 };
        children
            .flatten()
            .map(|child| match child.metadata() {
                Ok(meta) if meta.is_dir() => size(&child.path()),
                Ok(meta) => meta.len(),
                Err(_) => 0,
            })
            .sum()
    }
    let work = install.join(preflight::WORK_DIR_NAME);
    work.is_dir().then(|| size(&work))
}

/// How what a run left in an install folder is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Discard {
    /// An apply that was cut off is undone first, so that the folder is as it was before
    /// that run. Then what the run built and downloaded goes.
    Undoing,
    /// It goes as it is, with the record of an apply that nobody can read: nothing is
    /// undone, and the folder stays what the apply left.
    AsItIs,
}

/// How what a run left is removed from a folder, or `None` when it is not removed: the
/// user has to be asked first, and `confirmed` does not say that they were.
///
/// They are asked where this app cannot make good what goes: in a folder that a run left
/// partly new (`half_patched`), and in one where an apply was cut off and cannot be undone
/// (`unreadable`; see [`unreadable_journal`]). Only that last folder is one whose work is
/// removed as it is. Which folder it is, is looked up here. The page says that the user
/// confirmed, and has no say in which way the work goes.
fn way_to_discard(half_patched: bool, unreadable: bool, confirmed: bool) -> Option<Discard> {
    let asked = confirmed || !(half_patched || unreadable);
    asked.then_some(if unreadable { Discard::AsItIs } else { Discard::Undoing })
}

/// Deletes what a paused or interrupted run built and downloaded, for a run that will not
/// be continued. A run that was cut off while applying is undone first. A run that replaced
/// game files as it built them cannot be undone, and neither can an apply whose record
/// cannot be read where the folder could not be put back without it: the folder stays
/// partly new, and the screen asks before it comes to this. `confirmed` says that it has
/// asked.
#[tauri::command(async)]
pub(crate) fn discard_leftovers(
    app: AppHandle,
    patcher: State<Patcher>,
    server: String,
    confirmed: bool,
) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or(NO_FOLDER)?;
    let install = install_dir(site).ok_or(NO_FOLDER)?;
    if patcher.running_for(site) {
        return Err(RUN_AT_WORK.into());
    }
    // A check at work on these files gives up first.
    site.check_stop.store(true, Ordering::SeqCst);
    let _busy = site.busy.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // A run may have begun while this waited.
    if patcher.running_for(site) {
        return Err(RUN_AT_WORK.into());
    }
    let half_patched = mfp_engine::run::half_patched(&install);
    let unreadable = unreadable_journal(&install).is_some();
    let discarded = match way_to_discard(half_patched, unreadable, confirmed) {
        Some(Discard::Undoing) => mfp_engine::run::discard_work(&install),
        Some(Discard::AsItIs) => mfp_engine::run::discard_work_as_it_is(&install),
        None => return Err("삭제 후에는 공식 런처로 게임을 복구해야 합니다.".into()),
    };
    discarded.map_err(|err| describe_error(site.client.title, &err).0)?;
    // With its work gone, the run it belonged to starts from the beginning, and so does
    // the measuring of it. The taskbar has no paused run to show any more.
    forget_left_run(site, &install);
    *site.measure.lock().unwrap() = None;
    taskbar_progress(&app, None);
    // What was offered for a half-patched folder was the patch to finish. With that given
    // up, the folder is looked at afresh. It is a mix of two versions from here on, and so
    // is a folder whose apply could not be undone: what was known of the version of either
    // was known of other files than it now holds.
    let left_a_mix = half_patched || unreadable;
    if left_a_mix {
        site.forget_found();
        forget_version(site, &install);
    }
    let how = if unreadable { ", as it was: the record of its apply could not be read" } else { "" };
    site.log(&format!("discarded the work folder in {}{how}", install.display()));
    // The server is asked at once, and the folder looked at before this returns.
    update(&app, &patcher, site, |flow| flow.leftovers_discarded(left_a_mix, Instant::now()));
    look_now(&app, &patcher, site);
    Ok(())
}
