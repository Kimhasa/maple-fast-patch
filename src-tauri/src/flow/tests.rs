//! Tests of the flow: of what a site takes in, of its run on the screen, and of what the app
//! does by itself.

use super::*;
use crate::made::{
    GIVEN, GIVEN_ANOTHER, GIVEN_BEFORE, GIVEN_OTHER, GIVEN_WRONG, errored, game_at, game_open, half_patched,
    journal_unreadable, no_game_at, no_install, patch_to, resumable, up_to_date,
};

fn ended(answer: Availability, end: CheckEnd) -> Checked {
    Checked { answer, end }
}

/// Takes an answer in with the game open and no delay of this copy's own.
fn take(flow: &mut Flow, epoch: u64, checked: Checked, now: Instant) -> bool {
    flow.take_answer(epoch, checked, Some(true), now, 1_764_000_000_000, &|_| 0)
}

fn seconds(of: u64) -> Duration {
    Duration::from_secs(of)
}

/// The world as it is on a quiet day: nothing set, nothing at work, the game open.
fn world(now: Instant) -> World {
    World {
        auto_patch: false,
        auto_start: false,
        outdated: false,
        run_at_work: false,
        open: Some(true),
        sees_open: true,
        checking_files: false,
        update_asked: true,
        now,
    }
}

/// A site that has looked at its game folder and was told of the patch to 1.2.422.
fn with_a_patch(now: Instant) -> Flow {
    let mut flow = Flow::new(now);
    flow.take_look(game_at(r"C:\Games\Maple"), now);
    take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Answered), now);
    flow
}

/// Begins the run a start asks for, the way the app does.
fn begin(flow: &mut Flow, address: Option<&str>, now: Instant) -> Result<(), Refusal> {
    let pick = pick(&flow.facts(&world(now)), address, flow.left())?;
    flow.begin(&pick, now);
    flow.at_work();
    Ok(())
}

fn says(step: Step, percent: f64, activity: &str) -> Progress {
    Progress {
        percent,
        step: Some(step),
        status: "새 게임 파일 생성 중",
        done: 5,
        total: 10,
        worked: 5,
        activity: activity.to_owned(),
    }
}

fn failed(message: &str) -> RunEnd {
    RunEnd::Failed { message: message.to_owned(), details: Vec::new() }
}

fn refused(message: &str) -> RunEnd {
    RunEnd::Refused { message: message.to_owned(), details: Vec::new() }
}

// The pace of the looks and the checks.

#[test]
fn the_pace_of_the_checks_is_the_owners_table() {
    let (none, most) = (|_: u64| 0, |most: u64| most);
    let table: [(Option<bool>, u32, u64, u64); 12] = [
        // A maintenance is underway: every half minute, and up to ten seconds of its own.
        (Some(false), 0, 30, 40),
        // No maintenance: every ten minutes, and up to a minute of its own.
        (Some(true), 0, 600, 660),
        // Nexon's word on it could not be read.
        (None, 0, 120, 120),
        // Checks that fail: each wait twice the one before.
        (Some(false), 1, 30, 30),
        (Some(false), 2, 60, 60),
        (Some(false), 4, 240, 240),
        (Some(false), 5, 300, 300),
        (Some(false), 30, 300, 300),
        (Some(true), 1, 30, 30),
        (Some(true), 5, 480, 480),
        (Some(true), 6, 600, 600),
        (None, 60, 600, 600),
    ];
    for (open, failures, soonest, latest) in table {
        let what = format!("open {open:?}, {failures} failed");
        assert_eq!(next_check_in(open, failures, &none), seconds(soonest), "{what}");
        assert_eq!(next_check_in(open, failures, &most), seconds(latest), "{what}");
    }
}

/// The owner decided on half a minute during a maintenance and seldom otherwise.
#[test]
fn the_server_is_never_asked_more_often_than_every_half_minute() {
    for open in [Some(true), Some(false), None] {
        for failures in 0..40 {
            assert!(next_check_in(open, failures, &|_| 0) >= seconds(30), "open {open:?}, {failures} failed");
        }
    }
}

#[test]
fn the_folder_is_looked_at_every_three_seconds_and_seldom_while_nobody_sees_it() {
    let (seen, unseen, clear, blocked) = (false, true, false, true);
    // Never looked at, or asked to: at once.
    assert!(look_due(None, false, false, unseen, clear));
    assert!(look_due(Some(seconds(0)), true, false, unseen, clear));
    // By the clock.
    assert!(!look_due(Some(seconds(2)), false, false, seen, clear));
    assert!(look_due(Some(seconds(3)), false, false, seen, clear));
    // A window that is put away or made small.
    assert!(!look_due(Some(seconds(29)), false, false, unseen, clear));
    assert!(look_due(Some(seconds(30)), false, false, unseen, clear));
    // Something is in the way that whoever waits for it is to see go.
    assert!(look_due(Some(seconds(3)), false, false, unseen, blocked));
    // Not while a run is at work, whoever asks.
    for asked in [false, true] {
        assert!(!look_due(Some(seconds(60)), asked, true, seen, clear));
        assert!(!look_due(None, asked, true, seen, clear));
    }
}

#[test]
fn a_check_begins_when_its_time_has_come_and_nothing_else_is_at_work() {
    let due = Instant::now();
    assert!(check_due(due, due, false, false, true));
    assert!(check_due(due + seconds(1), due, false, false, true));
    assert!(!check_due(due - seconds(1), due, false, false, true));
    // One check at a time, none during a run, and none for a folder with nothing to ask about.
    assert!(!check_due(due, due, true, false, true));
    assert!(!check_due(due, due, false, true, true));
    assert!(!check_due(due, due, false, false, false));
}

#[test]
fn a_check_is_due_as_the_app_starts_and_begins_once_the_folder_is_known() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    assert!(flow.look_is_due(start, false, false));
    assert!(!flow.check_is_due(start, false));
    assert!(!flow.take_look(game_at(r"C:\Games\Maple"), start));
    assert!(flow.check_is_due(start, false) && !flow.check_is_due(start, true));
    assert_eq!((flow.folder_epoch, flow.answer.is_none()), (0, true));
}

/// The epoch rule: a check is about the folder it began under.
#[test]
fn an_answer_about_a_folder_the_site_has_left_is_dropped() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    let begun_under = flow.folder_epoch;
    flow.checking = true;
    // The user picks another folder while the check is at work.
    assert!(flow.take_look(game_at(r"D:\Copy\Maple"), start + seconds(1)));
    assert_eq!(flow.folder_epoch, begun_under + 1);

    let late = start + seconds(4);
    assert!(!take(&mut flow, begun_under, ended(patch_to("1.2.422"), CheckEnd::Answered), late));
    assert!(flow.answer.is_none() && flow.latest.is_none() && flow.checked_at.is_none());
    // The check for the folder it has now is still due, and can begin: the old one is over.
    assert!(flow.check_is_due(late, false));

    let now_under = flow.folder_epoch;
    assert!(take(&mut flow, now_under, ended(up_to_date("1.2.421"), CheckEnd::Answered), late));
    assert_eq!(flow.latest.as_deref(), Some("1.2.421"));
}

#[test]
fn another_folder_puts_aside_what_was_found_and_the_same_folder_keeps_it() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Failed), start);
    assert_eq!(flow.failures, 1);

    // The same folder, looked at again: nothing changes but the look.
    assert!(!flow.take_look(game_at(r"C:\Games\Maple"), start + seconds(3)));
    assert!(flow.answer.is_some() && flow.latest.is_some() && flow.next_check > start + seconds(3));

    // Another one, or none where there was one.
    for other in [game_at(r"D:\Copy\Maple"), no_install()] {
        let mut flow = Flow::new(start);
        flow.take_look(game_at(r"C:\Games\Maple"), start);
        take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Failed), start);
        let now = start + seconds(3);
        assert!(flow.take_look(other, now));
        assert_eq!((flow.folder_epoch, flow.failures, flow.next_check), (1, 0, now));
        assert!(flow.latest.is_none() && flow.checked_at.is_none());
        assert!(flow.answer.as_ref().is_none_or(|answer| answer.target.is_none() && answer.latest.is_none()));
    }
}

/// The screen tells "nothing to ask" from "not asked yet" by there being an answer.
#[test]
fn a_folder_with_nothing_to_ask_about_has_its_answer_without_a_check() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(no_game_at(r"D:\Empty"), start);
    let answer = flow.answer.as_ref().expect("the answer that there is nothing");
    assert!(answer.target.is_none() && answer.latest.is_none() && answer.error.is_none());
    assert!(!flow.check_is_due(start + seconds(60), false));

    // The game is installed into it: the same folder, and now one to ask about.
    let now = start + seconds(90);
    assert!(!flow.take_look(game_at(r"D:\Empty"), now));
    assert!(flow.answer.is_none());
    assert_eq!((flow.folder_epoch, flow.next_check), (0, now));
}

#[test]
fn an_answer_sets_the_time_of_the_next_check_by_how_the_check_ended() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(game_at(r"C:\Games\Maple"), start);

    // The server could not be asked: what stood before stands with the reason, and
    // the waits grow.
    for (failed, wait) in [(1, 30), (2, 60), (3, 120)] {
        flow.checking = true;
        assert!(take(&mut flow, 0, ended(errored("서버 요청 실패"), CheckEnd::Failed), start));
        assert_eq!((flow.failures, flow.next_check, flow.checking), (failed, start + seconds(wait), false));
    }
    assert_eq!(flow.checked_at, None);
    assert!(flow.answer.as_ref().is_some_and(|answer| answer.error.is_some()));

    // The first answer brings the pace back, and says when the server was last heard.
    assert!(take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Answered), start));
    assert_eq!((flow.failures, flow.next_check), (0, start + seconds(600)));
    assert_eq!((flow.checked_at, flow.latest.as_deref()), (Some(1_764_000_000_000), Some("1.2.422")));

    // A failure after that keeps the newest version that was seen, and the time it was.
    assert!(take(&mut flow, 0, ended(errored("서버 요청 실패"), CheckEnd::Failed), start));
    assert_eq!((flow.checked_at, flow.latest.as_deref()), (Some(1_764_000_000_000), Some("1.2.422")));

    // During a maintenance the next check comes in half a minute.
    flow.take_answer(0, ended(up_to_date("1.2.422"), CheckEnd::Answered), Some(false), start, 0, &|_| 0);
    assert_eq!(flow.next_check, start + seconds(30));
}

#[test]
fn a_check_that_gave_up_is_made_again_shortly_and_leaves_what_was_found() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);

    let now = start + seconds(700);
    flow.checking = true;
    assert!(!take(&mut flow, 0, ended(Availability::nothing(), CheckEnd::Stopped), now));
    assert_eq!((flow.next_check, flow.failures, flow.checking), (now + seconds(5), 0, false));
    assert!(flow.answer.as_ref().is_some_and(|answer| answer.target.is_some()));

    // A folder that is still being put back says so, and is asked about again as soon.
    assert!(take(&mut flow, 0, ended(errored("이전 패치 복구 대기"), CheckEnd::NotYet), now));
    assert_eq!(flow.next_check, now + seconds(5));
    assert!(flow.answer.as_ref().is_some_and(|answer| answer.error.is_some()));
}

#[test]
fn a_maintenance_that_begins_or_ends_makes_a_check_due_at_once() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    // The first word of the lamp is no turn of it when the game is open.
    assert!(!flow.see_pace(Some(true), start));
    take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
    assert_eq!(flow.next_check, start + seconds(600));

    let begins = start + seconds(200);
    assert!(flow.see_pace(Some(false), begins));
    assert_eq!(flow.next_check, begins);
    // While nothing changes nothing is due.
    take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), begins);
    assert!(!flow.see_pace(Some(false), begins + seconds(1)));
    assert!(flow.next_check > begins + seconds(1));

    let ends = begins + seconds(3600);
    assert!(flow.see_pace(Some(true), ends));
    assert_eq!(flow.next_check, ends);
    // Word that cannot be read for a while is neither.
    assert!(!flow.see_pace(None, ends + seconds(1)));
    assert!(!flow.see_pace(Some(true), ends + seconds(2)));
    // A maintenance that is found underway when word comes back is one that began.
    flow.see_pace(None, ends + seconds(3));
    assert!(flow.see_pace(Some(false), ends + seconds(4)));
}

#[test]
fn asking_again_puts_the_answer_aside_and_makes_a_check_due() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    take(&mut flow, 0, ended(errored("서버 요청 실패"), CheckEnd::Failed), start);
    let now = start + seconds(2);
    flow.ask_again(now);
    assert!(flow.answer.is_none());
    assert_eq!(flow.next_check, now);
    // Where there is nothing to ask about, the answer that says so stays.
    let mut flow = Flow::new(start);
    flow.take_look(no_game_at(r"D:\Empty"), start);
    flow.ask_again(now);
    assert!(flow.answer.is_some());
}

// The run and its phases.

#[test]
fn a_run_goes_from_ready_through_starting_to_running_and_is_done() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    assert_eq!(flow.phase, Phase::Ready);
    let pick = pick(&flow.facts(&world(start)), None, None).unwrap();
    assert_eq!((pick.address(), pick.target().map(|target| target.label.as_str())), (None, Some("1.2.422")));

    flow.begin(&pick, start);
    assert_eq!(flow.phase, Phase::Starting);
    let run = flow.run.as_ref().unwrap();
    assert_eq!(
        (run.source.as_deref(), run.label.as_deref(), run.step, run.percent),
        (None, Some("1.2.422"), None, 0.0)
    );
    flow.at_work();
    assert_eq!(flow.phase, Phase::Running);

    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start + seconds(30));
    flow.end(RunEnd::Done { seconds: 95, files: 718 }, start + seconds(95));
    assert_eq!(flow.phase, Phase::Done);
    let run = flow.run.as_ref().unwrap();
    assert_eq!((run.percent, run.took, run.files), (100.0, 95, 718));
    // What the server had for the folder before no longer holds; it is asked at once,
    // and the game is to be started if the user wants that.
    assert!(flow.answer.is_none() && flow.game_due);
    assert_eq!(flow.next_check, start + seconds(95));
    assert!(flow.look_is_due(start + seconds(95), false, false));
    // Nothing is hands-off after a patch that went through.
    assert!(flow.hands_off.is_empty());
}

#[test]
fn a_second_start_finds_the_first_at_work() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    let at_work = World { run_at_work: true, ..world(start) };
    assert_eq!(decide(&flow.facts(&World { auto_patch: true, ..at_work })), Decision::Nothing);
    // Whoever asks, and for whichever patch: the folder is the run's until it has ended.
    let asked = |flow: &Flow, address| pick(&flow.facts(&world(start)), address, None).map_err(|refusal| refusal.kind);
    assert_eq!(asked(&flow, None), Err(RefusalKind::Busy));
    assert_eq!(asked(&flow, Some(GIVEN_OTHER)), Err(RefusalKind::Busy));
    flow.stop(false);
    assert_eq!(asked(&flow, None), Err(RefusalKind::Busy));
    flow.end(RunEnd::Stopped, start);
    assert!(asked(&flow, None).is_ok());
}

#[test]
fn a_pause_stops_the_run_and_leaves_what_it_was_working_on_on_the_screen() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start + seconds(30));

    assert!(flow.stop(false));
    assert_eq!(flow.phase, Phase::Stopping);
    assert!(!flow.run.as_ref().unwrap().leaving);
    flow.end(RunEnd::Stopped, start + seconds(31));
    assert_eq!(flow.phase, Phase::Paused);
    let run = flow.run.as_ref().unwrap();
    assert_eq!((run.percent, run.activity.as_str(), run.failure.as_ref()), (61.5, "생성 중: Map001.wz", None));
    // There is nothing to stop any more.
    assert!(!flow.stop(false));
    assert_eq!(flow.phase, Phase::Paused);
}

/// What closing the window does, for every way the app and its run can stand.
#[test]
fn closing_the_window_asks_about_a_run_at_work_and_only_once() {
    let table = [
        // The app is not set to stay next to the clock.
        (false, AtClose::Idle, OnClose::Leave),
        (false, AtClose::AtWork, OnClose::Ask),
        // Closed again in the same press: nothing was asked that anyone has read.
        (false, AtClose::Asked(Duration::ZERO), OnClose::Ask),
        (false, AtClose::Asked(Duration::from_millis(499)), OnClose::Ask),
        // Closed again over the question, and closed on a run that is stopping.
        (false, AtClose::Asked(Duration::from_millis(500)), OnClose::StopThenLeave),
        (false, AtClose::Asked(seconds(60)), OnClose::StopThenLeave),
        (false, AtClose::Stopping, OnClose::StopThenLeave),
        // It stays there: closing only puts the window away, whatever is at work.
        (true, AtClose::Idle, OnClose::Hide),
        (true, AtClose::AtWork, OnClose::Hide),
        (true, AtClose::Asked(Duration::ZERO), OnClose::Hide),
        (true, AtClose::Asked(seconds(60)), OnClose::Hide),
        (true, AtClose::Stopping, OnClose::Hide),
    ];
    for (stays, run, does) in table {
        assert_eq!(on_close(stays, run), does, "stays: {stays}, run: {run:?}");
    }
}

#[test]
fn an_answer_puts_the_window_away_only_for_a_run_that_is_still_asked_about() {
    use CloseAnswer::{Background, Quit, Stay};
    let table = [
        (Background, true, Background),
        // The run has ended over the question: the window stays where it is.
        (Background, false, Stay),
        (Quit, true, Quit),
        (Quit, false, Quit),
        (Stay, true, Stay),
        (Stay, false, Stay),
    ];
    for (answer, open, comes_to) in table {
        assert_eq!(answer.given(open), comes_to, "{answer:?}, open: {open}");
    }
    // The page says them by these names.
    let named = |name: &str| serde_json::from_value::<CloseAnswer>(serde_json::json!(name)).ok();
    assert_eq!((named("background"), named("quit"), named("stay")), (Some(Background), Some(Quit), Some(Stay)));
    assert_eq!(named("leave"), None);
}

/// The way of patching is chosen on the ready screen, and the line for the room goes
/// by it: the folder is looked at again at once, also in a window nobody looks at.
#[test]
fn a_folder_is_looked_at_again_at_once_when_the_way_of_patching_is_chosen() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    assert!(!flow.look_is_due(start + seconds(1), false, true));
    flow.look_soon();
    assert!(flow.look_is_due(start + seconds(1), false, true));
    // Not beside the site's own run, which has the folder to itself.
    assert!(!flow.look_is_due(start + seconds(1), true, true));
}

/// The window is asked to close, well after the question it has open was asked.
fn close(flow: &mut Flow, stays: bool) -> OnClose {
    let asked_at = flow.run.as_ref().and_then(|run| run.asked_at);
    flow.close(stays, asked_at.map_or_else(Instant::now, |at| at + seconds(2)))
}

/// A button that is pressed twice in a row, or a mouse that bounces, closes the window
/// twice within a moment. The second time is no answer to a question nobody has read.
#[test]
fn a_window_closed_twice_in_one_press_asks_once_and_stops_nothing() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    let pressed = start + seconds(10);
    assert_eq!(flow.close(false, pressed), OnClose::Ask);
    for again in [0, 150, 499] {
        assert_eq!(flow.close(false, pressed + Duration::from_millis(again)), OnClose::Ask, "{again} ms");
        assert_eq!(flow.phase, Phase::Running);
    }
    // The question counts from when it was first asked, however often the press came
    // again. From half a second on, closing again is the answer it was before.
    assert_eq!(flow.run.as_ref().unwrap().asked_at, Some(pressed));
    assert_eq!(flow.close(false, pressed + Duration::from_millis(500)), OnClose::StopThenLeave);

    // Answered and asked anew, it counts anew.
    assert!(flow.answered());
    assert_eq!(flow.run.as_ref().unwrap().asked_at, None);
    let later = pressed + seconds(30);
    assert_eq!(flow.close(false, later), OnClose::Ask);
    assert_eq!(flow.close(false, later + Duration::from_millis(100)), OnClose::Ask);
    assert_eq!(flow.close(false, later + seconds(1)), OnClose::StopThenLeave);
}

/// Closing the window during a run, when the app is not set to stay next to the clock.
#[test]
fn a_window_closed_on_a_run_asks_first_and_stops_the_run_when_it_is_closed_again() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    // Nothing is at work: there is nothing to ask about.
    assert_eq!(close(&mut flow, false), OnClose::Leave);

    begin(&mut flow, None, start).unwrap();
    assert_eq!(close(&mut flow, false), OnClose::Ask);
    assert_eq!(flow.phase, Phase::Running);
    assert!(flow.run.as_ref().unwrap().asking);
    // The question stands, whether or not a page shows it. Closed again, the run is
    // asked to stop, and the app goes when it has: nothing more is asked from there.
    assert_eq!(close(&mut flow, false), OnClose::StopThenLeave);
    assert!(flow.stop(true));
    assert_eq!(flow.phase, Phase::Stopping);
    let run = flow.run.as_ref().unwrap();
    assert!(run.leaving && !run.asking);
    assert_eq!(close(&mut flow, false), OnClose::StopThenLeave);
    // An answer that comes now is one to a question that is over.
    assert!(!flow.answered());
    assert_eq!(CloseAnswer::Background.given(false), CloseAnswer::Stay);
}

#[test]
fn a_question_that_was_answered_is_asked_anew_at_the_next_close() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    // A run that still waits for its turn at the folder is at work as well.
    let pick = pick(&flow.facts(&world(start)), None, None).unwrap();
    flow.begin(&pick, start);
    assert_eq!(close(&mut flow, false), OnClose::Ask);
    // Answered, or put aside: the run goes on either way, and it is answered once.
    assert!(flow.answered());
    assert!(!flow.answered());
    flow.at_work();
    assert_eq!(flow.phase, Phase::Running);
    assert!(!flow.run.as_ref().unwrap().asking);
    assert_eq!(close(&mut flow, false), OnClose::Ask);
}

#[test]
fn a_run_that_is_stopping_is_not_asked_about() {
    let start = Instant::now();
    // Paused by the user, and the window closed before the run has stopped.
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.stop(false);
    assert_eq!(close(&mut flow, false), OnClose::StopThenLeave);
    assert!(!flow.run.as_ref().unwrap().asking);
    // A pause that was asked for before stays one that leaves once the window is closed.
    assert!(flow.stop(true));
    assert_eq!(flow.phase, Phase::Stopping);
    assert!(flow.run.as_ref().unwrap().leaving);

    // Paused over the question: the question goes with it.
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    assert_eq!(close(&mut flow, false), OnClose::Ask);
    flow.stop(false);
    assert!(!flow.answered());
    assert_eq!(close(&mut flow, false), OnClose::StopThenLeave);
}

#[test]
fn a_run_that_ends_over_the_question_takes_the_question_with_it() {
    let start = Instant::now();
    for end in [
        RunEnd::Done { seconds: 95, files: 718 },
        failed("새 게임 파일 검사에 실패했습니다."),
        refused("디스크 공간이 부족합니다."),
    ] {
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        assert_eq!(close(&mut flow, false), OnClose::Ask);
        flow.end(end, start + seconds(1));
        // How it ended stays on the screen, and the window with it: an answer that
        // would have put the window away comes to nothing.
        assert!(flow.run.as_ref().is_some_and(|run| !run.asking));
        assert_eq!(CloseAnswer::Background.given(flow.answered()), CloseAnswer::Stay);
        // Closed now, the window closes.
        assert_eq!(close(&mut flow, false), OnClose::Leave);
    }

    // The run after it is asked about like any other.
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    assert_eq!(close(&mut flow, false), OnClose::Ask);
    flow.end(failed("새 게임 파일 검사에 실패했습니다."), start + seconds(1));
    begin(&mut flow, None, start + seconds(2)).unwrap();
    assert!(!flow.run.as_ref().unwrap().asking);
    assert_eq!(close(&mut flow, false), OnClose::Ask);
}

#[test]
fn an_app_that_stays_next_to_the_clock_asks_nothing() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    assert_eq!(close(&mut flow, true), OnClose::Hide);
    assert!(!flow.run.as_ref().unwrap().asking);
    assert_eq!(flow.phase, Phase::Running);
}

#[test]
fn a_run_that_was_asked_to_stop_before_it_was_at_work_stays_one_that_is_stopping() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    let pick = pick(&flow.facts(&world(start)), None, None).unwrap();
    flow.begin(&pick, start);
    flow.stop(false);
    flow.at_work();
    assert_eq!(flow.phase, Phase::Stopping);
}

#[test]
fn a_run_that_never_got_going_returns_to_the_ready_screen() {
    let start = Instant::now();
    // Stopped before it had said anything: there is nothing to show of it.
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.end(RunEnd::Stopped, start);
    assert_eq!((flow.phase, flow.run.as_ref()), (Phase::Ready, None));

    // Failed or turned down before its first step: back to the ready screen, with what
    // there is to say of it.
    for (end, title) in [
        (failed("파일 처리 중 오류가 발생했습니다."), "패치에 실패했습니다."),
        (refused("이미 패치가 진행 중입니다."), "이미 패치가 진행 중입니다."),
    ] {
        let mut flow = with_a_patch(start);
        begin(&mut flow, None, start).unwrap();
        flow.end(end, start);
        assert_eq!(flow.phase, Phase::Ready);
        assert_eq!(
            flow.run.as_ref().and_then(|run| run.failure.as_ref()).map(|failure| failure.title.as_str()),
            Some(title)
        );
    }

    // A run that had said so much as its step stays on the screen.
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::VerifyPatch, 30.0, ""), start);
    flow.end(failed("패치 파일 검사에 실패했습니다."), start);
    assert_eq!(flow.phase, Phase::Failed);
}

#[test]
fn a_failure_is_said_once_each_with_what_went_wrong_under_its_headline() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::Rebuild, 50.0, "생성 중: Map001.wz"), start);
    let details = vec!["Map001.wz".to_owned()];
    flow.end(
        RunEnd::Failed {
            message: "새 게임 파일 검사에 실패했습니다.".to_owned(), details: details.clone()
        },
        start,
    );
    let first = flow.run.as_ref().unwrap().failure.clone().unwrap();
    let said = Some("새 게임 파일 검사에 실패했습니다.".to_owned());
    assert_eq!(first, Failure { id: 1, title: "패치에 실패했습니다.".to_owned(), message: said, details });

    // Tried again and turned down: what is in the way is the headline, the figures under it.
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::VerifyInstall, 50.0, "검사 중: Map001.wz"), start);
    let no_room = "디스크 공간이 부족합니다.\nC: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)";
    flow.end(refused(no_room), start);
    assert_eq!(flow.phase, Phase::Failed);
    let run = flow.run.as_ref().unwrap();
    let second = run.failure.as_ref().unwrap();
    assert!(run.refused);
    assert_eq!((second.id, second.title.as_str()), (2, "디스크 공간이 부족합니다."));
    assert_eq!(second.message.as_deref(), Some("C: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)"));
}

#[test]
fn the_bar_never_goes_back_within_a_patch() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    let told = flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start).unwrap();
    assert_eq!(told.percent, 61.5);
    flow.end(RunEnd::Stopped, start);

    // Taken up again from where it stands on the screen: the steps before are gone
    // through once more, and the figure waits for them.
    begin(&mut flow, None, start).unwrap();
    let run = flow.run.as_ref().unwrap();
    assert_eq!((run.percent, run.activity.as_str(), run.step), (61.5, "생성 중: Map001.wz", None));
    let told = flow.hear(says(Step::VerifyInstall, 33.0, "검사 중: Base.wz"), start).unwrap();
    assert_eq!((told.percent, told.step), (61.5, Some(Step::VerifyInstall)));
    assert_eq!(flow.run.as_ref().unwrap().activity, "검사 중: Base.wz");
    let told = flow.hear(says(Step::Rebuild, 70.25, "생성 중: Skill.wz"), start).unwrap();
    assert_eq!(told.percent, 70.25);
}

#[test]
fn a_run_that_was_put_away_is_taken_up_where_it_stood() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
    flow.end(RunEnd::Stopped, start);
    flow.dismiss();
    assert_eq!((flow.phase, flow.run.as_ref()), (Phase::Ready, None));

    begin(&mut flow, None, start).unwrap();
    let run = flow.run.as_ref().unwrap();
    assert_eq!((run.percent, run.activity.as_str()), (61.5, "생성 중: Map001.wz"));
    // It is remembered for that patch and no other, and once.
    flow.end(RunEnd::Stopped, start);
    flow.dismiss();
    begin(&mut flow, Some(GIVEN), start).unwrap();
    assert_eq!(flow.run.as_ref().unwrap().percent, 0.0);
    assert!(flow.left().is_none());
}

#[test]
fn the_bar_is_restored_after_a_restart_from_what_the_run_left_on_record() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    // The app was closed with the patch from the server at 69 percent.
    flow.take_look(resumable(game_at(r"C:\Games\Maple"), None, Some(69.25)), start);
    take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Answered), start);
    begin(&mut flow, None, start).unwrap();
    assert_eq!(flow.run.as_ref().unwrap().percent, 69.25);

    // A patch the user had given the address of, likewise, and only that one.
    let address = GIVEN;
    let mut flow = Flow::new(start);
    flow.take_look(resumable(game_at(r"C:\Games\Maple"), Some(address), Some(12.5)), start);
    let picked = pick(&flow.facts(&world(start)), None, None).unwrap();
    assert_eq!((picked.address(), picked.percent), (Some(address), 12.5));
    let other = pick(&flow.facts(&world(start)), Some(GIVEN_OTHER), None).unwrap();
    assert_eq!(other.percent, 0.0);
}

#[test]
fn a_done_run_is_put_away_without_a_trace() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.end(RunEnd::Done { seconds: 95, files: 718 }, start);
    flow.dismiss();
    assert_eq!((flow.phase, flow.run.as_ref(), flow.left()), (Phase::Ready, None, None));
    // The game is still to be started after it.
    assert!(flow.game_due);
    // A run at work is not put away.
    begin(&mut flow, Some(GIVEN_ANOTHER), start).unwrap();
    flow.dismiss();
    assert_eq!(flow.phase, Phase::Running);
}

#[test]
fn a_half_patched_folder_resumes_only_the_patch_on_record() {
    let start = Instant::now();
    let on_record = GIVEN_BEFORE;
    let mut flow = Flow::new(start);
    flow.take_look(half_patched(game_at(r"C:\Games\Maple"), Some(on_record), Some(41.0)), start);
    // The user tried another address in the meantime, and it is the one still in mind.
    flow.source = Some(GIVEN_OTHER.to_owned());
    let facts = flow.facts(&world(start));
    assert_eq!(facts.resume_address(), Some(on_record));
    assert_eq!(pick(&facts, None, None).unwrap().address(), Some(on_record));

    // In a whole folder with work left in it, the address that was last given is gone on with.
    let mut flow = Flow::new(start);
    flow.take_look(resumable(game_at(r"C:\Games\Maple"), Some(on_record), Some(41.0)), start);
    flow.source = Some(GIVEN_OTHER.to_owned());
    assert_eq!(flow.facts(&world(start)).resume_address(), Some(GIVEN_OTHER));
}

#[test]
fn a_wrong_patch_that_is_refused_returns_to_the_ready_screen_of_the_half_patched_folder() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(half_patched(game_at(r"C:\Games\Maple"), None, Some(41.0)), start);
    take(&mut flow, 0, ended(patch_to("1.2.421"), CheckEnd::Answered), start);
    let wrong = GIVEN;
    begin(&mut flow, Some(wrong), start).unwrap();
    // It got as far as the check of the patch file before the engine turned it down.
    flow.hear(says(Step::VerifyPatch, 4.0, "검사 중: 00420to00422.patch"), start);
    flow.end(refused("진행 중인 패치와 다른 패치 파일입니다."), start);

    assert_eq!(flow.phase, Phase::Ready);
    assert_eq!(flow.source, None);
    let failure = flow.run.as_ref().and_then(|run| run.failure.as_ref()).unwrap();
    assert_eq!((failure.title.as_str(), failure.message.as_deref()), ("진행 중인 패치와 다른 패치 파일입니다.", None));
    // The patch to finish is still the one that is offered, and may still start by itself.
    let facts = flow.facts(&World { auto_patch: true, ..world(start) });
    assert_eq!(facts.resume_address(), None);
    let offered = pick(&facts, None, None).unwrap();
    assert_eq!(offered.target().map(|target| target.label.as_str()), Some("1.2.421"));
    assert!(flow.hands_off.contains(wrong) && !flow.hands_off.contains("1.2.421"));

    // The right patch, paused there, stays on the screen like any other.
    let mut flow = Flow::new(start);
    flow.take_look(half_patched(game_at(r"C:\Games\Maple"), None, Some(41.0)), start);
    take(&mut flow, 0, ended(patch_to("1.2.421"), CheckEnd::Answered), start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::Rebuild, 60.0, "생성 중: Map001.wz"), start);
    flow.end(failed("파일 처리 중 오류가 발생했습니다."), start);
    assert_eq!(flow.phase, Phase::Failed);
}

#[test]
fn a_folder_that_is_changed_takes_the_run_on_its_screen_with_it() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, Some(GIVEN_ANOTHER), start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
    flow.end(RunEnd::Stopped, start);
    assert!(flow.take_look(game_at(r"D:\Copy\Maple"), start));
    assert_eq!((flow.phase, flow.run.as_ref(), flow.source.as_deref(), flow.left()), (Phase::Ready, None, None, None));
}

// What a start asks for.

#[test]
fn a_start_is_turned_down_with_what_is_in_the_way() {
    let start = Instant::now();
    let flow = with_a_patch(start);
    // Too old a version of the app is no reason: what the user starts is started.
    assert!(pick(&flow.facts(&World { outdated: true, ..world(start) }), None, None).is_ok());

    let mut none = Flow::new(start);
    assert_eq!(pick(&none.facts(&world(start)), None, None).unwrap_err().kind, RefusalKind::Nothing);
    none.take_look(no_install(), start);
    let no_folder = pick(&none.facts(&world(start)), None, None).unwrap_err();
    assert_eq!(
        (no_folder.kind, no_folder.messages),
        (RefusalKind::Nothing, vec!["설치 경로를 찾을 수 없습니다.".to_owned()])
    );

    // Every check that has not passed says so, the quiet ones included.
    let mut playing = with_a_patch(start);
    playing.take_look(game_open(game_at(r"C:\Games\Maple")), start);
    let blocked = pick(&playing.facts(&world(start)), None, None).unwrap_err();
    assert_eq!(blocked.kind, RefusalKind::Blocked);
    assert_eq!(blocked.messages, ["메이플스토리를 종료한 후 실행해 주세요."]);

    // A folder the server has nothing for, and nothing left to go on with.
    let mut up = Flow::new(start);
    up.take_look(game_at(r"C:\Games\Maple"), start);
    take(&mut up, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
    let nothing = pick(&up.facts(&world(start)), None, None).unwrap_err();
    assert_eq!((nothing.kind, nothing.messages), (RefusalKind::Nothing, vec!["적용할 패치가 없습니다.".to_owned()]));
    // An address the user gives needs no patch a check has found.
    assert!(pick(&up.facts(&world(start)), Some(GIVEN_ANOTHER), None).is_ok());
}

#[test]
fn a_run_that_stopped_is_gone_on_with_by_its_own_patch() {
    let start = Instant::now();
    let address = GIVEN;
    let mut flow = with_a_patch(start);
    begin(&mut flow, Some(address), start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
    flow.end(failed("파일 처리 중 오류가 발생했습니다."), start);
    // The button under a run that stopped names no address: the run's own is meant, not
    // the patch a check has found.
    let again = pick(&flow.facts(&world(start)), None, flow.left()).unwrap();
    assert_eq!((again.address(), again.target(), again.percent), (Some(address), None, 61.5));
}

// What the app does by itself.

/// A site with a patch waiting, and the user wanting patches applied without being asked.
fn wanting(now: Instant) -> (Flow, World) {
    (with_a_patch(now), World { auto_patch: true, ..world(now) })
}

#[test]
fn a_patch_starts_by_itself_when_the_user_wants_that_and_nothing_is_in_the_way() {
    let start = Instant::now();
    let (flow, world) = wanting(start);
    assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch);
    // Not without the setting.
    assert_eq!(decide(&flow.facts(&World { auto_patch: false, ..world })), Decision::Nothing);
}

#[test]
fn no_patch_starts_by_itself_while_a_run_is_on_the_screen_or_at_work_anywhere() {
    let start = Instant::now();
    let (mut flow, world) = wanting(start);
    // A run for another client.
    assert_eq!(decide(&flow.facts(&World { run_at_work: true, ..world })), Decision::Nothing);
    // This site's own, at every moment of it.
    begin(&mut flow, None, start).unwrap();
    for phase in [Phase::Starting, Phase::Running, Phase::Stopping, Phase::Paused, Phase::Failed] {
        flow.phase = phase;
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing, "{phase:?}");
    }
}

#[test]
fn no_patch_starts_by_itself_without_a_patch_or_with_something_in_the_way() {
    let start = Instant::now();
    let world = World { auto_patch: true, ..world(start) };
    // The server has not answered, or has nothing.
    let mut flow = Flow::new(start);
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);

    // The game is open: a check that only shows when it fails, and counts all the same.
    let mut flow = with_a_patch(start);
    flow.take_look(game_open(game_at(r"C:\Games\Maple")), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    // It is closed again: the patch starts.
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch);
    // A version of the app that is too old patches nothing by itself either.
    assert_eq!(decide(&flow.facts(&World { outdated: true, ..world })), Decision::Nothing);
}

#[test]
fn a_patch_that_was_paused_or_failed_is_the_users_to_start_for_the_rest_of_the_session() {
    let start = Instant::now();
    for end in [RunEnd::Stopped, failed("파일 처리 중 오류가 발생했습니다."), refused("디스크 공간이 부족합니다.")]
    {
        let (mut flow, world) = wanting(start);
        begin(&mut flow, None, start).unwrap();
        flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
        flow.end(end.clone(), start);
        flow.dismiss();
        assert_eq!(flow.phase, Phase::Ready);
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing, "{end:?}");
        // The user starts it, and it goes through: the next patch has its own turn.
        begin(&mut flow, None, start).unwrap();
        flow.end(RunEnd::Done { seconds: 60, files: 10 }, start);
        take(&mut flow, 0, ended(patch_to("1.2.423"), CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch, "{end:?}");
    }
    // A new session knows nothing of it.
    let (fresh, world) = wanting(start);
    assert_eq!(decide(&fresh.facts(&world)), Decision::StartPatch);
}

#[test]
fn a_patch_stays_hands_off_when_a_run_of_another_file_ends() {
    let start = Instant::now();
    let (mut flow, world) = wanting(start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
    flow.end(RunEnd::Stopped, start);
    flow.dismiss();
    begin(&mut flow, Some(GIVEN_ANOTHER), start).unwrap();
    flow.hear(says(Step::VerifyPatch, 2.0, "검사 중: 00420to00423.patch"), start);
    flow.end(failed("패치 파일 검사에 실패했습니다."), start);
    flow.dismiss();
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    assert!(flow.hands_off.contains("1.2.422") && flow.hands_off.contains(GIVEN_ANOTHER));
}

/// After a restart, what was left unfinished is only known from the records. A run of
/// the server's own patch is taken up by itself once the server offers that patch
/// again. A run by an address the user gave is not, whatever the server has: the app
/// takes up by itself what it found itself, and nothing that only a record says was
/// once asked for.
#[test]
fn of_the_runs_left_unfinished_when_the_app_was_closed_only_the_servers_patch_is_taken_up_by_itself() {
    let start = Instant::now();
    let world = World { auto_patch: true, ..world(start) };
    let mut flow = Flow::new(start);
    flow.take_look(resumable(game_at(r"C:\Games\Maple"), None, Some(12.5)), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    take(&mut flow, 0, ended(patch_to("1.2.422"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch);

    let address = GIVEN;
    for answer in [None, Some(patch_to("1.2.422"))] {
        let mut flow = Flow::new(start);
        flow.take_look(resumable(game_at(r"C:\Games\Maple"), Some(address), Some(12.5)), start);
        if let Some(answer) = answer {
            take(&mut flow, 0, ended(answer, CheckEnd::Answered), start);
        }
        assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
        // It is there to be continued at a press, and that press continues that patch.
        let facts = flow.facts(&world);
        assert!(facts.own_run() && facts.can_start());
        let continued = pick(&facts, None, None).unwrap();
        assert_eq!((continued.address(), continued.percent), (Some(address), 12.5));
    }
    // So it is for a folder such a run left partly new.
    let mut flow = Flow::new(start);
    flow.take_look(half_patched(game_at(r"C:\Games\Maple"), Some(address), Some(41.0)), start);
    take(&mut flow, 0, ended(errored("이전 패치 미완료"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
}

/// Whether this version may still patch is asked of the app's home as the app starts.
/// Until that was tried once, the app does nothing by itself; once it was, it goes on
/// whether or not there was an answer.
#[test]
fn nothing_is_done_by_itself_before_the_apps_home_was_asked_once() {
    let start = Instant::now();
    let (flow, asked) = wanting(start);
    let not_asked = World { update_asked: false, ..asked };
    assert_eq!(decide(&flow.facts(&not_asked)), Decision::Nothing);
    assert_eq!(decide(&flow.facts(&asked)), Decision::StartPatch);
    // The user's own press waits for nothing.
    assert!(pick(&flow.facts(&not_asked), None, None).is_ok());

    let (mut flow, asked) = patched(start);
    take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&World { update_asked: false, ..asked })), Decision::Nothing);
    // Nothing was given up meanwhile: the game is still to be started.
    assert_eq!(decide(&flow.facts(&asked)), Decision::StartGame);
}

#[test]
fn the_next_patch_starts_by_itself_from_the_screen_of_the_one_that_is_done() {
    let start = Instant::now();
    let (mut flow, world) = wanting(start);
    begin(&mut flow, None, start).unwrap();
    flow.end(RunEnd::Done { seconds: 60, files: 10 }, start);
    assert_eq!(flow.phase, Phase::Done);
    // Not before the server has said what there is for the folder as it is now.
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    take(&mut flow, 0, ended(patch_to("1.2.423"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::StartPatch);
}

/// What the page shows is none of the app's business: a failure that is still being
/// read does not keep the next patch waiting.
#[test]
fn a_patch_does_not_wait_for_what_the_screen_is_saying_of_another() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(half_patched(game_at(r"C:\Games\Maple"), None, Some(41.0)), start);
    take(&mut flow, 0, ended(patch_to("1.2.421"), CheckEnd::Answered), start);
    begin(&mut flow, Some(GIVEN_WRONG), start).unwrap();
    flow.hear(says(Step::VerifyPatch, 4.0, "검사 중: 00420to00425.patch"), start);
    flow.end(refused("진행 중인 패치와 다른 패치 파일입니다."), start);
    assert!(flow.run.as_ref().is_some_and(|run| run.failure.is_some()));
    assert_eq!(decide(&flow.facts(&World { auto_patch: true, ..world(start) })), Decision::StartPatch);
}

/// A site whose patch has just gone through, with the user wanting the game started.
fn patched(now: Instant) -> (Flow, World) {
    let mut flow = with_a_patch(now);
    begin(&mut flow, None, now).unwrap();
    flow.end(RunEnd::Done { seconds: 60, files: 10 }, now);
    (flow, World { auto_start: true, ..world(now) })
}

#[test]
fn the_game_is_started_after_a_patch_once_the_server_says_there_is_no_other() {
    let start = Instant::now();
    let (mut flow, world) = patched(start);
    // What the server had before the patch says nothing; its answer is waited for.
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
    take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&world)), Decision::StartGame);
    // Once.
    flow.launched(start);
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
}

#[test]
fn the_game_is_not_started_without_a_patch_or_without_the_setting() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(game_at(r"C:\Games\Maple"), start);
    take(&mut flow, 0, ended(up_to_date("1.2.421"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&World { auto_start: true, ..world(start) })), Decision::Nothing);

    let (mut flow, world) = patched(start);
    take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
    assert_eq!(decide(&flow.facts(&World { auto_start: false, ..world })), Decision::Nothing);
    // It still waits to be, should the user turn the setting on.
    assert!(flow.game_due);
}

/// The game is started by itself only once it is known to be open, and at that moment.
/// Nobody is at the PC to see it started into a server that is closed, so what is not
/// known does not count as open: it is waited for, with the button there all the while.
#[test]
fn the_game_is_started_by_itself_only_once_it_is_known_to_be_open() {
    let start = Instant::now();
    let (mut flow, world) = patched(start);
    take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
    // What is known of the game, whether it can be seen at all that it is open, and what
    // the app does by itself.
    let table = [
        ("seen to be open", Some(true), true, Decision::StartGame),
        // A maintenance is underway, or the game is between two operating periods.
        ("closed", Some(false), true, Decision::Nothing),
        // Nothing was read of Nexon yet, or Nexon cannot be read.
        ("not known", None, true, Decision::Nothing),
        // A client Nexon's service does not list is never seen to be open: there is
        // nothing to wait for, and its start is left to the user, whatever closes it.
        ("never seen to be open, with nothing in the way", None, false, Decision::LeaveGameToUser),
        ("never seen to be open, and closed", Some(false), false, Decision::LeaveGameToUser),
    ];
    for (what, open, sees_open, does) in table {
        assert_eq!(decide(&flow.facts(&World { open, sees_open, ..world })), does, "{what}");
        // Without the setting nothing of this is the app's to do.
        let unset = World { open, sees_open, auto_start: false, ..world };
        assert_eq!(decide(&flow.facts(&unset)), Decision::Nothing, "{what}, not set");
    }
    // What is waited for is still waited for: the game starts the moment it is seen to be
    // open, after a maintenance and after Nexon could not be read alike.
    assert!(flow.game_due);
    for before in [Some(false), None] {
        assert_eq!(decide(&flow.facts(&World { open: before, ..world })), Decision::Nothing);
        assert_eq!(decide(&flow.facts(&World { open: Some(true), ..world })), Decision::StartGame);
    }
    // Left to the user, it waits for nothing any more: the desk takes that in.
    flow.game_due = false;
    assert_eq!(decide(&flow.facts(&World { open: None, sees_open: false, ..world })), Decision::Nothing);

    // Another patch that waits is found out about when the game is open, and not before:
    // until then the game was not to be started anyway.
    let (mut flow, world) = patched(start);
    take(&mut flow, 0, ended(patch_to("1.2.423"), CheckEnd::Answered), start);
    for closed in [Some(false), None] {
        assert_eq!(decide(&flow.facts(&World { open: closed, ..world })), Decision::Nothing);
    }
    assert_eq!(decide(&flow.facts(&World { open: Some(true), ..world })), Decision::DropGameStart);
    // The server has not answered since the patch: open or not, that is waited for.
    let (flow, world) = patched(start);
    assert_eq!(decide(&flow.facts(&World { open: Some(true), ..world })), Decision::Nothing);
}

#[test]
fn the_game_is_not_started_when_another_patch_waits_or_that_cannot_be_ruled_out() {
    let start = Instant::now();
    for answer in [patch_to("1.2.423"), errored("서버 요청 실패")] {
        let (mut flow, world) = patched(start);
        let end = if answer.error.is_some() { CheckEnd::Failed } else { CheckEnd::Answered };
        take(&mut flow, 0, ended(answer, end), start);
        assert_eq!(decide(&flow.facts(&world)), Decision::DropGameStart);
        // With that given up, the patch is what is decided on: it starts if the user
        // wants patches applied, and the game is due again when it is done.
        flow.game_due = false;
        let then = decide(&flow.facts(&World { auto_patch: true, ..world }));
        let patch_waits = flow.answer.as_ref().is_some_and(|answer| answer.target.is_some());
        assert_eq!(then == Decision::StartPatch, patch_waits);
    }
}

#[test]
fn the_game_is_not_started_while_this_site_has_a_run_at_work() {
    let start = Instant::now();
    let (mut flow, world) = patched(start);
    take(&mut flow, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
    begin(&mut flow, Some(GIVEN_ANOTHER), start).unwrap();
    // A run that begins takes the game's turn: it is due again when the patch is done.
    assert!(!flow.game_due);
    // Nor would it be started under a run, were it still due.
    flow.game_due = true;
    assert_eq!(decide(&flow.facts(&world)), Decision::Nothing);
}

/// A copy of the app that takes over from another in the middle of a session is told what
/// that one remembered, and does by itself what that one would have done: it starts the
/// game that was due, and leaves alone the patch that was the user's to start.
#[test]
fn a_site_that_takes_up_what_another_copy_remembered_goes_on_as_that_one_would_have() {
    let start = Instant::now();
    // A patch was finished, and the game waits for the server to open.
    let (flow, world) = patched(start);
    let remembered = flow.remembered();
    assert_eq!(remembered, Remembered { game_due: true, hands_off: Vec::new() });
    // The copy that takes over finds the folder patched, and nothing that says by whom.
    let mut taken_over = Flow::new(start);
    taken_over.take_look(game_at(r"C:\Games\Maple"), start);
    take(&mut taken_over, 0, ended(up_to_date("1.2.422"), CheckEnd::Answered), start);
    assert_eq!(decide(&taken_over.facts(&world)), Decision::Nothing);
    taken_over.remember(remembered);
    assert_eq!(decide(&taken_over.facts(&World { open: Some(false), ..world })), Decision::Nothing);
    assert_eq!(decide(&taken_over.facts(&world)), Decision::StartGame);

    // A patch the user stopped is theirs to start, in the copy that takes over as well.
    let (mut flow, world) = wanting(start);
    begin(&mut flow, None, start).unwrap();
    flow.end(RunEnd::Stopped, start);
    let remembered = flow.remembered();
    assert_eq!(remembered, Remembered { game_due: false, hands_off: vec!["1.2.422".to_owned()] });
    let (mut taken_over, _) = wanting(start);
    assert_eq!(decide(&taken_over.facts(&world)), Decision::StartPatch);
    taken_over.remember(remembered);
    assert_eq!(decide(&taken_over.facts(&world)), Decision::Nothing);

    // A site that remembers nothing has nothing to tell, and taking that up changes nothing.
    let (mut fresh, world) = wanting(start);
    assert_eq!(fresh.remembered(), Remembered::default());
    fresh.remember(Remembered::default());
    assert_eq!(decide(&fresh.facts(&world)), Decision::StartPatch);
}

#[test]
fn a_download_that_brings_nothing_is_quiet_after_four_seconds_and_the_button_rests_five() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::Download, 3.0, "다운로드 중: 00421to00422.patch"), start);
    assert!(!flow.facts(&world(start + seconds(4))).quiet);
    assert!(flow.facts(&world(start + seconds(5))).quiet);
    flow.hear(says(Step::Download, 3.5, "다운로드 중: 00421to00422.patch"), start + seconds(6));
    assert!(!flow.facts(&world(start + seconds(7))).quiet);

    flow.launched(start);
    assert!(flow.facts(&world(start + seconds(4))).launching);
    assert!(!flow.facts(&world(start + seconds(5))).launching);
}

/// A client that is not installed costs nothing: its patch server is not asked, and
/// the watch on its maintenance, which goes by the same word, reads nothing.
#[test]
fn nothing_is_asked_of_any_server_for_a_client_that_is_not_installed() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    // Before the first look nothing is known, and nothing is asked.
    assert!(!flow.askable() && !flow.check_is_due(start, false));
    for not_there in [no_install(), no_game_at(r"D:\Empty")] {
        flow.take_look(not_there, start);
        assert!(!flow.askable());
        for later in [0, 60, 3600, 86_400] {
            assert!(!flow.check_is_due(start + seconds(later), false), "after {later} s");
        }
    }
    // The folder is looked at all the same, so that a game that gets installed is found.
    assert!(flow.look_is_due(start + seconds(3), false, false));

    // Once the game is there, both are asked.
    flow.take_look(game_at(r"C:\Games\MapleT"), start + seconds(3));
    assert!(flow.askable() && flow.check_is_due(start + seconds(3), false));
}

// The test world.

/// A moment in Korean time in the week of Thursday 2026-11-26, which is 20783 days after
/// the first of 1970: `days` after that Thursday, at `hour`:`minute`.
fn of_the_week(days: i64, hour: i64, minute: i64) -> Minutes {
    (20_783 + days) * 1440 + hour * 60 + minute
}

#[test]
fn the_test_worlds_patches_are_looked_for_often_on_thursday_afternoons() {
    let hours = &TEST_WORLD_PATCH_HOURS;
    // From 15:00 on, and no longer at 17:30.
    assert!(!hours.hold(of_the_week(0, 14, 59)));
    assert!(hours.hold(of_the_week(0, 15, 0)));
    assert!(hours.hold(of_the_week(0, 17, 29)));
    assert!(!hours.hold(of_the_week(0, 17, 30)));
    // On Thursdays, and on no other day of the week.
    for days in [-3, -2, -1, 1, 2, 3] {
        assert!(!hours.hold(of_the_week(days, 16, 0)), "{days} days from Thursday");
    }
    assert!(hours.hold(of_the_week(7, 16, 0)) && hours.hold(of_the_week(-7, 16, 0)));

    // In those hours the server is asked as during a maintenance; outside them as at
    // any other time.
    let none = |_: u64| 0;
    assert_eq!(next_check_in(calm(Some(false), true), 0, &none), seconds(30));
    assert_eq!(next_check_in(calm(Some(false), false), 0, &none), seconds(600));
}

#[test]
fn there_is_something_to_hurry_for_during_a_maintenance_and_in_a_clients_patch_hours() {
    assert_eq!(calm(Some(false), false), Some(true));
    assert_eq!(calm(Some(true), false), Some(false));
    // Whatever is known of a maintenance, the patch hours are the patch hours.
    for underway in [Some(false), Some(true), None] {
        assert_eq!(calm(underway, true), Some(false), "{underway:?}");
    }
    // Outside them, what could not be read is not known.
    assert_eq!(calm(None, false), None);

    // The hours beginning and ending make a check due at once, as a maintenance does.
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    assert!(!flow.see_pace(calm(Some(false), false), start));
    assert!(flow.see_pace(calm(Some(false), true), start + seconds(1)));
    assert_eq!(flow.next_check, start + seconds(1));
    assert!(flow.see_pace(calm(Some(false), false), start + seconds(2)));
}

/// A client this app was never tried on is patched at the user's own press the first
/// time, whatever is set.
#[test]
fn a_folder_that_is_the_users_to_patch_first_is_not_patched_by_itself() {
    let start = Instant::now();
    let mut flow = Flow::new(start);
    flow.take_look(Status { by_press_only: true, ..game_at(r"C:\Games\MapleT") }, start);
    take(&mut flow, 0, ended(patch_to("1.2.208"), CheckEnd::Answered), start);
    let wanting = World { auto_patch: true, ..world(start) };
    assert_eq!(decide(&flow.facts(&wanting)), Decision::Nothing);
    // The press is there to be made.
    assert!(pick(&flow.facts(&wanting), None, None).is_ok());

    // Once this app has patched the folder, the look says so, and the next patch is
    // the app's to start.
    flow.take_look(game_at(r"C:\Games\MapleT"), start);
    assert_eq!(decide(&flow.facts(&wanting)), Decision::StartPatch);
}

/// An address the user gives again for a run of it that is there to be gone on with is
/// the address of that run, and not a new one to ask about.
#[test]
fn a_run_by_an_address_that_is_there_to_be_gone_on_with_is_known_by_that_address() {
    let start = Instant::now();
    let (address, other) = (GIVEN, GIVEN_OTHER);
    // The run that stopped on the screen.
    let mut flow = with_a_patch(start);
    assert!(!flow.facts(&world(start)).continues(address));
    begin(&mut flow, Some(address), start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
    flow.end(RunEnd::Stopped, start);
    let facts = flow.facts(&world(start));
    assert!(facts.continues(address) && !facts.continues(other));

    // The run that a session before left on record.
    let mut flow = Flow::new(start);
    flow.take_look(resumable(game_at(r"C:\Games\Maple"), Some(address), Some(12.5)), start);
    let facts = flow.facts(&world(start));
    assert!(facts.continues(address) && !facts.continues(other));

    // A run of the patch a check found goes by no address.
    let mut flow = with_a_patch(start);
    begin(&mut flow, None, start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
    flow.end(RunEnd::Stopped, start);
    assert!(!flow.facts(&world(start)).continues(address));
}

/// The address of the patch a check found is that patch, whoever names it. A run of it
/// is the app's own: it is what the screen offers, what is offered again after a pause,
/// and what the app may take up by itself. Any other address is the user's own word.
#[test]
fn the_address_of_the_patch_the_app_found_is_that_patch_and_any_other_is_the_users_own() {
    let start = Instant::now();
    let mut flow = with_a_patch(start);
    let found = flow.answer.as_ref().and_then(|answer| answer.target.clone()).expect("the patch a check found");

    let own = pick(&flow.facts(&world(start)), Some(&found.url), None).unwrap();
    assert_eq!((own.address(), own.target()), (None, Some(&found)));
    let given = pick(&flow.facts(&world(start)), Some(GIVEN), None).unwrap();
    assert_eq!((given.address(), given.target()), (Some(GIVEN), None));

    // A run by the user's own address that stopped is on the screen. Giving the address
    // of the patch the app found begins that patch, and not the run's again.
    begin(&mut flow, Some(GIVEN), start).unwrap();
    flow.hear(says(Step::Rebuild, 61.5, "생성 중: Map001.wz"), start);
    flow.end(RunEnd::Stopped, start);
    let facts = flow.facts(&world(start));
    assert_eq!(pick(&facts, None, flow.left()).unwrap().address(), Some(GIVEN));
    let own = pick(&facts, Some(&found.url), flow.left()).unwrap();
    assert_eq!((own.address(), own.target(), own.percent), (None, Some(&found), 0.0));

    // A start by address is turned down for what any start is turned down for, and too
    // old a version of the app is not among that.
    let outdated = pick(&flow.facts(&World { outdated: true, ..world(start) }), Some(GIVEN), None).unwrap();
    assert_eq!(outdated.address(), Some(GIVEN));
    let mut playing = with_a_patch(start);
    playing.take_look(game_open(game_at(r"C:\Games\Maple")), start);
    assert_eq!(pick(&playing.facts(&world(start)), Some(GIVEN), None).unwrap_err().kind, RefusalKind::Blocked);
    let mut nowhere = Flow::new(start);
    nowhere.take_look(no_install(), start);
    assert_eq!(pick(&nowhere.facts(&world(start)), Some(GIVEN), None).unwrap_err().kind, RefusalKind::Nothing);
}

/// An apply was cut off in the folder and its record cannot be read. Nothing is started
/// over it: not by the app, not at a press, not by an address, and not when a patch
/// should be found for the folder all the same. There is no run to go on with either.
#[test]
fn nothing_is_started_over_an_apply_whose_record_cannot_be_read() {
    let start = Instant::now();
    let said = "이전 패치 작업 기록이 손상되었습니다.\n공식 런처로 게임을 복구해 주세요.";
    let wanting = World { auto_patch: true, auto_start: true, ..world(start) };
    let mut flow = Flow::new(start);
    flow.take_look(journal_unreadable(game_at(r"C:\Games\Maple")), start);
    for answer in [errored("작업 기록 손상"), patch_to("1.2.422")] {
        take(&mut flow, 0, ended(answer, CheckEnd::Answered), start);
        assert_eq!(decide(&flow.facts(&wanting)), Decision::Nothing);
        for address in [None, Some(GIVEN)] {
            let refusal = pick(&flow.facts(&world(start)), address, None).unwrap_err();
            assert_eq!((refusal.kind, refusal.messages), (RefusalKind::Blocked, vec![said.to_owned()]));
        }
        let facts = flow.facts(&world(start));
        assert!(!facts.own_run() && facts.resume_address().is_none());
    }
}
