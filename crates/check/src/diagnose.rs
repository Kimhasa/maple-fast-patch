//! Runs the diagnostic: puts the questions in order, asks the user before the steps that
//! take long or write, and hands back everything that was found.
//!
//! Nothing here prints. Whoever runs the diagnostic is spoken to through a [`Desk`], which
//! the console program fills in with a console and a test with a script.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mfp_kms::{Client, Server};

use crate::compare::{self, Folder, QUICK_LIMIT, Standing};
use crate::discover::{self, Places};
use crate::facts::{Asked, Comparison, Fact, Facts, How, PatchOnShelf, Registry, Shelf, Unknown, WriteTest, Wrote};
use crate::known::{Sheet, State, Table};
use crate::privacy::Scrub;
use crate::probe::game::Walked;
use crate::probe::{game, programs, read, system, volume, write};
use crate::server::{self, Nexon};
use crate::units::{count, size};
use crate::verdict::{Evidence, Verdict, verdict};

/// Whether a step that is only taken when asked for is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// The user is asked, and no answer means no.
    #[default]
    Ask,
    Yes,
    No,
}

/// How a diagnostic is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The install folder, when whoever starts the program names it. Nothing is looked for
    /// then: not where the registry says, not in the usual place, not on the drives.
    pub folder: Option<PathBuf>,
    /// Which client is meant. `None` goes by what is found.
    pub server: Option<Server>,
    /// Reading every file of the game for the full comparison.
    pub full: Choice,
    /// Writing the probe file.
    pub write_probe: Choice,
    /// Reading from the game's drive while the game runs.
    pub heavy_reads: Choice,
    /// Making and removing the empty file.
    pub write_test: bool,
    /// Asking Nexon's patch server.
    pub online: bool,
    /// The size of the probe file.
    pub probe_bytes: u64,
    /// A table of known file states to go by in place of the one built into the program,
    /// for the client it is of: one made after a patch that came out since.
    pub table: Option<Table>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            folder: None,
            server: None,
            full: Choice::Ask,
            write_probe: Choice::Ask,
            heavy_reads: Choice::Ask,
            write_test: true,
            online: true,
            probe_bytes: write::PROBE_BYTES,
            table: None,
        }
    }
}

/// Whoever runs the diagnostic: told what is going on, and asked what only they can decide.
pub trait Desk {
    /// What the diagnostic is doing now.
    fn status(&mut self, text: &str);
    /// A question with yes or no for an answer. No answer is a no.
    fn confirm(&mut self, question: &str) -> bool;
    /// A question with a line of text for an answer. `None` when there is none.
    fn answer(&mut self, question: &str) -> Option<String>;
}

impl Choice {
    fn decide(self, desk: &mut dyn Desk, question: &str) -> bool {
        match self {
            Choice::Yes => true,
            Choice::No => false,
            Choice::Ask => desk.confirm(question),
        }
    }
}

/// The verdict for an install folder from what can be told at once: how the drives are
/// attached, what is free, and which programs run. Nothing is printed, read from the game's
/// files, written or asked of a server, so that the app can call this before a patch
/// starts. `write_test` is what the caller's own test of the folder came to.
pub fn quick_verdict(install: &Path, write_test: WriteTest) -> Verdict {
    let scrub = system::hidden();
    let mut facts = Facts::none_yet(system::now());
    facts.system_drive = volume::look(&system::windows_dir());
    facts.install = Some(install.to_path_buf());
    facts.how = How::Given;
    facts.game_drive = volume::look(install);
    facts.write_test = write_test;
    look_at_programs(&mut facts);
    verdict(&Evidence::of(&facts, &scrub))
}

/// Runs the whole diagnostic. Setting `stop` ends it at the next step, with what was found
/// until then.
pub fn run(options: &Options, desk: &mut dyn Desk, stop: &AtomicBool) -> (Facts, Scrub) {
    let scrub = system::hidden();
    let mut facts = Facts::none_yet(system::now());
    let stopped = || stop.load(Ordering::Relaxed);

    desk.status("PC 확인 중");
    let machine = mfp_engine::preflight::machine();
    facts.started_from = system::started_from();
    facts.windows = system::windows(&machine.os);
    (facts.cpu, facts.cores, facts.threads, facts.ram) =
        (machine.cpu, machine.cores, machine.threads, machine.ram_bytes);
    facts.rights = system::rights();
    facts.webview = Ok(system::webview());
    facts.adapters = system::adapters();
    facts.system_drive = volume::look(&system::windows_dir());
    look_at_programs(&mut facts);

    desk.status("설치 폴더 확인 중");
    locate(options, desk, &mut facts);
    let client = facts.client.client();
    facts.registry = Registry { install_dir: discover::recorded(facts.client), version_hint: client.version_hint() };
    let Some(install) = facts.install.clone() else { return (facts, scrub) };

    facts.game_drive = volume::look(&install);
    let walked = game::walk(&install);
    facts.game = Some(game::look(&install, client.game_program, &walked));

    // What an earlier run could not take away again goes first.
    let (removed, left) = write::remove_leftovers(&install);
    if removed + left > 0 {
        facts.wrote.push(Wrote::Leftovers { removed, left });
    }
    if options.write_test {
        facts.write_test = write::empty_file(&install);
        match facts.write_test {
            WriteTest::Passed => facts.wrote.push(Wrote::EmptyFile { removed: true }),
            WriteTest::LeftBehind(_) => facts.wrote.push(Wrote::EmptyFile { removed: false }),
            WriteTest::Failed(_) | WriteTest::Skipped => {}
        }
    }

    // Reading gigabytes from the drive the game is being played from is not done unasked.
    let running = facts.programs.as_ref().map(|programs| programs.game.clone()).unwrap_or_default();
    let reads_allowed = running.is_empty() || {
        let names: Vec<String> = running.iter().map(|name| scrub.text(name)).collect();
        let question = format!("{} 실행 중입니다. 읽기 측정과 파일 대조를 진행하시겠습니까?", names.join(", "));
        options.heavy_reads.decide(desk, &question)
    };

    let nexon = Nexon::new();
    let given = options.table.as_ref().filter(|table| table.server == facts.client);
    let table = given.unwrap_or_else(|| Table::built_in(facts.client));
    facts.table_given = given.is_some();
    facts.tables[usize::from(facts.client == Server::Test)] = table.newest();
    let newest = if options.online && !stopped() {
        desk.status("넥슨 패치 서버 확인 중");
        server::newest(client, table.newest().version, &nexon)
    } else {
        Err(Unknown::Skipped)
    };

    let mut sheets = table.sheets();
    let mut folder = Folder::default();
    let mut settled = None;
    if reads_allowed && !stopped() {
        desk.status("읽기 측정 중");
        facts.reads = match &walked.largest {
            Some((largest, _)) => read::time(largest, stop),
            None => Err(Unknown::Skipped),
        };
        desk.status("파일 대조 중");
        folder = Folder::look(&install, &sheets);
        let mut cut_short = read_all(&mut folder, &install, QUICK_LIMIT, stop, desk);
        let mut standing = tell_apart(&sheets, &mut folder, &install, stop);
        // The server's patches are asked what they start from when the table built into
        // this program does not settle it, or when the server has moved on since.
        let table_is_behind = newest.is_ok_and(|newest| newest.order() > table.newest().order());
        if let Ok(newest) = newest
            && !cut_short
            && (standing.fits.is_empty() || table_is_behind)
        {
            desk.status("넥슨 패치 서버의 원본 표 확인 중");
            let have: Vec<State> = sheets.iter().map(|sheet| sheet.state).collect();
            let (more, _) = server::sheets(client, newest, &have, &nexon);
            if !more.is_empty() {
                sheets.extend(more);
                folder.look_at_more(&install, &sheets);
                cut_short = read_all(&mut folder, &install, QUICK_LIMIT, stop, desk);
                standing = tell_apart(&sheets, &mut folder, &install, stop);
            }
        }
        let quick = comparison(&sheets, &standing, &folder, &walked, cut_short || stopped());
        settled = quick.at;
        facts.quick = Ok(quick);
    }

    if let Ok(newest) = newest
        && !stopped()
    {
        facts.shelf = Ok(shelf(client, newest, settled, &nexon));
    } else if let Err(why) = newest {
        facts.shelf = Err(why);
    }
    facts.traffic = nexon.traffic();

    // The two steps that are only taken when asked for.
    if reads_allowed && !stopped() {
        let left = folder.unread(u64::MAX);
        let bytes: u64 = left.iter().map(|(_, len)| len).sum();
        let question = format!(
            "전체 대조: 아직 읽지 않은 게임 파일 {}개({})를 모두 읽습니다. 진행하시겠습니까?",
            count(left.len() as u64),
            size(bytes)
        );
        facts.full = if left.is_empty() {
            // The quick comparison read every file there is: it was the full one.
            facts.quick.clone().map_or(Asked::NotPossible, Asked::Done)
        } else if options.full.decide(desk, &question) {
            desk.status("전체 대조 중");
            let cut_short = read_all(&mut folder, &install, u64::MAX, stop, desk);
            let standing = compare::standing(&sheets, &folder.seen);
            Asked::Done(comparison(&sheets, &standing, &folder, &walked, cut_short))
        } else {
            Asked::Declined
        };
    }
    if !stopped() {
        let question = format!(
            "쓰기 위치 확인: 설치 폴더에 {} 임시 파일을 썼다가 지우고, 드라이브별 여유 공간 변화를 기록합니다. 진행하시겠습니까?",
            size(options.probe_bytes)
        );
        facts.landing = if options.write_probe.decide(desk, &question) {
            desk.status("쓰기 위치 확인 중");
            let mut last = 0;
            let landing = write::landing(&install, options.probe_bytes, stop, &mut |written| {
                if written - last >= 64 << 20 {
                    last = written;
                    desk.status(&format!("쓰기 위치 확인 중: {}", size(written)));
                }
            });
            // A file was made unless the folder refused it outright.
            if landing.written > 0 || landing.error.is_none() {
                facts.wrote.push(Wrote::ProbeFile { bytes: landing.written, removed: landing.deleted });
            }
            Asked::Done(landing)
        } else {
            Asked::Declined
        };
    }
    (facts, scrub)
}

/// Notes which of the running programs and drivers matter.
fn look_at_programs(facts: &mut Facts) {
    let games: Vec<&str> =
        Server::ALL.iter().flat_map(|server| [server.client().game_program, server.client().launcher]).collect();
    let drivers = programs::drivers();
    facts.drivers_listed = drivers.as_ref().map(|_| ()).map_err(|why| *why);
    facts.programs = programs::processes()
        .map(|processes| programs::sort(processes.into_iter().chain(drivers.unwrap_or_default()), &games));
}

/// Which client a folder that was named holds: the test world's only when its program is
/// there and the live game's is not.
fn client_in(folder: &Path) -> Server {
    let has = |client: &Client| folder.join(client.game_program).is_file();
    if has(&mfp_kms::TEST) && !has(&mfp_kms::LIVE) { Server::Test } else { Server::Live }
}

/// Finds the install folder: the one that was named, or else by looking, or else by asking.
fn locate(options: &Options, desk: &mut dyn Desk, facts: &mut Facts) {
    if let Some(folder) = &options.folder {
        facts.client = options.server.unwrap_or_else(|| client_in(folder));
        (facts.how, facts.install) = (How::Given, Some(folder.clone()));
        return;
    }
    let servers: Vec<Server> = options.server.map_or(Server::ALL.to_vec(), |server| vec![server]);
    for server in &servers {
        if let Some((how, dir)) = discover::find(server.client(), &Places::of_this_pc(*server)) {
            (facts.client, facts.how, facts.install) = (*server, how, Some(dir));
            return;
        }
    }
    facts.client = servers[0];
    let question = "설치 폴더를 찾지 못했습니다. 메이플스토리 설치 폴더 경로를 입력해 주세요. (건너뛰려면 Enter)";
    while let Some(typed) = desk.answer(question).filter(|typed| !typed.trim().is_empty()) {
        let found = servers.iter().find_map(|server| Some((*server, discover::from_typed(&typed, server.client())?)));
        if let Some((server, dir)) = found {
            (facts.client, facts.how, facts.install) = (server, How::Typed, Some(dir));
            return;
        }
        desk.status("메이플스토리 설치 폴더가 아닙니다.");
    }
}

/// Reads every file that is there, not read yet and no longer than `limit`, shortest first.
/// Says whether it was stopped before the end.
fn read_all(folder: &mut Folder, install: &Path, limit: u64, stop: &AtomicBool, desk: &mut dyn Desk) -> bool {
    let files = folder.unread(limit);
    let total: u64 = files.iter().map(|(_, len)| len).sum();
    let (mut done, mut told) = (0u64, 0u64);
    for (key, len) in files {
        if folder.read(install, &key, stop).is_err() {
            return true;
        }
        done += len;
        // Often enough to show that it is alive, and not so often that it floods a console.
        if done - told >= 512 << 20 {
            told = done;
            desk.status(&format!("파일 대조 중: {} / {}", size(done), size(total)));
        }
    }
    false
}

/// Works out how the folder stands, and while more than one state fits, reads the file that
/// tells them apart, however long it is.
fn tell_apart(sheets: &[Sheet], folder: &mut Folder, install: &Path, stop: &AtomicBool) -> Standing {
    let mut standing = compare::standing(sheets, &folder.seen);
    for _ in 0..32 {
        let Some(key) = compare::tells_apart(sheets, &standing.fits, &folder.seen).map(str::to_owned) else { break };
        if folder.read(install, &key, stop).is_err() {
            break;
        }
        standing = compare::standing(sheets, &folder.seen);
    }
    standing
}

/// What a comparison came to, for the report.
fn comparison(sheets: &[Sheet], standing: &Standing, folder: &Folder, walked: &Walked, cut_short: bool) -> Comparison {
    let against = standing.nearest.map(|at| &sheets[at]);
    let not_in_table = match against {
        Some(sheet) => walked.keys.iter().filter(|key| !sheet.files.contains_key(*key)).count() as u64,
        None => walked.files,
    };
    // While a file that would tell the fitting states apart is still unread, as after a
    // stop, the folder could be at any of them.
    let open = compare::tells_apart(sheets, &standing.fits, &folder.seen).is_some();
    Comparison {
        at: standing.settled(sheets).filter(|_| !open),
        either: if open { standing.fits.iter().map(|at| sheets[*at].state).collect() } else { Vec::new() },
        against: against.map(|sheet| (sheet.state, sheet.source)),
        tally: standing.nearest.map(|at| standing.tallies[at].clone()).unwrap_or_default(),
        not_in_table,
        bytes_read: folder.bytes_read,
        seconds: folder.seconds,
        cut_short,
    }
}

/// What the server has for a folder at `at`: the main patch it would take next, or else the
/// newest minor patch, and how fast the line to the server is.
fn shelf(client: &Client, newest: State, at: Option<State>, nexon: &Nexon) -> Shelf {
    use server::Shelf as _;
    let on_shelf = |url: String, file_name: String| -> Option<PatchOnShelf> {
        let size = nexon.size_of(&url).ok().flatten()?;
        let originals = server::originals_of(&url, nexon).ok().flatten().map(|table| table.len());
        Some(PatchOnShelf { file_name, size, originals })
    };
    let behind = at.filter(|at| at.version < newest.version);
    let next_main = behind.and_then(|at| {
        // Straight to the newest version where the server has that patch, or else one on.
        [newest.version, at.version + 1]
            .into_iter()
            .find_map(|to| on_shelf(client.patch_url(at.version, to), client.patch_file_name(at.version, to)))
    });
    let patch = next_main.clone().or_else(|| {
        let (from, to) = (newest.minor.checked_sub(1).filter(|from| *from >= 1)?, newest.minor);
        on_shelf(client.minor_patch_url(newest.version, from, to), format!("{from:02}to{to:02}.patch"))
    });
    // The newest main patch is the one file there that is sure to be long enough.
    let sample: Fact<(u64, f64)> = match newest.version.checked_sub(1) {
        Some(before) => server::sample(&client.patch_url(before, newest.version), nexon),
        None => Err(Unknown::Skipped),
    };
    Shelf { newest, patch, patch_is_next_main: next_main.is_some(), sample }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known::Expect;
    use crate::verdict::Outlook;

    /// A desk that answers from a script and notes what it was asked.
    #[derive(Default)]
    struct Scripted {
        yes: bool,
        asked: Vec<String>,
        said: Vec<String>,
    }

    impl Desk for Scripted {
        fn status(&mut self, text: &str) {
            self.said.push(text.to_owned());
        }

        fn confirm(&mut self, question: &str) -> bool {
            self.asked.push(question.to_owned());
            self.yes
        }

        fn answer(&mut self, question: &str) -> Option<String> {
            self.asked.push(question.to_owned());
            None
        }
    }

    /// Makes a folder that holds, for every file the newest sheet of the live client's
    /// table names, a file of a few made-up bytes: an install in shape, and in no file the
    /// real one. The game's program is made longer than the quick comparison reads, by
    /// setting its length, which takes no time and no room worth speaking of.
    fn made_up_install(dir: &Path) -> usize {
        let newest = Table::built_in(Server::Live).sheets().pop().unwrap();
        let mut made = 0;
        for (name, expect) in newest.files.values() {
            if matches!(expect, Expect::Gone) {
                continue;
            }
            let path = dir.join(mfp_engine::plan::relative_path(name).unwrap());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name.as_bytes()).unwrap();
            if name == mfp_kms::LIVE.game_program {
                std::fs::File::options().write(true).open(&path).unwrap().set_len(QUICK_LIMIT + 1).unwrap();
            }
            made += 1;
        }
        made
    }

    fn offline(folder: &Path) -> Options {
        Options {
            folder: Some(folder.to_path_buf()),
            online: false,
            heavy_reads: Choice::Yes,
            probe_bytes: 3 << 20,
            ..Options::default()
        }
    }

    #[test]
    fn folder_that_was_named_is_diagnosed_without_looking_anywhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let made = made_up_install(dir.path());
        let mut desk = Scripted::default();
        let (facts, scrub) = run(&offline(dir.path()), &mut desk, &AtomicBool::new(false));

        assert_eq!((facts.how, facts.client), (How::Given, Server::Live));
        assert_eq!(facts.install.as_deref(), Some(dir.path()));
        assert!(facts.system_drive.is_ok() && facts.game_drive.is_ok());
        assert_eq!(facts.game.as_ref().unwrap().files.unwrap().0, made as u64);
        // The empty file was made and removed, and that is all that was written: both
        // questions were answered with no.
        assert_eq!(facts.write_test, WriteTest::Passed);
        assert_eq!(facts.wrote, [Wrote::EmptyFile { removed: true }]);
        assert_eq!((&facts.full, &facts.landing), (&Asked::Declined, &Asked::Declined));
        assert!(desk.asked.iter().any(|question| question.starts_with("전체 대조: ")), "{:?}", desk.asked);
        assert!(desk.asked.iter().any(|question| question.starts_with("쓰기 위치 확인: 설치 폴더에 3 MB 임시 파일")));
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().starts_with(".mfp"))
                .count(),
            0
        );
        // No server was asked.
        assert_eq!(facts.shelf, Err(Unknown::Skipped));
        assert_eq!(facts.traffic.requests, 0);

        // Every file is made-up, so the folder fits no version: the files were read and
        // found to differ, by checksum or by length.
        let quick = facts.quick.as_ref().expect("the files were not compared");
        assert_eq!(quick.at, None);
        assert_eq!(quick.tally.same, 0);
        assert!(quick.tally.differs.len() > 1000, "{}", quick.tally.differs.len());
        assert!(quick.tally.missing.is_empty());
        // Without the server it cannot be told whether that is a newer version or a fault,
        // so the patch is not said to work and not said to fail.
        let verdict = crate::report::verdict_of(&facts, &scrub);
        assert_eq!(verdict.patch, Outlook::Unknown);
        assert!(
            verdict.patch_facts.iter().any(|fact| fact.contains("서버 최신 버전을 확인하지 못해")),
            "{:?}",
            verdict.patch_facts
        );
        // And the report of it, made on this very PC, does not hold the name of whoever
        // runs the tests. (A name that is part of the placeholder itself proves nothing.)
        let report = crate::report::render(&facts, &scrub);
        assert!(report.lines().count() >= 25, "{} lines", report.lines().count());
        if let Ok(user) = std::env::var("USERNAME")
            && user.chars().count() >= 3
            && !"%userprofile%".contains(&user.to_lowercase())
        {
            assert!(!report.to_lowercase().contains(&user.to_lowercase()), "the user's name is in the report");
        }
    }

    #[test]
    fn steps_that_were_agreed_to_are_taken_and_leave_the_folder_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let options = Options { full: Choice::Yes, write_probe: Choice::Yes, ..offline(dir.path()) };
        let mut desk = Scripted::default();
        let (facts, _) = run(&options, &mut desk, &AtomicBool::new(false));

        let Asked::Done(landing) = &facts.landing else { panic!("the probe was not run: {:?}", facts.landing) };
        assert_eq!((landing.written, landing.error, landing.deleted), (3 << 20, None, true));
        assert_eq!(
            facts.wrote,
            [Wrote::EmptyFile { removed: true }, Wrote::ProbeFile { bytes: 3 << 20, removed: true }]
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        // The full comparison read what the quick one had told by length alone.
        let Asked::Done(full) = &facts.full else { panic!("the full comparison was not run: {:?}", facts.full) };
        assert!(
            full.tally.differs.len() > 1000 && full.tally.same_length == 0 && !full.cut_short,
            "{:?}",
            full.tally.same_length
        );
        assert!(full.bytes_read > facts.quick.as_ref().unwrap().bytes_read);
        assert!(desk.asked.is_empty(), "{:?}", desk.asked);
    }

    #[test]
    fn nothing_is_written_when_the_write_test_is_left_out_and_the_probe_declined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Data")).unwrap();
        std::fs::write(dir.path().join("MapleStoryT.exe"), b"the test world").unwrap();
        // A file an earlier run left is taken away, and that is said.
        std::fs::write(dir.path().join(".mfp-check-write-probe-1.tmp"), b"left").unwrap();
        let options = Options { write_test: false, heavy_reads: Choice::No, ..offline(dir.path()) };
        let (facts, _) = run(&options, &mut Scripted::default(), &AtomicBool::new(false));
        assert_eq!(facts.client, Server::Test);
        assert_eq!(facts.write_test, WriteTest::Skipped);
        assert_eq!(facts.wrote, [Wrote::Leftovers { removed: 1, left: 0 }]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn stop_ends_the_run_before_anything_is_read_or_asked() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let mut desk = Scripted { yes: true, ..Scripted::default() };
        let (facts, _) = run(&offline(dir.path()), &mut desk, &AtomicBool::new(true));
        assert!(facts.quick.is_err() && facts.reads.is_err());
        assert_eq!((&facts.full, &facts.landing), (&Asked::NotPossible, &Asked::NotPossible));
        assert!(desk.asked.is_empty(), "{:?}", desk.asked);
    }

    #[test]
    fn app_gets_a_verdict_for_a_folder_without_anything_being_written_or_read() {
        let dir = tempfile::tempdir().unwrap();
        made_up_install(dir.path());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let verdict = quick_verdict(dir.path(), WriteTest::Passed);
        // Whatever kind of PC the tests run on, the answer comes with facts.
        assert!(!verdict.kind_facts.is_empty() && !verdict.patch_facts.is_empty() && !verdict.kept_facts.is_empty());
        assert!(verdict.patch_facts.iter().any(|fact| fact == "파일 대조 안 함") || verdict.patch == Outlook::Unlikely);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
        // A folder that takes no writes is said to take no patch.
        assert_eq!(quick_verdict(dir.path(), WriteTest::Failed(Some(5))).patch, Outlook::Unlikely);
    }
}
