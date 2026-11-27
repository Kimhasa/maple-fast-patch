//! Facts about the Korean MapleStory clients and the services Nexon runs for them: where a
//! client is installed, where its patch files are published, how the game is started, and
//! (in [`notice`]) what Nexon says about maintenance.
//!
//! There are two clients, installed side by side: the live game and the test world. What is
//! known of each is in its [`Client`]. The live one is known from a PC that has it; the test
//! world's is put together from what Nexon publishes, and the parts nobody has checked on an
//! installed test client say so.

pub mod notice;

use std::path::{Path, PathBuf};

/// Which of Nexon's two services a client belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Server {
    /// The live game (본섭).
    Live,
    /// The test world (테섭).
    Test,
}

impl Server {
    pub const ALL: [Server; 2] = [Server::Live, Server::Test];

    /// A short name for records, logs and messages between the app and its page.
    pub fn key(self) -> &'static str {
        match self {
            Server::Live => "live",
            Server::Test => "test",
        }
    }

    pub fn from_key(key: &str) -> Option<Server> {
        Server::ALL.into_iter().find(|server| server.key() == key)
    }

    pub fn client(self) -> &'static Client {
        match self {
            Server::Live => &LIVE,
            Server::Test => &TEST,
        }
    }
}

/// One of the two clients, and what Nexon's side looks like for it.
#[derive(Debug)]
pub struct Client {
    pub server: Server,
    /// What the game is called.
    pub title: &'static str,
    /// The game's program, in the install folder. An install folder is told from the other
    /// client's by this name.
    pub game_program: &'static str,
    /// The game's own launcher, in the install folder.
    pub launcher: &'static str,
    /// The number Nexon's launcher knows the game by.
    pub game_code: u32,
    /// The official page that offers the installer.
    pub download_page: &'static str,
    /// Fallback when the `ngm://` handler is not registered: the game's page on nexon.com.
    pub game_page: &'static str,
    /// Where the patch files are published.
    patch_files: &'static str,
    /// What the numbers in the patch files' names are ahead of the version Nexon speaks of:
    /// the test world's 1.2.208 is 1208 on the server.
    numbers_ahead: u32,
    /// Where the installer puts the client unless told otherwise.
    usual_dir: &'static str,
    /// The registry key with the install folder, and the one with the version hint, under
    /// `HKLM\SOFTWARE` in the 32-bit view. `None` where the key is not known.
    install_key: Option<&'static str>,
    version_key: Option<&'static str>,
    /// A version this client is known to have had, to start looking for the newest from when
    /// the PC says nothing.
    known_version: Option<u32>,
    maintenance: &'static notice::Sources,
}

/// The live game.
pub static LIVE: Client = Client {
    server: Server::Live,
    title: "메이플스토리",
    game_program: "MapleStory.exe",
    launcher: "GameLauncher.exe",
    game_code: 589825,
    download_page: "https://maplestory.nexon.com/Common/PDS/Download",
    game_page: "https://maplestory.nexon.com/",
    patch_files: "https://maplestory.dn.nexoncdn.co.kr/Patch",
    numbers_ahead: 0,
    usual_dir: r"C:\Nexon\Maple",
    install_key: Some(r"SOFTWARE\Wizet\Maple"),
    version_key: Some(r"SOFTWARE\Wizet\MapleStory"),
    known_version: None,
    maintenance: &notice::LIVE,
};

/// The test world.
///
/// Taken from what Nexon publishes (the patch server, the test world's pages); nobody has
/// looked at an installed test client for this. Not checked: the folder the installer uses,
/// what it writes to the registry, and what the launcher does when started with this game
/// code. Where that matters the answer is "not found" rather than a guess.
pub static TEST: Client = Client {
    server: Server::Test,
    title: "메이플스토리 테스트월드",
    game_program: "MapleStoryT.exe",
    launcher: "GameLauncherT.exe",
    game_code: 589826,
    download_page: "https://maplestory.nexon.com/Testworld/Download",
    game_page: "https://maplestory.nexon.com/Testworld/News/Inspect",
    patch_files: "https://maplestory.dn.nexoncdn.co.kr/PatchT",
    numbers_ahead: 1000,
    usual_dir: r"C:\Nexon\MapleT",
    // Not known. With the key found on a PC that has the test client, this is the one place
    // to put it.
    install_key: None,
    version_key: None,
    // 1.2.208, on the patch server on 2026-11-25.
    known_version: Some(1208),
    maintenance: &notice::TEST,
};

impl Client {
    /// The folder this client is installed in, if it can be found and still holds the game.
    ///
    /// The registry is asked first, where the key is known. Then the folder the installer
    /// uses by default, which only counts when this client's own program is in it: the two
    /// clients must never be taken for each other.
    pub fn find_install_dir(&self) -> Option<PathBuf> {
        let recorded = self.recorded_install_dir().map(PathBuf::from);
        recorded.filter(|dir| dir.is_dir()).or_else(|| {
            let usual = Path::new(self.usual_dir);
            usual.join(self.game_program).is_file().then(|| usual.to_path_buf())
        })
    }

    /// What the registry says this client's install folder is, as the text that stands
    /// there. The folder is not looked at: it may be gone, or hold something else by now.
    /// `None` where the key is not known, as for the test world, or nothing is recorded.
    pub fn recorded_install_dir(&self) -> Option<String> {
        self.install_key.and_then(|key| registry_text(key, "RootPath"))
    }

    /// The folder the installer puts this client in unless told otherwise. Says nothing
    /// about this PC.
    pub fn usual_install_dir(&self) -> &'static str {
        self.usual_dir
    }

    /// A guess at the installed version's number as the patch files count it (`421` for the
    /// live 1.2.421).
    ///
    /// The client does not record its version anywhere readable. This reads a value the game
    /// stores for its overlay cache, which has followed the client version so far. Treat it as
    /// a starting point for finding the right patch file, never as proof: the patch file's own
    /// checksum list is what decides whether a patch fits this install. `None` where no such
    /// value is known, as for the test world.
    pub fn version_hint(&self) -> Option<u32> {
        self.version_key.and_then(|key| registry_number(key, "NxOverlayCacheClearVer")).filter(|&value| value > 0)
    }

    /// A version this client is known to have had, to look upwards from when nothing on the
    /// PC says which version is installed. Says nothing about this install.
    pub fn known_version(&self) -> Option<u32> {
        self.known_version
    }

    /// The version as Nexon writes it, from the number the patch files carry: the live 421 is
    /// "1.2.421", the test world's 1208 is "1.2.208".
    pub fn version_label(&self, number: u32) -> String {
        format!("1.2.{}", number.saturating_sub(self.numbers_ahead))
    }

    /// Where this client's patch files are published: what their addresses begin with, before
    /// the folder of a version.
    pub fn patch_files(&self) -> &'static str {
        self.patch_files
    }

    /// Address of the patch file that takes this client from version `from` to `to` (both as
    /// the patch files count them).
    pub fn patch_url(&self, from: u32, to: u32) -> String {
        format!("{}/{to:05}/{}", self.patch_files, self.patch_file_name(from, to))
    }

    /// The name Nexon gives that patch file: `00420to00421.patch`.
    pub fn patch_file_name(&self, from: u32, to: u32) -> String {
        format!("{from:05}to{to:05}.patch")
    }

    /// Address of a minor patch file: from minor version `from` to `to` of main version
    /// `version`. A freshly released main version is minor 1; Nexon publishes a file for every
    /// pair, so `1 → 4` exists alongside `3 → 4`.
    pub fn minor_patch_url(&self, version: u32, from: u32, to: u32) -> String {
        format!("{}/{version:05}/Minor/{from:02}to{to:02}.patch", self.patch_files)
    }

    /// A name to keep a minor patch file under on this PC. Nexon's own name for it,
    /// `03to04.patch`, does not say which main version it belongs to.
    pub fn minor_patch_file_name(&self, version: u32, from: u32, to: u32) -> String {
        format!("{version:05}-minor-{from:02}to{to:02}.patch")
    }

    /// The address Nexon's own "바로실행" desktop shortcut opens. It starts the game through
    /// Nexon Plug and carries no account information.
    pub fn launch_url(&self) -> String {
        format!("ngm://launch/ -mode:pluglaunch -game:{} -locale:KR -publisher:NexonKorea", self.game_code)
    }

    /// Where Nexon says when this client's game is closed.
    pub fn maintenance(&self) -> &'static notice::Sources {
        self.maintenance
    }
}

/// A text value under `HKLM\SOFTWARE` as a 32-bit program sees it: Nexon's installers are
/// 32-bit, so their keys sit in that view.
#[cfg(windows)]
fn registry_text(key: &str, name: &str) -> Option<String> {
    registry_key(key)?.get_value(name).ok()
}

#[cfg(windows)]
fn registry_number(key: &str, name: &str) -> Option<u32> {
    registry_key(key)?.get_value(name).ok()
}

#[cfg(windows)]
fn registry_key(key: &str) -> Option<winreg::RegKey> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY};
    winreg::RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(key, KEY_READ | KEY_WOW64_32KEY).ok()
}

#[cfg(not(windows))]
fn registry_text(_: &str, _: &str) -> Option<String> {
    None
}

#[cfg(not(windows))]
fn registry_number(_: &str, _: &str) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Looks at this PC: its registry, and the folder a real game is installed in. Run on
    // demand with `cargo test -p mfp-kms -- --ignored`. It is no part of a default run,
    // whose result is not to hang on what is installed where it runs.
    #[test]
    #[ignore = "looks at this PC's registry and the real install folder"]
    fn found_install_dir_is_a_real_folder_with_that_clients_program() {
        // Passes on a PC without the game too: nothing found is a valid answer.
        for server in Server::ALL {
            if let Some(dir) = server.client().find_install_dir() {
                assert!(dir.is_dir(), "{} is not a folder", dir.display());
            }
        }
    }

    #[test]
    fn usual_folders_are_those_of_the_two_clients() {
        assert_eq!((LIVE.usual_install_dir(), TEST.usual_install_dir()), (r"C:\Nexon\Maple", r"C:\Nexon\MapleT"));
    }

    #[test]
    fn patch_urls_match_the_published_layout() {
        assert_eq!(LIVE.patch_url(420, 421), "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch");
        assert_eq!(TEST.patch_url(1207, 1208), "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch");
        // A patch that skips versions is named the same way.
        assert_eq!(LIVE.patch_file_name(416, 421), "00416to00421.patch");
    }

    #[test]
    fn minor_patch_urls_match_the_published_layout() {
        assert_eq!(
            LIVE.minor_patch_url(421, 3, 4),
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch"
        );
        assert_eq!(
            TEST.minor_patch_url(1208, 1, 2),
            "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/Minor/01to02.patch"
        );
        assert_eq!(LIVE.minor_patch_file_name(421, 3, 4), "00421-minor-03to04.patch");
    }

    #[test]
    fn versions_read_as_nexon_writes_them() {
        assert_eq!(LIVE.version_label(421), "1.2.421");
        assert_eq!(TEST.version_label(1208), "1.2.208");
    }

    #[test]
    fn the_two_clients_differ_in_everything_that_tells_them_apart() {
        assert_ne!(LIVE.game_program, TEST.game_program);
        assert_ne!(LIVE.launcher, TEST.launcher);
        assert_ne!(LIVE.launch_url(), TEST.launch_url());
        assert_eq!(LIVE.launch_url(), "ngm://launch/ -mode:pluglaunch -game:589825 -locale:KR -publisher:NexonKorea");
        assert_eq!(TEST.launch_url(), "ngm://launch/ -mode:pluglaunch -game:589826 -locale:KR -publisher:NexonKorea");
        // The test world's version cannot be guessed from the registry; it has to be proven.
        assert_eq!(TEST.version_hint(), None);
        assert_eq!((LIVE.known_version(), TEST.known_version()), (None, Some(1208)));
    }

    #[test]
    fn servers_are_named_for_records_and_messages() {
        for server in Server::ALL {
            assert_eq!(Server::from_key(server.key()), Some(server));
            assert_eq!(server.client().server, server);
        }
        assert_eq!(Server::from_key("other"), None);
    }
}
