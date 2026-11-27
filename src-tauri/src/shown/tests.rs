//! Tests of what the screen says: the footer, the figure, the lines of the ready screen, the
//! line under a finished run and the button.

use std::collections::BTreeSet;

use super::*;
use crate::flow::{Failure, Run};
use crate::made::{
    GIVEN, GIVEN_WRONG, errored, game_at, game_open, half_patched, journal_unreadable, minor_patch_to, no_game_at,
    no_install, patch_to, resumable, run_of, short_of_room, up_to_date,
};
use crate::state::{Availability, Check, Status};

/// Everything a screen is made from, as a test sets it up.
struct Scene {
    status: Option<Status>,
    answer: Option<Availability>,
    /// The newest version the server was last seen to have. A scene has the one its
    /// answer names, unless a test sets it apart.
    latest: Option<String>,
    phase: Phase,
    run: Option<Run>,
    source: Option<String>,
    outdated: bool,
    quiet: bool,
    launching: bool,
    /// The game's files are being checked in the folder, at the user's word.
    checking_files: bool,
}

impl Scene {
    /// The ready screen of `status` with the server having said `answer`.
    fn ready(status: Status, answer: Option<Availability>) -> Scene {
        Scene {
            status: Some(status),
            latest: answer.as_ref().and_then(|answer| answer.latest.clone()),
            answer,
            phase: Phase::Ready,
            run: None,
            source: None,
            outdated: false,
            quiet: false,
            launching: false,
            checking_files: false,
        }
    }

    /// A run of the patch to 1.2.422 on the screen, in `phase`.
    fn with_run(phase: Phase, change: impl FnOnce(&mut Run)) -> Scene {
        let mut run = run_of(None, Some("1.2.422"));
        (run.percent, run.step, run.status) = (61.5, Some(Step::Rebuild), "새 게임 파일 생성 중");
        (run.done, run.total, run.activity) = (5, 10, "생성 중: Map001.wz".to_owned());
        change(&mut run);
        Scene { phase, run: Some(run), ..Scene::ready(game_at(r"C:\Nexon\Maple"), Some(patch_to("1.2.422"))) }
    }

    /// The screen of a run that is done: 95 seconds, 718 files.
    fn done(answer: Option<Availability>) -> Scene {
        let mut scene = Scene::with_run(Phase::Done, |run| (run.percent, run.took, run.files) = (100.0, 95, 718));
        if let Some(latest) = answer.as_ref().and_then(|answer| answer.latest.clone()) {
            scene.latest = Some(latest);
        }
        scene.answer = answer;
        scene
    }

    fn shown(&self) -> Shown {
        let hands_off = BTreeSet::new();
        shown(&Facts {
            status: self.status.as_ref(),
            answer: self.answer.as_ref(),
            latest: self.latest.as_deref(),
            phase: self.phase,
            run: self.run.as_ref(),
            source: self.source.as_deref(),
            hands_off: &hands_off,
            game_due: false,
            auto_patch: false,
            auto_start: false,
            outdated: self.outdated,
            run_at_work: self.phase.at_work(),
            open: Some(true),
            sees_open: true,
            checking_files: self.checking_files,
            update_asked: true,
            quiet: self.quiet,
            launching: self.launching,
        })
    }

    /// The footer: its word, whether it is bad news, and whether the spinner turns.
    fn footer(&self) -> (String, bool, bool) {
        let shown = self.shown();
        (shown.status, shown.status_bad, shown.busy)
    }

    fn button(&self) -> (ButtonKind, bool, bool) {
        let Button { kind, enabled, primary } = self.shown().button;
        (kind, enabled, primary)
    }
}

fn game() -> Status {
    game_at(r"C:\Nexon\Maple")
}

fn word(of: &str, bad: bool, busy: bool) -> (String, bool, bool) {
    (of.to_owned(), bad, busy)
}

// The footer's word, in the order the screen has always decided it.

#[test]
fn an_app_that_is_too_old_says_so_where_the_screen_rests_and_as_no_bad_news() {
    // On the ready screen, whatever is offered there: something to know, and no wait.
    let update = word("최신 버전으로 업데이트해 주세요.", false, false);
    for answer in [None, Some(patch_to("1.2.422")), Some(up_to_date("1.2.421"))] {
        assert_eq!(Scene { outdated: true, ..Scene::ready(game(), answer) }.footer(), update);
    }
    // What is in the way of a patch is said in its place.
    let playing = || Scene::ready(game_open(game()), Some(patch_to("1.2.422")));
    assert_eq!(Scene { outdated: true, ..playing() }.footer(), playing().footer());
    assert!(playing().footer().1);
    // A run says its own, at work and once it has ended.
    for phase in [Phase::Starting, Phase::Running, Phase::Paused, Phase::Failed, Phase::Done] {
        let scene = Scene { outdated: true, ..Scene::with_run(phase, |_| ()) };
        assert_eq!(scene.footer(), Scene::with_run(phase, |_| ()).footer(), "{phase:?}");
    }
}

#[test]
fn a_paused_run_says_paused() {
    assert_eq!(Scene::with_run(Phase::Paused, |_| ()).footer(), word("일시정지", false, false));
}

#[test]
fn a_run_that_was_turned_down_says_that_it_cannot_patch() {
    assert_eq!(Scene::with_run(Phase::Failed, |run| run.refused = true).footer(), word("패치 불가", true, false));
}

#[test]
fn a_run_that_failed_says_so() {
    assert_eq!(Scene::with_run(Phase::Failed, |_| ()).footer(), word("패치 실패", true, false));
}

#[test]
fn a_run_that_is_stopping_waits_for_the_pause_or_for_the_app_to_go() {
    assert_eq!(Scene::with_run(Phase::Stopping, |_| ()).footer(), word("일시정지 대기", false, true));
    assert_eq!(Scene::with_run(Phase::Stopping, |run| run.leaving = true).footer(), word("종료 대기", false, true));
}

#[test]
fn a_download_that_brings_nothing_waits_for_the_server() {
    let downloading = |run: &mut Run| (run.step, run.status) = (Some(Step::Download), "패치 파일 다운로드 중");
    let quiet = Scene { quiet: true, ..Scene::with_run(Phase::Running, downloading) };
    assert_eq!(quiet.footer(), word("다운로드 응답 대기", false, false));
    assert_eq!(Scene::with_run(Phase::Running, downloading).footer(), word("패치 파일 다운로드 중", false, false));
    // Any other step is at work on this PC and waits for no one.
    let building = Scene { quiet: true, ..Scene::with_run(Phase::Running, |_| ()) };
    assert_eq!(building.footer(), word("새 게임 파일 생성 중", false, false));
    // Nor does a run that is stopping say it.
    let stopping = Scene { quiet: true, ..Scene::with_run(Phase::Stopping, downloading) };
    assert_eq!(stopping.footer(), word("일시정지 대기", false, true));
}

#[test]
fn a_run_at_work_says_its_step_and_nothing_before_the_first() {
    assert_eq!(Scene::with_run(Phase::Running, |_| ()).footer(), word("새 게임 파일 생성 중", false, false));
    let begun = |run: &mut Run| (run.step, run.status) = (None, "");
    assert_eq!(Scene::with_run(Phase::Starting, begun).footer(), word("", false, false));
}

#[test]
fn a_finished_run_says_that_the_patch_is_done() {
    assert_eq!(Scene::done(None).footer(), word("패치 완료", false, false));
}

#[test]
fn a_game_that_is_not_installed_says_that_the_folder_is_not_known() {
    // The answer where there is nothing to ask about, and before there is any.
    assert_eq!(
        Scene::ready(no_install(), Some(Availability::nothing())).footer(),
        word("설치 경로 미확인", true, false)
    );
    assert_eq!(Scene::ready(no_install(), None).footer(), word("설치 경로 미확인", false, false));
}

#[test]
fn a_run_by_an_address_that_waits_says_that_a_patch_was_left_off() {
    let waits = Scene::ready(resumable(game(), Some(GIVEN), Some(41.0)), None);
    // The server has no say in that: no answer is waited for.
    assert_eq!(waits.footer(), word("중단된 패치 있음", false, false));
    let blocked = Scene::ready(game_open(resumable(game(), Some(GIVEN), Some(41.0))), None);
    assert_eq!(blocked.footer(), word("패치 불가", true, false));
}

#[test]
fn a_folder_the_server_has_not_answered_for_says_that_it_is_asking() {
    assert_eq!(Scene::ready(game(), None).footer(), word("최신 버전 확인 중", false, true));
    // An answer that names no version and no reason came while the folder was being changed.
    assert_eq!(Scene::ready(game(), Some(Availability::nothing())).footer(), word("최신 버전 확인 중", false, true));
    // Before the first look there is nothing to say either.
    let nothing = Scene { status: None, ..Scene::ready(game(), None) };
    assert_eq!(nothing.footer(), word("최신 버전 확인 중", false, true));
}

#[test]
fn an_install_with_nothing_newer_on_the_server_says_newest_version() {
    assert_eq!(Scene::ready(game(), Some(up_to_date("1.2.421"))).footer(), word("최신 버전", false, false));
    // The game being open is in nobody's way then.
    assert_eq!(Scene::ready(game_open(game()), Some(up_to_date("1.2.421"))).footer(), word("최신 버전", false, false));
}

#[test]
fn a_check_that_could_not_be_made_says_why() {
    assert_eq!(Scene::ready(game(), Some(errored("서버 요청 실패"))).footer(), word("서버 요청 실패", true, false));
    // Also beside a patch that was found before the server went quiet.
    let stale = Availability { error: Some("서버 요청 실패".to_owned()), ..patch_to("1.2.422") };
    assert_eq!(Scene::ready(game(), Some(stale)).footer(), word("서버 요청 실패", true, false));
}

#[test]
fn something_in_the_way_of_a_patch_says_that_it_cannot_patch() {
    let playing = Scene::ready(game_open(game()), Some(patch_to("1.2.422")));
    assert_eq!(playing.footer(), word("패치 불가", true, false));
    let no_room = Scene::ready(short_of_room(game()), Some(patch_to("1.2.422")));
    assert_eq!(no_room.footer(), word("패치 불가", true, false));
    // Nothing to apply and something in the way all the same: not up to date, and bad.
    let idle = Scene::ready(short_of_room(game()), Some(up_to_date("1.2.421")));
    assert_eq!(idle.footer(), word("패치 불가", true, false));
}

#[test]
fn a_run_of_the_servers_patch_that_was_left_off_says_so() {
    let left = Scene::ready(resumable(game(), None, Some(69.25)), Some(patch_to("1.2.422")));
    assert_eq!(left.footer(), word("중단된 패치 있음", false, false));
}

/// What a run left lies in a folder the server has nothing for, and no file to go on
/// with: the install is at the newest version all the same. This is also why the word
/// for "no patch can be made out" is never said: every way to it leads through one of
/// the words before it.
#[test]
fn work_left_in_a_folder_that_is_up_to_date_does_not_keep_it_from_saying_so() {
    let stranded = Scene::ready(resumable(game(), None, Some(69.25)), Some(up_to_date("1.2.421")));
    assert_eq!(stranded.footer(), word("최신 버전", false, false));
}

#[test]
fn a_patch_to_a_new_version_on_the_server_says_that_there_is_a_new_client_patch() {
    let offered = Scene::ready(game(), Some(patch_to("1.2.422")));
    assert_eq!(offered.footer(), word("새 클라이언트 패치 있음", false, false));
}

#[test]
fn a_minor_patch_on_the_server_says_that_there_is_a_new_minor_patch() {
    let offered = Scene::ready(game(), Some(minor_patch_to("1.2.421 마이너버전(4)", 4)));
    assert_eq!(offered.footer(), word("새 마이너 패치 있음", false, false));
    // It is a patch like the other in everything else the screen says of it.
    let shown = offered.shown();
    assert_eq!(shown.button, Button { kind: ButtonKind::Download, enabled: true, primary: true });
    assert_eq!(shown.percent, Some(0.0));
}

/// A run that was left unfinished is not called by its kind: what waits is the run.
#[test]
fn a_minor_patch_that_was_left_off_says_that_a_patch_was_left_off() {
    let left = Scene::ready(resumable(game(), None, Some(41.0)), Some(minor_patch_to("1.2.421 마이너버전(4)", 4)));
    assert_eq!(left.footer(), word("중단된 패치 있음", false, false));
}

#[test]
fn a_folder_that_is_no_game_folder_says_that_it_cannot_patch() {
    let empty = Scene::ready(no_game_at(r"D:\Empty"), Some(Availability::nothing()));
    assert_eq!(empty.footer(), word("패치 불가", true, false));
}

// The big figure.

#[test]
fn the_figure_of_the_ready_screen_is_full_when_up_to_date_and_where_a_run_was_left_otherwise() {
    assert_eq!(Scene::ready(game(), Some(up_to_date("1.2.421"))).shown().percent, Some(100.0));
    assert_eq!(Scene::ready(game(), Some(patch_to("1.2.422"))).shown().percent, Some(0.0));
    assert_eq!(Scene::ready(game(), None).shown().percent, Some(0.0));
    let left = Scene::ready(resumable(game(), None, Some(69.25)), Some(patch_to("1.2.422")));
    assert_eq!(left.shown().percent, Some(69.25));
    // Work that nothing goes on with is not what the figure is of, nor has a record
    // without a figure one.
    let stranded = Scene::ready(resumable(game(), None, Some(69.25)), Some(up_to_date("1.2.421")));
    assert_eq!(stranded.shown().percent, Some(100.0));
    let unknown = Scene::ready(resumable(game(), None, None), Some(patch_to("1.2.422")));
    assert_eq!(unknown.shown().percent, Some(0.0));
}

/// Where no game is installed there is nothing to be at any point of: the page draws a
/// dash for it, for the live game and the test world alike.
#[test]
fn a_folder_that_is_no_game_folder_has_no_figure() {
    assert_eq!(Scene::ready(no_install(), Some(Availability::nothing())).shown().percent, None);
    assert_eq!(Scene::ready(no_game_at(r"D:\Empty"), Some(Availability::nothing())).shown().percent, None);
    // Before the folder has been looked at, nothing is known of it either way.
    let unseen = Scene { status: None, ..Scene::ready(game(), None) };
    assert_eq!(unseen.shown().percent, Some(0.0));
}

#[test]
fn the_figure_of_a_run_is_the_runs_and_a_finished_one_is_full() {
    for phase in [Phase::Starting, Phase::Running, Phase::Stopping, Phase::Paused, Phase::Failed] {
        assert_eq!(Scene::with_run(phase, |_| ()).shown().percent, Some(61.5), "{phase:?}");
    }
    assert_eq!(Scene::done(None).shown().percent, Some(100.0));
    // A run that only left something to say of itself is not on the ready screen's bar.
    let said = Scene::with_run(Phase::Ready, |run| run.percent = 4.0);
    assert_eq!(said.shown().percent, Some(0.0));
}

// The versions.

#[test]
fn the_installed_version_is_the_one_the_look_found_and_after_a_run_the_one_it_patched_to() {
    assert_eq!(Scene::ready(game(), None).shown().current.as_deref(), Some("1.2.421"));
    assert_eq!(Scene::done(None).shown().current.as_deref(), Some("1.2.422"));
    // A run by an address the user gave names no version; the look's stands.
    let given = Scene::with_run(Phase::Done, |run| run.label = None);
    assert_eq!(given.shown().current.as_deref(), Some("1.2.421"));
    // While a run is at work or stopped it is still the old one.
    assert_eq!(Scene::with_run(Phase::Paused, |_| ()).shown().current.as_deref(), Some("1.2.421"));
    assert_eq!(Scene::ready(half_patched(game(), None, None), None).shown().current, None);
}

// The lines of the ready screen.

fn kinds(shown: &Shown) -> Vec<RowKind> {
    shown.rows.iter().map(|row| row.kind).collect()
}

#[test]
fn the_ready_screen_lists_the_checks_that_show_then_the_patch_file() {
    let shown = Scene::ready(game(), Some(patch_to("1.2.422"))).shown();
    assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::PatchFile]);
    let folder = &shown.rows[0];
    assert_eq!(
        (folder.label.as_str(), folder.ok, folder.detail.as_str()),
        ("설치 경로", Some(true), r"C:\Nexon\Maple")
    );
    let space = &shown.rows[1];
    assert_eq!((space.label.as_str(), space.ok, space.detail.as_str()), ("여유 공간", Some(true), "160.0 GB"));
    // The patch file with its size, in whole megabytes.
    let file = &shown.rows[2];
    assert_eq!(
        (file.label.as_str(), file.ok, file.detail.as_str()),
        ("패치 파일", Some(true), "00421to00422.patch · 1,229 MB")
    );
}

#[test]
fn a_quiet_check_is_listed_only_when_it_has_failed_with_what_a_dialog_says_of_it() {
    let shown = Scene::ready(game_open(game()), Some(patch_to("1.2.422"))).shown();
    assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Running, RowKind::Space, RowKind::PatchFile]);
    let running = &shown.rows[1];
    assert_eq!(
        (running.label.as_str(), running.ok, running.detail.as_str()),
        ("메이플스토리를 종료한 후 실행해 주세요.", Some(false), "")
    );
    assert_eq!(running.message, "메이플스토리를 종료한 후 실행해 주세요.");
}

#[test]
fn what_a_run_left_is_listed_after_the_checks_and_before_the_patch_file() {
    let shown = Scene::ready(resumable(game(), None, Some(41.0)), Some(patch_to("1.2.422"))).shown();
    assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::Leftover, RowKind::PatchFile]);
    let left = &shown.rows[2];
    assert_eq!((left.label.as_str(), left.ok, left.detail.as_str()), ("이전 패치 작업", None, "12.3 GB"));
}

#[test]
fn the_patch_file_line_is_empty_without_a_patch_and_missing_without_a_game_folder() {
    let none = Scene::ready(game(), Some(up_to_date("1.2.421"))).shown();
    let file = none.rows.last().unwrap();
    assert_eq!((file.kind, file.ok, file.detail.as_str()), (RowKind::PatchFile, None, ""));

    let empty = Scene::ready(no_game_at(r"D:\Empty"), Some(Availability::nothing())).shown();
    assert_eq!(kinds(&empty), [RowKind::Folder]);
    assert_eq!((empty.rows[0].ok, empty.rows[0].message.as_str()), (Some(false), "메이플스토리 설치 경로가 아닙니다."));
    // Before the first look there is nothing to list.
    assert!(Scene { status: None, ..Scene::ready(game(), None) }.shown().rows.is_empty());
}

#[test]
fn a_cut_off_apply_that_could_not_be_undone_is_a_line_of_its_own() {
    let cut_off = Check::failed(CheckKind::Recovery, "실패", "이전 패치를 복구하지 못했습니다.");
    let mut status = game();
    status.checks.push(cut_off);
    let shown = Scene::ready(status, Some(errored("이전 패치 복구 대기"))).shown();
    assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::Recovery, RowKind::PatchFile]);
    assert_eq!((shown.status.as_str(), shown.status_bad), ("이전 패치 복구 대기", true));
}

/// The immediate way is chosen on a drive that does not take it. The page is told so
/// as a line of its own kind, which it shows on its line for the choice; the line for
/// the room is as it would be, and a press says what is in the way.
#[test]
fn a_way_of_patching_the_drive_does_not_take_is_a_line_of_its_own_kind() {
    let sentence = "즉시 패치는 NTFS 드라이브에서만 사용할 수 있습니다.";
    let mut status = game();
    status.checks.push(Check::failed(CheckKind::Way, "즉시 패치 불가", sentence));
    assert!(status.blocked());
    let shown = Scene::ready(status, Some(patch_to("1.2.422"))).shown();
    assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::Way, RowKind::PatchFile]);
    let (room, way) = (&shown.rows[1], &shown.rows[2]);
    assert_eq!(room.ok, Some(true));
    assert_eq!(
        (way.label.as_str(), way.ok, way.detail.as_str(), way.message.as_str()),
        ("패치 방식", Some(false), "즉시 패치 불가", sentence)
    );
    assert_eq!(serde_json::to_value(way.kind).unwrap(), serde_json::json!("way"));
    assert_eq!((shown.status.as_str(), shown.status_bad), ("패치 불가", true));
    // The button is there to be pressed, and says what is in the way: not the main one.
    assert!(shown.button.enabled && !shown.button.primary);
}

/// An apply was cut off and the folder is being put back, which can take minutes. The
/// footer says what is waited for, as something that is at work and not as bad news,
/// and no line of the screen speaks of it: whatever the server has last said.
#[test]
fn a_folder_that_is_being_put_back_is_waited_for_and_is_no_bad_news() {
    let putting_back = || {
        let mut status = game();
        status.putting_back = true;
        (status.version, status.resumable) = (None, false);
        status
    };
    for answer in [None, Some(errored("이전 패치 복구 대기")), Some(patch_to("1.2.422"))] {
        let shown = Scene::ready(putting_back(), answer).shown();
        assert_eq!(kinds(&shown), [RowKind::Folder, RowKind::Space, RowKind::PatchFile]);
        assert_eq!((shown.status.as_str(), shown.status_bad, shown.busy), ("이전 패치 복구 대기", false, true));
        assert_eq!(shown.current, None);
        // Whichever button it is, it is off until the folder is back.
        assert!(!shown.button.enabled && !shown.button.primary);
    }
}

// The line under a finished run.

#[test]
fn a_finished_run_says_how_long_it_took_and_how_many_files_it_changed() {
    assert_eq!(Scene::done(None).shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개"));
    let quick = Scene::with_run(Phase::Done, |run| (run.took, run.files) = (59, 3));
    let quick = Scene { answer: None, ..quick };
    assert_eq!(quick.shown().summary.as_deref(), Some("소요 시간 59초 · 변경 파일 3개"));
    // No other screen has the line.
    for phase in [Phase::Ready, Phase::Running, Phase::Paused, Phase::Failed] {
        assert_eq!(Scene::with_run(phase, |_| ()).shown().summary, None, "{phase:?}");
    }
}

/// What comes next is said the moment the server says it, and the button with it.
#[test]
fn the_summary_and_the_button_change_together_when_the_next_answer_arrives() {
    // The patch has just finished: the server is being asked, and the button waits.
    let settling = Scene::done(None);
    assert_eq!(settling.shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개"));
    assert_eq!(settling.button(), (ButtonKind::Launch, false, false));

    let nothing_more = Scene::done(Some(up_to_date("1.2.422")));
    assert_eq!(nothing_more.shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개 · 추가 패치 없음"));
    assert_eq!(nothing_more.button(), (ButtonKind::Launch, true, true));

    // Another patch comes out while the summary is up.
    let another = Scene::done(Some(patch_to("1.2.423")));
    assert_eq!(another.shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개 · 다음 패치 1.2.423"));
    assert_eq!(another.button(), (ButtonKind::Download, true, true));
}

#[test]
fn a_check_that_fails_after_a_run_adds_nothing_to_the_summary() {
    let failed = Scene::done(Some(errored("서버 요청 실패")));
    assert_eq!(failed.shown().summary.as_deref(), Some("소요 시간 1분 35초 · 변경 파일 718개"));
    assert_eq!(failed.button(), (ButtonKind::Launch, true, true));
}

// The button.

#[test]
fn a_game_that_is_not_installed_offers_its_download() {
    assert_eq!(Scene::ready(no_install(), Some(Availability::nothing())).button(), (ButtonKind::Install, true, true));
    // A folder that was picked and is not there is another matter: there the folder is the trouble.
    let picked = Status { custom_dir: true, ..no_install() };
    assert_eq!(Scene::ready(picked, Some(Availability::nothing())).button(), (ButtonKind::Download, false, false));
}

#[test]
fn a_check_that_could_not_be_made_offers_to_ask_again() {
    assert_eq!(Scene::ready(game(), Some(errored("서버 요청 실패"))).button(), (ButtonKind::CheckAgain, true, false));
    // With a patch that was found before, or a run to go on with, that is offered instead.
    let stale = Availability { error: Some("서버 요청 실패".to_owned()), ..patch_to("1.2.422") };
    assert_eq!(Scene::ready(game(), Some(stale)).button(), (ButtonKind::Download, true, true));
    let own = Scene::ready(resumable(game(), Some(GIVEN), None), Some(errored("서버 요청 실패")));
    assert_eq!(own.button(), (ButtonKind::Resume, true, true));
}

#[test]
fn an_install_that_is_up_to_date_offers_the_game() {
    assert_eq!(Scene::ready(game(), Some(up_to_date("1.2.421"))).button(), (ButtonKind::Launch, true, true));
}

#[test]
fn a_game_that_is_open_says_so_on_the_button_and_one_just_started_lets_it_rest() {
    let playing = Scene::ready(game_open(game()), Some(up_to_date("1.2.421")));
    assert_eq!(playing.button(), (ButtonKind::GameRunning, false, false));
    let started = Scene { launching: true, ..Scene::ready(game(), Some(up_to_date("1.2.421"))) };
    assert_eq!(started.button(), (ButtonKind::Launch, false, false));
}

#[test]
fn a_patch_offers_its_download_and_what_is_in_the_way_only_dims_the_button() {
    assert_eq!(Scene::ready(game(), Some(patch_to("1.2.422"))).button(), (ButtonKind::Download, true, true));
    // Pressing it says what is in the way.
    let playing = Scene::ready(game_open(game()), Some(patch_to("1.2.422")));
    assert_eq!(playing.button(), (ButtonKind::Download, true, false));
}

#[test]
fn a_patch_is_not_begun_in_a_folder_whose_game_files_are_being_checked() {
    // The check was asked for by the user and only reads: it is waited for, and the
    // button is off meanwhile, whichever it would be.
    let checking = Scene { checking_files: true, ..Scene::ready(game(), Some(patch_to("1.2.422"))) };
    assert_eq!(checking.button(), (ButtonKind::Download, false, false));
    let left = resumable(game(), None, Some(69.25));
    let checking = Scene { checking_files: true, ..Scene::ready(left, Some(patch_to("1.2.422"))) };
    assert_eq!(checking.button(), (ButtonKind::Resume, false, false));
    for phase in [Phase::Paused, Phase::Failed] {
        let kind = if phase == Phase::Paused { ButtonKind::Resume } else { ButtonKind::Retry };
        let halted = Scene::with_run(phase, |_| ());
        assert_eq!(halted.button(), (kind, true, true));
        assert_eq!(Scene { checking_files: true, ..halted }.button(), (kind, false, false));
    }
    // Starting the game is the user's to do all the while.
    let up_to_date = Scene { checking_files: true, ..Scene::ready(game(), Some(up_to_date("1.2.421"))) };
    assert_eq!(up_to_date.button(), (ButtonKind::Launch, true, true));
}

#[test]
fn nothing_to_start_disables_the_button_and_an_app_that_is_too_old_does_not() {
    assert_eq!(Scene::ready(game(), None).button(), (ButtonKind::Download, false, false));
    assert_eq!(Scene { status: None, ..Scene::ready(game(), None) }.button(), (ButtonKind::Download, false, false));
    let outdated = Scene { outdated: true, ..Scene::ready(game(), Some(patch_to("1.2.422"))) };
    assert_eq!(outdated.button(), (ButtonKind::Download, true, true));
}

#[test]
fn a_run_that_was_left_unfinished_offers_to_go_on_from_the_ready_screen() {
    let left = Scene::ready(resumable(game(), None, Some(69.25)), Some(patch_to("1.2.422")));
    assert_eq!(left.button(), (ButtonKind::Resume, true, true));
    // Work that nothing goes on with leaves the button to the game.
    let stranded = Scene::ready(resumable(game(), None, Some(69.25)), Some(up_to_date("1.2.421")));
    assert_eq!(stranded.button(), (ButtonKind::Launch, true, true));
    // While the server has not said, going on is offered and cannot be pressed.
    let unanswered = Scene::ready(resumable(game(), None, Some(69.25)), None);
    assert_eq!(unanswered.button(), (ButtonKind::Resume, false, false));
}

#[test]
fn a_finished_run_offers_the_next_patch_as_a_download_never_as_one_to_go_on_with() {
    assert_eq!(Scene::done(Some(patch_to("1.2.423"))).button(), (ButtonKind::Download, true, true));
    let mut left_over = Scene::done(Some(patch_to("1.2.423")));
    left_over.status = Some(resumable(game(), None, None));
    assert_eq!(left_over.button(), (ButtonKind::Download, true, true));
}

#[test]
fn a_run_that_stopped_offers_to_go_on_and_one_that_failed_to_try_again() {
    assert_eq!(Scene::with_run(Phase::Paused, |_| ()).button(), (ButtonKind::Resume, true, true));
    assert_eq!(Scene::with_run(Phase::Failed, |_| ()).button(), (ButtonKind::Retry, true, true));
    assert_eq!(Scene::with_run(Phase::Failed, |run| run.refused = true).button(), (ButtonKind::Retry, true, true));
    // Too old a version of the app keeps neither from being pressed.
    for phase in [Phase::Paused, Phase::Failed] {
        let outdated = Scene { outdated: true, ..Scene::with_run(phase, |_| ()) };
        assert_eq!(outdated.button(), Scene::with_run(phase, |_| ()).button(), "{phase:?}");
    }
}

#[test]
fn a_run_at_work_offers_the_pause_until_it_is_stopping_or_applying() {
    assert_eq!(Scene::with_run(Phase::Running, |_| ()).button(), (ButtonKind::Pause, true, false));
    assert_eq!(Scene::with_run(Phase::Starting, |run| run.step = None).button(), (ButtonKind::Pause, true, false));
    assert_eq!(Scene::with_run(Phase::Stopping, |_| ()).button(), (ButtonKind::Pause, false, false));
    // An apply runs to its end.
    let applying = Scene::with_run(Phase::Running, |run| run.step = Some(Step::Apply));
    assert_eq!(applying.button(), (ButtonKind::Pause, false, false));
}

#[test]
fn a_failure_that_is_still_to_be_read_changes_nothing_on_the_ready_screen() {
    let failure = Failure {
        id: 1,
        title: "진행 중인 패치와 다른 패치 파일입니다.".to_owned(),
        message: None,
        details: Vec::new(),
    };
    let mut said = Scene::ready(half_patched(game(), None, Some(41.0)), Some(patch_to("1.2.421")));
    let quiet = said.shown();
    said.run = Some(Run { failure: Some(failure), ..run_of(Some(GIVEN_WRONG), None) });
    assert_eq!(said.shown(), quiet);
    // The half-patched folder's own screen: the bar where the run was left, and going on.
    assert_eq!((quiet.status.as_str(), quiet.percent), ("중단된 패치 있음", Some(41.0)));
    assert_eq!(said.button(), (ButtonKind::Resume, true, true));
}

/// The page names the newest version on one line and says in the footer whether the
/// install is at it. The two are one statement: while there is no newest version to
/// name, the footer does not say that the install is up to date, whatever the answer
/// says. It waits, and the figure and the button wait with it.
#[test]
fn the_footer_does_not_call_the_install_up_to_date_while_the_page_has_no_newest_version_to_name() {
    let unnamed = Scene { latest: None, ..Scene::ready(game(), Some(up_to_date("1.2.421"))) };
    let shown = unnamed.shown();
    assert_eq!((shown.latest.as_deref(), shown.percent), (None, Some(0.0)));
    assert_eq!(unnamed.footer(), word("최신 버전 확인 중", false, true));
    assert_eq!(unnamed.button(), (ButtonKind::Download, false, false));
    // So it is with the game open, which is the one thing that may be in the way of a
    // folder that is up to date.
    let playing = Scene { latest: None, ..Scene::ready(game_open(game()), Some(up_to_date("1.2.421"))) };
    assert_eq!(playing.footer().0, "최신 버전 확인 중");

    // With the version named, both lines say it.
    let named = Scene::ready(game(), Some(up_to_date("1.2.421")));
    let shown = named.shown();
    assert_eq!((shown.latest.as_deref(), shown.percent), (Some("1.2.421"), Some(100.0)));
    assert_eq!(named.footer(), word("최신 버전", false, false));
    assert_eq!(named.button(), (ButtonKind::Launch, true, true));

    // A check that could not be made says why, with or without a version from before.
    for latest in [None, Some("1.2.421".to_owned())] {
        let failed = Scene { latest, ..Scene::ready(game(), Some(errored("서버 요청 실패"))) };
        assert_eq!(failed.footer(), word("서버 요청 실패", true, false));
    }
    // While the server is asked again, the version it had before stays on the page.
    let asking = Scene { latest: Some("1.2.421".to_owned()), ..Scene::ready(game(), None) };
    assert_eq!(asking.shown().latest.as_deref(), Some("1.2.421"));
    assert_eq!(asking.footer(), word("최신 버전 확인 중", false, true));
}

/// The 패치 파일 line says which file the patch is, by the name it has on the server,
/// and how large it is. The page is given the address to begin from when the user
/// states where the patch is themselves.
#[test]
fn the_patch_file_line_names_the_file_and_the_page_is_given_an_address_to_begin_from() {
    let found = Scene::ready(game(), Some(patch_to("1.2.422"))).shown();
    assert_eq!(found.rows.last().unwrap().detail, "00421to00422.patch · 1,229 MB");
    assert_eq!(found.address, "https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch");

    // A minor patch has the name Nexon gives it on the server, which is not the one its
    // file is kept under on this PC.
    let mut minor = minor_patch_to("1.2.421 마이너버전(4)", 4);
    let of_minor = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch";
    if let Some(target) = &mut minor.target {
        (target.url, target.file_name) = (of_minor.to_owned(), "00421-minor-03to04.patch".to_owned());
    }
    let shown = Scene::ready(game(), Some(minor)).shown();
    assert_eq!(
        (shown.rows.last().unwrap().detail.as_str(), shown.address.as_str()),
        ("03to04.patch · 1,229 MB", of_minor)
    );

    // With no patch found, it is where the patch to the next version would be.
    let next = "https://maplestory.dn.nexoncdn.co.kr/Patch/00422/00421to00422.patch";
    let at_a_version = Status { next_address: Some(next.to_owned()), ..game() };
    for answer in [None, Some(up_to_date("1.2.421")), Some(errored("서버 요청 실패"))] {
        let shown = Scene::ready(at_a_version.clone(), answer).shown();
        assert_eq!((shown.rows.last().unwrap().detail.as_str(), shown.address.as_str()), ("", next));
    }
    // A folder whose version is not known has nothing to begin from.
    let unknown = Scene::ready(Status { version: None, ..game() }, Some(errored("현재 버전 확인 불가"))).shown();
    assert_eq!(unknown.address, "");
}

/// An apply was cut off and its record cannot be read: the folder can neither be gone
/// on with nor put back. The screen says so on the line for the recovery and in the
/// footer, offers nothing to continue, and still lists what the run left, to be removed.
#[test]
fn a_folder_whose_record_of_an_apply_cannot_be_read_is_shown_as_that_with_nothing_to_go_on_with() {
    let folder = journal_unreadable(game());
    let scene = Scene::ready(folder.clone(), Some(errored("작업 기록 손상")));
    let shown = scene.shown();
    let lines = [RowKind::Folder, RowKind::Space, RowKind::Recovery, RowKind::Leftover, RowKind::PatchFile];
    assert_eq!(kinds(&shown), lines);
    let recovery = &shown.rows[2];
    assert_eq!(
        (recovery.label.as_str(), recovery.ok, recovery.detail.as_str()),
        ("패치 복구", Some(false), "작업 기록 손상")
    );
    assert_eq!(recovery.message, "이전 패치 작업 기록이 손상되었습니다.\n공식 런처로 게임을 복구해 주세요.");
    assert_eq!((shown.rows[3].label.as_str(), shown.rows[3].detail.as_str()), ("이전 패치 작업", "12.3 GB"));
    assert_eq!((shown.current.as_deref(), shown.percent), (None, Some(0.0)));
    assert_eq!(scene.footer(), word("작업 기록 손상", true, false));
    // The button asks the server again, as after any check that had nothing to offer.
    // It is never the one that goes on with a run.
    assert_eq!(scene.button(), (ButtonKind::CheckAgain, true, false));
    let asking = Scene::ready(folder.clone(), None);
    assert_eq!(asking.button(), (ButtonKind::Download, false, false));
    // Should a patch be known for the folder all the same, it is not the next thing to do.
    let offered = Scene::ready(folder, Some(patch_to("1.2.422")));
    assert_eq!(offered.footer(), word("패치 불가", true, false));
    assert_eq!(offered.button(), (ButtonKind::Download, true, false));
}
