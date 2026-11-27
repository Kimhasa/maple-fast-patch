//! The question that closing the window on a run at work opens: what closing the window
//! does, and what the user's answer to the question comes to.
//!
//! Plain rules, like the rest of the flow: [`crate::window`] is what asks them, and what
//! then hides or closes the window.

use std::time::{Duration, Instant};

use super::{Flow, Phase};

/// What becomes of a run when the window is asked to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnClose {
    /// The window is put away, and whatever is at work goes on.
    Hide,
    /// The window stays, and the user is asked whether the run at work is to go on with the
    /// app next to the clock.
    Ask,
    /// The run at work is asked to stop first; the app goes when it has.
    StopThenLeave,
    /// The window closes, and the app with it.
    Leave,
}

/// How the app's one run stands when the window is asked to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtClose {
    /// No run is at work.
    Idle,
    /// A run is at work, and nothing was said of closing the window on it.
    AtWork,
    /// A run is at work, and the user is being asked what is to become of it: the window
    /// was asked to close this long ago, and they have not answered.
    Asked(Duration),
    /// The run at work was asked to stop and has not stopped yet.
    Stopping,
}

/// How long after the window was asked to close a second request is still the same press:
/// a button pressed twice in a row, or a mouse button that bounces. Nobody has read a
/// question in that time, and what comes then is no answer to it.
const SAME_PRESS: Duration = Duration::from_millis(500);

/// What closing the window does: `stays` says that the app stays at work next to the clock
/// when its window is closed, `run` how the run stands.
///
/// Closing during a run would cut the run off, and a window is closed without a thought:
/// the user is asked first whether the run is to go on, with the app next to the clock.
/// Nothing here waits for the answer, though, or a page that has stopped, which asks
/// nothing, would keep its window from ever closing. A window that is closed again over the
/// question is one whose run is to stop, as is one whose run is stopping already: the run
/// is asked to stop (an apply that has begun is finished), and the app goes when it has.
/// Closed again within a moment of the first time, it is the same press, and changes
/// nothing: the question stands as it was asked.
pub(crate) fn on_close(stays: bool, run: AtClose) -> OnClose {
    match (stays, run) {
        (true, _) => OnClose::Hide,
        (false, AtClose::Idle) => OnClose::Leave,
        (false, AtClose::AtWork) => OnClose::Ask,
        (false, AtClose::Asked(since)) if since < SAME_PRESS => OnClose::Ask,
        (false, AtClose::Asked(_) | AtClose::Stopping) => OnClose::StopThenLeave,
    }
}

/// What the user answers when asked whether the run is to go on; see [`OnClose::Ask`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CloseAnswer {
    /// It is: the window is put away, and the app is next to the clock for the time being.
    Background,
    /// It is not: the run is asked to stop, and the app goes when it has.
    Quit,
    /// Neither: the question was put aside. The window stays, and the run goes on.
    Stay,
}

impl CloseAnswer {
    /// What the answer comes to. `open` says that the question was still open: the run it
    /// was about is at work, and was not asked to stop.
    ///
    /// A run that has ended over the question goes on nowhere, and the window is not put
    /// away for it: it stays, with how the run ended on it. Leaving is leaving, whatever
    /// became of the run.
    pub(crate) fn given(self, open: bool) -> CloseAnswer {
        match self {
            CloseAnswer::Background if !open => CloseAnswer::Stay,
            answer => answer,
        }
    }
}

impl Flow {
    /// How the run stands at `now` for a window that is asked to close; see [`on_close`].
    fn at_close(&self, now: Instant) -> AtClose {
        match (&self.run, self.phase) {
            (Some(_), Phase::Stopping) => AtClose::Stopping,
            (Some(run), Phase::Starting | Phase::Running) => match run.asked_at.filter(|_| run.asking) {
                Some(asked_at) => AtClose::Asked(now.saturating_duration_since(asked_at)),
                None => AtClose::AtWork,
            },
            _ => AtClose::Idle,
        }
    }

    /// The window was asked to close at `now`. Says what that does, with `stays` as in
    /// [`on_close`], and takes in the question it opens: the page asks it for as long as
    /// the run says so. The question counts from when it was first asked.
    pub(crate) fn close(&mut self, stays: bool, now: Instant) -> OnClose {
        let does = on_close(stays, self.at_close(now));
        if let (OnClose::Ask, Some(run)) = (does, self.run.as_mut())
            && !run.asking
        {
            (run.asking, run.asked_at) = (true, Some(now));
        }
        does
    }

    /// The question that closing the window opened was answered or put aside. Says whether
    /// it was still open: a run that has ended, or was asked to stop, is not asked about.
    pub(crate) fn answered(&mut self) -> bool {
        self.run.as_mut().is_some_and(|run| {
            run.asked_at = None;
            std::mem::take(&mut run.asking)
        })
    }
}
