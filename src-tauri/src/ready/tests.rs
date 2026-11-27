//! Tests of the look at an install folder, of putting a folder back, and of removing what a
//! run left.

use std::path::PathBuf;

use super::*;
use crate::flow::{Decision, Flow, RefusalKind, World, decide, pick};
use crate::harness::StandIns;
use crate::made::Registered;
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
    let site = Site::of(client, scratch.0.join("data"), StandIns::default(), Registered::nowhere());
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

/// The game is open, which is in the way of a patch. Its line asks for the game to be
/// closed, by the client's own name, and a dialog asks the same. Which of the game's
/// programs were found is not on the screen.
#[test]
fn the_line_for_a_game_that_is_open_asks_for_it_to_be_closed_and_names_no_program() {
    let found = || {
        let names = ["BlackCipher64.aes", "DwarfAxe.exe", "MapleStory.exe"];
        vec![Problem::GameRunning { names: names.map(str::to_owned).to_vec() }]
    };
    for (client, asks) in [
        (&mfp_kms::LIVE, "메이플스토리를 종료한 후 실행해 주세요."),
        (&mfp_kms::TEST, "메이플스토리 테스트월드를 종료한 후 실행해 주세요."),
    ] {
        let line = running_check(client.title, found());
        assert_eq!((line.kind, line.ok, line.quiet), (CheckKind::Running, false, true));
        assert_eq!((line.label.as_str(), line.detail.as_str(), line.message.as_str()), (asks, "", asks));
    }
    // Not open: the line has passed, and a line of this kind that has passed is not shown.
    let closed = running_check(mfp_kms::LIVE.title, Vec::new());
    assert!(closed.ok && closed.quiet && closed.detail.is_empty() && closed.message.is_empty());
}

#[test]
fn a_game_that_is_not_installed_is_said_to_be_not_found_by_its_own_name() {
    let scratch = Scratch::new("not-installed");
    // No install of it is registered, and no folder was picked.
    let site = Site::of(&mfp_kms::TEST, scratch.0.join("data"), StandIns::default(), Registered::nowhere());
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
    assert_eq!(status.version.as_deref(), Some("Ver 1.2.208"));
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
    let site = Site::of(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::default(), Registered::at(&install, 900));
    let status = look(&site, false, false);
    assert_eq!(status.version.as_deref(), Some("Ver 1.2.900"));
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
    assert_eq!(look(&site, false, false).version.as_deref(), Some("Ver 1.2.900"));
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
            (line.label.as_str(), line.ok, line.quiet, line.detail.as_str()),
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
    let site = Site::of(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::default(), Registered::at(&install, 900));
    let target = Target { size: 1000, ..crate::made::target("Ver 1.2.422") };
    *site.found.lock().unwrap() = Some(Found {
        install: install.clone(),
        mark: None,
        version: 421,
        main: true,
        newest_minor: 1,
        newest: None,
        target: Some(target.clone()),
        latest: "Ver 1.2.422".to_owned(),
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
    let read = |_: &Site, _: &Path, _: &Path| Some(Room { safe: u64::MAX, immediate: 1 });
    let now = Instant::now();
    let world = World {
        auto_patch: true,
        auto_start: false,
        outdated: false,
        run_at_work: false,
        open: Some(true),
        sees_open: true,
        checking_files: false,
        update_asked: true,
        now,
    };
    // What the app starts by itself with that on the screen, and what a press does.
    let starts = |status: Status| {
        let mut flow = Flow::new(now);
        flow.take_look(status, now);
        let answer = Availability { target: Some(target.clone()), latest: Some("Ver 1.2.422".to_owned()), error: None };
        let epoch = flow.folder_epoch;
        assert!(flow.take_answer(epoch, Checked { answer, end: CheckEnd::Answered }, Some(true), now, 0, &|_| 0));
        let facts = flow.facts(&world);
        (decide(&facts), pick(&facts, None, None).map(|_| ()).map_err(|refusal| (refusal.kind, refusal.messages)))
    };
    for (chosen, enough) in [(false, false), (true, true), (false, false), (true, true)] {
        let status = look_by(&site, false, chosen, &read);
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
    let site = Site::of(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::default(), Registered::at(&install, 900));
    let journal = stage::Dirs::new(&install).journal();
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, "this is no line of a journal\n").unwrap();
    (scratch, install, site)
}

/// An apply was cut off, the record it kept cannot be read, and the folder cannot be
/// put back without it: putting it back was tried, found that what lies aside is not
/// what the patch was made for, and left its note of that. The folder is shown as that,
/// with nothing to go on with, and what the run left is still listed. The ordinary way of
/// removing that removes nothing here: it would have to undo the apply first. Only
/// removing it as it is does.
#[test]
fn an_apply_that_cannot_be_undone_is_shown_as_that_and_what_it_left_only_goes_as_it_is() {
    let (_scratch, install, site) = cut_off_folder("unreadable-journal");
    let dirs = stage::Dirs::new(&install);
    std::fs::write(dirs.work().join("cannot-restore"), "what was moved aside is not the file the patch was made for")
        .unwrap();
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
    assert_eq!(status.version.as_deref(), Some("Ver 1.2.900"));
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
    assert_eq!(status.version.as_deref(), Some("Ver 1.2.900"));
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
    let site = Site::of(&mfp_kms::LIVE, scratch.0.join("data"), StandIns::default(), Registered::at(&install, 900));
    let found = || Found {
        install: install.clone(),
        mark: None,
        version: 901,
        main: false,
        newest_minor: 3,
        newest: None,
        target: None,
        latest: "Ver 1.2.901 마이너버전(3)".to_owned(),
        error: None,
    };
    for (ended, holds) in [(Leftover::RolledBack, true), (Leftover::Restored, true), (Leftover::PutBack, false)] {
        // The folder is at 1.2.901, minor 3, by this app's own records.
        record_version(&site, &install, 901);
        record_minor(&site, &install, 901, 3);
        *site.found.lock().unwrap() = Some(found());
        assert_eq!(look(&site, false, false).version.as_deref(), Some("Ver 1.2.901 마이너버전(3)"));

        heal_ended(&site, &install, Ok(ended));
        assert_eq!(version_proven(&site, &install, 901), holds, "{ended:?}");
        assert_eq!(known_minor(&site, &install, 901), holds.then_some(3), "{ended:?}");
        assert_eq!(site.found_for(&install).is_some(), holds, "{ended:?}");
        // Nothing is in the way either way. A folder that is not vouched for goes by
        // the registry's word again, which is a guess, until a check has looked.
        let status = look(&site, false, false);
        assert!(!status.blocked() && !status.putting_back);
        assert_eq!(
            status.version.as_deref(),
            Some(if holds { "Ver 1.2.901 마이너버전(3)" } else { "Ver 1.2.900" }),
            "{ended:?}"
        );
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
    assert_eq!(status.version.as_deref(), Some("Ver 1.2.900"));

    assert!(heal_due(false, None));
    assert!(!heal_due(true, None) && !heal_due(true, Some(Duration::from_secs(60))));
    assert!(!heal_due(false, Some(Duration::ZERO)) && heal_due(false, Some(HEAL_AGAIN)));
}

/// Which way what a run left goes is decided by what the folder is. The page says that
/// the user confirmed, where they had to be asked, and that is all it says.
#[test]
fn what_a_run_left_goes_as_it_is_only_from_a_folder_whose_record_cannot_be_read_and_only_after_a_yes() {
    let (whole, half_patched, unreadable) = ((false, false), (true, false), (false, true));
    let way = |(half_patched, unreadable): (bool, bool), confirmed| way_to_discard(half_patched, unreadable, confirmed);
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
