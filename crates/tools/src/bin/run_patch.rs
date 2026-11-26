//! Runs a whole patch the way the app does, from the command line. For test folders.
//!
//!     cargo run --release -p mfp-tools --bin run_patch -- <patch-url-or-file> <test-dir> [download-dir]
//!         [--server live|test] [--immediate] [--pretend-drive-size <bytes>] [--slack <bytes>]
//!
//! THIS CHANGES THE FOLDER IT IS GIVEN. It refuses folders under a `Nexon` folder, which is
//! where the real game lives; use `make_test_install` to make a folder to try it on.
//!
//! `--server` says which client the folder is of: the folder must hold that client's game
//! program, and that client's programs must not be running. Without it the live client.
//!
//! `--immediate` allows the immediate way of patching, as the player does in the app. The
//! engine takes it only when the new files do not fit next to the old ones.
//!
//! `--pretend-drive-size` makes that happen on a drive with plenty of room. The run is told
//! that the test folder is on a drive of that many bytes which holds nothing else, so what
//! is free there goes down and up with every byte the folder grows and shrinks, and is the
//! same when a run that was cut off is started again. Such a run says in its first lines and
//! its last that the drive was pretended: it rehearses a tight drive and is not one.
//!
//! `--slack` is the room the immediate way asks for at its start beyond the least it can work
//! with. Without it, what the app uses.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mfp_engine::run::{Disk, run_on_disk};
use mfp_engine::{Event, Options, Source, Stage, immediate, preflight};
use mfp_tools::{clients, folder_size};

/// How often the free space of the real drive is looked at while a run goes on, and how
/// often the test folder is measured, which takes a walk through all of it.
const LOOK: Duration = Duration::from_millis(20);
const MEASURE_EVERY: u32 = 12;

fn main() -> std::process::ExitCode {
    mfp_tools::end(run())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (mut plain, mut allow_immediate, mut drive_size, mut slack) = (Vec::new(), false, None, immediate::SLACK);
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let programs = clients::programs(clients::take_server(&mut args)?.client());
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut number = || args.next().and_then(|text| text.parse::<u64>().ok()).ok_or(format!("{arg} needs bytes"));
        match arg.as_str() {
            "--immediate" => allow_immediate = true,
            "--pretend-drive-size" => drive_size = Some(number()?),
            "--slack" => slack = number()?,
            other if other.starts_with("--") => return Err(format!("unknown option {other}").into()),
            _ => plain.push(arg),
        }
    }
    let mut plain = plain.into_iter();
    let patch = plain.next().ok_or("missing patch url or file")?;
    let install_dir = PathBuf::from(plain.next().ok_or("missing test dir")?);
    let downloads = plain.next().map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    if clients::under_nexon(&install_dir) {
        return Err("refusing to run on a folder under `Nexon`; this tool is for test folders".into());
    }

    let source = if patch.starts_with("http") {
        let dest = downloads.join(patch.rsplit('/').next().unwrap());
        Source::Url { url: patch, dest }
    } else {
        Source::File(PathBuf::from(patch))
    };
    // What the drive of the test folder has free: what it really has, or less when a
    // smaller drive is pretended. Other drives are what they are.
    let free = |dir: &Path| -> io::Result<u64> {
        let real = preflight::free_space(dir)?;
        Ok(match drive_size {
            Some(size) if dir.starts_with(&install_dir) => real.min(size.saturating_sub(folder_size(&install_dir))),
            _ => real,
        })
    };
    let folder_at_start = folder_size(&install_dir);
    let free_at_start = preflight::free_space(&install_dir)?;
    if let Some(size) = drive_size {
        println!(
            "          PRETENDED DRIVE of {size} B that holds this folder ({folder_at_start} B) and nothing else: \
             {} B free. The real drive has {free_at_start} B free. A rehearsal of a tight drive, not one.",
            free(&install_dir)?
        );
    }

    let started = Instant::now();
    let options =
        Options { install_dir: install_dir.clone(), source, immediate: allow_immediate, storage: Default::default() };
    let pretended = if drive_size.is_some() { " (pretended)" } else { "" };
    let (finished, largest, least_free) =
        (AtomicBool::new(false), AtomicU64::new(folder_at_start), AtomicU64::new(free_at_start));
    // New files this run was told are in their place for good.
    let placed = AtomicU64::new(0);
    let outcome = std::thread::scope(|scope| {
        // Watches the room the run takes while it goes on.
        scope.spawn(|| {
            let mut looks = 0u32;
            while !finished.load(Ordering::Relaxed) {
                if let Ok(now) = preflight::free_space(&install_dir) {
                    least_free.fetch_min(now, Ordering::Relaxed);
                }
                if looks.is_multiple_of(MEASURE_EVERY) {
                    let mut size = folder_size(&install_dir);
                    // A file that is moved while the folder is walked can be met in both
                    // places. What looks like a new high is measured once more: the file
                    // will not be caught in the same move twice.
                    if size > largest.load(Ordering::Relaxed) {
                        size = size.min(folder_size(&install_dir));
                    }
                    largest.fetch_max(size, Ordering::Relaxed);
                }
                looks += 1;
                std::thread::sleep(LOOK);
            }
        });
        let disk = Disk { free: &free, slack, file_system: None };
        // Whether the build has yet to say where it begins, and the files of an earlier run
        // that were found built and have not been told of yet.
        let (mut building_begins, mut kept, mut kept_bytes) = (false, 0u32, 0u64);
        let tell_kept = |kept: &mut u32, kept_bytes: &mut u64| {
            if *kept > 0 {
                println!("          kept {kept} file(s) an earlier run built ({} MB)", *kept_bytes >> 20);
                (*kept, *kept_bytes) = (0, 0);
            }
        };
        let outcome = run_on_disk(&programs, &options, &disk, &AtomicBool::new(false), &mut |event| {
            match event {
                Event::Stage(stage) => {
                    tell_kept(&mut kept, &mut kept_bytes);
                    println!("[{:6.1}s] {stage:?}", started.elapsed().as_secs_f32());
                    building_begins = stage == Stage::Rebuild;
                }
                Event::Kept { bytes, .. } => {
                    kept += 1;
                    kept_bytes += bytes;
                }
                // A build that does not begin at nothing: some new files are in place already.
                Event::Progress { done, total } if building_begins => {
                    building_begins = false;
                    if done > 0 {
                        println!("          begins at {} of {} MB", done >> 20, total >> 20);
                    }
                }
                Event::File(_) => tell_kept(&mut kept, &mut kept_bytes),
                Event::Placed { .. } => {
                    placed.fetch_add(1, Ordering::Relaxed);
                }
                Event::Space { needed, available } => {
                    println!(
                        "          needs {} MB, {} MB free{pretended} ({needed} B, {available} B)",
                        needed >> 20,
                        available >> 20
                    )
                }
                Event::Restored => println!("          restored an interrupted apply first"),
                Event::Storage { file_system, seek_ms, slow, readers, checkers, unbuffered } => println!(
                    "          drive: {file_system}, a read far away takes {seek_ms:?} ms, slow: {slow}; \
                 {readers} readers, {checkers} checkers, past the file cache: {unbuffered}"
                ),
                Event::Progress { .. } => {}
            }
        });
        finished.store(true, Ordering::Relaxed);
        outcome
    });

    let (largest, least_free) = (largest.load(Ordering::Relaxed), least_free.load(Ordering::Relaxed));
    let report = || {
        println!("          new files in place: {}", placed.load(Ordering::Relaxed));
        println!(
            "          folder: {folder_at_start} B at the start, {largest} B at its largest (+{} B), {} B now",
            largest - folder_at_start,
            folder_size(&install_dir)
        );
        println!(
            "          real drive: {free_at_start} B free at the start, {least_free} B at the least (-{} B)",
            free_at_start - least_free
        );
        if let Some(size) = drive_size {
            println!(
                "          PRETENDED DRIVE: {} B free at the least. Not a run on a tight drive.",
                size.saturating_sub(largest)
            );
        }
    };
    let outcome = outcome.inspect_err(|_| report())?;
    println!("[{:6.1}s] done, {} file(s) changed", started.elapsed().as_secs_f32(), outcome.files_changed);
    println!("          way: {}", if outcome.immediate { "immediate" } else { "safe" });
    report();
    Ok(())
}
