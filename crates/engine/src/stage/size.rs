//! How much a run has to build, and whether it changes the install folder at all.

use std::fs;
use std::path::{Path, PathBuf};

use super::originals::Checked;
use super::work::Dirs;
use crate::plan::{self, Action, Plan};

/// Bytes of new files that still have to be written to the work folder: everything the patch
/// builds, minus files that are already the new version and files an earlier run finished.
pub fn bytes_to_build(plan: &Plan, checked: &Checked, dirs: &Dirs) -> u64 {
    let staged_dir = dirs.staged();
    plan.entries
        .iter()
        .filter(|entry| checked.builds(entry))
        .filter_map(|entry| {
            let len = entry.new_len();
            let staged = staged_dir.join(plan::relative_path(&entry.name).ok()?);
            let built = fs::metadata(staged).is_ok_and(|meta| meta.is_file() && meta.len() == len);
            (!built).then_some(len)
        })
        .sum()
}

/// Bytes of new files that are certain to be written, told from the sizes of files alone,
/// before any file has been read: every new file whose place in the install folder is empty
/// or holds a file of another size. A file of the right size may be the new version already
/// and is left out. So is a file that waits among the staged files, when those are this
/// patch's (`ours`). Never more than what [`bytes_to_build`] finds once the files are read.
pub(crate) fn bytes_certain_to_build(plan: &Plan, dirs: &Dirs, ours: bool) -> u64 {
    let staged_dir = dirs.staged();
    let nothing_checked = Checked::default();
    let holds = |path: PathBuf, len: u64| fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == len);
    plan.entries
        .iter()
        .filter(|entry| nothing_checked.builds(entry))
        .filter_map(|entry| {
            let len = entry.new_len();
            let relative = plan::relative_path(&entry.name).ok()?;
            let in_place = holds(dirs.install.join(&relative), len);
            let built = ours && holds(staged_dir.join(&relative), len);
            (!in_place && !built).then_some(len)
        })
        .sum()
}

/// Whether applying this plan would change the install folder at all: a file that is not
/// yet the new version, or a file the patch removes that is still there. Staged leftovers do
/// not count; this is about the install folder only.
pub fn changes_install(plan: &Plan, checked: &Checked, install: &Path) -> bool {
    plan.entries.iter().any(|entry| {
        checked.builds(entry)
            || (entry.action == Action::Delete
                && plan::relative_path(&entry.name).is_ok_and(|relative| deletes(&entry.name, &install.join(relative))))
    })
}

/// Whether a delete entry named `name` removes what stands at `target`. A name that ends in
/// a separator removes a folder and any other name a file, never the other kind: a patch
/// that means to remove one file must not take a whole folder of that name with it.
pub(crate) fn deletes(name: &str, target: &Path) -> bool {
    if plan::names_folder(name) { target.is_dir() } else { target.is_file() }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::plan::Layout;

    #[test]
    fn bytes_certain_to_be_built_are_told_from_sizes_alone() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::new(dir.path());
        fs::create_dir_all(dir.path().join("Data")).unwrap();
        let entry = |name: &str, action: Action| plan::Entry { name: name.to_owned(), action };
        let plan = Plan {
            layout: Layout::Table,
            originals: BTreeMap::new(),
            entries: vec![
                entry(r"Data\Missing.wz", Action::Create { len: 100, crc: 0 }),
                entry(r"Data\OtherSize.wz", Action::Create { len: 20, crc: 0 }),
                entry(r"Data\SameSize.wz", Action::Create { len: 3, crc: 0 }),
                entry(r"Data\Empty.wz", Action::Create { len: 0, crc: 0 }),
                entry(
                    r"Data\Rebuilt.wz",
                    Action::Rebuild { old_crc: None, new_crc: 0, new_len: 7, sources: BTreeSet::new() },
                ),
                entry(r"Data\Gone.wz", Action::Delete),
                entry(r"Data\Folder", Action::CreateDir),
            ],
            contents_len: 0,
        };
        fs::write(dir.path().join("Data/OtherSize.wz"), b"twenty-one bytes long").unwrap();
        fs::write(dir.path().join("Data/SameSize.wz"), b"abc").unwrap();
        fs::write(dir.path().join("Data/Rebuilt.wz"), b"old").unwrap();
        fs::write(dir.path().join("Data/Gone.wz"), b"to be deleted").unwrap();
        assert_eq!(bytes_certain_to_build(&plan, &dirs, false), 100 + 20 + 7);

        // A staged file counts as done when it is this patch's and of the right size.
        fs::create_dir_all(dirs.staged().join("Data")).unwrap();
        fs::write(dirs.staged().join("Data/Missing.wz"), vec![0u8; 100]).unwrap();
        fs::write(dirs.staged().join("Data/Rebuilt.wz"), b"short").unwrap();
        assert_eq!(bytes_certain_to_build(&plan, &dirs, true), 20 + 7);
        assert_eq!(bytes_certain_to_build(&plan, &dirs, false), 100 + 20 + 7);
    }
}
