//! Builds made-up installs and patch files for the tests, with the expected result worked
//! out separately from the engine.
#![allow(dead_code)]

mod builder;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Layout};
use mfp_engine::preflight::{LOCK_FILE_NAME, Programs, SPACE_MARGIN, WORK_DIR_NAME};
use mfp_engine::{Error, Event, Options, Outcome, Source, immediate, run_for, stage};

/// Never set: a run that is not asked to pause.
pub static GO: AtomicBool = AtomicBool::new(false);

/// The two layouts a patch file comes in.
pub const LAYOUTS: [Layout; 2] = [Layout::Table, Layout::PerEntry];

/// The programs of the two clients that can be installed side by side. The tests are the
/// caller here, and the caller is who knows the games by name. The made-up installs are the
/// live client's unless a test says otherwise.
pub const LIVE: Programs = Programs { game: "MapleStory.exe", launcher: "GameLauncher.exe" };
pub const TEST: Programs = Programs { game: "MapleStoryT.exe", launcher: "GameLauncherT.exe" };

/// Runs a patch on the made-up install of the live client, which is the one nearly every
/// test has.
pub fn run(options: &Options, stop: &AtomicBool, on_event: &mut (dyn FnMut(Event) + Send)) -> Result<Outcome, Error> {
    run_for(&LIVE, options, stop, on_event)
}

/// A run of the patch file `patch` on `install`: the safe way, the drive read as the engine
/// sees fit.
pub fn options(install: &Path, patch: &Path) -> Options {
    Options {
        install_dir: install.to_path_buf(),
        source: Source::File(patch.to_path_buf()),
        immediate: false,
        storage: Default::default(),
    }
}

/// Where the file that a patch calls `name` is under `root`.
pub fn on_disk(root: &Path, name: &str) -> PathBuf {
    root.join(name.replace('\\', "/"))
}

/// Every file below `dir`, by its name relative to `dir` with `\` separators.
pub fn files_under(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        let Ok(children) = fs::read_dir(&next) else { continue };
        for child in children.flatten() {
            let path = child.path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let name = path.strip_prefix(dir).unwrap().to_string_lossy().replace('/', "\\");
                found.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    found
}

// Not every test uses every one of these.
#[allow(unused_imports)]
pub use builder::{Op, PatchBuilder, Rng, copy};

/// A made-up install, a patch for it, and what the install must look like afterwards.
pub struct World {
    _temp: tempfile::TempDir,
    pub install: PathBuf,
    pub patch: PathBuf,
    /// Every file of the install before the patch.
    pub before: BTreeMap<String, Vec<u8>>,
    /// Every file the install must hold after the patch. Files missing here must be gone.
    pub after: BTreeMap<String, Vec<u8>>,
    /// Folders the patch creates.
    pub new_dirs: Vec<String>,
}

impl World {
    /// Makes an install of a handful of files and a patch that rebuilds, replaces, creates
    /// and deletes among them, all chosen from `seed`.
    pub fn random(seed: u64, layout: Layout) -> World {
        World::random_for(&LIVE, seed, layout)
    }

    /// [`World::random`] with the install folder of the client whose programs are `programs`.
    pub fn random_for(programs: &Programs, seed: u64, layout: Layout) -> World {
        let mut rng = Rng::new(seed);
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Maple");

        let mut before = BTreeMap::new();
        before.insert(programs.game.to_owned(), rng.bytes(4096));
        let count = 4 + rng.below(5);
        for i in 0..count {
            // Mostly small files, with a few that span several of the engine's 1 MiB buffers.
            let len = if rng.chance(25) { (1 << 20) + rng.below(2 << 20) } else { rng.below(200_000) };
            before.insert(format!("Data\\Dir{}\\File_{i:03}.wz", rng.below(3)), rng.bytes(len));
        }
        for (name, contents) in &before {
            let path = on_disk(&install, name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        let names: Vec<String> = before.keys().filter(|name| name.starts_with("Data")).cloned().collect();
        let mut builder = PatchBuilder::new(layout);
        let mut after = before.clone();
        let mut new_dirs = Vec::new();
        let mut taken = std::collections::BTreeSet::new();

        // Rebuild some files out of pieces of the originals, filler, and new data.
        for name in &names {
            if !rng.chance(60) {
                continue;
            }
            taken.insert(name.clone());
            let old = &before[name];
            let (mut ops, mut new) = (Vec::new(), Vec::new());
            for _ in 0..1 + rng.below(12) {
                match rng.below(3) {
                    0 => {
                        let from = match layout {
                            Layout::Table => names[rng.below(names.len())].clone(),
                            Layout::PerEntry => name.clone(),
                        };
                        let source = &before[&from];
                        if source.is_empty() {
                            continue;
                        }
                        let start = rng.below(source.len());
                        let len = 1 + rng.below(source.len() - start);
                        new.extend(&source[start..start + len]);
                        builder.original(&from, source);
                        ops.push(Op::Copy(from, start as u32, len as u32));
                    }
                    1 => {
                        let len = if rng.chance(15) { 1 + rng.below(0xF_FFFF) } else { 1 + rng.below(5000) };
                        let byte = rng.next() as u8;
                        new.resize(new.len() + len, byte);
                        ops.push(Op::Fill(byte, len as u32));
                    }
                    _ => {
                        let len = if rng.chance(10) { 1 + rng.below(1_500_000) } else { 1 + rng.below(40_000) };
                        let data = rng.bytes(len);
                        new.extend(&data);
                        ops.push(Op::Literal(data));
                    }
                }
            }
            builder.original(name, old);
            builder.rebuild(name, old, &new, &ops);
            after.insert(name.clone(), new);
        }

        // Replace one file whole, delete one, and add new ones in old and new folders.
        let free: Vec<&String> = names.iter().filter(|name| !taken.contains(*name)).collect();
        if let Some(name) = free.first() {
            let len = rng.below(300_000);
            let contents = rng.bytes(len);
            builder.create(name, &contents);
            if !contents.is_empty() {
                after.insert((*name).clone(), contents);
            }
        }
        if let Some(name) = free.get(1) {
            builder.delete(name);
            after.remove(*name);
        }
        builder.create_dir("Data\\Fresh\\Deep");
        new_dirs.push("Data\\Fresh\\Deep".to_owned());
        for i in 0..1 + rng.below(3) {
            let name = format!("Data\\{}\\Added_{i:03}.wz", if rng.chance(50) { "Fresh" } else { "Dir0" });
            let len = 1 + rng.below(100_000);
            let contents = rng.bytes(len);
            builder.create(&name, &contents);
            after.insert(name, contents);
        }

        let patch = temp.path().join("made-up.patch");
        builder.write(&patch);
        World { _temp: temp, install, patch, before, after, new_dirs }
    }

    /// A run of this patch on this install, the safe way.
    pub fn options(&self) -> Options {
        options(&self.install, &self.patch)
    }

    /// Fails unless the install holds exactly the files in `expected`, byte for byte, and
    /// nothing of the patch run is left lying around.
    pub fn assert_holds(&self, expected: &BTreeMap<String, Vec<u8>>, what: &str) {
        for (name, contents) in expected {
            let found = fs::read(on_disk(&self.install, name)).unwrap_or_else(|e| panic!("{what}: {name}: {e}"));
            assert!(found == *contents, "{what}: {name} has the wrong contents ({} bytes)", found.len());
        }
        for name in self.before.keys().chain(self.after.keys()) {
            if !expected.contains_key(name) {
                assert!(!on_disk(&self.install, name).exists(), "{what}: {name} should not exist");
            }
        }
        assert!(!self.install.join(LOCK_FILE_NAME).exists(), "{what}: lock file left behind");
    }

    pub fn assert_patched(&self, what: &str) {
        self.assert_holds(&self.after, what);
        for dir in &self.new_dirs {
            assert!(on_disk(&self.install, dir).is_dir(), "{what}: folder {dir} was not created");
        }
        assert!(!self.install.join(WORK_DIR_NAME).exists(), "{what}: work folder left behind");
    }

    pub fn assert_untouched(&self, what: &str) {
        self.assert_holds(&self.before, what);
        for dir in &self.new_dirs {
            let top = dir.split('\\').take(2).collect::<Vec<_>>().join("\\");
            assert!(!on_disk(&self.install, &top).exists(), "{what}: folder {top} was left behind");
        }
    }

    /// The files the patch leaves new in the install folder: those that are there after it
    /// and are not what they were before.
    pub fn new_files(&self) -> BTreeSet<String> {
        let new =
            |(name, contents): (&String, &Vec<u8>)| (self.before.get(name) != Some(contents)).then(|| name.clone());
        self.after.iter().filter_map(new).collect()
    }

    /// Fails unless `placed`, the files that the runs of this patch named as placed, however
    /// many runs it took, are the new files of the install folder, each of them named once.
    pub fn assert_named_once(&self, placed: &[String], what: &str) {
        let named: BTreeSet<String> = placed.iter().cloned().collect();
        assert_eq!(named.len(), placed.len(), "{what}: a file was named as placed more than once: {placed:?}");
        assert_eq!(named, self.new_files(), "{what}: the files named as placed are not the new files of the folder");
    }
}

/// Adds the name of a file to `placed` when `event` says that it has taken its place.
pub fn note_placed(placed: &mut Vec<String>, event: Event) {
    if let Event::Placed { name } = event {
        placed.push(name);
    }
}

/// Bytes of all files under `dir`.
pub fn tree_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        let Ok(children) = fs::read_dir(&next) else { continue };
        for child in children.flatten() {
            // Asked of the file itself: the size a folder lists for a file that is being
            // written lags behind. A file that is gone by now was just moved.
            match fs::metadata(child.path()) {
                Ok(meta) if meta.is_dir() => todo.push(child.path()),
                Ok(meta) => total += meta.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// What a made-up drive has beyond the least the immediate way needs and the margin: room
/// for the few small files a run keeps in its work folder (which patch it is, the marker,
/// the journal). Far less than any file of the installs used here.
pub const ALLOWANCE: u64 = 16 << 10;

/// A made-up drive that holds one install folder and nothing else. What is free on it is
/// its size less everything in that folder, work folder included.
pub struct Drive {
    pub size: u64,
    pub install: PathBuf,
}

impl Drive {
    /// A drive on which `install` has `room` bytes left, the margin and the allowance on top.
    pub fn with_room(install: &Path, room: u64) -> Drive {
        Drive { size: tree_size(install) + room + SPACE_MARGIN + ALLOWANCE, install: install.to_path_buf() }
    }

    /// A drive with exactly the room the immediate way needs to put `patch` on `install`.
    pub fn tight(install: &Path, patch: &Path) -> Drive {
        Drive::with_room(install, least_room(install, patch))
    }

    pub fn free(&self) -> u64 {
        self.size.saturating_sub(tree_size(&self.install))
    }

    /// The most the install folder may hold if the margin is to stay free.
    pub fn limit(&self) -> u64 {
        self.size - SPACE_MARGIN
    }
}

/// The least room the immediate way needs to patch `install` as it is, nothing built yet.
pub fn least_room(install: &Path, patch: &Path) -> u64 {
    let patch = PatchFile::open(patch).unwrap();
    let plan = plan::scan(&patch, &GO).unwrap();
    let checked = stage::check_originals(&plan, install, &GO, &|_, _| {}).unwrap();
    immediate::space_needed(&plan, &checked, &BTreeSet::new(), install)
}
