//! Tests of the watch on the maintenance: of what is said of it, of the pace Nexon is read at,
//! and of the block on the site.

use super::*;

/// A look at `now` that found `underway`.
fn look(now: i64, underway: Option<Hours>) -> Look {
    Look { underway, ended: false, now }
}

/// The same, where the one seen underway before is over.
fn over(now: i64, underway: Option<Hours>) -> Look {
    Look { ended: true, ..look(now, underway) }
}

/// What each of `looks` gave to say, from a watch that knows nothing yet. `None` is a
/// look at which nothing was known.
fn words(looks: &[Option<Look>]) -> Vec<Word> {
    let mut standing = Standing::default();
    let after = |look: &Option<Look>| match look {
        None => {
            standing = Standing::Unknown;
            Word::default()
        }
        Some(look) => {
            let (stands, word) = standing.after(*look);
            standing = stands;
            word
        }
    };
    looks.iter().map(after).collect()
}

const QUIET: Word = Word { over: false, extended: None, begun: false };
const BEGAN: Word = Word { begun: true, ..QUIET };
const WAS_OVER: Word = Word { over: true, ..QUIET };
/// A maintenance from minute 240 to 600, and the same one extended to 720.
const FIRST: Hours = (240, 600);
const LONGER: Hours = (240, 720);
/// The one after it.
const NEXT: Hours = (900, 1000);

#[test]
fn a_maintenance_that_begins_while_the_app_is_open_is_said_to_have_begun_once() {
    let (open, on) = (|now| Some(look(now, None)), |now, hours| Some(look(now, Some(hours))));
    // Seen to begin while the app is open: listed for later until the clock says so.
    assert_eq!(words(&[open(100), open(200), on(240, FIRST), on(241, FIRST)]), [QUIET, QUIET, BEGAN, QUIET]);
    // The app is started in the middle of one: nothing is said of its start.
    assert_eq!(words(&[on(300, FIRST), on(301, FIRST)]), [QUIET, QUIET]);
    // Extended while underway: it is the same one, and the watch in kms says so.
    assert_eq!(words(&[open(100), on(300, FIRST), on(590, LONGER), on(700, LONGER)]), [QUIET, BEGAN, QUIET, QUIET]);
    // Nothing could be read for a while, and one is underway when it can be again.
    assert_eq!(words(&[open(100), None, on(300, FIRST)]), [QUIET, QUIET, QUIET]);
    assert_eq!(words(&[open(100), on(300, FIRST), None, on(500, FIRST)]), [QUIET, BEGAN, QUIET, QUIET]);
    // It left both accounts without being over, and is back within its hours.
    let lost = [open(100), on(300, FIRST), open(400), open(450), on(500, FIRST)];
    assert_eq!(words(&lost), [QUIET, BEGAN, QUIET, QUIET, QUIET]);
    // Lost for good: past its time, the next one is a new one.
    let gone = [open(100), on(300, FIRST), open(400), open(601), on(900, NEXT)];
    assert_eq!(words(&gone), [QUIET, BEGAN, QUIET, QUIET, BEGAN]);
    // Two in a row: each is said once, and so is the end of the first.
    let two = [open(100), on(300, FIRST), Some(over(600, None)), open(700), on(900, NEXT)];
    assert_eq!(words(&two), [QUIET, BEGAN, WAS_OVER, QUIET, BEGAN]);
    // The next is underway at the very look at which the one before is over.
    let at_once = [open(100), on(300, FIRST), Some(over(600, Some((600, 700))))];
    assert_eq!(words(&at_once), [QUIET, BEGAN, Word { over: true, begun: true, ..QUIET }]);
}

#[test]
fn a_maintenance_that_is_over_and_goes_on_after_all_was_extended_and_has_not_begun_again() {
    let (open, on) = (|now| Some(look(now, None)), |now, hours| Some(look(now, Some(hours))));
    // Said to be over, and extended after that: the same maintenance going on.
    let late = [on(300, FIRST), Some(over(600, None)), open(601), on(610, LONGER), on(611, LONGER)];
    assert_eq!(words(&late), [QUIET, WAS_OVER, QUIET, Word { extended: Some(720), ..QUIET }, QUIET]);
    // Another one after it has begun, and is said to have.
    assert_eq!(words(&[on(300, FIRST), Some(over(600, None)), on(900, NEXT)]), [QUIET, WAS_OVER, BEGAN]);
    // Over before its time, as the board says: it is over all the same.
    assert_eq!(words(&[on(300, FIRST), Some(over(500, None))]), [QUIET, WAS_OVER]);
    // Its end came while nothing was known of it here: it is said all the same.
    assert_eq!(words(&[on(300, FIRST), None, Some(over(600, None))]), [QUIET, QUIET, WAS_OVER]);
}

/// The watch as it runs, on a made-up clock: what the service lists (read at `listed`,
/// in seconds) held against the clock at `seconds`. Says what the lamp shows and what
/// is said.
fn held(
    following: &mut Following,
    planned: &[notice::Planned],
    notices: &[notice::Notice],
    (seconds, listed): (i64, i64),
) -> (bool, Vec<String>) {
    let now = seconds.div_euclid(60);
    let reading = following.watch.read_as_of(planned, notices, seconds, listed);
    let mut told: Vec<String> = reading.extended.iter().map(|ends| extended_until(*ends, now)).collect();
    let underway = reading.underway.as_ref().map(|closed| (closed.begins, closed.ends));
    let word = following.stand(Look { underway, ended: reading.ended, now }, seconds);
    told.extend(said(word, true, now));
    (live(&reading, now).open == Some(true), told)
}

/// The end of a maintenance that only the service lists: it is said, and the lamp
/// turns, with the reading that is made as the planned end arrives.
#[test]
fn the_end_is_said_with_the_reading_made_as_the_planned_end_arrives() {
    let (start, end) = (at(4, 0) * 60, at(10, 0) * 60);
    let longer = vec![notice::Planned { ends: at(12, 0), ..listed()[0].clone() }];
    let nothing: Vec<String> = Vec::new();

    // On time. Read twenty seconds before the end: closed up to the end and at it.
    let mut following = Following::default();
    assert_eq!(held(&mut following, &listed(), &[], (start - 1, start - 30)), (true, nothing.clone()));
    assert_eq!(held(&mut following, &listed(), &[], (start, start - 30)), (false, vec![BEGUN.to_owned()]));
    assert_eq!(held(&mut following, &listed(), &[], (end - 1, end - 20)), (false, nothing.clone()));
    assert_eq!(held(&mut following, &listed(), &[], (end, end - 20)), (false, nothing.clone()));
    // The reading made as the end arrived lists nothing longer: over within that second.
    assert_eq!(following.over_since(), None);
    assert_eq!(held(&mut following, &listed(), &[], (end, end)), (true, vec![OVER.to_owned()]));
    assert_eq!(held(&mut following, &listed(), &[], (end + 1, end)), (true, nothing.clone()));
    // What follows an end counts from the second it was said, and that is what is kept.
    assert_eq!(following.over_since(), Some(end));

    // That reading lists it as going on until noon: extended, and never said to be over.
    let mut following = Following::default();
    held(&mut following, &listed(), &[], (end - 1, end - 20));
    assert_eq!(held(&mut following, &listed(), &[], (end, end - 20)), (false, nothing.clone()));
    let extended = vec!["점검이 12:00까지 연장되었습니다.".to_owned()];
    assert_eq!(held(&mut following, &longer, &[], (end, end)), (false, extended.clone()));
    assert_eq!(held(&mut following, &longer, &[], (end + 300, end + 295)), (false, nothing.clone()));

    // The extension is posted five minutes after the end was said: extended, not begun.
    let mut following = Following::default();
    held(&mut following, &listed(), &[], (end - 1, end - 20));
    assert_eq!(held(&mut following, &listed(), &[], (end, end)), (true, vec![OVER.to_owned()]));
    assert_eq!(held(&mut following, &longer, &[], (end + 300, end + 300)), (false, extended));
    // Underway again, it has no end to count from.
    assert_eq!(following.over_since(), None);

    // Nexon cannot be read as the end arrives: over by the clock after a minute.
    let mut following = Following::default();
    held(&mut following, &listed(), &[], (end - 1, end - 20));
    assert_eq!(held(&mut following, &listed(), &[], (end + 59, end - 20)), (false, nothing.clone()));
    assert_eq!(held(&mut following, &listed(), &[], (end + 60, end - 20)), (true, vec![OVER.to_owned()]));
    // Said a minute after the end that was planned: that minute is what counts.
    assert_eq!(following.over_since(), Some(end + 60));

    // Over early, as the board says at the next reading: said with that reading.
    // 2026-11-26, the day in the notice, is 20783 days after the first of 1970.
    let on = |hour: i64| 20_783 * 1440 + hour * 60;
    let planned = vec![notice::Planned { begins: on(4), ends: on(10), ..listed()[0].clone() }];
    let patch = |title: &str| notice::Notice { id: 9, title: title.to_owned() };
    let open = patch("11/26(목) ver1.2.422 클라이언트 패치(4:00~10:00)");
    let done = patch("[패치완료] 11/26(목) ver1.2.422 클라이언트 패치(4:00~10:00)");
    let early = on(9) * 60;
    let mut following = Following::default();
    held(&mut following, &planned, std::slice::from_ref(&open), (early - 5, early - 5));
    assert_eq!(held(&mut following, &planned, &[open], (early - 1, early - 5)), (false, nothing.clone()));
    assert_eq!(held(&mut following, &planned, &[done], (early, early)), (true, vec![OVER.to_owned()]));
    // Said an hour before the end that was planned, and counted from there.
    assert_eq!(following.over_since(), Some(early));
}

/// A PC whose clock is two minutes off decides at the right second all the same.
#[test]
fn a_pc_clock_that_is_minutes_off_decides_by_nexons_clock() {
    let end = at(10, 0) * 60;
    for fast in [120_000_i64, -120_000] {
        let mut clock = Clock::default();
        // Two answers, stated half a second into Nexon's second 100 before the end.
        for _ in 0..2 {
            let sent = (end - 100) * 1000 + 480 + fast;
            clock.learn(sent, sent + 40, end - 100);
        }
        let by_nexon = |nexon_ms: i64| clock.corrected(nexon_ms + fast).div_euclid(1000);
        let mut following = Following::default();
        held(&mut following, &listed(), &[], (by_nexon((end - 1) * 1000), end - 20));
        // A second before the end by Nexon's clock it is closed, whatever this PC says.
        let (open, _) = held(&mut following, &listed(), &[], (by_nexon(end * 1000 - 600), end - 20));
        assert!(!open, "{fast}");
        // The reading at the end, by Nexon's clock, ends it.
        let read = by_nexon(end * 1000 + 300);
        assert_eq!(held(&mut following, &listed(), &[], (read, read)), (true, vec![OVER.to_owned()]), "{fast}");
    }
}

/// Minutes since 1970 of a time of day in Korea, on a day of the tests' own.
fn at(hour: i64, minute: i64) -> notice::Minutes {
    20_000 * 1440 + hour * 60 + minute
}

/// What the inspection service lists ahead of a patch day, read once at ten to four.
fn listed() -> Vec<notice::Planned> {
    let text = "[패치중] 11/26(목) ver1.2.422 클라이언트 패치(04:00~10:00)".to_owned();
    vec![notice::Planned { begins: at(4, 0), ends: at(10, 0), text }]
}

/// What the live game's site knows by a reading: it is always in season, and Nexon's
/// service lists it.
fn live(reading: &notice::Reading, now: notice::Minutes) -> Told {
    Told::at(reading, now, true, true)
}

fn open() -> Lamp {
    Lamp { open: true, extended: false, until: None, season: true }
}

/// The app's home is asked anew whether this version may still patch as a maintenance
/// begins: at the turn from none underway to one underway, and at no other.
#[test]
fn a_maintenance_begins_once_where_none_was_underway_before() {
    let listed = listed();
    let mut watch = notice::Watch::default();
    let before = live(&watch.read(&listed, &[], at(3, 59)), at(3, 59));
    let underway = live(&watch.read(&listed, &[], at(4, 0)), at(4, 0));
    assert!(underway.begins_after(&before));
    // It goes on, and it ends.
    let later = live(&watch.read(&listed, &[], at(4, 1)), at(4, 1));
    assert!(!later.begins_after(&underway));
    let over = live(&watch.read(&listed, &[], at(10, 0)), at(10, 0));
    assert!(!over.begins_after(&later));
    // One that is underway as the app starts has not begun since anything was known:
    // the home was asked a moment ago, as the app started.
    assert!(!underway.begins_after(&Told::default()));
}

/// A board is read from a page that is one. Whatever else comes back under the
/// board's address is no reading of it, and least of all one that says the game is open.
#[test]
fn a_page_that_is_no_board_is_no_reading_of_the_board() {
    let notice = r#"<li><a href="/News/Notice/Inspection/150506"><span>[점검] 11/26(목) 정기점검 안내</span></a></li>"#;
    let board = board_on(&format!(r#"<html><div class="news_board"><ul>{notice}</ul></div></html>"#)).expect("a board");
    assert_eq!((board.notices.len(), board.out_of_season), (1, false));
    assert_eq!(board.notices[0].id, 150_506);
    // A board with nothing on it is a board: no maintenance is announced.
    let empty = board_on(r#"<div class="news_board"><ul></ul></div>"#).expect("a board without notices");
    assert!(empty.notices.is_empty() && !empty.out_of_season);
    // The test world between two operating periods shows a panel in place of its board.
    let closed = board_on("<div class=\"season\">지금은 테스트월드 운영기간이 아닙니다.</div>").expect("the panel");
    assert!(closed.out_of_season && closed.notices.is_empty());

    let none_of_them = [
        "",
        "<html><body><form action=\"/login\">네트워크에 로그인해 주세요.</form></body></html>",
        "<html><body><h1>502 Bad Gateway</h1></body></html>",
        // A link to a notice in a page's menu is no list of notices.
        &format!("<html><ul class=\"menu\">{notice}</ul></html>"),
    ];
    for page in none_of_them {
        assert!(board_on(page).is_none(), "{page:?}");
    }
}

/// Nexon is asked once. What it said is held against the clock from then on.
#[test]
fn a_maintenance_that_was_listed_closes_the_game_at_its_minute_without_asking_again() {
    let listed = listed();
    let mut watch = notice::Watch::default();
    for minute in [50, 59] {
        let reading = watch.read(&listed, &[], at(3, minute));
        assert!(!reading.soon, "03:{minute}");
        let told = Told { lamp: Some(open()), open: Some(true), underway: Some(false) };
        assert_eq!(live(&reading, at(3, minute)), told);
    }
    let reading = watch.read(&listed, &[], at(4, 0));
    assert!(reading.soon);
    let closed = Lamp { open: false, extended: false, until: Some("10:00".to_owned()), season: true };
    let told = Told { lamp: Some(closed), open: Some(false), underway: Some(true) };
    assert_eq!(live(&reading, at(4, 0)), told);
}

/// The service alone says nothing of a maintenance being over, so its hours are what
/// there is to go by, and the watch says that it is over once.
#[test]
fn a_maintenance_that_only_the_service_lists_is_over_at_its_minute() {
    let listed = listed();
    let mut watch = notice::Watch::default();
    assert!(watch.read(&listed, &[], at(9, 59)).underway.is_some());
    let reading = watch.read(&listed, &[], at(10, 0));
    assert!(reading.ended && !reading.soon);
    assert_eq!(live(&reading, at(10, 0)).open, Some(true));
    // Held against the clock again a second later, it is not over a second time.
    assert!(!watch.read(&listed, &[], at(10, 0)).ended);
}

/// The test world is not listed by Nexon's service: its board is all there is, and
/// that says when it is closed, not that it is open.
#[test]
fn of_a_game_that_cannot_be_seen_into_only_what_closes_it_is_shown() {
    let check =
        notice::Notice { id: 195, title: "11/26(목) 전체 테스트월드 채널 점검 (15:50~16:10)".to_owned() };
    // 2026-11-26, the day in the notice, is 20783 days after the first of 1970.
    let now = |hour: i64, minute: i64| 20_783 * 1440 + hour * 60 + minute;
    let mut watch = notice::Watch::default();

    // Nothing is in the way as far as its board says. That is not the game being known to
    // be open: nothing is shown of it, and nothing is done on it.
    let reading = watch.read(&[], std::slice::from_ref(&check), now(15, 0));
    let told = Told::at(&reading, now(15, 0), true, false);
    assert_eq!(told, Told { lamp: None, open: None, underway: Some(false) });

    // A maintenance its board announces shows like any other.
    let reading = watch.read(&[], std::slice::from_ref(&check), now(15, 50));
    let closed = Lamp { open: false, extended: false, until: Some("16:10".to_owned()), season: true };
    let told = Told::at(&reading, now(15, 50), true, false);
    assert_eq!(told, Told { lamp: Some(closed), open: Some(false), underway: Some(true) });

    // Between two operating periods it is closed altogether: that is what shows,
    // whatever else its board says, and it is no maintenance.
    let off = Lamp { open: false, extended: false, until: None, season: false };
    for reading in [watch.read(&[], &[check], now(15, 55)), notice::Reading::default()] {
        let told = Told::at(&reading, now(15, 55), false, false);
        assert_eq!((told.lamp, told.open), (Some(off.clone()), Some(false)));
    }
    assert_eq!(Told::at(&notice::Reading::default(), now(15, 55), false, false).underway, Some(false));
}

/// What the page is told of the lamp, by the names it draws it by.
#[test]
fn the_lamp_is_told_under_the_names_the_page_draws_it_by() {
    let off = Lamp { open: false, extended: false, until: None, season: false };
    let told = serde_json::json!({ "open": false, "extended": false, "until": null, "season": false });
    assert_eq!(serde_json::to_value(off).unwrap(), told);
    let told = serde_json::json!({ "open": true, "extended": false, "until": null, "season": true });
    assert_eq!(serde_json::to_value(open()).unwrap(), told);
}

const UNSEEN: notice::Block = notice::Block::Unseen;
const THERE: notice::Block = notice::Block::There;
const GONE: notice::Block = notice::Block::Gone;

#[test]
fn nexon_is_read_closer_where_news_is_expected_and_seldom_elsewhere() {
    let minutes = |minutes: i64| Some(minutes * 60);
    let pace = |to_end| pace(to_end, UNSEEN);
    // No maintenance: seldom.
    assert_eq!(pace(None), 600);
    // Through the body of one, down to thirteen minutes before its planned end.
    assert_eq!((pace(minutes(360)), pace(minutes(60)), pace(Some(13 * 60 + 1))), (30, 30, 30));
    // From thirteen minutes to three before it.
    assert_eq!((pace(minutes(13)), pace(minutes(5)), pace(Some(181))), (10, 10, 10));
    // Its last three minutes, the end, and the first quarter of an hour overdue.
    assert_eq!((pace(minutes(3)), pace(Some(1)), pace(Some(0)), pace(minutes(-15))), (5, 5, 5, 5));
    // Overdue for longer.
    assert_eq!((pace(Some(-901)), pace(minutes(-120))), (10, 10));
    // Each copy of the app is late by up to a fifth of the wait, and no more than a minute.
    assert_eq!((spread(5), spread(10), spread(30), spread(600)), (1, 2, 6, 60));

    // The block on the site: while it is there, nothing changes before the planned
    // end, and past it the closest pace holds for as long as the block does.
    assert_eq!((super::pace(minutes(60), THERE), super::pace(minutes(5), THERE)), (30, 10));
    assert_eq!((super::pace(Some(-1), THERE), super::pace(minutes(-50), THERE)), (5, 5));
    // Gone: the end is a matter of moments, wherever the hours stand.
    assert_eq!((super::pace(minutes(60), GONE), super::pace(minutes(-20), GONE)), (5, 5));
    assert_eq!(super::pace(None, GONE), 600);
}

#[test]
fn rounds_that_fail_are_set_further_apart_except_where_the_end_is_decided() {
    let wait = |to_end, failed| wait(to_end, UNSEEN, failed);
    // Outside a maintenance a round that failed changes nothing: it is asked seldom anyway.
    assert_eq!((wait(None, 0), wait(None, 3)), (600, 600));
    // In the body of one: twice as long each time, up to five minutes.
    let body = Some(3 * 3600);
    assert_eq!(wait(body, 0), 30);
    assert_eq!((1..=7).map(|failed| wait(body, failed)).collect::<Vec<_>>(), [30, 60, 120, 240, 300, 300, 300]);
    // In the last three minutes and the first minute overdue: again at once, every time.
    for left in [180, 60, 0, -60] {
        assert_eq!((1..=4).map(|failed| wait(Some(left), failed)).collect::<Vec<_>>(), [5, 5, 5, 5], "{left}");
    }
    // Overdue for longer than a minute, the rounds are set apart again.
    assert_eq!((1..=4).map(|failed| wait(Some(-61), failed)).collect::<Vec<_>>(), [5, 10, 20, 40]);
    assert_eq!(wait(Some(181), 3), 40);
}

#[test]
fn at_the_closer_paces_only_the_account_that_says_the_end_is_asked() {
    // No maintenance underway, the first round, and whenever both were asked half a
    // minute ago or longer: both.
    assert_eq!(asked(None, UNSEEN, Some(5), true), Asked::Both);
    assert_eq!(asked(Some(true), UNSEEN, None, true), Asked::Both);
    assert_eq!(asked(Some(true), UNSEEN, Some(30), true), Asked::Both);
    assert_eq!(asked(Some(false), THERE, Some(45), true), Asked::Both);
    // In between: the board for a maintenance it has a notice of, the service for one
    // it has none of. For a client the service does not list, the board is all there is.
    assert_eq!(asked(Some(true), UNSEEN, Some(5), true), Asked::Board);
    assert_eq!(asked(Some(false), UNSEEN, Some(29), true), Asked::Service);
    assert_eq!(
        (asked(Some(false), UNSEEN, Some(5), false), asked(Some(true), UNSEEN, Some(5), false)),
        (Asked::Board, Asked::Board)
    );
    // While the site's block is there, the site is what is watched.
    assert_eq!(
        (asked(Some(false), THERE, Some(5), true), asked(Some(true), THERE, Some(5), true)),
        (Asked::Board, Asked::Board)
    );
    // Once it is gone, both are asked every time.
    assert_eq!(
        (asked(Some(false), GONE, Some(5), true), asked(Some(true), GONE, Some(1), false)),
        (Asked::Both, Asked::Both)
    );
}

#[test]
fn a_reading_is_made_as_a_planned_end_and_a_planned_start_arrive() {
    let (start, end) = (at(4, 0), at(10, 0));
    let ms = |minutes: i64| minutes * 60_000;
    // A copy with no delay of its own, and one that is 42 seconds late for a round and
    // 700 ms for a moment.
    let none = |read, ahead, failed| due(read, ahead, failed, &|_| 0, 0);
    let late = |read, ahead, failed| due(read, ahead, failed, &|_| 42, 700);
    // Nothing underway and nothing listed: at the usual pace, by this copy's delay later.
    assert_eq!(none(ms(start) - 3_600_000, Ahead::default(), 0), ms(start) - 3_000_000);
    assert_eq!(late(ms(start) - 3_600_000, Ahead::default(), 0), ms(start) - 2_958_000);
    // One is listed to begin within the wait: read as it begins, by this copy's own
    // fraction of a second later.
    let listed = Ahead { next_start: Some(start), ..Ahead::default() };
    assert_eq!(none(ms(start) - 240_000, listed, 0), ms(start));
    assert_eq!(late(ms(start) - 240_000, listed, 0), ms(start) + 700);
    // Underway: at the pace of where the last reading stood.
    let underway = Ahead { ends: Some(end), ..Ahead::default() };
    assert_eq!(none(ms(start), underway, 0), ms(start) + 30_000);
    assert_eq!(none(ms(end) - 600_000, underway, 0), ms(end) - 590_000);
    // The last reading before the end is followed by the one at the end itself.
    let a_second_late = |read, ahead| due(read, ahead, 0, &|_| 1, 700);
    assert_eq!(a_second_late(ms(end) - 3_000, underway), ms(end) + 700);
    // That one made, the readings go on at the pace of an overdue maintenance.
    assert_eq!(a_second_late(ms(end) + 700, underway), ms(end) + 6_700);
    // Said to be over at its planned end: read at the nearer pace for a quarter of an
    // hour, where an extension that is posted late shows, and seldom again after that.
    let over = Ahead { over: Some(end * 60), ..Ahead::default() };
    assert_eq!(none(ms(end) + 1_000, over, 0), ms(end) + 11_000);
    assert_eq!(none(ms(end) + 900_000, over, 0), ms(end) + 910_000);
    assert_eq!(none(ms(end) + 901_000, over, 0), ms(end) + 1_501_000);
    // Rounds that fail are set apart, and the reading at the end is made all the same.
    assert_eq!(none(ms(end) - 250_000, underway, 6), ms(end));
    assert_eq!(none(ms(end) - 250_000, underway, 2), ms(end) - 230_000);
}

/// How long after one reading the next one comes at the latest, this copy's own delay
/// included, at each stretch of a maintenance from 4:00 to 10:00: the wait in force and a
/// fifth of that very wait on top, and never the delay of another stretch's wait.
///
/// The delay is drawn by whoever decides the wait. Drawn apart from it, a reading in the
/// quarter of an hour after the end once came up to a minute late where ten seconds were
/// meant: the delay was that of the wait that holds when no maintenance is underway.
#[test]
fn a_reading_is_never_later_than_its_own_wait_and_a_fifth_of_it() {
    let (start, end) = (at(4, 0), at(10, 0));
    let ms = |minutes: i64| minutes * 60_000;
    let underway = |block| Ahead { ends: Some(end), block, ..Ahead::default() };
    let listed = Ahead { next_start: Some(start), ..Ahead::default() };
    // Said to be over as its planned end arrived, twenty minutes after it, and two hours
    // before it. The quarter of an hour of nearer readings counts from when it was said.
    let said_over = |seconds: i64| Ahead { over: Some(end * 60 + seconds), ..Ahead::default() };
    let (over, late, early) = (said_over(0), said_over(1_200), said_over(-7_200));
    // Where the last reading was made, what is ahead, the rounds that failed, and the
    // seconds to the next reading: of the copy that is on time, and of the latest one.
    let table = [
        ("an hour before, none listed", ms(start) - 3_600_000, Ahead::default(), 0, 600, 660),
        ("an hour before, one listed", ms(start) - 3_600_000, listed, 0, 600, 660),
        ("as it begins", ms(start), underway(UNSEEN), 0, 30, 36),
        ("in its body", ms(start) + 3_600_000, underway(UNSEEN), 0, 30, 36),
        ("in its body, blocked", ms(start) + 3_600_000, underway(THERE), 0, 30, 36),
        ("in its body, the block gone", ms(start) + 3_600_000, underway(GONE), 0, 5, 6),
        ("in its body, failing", ms(start) + 3_600_000, underway(UNSEEN), 3, 120, 144),
        ("in its body, failing for long", ms(start) + 3_600_000, underway(UNSEEN), 9, 300, 360),
        ("thirteen minutes before the end", ms(end) - 780_000, underway(UNSEEN), 0, 10, 12),
        ("ten minutes before the end", ms(end) - 600_000, underway(UNSEEN), 0, 10, 12),
        ("ten minutes before the end, failing", ms(end) - 600_000, underway(UNSEEN), 3, 40, 48),
        ("three minutes before the end", ms(end) - 180_000, underway(UNSEEN), 0, 5, 6),
        ("a minute before the end, failing", ms(end) - 60_000, underway(UNSEEN), 3, 5, 6),
        ("a minute overdue", ms(end) + 60_000, underway(UNSEEN), 0, 5, 6),
        ("a quarter of an hour overdue", ms(end) + 900_000, underway(UNSEEN), 0, 5, 6),
        ("overdue for longer", ms(end) + 901_000, underway(UNSEEN), 0, 10, 12),
        ("overdue for longer, blocked", ms(end) + 1_200_000, underway(THERE), 0, 5, 6),
        ("overdue for longer, the block gone", ms(end) + 1_200_000, underway(GONE), 0, 5, 6),
        ("overdue for longer, failing", ms(end) + 1_200_000, underway(UNSEEN), 4, 80, 96),
        ("said to be over as it ended", ms(end) + 1_000, over, 0, 10, 12),
        ("ten minutes after it was over", ms(end) + 600_000, over, 0, 10, 12),
        ("ten minutes after it was over, failing", ms(end) + 600_000, over, 5, 10, 12),
        ("a quarter of an hour after it was over", ms(end) + 900_000, over, 0, 10, 12),
        ("longer after it was over", ms(end) + 901_000, over, 0, 600, 660),
        // The reading that found it over began a moment before that was said.
        ("the reading that found it over", ms(end) - 1_000, over, 0, 10, 12),
        // It ran twenty minutes over: its quarter of an hour begins when it is said to be
        // over, where an extension that is posted yet again shows as soon.
        ("said to be over twenty minutes late", ms(end) + 1_201_000, late, 0, 10, 12),
        ("a quarter of an hour after that", ms(end) + 2_100_000, late, 0, 10, 12),
        ("longer after that", ms(end) + 2_101_000, late, 0, 600, 660),
        // It ended two hours early: its quarter of an hour is then, and there is none at
        // the end that was planned, where nothing is to be expected any more.
        ("said to be over two hours early", ms(end) - 7_199_000, early, 0, 10, 12),
        ("a quarter of an hour after that", ms(end) - 6_300_000, early, 0, 10, 12),
        ("longer after that", ms(end) - 6_299_000, early, 0, 600, 660),
        ("as the end arrives that was planned", ms(end), early, 0, 600, 660),
        ("a minute after the end that was planned", ms(end) + 60_000, early, 0, 600, 660),
    ];
    for (stretch, read, ahead, failed, on_time, latest) in table {
        let after = |own: &dyn Fn(u64) -> u64| (due(read, ahead, failed, own, 0) - read) / 1000;
        assert_eq!((after(&|_| 0), after(&|most| most)), (on_time, latest), "{stretch}");
        // A fifth of the wait that is in force there, and a minute at the most.
        assert_eq!(latest - on_time, (on_time / 5).min(60), "{stretch}");
    }

    // A reading at a moment the hours name comes this copy's fraction of a second after
    // it, and never by the delay of a round: as a listed one begins, and at the planned end.
    let at_a_moment = |read, ahead| due(read, ahead, 0, &|most| most, 1_000);
    assert_eq!(at_a_moment(ms(start) - 240_000, listed), ms(start) + 1_000);
    assert_eq!(at_a_moment(ms(end) - 3_000, underway(UNSEEN)), ms(end) + 1_000);
    assert_eq!(at_a_moment(ms(end) - 3_000, underway(THERE)), ms(end) + 1_000);
}

/// What one copy of the app asks of Nexon from the start of a maintenance to twenty
/// minutes past its planned end, where it is over: the rounds, and the questions in
/// them. `block` says whether the site is blocked all the while.
fn cost(block: notice::Block) -> (i32, i32) {
    let (start, end) = (at(4, 0) * 60_000, at(10, 0) * 60_000);
    let ahead = Ahead { ends: Some(at(10, 0)), block, ..Ahead::default() };
    let (mut read, mut rounds, mut questions, mut both_at) = (start, 0, 0, None::<i64>);
    while read <= end + 20 * 60_000 {
        let asks = asked(Some(true), block, both_at.map(|at| (read - at) / 1000), true);
        if asks == Asked::Both {
            both_at = Some(read);
        }
        rounds += 1;
        questions += if asks == Asked::Both { 2 } else { 1 };
        read = due(read, ahead, 0, &|_| 0, 0);
    }
    (rounds, questions)
}

/// What a maintenance of six hours that runs twenty minutes over costs Nexon. At the
/// old pace it was 760 rounds of two questions each.
#[test]
fn a_long_maintenance_that_runs_over_costs_a_thousand_rounds() {
    // 694 rounds through the body, 60 from thirteen minutes to three before the end,
    // 217 from there until a quarter of an hour over, 30 in the five minutes after.
    assert_eq!(cost(UNSEEN), (694 + 60 + 217 + 30, 1761));
    // With the site blocked all the while: the closest pace for all twenty minutes it
    // is overdue, which is 277 rounds from three minutes before the end.
    assert_eq!(cost(THERE), (694 + 60 + 277, 1792));
}

#[test]
fn a_block_is_seen_only_as_a_change_during_the_hours_of_a_maintenance() {
    use notice::Site::{Blocked, Board, OutOfSeason, Unreachable};
    let on = Some(FIRST);
    let fresh = Blocking::default();
    // The site cannot be read from the app's first look on: that proves nothing.
    assert_eq!(fresh.read(Blocked, on, 300).block, UNSEEN);
    assert_eq!(fresh.read(Unreachable, on, 300), fresh);
    // It read as the board before, and answers with something else during the hours
    // of a maintenance: the block is seen. Outside those hours it is not.
    let readable = fresh.read(Board, None, 100);
    assert!(readable.readable && readable.block == UNSEEN);
    assert_eq!(readable.read(Blocked, None, 200).block, UNSEEN);
    let there = readable.read(Blocked, on, 300);
    assert_eq!((there.block, there.of, there.gone_at), (THERE, on, None));
    // No answer is neither a block nor the end of one.
    assert_eq!(readable.read(Unreachable, on, 300), readable);
    assert_eq!(there.read(Unreachable, on, 400), there);
    assert_eq!(there.read(Blocked, on, 400), there);
    // The site reads again: gone, and when.
    let gone = there.read(Board, on, 36_007);
    assert_eq!((gone.block, gone.gone_at), (GONE, Some(36_007)));
    assert_eq!(there.read(OutOfSeason, on, 36_007).block, GONE);
    assert_eq!(gone.read(Board, on, 36_010), gone);
    // Blocked again in the same maintenance: there again.
    assert_eq!(gone.read(Blocked, Some(LONGER), 36_100).block, THERE);
    // The same maintenance, extended, keeps what is known. Another one starts anew,
    // and the site is still known to have been readable.
    assert_eq!((there.of(Some(LONGER)), gone.of(None)), (there, gone));
    let next = gone.of(Some(NEXT));
    assert_eq!((next.block, next.of, next.gone_at, next.readable), (UNSEEN, None, None, true));
}

/// The players' signal: while the site's block is there the maintenance goes on, past
/// its planned end too, and when the block is gone and the hours are up it is over.
#[test]
fn the_block_on_the_site_holds_the_end_back_and_says_it_when_it_is_gone() {
    let (start, end) = (at(4, 0) * 60, at(10, 0) * 60);
    let hours = Some((at(4, 0), at(10, 0)));
    let nothing: Vec<String> = Vec::new();
    let blocked = |following: &mut Following, site, seconds| {
        following.blocking = following.blocking.read(site, hours, seconds);
        following.watch.site_block(following.blocking.block);
    };
    let begun = |following: &mut Following| {
        blocked(following, notice::Site::Board, start - 60);
        held(following, &listed(), &[], (start - 1, start - 30));
        held(following, &listed(), &[], (start, start - 30));
        blocked(following, notice::Site::Blocked, start + 2);
    };

    // Still there as the planned end passes, though the service lists nothing longer:
    // not over, and the lamp stays closed.
    let mut following = Following::default();
    begun(&mut following);
    assert_eq!(following.blocking.block, THERE);
    assert_eq!(held(&mut following, &listed(), &[], (end, end)), (false, nothing.clone()));
    assert_eq!(held(&mut following, &listed(), &[], (end + 600, end + 595)), (false, nothing.clone()));
    // The site reads again: over at once.
    blocked(&mut following, notice::Site::Board, end + 601);
    assert_eq!(held(&mut following, &listed(), &[], (end + 601, end + 600)), (true, vec![OVER.to_owned()]));

    // Gone while the hours still cover the moment: nothing is said until they are up.
    let mut following = Following::default();
    begun(&mut following);
    blocked(&mut following, notice::Site::Board, end - 300);
    assert_eq!(held(&mut following, &listed(), &[], (end - 299, end - 300)), (false, nothing.clone()));
    // And then with the reading made as they are up, like any end by the hours.
    assert_eq!(held(&mut following, &listed(), &[], (end, end - 5)), (false, nothing.clone()));
    assert_eq!(held(&mut following, &listed(), &[], (end, end)), (true, vec![OVER.to_owned()]));

    // No answer from the site changes nothing: neither open nor closed for it.
    let mut following = Following::default();
    begun(&mut following);
    blocked(&mut following, notice::Site::Unreachable, end + 5);
    assert_eq!(held(&mut following, &listed(), &[], (end + 6, end + 5)), (false, nothing.clone()));

    // Still there an hour past the planned end, with nothing longer listed: passed
    // over, and over by the clock.
    let mut following = Following::default();
    begun(&mut following);
    assert_eq!(held(&mut following, &listed(), &[], (end + 3599, end + 3595)), (false, nothing.clone()));
    assert_eq!(held(&mut following, &listed(), &[], (end + 3600, end + 3595)), (true, vec![OVER.to_owned()]));

    // Never seen in this maintenance: everything as without it.
    let mut following = Following::default();
    blocked(&mut following, notice::Site::Board, start - 60);
    held(&mut following, &listed(), &[], (start, start - 30));
    assert_eq!(held(&mut following, &listed(), &[], (end, end)), (true, vec![OVER.to_owned()]));

    // What is written down of a block, for the rule to be drawn closer by.
    let answer = download::Answer {
        status: 503,
        path: "/News/Notice/Inspection".to_owned(),
        text: "<html><title>점검 안내</title></html>".to_owned(),
        date: None,
    };
    assert_eq!(
        block_line(THERE, start + 2, &answer),
        "the block on the site was seen at 04:00:02: status 503, /News/Notice/Inspection, title \"점검 안내\", 41 bytes"
    );
    assert!(block_line(GONE, end + 7, &answer).starts_with("the block on the site was gone at 10:00:07: "));
}
