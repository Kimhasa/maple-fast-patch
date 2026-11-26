//! Files that another program has open at the moment.
//!
//! Virus scanners, search indexing and backup tools open a file briefly right after it is
//! written or moved. Giving up at once would turn that moment into a failed patch, so a file
//! operation that such a program can be in the way of is tried again for a short while.

use std::io;
use std::time::Duration;

/// How often, and how far apart, a file operation is tried again while another program has
/// the file open: about two seconds in all.
const TRIES: u32 = 10;
const WAIT: Duration = Duration::from_millis(200);

/// Runs a file operation, trying again for a moment when another program is in the way.
pub(crate) fn when_free<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut tries = 0;
    loop {
        match operation() {
            Err(err) if tries < TRIES && (held(&err) || denied(&err)) => {
                tries += 1;
                std::thread::sleep(WAIT);
            }
            other => return other,
        }
    }
}

/// Whether a file operation failed because another program holds the file open, or a part
/// of it locked, in a way that does not allow the operation.
#[cfg(windows)]
pub(crate) fn held(err: &io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
    err.raw_os_error().is_some_and(|code| [ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION].contains(&(code as u32)))
}

#[cfg(not(windows))]
pub(crate) fn held(_err: &io::Error) -> bool {
    false
}

/// Whether Windows denied access, which is one of the answers it gives while another program
/// is busy with a file.
#[cfg(windows)]
fn denied(err: &io::Error) -> bool {
    err.raw_os_error().is_some_and(|code| code as u32 == windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED)
}

#[cfg(not(windows))]
fn denied(_err: &io::Error) -> bool {
    false
}
