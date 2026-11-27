//! The watch on the game's maintenance: the server status on the screen, and the word that
//! a maintenance has begun, has been extended or is over.

#[cfg(test)]
mod tests;

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
    /// Whether the game can be played. Of a client Nexon's service does not list this is
    /// never known to be so, only that it is not: what the app does by itself when the
    /// game is open, it does not do on such a guess.
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
        // That the game can be played is known only where it can be seen. That it cannot
        // is known of any client: its board says so.
        let open = match (underway.is_none() && in_season, seen_into) {
            (true, true) => Some(true),
            (true, false) => None,
            (false, _) => Some(false),
        };
        Told { lamp, open, underway: Some(underway.is_some()) }
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
    /// When the maintenance that was last said to be over was said to be, in seconds of
    /// Nexon's clock: the moment the watch decided so, which is not its planned end where
    /// it ended early or ran over.
    over: Option<i64>,
}

impl Ahead {
    /// The seconds from `read`, a moment of Nexon's clock in milliseconds, to the planned
    /// end of the maintenance underway: below zero once it is overdue, and `None` while
    /// none is underway.
    fn to_end(self, read: i64) -> Option<i64> {
        self.ends.map(|ends| ends * 60 - read.div_euclid(1000))
    }
}

/// The seconds from one round of asking Nexon to the next that are in force, before this
/// copy's own delay is added: `read` is when the last round began, in milliseconds of
/// Nexon's clock, and `failed` how many rounds in a row have failed.
///
/// The rounds come at the pace of where the last one stood; see [`wait`]. A maintenance
/// that was said to be over may yet be extended: Nexon posts that late at times. For a
/// quarter of an hour after it was said to be over the readings stay at the nearer pace, so
/// that the game is not shown as open for long when it is not. That counts from the moment
/// it was said, whenever that was: one that ran over has its quarter of an hour as well,
/// and one that ended early has it then, and none at the end that was planned.
///
/// This is the one place that says which wait holds. Whatever goes by the wait, this copy's
/// own delay above all, goes by what this says.
fn in_force(read: i64, ahead: Ahead, failed: u32) -> i64 {
    let to_end = ahead.to_end(read);
    // Below zero for the round that was at work when it was said: that round began before.
    let since_over = ahead.over.filter(|_| to_end.is_none()).map(|over| read.div_euclid(1000) - over);
    match since_over {
        Some(since) if since <= CLOSE_FOR => NEAR,
        _ => wait(to_end, ahead.block, failed),
    }
}

/// When Nexon is read next, in milliseconds of Nexon's clock: `read` is when the last
/// round began and `failed` how many rounds in a row have failed. `own` draws this copy's
/// own delay for a round at the pace in force, a number of seconds up to the one it is
/// given, and `own_moment` is its own delay for a reading at a moment that matters, in
/// milliseconds.
///
/// The rounds come at the wait that is in force; see [`in_force`]. Every copy of the app
/// comes later than that by an amount of its own, up to a fifth of that very wait (see
/// [`spread`]). It is drawn here, for the wait that is decided here: a delay drawn for
/// another wait would put a reading at a close pace off by as much as one at a slow pace
/// may be. Before that, a reading is made at the moments at which something is due by the
/// hours Nexon lists: as the planned end arrives, where an extension shows or the end is
/// said, and as a maintenance that is listed begins.
fn due(read: i64, ahead: Ahead, failed: u32, own: &dyn Fn(u64) -> u64, own_moment: i64) -> i64 {
    let wait = in_force(read, ahead, failed);
    let own = i64::try_from(own(spread(wait))).unwrap_or(0);
    let at_the_pace = read + (wait + own) * 1000;
    let moments = [ahead.ends, ahead.next_start].into_iter().flatten().map(|at| at * 60_000 + own_moment);
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
    /// When a maintenance was last said to be over, in seconds of Nexon's clock.
    said_over: Option<i64>,
}

impl Following {
    /// Takes in where the watch stands after `look`, which was made at `seconds` of Nexon's
    /// clock, and hands on what the look gave to say; see [`Standing::after`]. When a
    /// maintenance is said to be over is kept: what follows an end counts from that moment.
    fn stand(&mut self, look: Look, seconds: i64) -> Word {
        let (stands, word) = self.standing.after(look);
        self.standing = stands;
        if word.over {
            self.said_over = Some(seconds);
        }
        word
    }

    /// When the maintenance that is over was said to be, for as long as that is where the
    /// watch stands: none is underway since, and nothing was lost sight of.
    fn over_since(&self) -> Option<i64> {
        self.said_over.filter(|_| matches!(self.standing, Standing::Over(_)))
    }
}

/// What a look found that the next reading goes by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Held {
    /// The maintenance underway: its planned end, and whether the board has a notice of it.
    underway: Option<(notice::Minutes, bool)>,
    /// When the next one that is listed begins.
    next_start: Option<notice::Minutes>,
    block: notice::Block,
    /// When the one that was last said to be over was said to be, in seconds of Nexon's
    /// clock; see [`Following::over_since`].
    over: Option<i64>,
}

impl Held {
    fn ahead(&self) -> Ahead {
        let ends = self.underway.map(|(ends, _)| ends);
        Ahead { ends, next_start: self.next_start, block: self.block, over: self.over }
    }
}

/// Sets `clock` by the time one of Nexon's servers states in an answer that has just
/// arrived (`date`, where it states one), to a question that was sent at `sent`.
fn set_by(clock: &mut Clock, sent: i64, date: Option<&str>) {
    if let Some(stated) = date.and_then(notice::stated_time) {
        clock.learn(sent, pc_now(), stated);
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
    set_by(clock, sent, answer.date.as_deref());
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
    set_by(clock, sent, answer.date.as_deref());
    Ok(answer)
}

/// Puts aside what `site` knew of its game's maintenance, and has its desk say so where
/// there was anything.
fn put_aside(site: &Site) {
    if std::mem::take(&mut *site.told.lock().unwrap()) != Told::default() {
        site.wake_desk();
    }
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
            put_aside(site);
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
        let asked_service = service
            .map(|(url, service)| dated(&mut clock, || download::soap(url, service.action, service.request, 1 << 20)));
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
        // The rounds are set further apart while they fail, where the wait in force is one
        // that failing lengthens.
        let slower = in_force(read, held.ahead(), failed_rounds) > in_force(read, held.ahead(), 0);
        if slower != slowed {
            slowed = slower;
            let how = if slowed { "less often while it cannot be read" } else { "at the usual pace again" };
            site.log(&format!("asking for the maintenance information {how}"));
        }
        // Every copy of the app waits a little longer by an amount of its own, so that
        // they do not all ask at the same moment. For a round at the pace in force it is
        // drawn where that wait is decided, in `due`, and for no other wait.
        let own_moment = own_millis(MOMENT_SPREAD);
        let mut due_at = due(read, held.ahead(), failed_rounds, &own_delay, own_moment);
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
                due_at = due_at.min(due(read, held.ahead(), failed_rounds, &own_delay, own_moment));
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
        put_aside(site);
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
    let word = following.stand(Look { underway: hours, ended: reading.ended, now }, seconds);
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
        over: following.over_since(),
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
