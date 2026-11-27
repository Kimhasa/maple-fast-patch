//! Tests of the diagnosis: when one begins, what it is told, what the page is told of it,
//! and the check of the game's files. All of it on made-up diagnoses, but for one that the
//! diagnostic itself makes of a folder that is made up for it.

use super::*;
use crate::made::{folder, game_open, target};
use crate::scratch::Scratch;
use crate::state::{Check, Target};

/// One answer in plain words, as the diagnostic says it.
fn plain(name: &'static str, answer: &'static str, reason: &str) -> Plain {
    Plain { name, answer, reason: reason.to_owned() }
}

fn verdict(kind: Kind, patch: Outlook, kept: Outlook) -> Verdict {
    Verdict {
        kind,
        kind_grounds: vec!["Fibre Channel 디스크".to_owned(), "NSM 디스크".to_owned(), "iScsiPrt".to_owned()],
        speed_grounds: vec!["네트워크 전송 속도와 유사한 읽기 패턴".to_owned()],
        kind_plain: plain("PC 종류", kind.plain(), "게임 디스크를 매장 서버에서 네트워크로 받아 사용"),
        patch,
        patch_grounds: vec!["쓰기 속도가 네트워크 전송 속도와 유사".to_owned(), "한도 검사 4.0 GB에서 중단".to_owned()],
        patch_plain: plain("클라이언트 패치", "가능", "매장의 쓰기 공간이 부족하면 중단"),
        kept,
        kept_grounds: Vec::new(),
        kept_plain: plain("재부팅 후", "패치 초기화", ""),
    }
}

fn found(kind: Kind, report: &str) -> Found {
    Found { verdict: verdict(kind, Outlook::Unknown, Outlook::Unlikely), report: report.to_owned() }
}

/// A diagnosis that is made up: what it came to, whether and in how long the game's files
/// can be checked from it, and what a check adds to it.
struct Made {
    found: Found,
    takes: Option<Option<f64>>,
    checked: Option<Found>,
}

impl Kept for Made {
    fn found(&self) -> Found {
        self.found.clone()
    }

    fn check_takes(&self) -> Option<Option<f64>> {
        self.takes
    }

    fn check(&self, stop: &AtomicBool, said: &mut dyn FnMut(u64, u64)) -> Option<Arc<dyn Kept>> {
        said(1, 2);
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        said(2, 2);
        // With every file read, there is nothing left to check.
        Some(Arc::new(Made { found: self.checked.clone()?, takes: None, checked: None }))
    }

    /// What is said of the patch goes by the room a run needs: here it says the room.
    fn with_room(&self, room: Option<(u64, u64)>) -> Arc<dyn Kept> {
        let mut found = self.found.clone();
        if let Some((safe, immediate)) = room {
            found.verdict.patch_plain.reason = format!("여유 공간 {safe} / {immediate}");
        }
        Arc::new(Made { found, takes: self.takes, checked: self.checked.clone() })
    }
}

/// The report of a made-up diagnosis before the game's files were read, and after: its
/// first line as the diagnostic heads a report for the app, and one row of it.
const BY_SIZE: &str = "Maple Fast Patch 0.1.0 · 2026.11.27 01:19\n\n[게임 파일 검사]\n검사 기준         파일 크기 비교";
const BY_CONTENT: &str =
    "Maple Fast Patch 0.1.0 · 2026.11.27 01:19\n\n[게임 파일 검사]\n검사 기준         파일 내용 비교 (일부)";

fn made(kind: Kind, takes: Option<Option<f64>>) -> Arc<dyn Kept> {
    Arc::new(Made { found: found(kind, BY_SIZE), takes, checked: Some(found(kind, BY_CONTENT)) })
}

/// A site that has diagnosed its folder `games`, to `kept`.
fn diagnosed(kept: Arc<dyn Kept>) -> Diagnosed {
    let (mut diagnosed, now) = (Diagnosed::default(), Instant::now());
    diagnosed.see(Some(Path::new("games")), now);
    let stop = diagnosed.begin(true, true, now).expect("a diagnosis begins");
    assert!(diagnosed.end(&stop, Some(kept)));
    diagnosed
}

#[test]
fn a_diagnosis_begins_by_itself_once_for_a_folder_and_again_when_it_is_asked_for() {
    let quiet = At {
        at_work: false,
        diagnosed: false,
        again: false,
        quiet: true,
        answered: true,
        seen_for: Duration::from_secs(1),
    };
    let table = [
        ("a folder the server has answered about", quiet, true),
        // The diagnosis is told what the server has, so that is waited for: ten seconds.
        ("no answer yet", At { answered: false, ..quiet }, false),
        ("no answer after nine seconds", At { answered: false, seen_for: Duration::from_secs(9), ..quiet }, false),
        ("no answer after ten", At { answered: false, seen_for: Duration::from_secs(10), ..quiet }, true),
        ("diagnosed already", At { diagnosed: true, ..quiet }, false),
        ("diagnosed, and asked for again", At { diagnosed: true, again: true, ..quiet }, true),
        // Whoever asks is not kept waiting for the server.
        ("asked for before the server answered", At { again: true, answered: false, ..quiet }, true),
        // One at a time, and none beside a check of the game's files.
        ("one at work", At { at_work: true, ..quiet }, false),
        ("one at work, and asked for again", At { at_work: true, diagnosed: true, again: true, ..quiet }, false),
        // It reads a sample of the game's files: not under a run, and not while the folder
        // is being put back. It begins when the folder is quiet again.
        ("a run at work in the folder", At { quiet: false, ..quiet }, false),
        ("a run at work, and asked for again", At { quiet: false, diagnosed: true, again: true, ..quiet }, false),
    ];
    for (what, at, begins_now) in table {
        assert_eq!(begins(&at), begins_now, "{what}");
    }
}

#[test]
fn a_diagnosis_is_told_what_the_app_knows_of_the_folder_and_of_the_server() {
    let writable = |took: bool| match took {
        true => Check::passed(CheckKind::Writable, ""),
        false => Check::failed(CheckKind::Writable, "", "설치 폴더에 쓸 수 없습니다."),
    };
    let status = folder(Some(r"D:\Games\Maple"), vec![writable(true)]);
    let asked = asked_of(Server::Live, &status, None).unwrap();
    let plain = Asked {
        install: PathBuf::from(r"D:\Games\Maple"),
        server: Server::Live,
        custom: false,
        game_running: false,
        writable: Some(true),
        shelf: None,
        room: None,
        // The line to the server is timed by every diagnosis the app has made.
        time_the_line: true,
    };
    assert_eq!(asked, plain);
    // A folder the user picked, with the game open in it and no writing to it.
    let picked = Status { custom_dir: true, ..game_open(folder(Some(r"D:\Games\Maple"), vec![writable(false)])) };
    let asked = asked_of(Server::Test, &picked, None).unwrap();
    let open = Asked { server: Server::Test, custom: true, game_running: true, writable: Some(false), ..plain.clone() };
    assert_eq!(asked, open);
    // Where the app has not tried the folder, the diagnosis is not told that it has.
    assert_eq!(asked_of(Server::Live, &folder(Some(r"D:\Games\Maple"), Vec::new()), None).unwrap().writable, None);
    // No folder, nothing to diagnose.
    assert_eq!(asked_of(Server::Live, &folder(None, vec![writable(true)]), None), None);

    // What the last check found the server to have: the newest version, and the patch the
    // folder takes next, by Nexon's own name for its file.
    let checked = |newest: Option<(u32, u32)>, target: Option<Target>| Checked {
        install: PathBuf::from(r"D:\Games\Maple"),
        mark: None,
        version: 421,
        main: true,
        newest_minor: 4,
        newest,
        target,
        latest: "Ver 1.2.422".to_owned(),
        error: None,
    };
    let shelf = |checked: Checked| asked_of(Server::Live, &status, Some(&checked)).unwrap().shelf;
    let main =
        Shelf { newest: (422, 1), patch: Some(("00421to00422.patch".to_owned(), 1_288_490_189)), patch_is_main: true };
    assert_eq!(shelf(checked(Some((422, 1)), Some(target("Ver 1.2.422")))), Some(main));
    let minor = Target {
        url: "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch".to_owned(),
        file_name: "00421-minor-03to04.patch".to_owned(),
        size: 163_577_856,
        version: 421,
        minor: Some(4),
        ..target("Ver 1.2.421 마이너버전(4)")
    };
    let behind =
        Shelf { newest: (421, 4), patch: Some(("03to04.patch".to_owned(), 163_577_856)), patch_is_main: false };
    assert_eq!(shelf(checked(Some((421, 4)), Some(minor))), Some(behind));
    assert_eq!(
        shelf(checked(Some((421, 4)), None)),
        Some(Shelf { newest: (421, 4), patch: None, patch_is_main: false })
    );
    // A newest version that was only seen once is not passed on as what the server has.
    assert_eq!(shelf(checked(None, Some(target("Ver 1.2.422")))), None);
}

/// The page shows the verdict as three lines in plain words, the patch first, and every
/// word of them is the diagnostic's own: the name of the line, the answer, and the one
/// reason. The words of the report, which a player does not know, are not among them.
#[test]
fn the_page_is_told_the_three_answers_of_a_verdict_in_the_diagnostics_plain_words() {
    let seat = verdict(Kind::Diskless, Outlook::Unknown, Outlook::Unlikely);
    let said = answers(&seat);
    let said: Vec<(&str, &str, &str)> =
        said.iter().map(|said| (said.title.as_str(), said.answer.as_str(), said.reason.as_str())).collect();
    let expected = [
        ("클라이언트 패치", "가능", "매장의 쓰기 공간이 부족하면 중단"),
        ("PC 종류", "노하드 PC방 좌석", "게임 디스크를 매장 서버에서 네트워크로 받아 사용"),
        // Where the diagnostic says no reason, none is shown.
        ("재부팅 후", "패치 초기화", ""),
    ];
    assert_eq!(said, expected);
    let [kind, patch, kept] = seat.plain();
    for (said, plain) in answers(&seat).iter().zip([patch, kind, kept]) {
        assert_eq!(
            (said.title.as_str(), said.answer.as_str(), &said.reason),
            (plain.name, plain.answer, &plain.reason)
        );
    }
    // Nothing of what the report's analysis says of the same verdict is shown in its place.
    let shown = serde_json::to_string(&answers(&seat)).unwrap();
    let grounds = seat.kind_grounds.iter().chain(&seat.speed_grounds).chain(&seat.patch_grounds);
    for technical in grounds.map(String::as_str).chain(["Fibre Channel", "iScsiPrt", "한도 검사"]) {
        assert!(!shown.contains(technical), "{technical}");
    }
}

/// An answer is written in the colour of how it is to be taken. A patch that goes through
/// is good news on an ordinary PC and something to know of anywhere else, where it hangs
/// on what its reason names; so is one that goes through by the immediate way alone, and a
/// patch that is lost at a restart. What kind of PC this is, an answer that could not be
/// had and a patch that does not go through are painted as nothing.
#[test]
fn each_answer_says_how_it_is_to_be_taken() {
    let tones = |verdict: &Verdict| answers(verdict).iter().map(|said| said.tone).collect::<Vec<_>>();
    let patch_says = |kind: Kind, answer: &'static str, kept: Outlook| Verdict {
        patch_plain: plain("클라이언트 패치", answer, "여유 공간 충분"),
        ..verdict(kind, Outlook::Expected, kept)
    };
    let (good, caution, none) = (Tone::Good, Tone::Caution, Tone::Plain);
    assert_eq!(tones(&patch_says(Kind::Ordinary, "가능", Outlook::Expected)), [good, none, good]);
    assert_eq!(tones(&patch_says(Kind::Diskless, "가능", Outlook::Unlikely)), [caution, none, caution]);
    assert_eq!(tones(&patch_says(Kind::DiskWithRestore, "가능", Outlook::Unlikely)), [caution, none, caution]);
    assert_eq!(tones(&patch_says(Kind::Ordinary, "즉시 패치로 가능", Outlook::Expected)), [caution, none, good]);
    assert_eq!(tones(&patch_says(Kind::Ordinary, "불가", Outlook::Expected)), [none, none, good]);
    assert_eq!(tones(&patch_says(Kind::Unknown, "확인 불가", Outlook::Unknown)), [none, none, none]);
    // The page reads them as the names of its colours, and as nothing where there is none.
    let shown = serde_json::to_value(answers(&patch_says(Kind::Diskless, "불가", Outlook::Unlikely))).unwrap();
    let tones: Vec<&str> = shown.as_array().unwrap().iter().map(|said| said["tone"].as_str().unwrap()).collect();
    assert_eq!(tones, ["", "", "caution"]);
    assert_eq!(serde_json::to_value(Tone::Good).unwrap(), "good");
}

/// What is said of the patch goes by the room a run of the waiting patch needs, which the
/// app knows once the patch file is on this PC and read. The diagnosis is told it when it
/// is made, and whenever it changes afterwards, without anything being looked at again.
#[test]
fn the_diagnosis_that_stands_is_told_the_room_a_run_needs_whenever_that_is_known() {
    let reason = |site: &Diagnosed| site.view(true).unwrap().answers[0].reason.clone();
    let mut site = diagnosed(made(Kind::Ordinary, Some(Some(45.0))));
    assert_eq!(reason(&site), "매장의 쓰기 공간이 부족하면 중단");
    // The patch file has been read: the page is told anew, once.
    assert!(site.room_is(Some((36, 20))));
    assert_eq!(reason(&site), "여유 공간 36 / 20");
    assert!(!site.room_is(Some((36, 20))));
    // A check of the game's files that ends afterwards, or a diagnosis made anew, is told
    // the same: what it hands back was made without it.
    let (kept, stop) = site.begin_check(true).unwrap();
    let checked = kept.check(&stop, &mut |_, _| ());
    assert!(site.end_check(&stop, checked));
    assert_eq!(reason(&site), "여유 공간 36 / 20");
    site.again = true;
    let stop = site.begin(true, true, Instant::now()).unwrap();
    assert!(site.end(&stop, Some(made(Kind::Ordinary, None))));
    assert_eq!(reason(&site), "여유 공간 36 / 20");
    // Before there is a diagnosis there is nothing to tell, and it is told when it comes.
    let (mut site, now) = (Diagnosed::default(), Instant::now());
    site.see(Some(Path::new("games")), now);
    assert!(!site.room_is(Some((36, 20))));
    let stop = site.begin(true, true, now).unwrap();
    assert!(site.end(&stop, Some(made(Kind::Ordinary, None))));
    assert_eq!(reason(&site), "여유 공간 36 / 20");

    // The app passes on what the last look worked out for the folder.
    let status = Status { room: Some((36, 20)), ..folder(Some(r"D:\Games\Maple"), Vec::new()) };
    assert_eq!(asked_of(Server::Live, &status, None).unwrap().room, Some((36, 20)));
}

#[test]
fn the_log_gets_one_line_of_a_diagnosis_and_no_fact_of_the_pc() {
    assert_eq!(
        summary(&verdict(Kind::Diskless, Outlook::Unknown, Outlook::Unlikely)),
        "diagnosis: a seat without a disk; a main patch going through is not known, and its staying after a restart \
         is unlikely"
    );
    assert_eq!(
        summary(&verdict(Kind::Ordinary, Outlook::Expected, Outlook::Expected)),
        "diagnosis: an ordinary PC; a main patch going through is expected, and its staying after a restart is \
         expected"
    );
    for (kind, word) in [(Kind::DiskWithRestore, "a disk that is put back"), (Kind::Unknown, "no kind that can")] {
        assert!(summary(&verdict(kind, Outlook::Unlikely, Outlook::Unknown)).contains(word), "{word}");
    }
}

/// A client's own page says of the diagnosis only that the PC is no ordinary one, in the
/// report's words, as a line that is no check: nothing is in the way for it.
#[test]
fn a_clients_page_shows_a_line_only_of_a_pc_that_is_no_ordinary_one() {
    let shown = |kind: Kind| {
        let verdict = verdict(kind, Outlook::Unknown, Outlook::Unknown);
        row(&verdict).map(|row| (row.kind, row.label, row.ok, row.detail, row.message))
    };
    let line = |words: &str| Some((RowKind::Diagnosis, "PC 검사".to_owned(), None, words.to_owned(), String::new()));
    assert_eq!(shown(Kind::Diskless), line("노하드 PC방 좌석"));
    assert_eq!(shown(Kind::DiskWithRestore), line("복구 프로그램이 있는 PC"));
    assert_eq!((shown(Kind::Ordinary), shown(Kind::Unknown)), (None, None));
    // The line is of what the diagnosis that stands came to, and there is none before one.
    assert!(Diagnosed::default().row().is_none());
    assert!(diagnosed(made(Kind::Diskless, None)).row().is_some());
    assert!(diagnosed(made(Kind::Ordinary, None)).row().is_none());
}

#[test]
fn the_page_is_told_of_a_diagnosis_from_the_first_look_at_a_folder_to_its_report() {
    let (mut site, now) = (Diagnosed::default(), Instant::now());
    // No folder: nothing to diagnose, and no tab on the page.
    site.see(None, now);
    assert_eq!((site.view(true), site.begin(true, true, now).is_some()), (None, false));

    site.see(Some(Path::new("games")), now);
    let waiting = DiagnosisView::default();
    assert_eq!(site.view(true), Some(waiting.clone()));
    // Not before the server has answered about the folder, and not under a run.
    assert!(site.begin(true, false, now).is_none() && site.begin(false, true, now).is_none());
    let stop = site.begin(true, true, now).unwrap();
    assert_eq!(site.view(true), Some(DiagnosisView { working: true, ..waiting.clone() }));
    // One at a time.
    assert!(site.begin(true, true, now).is_none());

    assert!(site.end(&stop, Some(made(Kind::Diskless, Some(Some(45.0))))));
    let view = site.view(true).unwrap();
    assert!(!view.working && !view.checking && view.can_check);
    assert_eq!(view.answers, answers(&verdict(Kind::Diskless, Outlook::Unknown, Outlook::Unlikely)));
    // What is shown, and copied, is the diagnostic's report as it wrote it: one line that
    // says which version of the app it is out of, and nothing of the app's put before it.
    assert_eq!(view.report, BY_SIZE);
    assert_eq!(view.report.matches("Maple Fast Patch").count(), 1);
    // Once for a folder: the desk's later rounds begin none.
    assert!(site.begin(true, true, now + Duration::from_secs(60)).is_none());

    // The user asks for another. What stands is shown until the new one has ended.
    site.again = true;
    let stop = site.begin(true, false, now).unwrap();
    let view = site.view(true).unwrap();
    assert!(view.working && !view.can_check && !view.report.is_empty());
    assert!(site.end(&stop, Some(made(Kind::Ordinary, None))));
    assert_eq!(site.view(true).unwrap().answers[1].answer, "일반 PC");

    // Another folder: what was found of the one before goes with it, and it is diagnosed.
    site.see(Some(Path::new("other games")), now);
    assert_eq!(site.view(true), Some(waiting));
    assert!(site.begin(true, true, now).is_some());
}

/// A diagnosis reads a sample of the game's files, so one that is at work when a run
/// begins in the folder is ended, and what it found is not taken: it is made anew when the
/// folder is quiet again.
#[test]
fn a_diagnosis_that_a_run_cuts_short_is_made_anew_when_the_folder_is_quiet_again() {
    let (mut site, now) = (Diagnosed::default(), Instant::now());
    site.see(Some(Path::new("games")), now);
    let stop = site.begin(true, true, now).unwrap();
    site.put_off();
    assert!(stop.load(Ordering::SeqCst));
    // It is at work until it has ended, and what it hands back then is not kept.
    assert!(site.begin(true, true, now).is_none());
    assert!(!site.end(&stop, Some(made(Kind::Diskless, None))));
    assert_eq!(site.view(true), Some(DiagnosisView::default()));
    // Not while the run works in the folder, and then again.
    assert!(site.begin(false, true, now).is_none());
    assert!(site.begin(true, true, now).is_some());

    // A diagnosis that has ended is not undone by a run that begins later.
    let mut site = diagnosed(made(Kind::Diskless, None));
    site.put_off();
    assert!(site.row().is_some() && site.begin(true, true, now).is_none());
    // A diagnosis that could not be made leaves nothing, and is not tried round after round.
    let mut site = Diagnosed::default();
    site.see(Some(Path::new("games")), now);
    let stop = site.begin(true, true, now).unwrap();
    assert!(!site.end(&stop, None));
    assert!(site.view(true) == Some(DiagnosisView::default()) && site.begin(true, true, now).is_none());
}

/// The check of the game's files: offered only where the diagnostic takes it and the
/// folder is free, one at a time, and what it finds is the diagnosis from then on. Stopped,
/// it leaves what was there.
#[test]
fn the_games_files_are_checked_from_the_diagnosis_that_stands_and_replace_it_only_when_done() {
    let can_check = |site: &Diagnosed, free: bool| site.view(free).unwrap().can_check;
    // The diagnostic refuses: the game was open when the diagnosis was made, or there is
    // nothing to read. The button is off, and no check begins.
    let mut refused = diagnosed(made(Kind::Ordinary, None));
    assert!(!can_check(&refused, true) && refused.begin_check(true).is_none());
    // The game is open now, a run works in the folder, or it is being put back.
    let mut site = diagnosed(made(Kind::Ordinary, Some(Some(45.0))));
    assert!(can_check(&site, true) && !can_check(&site, false));
    assert!(site.begin_check(false).is_none() && !site.checking());
    // No diagnosis yet, and none while one is at work.
    assert!(Diagnosed::default().begin_check(true).is_none());
    site.again = true;
    let diagnosing = site.begin(true, true, Instant::now()).unwrap();
    assert!(!can_check(&site, true) && site.begin_check(true).is_none());
    assert!(site.end(&diagnosing, Some(made(Kind::Ordinary, Some(None)))));
    let before = site.view(true).unwrap();

    // The check begins, and is at work: no second one, no diagnosis beside it.
    let (kept, stop) = site.begin_check(true).unwrap();
    let view = site.view(true).unwrap();
    assert!(view.checking && !view.can_check && site.checking());
    assert_eq!((view.report.as_str(), &view.answers), (before.report.as_str(), &before.answers));
    site.again = true;
    assert!(site.begin_check(true).is_none() && site.begin(true, true, Instant::now()).is_none());
    site.again = false;

    // Stopped: it says how far it came, hands nothing back, and what stood stands.
    site.stop_check();
    let mut said = Vec::new();
    let checked = kept.check(&stop, &mut |done, total| said.push((done, total)));
    assert!(checked.is_none() && said == [(1, 2)]);
    assert!(site.checking());
    assert!(!site.end_check(&stop, checked));
    assert_eq!(site.view(true), Some(before.clone()));
    // Nor is the result of a check kept that ended after it was told to stop.
    let (kept, stop) = site.begin_check(true).unwrap();
    let checked = kept.check(&stop, &mut |_, _| ());
    site.stop_check();
    assert!(checked.is_some() && !site.end_check(&stop, checked));
    assert_eq!(site.view(true), Some(before.clone()));

    // Done: what it found is the diagnosis, and with every file read there is no other check.
    let (kept, stop) = site.begin_check(true).unwrap();
    let mut said = Vec::new();
    let checked = kept.check(&stop, &mut |done, total| said.push((done, total)));
    assert_eq!(said, [(1, 2), (2, 2)]);
    assert!(site.end_check(&stop, checked));
    let after = site.view(true).unwrap();
    assert!(!after.checking && !after.can_check);
    assert_eq!(after.report, BY_CONTENT);
    assert!(site.begin_check(true).is_none());

    // A folder that is another one ends the check that is at work in the one before.
    let mut site = diagnosed(made(Kind::Ordinary, Some(Some(45.0))));
    let (_, stop) = site.begin_check(true).unwrap();
    site.see(Some(Path::new("other games")), Instant::now());
    assert!(stop.load(Ordering::SeqCst));
    assert!(!site.end_check(&stop, Some(made(Kind::Diskless, None))));
    assert!(site.row().is_none() && !site.checking());
}

/// The diagnostic itself, on a folder that is made up for it: it is told what the app
/// knows, writes nothing, and hands back a verdict and a report. It asks no server here:
/// it is told not to time the line, which is the one thing the app's own diagnosis asks.
/// What the two say is this PC's, so only their shape is held to anything here.
///
/// With `MFP_PRINT_DIAGNOSIS` set the report is printed, to be looked at:
/// `cargo test -p maple-fast-patch diagnostic_itself -- --nocapture`.
#[cfg(windows)]
#[test]
fn the_diagnostic_itself_diagnoses_a_folder_it_is_given_and_says_so_in_a_report() {
    let scratch = Scratch::new("diagnosis");
    let install = scratch.0.join("Maple");
    std::fs::create_dir_all(install.join("Data")).unwrap();
    std::fs::write(install.join(mfp_kms::LIVE.game_program), b"the game").unwrap();
    std::fs::write(install.join("Data").join("Base.wz"), vec![7; 4096]).unwrap();
    let asked = Asked {
        install: install.clone(),
        server: Server::Live,
        custom: true,
        game_running: false,
        writable: Some(true),
        shelf: Some(Shelf { newest: (421, 4), patch: None, patch_is_main: false }),
        room: Some((36 << 30, 20 << 30)),
        // No network in a test: the line is not timed here, and the report says so.
        time_the_line: false,
    };
    let kept = diagnose(&asked, "0.1.0", &AtomicBool::new(false)).expect("a diagnosis");
    let found = kept.found();
    if std::env::var_os("MFP_PRINT_DIAGNOSIS").is_some() {
        println!("{}", found.report);
        answers(&found.verdict).iter().for_each(|said| println!("{} / {} / {}", said.title, said.answer, said.reason));
    }
    // The report is headed by the app and its version, in one line, and by nothing else.
    assert!(found.report.starts_with("Maple Fast Patch 0.1.0 · "), "{}", found.report);
    assert_eq!(found.report.matches("Maple Fast Patch").count(), 1, "{}", found.report);
    // What a row of it says, in its first line.
    let row = |name: &str| {
        let line = found.report.lines().find(|line| line.starts_with(&format!("{name}  ")));
        line.map(|line| line[name.len()..].trim().to_owned())
    };
    let said = answers(&found.verdict);
    assert_eq!(
        said.iter().map(|answer| answer.title.as_str()).collect::<Vec<_>>(),
        ["클라이언트 패치", "PC 종류", "재부팅 후"]
    );
    // Each has its answer and its reason in plain words, and the report says what the app
    // told the diagnostic: which game, that the folder takes writes, what the server has,
    // and what the waiting patch needs of the drive. Nothing was asked of the server, and
    // of the line that was not timed the report says that it was not measured.
    for answer in &said {
        assert!(!answer.answer.is_empty() && !answer.reason.is_empty(), "{}", answer.title);
    }
    assert_eq!(row("게임 이름").as_deref(), Some(mfp_kms::LIVE.title), "{}", found.report);
    assert_eq!(row("설치 폴더 쓰기").as_deref(), Some("성공"), "{}", found.report);
    assert_eq!(row("최신 게임 버전").as_deref(), Some("Ver 1.2.421 마이너버전(4)"), "{}", found.report);
    assert!(row("패치 공간").is_some_and(|room| room.ends_with(" · 필요 36.0 GB)")), "{}", found.report);
    assert_eq!(row("다운로드 속도").as_deref(), Some("미측정"), "{}", found.report);
    assert_eq!(row("응답 지연 시간").as_deref(), Some("미측정"), "{}", found.report);
    // Nothing was written to the folder, and nothing is left in it.
    let mut names: Vec<_> = std::fs::read_dir(&install).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["Data", mfp_kms::LIVE.game_program]);
    assert_eq!(row("잔여 파일").as_deref(), Some("없음"), "{}", found.report);
    assert!(!found.report.contains("임시 폴더") && !found.report.contains("쓰기 검사"), "{}", found.report);

    // Told to stop before it begins, it hands nothing back.
    assert!(diagnose(&asked, "0.1.0", &AtomicBool::new(true)).is_none());
}
