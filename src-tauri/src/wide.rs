//! Text as Windows takes it: 16-bit units with a zero at the end.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// `text` for a call into Windows: its units and the zero that ends them.
pub(crate) fn to_wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain([0]).collect()
}
