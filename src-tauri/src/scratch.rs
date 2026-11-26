//! A folder of its own for one test.

use std::path::PathBuf;

/// A folder that is made empty for a test and gone again when the test is.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    /// `name` tells the folders of the tests apart: they run side by side.
    pub(crate) fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("mfp-app-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
