//! Patch engine: reads a MapleStory `.patch` file and applies it to an install folder.
//!
//! The patch format handling follows the WzComparerR2 patcher (MIT, see
//! `THIRD-PARTY-NOTICES.md`). This crate knows nothing about the UI; progress is reported
//! through a callback so the same code runs under the app, a CLI, or tests.
//!
//! A run never changes the install folder until every new file has been built and verified
//! in the work folder, and an apply that fails or is interrupted is undone. The one
//! exception is asked for by the player and taken only when the drive has no room for that:
//! see [`immediate`].

pub mod apply;
mod busy;
pub mod container;
pub mod crc;
pub mod detect;
pub mod download;
pub mod error;
pub mod immediate;
pub mod plan;
pub mod preflight;
pub mod run;
pub mod stage;
pub mod storage;
#[cfg(windows)]
mod wide;

pub use error::{Error, Result};
pub use run::{Event, Options, Outcome, Source, Stage, run_for};
