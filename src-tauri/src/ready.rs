//! The install folder: the look at it that the ready screen is made from, with the checks
//! made before a patch may start, the folder the user picks, and what an unfinished run left
//! behind.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;

use mfp_engine::preflight::{self, Problem, SpaceNeed};
use mfp_engine::{download, stage};
use tauri::{AppHandle, State};

use crate::desk::{look_now, update};
use crate::offer::full_label;
use crate::patching::taskbar_progress;
use crate::records::{
    Left, choose_dir, chosen_dir, forget_left_run, install_dir, installed_version, known_minor, left_record,
    patched_before, to_apply, unsettled,
};
use crate::state::{Check, CheckKind, Patcher, Site, Status, Target};
use crate::wording::{describe_error, gib, no_folder_of, not_the_games_folder, sentence, size_label};

/// What there is to see of the install folder of `site`: the folder, the checks made before
/// a patch may start there, and what an unfinished run left. `running` says that the site's
/// run is at work, which has the folder to itself.
///
/// Whoever wants to know asks [`look_now`], which also keeps what is found.
pub(crate) fn look(site: &Site, running: bool) -> Status {
    let install_dir = install_dir(site);
    // What the game is called, for the sentences that name it.
    let game = site.client.title;
    let mut resumable = false;
    let mut left: Option<(Option<String>, f64)> = None;
    // A folder that is not a game folder has no version. Nor, yet, has a copy whose files
    // have not been looked at: the registry's version is that of another folder.
    let game_dir = install_dir.as_deref().filter(|dir| site.programs().check_install_dir(dir).is_empty());
    // Nor has a folder that a run left partly new: it is at neither version until that run
    // is finished.
    let half_patched = install_dir.as_deref().is_some_and(mfp_engine::run::half_patched);
    let version = game_dir.filter(|dir| !half_patched && unsettled(site, dir).is_none()).and_then(|dir| {
        let version = installed_version(site, dir)?;
        Some(full_label(site.client, version, known_minor(site, dir, version).unwrap_or(1)))
    });
    // A run that was cut off while applying is undone before anything else is looked at.
    // While that fails (a file is held open, say) it is tried again on every look, and
    // the install is shown as not ready: it is neither old nor new until this works.
    let not_restored = match (&install_dir, site.restoring.try_lock()) {
        // One attempt at a time: undoing can take longer than the time between looks.
        (Some(dir), Ok(_one_at_a_time)) if !running && stage::Dirs::new(dir).journal().exists() => {
            mfp_engine::run::recover(dir).err()
        }
        _ => None,
    };
    if let (Some(err), false) = (&not_restored, site.restore_logged.swap(not_restored.is_some(), Ordering::SeqCst)) {
        site.log(&format!("could not undo an interrupted apply: {err}"));
    }
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
            let partial_download = target.as_ref().is_some_and(|target| {
                download::part_path(&stage::Dirs::new(dir).download().join(&target.file_name)).exists()
            });
            // Leftovers are not a problem: the next run restores or reuses them.
            resumable = !running && (!preflight::check_leftovers(dir).is_empty() || partial_download || half_patched);
            // What that run was applying and how far it had come, for as long as the files it
            // built are there: the rule the run itself goes by when it is continued. A file
            // the user picked has to be there still; a patch from the server has to be the
            // one the server offers now.
            if resumable
                && dir.join(preflight::WORK_DIR_NAME).is_dir()
                && let Some(Left { patch, percent, .. }) = left_record(site, dir)
            {
                let picked = Path::new(&patch).is_absolute();
                if picked && Path::new(&patch).is_file() {
                    left = Some((Some(patch), percent));
                } else if !picked && target.as_ref().is_some_and(|target| target.file_name == patch) {
                    left = Some((None, percent));
                }
            }
            if site.programs().check_install_dir(dir).is_empty() {
                vec![
                    Check::passed(CheckKind::Folder, dir.display().to_string()),
                    running_check(game, site.programs().check_running(dir)),
                    Check::of(game, CheckKind::Writable, "", site.writable(dir)).quiet(),
                    space_check(game, dir, target.as_ref()),
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
    if not_restored.is_some() {
        checks.push(Check::failed(CheckKind::Recovery, "실패", "이전 패치를 복구하지 못했습니다."));
    }
    let leftover = if running { None } else { install_dir.as_deref().and_then(leftover_bytes).map(size_label) };
    let (resume_file, resume_percent) = match left {
        Some((file, percent)) => (file, Some(percent)),
        None => (None, None),
    };
    // A client this app was never tried on is patched at the user's press the first time.
    let by_press_only = site.untried() && install_dir.as_deref().is_some_and(|dir| !patched_before(site, dir));
    Status {
        install_dir: install_dir.map(|dir| dir.display().to_string()),
        version,
        custom_dir: chosen_dir(site).is_some(),
        leftover,
        checks,
        resumable,
        resume_file,
        resume_percent,
        half_patched,
        by_press_only,
    }
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

/// Compares free space with what is known to be needed so far.
///
/// Before the patch file has been read, only its own size is known, so this checks that the
/// download fits. What the new files need is computed from the patch file during the run and
/// checked again there, before anything is built.
fn space_check(game: &str, dir: &Path, target: Option<&Target>) -> Check {
    let Ok(free) = preflight::free_space(dir) else {
        return Check::failed(CheckKind::Space, "확인 불가", "디스크 여유 공간을 확인할 수 없습니다.");
    };
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

/// Uses the folder the user picked as the install folder. `None` goes back to the one the
/// Nexon installer recorded.
#[tauri::command(async)]
pub(crate) fn set_install_dir(
    app: AppHandle,
    patcher: State<Patcher>,
    server: String,
    path: Option<String>,
) -> Result<(), String> {
    let site = patcher.site_named(&server).ok_or("설치 경로를 찾을 수 없습니다.")?;
    if patcher.running_for(site) {
        return Err("패치가 진행 중입니다.".into());
    }
    choose_dir(site, path.as_deref()).map_err(|_| "설치 경로를 저장하지 못했습니다.".to_owned())?;
    // A run that was paused in the folder that was left has nothing to show for this one.
    taskbar_progress(&app, None);
    // The folder is looked at before this returns, so that whoever asked finds the new one.
    // Another folder may be at another version: the look puts aside what was found for the
    // one before, and the server is asked about this one at once.
    look_now(&app, &patcher, site);
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

/// Deletes what a paused or interrupted run built and downloaded, for a run that will not
/// be continued. A run that was cut off while applying is undone first. A run that replaced
/// game files as it built them cannot be undone: the folder stays partly new, and the
/// screen asks before it comes to this.
#[tauri::command(async)]
pub(crate) fn discard_leftovers(app: AppHandle, patcher: State<Patcher>, server: String) -> Result<(), String> {
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
    mfp_engine::run::discard_work(&install).map_err(|err| describe_error(site.client.title, &err).0)?;
    // With its work gone, the run it belonged to starts from the beginning, and so does
    // the measuring of it. The taskbar has no paused run to show any more.
    forget_left_run(site, &install);
    *site.measure.lock().unwrap() = None;
    taskbar_progress(&app, None);
    // What was offered for a half-patched folder was the patch to finish. With that given
    // up, the folder is looked at afresh.
    if half_patched {
        site.forget_found();
    }
    site.log(&format!("discarded the work folder in {}", install.display()));
    // The server is asked at once, and the folder looked at before this returns.
    update(&app, &patcher, site, |flow| flow.leftovers_discarded(half_patched, Instant::now()));
    look_now(&app, &patcher, site);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::harness::StandIns;
    use crate::records::record_version;
    use crate::scratch::Scratch;

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
            let status = look(&site, false);
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
        let status = look(&site, false);
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
        let status = look(&site, false);
        assert!(status.game_folder());
        // Nothing says which version it is at, and nothing is made up.
        assert_eq!(status.version, None);
        assert!(status.by_press_only);

        record_version(&site, &install, 1208);
        let status = look(&site, false);
        assert_eq!(status.version.as_deref(), Some("1.2.208"));
        assert!(!status.by_press_only);

        // The live game's folder was never held back.
        let scratch = Scratch::new("first-press-live");
        let (site, _) = picked(&mfp_kms::LIVE, mfp_kms::LIVE.game_program, &scratch);
        assert!(!look(&site, false).by_press_only);
    }
}
