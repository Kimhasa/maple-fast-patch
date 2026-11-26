//! Finds the folder a client is installed in.
//!
//! On a home PC the registry says where it is. On a PC room seat the registry may say
//! nothing and the game may be on any drive under any name, so the drives are looked
//! through as well: a few levels deep, for a folder that holds the game's program and its
//! `Data` folder. Only names of folders are read on the way, and the folders that hold the
//! users' own files are not entered.
//!
//! Where to look is passed in as [`Places`], so that all of this can be tried on made-up
//! folders. [`Places::of_this_pc`] is the one function that names real ones.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use mfp_engine::preflight::DATA_DIR_NAME;
use mfp_kms::{Client, Server};

use crate::facts::How;

/// How many folders deep a drive is looked through: `D:\Games\Nexon\Maple` is three deep.
const DEPTH: usize = 3;
/// The most folders that are opened on one drive.
const MOST_FOLDERS: usize = 4000;
/// Folders that are not entered: Windows' own, and those with the users' own files.
const NOT_ENTERED: [&str; 12] = [
    "windows",
    "users",
    "documents and settings",
    "programdata",
    "system volume information",
    "recovery",
    "perflogs",
    "msocache",
    "config.msi",
    "windowsapps",
    "windows.old",
    "onedrivetemp",
];
/// Words in a folder's name that make it worth looking into before the others.
const LIKELY: [&str; 4] = ["maple", "nexon", "game", "online"];

/// Where a client is looked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Places {
    /// The folder the registry names, if it names one.
    pub recorded: Option<PathBuf>,
    /// The folder Nexon's installer uses unless told otherwise.
    pub usual: Option<PathBuf>,
    /// The folders to look through, which are the roots of the PC's drives.
    pub roots: Vec<PathBuf>,
}

/// Whether `dir` is an install folder of `client`: it holds that client's game program and
/// a `Data` folder. The other client's folder is none: its program has another name.
pub fn is_install(dir: &Path, client: &Client) -> bool {
    dir.join(client.game_program).is_file() && dir.join(DATA_DIR_NAME).is_dir()
}

/// Finds the install folder of `client` in `places`: where the registry says, then where
/// the installer puts it, then on the drives.
pub fn find(client: &Client, places: &Places) -> Option<(How, PathBuf)> {
    let is_one = |dir: &&PathBuf| is_install(dir, client);
    if let Some(recorded) = places.recorded.as_ref().filter(is_one) {
        return Some((How::Registry, recorded.clone()));
    }
    if let Some(usual) = places.usual.as_ref().filter(is_one) {
        return Some((How::UsualPlace, usual.clone()));
    }
    places.roots.iter().find_map(|root| look_through(root, client)).map(|found| (How::DriveScan, found))
}

/// Looks through `root` for an install folder of `client`, the nearest to the root first.
pub fn look_through(root: &Path, client: &Client) -> Option<PathBuf> {
    let mut opened = 0;
    let mut todo = VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = todo.pop_front() {
        if is_install(&dir, client) {
            return Some(dir);
        }
        if depth >= DEPTH || opened >= MOST_FOLDERS {
            continue;
        }
        opened += 1;
        let Ok(children) = std::fs::read_dir(&dir) else { continue };
        let mut inside: Vec<(bool, String, PathBuf)> = children
            .flatten()
            .filter(|child| child.file_type().is_ok_and(|kind| kind.is_dir() && !kind.is_symlink()))
            .filter_map(|child| {
                let name = child.file_name().to_string_lossy().to_lowercase();
                let entered = !name.starts_with('$') && !NOT_ENTERED.contains(&name.as_str());
                entered.then(|| (!LIKELY.iter().any(|word| name.contains(word)), name, child.path()))
            })
            .collect();
        // Likely names first, and the same order every time.
        inside.sort();
        todo.extend(inside.into_iter().map(|(_, _, path)| (path, depth + 1)));
    }
    None
}

/// The folder a user named, if it is an install folder of `client` or has one close below
/// it: the folder that holds `Maple` is as good an answer as `Maple` itself.
pub fn from_typed(typed: &str, client: &Client) -> Option<PathBuf> {
    let typed = typed.trim().trim_matches('"');
    if typed.is_empty() {
        return None;
    }
    look_through(Path::new(typed), client)
}

/// The registry key Nexon's installer writes the install folder to, under `HKLM\SOFTWARE`
/// in the 32-bit view, and the name of the value. Known for the live client only. (The
/// same key is in `mfp-kms`, which does not hand it out.)
#[cfg(windows)]
const RECORDED: (&str, &str) = (r"SOFTWARE\Wizet\Maple", "RootPath");

/// What the registry says the install folder of the client of `server` is. The folder is
/// not looked at: this is the text and nothing more.
#[cfg(windows)]
pub fn recorded(server: Server) -> Option<String> {
    use crate::probe::registry::{self, Root};
    match server {
        Server::Live => registry::text(Root::Machine32, RECORDED.0, RECORDED.1).filter(|dir| !dir.is_empty()),
        Server::Test => None,
    }
}

/// The folder Nexon's installer puts the client of `server` in unless told otherwise. (As
/// in `mfp-kms`, which does not hand it out either.)
fn usual(server: Server) -> &'static str {
    match server {
        Server::Live => r"C:\Nexon\Maple",
        Server::Test => r"C:\Nexon\MapleT",
    }
}

#[cfg(windows)]
impl Places {
    /// Where the client of `server` is looked for on this PC: the folder the registry
    /// names, the usual one, and every drive that could hold a game.
    pub fn of_this_pc(server: Server) -> Places {
        use crate::facts::DriveKind;
        let roots = crate::probe::volume::letters()
            .into_iter()
            .filter(|(_, kind)| {
                matches!(kind, DriveKind::Fixed | DriveKind::Removable | DriveKind::Remote | DriveKind::RamDisk)
            })
            .map(|(letter, _)| PathBuf::from(format!("{letter}\\")))
            .collect();
        Places { recorded: recorded(server).map(PathBuf::from), usual: Some(PathBuf::from(usual(server))), roots }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &Client = &mfp_kms::LIVE;
    const TEST: &Client = &mfp_kms::TEST;

    /// Makes `dir` a folder that passes as an install folder of `client`.
    fn install(dir: &Path, client: &Client) -> PathBuf {
        std::fs::create_dir_all(dir.join("Data")).unwrap();
        std::fs::write(dir.join(client.game_program), b"").unwrap();
        dir.to_path_buf()
    }

    #[test]
    fn registry_comes_first_then_the_usual_folder_then_the_drives() {
        let pc = tempfile::tempdir().unwrap();
        let recorded = install(&pc.path().join("c").join("Custom").join("Maple"), LIVE);
        let usual = install(&pc.path().join("c").join("Nexon").join("Maple"), LIVE);
        let on_drive = install(&pc.path().join("d").join("Games").join("Maple"), LIVE);
        let roots = vec![pc.path().join("c"), pc.path().join("d")];

        let all = Places { recorded: Some(recorded.clone()), usual: Some(usual.clone()), roots: roots.clone() };
        assert_eq!(find(LIVE, &all), Some((How::Registry, recorded.clone())));
        // The registry names a folder that is gone, or names nothing.
        let stale = Places { recorded: Some(pc.path().join("gone")), ..all.clone() };
        assert_eq!(find(LIVE, &stale), Some((How::UsualPlace, usual.clone())));
        let silent = Places { recorded: None, ..all.clone() };
        assert_eq!(find(LIVE, &silent), Some((How::UsualPlace, usual)));
        // Neither: the drives are looked through, in their order.
        let only_drives = Places { recorded: None, usual: None, roots: vec![pc.path().join("d"), pc.path().join("c")] };
        assert_eq!(find(LIVE, &only_drives), Some((How::DriveScan, on_drive)));
        // Nothing of the other client is taken for this one's.
        assert_eq!(find(TEST, &all), None);
        assert_eq!(find(LIVE, &Places::default()), None);
    }

    #[test]
    fn drive_is_looked_through_three_folders_deep_and_no_deeper() {
        let drive = tempfile::tempdir().unwrap();
        let deep = install(&drive.path().join("Games").join("Nexon").join("Maple"), LIVE);
        assert_eq!(look_through(drive.path(), LIVE), Some(deep));

        let too_deep = tempfile::tempdir().unwrap();
        install(&too_deep.path().join("a").join("b").join("c").join("Maple"), LIVE);
        assert_eq!(look_through(too_deep.path(), LIVE), None);
        // A drive that is the install folder itself, as on a stick with nothing else on it.
        let stick = tempfile::tempdir().unwrap();
        install(stick.path(), LIVE);
        assert_eq!(look_through(stick.path(), LIVE), Some(stick.path().to_path_buf()));
    }

    #[test]
    fn nearest_install_is_found_first_and_likely_names_before_others() {
        let drive = tempfile::tempdir().unwrap();
        let near = install(&drive.path().join("zz-copy"), LIVE);
        install(&drive.path().join("Games").join("Maple"), LIVE);
        assert_eq!(look_through(drive.path(), LIVE), Some(near));

        // On one level, `Maple` is looked into before `Archive`.
        let drive = tempfile::tempdir().unwrap();
        install(&drive.path().join("Archive").join("old"), LIVE);
        let likely = install(&drive.path().join("Nexon").join("Maple"), LIVE);
        assert_eq!(look_through(drive.path(), LIVE), Some(likely));
    }

    #[test]
    fn folders_of_windows_and_of_users_are_not_entered() {
        let drive = tempfile::tempdir().unwrap();
        for hidden in ["Users", "Windows", "$Recycle.Bin", "ProgramData"] {
            install(&drive.path().join(hidden).join("Maple"), LIVE);
        }
        assert_eq!(look_through(drive.path(), LIVE), None);
        // The two clients are told apart by their programs.
        let test_world = install(&drive.path().join("Nexon").join("MapleT"), TEST);
        assert_eq!(look_through(drive.path(), LIVE), None);
        assert_eq!(look_through(drive.path(), TEST), Some(test_world));
    }

    #[test]
    fn folder_without_the_program_or_without_data_is_no_install() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_install(dir.path(), LIVE));
        std::fs::write(dir.path().join("MapleStory.exe"), b"").unwrap();
        assert!(!is_install(dir.path(), LIVE));
        std::fs::create_dir(dir.path().join("Data")).unwrap();
        assert!(is_install(dir.path(), LIVE) && !is_install(dir.path(), TEST));
    }

    #[test]
    fn folder_a_user_names_may_be_the_install_or_the_folder_above_it() {
        let drive = tempfile::tempdir().unwrap();
        let folder = install(&drive.path().join("Nexon").join("Maple"), LIVE);
        let quoted = format!("  \"{}\" ", folder.display());
        assert_eq!(from_typed(&quoted, LIVE), Some(folder.clone()));
        assert_eq!(from_typed(&drive.path().join("Nexon").display().to_string(), LIVE), Some(folder));
        assert_eq!(from_typed("", LIVE), None);
        assert_eq!(from_typed(&drive.path().join("gone").display().to_string(), LIVE), None);
    }

    #[test]
    fn usual_folders_are_those_of_the_two_clients() {
        assert_eq!((usual(Server::Live), usual(Server::Test)), (r"C:\Nexon\Maple", r"C:\Nexon\MapleT"));
    }
}
