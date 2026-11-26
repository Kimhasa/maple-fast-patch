//! Tests the made-up world: its patch files apply as real ones do, its server answers what
//! the downloader asks, what it tells of maintenance is read as it was meant, and the
//! stand-in for the game writes down that it was started.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use mfp_engine::container::PatchFile;
use mfp_engine::preflight::Problem;
use mfp_engine::{Error, Options, Outcome, Source, download, run_for};
use mfp_kms::Server;
use mfp_kms::notice::{self, Minutes, Notice, Reading, Watch};
use mfp_tools::clients::programs;
use mfp_tools::fake_notice::{self, BOARD_FILE, Change, INSPECTION_FILE, TEST_BOARD_FILE};
use mfp_tools::fake_server;
use mfp_tools::fake_world::{self, EXPECTED_DIR, HELD_DIR, Patch, SERVED_DIR, Version, World};

/// Never set: a run that is not asked to pause.
static GO: AtomicBool = AtomicBool::new(false);

/// The stand-in for a game's programs, as cargo built it for these tests.
const LAUNCHER: &str = env!("CARGO_BIN_EXE_fake_launcher");

/// The moment the worlds of these tests are made at: 2026-11-26 13:05 in Korea.
const NOW: Minutes = 20_783 * 1440 + 13 * 60 + 5;

/// Makes a world of 1 MB in `dir`: two main versions and `minors` minor patches.
fn make(dir: &Path, server: Server, minors: u32) -> World {
    let options = fake_world::Options { megabytes: 1, minors, ..fake_world::Options::new(server) };
    fake_world::make(dir, &options, Path::new(LAUNCHER), NOW).unwrap()
}

/// Runs a patch file of the world at `from`, published or not, on `install`, as a folder of
/// the client of `folder_of`.
fn apply(folder_of: &World, install: &Path, from: &Path, patch: &Patch) -> Result<Outcome, Error> {
    let held = from.join(HELD_DIR).join(&patch.path);
    let file = if held.is_file() { held } else { from.join(SERVED_DIR).join(&patch.path) };
    let options = Options {
        install_dir: install.to_path_buf(),
        source: Source::File(file),
        immediate: false,
        storage: Default::default(),
    };
    run_for(&programs(folder_of.client()), &options, &GO, &mut |_| {})
}

/// Copies a folder and everything in it.
fn copy_folder(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for child in fs::read_dir(from).unwrap().flatten() {
        let (source, dest) = (child.path(), to.join(child.file_name()));
        if source.is_dir() {
            copy_folder(&source, &dest);
        } else {
            fs::copy(&source, &dest).unwrap();
        }
    }
}

#[test]
fn made_up_chain_applies_end_to_end_for_each_client() {
    for server in Server::ALL {
        let what = server.key();
        let dir = tempfile::tempdir().unwrap();
        let world = make(dir.path(), server, 3);
        let (client, install) = (world.client(), fake_world::install_dir(dir.path()));
        assert_eq!(World::read(dir.path()).unwrap(), world, "{what}: the world is not read back as it was made");
        assert!(install.join(client.game_program).is_file() && install.join(client.launcher).is_file(), "{what}");
        assert_eq!(world.version_of(&install), Some(world.installed), "{what}");

        // The files lie where Nexon's server has them for this client.
        let first = world.installed.number;
        assert!(
            client.patch_url(first, first + 1).ends_with(&world.patches[0].path),
            "{what}: {}",
            world.patches[0].path
        );
        let minors: Vec<&Patch> = world.patches.iter().filter(|patch| patch.from.number == patch.to.number).collect();
        for patch in &minors {
            let url = client.minor_patch_url(patch.to.number, patch.from.minor, patch.to.minor);
            assert!(url.ends_with(&patch.path), "{what}: {}", patch.path);
        }
        // Two main versions, three minor steps on the newest, and a patch for every jump:
        // 1 to 3, 1 to 4 and 2 to 4.
        assert_eq!((world.steps().len(), minors.len(), world.patches.len()), (5, 6, 8), "{what}");
        // The first minor patch holds whole files only: its table of originals is empty.
        let empty: Vec<&&Patch> = minors.iter().filter(|patch| patch.originals == 0).collect();
        assert_eq!(empty.len(), 1, "{what}");
        assert_eq!((empty[0].from.minor, empty[0].to.minor), (1, 2), "{what}");

        // Step by step the install becomes what `make` said it would, to the newest version.
        // On the way a copy is kept of every minor version, for the jumps below.
        let mut at_minor = Vec::new();
        for patch in world.steps() {
            if patch.from.number == world.newest().number {
                at_minor.push((patch.from, dir.path().join(format!("at-minor-{}", patch.from.minor))));
                copy_folder(&install, &at_minor.last().unwrap().1);
            }
            let outcome =
                apply(&world, &install, dir.path(), patch).unwrap_or_else(|e| panic!("{what} {}: {e}", patch.path));
            assert!(outcome.files_changed > 0 && !outcome.immediate, "{what} {}: {outcome:?}", patch.path);
            assert_eq!(world.version_of(&install), Some(patch.to), "{what}: after {}", patch.path);
        }
        assert_eq!(world.version_of(&install), Some(world.newest()), "{what}");
        assert_eq!(world.newest(), Version { number: first + 2, minor: 4 }, "{what}");
        let expected =
            fs::read_to_string(dir.path().join(EXPECTED_DIR).join(format!("{:05}-04.txt", first + 2))).unwrap();
        assert_eq!(fake_world::manifest_of(&install), expected, "{what}");

        // A patch that jumps several minor versions ends where the steps end.
        let jumps: Vec<&Patch> = minors.iter().copied().filter(|patch| patch.to.minor > patch.from.minor + 1).collect();
        assert_eq!(jumps.len(), 3, "{what}");
        for patch in jumps {
            let (_, start) = at_minor.iter().find(|(version, _)| *version == patch.from).unwrap();
            let folder = dir.path().join(format!("jump-{}-{}", patch.from.minor, patch.to.minor));
            copy_folder(start, &folder);
            assert!(patch.originals > 0, "{what}: {} has no table", patch.path);
            apply(&world, &folder, dir.path(), patch).unwrap_or_else(|e| panic!("{what} {}: {e}", patch.path));
            assert_eq!(world.version_of(&folder), Some(patch.to), "{what}: after {}", patch.path);
        }
    }
}

#[test]
fn served_tree_answers_what_the_downloader_asks() {
    let dir = tempfile::tempdir().unwrap();
    let world = make(dir.path(), Server::Live, 2);
    let downloads = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let address = fake_server::serve(
        dir.path().to_path_buf(),
        0,
        None,
        Arc::new(move |line| log.lock().unwrap().push(line.to_owned())),
    )
    .unwrap();
    assert!(address.ip().is_loopback());
    let first = world.steps()[0];
    let url = format!("http://{address}/{}", first.path);

    // A file that is not published yet is answered as Nexon's server answers.
    assert!(matches!(download::probe(&url), Err(Error::NotPublished)));
    assert!(requests.lock().unwrap().last().unwrap().ends_with("-> 403, 0 bytes"));
    assert_eq!(fake_world::publish(dir.path(), "00900to00901.patch").unwrap(), std::slice::from_ref(&first.path));

    // Published, it is there: its length, its beginning, and all of it, a patch file the
    // engine opens. A file that is whole on this PC is not fetched again.
    let served = fs::read(dir.path().join(SERVED_DIR).join(&first.path)).unwrap();
    let remote = download::probe(&url).unwrap();
    assert_eq!(remote.len, first.bytes);
    assert!(remote.etag.is_some());
    assert_eq!(download::fetch_prefix(&url, 16).unwrap(), served[..16]);
    let dest = downloads.path().join("first.patch");
    download::fetch(&url, &dest, &GO, &mut |_, _| {}).unwrap();
    assert!(fs::read(&dest).unwrap() == served);
    PatchFile::open(&dest).unwrap().verify(&GO, |_| {}).unwrap();
    let asked = requests.lock().unwrap().len();
    download::fetch(&url, &dest, &GO, &mut |_, _| {}).unwrap();
    let again: Vec<String> = requests.lock().unwrap()[asked..].to_vec();
    assert!(again.len() == 1 && again[0].contains("] HEAD "), "{again:?}");

    // A large file, cut off after its first bytes and taken up again from where it stopped.
    let large: Vec<u8> = (0..20u32 << 20).map(|at| (at.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    fs::write(dir.path().join(SERVED_DIR).join("Patch").join("large.patch"), &large).unwrap();
    let (large_url, large_dest) = (format!("http://{address}/Patch/large.patch"), downloads.path().join("large.patch"));
    let stop = AtomicBool::new(false);
    let cut_off =
        download::fetch(&large_url, &large_dest, &stop, &mut |done, _| stop.store(done > 0, Ordering::Relaxed));
    assert!(matches!(cut_off, Err(Error::Stopped)), "{cut_off:?}");
    let kept = fs::metadata(download::part_path(&large_dest)).unwrap().len();
    assert!(0 < kept && kept < large.len() as u64, "{kept} bytes were kept");
    download::fetch(&large_url, &large_dest, &GO, &mut |_, _| {}).unwrap();
    assert!(fs::read(&large_dest).unwrap() == large);
    let requests = requests.lock().unwrap();
    assert!(
        requests.iter().any(|line| line.contains(&format!("large.patch bytes {kept}-")) && line.contains("-> 206")),
        "the rest was not asked for from byte {kept}: {requests:?}"
    );
    // Every request was noted with its time, and nothing outside the served tree is given.
    assert!(requests.iter().all(|line| line.starts_with("[20") && line.contains("] ")), "{requests:?}");
    drop(requests);
    for outside in
        ["world.txt", "../world.txt", "install/Data/Base/Base_000.wz", "not-yet/Patch/00902/00901to00902.patch"]
    {
        let answer = download::probe(&format!("http://{address}/{outside}"));
        assert!(matches!(answer, Err(Error::NotPublished)), "{outside}: {answer:?}");
    }

    // A server that sends at a pace sends the same file, only later.
    let slow = fake_server::serve(dir.path().to_path_buf(), 0, Some(1 << 20), Arc::new(|_| {})).unwrap();
    let paced = downloads.path().join("paced.patch");
    download::fetch(&format!("http://{slow}/{}", first.path), &paced, &GO, &mut |_, _| {}).unwrap();
    assert!(fs::read(&paced).unwrap() == served);
}

#[test]
fn patch_files_appear_when_they_are_published_and_a_world_is_only_made_in_an_empty_folder() {
    let dir = tempfile::tempdir().unwrap();
    let world = make(dir.path(), Server::Test, 2);
    assert!(fake_world::published(dir.path()).is_empty());
    let path = |ending: &str| world.patches.iter().find(|patch| patch.path.ends_with(ending)).unwrap().path.clone();

    // An older main patch appears alone. The one to the newest version brings the first
    // minor patch with it: that is how the test world publishes a version.
    assert_eq!(fake_world::publish(dir.path(), "01900to01901.patch").unwrap(), [path("01900to01901.patch")]);
    assert_eq!(
        fake_world::publish(dir.path(), "01901to01902.patch").unwrap(),
        [path("01901to01902.patch"), path("Minor/01to02.patch")]
    );
    assert!(fake_world::publish(dir.path(), "01901to01902.patch").unwrap_err().contains("published already"));
    assert!(fake_world::publish(dir.path(), "09999to10000.patch").unwrap_err().contains("no patch file"));
    assert!(fake_world::publish(dir.path(), ".patch").unwrap_err().contains("could be any of"));
    assert_eq!(
        fake_world::publish(dir.path(), "all").unwrap(),
        [path("Minor/02to03.patch"), path("Minor/01to03.patch")]
    );
    assert!(fake_world::publish(dir.path(), "all").is_err());
    assert_eq!(fake_world::published(dir.path()).len(), world.patches.len());
    for patch in &world.patches {
        assert!(
            dir.path().join(SERVED_DIR).join(&patch.path).is_file()
                && !dir.path().join(HELD_DIR).join(&patch.path).exists()
        );
    }

    // The live game's minor patches come on their own day.
    let live_dir = tempfile::tempdir().unwrap();
    make(live_dir.path(), Server::Live, 2);
    assert_eq!(fake_world::publish(live_dir.path(), "00901to00902.patch").unwrap().len(), 1);

    // A folder that holds something is not made a world of, and neither is one where the
    // real games live.
    let options = fake_world::Options::new(Server::Live);
    assert!(fake_world::make(dir.path(), &options, Path::new(LAUNCHER), NOW).unwrap_err().contains("empty"));
    let real = live_dir.path().join("Nexon").join("Maple");
    assert!(fake_world::make(&real, &options, Path::new(LAUNCHER), NOW).unwrap_err().contains("Nexon"));
    assert!(!real.exists());
}

#[test]
fn patch_with_a_table_is_refused_for_the_folder_of_the_other_client() {
    let (live_dir, test_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (live, test) = (make(live_dir.path(), Server::Live, 2), make(test_dir.path(), Server::Test, 2));
    for (patches, from, folder, folder_dir) in
        [(&live, live_dir.path(), &test, test_dir.path()), (&test, test_dir.path(), &live, live_dir.path())]
    {
        let what =
            format!("a patch of the {} client on a folder of the {} client", patches.server.key(), folder.server.key());
        let install = fake_world::install_dir(folder_dir);
        let patch = patches.steps()[0];
        assert!(patch.originals > 0, "{what}");

        // The folder is its own client's, so it passes as an install folder. What stops
        // the patch is its table: the files there are not the ones it was made for.
        let err = apply(folder, &install, from, patch).unwrap_err();
        assert!(matches!(&err, Error::OriginalMismatch(files) if !files.is_empty()), "{what}: {err}");
        assert_eq!(folder.version_of(&install), Some(folder.installed), "{what}: the folder was changed");
        // Asked about as a folder of the patch's own client, it is not that client's folder.
        let err = apply(patches, &install, from, patch).unwrap_err();
        let missing = patches.client().game_program;
        assert!(
            matches!(&err, Error::Blocked(problems) if matches!(&problems[..], [Problem::NotAnInstallDir { missing: named, .. }] if *named == missing)),
            "{what}: {err}"
        );
    }
}

#[test]
fn minor_patch_with_an_empty_table_is_not_refused_for_the_folder_of_the_other_client() {
    // This is not how it should be. It is how it is: a patch that holds whole files only
    // says nothing of the install it is for, and the engine has nothing to refuse it by.
    // Whoever picks the patch file has to pick it for the right client.
    let (live_dir, test_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (live, test) = (make(live_dir.path(), Server::Live, 2), make(test_dir.path(), Server::Test, 2));
    for (patches, from, folder, folder_dir) in
        [(&live, live_dir.path(), &test, test_dir.path()), (&test, test_dir.path(), &live, live_dir.path())]
    {
        let what =
            format!("a patch of the {} client on a folder of the {} client", patches.server.key(), folder.server.key());
        let install = fake_world::install_dir(folder_dir);
        let patch =
            patches.patches.iter().find(|patch| patch.originals == 0).expect("a minor patch with an empty table");

        let outcome = apply(folder, &install, from, patch).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(outcome.files_changed > 0, "{what}: {outcome:?}");
        // The folder now holds files of the other client, and is at no version of its own.
        assert_eq!(folder.version_of(&install), None, "{what}");
        let theirs =
            fs::read_to_string(from.join(EXPECTED_DIR).join(format!("{:05}-02.txt", patch.to.number))).unwrap();
        let written: Vec<&str> =
            theirs.lines().filter(|line| line.contains("String_000") || line.contains("UI_000")).collect();
        let found = fake_world::manifest_of(&install);
        assert!(written.len() == 2 && written.iter().all(|line| found.contains(line)), "{what}: {found}");
    }
}

/// Reads what the world at `dir` tells of the maintenance of `server`, the way the app does:
/// the inspection service where it lists the client, and the client's board.
fn look(dir: &Path, server: Server, watch: &mut Watch, now: Minutes) -> (Reading, Vec<Notice>) {
    let text = |file: &str| fs::read_to_string(dir.join(file)).unwrap();
    let (planned, board) = match server {
        Server::Live => (notice::read_inspection(&text(INSPECTION_FILE)), text(BOARD_FILE)),
        Server::Test => (Vec::new(), text(TEST_BOARD_FILE)),
    };
    let notices = notice::read_board(&board);
    (watch.read(&planned, &notices, now), notices)
}

#[test]
fn maintenance_is_read_from_the_files_as_it_was_told() {
    for server in Server::ALL {
        let what = server.key();
        let dir = tempfile::tempdir().unwrap();
        make(dir.path(), server, 2);
        let change =
            |change: Change, now: Minutes| fake_notice::change(dir.path(), server, change, "1.2.902", now).unwrap();
        let mut watch = Watch::default();

        // A world that was just made tells of nothing but maintenance that is long over.
        let (reading, notices) = look(dir.path(), server, &mut watch, NOW);
        assert_eq!(reading, Reading::default(), "{what}");
        assert!(notices.len() == 2 && notices.iter().all(Notice::finished), "{what}: {notices:?}");
        assert_eq!(notice::read_inspection(&fs::read_to_string(dir.path().join(INSPECTION_FILE)).unwrap()).len(), 1);
        assert!(fake_notice::change(dir.path(), server, Change::Extend, "1.2.902", NOW).is_err(), "{what}");

        // Announced for later: on the board with its day and hours, and the game still open.
        let told = change(Change::Announce { minutes: 30 }, NOW);
        let (reading, notices) = look(dir.path(), server, &mut watch, NOW);
        assert_eq!(notices[0].title, told.title(server), "{what}");
        assert!(notices[0].closes_game() && !notices[0].finished() && !notices[0].extended(), "{what}: {notices:?}");
        assert_eq!(notices[0].window(NOW), Some((NOW + 30, NOW + 90)), "{what}");
        assert!(reading.underway.is_none() && !reading.soon, "{what}: {reading:?}");
        // When its time has come it is underway, without anything being written anew.
        let (reading, _) = look(dir.path(), server, &mut watch, NOW + 31);
        assert_eq!(reading.underway.map(|underway| underway.ends), Some(NOW + 90), "{what}");

        // Begun now for an hour.
        let (mut watch, begun) = (Watch::default(), NOW + 200);
        let told = change(Change::Begin, begun);
        assert_eq!((told.begins, told.ends, told.extensions, told.over), (begun, begun + 60, 0, false), "{what}");
        let (reading, notices) = look(dir.path(), server, &mut watch, begun);
        let underway = reading.underway.clone().unwrap_or_else(|| panic!("{what}: nothing is underway: {notices:?}"));
        assert!(reading.soon && !reading.ended && reading.extended.is_none(), "{what}: {reading:?}");
        assert_eq!((underway.ends, underway.extended), (begun + 60, false), "{what}");
        assert_eq!(notices.len(), 3, "{what}: {notices:?}");

        // Extended by an hour, and by another: marked so in the title, with the new hours.
        let told = change(Change::Extend, begun + 50);
        let (reading, notices) = look(dir.path(), server, &mut watch, begun + 50);
        assert_eq!(reading.extended, Some(begun + 120), "{what}: {reading:?}");
        assert!(
            reading.underway.as_ref().is_some_and(|underway| underway.extended && underway.ends == begun + 120),
            "{what}"
        );
        assert!(notices[0].extended() && !notices[0].finished() && notices[0].title.starts_with("(연장)"), "{what}");
        assert_eq!(notices[0].title, told.title(server), "{what}");
        change(Change::Extend, begun + 110);
        let (reading, notices) = look(dir.path(), server, &mut watch, begun + 110);
        assert_eq!(reading.extended, Some(begun + 180), "{what}: {reading:?}");
        assert!(notices[0].title.starts_with("(연장2)"), "{what}: {}", notices[0].title);

        // Over, before its planned end: the board says so in each board's own word.
        let told = change(Change::End, begun + 130);
        let (reading, notices) = look(dir.path(), server, &mut watch, begun + 130);
        assert!(reading.ended && reading.underway.is_none() && !reading.soon, "{what}: {reading:?}");
        assert!(told.over && notices[0].finished() && notices[0].extended(), "{what}: {notices:?}");
        let (words, over) = match server {
            Server::Live => ("ver1.2.902 클라이언트 패치(", "(연장2)[패치완료] "),
            Server::Test => ("전체 테스트월드 채널 점검 (", "(연장2)[점검종료] "),
        };
        assert!(notices[0].title.contains(words) && notices[0].title.starts_with(over), "{what}: {}", notices[0].title);
        assert!(fake_notice::change(dir.path(), server, Change::End, "1.2.902", begun + 131).is_err(), "{what}");

        // The other client's board has heard of none of this.
        let other = if server == Server::Live { Server::Test } else { Server::Live };
        let (reading, notices) = look(dir.path(), other, &mut Watch::default(), begun + 130);
        assert!(reading == Reading::default() && notices.len() == 2, "{what}: {notices:?}");
    }
}

#[test]
fn maintenance_that_runs_past_midnight_is_told_with_the_day_it_began() {
    // 23:40 on November 26 in Korea, for an hour, then extended.
    let dir = tempfile::tempdir().unwrap();
    make(dir.path(), Server::Live, 2);
    let late = 20_783 * 1440 + 23 * 60 + 40;
    let told = fake_notice::change(dir.path(), Server::Live, Change::Begin, "1.2.902", late).unwrap();
    assert_eq!(told.title(Server::Live), "11/26(목) ver1.2.902 클라이언트 패치(23:40~0:40)");
    assert_eq!(told.title(Server::Test), "11/26(목) 전체 테스트월드 채널 점검 (23:40~00:40)");
    let inspection = fs::read_to_string(dir.path().join(INSPECTION_FILE)).unwrap();
    assert!(inspection.contains("<startDateTime>2026-11-26T23:40:00+09:00</startDateTime>"), "{inspection}");
    assert!(inspection.contains("<endDateTime>2026-11-27T00:40:00+09:00</endDateTime>"), "{inspection}");
    assert!(inspection.contains("[패치중] 11/26(목) ver1.2.902 클라이언트 패치(23:40~00:40)"), "{inspection}");

    let mut watch = Watch::default();
    let after_midnight = late + 45;
    let (reading, _) = look(dir.path(), Server::Live, &mut watch, after_midnight);
    assert_eq!(reading.underway.map(|underway| underway.ends), Some(late + 60));
    fake_notice::change(dir.path(), Server::Live, Change::Extend, "1.2.902", after_midnight).unwrap();
    let (reading, notices) = look(dir.path(), Server::Live, &mut watch, after_midnight);
    assert_eq!(reading.extended, Some(late + 120));
    assert_eq!(notices[0].title, "(연장)11/26(목) ver1.2.902 클라이언트 패치(23:40~1:40)");
}

#[test]
fn stand_in_for_the_game_writes_down_that_it_was_started() {
    let dir = tempfile::tempdir().unwrap();
    let world = make(dir.path(), Server::Test, 2);
    let install = fake_world::install_dir(dir.path());
    let launcher = install.join(world.client().launcher);

    // With the variable set, the line goes to the file it names.
    let noted = dir.path().join("started.txt");
    let started =
        Command::new(&launcher).args(["-mode:pluglaunch", "-game:589826"]).env("MFP_FAKE_LAUNCHED", &noted).status();
    assert!(started.unwrap().success());
    let text = fs::read_to_string(&noted).unwrap();
    let fields: Vec<&str> = text.trim_end().split('\t').collect();
    assert_eq!(fields[1..], ["GameLauncherT.exe", "-mode:pluglaunch -game:589826"], "{text:?}");
    assert!(fields[0].len() == 19 && fields[0].starts_with("20") && fields[0].as_bytes()[10] == b' ', "{text:?}");

    // Without it, to a file beside the program, one line for every start. The game's own
    // program is the same stand-in under the other name.
    for program in [world.client().launcher, world.client().game_program] {
        assert!(Command::new(install.join(program)).env_remove("MFP_FAKE_LAUNCHED").status().unwrap().success());
    }
    let beside = fs::read_to_string(install.join("fake-launched.txt")).unwrap();
    let names: Vec<&str> = beside.lines().map(|line| line.split('\t').nth(1).unwrap()).collect();
    assert_eq!(names, ["GameLauncherT.exe", "MapleStoryT.exe"]);
    // What it writes is beside the game's data and not in it: the install is as it was.
    assert_eq!(world.version_of(&install), Some(world.installed));
}
