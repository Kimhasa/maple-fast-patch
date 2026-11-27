//! Nexon's clock: how far this PC's clock is off it, learned from the time Nexon's servers
//! state as they answer.
//!
//! The hours of a maintenance are Nexon's, and the clock of a PC is whatever it was last
//! set to: minutes off in a PC room, drifting at home. What the maintenance watch holds
//! against those hours is this PC's clock set right by Nexon's. Nothing that is written to
//! a file goes by it.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use mfp_kms::notice::{KOREA_AHEAD, Seconds};

/// A stated time that is further from this PC's clock than this is taken for a mistake:
/// with a clock a day off, no secure connection to Nexon comes about in the first place.
const ABSURD: i64 = 24 * 3600 * 1000;
/// An answer that took longer than this says too little of when it was written.
const SLOW: i64 = 5_000;
/// How many of the last answers the clock goes by.
const KEPT: usize = 5;
/// What one answer can tell: the server states whole seconds. A PC whose clock is no
/// further off than this is left as it is, since setting it by less would be setting it by
/// the noise.
const TELLS: i64 = 1_000;

/// How far Nexon's clock is ahead of this PC's, as the last answers show it.
#[derive(Debug, Default)]
pub(crate) struct Clock {
    /// By how many milliseconds each of them showed Nexon's clock to be ahead.
    ahead: VecDeque<i64>,
}

impl Clock {
    /// Takes in the time a server stated (whole seconds, Korean time) in an answer to a
    /// question that was sent at `sent` and had arrived at `received`, by this PC's clock
    /// in Korean milliseconds.
    ///
    /// The server wrote down the second it was in while the question was on its way there
    /// and the answer on its way back: on average half a second into it, in the middle of
    /// the two moments. That is good to about a second and half the time the answer took.
    pub(crate) fn learn(&mut self, sent: i64, received: i64, stated: Seconds) {
        let took = received - sent;
        let ahead = stated * 1000 + 500 - (sent + took / 2);
        if !(0..=SLOW).contains(&took) || ahead.abs() > ABSURD {
            return;
        }
        if self.ahead.len() == KEPT {
            self.ahead.pop_front();
        }
        self.ahead.push_back(ahead);
    }

    /// By how many milliseconds Nexon's clock is ahead of this PC's: nothing until an answer
    /// has shown it, and nothing while the two agree as far as an answer can tell.
    ///
    /// It is the middle one of the last answers, so that one answer with a wrong time moves
    /// nothing. Of two that is the one nearer to this PC's clock.
    fn ahead(&self) -> i64 {
        let mut shown: Vec<i64> = self.ahead.iter().copied().collect();
        shown.sort_unstable();
        let ahead = match shown.as_slice() {
            [] => 0,
            [only] => *only,
            [one, other] => *if one.abs() <= other.abs() { one } else { other },
            more => more[more.len() / 2],
        };
        if ahead.abs() <= TELLS { 0 } else { ahead }
    }

    /// The moment `pc` of this PC's clock, by Nexon's.
    pub(crate) fn corrected(&self, pc: i64) -> i64 {
        pc + self.ahead()
    }
}

/// Now by this PC's clock, in Korean time, in milliseconds since 1970.
pub(crate) fn pc_now() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX) + KOREA_AHEAD * 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moment of the tests' own, in Korean seconds.
    const NOON: i64 = 1_800_000_000;

    /// An answer that took 40 ms, to a PC whose clock is `fast` milliseconds ahead of
    /// Nexon's, when Nexon's clock stood `into` milliseconds into its second.
    fn answer(clock: &mut Clock, fast: i64, into: i64) {
        let sent = NOON * 1000 + into - 20 + fast;
        clock.learn(sent, sent + 40, NOON);
    }

    #[test]
    fn a_clock_that_is_minutes_off_is_set_right_by_what_nexon_states() {
        for fast in [120_000, -120_000, 7_000, -3_600_000] {
            let mut clock = Clock::default();
            // Until an answer has come, this PC's clock is all there is.
            assert_eq!(clock.corrected(NOON * 1000), NOON * 1000);
            for into in [0, 250, 500, 750, 999] {
                answer(&mut clock, fast, into);
            }
            // Good to the half second the stated time leaves open.
            let off = clock.corrected(NOON * 1000 + fast) - NOON * 1000;
            assert!(off.abs() <= 500, "{fast}: {off}");
        }
    }

    #[test]
    fn a_clock_that_agrees_as_far_as_an_answer_can_tell_is_left_alone() {
        for fast in [0, 300, -400] {
            let mut clock = Clock::default();
            for into in [0, 250, 500, 750, 999] {
                answer(&mut clock, fast, into);
            }
            assert_eq!(clock.ahead(), 0, "{fast}");
        }
    }

    #[test]
    fn one_answer_with_a_wrong_time_moves_nothing() {
        // Among answers that agree.
        let mut clock = Clock::default();
        for into in [100, 500, 900] {
            answer(&mut clock, 120_000, into);
        }
        let before = clock.ahead();
        clock.learn(NOON * 1000, NOON * 1000 + 40, NOON + 3_000);
        // The middle one is another of those that agree, at most.
        assert!((clock.ahead() - before).abs() <= 500, "{} after {before}", clock.ahead());
        // As the second answer there is: the one nearer to this PC's clock holds.
        let mut clock = Clock::default();
        answer(&mut clock, 0, 500);
        clock.learn(NOON * 1000, NOON * 1000 + 40, NOON + 3_000);
        assert_eq!(clock.ahead(), 0);
        // And it is gone once five answers have come after it.
        let mut clock = Clock::default();
        clock.learn(NOON * 1000, NOON * 1000 + 40, NOON + 3_000);
        assert_eq!(clock.ahead(), 3_000_480);
        for into in [100, 300, 500, 700, 900] {
            answer(&mut clock, 0, into);
        }
        assert_eq!(clock.ahead(), 0);
    }

    #[test]
    fn a_time_that_cannot_be_right_and_an_answer_that_took_too_long_are_not_gone_by() {
        let mut clock = Clock::default();
        // More than a day off.
        clock.learn(NOON * 1000, NOON * 1000 + 40, NOON + 2 * 86_400);
        clock.learn(NOON * 1000, NOON * 1000 + 40, NOON - 2 * 86_400);
        // Six seconds on its way, and one that came back before it was sent.
        clock.learn(NOON * 1000, NOON * 1000 + 6_000, NOON + 120);
        clock.learn(NOON * 1000, NOON * 1000 - 1, NOON + 120);
        assert_eq!(clock.ahead(), 0);
        assert!(clock.ahead.is_empty());
    }
}
