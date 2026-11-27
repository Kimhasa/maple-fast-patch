//! Tries a real patch file end to end when the version it was made for is gone.
//!
//!     cargo run --release -p mfp-tools --bin prepatch -- survey  <patch-file> <patched-install>
//!     cargo run --release -p mfp-tools --bin prepatch -- make    <patch-file> <patched-install> <new-test-dir> [--link]
//!     cargo run --release -p mfp-tools --bin prepatch -- compare <patch-file> <patched-install> <test-dir>
//!
//! `make` builds a folder the patch can be applied to, out of an install that already has
//! the patch (see `mfp_tools::reconstruct`). Apply the patch to it with `run_patch` or with
//! the app, then `compare` checks every file the patch wrote against the patched install,
//! byte for byte.
//!
//! The patched install is only read. `--link` hard-links the originals that are unchanged
//! instead of copying them (same drive only).
//!
//!     cargo run --release -p mfp-tools --bin prepatch -- undo-size <patch-file> [old-install] [--each]
//!
//! `undo-size` reads a patch file and says what a way back to the old version would have to
//! keep, and what room the immediate way of patching needs (see `mfp_tools::undo_size`).
//! With a folder that is still at the old version, the lengths of its files make the figures
//! exact. `--each` adds a line for every file. Nothing is written.
//!
//! Every command takes `--server live|test` for the client the folders are of. Without it
//! the live client is meant.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use mfp_engine::immediate;
use mfp_engine::plan::{self, Action, Plan};
use mfp_engine::preflight::{LOCK_FILE_NAME, WORK_DIR_NAME};
use mfp_engine::stage::Checked;
use mfp_tools::undo_size::{self, Fate, Floor, Measured, Rule, Sequence};
use mfp_tools::{clients, reconstruct};

fn main() -> std::process::ExitCode {
    mfp_tools::end(run())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let programs = clients::programs(clients::take_server(&mut args)?.client());
    let mut args = args.into_iter();
    let what = args.next().ok_or("missing what to do: survey, make, compare or undo-size")?;
    let patch_path = PathBuf::from(args.next().ok_or("missing patch file")?);
    let go = AtomicBool::new(false);

    let started = Instant::now();
    let (patch, plan) = mfp_tools::read_patch(&patch_path)?;
    println!(
        "patch read: {} entries, {} originals ({:.1}s)",
        plan.entries.len(),
        plan.originals.len(),
        started.elapsed().as_secs_f64()
    );

    if what == "undo-size" {
        let rest: Vec<String> = args.collect();
        let old = rest.iter().find(|arg| !arg.starts_with("--")).map(PathBuf::from);
        let measured = undo_size::measure(&patch, &plan, old.as_deref(), &go)?;
        // The name alone: where the file lies is nobody's business.
        let name = patch_path.file_name().unwrap_or_default().to_string_lossy();
        print_undo_size(&name, &plan, &measured, old.as_deref(), rest.iter().any(|arg| arg == "--each"));
        return Ok(());
    }
    let patched = PathBuf::from(args.next().ok_or("missing patched install")?);
    match what.as_str() {
        "survey" => {
            let started = Instant::now();
            print_survey(&reconstruct::survey(&plan, &patched, &go)?);
            println!("({:.1}s)", started.elapsed().as_secs_f64());
        }
        "make" => {
            let out = test_dir(args.next(), &patched)?;
            let link = args.next().as_deref() == Some("--link");
            let started = Instant::now();
            let report = reconstruct::reconstruct(&patch, &plan, &patched, &out, link, &go, &mut |step| {
                println!("[{:>6.1}s] {step}", started.elapsed().as_secs_f64());
            })?;
            reconstruct::pass_as_install(&programs, &patched, &out)?;
            print_survey(&report.survey);
            println!(
                "copy instructions used: {} ({} read bytes another had already supplied)",
                report.copies, report.copies_overlapping
            );
            println!(
                "originals recovered: {} exact (checksum came out by itself), {} adjusted",
                report.exact, report.adjusted
            );
            println!(
                "bytes recovered: {} MB; never read by the patch: {} MB",
                report.recovered_bytes >> 20,
                report.unread_bytes >> 20
            );
            println!("placeholders for delete entries: {:?}", report.placeholders);
            println!("CONFLICTS: {}", report.conflicts.len());
            for conflict in &report.conflicts {
                println!("  {conflict}");
            }
            println!("stand-ins without a solution: {:?}", report.unsolved);
            let made = mfp_tools::shown(&out.display().to_string());
            println!("done in {:.1}s: {made}", started.elapsed().as_secs_f64());
        }
        "compare" => {
            let out = PathBuf::from(args.next().ok_or("missing test dir")?);
            compare(&plan, &patched, &out)?;
        }
        other => return Err(format!("unknown command {other}").into()),
    }
    Ok(())
}

fn print_survey(survey: &reconstruct::Survey) {
    println!("originals unchanged on disk: {} ({} MB)", survey.intact, survey.intact_bytes >> 20);
    println!("originals to recover: {}", survey.to_recover);
    println!("rebuilt files on disk as the patch made them: {}", survey.rebuilds_on_disk);
    println!("rebuilt files changed again later (stand-ins): {}", survey.stand_ins.len());
    for name in &survey.stand_ins {
        println!("  {name}");
    }
}

/// The folder to build in. Never the real game folder or anything near it.
fn test_dir(arg: Option<String>, patched: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out = PathBuf::from(arg.ok_or("missing test dir")?);
    if clients::under_nexon(&out) {
        return Err("refusing to build in a folder under `Nexon`".into());
    }
    if out.starts_with(patched) || patched.starts_with(&out) {
        return Err("the test folder must be separate from the patched install".into());
    }
    Ok(out)
}

/// Compares what the patch wrote in `out` with the patched install.
fn compare(plan: &Plan, patched: &Path, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let (mut same, mut different, mut missing, mut bytes) = (0, Vec::new(), Vec::new(), 0u64);
    let mut still_there = Vec::new();
    for entry in &plan.entries {
        let relative = plan::relative_path(&entry.name)?;
        match entry.action {
            Action::Delete => {
                if out.join(&relative).exists() {
                    still_there.push(entry.name.clone());
                }
            }
            Action::CreateDir => {
                if !out.join(&relative).is_dir() {
                    missing.push(entry.name.clone());
                }
            }
            Action::Create { len: 0, .. } => {}
            Action::Create { .. } | Action::Rebuild { .. } => {
                match files_equal(&out.join(&relative), &patched.join(&relative)) {
                    Ok(Some(len)) => {
                        same += 1;
                        bytes += len;
                    }
                    Ok(None) => different.push(entry.name.clone()),
                    Err(_) => missing.push(entry.name.clone()),
                }
            }
        }
    }
    println!(
        "identical to the patched install: {same} files, {} MB ({:.1}s)",
        bytes >> 20,
        started.elapsed().as_secs_f64()
    );
    println!("different: {}", different.len());
    for name in &different {
        println!("  {name}");
    }
    println!("missing: {missing:?}");
    println!("should have been deleted but are still there: {still_there:?}");
    for leftover in [WORK_DIR_NAME, LOCK_FILE_NAME] {
        println!("{leftover} left behind: {}", out.join(leftover).exists());
    }
    Ok(())
}

/// The length of two files when they hold the same bytes, `None` when they differ.
fn files_equal(a: &Path, b: &Path) -> std::io::Result<Option<u64>> {
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let len = a.metadata()?.len();
    if len != b.metadata()?.len() {
        return Ok(None);
    }
    let (mut left, mut right) = (vec![0u8; 4 << 20], vec![0u8; 4 << 20]);
    let mut done = 0u64;
    while done < len {
        let count = (len - done).min(left.len() as u64) as usize;
        a.read_exact(&mut left[..count])?;
        b.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] {
            return Ok(None);
        }
        done += count as u64;
    }
    Ok(Some(len))
}

/// A number of bytes as MiB.
fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 20) as f64
}

/// A number of bytes, exactly and as GiB.
fn amount(bytes: u64) -> String {
    format!("{bytes} B ({:.2} GiB)", bytes as f64 / (1u64 << 30) as f64)
}

/// Prints what `undo-size` found out about the patch file called `name`.
fn print_undo_size(name: &str, plan: &Plan, measured: &Measured, old: Option<&Path>, each: bool) {
    let counts = &measured.counts;
    println!("{name}: {:?} layout", plan.layout);
    println!(
        "entries: {} rebuilt, {} whole files, {} empty files, {} folders, {} deletes",
        counts.rebuilds, counts.whole, counts.empty, counts.folders, counts.deletes
    );
    println!("new files: {} rebuilt, {} whole", amount(counts.rebuilt_bytes), amount(counts.whole_bytes));
    println!(
        "instructions: {} copies ({:.1} MiB), {} literals ({:.1} MiB), {} fills ({:.1} MiB)",
        counts.copies,
        mib(counts.copy_bytes),
        counts.literals,
        mib(counts.literal_bytes),
        counts.fills,
        mib(counts.fill_bytes)
    );

    println!();
    println!("old files the patch takes away, and what a way back has to keep of them (MiB):");
    println!(
        "  {:<15}{:>6}{:>8}{:>14}{:>12}{:>15}{:>12}{:>12}{:>12}",
        "", "files", "read", "highest read", "covered", "least to keep", "in install", "length", "to keep"
    );
    let mut unread = 0;
    for (label, fate) in
        [("rebuilt", Fate::Rebuilt), ("replaced whole", Fate::ReplacedWhole), ("deleted", Fate::Deleted)]
    {
        let of_fate = || measured.olds.iter().filter(move |old| old.fate == fate);
        unread += of_fate().filter(|old| old.extent == 0).count();
        println!(
            "  {:<15}{:>6}{:>8}{:>14.1}{:>12.1}{:>15.1}{:>12}{:>12.1}{:>12.1}",
            label,
            of_fate().count(),
            of_fate().filter(|old| old.extent > 0).count(),
            mib(of_fate().map(|old| old.extent).sum()),
            mib(of_fate().map(|old| old.covered).sum()),
            mib(of_fate().map(|old| old.least_kept()).sum()),
            of_fate().filter(|old| old.len.is_some()).count(),
            mib(of_fate().filter_map(|old| old.len).sum()),
            mib(of_fate().filter_map(|old| old.kept()).sum()),
        );
    }
    println!("  old files the patch only reads: {}", measured.olds.iter().filter(|old| !old.changed()).count());
    println!("at least to keep, from the patch file alone: {}", amount(measured.least_kept()));
    println!(
        "  not in that figure: {unread} files that no copy reads. They are kept in full, and a patch \
         file does not say how long they were."
    );
    match old {
        None => println!("to keep, exactly: not known without a folder at the old version"),
        Some(_) => {
            println!("to keep, exactly, by the lengths in the install given: {}", amount(measured.kept()));
            if measured.too_short() > 0 {
                println!(
                    "  BUT {} files there are shorter than the patch reads them: that folder is not the \
                     version this patch was made for.",
                    measured.too_short()
                );
            }
        }
    }

    println!();
    let from_patch = |name: &str| {
        let key = plan::name_key(name);
        measured.olds.iter().find(|old| plan::name_key(&old.name) == key).map_or(0, |old| old.extent)
    };
    let from_install = |name: &str| {
        let path = old.zip(plan::relative_path(name).ok()).map(|(install, relative)| install.join(relative));
        path.and_then(|path| std::fs::metadata(path).ok()).filter(|meta| meta.is_file()).map_or(0, |meta| meta.len())
    };
    println!("room the immediate way needs beyond what the folder holds at the start (MiB),");
    println!("in square brackets the new files that wait for the apply at the end, and in round");
    println!("ones the most files that go into place in one go:");
    match old {
        None => println!(
            "  (the old files taken to be as long as the patch reads them; they are longer, and each \
             gives more room back than is counted here)"
        ),
        Some(_) => println!("  (the old files as long as they are in the install given)"),
    }
    println!("  {:<36}{:>32}{:>32}", "a new file goes into place", "built as in the patch", "consumers built first");
    for (label, rule) in [
        ("once its consumers are built", Rule::ConsumersBuilt),
        ("once its consumers are in place", Rule::ConsumersInPlace),
        ("with its consumers at the latest", Rule::ConsumersGoAlong),
    ] {
        let cell = |sequence: Sequence| {
            let Floor { peak, left, left_bytes, largest_group } = match old {
                None => undo_size::floor(plan, &from_patch, rule, sequence),
                Some(_) => undo_size::floor(plan, &from_install, rule, sequence),
            };
            format!("{:.1} [{left}: {:.1}] ({largest_group})", mib(peak), mib(left_bytes))
        };
        println!("  {:<36}{:>32}{:>32}", label, cell(Sequence::AsInPatch), cell(Sequence::ConsumersFirst));
    }
    println!("  the safe way, every new file next to the old ones: {:.1}", mib(plan.new_bytes()));
    if let Some(install) = old {
        let engine = immediate::space_needed(plan, &Checked::default(), &BTreeSet::new(), install);
        println!("  the engine's own figure for that install, nothing in place yet: {:.1} ({engine} B)", mib(engine));
        println!("  that install holds {} B in all", mfp_tools::folder_size(install));
    }

    if each {
        println!();
        println!(
            "{:<15}{:>14}{:>14}{:>14}{:>14}{:>14}  name",
            "", "highest read", "covered", "least to keep", "length", "to keep"
        );
        let known = |bytes: Option<u64>| bytes.map_or("?".to_owned(), |bytes| bytes.to_string());
        for old in &measured.olds {
            let fate = match old.fate {
                Fate::Rebuilt => "rebuilt",
                Fate::ReplacedWhole => "replaced whole",
                Fate::Deleted => "deleted",
                Fate::Kept => "only read",
            };
            println!(
                "{:<15}{:>14}{:>14}{:>14}{:>14}{:>14}  {}",
                fate,
                old.extent,
                old.covered,
                old.least_kept(),
                known(old.len),
                known(old.kept()),
                old.name
            );
        }
    }
}
