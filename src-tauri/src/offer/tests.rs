//! Tests of the check, on a made-up shelf of patches and a made-up install folder: nothing is
//! asked of a network.

use std::cell::RefCell;

use super::*;
use crate::harness::StandIns;
use crate::made::Registered;
use crate::records::{choose_dir, patch_at, patched_before, record_run_target, record_version};
use crate::scratch::Scratch;

#[test]
fn a_version_is_written_as_nexon_writes_it_for_the_client() {
    let (live, test) = (&mfp_kms::LIVE, &mfp_kms::TEST);
    assert_eq!(full_label(live, 421, 1), "Ver 1.2.421");
    assert_eq!(full_label(live, 421, 4), "Ver 1.2.421 마이너버전(4)");
    // The test world's patch files count a thousand ahead of its versions.
    assert_eq!(full_label(test, 1208, 2), "Ver 1.2.208 마이너버전(2)");
}

#[test]
fn the_versions_of_a_patch_file_are_read_from_a_name_nexon_gave_it() {
    assert_eq!(versions_in_name(Path::new("00420to00421.patch")), Some((420, 421)));
    assert_eq!(versions_in_name(Path::new("00417to00421.patch")), Some((417, 421)));
    for other in ["03to04.patch", "00421-minor-03to04.patch", "patch.patch", "420to421.patch"] {
        assert_eq!(versions_in_name(Path::new(other)), None, "{other}");
    }
}

// From here on the check itself, on a made-up shelf of patches and a made-up install
// folder: nothing is asked of a network.

/// One patch file on a made-up shelf.
#[derive(Clone, Default)]
struct OnShelf {
    size: u64,
    /// The checksums of the files it starts from. An empty table is that of a patch file
    /// that only holds whole files, and says nothing of the folder it is for.
    originals: BTreeMap<String, u32>,
    /// For a minor patch: the files it writes, with their checksums once it is applied.
    writes: BTreeMap<String, u32>,
}

/// A made-up shelf of the patch files of one client, by their addresses. It notes what
/// it is asked.
struct Shelved {
    client: &'static Client,
    files: BTreeMap<String, OnShelf>,
    asked: RefCell<Vec<String>>,
}

impl Shelf for Shelved {
    fn size_of(&self, url: &str) -> Result<Option<u64>, Error> {
        self.asked.borrow_mut().push(format!("size {url}"));
        Ok(self.files.get(url).map(|file| file.size))
    }

    fn originals_of(&self, url: &str) -> Result<Option<BTreeMap<String, u32>>, Error> {
        self.asked.borrow_mut().push(format!("originals {url}"));
        self.files.get(url).map(|file| Some(file.originals.clone())).ok_or(Error::NotPublished)
    }

    fn minor(&self, version: u32, from: u32, to: u32) -> Result<Box<dyn Minor>, Error> {
        let url = self.client.minor_patch_url(version, from, to);
        self.asked.borrow_mut().push(format!("read {url}"));
        let file = self.files.get(&url).ok_or(Error::NotPublished)?;
        let checks = !file.originals.is_empty();
        Ok(Box::new(Read { size: file.size, writes: file.writes.clone(), checks }))
    }
}

/// A minor patch of the made-up shelf, read.
struct Read {
    size: u64,
    writes: BTreeMap<String, u32>,
    /// It names files it starts from.
    checks: bool,
}

impl Minor for Read {
    fn size(&self) -> u64 {
        self.size
    }

    fn in_place(&self, install: &Path, _: &AtomicBool) -> Result<bool, Error> {
        let holds = |(name, sum): (&String, &u32)| {
            std::fs::read(install.join(name)).is_ok_and(|bytes| mfp_engine::crc::update(0, &bytes) == *sum)
        };
        Ok(self.writes.iter().all(holds))
    }

    fn checks_folder(&self) -> bool {
        self.checks
    }
}

/// A made-up install of one client that the registry knows nothing of, the site that
/// looks after it, and the shelf its patches are on.
struct Made {
    patcher: Patcher,
    install: PathBuf,
    shelf: Shelved,
    _scratch: Scratch,
}

impl Made {
    /// An install of the test world, of which nothing on a PC says which version it is.
    fn test_world(name: &str) -> Made {
        Made::of(&mfp_kms::TEST, name, StandIns::default())
    }

    /// An install of `client` that the user picked, on a PC whose registry knows of none.
    fn of(client: &'static Client, name: &str, stand_ins: StandIns) -> Made {
        let made = Made::with(client, name, stand_ins, |_| Registered::nowhere());
        choose_dir(made.site(), Some(&made.install.to_string_lossy())).unwrap();
        made
    }

    /// An install of the live game that is the one the Nexon installer registered, with
    /// `version` as what the registry gives for it: the folder nobody had to pick.
    fn registered(name: &str, version: u32) -> Made {
        Made::with(&mfp_kms::LIVE, name, StandIns::default(), |install| Registered::at(install, version))
    }

    /// A made-up install of `client` and its site, with `stand_ins` standing in for
    /// Nexon's side and what `registry` makes of the install folder for the registry.
    fn with(
        client: &'static Client,
        name: &str,
        stand_ins: StandIns,
        registry: impl FnOnce(&Path) -> Box<Registered>,
    ) -> Made {
        let scratch = Scratch::new(name);
        let install = scratch.0.join("game");
        std::fs::create_dir_all(install.join("Data")).unwrap();
        std::fs::write(install.join(client.game_program), b"the game").unwrap();
        let data = scratch.0.join("data");
        let site = Site::of(client, data.join(client.server.key()), stand_ins, registry(&install));
        let shelf = Shelved { client, files: BTreeMap::new(), asked: RefCell::default() };
        Made { patcher: Patcher::of(data, vec![site]), install, shelf, _scratch: scratch }
    }

    fn site(&self) -> &Site {
        &self.patcher.sites[0]
    }

    /// Puts a file of the game into the install folder, and says what a patch file
    /// would list it as: its name there, and its checksum.
    fn holds(&self, name: &str, bytes: &[u8]) -> (String, u32) {
        std::fs::write(self.install.join("Data").join(name), bytes).unwrap();
        (format!(r"Data\{name}"), mfp_engine::crc::update(0, bytes))
    }

    /// Puts the main patch from `from` to `to` on the shelf, made from `originals`.
    fn main_patch(&mut self, from: u32, to: u32, originals: &[(String, u32)]) {
        let file = OnShelf { size: 1000, originals: originals.iter().cloned().collect(), ..OnShelf::default() };
        self.shelf.files.insert(self.shelf.client.patch_url(from, to), file);
    }

    /// Puts a minor patch of `version` on the shelf, made from `originals` and writing
    /// `writes`.
    fn minor_patch(&mut self, version: u32, minors: (u32, u32), originals: &[(String, u32)], writes: &[(String, u32)]) {
        let file = OnShelf {
            size: 200,
            originals: originals.iter().cloned().collect(),
            writes: writes.iter().cloned().collect(),
        };
        self.shelf.files.insert(self.shelf.client.minor_patch_url(version, minors.0, minors.1), file);
    }

    fn check(&self) -> Availability {
        let checked = check(&self.patcher, self.site(), &self.shelf, &mut None);
        assert_eq!(checked.end, CheckEnd::Answered);
        checked.answer
    }

    /// What the shelf was asked since this was last asked, by the first word of each
    /// question.
    fn asked(&self, what: &str) -> usize {
        self.shelf.asked.borrow().iter().filter(|asked| asked.starts_with(what)).count()
    }
}

/// What a check says of a folder it has no proof for: nothing is offered, and the
/// footer says that the version is not known.
fn unproven(latest: &str) -> Availability {
    Availability {
        target: None, latest: Some(latest.to_owned()), error: Some("현재 버전 확인 불가".to_owned())
    }
}

#[test]
fn nothing_is_offered_for_a_folder_whose_version_nothing_proves() {
    let mut made = Made::test_world("no-proof");
    let other = made.holds("Base.wz", b"what the folder holds");
    // Nothing is on the shelf. The newest version is the one the test world is known
    // to have had.
    assert_eq!(made.check(), unproven("Ver 1.2.208"));
    assert_eq!(installed_version(made.site(), &made.install), None);

    // A main patch that starts from other files than the folder's proves nothing, and
    // neither does one that starts from a file the folder does not have.
    made.site().forget_found();
    made.main_patch(1207, 1208, &[(other.0.clone(), other.1 ^ 1)]);
    made.main_patch(1206, 1208, &[(r"Data\Gone.wz".to_owned(), 7)]);
    assert_eq!(made.check(), unproven("Ver 1.2.208"));
    assert_eq!(installed_version(made.site(), &made.install), None);
}

/// The first proof: the folder holds every file the main patch to the newest version
/// starts from.
#[test]
fn a_folder_that_holds_what_the_main_patch_starts_from_is_at_the_version_before() {
    let mut made = Made::test_world("proof-main");
    let base = made.holds("Base.wz", b"base of 1.2.207");
    let map = made.holds("Map.wz", b"map of 1.2.207");
    made.main_patch(1207, 1208, &[base, map]);

    let answer = made.check();
    let target = answer.target.expect("the main patch");
    assert_eq!((target.label.as_str(), target.version, target.minor), ("Ver 1.2.208", 1208, None));
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch");
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.208"), None));
    // What was proven is on record: the version, and that the folder is at its last
    // minor. It is no record of a patch by this app.
    assert_eq!(installed_version(made.site(), &made.install), Some(1207));
    assert!(at_final(made.site(), &made.install, 1207));
    assert!(!patched_before(made.site(), &made.install));
}

/// Nexon also publishes patches that skip versions. One proves the version it starts
/// from like any other, and is the way on from there: no single step has to lead
/// anywhere for it.
#[test]
fn a_patch_that_skips_versions_proves_the_one_it_starts_from_and_is_the_way_on_from_there() {
    let mut made = Made::test_world("proof-skip");
    let base = made.holds("Base.wz", b"base of 1.2.204");
    made.main_patch(1204, 1208, std::slice::from_ref(&base));
    // No step leads on from 1.2.204. The server is known to be at 1.2.208 all the same,
    // and has the patch from here to there.
    let answer = made.check();
    assert_eq!(installed_version(made.site(), &made.install), Some(1204));
    let target = answer.target.expect("the patch straight to the newest version");
    assert_eq!((target.label.as_str(), target.version, target.minor), ("Ver 1.2.208", 1208, None));
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01204to01208.patch");
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.208"), None));

    // The step to the next version on the shelf as well changes nothing of that.
    made.main_patch(1204, 1205, &[base]);
    let answer = made.check();
    assert_eq!((answer.target, answer.latest.as_deref(), answer.error), (Some(target), Some("Ver 1.2.208"), None));
}

/// The server is further on than anything it has for the folder's version leads to.
#[test]
fn a_folder_that_no_patch_on_the_server_leads_on_from_is_not_called_up_to_date() {
    let mut made = Made::test_world("no-way-on");
    record_version(made.site(), &made.install, 1204);
    made.main_patch(1207, 1208, &[]);
    see_latest(made.site(), 1208);
    let answer = made.check();
    assert_eq!((answer.target, answer.latest.as_deref()), (None, Some("Ver 1.2.208")));
    assert_eq!(answer.error.as_deref(), Some("패치 확인 불가"));
}

/// A folder that is more than one main version behind: a seat in a PC room, or a
/// player who is back after months.
#[test]
fn a_folder_two_versions_behind_takes_the_patch_straight_to_the_newest_where_the_server_has_it() {
    let mut made = Made::registered("two-behind", 900);
    let base = made.holds("Base.wz", b"base of 1.2.900");
    made.main_patch(900, 901, std::slice::from_ref(&base));
    made.main_patch(901, 902, &[]);
    // The server has none that skips a version: the single step, as ever.
    let answer = made.check();
    let step = answer.target.expect("the step to the next version");
    assert_eq!(
        (step.label.as_str(), step.version, step.file_name.as_str()),
        ("Ver 1.2.901", 901, "00900to00901.patch")
    );
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.902"), None));

    // It has one: straight to the newest. Nothing of the folder is read to choose.
    made.main_patch(900, 902, std::slice::from_ref(&base));
    made.site().forget_found();
    made.shelf.asked.borrow_mut().clear();
    let answer = made.check();
    let direct = answer.target.expect("the patch straight to the newest version");
    assert_eq!((direct.label.as_str(), direct.version, direct.minor), ("Ver 1.2.902", 902, None));
    assert_eq!(direct.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00902/00900to00902.patch");
    assert_eq!((direct.file_name.as_str(), direct.size), ("00900to00902.patch", 1000));
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.902"), None));
    assert_eq!((made.asked("read"), made.asked("originals")), (0, 0));
    // It is a client patch like the step: on the screen, and in what is said of one
    // that appears.
    assert_eq!(direct.kind(), "클라이언트 패치");
    assert_eq!(registered(&direct), "Ver 1.2.902 클라이언트 패치가 등록되었습니다.");
    // Its run holds the folder against the version it starts from, as the step's does.
    assert_eq!(starts_from(&direct), Some(900));
    assert!(checks_its_folder(made.site(), &made.install, &direct, &made.shelf).unwrap());
}

/// A folder that is further behind than any patch to the newest version starts from:
/// it is taken as far on as the server has a patch for, and from there again.
#[test]
fn a_folder_eight_versions_behind_takes_the_furthest_patch_the_server_has_each_time() {
    let mut made = Made::registered("eight-behind", 900);
    let base = made.holds("Base.wz", b"base of 1.2.900");
    for from in 900..908 {
        made.main_patch(from, from + 1, std::slice::from_ref(&base));
    }
    // As Nexon publishes: to each version from each of the five before it.
    for to in 902..=908u32 {
        for from in to.saturating_sub(5).max(900)..to - 1 {
            made.main_patch(from, to, std::slice::from_ref(&base));
        }
    }
    made.shelf.asked.borrow_mut().clear();
    let answer = made.check();
    let first = answer.target.expect("the patch five versions on");
    assert_eq!(
        (first.label.as_str(), first.version, first.file_name.as_str()),
        ("Ver 1.2.905", 905, "00900to00905.patch")
    );
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.908"), None));
    // One question finds it, and nothing of the folder is read.
    let jumps = |made: &Made| made.shelf.asked.borrow().iter().filter(|asked| asked.contains("/00900to0090")).count();
    assert_eq!((jumps(&made), made.asked("read"), made.asked("originals")), (2, 0, 0));

    // Applied: from 1.2.905 the newest is within reach, and the patch leads straight there.
    record_version(made.site(), &made.install, 905);
    made.site().forget_found();
    let answer = made.check();
    let second = answer.target.expect("the patch straight to the newest version");
    assert_eq!(
        (second.label.as_str(), second.version, second.file_name.as_str()),
        ("Ver 1.2.908", 908, "00905to00908.patch")
    );
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.908"), None));
}

/// The server has only some of the patches that skip versions: the furthest of those it
/// has is the one, and with none of them the single step.
#[test]
fn of_the_patches_that_skip_versions_the_furthest_the_server_has_is_taken() {
    let mut made = Made::registered("some-jumps", 900);
    let base = made.holds("Base.wz", b"base of 1.2.900");
    for from in 900..908 {
        made.main_patch(from, from + 1, std::slice::from_ref(&base));
    }
    let offered = |made: &Made| {
        made.site().forget_found();
        made.shelf.asked.borrow_mut().clear();
        let answer = made.check();
        assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.908"), None));
        let jumps: Vec<String> = (902..=908u32)
            .filter(|to| made.shelf.asked.borrow().contains(&format!("size {}", made.shelf.client.patch_url(900, *to))))
            .map(|to| to.to_string())
            .collect();
        (answer.target.expect("a patch").file_name, jumps.join(" "))
    };
    // None of them: the single step, after the four that were asked for.
    assert_eq!(offered(&made), ("00900to00901.patch".to_owned(), "902 903 904 905".to_owned()));
    // The one over two versions only.
    made.main_patch(900, 902, std::slice::from_ref(&base));
    assert_eq!(offered(&made), ("00900to00902.patch".to_owned(), "902 903 904 905".to_owned()));
    // One over four as well: that one, and nothing nearer is asked for.
    made.main_patch(900, 904, std::slice::from_ref(&base));
    assert_eq!(offered(&made), ("00900to00904.patch".to_owned(), "904 905".to_owned()));
    // A patch that leads further than any is published for is never asked for.
    made.main_patch(900, 906, std::slice::from_ref(&base));
    assert_eq!(offered(&made), ("00900to00904.patch".to_owned(), "904 905".to_owned()));
}

#[test]
fn the_minor_patches_of_the_folders_own_version_come_before_the_patch_that_skips_versions() {
    let mut made = Made::registered("minor-then-direct", 900);
    let base = made.holds("Base.wz", b"base of 1.2.900");
    let string = made.holds("String.wz", b"string of 1.2.900");
    made.main_patch(900, 901, std::slice::from_ref(&base));
    made.main_patch(901, 902, &[]);
    made.main_patch(900, 902, std::slice::from_ref(&base));
    made.minor_patch(900, (1, 2), &[string], &[("Data/String.wz".to_owned(), 12345)]);
    let answer = made.check();
    let minor = answer.target.expect("the minor patch of the folder's version");
    assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("Ver 1.2.900 마이너버전(2)", 900, Some(2)));
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.902"), None));

    // Applied, which its run puts on record: the patch straight to the newest is next.
    record_version(made.site(), &made.install, 900);
    record_minor(made.site(), &made.install, 900, 2);
    made.site().forget_found();
    let direct = made.check().target.expect("the patch straight to the newest version");
    assert_eq!((direct.label.as_str(), direct.version, direct.minor), ("Ver 1.2.902", 902, None));

    // That one applied: the folder is at 1.2.902, at its first minor, and the minor
    // patches of 1.2.902 are what it is offered next.
    record_version(made.site(), &made.install, direct.version);
    made.site().forget_found();
    made.minor_patch(902, (1, 2), &[base], &[("Data/Base.wz".to_owned(), 54321)]);
    let answer = made.check();
    assert_eq!(installed_version(made.site(), &made.install), Some(902));
    let minor = answer.target.expect("the minor patch of the newest version");
    assert_eq!((minor.label.as_str(), minor.version, minor.minor), ("Ver 1.2.902 마이너버전(2)", 902, Some(2)));
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.902 마이너버전(2)"), None));
}

/// The test world's patch files count a thousand ahead of its versions. Its patch that
/// skips versions is its own in every form the app keeps it in.
#[test]
fn the_test_worlds_patch_that_skips_versions_is_its_own_by_name_address_and_record() {
    let mut made = Made::test_world("direct-test-world");
    let base = made.holds("Base.wz", b"base of 1.2.206");
    made.main_patch(1206, 1207, std::slice::from_ref(&base));
    made.main_patch(1207, 1208, &[]);
    made.main_patch(1206, 1208, std::slice::from_ref(&base));
    let answer = made.check();
    let direct = answer.target.expect("the patch straight to the newest version");
    assert_eq!((direct.label.as_str(), direct.version, direct.minor), ("Ver 1.2.208", 1208, None));
    assert_eq!(direct.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01206to01208.patch");
    assert_eq!(direct.file_name, "01206to01208.patch");
    assert_eq!(starts_from(&direct), Some(1206));
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.208"), None));

    // What a run of it writes down of itself leads back to the same patch: a folder
    // that an immediate run of it left partly new is offered the rest of it.
    record_run_target(made.site(), &made.install, Some(&direct), &direct.url);
    assert_eq!(run_target(made.site(), &made.install), Some(direct.clone()));
    // Given by its address, it is the same patch for the test world's site, and none
    // of the live game's.
    let live = Site::of(&mfp_kms::LIVE, made.patcher.data.join("live"), StandIns::default(), Registered::nowhere());
    assert_eq!(patch_at(&live, &direct.url, direct.size), None);
    assert_eq!(patch_at(made.site(), &direct.url, direct.size), Some(direct));
}

/// The second proof: a minor patch that says what it starts from, and the folder holds it.
#[test]
fn a_folder_that_holds_what_a_minor_patch_starts_from_is_at_the_minor_it_starts_from() {
    let mut made = Made::test_world("proof-minor");
    let skill = made.holds("Skill.wz", b"skill of minor 2");
    made.minor_patch(1208, (1, 2), &[], &[("Data/String.wz".to_owned(), 1)]);
    made.minor_patch(1208, (2, 3), &[skill], &[("Data/Skill.wz".to_owned(), 2)]);
    made.minor_patch(1208, (1, 3), &[], &[("Data/Skill.wz".to_owned(), 2)]);

    let answer = made.check();
    let target = answer.target.expect("the minor patch");
    assert_eq!((target.label.as_str(), target.version, target.minor), ("Ver 1.2.208 마이너버전(3)", 1208, Some(3)));
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/Minor/02to03.patch");
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.208 마이너버전(3)"), None));
    assert_eq!(installed_version(made.site(), &made.install), Some(1208));
    assert_eq!(known_minor(made.site(), &made.install, 1208), Some(2));
}

/// The third proof: everything the newest minor patch writes is in the folder.
#[test]
fn a_folder_that_holds_what_the_newest_minor_patch_writes_is_at_the_newest_version() {
    let mut made = Made::test_world("proof-newest");
    let (_, string) = made.holds("String.wz", b"string of minor 2");
    made.minor_patch(1208, (1, 2), &[], &[("Data/String.wz".to_owned(), string)]);

    let answer = made.check();
    assert_eq!(
        answer,
        Availability { target: None, latest: Some("Ver 1.2.208 마이너버전(2)".to_owned()), error: None }
    );
    assert_eq!(installed_version(made.site(), &made.install), Some(1208));
    assert_eq!(known_minor(made.site(), &made.install, 1208), Some(2));
}

/// A minor patch that only holds whole files fits any folder, the other client's
/// included. It is offered to a folder whose version is proven, and to no other.
#[test]
fn a_minor_patch_that_says_nothing_of_its_folder_is_not_offered_without_a_proof() {
    let mut made = Made::test_world("empty-table");
    made.holds("String.wz", b"string of who knows which version");
    made.minor_patch(1208, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
    assert_eq!(made.check(), unproven("Ver 1.2.208 마이너버전(2)"));
    // It was read, to see whether it is in place. Reading it is not offering it.
    assert_eq!(made.asked("read"), 1);

    // The same folder and the same shelf, once this app has patched the folder to
    // 1.2.208 itself: the folder is then at the first minor, and the patch is its next.
    record_version(made.site(), &made.install, 1208);
    made.site().forget_found();
    let target = made.check().target.expect("the minor patch");
    assert_eq!((target.label.as_str(), target.minor), ("Ver 1.2.208 마이너버전(2)", Some(2)));
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/Minor/01to02.patch");
}

/// Looking for a proof reads the folder's files and fetches the starts of patch files.
/// It is done again when the shelf or the folder has changed, and not before.
#[test]
fn a_proof_is_looked_for_again_only_when_the_shelf_or_the_folder_has_changed() {
    let mut made = Made::test_world("proof-again");
    let base = made.holds("Base.wz", b"base of 1.2.208");
    assert_eq!(made.check(), unproven("Ver 1.2.208"));
    assert_eq!(made.asked("originals"), 5);

    made.shelf.asked.borrow_mut().clear();
    assert_eq!(made.check(), unproven("Ver 1.2.208"));
    assert_eq!((made.asked("originals"), made.asked("read")), (0, 0));
    // What is asked every time is what a new patch would show by: the step from the
    // newest version, and its first minor.
    assert_eq!(made.asked("size"), 2);

    // The next version comes out. Its patch starts from what the folder holds.
    made.main_patch(1208, 1209, &[base]);
    let answer = made.check();
    assert_eq!(answer.target.map(|target| target.label), Some("Ver 1.2.209".to_owned()));
    assert_eq!(installed_version(made.site(), &made.install), Some(1208));
    // The newest version that was seen is where the next look for one begins.
    assert_eq!(latest_seen(made.site()), Some(1209));
}

/// The live game's check goes the way it always went, now through the shelf: the
/// registry's guess is where the look for the folder's patches starts.
#[test]
fn a_folder_the_registry_speaks_for_is_checked_from_what_the_registry_says() {
    let mut made = Made::of(&mfp_kms::LIVE, "hinted", StandIns::at_version(900));
    made.holds("String.wz", b"string of 1.2.900");
    made.main_patch(900, 901, &[]);
    let answer = made.check();
    let target = answer.target.expect("the main patch");
    assert_eq!((target.label.as_str(), target.version), ("Ver 1.2.901", 901));
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00901/00900to00901.patch");
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.901"), None));
    // The one thing looked into is whether the folder is a version behind the guess.
    assert_eq!((made.asked("originals"), made.asked("read")), (1, 0));

    // A minor patch that says nothing of its folder is not offered on the strength of
    // the guess: nothing shows that the folder is at 1.2.900 and not at another version.
    made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
    assert_eq!(made.check(), unproven("Ver 1.2.901"));
}

/// A patch that comes out while the app is open is announced with the version it leads
/// to and its kind. One that is there at the first check is not: the screen shows it.
#[test]
fn a_patch_that_appears_is_announced_by_its_version_and_its_kind() {
    let checked = |made: &Made| {
        let mut appeared = None;
        check(&made.patcher, made.site(), &made.shelf, &mut appeared);
        appeared.as_ref().map(registered)
    };
    let mut made = Made::of(&mfp_kms::LIVE, "appeared-main", StandIns::at_version(900));
    assert_eq!(checked(&made), None);
    made.main_patch(900, 901, &[]);
    assert_eq!(checked(&made).as_deref(), Some("Ver 1.2.901 클라이언트 패치가 등록되었습니다."));
    // Said once: at the next check the patch is no news.
    assert_eq!(checked(&made), None);

    let mut made = Made::of(&mfp_kms::LIVE, "appeared-minor", StandIns::at_version(900));
    let string = made.holds("String.wz", b"string of 1.2.900");
    assert_eq!(checked(&made), None);
    made.minor_patch(900, (1, 2), &[string], &[("Data/String.wz".to_owned(), 12345)]);
    assert_eq!(checked(&made).as_deref(), Some("Ver 1.2.900 마이너버전(2) 패치가 등록되었습니다."));

    // A patch that is there when the app first looks is on the screen, and not announced.
    let mut made = Made::of(&mfp_kms::LIVE, "appeared-first", StandIns::at_version(900));
    made.main_patch(900, 901, &[]);
    assert_eq!(checked(&made), None);
}

/// Most PCs have no test client. For a client that is not installed the check has
/// its answer without asking anything, and leaves nothing behind.
#[test]
fn nothing_is_asked_or_written_for_a_client_that_is_not_installed() {
    let scratch = Scratch::new("not-installed-check");
    let data = scratch.0.join("data");
    // No install of it is registered, and no folder was picked.
    let site = Site::of(&mfp_kms::TEST, data.join("test"), StandIns::default(), Registered::nowhere());
    let patcher = Patcher::of(data.clone(), vec![site]);
    let shelf = Shelved { client: &mfp_kms::TEST, files: BTreeMap::new(), asked: RefCell::default() };
    let checked = check(&patcher, &patcher.sites[0], &shelf, &mut None);
    assert_eq!((checked.answer, checked.end), (Availability::nothing(), CheckEnd::Answered));
    assert!(shelf.asked.borrow().is_empty());
    assert!(!data.exists());
}

/// A folder with the other client's program in it is none of this client's: nothing
/// is asked about it.
#[test]
fn nothing_is_asked_about_a_folder_of_the_other_client() {
    let made = Made::test_world("other-client");
    std::fs::rename(made.install.join("MapleStoryT.exe"), made.install.join("MapleStory.exe")).unwrap();
    assert_eq!(made.check(), Availability::nothing());
    assert!(made.shelf.asked.borrow().is_empty());
}

// From here on: what is offered on the registry's word alone. That word is a guess at
// the folder's version. A patch that names the files it starts from can be offered on
// a guess, since a run of it holds the folder against them first. A patch that names
// none is for a folder whose version is proven, and for no other.

/// A folder the user picked, of which the registry says nothing. The version the
/// registry gives for the registered install is all there is to go by, and the folder
/// may as well be at another.
#[test]
fn a_picked_folder_at_another_version_is_not_offered_a_patch_that_fits_any_folder() {
    let mut made = Made::of(&mfp_kms::LIVE, "picked-other-version", StandIns::at_version(900));
    made.holds("String.wz", b"string of some other version");
    made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
    assert_eq!(made.check(), unproven("Ver 1.2.900 마이너버전(2)"));
    // Nothing is on record of the folder being at 1.2.900: it was looked into and not found.
    assert!(!version_proven(made.site(), &made.install, 900));
    assert!(crate::records::found_unproven(made.site(), &made.install));

    // That stands until the shelf or the folder changes, and is not worked out again:
    // only what a new patch would show by is asked.
    made.shelf.asked.borrow_mut().clear();
    assert_eq!(made.check(), unproven("Ver 1.2.900 마이너버전(2)"));
    assert_eq!((made.asked("originals"), made.asked("read")), (0, 0));

    // The same folder and shelf once this app has patched the folder to 1.2.900 itself.
    record_version(made.site(), &made.install, 900);
    made.site().forget_found();
    let target = made.check().target.expect("the minor patch");
    assert_eq!((target.label.as_str(), target.minor), ("Ver 1.2.900 마이너버전(2)", Some(2)));
}

/// The registry's value only catches up once the game has been started. An install
/// that was patched to the next version by something else is behind it until then, and
/// a patch of the version before, laid over it, would put old files among the new.
#[test]
fn a_registered_install_that_is_ahead_of_the_registry_is_not_offered_a_patch_of_the_version_before() {
    let mut made = Made::registered("registry-behind", 900);
    let (base, of_901) = made.holds("Base.wz", b"base of 1.2.901");
    made.holds("String.wz", b"string of 1.2.901");
    // The main patch starts from the base of 1.2.900, which the folder no longer has.
    made.main_patch(900, 901, &[(base, of_901 ^ 1)]);
    made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);

    let answer = made.check();
    assert_eq!(answer, unproven("Ver 1.2.901"));
    // The look for a proof went up from what the registry says, to the patches of the
    // version the folder is really at. None of them shows it.
    assert_eq!(latest_seen(made.site()), Some(901));
    assert_eq!(installed_version(made.site(), &made.install), Some(900));
}

/// The same install with the files the registry's version has: the proof that was
/// looked for is found, and what the folder needs is worked out from it.
#[test]
fn a_proof_that_is_looked_for_and_found_settles_what_the_folder_is_offered() {
    // The folder holds what the main patch to the next version starts from. So it is
    // at the last minor of its version, and the minor patch is not its next.
    let mut made = Made::registered("proof-found-final", 900);
    let base = made.holds("Base.wz", b"base of 1.2.900");
    made.holds("String.wz", b"string of 1.2.900, minor 2");
    made.main_patch(900, 901, &[base]);
    made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
    let answer = made.check();
    let target = answer.target.expect("the main patch");
    assert_eq!((target.label.as_str(), target.minor, answer.error), ("Ver 1.2.901", None, None));
    assert!(version_proven(made.site(), &made.install, 900) && at_final(made.site(), &made.install, 900));

    // The folder is a version behind what the registry says: it holds what the main
    // patch to that version starts from. That patch is its next.
    let mut made = Made::registered("proof-found-behind", 900);
    let base = made.holds("Base.wz", b"base of 1.2.899");
    made.main_patch(899, 900, &[base]);
    made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), 12345)]);
    let answer = made.check();
    let target = answer.target.expect("the main patch from the version before");
    assert_eq!((target.label.as_str(), target.version, answer.error), ("Ver 1.2.900", 900, None));
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00900/00899to00900.patch");
    assert_eq!(installed_version(made.site(), &made.install), Some(899));
}

/// The ordinary case: the install the Nexon installer registered, at the version the
/// registry says, and patches as Nexon makes them, which name the files they start
/// from. Nothing about it has changed: the same questions to the server in the same
/// order, no proof looked for, and the patch offered on the registry's word.
#[test]
fn a_registered_install_with_patches_that_name_their_originals_is_checked_exactly_as_before() {
    let mut made = Made::registered("ordinary", 900);
    let base = made.holds("Base.wz", b"base of 1.2.900");
    let skill = made.holds("Skill.wz", b"skill of 1.2.900, minor 1");
    let of_minor = |minor: u32| (skill.0.clone(), mfp_engine::crc::update(0, format!("minor {minor}").as_bytes()));
    made.main_patch(900, 901, &[base, of_minor(3)]);
    made.minor_patch(900, (1, 2), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), of_minor(2).1)]);
    made.minor_patch(900, (2, 3), &[of_minor(2)], &[("Data/Skill.wz".to_owned(), of_minor(3).1)]);
    made.minor_patch(900, (1, 3), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), of_minor(3).1)]);

    let answer = made.check();
    let target = answer.target.expect("the minor patch from the first minor to the newest");
    assert_eq!((target.label.as_str(), target.version, target.minor), ("Ver 1.2.900 마이너버전(3)", 900, Some(3)));
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00900/Minor/01to03.patch");
    assert_eq!((answer.latest.as_deref(), answer.error), (Some("Ver 1.2.901"), None));
    let files = "https://maplestory.dn.nexoncdn.co.kr/Patch";
    let asked: Vec<String> = [
        // What the server has for the version the registry says.
        "size {files}/00901/00900to00901.patch",
        "size {files}/00900/Minor/01to02.patch",
        "size {files}/00900/Minor/02to03.patch",
        "size {files}/00900/Minor/03to04.patch",
        // The newest version there is.
        "size {files}/00902/00901to00902.patch",
        "size {files}/00901/Minor/01to02.patch",
        // Which minor patch the folder needs: the newest is read and is not in place,
        // and the one from the first minor starts from the files the folder has.
        "read {files}/00900/Minor/02to03.patch",
        "originals {files}/00900/Minor/01to03.patch",
        "size {files}/00900/Minor/01to03.patch",
    ]
    .iter()
    .map(|question| question.replace("{files}", files))
    .collect();
    assert_eq!(*made.shelf.asked.borrow(), asked);
    // No proof was looked for, and none is on record: the patch is its own.
    assert!(!version_proven(made.site(), &made.install, 900));
    assert!(!crate::records::found_unproven(made.site(), &made.install));
    assert_eq!(installed_version(made.site(), &made.install), Some(900));

    // At the next check, with nothing changed, only what a new patch would show by.
    made.shelf.asked.borrow_mut().clear();
    assert_eq!(made.check().target, Some(target));
    assert_eq!((made.asked("size"), made.asked("read"), made.asked("originals")), (4, 0, 0));
}

/// The ordinary case of a new version: the main patch is offered on the registry's
/// word, and nothing of it is fetched to offer it.
#[test]
fn a_registered_install_is_offered_the_main_patch_without_anything_of_it_being_read() {
    let mut made = Made::registered("ordinary-main", 900);
    let base = made.holds("Base.wz", b"base of 1.2.900");
    made.main_patch(900, 901, std::slice::from_ref(&base));
    let answer = made.check();
    let target = answer.target.expect("the main patch");
    assert_eq!((target.label.as_str(), target.minor, answer.error), ("Ver 1.2.901", None, None));
    assert_eq!((made.asked("size"), made.asked("read"), made.asked("originals")), (4, 0, 0));

    // As its run begins, it is asked once whether the patch names what it starts from:
    // the folder's version is the registry's word, and the patch is what will prove it.
    made.shelf.asked.borrow_mut().clear();
    assert!(checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
    assert_eq!(*made.shelf.asked.borrow(), [format!("originals {}", target.url)]);

    // Of a folder this app has patched to the version itself, nothing is asked.
    record_version(made.site(), &made.install, 900);
    made.shelf.asked.borrow_mut().clear();
    assert!(checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
    assert!(made.shelf.asked.borrow().is_empty());
}

/// A main patch that names no file it starts from would be laid over whatever the
/// folder holds. For a folder whose version nothing proves its run ends before it has
/// fetched or changed anything, and the patch is no longer offered.
#[test]
fn a_main_patch_that_names_nothing_is_not_applied_on_the_registrys_word() {
    let mut made = Made::registered("main-names-nothing", 900);
    made.main_patch(900, 901, &[]);
    let target = made.check().target.expect("the main patch, of which nothing was read yet");
    assert!(!checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
    let answer = made.check();
    assert_eq!((answer.target, answer.error.as_deref()), (None, Some("패치 형식 미지원")));

    // The same patch for a folder that is known to be at the version it starts from.
    record_version(made.site(), &made.install, 900);
    made.site().forget_found();
    let target = made.check().target.expect("the main patch");
    assert!(checks_its_folder(made.site(), &made.install, &target, &made.shelf).unwrap());
    // A minor patch was looked into before it was offered, and is not asked about again.
    let minor = Target { minor: Some(2), ..target };
    made.shelf.asked.borrow_mut().clear();
    assert!(checks_its_folder(made.site(), &made.install, &minor, &made.shelf).unwrap());
    assert!(made.shelf.asked.borrow().is_empty());
}

/// With the last step of the minors being one that names what it starts from, the
/// registry's word is enough to offer it: the run compares the folder with it first.
#[test]
fn the_one_minor_patch_there_is_is_offered_on_the_registrys_word_when_it_names_its_originals() {
    let mut made = Made::registered("one-minor", 900);
    let skill = made.holds("Skill.wz", b"skill of 1.2.900, minor 1");
    made.minor_patch(900, (1, 2), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), 7)]);
    let target = made.check().target.expect("the minor patch");
    assert_eq!((target.label.as_str(), target.minor), ("Ver 1.2.900 마이너버전(2)", Some(2)));
    assert_eq!(made.asked("originals"), 0);

    // The same step as a patch of whole files only: not on the registry's word.
    let mut made = Made::registered("one-minor-whole-files", 900);
    made.holds("Skill.wz", b"skill of 1.2.900, minor 1");
    made.minor_patch(900, (1, 2), &[], &[("Data/Skill.wz".to_owned(), 7)]);
    assert_eq!(made.check(), unproven("Ver 1.2.900 마이너버전(2)"));
}

/// A step that is found in place shows by the folder's own files which version and
/// minor it is at. The patch from there is offered, whatever kind it is.
#[test]
fn a_minor_step_that_is_in_place_proves_the_folder_for_the_patch_from_there() {
    let mut made = Made::registered("step-in-place", 900);
    let (_, string) = made.holds("String.wz", b"string of 1.2.900, minor 2");
    made.minor_patch(900, (1, 2), &[], &[("Data/String.wz".to_owned(), string)]);
    made.minor_patch(900, (2, 3), &[], &[("Data/Skill.wz".to_owned(), 7)]);
    made.minor_patch(900, (3, 4), &[], &[("Data/Skill.wz".to_owned(), 8)]);
    made.minor_patch(900, (2, 4), &[], &[("Data/Skill.wz".to_owned(), 8)]);
    let target = made.check().target.expect("the patch from the second minor");
    assert_eq!(target.url, "https://maplestory.dn.nexoncdn.co.kr/Patch/00900/Minor/02to04.patch");
    assert_eq!(known_minor(made.site(), &made.install, 900), Some(2));
}

/// An address the user gave proves nothing of the folder it is given for, and no check
/// has looked into the patch there. Whichever patch it is, a run of it has to have
/// something to hold the folder against: the files the patch names as what it starts
/// from, or what is known of the folder's version.
#[test]
fn a_patch_given_by_its_address_is_applied_only_where_there_is_something_to_hold_the_folder_against() {
    let mut made = Made::registered("given-address", 900);
    let skill = made.holds("Skill.wz", b"skill of 1.2.900, minor 2");
    made.minor_patch(900, (2, 3), std::slice::from_ref(&skill), &[("Data/Skill.wz".to_owned(), 7)]);
    made.minor_patch(900, (1, 3), &[], &[("Data/Skill.wz".to_owned(), 7)]);
    made.main_patch(900, 901, &[]);
    let names_them = mfp_kms::LIVE.minor_patch_url(900, 2, 3);
    let names_none = mfp_kms::LIVE.minor_patch_url(900, 1, 3);
    let main_names_none = mfp_kms::LIVE.patch_url(900, 901);
    let given =
        |made: &Made, url: &str, from| given_checks_its_folder(made.site(), &made.install, url, from, &made.shelf);

    // The registry's word for the version is no more than that. Each patch is asked for
    // what it names, a minor one as well: no check has read it for this folder.
    assert!(given(&made, &names_them, Some(900)).unwrap());
    assert!(!given(&made, &names_none, Some(900)).unwrap());
    assert!(!given(&made, &main_names_none, Some(900)).unwrap());
    assert_eq!(made.asked("originals"), 3);

    // The folder is known to be at the version the patches start from: this app patched
    // it to that version. Nothing has to be asked.
    record_version(made.site(), &made.install, 900);
    made.shelf.asked.borrow_mut().clear();
    for url in [&names_them, &names_none, &main_names_none] {
        assert!(given(&made, url, Some(900)).unwrap(), "{url}");
    }
    assert!(made.shelf.asked.borrow().is_empty());
    // That says nothing for a patch that starts from another version, or for one whose
    // address does not say which version it starts from.
    assert!(!given(&made, &names_none, Some(899)).unwrap());
    assert!(!given(&made, &names_none, None).unwrap());
    assert!(given(&made, &names_them, None).unwrap());
}

#[test]
fn a_patch_starts_from_the_version_its_main_patch_leads_on_from_or_its_minor_patch_is_of() {
    let main = crate::made::target("Ver 1.2.422");
    assert_eq!(starts_from(&main), Some(421));
    let skips = Target { file_name: "00417to00422.patch".to_owned(), ..main.clone() };
    assert_eq!(starts_from(&skips), Some(417));
    let minor = Target { file_name: "00421-minor-03to04.patch".to_owned(), version: 421, minor: Some(4), ..main };
    assert_eq!(starts_from(&minor), Some(421));
}

/// An apply was cut off in the folder and its record cannot be read. Until the folder
/// has been put back, nothing is worked out of it and nothing is asked about it, and
/// no word is said of a damaged record: putting it back is what comes first.
#[test]
fn nothing_is_worked_out_of_a_folder_that_is_still_to_be_put_back() {
    let mut made = Made::registered("check-cut-off", 900);
    made.main_patch(900, 901, &[]);
    let journal = stage::Dirs::new(&made.install).journal();
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, "this is no line of a journal\n").unwrap();
    let checked = check(&made.patcher, made.site(), &made.shelf, &mut None);
    assert_eq!(checked.end, CheckEnd::NotYet);
    assert_eq!(
        checked.answer,
        Availability { target: None, latest: None, error: Some("이전 패치 복구 대기".to_owned()) }
    );
    assert!(made.shelf.asked.borrow().is_empty());

    // Put back, it is a folder to ask about like any other.
    crate::ready::put_back(made.site(), &made.install);
    assert!(made.check().target.is_some());
}

/// The folder cannot be put back without the record either: putting it back was tried,
/// and left its note that it cannot be done. Nothing is worked out of such a folder and
/// nothing is asked about it: the answer is that, for as long as what the run left is
/// there.
#[test]
fn nothing_is_worked_out_of_a_folder_whose_apply_cannot_be_undone() {
    let mut made = Made::registered("check-unreadable-journal", 900);
    made.main_patch(900, 901, &[]);
    let dirs = stage::Dirs::new(&made.install);
    std::fs::create_dir_all(dirs.work()).unwrap();
    std::fs::write(dirs.work().join("cannot-restore"), "what was moved aside is not the file the patch was made for")
        .unwrap();
    std::fs::write(dirs.journal(), "this is no line of a journal\n").unwrap();
    let said = Availability { target: None, latest: None, error: Some("작업 기록 손상".to_owned()) };
    assert_eq!(made.check(), said);
    assert!(made.shelf.asked.borrow().is_empty());

    // With what the run left removed as it is, the folder is one to ask about again.
    mfp_engine::run::discard_work_as_it_is(&made.install).unwrap();
    assert!(made.check().target.is_some());
}

/// A run begins in the very moment a check does. The run has told every check to give up,
/// the other client's too, and a check clears what told the one before it to: it looks for
/// a run once more after that, so that the run's word is not lost with it. Nothing is asked
/// of the server and nothing is read beside a run, and the word stands for whatever else
/// goes by it.
#[test]
fn a_check_that_begins_as_a_run_does_gives_up_and_leaves_the_runs_word_standing() {
    let mut made = Made::registered("check-beside-a-run", 900);
    made.main_patch(900, 901, &[]);
    // The test world beside the live game, for the run of another client.
    let other = Site::of(&mfp_kms::TEST, made.patcher.data.join("test"), StandIns::default(), Registered::nowhere());
    made.patcher.sites.push(other);
    // A run begins as `patching::begin` has it: the turn is taken, and then every check is
    // told to give up.
    let begins = |made: &Made, place: usize| {
        assert!(made.patcher.begin_run(&made.patcher.sites[place]));
        made.patcher.sites.iter().for_each(|site| site.check_stop.store(true, Ordering::SeqCst));
    };
    let told_to_give_up = |made: &Made| made.site().check_stop.load(Ordering::SeqCst);

    // The other client's run, and then this client's own.
    for place in [1, 0] {
        begins(&made, place);
        let checked = check(&made.patcher, made.site(), &made.shelf, &mut None);
        assert_eq!(checked.end, CheckEnd::Stopped, "{place}");
        assert!(made.shelf.asked.borrow().is_empty(), "{place}");
        assert!(told_to_give_up(&made), "{place}");
        // Nothing was found, and nothing is kept as found.
        assert!(made.site().found_for(&made.install).is_none(), "{place}");
        made.patcher.end_run();
    }

    // With no run at work, the check is made, and what told the one before to give up is
    // cleared with it.
    assert!(made.check().target.is_some());
    assert!(!told_to_give_up(&made) && made.asked("size") > 0);
}
