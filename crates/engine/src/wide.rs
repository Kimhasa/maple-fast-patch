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
