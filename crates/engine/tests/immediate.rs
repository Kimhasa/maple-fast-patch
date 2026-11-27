//! Tests of the immediate way of patching, in which new files replace the originals while
//! others are still to be built. They run on a made-up drive that holds one install folder
//! and nothing else, so that the room on it is exactly what the test says.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mfp_engine::apply::{self, Leftover};
use mfp_engine::container::PatchFile;
use mfp_engine::immediate::{self, Job};
use mfp_engine::plan::{self, Action, Layout, Plan};
use mfp_engine::preflight::{Problem, SPACE_MARGIN};
use mfp_engine::run::{Disk, Room, half_patched, room_needed, run_on_disk};
use mfp_engine::stage::{self, Checked, Dirs};
use mfp_engine::storage::Profile;
use mfp_engine::{Error, Event, Options, Outcome, Stage};
use support::{
    ALLOWANCE, Drive, GO, LAYOUTS, LIVE, Op, PatchBuilder, Rng, World, copy, files_under, least_room, note_placed,
    on_disk, run, tree_size,
};

const KIB: usize = 1 << 10;

/// Runs a patch on `drive`, where the immediate way asks for no room beyond the least it
/// can work with: the drive is as tight as the test made it.
fn run_tight(
    options: &Options,
    drive: &Drive,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Outcome, Error> {
    run_on_drive(options, drive, None, stop, on_event)
}

/// [`run_tight`] on a drive that is said to have the file system `file_system`, where the
/// test does not leave that to the drive its folder is on.
fn run_on_drive(
    options: &Options,
    drive: &Drive,
    file_system: Option<&str>,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Outcome, Error> {
    let free = |_: &Path| -> io::Result<u64> { Ok(drive.free()) };
    run_on_disk(&LIVE, options, &Disk { free: &free, slack: 0, file_system }, stop, on_event)
}

/// A run of `patch` on `install`, with the immediate way chosen if `immediate` says so and
/// the safe way otherwise.
fn options(install: &Path, patch: &Path, immediate: bool) -> Options {
    Options { immediate, ..support::options(install, patch) }
}

/// Reads a patch file and checks the install against it, as a run does before it builds.
fn look(install: &Path, patch: &Path) -> (PatchFile, Plan, Checked) {
    let patch = PatchFile::open(patch).unwrap();
    patch.verify(&GO, |_| {}).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, install, &GO, &|_, _| {}).unwrap();
    (patch, plan, checked)
}

/// Does what a run in the immediate way does before its apply, with all the room it could
/// want: everything is built, and what may be placed ahead of the apply is placed. Returns
/// what the check of the originals found before and after, and how many files were placed.
fn build_and_place(install: &Path, patch: &Path) -> (Plan, Checked, Checked, usize) {
    let (patch, plan, checked) = look(install, patch);
    let (dirs, profile) = (Dirs::new(install), Profile::fast());
    let free = |_: &Path| -> io::Result<u64> { Ok(u64::MAX / 2) };
    let programs = &LIVE;
    let job =
        Job { programs, patch: &patch, plan: &plan, dirs: &dirs, profile: &profile, free: &free, slack: 0, stop: &GO };
    let (placed, early) = immediate::build_and_place(&job, checked.clone(), None, &mut |_| {}).unwrap();
    (plan, checked, placed, early)
}

/// An original by the name a patch entry gives it, which may be spelled another way than
/// the file is.
fn original<'a>(before: &'a BTreeMap<String, Vec<u8>>, name: &str) -> Option<&'a Vec<u8>> {
    let name = name.replace('/', "\\");
    before.iter().find(|(known, _)| known.eq_ignore_ascii_case(&name)).map(|(_, contents)| contents)
}

/// Fails unless every file of the install is wholly its old self or wholly its new self,
/// and every rebuild that is neither in place nor built still has, unchanged, every
/// original it copies from. That is what lets a run go on from wherever it stopped.
fn assert_whole(
    install: &Path,
    before: &BTreeMap<String, Vec<u8>>,
    after: &BTreeMap<String, Vec<u8>>,
    plan: &Plan,
    what: &str,
) {
    for name in before.keys().chain(after.keys()) {
        let found = fs::read(on_disk(install, name)).ok();
        assert!(
            found.as_ref() == before.get(name) || found.as_ref() == after.get(name),
            "{what}: {name} is neither the old file nor the new one"
        );
    }
    let staged = Dirs::new(install).staged();
    for entry in &plan.entries {
        let Action::Rebuild { sources, .. } = &entry.action else { continue };
        let new = after.get(&entry.name);
        let holds_new = |root: &Path| new.is_some() && fs::read(on_disk(root, &entry.name)).ok().as_ref() == new;
        if holds_new(install) || holds_new(&staged) {
            continue;
        }
        for source in sources {
            let found = fs::read(on_disk(install, source)).ok();
            assert!(
                found.as_ref() == original(before, source),
                "{what}: {source} was replaced while {} still has to be built from it",
                entry.name
            );
        }
    }
}

/// A made-up install put together by hand, a patch for it, and what the patch makes of it.
struct Case {
    _temp: tempfile::TempDir,
    install: PathBuf,
    patch: PathBuf,
    before: BTreeMap<String, Vec<u8>>,
    after: BTreeMap<String, Vec<u8>>,
    builder: PatchBuilder,
    rng: Rng,
}

impl Case {
    /// An install folder with files of the given sizes, filled from `seed`.
    fn new(seed: u64, files: &[(&str, usize)]) -> Case {
        let mut rng = Rng::new(seed);
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Maple");
        fs::create_dir_all(install.join("Data")).unwrap();
        fs::write(install.join("MapleStory.exe"), b"exe").unwrap();
        let mut before = BTreeMap::new();
        for (name, len) in files {
            let contents = rng.bytes(*len);
            let path = on_disk(&install, name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, &contents).unwrap();
            before.insert((*name).to_owned(), contents);
        }
        let patch = temp.path().join("made-up.patch");
        Case {
            _temp: temp,
            install,
            patch,
            after: before.clone(),
            before,
            builder: PatchBuilder::new(Layout::Table),
            rng,
        }
    }

    /// Adds an entry that rebuilds `name` out of `ops`, which copy from the originals.
    fn rebuild(&mut self, name: &str, ops: &[Op]) {
        let mut new = Vec::new();
        for op in ops {
            match op {
                Op::Literal(data) => new.extend(data),
                Op::Fill(byte, len) => new.resize(new.len() + *len as usize, *byte),
                Op::Copy(from, start, len) => {
                    let source =
                        original(&self.before, from).unwrap_or_else(|| panic!("no original named {from}")).clone();
                    new.extend(&source[*start as usize..(*start + *len) as usize]);
                    self.builder.original(from, &source);
                }
            }
        }
        let old = self.before[name].clone();
        self.builder.original(name, &old);
        self.builder.rebuild(name, &old, &new, ops);
        self.after.insert(name.to_owned(), new);
    }

    /// Adds an entry that rebuilds `name` out of its own original and nothing else, to a
    /// file of the same size.
    fn rebuild_from_itself(&mut self, name: &str) {
        let len = self.before[name].len();
        let fresh = self.rng.bytes(len / 4);
        self.rebuild(name, &[copy(name, len / 4, len / 2), Op::Literal(fresh), copy(name, 0, len / 4)]);
    }

    /// Adds an entry that puts a whole new file of `len` bytes at `name`.
    fn create(&mut self, name: &str, len: usize) {
        let contents = self.rng.bytes(len);
        self.builder.create(name, &contents);
        self.after.insert(name.to_owned(), contents);
    }

    /// Adds an entry that deletes `name`: a file, or with a separator at its end a folder
    /// and everything in it.
    fn delete(&mut self, name: &str) {
        self.builder.delete(name);
        let gone = name.trim_end_matches('\\').to_owned();
        let below = format!("{gone}\\");
        let folder = name.ends_with('\\');
        self.after.retain(|known, _| *known != gone && !(folder && known.starts_with(&below)));
    }

    /// Writes the patch file and returns the plan it makes.
    fn write(&self) -> Plan {
        fs::write(&self.patch, self.builder.bytes()).unwrap();
        look(&self.install, &self.patch).1
    }

    fn options(&self, immediate: bool) -> Options {
        options(&self.install, &self.patch, immediate)
    }

    /// A drive with exactly the room the immediate way needs for this patch.
    fn tight_drive(&self) -> Drive {
        Drive::with_room(&self.install, least_room(&self.install, &self.patch))
    }

    fn staged(&self, name: &str) -> PathBuf {
        on_disk(&Dirs::new(&self.install).staged(), name)
    }

    fn is_old(&self, name: &str) -> bool {
        fs::read(on_disk(&self.install, name)).ok().as_ref() == self.before.get(name)
    }

    fn is_new(&self, name: &str) -> bool {
        fs::read(on_disk(&self.install, name)).ok().as_ref() == self.after.get(name)
    }

    /// The new files of the patch that are neither in place nor built and waiting among the
    /// staged files.
    fn left_to_build(&self) -> Vec<&str> {
        let new = self.after.iter().filter(|(name, contents)| self.before.get(*name) != Some(*contents));
        new.filter(|(name, contents)| {
            !self.is_new(name) && fs::read(self.staged(name)).ok().as_ref() != Some(*contents)
        })
        .map(|(name, _)| name.as_str())
        .collect()
    }

    /// The files of the game as they are now: everything in the install folder but what a
    /// run keeps in its work folder.
    fn game_files(&self) -> BTreeMap<String, Vec<u8>> {
        let mut files = files_under(&self.install);
        files.retain(|name, _| !name.starts_with(".maple-fast-patch\\"));
        files
    }

    /// A drive on which the install folder, as a paused run has left it, has exactly the
    /// room the immediate way needs to go on from there.
    fn tight_drive_from_here(&self, plan: &Plan) -> Drive {
        let (patch, dirs) = (PatchFile::open(&self.patch).unwrap(), Dirs::new(&self.install));
        let built = immediate::built_before(&Profile::fast(), &patch, plan, &dirs, &GO).unwrap();
        let checked = stage::check_originals_except(plan, &self.install, &built, &GO, &|_, _| {}).unwrap();
        Drive::with_room(&self.install, immediate::space_needed(plan, &checked, &built, &self.install))
    }

    fn assert_whole(&self, plan: &Plan, what: &str) {
        assert_whole(&self.install, &self.before, &self.after, plan, what);
    }

    fn assert_patched(&self, what: &str) {
        for (name, contents) in &self.after {
            let found = fs::read(on_disk(&self.install, name)).unwrap_or_else(|e| panic!("{what}: {name}: {e}"));
            assert!(found == *contents, "{what}: {name} has the wrong contents ({} bytes)", found.len());
        }
        for name in self.before.keys().filter(|name| !self.after.contains_key(*name)) {
            assert!(!on_disk(&self.install, name).is_file(), "{what}: {name} should be gone");
        }
        assert!(!self.install.join(".maple-fast-patch").exists(), "{what}: work folder left behind");
        assert!(!self.install.join(".maple-fast-patch.lock").exists(), "{what}: lock file left behind");
    }
}

/// Runs a case in the immediate way on a drive that is exactly as tight as that way can
/// bear, checking at every event that every file is whole. `at_event` sees each event too.
fn run_case(case: &Case, plan: &Plan, what: &str, at_event: &mut (dyn FnMut(&Event) + Send)) -> Outcome {
    let drive = case.tight_drive();
    let mut applying = false;
    let outcome = run_tight(&case.options(true), &drive, &GO, &mut |event| {
        // The apply moves folders aside and puts new ones in their place under its journal,
        // and cannot be paused. Up to the moment it begins, every file is whole.
        if !applying {
            case.assert_whole(plan, what);
        }
        applying |= matches!(event, Event::Stage(Stage::Apply));
        at_event(&event);
    })
    .unwrap_or_else(|e| panic!("{what}: {e}"));
    case.assert_patched(what);
    outcome
}

#[test]
fn immediate_way_ends_in_the_install_the_safe_way_ends_in() {
    for layout in LAYOUTS {
        for seed in 1..=25 {
            let what = format!("{layout:?} seed {seed}");
            let safe = World::random(seed, layout);
            let by_the_safe_way = run(&options(&safe.install, &safe.patch, false), &GO, &mut |_| {}).unwrap();

            // The same install and patch with the immediate way chosen, on a drive with
            // barely the room that way needs. It is the way taken because it was chosen,
            // whether or not the safe way would have fitted there.
            let world = World::random(seed, layout);
            let drive = Drive::with_room(&world.install, least_room(&world.install, &world.patch));
            let chosen = options(&world.install, &world.patch, true);
            let outcome = run_tight(&chosen, &drive, &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert!(outcome.immediate, "{what}: {outcome:?}");
            assert_eq!(outcome.files_changed, by_the_safe_way.files_changed, "{what}");
            world.assert_patched(&what);
            assert!(
                files_under(&world.install) == files_under(&safe.install),
                "{what}: the two ways ended differently"
            );

            // A second run finds nothing left to do. It goes the way that was chosen all the
            // same, changes nothing and leaves nothing behind.
            let again = run_tight(&chosen, &drive, &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}, again: {e}"));
            assert_eq!(again, Outcome { files_changed: 0, immediate: true }, "{what}, again");
            world.assert_patched(&format!("{what}, again"));
        }
    }
}

#[test]
fn pausing_at_any_moment_leaves_every_file_whole_and_the_run_can_go_on() {
    for (layout, seed) in [(Layout::Table, 3), (Layout::PerEntry, 11)] {
        let (plan, room) = {
            let world = World::random(seed, layout);
            (look(&world.install, &world.patch).1, least_room(&world.install, &world.patch))
        };
        let mut paused_half_patched = 0;
        // Pause at the first event, then at the second, and so on until a run gets through
        // without reaching the pause.
        for pause_at in 1.. {
            let world = World::random(seed, layout);
            let what = format!("{layout:?} seed {seed}, paused at event {pause_at}");
            let drive = Drive::with_room(&world.install, room);
            let patch = |immediate| options(&world.install, &world.patch, immediate);
            let stop = AtomicBool::new(false);
            let mut events = 0;
            // The files named as placed, over all the runs it takes.
            let mut placed = Vec::new();
            let first = run_tight(&patch(true), &drive, &stop, &mut |event| {
                events += 1;
                if events == pause_at {
                    stop.store(true, Ordering::Relaxed);
                }
                note_placed(&mut placed, event);
            });
            match first {
                Ok(_) => {
                    world.assert_patched(&what);
                    world.assert_named_once(&placed, &what);
                    if events < pause_at {
                        break;
                    }
                    continue;
                }
                Err(Error::Stopped) => {}
                Err(err) => panic!("{what}: {err}"),
            }
            // What was named so far is in place, and a folder with such a file is half patched.
            for name in &placed {
                assert!(
                    fs::read(on_disk(&world.install, name)).ok().as_ref() == world.after.get(name),
                    "{what}: {name}"
                );
            }
            assert!(placed.is_empty() || half_patched(&world.install), "{what}: {placed:?}");
            assert_whole(&world.install, &world.before, &world.after, &plan, &what);
            let marked = half_patched(&world.install);
            paused_half_patched += usize::from(marked);
            if !marked {
                world.assert_untouched(&what);
            }

            // Going on with the safe way chosen. A half-patched install goes on the
            // immediate way all the same. One that nothing was placed in is refused for want
            // of room, and is patched once the immediate way is chosen again.
            match run_tight(&patch(false), &drive, &GO, &mut |event| note_placed(&mut placed, event)) {
                Ok(outcome) => assert_eq!(outcome.immediate, marked, "{what}, continued"),
                Err(Error::Blocked(problems))
                    if !marked && matches!(problems[..], [Problem::NotEnoughSpace { .. }]) =>
                {
                    world.assert_untouched(&format!("{what}, refused"));
                    assert!(!half_patched(&world.install), "{what}, refused");
                    run_tight(&patch(true), &drive, &GO, &mut |event| note_placed(&mut placed, event))
                        .unwrap_or_else(|e| panic!("{what}, chosen again: {e}"));
                }
                Err(err) => panic!("{what}, continued: {err}"),
            }
            world.assert_patched(&format!("{what}, continued"));
            // Paused wherever it was, and continued: each new file was named once in all.
            world.assert_named_once(&placed, &format!("{what}, continued"));
        }
        assert!(paused_half_patched > 0, "{layout:?} seed {seed}: no pause came after a file was placed");
    }
}

#[test]
fn files_the_safe_way_built_are_kept_when_the_immediate_way_takes_over() {
    for (layout, seed) in [(Layout::Table, 5), (Layout::PerEntry, 17), (Layout::PerEntry, 9)] {
        let what = format!("{layout:?} seed {seed}");
        let world = World::random(seed, layout);
        let dirs = Dirs::new(&world.install);

        // The safe way is paused while it builds its second file.
        let stop = AtomicBool::new(false);
        let (mut rebuilding, mut files) = (false, 0);
        let paused = run(&options(&world.install, &world.patch, false), &stop, &mut |event| match event {
            Event::Stage(stage) => rebuilding = stage == Stage::Rebuild,
            Event::File(_) if rebuilding => {
                files += 1;
                if files == 2 {
                    stop.store(true, Ordering::Relaxed);
                }
            }
            _ => {}
        });
        assert!(matches!(paused, Err(Error::Stopped)), "{what}: {paused:?}");
        world.assert_untouched(&what);
        let staged = files_under(&dirs.staged());
        let staged_bytes: u64 = staged.values().map(|file| file.len() as u64).sum();
        assert!(staged_bytes > 0, "{what}: the pause came before anything was built");

        // What is left is done with the immediate way chosen, on a drive with just the room
        // that way needs for it. What the safe way built is not built again.
        let (_, plan, checked) = look(&world.install, &world.patch);
        let built: BTreeSet<String> = staged.keys().cloned().collect();
        let drive = Drive::with_room(&world.install, immediate::space_needed(&plan, &checked, &built, &world.install));
        let total = plan.new_bytes();
        let mut kept = BTreeMap::new();
        let (mut rebuilding, mut done, mut rebuilt) = (false, 0, 0);
        let outcome = run_tight(&options(&world.install, &world.patch, true), &drive, &GO, &mut |event| match event {
            Event::Stage(stage) => (rebuilding, done) = (stage == Stage::Rebuild, 0),
            Event::Kept { name, bytes } => drop(kept.insert(name, bytes)),
            Event::Progress { done: now, total: of } if rebuilding => {
                assert_eq!(of, total, "{what}");
                assert!(done <= now && now <= total, "{what}: progress went from {done} to {now} of {total}");
                (done, rebuilt) = (now, now);
            }
            _ => {}
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        world.assert_patched(&what);
        assert!(outcome.immediate, "{what}: {outcome:?}");

        // Every file that was staged is reported as kept, with its size, and no other is.
        // Together with what was built now, the new files add up to their total.
        let staged_sizes: BTreeMap<String, u64> =
            staged.iter().map(|(name, file)| (name.clone(), file.len() as u64)).collect();
        assert_eq!(kept, staged_sizes, "{what}");
        assert_eq!(kept.values().sum::<u64>(), staged_bytes, "{what}");
        assert_eq!(rebuilt, total, "{what}: the step did not end at its total");
    }
}

#[test]
fn apply_cut_off_at_any_moment_keeps_what_was_placed_and_the_next_run_finishes() {
    let mut placed_early = 0;
    for (layout, seed) in [(Layout::Table, 5), (Layout::PerEntry, 17)] {
        for fuse in 0.. {
            let world = World::random(seed, layout);
            let what = format!("{layout:?} seed {seed}, cut off at moment {fuse}");
            let dirs = Dirs::new(&world.install);
            let (plan, checked, placed, early) = build_and_place(&world.install, &world.patch);
            let in_place: Vec<&String> = placed.up_to_date.difference(&checked.up_to_date).collect();
            assert_eq!(in_place.len(), early, "{what}");
            assert_eq!(half_patched(&world.install), early > 0, "{what}");
            placed_early += early;

            let ran_to_the_end = apply::apply_then_crash(&plan, &placed, &dirs, fuse).unwrap();
            assert_eq!(apply::recover(&dirs).unwrap(), Leftover::RolledBack, "{what}");

            // What the apply had done is undone. What was placed before it stays placed:
            // that was never the apply's to undo.
            for name in world.before.keys().chain(world.after.keys()) {
                let found = fs::read(on_disk(&world.install, name)).ok();
                let expected = if in_place.contains(&name) { world.after.get(name) } else { world.before.get(name) };
                assert!(found.as_ref() == expected, "{what}: {name} is not as it should be after the apply was undone");
            }
            assert_eq!(half_patched(&world.install), early > 0, "{what}, undone");

            // The next run finishes it, the immediate way if the install is half patched,
            // whichever way is chosen.
            let outcome = run(&options(&world.install, &world.patch, fuse % 2 == 0), &GO, &mut |_| {})
                .unwrap_or_else(|e| panic!("{what}, next run: {e}"));
            assert_eq!(outcome.immediate, early > 0, "{what}, next run");
            world.assert_patched(&format!("{what}, next run"));

            if ran_to_the_end {
                break;
            }
        }
    }
    assert!(placed_early > 0, "nothing was ever placed ahead of the apply");
}

#[test]
fn files_under_a_folder_that_is_deleted_first_stay_staged_until_the_apply() {
    let old = (r"Data\Old\a.wz", r"Data\Old\gone.wz", r"Data\Old\New.wz");
    let (x, y) = (r"Data\X.wz", r"Data\Y.wz");
    let mut case = Case::new(101, &[(old.0, 50 * KIB), (old.1, 10 * KIB), (x, 60 * KIB), (y, 60 * KIB)]);
    case.delete(r"Data\Old\");
    case.create(old.2, 20 * KIB);
    case.rebuild(old.0, &[copy(old.0, 10 * KIB, 30 * KIB)]);
    case.rebuild_from_itself(x);
    case.rebuild_from_itself(y);
    let plan = case.write();

    let mut stages = Vec::new();
    let mut seen_at_apply = false;
    let outcome = run_case(&case, &plan, "deleted folder", &mut |event| {
        if let Event::Stage(stage) = event {
            stages.push(*stage);
        }
        // When the apply begins, the two files outside the folder are in place already.
        // The folder is as it was, and what goes into it waits among the staged files.
        if matches!(event, Event::Stage(Stage::Apply)) {
            seen_at_apply = true;
            assert!(case.is_new(x) && case.is_new(y), "the files nothing waits for were not placed");
            assert!(case.is_old(old.0) && case.is_old(old.1), "the folder was touched before the apply");
            assert!(
                !on_disk(&case.install, old.2).exists(),
                "a new file was put into a folder that is yet to be deleted"
            );
            assert!(case.staged(old.0).is_file() && case.staged(old.2).is_file());
            assert!(!case.staged(x).exists() && !case.staged(y).exists());
            assert!(half_patched(&case.install));
        }
    });
    assert!(seen_at_apply);
    assert_eq!(stages, [Stage::VerifyPatch, Stage::VerifyInstall, Stage::Trial, Stage::Rebuild, Stage::Apply]);
    // Two files placed early, two by the apply, and the folder it removed.
    assert_eq!(outcome, Outcome { files_changed: 5, immediate: true });
}

#[test]
fn file_under_the_name_of_a_file_that_is_deleted_first_stays_staged_until_the_apply() {
    // A delete that names a file, not a folder, and a new file below that very name. Today
    // a file stands there. Only once it is gone can the folder of that name be made.
    let (thing, inner) = (r"Data\Thing", r"Data\Thing\Inner.wz");
    let (x, y) = (r"Data\X.wz", r"Data\Y.wz");
    let mut case = Case::new(103, &[(thing, 5 * KIB), (x, 60 * KIB), (y, 60 * KIB)]);
    case.delete(thing);
    case.create(inner, 20 * KIB);
    case.rebuild_from_itself(x);
    case.rebuild_from_itself(y);
    let plan = case.write();

    let mut seen_at_apply = false;
    let outcome = run_case(&case, &plan, "deleted file", &mut |event| {
        if matches!(event, Event::Stage(Stage::Apply)) {
            seen_at_apply = true;
            assert!(case.is_new(x) && case.is_new(y));
            assert!(case.is_old(thing), "the file was touched before the apply");
            assert!(case.staged(inner).is_file());
        }
    });
    assert!(seen_at_apply);
    assert!(on_disk(&case.install, thing).is_dir());
    assert_eq!(outcome, Outcome { files_changed: 4, immediate: true });
}

#[test]
fn file_built_from_itself_is_placed_as_soon_as_it_is_built() {
    let (a, b) = (r"Data\A.wz", r"Data\B.wz");
    let mut case = Case::new(107, &[(a, 100 * KIB), (b, 100 * KIB)]);
    case.rebuild_from_itself(a);
    case.rebuild_from_itself(b);
    let plan = case.write();
    // The drive has room for one of the two new files at a time.
    assert_eq!(least_room(&case.install, &case.patch), 100 * KIB as u64);

    let mut rebuilding = false;
    let mut second_seen = false;
    let outcome = run_case(&case, &plan, "built from itself", &mut |event| match event {
        Event::Stage(stage) => rebuilding = *stage == Stage::Rebuild,
        // By the time the second file is first named in this step, as it starts to be
        // built, the first is in place and its original is gone.
        Event::File(name) if rebuilding && name == b && !second_seen => {
            second_seen = true;
            assert!(case.is_new(a), "the first file was not placed before the second was built");
            assert!(case.is_old(b));
        }
        _ => {}
    });
    assert!(second_seen);
    assert_eq!(outcome, Outcome { files_changed: 2, immediate: true });
}

/// A patch in which `U` is built from the original of `F`, which the patch replaces whole
/// in an earlier entry, with a file `X` in front of both that depends on nothing. `source`
/// is how the entry of `U` spells the name of `F`.
fn consumer_after_its_source(seed: u64, source: &str) -> (Case, Plan) {
    let (x, f, u) = (r"Data\X.wz", r"Data\F.wz", r"Data\U.wz");
    let mut case = Case::new(seed, &[(x, 100 * KIB), (f, 50 * KIB), (u, 100 * KIB)]);
    case.rebuild_from_itself(x);
    case.create(f, 50 * KIB);
    case.rebuild(u, &[copy(source, 0, 50 * KIB), copy(u, 25 * KIB, 50 * KIB)]);
    let plan = case.write();
    // X and its room back, then F and U together: F cannot go before U is built.
    assert_eq!(least_room(&case.install, &case.patch), 150 * KIB as u64);
    (case, plan)
}

/// Runs [`consumer_after_its_source`] and fails unless the new `F` waited for `U`.
fn assert_source_waits_for_its_consumer(case: &Case, plan: &Plan, what: &str) {
    let (x, f, u) = (r"Data\X.wz", r"Data\F.wz", r"Data\U.wz");
    let mut building_u = 0;
    let mut rebuilding = false;
    let outcome = run_case(case, plan, what, &mut |event| match event {
        Event::Stage(stage) => rebuilding = *stage == Stage::Rebuild,
        // While U is being built, the new F is built and waits; the old one is in place,
        // because U is being built from it. X, which nothing needs, is in place already.
        Event::Progress { .. }
            if rebuilding && !case.is_new(u) && !case.staged(u).exists() && case.staged(f).is_file() =>
        {
            building_u += 1;
            assert!(case.is_old(f), "{what}: the original was replaced while a file was still built from it");
            assert!(case.is_new(x), "{what}: the file nothing waits for was not placed");
        }
        _ => {}
    });
    assert!(building_u > 0, "{what}: the two files were never apart");
    assert_eq!(outcome, Outcome { files_changed: 3, immediate: true }, "{what}");
}

#[test]
fn whole_file_that_another_is_still_built_from_waits_for_it() {
    let (case, plan) = consumer_after_its_source(109, r"Data\F.wz");
    assert_source_waits_for_its_consumer(&case, &plan, "create before its consumer");
}

#[cfg(windows)]
#[test]
fn source_spelled_another_way_is_still_the_same_file() {
    // Windows opens `data\f.WZ` and `Data\F.wz` as one file, so they are one file here.
    let (case, plan) = consumer_after_its_source(113, r"data\f.WZ");
    assert_source_waits_for_its_consumer(&case, &plan, "another spelling");
}

#[test]
fn chain_of_files_built_from_one_another_is_placed_link_by_link() {
    // B is built from the original of A, and C from the original of B. A has to wait
    // for B and B for C, each in place the moment the next is built.
    let (a, b, c) = (r"Data\A.wz", r"Data\B.wz", r"Data\C.wz");
    let mut case = Case::new(127, &[(a, 100 * KIB), (b, 100 * KIB), (c, 100 * KIB)]);
    case.rebuild_from_itself(a);
    case.rebuild(b, &[copy(a, 0, 60 * KIB), copy(b, 0, 40 * KIB)]);
    case.rebuild(c, &[copy(b, 0, 60 * KIB), copy(c, 0, 40 * KIB)]);
    let plan = case.write();
    assert_eq!(least_room(&case.install, &case.patch), 200 * KIB as u64);

    let mut order = Vec::new();
    let outcome = run_case(&case, &plan, "chain", &mut |_| {
        // The order in which the files turn up in place.
        for name in [a, b, c] {
            if case.is_new(name) && !order.contains(&name) {
                order.push(name);
            }
        }
        // None is ever in place before the one that is built from it is built.
        assert!(!case.is_new(a) || case.is_new(b) || case.staged(b).is_file(), "A went before B was built");
        assert!(!case.is_new(b) || case.is_new(c) || case.staged(c).is_file(), "B went before C was built");
    });
    assert_eq!(order, [a, b, c]);
    assert_eq!(outcome, Outcome { files_changed: 3, immediate: true });
}

/// The size of the install folder, looked at again when it seems to be over `limit`: a file
/// that is being moved into the staged files at that moment can be seen in both places.
fn settled_tree_size(install: &Path, limit: u64) -> u64 {
    let mut size = tree_size(install);
    for _ in 0..5 {
        if size <= limit {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        size = tree_size(install);
    }
    size
}

#[test]
fn immediate_way_stays_within_its_room_and_refuses_a_drive_that_has_less() {
    for (layout, seed) in [(Layout::Table, 7), (Layout::PerEntry, 13)] {
        let what = format!("{layout:?} seed {seed}");
        let world = World::random(seed, layout);
        let chosen = options(&world.install, &world.patch, true);
        let (patch, plan, checked) = look(&world.install, &world.patch);
        let room = immediate::space_needed(&plan, &checked, &BTreeSet::new(), &world.install);
        let to_build = stage::bytes_to_build(&plan, &checked, &Dirs::new(&world.install));
        assert!(0 < room && room <= to_build, "{what}: {room} of {to_build}");
        let needed = room + SPACE_MARGIN;

        // One byte short of the least and the margin: refused with the figures, before
        // anything is built or marked.
        let short = Drive { size: tree_size(&world.install) + needed - 1, install: world.install.clone() };
        let mut space = Vec::new();
        let err = run_tight(&chosen, &short, &GO, &mut |event| {
            if let Event::Space { needed, available } = event {
                space.push((needed, available));
            }
        })
        .unwrap_err();
        let Error::Blocked(problems) = &err else { panic!("{what}: unexpected error: {err}") };
        assert!(
            matches!(&problems[..], [Problem::NotEnoughSpace { needed: n, available: a, .. }] if (*n, *a) == (needed, needed - 1)),
            "{what}: {problems:?}"
        );
        assert_eq!(space, [(needed, needed - 1)], "{what}");
        world.assert_untouched(&format!("{what}, short of room"));
        assert!(!half_patched(&world.install) && !world.install.join(".maple-fast-patch").exists(), "{what}");

        // The immediate way asks for some room on top of the least, so that it does not
        // crawl along a file at a time. One byte short of that is refused as well. The
        // room to spare is a gigabyte outside tests; here it is half of what lies between
        // the least and the room for everything, for it is never asked for beyond that
        // (see `immediate_way_never_asks_for_more_room_than_the_safe_way`).
        let to_spare = (to_build - room) / 2;
        let asked = needed + to_spare;
        let free = |_: &Path| -> io::Result<u64> { Ok(asked - 1) };
        let (dirs, profile) = (Dirs::new(&world.install), Profile::fast());
        let job = Job {
            programs: &LIVE,
            patch: &patch,
            plan: &plan,
            dirs: &dirs,
            profile: &profile,
            free: &free,
            slack: to_spare,
            stop: &GO,
        };
        let err = immediate::build_and_place(&job, checked.clone(), None, &mut |_| {}).unwrap_err();
        let Error::Blocked(problems) = &err else { panic!("{what}: unexpected error: {err}") };
        assert!(
            matches!(&problems[..], [Problem::NotEnoughSpace { needed: n, available: a, .. }] if (*n, *a) == (asked, asked - 1)),
            "{what}: {problems:?}"
        );
        world.assert_untouched(&format!("{what}, short of the room asked for"));
        assert!(!half_patched(&world.install) && !world.install.join(".maple-fast-patch").exists(), "{what}");

        // With the least, the margin stays free at every moment of the run.
        let drive = Drive::with_room(&world.install, room);
        let before = tree_size(&world.install);
        let mut most = 0;
        let outcome = run_tight(&chosen, &drive, &GO, &mut |_| {
            let size = settled_tree_size(&world.install, drive.limit());
            assert!(size <= drive.limit(), "{what}: the install folder grew to {size}, past {}", drive.limit());
            most = most.max(size);
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        world.assert_patched(&what);
        assert!(outcome.immediate, "{what}: {outcome:?}");
        assert!(most > before, "{what}: the run was never seen to take any room");
    }
}

#[test]
fn safe_way_on_a_tight_drive_is_refused_for_room() {
    let world = World::random(13, Layout::PerEntry);
    let (_, plan, checked) = look(&world.install, &world.patch);
    let to_build = stage::bytes_to_build(&plan, &checked, &Dirs::new(&world.install));
    let drive = Drive::with_room(&world.install, least_room(&world.install, &world.patch));
    assert!(drive.free() < to_build + SPACE_MARGIN, "the drive is not tight for the safe way");

    let err = run_tight(&options(&world.install, &world.patch, false), &drive, &GO, &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, Error::Blocked(problems) if matches!(problems[..], [Problem::NotEnoughSpace { .. }])),
        "{err}"
    );
    world.assert_untouched("the safe way, short of room");
    assert!(!half_patched(&world.install));
}

#[test]
fn originals_check_passes_over_what_built_files_were_built_from_and_nothing_else() {
    // With nothing named as built, it is the plain check of the originals.
    let nothing = BTreeSet::new();
    let world = World::random(19, Layout::Table);
    let (_, plan, checked) = look(&world.install, &world.patch);
    let with_none = stage::check_originals_except(&plan, &world.install, &nothing, &GO, &|_, _| {}).unwrap();
    assert_eq!(with_none.up_to_date, checked.up_to_date);
    let copied_from = |entry: &plan::Entry| match &entry.action {
        Action::Rebuild { sources, .. } => sources.iter().next().cloned(),
        _ => None,
    };
    let damaged = plan.entries.iter().find_map(copied_from).expect("the patch copies from no original");
    fs::write(on_disk(&world.install, &damaged), b"not the original").unwrap();
    let plain = stage::check_originals(&plan, &world.install, &GO, &|_, _| {}).unwrap_err();
    let with_none = stage::check_originals_except(&plan, &world.install, &nothing, &GO, &|_, _| {}).unwrap_err();
    let (Error::OriginalMismatch(plain), Error::OriginalMismatch(with_none)) = (plain, with_none) else { panic!() };
    assert_eq!(plain, with_none);
    assert!(plain.contains(&damaged));

    // H is built from the original of S and waits for the apply, because the folder it
    // goes into is deleted first. S meanwhile is placed: the original of S is gone, and
    // the staged H is all there is of what it was built from.
    let (h, s) = (r"Data\Old\h.wz", r"Data\S.wz");
    let mut case = Case::new(131, &[(h, 20 * KIB), (s, 40 * KIB)]);
    case.delete(r"Data\Old\");
    case.rebuild(h, &[copy(s, 5 * KIB, 30 * KIB)]);
    case.rebuild_from_itself(s);
    case.write();
    let (plan, _, placed, early) = build_and_place(&case.install, &case.patch);
    assert_eq!(early, 1);
    assert!(placed.up_to_date.contains(s) && case.is_new(s) && case.staged(h).is_file());
    assert!(half_patched(&case.install));
    let (patch, dirs) = (PatchFile::open(&case.patch).unwrap(), Dirs::new(&case.install));
    let built_before = || immediate::built_before(&Profile::fast(), &patch, &plan, &dirs, &GO).unwrap();
    let check = |built: &BTreeSet<String>| stage::check_originals_except(&plan, &case.install, built, &GO, &|_, _| {});
    let replaced_source = |result: mfp_engine::Result<Checked>| match result {
        Err(Error::OriginalMismatch(files)) => files,
        other => panic!("expected the replaced original to be reported, got {other:?}"),
    };

    // With the staged file whole, it is found built and its original is not asked for.
    assert_eq!(built_before(), BTreeSet::from([h.to_owned()]));
    assert!(check(&built_before()).unwrap().up_to_date.contains(s));
    // Without that knowledge the check can only say what it sees: the original is gone.
    assert_eq!(replaced_source(check(&nothing)), [s]);

    // A staged file of the right size and the wrong contents is not a built file, and
    // neither is one that is gone. Then the original it needs is reported, by name, and
    // a run stops there without touching anything.
    let good = fs::read(case.staged(h)).unwrap();
    let mut bad = good.clone();
    bad[1000] ^= 0x04;
    fs::write(case.staged(h), &bad).unwrap();
    assert!(built_before().is_empty());
    assert_eq!(replaced_source(check(&built_before())), [s]);
    let err = run(&case.options(true), &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(&err, Error::OriginalMismatch(files) if files[..] == [s]), "unexpected error: {err}");
    assert_eq!(fs::read(case.staged(h)).unwrap(), bad, "the run changed the staged files");

    fs::remove_file(case.staged(h)).unwrap();
    assert_eq!(replaced_source(check(&built_before())), [s]);
    let err = run(&case.options(false), &GO, &mut |_| {}).unwrap_err();
    assert!(matches!(&err, Error::OriginalMismatch(files) if files[..] == [s]), "unexpected error: {err}");
    assert!(case.is_new(s) && case.is_old(h) && half_patched(&case.install));

    // Put back as it was built, the staged file lets the same patch finish.
    fs::write(case.staged(h), &good).unwrap();
    let outcome = run(&case.options(false), &GO, &mut |_| {}).unwrap();
    assert_eq!(outcome, Outcome { files_changed: 2, immediate: true });
    case.assert_patched("after the staged file was put back");
}

/// Pauses a run in the immediate way right after its first file was placed. Returns the
/// drive it ran on.
fn pause_half_patched(world: &World, what: &str) -> Drive {
    let drive = Drive::with_room(&world.install, least_room(&world.install, &world.patch));
    let stop = AtomicBool::new(false);
    let paused = run_tight(&options(&world.install, &world.patch, true), &drive, &stop, &mut |_| {
        stop.store(half_patched(&world.install), Ordering::Relaxed);
    });
    assert!(matches!(paused, Err(Error::Stopped)), "{what}: {paused:?}");
    assert!(half_patched(&world.install), "{what}");
    drive
}

#[test]
fn half_patched_install_takes_no_other_patch_and_loses_nothing_to_one() {
    let world = World::random(11, Layout::PerEntry);
    let dirs = Dirs::new(&world.install);
    pause_half_patched(&world, "paused");
    assert_eq!(apply::recover(&dirs).unwrap(), Leftover::Staged);
    let work_before = files_under(&dirs.work());
    let install_before = files_under(&world.install);
    assert!(
        work_before.contains_key("half-patched") && work_before.contains_key("patch.id"),
        "{:?}",
        work_before.keys()
    );

    // Another patch, whichever way is chosen for it: refused, and nothing is touched.
    let other = World::random(12, Layout::PerEntry);
    for chosen in [false, true] {
        let err = run(&options(&world.install, &other.patch, chosen), &GO, &mut |_| {}).unwrap_err();
        let expected = [Problem::LeftoverWork { dir: dirs.work() }];
        assert!(matches!(&err, Error::Blocked(problems) if problems[..] == expected), "unexpected error: {err}");
        assert!(files_under(&dirs.work()) == work_before, "the work folder was changed");
        assert!(files_under(&world.install) == install_before, "the install folder was changed");
    }
    // Nor does staging for the other patch, asked for directly, throw the files away.
    let (other_patch, other_plan) = {
        let patch = PatchFile::open(&other.patch).unwrap();
        let plan = plan::scan(&patch, &GO).unwrap();
        (patch, plan)
    };
    let err = stage::stage(&other_patch, &other_plan, &Checked::default(), &dirs, &GO, &mut |_, _, _| {}).unwrap_err();
    assert!(matches!(err, Error::Blocked(_)), "unexpected error: {err}");
    assert!(files_under(&dirs.work()) == work_before, "the work folder was changed");

    // The patch that began there finishes it.
    let outcome = run(&options(&world.install, &world.patch, false), &GO, &mut |_| {}).unwrap();
    assert!(outcome.immediate && outcome.files_changed > 0, "{outcome:?}");
    world.assert_patched("finished by the patch that began");
}

#[test]
fn half_patched_install_whose_patch_id_was_lost_keeps_its_staged_files() {
    let damages: [&[u8]; 3] = [b"", b"00000000-1", b"\xFF\xFE"];
    for damage in damages {
        let what = format!("patch.id overwritten with {damage:?}");
        let world = World::random(11, Layout::PerEntry);
        let dirs = Dirs::new(&world.install);
        let drive = pause_half_patched(&world, &what);
        let id = PatchFile::open(&world.patch).unwrap().id();
        let id_file = dirs.work().join("patch.id");
        assert_eq!(fs::read_to_string(&id_file).unwrap(), id, "{what}");
        let staged: BTreeSet<String> = files_under(&dirs.staged()).into_keys().collect();

        // The file that says which patch the staged files belong to is written without a
        // sync, and a power cut can leave anything in it. The marker says the same and
        // was synced: by it the staged files are known, kept, and the file put right.
        fs::write(&id_file, damage).unwrap();
        let mut kept = BTreeSet::new();
        let mut id_at_apply = None;
        let outcome = run_tight(&options(&world.install, &world.patch, false), &drive, &GO, &mut |event| match event {
            Event::Kept { name, .. } => drop(kept.insert(name)),
            Event::Stage(Stage::Apply) => id_at_apply = fs::read_to_string(&id_file).ok(),
            _ => {}
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(outcome.immediate, "{what}");
        assert_eq!(kept, staged, "{what}: the staged files were not kept");
        assert_eq!(id_at_apply, Some(id), "{what}: the id was not put right");
        world.assert_patched(&what);
    }
}

#[cfg(windows)]
#[test]
fn read_only_files_are_replaced() {
    let world = World::random(13, Layout::PerEntry);
    let read_only = |path: &Path, on: bool| {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(on);
        fs::set_permissions(path, permissions).unwrap();
    };
    for name in world.before.keys() {
        read_only(&on_disk(&world.install, name), true);
    }

    let drive = Drive::with_room(&world.install, least_room(&world.install, &world.patch));
    let outcome = run_tight(&options(&world.install, &world.patch, true), &drive, &GO, &mut |_| {}).unwrap();
    assert!(outcome.immediate, "{outcome:?}");
    world.assert_patched("read-only install");

    // Files the patch left alone are read-only still. They are freed so that the folder
    // can be removed when the test ends.
    for name in world.after.keys().filter(|name| world.before.get(*name) == world.after.get(*name)) {
        let path = on_disk(&world.install, name);
        assert!(fs::metadata(&path).unwrap().permissions().readonly(), "{name} lost its read-only mark");
        read_only(&path, false);
    }
}

#[cfg(windows)]
#[test]
fn file_held_open_by_another_program_stops_the_run_and_releasing_it_lets_the_run_finish() {
    use std::os::windows::fs::OpenOptionsExt;

    let world = World::random(13, Layout::PerEntry);
    let (_, plan, _) = look(&world.install, &world.patch);
    // A file the patch rebuilds, held the way a program holds a file it is using: others
    // may read it, nobody may move or replace it.
    let held_name = plan
        .entries
        .iter()
        .find(|entry| matches!(entry.action, Action::Rebuild { .. }))
        .map(|entry| entry.name.clone())
        .expect("the patch rebuilds nothing");
    let held_path = on_disk(&world.install, &held_name);
    let held = fs::OpenOptions::new().read(true).share_mode(1).open(&held_path).unwrap();

    let drive = Drive::with_room(&world.install, least_room(&world.install, &world.patch));
    let chosen = options(&world.install, &world.patch, true);
    let err = run_tight(&chosen, &drive, &GO, &mut |_| {}).unwrap_err();
    let in_use = match &err {
        Error::Blocked(problems) => {
            matches!(&problems[..], [Problem::FilesLocked { files }] if files[..] == [held_path.clone()])
        }
        _ => false,
    };
    assert!(in_use, "unexpected error: {err}");
    // The file is its old self, and every other file is whole.
    assert!(fs::read(&held_path).unwrap() == world.before[&held_name]);
    assert_whole(&world.install, &world.before, &world.after, &plan, "held open");

    drop(held);
    run_tight(&chosen, &drive, &GO, &mut |_| {}).unwrap();
    world.assert_patched("released");
}

#[test]
fn steps_of_a_run_in_rounds_are_the_safe_ways_four_and_a_trial_and_progress_only_grows() {
    let world = World::random(11, Layout::PerEntry);
    let drive = Drive::with_room(&world.install, least_room(&world.install, &world.patch));
    let mut stages = Vec::new();
    let mut last_progress = None;
    let mut named = BTreeSet::new();
    let mut placed_while_building = false;
    let outcome = run_tight(&options(&world.install, &world.patch, true), &drive, &GO, &mut |event| match event {
        Event::Stage(stage) => {
            stages.push(format!("{stage:?}"));
            last_progress = None;
        }
        Event::Progress { done, total } => {
            assert!(done <= total, "progress {done} exceeds its total {total}");
            assert!(last_progress <= Some(done), "progress went backwards within a step");
            last_progress = Some(done);
        }
        Event::File(name) => {
            assert!(!name.is_empty());
            named.insert(stages.last().cloned().unwrap_or_default());
            placed_while_building |=
                stages.last().is_some_and(|stage| stage == "Rebuild") && half_patched(&world.install);
        }
        _ => {}
    })
    .unwrap();
    assert!(outcome.immediate, "{outcome:?}");
    // Files are placed during the step that builds them, and no step is announced for it:
    // the app takes each announcement for the start of something new. The trial, which a
    // run has on a drive that cannot hold all the new files at once, is something new: a
    // step of its own, between the check of the originals and the build.
    assert_eq!(stages, ["VerifyPatch", "VerifyInstall", "Trial", "Rebuild", "Apply"]);
    assert!(placed_while_building);
    assert!(named.contains("VerifyInstall") && named.contains("Trial") && named.contains("Rebuild"), "{named:?}");
    world.assert_patched("steps");
}

/// The file of [`case_that_cannot_be_built`] that cannot be built.
const UNBUILDABLE: &str = r"Data\C.wz";
/// What is wrong with it: the instructions make another file than the patch promises.
const WRONG_CHECKSUM: &str = "another checksum than promised";

/// Three files, each built from itself, so that each would go into place as soon as it is
/// built. The last one, [`UNBUILDABLE`], cannot be built, and `flaw` says why.
fn case_that_cannot_be_built(flaw: &str) -> Case {
    let (a, b, c) = (r"Data\A.wz", r"Data\B.wz", UNBUILDABLE);
    let mut case = Case::new(41, &[(a, 40 * KIB), (b, 30 * KIB), (c, 20 * KIB)]);
    case.rebuild_from_itself(a);
    case.rebuild_from_itself(b);
    let old = case.before[c].clone();
    case.builder.original(c, &old);
    match flaw {
        WRONG_CHECKSUM => case.builder.rebuild(c, &old, b"not what the instructions make", &[copy(c, 0, 10 * KIB)]),
        _ => case.builder.rebuild(c, &old, &old[..10 * KIB], &[copy(c, 15 * KIB, 10 * KIB)]),
    };
    fs::write(&case.patch, case.builder.bytes()).unwrap();
    case
}

#[test]
fn patch_that_cannot_be_built_is_found_before_anything_is_replaced() {
    // In rounds of some files at a time, the first file would be in place for good by the
    // time the last one shows that it cannot be built. So such a run has a trial.
    //
    // A copy from beyond the end of an original does not get as far as the trial: the
    // original is the right file by its checksum, so the patch is at fault, and the check
    // of the originals says so.
    let mut room = 0;
    for flaw in [WRONG_CHECKSUM, "a copy from beyond the end of the original"] {
        let case = case_that_cannot_be_built(flaw);
        // Both patches make new files of the same sizes. The room is worked out with the
        // one whose originals can be checked.
        if flaw == WRONG_CHECKSUM {
            room = least_room(&case.install, &case.patch);
        }
        let untouched = files_under(&case.install);
        let refused = |err: &Error| match flaw {
            WRONG_CHECKSUM => matches!(err, Error::NewFileMismatch(name) if name == UNBUILDABLE),
            _ => matches!(err, Error::CorruptPatch(_)),
        };

        // The drive has not the room for all the new files at once.
        let mut stages = Vec::new();
        let err = run_tight(&case.options(true), &Drive::with_room(&case.install, room), &GO, &mut |event| {
            if let Event::Stage(stage) = event {
                stages.push(stage);
            }
        })
        .unwrap_err();
        assert!(refused(&err), "{flaw}: {err}");
        let expected: &[Stage] = match flaw {
            WRONG_CHECKSUM => &[Stage::VerifyPatch, Stage::VerifyInstall, Stage::Trial],
            _ => &[Stage::VerifyPatch, Stage::VerifyInstall],
        };
        assert_eq!(stages, expected, "{flaw}");
        assert!(!half_patched(&case.install), "{flaw}");
        assert!(files_under(&case.install) == untouched, "{flaw}: the install folder was changed");

        // With room for all of them there is no trial, and none is needed. Every file is
        // built before the first is placed, so the build comes to the file that cannot be
        // built while the folder is still the old version. What it had built by then waits
        // in the work folder, as it does the safe way.
        let (mut stages, mut placed) = (Vec::new(), 0);
        let err = run_with_room(&case.options(true), &GO, &mut |event| match event {
            Event::Stage(stage) => stages.push(stage),
            Event::Placed { .. } => placed += 1,
            _ => {}
        })
        .unwrap_err();
        assert!(refused(&err), "{flaw}, with room: {err}");
        let expected: &[Stage] = match flaw {
            WRONG_CHECKSUM => &[Stage::VerifyPatch, Stage::VerifyInstall, Stage::Rebuild],
            _ => &[Stage::VerifyPatch, Stage::VerifyInstall],
        };
        assert_eq!(stages, expected, "{flaw}, with room");
        assert!(placed == 0 && !half_patched(&case.install), "{flaw}, with room: {placed} placed");
        assert!(case.game_files() == untouched, "{flaw}, with room: a file of the game was changed");
    }
}

#[test]
fn trial_comes_before_the_first_file_is_replaced_and_not_again_on_the_way_on() {
    let world = World::random(11, Layout::PerEntry);
    let (_, plan, checked) = look(&world.install, &world.patch);
    let to_try = stage::bytes_to_build(&plan, &checked, &Dirs::new(&world.install));
    let drive = Drive::tight(&world.install, &world.patch);
    let chosen = options(&world.install, &world.patch, true);

    // Paused in the middle of the trial: it has written nothing anywhere.
    let untouched = files_under(&world.install);
    let stop = AtomicBool::new(false);
    let mut trying = false;
    let paused = run_tight(&chosen, &drive, &stop, &mut |event| match event {
        Event::Stage(stage) => trying = stage == Stage::Trial,
        Event::Progress { .. } if trying => stop.store(true, Ordering::Relaxed),
        _ => {}
    });
    assert!(matches!(paused, Err(Error::Stopped)), "{paused:?}");
    assert!(files_under(&world.install) == untouched, "a trial left something behind");

    // Begun again, the trial goes through every byte that is to be built, and only then
    // does the first file go into place.
    let stop = AtomicBool::new(false);
    let mut stages = Vec::new();
    let mut tried = 0;
    let paused = run_tight(&chosen, &drive, &stop, &mut |event| {
        match event {
            Event::Stage(stage) => stages.push(stage),
            Event::Progress { done, total } if stages.last() == Some(&Stage::Trial) => {
                assert!(!half_patched(&world.install), "a file was replaced before the trial was over");
                assert_eq!(total, to_try);
                tried = done;
            }
            _ => {}
        }
        stop.store(half_patched(&world.install), Ordering::Relaxed);
    });
    assert!(matches!(paused, Err(Error::Stopped)), "{paused:?}");
    assert_eq!(stages, [Stage::VerifyPatch, Stage::VerifyInstall, Stage::Trial, Stage::Rebuild]);
    assert_eq!(tried, to_try);

    // A folder that is half patched is not tried again: there is no way back from there.
    let mut stages = Vec::new();
    run_tight(&chosen, &drive, &GO, &mut |event| {
        if let Event::Stage(stage) = event {
            stages.push(stage);
        }
    })
    .unwrap();
    assert_eq!(stages, [Stage::VerifyPatch, Stage::VerifyInstall, Stage::Rebuild, Stage::Apply]);
    world.assert_patched("after the trial");
}

#[test]
fn immediate_way_begins_only_on_ntfs_and_goes_on_wherever_it_has_begun() {
    let world = World::random(11, Layout::PerEntry);
    let drive = Drive::tight(&world.install, &world.patch);
    let chosen = options(&world.install, &world.patch, true);
    let not_chosen = options(&world.install, &world.patch, false);
    let untouched = files_under(&world.install);

    // On a drive where a file moved over another is not known to survive a power cut
    // whole, a run with the immediate way chosen is refused, for that and nothing else, and
    // before it has done anything at all. It is not sent the safe way: the player chose.
    for file_system in ["exFAT", "FAT32", ""] {
        let mut told = 0;
        let err = run_on_drive(&chosen, &drive, Some(file_system), &GO, &mut |_| told += 1).unwrap_err();
        let Error::Blocked(problems) = &err else { panic!("{file_system}: unexpected error: {err}") };
        assert!(
            matches!(&problems[..], [Problem::ImmediateNeedsNtfs { file_system: named }] if named == file_system),
            "{file_system}: {problems:?}"
        );
        assert_eq!(told, 0, "{file_system}: the run had begun before it was refused");
        assert!(files_under(&world.install) == untouched, "{file_system}: the install folder was changed");
    }
    // All the room in the world changes nothing about that.
    let roomy = World::random(12, Layout::PerEntry);
    let plenty = Drive::with_room(&roomy.install, 1 << 30);
    let err = run_on_drive(&options(&roomy.install, &roomy.patch, true), &plenty, Some("exFAT"), &GO, &mut |_| {})
        .unwrap_err();
    assert!(
        matches!(&err, Error::Blocked(problems) if matches!(&problems[..], [Problem::ImmediateNeedsNtfs { .. }])),
        "unexpected error: {err}"
    );
    roomy.assert_untouched("the immediate way chosen on exFAT");

    // Without the choice the kind of drive is no obstacle to anything. The safe way goes
    // through where it has room, and where it has none it is refused for room, with not a
    // word about the other way.
    let outcome =
        run_on_drive(&options(&roomy.install, &roomy.patch, false), &plenty, Some("exFAT"), &GO, &mut |_| {}).unwrap();
    assert!(!outcome.immediate, "{outcome:?}");
    roomy.assert_patched("the safe way on exFAT");
    let err = run_on_drive(&not_chosen, &drive, Some("exFAT"), &GO, &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, Error::Blocked(problems) if matches!(&problems[..], [Problem::NotEnoughSpace { .. }])),
        "unexpected error: {err}"
    );

    // On NTFS it begins, however the name is spelled.
    let stop = AtomicBool::new(false);
    let paused = run_on_drive(&chosen, &drive, Some("ntfs"), &stop, &mut |_| {
        stop.store(half_patched(&world.install), Ordering::Relaxed);
    });
    assert!(matches!(paused, Err(Error::Stopped)) && half_patched(&world.install), "{paused:?}");
    // Once begun it goes on wherever the folder is by now, and whatever is chosen by now.
    // Stopping helps nobody.
    let outcome = run_on_drive(&not_chosen, &drive, Some("exFAT"), &GO, &mut |_| {}).unwrap();
    assert!(outcome.immediate, "{outcome:?}");
    world.assert_patched("finished on another file system, with the other way chosen");
}

#[test]
fn files_named_as_placed_are_the_new_files_of_the_folder_whichever_way_a_run_goes() {
    for layout in LAYOUTS {
        for seed in [3, 7, 11, 13] {
            // The safe way; the immediate way on a drive that is too tight for anything
            // else; and the immediate way with room for everything.
            for way in ["safe", "immediate, tight", "immediate, roomy"] {
                let what = format!("{layout:?} seed {seed}, {way}");
                let world = World::random(seed, layout);
                let mut placed = Vec::new();
                let mut building = false;
                let mut named_while_building = 0;
                let mut on_event = |event: Event| {
                    if let Event::Stage(stage) = &event {
                        building = *stage == Stage::Rebuild;
                    }
                    named_while_building += usize::from(building && matches!(event, Event::Placed { .. }));
                    note_placed(&mut placed, event);
                };
                let chosen = options(&world.install, &world.patch, true);
                let outcome = match way {
                    "safe" => run(&world.options(), &GO, &mut on_event),
                    "immediate, tight" => {
                        run_tight(&chosen, &Drive::tight(&world.install, &world.patch), &GO, &mut on_event)
                    }
                    _ => run_with_room(&chosen, &GO, &mut on_event),
                }
                .unwrap_or_else(|e| panic!("{what}: {e}"));
                world.assert_patched(&what);
                world.assert_named_once(&placed, &what);
                // What a run counts as changed is these files and the files it removed.
                let removed = world.before.keys().filter(|name| !world.after.contains_key(*name)).count();
                assert_eq!(outcome.files_changed, placed.len() + removed, "{what}");
                // The safe way names its files when its apply is finished. The immediate way
                // names those it puts in place early while it is still at the step of
                // building, whether it builds them a few at a time or all at once.
                assert_eq!(outcome.immediate, way != "safe", "{what}");
                assert_eq!(named_while_building > 0, outcome.immediate, "{what}: {named_while_building}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// The immediate way as the player's choice: taken by every run, whatever room there is.
// ---------------------------------------------------------------------------------------

/// Runs on a drive that is NTFS and has room for anything.
fn run_with_room(
    options: &Options,
    stop: &AtomicBool,
    on_event: &mut (dyn FnMut(Event) + Send),
) -> Result<Outcome, Error> {
    let drive = Drive::with_room(&options.install_dir, 1 << 40);
    run_on_drive(options, &drive, Some("NTFS"), stop, on_event)
}

#[test]
fn immediate_way_that_was_chosen_is_taken_with_room_for_everything() {
    let mut with_trial = 0;
    for layout in LAYOUTS {
        for seed in [2, 4, 6, 8] {
            let what = format!("{layout:?} seed {seed}");
            let safe = World::random(seed, layout);
            let by_the_safe_way = run(&safe.options(), &GO, &mut |_| {}).unwrap();

            let world = World::random(seed, layout);
            let (mut stages, mut building) = (Vec::new(), false);
            let chosen = options(&world.install, &world.patch, true);
            let outcome = run_with_room(&chosen, &GO, &mut |event| match event {
                Event::Stage(stage) => {
                    stages.push(stage);
                    building = stage == Stage::Rebuild;
                }
                // With room for all of it, everything is built in one go before anything is
                // placed. Until the last new file is built, the folder is the old version.
                Event::Progress { done, total } if building && done < total => {
                    assert!(!half_patched(&world.install), "{what}: a file was placed before all were built");
                }
                _ => {}
            })
            .unwrap_or_else(|e| panic!("{what}: {e}"));
            // It is the way that was chosen, and it ends where the safe way ends. It has no
            // trial: building every file before the first is placed shows all that a trial
            // would, so the steps are the four of the safe way.
            assert!(outcome.immediate, "{what}: {outcome:?}");
            assert_eq!(stages, [Stage::VerifyPatch, Stage::VerifyInstall, Stage::Rebuild, Stage::Apply], "{what}");
            assert_eq!(outcome.files_changed, by_the_safe_way.files_changed, "{what}");
            world.assert_patched(&what);
            assert!(
                files_under(&world.install) == files_under(&safe.install),
                "{what}: the two ways ended differently"
            );

            // The same patch on a drive that cannot hold all its new files at once is tried
            // first, and ends in the same folder.
            let tight = World::random(seed, layout);
            let (_, plan, checked) = look(&tight.install, &tight.patch);
            let to_build = stage::bytes_to_build(&plan, &checked, &Dirs::new(&tight.install));
            let room = least_room(&tight.install, &tight.patch);
            let mut tried = false;
            let chosen = options(&tight.install, &tight.patch, true);
            run_tight(&chosen, &Drive::with_room(&tight.install, room), &GO, &mut |event| {
                tried |= matches!(event, Event::Stage(Stage::Trial));
            })
            .unwrap_or_else(|e| panic!("{what}, tight: {e}"));
            assert_eq!(tried, to_build > room + ALLOWANCE, "{what}: {to_build} to build, room for {room}");
            with_trial += usize::from(tried);
            assert!(
                files_under(&tight.install) == files_under(&world.install),
                "{what}: the run with a trial and the run without one ended differently"
            );
        }
    }
    assert!(with_trial > 0, "no drive was too tight for all the new files at once");
}

#[test]
fn small_patches_go_the_immediate_way_like_any_other() {
    type Entries = fn(&mut Case);
    let new_files: Entries = |case| {
        case.create(r"Data\New\C.wz", 8 * KIB);
        case.create(r"Data\D.wz", 3 * KIB);
    };
    let deletes: Entries = |case| {
        case.delete(r"Data\A.wz");
        case.delete(r"Data\Old\");
    };
    let folders: Entries = |case| {
        case.builder.create_dir(r"Data\Made\Deep").create_dir(r"Data\Old");
    };
    let patches: [(&str, Entries, usize); 5] = [
        ("whole new files only", new_files, 2),
        ("deletes only", deletes, 2),
        ("folders only", folders, 0),
        ("nothing at all", |_| (), 0),
        (
            "a little of everything, as a minor patch has it",
            |case| {
                case.rebuild_from_itself(r"Data\A.wz");
                case.create(r"Data\New\C.wz", KIB);
                case.delete(r"Data\Old\B.wz");
            },
            3,
        ),
    ];
    for (what, entries, changes) in patches {
        // With room for everything, and on a drive with exactly the room this way needs.
        for tight in [false, true] {
            let what = format!("{what}, tight: {tight}");
            let mut case = Case::new(51, &[(r"Data\A.wz", 10 * KIB), (r"Data\Old\B.wz", 5 * KIB)]);
            entries(&mut case);
            case.write();
            let drive = if tight { case.tight_drive() } else { Drive::with_room(&case.install, 1 << 40) };
            let mut stages = Vec::new();
            let outcome = run_tight(&case.options(true), &drive, &GO, &mut |event| {
                if let Event::Stage(stage) = event {
                    stages.push(stage);
                }
                // These patches are small enough for even the tight drive to hold all their
                // new files at once, so each is built whole before its first file goes in.
                if matches!(event, Event::Placed { .. }) {
                    assert!(case.left_to_build().is_empty(), "{what}: a file went in before all were built");
                }
            })
            .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(outcome, Outcome { files_changed: changes, immediate: true }, "{what}");
            case.assert_patched(&what);
            assert!(!half_patched(&case.install), "{what}");
            // So none of them has a trial, whatever it builds.
            assert!(!stages.contains(&Stage::Trial), "{what}: {stages:?}");
            if what.starts_with("folders only") {
                assert!(case.install.join("Data").join("Made").join("Deep").is_dir(), "{what}");
            }

            // Run again, it finds nothing to do, goes the way that was chosen all the same,
            // and leaves nothing behind.
            let again =
                run_tight(&case.options(true), &drive, &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(again, Outcome { files_changed: 0, immediate: true }, "{what}, again");
            case.assert_patched(&format!("{what}, again"));
        }
    }
}

#[test]
fn immediate_way_never_asks_for_more_room_than_the_safe_way() {
    // The immediate way asks for some room to spare at its start, a gigabyte outside
    // tests, so that it does not build one file at a time. A small patch has less than
    // that to build in all, and must not be refused for room that the safe way would not
    // have needed.
    let mut case = Case::new(52, &[(r"Data\A.wz", 60 * KIB), (r"Data\B.wz", 40 * KIB)]);
    case.rebuild_from_itself(r"Data\A.wz");
    case.rebuild_from_itself(r"Data\B.wz");
    case.create(r"Data\C.wz", 20 * KIB);
    let plan = case.write();
    let to_build = plan.new_bytes();
    let on_a_drive_with = |free: u64, immediate: bool| {
        let free = move |_: &Path| -> io::Result<u64> { Ok(free) };
        let disk = Disk { free: &free, slack: immediate::SLACK, file_system: Some("NTFS") };
        run_on_disk(&LIVE, &case.options(immediate), &disk, &GO, &mut |_| {})
    };
    let asked_for = |refused: Result<Outcome, Error>| match refused {
        Err(Error::Blocked(problems)) => match &problems[..] {
            [Problem::NotEnoughSpace { needed, .. }] => *needed,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };

    // One byte short of what the safe way needs, both ways are refused, and ask for the
    // same: the room for every new file, and the margin.
    let enough = to_build + SPACE_MARGIN;
    assert_eq!(asked_for(on_a_drive_with(enough - 1, false)), enough);
    assert_eq!(asked_for(on_a_drive_with(enough - 1, true)), enough);
    // With exactly that, the immediate way goes through.
    let outcome = on_a_drive_with(enough, true).unwrap();
    assert!(outcome.immediate, "{outcome:?}");
    case.assert_patched("with the room the safe way needs");
}

#[test]
fn room_each_way_asks_for_is_told_without_a_run_and_is_what_the_run_asks_for() {
    for layout in LAYOUTS {
        for seed in [5, 9, 14] {
            let what = format!("{layout:?} seed {seed}");
            let world = World::random(seed, layout);
            let (patch, plan, _) = look(&world.install, &world.patch);
            let untouched = files_under(&world.install);
            let room = room_needed(&patch, &plan, &world.install);
            assert!(files_under(&world.install) == untouched, "{what}: asking changed the folder");
            assert!(0 < room.immediate && room.immediate <= room.safe, "{what}: {room:?}");
            assert_eq!((room.of_run(false, false), room.of_run(true, false)), (room.safe, room.immediate));
            assert_eq!(
                room.of_run(false, true),
                room.immediate,
                "{what}: a half-patched folder goes the immediate way"
            );

            // A run on a drive with nothing free says what it needs. It is the same figure,
            // for either way, with the room to spare that a run outside tests asks for.
            let asked_by_a_run = |immediate: bool| {
                let free = |_: &Path| -> io::Result<u64> { Ok(0) };
                let disk = Disk { free: &free, slack: immediate::SLACK, file_system: Some("NTFS") };
                let refused =
                    run_on_disk(&LIVE, &options(&world.install, &world.patch, immediate), &disk, &GO, &mut |_| {});
                match refused {
                    Err(Error::Blocked(problems)) => match &problems[..] {
                        [Problem::NotEnoughSpace { needed, .. }] => *needed,
                        other => panic!("{what}: {other:?}"),
                    },
                    other => panic!("{what}: {other:?}"),
                }
            };
            assert_eq!(room.safe, asked_by_a_run(false), "{what}: the safe way");
            assert_eq!(room.immediate, asked_by_a_run(true), "{what}: the immediate way");

            // Once the patch is in, nothing is left to make room for.
            run(&world.options(), &GO, &mut |_| {}).unwrap();
            let (patch, plan, _) = look(&world.install, &world.patch);
            assert_eq!(room_needed(&patch, &plan, &world.install), Room { safe: 0, immediate: 0 }, "{what}");
        }
    }
}

/// A small install and a patch that rebuilds, creates and deletes in it, for the tests that
/// pause a run at every one of its moments.
fn small_case() -> (Case, Plan) {
    let mut case = Case::new(53, &[(r"Data\A.wz", 40 * KIB), (r"Data\B.wz", 30 * KIB), (r"Data\Old\C.wz", 10 * KIB)]);
    case.rebuild_from_itself(r"Data\A.wz");
    case.rebuild(r"Data\B.wz", &[copy(r"Data\B.wz", 0, 20 * KIB), copy(r"Data\A.wz", KIB, 4 * KIB)]);
    case.create(r"Data\New\D.wz", 12 * KIB);
    case.delete(r"Data\Old\");
    let plan = case.write();
    (case, plan)
}

#[test]
fn run_paused_at_any_moment_is_finished_whichever_way_is_chosen_by_then() {
    // The way the run began, and the way that is chosen when it is taken up again: the
    // same way, and the choice changed in between, in both directions.
    for (began, resumed) in [(true, true), (true, false), (false, true)] {
        for pause_at in 0.. {
            let what = format!("began immediate: {began}, resumed immediate: {resumed}, paused at {pause_at}");
            let (case, plan) = small_case();
            let stop = AtomicBool::new(false);
            let mut told = 0;
            let first = run_with_room(&case.options(began), &stop, &mut |_| {
                told += 1;
                stop.store(told > pause_at, Ordering::Relaxed);
            });
            match first {
                // The run was through before it had that much to tell.
                Ok(_) => {
                    case.assert_patched(&what);
                    assert!(pause_at > 10, "{what}: the run tells of only {pause_at} things");
                    break;
                }
                Err(Error::Stopped) => {}
                Err(err) => panic!("{what}: {err}"),
            }
            // A pause leaves every file whole. What it has left is a folder that is half
            // patched, if a new file went in ahead of the others, or the old folder with
            // some of the new files built beside it.
            case.assert_whole(&plan, &what);
            let half = half_patched(&case.install);
            assert!(began || !half, "{what}: the safe way left the folder half patched");

            // Taken up again, the run goes the way that is chosen now, and uses what the
            // other way had built. Only a folder that is half patched has no choice left:
            // it is finished the immediate way whatever is chosen.
            let outcome =
                run_with_room(&case.options(resumed), &GO, &mut |_| {}).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(outcome.immediate, resumed || half, "{what}");
            case.assert_patched(&what);
        }
    }
}

// ---------------------------------------------------------------------------------------
// The trial, which only a run in rounds has: no file is replaced before every new file has
// been built once, whatever room the run began with and whatever room it goes on with.
// ---------------------------------------------------------------------------------------

/// An install and a patch whose new files are far more, taken together, than a drive that
/// is tight for the immediate way can hold at once: each of the three large files is built
/// from itself and can go into place as soon as it is built. Every new file is larger than
/// what a made-up drive has to spare.
fn case_of_rounds() -> (Case, Plan) {
    let large = [r"Data\A.wz", r"Data\B.wz", r"Data\C.wz"];
    let mut files: Vec<(&str, usize)> = large.iter().map(|name| (*name, 100 * KIB)).collect();
    files.push((r"Data\Old\E.wz", 10 * KIB));
    let mut case = Case::new(57, &files);
    for name in large {
        case.rebuild_from_itself(name);
    }
    case.create(r"Data\New\D.wz", 40 * KIB);
    case.delete(r"Data\Old\");
    let plan = case.write();
    (case, plan)
}

/// Watches a run and fails at the moment a file is replaced before every new file has been
/// built once: for real, or in a trial that went through all of them.
struct Watch<'a> {
    case: &'a Case,
    what: &'a str,
    stages: Vec<Stage>,
    /// How far the trial has got, and how far it has to go.
    tried: (u64, u64),
    /// The first replacement has been seen and judged.
    judged: bool,
}

impl<'a> Watch<'a> {
    /// A watch of a run on the folder of `case` as it is now. A folder that is half patched
    /// already has had its first replacement, and is not judged again.
    fn new(case: &'a Case, what: &'a str) -> Watch<'a> {
        Watch { case, what, stages: Vec::new(), tried: (0, 0), judged: half_patched(&case.install) }
    }

    fn tried(&self) -> bool {
        self.stages.contains(&Stage::Trial)
    }

    fn see(&mut self, event: &Event) {
        match event {
            Event::Stage(stage) => self.stages.push(*stage),
            Event::Progress { done, total } if self.stages.last() == Some(&Stage::Trial) => {
                self.tried = (*done, *total)
            }
            _ => {}
        }
        // The folder is marked as half patched before its first file is replaced.
        if self.judged || !half_patched(&self.case.install) {
            return;
        }
        self.judged = true;
        let tried_all = self.tried() && self.stages.last() != Some(&Stage::Trial) && self.tried.0 == self.tried.1;
        let left = self.case.left_to_build();
        assert!(
            tried_all || left.is_empty(),
            "{}: a file is replaced while {left:?} were never built, and the trial got to {:?}",
            self.what,
            self.tried
        );
    }
}

#[test]
fn run_begun_with_room_and_gone_on_without_it_is_tried_before_its_first_file_is_replaced() {
    let (mut tried_then, mut half_patched_then) = (0, 0);
    for pause_at in 0.. {
        let what = format!("paused at {pause_at}");
        let (case, plan) = case_of_rounds();
        let stop = AtomicBool::new(false);
        let mut told = 0;
        let mut watch = Watch::new(&case, &what);
        let first = run_with_room(&case.options(true), &stop, &mut |event| {
            told += 1;
            stop.store(told > pause_at, Ordering::Relaxed);
            watch.see(&event);
        });
        // With room for everything the run has no trial, and has placed nothing before
        // every new file was built: the watch saw to that.
        assert!(!watch.tried(), "{what}: {:?}", watch.stages);
        match first {
            Ok(_) => {
                case.assert_patched(&what);
                break;
            }
            Err(Error::Stopped) => {}
            Err(err) => panic!("{what}: {err}"),
        }
        case.assert_whole(&plan, &what);
        let (half, left) = (half_patched(&case.install), case.left_to_build().len());

        // By the time it goes on, the drive has no more than the least this way can work
        // with. What is left to build goes in rounds now, so it is tried first: all of it,
        // and before the files the first run built are placed. Only a folder that is half
        // patched is not tried, and it is half patched only where nothing is left to build.
        assert!(!half || left == 0, "{what}: half patched with {left} files still to build");
        let drive = case.tight_drive_from_here(&plan);
        let what = format!("{what}, gone on");
        let mut watch = Watch::new(&case, &what);
        let outcome = run_tight(&case.options(true), &drive, &GO, &mut |event| watch.see(&event))
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(outcome.immediate, "{what}: {outcome:?}");
        assert_eq!(watch.tried(), left > 0, "{what}: {left} files were left to build, {:?}", watch.stages);
        case.assert_patched(&what);
        tried_then += usize::from(watch.tried());
        half_patched_then += usize::from(half);
    }
    assert!(tried_then > 0, "no run that went on had anything left to try");
    assert!(half_patched_then > 0, "no pause came after a file was placed");
}

#[test]
fn run_begun_without_room_and_gone_on_with_it_builds_the_rest_before_its_first_file_is_replaced() {
    let (mut whole_then, mut half_patched_then) = (0, 0);
    for pause_at in 0.. {
        let what = format!("paused at {pause_at}");
        let (case, plan) = case_of_rounds();
        let drive = case.tight_drive();
        let stop = AtomicBool::new(false);
        let mut told = 0;
        let mut watch = Watch::new(&case, &what);
        let first = run_tight(&case.options(true), &drive, &stop, &mut |event| {
            told += 1;
            stop.store(told > pause_at, Ordering::Relaxed);
            watch.see(&event);
        });
        match first {
            Ok(_) => {
                // On the tight drive the run went in rounds, and had its trial.
                assert!(watch.tried(), "{what}: {:?}", watch.stages);
                case.assert_patched(&what);
                break;
            }
            Err(Error::Stopped) => {}
            Err(err) => panic!("{what}: {err}"),
        }
        case.assert_whole(&plan, &what);
        let half = half_patched(&case.install);
        assert!(!half || watch.tried(), "{what}: half patched without a trial");

        // By the time it goes on, the drive has room for everything. No trial is made
        // then: what is left is built in one round before the first file is replaced, and
        // a folder that is half patched has been tried.
        let what = format!("{what}, gone on");
        let mut watch = Watch::new(&case, &what);
        let outcome = run_with_room(&case.options(true), &GO, &mut |event| watch.see(&event))
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(outcome.immediate, "{what}: {outcome:?}");
        assert!(!watch.tried(), "{what}: {:?}", watch.stages);
        case.assert_patched(&what);
        whole_then += usize::from(!half);
        half_patched_then += usize::from(half);
    }
    assert!(whole_then > 0, "no pause came before a file was placed");
    assert!(half_patched_then > 0, "no pause came after a file was placed");
}

#[test]
fn patch_that_cannot_be_built_is_found_before_anything_is_replaced_though_the_room_changes() {
    // The run is paused before it comes to the file that cannot be built, and goes on with
    // other room than it began with: begun with room for everything and gone on without,
    // and the other way round. Whichever of the two finds the file, by a trial or by the
    // build, no file of the game has been replaced by then.
    for began_with_room in [true, false] {
        let mut gone_on = 0;
        for pause_at in 0.. {
            let what = format!("began with room: {began_with_room}, paused at {pause_at}");
            let case = case_that_cannot_be_built(WRONG_CHECKSUM);
            let plan = look(&case.install, &case.patch).1;
            let untouched = files_under(&case.install);
            let found = |result: &Result<Outcome, Error>| matches!(result, Err(Error::NewFileMismatch(name)) if name == UNBUILDABLE);
            let stop = AtomicBool::new(false);
            let (mut told, mut placed) = (0, 0);
            let first = {
                let mut at_event = |event: Event| {
                    told += 1;
                    stop.store(told > pause_at, Ordering::Relaxed);
                    placed += usize::from(matches!(event, Event::Placed { .. }));
                };
                match began_with_room {
                    true => run_with_room(&case.options(true), &stop, &mut at_event),
                    false => run_tight(&case.options(true), &case.tight_drive(), &stop, &mut at_event),
                }
            };
            assert!(placed == 0 && !half_patched(&case.install), "{what}: {placed} placed");
            assert!(case.game_files() == untouched, "{what}: a file of the game was changed");
            // The run came to the file before it came to the pause.
            if found(&first) {
                break;
            }
            assert!(matches!(first, Err(Error::Stopped)), "{what}: {first:?}");

            let what = format!("{what}, gone on");
            let mut count = |event: Event| placed += usize::from(matches!(event, Event::Placed { .. }));
            let second = match began_with_room {
                true => run_tight(&case.options(true), &case.tight_drive_from_here(&plan), &GO, &mut count),
                false => run_with_room(&case.options(true), &GO, &mut count),
            };
            assert!(found(&second), "{what}: {second:?}");
            assert!(placed == 0 && !half_patched(&case.install), "{what}: {placed} placed");
            assert!(case.game_files() == untouched, "{what}: a file of the game was changed");
            gone_on += 1;
        }
        assert!(gone_on > 3, "began with room: {began_with_room}: only {gone_on} pauses came before the file");
    }
}
