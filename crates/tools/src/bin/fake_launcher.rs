//! Stands in for a game's program in a made-up world (see `fake_nexon`). When started it
//! writes down one line and ends: the time in Korea, the name it was started under, and what
//! it was started with.
//!
//! The line goes to the file that the environment variable `MFP_FAKE_LAUNCHED` names, or
//! without that variable to `fake-launched.txt` beside the program. So "the game was
//! started" can be seen without anything real being started.

// A game's launcher opens no console, and neither does its stand-in.
#![windows_subsystem = "windows"]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use mfp_tools::calendar::log_stamp;

/// The variable that names the file the line is added to.
const WHERE: &str = "MFP_FAKE_LAUNCHED";

fn main() -> std::io::Result<()> {
    let program = std::env::current_exe()?;
    let name = program.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let started_with: Vec<String> = std::env::args().skip(1).collect();
    let file =
        std::env::var_os(WHERE).map(PathBuf::from).unwrap_or_else(|| program.with_file_name("fake-launched.txt"));
    // What it was started with may name a folder of whoever sits at this PC.
    let line = format!("{}\t{name}\t{}\n", log_stamp(), mfp_tools::shown(&started_with.join(" ")));
    OpenOptions::new().create(true).append(true).open(file)?.write_all(line.as_bytes())
}
