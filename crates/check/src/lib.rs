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
//! - [`diagnose`] runs the lot. The program in `main.rs` is a console around it.
//!
//! The app can ask for a verdict without any of the rest: [`quick_verdict`] looks at the
//! drives and at what runs, prints nothing, and reads none of the game's files.
//!
//! Almost everything here only reads. The two things that write are in [`probe::write`];
//! one is an empty file, the other is only written when the user says yes, and a report
//! ends by saying what was written.

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
pub use diagnose::quick_verdict;
pub use verdict::{Kind, Outlook, Verdict, verdict};
