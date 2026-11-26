//! Tests of how new files are built: copies fetched ahead by reader threads and written in
//! order, finished files synced and moved off the writing thread, and what a paused, failed
//! or killed run leaves behind.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use mfp_engine::apply;
use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Layout, Plan};
use mfp_engine::stage::{self, Checked, Dirs};
use mfp_engine::storage::{Choice, Profile};
use mfp_engine::{Error, Event, Options};
use support::{GO, LAYOUTS, Op, PatchBuilder, Rng, copy, files_under, on_disk, run};

const MIB: usize = 1 << 20;

/// An install folder made of `files`, and room for a patch file beside it.
struct Case {
    _temp: tempfile::TempDir,
    install: PathBuf,
    patch: PathBuf,
    before: BTreeMap<String, Vec<u8>>,
    layout: Layout,
    builder: PatchBuilder,
    /// What the patch builds: name and contents of every new file.
    after: BTreeMap<String, Vec<u8>>,
}

impl Case {
    fn new(layout: Layout, files: Vec<(&str, Vec<u8>)>) -> Case {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Maple");
        fs::create_dir_all(install.join("Data")).unwrap();
        fs::write(install.join("MapleStory.exe"), b"exe").unwrap();
        let mut before = BTreeMap::new();
        for (name, contents) in files {
            let path = on_disk(&install, name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, &contents).unwrap();
            before.insert(name.to_owned(), contents);
        }
        let patch = temp.path().join("made-up.patch");
        Case { _temp: temp, install, patch, before, layout, builder: PatchBuilder::new(layout), after: BTreeMap::new() }
    }

    /// What the instructions make, worked out here and not by the engine.
    fn made_by(&self, ops: &[Op]) -> Vec<u8> {
        let mut new = Vec::new();
        for op in ops {
            match op {
                Op::Literal(data) => new.extend(data),
                Op::Fill(byte, len) => new.resize(new.len() + *len as usize, *byte),
                Op::Copy(from, start, len) => {
                    new.extend(&self.before[from][*start as usize..(*start + *len) as usize]);
                }
            }
        }
        new
    }

    /// Adds an entry that rebuilds `name` from `ops`.
    fn rebuild(&mut self, name: &str, ops: &[Op]) {
        let new = self.made_by(ops);
        self.rebuild_claiming(name, ops, &new);
        self.after.insert(name.to_owned(), new);
    }

    /// Adds a rebuild entry that says the result is `new`, whether or not that is true.
    fn rebuild_claiming(&mut self, name: &str, ops: &[Op], new: &[u8]) {
        self.builder.original(name, &self.before[name]);
        for op in ops {
            if let Op::Copy(from, ..) = op {
                self.builder.original(from, &self.before[from]);
            }
        }
        self.builder.rebuild(name, &self.before[name], new, ops);
    }

    fn create(&mut self, name: &str, contents: Vec<u8>) {
        self.builder.create(name, &contents);
        self.after.insert(name.to_owned(), contents);
    }

    /// Writes the patch file and reads it the way a run does, up to the check of the
    /// originals.
    fn open(&self) -> Opened {
        fs::write(&self.patch, self.builder.bytes()).unwrap();
        let patch = PatchFile::open(&self.patch).unwrap();
        let plan = plan::scan(&patch, &GO).unwrap();
        assert_eq!(plan.layout, self.layout);
        let checked = stage::check_originals(&plan, &self.install, &GO, &|_, _| {}).unwrap();
        Opened { patch, plan, checked, dirs: Dirs::new(&self.install) }
    }

    fn options(&self) -> Options {
        support::options(&self.install, &self.patch)
    }

    fn assert_install_untouched(&self, what: &str) {
        for (name, contents) in &self.before {
            assert!(fs::read(on_disk(&self.install, name)).unwrap() == *contents, "{what}: {name} was changed");
        }
    }

    fn assert_install_patched(&self, what: &str) {
        for (name, contents) in &self.after {
            let found = fs::read(on_disk(&self.install, name)).unwrap_or_else(|e| panic!("{what}: {name}: {e}"));
            assert!(found == *contents, "{what}: {name} has the wrong contents ({} bytes)", found.len());
        }
        assert!(!self.install.join(".maple-fast-patch").exists(), "{what}: work folder left behind");
    }

    /// Fails unless every file among the staged files is one the patch builds, whole and
    /// exactly right. With `all` set, every file the patch builds has to be there.
    fn assert_staged(&self, dirs: &Dirs, all: bool, what: &str) {
        let staged = files_under(&dirs.staged());
        for (name, found) in &staged {
            let wanted =
                self.after.get(name).unwrap_or_else(|| panic!("{what}: {name} is staged but not built by the patch"));
            assert!(found == wanted, "{what}: staged {name} is not complete and correct ({} bytes)", found.len());
        }
        if all {
            assert_eq!(staged.len(), self.after.len(), "{what}: not every new file is staged");
        }
    }
}

struct Opened {
    patch: PatchFile,
    plan: Plan,
    checked: Checked,
    dirs: Dirs,
}

impl Opened {
    fn stage(&self) -> mfp_engine::Result<()> {
        stage::stage(&self.patch, &self.plan, &self.checked, &self.dirs, &GO, &mut |_, _, _| {})
    }

    /// Stages and adds up the progress reported, checking it against the total as it goes.
    fn stage_counting(&self, what: &str) -> u64 {
        let total = self.plan.new_bytes();
        let mut done = 0u64;
        stage::stage(&self.patch, &self.plan, &self.checked, &self.dirs, &GO, &mut |_, bytes, _| {
            done += bytes;
            assert!(done <= total, "{what}: progress {done} passed its total {total}");
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        done
    }

    fn apply(&self) -> usize {
        apply::apply(&self.plan, &self.checked, &self.dirs, &mut |_| {}).unwrap()
    }
}

/// Names of the files in the work folder that a run was still writing.
fn unfinished(dirs: &Dirs) -> Vec<String> {
    let Ok(children) = fs::read_dir(dirs.work()) else { return Vec::new() };
    let mut names: Vec<String> = children
        .flatten()
        .map(|child| child.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("building"))
        .collect();
    names.sort();
    names
}

/// Runs `work` on another thread and fails the test if it has not come back in a minute:
/// a run that waits for a thread that will never answer must show up as a failure, not as
/// a test that never ends.
fn must_finish<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, waiting) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(work());
    });
    waiting.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| panic!("{what}: did not finish"))
}

#[test]
fn copies_longer_than_a_piece_and_longer_than_the_window_arrive_whole() {
    // Copies are fetched in pieces of 1 MiB and at most 32 MiB wait to be written. These
    // copies are longer than either, start and end off every boundary, and have patch data
    // and filler longer than a piece between them.
    let mut rng = Rng::new(41);
    let big_len = 40 * MIB + 4321;
    let other_len = 3 * MIB + 17;
    let mut case = Case::new(
        Layout::Table,
        vec![
            (r"Data\Big.wz", rng.bytes(big_len)),
            (r"Data\Other.wz", rng.bytes(other_len)),
            (r"Data\Small.wz", rng.bytes(900)),
        ],
    );
    let ops = [
        copy(r"Data\Big.wz", 5, 36 * MIB + 7),
        Op::Literal(rng.bytes(3 * MIB + 1)),
        copy(r"Data\Other.wz", MIB - 3, MIB + 6),
        Op::Fill(0xAB, 0xF_FFFF),
        copy(r"Data\Small.wz", 0, 900),
        // Ends exactly where the file ends.
        copy(r"Data\Big.wz", big_len - (2 * MIB + 5), 2 * MIB + 5),
        Op::Literal(vec![7]),
        copy(r"Data\Other.wz", 0, other_len),
        // Exactly one piece, and exactly two.
        copy(r"Data\Big.wz", 4096, MIB),
        copy(r"Data\Big.wz", 12345, 2 * MIB),
    ];
    case.rebuild(r"Data\Big.wz", &ops);
    // A second file made of one copy that fills the window more than once over.
    case.rebuild(r"Data\Other.wz", &[copy(r"Data\Big.wz", 1, big_len - 1)]);

    let opened = case.open();
    assert_eq!(opened.stage_counting("long copies"), opened.plan.new_bytes());
    case.assert_staged(&opened.dirs, true, "long copies");
    assert!(unfinished(&opened.dirs).is_empty());
    assert_eq!(opened.apply(), 2);
    case.assert_install_patched("long copies");
}

#[test]
fn copies_reach_the_last_byte_of_files_of_any_size() {
    // Originals are read past the file cache, which only works in whole 4096-byte sectors.
    // Files end wherever they end, so the last sector of most files is not whole.
    for layout in LAYOUTS {
        let mut rng = Rng::new(43);
        let sizes = [1usize, 100, 4095, 4096, 4097, 8191, 8192, 8193, 12_411, 70_001, MIB, MIB + 4097];
        let files: Vec<(String, Vec<u8>)> =
            sizes.iter().map(|size| (format!("Data\\Size_{size}.wz"), rng.bytes(*size))).collect();
        let mut case =
            Case::new(layout, files.iter().map(|(name, contents)| (name.as_str(), contents.clone())).collect());
        for (name, contents) in &files {
            let size = contents.len();
            // (start, length) pairs around the end of the file and around sector boundaries.
            let mut pairs = vec![(0, size), (size - 1, 1), (0, 1)];
            for back in [2usize, 1000, 4095, 4096, 4097, 5000] {
                if size >= back {
                    pairs.push((size - back, back)); // ends exactly at the end
                    pairs.push((size - back, back - 1)); // ends one byte before it
                }
                if size > back {
                    pairs.push((size - back - 1, back)); // the same, one byte earlier
                }
            }
            for (start, len) in
                [(4095, 1), (4095, 2), (4096, 1), (4096, 4096), (4097, 4095), (1, 4095), (0, 4096), (0, 4097)]
            {
                if start + len <= size {
                    pairs.push((start, len));
                }
            }
            let ops: Vec<Op> =
                pairs.into_iter().filter(|(_, len)| *len > 0).map(|(start, len)| copy(name, start, len)).collect();
            case.rebuild(name, &ops);
        }

        let opened = case.open();
        opened.stage().unwrap_or_else(|e| panic!("{layout:?}: {e}"));
        case.assert_staged(&opened.dirs, true, &format!("{layout:?}"));
        assert_eq!(opened.apply(), sizes.len());
        case.assert_install_patched(&format!("{layout:?}"));
    }
}

#[test]
fn copy_that_runs_past_the_end_of_an_original_fails_and_leaves_nothing() {
    // A copy that needs more than its original has. If the original is the very file the
    // patch was made for, the patch is at fault, and the check of the originals says so
    // before anything is built. If the original got shorter after it was checked, it is
    // the build that runs into its end: that has to end with a read error, as it always
    // did; not hang, not build.
    let size = 10_000usize;
    let bad_copies: [(usize, usize); 6] = [
        (9_000, 2_000),  // starts inside, runs past
        (9_999, 2),      // one byte too many
        (10_000, 1),     // starts exactly at the end
        (12_288, 4_096), // a whole sector beyond the end
        (20_000, 10),
        (0, 3 * MIB), // several pieces, all but the first beyond the end
    ];
    for (start, len) in bad_copies {
        for (before, after) in [(0, 0), (300, 300), (1, 2000)] {
            let what = format!("copy of {len} at {start} after {before} and before {after} good ones");
            // The original with every byte the copy asks for, of which the first `size` are
            // the original that is too short for it.
            let whole = Rng::new(47).bytes(start + len);
            let add_entries = |case: &mut Case| {
                // A whole file that is finished before the rebuild fails.
                case.create(r"Data\Fresh.wz", b"finished before the rebuild fails".to_vec());
                let mut ops: Vec<Op> = (0..before).map(|_| copy(r"Data\A.wz", 0, 100)).collect();
                ops.push(copy(r"Data\A.wz", start, len));
                ops.extend((0..after).map(|_| copy(r"Data\A.wz", 0, 100)));
                case.rebuild_claiming(r"Data\A.wz", &ops, b"whatever the patch claims");
            };

            // A patch that asks this of an original that never was any longer: it lies.
            let mut lying = Case::new(Layout::Table, vec![(r"Data\A.wz", whole[..size].to_vec())]);
            add_entries(&mut lying);
            fs::write(&lying.patch, lying.builder.bytes()).unwrap();
            let plan = plan::scan(&PatchFile::open(&lying.patch).unwrap(), &GO).unwrap();
            let err = stage::check_originals(&plan, &lying.install, &GO, &|_, _| {}).unwrap_err();
            assert!(matches!(err, Error::CorruptPatch(_)), "{what}: unexpected error: {err}");
            lying.assert_install_untouched(&what);

            // A patch that asks it of an original that has those bytes, and loses them once
            // it has been checked.
            let mut case = Case::new(Layout::Table, vec![(r"Data\A.wz", whole.clone())]);
            add_entries(&mut case);
            let opened = Arc::new(case.open());
            let original = on_disk(&case.install, r"Data\A.wz");
            fs::write(&original, &whole[..size]).unwrap();
            let staging = opened.clone();
            let err = must_finish(&what, move || staging.stage()).unwrap_err();
            let Error::Io { what: action, source } = &err else { panic!("{what}: unexpected error: {err}") };
            assert!(action.starts_with("read "), "{what}: {err}");
            assert_eq!(source.kind(), std::io::ErrorKind::UnexpectedEof, "{what}: {err}");
            // The finished file is kept, whole; the failed one is nowhere.
            case.assert_staged(&opened.dirs, true, &what);
            assert!(!opened.dirs.staged().join("Data/A.wz").exists(), "{what}: the file was staged");
            assert!(unfinished(&opened.dirs).is_empty(), "{what}: {:?} left behind", unfinished(&opened.dirs));
            // The original is as the test cut it down, and with that put right the install
            // is as it was.
            assert!(fs::read(&original).unwrap() == whole[..size], "{what}: the original was changed");
            fs::write(&original, &whole).unwrap();
            case.assert_install_untouched(&what);
        }
    }
}

#[test]
fn empty_and_one_byte_instructions_are_written_in_place() {
    // A copy of no bytes cannot be written in a patch file: its instruction word would be
    // zero, which ends the list. Patch data and filler of no bytes can.
    for layout in LAYOUTS {
        let mut rng = Rng::new(53);
        let a = rng.bytes(5000);
        let mut case = Case::new(layout, vec![(r"Data\A.wz", a), (r"Data\Empty.wz", b"will be emptied".to_vec())]);
        let mut ops = vec![Op::Literal(Vec::new()), Op::Fill(7, 0)];
        for i in 0..300usize {
            ops.push(copy(r"Data\A.wz", (i * 17) % 5000, 1));
            ops.push(Op::Literal(Vec::new()));
            ops.push(Op::Fill(i as u8, (i % 2) as u32));
            ops.push(Op::Literal(vec![i as u8; i % 2]));
            ops.push(copy(r"Data\A.wz", 4999, 1));
            ops.push(Op::Fill(1, 0));
        }
        ops.push(Op::Literal(Vec::new()));
        case.rebuild(r"Data\A.wz", &ops);
        // Instructions that add up to nothing make an empty file.
        case.rebuild(r"Data\Empty.wz", &[Op::Literal(Vec::new()), Op::Fill(0, 0), Op::Literal(Vec::new())]);

        let opened = case.open();
        assert_eq!(opened.stage_counting("tiny parts"), opened.plan.new_bytes(), "{layout:?}");
        case.assert_staged(&opened.dirs, true, &format!("{layout:?}"));
        assert_eq!(opened.apply(), 2);
        case.assert_install_patched(&format!("{layout:?}"));
        assert_eq!(fs::read(case.install.join("Data/Empty.wz")).unwrap(), b"");
    }
}

/// A patch of two rebuilds made of many small parts of every kind, plus whole files.
fn many_small_parts(layout: Layout, seed: u64, parts: usize) -> Case {
    let mut rng = Rng::new(seed);
    let names = [r"Data\A.wz", r"Data\Sub\B.wz", r"Data\C.wz"];
    let mut case = Case::new(
        layout,
        vec![(names[0], rng.bytes(300_000)), (names[1], rng.bytes(200_000)), (names[2], rng.bytes(5_000))],
    );
    case.create(r"Data\First.wz", rng.bytes(10_000));
    for name in &names[..2] {
        let mut ops = Vec::new();
        for _ in 0..parts {
            match rng.below(100) {
                0..40 => {
                    let from = match layout {
                        Layout::Table => names[rng.below(names.len())],
                        Layout::PerEntry => *name,
                    };
                    let size = case.before[from].len();
                    let len = 1 + rng.below(300);
                    ops.push(copy(from, rng.below(size - len + 1), len));
                }
                40..75 => {
                    let len = rng.below(200);
                    ops.push(Op::Literal(rng.bytes(len)));
                }
                _ => ops.push(Op::Fill(rng.next() as u8, rng.below(500) as u32)),
            }
        }
        case.rebuild(name, &ops);
    }
    case.create(r"Data\New\Last.wz", rng.bytes(3_000));
    case
}

#[test]
fn thousands_of_small_parts_keep_their_order() {
    for layout in LAYOUTS {
        let what = format!("{layout:?}");
        let case = many_small_parts(layout, 59, 6000);
        let opened = case.open();
        assert_eq!(opened.stage_counting(&what), opened.plan.new_bytes(), "{what}");
        case.assert_staged(&opened.dirs, true, &what);
        assert!(unfinished(&opened.dirs).is_empty(), "{what}");

        // Nothing still holds an original open once staging has returned: the apply step
        // is about to move those files.
        #[cfg(windows)]
        for name in case.before.keys() {
            use std::os::windows::fs::OpenOptionsExt;
            let alone = fs::OpenOptions::new().read(true).write(true).share_mode(0).open(on_disk(&case.install, name));
            assert!(alone.is_ok(), "{what}: {name} is still open after staging: {:?}", alone.err());
        }

        assert_eq!(opened.apply(), 4, "{what}");
        case.assert_install_patched(&what);
    }
}

#[test]
fn pausing_at_every_progress_report_keeps_only_whole_files_and_resumes() {
    // With copies fetched ahead, every report comes while later copies are still being
    // fetched. Pausing at each report in turn stops the run in every such state.
    for layout in LAYOUTS {
        let case = many_small_parts(layout, 61, 150);
        let opened = case.open();
        let total = opened.plan.new_bytes();
        let mut reports = 0u64;
        stage::stage(&opened.patch, &opened.plan, &opened.checked, &opened.dirs, &GO, &mut |_, _, _| reports += 1)
            .unwrap();
        assert!(reports > 200, "{layout:?}: only {reports} reports, the test would not see much");

        for pause_at in 1..=reports {
            let what = format!("{layout:?}, paused at report {pause_at} of {reports}");
            fs::remove_dir_all(opened.dirs.work()).unwrap();
            let stop = AtomicBool::new(false);
            let (mut seen, mut done) = (0u64, 0u64);
            let paused =
                stage::stage(&opened.patch, &opened.plan, &opened.checked, &opened.dirs, &stop, &mut |_, bytes, _| {
                    seen += 1;
                    done += bytes;
                    if seen == pause_at {
                        stop.store(true, Ordering::Relaxed);
                    }
                });
            assert!(done <= total, "{what}: progress passed its total");
            match &paused {
                Err(Error::Stopped) => {}
                // The pause came with the very last bytes: there was nothing left to stop.
                Ok(()) => assert_eq!(done, total, "{what}: finished without building everything"),
                Err(err) => panic!("{what}: {err}"),
            }
            // Whatever is among the staged files is whole; nothing half-written is left.
            case.assert_staged(&opened.dirs, paused.is_ok(), &what);
            assert!(unfinished(&opened.dirs).is_empty(), "{what}: {:?} left behind", unfinished(&opened.dirs));

            // The next run keeps what was finished, builds the rest, and reports each
            // byte once.
            assert_eq!(opened.stage_counting(&what), total, "{what}, continued");
            case.assert_staged(&opened.dirs, true, &format!("{what}, continued"));
        }
        case.assert_install_untouched(&format!("{layout:?}"));
        assert_eq!(opened.apply(), 4);
        case.assert_install_patched(&format!("{layout:?}"));
    }
}

/// A rebuild that takes many copies from one original, a few from another, then many more
/// from the first: when the second cannot be read, copies before and after it are in flight.
fn two_source_case() -> Case {
    let mut rng = Rng::new(67);
    let mut case =
        Case::new(Layout::Table, vec![(r"Data\A.wz", rng.bytes(200_000)), (r"Data\B.wz", rng.bytes(50_000))]);
    let mut ops = Vec::new();
    for i in 0..200 {
        ops.push(copy(r"Data\A.wz", i * 900, 700));
        ops.push(Op::Literal(rng.bytes(20)));
    }
    for i in 0..5 {
        ops.push(copy(r"Data\B.wz", i * 9000, 4000));
    }
    for i in 0..200 {
        ops.push(copy(r"Data\A.wz", i * 500, 1500));
        ops.push(Op::Fill(0, 30));
    }
    case.rebuild(r"Data\A.wz", &ops);
    case
}

#[cfg(windows)]
#[test]
fn original_that_cannot_be_opened_fails_the_run_while_copies_are_in_flight() {
    use std::os::windows::fs::OpenOptionsExt;

    let case = two_source_case();
    let opened = Arc::new(case.open());
    // Another program takes the second original for itself after it was checked.
    let held = fs::OpenOptions::new().read(true).share_mode(0).open(case.install.join("Data/B.wz")).unwrap();

    let staging = opened.clone();
    let started = std::time::Instant::now();
    let err = must_finish("held original", move || staging.stage()).unwrap_err();
    let Error::Io { what, source } = &err else { panic!("unexpected error: {err}") };
    assert!(what.starts_with("open ") && what.ends_with("B.wz"), "{err}");
    assert_eq!(source.raw_os_error(), Some(32), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10), "the failure took {:?} to arrive", started.elapsed());
    assert!(
        !opened.dirs.staged().join("Data/A.wz").exists(),
        "a file was staged from an original that could not be read"
    );
    assert!(unfinished(&opened.dirs).is_empty(), "{:?} left behind", unfinished(&opened.dirs));

    // Released, the same run goes through.
    drop(held);
    case.assert_install_untouched("held original");
    opened.stage().unwrap();
    case.assert_staged(&opened.dirs, true, "released original");
    assert_eq!(opened.apply(), 1);
    case.assert_install_patched("released original");
}

#[test]
fn original_that_disappeared_fails_the_run_while_copies_are_in_flight() {
    let case = two_source_case();
    let opened = Arc::new(case.open());
    let gone = fs::read(case.install.join("Data/B.wz")).unwrap();
    fs::remove_file(case.install.join("Data/B.wz")).unwrap();

    let staging = opened.clone();
    let err = must_finish("missing original", move || staging.stage()).unwrap_err();
    let Error::Io { what, source } = &err else { panic!("unexpected error: {err}") };
    assert!(what.starts_with("open ") && what.ends_with("B.wz"), "{err}");
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound, "{err}");
    assert!(files_under(&opened.dirs.staged()).is_empty(), "something was staged");
    assert!(unfinished(&opened.dirs).is_empty(), "{:?} left behind", unfinished(&opened.dirs));

    fs::write(case.install.join("Data/B.wz"), gone).unwrap();
    opened.stage().unwrap();
    case.assert_staged(&opened.dirs, true, "original put back");
}

#[test]
fn rebuilt_file_that_does_not_match_its_checksum_is_refused_with_copies_in_flight() {
    // The checksum is taken of the bytes as they are written, in order. A patch whose
    // instructions do not make the file it promises is caught before anything is staged.
    let mut case = two_source_case();
    let ops: Vec<Op> = (0..500).map(|i| copy(r"Data\B.wz", i * 90, 800)).collect();
    case.rebuild_claiming(r"Data\B.wz", &ops, b"not what the copies make");

    let opened = Arc::new(case.open());
    let staging = opened.clone();
    let err = must_finish("wrong checksum", move || staging.stage()).unwrap_err();
    assert!(matches!(&err, Error::NewFileMismatch(name) if name == r"Data\B.wz"), "unexpected error: {err}");
    // The first rebuild was right and is kept; the wrong one is nowhere.
    case.assert_staged(&opened.dirs, true, "wrong checksum");
    assert!(!opened.dirs.staged().join("Data/B.wz").exists());
    assert!(unfinished(&opened.dirs).is_empty(), "{:?} left behind", unfinished(&opened.dirs));
}

#[test]
fn files_a_killed_run_was_writing_are_removed_and_never_applied() {
    let case = many_small_parts(Layout::Table, 71, 400);
    let opened = case.open();

    // A run of this patch that was killed: the work folder is this patch's, one file was
    // finished, and files it was still writing lie around under every name they can have,
    // the single name older versions used included.
    let stop = AtomicBool::new(false);
    let mut names = Vec::new();
    let killed = stage::stage(&opened.patch, &opened.plan, &opened.checked, &opened.dirs, &stop, &mut |name, _, _| {
        if names.last().map(String::as_str) != Some(name) {
            names.push(name.to_owned());
        }
        stop.store(names.len() == 2, Ordering::Relaxed);
    });
    assert!(matches!(killed, Err(Error::Stopped)));
    let junk = vec![0x5Au8; 123_456];
    let planted =
        ["building.tmp", "building-0.tmp", "building-1.tmp", "building-2.tmp", "building-3.tmp", "building-9999.tmp"];
    for name in planted {
        fs::write(opened.dirs.work().join(name), &junk).unwrap();
    }
    // Things that only look similar are not the engine's to remove.
    fs::write(opened.dirs.work().join("building-notes.txt"), b"kept").unwrap();
    assert_eq!(unfinished(&opened.dirs).len(), planted.len() + 1);

    // The next run removes them before it measures the space it needs, builds its own
    // files, and applies only those.
    let mut left_at_space_check = None;
    let outcome = run(&case.options(), &GO, &mut |event| {
        if matches!(event, Event::Space { .. }) {
            left_at_space_check = Some(unfinished(&opened.dirs));
        }
    })
    .unwrap();
    assert_eq!(left_at_space_check, Some(vec!["building-notes.txt".to_owned()]));
    assert_eq!((outcome.files_changed, outcome.immediate), (4, false));
    case.assert_install_patched("after a killed run");
    for (name, contents) in files_under(&case.install) {
        assert!(contents != junk, "{name} is one of the half-written files");
    }
}

#[test]
fn staging_removes_files_a_killed_run_was_writing() {
    // The same through the staging step alone, which a caller may use without `run`.
    let case = many_small_parts(Layout::PerEntry, 73, 200);
    let opened = case.open();
    fs::create_dir_all(opened.dirs.work()).unwrap();
    fs::write(opened.dirs.work().join("patch.id"), opened.patch.id()).unwrap();
    for name in ["building.tmp", "building-1.tmp", "building-77.tmp"] {
        fs::write(opened.dirs.work().join(name), b"half a file").unwrap();
    }

    opened.stage().unwrap();
    assert!(unfinished(&opened.dirs).is_empty(), "{:?} left behind", unfinished(&opened.dirs));
    case.assert_staged(&opened.dirs, true, "staging after a killed run");
    assert_eq!(opened.apply(), 4);
    case.assert_install_patched("staging after a killed run");
}

#[test]
fn staged_file_that_was_damaged_is_built_again_on_resume() {
    for layout in LAYOUTS {
        let what = format!("{layout:?}");
        let case = many_small_parts(layout, 79, 500);
        let opened = case.open();
        opened.stage().unwrap();
        let staged = |name: &str| on_disk(&opened.dirs.staged(), name);

        // While the run was paused: one rebuilt file has a flipped bit, one is cut short,
        // one whole file is other bytes of the same length. One file is left alone.
        let mut flipped = fs::read(staged(r"Data\A.wz")).unwrap();
        let middle = flipped.len() / 2;
        flipped[middle] ^= 0x10;
        fs::write(staged(r"Data\A.wz"), &flipped).unwrap();
        let short = fs::read(staged(r"Data\Sub\B.wz")).unwrap();
        fs::write(staged(r"Data\Sub\B.wz"), &short[..short.len() - 1]).unwrap();
        fs::write(staged(r"Data\First.wz"), vec![0u8; 10_000]).unwrap();
        let untouched = fs::metadata(staged(r"Data\New\Last.wz")).unwrap().modified().unwrap();

        // Every byte is reported once: the file that was fine when it was checked, the
        // others as they are built again.
        assert_eq!(opened.stage_counting(&what), opened.plan.new_bytes(), "{what}");
        case.assert_staged(&opened.dirs, true, &what);
        assert_eq!(
            fs::metadata(staged(r"Data\New\Last.wz")).unwrap().modified().unwrap(),
            untouched,
            "{what}: a file that was fine was written again"
        );
        assert!(unfinished(&opened.dirs).is_empty(), "{what}");

        // And once more with nothing damaged: nothing is built, everything is reported.
        let before: Vec<_> =
            case.after.keys().map(|name| fs::metadata(staged(name)).unwrap().modified().unwrap()).collect();
        assert_eq!(opened.stage_counting(&what), opened.plan.new_bytes(), "{what}, third time");
        let after: Vec<_> =
            case.after.keys().map(|name| fs::metadata(staged(name)).unwrap().modified().unwrap()).collect();
        assert_eq!(before, after, "{what}: staged files were written again without need");

        assert_eq!(opened.apply(), 4, "{what}");
        case.assert_install_patched(&what);
    }
}

#[test]
fn pause_while_staged_files_are_checked_again_stops_the_run() {
    let case = many_small_parts(Layout::Table, 83, 300);
    let opened = case.open();
    opened.stage().unwrap();

    // Everything is staged; the second run only checks. A pause that is already asked for
    // ends it before anything is reported, and nothing is lost.
    let stop = AtomicBool::new(true);
    let mut reports = 0;
    let paused =
        stage::stage(&opened.patch, &opened.plan, &opened.checked, &opened.dirs, &stop, &mut |_, _, _| reports += 1);
    assert!(matches!(paused, Err(Error::Stopped)), "{paused:?}");
    assert_eq!(reports, 0);
    case.assert_staged(&opened.dirs, true, "paused while checking");
    assert_eq!(opened.stage_counting("after the pause"), opened.plan.new_bytes());
}

#[test]
fn drive_read_the_slow_way_gives_exactly_the_files_the_fast_way_gives() {
    // One file checked at a time, two readers, originals read through the file cache: how
    // a hard disk is read. What is found and what is built must not depend on it.
    let slow = Profile::slow();
    assert_ne!(slow, Profile::fast());
    for layout in LAYOUTS {
        let what = format!("{layout:?}");
        let case = many_small_parts(layout, 89, 800);
        let opened = case.open();

        let checked =
            stage::check_originals_with(&slow, &opened.plan, &case.install, &BTreeSet::new(), &GO, &|_, _| {})
                .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(checked.up_to_date, opened.checked.up_to_date, "{what}");
        let mut done = 0u64;
        stage::stage_with(&slow, &opened.patch, &opened.plan, &checked, &opened.dirs, &GO, &mut |_, bytes, _| {
            done += bytes
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(done, opened.plan.new_bytes(), "{what}");
        case.assert_staged(&opened.dirs, true, &what);
        assert!(unfinished(&opened.dirs).is_empty(), "{what}");
        let the_slow_way = files_under(&opened.dirs.staged());

        // Staged again the slow way, the files are found whole and kept.
        let mut kept = 0u64;
        stage::stage_with(
            &slow,
            &opened.patch,
            &opened.plan,
            &checked,
            &opened.dirs,
            &GO,
            &mut |_, bytes, was_kept| {
                kept += if was_kept { bytes } else { 0 };
            },
        )
        .unwrap();
        assert_eq!(kept, opened.plan.new_bytes(), "{what}");

        // Built from nothing the fast way, they are the same files byte for byte.
        fs::remove_dir_all(opened.dirs.work()).unwrap();
        opened.stage().unwrap();
        assert!(files_under(&opened.dirs.staged()) == the_slow_way, "{what}: the two ways built different files");
        assert_eq!(opened.apply(), 4, "{what}");
        case.assert_install_patched(&what);
    }
}

#[test]
fn run_says_how_it_reads_the_drive_and_follows_the_choice_made_for_it() {
    // The files of a made-up install are far too small to time a drive with, so left to
    // itself the engine reads the fast way.
    for (choice, expected) in
        [(Choice::Auto, Profile::fast()), (Choice::Fast, Profile::fast()), (Choice::Slow, Profile::slow())]
    {
        let what = format!("{choice:?}");
        let case = many_small_parts(Layout::Table, 97, 300);
        case.open();
        let options = Options { storage: choice, ..case.options() };

        let mut stages = 0;
        let mut told = Vec::new();
        let outcome = run(&options, &GO, &mut |event| match event {
            Event::Stage(_) => stages += 1,
            Event::Storage { file_system, seek_ms, slow, readers, checkers, unbuffered } => {
                told.push((file_system, seek_ms, slow, Profile { readers, checkers, unbuffered }, stages));
            }
            _ => {}
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!((outcome.files_changed, outcome.immediate), (4, false), "{what}");
        case.assert_install_patched(&what);

        // Said once, after the patch file was read and before the game files are.
        let [(file_system, seek_ms, slow, profile, stages_before)] = &told[..] else { panic!("{what}: {told:?}") };
        assert_eq!((*seek_ms, *slow), (None, false), "{what}");
        assert_eq!(*profile, expected, "{what}");
        assert_eq!(*stages_before, 1, "{what}");
        assert!(cfg!(not(windows)) || !file_system.is_empty(), "{what}: the file system has no name");
    }
}
