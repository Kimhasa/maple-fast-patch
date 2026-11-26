//! A made-up world to try the app against: a small install of one client at an old version,
//! and patch files that take it forward, laid out as Nexon publishes them.
//!
//! Nothing of it is the real game. The install holds a handful of files of made-up bytes,
//! and in the place of the game's programs a stand-in that only writes down that it was
//! started. The patch files are real ones all the same: the engine reads and applies them
//! like any other, they have tables of originals, copies from one file into another, whole
//! files, deletes and folders, and among the minor patches there is one whose table of
//! originals is empty, which exists for real and is the kind that fits any folder.
//!
//! A world is one folder:
//!
//! - `install\` is the client's install folder.
//! - `served\` is what the made-up server hands out, under the names Nexon's server uses.
//! - `not-yet\` holds the patch files that are not published yet, under the same names.
//! - `expected\` says for every version what the install must hold when it is there.
//! - `world.txt` lists it all, and three files beside it tell of maintenance (see
//!   [`crate::fake_notice`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mfp_engine::crc;
use mfp_engine::plan::Layout;
use mfp_kms::notice::Minutes;
use mfp_kms::{Client, Server};

use crate::builder::{Op, PatchBuilder, Rng, copy};
use crate::clients::under_nexon;
use crate::fake_notice;

pub const INSTALL_DIR: &str = "install";
pub const SERVED_DIR: &str = "served";
pub const HELD_DIR: &str = "not-yet";
pub const EXPECTED_DIR: &str = "expected";
const WORLD_FILE: &str = "world.txt";

/// The files of an install by the names a patch gives them, with what they hold.
type Files = BTreeMap<String, Vec<u8>>;

/// The data files a made-up install begins with, and how its bytes are shared out among
/// them, in thirty-seconds.
const DATA: [(&str, usize); 7] = [
    (r"Data\Base\Base_000.wz", 2),
    (r"Data\Character\Character_000.wz", 6),
    (r"Data\Map\Map_000.wz", 8),
    (r"Data\Skill\Skill_000.wz", 5),
    (r"Data\Sound\Sound_000.wz", 7),
    (r"Data\String\String_000.wz", 1),
    (r"Data\UI\UI_000.wz", 3),
];

/// The large files, which the patches rebuild in turn.
const REBUILT: [&str; 4] = [
    r"Data\Character\Character_000.wz",
    r"Data\Map\Map_000.wz",
    r"Data\Skill\Skill_000.wz",
    r"Data\Sound\Sound_000.wz",
];

/// A version of a client: its number as the patch files count it, and its minor version,
/// which is 1 for a version that was just released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub number: u32,
    pub minor: u32,
}

impl Version {
    /// The version as Nexon writes it, with the minor version behind it once there is one.
    pub fn label(&self, client: &Client) -> String {
        let label = client.version_label(self.number);
        if self.minor > 1 { format!("{label} ({})", self.minor) } else { label }
    }

    /// The name of the file in `expected\` that says what an install at this version holds.
    fn expected_file(&self) -> String {
        format!("{:05}-{:02}.txt", self.number, self.minor)
    }
}

/// One made-up patch file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// Where it lies under the root of the patch server, with `/` between the parts:
    /// `Patch/00901/00900to00901.patch`.
    pub path: String,
    pub from: Version,
    pub to: Version,
    pub bytes: u64,
    /// How many files its table of originals lists. With none, nothing in the patch file
    /// says which install it is for.
    pub originals: usize,
}

/// What [`make`] made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct World {
    pub server: Server,
    /// The version the install was made at.
    pub installed: Version,
    /// Every version the patches lead to, the installed one first, each with the mark of
    /// what the install holds there.
    pub states: Vec<(Version, String)>,
    pub patches: Vec<Patch>,
}

/// What kind of world to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub server: Server,
    /// How many main versions follow the installed one.
    pub versions: u32,
    /// How many minor patches follow on the newest version.
    pub minors: u32,
    /// The size of the install's data.
    pub megabytes: u32,
    /// The number of the installed version, as the patch files count it. `None` takes a
    /// number that no real version has had.
    pub from: Option<u32>,
    /// What the made-up bytes are drawn from. `None` takes one per client, so that the
    /// worlds of the two clients differ.
    pub seed: Option<u64>,
}

impl Options {
    /// A small world of the client of `server`: two main versions and two minor patches.
    pub fn new(server: Server) -> Options {
        Options { server, versions: 2, minors: 2, megabytes: 256, from: None, seed: None }
    }
}

/// Says what went wrong with a file of the world, by its name inside the world.
fn failed<'a, E: std::fmt::Display>(what: &'a str, name: &'a str) -> impl Fn(E) -> String + 'a {
    move |err| format!("{what} {name}: {err}")
}

/// Makes a world in `dir`, which must not exist yet or be empty. `launcher` is the stand-in
/// that is put in the place of the client's programs.
pub fn make(dir: &Path, options: &Options, launcher: &Path, now: Minutes) -> Result<World, String> {
    if under_nexon(dir) {
        return Err("refusing to make a world under a `Nexon` folder: that is where the real games live".into());
    }
    if fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err("the folder for the world must not exist yet or be empty".into());
    }
    if options.megabytes == 0 || options.megabytes > 1024 {
        return Err("the size has to be between 1 and 1024 MB".into());
    }
    let client = options.server.client();
    let mut rng = Rng::new(options.seed.unwrap_or(match options.server {
        Server::Live => 1,
        Server::Test => 2,
    }));
    // Numbers no real version has had. The test world's are a thousand ahead of its names.
    let first = options.from.unwrap_or(match options.server {
        Server::Live => 900,
        Server::Test => 1900,
    });

    // The install, at the old version.
    let part = options.megabytes as usize * (1 << 20) / 32;
    let mut files: Files = DATA.iter().map(|(name, parts)| ((*name).to_owned(), rng.bytes(part * parts))).collect();
    let install = dir.join(INSTALL_DIR);
    write_files(&install, &files)?;
    for program in [client.game_program, client.launcher] {
        fs::copy(launcher, install.join(program)).map_err(failed("copy the stand-in to", program))?;
    }

    let installed = Version { number: first, minor: 1 };
    let mut world = World { server: options.server, installed, states: Vec::new(), patches: Vec::new() };
    world.expect(dir, installed, &files)?;

    // The main versions, one patch each.
    let mut at = installed;
    for number in first + 1..=first + options.versions {
        let (builder, next) = main_step(&files, number, &mut rng);
        let to = Version { number, minor: 1 };
        world.hold(dir, client.patch_url(at.number, number), at, to, &builder)?;
        world.expect(dir, to, &next)?;
        (files, at) = (next, to);
    }

    // The minor patches on the newest version: one for every step, and one for every jump.
    let mut minors = vec![files];
    for minor in 2..=options.minors + 1 {
        let old = minors.last().expect("the newest version itself");
        let (builder, next) = if minor == 2 { whole_step(old, &mut rng) } else { minor_step(old, minor, &mut rng) };
        let to = Version { number: at.number, minor };
        let from = Version { number: at.number, minor: minor - 1 };
        world.hold(dir, client.minor_patch_url(at.number, minor - 1, minor), from, to, &builder)?;
        world.expect(dir, to, &next)?;
        minors.push(next);
    }
    for from in 1..=minors.len() {
        for to in from + 2..=minors.len() {
            let builder = jump(&minors[from - 1], &minors[to - 1]);
            let (from, to) = (from as u32, to as u32);
            let versions = (Version { number: at.number, minor: from }, Version { number: at.number, minor: to });
            world.hold(dir, client.minor_patch_url(at.number, from, to), versions.0, versions.1, &builder)?;
        }
    }

    fs::create_dir_all(dir.join(SERVED_DIR)).map_err(failed("create", SERVED_DIR))?;
    world.write(dir)?;
    fake_notice::write_files(dir, now)?;
    Ok(world)
}

/// Writes `files` under `root`, each by the name a patch gives it.
fn write_files(root: &Path, files: &Files) -> Result<(), String> {
    for (name, contents) in files {
        let path = root.join(name.replace('\\', "/"));
        let parent = path.parent().expect("a file inside the install");
        fs::create_dir_all(parent).map_err(failed("create the folder of", name))?;
        fs::write(&path, contents).map_err(failed("write", name))?;
    }
    Ok(())
}

/// What `ops` build out of the files in `old`.
fn built(old: &Files, ops: &[Op]) -> Vec<u8> {
    let mut new = Vec::new();
    for op in ops {
        match op {
            Op::Literal(data) => new.extend(data),
            Op::Fill(byte, len) => new.resize(new.len() + *len as usize, *byte),
            Op::Copy(from, start, len) => new.extend(&old[from][*start as usize..(*start + *len) as usize]),
        }
    }
    new
}

/// Adds to `builder` an entry that rebuilds `name` out of its old self, a piece of
/// `partner`, a filler and new bytes, and returns the new file.
///
/// The new file begins with the beginning of the old one. Versions several steps apart then
/// still begin alike, and a patch that jumps from one to the other has something to copy.
fn rebuild(builder: &mut PatchBuilder, old: &Files, name: &str, partner: &str, rng: &mut Rng) -> Vec<u8> {
    let (own, other) = (&old[name], &old[partner]);
    let (head, tail) = (own.len() * 2 / 5, own.len() * 9 / 20);
    let lent = (other.len() / 10).max(1);
    let ops = [
        copy(name, 0, head),
        Op::Literal(rng.bytes((own.len() / 20).max(64))),
        copy(partner, rng.below(other.len() - lent + 1), lent),
        Op::Fill(rng.next() as u8, 1 + rng.below(4000) as u32),
        copy(name, own.len() - tail, tail),
    ];
    let new = built(old, &ops);
    builder.original(name, own).original(partner, other).rebuild(name, own, &new, &ops);
    new
}

/// The patch to main version `number`: three of the large files rebuilt out of themselves
/// and one another, one file replaced whole, one new file in the place of the one the
/// version before added, and a new folder.
fn main_step(old: &Files, number: u32, rng: &mut Rng) -> (PatchBuilder, Files) {
    let mut builder = PatchBuilder::new(Layout::Table);
    let mut new = old.clone();
    let left_out = number as usize % REBUILT.len();
    for (index, name) in REBUILT.iter().enumerate().filter(|(index, _)| *index != left_out) {
        let partner = REBUILT[(index + 1) % REBUILT.len()];
        new.insert((*name).to_owned(), rebuild(&mut builder, old, name, partner, rng));
    }
    let base = DATA[0].0;
    let whole = rng.bytes(old[base].len());
    builder.create(base, &whole);
    new.insert(base.to_owned(), whole);

    let added = |number: u32| format!(r"Data\Etc\Etc_{number:05}.wz");
    if new.remove(&added(number - 1)).is_some() {
        builder.delete(&added(number - 1));
    }
    let fresh = rng.bytes(old[base].len() / 2);
    builder.create(&added(number), &fresh);
    new.insert(added(number), fresh);
    builder.create_dir(&format!(r"Data\Event{number:05}"));
    (builder, new)
}

/// A minor patch that only holds whole files, and so has an empty table of originals.
fn whole_step(old: &Files, rng: &mut Rng) -> (PatchBuilder, Files) {
    let mut builder = PatchBuilder::new(Layout::Table);
    let mut new = old.clone();
    for (name, _) in &DATA[5..] {
        let whole = rng.bytes(old[*name].len());
        builder.create(name, &whole);
        new.insert((*name).to_owned(), whole);
    }
    (builder, new)
}

/// The patch to minor version `minor`: two of the large files rebuilt.
fn minor_step(old: &Files, minor: u32, rng: &mut Rng) -> (PatchBuilder, Files) {
    let mut builder = PatchBuilder::new(Layout::Table);
    let mut new = old.clone();
    for index in [minor as usize % REBUILT.len(), (minor as usize + 2) % REBUILT.len()] {
        let (name, partner) = (REBUILT[index], REBUILT[(index + 1) % REBUILT.len()]);
        new.insert(name.to_owned(), rebuild(&mut builder, old, name, partner, rng));
    }
    (builder, new)
}

/// A patch that takes an install from `from` to `to`, whatever lies between them. A file
/// that still begins as it did is rebuilt from that beginning; any other comes whole.
fn jump(from: &Files, to: &Files) -> PatchBuilder {
    let mut builder = PatchBuilder::new(Layout::Table);
    for (name, new) in to {
        let old = from.get(name);
        if old == Some(new) {
            continue;
        }
        let shared = old.map_or(0, |old| old.iter().zip(new).take_while(|(a, b)| a == b).count());
        match old {
            Some(old) if shared >= 16 => {
                let mut ops = vec![copy(name, 0, shared)];
                if shared < new.len() {
                    ops.push(Op::Literal(new[shared..].to_vec()));
                }
                builder.original(name, old).rebuild(name, old, new, &ops);
            }
            _ => {
                builder.create(name, new);
            }
        }
    }
    for name in from.keys().filter(|name| !to.contains_key(*name)) {
        builder.delete(name);
    }
    builder
}

/// The path of a patch file under the root of Nexon's patch server: the address `mfp-kms`
/// gives for it, without the server.
fn published_path(url: &str) -> String {
    url.splitn(4, '/').nth(3).unwrap_or(url).to_owned()
}

/// What an install holds, one line for each data file: checksum, length and name. Two
/// installs hold the same files when these texts are the same.
fn manifest(files: impl Iterator<Item = (String, u32, u64)>) -> String {
    let mut lines: Vec<String> = files.map(|(name, crc, len)| format!("{crc:08x} {len:>11} {name}\n")).collect();
    lines.sort_by(|a, b| a[21..].cmp(&b[21..]));
    lines.concat()
}

/// A short mark of a [`manifest`].
fn mark(manifest: &str) -> String {
    format!("{:08x}", crc::update(0, manifest.as_bytes()))
}

/// The manifest of the data files in the install folder `install`.
pub fn manifest_of(install: &Path) -> String {
    let mut found = Vec::new();
    let mut todo = vec![install.join("Data")];
    while let Some(next) = todo.pop() {
        let Ok(children) = fs::read_dir(&next) else { continue };
        for child in children.flatten() {
            let path = child.path();
            if path.is_dir() {
                todo.push(path);
            } else if let (Ok(relative), Ok(contents)) = (path.strip_prefix(install), fs::read(&path)) {
                let name = relative.to_string_lossy().replace('/', "\\");
                found.push((name, crc::update(0, &contents), contents.len() as u64));
            }
        }
    }
    manifest(found.into_iter())
}

impl World {
    pub fn client(&self) -> &'static Client {
        self.server.client()
    }

    /// The newest version the patches lead to.
    pub fn newest(&self) -> Version {
        self.states.last().map_or(self.installed, |(version, _)| *version)
    }

    /// The patches that go one step, in the order an install takes them: every main
    /// version, then every minor version of the newest.
    pub fn steps(&self) -> Vec<&Patch> {
        let step = |patch: &&Patch| {
            (patch.from.number + 1, 1) == (patch.to.number, patch.to.minor)
                || (patch.from.number, patch.from.minor + 1) == (patch.to.number, patch.to.minor)
        };
        self.patches.iter().filter(step).collect()
    }

    /// The version whose files the install folder `install` holds, if it is one of this
    /// world's.
    pub fn version_of(&self, install: &Path) -> Option<Version> {
        let found = mark(&manifest_of(install));
        self.states.iter().find(|(_, mark)| *mark == found).map(|(version, _)| *version)
    }

    /// Notes what an install at `version` holds, in `expected\` and in the list of states.
    fn expect(&mut self, dir: &Path, version: Version, files: &Files) -> Result<(), String> {
        let text =
            manifest(files.iter().map(|(name, bytes)| (name.clone(), crc::update(0, bytes), bytes.len() as u64)));
        let expected = dir.join(EXPECTED_DIR);
        fs::create_dir_all(&expected).map_err(failed("create", EXPECTED_DIR))?;
        fs::write(expected.join(version.expected_file()), &text).map_err(failed("write", EXPECTED_DIR))?;
        self.states.push((version, mark(&text)));
        Ok(())
    }

    /// Writes a patch file among those that are not published yet, and lists it.
    fn hold(
        &mut self,
        dir: &Path,
        url: String,
        from: Version,
        to: Version,
        builder: &PatchBuilder,
    ) -> Result<(), String> {
        let path = published_path(&url);
        let file = dir.join(HELD_DIR).join(&path);
        fs::create_dir_all(file.parent().expect("a patch file in a folder"))
            .map_err(failed("create the folder of", &path))?;
        let bytes = builder.bytes();
        fs::write(&file, &bytes).map_err(failed("write", &path))?;
        let originals = builder.originals();
        self.patches.push(Patch { path, from, to, bytes: bytes.len() as u64, originals });
        Ok(())
    }

    /// Writes `world.txt`.
    fn write(&self, dir: &Path) -> Result<(), String> {
        let mut text = format!("server {}\n", self.server.key());
        for (version, mark) in &self.states {
            text.push_str(&format!("state {} {} {mark}\n", version.number, version.minor));
        }
        for Patch { path, from, to, bytes, originals } in &self.patches {
            let versions = format!("{} {} {} {}", from.number, from.minor, to.number, to.minor);
            text.push_str(&format!("patch {versions} {bytes} {originals} {path}\n"));
        }
        fs::write(dir.join(WORLD_FILE), text).map_err(failed("write", WORLD_FILE))
    }

    /// Reads the world that [`make`] made in `dir`.
    pub fn read(dir: &Path) -> Result<World, String> {
        let text = fs::read_to_string(dir.join(WORLD_FILE))
            .map_err(|_| format!("no {WORLD_FILE} there: not a folder that `fake_nexon make` made"))?;
        let damaged = || format!("{WORLD_FILE} is not as it was written");
        let (mut server, mut states, mut patches) = (None, Vec::new(), Vec::new());
        for line in text.lines() {
            let words: Vec<&str> = line.split(' ').collect();
            let number = |at: usize| words.get(at).and_then(|word| word.parse::<u64>().ok()).ok_or_else(damaged);
            let version =
                |at: usize| Ok::<_, String>(Version { number: number(at)? as u32, minor: number(at + 1)? as u32 });
            let last = |at: usize| words.get(at).map(|word| (*word).to_owned()).ok_or_else(damaged);
            match words[0] {
                "server" => server = words.get(1).and_then(|key| Server::from_key(key)),
                "state" => states.push((version(1)?, last(3)?)),
                "patch" => patches.push(Patch {
                    from: version(1)?,
                    to: version(3)?,
                    bytes: number(5)?,
                    originals: number(6)? as usize,
                    path: last(7)?,
                }),
                _ => return Err(damaged()),
            }
        }
        let installed = states.first().map(|(version, _)| *version).ok_or_else(damaged)?;
        Ok(World { server: server.ok_or_else(damaged)?, installed, states, patches })
    }

    /// What was made, as a table: the install, every patch file with its size and its
    /// table, and what the install is after each.
    pub fn table(&self) -> String {
        let client = self.client();
        let mark_of = |version: Version| {
            self.states.iter().find(|(known, _)| *known == version).map_or("-", |(_, mark)| mark.as_str())
        };
        let mut text = format!("a made-up world of the {} client\n", self.server.key());
        text.push_str(&format!(
            "  {INSTALL_DIR}\\ is at {} (number {}), its data marked {}; {} and {} are stand-ins\n",
            self.installed.label(client),
            self.installed.number,
            mark_of(self.installed),
            client.game_program,
            client.launcher
        ));
        text.push_str(&format!(
            "  {:<14}{:<14}{:>12}  {:<9}{:<10}file under {SERVED_DIR}\\ or {HELD_DIR}\\\n",
            "from", "to", "bytes", "table", "then"
        ));
        for patch in &self.patches {
            let table = if patch.originals == 0 { "EMPTY".to_owned() } else { patch.originals.to_string() };
            text.push_str(&format!(
                "  {:<14}{:<14}{:>12}  {:<9}{:<10}{}\n",
                patch.from.label(client),
                patch.to.label(client),
                patch.bytes,
                table,
                mark_of(patch.to),
                patch.path
            ));
        }
        text.push_str(&format!(
            "  `then` is the mark of the install's data after the patch; {EXPECTED_DIR}\\ has the files behind each mark\n"
        ));
        text
    }
}

/// Moves patch files of the world at `dir` from those held back to those that are served.
/// `which` is `all`, or the end of one patch file's path: `00900to00901.patch`,
/// `Minor/01to02.patch`. Returns the paths that are served now.
///
/// The test world publishes a new main version together with its first minor patch, so
/// the main patch to the newest version takes `Minor/01to02.patch` with it there.
pub fn publish(dir: &Path, which: &str) -> Result<Vec<String>, String> {
    let world = World::read(dir)?;
    let held = |patch: &&Patch| dir.join(HELD_DIR).join(&patch.path).is_file();
    let wanted = which.replace('\\', "/");
    let mut chosen: Vec<&Patch> = match which {
        "all" => world.patches.iter().filter(held).collect(),
        _ => world.patches.iter().filter(|patch| patch.path.ends_with(&wanted)).collect(),
    };
    match chosen.as_slice() {
        [] if which == "all" => return Err("every patch file is published already".into()),
        [] => return Err(format!("no patch file of this world ends in {which}")),
        [one] if !held(one) => return Err(format!("{} is published already", one.path)),
        [_] => {}
        _ if which == "all" => {}
        several => {
            let names: Vec<&str> = several.iter().map(|patch| patch.path.as_str()).collect();
            return Err(format!("{which} could be any of: {}", names.join(", ")));
        }
    }
    let newest = world.newest().number;
    if world.server == Server::Test && chosen.iter().any(|patch| (patch.to.number, patch.to.minor) == (newest, 1)) {
        let first_minor = |patch: &&Patch| (patch.from.minor, patch.to.minor) == (1, 2) && patch.from.number == newest;
        let with_it: Vec<&Patch> =
            world.patches.iter().filter(first_minor).filter(held).filter(|patch| !chosen.contains(patch)).collect();
        chosen.extend(with_it);
    }

    let mut published = Vec::new();
    for patch in chosen {
        let (from, to) = (dir.join(HELD_DIR).join(&patch.path), dir.join(SERVED_DIR).join(&patch.path));
        fs::create_dir_all(to.parent().expect("a patch file in a folder"))
            .map_err(failed("create the folder of", &patch.path))?;
        fs::rename(&from, &to).map_err(failed("publish", &patch.path))?;
        published.push(patch.path.clone());
    }
    Ok(published)
}

/// The patch files of the world at `dir` that are served, as paths under the server's root.
pub fn published(dir: &Path) -> Vec<String> {
    let Ok(world) = World::read(dir) else { return Vec::new() };
    let served = |patch: &&Patch| dir.join(SERVED_DIR).join(&patch.path).is_file();
    world.patches.iter().filter(served).map(|patch| patch.path.clone()).collect()
}

/// Where the install folder of the world at `dir` is.
pub fn install_dir(dir: &Path) -> PathBuf {
    dir.join(INSTALL_DIR)
}
