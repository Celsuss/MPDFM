//! Everything the event loop can be told, in one enum.
//!
//! The loop has exactly one input: a [`Msg`] off a channel. A keystroke, the
//! slow tick, a worker's progress, a signal — all the same shape, all handled by
//! [`App::update`][crate::tui::app::App::update]. That is what makes the loop
//! testable without a terminal: a test scripts a `Vec<Msg>` and asserts on what
//! the app drew, with no thread, no tty and no timing in it (`event::Events::scripted`).
//!
//! Two consequences worth keeping:
//!
//! - **Nothing blocks on the UI thread.** A long operation is a thread that
//!   sends [`Msg::ScanDone`] or [`Msg::TaskDone`] when it is finished, so the
//!   only thing the loop ever waits on is the channel.
//! - **Results travel whole.** `ScanDone` carries a `Result`, not an already
//!   formatted message, so the app decides what a failure looks like and the
//!   worker stays ignorant of the UI.
//!
//! The payloads are boxed where they are large. A `Msg` sits in a channel and is
//! moved several times; `Library` is a few hundred kilobytes for a real library,
//! and that is not a thing to copy through a `match`.

use std::time::Duration;

use crossterm::event::Event;
use mpdfm_core::library::{Library, ScanProgress};
use mpdfm_core::playlist::{IndexWarning, PlaylistIndex};

/// One thing that happened.
#[derive(Debug)]
pub enum Msg {
    /// A key, a resize, a paste, a mouse report — whatever the terminal sent.
    ///
    /// Resize arrives here too: `crossterm` turns `SIGWINCH` into
    /// [`Event::Resize`], so the loop needs no signal handling of its own for it.
    Input(Event),

    /// The slow tick, about once a second.
    ///
    /// It exists for the things that change without the user doing anything: the
    /// MPD indicator (task 26) and the expiry of a toast. It is deliberately slow
    /// — a tick is a wakeup, and a wakeup that redraws is a wakeup a user pays
    /// for in battery.
    Tick,

    /// A scan is running and has got this far.
    Progress(ScanProgress),

    /// A scan finished, for better or worse.
    ScanDone(Box<ScanOutcome>),

    /// What MPD said when it was last asked. Task 26 draws it; task 20 carries
    /// it, so that the thing which must never block the UI is on the channel from
    /// the start rather than retrofitted onto it.
    MpdStatus(Box<MpdSnapshot>),

    /// A worker that was not a scan finished. Tag reads for a window (task 22)
    /// and commits (task 24) arrive here.
    ///
    /// Handled but not yet sent, for the same reason [`TaskOutcome::Failed`] is:
    /// there is no worker but the scan yet. The expectation turns into a warning
    /// of its own the moment one exists, which is when this note should go.
    #[expect(
        dead_code,
        reason = "handled by the loop, sent by the first non-scan worker (task 22)"
    )]
    TaskDone(Box<TaskOutcome>),

    /// The process was asked to stop — `SIGTERM`, `SIGHUP`, or a `SIGINT` from
    /// outside. The loop leaves through the same path `q` takes, so the terminal
    /// is restored by the guard rather than by the kernel's default disposition,
    /// which restores nothing.
    Shutdown,
}

/// What a scan produced: the model, or the one error a scan can raise.
///
/// The playlist index is loaded by the same worker and travels with it, because
/// the two are read together and a browser holding one without the other can
/// answer nothing useful.
#[derive(Debug)]
pub struct ScanOutcome {
    /// The library, or why there is not one. A missing or unreadable
    /// `music_directory` is the only failure; everything the walk met below it is
    /// a [`ScanWarning`][mpdfm_core::library::ScanWarning] on the library.
    pub library: Result<Library, String>,
    /// The playlist index. `None` when the scan itself failed, since there was
    /// nothing to index against.
    pub index: Option<PlaylistIndex>,
    /// What loading the playlists complained about. Never fatal.
    pub playlist_warnings: Vec<IndexWarning>,
    /// How long the whole thing took, for the log and for the toast.
    pub elapsed: Duration,
}

/// What MPD was doing when it was last asked, or why it could not be asked.
#[derive(Debug, Clone)]
pub struct MpdSnapshot {
    /// `None` when MPD is unreachable or switched off.
    pub state: Option<MpdState>,
    /// Whether MPDFM is willing to talk to MPD at all —
    /// [`Config::mpd_enabled`][mpdfm_core::config::Config::mpd_enabled], which
    /// `--no-mpd` clears.
    ///
    /// Separate from `state` because "I did not ask" and "it did not answer" are
    /// different things to put in front of a user: the first is their own setting
    /// and the second is a daemon to go and look at.
    pub enabled: bool,
    /// The message to show when there is no state, already flattened to a string
    /// because the UI does nothing with the variant.
    pub problem: Option<String>,
}

/// The part of MPD's status the chrome shows.
#[derive(Debug, Clone)]
pub struct MpdState {
    /// Whether MPD is playing, paused or stopped.
    pub play_state: mpdfm_core::mpd::PlayState,
    /// The path of the current song, when there is one.
    pub song: Option<String>,
    /// Whether the daemon is updating its database.
    pub updating: bool,
}

/// What a worker that was not a scan produced.
///
/// One variant for now. The point of the type is that task 22's tag reads and
/// task 24's commits already have a way home that does not involve adding a
/// variant to [`Msg`] and touching the loop.
#[derive(Debug)]
pub enum TaskOutcome {
    /// Something went wrong on a worker thread, with the full message.
    ///
    /// Handled — [`App::update`][crate::tui::app::App::update] opens the error
    /// panel on it — but not yet constructed; see [`Msg::TaskDone`].
    #[expect(
        dead_code,
        reason = "handled by the loop, constructed by the first non-scan worker (task 22)"
    )]
    Failed {
        /// What the worker was doing, in the user's words: "scan", "commit".
        what: String,
        /// The whole error chain. Never truncated here; the UI decides.
        message: String,
    },
}

impl Msg {
    /// A short name for the log, so a trace of a session reads as a list of
    /// events rather than of `Debug` dumps.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Input(_) => "input",
            Self::Tick => "tick",
            Self::Progress(_) => "progress",
            Self::ScanDone(_) => "scan-done",
            Self::MpdStatus(_) => "mpd-status",
            Self::TaskDone(_) => "task-done",
            Self::Shutdown => "shutdown",
        }
    }
}
