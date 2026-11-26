//! The log file, and the word the program leaves there and on the screen when it fails.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use crate::window::message_box;

/// The log file may grow to this size. Then it is put aside and a new one begun.
const LOG_MOST: u64 = 1 << 20;

/// Where the log file is, from the moment the app knows its data folder. There is one log
/// for the whole app, and whatever has a line for it finds it here: a run, a watch, and
/// what speaks up when the program fails.
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Says where the log is kept: in `data`, the app's data folder. Said once, as the app
/// starts; nothing is written before.
pub(crate) fn keep_log_in(data: &Path) {
    let _ = LOG_PATH.set(data.join("maple-fast-patch.log"));
}

/// Opens the log file to add lines to it. One that has grown past [`LOG_MOST`] is first put
/// aside as the one older log that is kept, in place of the one kept before.
pub(crate) fn open_log() -> Option<std::fs::File> {
    let path = LOG_PATH.get()?;
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_MOST) {
        let _ = std::fs::rename(path, dir.join("maple-fast-patch.log.1"));
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Makes the program say so when it fails by itself: a line in the log, and one box on the
/// screen. Without this it would only be gone, since the program as it is given out has no
/// console to say anything on.
pub(crate) fn speak_up_on_failure() {
    static SAID: AtomicBool = AtomicBool::new(false);
    let usual = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |failure| {
        usual(failure);
        // Where it happened is a file on the PC the program was built on. Its name and the
        // line are of use; the folders above it are that PC's own and are left out.
        let place = failure.location().map(|at| {
            let file = at.file().rsplit(['/', '\\']).next().unwrap_or_default();
            format!(" at {file}:{}", at.line())
        });
        let message = failure.payload_as_str().unwrap_or("no message");
        // The log is known once the app has started. A failure before that has only the box.
        let log = LOG_PATH.get().and_then(|path| std::fs::OpenOptions::new().append(true).open(path).ok());
        if let Some(mut log) = log {
            write_line(&mut log, &format!("failed: {message}{}", place.unwrap_or_default()));
        }
        // One box, however many threads fail. A thread that fails while the box is up waits
        // here; the program ends when the box of the first one has been read.
        if SAID.swap(true, Ordering::SeqCst) {
            loop {
                std::thread::park();
            }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SETFOREGROUND};
            message_box("프로그램에 문제가 발생하여 종료합니다.", MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
        }
        // The program as it is given out ends by itself after any failure of this kind. A
        // build made for trying things out would only end the thread that failed, and leave
        // a run that is no longer at work marked as running. It ends here instead, as the
        // box says.
        #[cfg(panic = "unwind")]
        {
            std::process::exit(1);
        }
    }));
}

/// Adds a line to the log.
pub(crate) fn write_line(log: &mut std::fs::File, text: &str) {
    use std::io::Write;
    // One event, one line: a file name from a patch file must not be able to make up lines
    // of its own.
    let mut text = text.replace(|c: char| c.is_control(), " ");
    // The log is a file people are asked to send along when something went wrong. A path
    // in the user's own folder would give their Windows user name away, so that folder is
    // written the way Windows itself abbreviates it. It can also stand in a line with every
    // backslash doubled, where the engine prints a path as part of a larger value.
    if let Some(profile) = std::env::var_os("USERPROFILE").map(|folder| folder.to_string_lossy().into_owned()) {
        for written in [profile.replace('\\', "\\\\"), profile] {
            text = replace_ignoring_case(&text, &written, "%USERPROFILE%");
        }
    }
    // The line is put together first and handed over in one piece. A run and the rest of
    // the app each have the file open, and a line written in parts could end up with
    // another one in its middle.
    let line = format!("{} {text}\n", local_time());
    let _ = log.write_all(line.as_bytes());
}

/// `text` with `with` in every place where `what` stands, in capitals or small letters.
/// Windows does not tell the two apart in a path, and writes the same folder either way.
fn replace_ignoring_case(text: &str, what: &str, with: &str) -> String {
    // An empty `what` is found at every place, and replacing it would never end.
    if what.is_empty() {
        return text.to_owned();
    }
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let mut replaced = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        // Compared letter by letter, since a letter and its other case need not be equally
        // long in bytes.
        let mut found = rest.char_indices();
        if what.chars().all(|wanted| found.next().is_some_and(|(_, letter)| same(letter, wanted))) {
            replaced.push_str(with);
            rest = &rest[found.next().map_or(rest.len(), |(after, _)| after)..];
        } else {
            replaced.push(first);
            rest = &rest[first.len_utf8()..];
        }
    }
    replaced
}

/// Adds a line to the log file, for things that happen outside a run.
pub(crate) fn log_line(text: &str) {
    if let Some(mut log) = open_log() {
        write_line(&mut log, text);
    }
}

/// The date and time on this PC's clock, for a line in the log.
fn local_time() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        // SAFETY: the call only fills in the struct it is handed, and every bit pattern is a
        // valid `SYSTEMTIME`.
        let now = unsafe {
            let mut now: SYSTEMTIME = std::mem::zeroed();
            GetLocalTime(&mut now);
            now
        };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        let since_1970 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{}", since_1970.as_secs())
    }
}
