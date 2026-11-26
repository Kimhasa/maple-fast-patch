//! The watch on the game's maintenance: the server status on the screen, and the word that
//! a maintenance has begun, has been extended or is over.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mfp_engine::download;
use mfp_kms::notice;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::clock::{Clock, pc_now};
use crate::state::{Patcher, Site};
use crate::window::notify;

/// Whether a client's game can be played right now, as the page shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Lamp {
    /// No maintenance is underway.
    open: bool,
    /// The maintenance underway has been extended.
    extended: bool,
    /// When the maintenance underway is planned to end, as a clock shows it: `10:00`. Left
    /// out once that time has passed and the maintenance goes on.
    until: Option<String>,
    /// The game is within one of its operating periods. The test world has times when it
    /// is not: it is closed altogether then, and that is all there is to say of it.
    season: bool,
}

/// What a site knows of when its game is closed, as last read from what Nexon publishes.
/// Nothing of it is known until something could be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Told {
    /// What the page shows of it.
    pub(crate) lamp: Option<Lamp>,
    /// Whether the game can be played.
    pub(crate) open: Option<bool>,
    /// Whether a maintenance is underway, which is when a patch comes out.
    pub(crate) underway: Option<bool>,
}

impl Told {
    /// What is known at `now`, going by a reading of what Nexon last said. `in_season` is
    /// what the client's board says of its operating periods, and `seen_into` whether
    /// Nexon's service lists this client's maintenance.
    ///
    /// That a game can be played is only said where it can be seen: of a client the service
    /// does not list, the page is shown nothing while nothing is known to be in the way.
    fn at(reading: &notice::Reading, now: notice::Minutes, in_season: bool, seen_into: bool) -> Told {
        let underway = reading.underway.as_ref();
        let lamp = match underway {
            _ if !in_season => Some(Lamp { open: false, extended: false, until: None, season: false }),
            Some(closed) => Some(Lamp {
                open: false,
                extended: closed.extended,
                until: (closed.ends > now).then(|| notice::shown(closed.ends, now)),
                season: true,
            }),
            None => seen_into.then_some(Lamp { open: true, extended: false, until: None, season: true }),
        };
        Told { lamp, open: Some(underway.is_none() && in_season), underway: Some(underway.is_some()) }
    }

    /// Whether a maintenance has begun since `before` was known: none was underway then,
    /// and one is now. A maintenance that is found underway as the app starts has not begun
    /// since anything.
    fn begins_after(&self, before: &Told) -> bool {
        (before.underway, self.underway) == (Some(false), Some(true))
    }
}

/// What is said of a maintenance that has begun while the app is open, and of one that is
/// over.
const BEGUN: &str = "점검이 시작되었습니다.";
const OVER: &str = "점검이 완료되었습니다.";

/// What is said of a maintenance that goes on until `ends`, later than was planned.
fn extended_until(ends: notice::Minutes, now: notice::Minutes) -> String {
    format!("점검이 {}까지 연장되었습니다.", notice::shown(ends, now))
}

/// A maintenance by its hours: when it began, and when it is planned to end.
type Hours = (notice::Minutes, notice::Minutes);

/// Whether two accounts of hours are of one maintenance: they overlap. An extension moves
/// the end and leaves the start where it was, and the service and the board may give the
/// start a few minutes apart. The next maintenance begins after this one's hours.
fn same(one: Hours, other: Hours) -> bool {
    one.0 < other.1 && other.0 < one.1
}

/// Where the watch stands with the maintenance of a client's game, from one look to the
/// next, for what is said of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Standing {
    /// Nothing is known: the app has only just started, or what Nexon says could not be
    /// read for too long.
    #[default]
    Unknown,
    /// None is underway, and none is missing.
    Open,
    /// One is underway.
    Underway(Hours),
    /// It was said to be over. One that is underway again within its hours is the same
    /// one, going on.
    Over(Hours),
    /// One was underway and is in neither account any more, without having been said to
    /// be over: one of the two could not be read, or dropped it.
    Lost(Hours),
}

/// One look of the watch at what Nexon last said.
#[derive(Clone, Copy, Debug)]
struct Look {
    /// The hours of the maintenance underway, if one is.
    underway: Option<Hours>,
    /// The one seen underway at the look before is over; see [`notice::Watch`].
    ended: bool,
    now: notice::Minutes,
}

/// What a look gives to say of the maintenance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Word {
    /// It is over.
    over: bool,
    /// It goes on until this time, after it was over: it was extended late.
    extended: Option<notice::Minutes>,
    /// One has begun.
    begun: bool,
}

impl Standing {
    /// Where the watch stands after `look`, and what there is to say.
    ///
    /// A maintenance has begun when none was underway at the look before and one is now,
    /// and that is said once of each. It is not so for one that is found underway when
    /// nothing was known: as the app starts in the middle of it, or when what Nexon says
    /// can be read again. Nor for one that is extended, which stays underway, nor for one
    /// that had left both accounts and is back within its hours. A maintenance that is
    /// only listed for later has not begun until the clock says so.
    ///
    /// A maintenance that is underway again within the hours of the one that was over is
    /// that same one going on: it was extended after its end, and that is what is said,
    /// not that one has begun.
    fn after(self, look: Look) -> (Standing, Word) {
        let Look { underway, ended, now } = look;
        let quiet = Word::default();
        match (self, underway) {
            (Standing::Open, Some(new)) => (Standing::Underway(new), Word { begun: true, ..quiet }),
            (Standing::Underway(old) | Standing::Lost(old), Some(new)) if same(old, new) => {
                (Standing::Underway(new), quiet)
            }
            (Standing::Over(old), Some(new)) if same(old, new) => {
                (Standing::Underway(new), Word { extended: Some(new.1), ..quiet })
            }
            // Another one is underway than the one before, which is over with that.
            (Standing::Underway(_), Some(new)) => (Standing::Underway(new), Word { over: ended, begun: true, ..quiet }),
            (Standing::Over(_) | Standing::Lost(_), Some(new)) => {
                (Standing::Underway(new), Word { begun: true, ..quiet })
            }
            // Found underway when nothing was known.
            (Standing::Unknown, Some(new)) => (Standing::Underway(new), Word { over: ended, ..quiet }),

            (Standing::Underway(old), None) if ended => (Standing::Over(old), Word { over: true, ..quiet }),
            (Standing::Underway(old) | Standing::Lost(old), None) if old.1 > now => (Standing::Lost(old), quiet),
            (Standing::Over(old), None) => (Standing::Over(old), quiet),
            (_, None) => (Standing::Open, Word { over: ended, ..quiet }),
        }
    }
}

/// What the user is told of `word` at `now`, in the order it came about, when they want to
/// be told of the maintenance (`alert`).
fn said(word: Word, alert: bool, now: notice::Minutes) -> Vec<String> {
    let over = word.over.then(|| OVER.to_owned());
    let extended = word.extended.map(|ends| extended_until(ends, now));
    let begun = word.begun.then(|| BEGUN.to_owned());
    [over, extended, begun].into_iter().flatten().filter(|_| alert).collect()
}

/// How often Nexon is read, in seconds, and where each pace holds.
///
/// What is on the screen and what is said can only be as near to the moment as the last
/// reading. Outside a maintenance nothing is expected, and the hours of the next one are
/// listed days ahead: seldom. Through the body of a maintenance an early end or an
/// extension may come at any time, and is seen within half a minute. News is expected
/// around the planned end: an extension is posted in the last minutes, and the end is said
/// then or shortly after. So the readings come closer from thirteen minutes before it,
/// and closest in its last three minutes and for the first quarter of an hour that it is
/// overdue. One that is overdue for longer than that may end at any time, and is read at
/// the nearer pace until it does. On top of these, a reading is made at the planned end
/// and at a planned start themselves; see [`due`].
const SELDOM: i64 = 10 * 60;
const BODY: i64 = 30;
const NEAR: i64 = 10;
const CLOSE: i64 = 5;
/// From how long before the planned end the pace is the nearer and the closest one, and
/// for how long after it the closest.
const NEAR_FROM: i64 = 13 * 60;
const CLOSE_FROM: i64 = 3 * 60;
const CLOSE_FOR: i64 = 15 * 60;
/// For how long after the planned end a reading that fails is made again at the closest
/// pace, as in the minutes before it: until then the end is not said by the clock alone.
const DECIDED_WITHIN: i64 = 60;
/// The longest wait between two rounds of a maintenance while the rounds fail.
const SLOWEST: i64 = 5 * 60;
/// By how much at most the reading at a planned end or start comes after it, in
/// milliseconds: every copy of the app by an amount of its own, so that they do not all ask
/// in the same instant.
const MOMENT_SPREAD: u64 = 1000;
/// An account that cannot be read at the moment stands for this long.
const STANDS: Duration = Duration::from_secs(20 * 60);

/// The seconds between two readings while a maintenance is underway that is planned to end
/// in `to_end` seconds, below zero once it is overdue; `None` while none is underway.
/// `block` is what is known of the block on the game's site.
///
/// The block says where news is due more surely than the hours do. While it is there past
/// the planned end, the maintenance goes on and may end at any moment: the closest pace,
/// for as long as that lasts. Once it is gone, the end is a matter of the hours being up.
fn pace(to_end: Option<i64>, block: notice::Block) -> i64 {
    match (to_end, block) {
        (None, _) => SELDOM,
        (Some(_), notice::Block::Gone) => CLOSE,
        (Some(left), notice::Block::There) if left < 0 => CLOSE,
        (Some(left), _) if left > NEAR_FROM => BODY,
        (Some(left), _) if left > CLOSE_FROM => NEAR,
        (Some(left), _) if left >= -CLOSE_FOR => CLOSE,
        (Some(_), _) => NEAR,
    }
}

/// The seconds until the next round of asking Nexon, before this copy's own delay is added,
/// after `failed` rounds in a row could not read one of the accounts; see [`pace`].
///
/// A server that cannot answer is not helped by being asked often: while rounds fail during
/// a maintenance, each wait is twice the one before, up to five minutes. The first round
/// that works brings the pace back. Around the planned end that does not hold: the end is
/// decided there, and a reading that fails is made again at the pace of that time.
fn wait(to_end: Option<i64>, block: notice::Block, failed: u32) -> i64 {
    let pace = pace(to_end, block);
    let decided_now = to_end.is_some_and(|left| (-DECIDED_WITHIN..=CLOSE_FROM).contains(&left));
    match to_end {
        Some(_) if failed > 0 && !decided_now => (pace << (failed - 1).min(6)).min(SLOWEST).max(pace),
        _ => pace,
    }
}

/// By how many seconds at most a copy of the app puts a round off that is `wait` seconds
/// away, so that the copies do not all ask at the same moment: a fifth of the wait.
fn spread(wait: i64) -> u64 {
    (wait / 5).clamp(0, 60) as u64
}

/// Which of the two accounts a round asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Both,
    Service,
    Board,
}

/// Which accounts the next round asks. `noticed` says of the maintenance underway whether
/// the board has a notice of it, `block` what is known of the block on the site,
/// `since_both` how many seconds ago both were asked, and `listed` whether Nexon's service
/// lists this client at all.
///
/// Neither account answers a question of whether anything has changed (they send no tag
/// and no date of their page to ask by), so each reading is a whole one. At the closer
/// paces only the account is read that says when the maintenance underway is over: the
/// site while its block is there, and otherwise the board for a maintenance it has a notice
/// of and the service for one it has none of. Both are read at the pace of the body of a
/// maintenance, as ever: an extension may reach either first. And both are read once the
/// block is gone: what either says then is what ends the maintenance.
fn asked(noticed: Option<bool>, block: notice::Block, since_both: Option<i64>, listed: bool) -> Asked {
    let Some(noticed) = noticed else { return Asked::Both };
    match block {
        notice::Block::Gone => Asked::Both,
        _ if !since_both.is_some_and(|since| since < BODY) => Asked::Both,
        notice::Block::There => Asked::Board,
        notice::Block::Unseen if noticed || !listed => Asked::Board,
        notice::Block::Unseen => Asked::Service,
    }
}

/// What the time of the next reading goes by: the planned end of the maintenance underway,
/// the start of the next one that is listed, and what is known of the block on the site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ahead {
    ends: Option<notice::Minutes>,
    next_start: Option<notice::Minutes>,
    block: notice::Block,
    /// The planned end of the maintenance that was last said to be over.
    over: Option<notice::Minutes>,
}

/// When Nexon is read next, in milliseconds of Nexon's clock: `read` is when the last
/// round began, `failed` how many rounds in a row have failed, and `own` this copy's own
/// delays, for a round at the usual pace and for one at a moment that matters.
///
/// The rounds come at the pace of where the last one stood; see [`wait`]. Before that, a
/// reading is made at the moments at which something is due by the hours Nexon lists: as
/// the planned end arrives, where an extension shows or the end is said, and as a
/// maintenance that is listed begins.
///
/// A maintenance that was said to be over at its planned end may yet be extended: Nexon
/// posts that late at times. For a quarter of an hour after that end the readings stay at
/// the nearer pace, so that the game is not shown as open for long when it is not.
fn due(read: i64, ahead: Ahead, failed: u32, own: (i64, i64)) -> i64 {
    let to_end = ahead.ends.map(|ends| ends * 60 - read.div_euclid(1000));
    let since_over = ahead.over.filter(|_| to_end.is_none()).map(|over| read.div_euclid(1000) - over * 60);
    let wait = match since_over {
        Some(since) if (0..=CLOSE_FOR).contains(&since) => NEAR,
        _ => wait(to_end, ahead.block, failed),
    };
    let at_the_pace = read + wait * 1000 + own.0;
    let moments = [ahead.ends, ahead.next_start].into_iter().flatten().map(|at| at * 60_000 + own.1);
    moments.filter(|at| *at > read).fold(at_the_pace, i64::min)
}

/// What the watch knows of the block on the game's site; see [`notice::Block`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Blocking {
    /// The site read as the board at some time since the app was started. Only then does
    /// an answer that is not the board show that something has changed.
    readable: bool,
    /// The hours of the maintenance the block was seen in.
    of: Option<Hours>,
    block: notice::Block,
    /// When it was seen to go, in seconds of Nexon's clock.
    gone_at: Option<i64>,
    /// That it was passed over, still there an hour past the planned end, is in the log.
    passed_over: bool,
}

impl Blocking {
    /// Takes in what the site answered at `seconds`, with the maintenance of `underway`
    /// underway, if one is.
    ///
    /// A block is seen where the site, which read as the board before in this run of the
    /// app, answers with something else during the hours of a maintenance. A site that
    /// cannot be read from the app's first look on proves nothing, and neither does such
    /// an answer outside the hours of any maintenance. It is gone when the site reads
    /// again. No answer says nothing: it is never a block, and never the end of one.
    fn read(self, site: notice::Site, underway: Option<Hours>, seconds: i64) -> Blocking {
        match site {
            notice::Site::Board | notice::Site::OutOfSeason if self.block == notice::Block::There => {
                Blocking { readable: true, block: notice::Block::Gone, gone_at: Some(seconds), ..self }
            }
            notice::Site::Board | notice::Site::OutOfSeason => Blocking { readable: true, ..self },
            notice::Site::Blocked if self.readable && underway.is_some() => {
                Blocking { of: underway, block: notice::Block::There, gone_at: None, ..self }
            }
            notice::Site::Blocked | notice::Site::Unreachable => self,
        }
    }

    /// The block is that of one maintenance. With another one underway than it was seen
    /// in, nothing is known of a block.
    fn of(self, underway: Option<Hours>) -> Blocking {
        match (self.of, underway) {
            (Some(of), Some(now)) if !same(of, now) => Blocking { readable: self.readable, ..Blocking::default() },
            _ => self,
        }
    }
}

/// A moment of Nexon's clock as the log has it, to the second: `10:00:07`.
fn to_the_second(seconds: i64) -> String {
    format!("{}:{:02}", notice::clock(seconds.div_euclid(60)), seconds.rem_euclid(60))
}

/// What is written down as the block on the site is seen and as it goes: when, and what
/// the site answered with. The rule for what a block is can be drawn closer by these
/// after a maintenance. Nothing in them is of this PC or its user.
fn block_line(block: notice::Block, seconds: i64, answer: &download::Answer) -> String {
    let what = if block == notice::Block::There { "seen" } else { "gone" };
    let (title, length) = (notice::title(&answer.text), answer.text.len());
    let at = to_the_second(seconds);
    format!(
        "the block on the site was {what} at {at}: status {}, {}, title {title:?}, {length} bytes",
        answer.status, answer.path
    )
}

/// What Nexon last said about the game's maintenance, by each of its two accounts, and when
/// each was read: for the service also when it was asked, by this PC's clock.
#[derive(Default)]
struct Heard {
    planned: Option<(Instant, i64, Vec<notice::Planned>)>,
    board: Option<(Instant, Board)>,
}

/// What a client's board says: its notices, and whether its page says that the game is
/// closed between two of its operating periods.
struct Board {
    notices: Vec<notice::Notice>,
    out_of_season: bool,
}

/// What a client's board says, read from its page. `None` for a page that is no board at
/// all: the login page of a network that wants one first, an error page, an empty answer.
///
/// Such a page holds no notices and no word of the game being closed, and that is not a
/// board that announces no maintenance. A page counts as the board when it has the board's
/// list, or the panel the test world shows in place of everything between two of its
/// operating periods.
fn board_on(page: &str) -> Option<Board> {
    let out_of_season = notice::out_of_season(page);
    (notice::is_board(page) || out_of_season).then(|| Board { notices: notice::read_board(page), out_of_season })
}

/// Follows the maintenance of every client's game for as long as the app is open: for the
/// lamp on the screen and for the word that it has begun or is over. A thread for each
/// client.
///
/// Only what Nexon publishes for everyone is read; the game's servers are not asked. It is
/// read seldom, often while a maintenance is underway, and at the moments its hours name.
/// Between two readings what was read is held against the clock every second: the hours
/// of a maintenance are listed before it begins, and the lamp turns at the second they say.
pub(crate) fn watch_maintenance(app: &AppHandle) {
    for place in 0..app.state::<Patcher>().sites.len() {
        let app = app.clone();
        std::thread::spawn(move || watch(&app, place));
    }
}

/// What follows a maintenance from one look to the next: for what is underway, for what
/// is said of it, and for the block on the site.
#[derive(Default)]
struct Following {
    watch: notice::Watch,
    standing: Standing,
    blocking: Blocking,
}

/// What a look found that the next reading goes by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Held {
    /// The maintenance underway: its planned end, and whether the board has a notice of it.
    underway: Option<(notice::Minutes, bool)>,
    /// When the next one that is listed begins.
    next_start: Option<notice::Minutes>,
    block: notice::Block,
    /// The planned end of the one that was last said to be over.
    over: Option<notice::Minutes>,
}

impl Held {
    fn ahead(&self) -> Ahead {
        let ends = self.underway.map(|(ends, _)| ends);
        Ahead { ends, next_start: self.next_start, block: self.block, over: self.over }
    }
}

/// Asks Nexon's service with `ask`, and sets `clock` by the time its answer states. Hands
/// on the text of the answer with the moment the question was sent, by this PC's clock.
fn dated(
    clock: &mut Clock,
    ask: impl FnOnce() -> mfp_engine::Result<download::Dated>,
) -> Result<(i64, String), String> {
    let sent = pc_now();
    let answer = ask().map_err(|err| err.to_string())?;
    if let Some(stated) = answer.date.as_deref().and_then(notice::stated_time) {
        clock.learn(sent, pc_now(), stated);
    }
    Ok((sent, answer.text))
}

/// Asks the game's site with `ask`, and sets `clock` by the time its answer states,
/// whatever the answer is.
fn answered(
    clock: &mut Clock,
    ask: impl FnOnce() -> mfp_engine::Result<download::Answer>,
) -> Result<download::Answer, String> {
    let sent = pc_now();
    let answer = ask().map_err(|err| err.to_string())?;
    if let Some(stated) = answer.date.as_deref().and_then(notice::stated_time) {
        clock.learn(sent, pc_now(), stated);
    }
    Ok(answer)
}

/// Follows the maintenance of the game of the site at `place`.
fn watch(app: &AppHandle, place: usize) {
    let patcher = app.state::<Patcher>();
    let site = &patcher.sites[place];
    let mut following = Following::default();
    let mut heard = Heard::default();
    let mut clock = Clock::default();
    let mut unread = false;
    // Rounds in a row in which one of the two accounts could not be read, and whether
    // the rounds have been set further apart for it.
    let mut failed_rounds: u32 = 0;
    let mut slowed = false;
    // What the last look found, and when both accounts were last asked.
    let mut held = Held::default();
    let mut both_at: Option<i64> = None;
    loop {
        // A client that is not installed has no maintenance anyone here waits for: nothing
        // is read for it, and what was read for a folder that is gone is put aside.
        if !site.flow.lock().unwrap().askable() {
            if std::mem::take(&mut *site.told.lock().unwrap()) != Told::default() {
                site.wake_desk();
            }
            (following, heard, held, both_at) = (Following::default(), Heard::default(), Held::default(), None);
            (unread, failed_rounds, slowed) = (false, 0, false);
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        let began = pc_now();
        let noticed = held.underway.map(|(_, noticed)| noticed);
        let since_both = both_at.map(|at| (began - at) / 1000);
        let asks = asked(noticed, held.block, since_both, site.inspection().is_some());
        if asks == Asked::Both {
            both_at = Some(began);
        }
        // The two accounts of the game's maintenance: Nexon's inspection service, where
        // it lists this client, and the client's notice board on the game's site.
        let service = site.inspection().filter(|_| asks != Asked::Board);
        let asked_service = service.map(|(url, service)| {
            dated(&mut clock, || download::soap_dated(url, service.action, service.request, 1 << 20))
        });
        let asked_site =
            (asks != Asked::Service).then(|| answered(&mut clock, || download::page_answer(site.board(), 1 << 20)));
        // What the site answered with: its board, its block, or nothing to go by.
        let site_said = asked_site.as_ref().map(|answer| match answer {
            Ok(answer) => notice::site(answer.status, &answer.text),
            Err(_) => notice::Site::Unreachable,
        });
        let not_asked = asked_service.as_ref().and_then(|asked| asked.as_ref().err()).cloned();
        // An answer that is neither the board nor the site's block says nothing either way.
        // That read has failed like one that fetched nothing: what was last read stands for
        // a while, and the rounds are set further apart. A block is an answer, and no failure.
        let not_read = match (&asked_site, site_said) {
            (Some(Err(why)), _) => Some(why.clone()),
            (Some(Ok(answer)), Some(notice::Site::Unreachable)) => {
                Some(format!("the page is not the notice board (status {})", answer.status))
            }
            _ => None,
        };
        let failure = not_asked.clone().or(not_read);
        if let (Some(failure), false) = (&failure, unread) {
            let which = if not_asked.is_some() { "from the inspection service" } else { "from the notice board" };
            site.log(&format!("could not read the maintenance information {which}: {failure}"));
        }
        unread = failure.is_some();
        failed_rounds = if unread { failed_rounds.saturating_add(1) } else { 0 };
        let first = heard.planned.is_none() && heard.board.is_none();
        if let Some(Ok((sent, answer))) = asked_service {
            heard.planned = Some((Instant::now(), sent, notice::read_inspection(&answer)));
        }
        if let (Some(Ok(answer)), Some(said)) = (&asked_site, site_said) {
            let read = matches!(said, notice::Site::Board | notice::Site::OutOfSeason);
            if let Some(board) = board_on(&answer.text).filter(|_| read) {
                heard.board = Some((Instant::now(), board));
            }
            // What the answer shows of the site's block, in the maintenance underway.
            let underway = match following.standing {
                Standing::Underway(hours) => Some(hours),
                _ => None,
            };
            let at = clock.corrected(pc_now()).div_euclid(1000);
            let before = following.blocking.block;
            following.blocking = following.blocking.read(said, underway, at);
            if following.blocking.block != before {
                site.log(&block_line(following.blocking.block, at, answer));
            }
        }
        if first {
            let listed = heard.planned.as_ref().map(|(_, _, planned)| planned.len());
            let on_board = heard.board.as_ref().map(|(_, board)| board.notices.len());
            match (site.inspection().is_some(), listed, on_board) {
                (true, Some(listed), Some(on_board)) => site
                    .log(&format!("maintenance information read: {listed} listed by the service, {on_board} notices")),
                // The service does not list this client: its board is all there is to read.
                (false, _, Some(on_board)) => site.log(&format!("maintenance information read: {on_board} notices")),
                _ => {}
            }
        }

        held = hold_against_clock(app, site, &mut following, &heard, &clock);
        // When this round began, by Nexon's clock: the next one is counted from there.
        let read = clock.corrected(began);
        let to_end = held.ahead().ends.map(|ends| ends * 60 - read.div_euclid(1000));
        let waits = wait(to_end, held.block, failed_rounds);
        if (waits > pace(to_end, held.block)) != slowed {
            slowed = waits > pace(to_end, held.block);
            let how = if slowed { "less often while it cannot be read" } else { "at the usual pace again" };
            site.log(&format!("asking for the maintenance information {how}"));
        }
        // Every copy of the app waits a little longer by an amount of its own, so that
        // they do not all ask at the same moment.
        let own = (i64::try_from(own_delay(spread(waits))).unwrap_or(0) * 1000, own_millis(MOMENT_SPREAD));
        let mut due_at = due(read, held.ahead(), failed_rounds, own);
        while site.flow.lock().unwrap().askable() {
            let now = clock.corrected(pc_now());
            if now >= due_at {
                break;
            }
            // Held against the clock every second, and at the moment the next reading is due.
            std::thread::sleep(Duration::from_millis((due_at - now).clamp(1, 1000) as u64));
            let before = held.ahead();
            held = hold_against_clock(app, site, &mut following, &heard, &clock);
            // A maintenance that was listed has begun by the clock, or the one underway is
            // over. The next reading goes by what is ahead now, and comes no later for it.
            if held.ahead() != before {
                due_at = due_at.min(due(read, held.ahead(), failed_rounds, own));
            }
        }
    }
}

/// Holds what Nexon last said against the clock: sets what `site` knows of its game's
/// maintenance, and says so when a maintenance has begun, has been extended or is over.
/// Nexon is not asked. Says what the next reading goes by.
///
/// The clock is this PC's, set right by Nexon's (`clock`), and is held to the second
/// against the hours Nexon lists, which are whole minutes.
fn hold_against_clock(app: &AppHandle, site: &Site, following: &mut Following, heard: &Heard, clock: &Clock) -> Held {
    let planned = heard.planned.as_ref().filter(|(at, ..)| at.elapsed() < STANDS);
    let board = heard.board.as_ref().filter(|(at, _)| at.elapsed() < STANDS).map(|(_, board)| board);
    if planned.is_none() && board.is_none() {
        following.standing = Standing::Unknown;
        if std::mem::take(&mut *site.told.lock().unwrap()) != Told::default() {
            site.wake_desk();
        }
        return Held::default();
    }
    let seconds = clock.corrected(pc_now()).div_euclid(1000);
    let now = seconds.div_euclid(60);
    // When the service was asked for what it lists: a planned end that was read before
    // its time is a plan, and holds once the service was asked at that time or after.
    let listed = planned.map_or(seconds, |(_, sent, _)| clock.corrected(*sent).div_euclid(1000));
    let planned = planned.map(|(_, _, found)| found.as_slice()).unwrap_or_default();
    let notices = board.map(|board| board.notices.as_slice()).unwrap_or_default();
    following.watch.site_block(following.blocking.block);
    let reading = following.watch.read_as_of(planned, notices, seconds, listed);
    let in_season = !board.is_some_and(|board| board.out_of_season);
    let told = Told::at(&reading, now, in_season, site.inspection().is_some());
    let before = std::mem::replace(&mut *site.told.lock().unwrap(), told.clone());
    // A patch comes out during a maintenance. Whether this version of the app may still
    // patch is asked anew as one begins; see `watch_updates`.
    if told.begins_after(&before) {
        app.state::<Patcher>().update_due.store(true, Ordering::SeqCst);
    }
    if before != told {
        match &reading.underway {
            _ if !in_season => site.log("closed between two operating periods"),
            Some(closed) => {
                let until = notice::clock(closed.ends);
                site.log(&format!("maintenance underway until {until}: {}", closed.title));
            }
            None => site.log("no maintenance underway"),
        }
        // The desk shows the lamp, and asks the server by what it says.
        site.wake_desk();
    }
    let alert = app.state::<Patcher>().settings.lock().unwrap().maintenance_alert;
    if let Some(ends) = reading.extended {
        site.log(&format!("maintenance extended until {}", notice::shown(ends, now)));
        if alert {
            announce(app, site, &extended_until(ends, now));
        }
    }
    let underway = reading.underway.as_ref();
    let hours = underway.map(|closed| (closed.begins, closed.ends));
    let was = following.standing;
    let (stands, word) = was.after(Look { underway: hours, ended: reading.ended, now });
    following.standing = stands;
    // The block on the site was there an hour past the planned end with nothing longer
    // listed: it is passed over from here on, and the clock holds again. Written once.
    let blocking = following.blocking;
    if let (notice::Block::There, Some((_, ends)), false) = (blocking.block, blocking.of, blocking.passed_over)
        && now >= ends + notice::BLOCK_HOLDS_FOR
    {
        site.log("the block on the site is still there an hour after the planned end: it is passed over");
        following.blocking.passed_over = true;
    }
    if word.over {
        // The three moments of an end, to draw the rule closer by after a maintenance.
        let planned_end = match was {
            Standing::Underway((_, ends)) => notice::clock(ends),
            _ => "unknown".to_owned(),
        };
        let block = match (blocking.block, blocking.gone_at) {
            (notice::Block::Unseen, _) => "not seen".to_owned(),
            (_, Some(gone_at)) => format!("gone at {}", to_the_second(gone_at)),
            (_, None) => "still there".to_owned(),
        };
        let at = to_the_second(seconds);
        site.log(&format!("maintenance over: said at {at}, planned end {planned_end}, block on the site {block}"));
    }
    if let Some(ends) = word.extended {
        site.log(&format!("maintenance goes on until {}: extended after it was over", notice::shown(ends, now)));
    }
    if word.begun {
        site.log("maintenance begun");
    }
    for said in said(word, alert, now) {
        announce(app, site, &said);
    }
    // The block is that of the maintenance it was seen in, and of no other.
    following.blocking = following.blocking.of(hours);
    Held {
        underway: underway.map(|closed| (closed.ends, closed.noticed)),
        next_start: notice::next_start(planned, notices, now),
        block: following.blocking.block,
        over: match following.standing {
            Standing::Over((_, ends)) => Some(ends),
            _ => None,
        },
    }
}

/// A number of seconds up to `most` that differs from one copy of the app and one moment to
/// the next.
pub(crate) fn own_delay(most: u64) -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (u64::from(now.subsec_nanos()) ^ u64::from(std::process::id()).wrapping_mul(2_654_435_761)) % (most + 1)
}

/// The same in milliseconds, for a reading at a moment that matters.
fn own_millis(most: u64) -> i64 {
    i64::try_from(own_delay(most)).unwrap_or(0)
}

/// Tells the user something about the maintenance of the game of `site`: a Windows
/// notification, the taskbar button flashing until they come back to the window, and one
/// sound. The sound is heard even while Windows holds notifications back, as it does during
/// a full-screen video or game.
fn announce(app: &AppHandle, site: &Site, text: &str) {
    notify(app, site, text);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Critical));
    }
    #[cfg(windows)]
    // SAFETY: the call takes a constant and plays one of the system's sounds.
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK;
        MessageBeep(MB_ICONASTERISK);
    }
}

#[cfg(test)]
mod tests {
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
        let (stands, word) = following.standing.after(Look { underway, ended: reading.ended, now });
        following.standing = stands;
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
        assert_eq!(held(&mut following, &listed(), &[], (end, end)), (true, vec![OVER.to_owned()]));
        assert_eq!(held(&mut following, &listed(), &[], (end + 1, end)), (true, nothing.clone()));

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

        // Nexon cannot be read as the end arrives: over by the clock after a minute.
        let mut following = Following::default();
        held(&mut following, &listed(), &[], (end - 1, end - 20));
        assert_eq!(held(&mut following, &listed(), &[], (end + 59, end - 20)), (false, nothing.clone()));
        assert_eq!(held(&mut following, &listed(), &[], (end + 60, end - 20)), (true, vec![OVER.to_owned()]));

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
        let notice =
            r#"<li><a href="/News/Notice/Inspection/150506"><span>[점검] 11/26(목) 정기점검 안내</span></a></li>"#;
        let board =
            board_on(&format!(r#"<html><div class="news_board"><ul>{notice}</ul></div></html>"#)).expect("a board");
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
        let check = notice::Notice {
            id: 195, title: "11/26(목) 전체 테스트월드 채널 점검 (15:50~16:10)".to_owned()
        };
        // 2026-11-26, the day in the notice, is 20783 days after the first of 1970.
        let now = |hour: i64, minute: i64| 20_783 * 1440 + hour * 60 + minute;
        let mut watch = notice::Watch::default();

        // Nothing is in the way as far as its board says. That is known, and not shown.
        let reading = watch.read(&[], std::slice::from_ref(&check), now(15, 0));
        let told = Told::at(&reading, now(15, 0), true, false);
        assert_eq!(told, Told { lamp: None, open: Some(true), underway: Some(false) });

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
        let none = (0, 0);
        // Nothing underway and nothing listed: at the usual pace, by this copy's delay later.
        assert_eq!(due(ms(start) - 3_600_000, Ahead::default(), 0, none), ms(start) - 3_000_000);
        assert_eq!(due(ms(start) - 3_600_000, Ahead::default(), 0, (42_000, 700)), ms(start) - 2_958_000);
        // One is listed to begin within the wait: read as it begins, by this copy's own
        // fraction of a second later.
        let listed = Ahead { next_start: Some(start), ..Ahead::default() };
        assert_eq!(due(ms(start) - 240_000, listed, 0, none), ms(start));
        assert_eq!(due(ms(start) - 240_000, listed, 0, (42_000, 700)), ms(start) + 700);
        // Underway: at the pace of where the last reading stood.
        let underway = Ahead { ends: Some(end), ..Ahead::default() };
        assert_eq!(due(ms(start), underway, 0, none), ms(start) + 30_000);
        assert_eq!(due(ms(end) - 600_000, underway, 0, none), ms(end) - 590_000);
        // The last reading before the end is followed by the one at the end itself.
        assert_eq!(due(ms(end) - 3_000, underway, 0, (1_000, 700)), ms(end) + 700);
        // That one made, the readings go on at the pace of an overdue maintenance.
        assert_eq!(due(ms(end) + 700, underway, 0, (1_000, 700)), ms(end) + 6_700);
        // Said to be over at its planned end: read at the nearer pace for a quarter of an
        // hour, where an extension that is posted late shows, and seldom again after that.
        let over = Ahead { over: Some(end), ..Ahead::default() };
        assert_eq!(due(ms(end) + 1_000, over, 0, none), ms(end) + 11_000);
        assert_eq!(due(ms(end) + 900_000, over, 0, none), ms(end) + 910_000);
        assert_eq!(due(ms(end) + 901_000, over, 0, none), ms(end) + 1_501_000);
        // Rounds that fail are set apart, and the reading at the end is made all the same.
        assert_eq!(due(ms(end) - 250_000, underway, 6, none), ms(end));
        assert_eq!(due(ms(end) - 250_000, underway, 2, none), ms(end) - 230_000);
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
            read = due(read, ahead, 0, (0, 0));
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
}
