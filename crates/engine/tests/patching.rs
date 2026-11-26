//! End-to-end tests on small patch files built here in both layouts the format has.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use mfp_engine::apply::{self, Leftover};
use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Action, Layout};
use mfp_engine::preflight::{self, Problem, SPACE_MARGIN};
use mfp_engine::run::run_on;
use mfp_engine::stage::{self, Dirs};
use mfp_engine::{Error, Event, Options, Outcome, Source, Stage, crc, run_for};
use support::{GO, LAYOUTS, LIVE, Op, PatchBuilder, TEST, World, files_under, run};

const OLD_SKILL: &[u8] = b"AAAAAAAAAABBBBBBBBBBCCCCCCCCCC";
const OLD_MOB: &[u8] = b"0123456789";
const NEW_SKILL: &[u8] = b"BBBBBBBBBBnew!\0\0\0\0\0\x0056789";
const NEW_STRING: &[u8] = b"brand new file";

/// A small install folder plus a patch that creates, rebuilds, deletes and makes a folder.
struct Fixture {
    _temp: tempfile::TempDir,
    install: PathBuf,
    patch: PathBuf,
}

fn fixture(layout: Layout) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let install = temp.path().join("Maple");
    fs::create_dir_all(install.join("Data/Skill")).unwrap();
    fs::create_dir_all(install.join("Data/Mob")).unwrap();
    fs::write(install.join("MapleStory.exe"), b"exe").unwrap();
    fs::write(install.join("Data/Skill/Skill_000.wz"), OLD_SKILL).unwrap();
    fs::write(install.join("Data/Mob/Mob_000.wz"), OLD_MOB).unwrap();
    fs::write(install.join("Data/Obsolete.wz"), b"to be removed").unwrap();

    // In the table layout a rebuild may copy from another file; the older layout cannot.
    let (tail_source, tail): (&str, Vec<u8>) = match layout {
        Layout::Table => (r"Data\Mob\Mob_000.wz", NEW_SKILL.to_vec()),
        Layout::PerEntry => (r"Data\Skill\Skill_000.wz", [&NEW_SKILL[..20], &OLD_SKILL[5..10]].concat()),
    };
    let patch = temp.path().join("00001to00002.patch");
    PatchBuilder::new(layout)
        .original(r"Data\Skill\Skill_000.wz", OLD_SKILL)
        .original(r"Data\Mob\Mob_000.wz", OLD_MOB)
        .create_dir(r"Data\String")
        .create(r"Data\String\String_000.wz", NEW_STRING)
        .rebuild(
            r"Data\Skill\Skill_000.wz",
            OLD_SKILL,
            &tail,
            &[
                Op::Copy(r"Data\Skill\Skill_000.wz".into(), 10, 10),
                Op::Literal(b"new!".to_vec()),
                Op::Fill(0, 6),
                Op::Copy(tail_source.into(), 5, 5),
            ],
        )
        .delete(r"Data\Obsolete.wz")
        .write(&patch);
    Fixture { _temp: temp, install, patch }
}

fn new_skill(layout: Layout) -> Vec<u8> {
    match layout {
        Layout::Table => NEW_SKILL.to_vec(),
        Layout::PerEntry => [&NEW_SKILL[..20], &OLD_SKILL[5..10]].concat(),
    }
}

fn options(fx: &Fixture) -> Options {
    support::options(&fx.install, &fx.patch)
}

fn assert_patched(fx: &Fixture, layout: Layout) {
    assert_eq!(fs::read(fx.install.join("Data/Skill/Skill_000.wz")).unwrap(), new_skill(layout));
    assert_eq!(fs::read(fx.install.join("Data/String/String_000.wz")).unwrap(), NEW_STRING);
    assert_eq!(fs::read(fx.install.join("Data/Mob/Mob_000.wz")).unwrap(), OLD_MOB);
    assert!(!fx.install.join("Data/Obsolete.wz").exists());
    assert!(!fx.install.join(".maple-fast-patch").exists(), "work folder was left behind");
}

fn assert_untouched(fx: &Fixture) {
    assert_eq!(fs::read(fx.install.join("Data/Skill/Skill_000.wz")).unwrap(), OLD_SKILL);
    assert_eq!(fs::read(fx.install.join("Data/Mob/Mob_000.wz")).unwrap(), OLD_MOB);
    assert_eq!(fs::read(fx.install.join("Data/Obsolete.wz")).unwrap(), b"to be removed");
    assert!(!fx.install.join("Data/String/String_000.wz").exists());
}

#[test]
fn patch_is_applied_in_both_layouts() {
    for layout in LAYOUTS {
        let fx = fixture(layout);
        let mut stages = Vec::new();
        let outcome = run(&options(&fx), &GO, &mut |event| {
            if let Event::Stage(stage) = event {
                stages.push(stage);
            }
        })
        .unwrap();

        assert_eq!(outcome, Outcome { files_changed: 3, immediate: false }, "{layout:?}");
        assert_patched(&fx, layout);
        assert_eq!(stages.len(), 4, "{layout:?}: every step after the download is announced once");
    }
}

#[test]
fn scan_lists_what_the_patch_will_do() {
    let fx = fixture(Layout::Table);
    let patch = PatchFile::open(&fx.patch).unwrap();
    patch.verify(&GO, |_| {}).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();

    assert_eq!(plan.layout, Layout::Table);
    assert_eq!(plan.originals.len(), 2);
    let names: Vec<&str> = plan.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, [r"Data\String", r"Data\String\String_000.wz", r"Data\Skill\Skill_000.wz", r"Data\Obsolete.wz"]);
    let Action::Rebuild { new_len, sources, old_crc, .. } = &plan.entries[2].action else { panic!() };
    assert_eq!(*new_len, NEW_SKILL.len() as u64);
    assert_eq!(sources.len(), 2);
    assert_eq!(*old_crc, Some(crc::update(0, OLD_SKILL)));
    assert_eq!(plan.new_bytes(), (NEW_STRING.len() + NEW_SKILL.len()) as u64);
}

#[test]
fn running_the_same_patch_twice_changes_nothing_the_second_time() {
    let fx = fixture(Layout::Table);
    run(&options(&fx), &GO, &mut |_| {}).unwrap();

    // Every file already is the new version and the deleted file is gone.
    let outcome = run(&options(&fx), &GO, &mut |_| {}).unwrap();
    assert_eq!(outcome, Outcome { files_changed: 0, immediate: false });
    assert_patched(&fx, Layout::Table);
}

#[test]
fn every_differing_original_is_reported_and_nothing_changes() {
    let fx = fixture(Layout::Table);
    fs::write(fx.install.join("Data/Skill/Skill_000.wz"), b"modified by something else!!!!").unwrap();
    fs::write(fx.install.join("Data/Mob/Mob_000.wz"), b"also wrong").unwrap();

    let err = run(&options(&fx), &GO, &mut |_| {}).unwrap_err();
    let Error::OriginalMismatch(files) = err else { panic!("unexpected error: {err}") };
    assert_eq!(files, [r"Data\Mob\Mob_000.wz", r"Data\Skill\Skill_000.wz"]);
    assert_eq!(fs::read(fx.install.join("Data/Obsolete.wz")).unwrap(), b"to be removed");
    assert!(!fx.install.join("Data/String/String_000.wz").exists());
}

#[test]
fn damaged_patch_file_is_refused_before_anything_is_read_from_it() {
    let fx = fixture(Layout::Table);
    let mut bytes = fs::read(&fx.patch).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&fx.patch, bytes).unwrap();

    let err = run(&options(&fx), &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::CorruptPatch(_)), "unexpected error: {err}");
    assert_untouched(&fx);
}

#[test]
fn file_that_is_not_a_patch_is_refused() {
    let fx = fixture(Layout::Table);
    fs::write(&fx.patch, b"this is definitely not a patch file").unwrap();
    assert!(matches!(run(&options(&fx), &GO, &mut |_| {}), Err(Error::NotAPatchFile)));
    assert_untouched(&fx);
}

#[test]
fn patch_naming_a_path_outside_the_install_folder_is_refused() {
    let fx = fixture(Layout::Table);
    PatchBuilder::new(Layout::Table).create(r"..\outside.dll", b"payload").write(&fx.patch);

    let err = run(&options(&fx), &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::UnsafePath(_)), "unexpected error: {err}");
    assert!(!fx.install.parent().unwrap().join("outside.dll").exists());
}

#[test]
fn stopping_keeps_built_files_and_the_next_run_finishes() {
    let fx = fixture(Layout::Table);
    let stop = AtomicBool::new(false);

    // Stop as soon as the rebuild step starts writing its second file. The step before it
    // names files too, the ones it checks.
    let mut rebuilding = false;
    let mut files = 0;
    let err = run(&options(&fx), &stop, &mut |event| match event {
        Event::Stage(stage) => rebuilding = stage == Stage::Rebuild,
        Event::File(_) if rebuilding => {
            files += 1;
            if files == 2 {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        _ => {}
    })
    .unwrap_err();
    assert!(matches!(err, Error::Stopped), "unexpected error: {err}");
    assert_untouched(&fx);

    let dirs = Dirs::new(&fx.install);
    assert!(dirs.staged().join("Data/String/String_000.wz").is_file(), "the finished file was not kept");
    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::Staged);

    // The next run says which file it found already built, so that its bytes are not taken
    // for work done now.
    let mut kept = Vec::new();
    let outcome = run(&options(&fx), &GO, &mut |event| {
        if let Event::Kept { name, bytes } = event {
            assert!(bytes > 0, "a kept file was reported without its size");
            kept.push(name);
        }
    })
    .unwrap();
    assert!(kept.iter().any(|name| name.ends_with("String_000.wz")), "not reported as kept: {kept:?}");
    assert_eq!(outcome, Outcome { files_changed: 3, immediate: false });
    assert_patched(&fx, Layout::Table);
}

#[cfg(windows)]
#[test]
fn apply_that_fails_halfway_puts_every_file_back() {
    use std::os::windows::fs::OpenOptionsExt;

    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    let patch = PatchFile::open(&fx.patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, &fx.install, &GO, &|_, _| {}).unwrap();
    stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap();

    // The first two entries apply; the rebuilt file cannot be replaced while it is held open.
    let held =
        fs::OpenOptions::new().read(true).share_mode(0).open(fx.install.join("Data/Skill/Skill_000.wz")).unwrap();
    let err = apply::apply(&plan, &checked, &dirs, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "unexpected error: {err}");
    drop(held);

    assert_untouched(&fx);
    assert!(!fx.install.join("Data/String").exists(), "the created folder was not removed again");
    assert!(dirs.staged().join("Data/String/String_000.wz").is_file(), "the new file was not kept for the next run");

    // With the file released, the same staged files apply cleanly.
    assert_eq!(apply::apply(&plan, &checked, &dirs, &mut |_| {}).unwrap(), 3);
    assert_patched(&fx, Layout::Table);
}

#[test]
fn apply_interrupted_by_a_crash_is_undone_on_the_next_start() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);

    // The state a crash leaves behind: one original moved to the backup folder, its
    // replacement in place, and the journal saying so.
    fs::create_dir_all(dirs.backup()).unwrap();
    fs::rename(fx.install.join("Data/Skill/Skill_000.wz"), dirs.backup().join("0")).unwrap();
    fs::write(fx.install.join("Data/Skill/Skill_000.wz"), NEW_SKILL).unwrap();
    fs::write(dirs.work().join("applying.log"), "B\t0\tData/Skill/Skill_000.wz\n").unwrap();

    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::RolledBack);
    assert_untouched(&fx);
    assert_eq!(fs::read(dirs.staged().join("Data/Skill/Skill_000.wz")).unwrap(), NEW_SKILL);

    run(&options(&fx), &GO, &mut |_| {}).unwrap();
    assert_patched(&fx, Layout::Table);
}

#[test]
fn finished_apply_is_not_undone_when_only_cleanup_was_interrupted() {
    let fx = fixture(Layout::Table);
    run(&options(&fx), &GO, &mut |_| {}).unwrap();

    let dirs = Dirs::new(&fx.install);
    fs::create_dir_all(dirs.work()).unwrap();
    fs::write(dirs.work().join("applying.log"), "B\tData\\Skill\\Skill_000.wz\n").unwrap();
    fs::write(dirs.work().join("committed"), b"").unwrap();

    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::None);
    assert_patched(&fx, Layout::Table);
}

#[test]
fn space_needed_counts_only_what_is_still_to_be_built() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    let patch = PatchFile::open(&fx.patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, &fx.install, &GO, &|_, _| {}).unwrap();
    let everything = (NEW_STRING.len() + NEW_SKILL.len()) as u64;
    assert_eq!(stage::bytes_to_build(&plan, &checked, &dirs), everything);

    // Files an earlier run finished need no more room.
    stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap();
    assert_eq!(stage::bytes_to_build(&plan, &checked, &dirs), 0);

    // Neither does a file that already is the new version.
    fs::remove_dir_all(dirs.work()).unwrap();
    fs::write(fx.install.join("Data/Skill/Skill_000.wz"), NEW_SKILL).unwrap();
    let checked = stage::check_originals(&plan, &fx.install, &GO, &|_, _| {}).unwrap();
    assert_eq!(stage::bytes_to_build(&plan, &checked, &dirs), NEW_STRING.len() as u64);
}

#[test]
fn run_reports_the_space_it_needs_before_building() {
    let fx = fixture(Layout::Table);
    let mut reported = Vec::new();
    run(&options(&fx), &GO, &mut |event| {
        if let Event::Space { needed, available } = event {
            reported.push((needed, available));
        }
    })
    .unwrap();

    let margin = mfp_engine::preflight::SPACE_MARGIN;
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].0, (NEW_STRING.len() + NEW_SKILL.len()) as u64 + margin);
    assert!(reported[0].1 > 0);
}

/// A patch that replaces the Skill file with `contents` as a whole file.
fn whole_file_patch(path: &Path, contents: &[u8]) {
    PatchBuilder::new(Layout::Table).create(r"Data\Skill\Skill_000.wz", contents).write(path);
}

#[test]
fn files_staged_from_another_patch_are_never_applied() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);

    // Patch A is paused after it built its version of the Skill file.
    let patch_a = fx.patch.with_file_name("a.patch");
    whole_file_patch(&patch_a, b"version from patch A");
    let a = PatchFile::open(&patch_a).unwrap();
    let plan_a = plan::scan(&a, &GO).unwrap();
    let checked_a = stage::check_originals(&plan_a, &fx.install, &GO, &|_, _| {}).unwrap();
    stage::stage(&a, &plan_a, &checked_a, &dirs, &GO, &mut |_, _, _| {}).unwrap();
    assert!(dirs.staged().join("Data/Skill/Skill_000.wz").is_file());

    // Patch B wants a Skill file that is already in place, so it builds nothing for it.
    let patch_b = fx.patch.with_file_name("b.patch");
    whole_file_patch(&patch_b, OLD_SKILL);
    let options = Options { source: Source::File(patch_b), ..options(&fx) };
    let outcome = run(&options, &GO, &mut |_| {}).unwrap();

    assert_eq!(outcome, Outcome { files_changed: 0, immediate: false });
    assert_eq!(fs::read(fx.install.join("Data/Skill/Skill_000.wz")).unwrap(), OLD_SKILL);
}

#[cfg(windows)]
#[test]
fn deleted_folder_with_new_contents_is_put_back_when_the_apply_fails() {
    use std::os::windows::fs::OpenOptionsExt;

    let fx = fixture(Layout::Table);
    fs::create_dir_all(fx.install.join("Data/Old/Deep")).unwrap();
    fs::write(fx.install.join("Data/Old/a.wz"), b"old a").unwrap();
    fs::write(fx.install.join("Data/Old/Deep/b.wz"), b"old b").unwrap();
    PatchBuilder::new(Layout::Table)
        .delete(r"Data\Old\a.wz")
        .delete(r"Data\Old/")
        .create(r"Data\Old\New\c.wz", b"new c")
        .create_dir(r"Data\Fresh\Nested")
        .create(r"Data\Mob\Mob_000.wz", b"new mob file")
        .write(&fx.patch);

    let held = fs::OpenOptions::new().read(true).share_mode(0).open(fx.install.join("Data/Mob/Mob_000.wz")).unwrap();
    let dirs = Dirs::new(&fx.install);
    let patch = PatchFile::open(&fx.patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    // The held file cannot be read either, so it is checked before the lock is taken.
    drop(held);
    let checked = stage::check_originals(&plan, &fx.install, &GO, &|_, _| {}).unwrap();
    stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap();
    let held = fs::OpenOptions::new().read(true).share_mode(0).open(fx.install.join("Data/Mob/Mob_000.wz")).unwrap();

    let err = apply::apply(&plan, &checked, &dirs, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "unexpected error: {err}");
    drop(held);

    assert_eq!(fs::read(fx.install.join("Data/Old/a.wz")).unwrap(), b"old a");
    assert_eq!(fs::read(fx.install.join("Data/Old/Deep/b.wz")).unwrap(), b"old b");
    assert!(!fx.install.join("Data/Old/New").exists(), "the new folder inside the old one was left behind");
    assert!(!fx.install.join("Data/Fresh").exists(), "the created folders were left behind");
    assert_eq!(fs::read(fx.install.join("Data/Mob/Mob_000.wz")).unwrap(), OLD_MOB);

    // Released, the same staged files apply: the old folder goes, the new contents arrive.
    assert_eq!(apply::apply(&plan, &checked, &dirs, &mut |_| {}).unwrap(), 4);
    assert!(!fx.install.join("Data/Old/a.wz").exists());
    assert_eq!(fs::read(fx.install.join("Data/Old/New/c.wz")).unwrap(), b"new c");
    assert!(fx.install.join("Data/Fresh/Nested").is_dir());
    assert!(!dirs.work().exists());
}

#[test]
fn run_restores_an_install_whose_executable_was_moved_aside() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);

    // A crash right after the executable was moved to the backup folder: the install folder
    // no longer looks like an install.
    fs::create_dir_all(dirs.backup()).unwrap();
    fs::rename(fx.install.join("MapleStory.exe"), dirs.backup().join("0")).unwrap();
    fs::write(dirs.work().join("applying.log"), "B\t0\tMapleStory.exe\n").unwrap();

    let mut restored = false;
    run(&options(&fx), &GO, &mut |event| restored |= matches!(event, Event::Restored)).unwrap();
    assert!(restored);
    assert_eq!(fs::read(fx.install.join("MapleStory.exe")).unwrap(), b"exe");
    assert_patched(&fx, Layout::Table);
}

#[test]
fn journal_line_cut_off_while_being_written_is_ignored() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    fs::create_dir_all(dirs.backup()).unwrap();
    fs::rename(fx.install.join("Data/Obsolete.wz"), dirs.backup().join("0")).unwrap();
    // The second line has no line break: the app died while writing it.
    fs::write(dirs.work().join("applying.log"), "B\t0\tData/Obsolete.wz\nB\t1\tData/Sk").unwrap();

    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::RolledBack);
    assert_untouched(&fx);
}

#[cfg(windows)]
#[test]
fn second_run_on_the_same_install_is_refused() {
    let fx = fixture(Layout::Table);
    let lock = mfp_engine::run::InstallLock::acquire(&Dirs::new(&fx.install)).unwrap();

    let err = run(&options(&fx), &GO, &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, Error::Blocked(problems) if problems == &[mfp_engine::preflight::Problem::AlreadyRunning]),
        "unexpected error: {err}"
    );
    assert_untouched(&fx);

    drop(lock);
    run(&options(&fx), &GO, &mut |_| {}).unwrap();
    assert_patched(&fx, Layout::Table);
}

#[test]
fn patch_naming_one_file_twice_is_refused() {
    let fx = fixture(Layout::Table);
    PatchBuilder::new(Layout::Table)
        .create(r"Data\Skill\Skill_000.wz", b"first")
        .create(r"data\skill\SKILL_000.wz", b"second")
        .write(&fx.patch);

    // The file is whole, by its checksum: it is what is in it that cannot be used, and a
    // run says so instead of calling the file damaged.
    let err = run(&options(&fx), &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(&err, Error::UnknownForm(detail) if detail.contains("appears twice")), "unexpected error: {err}");
    assert_untouched(&fx);
    // Read without a run, the same file is refused the way it always was.
    let patch = PatchFile::open(&fx.patch).unwrap();
    patch.verify(&GO, |_| {}).unwrap();
    assert!(matches!(plan::scan(&patch, &GO), Err(Error::CorruptPatch(_))));
}

#[test]
fn patch_file_swapped_after_it_was_read_is_refused() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    whole_file_patch(&fx.patch, b"what was planned");
    let patch = PatchFile::open(&fx.patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, &fx.install, &GO, &|_, _| {}).unwrap();

    // Same entry name, different contents.
    whole_file_patch(&fx.patch, b"something else!!");
    let err = stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap_err();
    assert!(matches!(err, Error::CorruptPatch(_)), "unexpected error: {err}");
    assert!(!dirs.staged().join("Data/Skill/Skill_000.wz").exists());
}

#[test]
fn a_run_leaves_no_lock_file_behind() {
    let fx = fixture(Layout::Table);
    run(&options(&fx), &GO, &mut |_| {}).unwrap();
    assert!(!fx.install.join(".maple-fast-patch.lock").exists());

    // Nor does a run that fails.
    fs::write(&fx.patch, b"not a patch file at all, just some text").unwrap();
    run(&options(&fx), &GO, &mut |_| {}).unwrap_err();
    assert!(!fx.install.join(".maple-fast-patch.lock").exists());
}

#[test]
fn file_put_back_into_a_folder_the_patch_deletes_survives_every_later_run() {
    let fx = fixture(Layout::Table);
    fs::create_dir_all(fx.install.join("Data/Old")).unwrap();
    fs::write(fx.install.join("Data/Old/a.wz"), b"old a").unwrap();
    PatchBuilder::new(Layout::Table).delete(r"Data\Old\").create(r"Data\Old\New\c.wz", b"new c").write(&fx.patch);

    // From the second run on the file is already the new version, but the folder it is in
    // is deleted first each time, so it has to be placed again each time.
    for round in 1..=3 {
        run(&options(&fx), &GO, &mut |_| {}).unwrap();
        assert!(!fx.install.join("Data/Old/a.wz").exists(), "round {round}");
        assert_eq!(fs::read(fx.install.join("Data/Old/New/c.wz")).unwrap(), b"new c", "round {round}");
    }
}

#[test]
fn delete_entry_removes_only_the_kind_of_thing_it_names() {
    let fx = fixture(Layout::Table);
    fs::create_dir_all(fx.install.join("Data/Folder")).unwrap();
    fs::write(fx.install.join("Data/Folder/kept.wz"), b"kept").unwrap();
    fs::write(fx.install.join("Data/File"), b"kept too").unwrap();
    PatchBuilder::new(Layout::Table)
        // Names a file, but a folder stands there.
        .delete(r"Data\Folder")
        // Names a folder, but a file stands there.
        .delete(r"Data\File\")
        .delete(r"Data\Obsolete.wz")
        .write(&fx.patch);

    let outcome = run(&options(&fx), &GO, &mut |_| {}).unwrap();
    assert_eq!(outcome, Outcome { files_changed: 1, immediate: false });
    assert_eq!(fs::read(fx.install.join("Data/Folder/kept.wz")).unwrap(), b"kept");
    assert_eq!(fs::read(fx.install.join("Data/File")).unwrap(), b"kept too");
    assert!(!fx.install.join("Data/Obsolete.wz").exists());

    // With nothing left to remove, the patch counts as in place.
    assert_eq!(run(&options(&fx), &GO, &mut |_| {}).unwrap().files_changed, 0);
}

#[test]
fn file_that_would_replace_a_folder_is_refused_and_nothing_changes() {
    let fx = fixture(Layout::Table);
    fs::create_dir_all(fx.install.join("Data/Thing.wz")).unwrap();
    fs::write(fx.install.join("Data/Thing.wz/inside.txt"), b"inside").unwrap();
    PatchBuilder::new(Layout::Table)
        .create(r"Data\Mob\Mob_000.wz", b"new mob file")
        .create(r"Data\Thing.wz", b"a file of the same name")
        .write(&fx.patch);

    let err = run(&options(&fx), &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "unexpected error: {err}");
    assert_eq!(fs::read(fx.install.join("Data/Mob/Mob_000.wz")).unwrap(), OLD_MOB);
    assert_eq!(fs::read(fx.install.join("Data/Thing.wz/inside.txt")).unwrap(), b"inside");
}

#[cfg(windows)]
#[test]
fn undoing_an_apply_in_two_attempts_undoes_each_step_once() {
    use std::os::windows::fs::OpenOptionsExt;

    let fx = fixture(Layout::Table);
    fs::create_dir_all(fx.install.join("Data/Old")).unwrap();
    fs::write(fx.install.join("Data/Old/x.wz"), b"old x").unwrap();
    PatchBuilder::new(Layout::Table)
        .create(r"Data\Mob\Mob_000.wz", b"new mob file")
        .delete(r"Data\Old\")
        .create(r"Data\Old\x.wz", b"new x")
        .write(&fx.patch);
    let dirs = Dirs::new(&fx.install);
    let patch = PatchFile::open(&fx.patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, &fx.install, &GO, &|_, _| {}).unwrap();
    stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap();
    // Every step done, but cut off before the apply was marked finished.
    assert!(apply::apply_then_crash(&plan, &checked, &dirs, usize::MAX).unwrap());
    assert_eq!(fs::read(fx.install.join("Data/Old/x.wz")).unwrap(), b"new x");

    // The first attempt puts the old folder back and then gets stuck on a file in use.
    let held = fs::OpenOptions::new().read(true).share_mode(0).open(fx.install.join("Data/Mob/Mob_000.wz")).unwrap();
    apply::recover(&dirs).unwrap_err();
    assert_eq!(fs::read(fx.install.join("Data/Old/x.wz")).unwrap(), b"old x");
    drop(held);

    // The second attempt must not take the restored file for the new one and remove it.
    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::RolledBack);
    assert_eq!(fs::read(fx.install.join("Data/Old/x.wz")).unwrap(), b"old x");
    assert_eq!(fs::read(fx.install.join("Data/Mob/Mob_000.wz")).unwrap(), OLD_MOB);
}

#[test]
fn work_left_by_a_run_that_will_not_be_continued_can_be_discarded() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    let patch = PatchFile::open(&fx.patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, &fx.install, &GO, &|_, _| {}).unwrap();
    stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap();
    assert!(dirs.work().exists());

    mfp_engine::run::discard_work(&fx.install).unwrap();
    assert!(!dirs.work().exists());
    assert_untouched(&fx);
    assert!(!fx.install.join(".maple-fast-patch.lock").exists());

    // Also after an apply that was cut off: the install is put back first.
    stage::stage(&patch, &plan, &checked, &dirs, &GO, &mut |_, _, _| {}).unwrap();
    apply::apply_then_crash(&plan, &checked, &dirs, 3).unwrap();
    mfp_engine::run::discard_work(&fx.install).unwrap();
    assert!(!dirs.work().exists());
    assert_untouched(&fx);
}

#[test]
fn files_built_from_another_patch_do_not_count_against_free_space() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    // An earlier, abandoned run of some other patch left its files behind.
    fs::create_dir_all(dirs.staged().join("Data")).unwrap();
    fs::write(dirs.work().join("patch.id"), "another-patch").unwrap();
    fs::write(dirs.staged().join("Data/Leftover.wz"), vec![0u8; 4096]).unwrap();

    // They are gone before the run measures what it needs, not only once it starts to build.
    let mut seen_at_space_check = None;
    run(&options(&fx), &GO, &mut |event| {
        if matches!(event, Event::Space { .. }) {
            seen_at_space_check = Some(dirs.staged().join("Data/Leftover.wz").exists());
        }
    })
    .unwrap();
    assert_eq!(seen_at_space_check, Some(false));
    assert_patched(&fx, Layout::Table);
}

/// Puts the fixture's patch file where the app keeps its downloads: inside the work folder.
fn downloaded_into_the_work_folder(fx: &Fixture) -> PathBuf {
    let downloads = Dirs::new(&fx.install).download();
    fs::create_dir_all(&downloads).unwrap();
    let downloaded = downloads.join("00001to00002.patch");
    fs::copy(&fx.patch, &downloaded).unwrap();
    downloaded
}

#[test]
fn patch_file_downloaded_into_the_work_folder_lasts_as_long_as_the_run_and_no_longer() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    let downloaded = downloaded_into_the_work_folder(&fx);
    fs::write(mfp_engine::download::part_path(&dirs.download().join("next.patch")), b"half of the next one").unwrap();

    // A work folder that holds nothing but downloads is no work left behind.
    assert!(preflight::check_leftovers(&fx.install).is_empty());
    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::None);
    assert_eq!(mfp_engine::run::recover(&fx.install).unwrap(), Leftover::None);
    assert!(downloaded.is_file());

    // The run reads its patch file from there to the end, and takes the folder with it
    // when the patch is in place.
    let options = Options { source: Source::File(downloaded.clone()), ..options(&fx) };
    let mut events = 0;
    run(&options, &GO, &mut |_| {
        events += 1;
        assert!(downloaded.is_file(), "the patch file was gone at event {events}");
    })
    .unwrap();
    assert!(events > 0);
    assert_patched(&fx, Layout::Table);
    assert!(!dirs.download().exists());
}

#[test]
fn work_of_another_patch_is_discarded_and_the_downloaded_patch_file_stays() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    let downloaded = downloaded_into_the_work_folder(&fx);
    fs::create_dir_all(dirs.staged().join("Data")).unwrap();
    fs::write(dirs.work().join("patch.id"), "another-patch").unwrap();
    fs::write(dirs.staged().join("Data/Leftover.wz"), vec![0u8; 4096]).unwrap();
    // With staged files beside the downloads, there is work left behind.
    assert_eq!(preflight::check_leftovers(&fx.install), [Problem::LeftoverWork { dir: dirs.work() }]);
    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::Staged);

    let options = Options { source: Source::File(downloaded.clone()), ..options(&fx) };
    let mut seen_at_space_check = None;
    run(&options, &GO, &mut |event| {
        if matches!(event, Event::Space { .. }) {
            seen_at_space_check = Some((dirs.staged().join("Data/Leftover.wz").exists(), downloaded.is_file()));
        }
    })
    .unwrap();
    assert_eq!(seen_at_space_check, Some((false, true)), "(the other patch's file, this patch's download)");
    assert_patched(&fx, Layout::Table);
}

#[test]
fn discarding_work_takes_the_downloads_with_it() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    downloaded_into_the_work_folder(&fx);
    mfp_engine::run::discard_work(&fx.install).unwrap();
    assert!(!dirs.work().exists());
    assert_untouched(&fx);
}

/// A made-up drive with `bytes` free, whatever folder is asked about.
fn drive_with(bytes: u64) -> impl Fn(&Path) -> std::io::Result<u64> {
    move |_| Ok(bytes)
}

#[test]
fn run_without_room_for_what_is_certain_stops_before_the_game_files_are_read() {
    let fx = fixture(Layout::Table);
    // Both new files are certain to be written: one is not there yet, the other replaces
    // a file of another size.
    let needed = (NEW_STRING.len() + NEW_SKILL.len()) as u64 + SPACE_MARGIN;
    let mut stages = Vec::new();
    let mut space = Vec::new();
    let err = run_on(&LIVE, &options(&fx), &drive_with(needed - 1), &GO, &mut |event| match event {
        Event::Stage(stage) => stages.push(stage),
        Event::Space { needed, available } => space.push((needed, available)),
        _ => {}
    })
    .unwrap_err();
    let Error::Blocked(problems) = &err else { panic!("unexpected error: {err}") };
    assert!(
        matches!(&problems[..], [Problem::NotEnoughSpace { needed: n, available: a, .. }] if (*n, *a) == (needed, needed - 1)),
        "{problems:?}"
    );
    assert_eq!(stages, [Stage::VerifyPatch], "the game files were read before the answer came");
    assert_eq!(space, [(needed, needed - 1)]);
    assert_untouched(&fx);
    assert!(!fx.install.join(".maple-fast-patch").exists());

    // One byte more and the run goes through, saying once what it needs, as it always did.
    let mut space = Vec::new();
    let outcome = run_on(&LIVE, &options(&fx), &drive_with(needed), &GO, &mut |event| {
        if let Event::Space { needed, available } = event {
            space.push((needed, available));
        }
    })
    .unwrap();
    assert_eq!(outcome, Outcome { files_changed: 3, immediate: false });
    assert_eq!(space, [(needed, needed)]);
    assert_patched(&fx, Layout::Table);
}

#[test]
fn room_that_is_not_certain_to_be_needed_is_only_asked_for_once_the_game_files_are_read() {
    let fx = fixture(Layout::Table);
    // A file of the same size as the one it replaces may be in place already; only reading
    // it tells. Here it is not, so the exact check is the one that refuses.
    let replacement = vec![b'Z'; OLD_SKILL.len()];
    whole_file_patch(&fx.patch, &replacement);
    let needed = replacement.len() as u64 + SPACE_MARGIN;
    let mut stages = Vec::new();
    let err = run_on(&LIVE, &options(&fx), &drive_with(needed - 1), &GO, &mut |event| {
        if let Event::Stage(stage) = event {
            stages.push(stage);
        }
    })
    .unwrap_err();
    assert!(
        matches!(&err, Error::Blocked(problems) if matches!(problems[..], [Problem::NotEnoughSpace { .. }])),
        "{err}"
    );
    assert_eq!(stages, [Stage::VerifyPatch, Stage::VerifyInstall]);
    assert_untouched(&fx);
}

#[cfg(windows)]
#[test]
fn forged_journal_cannot_move_files_out_of_the_install_folder() {
    let fx = fixture(Layout::Table);
    let dirs = Dirs::new(&fx.install);
    let outside = fx.install.parent().unwrap().join("Elsewhere");
    fs::create_dir_all(&outside).unwrap();
    // Someone with access to the install folder plants a link that leads out of it, a file
    // in the backup folder, and a journal that says the file belongs behind the link.
    let link = fx.install.join("j");
    let made =
        std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&outside).output().unwrap();
    assert!(made.status.success(), "could not make the link: {}", String::from_utf8_lossy(&made.stderr));
    fs::create_dir_all(dirs.backup()).unwrap();
    fs::write(dirs.backup().join("0"), b"planted").unwrap();
    fs::write(dirs.work().join("applying.log"), "B\t0\tj/planted.exe\n").unwrap();

    let err = apply::recover(&dirs).unwrap_err();
    assert!(matches!(err, Error::Blocked(_)), "unexpected error: {err}");
    assert!(!outside.join("planted.exe").exists(), "a file was moved out of the install folder");

    // Neither does a work folder that is itself such a link get used.
    fs::remove_dir_all(dirs.work()).unwrap();
    fs::remove_dir(&link).unwrap();
    let made =
        std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(dirs.work()).arg(&outside).output().unwrap();
    assert!(made.status.success());
    fs::write(outside.join("applying.log"), "N\tData/Mob/Mob_000.wz\n").unwrap();
    let err = apply::recover(&dirs).unwrap_err();
    assert!(matches!(err, Error::Blocked(_)), "unexpected error: {err}");
    assert_eq!(fs::read(fx.install.join("Data/Mob/Mob_000.wz")).unwrap(), OLD_MOB);
    fs::remove_dir(dirs.work()).unwrap();
}

#[test]
fn rebuilt_file_inside_a_folder_the_patch_deletes_is_never_silently_lost() {
    let fx = fixture(Layout::Table);
    fs::create_dir_all(fx.install.join("Data/Old")).unwrap();
    fs::write(fx.install.join("Data/Old/x.wz"), b"old x").unwrap();
    PatchBuilder::new(Layout::Table)
        .original("Data/Old/x.wz", b"old x")
        .delete("Data/Old/")
        .rebuild("Data/Old/x.wz", b"old x", b"new x", &[Op::Literal(b"new x".to_vec())])
        .write(&fx.patch);

    run(&options(&fx), &GO, &mut |_| {}).unwrap();
    assert_eq!(fs::read(fx.install.join("Data/Old/x.wz")).unwrap(), b"new x");

    // Run again, the file already is the new version. Skipping it would let it vanish with
    // the folder; it is placed again, or the run stops, but the file stays.
    let _ = run(&options(&fx), &GO, &mut |_| {});
    assert_eq!(fs::read(fx.install.join("Data/Old/x.wz")).unwrap(), b"new x");
}

#[test]
fn install_folder_of_the_other_client_is_refused_before_anything_is_touched() {
    // Each client's folder with the other client's names, and then with its own.
    for (theirs, asked_with) in [(LIVE, TEST), (TEST, LIVE)] {
        let what = format!("a folder of {} asked about with {}", theirs.game, asked_with.game);
        let world = World::random_for(&theirs, 31, Layout::Table);
        let untouched = files_under(&world.install);

        let err = run_for(&asked_with, &world.options(), &GO, &mut |_| {}).unwrap_err();
        let expected = [Problem::NotAnInstallDir { dir: world.install.clone(), missing: asked_with.game }];
        assert!(matches!(&err, Error::Blocked(problems) if problems[..] == expected), "{what}: {err}");
        // Not even the file that says a run is going on is left behind.
        assert!(files_under(&world.install) == untouched, "{what}: the folder was changed");
        world.assert_untouched(&what);

        let outcome = run_for(&theirs, &world.options(), &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(outcome.files_changed > 0, "{what}: {outcome:?}");
        world.assert_patched(&what);
    }
}

#[cfg(windows)]
#[test]
fn folder_of_another_client_is_refused_without_being_locked() {
    // Another run holds the folder. A run for the wrong client does not get as far as
    // asking for it: the answer is that the folder is not that client's, not that it is busy.
    let world = World::random_for(&TEST, 33, Layout::Table);
    let held = mfp_engine::run::InstallLock::acquire(&Dirs::new(&world.install)).unwrap();
    let err = run_for(&LIVE, &world.options(), &GO, &mut |_| {}).unwrap_err();
    let expected = [Problem::NotAnInstallDir { dir: world.install.clone(), missing: LIVE.game }];
    assert!(matches!(&err, Error::Blocked(problems) if problems[..] == expected), "unexpected error: {err}");
    // For its own client the folder is busy, as it is.
    let err = run_for(&TEST, &world.options(), &GO, &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, Error::Blocked(problems) if problems[..] == [Problem::AlreadyRunning]),
        "unexpected error: {err}"
    );
    drop(held);
    world.assert_untouched("after both were refused");
}

#[test]
fn folder_with_an_apply_that_was_cut_off_is_restored_before_it_is_judged() {
    // A folder of the test client in which an apply died after it had moved one file aside.
    let world = World::random_for(&TEST, 34, Layout::Table);
    let dirs = Dirs::new(&world.install);
    let moved = world.before.keys().find(|name| name.starts_with("Data")).unwrap();
    fs::create_dir_all(dirs.backup()).unwrap();
    fs::rename(support::on_disk(&world.install, moved), dirs.backup().join("0")).unwrap();
    fs::write(dirs.work().join("applying.log"), format!("B\t0\t{}\n", moved.replace('\\', "/"))).unwrap();

    // Asked about as the live client's, it is short of that client's program. That alone does
    // not say whose folder it is: the program could be among what the apply had moved. So
    // the apply is undone first, and then the folder is refused for what it is.
    let mut restored = false;
    let err =
        run_for(&LIVE, &world.options(), &GO, &mut |event| restored |= matches!(event, Event::Restored)).unwrap_err();
    let expected = [Problem::NotAnInstallDir { dir: world.install.clone(), missing: LIVE.game }];
    assert!(matches!(&err, Error::Blocked(problems) if problems[..] == expected), "unexpected error: {err}");
    assert!(restored, "the apply that was cut off was not undone");
    world.assert_untouched("after the apply was undone");

    // Its own client patches it.
    run_for(&TEST, &world.options(), &GO, &mut |_| {}).unwrap();
    world.assert_patched("by its own client");
}

#[test]
fn files_of_an_apply_that_failed_are_not_named_as_placed() {
    // Two new files, and a folder standing where the second one goes. The apply puts the
    // first in its place, fails at the second, and puts everything back.
    let fx = fixture(Layout::Table);
    PatchBuilder::new(Layout::Table)
        .create(r"Data\First.wz", b"the first new file")
        .create(r"Data\Second.wz", b"the second new file")
        .write(&fx.patch);
    let (first, second) = (fx.install.join("Data/First.wz"), fx.install.join("Data/Second.wz"));
    fs::create_dir(&second).unwrap();

    let mut placed = Vec::new();
    let mut applying = false;
    let err = run(&options(&fx), &GO, &mut |event| match event {
        Event::Stage(stage) => applying = stage == Stage::Apply,
        Event::Placed { name } => placed.push(name),
        _ => {}
    })
    .unwrap_err();
    assert!(matches!(&err, Error::Io { what, .. } if what.starts_with("replace ")), "unexpected error: {err}");
    assert!(applying, "the run did not get as far as the apply");
    assert!(!first.exists() && second.is_dir(), "the apply was not put back");
    assert!(placed.is_empty(), "files that were put back were named as placed: {placed:?}");

    // With the folder out of the way both go in, and both are named, once.
    fs::remove_dir(&second).unwrap();
    let outcome = run(&options(&fx), &GO, &mut |event| {
        if let Event::Placed { name } = event {
            placed.push(name);
        }
    })
    .unwrap();
    assert_eq!(placed, [r"Data\First.wz", r"Data\Second.wz"]);
    assert_eq!(outcome.files_changed, 2);
    assert_eq!(fs::read(&first).unwrap(), b"the first new file");
    assert_eq!(fs::read(&second).unwrap(), b"the second new file");
}
