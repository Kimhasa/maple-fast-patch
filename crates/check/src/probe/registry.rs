//! Values read from the registry. Nothing is ever written there.

use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY,
    RRF_SUBKEY_WOW6464KEY, RegGetValueW,
};

use crate::wide::{from_wide, to_wide};

/// Which part of the registry a key is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// What holds for the whole PC, as a 64-bit program sees it.
    Machine,
    /// The same as a 32-bit program sees it. Nexon's installers are 32-bit, so their keys
    /// are there.
    Machine32,
    /// What holds for the user who runs this program.
    User,
}

impl Root {
    fn key_and_view(self) -> (HKEY, u32) {
        match self {
            Root::Machine => (HKEY_LOCAL_MACHINE, RRF_SUBKEY_WOW6464KEY),
            Root::Machine32 => (HKEY_LOCAL_MACHINE, RRF_SUBKEY_WOW6432KEY),
            Root::User => (HKEY_CURRENT_USER, 0),
        }
    }
}

/// A text value, without spaces around it. `None` when it is not there or is no text.
pub fn text(root: Root, key: &str, value: &str) -> Option<String> {
    let (root, view) = root.key_and_view();
    let (key, value) = (to_wide(key), to_wide(value));
    let mut units = [0u16; 1024];
    let mut bytes = std::mem::size_of_val(&units) as u32;
    // SAFETY: both names end in a zero, `units` has room for `bytes` bytes, which the call
    // fills with text and its ending zero, and the kind of value is not asked back.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | view,
            std::ptr::null_mut(),
            units.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != 0 {
        return None;
    }
    Some(from_wide(units.get(..bytes as usize / 2)?).trim().to_owned())
}

/// A 32-bit number. `None` when it is not there or is no such number.
pub fn number(root: Root, key: &str, value: &str) -> Option<u32> {
    let (root, view) = root.key_and_view();
    let (key, value) = (to_wide(key), to_wide(value));
    let mut number = 0u32;
    let mut bytes = std::mem::size_of::<u32>() as u32;
    // SAFETY: both names end in a zero, and the four bytes of the number are written to a
    // variable that outlives the call.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD | view,
            std::ptr::null_mut(),
            (&raw mut number).cast(),
            &mut bytes,
        )
    };
    (status == 0).then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_every_windows_has_are_read_and_others_are_not_there() {
        const WINDOWS: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let build = text(Root::Machine, WINDOWS, "CurrentBuildNumber").expect("Windows did not say its build");
        assert!(build.parse::<u32>().is_ok_and(|build| build >= 10_000), "{build}");
        assert!(number(Root::Machine, WINDOWS, "CurrentMajorVersionNumber").is_some_and(|major| major >= 10));
        // A number is no text, a text is no number, and a value that is not there is neither.
        assert_eq!(text(Root::Machine, WINDOWS, "CurrentMajorVersionNumber"), None);
        assert_eq!(number(Root::Machine, WINDOWS, "CurrentBuildNumber"), None);
        assert_eq!(text(Root::User, r"Software\MapleFastPatch\NoSuchKey", "nothing"), None);
        assert_eq!(number(Root::Machine32, r"SOFTWARE\MapleFastPatch\NoSuchKey", "nothing"), None);
    }
}
