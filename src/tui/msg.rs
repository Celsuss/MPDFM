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
use mpdfm_core::journal::Reversed;
use mpdfm_core::library::{Library, ScanProgress};
use mpdfm_core::ops::commit::{self, Committed};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::{IndexWarning, PlaylistIndex};
use mpdfm_core::query::{FindProgress, Found};
use mpdfm_core::tags::{AudioInfo, TagSet};

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

    /// A worker that was not a scan finished. Tag reads for a window (task 22),
    /// the tag editor's selection and the transactions it commits (task 23) all
    /// arrive here.
    TaskDone(Box<TaskOutcome>),

    /// A library-wide search that is still running has got this far (task 25).
    ///
    /// Its own variant and not a [`TaskOutcome`] for the same reason
    /// [`Msg::Committing`] is: it says a worker is *not* finished, and the loop
    /// treats it as a line on screen and no change to anything a key acts on.
    Finding(FindProgress),

    /// A commit that is still running has got this far (task 24).
    ///
    /// Its own variant rather than something on [`TaskOutcome`], which is for
    /// answers: this is the one message that says a worker is *not* finished,
    /// and the loop treats it the way it treats [`Msg::Progress`] — a line on
    /// screen and no change to any state a key can act on.
    Committing(commit::Progress),

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
    /// MPD's **live** queue, in queue order, when the daemon answered and
    /// somebody asked for it.
    ///
    /// Only asked for while something is staged (see
    /// [`work::poll_mpd`][crate::tui::work::poll_mpd]), because it is the one
    /// thing in a poll whose cost is the user's queue rather than a constant,
    /// and nothing but a preview has any use for it.
    ///
    /// It decides whether a plan *rewrites* MPD's saved queue or merely warns
    /// about it ([`Live`][mpdfm_core::ops::Live]), so the preview and the commit
    /// must be given the same answer or the commit refuses as drift.
    pub queue: Option<Vec<RelPath>>,
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
/// The point of the type is that task 22's tag reads and the transactions tasks
/// 23 and 24 commit have a way home that does not involve adding a variant to
/// [`Msg`] and touching the loop.
#[derive(Debug)]
pub enum TaskOutcome {
    /// A window of files was read for the browser (task 22).
    ///
    /// See [`Reads`] for the shape, which is also the one a failure takes.
    Tags(Reads),

    /// Every file the tag editor was opened on was read (task 23).
    ///
    /// A whole selection and not a window, which is why it is not
    /// [`TaskOutcome::Tags`]: the editor cannot say `<multiple>` honestly until
    /// it has every file's tags, so this is up to a few hundred files read in one
    /// go — on a worker, with the form on screen saying what it is waiting for.
    Selection(Reads),

    /// A transaction was committed, or it was not.
    ///
    /// The thread that draws is never the thread that waits for a few hundred
    /// files to be rewritten; `Msg::Committing` is what arrives in the meantime.
    Committed(Result<Box<Committed>, NotCommitted>),

    /// A transaction was reversed, or could not be.
    Undone(Result<Box<Reversed>, String>),

    /// A library-wide search finished, or was called off part way (task 25).
    ///
    /// Boxed because it carries a [`TagSet`] per hit it had to open a file for:
    /// a `missing:genre` over the real library is 2 800 reads and some hundreds
    /// of hits, and that is not a thing to move through a `match` by value.
    Found(Box<FoundOutcome>),

    /// Something went wrong on a worker thread, with the full message.
    ///
    /// Not for a single file that would not read — that is an `Err` inside
    /// [`TaskOutcome::Tags`] and belongs next to the row it is about. This is for
    /// a worker that could not do its job at all.
    Failed {
        /// What the worker was doing, in the user's words: "scan", "commit".
        what: String,
        /// The whole error chain. Never truncated here; the UI decides.
        message: String,
    },
}

/// Why a commit produced no transaction.
///
/// Two outcomes and not one string, because they are different things to put in
/// front of somebody: one of them is their own decision and has nothing in it to
/// read, and the other is a failure whose message names the command that puts
/// the library back.
#[derive(Debug)]
pub enum NotCommitted {
    /// It was called off before the first mutation, so nothing was written: no
    /// backup, no record, nothing to recover.
    Cancelled,

    /// It was refused, or it stopped partway through. Core's whole message,
    /// which says which step stopped it and — when there is something on disk
    /// to put back — the `mpdfm recover <txid>` that does it.
    Failed(String),
}

/// What a library-wide search produced, and what it was looking for.
///
/// The query travels with the answer rather than being read back off the view
/// that asked: by the time this lands the user may have typed another one, and a
/// result set labelled with the wrong pattern is worse than one with no label.
#[derive(Debug)]
pub struct FoundOutcome {
    /// The query, as the user typed it.
    pub query: String,
    /// The hits, the failures, and whether it was called off.
    pub found: Found,
}

/// What a batch of tag reads produced: one entry per path asked for, in the
/// order they were asked for, each holding either what was read or the reason
/// that one file failed.
///
/// The same shape [`read_many`][mpdfm_core::tags::read_many] uses, and for the
/// same reason: one corrupt track in an album of fourteen must not take the other
/// thirteen's metadata away.
pub type Reads = Vec<(RelPath, Result<TrackInfo, String>)>;

/// What one tag read produced: the metadata, and the properties of the audio
/// itself.
///
/// Both, from one open, because the browser shows a duration and a bitrate next
/// to the title and reading the file twice to get them would double the I/O for
/// exactly the rows the user is looking at. See `docs/tasks/22-browser-view.md`
/// for the measurement that says this is affordable.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// What the file says it is.
    pub tags: TagSet,
    /// What the audio is: duration, bitrate, sample rate, real container.
    pub info: AudioInfo,
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
            Self::Finding(_) => "finding",
            Self::Committing(_) => "committing",
            Self::Shutdown => "shutdown",
        }
    }
}
