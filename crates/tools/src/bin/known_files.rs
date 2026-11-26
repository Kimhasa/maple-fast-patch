//! Writes down what the files of a client are after each of a row of patches, for the
//! diagnostic program (`mfp-check`) to hold a folder against.
//!
//!     cargo run --release -p mfp-tools --bin known_files -- <out-file> --server live|test <patch-file>...
//!
//! The patch files are given in the order they were published, a main patch first:
//! `00420to00421.patch 01to02.patch 02to03.patch 03to04.patch` is the live client from the
//! last minor of 1.2.420 to 1.2.421 (4). A file named `<from>to<to>.patch` with five-digit
//! numbers is a main patch, and any other name is the next minor patch of the version before
//! it. A second main patch may follow the minors of the first.
//!
//! A patch says of every file it writes what the new file's checksum and length are, and of
//! every file it starts from what its checksum was. Both are kept. A file no patch in the
//! row names is not in the table: nothing is known of it.
//!
//! Reads only the patch files. The table is plain text; `crates\check\src\known.rs` says
//! what its lines mean and reads it back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use mfp_engine::container::PatchFile;
use mfp_engine::plan::{self, Action, Plan};
use mfp_tools::clients;

fn main() -> std::process::ExitCode {
    mfp_tools::end(run())
}

/// A point in the life of a client that a patch starts from or leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    /// The main version, as the patch files count it.
    version: u32,
    /// The minor version. Zero stands for "the last minor of this version, whichever it
    /// was": a main patch starts from there and does not say which minor that is.
    minor: u32,
}

/// What a patch says a file is once the patch is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Written {
    File { crc: u32, len: u64 },
    Gone,
}

/// Everything the row of patches says about one file.
#[derive(Default)]
struct History {
    /// The name as a patch first wrote it.
    name: String,
    /// What the file became, by the number of the state it became that at.
    written: BTreeMap<usize, Written>,
    /// The checksum a patch expected the file to have, by the number of the state the patch
    /// starts from.
    expected: BTreeMap<usize, u32>,
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let server = clients::take_server(&mut args)?;
    let mut args = args.into_iter();
    let out = PathBuf::from(args.next().ok_or("missing the file to write the table to")?);
    let patches: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if patches.is_empty() {
        return Err("missing patch files".into());
    }
    let go = AtomicBool::new(false);

    let mut states: Vec<State> = Vec::new();
    let mut files: BTreeMap<String, History> = BTreeMap::new();
    for path in &patches {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let to = match (main_versions(path), states.last()) {
            (Some((from, to)), None) => {
                states.push(State { version: from, minor: 0 });
                State { version: to, minor: 1 }
            }
            (Some((from, to)), Some(last)) if last.version == from => State { version: to, minor: 1 },
            (Some((from, _)), Some(last)) => {
                return Err(
                    format!("{name} starts from {from}, and the patches before it lead to {}", last.version).into()
                );
            }
            (None, Some(last)) if last.minor >= 1 => State { version: last.version, minor: last.minor + 1 },
            (None, _) => return Err(format!("{name} is a minor patch, and no main patch comes before it").into()),
        };
        let patch = PatchFile::open(path)?;
        patch.verify(&go, |_| {})?;
        let plan = plan::scan(&patch, &go)?;
        let from = states.len() - 1;
        states.push(to);
        take(&plan, from, &mut files);
        println!(
            "{name}: to {}.{} with {} files it starts from and {} entries",
            to.version,
            to.minor,
            plan.originals.len(),
            plan.entries.len()
        );
    }

    let broken = contradictions(&files);
    for line in broken.iter().take(20) {
        println!("  does not follow: {line}");
    }
    if !broken.is_empty() {
        return Err(format!(
            "{} files are not what the patch before left them as: the row of patches has a gap",
            broken.len()
        )
        .into());
    }

    let text = table(server.key(), &states, &files);
    std::fs::write(&out, text)?;
    summarise(&states, &files);
    println!("written: {}", mfp_tools::shown(&out.display().to_string()));
    Ok(())
}

/// The main versions a patch file leads from and to, read from a name such as
/// `00421to00422.patch`. `None` for the name of a minor patch.
fn main_versions(file: &Path) -> Option<(u32, u32)> {
    let name = file.file_stem()?.to_str()?;
    let (from, to) = name.split_once("to")?;
    let number = |text: &str| if text.len() == 5 { text.parse::<u32>().ok() } else { None };
    Some((number(from)?, number(to)?))
}

/// Adds what one patch says to the histories: `from` is the number of the state it starts
/// from, and it leads to the one after.
fn take(plan: &Plan, from: usize, files: &mut BTreeMap<String, History>) {
    for (name, crc) in &plan.originals {
        history(files, name).expected.insert(from, *crc);
    }
    let mut gone_folders = Vec::new();
    for entry in &plan.entries {
        match &entry.action {
            Action::CreateDir => {}
            Action::Create { len, crc } => {
                history(files, &entry.name).written.insert(from + 1, Written::File { crc: *crc, len: *len });
            }
            Action::Rebuild { old_crc, new_crc, new_len, .. } => {
                let history = history(files, &entry.name);
                if let Some(old) = old_crc {
                    history.expected.insert(from, *old);
                }
                history.written.insert(from + 1, Written::File { crc: *new_crc, len: *new_len });
            }
            // A delete names a file or a folder, and the name does not always say which.
            // Whatever lies below the name goes with it.
            Action::Delete => {
                history(files, &entry.name);
                gone_folders.push(plan::name_key(&entry.name));
            }
        }
    }
    for gone in gone_folders {
        let below = format!("{gone}\\");
        for (key, history) in files.iter_mut() {
            if *key == gone || key.starts_with(&below) {
                // A patch that deletes a name and writes it anew has written it.
                history.written.entry(from + 1).or_insert(Written::Gone);
            }
        }
    }
}

/// The history of the file a patch calls `name`, begun if this is the first word of it.
fn history<'a>(files: &'a mut BTreeMap<String, History>, name: &str) -> &'a mut History {
    let history = files.entry(plan::name_key(name)).or_default();
    if history.name.is_empty() {
        history.name = name.trim_end_matches(['\\', '/']).replace('/', "\\");
    }
    history
}

/// What is known of a file at the state numbered `state`: what the last patch up to there
/// made of it, or else what a patch expected it to be, earlier or later, while no patch
/// in the row had written it yet. `None` when nothing says.
fn at(history: &History, state: usize) -> Option<Written> {
    if let Some((_, written)) = history.written.range(..=state).next_back() {
        return Some(*written);
    }
    // No patch has written the file up to here, so it is what it was at every state before
    // the first patch that writes it. The length is not said where only the checksum was
    // expected.
    let first_write = history.written.keys().next().copied().unwrap_or(usize::MAX);
    history.expected.range(..first_write).next().map(|(_, crc)| Written::File { crc: *crc, len: u64::MAX })
}

/// The files a patch expected to be something else than the patch before had made of them.
fn contradictions(files: &BTreeMap<String, History>) -> Vec<String> {
    let mut found = Vec::new();
    for history in files.values() {
        for (state, expected) in &history.expected {
            match history.written.range(..=*state).next_back() {
                Some((_, Written::File { crc, .. })) if crc != expected => {
                    found.push(format!(
                        "{} at state {state}: left as {crc:08x}, expected as {expected:08x}",
                        history.name
                    ));
                }
                Some((_, Written::Gone)) => {
                    found.push(format!("{} at state {state}: deleted, then expected", history.name))
                }
                _ => {}
            }
        }
        // Two patches that expect an untouched file to be two different things.
        let first_write = history.written.keys().next().copied().unwrap_or(usize::MAX);
        let mut untouched = history.expected.range(..first_write).map(|(_, crc)| *crc);
        if let Some(first) = untouched.next()
            && untouched.any(|crc| crc != first)
        {
            found.push(format!("{}: expected as two different files while no patch wrote it", history.name));
        }
    }
    found
}

/// The table as text. One line for each state, then one for each file: its name and, after
/// tabs, what it is from which state on, as `state:checksum:length`, `state:checksum` where
/// the length is not known, and `state:-` where the file is gone.
fn table(server: &str, states: &[State], files: &BTreeMap<String, History>) -> String {
    let mut text = String::new();
    text.push_str("# What the files of a client are after each patch. Written by the known_files program of\n");
    text.push_str("# mfp-tools from Nexon's patch files; not to be changed by hand.\n");
    text.push_str(&format!("client {server}\n"));
    for state in states {
        text.push_str(&format!("state {} {}\n", state.version, state.minor));
    }
    for history in files.values() {
        let mut line = history.name.clone();
        let mut last = None;
        for state in 0..states.len() {
            let now = at(history, state);
            if now == last || now.is_none() {
                continue;
            }
            last = now;
            match now {
                Some(Written::File { crc, len: u64::MAX }) => line.push_str(&format!("\t{state}:{crc:08x}")),
                Some(Written::File { crc, len }) => line.push_str(&format!("\t{state}:{crc:08x}:{len}")),
                Some(Written::Gone) => line.push_str(&format!("\t{state}:-")),
                None => {}
            }
        }
        if last.is_some() {
            text.push_str(&line);
            text.push('\n');
        }
    }
    text
}

/// Prints how much of the client the table knows at each state.
fn summarise(states: &[State], files: &BTreeMap<String, History>) {
    for (number, state) in states.iter().enumerate() {
        let (mut sized, mut bytes, mut unsized_, mut gone) = (0u64, 0u64, 0u64, 0u64);
        for history in files.values() {
            match at(history, number) {
                Some(Written::File { len: u64::MAX, .. }) => unsized_ += 1,
                Some(Written::File { len, .. }) => {
                    sized += 1;
                    bytes += len;
                }
                Some(Written::Gone) => gone += 1,
                None => {}
            }
        }
        println!(
            "  at {}.{}: {} files known with their length ({:.2} GiB), {} by checksum alone, {} known to be gone",
            state.version,
            state.minor,
            sized,
            bytes as f64 / (1u64 << 30) as f64,
            unsized_,
            gone
        );
    }
    println!("  files named in all: {}", files.len());
}
