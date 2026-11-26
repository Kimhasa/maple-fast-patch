//! What the commands, the desks and the patch thread share: the state the app keeps while
//! it is open, and what a look at an install folder and a check of the server find.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use mfp_engine::Stage;
use mfp_engine::detect::Checksums;
use mfp_engine::preflight::{self, Problem, Programs};
use mfp_kms::{Client, Server, notice};
use serde::Serialize;

use crate::flow::{Flow, PatchHours, TEST_WORLD_PATCH_HOURS};
use crate::harness::StandIns;
use crate::log::log_line;
use crate::maintenance::Told;
use crate::measure::Measure;
use crate::settings::Settings;
use crate::update::Update;
use crate::wording::{APP_TITLE, describe_problem, sentence};

/// What the whole app shares: what the user has set, the one patch run there is at a time,
/// and a [`Site`] for each client.
pub(crate) struct Patcher {
    /// Which client a run is at work for: the place of its site among [`Patcher::sites`]
    /// and one more, or nothing. There is one run at a time, whichever client it is for.
    running: AtomicU8,
    pub(crate) stop: Arc<AtomicBool>,
    /// The window was asked to close during a run; the app exits once the run has stopped.
    pub(crate) closing: AtomicBool,
    pub(crate) settings: Mutex<Settings>,
    /// Word of a newer version of this app, when its home was last read and had one.
    pub(crate) update: Mutex<Option<Update>>,
    /// The app's data folder, where the settings and the log are kept.
    pub(crate) data: PathBuf,
    /// One for each client the app looks after: the live game first, then the test world.
    pub(crate) sites: Vec<Site>,
}

impl Patcher {
    /// The app's state as it starts, with `data` as its data folder.
    pub(crate) fn new(data: PathBuf) -> Patcher {
        // The live game's site keeps its files in the app's data folder itself, where they
        // have been all along. The test world's are in a folder of their own inside it.
        let live = Site::new(&mfp_kms::LIVE, data.clone(), StandIns::of(Server::Live));
        let test = Site::new(&mfp_kms::TEST, data.join(Server::Test.key()), StandIns::of(Server::Test));
        Patcher::of(data, vec![live, test])
    }

    /// The app's state as it starts with `sites` as its sites.
    pub(crate) fn of(data: PathBuf, sites: Vec<Site>) -> Patcher {
        Patcher {
            running: AtomicU8::new(0),
            stop: Arc::default(),
            closing: AtomicBool::new(false),
            settings: Mutex::new(Settings::load(&data)),
            update: Mutex::default(),
            data,
            sites,
        }
    }

    /// The site of the client of `server`, if the app looks after that client.
    pub(crate) fn site(&self, server: Server) -> Option<&Site> {
        self.sites.iter().find(|site| site.client.server == server)
    }

    /// The site of the client the page calls `server`: `live`.
    pub(crate) fn site_named(&self, server: &str) -> Option<&Site> {
        self.site(Server::from_key(server)?)
    }

    /// The site a run is at work for, if one is.
    pub(crate) fn running(&self) -> Option<&Site> {
        self.sites.get(usize::from(self.running.load(Ordering::SeqCst)).checked_sub(1)?)
    }

    /// Whether the run that is at work is for `site`.
    pub(crate) fn running_for(&self, site: &Site) -> bool {
        self.running().is_some_and(|at_work| std::ptr::eq(at_work, site))
    }

    /// The site whose run keeps `site` from starting one of its own: a run is at work, and
    /// it is another client's. A run that is paused keeps nobody.
    pub(crate) fn holding(&self, site: &Site) -> Option<&Site> {
        self.running().filter(|at_work| !std::ptr::eq(*at_work, site))
    }

    /// Where `site` is among the sites.
    pub(crate) fn place(&self, site: &Site) -> Option<usize> {
        self.sites.iter().position(|known| std::ptr::eq(known, site))
    }

    /// Takes the one turn there is for a run of `site`. Says whether it was free.
    pub(crate) fn begin_run(&self, site: &Site) -> bool {
        let Some(place) = self.place(site) else { return false };
        self.running.compare_exchange(0, place as u8 + 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }

    /// The run that was at work has ended.
    pub(crate) fn end_run(&self) {
        self.running.store(0, Ordering::SeqCst);
    }
}

/// One client's side of the app: where its game is installed, what the app keeps about that
/// install, and what it has found out about it since it was started.
pub(crate) struct Site {
    pub(crate) client: &'static Client,
    /// Where the app keeps its files about this client: the install folder that was picked,
    /// the records of each install folder, and the cached patch files.
    pub(crate) data: PathBuf,
    /// What a build made for trying the app out puts in the place of Nexon's side.
    stand_ins: StandIns,
    /// What the site's desk knows and has told the page; see [`Flow`]. Whoever changes it
    /// wakes the desk through `woken`, which goes with this lock.
    pub(crate) flow: Mutex<Flow>,
    pub(crate) woken: Condvar,
    /// Held for as long as the install folder is being looked at: one look at a time, so
    /// that what the looks find is taken in in the order they were made.
    pub(crate) looking: Mutex<()>,
    /// What a check last worked out, and for which install folder; see
    /// [`availability`](crate::offer::availability).
    pub(crate) found: Mutex<Option<Found>>,
    /// Held by a check, and by a run, for as long as it works on the patch cache and the
    /// install folder, so that no two of them ever do so at once.
    pub(crate) busy: Mutex<()>,
    /// Set to make a check that is at work give up: the folder it is about was left, or a
    /// run wants to start. Each check clears it when it begins.
    pub(crate) check_stop: Arc<AtomicBool>,
    /// Held while an apply that was cut off is being undone, so two looks do not both
    /// try.
    pub(crate) restoring: Mutex<()>,
    /// A failed attempt to undo a cut-off run has been written to the log.
    pub(crate) restore_logged: AtomicBool,
    /// Checksums of install files read so far, and the install folder they are of.
    pub(crate) checksums: Mutex<(PathBuf, Checksums)>,
    /// Last write test of the install folder and when it was made.
    write_test: Mutex<Option<(Instant, PathBuf, Vec<Problem>)>>,
    /// What is known of when the game is closed, as last read from what Nexon publishes.
    pub(crate) told: Mutex<Told>,
    /// The last check that found no proof of the version the install folder is at, with
    /// what it went by; see [`Unproven`].
    pub(crate) unproven: Mutex<Option<Unproven>>,
    /// The failure of a check that was last written to the log. While checks keep failing
    /// the same way, as they do every half minute without a connection, it is not written
    /// again.
    pub(crate) check_failure: Mutex<Option<String>>,
    /// How the patch being applied in this session has gone so far; see [`Measure`].
    pub(crate) measure: Mutex<Option<Measure>>,
    /// The measurement of the last run that finished, as the text [`run_report`](crate::measure::run_report) hands out.
    pub(crate) report: Mutex<Option<String>>,
}

impl Site {
    /// The site of `client` as the app starts, keeping its files in `data`.
    pub(crate) fn new(client: &'static Client, data: PathBuf, stand_ins: StandIns) -> Site {
        Site {
            client,
            data,
            stand_ins,
            flow: Mutex::new(Flow::new(Instant::now())),
            woken: Condvar::new(),
            looking: Mutex::default(),
            found: Mutex::default(),
            busy: Mutex::default(),
            check_stop: Arc::default(),
            restoring: Mutex::default(),
            restore_logged: AtomicBool::new(false),
            checksums: Mutex::default(),
            write_test: Mutex::default(),
            told: Mutex::default(),
            unproven: Mutex::default(),
            check_failure: Mutex::default(),
            measure: Mutex::default(),
            report: Mutex::default(),
        }
    }

    /// A line for the log as this site writes it. The live game's lines are as they have
    /// always been; another client's say whose they are: `[test] done, 4 file(s) changed`.
    pub(crate) fn said(&self, text: &str) -> String {
        match self.client.server {
            Server::Live => text.to_owned(),
            other => format!("[{}] {text}", other.key()),
        }
    }

    /// Writes a line of this site to the log.
    pub(crate) fn log(&self, text: &str) {
        log_line(&self.said(text));
    }

    /// What a notification about this site's game is headed by: the app's name for the
    /// live game, as always, and the game's own name for another client, so that it is
    /// known which game is meant.
    pub(crate) fn heading(&self) -> &'static str {
        match self.client.server {
            Server::Live => APP_TITLE,
            _ => self.client.title,
        }
    }

    /// Whether what this app does for the client has never been tried on a real install
    /// of it. That is so for the test world, which is put together from what Nexon
    /// publishes. For such a client the app does nothing by itself that it has not once
    /// done at the user's own press, and guesses nothing.
    pub(crate) fn untried(&self) -> bool {
        self.client.server == Server::Test
    }

    /// The hours of the week in which this client's patches come out with no maintenance
    /// to announce them, if it has such hours.
    pub(crate) fn patch_hours(&self) -> Option<&'static PatchHours> {
        match self.client.server {
            Server::Live => None,
            Server::Test => Some(&TEST_WORLD_PATCH_HOURS),
        }
    }

    /// The client's programs, as the engine asks for them: a folder is this client's by
    /// them, and they are the ones that must not be running during a patch.
    pub(crate) fn programs(&self) -> Programs {
        Programs { game: self.client.game_program, launcher: self.client.launcher }
    }

    /// Address of the patch file that takes the client from version `from` to `to`; see
    /// [`Client::patch_url`].
    pub(crate) fn patch_url(&self, from: u32, to: u32) -> String {
        self.stand_ins.patch_address(self.client, self.client.patch_url(from, to))
    }

    /// Address of a minor patch file; see [`Client::minor_patch_url`].
    pub(crate) fn minor_patch_url(&self, version: u32, from: u32, to: u32) -> String {
        self.stand_ins.patch_address(self.client, self.client.minor_patch_url(version, from, to))
    }

    /// The registry's guess at the installed version, for an install folder that is not the
    /// one the registry names; see [`Client::version_hint`].
    pub(crate) fn version_guess(&self) -> Option<u32> {
        self.stand_ins.version().or_else(|| self.client.version_hint())
    }

    /// The inspection service, if it lists this client's maintenance: the address it is
    /// asked at, and how it is asked.
    pub(crate) fn inspection(&self) -> Option<(&str, &'static notice::Inspection)> {
        let service = self.client.maintenance().inspection.as_ref()?;
        Some((self.stand_ins.inspection().unwrap_or(service.url), service))
    }

    /// The address of the board with this client's maintenance notices.
    pub(crate) fn board(&self) -> &str {
        self.stand_ins.board().unwrap_or(self.client.maintenance().board_url)
    }

    /// What stands in for Nexon's side of this client, for a line in the log; see
    /// [`StandIns::told`].
    pub(crate) fn stand_ins_told(&self) -> Option<String> {
        self.stand_ins.told()
    }

    /// Whether `dir` is turned down as this client's install folder, whatever is in it; see
    /// [`StandIns::refuses`]. `registered` says whether it is the folder the Nexon installer
    /// registered, and is only asked where that matters.
    pub(crate) fn refuses(&self, dir: &Path, registered: impl FnOnce() -> bool) -> bool {
        self.stand_ins.refuses(dir, registered)
    }

    /// Wakes the site's desk, so that what was just asked of it or changed under it does
    /// not wait for its next round.
    pub(crate) fn wake_desk(&self) {
        self.flow.lock().unwrap().wake();
        self.woken.notify_all();
    }

    /// Whether the install folder accepts writes. The test creates a file there, so its
    /// result is reused for a while instead of repeating it on every look.
    pub(crate) fn writable(&self, dir: &Path) -> Vec<Problem> {
        let mut cached = self.write_test.lock().unwrap();
        match &*cached {
            Some((at, tested, problems)) if tested == dir && at.elapsed() < Duration::from_secs(30) => problems.clone(),
            _ => {
                let problems = preflight::check_writable(dir);
                *cached = Some((Instant::now(), dir.to_path_buf(), problems.clone()));
                problems
            }
        }
    }

    /// What the last check found, if it was made for `install`.
    pub(crate) fn found_for(&self, install: &Path) -> Option<Found> {
        self.found.lock().unwrap().clone().filter(|found| found.install == install)
    }

    pub(crate) fn forget_found(&self) {
        *self.found.lock().unwrap() = None;
        *self.unproven.lock().unwrap() = None;
    }
}

/// What a check went by when it found no proof of the version an install folder is at: the
/// folder as it was, and what the server had. While neither has changed, looking for a
/// proof again would find none again, and is not done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unproven {
    pub(crate) install: PathBuf,
    /// The state of the folder's files then; see [`install_mark`](crate::records::install_mark).
    pub(crate) mark: Option<String>,
    /// The newest main version the server had, and the newest minor of it.
    pub(crate) newest: u32,
    pub(crate) newest_minor: u32,
}

/// The outcome of one check, with what it was worked out from. It is reused for as long as
/// the install folder, its version and what the server offers stay the same.
#[derive(Clone)]
pub(crate) struct Found {
    pub(crate) install: PathBuf,
    /// The state of the folder's files when this was worked out; see [`install_mark`](crate::records::install_mark).
    pub(crate) mark: Option<String>,
    pub(crate) version: u32,
    /// Whether the server has a main patch from `version`, and the newest minor of `version`.
    pub(crate) main: bool,
    pub(crate) newest_minor: u32,
    pub(crate) target: Option<Target>,
    /// The newest version on the server.
    pub(crate) latest: String,
    /// Why no target could be worked out although the server has patches: the install
    /// matches none of them, or they are in a form this app cannot read.
    pub(crate) error: Option<String>,
}

impl Found {
    pub(crate) fn availability(&self) -> Availability {
        Availability { target: self.target.clone(), latest: Some(self.latest.clone()), error: self.error.clone() }
    }
}

/// A patch file on Nexon's server that this install can take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// Version the patch leads to, such as `1.2.422` or `1.2.421 (4)`.
    pub(crate) label: String,
    pub(crate) size: u64,
    /// Where the file is on the server and what it is kept as on this PC.
    pub(crate) url: String,
    pub(crate) file_name: String,
    /// Main version after the patch, and the minor it leaves the install at when known.
    pub(crate) version: u32,
    pub(crate) minor: Option<u32>,
}

/// What a look at the install folder finds: the folder, the checks made before a patch may
/// start there, and what an unfinished run left.
#[derive(Clone)]
pub(crate) struct Status {
    pub(crate) install_dir: Option<String>,
    /// Installed version, such as `1.2.421`, when it is known.
    pub(crate) version: Option<String>,
    /// Every check made before a patch may start, passed or not.
    pub(crate) checks: Vec<Check>,
    /// The folder was picked by the user instead of found automatically.
    pub(crate) custom_dir: bool,
    /// Size of what a paused or interrupted run left in the install folder, such as `12.3 GB`.
    pub(crate) leftover: Option<String>,
    /// An earlier run was paused or interrupted and can be continued.
    pub(crate) resumable: bool,
    /// The patch file on this PC that run was applying, when it was one the user had picked.
    /// Continuing means continuing with that file, also after the app was closed in between.
    pub(crate) resume_file: Option<String>,
    /// How far the bar had come in that run, when the files it built are still there.
    pub(crate) resume_percent: Option<f64>,
    /// That run replaced game files as it built them, so the folder is partly new. Only
    /// finishing the same patch makes it whole; its files exist nowhere else.
    pub(crate) half_patched: bool,
    /// A patch here is the user's to start, whatever they have set: the client is one this
    /// app has never been tried on (see [`Site::untried`]), and it has not patched this
    /// folder before.
    pub(crate) by_press_only: bool,
}

impl Status {
    /// The install folder is there and is a game folder. Only then is there anything to
    /// ask the server about.
    pub(crate) fn game_folder(&self) -> bool {
        self.install_dir.is_some() && self.checks.iter().any(|check| check.kind == CheckKind::Folder && check.ok)
    }

    /// Whether a check has anything to say of the folder: it is a game folder, or an apply
    /// that was cut off in it is still to be undone, which is what a check then says.
    pub(crate) fn askable(&self) -> bool {
        self.game_folder() || self.checks.iter().any(|check| check.kind == CheckKind::Recovery)
    }

    /// Something is in the way of a patch: one of the checks has not passed.
    pub(crate) fn blocked(&self) -> bool {
        self.checks.iter().any(|check| !check.ok)
    }
}

/// Which of the checks made before a patch may start one is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckKind {
    /// The install folder: that there is one, and that the game is in it.
    Folder,
    /// The game or one of its programs is open.
    Running,
    /// Files can be written to the install folder.
    Writable,
    /// The drive has room for the patch file.
    Space,
    /// An apply that was cut off could be undone.
    Recovery,
}

impl CheckKind {
    /// What the line is called on the screen.
    fn label(self) -> &'static str {
        match self {
            CheckKind::Folder => "설치 경로",
            CheckKind::Running => "관련 프로그램 실행 중",
            CheckKind::Writable => "쓰기 권한",
            CheckKind::Space => "여유 공간",
            CheckKind::Recovery => "패치 복구",
        }
    }
}

#[derive(Clone)]
pub(crate) struct Check {
    pub(crate) kind: CheckKind,
    pub(crate) label: &'static str,
    pub(crate) ok: bool,
    /// Shown only when it fails; passing needs no line on the screen.
    pub(crate) quiet: bool,
    /// What was found, in a few words for the line: the folder, the free space, what is in
    /// the way.
    pub(crate) detail: String,
    /// For a failed check: what is wrong, as the sentence a dialog shows.
    pub(crate) message: String,
}

impl Check {
    pub(crate) fn passed(kind: CheckKind, detail: impl Into<String>) -> Check {
        Check { kind, label: kind.label(), ok: true, quiet: false, detail: detail.into(), message: String::new() }
    }

    pub(crate) fn failed(kind: CheckKind, detail: impl Into<String>, message: impl Into<String>) -> Check {
        Check { kind, label: kind.label(), ok: false, quiet: false, detail: detail.into(), message: message.into() }
    }

    pub(crate) fn quiet(mut self) -> Check {
        self.quiet = true;
        self
    }

    /// Turns an engine check into a line: passed when it found no problems. `game` is what
    /// the client's game is called, for a sentence that names it.
    pub(crate) fn of(game: &str, kind: CheckKind, passed: impl Into<String>, problems: Vec<Problem>) -> Check {
        match problems.into_iter().next() {
            None => Check::passed(kind, passed),
            Some(problem) => Check::failed(kind, describe_problem(&problem), sentence(game, &problem)),
        }
    }
}

/// Whether a patch this install can take is on Nexon's server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Availability {
    pub(crate) target: Option<Target>,
    /// The newest version the server has, whether or not this install can take it yet.
    pub(crate) latest: Option<String>,
    /// Set when the server could not be asked at all.
    pub(crate) error: Option<String>,
}

impl Availability {
    /// The answer where there is nothing to ask the server about, and nothing went wrong:
    /// no install folder, or a folder that is not a game folder.
    pub(crate) fn nothing() -> Availability {
        Availability { target: None, latest: None, error: None }
    }
}

/// The step a run is at. The page goes by this; what the step is called is only shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Step {
    Download,
    VerifyPatch,
    VerifyInstall,
    Trial,
    Rebuild,
    Apply,
}

impl From<Stage> for Step {
    fn from(stage: Stage) -> Step {
        match stage {
            Stage::Download => Step::Download,
            Stage::VerifyPatch => Step::VerifyPatch,
            Stage::VerifyInstall => Step::VerifyInstall,
            Stage::Trial => Step::Trial,
            Stage::Rebuild => Step::Rebuild,
            Stage::Apply => Step::Apply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(client: &'static Client, stand_ins: StandIns) -> Site {
        Site::new(client, PathBuf::from("data"), stand_ins)
    }

    /// What the live game's site asks for and where, written out as the release before the
    /// sites asked for it.
    #[test]
    fn the_live_site_asks_nexon_for_what_the_app_always_asked() {
        let live = site(&mfp_kms::LIVE, StandIns::default());
        assert_eq!(live.programs(), Programs { game: "MapleStory.exe", launcher: "GameLauncher.exe" });
        assert_eq!(live.patch_url(420, 421), "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch");
        assert_eq!(
            live.minor_patch_url(421, 3, 4),
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch"
        );
        let (asked_at, service) = live.inspection().unwrap();
        assert_eq!(asked_at, "https://api.maplestory.nexon.com/soap/maplestory.asmx");
        assert_eq!(service.action, "https://api.maplestory.nexon.com/soap/GetInspectionInfo");
        assert_eq!(live.board(), "https://maplestory.nexon.com/News/Notice/Inspection");
        assert_eq!(live.version_guess(), mfp_kms::LIVE.version_hint());
        assert_eq!(live.stand_ins_told(), None);
        assert!(!live.refuses(Path::new(r"C:\Nexon\Maple"), || true));
    }

    #[test]
    fn a_site_asks_what_stands_in_for_nexon_in_the_same_way() {
        let live = site(&mfp_kms::LIVE, StandIns::of_a_session());
        assert_eq!(live.patch_url(900, 901), "http://127.0.0.1:18421/Patch/00901/00900to00901.patch");
        assert_eq!(live.minor_patch_url(902, 1, 2), "http://127.0.0.1:18421/Patch/00902/Minor/01to02.patch");
        let (asked_at, service) = live.inspection().unwrap();
        assert_eq!(asked_at, "http://127.0.0.1:18421/inspection");
        // Only the address is another. The question is the one Nexon's service is asked.
        let nexons = mfp_kms::LIVE.maintenance().inspection.as_ref().unwrap();
        assert_eq!((service.action, service.request), (nexons.action, nexons.request));
        assert_eq!(live.board(), "http://127.0.0.1:18421/board");
        assert_eq!(live.version_guess(), Some(900));
        assert!(live.refuses(Path::new(r"C:\Nexon\Maple"), || false));
        // The programs are the client's own whatever stands in for the servers.
        assert_eq!(live.programs(), site(&mfp_kms::LIVE, StandIns::default()).programs());
    }

    /// Nothing in the app is a site of the test world yet. One would be this client's in
    /// everything that tells the two apart.
    #[test]
    fn a_site_is_its_clients_own() {
        let test = site(&mfp_kms::TEST, StandIns::default());
        assert_eq!(test.programs(), Programs { game: "MapleStoryT.exe", launcher: "GameLauncherT.exe" });
        assert_eq!(test.patch_url(1207, 1208), "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch");
        // The inspection service does not list the test world.
        assert!(test.inspection().is_none());
        assert_eq!(test.board(), "https://maplestory.nexon.com/Testworld/News/Inspect");
    }

    #[test]
    fn there_is_one_run_at_a_time_and_it_is_known_whose() {
        let patcher = Patcher::new(PathBuf::from("data"));
        let site = &patcher.sites[0];
        assert!(patcher.running().is_none() && !patcher.running_for(site));
        assert!(patcher.begin_run(site));
        assert!(patcher.running_for(site));
        // The turn is taken.
        assert!(!patcher.begin_run(site));
        patcher.end_run();
        assert!(patcher.running().is_none());
        assert!(patcher.begin_run(site));
    }

    #[test]
    fn a_status_says_whether_there_is_a_folder_to_ask_about_and_whether_something_is_in_the_way() {
        let status = crate::made::folder;
        let game = status(Some(r"C:\Games\Maple"), vec![Check::passed(CheckKind::Folder, r"C:\Games\Maple")]);
        assert!(game.game_folder() && game.askable() && !game.blocked());

        // The game is open: in the way of a patch, and nothing to do with what the server has.
        let open = Check::failed(CheckKind::Running, "MapleStory.exe", "게임 관련 프로그램이 실행 중입니다.").quiet();
        let playing = status(Some(r"C:\Games\Maple"), vec![Check::passed(CheckKind::Folder, ""), open]);
        assert!(playing.game_folder() && playing.askable() && playing.blocked());

        let no_game = Check::failed(CheckKind::Folder, "설치 경로 아님", "메이플스토리 설치 경로가 아닙니다.");
        let empty = status(Some(r"D:\Empty"), vec![no_game.clone()]);
        assert!(!empty.game_folder() && !empty.askable() && empty.blocked());
        assert!(!status(None, vec![no_game.clone()]).askable());

        // A folder whose game program lies among the backups of an apply that was cut off:
        // no game folder as it stands, and a check says that it is being put back.
        let cut_off = Check::failed(CheckKind::Recovery, "실패", "이전 패치를 복구하지 못했습니다.");
        let waiting = status(Some(r"C:\Games\Maple"), vec![no_game, cut_off]);
        assert!(!waiting.game_folder() && waiting.askable());
    }

    /// The app looks after both clients. What it keeps of the test world is in a folder
    /// of its own, and the live game's stays where it has always been.
    #[test]
    fn the_app_has_a_site_for_each_client_and_keeps_the_live_games_files_where_it_always_did() {
        let patcher = Patcher::new(PathBuf::from("data"));
        assert_eq!(patcher.sites.len(), 2);
        assert_eq!(patcher.site_named("live").map(|site| site.data.clone()), Some(patcher.data.clone()));
        assert!(patcher.site_named("other").is_none());
        let (live, test) = (patcher.site_named("live").unwrap(), patcher.site_named("test").unwrap());
        assert_eq!((live.client.server, test.client.server), (Server::Live, Server::Test));
        assert_eq!(live.data, Path::new("data"));
        assert_eq!(test.data, Path::new("data").join("test"));
        // The live game comes first: it is the page the window opens on.
        assert!(std::ptr::eq(live, &patcher.sites[0]));

        // Its lines in the log say whose they are, and its notifications are headed by its
        // own name. The live game's are as they have always been.
        assert_eq!(live.said("done, 4 file(s) changed"), "done, 4 file(s) changed");
        assert_eq!(test.said("done, 4 file(s) changed"), "[test] done, 4 file(s) changed");
        assert_eq!((live.heading(), test.heading()), ("MAPLE FAST PATCH", "메이플스토리 테스트월드"));
        // The test world is the client this app was never tried on, and the one with hours
        // of its own in which its patches come out.
        assert!(test.untried() && !live.untried());
        assert!(test.patch_hours().is_some() && live.patch_hours().is_none());
    }

    #[test]
    fn a_run_of_one_site_keeps_the_other_from_starting_until_it_has_ended() {
        let patcher = Patcher::new(PathBuf::from("data"));
        let (live, test) = (&patcher.sites[0], &patcher.sites[1]);
        let holder = |of: &Site| patcher.holding(of).map(|site| site.client.server);
        assert_eq!((holder(live), holder(test)), (None, None));

        assert!(patcher.begin_run(live));
        assert_eq!((holder(live), holder(test)), (None, Some(Server::Live)));
        // The one turn is taken, whichever site asks for it.
        assert!(!patcher.begin_run(test) && !patcher.begin_run(live));
        assert!(patcher.running_for(live) && !patcher.running_for(test));

        // Paused, failed or done: once the run has ended it keeps nobody.
        patcher.end_run();
        assert_eq!((holder(live), holder(test)), (None, None));
        assert!(patcher.begin_run(test));
        assert_eq!((holder(live), holder(test)), (Some(Server::Test), None));
    }
}
