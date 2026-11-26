//! The questions the diagnostic puts to Windows and to the install folder.
//!
//! Every one of them only reads, except the two in [`mod@write`], which say so. None of them
//! needs an administrator's rights, and a question Windows refuses to answer comes back as
//! the error number it refused with: the diagnostic goes on without that one fact.

pub mod game;
pub mod programs;
pub mod read;
pub mod registry;
pub mod system;
pub mod volume;
pub mod write;

use crate::facts::Unknown;

/// Why the last call into Windows failed.
pub(crate) fn last_error() -> Unknown {
    // SAFETY: the call takes no arguments and reads a number kept for this thread.
    Unknown::Windows(unsafe { windows_sys::Win32::Foundation::GetLastError() })
}

/// Why a file operation failed.
pub(crate) fn io_error(err: &std::io::Error) -> Unknown {
    err.raw_os_error().map_or(Unknown::NoAnswer, |code| Unknown::Windows(code as u32))
}
