//! Tests of the records: that each is read and written as it always was, and of what they say
//! of the version of a folder.

use super::*;
use crate::harness::StandIns;
use crate::made::Registered;
use crate::offer::Published;
use crate::scratch::Scratch;

fn left(percent: f64, files: usize, pauses: u32) -> Left {
    Left { patch: "00420to00421.patch".to_owned(), percent, active: Duration::from_millis(68_270), files, pauses }
}

#[test]
fn what_a_run_left_is_read_back_as_it_was_written() {
    let record = left(69.25, 470, 1);
    assert_eq!(record.written(), "00420to00421.patch\n69.25\n68270\n470\n1");
    assert_eq!(Left::read(&record.written()), Some(record));
}

#[test]
fn a_record_from_before_the_counts_counts_none() {
    assert_eq!(Left::read("00420to00421.patch\n69.25\n68270"), Some(left(69.25, 0, 0)));
}

#[test]
fn a_patch_the_user_gave_the_address_of_is_on_record_by_that_address() {
    let address = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
    let record = Left { patch: address.to_owned(), ..left(12.5, 0, 2) };
    assert_eq!(record.written(), format!("{address}\n12.50\n68270\n0\n2"));
    assert_eq!(Left::read(&record.written()), Some(record));
}

#[test]
fn a_run_is_never_taken_up_inside_the_apply() {
    assert_eq!(Left::read("a.patch\n99.40\n1000\n3\n0").map(|left| left.percent), Some(95.0));
}

#[test]
fn a_record_that_is_cut_short_or_not_one_is_no_record() {
    for record in ["", "a.patch", "a.patch\n69.01", "a.patch\nsixty\n1000", "a.patch\n69.01\nlong"] {
        assert_eq!(Left::read(record), None, "{record:?}");
    }
}

/// The figure on record is what a restart shows. The screen shows whole percent, so the
/// two are the same figure, and the record is never ahead of where the run was.
#[test]
fn the_figure_on_record_is_the_figure_that_was_shown() {
    for shown in [0.0, 6.99, 67.43, 67.996, 68.0, 69.004, 94.999] {
        let read = Left::read(&left(shown, 0, 0).written()).unwrap().percent;
        assert_eq!(read.floor(), f64::floor(shown), "{shown}");
        assert!(read <= shown && shown - read < 0.011, "{shown} was read back as {read}");
    }
}

#[test]
fn a_record_is_replaced_whole_and_leaves_nothing_beside_it() {
    let scratch = Scratch::new("write-whole");
    let path = scratch.0.join("run-left");
    write_whole(&path, &left(10.0, 0, 0).written()).unwrap();
    write_whole(&path, &left(69.0, 470, 1).written()).unwrap();
    assert_eq!(Left::read(&std::fs::read_to_string(&path).unwrap()), Some(left(69.0, 470, 1)));
    let beside: Vec<_> = std::fs::read_dir(&scratch.0).unwrap().flatten().map(|file| file.file_name()).collect();
    assert_eq!(beside, ["run-left"]);
}

/// A write that was cut off has only got as far as the other name. What was there
/// before is still there, whole.
#[test]
fn a_write_that_was_cut_off_leaves_the_record_as_it_was() {
    let scratch = Scratch::new("cut-off");
    let path = scratch.0.join("run-left");
    write_whole(&path, &left(67.0, 400, 0).written()).unwrap();
    std::fs::write(path.with_extension(FRESH), "00420to00421.patch\n69").unwrap();
    assert_eq!(Left::read(&std::fs::read_to_string(&path).unwrap()), Some(left(67.0, 400, 0)));
}

// From here on the records are files, written out below in exactly the forms the
// release before the sites wrote them. A test that fails here has found a change that
// would make the app forget what it knew of somebody's install.

/// The mark of the made-up install: its game program is this long and was last written
/// at this second.
const MARK: &str = "4096 1764000000";

/// A site of the live game with a data folder of its own, and beside it an install
/// folder the registry knows nothing of, with the mark [`MARK`].
struct Made {
    site: Site,
    install: PathBuf,
    _scratch: Scratch,
}

impl Made {
    fn new(name: &str) -> Made {
        Made::with(name, StandIns::default())
    }

    fn with(name: &str, stand_ins: StandIns) -> Made {
        Made::set_up(name, stand_ins, |_| Registered::nowhere())
    }

    /// The same with the install folder being the one the Nexon installer registered,
    /// and `version` what the registry gives for it.
    fn registered(name: &str, version: u32) -> Made {
        Made::set_up(name, StandIns::default(), |install| Registered::at(install, version))
    }

    /// The site with `stand_ins` for Nexon's side, and what `registry` makes of the
    /// install folder as what the registry says.
    fn set_up(name: &str, stand_ins: StandIns, registry: impl FnOnce(&Path) -> Box<Registered>) -> Made {
        let scratch = Scratch::new(name);
        let install = scratch.0.join("Maple");
        std::fs::create_dir_all(&install).unwrap();
        let program = install.join("MapleStory.exe");
        std::fs::write(&program, vec![0; 4096]).unwrap();
        let written = std::time::UNIX_EPOCH + Duration::from_secs(1_764_000_000);
        std::fs::File::options().write(true).open(&program).unwrap().set_modified(written).unwrap();
        let site = Site::of(&mfp_kms::LIVE, scratch.0.join("data"), stand_ins, registry(&install));
        Made { site, install, _scratch: scratch }
    }

    /// Puts the record `name` of the install folder there as `text`.
    fn given(&self, name: &str, text: &str) {
        std::fs::write(self.record(name), text).unwrap();
    }

    fn record(&self, name: &str) -> PathBuf {
        records(&self.site, &self.install).unwrap().join(name)
    }

    /// What the record `name` holds now.
    fn kept(&self, name: &str) -> String {
        std::fs::read_to_string(self.record(name)).unwrap()
    }
}

#[test]
fn everything_of_a_site_is_kept_in_its_data_folder() {
    let made = Made::new("layout");
    let data = &made.site.data;
    assert_eq!(patch_cache(&made.site), data.join("patches"));
    let dir = records(&made.site, &made.install).unwrap();
    assert_eq!(dir.parent(), Some(data.join("installs").as_path()));
    assert!(dir.is_dir());
    // Another folder has records of its own, and the same folder the same ones however
    // its path is written.
    let other = made.install.with_file_name("Copy");
    std::fs::create_dir_all(&other).unwrap();
    assert_ne!(records(&made.site, &other).unwrap(), dir);
    let shouted = PathBuf::from(made.install.to_string_lossy().to_uppercase());
    assert_eq!(records(&made.site, &shouted).unwrap(), dir);
}

/// The name of the folder with an install's records, for the live game where the Nexon
/// installer puts it. The path is the one Windows gives for the folder itself, and the
/// name is the one the release keeps that folder's records under.
#[test]
fn the_records_of_an_install_folder_are_found_under_the_same_name_as_before() {
    assert_eq!(install_key(Path::new(r"\\?\C:\Nexon\Maple")), "4f70a139");
    assert_eq!(install_key(Path::new(r"\\?\c:\NEXON\maple")), install_key(Path::new(r"\\?\C:\Nexon\Maple")));
}

#[test]
fn the_mark_of_a_folder_is_the_size_and_the_time_of_its_game_program() {
    let made = Made::new("mark");
    assert_eq!(install_mark(&made.site, &made.install).as_deref(), Some(MARK));
    // A folder without the client's own program has none, whatever else is in it.
    std::fs::rename(made.install.join("MapleStory.exe"), made.install.join("MapleStoryT.exe")).unwrap();
    assert_eq!(install_mark(&made.site, &made.install), None);
}

#[test]
fn the_version_an_install_is_at_is_read_and_written_as_before() {
    let made = Made::new("applied-version");
    made.given("applied-version", "421\n4096 1764000000");
    assert_eq!(recorded_version(&made.site, &made.install), Some(421));
    assert_eq!(installed_version(&made.site, &made.install), Some(421));
    record_version(&made.site, &made.install, 422);
    assert_eq!(made.kept("applied-version"), "422\n4096 1764000000");
    assert_eq!(recorded_version(&made.site, &made.install), Some(422));
}

/// The game was patched by something else since, or the record is from a release that
/// did not write the mark yet, as some on people's PCs still are.
#[test]
fn a_record_made_under_another_mark_or_under_none_says_nothing_about_the_folder() {
    let made = Made::new("another-mark");
    for version in ["421\n4096 1763999999", "421"] {
        made.given("applied-version", version);
        assert_eq!(recorded_version(&made.site, &made.install), None, "{version:?}");
    }
    made.given("minor-in-place", "421 4\n8192 1764000000");
    made.given("final-in-place", "421");
    assert_eq!(known_minor(&made.site, &made.install, 421), None);
    assert!(!at_final(&made.site, &made.install, 421));
}

#[test]
fn the_minor_an_install_is_at_is_read_and_written_as_before() {
    let made = Made::new("minor-in-place");
    made.given("minor-in-place", "421 4\n4096 1764000000");
    assert_eq!(known_minor(&made.site, &made.install, 421), Some(4));
    // It is the minor of that main version and of no other.
    assert_eq!(known_minor(&made.site, &made.install, 422), None);
    record_minor(&made.site, &made.install, 421, 5);
    assert_eq!(made.kept("minor-in-place"), "421 5\n4096 1764000000");
    assert_eq!(known_minor(&made.site, &made.install, 421), Some(5));
}

#[test]
fn the_last_minor_of_a_version_is_read_and_written_as_before() {
    let made = Made::new("final-in-place");
    made.given("final-in-place", "420\n4096 1764000000");
    assert!(at_final(&made.site, &made.install, 420));
    assert!(!at_final(&made.site, &made.install, 421));
    record_final(&made.site, &made.install, 421);
    assert_eq!(made.kept("final-in-place"), "421\n4096 1764000000");
    assert!(at_final(&made.site, &made.install, 421));
}

#[test]
fn what_a_run_left_is_read_from_its_file_with_the_counts_and_without() {
    let made = Made::new("run-left");
    made.given("run-left", "00420to00421.patch\n69.25\n68270\n470\n1");
    assert_eq!(left_record(&made.site, &made.install), Some(left(69.25, 470, 1)));
    assert_eq!(left_run(&made.site, &made.install, "00420to00421.patch"), Some(left(69.25, 470, 1)));
    // It is the record of that patch and of no other.
    assert_eq!(left_run(&made.site, &made.install, "00421to00422.patch"), None);
    // As the release before the counts wrote it.
    made.given("run-left", "00420to00421.patch\n69.25\n68270");
    assert_eq!(left_record(&made.site, &made.install), Some(left(69.25, 0, 0)));
}

#[test]
fn a_patch_from_the_server_that_a_run_was_applying_is_read_and_written_as_before() {
    let made = Made::new("run-target-server");
    let main = "server\nVer 1.2.421\nhttps://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch\n\
                    00420to00421.patch\n1234567890\n421\n-";
    made.given("run-target", main);
    let target = run_target(&made.site, &made.install).unwrap();
    let Target { label, size, url, file_name, version, minor } = &target;
    assert_eq!((label.as_str(), *size, *version, *minor), ("Ver 1.2.421", 1_234_567_890, 421, None));
    assert_eq!(url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch");
    assert_eq!(file_name, "00420to00421.patch");
    std::fs::remove_file(made.record("run-target")).unwrap();
    record_run_target(&made.site, &made.install, Some(&target), url);
    assert_eq!(made.kept("run-target"), main);

    let minor = "server\nVer 1.2.421 마이너버전(4)\nhttps://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch\n\
                     00421-minor-03to04.patch\n2345678\n421\n4";
    made.given("run-target", minor);
    let target = run_target(&made.site, &made.install).unwrap();
    assert_eq!((target.label.as_str(), target.minor), ("Ver 1.2.421 마이너버전(4)", Some(4)));
    record_run_target(&made.site, &made.install, Some(&target), &target.url);
    assert_eq!(made.kept("run-target"), minor);
    // A folder that a run left partly new is offered that patch and no other.
    assert_eq!(to_apply(&made.site, &made.install, true).map(|target| target.file_name), Some(target.file_name));
    assert!(to_apply(&made.site, &made.install, false).is_none());
}

#[test]
fn a_run_by_an_address_the_user_gave_is_written_down_with_the_address_and_is_no_patch_the_app_found() {
    let made = Made::new("run-target-address");
    let address = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
    // A run of a patch the app found was on record before: this one takes its place.
    made.given("run-target", &format!("server\nVer 1.2.421\n{address}\n00420to00421.patch\n1234567890\n421\n-"));
    assert!(run_target(&made.site, &made.install).is_some());
    record_run_target(&made.site, &made.install, None, address);
    assert_eq!(made.kept("run-target"), format!("address\n{address}"));
    assert!(run_target(&made.site, &made.install).is_none());
    // A folder such a run left partly new is not offered any patch a check works out.
    assert!(to_apply(&made.site, &made.install, true).is_none());

    // What a release that took patch files from the user wrote of one is no patch
    // either, and no address.
    made.given("run-target", "file\nD:\\patches\\00420to00421.patch");
    assert!(run_target(&made.site, &made.install).is_none());
}

/// One of the client's own patches is the same patch whether a check found it or the
/// user gave its address: where it leads and what its file is kept as are worked out
/// from the address. Any other address is of a patch this app knows nothing more of.
#[test]
fn the_patch_at_an_address_among_the_clients_own_is_known_as_a_check_knows_it() {
    let made = Made::new("patch-at");
    let files = "https://maplestory.dn.nexoncdn.co.kr/Patch";
    let main = patch_at(&made.site, &format!("{files}/00421/00420to00421.patch"), 1000).expect("a main patch");
    assert_eq!((main.label.as_str(), main.version, main.minor, main.size), ("Ver 1.2.421", 421, None, 1000));
    assert_eq!(main.file_name, "00420to00421.patch");
    // A patch that skips versions is published under the one it leads to.
    let skips = patch_at(&made.site, &format!("{files}/00421/00417to00421.patch"), 1000).expect("a patch that skips");
    assert_eq!((skips.label.as_str(), skips.file_name.as_str()), ("Ver 1.2.421", "00417to00421.patch"));
    let minor = patch_at(&made.site, &format!("{files}/00421/Minor/03to04.patch"), 200).expect("a minor patch");
    assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("Ver 1.2.421 마이너버전(4)", 421, Some(4)));
    assert_eq!(minor.file_name, "00421-minor-03to04.patch");
    for target in [&main, &skips, &minor] {
        assert_eq!(kept_as(&made.site, &target.url), target.file_name);
        // The round trip: what a run of it writes down is read back as the same patch.
        record_run_target(&made.site, &made.install, Some(target), &target.url);
        assert_eq!(run_target(&made.site, &made.install).as_ref(), Some(target));
    }

    let unknown = [
        // Under another version's folder than the one it leads to, or under none.
        "/00422/00420to00421.patch",
        "/00420to00421.patch",
        "/421/00420to00421.patch",
        "/00421/420to421.patch",
        "/00421/00421to00420.patch",
        "/00421/minor/03to04.patch",
        "/00421/Minor/3to4.patch",
        "/00421/Minor/04to03.patch",
        "/00421/Minor/00421-minor-03to04.patch",
        "/00421/ExtraPatch.patch",
    ];
    for path in unknown {
        let url = format!("{files}{path}");
        assert_eq!(patch_at(&made.site, &url, 1000), None, "{path}");
        assert_eq!(kept_as(&made.site, &url), file_at(&url), "{path}");
    }
    // The other client's patches are none of this client's, under whatever folder.
    assert_eq!(patch_at(&made.site, &mfp_kms::TEST.patch_url(1207, 1208), 1000), None);
    assert_eq!(patch_at(&made.site, "https://maplestory.dn.nexoncdn.co.krT/00421/00420to00421.patch", 1), None);
}

#[test]
fn a_run_that_is_forgotten_leaves_none_of_its_records() {
    let made = Made::new("forget");
    made.given("run-left", "00420to00421.patch\n69.25\n68270\n470\n1");
    made.given("run-left.new", "00420to00421.patch\n70");
    made.given("run-target", "file\nD:\\patches\\00420to00421.patch");
    made.given("applied-version", "420\n4096 1764000000");
    forget_left_run(&made.site, &made.install);
    let left: Vec<_> = std::fs::read_dir(records(&made.site, &made.install).unwrap())
        .unwrap()
        .flatten()
        .map(|file| file.file_name())
        .collect();
    assert_eq!(left, ["applied-version"]);
}

#[test]
fn the_folder_the_user_picked_is_read_and_written_as_before() {
    let made = Made::new("install-dir");
    assert_eq!(chosen_dir(&made.site), None);
    // As the release wrote it, and as someone might write it by hand.
    std::fs::create_dir_all(&made.site.data).unwrap();
    for written in [r"D:\Games\Maple", "D:\\Games\\Maple\r\n"] {
        std::fs::write(made.site.data.join("install-dir"), written).unwrap();
        assert_eq!(chosen_dir(&made.site), Some(PathBuf::from(r"D:\Games\Maple")));
    }

    let picked = made.install.to_string_lossy().into_owned();
    choose_dir(&made.site, Some(&picked)).unwrap();
    assert_eq!(std::fs::read_to_string(made.site.data.join("install-dir")).unwrap(), picked);
    assert_eq!(install_dir(&made.site), Some(made.install.clone()));
    choose_dir(&made.site, None).unwrap();
    assert_eq!(chosen_dir(&made.site), None);
    // With none picked and none registered, there is none.
    assert_eq!(install_dir(&made.site), None);

    // A picked folder that is not there is not quietly replaced by another: not by the
    // install that is registered either, which is the folder there is without a pick.
    let registered = Made::registered("install-dir-registered", 900);
    assert_eq!(install_dir(&registered.site), Some(registered.install.clone()));
    let gone = registered.install.with_file_name("Gone");
    choose_dir(&registered.site, Some(&gone.to_string_lossy())).unwrap();
    assert_eq!((chosen_dir(&registered.site), install_dir(&registered.site)), (Some(gone), None));
    // Back from the pick, it is the registered one again.
    choose_dir(&registered.site, None).unwrap();
    assert_eq!(install_dir(&registered.site), Some(registered.install.clone()));
}

#[test]
fn the_first_folder_to_be_picked_makes_the_data_folder() {
    let made = Made::new("first-pick");
    assert!(!made.site.data.exists());
    choose_dir(&made.site, Some(r"D:\Games\Maple")).unwrap();
    assert_eq!(chosen_dir(&made.site), Some(PathBuf::from(r"D:\Games\Maple")));
}

/// The guess at the version of a folder the registry does not name is looked into once
/// for the folder as it is, and that it was is read and written as before.
#[test]
fn a_guess_that_was_looked_into_is_not_looked_into_again() {
    let made = Made::with("version-asked", StandIns::at_version(900));
    assert_eq!(unsettled(&made.site, &made.install), Some(900));
    assert_eq!(installed_version(&made.site, &made.install), Some(900));
    made.given("version-asked", "900\n4096 1764000000");
    assert_eq!(unsettled(&made.site, &made.install), None);
    // Asked about another guess, or about the folder as it was, is not asked about this.
    for other in ["899\n4096 1764000000", "900\n4096 1763999999"] {
        made.given("version-asked", other);
        assert_eq!(unsettled(&made.site, &made.install), Some(900), "{other:?}");
    }
    // A record of the version settles it for good.
    made.given("applied-version", "899\n4096 1764000000");
    assert_eq!(unsettled(&made.site, &made.install), None);
    assert_eq!(installed_version(&made.site, &made.install), Some(899));
}

/// No patch leads to the first version there can be, so nothing is asked of the server
/// and only the note is written.
#[test]
fn looking_into_a_guess_writes_down_that_it_was_looked_into() {
    let made = Made::with("version-asked-written", StandIns::at_version(1));
    settle_version(&made.site, &made.install, &Published::of(&made.site)).unwrap();
    assert_eq!(made.kept("version-asked"), "1\n4096 1764000000");
    assert_eq!(unsettled(&made.site, &made.install), None);
}

#[test]
fn a_build_for_trying_the_app_out_takes_a_made_up_folder_and_none_where_the_real_games_live() {
    let made = Made::with("turned-down", StandIns::of_a_session());
    choose_dir(&made.site, Some(&made.install.to_string_lossy())).unwrap();
    assert_eq!(install_dir(&made.site), Some(made.install.clone()));

    let real = made.install.with_file_name("Nexon").join("Maple");
    std::fs::create_dir_all(&real).unwrap();
    choose_dir(&made.site, Some(&real.to_string_lossy())).unwrap();
    assert_eq!(install_dir(&made.site), None);
    // The folder was picked all the same, and the screen says of it that it is not found.
    assert_eq!(chosen_dir(&made.site), Some(real.clone()));
    // A release build turns nothing down.
    let release = Site::of(&mfp_kms::LIVE, made.site.data.clone(), StandIns::default(), Registered::nowhere());
    assert_eq!(install_dir(&release), Some(real));

    // Nor does such a build take the folder the Nexon installer registered, wherever that
    // is: not found by itself, and not picked.
    let at = |install: &Path| Registered::at(install, 900);
    let registered = Made::set_up("turned-down-registered", StandIns::of_a_session(), at);
    assert!(is_registered_install(&registered.site, &registered.install));
    assert_eq!(install_dir(&registered.site), None);
    choose_dir(&registered.site, Some(&registered.install.to_string_lossy())).unwrap();
    assert_eq!(install_dir(&registered.site), None);
    // A copy of it elsewhere is a made-up folder like any other.
    let copy = registered.install.with_file_name("Copy");
    std::fs::create_dir_all(&copy).unwrap();
    choose_dir(&registered.site, Some(&copy.to_string_lossy())).unwrap();
    assert_eq!(install_dir(&registered.site), Some(copy));
    // A release build finds the registered install by itself, as ever.
    let release = Made::registered("found-by-itself", 900);
    assert_eq!(install_dir(&release.site), Some(release.install.clone()));
}

/// A record is a file on the PC, and whoever can write to the app's data folder can
/// write one. Whatever stands in it, the patch a run takes up is one of this client's
/// own, fetched from where this client's patches are published.
#[test]
fn of_what_a_run_wrote_down_only_the_name_of_its_patch_file_and_the_size_are_taken() {
    let made = Made::new("run-target-rebuilt");
    let record = |address: &str, kept_as: &str| format!("server\n9.9.999\n{address}\n{kept_as}\n1234567890\n999\n7");
    let elsewhere = "http://example.invalid/Patch/00421/00420to00421.patch";

    made.given("run-target", &record(elsewhere, "00420to00421.patch"));
    let main = Target {
        label: "Ver 1.2.421".to_owned(),
        size: 1_234_567_890,
        url: "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch".to_owned(),
        file_name: "00420to00421.patch".to_owned(),
        version: 421,
        minor: None,
    };
    assert_eq!(run_target(&made.site, &made.install), Some(main));

    made.given("run-target", &record("file:///C:/Windows/evil.patch", "00421-minor-03to04.patch"));
    let minor = run_target(&made.site, &made.install).expect("the minor patch");
    assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("Ver 1.2.421 마이너버전(4)", 421, Some(4)));
    assert_eq!(minor.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch");
    assert_eq!(minor.file_name, "00421-minor-03to04.patch");

    // A name that is not the one a patch file of this client is kept under names no
    // patch, wherever it would lead.
    let none_of_them = [
        r"..\..\evil.patch",
        r"..\00420to00421.patch",
        r"C:\Windows\System32\00420to00421.patch",
        r"00420to00421.patch\..\evil.exe",
        "00420to00421.patch.exe",
        "00420to00421.exe",
        "+0420to00421.patch",
        "0420to00421.patch",
        " 00420to00421.patch",
        "00421to00420.patch",
        "00421to00421.patch",
        "00421-minor-04to03.patch",
        "00421-minor-3to4.patch",
        "421-minor-03to04.patch",
        "evil.patch",
        "",
    ];
    for other in none_of_them {
        let nexons = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
        made.given("run-target", &record(nexons, other));
        assert_eq!(run_target(&made.site, &made.install), None, "{other:?}");
    }

    // The test world's patch is fetched from where the test world's are published.
    let test = Site::of(&mfp_kms::TEST, made.site.data.join("test"), StandIns::default(), Registered::nowhere());
    std::fs::write(records(&test, &made.install).unwrap().join("run-target"), record(elsewhere, "01207to01208.patch"))
        .unwrap();
    let target = run_target(&test, &made.install).expect("the test world's patch");
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch");
    assert_eq!((target.label.as_str(), target.version), ("Ver 1.2.208", 1208));
}

/// What a check has proven from a folder's own files goes before what the registry
/// says, for the registered install and for any other folder. What the registry says
/// alone is a guess, and is not what a folder is known to be at.
#[test]
fn what_the_folders_files_prove_goes_before_what_the_registry_says() {
    // A copy: the registry's value is that of another folder.
    let made = Made::with("proof-before-guess", StandIns::at_version(900));
    assert_eq!(installed_version(&made.site, &made.install), Some(900));
    assert!(!version_proven(&made.site, &made.install, 900));
    made.given("proven-version", "899\n4096 1764000000");
    assert_eq!(installed_version(&made.site, &made.install), Some(899));
    assert!(version_proven(&made.site, &made.install, 899) && !version_proven(&made.site, &made.install, 900));
    // The folder's own files were looked at: the guess has not to be looked into.
    assert_eq!(unsettled(&made.site, &made.install), None);
    // A proof of the folder as it was says nothing of the folder as it is.
    made.given("proven-version", "899\n4096 1763999999");
    assert_eq!(installed_version(&made.site, &made.install), Some(900));
    assert!(!version_proven(&made.site, &made.install, 899));

    // The install the Nexon installer registered.
    let made = Made::registered("proof-before-registry", 900);
    assert!(is_registered_install(&made.site, &made.install));
    assert_eq!(install_dir(&made.site), Some(made.install.clone()));
    assert_eq!(installed_version(&made.site, &made.install), Some(900));
    assert!(!version_proven(&made.site, &made.install, 900));
    made.given("proven-version", "901\n4096 1764000000");
    assert_eq!(installed_version(&made.site, &made.install), Some(901));
    // What this app has patched the folder to since is newer than the proof.
    made.given("applied-version", "902\n4096 1764000000");
    assert_eq!(installed_version(&made.site, &made.install), Some(902));
    // Without a proof it goes as it always did: by the registry, or by this app's own
    // record when that is newer.
    std::fs::remove_file(made.record("proven-version")).unwrap();
    assert_eq!(installed_version(&made.site, &made.install), Some(902));
    made.given("applied-version", "899\n4096 1764000000");
    assert_eq!(installed_version(&made.site, &made.install), Some(900));
}

/// A folder that was left a mix of two versions has the mark it had, when the game's
/// program was not among the files that changed. What was known of its version is
/// forgotten outright.
#[test]
fn what_was_known_of_a_folders_version_is_forgotten_when_its_files_are_left_a_mix() {
    let made = Made::registered("forget-version", 900);
    for (record, said) in
        [("applied-version", "901"), ("proven-version", "901"), ("minor-in-place", "901 3"), ("final-in-place", "901")]
    {
        made.given(record, &format!("{said}\n4096 1764000000"));
    }
    made.given("run-left", "00901to00902.patch\n12.50\n68270\n0\n1");
    assert!(version_proven(&made.site, &made.install, 901));
    assert_eq!(installed_version(&made.site, &made.install), Some(901));
    forget_version(&made.site, &made.install);
    assert!(!version_proven(&made.site, &made.install, 901));
    assert_eq!(known_minor(&made.site, &made.install, 901), None);
    assert!(!at_final(&made.site, &made.install, 901));
    // The registry's word is all that is left, and it is a guess.
    assert_eq!(installed_version(&made.site, &made.install), Some(900));
    // What a run left of itself is another matter, and is forgotten with its work.
    assert!(left_record(&made.site, &made.install).is_some());
}

#[test]
fn a_folder_is_known_to_be_at_a_version_by_a_record_made_of_it_as_it_is() {
    let made = Made::registered("version-proven", 900);
    let proven = |version| version_proven(&made.site, &made.install, version);
    assert!(!proven(900));
    let records = [
        // This app patched it to the version, or a check proved the version, the
        // minor it is at, or that it is at the version's last minor.
        ("applied-version", "900"),
        ("proven-version", "900"),
        ("minor-in-place", "900 3"),
        ("final-in-place", "900"),
    ];
    for (record, said) in records {
        made.given(record, &format!("{said}\n4096 1764000000"));
        assert!(proven(900) && !proven(901), "{record}");
        made.given(record, &format!("{said}\n4096 1763999999"));
        assert!(!proven(900), "{record}, made of the folder as it was");
        std::fs::remove_file(made.record(record)).unwrap();
    }
}
