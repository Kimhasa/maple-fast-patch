//! Attacks on the engine by a folder that fights back, and by being killed.
//!
//! Each test is an attack that was tried and stays in the suite. What has to hold under every
//! one of them: no panic and no hang; a run that fails leaves the install folder as it was
//! (the safe way) or with every file wholly old or wholly new and able to go on (the
//! immediate way); nothing outside the install folder is written, moved or deleted, whatever
//! links lead there; and every refusal is an [`Error`].

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use mfp_engine::apply::{self, Leftover};
use mfp_engine::container::PatchFile;
use mfp_engine::immediate::{self, Job};
use mfp_engine::plan::{self, Layout};
use mfp_engine::preflight::{self, Problem};
use mfp_engine::run::{self, Disk, half_patched, run_on_disk};
use mfp_engine::stage::{self, Dirs};
use mfp_engine::storage::Profile;
use mfp_engine::{Error, Event, Options, Outcome, Stage};
use support::{Drive, GO, LAYOUTS, LIVE, Op, PatchBuilder, copy, files_under, on_disk};

const EXE: &str = "MapleStory.exe";
const A: &str = r"Data\A.wz";
const B: &str = r"Data\B.wz";
const STAY: &str = r"Data\Stay.wz";
const GONE: &str = r"Data\Old\Gone.wz";
const MORE: &str = r"Data\Old\Deep\More.wz";
const FRESH: &str = r"Data\New\Fresh.wz";

/// The files of a folder by name.
type Files = BTreeMap<String, Vec<u8>>;

/// Bytes that differ from place to place, so that a copy from the wrong place shows.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|at| (at as u8).wrapping_mul(31).wrapping_add(seed).wrapping_add((at >> 8) as u8)).collect()
}

/// A small install put together by hand, a patch that does one of everything to it, and a
/// folder beside it that nothing may ever touch.
struct Case {
    _temp: tempfile::TempDir,
    install: PathBuf,
    outside: PathBuf,
    patch: PathBuf,
    /// The files of the install before the patch and after it, and those of the folder
    /// beside it.
    before: Files,
    after: Files,
    beside: Files,
}

/// Writes `files` below `root`.
fn write_files(root: &Path, files: &Files) {
    for (name, contents) in files {
        let path = on_disk(root, name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

fn case(layout: Layout) -> Case {
    case_in_order(layout, false)
}

/// [`case`] with a choice of which of the two rebuilds the patch has first. With `b_first`,
/// B is replaced before A is built again, and in the table layout A is built from the old B.
fn case_in_order(layout: Layout, b_first: bool) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let (install, outside) = (temp.path().join("Maple"), temp.path().join("Elsewhere"));
    let files = [
        (EXE, pattern(64, 1)),
        (A, pattern(40_000, 2)),
        (B, pattern(30_000, 3)),
        (STAY, pattern(2000, 4)),
        (GONE, pattern(500, 5)),
        (MORE, pattern(300, 6)),
    ];
    let before: Files = files.into_iter().map(|(name, contents)| (name.to_owned(), contents)).collect();
    write_files(&install, &before);
    let beside: Files = [("keep.txt", b"nothing may touch this".to_vec()), (r"sub\more.txt", b"nor this".to_vec())]
        .map(|(n, c)| (n.to_owned(), c))
        .into();
    write_files(&outside, &beside);

    // A is rebuilt from itself and, where the layout allows it, from B, which is rebuilt
    // too: in the immediate way B has to wait for A.
    let other = if layout == Layout::Table { B } else { A };
    let for_a = [copy(A, 100, 30_000), Op::Literal(pattern(200, 7)), Op::Fill(3, 500), copy(other, 0, 5000)];
    let for_b = [copy(B, 1000, 20_000), Op::Literal(pattern(50, 8))];
    let build = |ops: &[Op]| {
        let mut new = Vec::new();
        for op in ops {
            match op {
                Op::Literal(data) => new.extend(data),
                Op::Fill(byte, len) => new.resize(new.len() + *len as usize, *byte),
                Op::Copy(from, start, len) => new.extend(&before[from][*start as usize..(*start + *len) as usize]),
            }
        }
        new
    };
    let (new_a, new_b, fresh) = (build(&for_a), build(&for_b), pattern(5000, 9));
    let patch = temp.path().join("made-up.patch");
    let mut builder = PatchBuilder::new(layout);
    builder.original(A, &before[A]).original(B, &before[B]).create_dir(r"Data\New").create(FRESH, &fresh);
    if b_first {
        builder.rebuild(B, &before[B], &new_b, &for_b).rebuild(A, &before[A], &new_a, &for_a);
    } else {
        builder.rebuild(A, &before[A], &new_a, &for_a).rebuild(B, &before[B], &new_b, &for_b);
    }
    builder.delete(r"Data\Old\").write(&patch);

    let mut after = before.clone();
    after.extend([(A.to_owned(), new_a), (B.to_owned(), new_b), (FRESH.to_owned(), fresh)]);
    after.retain(|name, _| !name.starts_with(r"Data\Old"));
    Case { _temp: temp, install, outside, patch, before, after, beside }
}

impl Case {
    fn dirs(&self) -> Dirs {
        Dirs::new(&self.install)
    }

    fn work(&self) -> PathBuf {
        self.dirs().work()
    }

    /// The files of the install now, the engine's own work folder and lock file aside, and
    /// not what a link that a test planted as `Data\Link` leads to.
    fn files(&self) -> Files {
        let mut files = files_under(&self.install);
        files.retain(|name, _| !name.starts_with(".maple-fast-patch") && !name.starts_with(r"Data\Link\"));
        files
    }

    fn options(&self, immediate: bool) -> Options {
        Options { immediate, ..support::options(&self.install, &self.patch) }
    }

    /// A drive with just the room the immediate way needs for this patch, which is less
    /// than the safe way needs.
    fn tight_drive(&self) -> Drive {
        Drive::tight(&self.install, &self.patch)
    }

    /// Runs the patch the safe way.
    fn run(&self, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<Outcome, Error> {
        support::run(&self.options(false), &GO, on_event)
    }

    /// Runs the patch on `drive`, where it goes the immediate way.
    fn run_on(&self, drive: &Drive, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<Outcome, Error> {
        let free = |_: &Path| -> io::Result<u64> { Ok(drive.free()) };
        let disk = Disk { free: &free, slack: 0, file_system: Some("NTFS") };
        run_on_disk(&LIVE, &self.options(true), &disk, &GO, on_event)
    }

    /// Runs the patch one of the two ways.
    fn run_way(&self, immediate: bool, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<Outcome, Error> {
        if immediate { self.run_on(&self.tight_drive(), on_event) } else { self.run(on_event) }
    }

    #[track_caller]
    fn assert_untouched(&self, what: &str) {
        let now = self.files();
        let changed: Vec<&String> =
            self.before.keys().chain(now.keys()).filter(|n| self.before.get(*n) != now.get(*n)).collect();
        assert!(changed.is_empty(), "{what}: the install folder is not as it was: {changed:?}");
    }

    #[track_caller]
    fn assert_patched(&self, what: &str) {
        let now = self.files();
        let wrong: Vec<&String> =
            self.after.keys().chain(now.keys()).filter(|n| self.after.get(*n) != now.get(*n)).collect();
        assert!(wrong.is_empty(), "{what}: the install folder is not the patched one: {wrong:?}");
        assert!(fs::symlink_metadata(self.work()).is_err(), "{what}: the work folder was left behind");
    }

    /// Fails unless every file of the install is wholly what it was or wholly what the
    /// patch makes it.
    #[track_caller]
    fn assert_whole(&self, what: &str) {
        let now = self.files();
        for name in self.before.keys().chain(self.after.keys()).chain(now.keys()) {
            let found = now.get(name);
            assert!(
                found == self.before.get(name) || found == self.after.get(name),
                "{what}: {name} is neither old nor new"
            );
        }
    }

    #[track_caller]
    fn assert_nothing_outside_touched(&self, what: &str) {
        assert!(files_under(&self.outside) == self.beside, "{what}: something beside the install folder was touched");
    }
}

/// Makes `link` a junction to the folder `to`: the kind of link anybody may make.
fn junction(link: &Path, to: &Path) {
    let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(to).output().unwrap();
    assert!(made.status.success(), "could not make the link: {}", String::from_utf8_lossy(&made.stderr));
}

/// Makes `link` a symbolic link to the file `to`, which on most PCs only an administrator
/// may. Says whether it could be made.
fn file_link(link: &Path, to: &Path) -> bool {
    std::os::windows::fs::symlink_file(to, link).is_ok()
}

/// Whether `event` says that the run has come to `stage`.
fn begins(event: &Event, stage: Stage) -> bool {
    matches!(event, Event::Stage(begun) if *begun == stage)
}

/// Whether a refusal says that a folder leads out of the install folder.
fn leads_outside(result: &Result<Outcome, Error>) -> bool {
    matches!(result, Err(Error::Blocked(problems)) if format!("{problems:?}").contains("OutsideInstall"))
}

// ---------------------------------------------------------------------------------------
// Links.
// ---------------------------------------------------------------------------------------

#[test]
fn work_folder_that_is_a_link_leads_nothing_out_of_the_install_folder() {
    // What the folder at the other end holds besides its own files, to look like work of
    // the engine: nothing; the mark of a finished apply; the journal of one that was cut
    // off; files staged by this very patch; and those with the mark of a finished apply.
    for scene in 0..5 {
        let mut case = case(Layout::Table);
        let dirs = case.dirs();
        if scene >= 3 {
            // Staged files as the engine itself leaves them, moved to the other end.
            build(&case);
            for child in fs::read_dir(dirs.work()).unwrap() {
                let child = child.unwrap();
                fs::rename(child.path(), case.outside.join(child.file_name())).unwrap();
            }
            fs::remove_dir(dirs.work()).unwrap();
        }
        match scene {
            1 | 4 => fs::write(case.outside.join("committed"), b"").unwrap(),
            2 => fs::write(case.outside.join("applying.log"), "N\tData/A.wz\n").unwrap(),
            _ => {}
        }
        case.beside = files_under(&case.outside);
        junction(&dirs.work(), &case.outside);

        // Whatever is asked of the engine, nothing at the other end is used, moved or
        // removed, and nothing is written there.
        for immediate in [false, true] {
            let result = case.run_way(immediate, &mut |_| {});
            assert!(leads_outside(&result), "scene {scene}, immediate {immediate}: {result:?}");
            case.assert_nothing_outside_touched(&format!("scene {scene}, a run, immediate {immediate}"));
            case.assert_untouched(&format!("scene {scene}, a run, immediate {immediate}"));
        }
        let recovered = run::recover(&case.install);
        assert!(matches!(&recovered, Err(Error::Blocked(_))), "scene {scene}: {recovered:?}");
        case.assert_nothing_outside_touched(&format!("scene {scene}, a recover"));

        // Discarding the work takes the link away, and leaves alone what it led to.
        run::discard_work(&case.install).unwrap();
        assert!(fs::symlink_metadata(dirs.work()).is_err(), "scene {scene}: the link is still there");
        case.assert_nothing_outside_touched(&format!("scene {scene}, discarding the work"));
        case.assert_untouched(&format!("scene {scene}"));
        case.run(&mut |_| {}).unwrap();
        case.assert_patched(&format!("scene {scene}, with the link gone"));
    }
}

#[test]
fn link_inside_the_work_folder_leads_nothing_out_of_the_install_folder() {
    // Each of the folders the engine keeps in its work folder, as a link to elsewhere.
    for linked in ["new", r"new\Data", "backup", "download"] {
        let case = case(Layout::Table);
        let dirs = case.dirs();
        // Built files wait in the work folder, as after a pause.
        build(&case);
        let link = linked.split('\\').fold(dirs.work(), |path, part| path.join(part));
        let _ = fs::remove_dir_all(&link);
        junction(&link, &case.outside);

        for immediate in [false, true] {
            let result = case.run_way(immediate, &mut |_| {});
            assert!(leads_outside(&result), "{linked}, immediate {immediate}: {result:?}");
            case.assert_nothing_outside_touched(&format!("{linked}, a run, immediate {immediate}"));
            case.assert_untouched(&format!("{linked}, a run, immediate {immediate}"));
        }
        // Discarding the work removes the link with the folder it is in, and nothing of
        // what it led to.
        run::discard_work(&case.install).unwrap();
        assert!(fs::symlink_metadata(dirs.work()).is_err(), "{linked}: the work folder is still there");
        case.assert_nothing_outside_touched(&format!("{linked}, discarding the work"));
    }
}

/// An install with a folder `Data\Link` that is a link to the folder beside the install,
/// and a patch made of `entries` for it.
fn case_with_link(entries: impl FnOnce(&mut PatchBuilder)) -> Case {
    let case = case(Layout::Table);
    junction(&case.install.join("Data").join("Link"), &case.outside);
    let mut builder = PatchBuilder::new(Layout::Table);
    entries(&mut builder);
    builder.write(&case.patch);
    case
}

#[test]
fn nothing_is_written_or_deleted_through_a_link_in_the_install_folder() {
    let data = pattern(100, 1);
    let data = &data;
    type Entries<'a> = &'a dyn Fn(&mut PatchBuilder);
    let created = |name: &'static str| move |patch: &mut PatchBuilder| _ = patch.create(name, data);
    let deleted = |name: &'static str| move |patch: &mut PatchBuilder| _ = patch.delete(name);
    let (new_file, deep_file, over_file) =
        (created(r"Data\Link\new.wz"), created(r"Data\Link\sub\new.wz"), created(r"Data\Link\keep.txt"));
    let (no_file, no_folder) = (deleted(r"Data\Link\keep.txt"), deleted(r"Data\Link\sub\"));
    let through: [(&str, Entries); 6] = [
        ("a new file", &new_file),
        ("a new file two folders in", &deep_file),
        ("a new folder", &|patch| _ = patch.create_dir(r"Data\Link\Made")),
        ("a file put in the place of one", &over_file),
        ("a delete of a file", &no_file),
        ("a delete of a folder", &no_folder),
    ];
    for (what, entries) in through {
        for immediate in [false, true] {
            let case = case_with_link(entries);
            // The immediate way is asked for on a drive without room for anything else.
            let result = if immediate {
                let drive = Drive::with_room(&case.install, 0);
                case.run_on(&drive, &mut |_| {})
            } else {
                case.run(&mut |_| {})
            };
            assert!(leads_outside(&result), "{what}, immediate {immediate}: {result:?}");
            case.assert_nothing_outside_touched(&format!("{what}, immediate {immediate}"));
            case.assert_untouched(what);
        }
    }

    // A delete of the link itself takes the link, as a folder of the install, and nothing
    // of what it led to.
    let case = case_with_link(|patch| _ = patch.delete(r"Data\Link\"));
    case.run(&mut |_| {}).unwrap();
    assert!(fs::symlink_metadata(case.install.join("Data").join("Link")).is_err(), "the link is still there");
    case.assert_nothing_outside_touched("a delete of the link itself");
}

#[test]
fn folder_swapped_for_a_link_while_the_run_builds_is_seen_before_anything_is_moved() {
    for immediate in [false, true] {
        let case = case(Layout::Table);
        let drive = case.tight_drive();
        let mut swap = |event: Event| {
            // Everything is checked and the building has begun: `Data\Old`, which the patch
            // deletes, and `Data\New`, which it fills, become links to elsewhere.
            if begins(&event, Stage::Rebuild) {
                fs::remove_dir_all(case.install.join("Data").join("Old")).unwrap();
                junction(&case.install.join("Data").join("Old"), &case.outside);
                junction(&case.install.join("Data").join("New"), &case.outside);
            }
        };
        let result = if immediate { case.run_on(&drive, &mut swap) } else { case.run(&mut swap) };
        // The folder that is deleted goes as the link it now is. The one that is written
        // into is refused.
        assert!(leads_outside(&result), "immediate {immediate}: {result:?}");
        case.assert_nothing_outside_touched(&format!("immediate {immediate}"));
    }
}

// ---------------------------------------------------------------------------------------
// The lock file.
// ---------------------------------------------------------------------------------------

#[test]
fn lock_file_that_is_stale_or_something_else_entirely_does_no_harm() {
    let case = case(Layout::Table);
    let lock = case.install.join(".maple-fast-patch.lock");

    // Left by a run that was killed: nobody holds it, and the next run takes it.
    fs::write(&lock, b"").unwrap();
    case.run(&mut |_| {}).unwrap();
    case.assert_patched("after a stale lock file");
    assert!(!lock.exists(), "the lock file was left behind");

    // A folder of that name, and a link to a folder elsewhere: the run cannot have the
    // lock, says so, and touches nothing.
    for linked in [false, true] {
        let case = self::case(Layout::Table);
        let lock = case.install.join(".maple-fast-patch.lock");
        if linked {
            junction(&lock, &case.outside)
        } else {
            fs::create_dir(&lock).unwrap()
        }
        let result = case.run(&mut |_| {});
        assert!(result.is_err(), "linked {linked}: {result:?}");
        case.assert_untouched(&format!("linked {linked}"));
        case.assert_nothing_outside_touched(&format!("linked {linked}"));
        assert!(fs::symlink_metadata(&lock).is_ok(), "linked {linked}: what stood in for the lock file was removed");
        fs::remove_dir(&lock).unwrap();
        case.run(&mut |_| {}).unwrap();
        case.assert_patched(&format!("linked {linked}, once it was taken away"));
    }

    // A link to a file elsewhere, and one to a file that is not there. Such links take an
    // administrator to make; where the test may not make one, it has nothing to try.
    for dangling in [false, true] {
        let case = self::case(Layout::Table);
        let lock = case.install.join(".maple-fast-patch.lock");
        let target = case.outside.join(if dangling { "not-there.txt" } else { "keep.txt" });
        if !file_link(&lock, &target) {
            eprintln!("links to files cannot be made here: that part was not tried");
            return;
        }
        let _ = case.run(&mut |_| {});
        // Neither opened through the link, nor made through it.
        case.assert_nothing_outside_touched(&format!("dangling {dangling}"));
    }
}

// ---------------------------------------------------------------------------------------
// Files that change under the run.
// ---------------------------------------------------------------------------------------

/// What something else does to a file of the install while a run is going on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Meddling {
    Change,
    Grow,
    Shrink,
    Remove,
    MakeFolder,
}

const MEDDLINGS: [Meddling; 5] =
    [Meddling::Change, Meddling::Grow, Meddling::Shrink, Meddling::Remove, Meddling::MakeFolder];

fn meddle(path: &Path, how: Meddling) {
    // A file that is not there yet is one that something else got to first.
    let mut contents = fs::read(path).unwrap_or_else(|_| b"somebody else was here first".to_vec());
    match how {
        Meddling::Change => contents[10] ^= 0xFF,
        Meddling::Grow => contents.extend(b"more"),
        Meddling::Shrink => contents.truncate(contents.len() / 2),
        Meddling::Remove | Meddling::MakeFolder => {}
    }
    let _ = fs::remove_file(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    match how {
        Meddling::Remove => {}
        Meddling::MakeFolder => fs::create_dir_all(path.join("in")).unwrap(),
        _ => fs::write(path, contents).unwrap(),
    }
}

/// Puts back what [`meddle`] did to the file `name`.
fn mend(case: &Case, name: &str) {
    let path = on_disk(&case.install, name);
    if path.is_dir() {
        fs::remove_dir_all(&path).unwrap();
    }
    fs::write(path, &case.before[name]).unwrap();
}

#[test]
fn original_that_changes_between_the_check_and_the_build_ends_the_run_with_every_file_whole() {
    for layout in LAYOUTS {
        for how in MEDDLINGS {
            for immediate in [false, true] {
                let what = format!("{layout:?}, {how:?}, immediate {immediate}");
                let case = case(layout);
                let drive = case.tight_drive();
                // B is copied from (in the table layout by A as well) and was just found
                // to be the file the patch was made for.
                let mut done = false;
                let mut tamper = |event: Event| {
                    if begins(&event, Stage::Rebuild) && !std::mem::replace(&mut done, true) {
                        meddle(&on_disk(&case.install, B), how);
                    }
                };
                let result = if immediate { case.run_on(&drive, &mut tamper) } else { case.run(&mut tamper) };
                let Err(err) = result else {
                    // The meddling reached no byte that is copied: bytes added behind the
                    // end, or one changed where no instruction reads. Every new file has
                    // the checksum the patch promises, and that is all a patch asks.
                    assert!(matches!(how, Meddling::Grow | Meddling::Change), "{what}: the run went through");
                    case.assert_patched(&what);
                    continue;
                };
                assert!(matches!(err, Error::NewFileMismatch(_) | Error::Io { .. }), "{what}: {err}");

                // B is what the meddling made of it. Every other file is whole, and in the
                // safe way it is the file it was.
                mend(&case, B);
                if immediate {
                    case.assert_whole(&what)
                } else {
                    case.assert_untouched(&what)
                }
                // With B put back, the run goes on from there and finishes.
                let again = if immediate { case.run_on(&drive, &mut |_| {}) } else { case.run(&mut |_| {}) };
                again.unwrap_or_else(|err| panic!("{what}: with the file put back: {err}"));
                case.assert_patched(&what);
            }
        }
    }
}

#[test]
fn files_that_change_between_the_build_and_the_apply_are_replaced_all_the_same_or_put_back() {
    // The new files are built. Then something else changes what they are about to take the
    // place of, removes what is about to be deleted, or sits down where a new file goes.
    for how in MEDDLINGS {
        for name in [A, GONE, FRESH] {
            let what = format!("{how:?} of {name}");
            let case = case(Layout::Table);
            let mut done = false;
            let mut tamper = |event: Event| {
                if begins(&event, Stage::Apply) && !std::mem::replace(&mut done, true) {
                    meddle(&on_disk(&case.install, name), how);
                }
            };
            let result = case.run(&mut tamper);
            let folder_in_the_way = how == Meddling::MakeFolder && name != GONE;
            match result {
                // A folder cannot be replaced by a file. Everything that was applied
                // before it is put back, and the folder is still what it was made.
                Err(Error::Io { .. }) if folder_in_the_way => {
                    assert!(on_disk(&case.install, name).join("in").is_dir(), "{what}: the folder in the way is gone");
                    fs::remove_dir_all(on_disk(&case.install, name)).unwrap();
                    if case.before.contains_key(name) {
                        fs::write(on_disk(&case.install, name), &case.before[name]).unwrap();
                    }
                    case.assert_untouched(&what);
                    case.run(&mut |_| {}).unwrap();
                }
                // Anything else was on its way out, and goes.
                Ok(_) if !folder_in_the_way => {}
                other => panic!("{what}: {other:?}"),
            }
            case.assert_patched(&what);
        }
    }
}

#[test]
fn file_held_open_by_another_program_stops_a_run_whole_and_letting_go_lets_it_finish() {
    // How the other program shares the file: with nobody; with readers; with readers and
    // writers; and with those and whoever would delete or move it, which is no hold at all.
    for sharing in [0u32, 1, 3, 7] {
        // And when it takes hold of a file that the patch rebuilds: before the run, or once
        // the files are checked.
        for from in [None, Some(Stage::Rebuild)] {
            for immediate in [false, true] {
                let what = format!("sharing {sharing}, from {from:?}, immediate {immediate}");
                let ended = run_with_a_file_held(A, sharing, from, immediate, &what);
                assert_eq!(ended.is_ok(), sharing == 7, "{what}: {ended:?}");
            }
        }
    }
    // A hold that comes when the apply has begun, past the last look at who holds what: on
    // a file that is replaced, and on one in a folder that is deleted, which cannot be
    // moved away with it however the file is shared. The apply waits two seconds for it,
    // gives up and puts back what it had done.
    for (name, sharing, immediate) in [(A, 1, false), (GONE, 7, false), (GONE, 7, true)] {
        let what = format!("{name} held from the apply on, sharing {sharing}, immediate {immediate}");
        let ended = run_with_a_file_held(name, sharing, Some(Stage::Apply), immediate, &what);
        assert!(matches!(ended, Err(Error::Io { .. })), "{what}: {ended:?}");
    }
}

/// Runs the patch while another program holds the file `name` open, sharing it as `sharing`
/// says, from the start or from when the run comes to `from`. A run that fails must leave
/// every file whole and finish once the file is let go. Returns how the first run ended.
fn run_with_a_file_held(
    name: &str,
    sharing: u32,
    from: Option<Stage>,
    immediate: bool,
    what: &str,
) -> Result<Outcome, Error> {
    let case = case(Layout::Table);
    let drive = case.tight_drive();
    let hold = || fs::OpenOptions::new().read(true).share_mode(sharing).open(on_disk(&case.install, name)).unwrap();
    let mut held = from.is_none().then(hold);
    let mut take_hold = |event: Event| {
        if from.is_some_and(|stage| begins(&event, stage)) && held.is_none() {
            held = Some(hold());
        }
    };
    let ended = if immediate { case.run_on(&drive, &mut take_hold) } else { case.run(&mut take_hold) };
    if let Err(err) = &ended {
        assert!(matches!(err, Error::Blocked(_) | Error::Io { .. }), "{what}: {err}");
        // Let go first: the test reads the files too, and the hold may not allow it.
        drop(held.take());
        if immediate {
            case.assert_whole(what)
        } else {
            case.assert_untouched(what)
        }
        let again = if immediate { case.run_on(&drive, &mut |_| {}) } else { case.run(&mut |_| {}) };
        again.unwrap_or_else(|err| panic!("{what}: with the file let go: {err}"));
    }
    drop(held);
    case.assert_patched(what);
    ended
}

#[test]
fn read_only_hidden_and_system_files_are_replaced_and_deleted_like_any_other() {
    for immediate in [false, true] {
        let case = case(Layout::Table);
        // One that is rebuilt, one that is only copied from and then rebuilt, one that goes
        // with its folder, and the folder itself.
        for name in [A, B, GONE, MORE, r"Data\Old"] {
            let marked = std::process::Command::new("attrib")
                .args(["+R", "+H", "+S"])
                .arg(on_disk(&case.install, name))
                .output()
                .unwrap();
            assert!(marked.status.success(), "{name}: {}", String::from_utf8_lossy(&marked.stdout));
        }
        case.run_way(immediate, &mut |_| {}).unwrap_or_else(|err| panic!("immediate {immediate}: {err}"));
        case.assert_patched(&format!("immediate {immediate}"));
    }
}

// ---------------------------------------------------------------------------------------
// What an earlier run, or something else, left in the work folder.
// ---------------------------------------------------------------------------------------

#[test]
fn work_folder_of_things_the_engine_does_not_know_does_not_stop_a_run_or_survive_it() {
    type Plant<'a> = &'a dyn Fn(&Dirs);
    let plants: [(&str, Plant); 7] = [
        ("a file where the work folder should be", &|dirs| fs::write(dirs.work(), b"not a folder").unwrap()),
        ("a file the engine does not know", &|dirs| fs::write(dirs.work().join("from-a-later-version"), b"?").unwrap()),
        ("a folder the engine does not know", &|dirs| {
            fs::create_dir_all(dirs.work().join("later").join("deep")).unwrap()
        }),
        ("a file where the staged files should be", &|dirs| fs::write(dirs.staged(), b"not a folder").unwrap()),
        ("a file where the backups should be", &|dirs| fs::write(dirs.backup(), b"not a folder").unwrap()),
        ("backups without a journal", &|dirs| {
            fs::create_dir_all(dirs.backup().join("0").join("deep")).unwrap();
            fs::write(dirs.backup().join("1"), b"left over").unwrap();
        }),
        ("an empty journal", &|dirs| fs::write(dirs.journal(), b"").unwrap()),
    ];
    for (what, plant) in plants {
        for immediate in [false, true] {
            let case = case(Layout::Table);
            if !what.starts_with("a file where the work folder") {
                fs::create_dir_all(case.work()).unwrap();
            }
            plant(&case.dirs());
            case.run_way(immediate, &mut |_| {}).unwrap_or_else(|err| panic!("{what}, immediate {immediate}: {err}"));
            case.assert_patched(&format!("{what}, immediate {immediate}"));
        }
    }
}

/// Journals that cannot be read: lines that are cut short, garbage, places outside the
/// install folder, and lines with more to them than the engine writes. `outside` is a
/// folder beside the install folder.
///
/// Every one of them ends in a line break. A journal whose last line has none was cut off
/// while that line was being written, which is what a power cut leaves and says nothing
/// against the journal: see the test of that further down.
fn unreadable_journals(outside: &Path) -> Vec<Vec<u8>> {
    let keep = outside.join("keep.txt").to_string_lossy().into_owned();
    let fixed: [&[u8]; 23] = [
        // Cut short: the kind of a line, and not all that belongs to it.
        b"B\t0\n",
        b"B\n",
        b"N\n",
        b"D\n",
        b"\n",
        // Garbage, and lines of a kind this version does not write.
        b"X\tData\\A.wz\n",
        b"from a later version of the engine\n",
        b"\xFF\xFE\x00garbage\n",
        b"B\tnot-a-number\tData\\A.wz\n",
        "\u{FEFF}N\tData\\A.wz\n".as_bytes(),
        b"N\tData\\A.wz\r\n",
        // A place outside the install folder, or one that is the engine's own.
        b"B\t0\t..\\Elsewhere\\keep.txt\n",
        b"N\t..\\Elsewhere\\keep.txt\n",
        b"D\t..\\Elsewhere\n",
        b"N\tData\\..\\..\\Elsewhere\\keep.txt\n",
        b"B\t0\t.maple-fast-patch.lock\n",
        b"N\tCON\n",
        // More to a line than the engine writes.
        b"B\t0\tData\\A.wz\textra\n",
        b"N\tData\\A.wz\textra\n",
        b"D\tData\\New\textra\n",
        // One such line among good ones: first, in the middle, last.
        b"X\nN\tData\\New\\Fresh.wz\nB\t0\tData\\A.wz\n",
        b"N\tData\\New\\Fresh.wz\nX\nB\t0\tData\\A.wz\n",
        b"N\tData\\New\\Fresh.wz\nB\t0\tData\\A.wz\nX\n",
    ];
    let mut journals: Vec<Vec<u8>> = fixed.iter().map(|journal| journal.to_vec()).collect();
    let named = [format!("B\t0\t{keep}\n"), format!("N\t{keep}\n"), format!("N\t\\\\?\\{keep}\n")];
    journals.extend(named.map(String::into_bytes));
    journals
}

/// Whether `problems` are the one of a journal that cannot be read, in the work folder `work`,
/// with a word on what could not be read.
fn is_unreadable_journal(problems: &[Problem], work: &Path) -> bool {
    matches!(problems, [Problem::UnreadableJournal { dir, detail }] if dir == work && !detail.is_empty())
}

/// Whether a refusal is the one for a journal that cannot be read, in the work folder `work`.
fn names_unreadable_journal<T>(result: &Result<T, Error>, work: &Path) -> bool {
    matches!(result, Err(Error::Blocked(problems)) if is_unreadable_journal(problems, work))
}

#[test]
fn journal_that_cannot_be_read_is_told_of_as_that_wherever_it_is_read() {
    // A journal says that an apply was cut off and files of the install lie among the
    // backups. One that cannot be read cannot be undone, and nothing may go on over it:
    // every place that reads it says so, in the same words, and changes nothing.
    for index in 0.. {
        let case = case(Layout::Table);
        let journals = unreadable_journals(&case.outside);
        let Some(journal) = journals.get(index) else { break };
        let what = format!("{:?}", String::from_utf8_lossy(journal));
        let (dirs, work) = (case.dirs(), case.work());
        write_files(&dirs.backup(), &[("0".to_owned(), b"an original that was moved aside".to_vec())].into());
        fs::write(dirs.journal(), journal).unwrap();
        let nothing_changed = |after: &str| {
            case.assert_untouched(&format!("{what}, after {after}"));
            case.assert_nothing_outside_touched(&format!("{what}, after {after}"));
            assert!(fs::read(dirs.journal()).unwrap() == *journal, "{what}: the journal was changed by {after}");
            assert!(dirs.backup().join("0").is_file(), "{what}: the backup was touched by {after}");
        };

        // The look the app takes at a folder, before anything is asked of it.
        let seen = preflight::check_leftovers(&case.install);
        assert!(is_unreadable_journal(&seen, &work), "{what}: {seen:?}");
        nothing_changed("a look");
        // The undoing that the start of the app asks for.
        let recovered = run::recover(&case.install);
        assert!(names_unreadable_journal(&recovered, &work), "{what}: {recovered:?}");
        nothing_changed("a recover");
        // The start of a run, in either way.
        for immediate in [false, true] {
            let result = case.run_way(immediate, &mut |_| {});
            assert!(names_unreadable_journal(&result, &work), "{what}, immediate {immediate}: {result:?}");
            nothing_changed("a run");
        }
        // And the ordinary discarding of the work, which undoes before it removes.
        let discarded = run::discard_work(&case.install);
        assert!(names_unreadable_journal(&discarded, &work), "{what}: {discarded:?}");
        nothing_changed("discarding the work");
    }
}

#[test]
fn journal_with_nothing_but_a_line_that_was_never_finished_says_nothing() {
    // No line break, no line: whatever stands there was being written when the power went,
    // and its step never began. Such a journal is undone by taking it away.
    for journal in [&b""[..], b"N", b"B\t0\tData\\A.w", b"\xFF\xFE\x00garbage", &[0; 4096]] {
        let what = format!("{:?}", String::from_utf8_lossy(&journal[..journal.len().min(20)]));
        let case = case(Layout::Table);
        fs::create_dir_all(case.work()).unwrap();
        fs::write(case.dirs().journal(), journal).unwrap();
        let seen = preflight::check_leftovers(&case.install);
        assert!(matches!(&seen[..], [Problem::LeftoverWork { .. }]), "{what}: {seen:?}");
        assert_eq!(run::recover(&case.install).unwrap(), Leftover::RolledBack, "{what}");
        case.assert_untouched(&what);
        case.run(&mut |_| {}).unwrap();
        case.assert_patched(&what);
    }

    // The same behind lines that were finished: those are undone, and the rest is dropped.
    let case = case(Layout::Table);
    let (plan, checked) = build(&case);
    assert!(apply::apply_then_crash(&plan, &checked, &case.dirs(), usize::MAX).unwrap());
    let mut journal = fs::read(case.dirs().journal()).unwrap();
    journal.extend(b"B\t99\tData\\Sta");
    fs::write(case.dirs().journal(), journal).unwrap();
    assert!(preflight::check_leftovers(&case.install) == [Problem::LeftoverWork { dir: case.work() }]);
    assert_eq!(run::recover(&case.install).unwrap(), Leftover::RolledBack);
    case.assert_untouched("a journal that ends in an unfinished line");
}

/// Cuts the apply of `case` off after `cut` of its moments, with everything built, and
/// makes its journal one that cannot be read by writing `journal` over it. Returns whether
/// the apply had done all its steps by then.
fn cut_off_with_journal(case: &Case, cut: usize, journal: &[u8]) -> bool {
    let (plan, checked) = build(case);
    let finished = apply::apply_then_crash(&plan, &checked, &case.dirs(), cut).unwrap();
    fs::write(case.dirs().journal(), journal).unwrap();
    finished
}

#[test]
fn knowing_discard_removes_a_work_folder_whose_journal_cannot_be_read_and_nothing_else() {
    for index in 0.. {
        let case = case(Layout::Table);
        let journals = unreadable_journals(&case.outside);
        let Some(journal) = journals.get(index) else { break };
        let what = format!("{:?}", String::from_utf8_lossy(journal));
        // An apply that got some of the way: new files in place, originals among the
        // backups, built files still waiting, and a patch file that was downloaded.
        cut_off_with_journal(&case, 9, journal);
        let (dirs, work) = (case.dirs(), case.work());
        write_files(&dirs.download(), &[("some.patch".to_owned(), b"a patch file".to_vec())].into());
        // And links that lead out of the install folder, wherever one could sit.
        fs::create_dir_all(dirs.backup()).unwrap();
        for link in [work.join("linked"), dirs.staged().join("Linked"), dirs.backup().join("77")] {
            junction(&link, &case.outside);
        }
        let left_by_the_apply = case.files();
        assert!(left_by_the_apply != case.before && left_by_the_apply != case.after, "{what}: the apply left no mix");

        // The ordinary way refuses, and leaves everything where it is.
        let ordinary = run::discard_work(&case.install);
        assert!(matches!(ordinary, Err(Error::Blocked(_))), "{what}: {ordinary:?}");
        assert!(
            dirs.journal().is_file() && case.files() == left_by_the_apply,
            "{what}: refused, and changed something"
        );

        // The knowing way takes the work folder, and only that.
        run::discard_work_as_it_is(&case.install).unwrap_or_else(|err| panic!("{what}: {err}"));
        assert!(fs::symlink_metadata(&work).is_err(), "{what}: the work folder is still there");
        assert!(case.files() == left_by_the_apply, "{what}: a file of the game was touched");
        assert!(
            files_under(&case.install) == left_by_the_apply,
            "{what}: something else is left in the install folder"
        );
        case.assert_nothing_outside_touched(&what);
        assert!(preflight::check_leftovers(&case.install).is_empty(), "{what}: the folder still reports leftovers");
    }
}

#[test]
fn knowing_discard_is_the_ordinary_one_wherever_the_ordinary_one_works() {
    // A journal that can be read is undone first: the folder is as it was before the apply.
    let case = case(Layout::Table);
    let (plan, checked) = build(&case);
    assert!(apply::apply_then_crash(&plan, &checked, &case.dirs(), usize::MAX).unwrap());
    run::discard_work_as_it_is(&case.install).unwrap();
    case.assert_untouched("after a journal that could be read");
    assert!(fs::symlink_metadata(case.work()).is_err());

    // Work that was only built goes, and so does a folder with no work in it.
    build(&case);
    run::discard_work_as_it_is(&case.install).unwrap();
    run::discard_work_as_it_is(&case.install).unwrap();
    case.assert_untouched("after work that was only built");
    assert!(fs::symlink_metadata(case.work()).is_err());

    // A work folder that is a link goes as the link, whatever the journal at its other end.
    let mut case = self::case(Layout::Table);
    fs::write(case.outside.join("applying.log"), b"X\n").unwrap();
    case.beside = files_under(&case.outside);
    junction(&case.work(), &case.outside);
    run::discard_work_as_it_is(&case.install).unwrap();
    assert!(fs::symlink_metadata(case.work()).is_err());
    case.assert_nothing_outside_touched("after a linked work folder");

    // While a run holds the folder, nothing is discarded in either way.
    let case = self::case(Layout::Table);
    cut_off_with_journal(&case, 9, b"X\n");
    let held = run::InstallLock::acquire(&case.dirs()).unwrap();
    let refused = run::discard_work_as_it_is(&case.install);
    assert!(
        matches!(&refused, Err(Error::Blocked(problems)) if problems[..] == [Problem::AlreadyRunning]),
        "{refused:?}"
    );
    assert!(case.dirs().journal().is_file());
    drop(held);
}

#[test]
fn after_the_knowing_discard_a_patch_goes_by_the_files_as_they_then_are() {
    // The apply is cut off at each of its moments, its journal made unreadable, and the
    // work folder removed as it is. What is left is a mix of old and new files, for good.
    for (layout, b_first) in [(Layout::Table, false), (Layout::Table, true), (Layout::PerEntry, true)] {
        let (mut went_through, mut refused) = (0, 0);
        for cut in 0.. {
            let what = format!("{layout:?}, B first: {b_first}, cut at {cut}");
            let case = case_in_order(layout, b_first);
            let finished = cut_off_with_journal(&case, cut, b"from a later version of the engine\n");
            // Putting the folder back was tried before, and could not be done.
            fs::write(case.work().join("cannot-restore"), "a backup was not the file the patch was made for").unwrap();
            run::discard_work_as_it_is(&case.install).unwrap_or_else(|err| panic!("{what}: {err}"));
            let mix = case.files();
            case.assert_whole(&what);

            // A patch that was made for other files than these is refused by the check of
            // the originals, and changes nothing.
            let other = case.patch.with_extension("other");
            let mut for_other_files = PatchBuilder::new(Layout::Table);
            for_other_files.original(A, b"another file").rebuild(A, b"another file", b"never built", &[copy(A, 0, 10)]);
            for_other_files.write(&other);
            let result = support::run(&support::options(&case.install, &other), &GO, &mut |_| {});
            assert!(matches!(&result, Err(Error::OriginalMismatch(files)) if files[..] == [A]), "{what}: {result:?}");
            assert!(case.files() == mix, "{what}: a patch that was refused changed something");

            // The patch that was cut off goes by the same check. Where every file it still
            // has to build can be built from what is there, it goes through and the folder
            // is the new version. Where its own apply has replaced an original that a file
            // still to be built copies from, it is refused like any other.
            match case.run(&mut |_| {}) {
                Ok(_) => {
                    went_through += 1;
                    case.assert_patched(&what);
                }
                Err(Error::OriginalMismatch(files)) => {
                    refused += 1;
                    assert!(files == [B] && case.files() == mix, "{what}: {files:?}");
                }
                Err(err) => panic!("{what}: {err}"),
            }
            if finished {
                break;
            }
        }
        // Only where B is replaced first and A is built from the old B is there a moment
        // at which the patch no longer fits what it has itself left.
        let has_such_a_moment = layout == Layout::Table && b_first;
        assert!(
            went_through > 0 && (refused > 0) == has_such_a_moment,
            "{layout:?}, {b_first}: {went_through} / {refused}"
        );
    }
}

#[test]
fn journal_that_names_places_outside_moves_nothing_there_and_nothing_from_there() {
    let case = case(Layout::Table);
    let keep = case.outside.join("keep.txt").to_string_lossy().into_owned();
    let lines = [
        "B\t0\t..\\Elsewhere\\keep.txt\n".to_owned(),
        "N\t..\\Elsewhere\\keep.txt\n".to_owned(),
        "D\t..\\Elsewhere\\sub\n".to_owned(),
        "D\t..\\Elsewhere\n".to_owned(),
        format!("B\t0\t{keep}\n"),
        format!("N\t{keep}\n"),
        format!("N\t\\\\?\\{keep}\n"),
        "N\tData\\..\\..\\Elsewhere\\keep.txt\n".to_owned(),
        "B\t..\\..\\Elsewhere\\keep.txt\tData\\A.wz\n".to_owned(),
        "B\t0\t.maple-fast-patch.lock\n".to_owned(),
        "N\tCON\n".to_owned(),
    ];
    for line in lines {
        let dirs = case.dirs();
        fs::create_dir_all(dirs.backup()).unwrap();
        fs::write(dirs.backup().join("0"), b"planted").unwrap();
        fs::write(dirs.journal(), &line).unwrap();
        let result = run::recover(&case.install);
        assert!(names_unreadable_journal(&result, &dirs.work()), "{line:?}: {result:?}");
        case.assert_nothing_outside_touched(&format!("{line:?}"));
        case.assert_untouched(&format!("{line:?}"));
        assert!(!case.outside.join("0").exists() && fs::read(dirs.backup().join("0")).unwrap() == b"planted");
        fs::remove_dir_all(dirs.work()).unwrap();
    }
}

#[test]
fn marker_of_a_half_patched_folder_that_says_nothing_sensible_keeps_every_patch_out_until_the_work_is_discarded() {
    for marker in [&b""[..], b"not an id", b"\xFF\xFE\x00\x01", &[b'9'; 100_000]] {
        let case = case(Layout::Table);
        fs::create_dir_all(case.work()).unwrap();
        fs::write(case.dirs().half_patched(), marker).unwrap();
        for immediate in [false, true] {
            let result = case.run_way(immediate, &mut |_| {});
            assert!(
                matches!(&result, Err(Error::Blocked(_))),
                "{} bytes, immediate {immediate}: {result:?}",
                marker.len()
            );
            case.assert_untouched("under a marker that says nothing");
        }
        run::discard_work(&case.install).unwrap();
        case.run(&mut |_| {}).unwrap();
        case.assert_patched("after the work was discarded");
    }
}

// ---------------------------------------------------------------------------------------
// No room.
// ---------------------------------------------------------------------------------------

#[test]
fn drive_that_runs_out_of_room_at_any_question_stops_the_run_whole_and_room_lets_it_finish() {
    // The engine asks the drive how much is free before every step that needs room. Here
    // the drive answers truthfully up to one of those questions, and from then on says
    // that nothing is free. What cannot be tried this way is a drive that has room when it
    // is asked and none when it is written to: see the report of this campaign.
    for immediate in [false, true] {
        for full_from in 0.. {
            let what = format!("immediate {immediate}, no room from question {full_from}");
            let case = case(Layout::Table);
            let drive = case.tight_drive();
            let asked = AtomicUsize::new(0);
            let free = |_: &Path| -> io::Result<u64> {
                let roomy = if immediate { drive.free() } else { u64::MAX / 2 };
                Ok(if asked.fetch_add(1, Ordering::Relaxed) >= full_from { 0 } else { roomy })
            };
            let disk = Disk { free: &free, slack: 0, file_system: Some("NTFS") };
            let result = run_on_disk(&LIVE, &case.options(immediate), &disk, &GO, &mut |_| {});
            let questions = asked.load(Ordering::Relaxed);
            match result {
                // The run is through: nothing it still asked about needed room. Once it
                // is through without having been told of a full drive at all, every
                // question it has was tried.
                Ok(_) => {
                    case.assert_patched(&what);
                    if questions <= full_from {
                        break;
                    }
                }
                Err(err) => {
                    assert!(
                        matches!(&err, Error::Blocked(problems) if format!("{problems:?}").contains("NotEnoughSpace")),
                        "{what}: {err}"
                    );
                    if half_patched(&case.install) {
                        case.assert_whole(&what)
                    } else {
                        case.assert_untouched(&what)
                    }
                    let roomy =
                        |_: &Path| -> io::Result<u64> { Ok(if immediate { drive.free() } else { u64::MAX / 2 }) };
                    let disk = Disk { free: &roomy, slack: 0, file_system: Some("NTFS") };
                    run_on_disk(&LIVE, &case.options(immediate), &disk, &GO, &mut |_| {})
                        .unwrap_or_else(|err| panic!("{what}: with room again: {err}"));
                    case.assert_patched(&what);
                }
            }
            assert!(full_from < 200, "{what}: the run never stops asking");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Being killed.
// ---------------------------------------------------------------------------------------

/// Reads the patch of `case`, checks the install against it and builds every new file, as a
/// run does before its apply.
fn build(case: &Case) -> (plan::Plan, stage::Checked) {
    let patch = PatchFile::open(&case.patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, &case.install, &GO, &|_, _| {}).unwrap();
    stage::stage(&patch, &plan, &checked, &case.dirs(), &GO, &mut |_, _, _| {}).unwrap();
    (plan, checked)
}

#[test]
fn undo_of_an_apply_that_is_killed_at_any_moment_is_finished_by_the_next_start() {
    for layout in LAYOUTS {
        // The apply is cut off at each of its moments in turn...
        for cut in 0.. {
            let case = case(layout);
            let (plan, checked) = build(&case);
            let dirs = case.dirs();
            let finished = apply::apply_then_crash(&plan, &checked, &dirs, cut).unwrap();

            // ...and its undoing is killed at every one of its own, each time one moment
            // further on than the last. The next start carries on from wherever that was.
            let mut kills = 0;
            while !apply::undo_then_crash(&dirs, kills).unwrap() {
                kills += 1;
                assert!(kills < 1000, "{layout:?}, cut {cut}: the undoing never ends");
            }
            case.assert_untouched(&format!("{layout:?}, apply cut at {cut}, undo killed {kills} times"));
            assert!(!dirs.journal().exists() && !dirs.backup().exists(), "{layout:?}, cut {cut}");
            // Nothing is left that a start would have to undo, and the patch goes on from
            // the files that were built.
            assert_eq!(apply::recover(&dirs).unwrap(), Leftover::Staged, "{layout:?}, cut {cut}");
            case.run(&mut |_| {}).unwrap();
            case.assert_patched(&format!("{layout:?}, apply cut at {cut}"));
            if finished {
                break;
            }
        }
    }
}

#[test]
fn undo_killed_at_one_moment_and_left_to_the_next_start_puts_everything_back() {
    let layout = Layout::Table;
    // The apply got through every step and died before it was marked finished: the most
    // there can be to undo. The undoing is killed once, at each of its moments in turn,
    // and the next start has to finish it in one go.
    for kill_at in 0.. {
        let case = case(layout);
        let (plan, checked) = build(&case);
        let dirs = case.dirs();
        assert!(apply::apply_then_crash(&plan, &checked, &dirs, usize::MAX).unwrap());
        let undone = apply::undo_then_crash(&dirs, kill_at).unwrap();
        if !undone {
            assert_eq!(run::recover(&case.install).unwrap(), Leftover::RolledBack, "killed at {kill_at}");
        }
        case.assert_untouched(&format!("undo killed at {kill_at}"));
        case.run(&mut |_| {}).unwrap();
        case.assert_patched(&format!("undo killed at {kill_at}"));
        if undone {
            assert!(kill_at > 10, "the undoing has only {kill_at} moments");
            break;
        }
    }
}

#[test]
fn kill_while_the_work_folder_of_a_finished_apply_is_removed_leaves_a_patched_folder() {
    // The removal takes the journal first, then the marker of a half-patched folder, then
    // everything else, and the mark of the finished apply last. These are the states a
    // kill can leave on the way, each of which the next start has to read as "patched".
    type Left = &'static [&'static str];
    let states: [Left; 11] = [
        &["journal", "marker", "new", "backup", "aside", "download", "committed"],
        &["journal", "aside", "committed"],
        &["aside", "committed"],
        &["journal", "marker", "new", "backup", "download", "committed"],
        &["marker", "new", "backup", "download", "committed"],
        &["new", "backup", "download", "committed"],
        &["backup", "download", "committed"],
        &["download", "committed"],
        &["committed"],
        &[],
        &["download"],
    ];
    for left in states {
        for immediate in [false, true] {
            let what = format!("{left:?}, immediate {immediate}");
            let case = case(Layout::Table);
            let id = PatchFile::open(&case.patch).unwrap().id();
            case.run_way(immediate, &mut |_| {}).unwrap();
            let dirs = case.dirs();
            fs::create_dir_all(dirs.work()).unwrap();
            for thing in left {
                match *thing {
                    // A journal that would move the new files out again if it were obeyed.
                    "journal" => fs::write(dirs.journal(), "N\tData\\New\\Fresh.wz\nB\t0\tData\\A.wz\n").unwrap(),
                    "marker" => fs::write(dirs.half_patched(), &id).unwrap(),
                    "new" => write_files(&dirs.staged(), &[(STAY.to_owned(), b"stale".to_vec())].into()),
                    "backup" => write_files(&dirs.backup(), &[("0".to_owned(), case.before[A].clone())].into()),
                    // The old A, kept under its name as an apply keeps what it moves aside.
                    "aside" => write_files(&dirs.aside(), &[(format!("0\\{A}"), case.before[A].clone())].into()),
                    "download" => {
                        write_files(&dirs.download(), &[("old.patch".to_owned(), b"a patch".to_vec())].into())
                    }
                    _ => fs::write(dirs.work().join("committed"), b"").unwrap(),
                }
            }

            assert_eq!(run::recover(&case.install).unwrap(), Leftover::None, "{what}");
            assert!(!half_patched(&case.install), "{what}: the folder counts as half patched");
            // The same patch finds nothing left to do, in either way, and leaves no work.
            let again = case.run_way(immediate, &mut |_| {}).unwrap_or_else(|err| panic!("{what}: {err}"));
            assert_eq!(again.files_changed, 0, "{what}");
            case.assert_patched(&what);
        }
    }
}

#[test]
fn backups_that_a_finished_apply_could_not_remove_are_not_in_the_way_of_the_next_patch() {
    // Removing the work folder of a finished apply gives up quietly on what it cannot
    // remove. What is left of the backups must not be taken for backups of the next apply.
    let case = case(Layout::Table);
    let dirs = case.dirs();
    fs::create_dir_all(dirs.backup().join("0").join("deep")).unwrap();
    fs::write(dirs.backup().join("0").join("deep").join("old.wz"), b"left over").unwrap();
    fs::create_dir_all(dirs.backup().join("1")).unwrap();
    fs::write(dirs.backup().join("2"), b"left over").unwrap();
    // The folder holds the same patch as built, so the work folder is this patch's own.
    let (plan, checked) = build(&case);
    fs::create_dir_all(dirs.backup().join("0").join("deep")).unwrap();
    fs::write(dirs.backup().join("0").join("deep").join("old.wz"), b"left over").unwrap();
    apply::apply(&plan, &checked, &dirs, &mut |_| {}).unwrap();
    case.assert_patched("with backups of an earlier apply in the work folder");
}

/// Runs the patch and dies at the `at`-th thing the run tells of: the run is torn down
/// from inside whatever it was doing, with no chance to tidy up. Returns `None` if the run
/// was through before it had that much to tell.
fn die_at(case: &Case, drive: Option<&Drive>, at: usize) -> Option<()> {
    let mut told = 0;
    let died = catch_unwind(AssertUnwindSafe(|| {
        let mut count = |_: Event| {
            told += 1;
            if told > at {
                std::panic::resume_unwind(Box::new("killed"));
            }
        };
        match drive {
            Some(drive) => case.run_on(drive, &mut count),
            None => case.run(&mut count),
        }
    }));
    match died {
        Err(_) => Some(()),
        Ok(result) => {
            result.unwrap();
            None
        }
    }
}

#[test]
fn run_that_dies_at_any_moment_leaves_every_file_whole_and_the_next_run_finishes() {
    for layout in LAYOUTS {
        for immediate in [false, true] {
            for at in 0.. {
                let what = format!("{layout:?}, immediate {immediate}, died at {at}");
                let case = case(layout);
                let drive = immediate.then(|| case.tight_drive());
                let died = die_at(&case, drive.as_ref(), at);
                if died.is_some() {
                    // In the safe way the folder is the old one until the apply, and an
                    // apply that died is undone by the next start.
                    case.assert_whole(&what);
                    let again = match &drive {
                        Some(drive) => case.run_on(drive, &mut |_| {}),
                        None => case.run(&mut |_| {}),
                    };
                    again.unwrap_or_else(|err| panic!("{what}: the next run: {err}"));
                }
                case.assert_patched(&what);
                if died.is_none() {
                    assert!(at > 20, "{what}: the run tells of only {at} things");
                    break;
                }
            }
        }
    }
}

#[test]
fn link_that_an_apply_moved_aside_is_moved_back_when_the_apply_is_undone() {
    // The patch deletes a folder of the install that is a link. The apply moves it aside as
    // the link it is, and dies. Undoing it finds a link among its backups, which is what it
    // put there, and puts it back.
    let case = case_with_link(|patch| _ = patch.delete(r"Data\Link\"));
    let link = case.install.join("Data").join("Link");
    let (plan, checked) = build(&case);
    assert!(apply::apply_then_crash(&plan, &checked, &case.dirs(), usize::MAX).unwrap());
    assert!(fs::symlink_metadata(&link).is_err(), "the link was not moved aside");

    assert_eq!(run::recover(&case.install).unwrap(), Leftover::RolledBack);
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link was not put back as a link");
    assert!(link.join("keep.txt").is_file(), "the link no longer leads where it led");
    case.assert_nothing_outside_touched("after the undo");
    case.assert_untouched("after the undo");
}

// ---------------------------------------------------------------------------------------
// A journal that cannot be read: the folder is put back from what the apply moved aside.
// ---------------------------------------------------------------------------------------

/// Puts the patch file of `case` among the downloads in its work folder, which is where
/// the app keeps it, and where a start of the app that has no run to ask finds it.
fn patch_among_downloads(case: &Case) {
    write_files(&case.dirs().download(), &[("made-up.patch".to_owned(), fs::read(&case.patch).unwrap())].into());
}

/// Builds everything, cuts the apply of `case` off after `cut` of its moments, and makes
/// its journal `journal`, or takes the journal away. Returns whether the apply had done all
/// its steps by then.
fn cut_off_and_damage(case: &Case, cut: usize, journal: Option<&[u8]>) -> bool {
    let (plan, checked) = build(case);
    let finished = apply::apply_then_crash(&plan, &checked, &case.dirs(), cut).unwrap();
    match journal {
        Some(journal) => fs::write(case.dirs().journal(), journal).unwrap(),
        None => fs::remove_file(case.dirs().journal()).unwrap(),
    }
    finished
}

/// The file that the apply of `case` moved aside from the place `name`.
fn moved_aside(case: &Case, name: &str) -> PathBuf {
    let aside = case.dirs().aside();
    let kept = files_under(&aside).into_keys().find(|kept| kept.ends_with(name));
    aside.join(kept.unwrap_or_else(|| panic!("{name} was not moved aside")))
}

/// Fails unless the install folder of `case` is the one before the patch in everything: its
/// files byte for byte, the folder the patch makes gone, the folders it deletes back.
#[track_caller]
fn assert_as_before_the_patch(case: &Case, what: &str) {
    case.assert_untouched(what);
    assert!(!case.install.join("Data").join("New").exists(), "{what}: the folder the patch makes is still there");
    assert!(
        case.install.join("Data").join("Old").join("Deep").is_dir(),
        "{what}: a folder the patch deletes is not back"
    );
    let dirs = case.dirs();
    // Folders that held what was moved aside may be left, empty. They say nothing.
    let aside = files_under(&dirs.aside());
    assert!(!dirs.journal().exists() && aside.is_empty(), "{what}: traces of the apply are left");
}

#[test]
fn folder_whose_journal_is_damaged_is_put_back_byte_for_byte_and_proven() {
    for layout in LAYOUTS {
        let damaged = unreadable_journals(Path::new("anywhere")).len();
        for cut in 0.. {
            // Every damaged journal at one moment of the apply. At every other moment three
            // of them, and no journal at all.
            let picks: Vec<Option<usize>> = match cut {
                9 => (0..damaged).map(Some).chain([None]).collect(),
                _ => vec![Some(cut % damaged), Some((7 * cut + 3) % damaged), None],
            };
            let mut finished = false;
            for pick in picks {
                let case = case(layout);
                let journals = unreadable_journals(&case.outside);
                let journal = pick.map(|index| &journals[index][..]);
                finished = cut_off_and_damage(&case, cut, journal);
                patch_among_downloads(&case);
                let what = format!("{layout:?}, cut at {cut}, journal {:?}", journal.map(String::from_utf8_lossy));
                let anything_aside = !files_under(&case.dirs().aside()).is_empty();

                // The look that comes before anything was tried tells of work that is left,
                // as it does for a journal that can be read. There is no scare in it.
                let seen = preflight::check_leftovers(&case.install);
                assert!(matches!(&seen[..], [Problem::LeftoverWork { .. }]), "{what}: {seen:?}");

                let healed = run::recover(&case.install).unwrap_or_else(|err| panic!("{what}: {err}"));
                if journal.is_none() && !anything_aside {
                    // No journal and nothing moved aside: nothing says that an apply was
                    // ever begun here, and there is no original to put anywhere.
                    assert_eq!(healed, Leftover::Staged, "{what}");
                    case.assert_whole(&what);
                } else {
                    assert_eq!(healed, Leftover::Restored, "{what}");
                    assert_as_before_the_patch(&case, &what);
                }
                let seen = preflight::check_leftovers(&case.install);
                assert!(!matches!(&seen[..], [Problem::UnreadableJournal { .. }]), "{what}: {seen:?}");
                assert!(!run::cut_off(&case.install), "{what}: the folder still counts as cut off");

                // The patch applies as it would have. What was built is still there.
                case.run(&mut |_| {}).unwrap_or_else(|err| panic!("{what}: the patch again: {err}"));
                case.assert_patched(&what);
            }
            if finished {
                break;
            }
        }
    }
}

#[test]
fn backup_that_is_damaged_or_missing_is_found_and_nothing_is_called_restored() {
    for layout in LAYOUTS {
        // What was moved aside from A is not the old A any more.
        let case = case(layout);
        cut_off_and_damage(&case, usize::MAX, Some(b"X\n"));
        patch_among_downloads(&case);
        let kept = moved_aside(&case, A);
        let mut damaged = fs::read(&kept).unwrap();
        damaged[100] ^= 0x01;
        fs::write(&kept, &damaged).unwrap();
        let left_by_the_apply = case.files();
        let work = case.work();

        // It is found before anything is moved, and the folder is told of as one that
        // cannot be put back, by every place that looks.
        let tried = run::recover(&case.install);
        assert!(names_unreadable_journal(&tried, &work), "{layout:?}: {tried:?}");
        assert!(
            case.files() == left_by_the_apply && fs::read(&kept).unwrap() == damaged,
            "{layout:?}: something was moved"
        );
        assert!(is_unreadable_journal(&preflight::check_leftovers(&case.install), &work), "{layout:?}");
        for immediate in [false, true] {
            let result = case.run_way(immediate, &mut |_| {});
            assert!(names_unreadable_journal(&result, &work), "{layout:?}, immediate {immediate}: {result:?}");
        }
        assert!(names_unreadable_journal(&run::discard_work(&case.install), &work), "{layout:?}");
        assert!(case.files() == left_by_the_apply, "{layout:?}: a folder that cannot be put back was changed");
        // The last resort is still there, and takes the work folder as it is.
        run::discard_work_as_it_is(&case.install).unwrap();
        assert!(fs::symlink_metadata(&work).is_err() && case.files() == left_by_the_apply, "{layout:?}");

        // What was moved aside from A is gone. Everything else goes back, and the new A
        // stays where it is, being the only A there is. The folder is not the old version
        // and is not called restored. But nothing the patch still needs is missing: it is
        // a folder that the patch can be applied to, and that is what is said of it.
        let case = self::case(layout);
        cut_off_and_damage(&case, usize::MAX, Some(b"X\n"));
        patch_among_downloads(&case);
        fs::remove_file(moved_aside(&case, A)).unwrap();
        assert_eq!(run::recover(&case.install).unwrap(), Leftover::PutBack, "{layout:?}");
        let now = case.files();
        assert!(now[A] == case.after[A] && now[B] == case.before[B] && now.contains_key(GONE), "{layout:?}");
        assert!(matches!(&preflight::check_leftovers(&case.install)[..], [Problem::LeftoverWork { .. }]), "{layout:?}");
        case.run(&mut |_| {}).unwrap_or_else(|err| panic!("{layout:?}: the patch on what was put back: {err}"));
        case.assert_patched(&format!("{layout:?}, after a backup was missing"));
    }

    // What was moved aside from B is gone, and A, which is old again, is built from the
    // old B: the folder is not the old version, and the patch cannot be applied to it
    // either. That is a folder that cannot be put back.
    let case = case(Layout::Table);
    cut_off_and_damage(&case, usize::MAX, Some(b"X\n"));
    patch_among_downloads(&case);
    fs::remove_file(moved_aside(&case, B)).unwrap();
    let tried = run::recover(&case.install);
    assert!(names_unreadable_journal(&tried, &case.work()), "{tried:?}");
    assert!(is_unreadable_journal(&preflight::check_leftovers(&case.install), &case.work()));
    let now = case.files();
    assert!(now[A] == case.before[A] && now[B] == case.after[B] && now.contains_key(GONE));
}

#[test]
fn backup_of_a_file_the_patch_states_no_checksum_for_cannot_be_vouched_for() {
    // The patch deletes `Data\Old` with what is in it and says nothing of what that was.
    // What was moved aside from there goes back as it lies, and there is nothing to hold
    // it against: this is what "proven" does not cover.
    let case = case(Layout::Table);
    cut_off_and_damage(&case, usize::MAX, Some(b"X\n"));
    patch_among_downloads(&case);
    fs::write(moved_aside(&case, GONE), b"damaged while it lay aside").unwrap();
    assert_eq!(run::recover(&case.install).unwrap(), Leftover::Restored);
    assert_eq!(fs::read(on_disk(&case.install, GONE)).unwrap(), b"damaged while it lay aside");
}

#[test]
fn putting_back_that_is_killed_at_any_moment_is_finished_by_the_next_start() {
    for layout in LAYOUTS {
        // Killed once, at each of its moments in turn, and left to the next start.
        for kill_at in 0.. {
            let case = case(layout);
            cut_off_and_damage(&case, usize::MAX, Some(b"from a later version of the engine\n"));
            patch_among_downloads(&case);
            let done = apply::restore_then_crash(&case.dirs(), None, kill_at).unwrap();
            if !done {
                // No file is anything but wholly old or wholly new. One may be out of the
                // folder for the moment: the one that was being put back.
                let whole = |(name, contents): (&String, &Vec<u8>)| {
                    case.before.get(name) == Some(contents) || case.after.get(name) == Some(contents)
                };
                assert!(
                    case.files().iter().all(whole),
                    "{layout:?}, killed at {kill_at}: a file is neither old nor new"
                );
                // Killed after the journal went, in the last of its moments, it has left no
                // trace of the apply: everything was back and checked by then, and the
                // next start finds a folder like any other with built files in it.
                let finishes = if run::cut_off(&case.install) { Leftover::Restored } else { Leftover::Staged };
                assert_eq!(run::recover(&case.install).unwrap(), finishes, "{layout:?}, killed at {kill_at}");
            }
            assert_as_before_the_patch(&case, &format!("{layout:?}, killed at {kill_at}"));
            if done {
                assert!(kill_at > 10, "{layout:?}: putting back has only {kill_at} moments");
                break;
            }
        }

        // Killed again and again in one folder, each time a moment later than the last.
        let case = case(layout);
        cut_off_and_damage(&case, usize::MAX, None);
        patch_among_downloads(&case);
        let mut kills = 0;
        while !apply::restore_then_crash(&case.dirs(), None, kills).unwrap() {
            kills += 1;
            assert!(kills < 1000, "{layout:?}: putting back never ends");
        }
        assert_as_before_the_patch(&case, &format!("{layout:?}, killed {kills} times"));
        // Doing it once more is doing nothing.
        assert_eq!(run::recover(&case.install).unwrap(), Leftover::Staged, "{layout:?}");
        assert_as_before_the_patch(&case, &format!("{layout:?}, after one more start"));
    }
}

#[test]
fn links_in_the_work_folder_keep_the_folder_from_being_put_back_and_lead_nothing_out() {
    // A step that is a link, a link among the built files, a link where the journal is.
    for planted in ["aside\\7", "new\\Linked", "aside"] {
        let case = case(Layout::Table);
        cut_off_and_damage(&case, usize::MAX, Some(b"X\n"));
        patch_among_downloads(&case);
        let link = planted.split('\\').fold(case.work(), |path, part| path.join(part));
        if planted == "aside" {
            // What was moved aside is taken elsewhere, and a link left in its place.
            fs::rename(&link, case.outside.join("taken")).unwrap();
        }
        let left_by_the_apply = case.files();
        let beside = files_under(&case.outside);
        junction(&link, &case.outside);

        let tried = run::recover(&case.install);
        assert!(
            matches!(&tried, Err(Error::Blocked(problems)) if format!("{problems:?}").contains("OutsideInstall")),
            "{planted}: {tried:?}"
        );
        assert!(files_under(&case.outside) == beside, "{planted}: something beside the install folder was touched");
        assert!(case.files() == left_by_the_apply, "{planted}: something was moved");
    }

    // A link that the game folder had in a folder the patch deletes is none of that. It was
    // moved aside with its folder, and is moved back as the link it is.
    let case = case(Layout::Table);
    let link = case.install.join("Data").join("Old").join("Out");
    junction(&link, &case.outside);
    let before = case.files();
    cut_off_and_damage(&case, usize::MAX, Some(b"X\n"));
    patch_among_downloads(&case);
    assert!(fs::symlink_metadata(&link).is_err(), "the folder with the link in it was not moved aside");
    assert_eq!(run::recover(&case.install).unwrap(), Leftover::Restored);
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink() && link.join("keep.txt").is_file());
    assert!(case.files() == before, "the folder is not as it was");
    case.assert_nothing_outside_touched("after a link was moved aside and back");
}

#[test]
fn half_patched_folder_is_put_back_to_half_patched_and_never_called_restored() {
    for layout in LAYOUTS {
        for cut in 0.. {
            let what = format!("{layout:?}, the last apply cut at {cut}");
            let case = case(layout);
            let drive = case.tight_drive();
            // The immediate way has put its new files in place, for good, and has only its
            // last apply left to do: the deletes and whatever waited for them.
            let patch = PatchFile::open(&case.patch).unwrap();
            let plan = plan::scan(&patch, &GO).unwrap();
            let checked = stage::check_originals(&plan, &case.install, &GO, &|_, _| {}).unwrap();
            let (dirs, profile) = (case.dirs(), Profile::fast());
            let free = |_: &Path| -> io::Result<u64> { Ok(u64::MAX / 2) };
            let job = Job {
                programs: &LIVE,
                patch: &patch,
                plan: &plan,
                dirs: &dirs,
                profile: &profile,
                free: &free,
                slack: 0,
                stop: &GO,
            };
            let (placed, early) = immediate::build_and_place(&job, checked, None, &mut |_| {}).unwrap();
            assert!(early > 0 && half_patched(&case.install), "{what}: nothing was placed early");
            let finished = apply::apply_then_crash(&plan, &placed, &dirs, cut).unwrap();
            fs::write(dirs.journal(), b"X\n").unwrap();
            patch_among_downloads(&case);
            let placed_early = case.files();

            // What that apply had moved aside is back. The folder is half patched as it
            // was before that apply, which is all it can be: nothing was kept of the files
            // that went in early.
            assert_eq!(run::recover(&case.install).unwrap(), Leftover::PutBack, "{what}");
            assert!(half_patched(&case.install), "{what}: the folder no longer counts as half patched");
            case.assert_whole(&what);
            let now = case.files();
            assert!(now[A] == placed_early[A] && now[B] == placed_early[B] && now.contains_key(GONE), "{what}");
            // Its own patch finishes it.
            case.run_on(&drive, &mut |_| {}).unwrap_or_else(|err| panic!("{what}: {err}"));
            case.assert_patched(&what);
            if finished {
                break;
            }
        }
    }
}

#[test]
fn without_the_patch_file_the_originals_go_back_and_the_folder_is_not_called_proven() {
    for layout in LAYOUTS {
        // No patch file among the downloads, and one that is another patch.
        for other_patch in [false, true] {
            let what = format!("{layout:?}, another patch among the downloads: {other_patch}");
            let case = case(layout);
            cut_off_and_damage(&case, usize::MAX, Some(b"X\n"));
            if other_patch {
                let mut other = PatchBuilder::new(layout);
                other.create(r"Data\Other.wz", b"of another patch");
                write_files(&case.dirs().download(), &[("other.patch".to_owned(), other.bytes())].into());
            }
            assert_eq!(run::recover(&case.install).unwrap(), Leftover::PutBack, "{what}");
            // Every original is back. What the patch made where nothing was stays: only
            // the patch could say which files those are.
            let now = case.files();
            let extra: Vec<&String> = now.keys().filter(|name| !case.before.contains_key(*name)).collect();
            assert!(
                case.before.iter().all(|(name, contents)| now.get(name) == Some(contents)),
                "{what}: an original is not back"
            );
            assert_eq!(extra, [FRESH], "{what}");
            assert!(now[FRESH] == case.after[FRESH], "{what}");
            // The run that follows makes the check that this could not, and applies.
            case.run(&mut |_| {}).unwrap_or_else(|err| panic!("{what}: {err}"));
            case.assert_patched(&what);
        }
    }
}

#[test]
fn run_on_a_folder_with_a_damaged_journal_puts_it_back_and_goes_on() {
    for immediate in [false, true] {
        let case = case(Layout::Table);
        let drive = case.tight_drive();
        cut_off_and_damage(&case, usize::MAX, Some(b"N\tData\\A.wz\textra\n"));
        // The patch file the run is given is the one that was cut off. It is nowhere else,
        // and it is what the folder is proven against: the run says so.
        let mut undone = Vec::new();
        let mut note = |event: Event| {
            if let Event::Restored(how) = event {
                undone.push(how);
            }
        };
        let result = if immediate { case.run_on(&drive, &mut note) } else { case.run(&mut note) };
        result.unwrap_or_else(|err| panic!("immediate {immediate}: {err}"));
        assert_eq!(undone, [Leftover::Restored], "immediate {immediate}");
        case.assert_patched(&format!("immediate {immediate}"));
    }
}
