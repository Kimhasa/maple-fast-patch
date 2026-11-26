//! How much a run has to build, and whether it changes the install folder at all.

use std::collections::BTreeSet;
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
    let (in_place, built) = told_by_sizes(plan, dirs, ours);
    let still_to_build = |entry: &&plan::Entry| in_place.builds(entry) && !built.contains(&entry.name);
    plan.entries.iter().filter(still_to_build).map(plan::Entry::new_len).sum()
}

/// What the sizes of files alone say of how far a patch has come, before any file has been
/// read: the entries whose place in the install folder holds a file of the size of the new
/// one, and may be that file; and those with a file of that size among the staged files,
/// when those are this patch's (`ours`).
///
/// An entry with both is counted as staged and not as in place. A new file is often as long
/// as the one it replaces, so the file in place is as likely the old one, and the staged
/// file is then what a run puts there. The immediate way gets the room of the old file back
/// when it does, and what is certain to be needed must not leave that out.
pub(crate) fn told_by_sizes(plan: &Plan, dirs: &Dirs, ours: bool) -> (Checked, BTreeSet<String>) {
    let staged_dir = dirs.staged();
    let holds = |path: PathBuf, len: u64| fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == len);
    let (mut in_place, mut built) = (Checked::default(), BTreeSet::new());
    for entry in plan.entries.iter().filter(|entry| Checked::default().builds(entry)) {
        let Ok(relative) = plan::relative_path(&entry.name) else { continue };
        let len = entry.new_len();
        if ours && holds(staged_dir.join(&relative), len) {
            built.insert(entry.name.clone());
        } else if holds(dirs.install.join(&relative), len) {
            in_place.up_to_date.insert(entry.name.clone());
        }
    }
    (in_place, built)
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
            reach: BTreeMap::new(),
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

        // A file in place that is as long as the new one may be the new one. With a staged
        // file of that length beside it, it is taken for the old one, which the staged file
        // is there to replace: placing it gives room back, and that has to be counted.
        let same_size = r"Data\SameSize.wz".to_owned();
        let (in_place, built) = told_by_sizes(&plan, &dirs, true);
        assert!(in_place.up_to_date.contains(&same_size) && !built.contains(&same_size));
        fs::write(dirs.staged().join("Data/SameSize.wz"), b"xyz").unwrap();
        let (in_place, built) = told_by_sizes(&plan, &dirs, true);
        assert!(built.contains(&same_size) && !in_place.up_to_date.contains(&same_size));
        // Staged files of another patch tell nothing.
        let (in_place, built) = told_by_sizes(&plan, &dirs, false);
        assert!(in_place.up_to_date.contains(&same_size) && built.is_empty());
        // Either way it is not among the bytes that are certain to be built.
        assert_eq!(bytes_certain_to_build(&plan, &dirs, true), 20 + 7);
    }
}
