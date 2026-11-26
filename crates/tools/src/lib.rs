//! Programs for trying real patch files on test folders, and what they share.
//!
//! None of this is part of the app. The programs are in `src\bin`:
//!
//! - `inspect` compares a patch file with an install folder and changes nothing.
//! - `make_test_install` makes a small folder that is one patch behind.
//! - `prepatch` makes a folder a patch can be applied to out of an install that already has
//!   the patch (see [`reconstruct`]), compares the result of a run with that install, and
//!   says what a way back would have to keep (see [`undo_size`]).
//! - `run_patch` runs a patch on a test folder the way the app does, on a drive that is
//!   pretended to be tight if that is asked for.
//! - `fake_nexon` makes a small world to try the app against without the real game or
//!   Nexon's servers (see [`fake_world`]): an install, patch files for it, a server on this
//!   PC that hands them out (see [`fake_server`]) and tells of maintenance (see
//!   [`fake_notice`]).
//! - `fake_launcher` stands in for the game's programs in that world: it writes down that
//!   it was started.
//!
//! The first four take `--server live|test` for the client they are run for (see
//! [`clients`]).

pub mod calendar;
pub mod clients;
pub mod fake_notice;
pub mod fake_server;
pub mod fake_world;
pub mod reconstruct;
pub mod undo_size;

/// The writer of patch files that the engine's own tests use, taken from where they keep
/// it. The made-up world is written with it, so that there is one writer and not two.
#[path = "../../engine/tests/support/builder.rs"]
mod builder;

use std::fs;
use std::path::Path;

/// Bytes of all files under `dir`. Each file is asked for its own length: what a folder
/// lists for a file that is being written lags behind.
pub fn folder_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        let Ok(children) = fs::read_dir(&next) else { continue };
        for child in children.flatten() {
            match fs::metadata(child.path()) {
                Ok(meta) if meta.is_dir() => todo.push(child.path()),
                Ok(meta) => total += meta.len(),
                // Gone by now: it was just moved.
                Err(_) => {}
            }
        }
    }
    total
}

/// What stands in the place of the home folder in anything a program shows.
const HOME: &str = "%USERPROFILE%";

/// A message or a path as a program may show it. Folders that were given to a program come
/// back in what it prints and in what goes wrong, and a folder under the home folder carries
/// the name of whoever sits at this PC. That part is taken out, so that what a program
/// printed can be passed on as it is.
pub fn shown(text: &str) -> String {
    match std::env::var("USERPROFILE") {
        Ok(home) if !home.is_empty() => without_home(text, &home),
        _ => text.to_owned(),
    }
}

/// [`shown`] for a home folder that is given. The folder is found however its letters are
/// cased, with either kind of separator, and with its separators doubled, as they are where
/// an error is printed in full.
fn without_home(text: &str, home: &str) -> String {
    let home = home.trim_end_matches(['\\', '/']);
    let mut text = text.to_owned();
    for form in [home.replace('\\', "\\\\"), home.replace('\\', "/"), home.to_owned()] {
        // Lower case moves no ASCII letter, so a place in one text is the same place in the other.
        let (lower, wanted) = (text.to_ascii_lowercase(), form.to_ascii_lowercase());
        let mut cleaned = String::with_capacity(text.len());
        let mut from = 0;
        while let Some(found) = lower[from..].find(&wanted) {
            let (at, end) = (from + found, from + found + wanted.len());
            cleaned.push_str(&text[from..at]);
            // Only where the folder's name ends there: `C:\Users\an` is not `C:\Users\anna`.
            let whole = !text[end..].starts_with(|next: char| next.is_alphanumeric());
            cleaned.push_str(if whole { HOME } else { &text[at..end] });
            from = end;
        }
        cleaned.push_str(&text[from..]);
        text = cleaned;
    }
    text
}

/// Ends a program: with what went wrong, as [`shown`], when something did.
pub fn end<E: std::fmt::Debug>(result: Result<(), E>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {}", shown(&format!("{err:?}")));
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_folder_is_taken_out_of_what_a_program_shows() {
        let home = r"C:\Users\anna";
        assert_eq!(
            without_home(r"test folder: C:\Users\anna\AppData\Local\Temp\world", home),
            r"test folder: %USERPROFILE%\AppData\Local\Temp\world"
        );
        // As an error prints it in full, with other letters, and with the other separator.
        assert_eq!(
            without_home(
                r#"NotAnInstallDir { dir: "c:\\users\\ANNA\\world\\install", missing: "MapleStory.exe" }"#,
                home
            ),
            r#"NotAnInstallDir { dir: "%USERPROFILE%\\world\\install", missing: "MapleStory.exe" }"#
        );
        assert_eq!(
            without_home("open C:/Users/anna/a.patch and C:/Users/anna", home),
            "open %USERPROFILE%/a.patch and %USERPROFILE%"
        );
        // Another folder that only begins alike is left as it is, and so is everything else.
        assert_eq!(without_home(r"C:\Users\annabel\world", home), r"C:\Users\annabel\world");
        assert_eq!(without_home(r"C:\MapleFastPatchTest\Bench", home), r"C:\MapleFastPatchTest\Bench");
        assert_eq!(without_home(r"C:\Users\anna\x", r"C:\Users\anna\"), r"%USERPROFILE%\x");
    }
}
