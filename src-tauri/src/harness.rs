//! Switches for trying the app out, read from the environment once when the app starts.
//! This comment is the manual for whoever tests with them.
//!
//! # In every build
//!
//! - `MFP_DATA_DIR=<folder>`: the app's data folder, in place of the usual one
//!   (`%LOCALAPPDATA%\com.maplefastpatch.app`). The settings, the log, the install folder
//!   that was picked, the records of every install folder and the cached patch files are
//!   kept there, so a copy of the app started with it shares none of that with the usual
//!   one. The folder is made when something is first written to it. Such a copy runs
//!   beside the usual one and beside others like it: only without the variable does
//!   starting the app a second time bring the first to the front. What the window's own
//!   engine keeps (`EBWebView`) stays in the usual folder for every copy. What the app
//!   keeps about the test world is in `test\` inside the data folder.
//!
//! # In debug builds only
//!
//! The rest exists in a debug build (`cargo build`, `yarn tauri dev`) and is compiled out of
//! a release build, which reads none of these variables. [`StandIns::read`] is where that is
//! decided, under `cfg(debug_assertions)`.
//!
//! Each is named for the client it is about: `LIVE` for the live game, `TEST` for the test
//! world.
//!
//! - `MFP_LIVE_PATCH_FILES`, `MFP_TEST_PATCH_FILES`: where the client's patch files are
//!   published, in place of Nexon's patch server: what their addresses begin with, before
//!   the folder of a version (`http://127.0.0.1:18421/Patch`).
//! - `MFP_LIVE_INSPECTION`: the address of the inspection service. The service does not
//!   list the test world, so there is no such variable for it.
//! - `MFP_LIVE_BOARD`, `MFP_TEST_BOARD`: the address of the board with the client's
//!   maintenance notices.
//! - `MFP_LIVE_VERSION`, `MFP_TEST_VERSION`: the number of the installed version as the
//!   patch files count it (`900`), for an install folder that is not the one in the
//!   registry. It stands in for the registry's guess and for nothing else: what the app
//!   works out or patches to is recorded, and a record goes first, as always.
//!
//! One is for both clients:
//!
//! - `MFP_ALLOW_REAL=1`: without it a debug build takes neither a folder with a `Nexon`
//!   part in its path nor the folder the Nexon installer registered, wherever that is, as
//!   an install folder, picked or found, so that it cannot patch or start the real game by
//!   accident. The screen then shows such a folder as not found.
//! - `MFP_WORK_AREA=<width>x<height>`: the room the window is fitted to, in logical pixels,
//!   in place of the work area of the screen it is on. A window that is larger than its
//!   screen is made smaller as a whole; with this that can be seen on a large screen:
//!   `1097x569` is a 1920 × 1080 screen at 175 %, `1093x566` a 1366 × 768 one at 125 %.
//!
//! An address may be plain `http://`. What stands in is written to the log when the app
//! starts.
//!
//! Starting the game needs no switch. The app starts the launcher that is in the install
//! folder, in every build, and a made-up install has a stand-in there, which adds a line to
//! the file that `MFP_FAKE_LAUNCHED` names (see `fake_launcher`). A made-up install without
//! the stand-in is not started at all, and the screen says that the launcher is not found:
//! the address Nexon's shortcut uses is only ever opened for the registered install.
//!
//! # A session against a made-up world
//!
//! `fake_nexon` (in `crates\tools`, where its own help is) makes the world and serves it. In
//! one console:
//!
//! ```text
//! cargo build -p mfp-tools
//! fake_nexon make C:\MfpWorld\live --size 64
//! fake_nexon serve C:\MfpWorld\live
//! ```
//!
//! In a second one, the app:
//!
//! ```text
//! set MFP_DATA_DIR=C:\MfpWorld\live-data
//! set MFP_LIVE_PATCH_FILES=http://127.0.0.1:18421/Patch
//! set MFP_LIVE_INSPECTION=http://127.0.0.1:18421/inspection
//! set MFP_LIVE_BOARD=http://127.0.0.1:18421/board
//! set MFP_LIVE_VERSION=900
//! set MFP_FAKE_LAUNCHED=C:\MfpWorld\launched.txt
//! mkdir C:\MfpWorld\live-data
//! echo C:\MfpWorld\live\install> C:\MfpWorld\live-data\install-dir
//! yarn tauri dev
//! ```
//!
//! The file `install-dir` is what picking the folder on the screen writes. In a third
//! console, while the app is open:
//!
//! ```text
//! fake_nexon publish C:\MfpWorld\live all
//! fake_nexon maintenance C:\MfpWorld\live begin
//! fake_nexon maintenance C:\MfpWorld\live end
//! fake_nexon state C:\MfpWorld\live
//! ```
//!
//! How soon the app notices goes by its own pace, which the made-up server's console shows
//! request by request. Patch files are asked about as the app starts, every half minute
//! while a maintenance is underway, and every ten minutes otherwise; what Nexon says about
//! maintenance is read at that same pace. So a patch that is published with no maintenance
//! underway can take ten minutes to show. To see it sooner, have a maintenance listed first
//! and start the app after that (`fake_nexon maintenance <dir> announce --in 3`): the app
//! then turns to it at the listed minute by its own clock, without asking again.
//!
//! # The test world
//!
//! The test world is made and served the same way, on a port of its own, and its variables
//! are read the same way. The app has a page for it beside the live game's:
//!
//! ```text
//! fake_nexon make C:\MfpWorld\test --server test --from 1207 --versions 1 --size 64
//! fake_nexon serve C:\MfpWorld\test --port 18422
//! set MFP_TEST_PATCH_FILES=http://127.0.0.1:18422/PatchT
//! set MFP_TEST_BOARD=http://127.0.0.1:18422/board-test
//! mkdir C:\MfpWorld\live-data\test
//! echo C:\MfpWorld\test\install> C:\MfpWorld\live-data\test\install-dir
//! ```
//!
//! A client that is not installed costs nothing: with no folder found and none picked,
//! the app looks for the folder and does nothing else for that client. Neither its patch
//! server nor its board is asked, so a session with one made-up world has no need to stand
//! in for the other client's addresses.
//!
//! Nothing on a PC says which version a test client is at, so the app has to prove it from
//! the folder's own files, and looks for the newest version upwards from the last one it
//! knows the test world to have had (1208). A made-up test world therefore has to lie
//! around that number, as the one above does: its install is at 1207, and one patch leads
//! to 1208. With `MFP_TEST_VERSION` set, the app takes that number for the registry's guess
//! instead, as it does for a copy of the live game, and proves nothing: leave it unset to
//! see what a real test client would get.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mfp_kms::{Client, Server};

/// The variable that names the app's data folder in place of the usual one.
pub(crate) const DATA_DIR: &str = "MFP_DATA_DIR";

/// The variable that names the room the window is fitted to, in a debug build.
#[cfg(debug_assertions)]
const WORK_AREA: &str = "MFP_WORK_AREA";

/// The room the window is fitted to in place of the work area of its screen, when a build
/// made for trying the app out was started with one. A release build has none.
pub(crate) fn work_area() -> Option<(f64, f64)> {
    #[cfg(debug_assertions)]
    {
        room_named(&std::env::var(WORK_AREA).ok()?)
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// The width and height a variable names as `1097x569`, in logical pixels.
#[cfg(any(debug_assertions, test))]
fn room_named(named: &str) -> Option<(f64, f64)> {
    let (width, height) = named.trim().split_once('x')?;
    let size = |text: &str| text.trim().parse::<f64>().ok().filter(|size| *size > 0.0);
    Some((size(width)?, size(height)?))
}

/// The data folder a variable names, `named` being what the variable holds. One that is not
/// set, or set to nothing, names none.
pub(crate) fn data_dir(named: Option<OsString>) -> Option<PathBuf> {
    let folder = PathBuf::from(named.filter(|folder| !folder.is_empty())?);
    // Said once and for as long as the app is open, whatever folder it is started from.
    Some(std::path::absolute(&folder).unwrap_or(folder))
}

/// What stands in, for one client, for Nexon's side of it and for what the registry says of
/// its install, and whether the real game's folders are turned down.
///
/// A release build has the default for every client: nothing stands in for anything, and no
/// folder is turned down.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StandIns {
    patch_files: Option<String>,
    inspection: Option<String>,
    board: Option<String>,
    version: Option<u32>,
    refuse_real: bool,
}

impl StandIns {
    /// What the environment says for the client of `server`.
    pub(crate) fn of(server: Server) -> StandIns {
        StandIns::read(server, &|name| std::env::var(name).ok())
    }

    /// What the variables say for the client of `server`, `var` being how one is looked up.
    ///
    /// This is the one place that knows the debug-only switches by name. A release build
    /// has none of it.
    fn read(server: Server, var: &dyn Fn(&str) -> Option<String>) -> StandIns {
        #[cfg(debug_assertions)]
        {
            let set = |name: &str| var(name).map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
            let of_client = |what: &str| set(&format!("MFP_{}_{what}", server.key().to_ascii_uppercase()));
            let address = |what: &str| of_client(what).map(|address| address.trim_end_matches('/').to_owned());
            // The inspection service has a stand-in for a client it lists, and for no other.
            let listed = server.client().maintenance().inspection.is_some();
            StandIns {
                patch_files: address("PATCH_FILES"),
                inspection: address("INSPECTION").filter(|_| listed),
                board: address("BOARD"),
                version: of_client("VERSION").and_then(|number| number.parse().ok()),
                refuse_real: set("MFP_ALLOW_REAL").as_deref() != Some("1"),
            }
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = (server, var);
            StandIns::default()
        }
    }

    /// What stands in and what is turned down, for a line in the log. `None` when nothing
    /// is, as in every release build.
    pub(crate) fn told(&self) -> Option<String> {
        let StandIns { patch_files, inspection, board, version, refuse_real } = self;
        let told: Vec<String> = [
            patch_files.as_ref().map(|from| format!("patch files from {from}")),
            inspection.as_ref().map(|at| format!("inspection service at {at}")),
            board.as_ref().map(|at| format!("notice board at {at}")),
            version.map(|number| format!("installed version {number} where the registry knows none")),
            refuse_real.then(|| "folders under Nexon are turned down".to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!told.is_empty()).then(|| told.join(", "))
    }

    /// `url`, the address of one of `client`'s patch files, at the stand-in for Nexon's
    /// patch server when there is one.
    pub(crate) fn patch_address(&self, client: &Client, url: String) -> String {
        match (&self.patch_files, url.strip_prefix(client.patch_files())) {
            (Some(stand_in), Some(rest)) => format!("{stand_in}{rest}"),
            _ => url,
        }
    }

    /// The address that stands in for the inspection service's.
    pub(crate) fn inspection(&self) -> Option<&str> {
        self.inspection.as_deref()
    }

    /// The address that stands in for that of the client's notice board.
    pub(crate) fn board(&self) -> Option<&str> {
        self.board.as_deref()
    }

    /// The number that stands in for the registry's guess at the installed version.
    pub(crate) fn version(&self) -> Option<u32> {
        self.version
    }

    /// Whether `dir` is turned down as an install folder: it is where a real game lives,
    /// and this build leaves those alone. That is a folder under `Nexon`, by its path as
    /// given or by where that leads, and the folder the Nexon installer registered wherever
    /// it is, which `registered` says and is only asked when it matters.
    pub(crate) fn refuses(&self, dir: &Path, registered: impl FnOnce() -> bool) -> bool {
        let in_nexon = || under_nexon(dir) || dir.canonicalize().is_ok_and(|real| under_nexon(&real));
        self.refuse_real && (in_nexon() || registered())
    }

    /// Stand-ins with nothing but a version, for tests of what goes by it.
    #[cfg(test)]
    pub(crate) fn at_version(version: u32) -> StandIns {
        StandIns { version: Some(version), ..StandIns::default() }
    }

    /// The live game's stand-ins in a session against a made-up world, for tests of what
    /// asks them. They are put together here, so that such a test is the same in any build.
    #[cfg(test)]
    pub(crate) fn of_a_session() -> StandIns {
        StandIns {
            patch_files: Some("http://127.0.0.1:18421/Patch".to_owned()),
            inspection: Some("http://127.0.0.1:18421/inspection".to_owned()),
            board: Some("http://127.0.0.1:18421/board".to_owned()),
            version: Some(900),
            refuse_real: true,
        }
    }
}

/// Whether a path has a folder called `Nexon` in it, which is where the real games live.
/// The test tools refuse a folder by the same rule.
fn under_nexon(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str().eq_ignore_ascii_case("nexon"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Looks a variable up among `set`, as the environment would.
    fn among(set: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| set.iter().find(|(known, _)| *known == name).map(|(_, value)| (*value).to_owned())
    }

    /// Every switch there is, set as a session against `fake_nexon` sets them.
    const SESSION: &[(&str, &str)] = &[
        ("MFP_LIVE_PATCH_FILES", "http://127.0.0.1:18421/Patch"),
        ("MFP_LIVE_INSPECTION", "http://127.0.0.1:18421/inspection"),
        ("MFP_LIVE_BOARD", "http://127.0.0.1:18421/board"),
        ("MFP_LIVE_VERSION", "900"),
        ("MFP_TEST_PATCH_FILES", "http://127.0.0.1:18422/PatchT"),
        // Not a switch: the service does not list the test world, and nothing is asked of it.
        ("MFP_TEST_INSPECTION", "http://127.0.0.1:18422/inspection"),
        ("MFP_TEST_BOARD", "http://127.0.0.1:18422/board-test"),
        ("MFP_TEST_VERSION", "1900"),
        ("MFP_ALLOW_REAL", "1"),
    ];

    #[test]
    fn the_room_to_fit_the_window_to_is_named_as_width_and_height() {
        assert_eq!(room_named("1097x569"), Some((1097.0, 569.0)));
        assert_eq!(room_named(" 1092.8 x 566.4 "), Some((1092.8, 566.4)));
        for other in ["", "1097", "1097x", "x569", "wide x high", "0x569", "-5x569"] {
            assert_eq!(room_named(other), None, "{other:?}");
        }
    }

    #[test]
    fn the_data_folder_is_the_one_that_is_named_and_none_when_nothing_is() {
        assert_eq!(data_dir(None), None);
        assert_eq!(data_dir(Some(OsString::new())), None);
        let named = if cfg!(windows) { r"D:\mfp\data" } else { "/mfp/data" };
        assert_eq!(data_dir(Some(OsString::from(named))), Some(PathBuf::from(named)));
        // A folder named from where the app is started is the same folder from then on.
        assert!(data_dir(Some(OsString::from("data"))).is_some_and(|folder| folder.is_absolute()));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn each_client_has_its_own_switches() {
        let live = StandIns::read(Server::Live, &among(SESSION));
        assert_eq!(
            live,
            StandIns {
                patch_files: Some("http://127.0.0.1:18421/Patch".to_owned()),
                inspection: Some("http://127.0.0.1:18421/inspection".to_owned()),
                board: Some("http://127.0.0.1:18421/board".to_owned()),
                version: Some(900),
                refuse_real: false,
            }
        );
        let test = StandIns::read(Server::Test, &among(SESSION));
        assert_eq!(
            test,
            StandIns {
                patch_files: Some("http://127.0.0.1:18422/PatchT".to_owned()),
                inspection: None,
                board: Some("http://127.0.0.1:18422/board-test".to_owned()),
                version: Some(1900),
                refuse_real: false,
            }
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_debug_build_with_nothing_set_only_turns_the_real_game_down() {
        let nothing = StandIns::read(Server::Live, &among(&[]));
        assert_eq!(nothing, StandIns { refuse_real: true, ..StandIns::default() });
        assert_eq!(nothing.told().as_deref(), Some("folders under Nexon are turned down"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_switch_is_read_without_the_space_around_it_and_counts_as_unset_when_empty() {
        let read = StandIns::read(
            Server::Live,
            &among(&[
                ("MFP_LIVE_PATCH_FILES", " http://127.0.0.1:18421/Patch/ "),
                ("MFP_LIVE_BOARD", "  "),
                ("MFP_LIVE_VERSION", " 900 "),
            ]),
        );
        assert_eq!(read.patch_files.as_deref(), Some("http://127.0.0.1:18421/Patch"));
        assert_eq!((read.board(), read.version()), (None, Some(900)));
        // What is no number is no version.
        let odd = StandIns::read(Server::Live, &among(&[("MFP_LIVE_VERSION", "1.2.900")]));
        assert_eq!(odd.version(), None);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_real_game_is_only_let_in_by_the_one_word_for_it() {
        for word in ["0", "yes", "true", ""] {
            let set = [("MFP_ALLOW_REAL", word)];
            let read = StandIns::read(Server::Live, &|name| {
                set.iter().find(|(known, _)| *known == name).map(|(_, value)| (*value).to_owned())
            });
            assert!(read.refuses(Path::new(r"C:\Nexon\Maple"), || false), "{word:?}");
        }
        assert!(
            !StandIns::read(Server::Live, &among(&[("MFP_ALLOW_REAL", "1")]))
                .refuses(Path::new(r"C:\Nexon\Maple"), || true)
        );
    }

    /// Run with `cargo test --release`: there every switch but the data folder is gone.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_release_build_reads_none_of_the_debug_switches() {
        for server in Server::ALL {
            let read = StandIns::read(server, &among(SESSION));
            assert_eq!(read, StandIns::default());
            assert_eq!(read.told(), None);
        }
        let nothing_set = StandIns::read(Server::Live, &among(&[]));
        assert!(!nothing_set.refuses(Path::new(r"C:\Nexon\Maple"), || true));
    }

    #[test]
    fn without_stand_ins_everything_is_nexons_own_and_no_folder_is_turned_down() {
        let none = StandIns::default();
        let url = mfp_kms::LIVE.patch_url(420, 421);
        assert_eq!(none.patch_address(&mfp_kms::LIVE, url.clone()), url);
        assert_eq!((none.inspection(), none.board(), none.version(), none.told()), (None, None, None, None));
        assert!(!none.refuses(Path::new(r"C:\Nexon\Maple"), || true));
    }

    #[test]
    fn patch_files_are_asked_for_at_the_stand_in_under_the_names_nexon_gives_them() {
        let (live, test) = (&mfp_kms::LIVE, &mfp_kms::TEST);
        let at = |stand_in: &str| StandIns { patch_files: Some(stand_in.to_owned()), ..StandIns::default() };
        let stand_in = at("http://127.0.0.1:18421/Patch");
        assert_eq!(
            stand_in.patch_address(live, live.patch_url(900, 901)),
            "http://127.0.0.1:18421/Patch/00901/00900to00901.patch"
        );
        assert_eq!(
            stand_in.patch_address(live, live.minor_patch_url(902, 1, 2)),
            "http://127.0.0.1:18421/Patch/00902/Minor/01to02.patch"
        );
        assert_eq!(
            at("http://127.0.0.1:18422/PatchT").patch_address(test, test.patch_url(1900, 1901)),
            "http://127.0.0.1:18422/PatchT/01901/01900to01901.patch"
        );
        // An address that is not among the client's patch files is left as it is.
        let elsewhere = "https://example.invalid/Patch/00901/00900to00901.patch".to_owned();
        assert_eq!(stand_in.patch_address(live, elsewhere.clone()), elsewhere);
    }

    #[test]
    fn a_folder_of_the_real_game_is_told_by_the_nexon_in_its_path() {
        assert!(under_nexon(Path::new(r"C:\Nexon\Maple")) && under_nexon(Path::new(r"D:\games\NEXON\MapleT\Data")));
        assert!(!under_nexon(Path::new(r"C:\MfpWorld\live\install")) && !under_nexon(Path::new(r"C:\NexonX\Maple")));
        let refusing = StandIns { refuse_real: true, ..StandIns::default() };
        assert!(refusing.refuses(Path::new(r"C:\Nexon\Maple"), || false));
        assert!(!refusing.refuses(Path::new(r"C:\MfpWorld\live\install"), || false));
        // The folder the Nexon installer registered is the real game wherever it is.
        assert!(refusing.refuses(Path::new(r"D:\Games\Maple"), || true));
        // Nor is the registry asked about a folder that is turned down already, or by a
        // build that turns nothing down.
        let never = || -> bool { panic!("asked whether the folder is registered") };
        assert!(refusing.refuses(Path::new(r"C:\Nexon\Maple"), never));
        assert!(!StandIns::default().refuses(Path::new(r"D:\Games\Maple"), never));
    }
}
