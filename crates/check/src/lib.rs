//! A diagnostic for any PC: says what kind of PC it is, as far as that can be told, and
//! whether a patch ahead of time is expected to work on it.
//!
//! The app was made for a PC at home. A PC room is another matter, and which one depends
//! on the room: a seat with a disk of its own takes a patch like a home PC does, a seat
//! with a program that puts its disk back at every start takes it and may lose it, and a
//! seat without a disk keeps what it writes in a hidden space that is small and is gone at
//! the next start. Nobody knows which of these a PC is by looking at the screen, so this
//! crate finds out what it can and says how sure it is.
//!
//! - [`facts`] is what was found, as plain values. [`probe`] fills them in by asking
//!   Windows and reading the install folder, [`server`] by asking Nexon's patch server.
//! - [`discover`] finds the install folder, [`known`] and [`compare`] tell which version it
//!   is at and which of its files are not Nexon's.
//! - [`mod@verdict`] holds the rules that turn the facts into a verdict, in one function
//!   without side effects.
//! - [`privacy`] and [`report`] turn it all into text that can be passed on: nothing in a
//!   report names the PC or the person at it.
//! - [`diagnose`] runs the lot, as calls an app makes: [`Diagnosis::start`] for the part
//!   that asks nothing, [`Diagnosis::offer`] for what a further step costs and the words
//!   to ask for it with, [`Diagnosis::run`] for the step. The program in `main.rs` is a
//!   console that makes the same calls.
//!
//! The app can also ask for a verdict without any of the rest: [`quick_verdict`] looks at
//! the drives and at what runs, prints nothing, and reads none of the game's files.
//!
//! A run asks nothing and reads little until it has a verdict: about ten seconds on a
//! seat that reads its game over the network. Whatever reads much or writes is asked for
//! by itself, with what it costs on the PC at hand.
//!
//! Almost everything here only reads. The three things that write are in
//! [`probe::write`]: one is an empty file, the other two are only written when the user
//! says yes, and a report ends by saying what was written.

pub mod compare;
pub mod discover;
pub mod facts;
pub mod known;
pub mod privacy;
pub mod report;
pub mod server;
pub mod units;
pub mod verdict;

#[cfg(windows)]
pub mod deliver;
#[cfg(windows)]
pub mod diagnose;
#[cfg(windows)]
pub mod probe;
#[cfg(windows)]
mod wide;

#[cfg(windows)]
pub use diagnose::{Diagnosis, Extra, Offer, OnShelf, Progress, Refused, Request, Sizes, Stage, Target, quick_verdict};
pub use facts::Room;
pub use report::Head;
pub use verdict::{Kind, Outlook, Plain, Verdict, verdict};

#[cfg(test)]
mod tests {
    /// What the diagnostic does is called `검사` wherever a person reads of it: in the
    /// report, in the questions, in what the console program says. The word it was called
    /// by before must not come back in any text of this crate, the console program's and
    /// the tests' included. Rust names and these comments are English and do not hold it.
    #[test]
    fn nothing_a_person_reads_uses_the_word_the_check_was_called_by_before() {
        // Put together here, so that this file does not hold the word either.
        let before = ["진", "단"].concat();
        let sources = [
            ("compare.rs", include_str!("compare.rs")),
            ("deliver.rs", include_str!("deliver.rs")),
            ("diagnose.rs", include_str!("diagnose.rs")),
            ("discover.rs", include_str!("discover.rs")),
            ("facts.rs", include_str!("facts.rs")),
            ("known.rs", include_str!("known.rs")),
            ("lib.rs", include_str!("lib.rs")),
            ("main.rs", include_str!("main.rs")),
            ("privacy.rs", include_str!("privacy.rs")),
            ("probe/game.rs", include_str!("probe/game.rs")),
            ("probe/mod.rs", include_str!("probe/mod.rs")),
            ("probe/programs.rs", include_str!("probe/programs.rs")),
            ("probe/read.rs", include_str!("probe/read.rs")),
            ("probe/registry.rs", include_str!("probe/registry.rs")),
            ("probe/system.rs", include_str!("probe/system.rs")),
            ("probe/volume.rs", include_str!("probe/volume.rs")),
            ("probe/write.rs", include_str!("probe/write.rs")),
            ("report.rs", include_str!("report.rs")),
            ("server.rs", include_str!("server.rs")),
            ("units.rs", include_str!("units.rs")),
            ("verdict.rs", include_str!("verdict.rs")),
            ("wide.rs", include_str!("wide.rs")),
        ];
        for (name, text) in sources {
            let line = text.lines().position(|line| line.contains(&before));
            assert_eq!(line, None, "{name} holds the old word at line {}", line.map_or(0, |at| at + 1));
        }
        // And the report says the new one where it names itself.
        assert!(include_str!("report.rs").contains("{APP_NAME} 검사 {}"));
    }
}
