//! Text as Windows takes it and gives it back: 16-bit units with a zero at the end.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// `text` for a call into Windows: its units and the zero that ends them.
pub(crate) fn to_wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain([0]).collect()
}

/// What Windows wrote into `units`, up to the zero that ends it.
pub(crate) fn from_wide(units: &[u16]) -> String {
    let len = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..len])
}

/// The text Windows points to with `start`, up to the zero that ends it and no further
/// than `most` units.
///
/// # Safety
/// `start` must be null or point to units that are readable up to a zero or to `most`.
pub(crate) unsafe fn from_wide_ptr(start: *const u16, most: usize) -> String {
    if start.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller vouches for every unit up to the zero, and no more are read.
    while len < most && unsafe { *start.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` units were just read one by one.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(start, len) })
}
