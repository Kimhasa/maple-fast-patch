//! The install folder: the look at it that the ready screen is made from, with the checks
//! made before a patch may start, the folder the user picks, and what an unfinished run left
//! behind.

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
    self, describe_error, describe_problem, drive_letter, gib, no_folder_of, not_the_games_folder, sentence, size_label,
};

/// What there is to see of the install folder of `site`: the folder, the checks made before
/// a patch may start there, and what an unfinished run left. `running` says that the site's
/// run is at work, which has the folder to itself, and `immediate` which way of patching
/// the user chose: the room a run needs is judged by it.
///
/// Whoever wants to know asks [`look_now`], which also keeps what is found.
pub(crate) fn look(site: &Site, running: bool, immediate: bool) -> Status {
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
            if site.programs().check_install_dir(dir).is_empty() {
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
    }
}

/// How long after an attempt to put a folder back has failed the next one is made: the pace
/// of the checks. Without the record of the apply each attempt reads the patch file again.
const HEAL_AGAIN: Duration = Duration::from_secs(30);

/// Whether an attempt to put a folder back is to begin: none is `at_work`, and the one
/// before it, if it failed, did so long enough ago (`failed` is how long).
pub(crate) fn heal_due(at_work: bool, failed: Option<Duration>) -> bool {
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

/// Whether a run left work in `install` that a run can go on with. Work whose record of an
/// apply cannot be read is none of that: nothing goes on over it.
pub(crate) fn work_left(install: &Path) -> bool {
    let left = preflight::check_leftovers(install);
    !left.is_empty() && !left.iter().any(|problem| matches!(problem, Problem::UnreadableJournal { .. }))
}

/// What is wrong with the record of an apply that was cut off in `install`, when that
/// record cannot be read. Only looks.
pub(crate) fn unreadable_journal(install: &Path) -> Option<Problem> {
    preflight::check_leftovers(install).into_iter().find(|problem| matches!(problem, Problem::UnreadableJournal { .. }))
}

/// The line for the game being open. It only ever shows when it is, so it says so, with the
/// programs found.
fn running_check(game: &str, problems: Vec<Problem>) -> Check {
    let running = problems.into_iter().find(|problem| matches!(problem, Problem::GameRunning { .. }));
    match running {
        Some(Problem::GameRunning { names }) => {
            let message = sentence(game, &Problem::GameRunning { names: names.clone() });
            Check::failed(CheckKind::Running, names.join(", "), message).quiet()
        }
        _ => Check::passed(CheckKind::Running, "").quiet(),
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
    #[cfg(test)]
    if let Some(made) = *site.made_room.lock().unwrap() {
        return Some(made);
    }
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

/// Lets the user pick the install folder of the client the page calls `server`, and uses
/// the folder they pick. Nothing changes when they pick none.
#[tauri::command(async)]
pub(crate) fn pick_install_dir(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or("설치 경로를 찾을 수 없습니다.")?;
    if patcher.running_for(site) {
        return Err("패치가 진행 중입니다.".into());
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
    let site = patcher.site_named(&server).ok_or("설치 경로를 찾을 수 없습니다.")?;
    use_install_dir(&app, &patcher, site, None)
}

/// Uses `folder` as the install folder of `site`. `None` goes back to the one the Nexon
/// installer recorded.
fn use_install_dir(app: &AppHandle, patcher: &Patcher, site: &Site, folder: Option<&str>) -> Result<(), String> {
    if patcher.running_for(site) {
        return Err("패치가 진행 중입니다.".into());
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
/// partly new (`half_patched`), and in one where an apply was cut off and its record
/// cannot be read (`unreadable`). Only that last folder is one whose work is removed as
/// it is. Which folder it is, is looked up here. The page says that the user confirmed,
/// and has no say in which way the work goes.
fn way_to_discard(half_patched: bool, unreadable: bool, confirmed: bool) -> Option<Discard> {
    let asked = confirmed || !(half_patched || unreadable);
    asked.then_some(if unreadable { Discard::AsItIs } else { Discard::Undoing })
}

/// Deletes what a paused or interrupted run built and downloaded, for a run that will not
/// be continued. A run that was cut off while applying is undone first. A run that replaced
/// game files as it built them cannot be undone, and neither can an apply whose record
/// cannot be read: the folder stays partly new, and the screen asks before it comes to
/// this. `confirmed` says that it has asked.
#[tauri::command(async)]
pub(crate) fn discard_leftovers(
    app: AppHandle,
    patcher: State<Patcher>,
    server: String,
    confirmed: bool,
) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or("설치 경로를 찾을 수 없습니다.")?;
    let install = install_dir(site).ok_or("설치 경로를 찾을 수 없습니다.")?;
    if patcher.running_for(site) {
        return Err("패치가 진행 중입니다.".into());
    }
    // A check at work on these files gives up first.
    site.check_stop.store(true, Ordering::SeqCst);
    let _busy = site.busy.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // A run may have begun while this waited.
    if patcher.running_for(site) {
        return Err("패치가 진행 중입니다.".into());
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::flow::{Decision, Flow, RefusalKind, World, decide, pick};
    use crate::harness::StandIns;
    use crate::offer::{CheckEnd, Checked};
    use crate::records::{record_minor, record_version, version_proven};
    use crate::scratch::Scratch;
    use crate::state::{Availability, Found};

    /// A site of `client` whose user has picked a folder that holds the program `program`
    /// and the game's data folder.
    fn picked(client: &'static mfp_kms::Client, program: &str, scratch: &Scratch) -> (Site, PathBuf) {
        let install = scratch.0.join("game");
        std::fs::create_dir_all(install.join("Data")).unwrap();
        std::fs::write(install.join(program), b"the game").unwrap();
        let site = Site::new(client, scratch.0.join("data"), StandIns::default());
        choose_dir(&site, Some(&install.to_string_lossy())).unwrap();
        (site, install)
    }

    /// A folder is a client's by the game program in it. The other client's folder is
    /// turned down like any folder without the game, in the words that name this client.
    #[test]
    fn a_folder_of_the_other_client_is_not_this_clients_install_folder() {
        let (live, test) = (&mfp_kms::LIVE, &mfp_kms::TEST);
        for (client, others_program, message) in [
            (test, live.game_program, "메이플스토리 테스트월드 설치 경로가 아닙니다."),
            (live, test.game_program, "메이플스토리 설치 경로가 아닙니다."),
        ] {
            let scratch = Scratch::new(&format!("others-folder-{}", client.server.key()));
            let (site, install) = picked(client, others_program, &scratch);
            let status = look(&site, false, false);
            assert!(!status.game_folder() && status.version.is_none());
            // The folder is the one line there is, and nothing else was looked at in it.
            assert_eq!(status.checks.len(), 1);
            let folder = &status.checks[0];
            assert_eq!((folder.kind, folder.ok), (CheckKind::Folder, false));
            assert_eq!(folder.detail, format!("{} · 설치 경로 아님", install.display()));
            assert_eq!(folder.message, message);
        }
    }

    #[test]
    fn a_game_that_is_not_installed_is_said_to_be_not_found_by_its_own_name() {
        let scratch = Scratch::new("not-installed");
        // A build for trying the app out never takes a folder where the real games live,
        // so whatever is installed on this PC, nothing is found.
        let site = Site::new(&mfp_kms::TEST, scratch.0.join("data"), StandIns::of_a_session());
        let status = look(&site, false, false);
        assert!(status.install_dir.is_none() && !status.custom_dir && !status.by_press_only);
        let folder = &status.checks[0];
        assert_eq!((folder.detail.as_str(), folder.ok), ("찾을 수 없음", false));
        assert_eq!(folder.message, "메이플스토리 테스트월드 설치 경로를 찾을 수 없습니다.");
        // The look is all that is done for it, and it leaves nothing behind: no folder is
        // made for what the app would keep about a client that is not there.
        assert!(!status.askable() && !scratch.0.join("data").exists());
    }

    /// The test world is the client this app was never tried on: its folder is the user's
    /// to patch until this app has patched it once.
    #[test]
    fn a_test_folder_is_the_users_to_patch_until_this_app_has_patched_it_once() {
        let scratch = Scratch::new("first-press");
        let (site, install) = picked(&mfp_kms::TEST, mfp_kms::TEST.game_program, &scratch);
        let status = look(&site, false, false);
        assert!(status.game_folder());
        // Nothing says which version it is at, and nothing is made up.
        assert_eq!(status.version, None);
        assert!(status.by_press_only);

        record_version(&site, &install, 1208);
        let status = look(&site, false, false);
        assert_eq!(status.version.as_deref(), Some("1.2.208"));
        assert!(!status.by_press_only);

        // The live game's folder was never held back.
        let scratch = Scratch::new("first-press-live");
        let (site, _) = picked(&mfp_kms::LIVE, mfp_kms::LIVE.game_program, &scratch);
        assert!(!look(&site, false, false).by_press_only);
    }

    /// What the registry says of the registered install is shown as its version, as it
    /// always was. Once a check has looked for a proof of that and found none, it is a
    /// guess that was found wanting: the page then says that the version is not known,
    /// and does not show the guess above that.
    #[test]
    fn a_version_that_a_check_could_not_prove_is_not_shown_as_the_folders() {
        let scratch = Scratch::new("unproven-version");
        let install = scratch.0.join("game");
        std::fs::create_dir_all(install.join("Data")).unwrap();
        std::fs::write(install.join(mfp_kms::LIVE.game_program), b"the game").unwrap();
        let site = Site::new(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::registered_at(&install, 900));
        let status = look(&site, false, false);
        assert_eq!(status.version.as_deref(), Some("1.2.900"));
        // Where the patch to the next version would be is what the user is given to begin
        // from when they state an address themselves.
        let next = "https://maplestory.dn.nexoncdn.co.kr/Patch/00901/00900to00901.patch";
        assert_eq!(status.next_address.as_deref(), Some(next));

        let mark = crate::records::install_mark(&site, &install);
        let went_by = crate::state::Unproven { install: install.clone(), mark, newest: 900, newest_minor: 2 };
        *site.unproven.lock().unwrap() = Some(went_by);
        let status = look(&site, false, false);
        assert!(status.game_folder() && status.version.is_none() && status.next_address.is_none());

        // The folder has changed since: what was found of it as it was says nothing now.
        std::fs::write(install.join(mfp_kms::LIVE.game_program), b"the game, patched").unwrap();
        assert_eq!(look(&site, false, false).version.as_deref(), Some("1.2.900"));
    }

    /// A run by an address the user gave writes that address down, and when the run was
    /// left unfinished it is offered again by what was written down. A record is a file
    /// like any other on the PC: the address in it is held against the same rule as one
    /// that has just been given, and whatever else stands there is taken up by nothing.
    #[test]
    fn a_run_by_address_that_was_left_is_taken_up_again_only_by_an_address_a_patch_is_fetched_from() {
        let scratch = Scratch::new("left-by-address");
        let (site, install) = picked(&mfp_kms::LIVE, mfp_kms::LIVE.game_program, &scratch);
        // Part of the patch file the run was fetching is in the work folder.
        let fetching = |name: &str| {
            let part = download::part_path(&stage::Dirs::new(&install).download().join(name));
            std::fs::create_dir_all(part.parent().unwrap()).unwrap();
            std::fs::write(part, b"part of the patch file").unwrap();
        };
        let left_by = |patch: &str| {
            let active = std::time::Duration::from_secs(8);
            let record = Left { patch: patch.to_owned(), percent: 12.5, active, files: 0, pauses: 1 };
            let kept_in = crate::records::records(&site, &install).unwrap();
            std::fs::write(kept_in.join("run-left"), record.written()).unwrap();
            look(&site, false, false)
        };
        fetching("00420to00421.patch");
        fetching("01207to01208.patch");

        // One of the client's own patches, and one of Nexon's that is not: the second was
        // asked about before its run began, and going on with it asks nothing again.
        let own = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
        let nexons = "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch";
        for address in [own, nexons] {
            let status = left_by(address);
            assert!(status.resumable, "{address}");
            assert_eq!((status.resume_address.as_deref(), status.resume_percent), (Some(address), Some(12.5)));
        }

        let not_taken_up = [
            "https://example.com/Patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr@example.com/Patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/../Patch/00421/00420to00421.patch",
            // Not in the spelling this app writes an address in: it writes the one it asks
            // for, which is never one with http.
            "HTTPS://MAPLESTORY.DN.NEXONCDN.CO.KR/Patch/00421/00420to00421.patch",
            "http://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            // What a release that took patch files from the user wrote of one.
            r"D:\patches\00420to00421.patch",
            // The name the file of a patch the app found is kept under, with no such patch
            // found now.
            "00420to00421.patch",
        ];
        for other in not_taken_up {
            let status = left_by(other);
            assert_eq!((status.resume_address, status.resume_percent), (None, None), "{other}");
        }
    }

    const SAFE: Way = Way { chosen: false, half_patched: false };
    const IMMEDIATE: Way = Way { chosen: true, half_patched: false };
    /// A folder that a run left partly new, with the safe way chosen since.
    const PINNED: Way = Way { chosen: false, half_patched: true };

    #[test]
    fn the_room_a_run_needs_is_that_of_the_way_it_goes() {
        const GB: u64 = 1 << 30;
        let room = Room { safe: 40 * GB, immediate: 12 * GB };
        let short = |free: u64, way: Way| match short_of_room("C:", free, room, way).as_slice() {
            [] => None,
            [Problem::NotEnoughSpace { volume, needed, available }] if volume == "C:" && *available == free => {
                Some(*needed)
            }
            other => panic!("{other:?}"),
        };
        // Room for either way.
        assert_eq!((short(50 * GB, SAFE), short(50 * GB, IMMEDIATE)), (None, None));
        // Room for the immediate way only: which way is chosen decides.
        assert_eq!((short(20 * GB, SAFE), short(20 * GB, IMMEDIATE)), (Some(40 * GB), None));
        // Room for neither: each is short of its own figure.
        assert_eq!((short(5 * GB, SAFE), short(5 * GB, IMMEDIATE)), (Some(40 * GB), Some(12 * GB)));
        // A folder that a run left partly new goes on the immediate way whatever is chosen.
        assert_eq!((short(20 * GB, PINNED), short(5 * GB, PINNED)), (None, Some(12 * GB)));
        // Exactly what is asked for is enough, and a patch with nothing to build asks nothing.
        assert_eq!((short(40 * GB, SAFE), short(12 * GB, IMMEDIATE)), (None, None));
        assert!(short_of_room("C:", 0, Room { safe: 0, immediate: 0 }, SAFE).is_empty());
        // The line says it as a run that is turned down for it does.
        let said = sentence("메이플스토리", &short_of_room("C:", 20 * GB, room, SAFE)[0]);
        assert_eq!(said, "디스크 공간이 부족합니다.\nC: 여유 공간을 40.0 GB 이상 확보해 주세요. (현재 20.0 GB)");
    }

    #[test]
    fn the_immediate_way_is_not_begun_on_a_drive_that_is_not_ntfs_and_the_safe_way_is() {
        let drive = |file_system: &str| Volume { file_system: file_system.to_owned(), seek_ms: None, slow: false };
        let refused = |file_system: &str, way: Way| drive_refuses(&drive(file_system), way);
        for way in [SAFE, IMMEDIATE, PINNED] {
            assert!(refused("NTFS", way).is_none() && refused("ntfs", way).is_none(), "{way:?}");
        }
        // A file system that cannot be told is not one that is known to take it.
        for other in ["exFAT", "FAT32", "ReFS", ""] {
            let said = refused(other, IMMEDIATE).expect("refused");
            assert!(matches!(&said, Problem::ImmediateNeedsNtfs { file_system } if file_system == other));
            // It is said on the line of the choice, and is in the way of a start like any
            // check that has not passed.
            let line = Check::of("메이플스토리", CheckKind::Way, "", vec![said]);
            assert_eq!(
                (line.label, line.ok, line.quiet, line.detail.as_str()),
                ("패치 방식", false, false, "즉시 패치 불가")
            );
            assert_eq!(line.message, "즉시 패치는 NTFS 드라이브에서만 사용할 수 있습니다.");
            // The way out is the choice: the safe way is begun there. So is the rest of a
            // patch that left the folder partly new, which was begun the immediate way.
            assert!(refused(other, SAFE).is_none() && refused(other, PINNED).is_none(), "{other}");
        }
    }

    /// A patch whose file is on the PC: paused after its download, or turned down for
    /// room. What its run needs is on the line for the room, by the way that is chosen,
    /// and choosing the other way changes the line, the start and what starts by itself.
    #[test]
    fn the_line_for_the_room_and_every_start_go_by_the_way_that_is_chosen() {
        let scratch = Scratch::new("room-by-way");
        let install = scratch.0.join("game");
        std::fs::create_dir_all(install.join("Data")).unwrap();
        std::fs::write(install.join(mfp_kms::LIVE.game_program), b"the game").unwrap();
        let site = Site::new(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::registered_at(&install, 900));
        let target = Target { size: 1000, ..crate::made::target("1.2.422") };
        *site.found.lock().unwrap() = Some(Found {
            install: install.clone(),
            mark: None,
            version: 421,
            main: true,
            newest_minor: 1,
            target: Some(target.clone()),
            latest: "1.2.422".to_owned(),
            error: None,
        });
        let room_line = |status: &Status| {
            status.checks.iter().find(|check| check.kind == CheckKind::Space).cloned().expect("the line for the room")
        };

        // Before the patch file is here, nothing knows what a run needs: the download has
        // to fit, whichever way is chosen.
        for chosen in [false, true] {
            let status = look(&site, false, chosen);
            assert!(room_line(&status).ok && !status.blocked() && status.unread_patch.is_none(), "{chosen}");
        }

        // All of it is here. What it does is to be read, once, and until then the line is
        // as it was.
        let dest = stage::Dirs::new(&install).download().join(&target.file_name);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, vec![0u8; 1000]).unwrap();
        let status = look(&site, false, false);
        assert!(room_line(&status).ok);
        assert_eq!(status.unread_patch, Some(dest.display().to_string()));
        assert!(read_begins(&site, &dest) && !read_begins(&site, &dest));
        // This one is no patch file. It says nothing of what a run needs, and is not read
        // again for it.
        read_patch(&site, &dest);
        let status = look(&site, false, false);
        assert!(room_line(&status).ok && status.unread_patch.is_none());

        // Read: the safe way needs more than any drive has, the immediate way next to nothing.
        *site.made_room.lock().unwrap() = Some(Room { safe: u64::MAX, immediate: 1 });
        let now = Instant::now();
        let world = World {
            auto_patch: true,
            auto_start: false,
            outdated: false,
            run_at_work: false,
            open: Some(true),
            update_asked: true,
            now,
        };
        // What the app starts by itself with that on the screen, and what a press does.
        let starts = |status: Status| {
            let mut flow = Flow::new(now);
            flow.take_look(status, now);
            let answer = Availability { target: Some(target.clone()), latest: Some("1.2.422".to_owned()), error: None };
            let epoch = flow.folder_epoch;
            assert!(flow.take_answer(epoch, Checked { answer, end: CheckEnd::Answered }, Some(true), now, 0, &|_| 0));
            let facts = flow.facts(&world);
            (decide(&facts), pick(&facts, None, None).map(|_| ()).map_err(|refusal| (refusal.kind, refusal.messages)))
        };
        for (chosen, enough) in [(false, false), (true, true), (false, false), (true, true)] {
            let status = look(&site, false, chosen);
            let line = room_line(&status);
            assert_eq!((line.ok, status.blocked()), (enough, !enough), "{chosen}");
            assert!(status.unread_patch.is_none());
            let (by_itself, pressed) = starts(status);
            if enough {
                assert_eq!((by_itself, pressed), (Decision::StartPatch, Ok(())));
            } else {
                assert!(line.message.starts_with("디스크 공간이 부족합니다.\n"), "{}", line.message);
                assert_eq!((by_itself, pressed), (Decision::Nothing, Err((RefusalKind::Blocked, vec![line.message]))));
            }
        }
    }

    /// A game folder that the registry names, at 1.2.900, in which an apply was cut off:
    /// what stands where its record was is no record.
    fn cut_off_folder(name: &str) -> (Scratch, PathBuf, Site) {
        let scratch = Scratch::new(name);
        let install = scratch.0.join("game");
        std::fs::create_dir_all(install.join("Data")).unwrap();
        std::fs::write(install.join(mfp_kms::LIVE.game_program), b"the game").unwrap();
        let site = Site::new(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::registered_at(&install, 900));
        let journal = stage::Dirs::new(&install).journal();
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(&journal, "this is no line of a journal\n").unwrap();
        (scratch, install, site)
    }

    /// An apply was cut off, the record it kept cannot be read, and the folder cannot be
    /// put back without it: what was moved aside lies where an earlier version put it,
    /// without names. The folder is shown as that, with nothing to go on with, and what
    /// the run left is still listed. The ordinary way of removing that removes nothing
    /// here: it would have to undo the apply first. Only removing it as it is does.
    #[test]
    fn an_apply_that_cannot_be_undone_is_shown_as_that_and_what_it_left_only_goes_as_it_is() {
        let (_scratch, install, site) = cut_off_folder("unreadable-journal");
        let dirs = stage::Dirs::new(&install);
        std::fs::create_dir_all(dirs.backup()).unwrap();
        std::fs::write(dirs.backup().join("0"), b"an original, under no name").unwrap();
        let journal = dirs.journal();

        let status = look(&site, false, false);
        assert!(status.unreadable_journal && status.discard_asks() && !status.putting_back);
        // The folder may be a mix of two versions: it is said to be at neither.
        assert!(status.version.is_none() && status.next_address.is_none());
        assert!(!status.resumable && status.blocked() && status.leftover.is_some());
        let recovery = status.checks.iter().find(|check| check.kind == CheckKind::Recovery).expect("the line");
        assert_eq!((recovery.ok, recovery.quiet, recovery.detail.as_str()), (false, false, "작업 기록 손상"));
        assert_eq!(recovery.message, "이전 패치 작업 기록이 손상되었습니다.\n공식 런처로 게임을 복구해 주세요.");
        assert!(!work_left(&install) && unreadable_journal(&install).is_some());
        // Nothing is tried on it, by a look or by the desk: it is the same folder afterwards.
        assert!(!heal_begins(&site, &install, false, Instant::now()));
        assert!(look(&site, false, false).unreadable_journal && journal.exists());

        assert!(mfp_engine::run::discard_work(&install).is_err());
        assert!(journal.exists());
        mfp_engine::run::discard_work_as_it_is(&install).unwrap();
        let status = look(&site, false, false);
        assert!(!status.unreadable_journal && !status.discard_asks() && !status.blocked());
        assert!(status.leftover.is_none() && !journal.exists());
        assert_eq!(status.version.as_deref(), Some("1.2.900"));
    }

    /// The record of an apply that was cut off cannot be read, and what the apply moved
    /// aside is there under its names. The folder is put back from that, and until it is,
    /// nothing is said of it but that this is waited for: no word of a damaged record.
    #[test]
    fn a_folder_whose_apply_was_cut_off_is_put_back_before_anything_is_said_of_it() {
        let (_scratch, install, site) = cut_off_folder("put-back");
        // The apply had moved the base aside, and put the new one in its place.
        let aside = stage::Dirs::new(&install).aside().join("1").join("Data");
        std::fs::create_dir_all(&aside).unwrap();
        std::fs::write(aside.join("Base.wz"), b"base of 1.2.900").unwrap();
        std::fs::write(install.join("Data").join("Base.wz"), b"base of 1.2.901").unwrap();

        let status = look(&site, false, false);
        assert!(status.putting_back && !status.unreadable_journal && !status.discard_asks());
        assert!(status.blocked() && status.askable() && !status.resumable);
        assert!(status.version.is_none() && status.next_address.is_none());
        assert!(status.checks.iter().all(|check| check.kind != CheckKind::Recovery));
        // Looking puts nothing back.
        assert!(mfp_engine::run::cut_off(&install));

        // One attempt at a time, and none while the site's run has the folder.
        let now = Instant::now();
        assert!(!heal_begins(&site, &install, true, now));
        assert!(heal_begins(&site, &install, false, now));
        assert!(!heal_begins(&site, &install, false, now));
        assert!(look(&site, false, false).putting_back);
        put_back(&site, &install);

        // The base is the one the apply had moved aside, and nothing is left to undo.
        assert_eq!(std::fs::read(install.join("Data").join("Base.wz")).unwrap(), b"base of 1.2.900");
        assert!(!mfp_engine::run::cut_off(&install));
        let status = look(&site, false, false);
        assert!(!status.putting_back && !status.unreadable_journal && !status.blocked());
        assert!(status.checks.iter().all(|check| check.ok));
        // It is a folder like any other again, and the registry's word is where its next
        // check starts.
        assert_eq!(status.version.as_deref(), Some("1.2.900"));
        assert!(!heal_begins(&site, &install, false, Instant::now()));
    }

    /// What the app had on record of a folder was known of the files it held then. It
    /// holds again for a folder that is back as it was, and not for one that is back
    /// without being vouched for as the version before the patch.
    #[test]
    fn what_was_known_of_a_folder_holds_after_it_is_restored_and_not_after_it_is_only_put_back() {
        let scratch = Scratch::new("heal-records");
        let install = scratch.0.join("game");
        std::fs::create_dir_all(install.join("Data")).unwrap();
        std::fs::write(install.join(mfp_kms::LIVE.game_program), b"the game").unwrap();
        let site = Site::new(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::registered_at(&install, 900));
        let found = || Found {
            install: install.clone(),
            mark: None,
            version: 901,
            main: false,
            newest_minor: 3,
            target: None,
            latest: "1.2.901 (3)".to_owned(),
            error: None,
        };
        for (ended, holds) in [(Leftover::RolledBack, true), (Leftover::Restored, true), (Leftover::PutBack, false)] {
            // The folder is at 1.2.901, minor 3, by this app's own records.
            record_version(&site, &install, 901);
            record_minor(&site, &install, 901, 3);
            *site.found.lock().unwrap() = Some(found());
            assert_eq!(look(&site, false, false).version.as_deref(), Some("1.2.901 (3)"));

            heal_ended(&site, &install, Ok(ended));
            assert_eq!(version_proven(&site, &install, 901), holds, "{ended:?}");
            assert_eq!(known_minor(&site, &install, 901), holds.then_some(3), "{ended:?}");
            assert_eq!(site.found_for(&install).is_some(), holds, "{ended:?}");
            // Nothing is in the way either way. A folder that is not vouched for goes by
            // the registry's word again, which is a guess, until a check has looked.
            let status = look(&site, false, false);
            assert!(!status.blocked() && !status.putting_back);
            assert_eq!(status.version.as_deref(), Some(if holds { "1.2.901 (3)" } else { "1.2.900" }), "{ended:?}");
            assert!(site.heal.lock().unwrap().failed.is_none());
        }
    }

    /// A file of the game is held by another program, say. The folder is shown as not put
    /// back, and the next attempt is made at the pace of the checks, not at every look.
    #[test]
    fn a_folder_that_could_not_be_put_back_says_so_and_is_tried_again_after_a_while() {
        let (_scratch, install, site) = cut_off_folder("held");
        let start = Instant::now();
        assert!(heal_begins(&site, &install, false, start));
        let held = Error::Io { what: "rename Base.wz".to_owned(), source: std::io::Error::other("held") };
        heal_ended(&site, &install, Err(held));

        let status = look(&site, false, false);
        assert!(!status.putting_back && !status.unreadable_journal && status.blocked() && !status.resumable);
        assert!(status.version.is_none());
        let recovery = status.checks.iter().find(|check| check.kind == CheckKind::Recovery).expect("the line");
        assert_eq!((recovery.ok, recovery.detail.as_str()), (false, "실패"));
        assert_eq!(recovery.message, "이전 패치를 복구하지 못했습니다.");

        // However often the folder is looked at meanwhile.
        let failed_at = site.heal.lock().unwrap().failed.as_ref().expect("when it failed").0;
        assert!(!heal_begins(&site, &install, false, failed_at + Duration::from_secs(1)));
        assert!(!heal_begins(&site, &install, false, failed_at + Duration::from_secs(29)));
        assert!(heal_begins(&site, &install, false, failed_at + Duration::from_secs(30)));
        // The row stays until that attempt has ended, and goes with one that worked.
        assert!(look(&site, false, false).checks.iter().any(|check| check.kind == CheckKind::Recovery));
        put_back(&site, &install);
        let status = look(&site, false, false);
        assert!(!status.blocked() && !status.putting_back);
        assert_eq!(status.version.as_deref(), Some("1.2.900"));

        assert!(heal_due(false, None));
        assert!(!heal_due(true, None) && !heal_due(true, Some(Duration::from_secs(60))));
        assert!(!heal_due(false, Some(Duration::ZERO)) && heal_due(false, Some(HEAL_AGAIN)));
    }

    /// Which way what a run left goes is decided by what the folder is. The page says that
    /// the user confirmed, where they had to be asked, and that is all it says.
    #[test]
    fn what_a_run_left_goes_as_it_is_only_from_a_folder_whose_record_cannot_be_read_and_only_after_a_yes() {
        let (whole, half_patched, unreadable) = ((false, false), (true, false), (false, true));
        let way =
            |(half_patched, unreadable): (bool, bool), confirmed| way_to_discard(half_patched, unreadable, confirmed);
        // Nothing to ask about: the apply is undone if one was cut off, and the work goes.
        // A yes that nobody was asked for changes nothing about the way it goes.
        assert_eq!(way(whole, false), Some(Discard::Undoing));
        assert_eq!(way(whole, true), Some(Discard::Undoing));
        // A folder that a run left partly new: after a yes, and in the ordinary way.
        assert_eq!(way(half_patched, false), None);
        assert_eq!(way(half_patched, true), Some(Discard::Undoing));
        // A record that cannot be read: after a yes, as it is.
        assert_eq!(way(unreadable, false), None);
        assert_eq!(way(unreadable, true), Some(Discard::AsItIs));
        assert_eq!(way((true, true), false), None);
        assert_eq!(way((true, true), true), Some(Discard::AsItIs));
    }
}
