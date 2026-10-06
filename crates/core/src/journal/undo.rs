//! Reversing a transaction that finished — with verification first, and a record
//! of the reversal afterwards.
//!
//! ```no_run
//! use mpdfm_core::journal::{Store, undo};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (config, _warnings) = mpdfm_core::config::resolve(&Default::default(),
//!                                                       &mpdfm_core::config::Env::from_process());
//! let store = Store::at(&config.data_dir);
//!
//! // `mpdfm undo --list`
//! print!("{}", undo::list(&store)?);
//!
//! // `mpdfm undo`
//! let record = undo::latest(&store)?;
//! let check = undo::check(&record, &config)?;
//! print!("{}", check.render());          // and the user says yes
//! if check.is_clear() {
//!     let reversed = undo::undo(&store, &record, &config, &undo::Options::default())?;
//!     println!("{}", reversed.headline());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # The order, which is the commit's read backwards
//!
//! ```text
//! 1  load the record, refuse what cannot be reversed    nothing mutated
//! 2  check every precondition and report all of them    nothing mutated
//! 3  take this undo's own backups, write its record     nothing mutated
//!    as pending, fsync
//! 4  restore the playlists and the state file from      the playlists change
//!    the transaction's backup directory
//! 5  reverse every completed step, last one first,      the library changes
//!    journaling each reversal as it happens
//! 6  the undo's record: complete. The transaction's:    nothing mutated
//!    reverted, naming the undo that did it
//! 7  ask MPD to rescan                                  a warning at worst
//! ```
//!
//! Steps 4 and 5 swap over when the undo is a *replay* — re-applying a change
//! is a commit read forwards, and a commit writes the playlists last. Which one
//! it is comes from the record, never from the caller: see below.
//!
//! Step 3 is not symmetry for its own sake. An undo is a mutation like any
//! other, so safety invariant 2 applies to it too: its record is durable before
//! it touches anything, which is what lets [`recover`][super::recover] finish or
//! reverse an undo that a crash interrupted.
//!
//! # Verification, not faith
//!
//! `docs/PLAN.md` safety invariant 9: *every committed transaction is undoable,
//! and `undo` verifies preconditions before acting rather than blindly
//! reversing*. Before anything moves, every completed step's destination is
//! compared against the [`Facts`] its receipt recorded — size, mtime, and the
//! hash when `--verify` put one there. A file somebody has written to since the
//! commit is **not** moved back: the undo stops, names every such file at once,
//! and offers `--force`, which skips exactly those steps and reports them.
//!
//! Stopping on the *first* changed file would be the cheaper implementation and
//! a worse one — a user who is told about all four at once looks at them once.
//!
//! # Why a full file restore for the playlists
//!
//! The steps are reversed from their receipts; the playlists are not. They are
//! restored wholesale from the copies the commit took, which cannot drift out of
//! step with a line-arithmetic bug and does not care how far through the writes
//! the transaction got. The cost is that an *unrelated* edit someone made to an
//! affected playlist since the commit is overwritten — so that is detected
//! ([`Trouble::PlaylistChanged`]) and reported before anything is written, and
//! the file's current bytes are copied into the undo's own backup directory
//! first, so `--force` loses nothing permanently.
//!
//! # Undo is itself undoable, which is why there is no `redo`
//!
//! Step 6 writes a new record whose [`Direction`] is `reverse`: its steps are
//! work that was *taken back*. Undoing that record therefore executes those
//! steps forward again — [`Action::Replay`] — and writes another record, this
//! one `forward`. The direction flips every time, so `undo` alternates between
//! taking a change back and putting it back, and a separate `redo` command would
//! be a second way to spell the same thing.

use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};

use crate::config::Config;
use crate::mpd::state;
use crate::ops::commit::{self, Updater};
use crate::ops::exec_fs::{self, Done, Facts, FsError, FsStep, FsWarning, Method, StepReceipt};
use crate::playlist::rewrite;
use crate::{Error, Result};

use super::JournalError;
use super::record::{Direction, Record, Status, StepRecord, TxId};
use super::store::Store;

/// What an undo is allowed to do beyond reversing what is safe.
///
/// [`Default`] is the cautious answer: stop on anything that has changed, and
/// tell MPD nothing.
#[derive(Default)]
pub struct Options<'a> {
    /// `--force`: reverse everything that is still safe to reverse, skipping the
    /// steps and overwriting the playlists the check complained about — and
    /// reporting every one of them.
    pub force: bool,

    /// How to ask MPD to rescan, as [`commit::Options::update`]. `None` means
    /// there is nothing to ask with, which is not a failure.
    pub update: Option<Updater<'a>>,
}

impl std::fmt::Debug for Options<'_> {
    /// Hand-written because a function pointer has no useful `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("force", &self.force)
            .field("update", &self.update.map(|_| "<fn>"))
            .finish()
    }
}

/// Which way undoing a record goes.
///
/// Read off [`Record::direction`], never chosen: a record whose steps were
/// executed is undone by reversing them, and a record whose steps were reversed
/// — an undo's own — is undone by executing them again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Reverse every completed step, last one first.
    Reverse,
    /// Execute every reversed step again, first one first. What undoing an undo
    /// does, and the whole of MPDFM's redo.
    Replay,
}

impl Action {
    /// What undoing a record of this direction does to the steps it reached.
    #[must_use]
    pub fn of(direction: Direction) -> Self {
        match direction {
            Direction::Forward => Self::Reverse,
            Direction::Reverse => Self::Replay,
        }
    }

    /// What *finishing* a record of this direction does to the steps it has not
    /// reached — [`recover::roll_forward`][super::recover::roll_forward]'s half
    /// of the same pair. A commit that stopped halfway carries on executing; an
    /// undo that stopped halfway carries on reversing.
    #[must_use]
    pub fn continuing(direction: Direction) -> Self {
        match direction {
            Direction::Forward => Self::Replay,
            Direction::Reverse => Self::Reverse,
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Reverse => "reverse",
            Self::Replay => "re-apply",
        })
    }
}

// ---------------------------------------------------------------------------
// The report.

/// What undoing a transaction would run into, worked out without writing
/// anything.
///
/// This is what a caller shows the user before saying yes, and what [`undo`]
/// refuses on. [`Check::problems`] stops the undo unless
/// [`Options::force`]; [`Check::warnings`] never does.
#[derive(Debug, Clone)]
pub struct Check {
    /// The transaction this is about.
    pub txid: TxId,
    /// What undoing it would do to its steps.
    pub action: Action,
    /// How many steps that is.
    pub steps: usize,
    /// Everything that is not as the record left it. Each one names a file.
    pub problems: Vec<Problem>,
    /// Everything worth mentioning that would not stop the undo.
    pub warnings: Vec<UndoWarning>,
}

impl Check {
    /// Whether the undo can go ahead without `--force`.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.problems.is_empty()
    }

    /// The problems that name a step, as positions into [`Record::steps`].
    fn skips(&self) -> Vec<usize> {
        self.problems.iter().filter_map(|p| p.step).collect()
    }

    /// The report, as the CLI and the TUI print it. No trailing newline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = vec![format!(
            "UNDO {}  {} {} step(s)",
            self.txid,
            match self.action {
                Action::Reverse => "reverses",
                Action::Replay => "re-applies",
            },
            self.steps,
        )];
        if !self.problems.is_empty() {
            out.push(String::new());
            out.push(format!(
                "Changed since the transaction ({})",
                self.problems.len()
            ));
            out.extend(self.problems.iter().map(|problem| format!("  x {problem}")));
            out.push(String::new());
            out.push(
                "Nothing has been changed. Re-run with --force to undo everything \
                 else and skip these."
                    .to_owned(),
            );
        }
        if !self.warnings.is_empty() {
            out.push(String::new());
            out.push(format!("Warnings ({})", self.warnings.len()));
            out.extend(self.warnings.iter().map(|warning| format!("  ! {warning}")));
        }
        out.join("\n")
    }
}

impl std::fmt::Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// One thing that is no longer as the record left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The file or directory it is about.
    pub at: Utf8PathBuf,
    /// What kind of trouble it is.
    pub what: Trouble,
    /// What exactly differs, as a phrase.
    pub detail: String,
    /// Which step it blocks, as a position into [`Record::steps`]. `None` for a
    /// playlist, which belongs to no single step.
    pub step: Option<usize>,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.at, self.detail)
    }
}

/// The kinds of trouble [`check`] can find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trouble {
    /// The file is no longer the file the receipt describes — someone has
    /// written to it since the commit.
    Modified,
    /// It is not there at all, and neither is the path it would go back to.
    Missing,
    /// Where it would go back to is occupied by something else.
    Occupied,
    /// A playlist has changed since the transaction, so restoring the backup
    /// over it would throw that change away.
    PlaylistChanged,
    /// It could not be looked at.
    Unreadable,
}

impl std::fmt::Display for Trouble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Modified => "modified",
            Self::Missing => "missing",
            Self::Occupied => "occupied",
            Self::PlaylistChanged => "playlist changed",
            Self::Unreadable => "unreadable",
        })
    }
}

/// Something an undo should mention, which did not stop it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UndoWarning {
    /// A step that is already back where it started: its destination is gone and
    /// its source is there. Skipped without being asked, because there is
    /// nothing left to reverse — which is what a second `undo` after one that
    /// failed halfway finds.
    #[error("step {step} is already back ({detail})")]
    AlreadyBack {
        /// The step's position, counting from one.
        step: usize,
        /// What was found.
        detail: String,
    },

    /// A step `--force` left alone.
    #[error("step {step} was skipped: {problem}")]
    Skipped {
        /// The step's position, counting from one.
        step: usize,
        /// Why.
        problem: Problem,
    },

    /// A directory the transaction created could not go away, because something
    /// that is not the transaction's is in it.
    #[error("{at} was left in place: it holds {}", holds.join(", "))]
    DirKept {
        /// The directory.
        at: Utf8PathBuf,
        /// What is in it.
        holds: Vec<String>,
    },

    /// A playlist was restored over contents that had changed since the
    /// transaction. The bytes that were there are in the undo's own backup
    /// directory.
    #[error(
        "{file_name} had changed since the transaction; its contents before this undo are in {kept}"
    )]
    PlaylistOverwritten {
        /// The playlist, as the playlist directory spells it.
        file_name: String,
        /// Where the overwritten bytes are.
        kept: Utf8PathBuf,
    },

    /// The configured music directory is not the one the transaction ran
    /// against. The record's own root is used, never the configuration's — a
    /// record outlives a config file.
    #[error("{recorded} is the root this transaction ran against, not the configured {configured}")]
    DifferentRoot {
        /// What the record says.
        recorded: Utf8PathBuf,
        /// What the configuration says now.
        configured: Utf8PathBuf,
    },

    /// A note from one of the filesystem steps.
    #[error("{0}")]
    Fs(FsWarning),

    /// MPD could not be told to rescan.
    #[error("MPD was not told to rescan ({0}); run `mpc update` when it is back")]
    Mpd(String),

    /// The MPD state file could not be put back, though the record says the
    /// transaction changed it.
    #[error("MPD's saved queue was not restored: {0}")]
    State(String),

    /// Retention could not run afterwards.
    #[error("old backups were not pruned: {0}")]
    Pruning(String),
}

/// Why a transaction was not reversed.
#[derive(Debug, thiserror::Error)]
pub enum UndoError {
    /// There is nothing in the journal that can be undone.
    #[error("there is no completed transaction to undo")]
    Nothing,

    /// The transaction never finished, so this is `recover`'s job and not
    /// undo's: rolling back a half-finished transaction has to look at the disk
    /// to work out what actually happened.
    #[error(
        "transaction {txid} is {status}, not complete — run `mpdfm recover {txid}` \
         to roll it back or finish it"
    )]
    Unfinished {
        /// The transaction.
        txid: TxId,
        /// What it says instead.
        status: Status,
    },

    /// It has already been undone. Undoing it twice would reverse a reversal
    /// that is not there.
    #[error(
        "transaction {txid} has already been undone{}; undo that one to put the \
         change back",
        .by.as_ref().map(|by| format!(" by {by}")).unwrap_or_default()
    )]
    AlreadyReverted {
        /// The transaction.
        txid: TxId,
        /// The undo that did it, when the record names one.
        by: Option<TxId>,
    },

    /// Its backups are not there, so the playlists cannot be put back and the
    /// bytes of anything it deleted are gone. Retention is the usual reason.
    #[error(
        "transaction {txid} cannot be undone: {} ({dir} is not there), so its \
         playlists cannot be put back and the bytes of anything it deleted are gone",
        if *pruned {
            "its backups were pruned to keep the journal inside backup_keep"
        } else {
            "its backup directory has been removed"
        }
    )]
    BackupGone {
        /// The transaction.
        txid: TxId,
        /// Where its backups were.
        dir: Utf8PathBuf,
        /// Whether retention is what removed them.
        pruned: bool,
    },

    /// The library root the transaction ran against is not there any more.
    /// Undoing against a *different* root would move files nobody asked about.
    #[error(
        "transaction {txid} ran against {root}, which is not a directory now; \
         undoing it against anywhere else would move files nobody asked about"
    )]
    RootGone {
        /// The transaction.
        txid: TxId,
        /// The root its paths are relative to.
        root: Utf8PathBuf,
    },

    /// Something has changed since the commit. Nothing has been touched.
    #[error(
        "transaction {txid} was not undone: {} thing(s) have changed since it ran:\n{}\n\
         Re-run with --force to undo everything else and skip these.",
        .problems.len(),
        .problems.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n")
    )]
    Blocked {
        /// The transaction.
        txid: TxId,
        /// Everything that is not as the record left it.
        problems: Vec<Problem>,
    },

    /// A step is marked done with no receipt to reverse it with — a record
    /// written by a build with a bug in it. Refused rather than guessed at.
    #[error("transaction {txid} says `{step}` was done but records nothing about what it did")]
    NoReceipt {
        /// The transaction.
        txid: TxId,
        /// The step, rendered.
        step: String,
    },

    /// A step could not be put back. The undo stopped there; its own record says
    /// `failed` and lists what it did manage.
    #[error(
        "undo {txid} stopped at step {position} ({step}): {source}\n\
         Run `mpdfm recover {txid}` to see where it got to."
    )]
    Step {
        /// The undo's own transaction id, not the one being undone.
        txid: TxId,
        /// Which step, counting from one as the record lists them.
        position: usize,
        /// The step, rendered.
        step: String,
        /// What the filesystem said.
        #[source]
        source: Box<FsError>,
    },
}

/// A transaction that was put back, and the record that says so.
#[derive(Debug, Clone)]
pub struct Reversed {
    /// The undo's own transaction id — `mpdfm undo <this>` puts the change back.
    pub txid: TxId,
    /// What was undone.
    pub of: TxId,
    /// What was done to its steps.
    pub action: Action,
    /// How many steps were put back.
    pub steps: usize,
    /// The steps `--force` left alone, with the reason for each.
    pub skipped: Vec<Problem>,
    /// The undo's own record, `complete`.
    pub record: Record,
    /// Everything worth telling the user that did not stop it.
    pub warnings: Vec<UndoWarning>,
}

impl Reversed {
    /// One line for the end of `mpdfm undo`.
    #[must_use]
    pub fn headline(&self) -> String {
        let skipped = if self.skipped.is_empty() {
            String::new()
        } else {
            format!(", {} skipped", self.skipped.len())
        };
        format!(
            "{} {} ({} step(s){skipped}) — undo it with `mpdfm undo {}`",
            match self.action {
                Action::Reverse => "undid",
                Action::Replay => "re-applied",
            },
            self.of,
            self.steps,
            self.txid,
        )
    }
}

// ---------------------------------------------------------------------------
// Finding a transaction.

/// The most recent transaction that can be undone.
///
/// Newest first, skipping the ones that cannot: an already-reverted transaction,
/// one whose backups have been pruned, and one that never finished — which is
/// `recover`'s and would otherwise make `mpdfm undo` with no argument refuse
/// every time until it was dealt with.
///
/// # Errors
///
/// [`UndoError::Nothing`] when the journal holds no undoable transaction, and
/// [`Error::Journal`] if it cannot be read at all.
pub fn latest(store: &Store) -> Result<Record> {
    let (records, _unreadable) = store.records()?;
    records
        .into_iter()
        .find(Record::is_undoable)
        .ok_or_else(|| UndoError::Nothing.into())
}

/// Every transaction, newest first, with whether it can be undone.
///
/// `mpdfm undo --list`. A record that cannot be read is reported in
/// [`Listing::unreadable`] rather than hidden or raised: one bad record must not
/// make the other forty invisible.
///
/// # Errors
///
/// [`JournalError::Io`] if the journal directory cannot be listed.
pub fn list(store: &Store) -> std::result::Result<Listing, JournalError> {
    let (records, unreadable) = store.records()?;
    Ok(Listing {
        rows: records
            .iter()
            .map(|record| Row {
                txid: record.txid.clone(),
                started_at: record.started_at.clone(),
                status: record.status,
                direction: record.direction,
                summary: record.summary_phrase(),
                why_not: record.why_not_undoable(),
            })
            .collect(),
        unreadable: unreadable.iter().map(ToString::to_string).collect(),
    })
}

/// What [`list`] found.
#[derive(Debug, Clone)]
pub struct Listing {
    /// One row per readable record, newest first.
    pub rows: Vec<Row>,
    /// The records that could not be read, as their errors describe them.
    pub unreadable: Vec<String>,
}

/// One transaction, as `undo --list` shows it.
#[derive(Debug, Clone)]
pub struct Row {
    /// Its id.
    pub txid: TxId,
    /// When it started, as `2026-09-24T22:45:00Z`.
    pub started_at: String,
    /// Where it got to.
    pub status: Status,
    /// Whether its steps were executed or reversed.
    pub direction: Direction,
    /// What it did, in a few words.
    pub summary: String,
    /// Why it cannot be undone, when it cannot.
    pub why_not: Option<String>,
}

impl Row {
    /// Whether `mpdfm undo <txid>` would get anywhere.
    #[must_use]
    pub fn undoable(&self) -> bool {
        self.why_not.is_none()
    }
}

impl std::fmt::Display for Listing {
    /// The table, newest first, one transaction per line and a trailing
    /// newline — this one is the whole of a command's output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.rows.is_empty() && self.unreadable.is_empty() {
            return writeln!(f, "the journal is empty: nothing has been committed yet");
        }
        let widest = self
            .rows
            .iter()
            .map(|row| row.summary.chars().count())
            .max()
            .unwrap_or(0);
        for row in &self.rows {
            writeln!(
                f,
                "{}  {}  {:<width$}  {}",
                row.txid,
                row.started_at,
                row.summary,
                match &row.why_not {
                    None => "undoable".to_owned(),
                    Some(why) => format!("not undoable: {why}"),
                },
                width = widest,
            )?;
        }
        for problem in &self.unreadable {
            writeln!(f, "! {problem}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Step 1 and 2 — what cannot be undone, and what has changed.

/// Refuse what cannot be undone at all, and say what undoing it would do.
///
/// # Errors
///
/// [`UndoError::Unfinished`] for a transaction that never finished,
/// [`UndoError::AlreadyReverted`] for one that has already been undone,
/// [`UndoError::BackupGone`] when its backups are not there and
/// [`UndoError::RootGone`] when its library root is not.
fn gate(record: &Record) -> std::result::Result<Action, UndoError> {
    match record.status {
        Status::Complete => {}
        Status::Reverted => {
            return Err(UndoError::AlreadyReverted {
                txid: record.txid.clone(),
                by: record.undone_by.clone(),
            });
        }
        status => {
            return Err(UndoError::Unfinished {
                txid: record.txid.clone(),
                status,
            });
        }
    }
    backups_present(record)?;
    root_present(record)?;
    Ok(Action::of(record.direction))
}

/// Whether the backups the record names are still on disk.
///
/// Shared with [`recover`][super::recover], which needs exactly the same answer
/// about a transaction that never finished.
///
/// # Errors
///
/// [`UndoError::BackupGone`], saying whether retention is what removed them —
/// which is the difference between "this is how MPDFM is configured" and
/// "something removed a directory".
pub(crate) fn backups_present(record: &Record) -> std::result::Result<(), UndoError> {
    if record.backup_pruned || !record.backup_dir.is_dir() {
        return Err(UndoError::BackupGone {
            txid: record.txid.clone(),
            dir: record.backup_dir.clone(),
            pruned: record.backup_pruned,
        });
    }
    Ok(())
}

/// Whether the library root the record's paths are relative to is still there.
///
/// # Errors
///
/// [`UndoError::RootGone`].
pub(crate) fn root_present(record: &Record) -> std::result::Result<(), UndoError> {
    if record.music_dir.is_dir() {
        Ok(())
    } else {
        Err(UndoError::RootGone {
            txid: record.txid.clone(),
            root: record.music_dir.clone(),
        })
    }
}

/// Everything undoing this transaction would run into, without writing
/// anything.
///
/// # Errors
///
/// As [`gate`]: the refusals that no `--force` can get past.
pub fn check(record: &Record, config: &Config) -> std::result::Result<Check, UndoError> {
    let action = gate(record)?;
    Ok(inspect(record, config, action))
}

/// [`check`] without the gating: everything that has changed since the record
/// was written, for an `action` the caller has already settled on.
///
/// [`recover`][super::recover] needs exactly this and cannot use [`check`],
/// whose first act is to refuse a transaction that never finished.
pub(crate) fn inspect(record: &Record, config: &Config, action: Action) -> Check {
    let mut problems = Vec::new();
    let mut warnings = Vec::new();

    if config.music_dir != record.music_dir {
        warnings.push(UndoWarning::DifferentRoot {
            recorded: record.music_dir.clone(),
            configured: config.music_dir.clone(),
        });
    }

    let positions = positions(record, action, true);
    for &position in &positions {
        let step = &record.steps[position];
        match action {
            Action::Reverse => {
                inspect_reverse(record, step, position, &mut problems, &mut warnings)
            }
            Action::Replay => inspect_replay(record, step, position, config, &mut problems),
        }
    }
    problems.extend(playlist_problems(record));

    Check {
        txid: record.txid.clone(),
        action,
        steps: positions.len(),
        problems,
        warnings,
    }
}

/// The steps to act on, in the order they have to be acted on.
///
/// `reached` picks the side: the steps this record's own work got to — which is
/// what `done` means whichever direction the record is, since a reversal's done
/// step is a step it reversed — or the ones it did not, which is what finishing
/// it deals with.
///
/// Reversing goes innermost-last: a `RmDirIfEmpty` has to be undone *before*
/// the files that lived in that directory are moved back into it, or there is
/// nowhere to put them. Executing goes in the order the commit chose, for the
/// reasons it chose it.
pub(crate) fn positions(record: &Record, action: Action, reached: bool) -> Vec<usize> {
    let steps = record
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.done == reached)
        .map(|(position, _)| position);
    match action {
        Action::Reverse => steps.rev().collect(),
        Action::Replay => steps.collect(),
    }
}

/// Whether the destination of a completed step is still what its receipt says.
fn inspect_reverse(
    record: &Record,
    step: &StepRecord,
    position: usize,
    problems: &mut Vec<Problem>,
    warnings: &mut Vec<UndoWarning>,
) {
    let root = &record.music_dir;
    let Some(receipt) = &step.receipt else {
        // `undo` refuses the whole transaction for this; here it is one more
        // thing the report has to name.
        problems.push(Problem {
            at: root.clone(),
            what: Trouble::Unreadable,
            detail: format!("`{}` is marked done with no receipt", step.step),
            step: Some(position),
        });
        return;
    };

    // Where the step's work ended up, and where reversing it would put it back.
    let (now, back) = match (&step.step, &receipt.done) {
        (FsStep::RenameFile { from, to } | FsStep::CopyDelete { from, to }, Done::Moved { .. }) => {
            (to.to_abs(root), Some(from.to_abs(root)))
        }
        (
            FsStep::RemoveFile {
                target,
                backup: Some(backup),
            },
            Done::Removed { method, .. },
        ) => {
            if *method == Method::Unlink {
                problems.push(Problem {
                    at: target.to_abs(root),
                    what: Trouble::Missing,
                    detail: "it was removed with no backup, so its bytes are gone".to_owned(),
                    step: Some(position),
                });
                return;
            }
            (backup.clone(), Some(target.to_abs(root)))
        }
        (
            FsStep::RemoveFile {
                target,
                backup: None,
            },
            Done::Removed { .. },
        ) => {
            problems.push(Problem {
                at: target.to_abs(root),
                what: Trouble::Missing,
                detail: "it was removed with no backup, so its bytes are gone".to_owned(),
                step: Some(position),
            });
            return;
        }
        // A tag write stayed where it was, so there is nothing to put back *to*
        // — only the file itself to check. Its receipt's facts describe the file
        // as the write left it, which is what makes "somebody has edited this
        // track since" visible here.
        (
            FsStep::WriteTags {
                target,
                backup: Some(_),
                ..
            },
            Done::TagsWritten { .. },
        ) => (target.to_abs(root), None),
        (
            FsStep::WriteTags {
                target,
                backup: None,
                ..
            },
            Done::TagsWritten { .. },
        ) => {
            problems.push(Problem {
                at: target.to_abs(root),
                what: Trouble::Missing,
                detail: "its tags were written with no backup, so the originals are gone"
                    .to_owned(),
                step: Some(position),
            });
            return;
        }
        // A directory step has no contents to compare, and whether one can be
        // removed is not knowable yet: the steps reversed before it are what
        // empty it. `DirKept` is the answer, and the revert itself is the only
        // thing in a position to give it.
        (FsStep::MkDir { .. }, Done::DirsCreated { .. })
        | (FsStep::RmDirIfEmpty { .. }, Done::DirsRemoved { .. }) => return,
        _ => {
            problems.push(Problem {
                at: root.clone(),
                what: Trouble::Unreadable,
                detail: format!("`{}` does not match what it says it did", step.step),
                step: Some(position),
            });
            return;
        }
    };

    let facts = match &receipt.done {
        Done::Moved { facts, .. }
        | Done::Removed { facts, .. }
        | Done::TagsWritten { facts, .. } => facts,
        // Unreachable: the match above only produced a path for the other three.
        Done::DirsCreated { .. } | Done::DirsRemoved { .. } => return,
    };

    match Facts::of(&now) {
        Err(FsError::Missing { .. }) => {
            // An undo that failed halfway through leaves exactly this, and so
            // does a user who moved the file back by hand. There is nothing left
            // to reverse either way, so it is a note and not a refusal.
            if back.as_ref().is_some_and(|back| back.exists()) {
                warnings.push(UndoWarning::AlreadyBack {
                    step: position + 1,
                    detail: format!("{now} is gone and it is back where it came from"),
                });
            } else {
                problems.push(Problem {
                    at: now,
                    what: Trouble::Missing,
                    detail: "it is not there, and neither is the path it came from".to_owned(),
                    step: Some(position),
                });
            }
        }
        Err(err) => problems.push(Problem {
            at: now,
            what: Trouble::Unreadable,
            detail: err.to_string(),
            step: Some(position),
        }),
        Ok(current) => {
            if let Some(detail) = differences(facts, &current, &now) {
                problems.push(Problem {
                    at: now,
                    what: Trouble::Modified,
                    detail,
                    step: Some(position),
                });
            } else if let Some(back) = back.filter(|back| back.symlink_metadata().is_ok()) {
                problems.push(Problem {
                    at: back,
                    what: Trouble::Occupied,
                    detail: "something is already there, so it cannot be moved back".to_owned(),
                    step: Some(position),
                });
            }
        }
    }
}

/// Whether a reversed step can be executed again, which is the question
/// [`exec_fs::check`] already answers without writing anything.
fn inspect_replay(
    record: &Record,
    step: &StepRecord,
    position: usize,
    config: &Config,
    problems: &mut Vec<Problem>,
) {
    let options = exec_fs::Options::from_config(config);
    if let Err(err) = exec_fs::check(&step.step, &record.music_dir, &options) {
        problems.push(Problem {
            at: record.music_dir.clone(),
            what: match err {
                FsError::Exists { .. } => Trouble::Occupied,
                FsError::Missing { .. } => Trouble::Missing,
                _ => Trouble::Unreadable,
            },
            detail: format!("`{}` cannot be done again: {err}", step.step),
            step: Some(position),
        });
    }
}

/// How the entry at `path` differs from the facts a receipt recorded, or `None`
/// when it does not.
///
/// A directory is compared by existence alone: its size means nothing and its
/// mtime changes when anything inside it does, so a case-only directory rename
/// would otherwise refuse to come back the moment a track was added to it. Mode
/// bits are left out for a related reason — a `chmod` is not a change to the
/// file, and refusing to undo a move because of one would be theatre.
fn differences(recorded: &Facts, current: &Facts, path: &Utf8Path) -> Option<String> {
    if path.is_dir() {
        return None;
    }
    if recorded.size != current.size {
        return Some(format!(
            "it was {} bytes and is now {}",
            recorded.size, current.size
        ));
    }
    if let (Some(then), Some(now)) = (recorded.mtime, current.mtime)
        && then != now
    {
        return Some("its modification time is not the one the transaction left".to_owned());
    }
    // Only when `--verify` put one there: computing it costs a second read of
    // the file, and the size and mtime have already agreed.
    if let Some(expected) = recorded.hash {
        return match exec_fs::hash_file(path) {
            Ok(actual) if actual == expected => None,
            Ok(actual) => Some(format!(
                "its contents differ (hash {actual:#018x}, expected {expected:#018x})"
            )),
            Err(err) => Some(format!("its contents could not be read: {err}")),
        };
    }
    None
}

/// Every playlist that has changed since the transaction left it.
///
/// What the record's own backup holds is one side of the transaction and the
/// edits are the difference, so the question is whether the file still sits on
/// the side the record left it on. [`rewrite::after`] applies the edits, and
/// which way round it is applied depends on which way the record went:
///
/// - a commit applied them to the backup, so the file should hold the result;
/// - an undo put the backup's *source* back, so applying them to what is there
///   now has to give the backup's own bytes.
///
/// A file that answers neither question was edited by somebody else in between,
/// and restoring over it would throw that away. A file that is byte-identical to
/// the backup is never a problem: there is nothing for this undo to do to it.
fn playlist_problems(record: &Record) -> Vec<Problem> {
    let mut problems = Vec::new();
    for edit in &record.playlist_edits {
        let before = match std::fs::read(record.backup_dir.join(&edit.file_name)) {
            Ok(bytes) => bytes,
            Err(source) => {
                problems.push(Problem {
                    at: record.backup_dir.join(&edit.file_name),
                    what: Trouble::Missing,
                    detail: format!("the backup of {} cannot be read: {source}", edit.file_name),
                    step: None,
                });
                continue;
            }
        };
        let current = match std::fs::read(&edit.real_path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                problems.push(Problem {
                    at: edit.real_path.clone(),
                    what: Trouble::Missing,
                    detail: "the playlist is no longer there".to_owned(),
                    step: None,
                });
                continue;
            }
            Err(source) => {
                problems.push(Problem {
                    at: edit.real_path.clone(),
                    what: Trouble::Unreadable,
                    detail: source.to_string(),
                    step: None,
                });
                continue;
            }
        };
        if current == before {
            continue;
        }
        let answer = match record.direction {
            Direction::Forward => rewrite::after(&before, edit).map(|after| after == current),
            Direction::Reverse => rewrite::after(&current, edit).map(|after| after == before),
        };
        match answer {
            Ok(true) => {}
            Ok(false) | Err(Error::Rewrite(_)) => problems.push(Problem {
                at: edit.real_path.clone(),
                what: Trouble::PlaylistChanged,
                detail: format!(
                    "{} has changed since the transaction; restoring the backup \
                     would lose that",
                    edit.file_name
                ),
                step: None,
            }),
            Err(err) => problems.push(Problem {
                at: edit.real_path.clone(),
                what: Trouble::Unreadable,
                detail: format!("what the transaction left here cannot be worked out: {err}"),
                step: None,
            }),
        }
    }
    problems
}

// ---------------------------------------------------------------------------
// Steps 3 to 7 — the undo itself.

/// Reverse a completed transaction, or re-apply one that was reversed.
///
/// See the [module docs][self] for the sequence and what each step leaves behind.
/// `record` is what [`Store::load`] gave; it is written back as
/// [`Status::Reverted`] at the end, and a new record is written for the undo
/// itself.
///
/// # Errors
///
/// [`Error::Undo`] for a transaction that cannot be undone ([`UndoError`]), or
/// one that is blocked by something that has changed since — unless
/// [`Options::force`]; [`Error::Journal`] if the undo's own record cannot be made
/// durable; [`Error::Io`] or [`Error::Rewrite`] if a playlist cannot be backed up
/// or restored.
pub fn undo(
    store: &Store,
    record: &Record,
    config: &Config,
    options: &Options<'_>,
) -> Result<Reversed> {
    let check = check(record, config)?;
    if !check.is_clear() && !options.force {
        return Err(UndoError::Blocked {
            txid: record.txid.clone(),
            problems: check.problems,
        }
        .into());
    }
    put_back(store, record, config, &check, options)
}

/// The mutating half of [`undo`], which [`recover`][super::recover] rolls a
/// half-finished transaction back with.
///
/// `check` decides what is skipped, so a caller that means to skip nothing
/// passes one with no problems in it.
///
/// # Errors
///
/// As [`undo`], without the gating [`check`] has already done.
pub(crate) fn put_back(
    store: &Store,
    record: &Record,
    config: &Config,
    check: &Check,
    options: &Options<'_>,
) -> Result<Reversed> {
    let root = record.music_dir.clone();
    let action = check.action;
    let skips = check.skips();
    let mut warnings = check.warnings.clone();

    // Step 3 — this undo's own backups and its own record, before anything moves.
    let txid = TxId::now();
    store.create_dirs()?;
    let backup_dir = store.create_backup_dir(&txid)?;
    let taken = rewrite::back_up_now(&record.playlist_edits, &backup_dir)?;
    let state_backup = back_up_state_file(record, config, &backup_dir)?;

    let mut mine = Record::opening(
        txid.clone(),
        SystemTime::now(),
        root.clone(),
        record.playlist_dir.clone(),
        backup_dir.clone(),
    );
    mine.direction = record.direction.flipped();
    mine.undo_of = Some(record.txid.clone());
    mine.ops = record.ops.clone();
    mine.steps = record
        .steps
        .iter()
        .map(|step| StepRecord::planned(step.step.clone()))
        .collect();
    mine.playlist_edits = taken
        .iter()
        .map(|position| record.playlist_edits[*position].clone())
        .collect();
    mine.state_edits = record.state_edits.clone();
    mine.state_backup = state_backup;
    mine.summary = record.summary;
    let mut skipped = Vec::new();
    for problem in &check.problems {
        if let Some(position) = problem.step {
            mine.steps[position].skipped(problem);
            skipped.push(problem.clone());
            warnings.push(UndoWarning::Skipped {
                step: position + 1,
                problem: problem.clone(),
            });
        }
    }
    store.write(&mine)?;

    // Steps 4 and 5, in the order that mirrors the commit's: the playlists were
    // written last, so a reversal puts them back first, and a replay writes them
    // last.
    let acting: Vec<usize> = positions(record, action, true)
        .into_iter()
        .filter(|position| !skips.contains(position))
        .collect();
    let pass = Pass {
        store,
        steps: record.steps.clone(),
        action,
        root: root.clone(),
        fs: exec_fs::Options::from_config(config),
    };

    if action == Action::Reverse {
        restore_playlists(record, config, &check.problems, &backup_dir, &mut warnings)?;
    }
    let steps = match pass.run(&mut mine, &acting, &mut warnings) {
        Ok(steps) => steps,
        Err(err) => {
            mine.finish(Status::Failed, SystemTime::now());
            store.write(&mine)?;
            store.forget_steps(&txid);
            return Err(err);
        }
    };
    if action == Action::Replay {
        restore_playlists(record, config, &check.problems, &backup_dir, &mut warnings)?;
    }

    // Step 6 — the transaction first, then the undo. A crash in between leaves a
    // `pending` undo whose log says every step is done, which `recover` finishes
    // by writing one record; the other order would leave a transaction that
    // claims to be in effect when it is not.
    let mut target = record.clone();
    target.status = Status::Reverted;
    target.undone_by = Some(txid.clone());
    store.write(&target)?;

    mine.finish(Status::Complete, SystemTime::now());
    store.write(&mine)?;
    store.forget_steps(&txid);

    // Step 7 — MPD, which cannot fail an undo that has already finished.
    tell_mpd(&mut mine, record, &acting, config, options, &mut warnings);
    if let Err(err) = store.write(&mine) {
        warnings.push(UndoWarning::Mpd(err.to_string()));
    }
    // Retention, which is housekeeping and never fails an undo. The record this
    // undo just wrote is the newest in the journal, so its own backups are the
    // last thing that would ever be pruned — which is what keeps the redo
    // possible.
    if let Err(err) = store.prune(config.backup_keep) {
        warnings.push(UndoWarning::Pruning(err.to_string()));
    }

    Ok(Reversed {
        txid,
        of: record.txid.clone(),
        action,
        steps,
        skipped,
        record: mine,
        warnings,
    })
}

/// One pass over a record's steps, in one direction.
///
/// A struct rather than eight arguments, and the one place either direction's
/// work actually happens: [`undo`] and [`recover`][super::recover] both end up
/// here.
pub(crate) struct Pass<'a> {
    /// The journal, for making each step durable as it happens.
    pub store: &'a Store,
    /// The steps, with the receipts a reversal needs.
    pub steps: Vec<StepRecord>,
    /// Which way to go.
    pub action: Action,
    /// The library root the steps are relative to — the record's, never the
    /// configuration's.
    pub root: Utf8PathBuf,
    /// What a replayed step is allowed to do.
    pub fs: exec_fs::Options,
}

impl Pass<'_> {
    /// Act on each position in turn, journaling into `journal` as each one
    /// lands.
    ///
    /// `journal` is the record being *written* — the undo's own — and its steps
    /// are in the same order as [`Pass::steps`], so one position names both.
    ///
    /// # Errors
    ///
    /// [`Error::Undo`] with the step that stopped it. Everything before it has
    /// been journaled, so the record is a true account of how far it got.
    pub(crate) fn run(
        &self,
        journal: &mut Record,
        positions: &[usize],
        warnings: &mut Vec<UndoWarning>,
    ) -> Result<usize> {
        let mut acted = 0;
        for &position in positions {
            let step = &self.steps[position];
            let outcome = match self.action {
                Action::Reverse => self.reverse(step, warnings),
                Action::Replay => self.replay(step, warnings),
            };
            match outcome {
                // A replay produces a receipt of its own, which is what makes
                // *its* record reversible in turn; a reversal produces none —
                // the step's own receipt is what reversed it, and there is
                // nothing new to say about putting a file back where the record
                // already says it came from.
                Ok(Some(receipt)) => journal.steps[position].completed(receipt),
                Ok(None) => {
                    journal.steps[position].done = true;
                    journal.steps[position].error = None;
                }
                Err(err) => {
                    journal.steps[position].failed(&err);
                    self.store
                        .append_step(&journal.txid, position, &journal.steps[position])?;
                    return Err(UndoError::Step {
                        txid: journal.txid.clone(),
                        position: position + 1,
                        step: step.step.to_string(),
                        source: Box::new(err),
                    }
                    .into());
                }
            }
            // Journaled after the fact, never before: a record that claimed a
            // step was put back before it was would send `recover` looking for a
            // file that has not moved.
            self.store
                .append_step(&journal.txid, position, &journal.steps[position])?;
            acted += 1;
        }
        Ok(acted)
    }

    /// Put one step back from its receipt.
    fn reverse(
        &self,
        step: &StepRecord,
        warnings: &mut Vec<UndoWarning>,
    ) -> std::result::Result<Option<StepReceipt>, FsError> {
        let Some(receipt) = step.receipt() else {
            return Err(FsError::NotRevertible {
                step: step.step.to_string(),
                why: "it is marked done but records nothing about what it did",
            });
        };
        match exec_fs::revert(&receipt, &self.root) {
            // A directory that is not empty any more is not a failed undo: the
            // files are back where they belong and something that is not this
            // transaction's is in the way. MPDFM does not delete that to tidy up.
            Err(FsError::NotEmpty { path, remaining }) => {
                warnings.push(UndoWarning::DirKept {
                    at: path,
                    holds: remaining,
                });
                Ok(None)
            }
            Err(err) => Err(err),
            Ok(()) => Ok(None),
        }
    }

    /// Do one step again.
    fn replay(
        &self,
        step: &StepRecord,
        warnings: &mut Vec<UndoWarning>,
    ) -> std::result::Result<Option<StepReceipt>, FsError> {
        let receipt = exec_fs::execute_with(&step.step, &self.root, &self.fs)?;
        warnings.extend(receipt.warnings.iter().cloned().map(UndoWarning::Fs));
        Ok(Some(receipt))
    }
}

/// Put every playlist back to what the record's backup directory holds, and the
/// state file with them.
///
/// Unconditional, and deliberately so ([`rewrite::restore`]): a playlist the
/// transaction never got to write is restored to the bytes it already has, which
/// is a no-op worth doing rather than a state worth reasoning about. A playlist
/// the check flagged as changed has already had its current bytes copied into
/// `kept`, so `--force` overwrites it without losing them.
fn restore_playlists(
    record: &Record,
    config: &Config,
    problems: &[Problem],
    kept: &Utf8Path,
    warnings: &mut Vec<UndoWarning>,
) -> Result<()> {
    for problem in problems {
        if problem.what == Trouble::PlaylistChanged {
            warnings.push(UndoWarning::PlaylistOverwritten {
                file_name: problem
                    .at
                    .file_name()
                    .unwrap_or_else(|| problem.at.as_str())
                    .to_owned(),
                kept: kept.to_owned(),
            });
        }
    }
    rewrite::restore(&record.playlist_edits, &record.backup_dir)?;
    restore_state_file(record, config, warnings);
    Ok(())
}

/// Copy MPD's state file into the undo's own backup directory, when the record
/// says the transaction changed it.
///
/// [`Record::state_edits`] is the authority, not [`Record::state_backup`]: the
/// commit takes a copy of the state file whenever it is configured to rewrite
/// one, and restoring a file no edit ever named would throw away whatever MPD
/// has written to it since.
fn back_up_state_file(
    record: &Record,
    config: &Config,
    backup_dir: &Utf8Path,
) -> Result<Option<String>> {
    if record.state_edits.is_empty() || record.state_backup.is_none() {
        return Ok(None);
    }
    let Some(state_file) = &config.state_file else {
        return Ok(None);
    };
    if !state_file.is_file() {
        return Ok(None);
    }
    let name = state_file.file_name().unwrap_or("state").to_owned();
    std::fs::copy(state_file, backup_dir.join(&name)).map_err(|source| Error::Io {
        path: state_file.to_string(),
        source,
    })?;
    Ok(Some(name))
}

/// Put MPD's saved queue back, when the record says the transaction changed it.
///
/// A warning rather than a failure: the library and the playlists are already
/// consistent by the time this runs, and MPD rereads its state file when it is
/// restarted either way.
///
/// Unconditional, like [`rewrite::restore`], and wholesale: the backup is a copy
/// of the entire file, so what goes back is every byte the transaction found —
/// the renumbered entries, the `current:` line, and the keys MPDFM never looked
/// at. `pub(super)` for [`recover`][super::recover], which does the same thing
/// when it finishes an interrupted undo.
pub(super) fn restore_state_file(
    record: &Record,
    config: &Config,
    warnings: &mut Vec<UndoWarning>,
) {
    if record.state_edits.is_empty() {
        return;
    }
    let Some(name) = &record.state_backup else {
        warnings.push(UndoWarning::State(
            "the transaction changed it but took no copy of it".to_owned(),
        ));
        return;
    };
    let Some(state_file) = &config.state_file else {
        warnings.push(UndoWarning::State(
            "there is no state_file configured to restore it to".to_owned(),
        ));
        return;
    };
    // Through the state module's atomic writer rather than `fs::copy`: it
    // resolves a state file that is a symlink into a dotfiles repository, and it
    // leaves either the whole old queue or the whole new one if this is the
    // moment the power goes.
    let backup = record.backup_dir.join(name);
    let restored = std::fs::read(&backup)
        .map_err(|source| Error::Io {
            path: backup.to_string(),
            source,
        })
        .and_then(|bytes| state::replace(state_file, &bytes));
    if let Err(err) = restored {
        warnings.push(UndoWarning::State(err.to_string()));
    }
}

/// Ask MPD to rescan what the undo moved, and record the ask.
fn tell_mpd(
    mine: &mut Record,
    record: &Record,
    acted: &[usize],
    config: &Config,
    options: &Options<'_>,
    warnings: &mut Vec<UndoWarning>,
) {
    let Some(update) = options.update else {
        return;
    };
    if !config.mpd_enabled || !config.trigger_update_after_commit {
        return;
    }
    let steps: Vec<FsStep> = acted
        .iter()
        .map(|position| record.steps[*position].step.clone())
        .collect();
    let dirs = commit::affected_dirs(&steps);
    mine.mpd_update_requested = true;
    mine.mpd_update_dirs = dirs.clone();
    if let Err(message) = update(&dirs) {
        mine.mpd_update_failed = Some(message.clone());
        warnings.push(UndoWarning::Mpd(message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undoing_a_record_goes_the_way_its_direction_says() {
        assert_eq!(Action::of(Direction::Forward), Action::Reverse);
        assert_eq!(Action::of(Direction::Reverse), Action::Replay);
        assert_eq!(Direction::Forward.flipped(), Direction::Reverse);
        assert_eq!(Direction::Reverse.flipped(), Direction::Forward);
    }

    #[test]
    fn a_reversal_visits_the_steps_in_the_opposite_order() {
        let mut record = blank();
        record.steps = (0..4).map(|_| planned()).collect();
        for step in &mut record.steps {
            step.done = true;
        }
        record.steps[2].done = false;

        assert_eq!(positions(&record, Action::Reverse, true), vec![3, 1, 0]);
        assert_eq!(positions(&record, Action::Replay, true), vec![0, 1, 3]);
        assert_eq!(
            positions(&record, Action::Replay, false),
            vec![2],
            "and the other side is what finishing it would do"
        );
    }

    #[test]
    fn a_file_that_grew_or_was_touched_is_reported_and_a_directory_is_not() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = Utf8Path::from_path(dir.path()).expect("utf-8");
        let file = root.join("track.mp3");
        std::fs::write(&file, b"0123456789").expect("writable");

        let recorded = Facts::of(&file).expect("it is there");
        assert_eq!(differences(&recorded, &recorded, &file), None);

        let bigger = Facts {
            size: recorded.size + 1,
            ..recorded
        };
        assert!(
            differences(&bigger, &recorded, &file).is_some_and(|detail| detail.contains("bytes")),
            "a different size has to be reported"
        );

        let touched = Facts {
            mtime: recorded
                .mtime
                .map(|mtime| mtime + std::time::Duration::from_secs(60)),
            ..recorded
        };
        assert!(
            differences(&touched, &recorded, &file)
                .is_some_and(|detail| detail.contains("modification time")),
            "and so does a different mtime"
        );

        // The same comparison against a directory says nothing: its mtime moves
        // whenever anything inside it does.
        assert_eq!(differences(&touched, &recorded, root), None);
    }

    #[test]
    fn a_recorded_hash_that_no_longer_matches_is_reported() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = Utf8Path::from_path(dir.path()).expect("utf-8");
        let file = root.join("track.mp3");
        std::fs::write(&file, b"0123456789").expect("writable");

        let current = Facts::of(&file).expect("it is there");
        let recorded = Facts {
            hash: Some(exec_fs::hash_file(&file).expect("readable")),
            ..current
        };
        assert_eq!(differences(&recorded, &current, &file), None);

        let wrong = Facts {
            hash: Some(1),
            ..current
        };
        assert!(
            differences(&wrong, &current, &file)
                .is_some_and(|detail| detail.contains("contents differ")),
            "same size, same mtime, different bytes — which is what a hash is for"
        );
    }

    /// A record with nothing in it but the fields these tests set.
    fn blank() -> Record {
        Record::opening(
            TxId::parse("20260101T000000Z-0001").expect("a valid id"),
            SystemTime::UNIX_EPOCH,
            Utf8PathBuf::from("/music"),
            Utf8PathBuf::from("/playlists"),
            Utf8PathBuf::from("/data/backups/tx"),
        )
    }

    fn planned() -> StepRecord {
        StepRecord::planned(FsStep::MkDir {
            at: crate::paths::RelPath::parse("a").expect("a valid path"),
        })
    }
}
