//! The work folder: where a run keeps what, which patch file its contents were built from,
//! and throwing away what is of no use any more.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::busy::when_free;
use crate::container::PatchFile;
use crate::error::{Error, Result};
use crate::preflight::{DOWNLOAD_DIR_NAME, LOCK_FILE_NAME, Problem, WORK_DIR_NAME};

/// The folder in the work folder that holds what an apply has moved aside. See [`Dirs::aside`].
const ASIDE_DIR_NAME: &str = "aside";

/// Files being written are named `building-<entry number>.tmp` until they are complete.
const BUILDING_PREFIX: &str = "building";
const BUILDING_SUFFIX: &str = ".tmp";

/// Where a patch run keeps its files. Everything sits inside the install folder so that
/// moving a file into place is a rename on the same drive, which needs no extra space.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub install: PathBuf,
}

impl Dirs {
    pub fn new(install: impl Into<PathBuf>) -> Dirs {
        Dirs { install: install.into() }
    }
    pub fn work(&self) -> PathBuf {
        self.install.join(WORK_DIR_NAME)
    }
    /// Finished new files, waiting to be moved into place.
    pub fn staged(&self) -> PathBuf {
        self.work().join("new")
    }
    /// What an apply has moved out of the install folder while new files take its place,
    /// each under the name it had there: `aside\<step>\<name>`, the steps numbered in the
    /// order they were made. What lies here says by itself where it belongs, so an apply
    /// that was cut off can be undone from it alone when its journal cannot be read.
    pub fn aside(&self) -> PathBuf {
        self.work().join(ASIDE_DIR_NAME)
    }
    /// What an attempt to put the folder back without its journal leaves behind when it
    /// finds that it cannot be done: the reason. It spares every later look the reading
    /// that it took to find out.
    pub(crate) fn cannot_restore(&self) -> PathBuf {
        self.work().join("cannot-restore")
    }
    /// Where the app keeps the patch files it downloads: on the drive the game is on, which
    /// has to have the room for a patch anyway. They are not work of a run. They stay when
    /// the work of another patch is thrown away and go when the work folder goes.
    pub fn download(&self) -> PathBuf {
        self.work().join(DOWNLOAD_DIR_NAME)
    }
    /// The file entry number `index` is written to until it is complete. It sits outside
    /// `new`, so no file name a patch can contain collides with it. Each entry has its own:
    /// one file is still being synced while the next is written.
    pub(super) fn building(&self, index: usize) -> PathBuf {
        self.work().join(format!("{BUILDING_PREFIX}-{index}{BUILDING_SUFFIX}"))
    }
    /// Says which patch file the staged files were built from.
    fn patch_id(&self) -> PathBuf {
        self.work().join("patch.id")
    }
    /// The marker of an install folder that is partly the old version and partly the new
    /// one. It holds what `patch.id` holds. See `pin`.
    pub fn half_patched(&self) -> PathBuf {
        self.work().join("half-patched")
    }
    /// The record of an apply in progress. It exists only while an apply is unfinished.
    pub fn journal(&self) -> PathBuf {
        self.work().join("applying.log")
    }
    pub(crate) fn committed(&self) -> PathBuf {
        self.work().join("committed")
    }
    pub(crate) fn lock(&self) -> PathBuf {
        self.install.join(LOCK_FILE_NAME)
    }
}

/// Throws away files staged from a different patch file: they must never be mistaken for
/// this patch's output, and the space they take is about to be needed.
///
/// Files of this patch that a cut-off run was still writing go as well. They are no use to
/// anyone, and the caller is about to measure free space.
///
/// Downloaded patch files stay: one of them may be the patch this run is using. And nothing
/// at all is thrown away from an install folder that another patch has left half patched.
pub(crate) fn discard_foreign_work(patch: &PatchFile, dirs: &Dirs) -> Result<()> {
    let work = dirs.work();
    if !work.exists() {
        return Ok(());
    }
    if staged_is_ours(patch, dirs) {
        remove_unfinished(dirs);
        return Ok(());
    }
    // What was built for a half-patched install exists nowhere else, and some of the
    // originals it was built from are gone.
    if half_patched(dirs).is_some() {
        return Err(Error::Blocked(vec![Problem::LeftoverWork { dir: work }]));
    }
    clear_work(&work)
}

/// Whether what is in the work folder was built from this patch file.
///
/// For an install folder that is half patched the marker says so. It is written with more
/// care than `patch.id`, which a power cut can leave empty.
pub(crate) fn staged_is_ours(patch: &PatchFile, dirs: &Dirs) -> bool {
    let id = patch.id();
    match half_patched(dirs) {
        Some(pinned) => pinned == id,
        None => fs::read_to_string(dirs.patch_id()).is_ok_and(|saved| saved == id),
    }
}

/// The id of the patch that the work folder was made for: the one a half-patched install
/// folder is pinned to, or else the one the staged files were built from. `None` where
/// neither is on record.
pub(crate) fn made_for(dirs: &Dirs) -> Option<String> {
    let known = half_patched(dirs).or_else(|| fs::read_to_string(dirs.patch_id()).ok());
    known.filter(|id| !id.is_empty())
}

/// The id of the patch that has left the install folder half patched, if one has: new files
/// were moved into place while others were still to be built (see `pin`), and that run
/// has not finished. Only that patch can finish it.
///
/// A marker that is there but cannot be read is answered with an id that no patch has, so
/// that nothing is done to such an install until the marker can be read.
pub fn half_patched(dirs: &Dirs) -> Option<String> {
    match when_free(|| fs::read_to_string(dirs.half_patched())) {
        Ok(id) => Some(id),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => None,
        Err(_) => Some(String::new()),
    }
}

/// Marks the install folder as half patched by this patch file. This comes before the first
/// new file is moved into place ahead of the others. From then on the folder is neither the
/// old version nor the new one, the staged files are the only copy of what they hold, and
/// only this patch may go on.
///
/// The marker is written under another name, synced and then renamed, so it is either not
/// there or whole. `patch.id` is put right every time this is called: that file is written
/// without a sync, and a power cut can have emptied it.
pub(crate) fn pin(patch: &PatchFile, dirs: &Dirs) -> Result<()> {
    let id = patch.id();
    let pinned = half_patched(dirs);
    if pinned.as_ref().is_some_and(|pinned| *pinned != id) {
        return Err(Error::Blocked(vec![Problem::LeftoverWork { dir: dirs.work() }]));
    }
    let id_path = dirs.patch_id();
    if !fs::read_to_string(&id_path).is_ok_and(|saved| saved == id) {
        fs::write(&id_path, &id).map_err(|e| Error::file("write", &id_path, e))?;
    }
    if pinned.is_some() {
        return Ok(());
    }
    let marker = dirs.half_patched();
    let fresh = dirs.work().join("half-patched.tmp");
    File::create(&fresh)
        .and_then(|mut file| {
            file.write_all(id.as_bytes())?;
            file.sync_all()
        })
        .map_err(|e| Error::file("write", &fresh, e))?;
    when_free(|| fs::rename(&fresh, &marker)).map_err(|e| Error::file("rename", &fresh, e))
}

/// Empties the work folder of everything but the downloaded patch files. A work folder that
/// is left with nothing in it goes as well.
fn clear_work(work: &Path) -> Result<()> {
    let Ok(children) = fs::read_dir(work) else {
        // Not a folder the engine made: something else of that name.
        return remove_work(work);
    };
    // What a finished apply had moved aside goes before anything else, see `remove_work`.
    remove(&work.join(ASIDE_DIR_NAME))?;
    for child in children.flatten() {
        if child.file_name().eq_ignore_ascii_case(DOWNLOAD_DIR_NAME) {
            continue;
        }
        remove(&child.path())?;
    }
    // Fails while the downloads are in it, which is as it should be.
    let _ = fs::remove_dir(work);
    Ok(())
}

/// Deletes the files an earlier run was writing when it was cut off. A file only counts as
/// built once it has moved to the staged files, so these are never complete. Only one run
/// works on an install folder at a time, which makes any such file a leftover.
pub(crate) fn remove_unfinished(dirs: &Dirs) {
    let Ok(children) = fs::read_dir(dirs.work()) else { return };
    for child in children.flatten() {
        let name = child.file_name();
        let name = name.to_string_lossy();
        let unfinished = name.starts_with(BUILDING_PREFIX) && name.ends_with(BUILDING_SUFFIX);
        if unfinished && child.file_type().is_ok_and(|kind| kind.is_file()) {
            // One that cannot be removed is not a reason to stop: it is written over when
            // its entry is built, and goes with the work folder otherwise.
            let _ = when_free(|| fs::remove_file(child.path()));
        }
    }
}

/// Removes the work folder, whether it is a folder or something else of that name. A link
/// of that name is removed as the link it is, and what it leads to is left alone.
///
/// What an apply had moved aside goes first. If the removing stops halfway, what is left
/// must not be that with the mark of a finished apply gone from beside it: it would read as
/// an apply that was cut off, and be put back over the new files.
pub(crate) fn remove_work(work: &Path) -> Result<()> {
    if fs::symlink_metadata(work).is_ok_and(|meta| meta.is_dir()) {
        remove(&work.join(ASIDE_DIR_NAME))?;
    }
    remove(work)
}

/// Removes whatever is at `path`: a file, a folder with all that is in it, or a link, which
/// goes as the link it is. Nothing being there is no failure.
pub(crate) fn remove(path: &Path) -> Result<()> {
    let removed = match fs::symlink_metadata(path) {
        // A link to a folder goes the way a folder does, and one to a file the way a file does.
        Ok(meta) if meta.file_type().is_symlink() => fs::remove_dir(path).or_else(|_| fs::remove_file(path)),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        _ => fs::remove_file(path),
    };
    match removed {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(Error::file("remove", path, e)),
        _ => Ok(()),
    }
}

/// Makes the work folder ready for this patch file.
pub(super) fn prepare_work(patch: &PatchFile, dirs: &Dirs) -> Result<()> {
    let id = patch.id();
    let id_path = dirs.patch_id();
    // Both have to say so: in a half-patched install folder the marker has the last word,
    // and a `patch.id` that a power cut emptied is written again below.
    if fs::read_to_string(&id_path).is_ok_and(|saved| saved == id) && staged_is_ours(patch, dirs) {
        return Ok(());
    }
    discard_foreign_work(patch, dirs)?;
    let staged = dirs.staged();
    fs::create_dir_all(&staged).map_err(|e| Error::file("create", &staged, e))?;
    fs::write(&id_path, id).map_err(|e| Error::file("write", &id_path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_files_are_removed_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::new(dir.path());
        fs::create_dir_all(dirs.staged().join("Data")).unwrap();
        let work = dirs.work();
        for name in ["building.tmp", "building-0.tmp", "building-12.tmp"] {
            fs::write(work.join(name), b"half").unwrap();
        }
        assert_eq!(dirs.building(12), work.join("building-12.tmp"));
        for name in [
            "patch.id",
            "applying.log",
            "building-notes.txt",
            "rebuilding-1.tmp",
            "new/building-3.tmp",
            "new/Data/x.wz",
        ] {
            fs::write(work.join(name), b"kept").unwrap();
        }
        fs::create_dir(work.join("building-7.tmp")).unwrap();

        remove_unfinished(&dirs);
        let mut left: Vec<String> = fs::read_dir(&work)
            .unwrap()
            .map(|child| child.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["applying.log", "building-7.tmp", "building-notes.txt", "new", "patch.id", "rebuilding-1.tmp"]
        );
        assert!(work.join("new/building-3.tmp").exists() && work.join("new/Data/x.wz").exists());

        // Without a work folder there is nothing to do.
        remove_unfinished(&Dirs::new(dir.path().join("elsewhere")));
    }

    /// A patch file that is nothing but a header carrying `checksum`: enough to have an id.
    fn patch_with_id(dir: &Path, checksum: u32) -> PatchFile {
        let path = dir.join(format!("{checksum:08x}.patch"));
        let mut header = b"WzPatch\x1A".to_vec();
        header.extend(2u32.to_le_bytes());
        header.extend(checksum.to_le_bytes());
        fs::write(&path, header).unwrap();
        PatchFile::open(&path).unwrap()
    }

    /// Names of what lies in the work folder, sorted.
    fn in_work(dirs: &Dirs) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dirs.work())
            .unwrap()
            .map(|child| child.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn work_of_another_patch_goes_and_downloads_stay() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::new(dir.path().join("Maple"));
        let (ours, theirs) = (patch_with_id(dir.path(), 1), patch_with_id(dir.path(), 2));
        assert_ne!(ours.id(), theirs.id());
        let fill = |id: &str| {
            fs::create_dir_all(dirs.staged().join("Data")).unwrap();
            fs::create_dir_all(dirs.download()).unwrap();
            fs::write(dirs.patch_id(), id).unwrap();
            fs::write(dirs.staged().join("Data/x.wz"), b"built").unwrap();
            fs::write(dirs.work().join("building-3.tmp"), b"half").unwrap();
            fs::write(dirs.download().join("next.patch.part"), b"downloaded").unwrap();
        };

        // Work of this patch stays, but for the file that was still being written.
        fill(&ours.id());
        discard_foreign_work(&ours, &dirs).unwrap();
        assert_eq!(in_work(&dirs), ["download", "new", "patch.id"]);
        assert!(dirs.staged().join("Data/x.wz").exists());

        // Work of another patch goes. What was downloaded does not.
        fill(&theirs.id());
        discard_foreign_work(&ours, &dirs).unwrap();
        assert_eq!(in_work(&dirs), ["download"]);
        assert_eq!(fs::read(dirs.download().join("next.patch.part")).unwrap(), b"downloaded");

        // With nothing downloaded the folder goes too.
        fill(&theirs.id());
        fs::remove_dir_all(dirs.download()).unwrap();
        discard_foreign_work(&ours, &dirs).unwrap();
        assert!(!dirs.work().exists());
        discard_foreign_work(&ours, &dirs).unwrap();
    }

    #[test]
    fn half_patched_install_is_pinned_to_one_patch() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::new(dir.path().join("Maple"));
        let (ours, theirs) = (patch_with_id(dir.path(), 1), patch_with_id(dir.path(), 2));
        fs::create_dir_all(dirs.staged()).unwrap();
        fs::write(dirs.staged().join("x.wz"), b"built").unwrap();
        fs::write(dirs.patch_id(), ours.id()).unwrap();
        assert_eq!(half_patched(&dirs), None);
        assert!(staged_is_ours(&ours, &dirs) && !staged_is_ours(&theirs, &dirs));

        pin(&ours, &dirs).unwrap();
        assert_eq!(half_patched(&dirs), Some(ours.id()));
        // Pinning again changes nothing.
        pin(&ours, &dirs).unwrap();
        assert_eq!(in_work(&dirs), ["half-patched", "new", "patch.id"]);

        // The marker has the last word, whether `patch.id` lost what it held or says
        // something else.
        fs::write(dirs.patch_id(), b"").unwrap();
        assert!(staged_is_ours(&ours, &dirs));
        fs::write(dirs.patch_id(), theirs.id()).unwrap();
        assert!(staged_is_ours(&ours, &dirs) && !staged_is_ours(&theirs, &dirs));

        // Another patch can neither take the marker over nor throw the staged files away.
        for refused in [pin(&theirs, &dirs), discard_foreign_work(&theirs, &dirs)] {
            let expected = [Problem::LeftoverWork { dir: dirs.work() }];
            assert!(matches!(&refused, Err(Error::Blocked(problems)) if problems[..] == expected), "{refused:?}");
        }
        assert_eq!(fs::read(dirs.staged().join("x.wz")).unwrap(), b"built");
        assert_eq!(half_patched(&dirs), Some(ours.id()));

        // Pinning puts right a `patch.id` that does not say this patch, with the marker
        // there and without it.
        pin(&ours, &dirs).unwrap();
        assert_eq!(fs::read_to_string(dirs.patch_id()).unwrap(), ours.id());
        fs::write(dirs.patch_id(), b"").unwrap();
        fs::remove_file(dirs.half_patched()).unwrap();
        pin(&ours, &dirs).unwrap();
        assert_eq!(fs::read_to_string(dirs.patch_id()).unwrap(), ours.id());
        assert_eq!(half_patched(&dirs), Some(ours.id()));

        // A marker that is there and cannot be read belongs to no patch.
        fs::write(dirs.half_patched(), [0xFF, 0xFE, 0xFD]).unwrap();
        assert_eq!(half_patched(&dirs), Some(String::new()));
        assert!(!staged_is_ours(&ours, &dirs) && !staged_is_ours(&theirs, &dirs));
        assert!(pin(&ours, &dirs).is_err());
    }
}
