//! Hands the finished report to the user: onto the clipboard, or into a text file beside
//! the program.

use std::path::{Path, PathBuf};

use crate::facts::Unknown;
use crate::units::korean_time_for_a_name;

/// The mark at the start of a text file that tells Notepad, on every Windows there is, that
/// the file is UTF-8.
const UTF8_MARK: &str = "\u{FEFF}";

/// The name a report made at `when` (seconds since 1970) is saved under.
pub fn file_name(when: u64) -> String {
    format!("mfp-check-{}.txt", korean_time_for_a_name(when))
}

/// The bytes of a report as a text file: marked as UTF-8, with the line ends Windows uses.
pub fn as_file(report: &str) -> Vec<u8> {
    let lines: Vec<&str> = report.lines().collect();
    format!("{UTF8_MARK}{}\r\n", lines.join("\r\n")).into_bytes()
}

/// Saves a report into `folder` under [`file_name`], and returns where. A file of that name
/// that is there already is from the same minute, and is replaced.
pub fn save(report: &str, folder: &Path, when: u64) -> Result<PathBuf, Unknown> {
    let path = folder.join(file_name(when));
    std::fs::write(&path, as_file(report)).map_err(|err| crate::probe::io_error(&err))?;
    Ok(path)
}

/// Puts a text onto the clipboard.
pub fn copy(text: &str) -> Result<(), Unknown> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

    use crate::probe::last_error;

    /// Text in 16-bit units, as Windows numbers the forms of what a clipboard holds.
    const UNICODE_TEXT: u32 = 13;
    let units: Vec<u16> = text.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().chain([0]).collect();
    // SAFETY: the memory is asked for in the size that is then written to it, it is locked
    // while it is written and unlocked after, and it is either handed to the clipboard,
    // which then owns it, or freed here. The clipboard is closed again on every way out.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
        if memory.is_null() {
            return Err(last_error());
        }
        let place = GlobalLock(memory);
        if place.is_null() {
            let why = last_error();
            GlobalFree(memory);
            return Err(why);
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), place.cast::<u16>(), units.len());
        GlobalUnlock(memory);
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            let why = last_error();
            GlobalFree(memory);
            return Err(why);
        }
        let put = EmptyClipboard() != 0 && !SetClipboardData(UNICODE_TEXT, memory).is_null();
        let why = last_error();
        CloseClipboard();
        if !put {
            GlobalFree(memory);
            return Err(why);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_saved_as_text_that_notepad_reads_as_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let saved = save("검사 0.1.0\n판정: 일반 PC로 보임\n", dir.path(), 1_795_675_260).unwrap();
        assert_eq!(saved.file_name().unwrap(), "mfp-check-20261126-1541.txt");
        let bytes = std::fs::read(&saved).unwrap();
        assert_eq!(&bytes[..3], [0xEF, 0xBB, 0xBF]);
        assert_eq!(String::from_utf8(bytes[3..].to_vec()).unwrap(), "검사 0.1.0\r\n판정: 일반 PC로 보임\r\n");
        // Line ends that are Windows' already stay single.
        assert_eq!(as_file("a\r\nb")[3..], *b"a\r\nb\r\n");
        // A folder that is not there is an error with its number.
        assert_eq!(save("x", &dir.path().join("gone"), 0), Err(Unknown::Windows(3)));
    }
}
