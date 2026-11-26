//! Builds the new files in the work folder. The install folder is only read here.
//!
//! `work` knows the work folder, `originals` checks the files that new ones are built from,
//! `build` builds them and `size` tells how much there is to build.

mod build;
mod originals;
mod size;
mod work;

pub use build::{stage, stage_with};
pub(crate) use build::{try_build, verify_staged};
pub(crate) use originals::originals_to_check;
pub use originals::{
    Checked, Fingerprint, check_originals, check_originals_except, check_originals_with, files_present, fingerprint,
    fingerprint_all,
};
pub(crate) use size::{bytes_certain_to_build, deletes};
pub use size::{bytes_to_build, changes_install};
pub use work::{Dirs, half_patched};
pub(crate) use work::{discard_foreign_work, pin, remove_work, staged_is_ours};

const BUFFER: usize = 1 << 20;
