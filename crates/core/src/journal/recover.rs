//! What to do about a transaction a crash left halfway through: look first, then
//! roll it back or finish it.
//!
//! ```no_run
//! use mpdfm_core::journal::{Store, recover, undo};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (config, _warnings) = mpdfm_core::config::resolve(&Default::default(),
//!                                                       &mpdfm_core::config::Env::from_process());
//! let store = Store::at(&config.data_dir);
//!
//! for record in recover::pending(&store)? {
//!     let survey = recover::survey(&record, &config)?;
//!     print!("{}", survey.render());        // what happened, as the disk tells it
//!
//!     // Rolling back is the default and the safe one.
//!     recover::roll_back(&store, &record, &config, &undo::Options::default())?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Why this cannot work from the record alone
//!
//! [`undo`][super::undo] trusts the record, because a `complete` record is a
//! finished account of what happened. A `pending` one is not: the commit
//! appends a step to the journal *after* doing it (which is the only order that
//! is safe to be wrong in), so the last line can be missing for a step that ran
//! — and a power cut in the middle of the append leaves a torn line, which
//! [`Store::read_steps`][super::store::Store::read_steps] drops.
//!
//! So recovery looks at the disk. A planned step whose destination is there and
//! whose source is not **is a step that ran**, whatever the log says, and
//! [`Survey`] says so for each one. A receipt is reconstructed from what is on
//! disk now ([`StepRecord::reconstructed`]), appended to the transaction's own
//! step log so the knowledge is durable before anything is moved, and the
//! rollback then proceeds exactly as [`undo`][super::undo]'s does — one engine,
//! not two.
//!
//! A step whose source *and* destination both exist, or neither, is the one case
//! that cannot be told apart by looking: it is reported, it stops the recovery,
//! and `--force` is what says "deal with the rest and leave that one alone".
//! Guessing would be the one thing worse than stopping.
//!
//! # Rolling back is the default
//!
//! Both directions are offered because both are sometimes right — a transaction
//! that got as far as the last playlist is cheaper to finish than to reverse —
//! but roll-back is the one to reach for, and it is what the CLI defaults to.
//! Finishing a transaction means doing work the user has not looked at since
//! they agreed to it, possibly days ago; reversing it puts the library back
//! where they last saw it.
//!
//! An interrupted *undo* recovers the same two ways, and the symmetry is not a
//! coincidence: its record's [`Direction`] is `reverse`, so "finish it" means
//! reversing the steps it had not got to, and "roll it back" means re-applying
//! the ones it had.

use std::time::SystemTime;

use camino::Utf8PathBuf;

use crate::config::Config;
use crate::ops::exec_fs::{Done, Facts, FsStep, Method, RemovedDir, StepReceipt};
use crate::paths::RelPath;
use crate::playlist::rewrite;
use crate::{Error, Result};

use super::JournalError;
use super::record::{Direction, Record, Status, StepRecord, TxId};
use super::store::Store;
use super::undo::{self, Action, Options, Problem, Reversed, Trouble, UndoWarning};

/// Every transaction a previous run left `pending` or `failed`, newest first.
///
/// `mpdfm recover` with no argument walks this list; a front-end that finds it
/// non-empty at startup should say so (`docs/PLAN.md` §5).
///
/// # Errors
///
/// [`JournalError::Io`] if the journal directory cannot be listed.
pub fn pending(store: &Store) -> std::result::Result<Vec<Record>, JournalError> {
    store.unfinished()
}

// ---------------------------------------------------------------------------
// Looking.

/// What a half-finished transaction actually did, as the record and the disk
/// together tell it.
#[derive(Debug, Clone)]
pub struct Survey {
    /// The transaction.
    pub txid: TxId,
    /// What its record says.
    pub status: Status,
    /// Whether its steps were being executed or reversed.
    pub direction: Direction,
    /// How many steps it has in total.
    pub steps: usize,
    /// The steps the journal records as having happened.
    pub recorded: Vec<usize>,
    /// The steps that happened without being recorded, with the receipt
    /// reconstructed for each — see the [module docs][self].
    pub found: Vec<Found>,
    /// The steps that have not happened.
    pub outstanding: Vec<usize>,
    /// The steps whose state cannot be told apart by looking, and anything else
    /// that stops a recovery until the user says `--force`.
    pub blocked: Vec<Problem>,
    /// Every affected playlist, and whether the transaction got to it.
    pub playlists: Vec<Playlist>,
    /// Everything worth mentioning that stops nothing.
    pub warnings: Vec<UndoWarning>,
}

/// One step whose work had happened without being journaled.
#[derive(Debug, Clone)]
pub struct Found {
    /// Its position in [`Record::steps`].
    pub at: usize,
    /// The step, rendered, for the report.
    pub step: String,
    /// The receipt reconstructed from the disk, for a step that was *executed*
    /// — which is what reverses it.
    ///
    /// `None` for a step that was *reversed* without being journaled, in an
    /// interrupted undo: putting a reversed step back means executing it again,
    /// and executing a step needs nothing but the step.
    pub receipt: Option<StepReceipt>,
}

/// One affected playlist, and which side of the transaction it is on.
#[derive(Debug, Clone)]
pub struct Playlist {
    /// Its name, as the playlist directory spells it.
    pub file_name: String,
    /// The file itself, symlinks resolved.
    pub real_path: Utf8PathBuf,
    /// What is in it now.
    pub state: PlaylistState,
}

/// What a playlist's current bytes say about how far the transaction got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistState {
    /// Byte-for-byte the backup: the transaction never wrote it.
    AsItWas,
    /// Byte-for-byte what the transaction's edits produce: it was written.
    Rewritten,
    /// Neither. Somebody else has edited it since the backup was taken, and a
    /// restore would throw that away.
    Changed,
    /// It could not be read, or what the transaction would have written could
    /// not be worked out.
    Unknown,
}

impl std::fmt::Display for PlaylistState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AsItWas => "not written",
            Self::Rewritten => "written",
            Self::Changed => "changed since the backup",
            Self::Unknown => "unreadable",
        })
    }
}

impl Survey {
    /// Whether a rollback would go ahead without `--force`.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.blocked.is_empty()
    }

    /// How far the transaction got, as a fraction of its steps.
    #[must_use]
    pub fn reached(&self) -> usize {
        self.recorded.len() + self.found.len()
    }

    /// The report, as `mpdfm recover` prints it. No trailing newline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = vec![
            format!(
                "RECOVER {}  {} transaction, {} of {} step(s) done",
                self.txid,
                self.status,
                self.reached(),
                self.steps
            ),
            format!(
                "  rolling back {}; finishing it {}",
                match self.direction {
                    Direction::Forward => "undoes what it managed",
                    Direction::Reverse => "re-applies what it had already undone",
                },
                match self.direction {
                    Direction::Forward => "does the rest of what it was asked to do",
                    Direction::Reverse => "undoes the rest of what it was undoing",
                },
            ),
        ];
        if !self.found.is_empty() {
            out.push(String::new());
            out.push(format!(
                "Done but not journaled ({}) — the crash cut the log short",
                self.found.len()
            ));
            out.extend(
                self.found
                    .iter()
                    .map(|found| format!("  + step {}: {}", found.at + 1, found.step)),
            );
        }
        if !self.playlists.is_empty() {
            out.push(String::new());
            out.push("Playlists".to_owned());
            out.extend(
                self.playlists
                    .iter()
                    .map(|playlist| format!("  {} — {}", playlist.file_name, playlist.state)),
            );
        }
        if !self.blocked.is_empty() {
            out.push(String::new());
            out.push(format!("Cannot be told apart ({})", self.blocked.len()));
            out.extend(self.blocked.iter().map(|problem| format!("  x {problem}")));
            out.push(String::new());
            out.push(
                "Nothing has been changed. Re-run with --force to deal with the rest \
                 and leave these alone."
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

    /// Make what was found by looking durable, and hand back the record with it
    /// folded in.
    ///
    /// The reconstructed receipts are appended to the transaction's own step log
    /// *before* the rollback starts, so a second crash finds a record that
    /// already knows what the first one left — the same reason the commit
    /// journals a step before starting the next.
    ///
    /// # Errors
    ///
    /// [`Error::Journal`] if the log cannot be appended to.
    fn repair(&self, store: &Store, record: &Record) -> Result<Record> {
        let mut repaired = record.clone();
        for found in &self.found {
            match &found.receipt {
                Some(receipt) => repaired.steps[found.at].found_done(receipt.clone()),
                None => repaired.steps[found.at].found_reversed(),
            }
            store.append_step(&record.txid, found.at, &repaired.steps[found.at])?;
        }
        Ok(repaired)
    }
}

impl std::fmt::Display for Survey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// Why a transaction could not be recovered.
#[derive(Debug, thiserror::Error)]
pub enum RecoverError {
    /// Nothing needs recovering.
    #[error("there is no unfinished transaction to recover")]
    Nothing,

    /// The transaction finished, one way or the other. `undo` is what reverses
    /// a finished transaction, and it can check its preconditions properly
    /// because the record it works from is complete.
    #[error(
        "transaction {txid} is {status}, not unfinished — `mpdfm undo {txid}` is \
         what reverses a transaction that finished"
    )]
    Finished {
        /// The transaction.
        txid: TxId,
        /// What its record says.
        status: Status,
    },

    /// Something cannot be told apart by looking, or has changed since.
    #[error(
        "transaction {txid} was not recovered: {} thing(s) cannot be dealt with \
         safely:\n{}\n\
         Re-run with --force to deal with the rest and leave these alone.",
        .problems.len(),
        .problems.iter().map(|problem| format!("  - {problem}")).collect::<Vec<_>>().join("\n")
    )]
    Blocked {
        /// The transaction.
        txid: TxId,
        /// What is in the way.
        problems: Vec<Problem>,
    },

    /// This is an interrupted *undo*, and the transaction it was undoing cannot
    /// be read — so there is no way to know what its playlists should go back
    /// to.
    #[error(
        "transaction {txid} is an undo of {of}, whose record cannot be read: \
         {source}"
    )]
    Chain {
        /// The interrupted undo.
        txid: TxId,
        /// What it was undoing.
        of: TxId,
        /// Why that record is no good.
        #[source]
        source: Box<JournalError>,
    },
}

/// Look at a half-finished transaction and work out what it did.
///
/// Writes nothing, which is what makes it safe to run at startup over every
/// pending record.
///
/// # Errors
///
/// [`Error::Recover`] for a transaction that is not unfinished, and
/// [`Error::Undo`] when its backups or its library root are not there — a
/// recovery needs both for exactly the reasons an undo does.
pub fn survey(record: &Record, config: &Config) -> Result<Survey> {
    if !record.status.is_unfinished() {
        return Err(RecoverError::Finished {
            txid: record.txid.clone(),
            status: record.status,
        }
        .into());
    }
    undo::backups_present(record)?;
    undo::root_present(record)?;

    let mut survey = Survey {
        txid: record.txid.clone(),
        status: record.status,
        direction: record.direction,
        steps: record.steps.len(),
        recorded: Vec::new(),
        found: Vec::new(),
        outstanding: Vec::new(),
        blocked: Vec::new(),
        playlists: playlists(record),
        warnings: Vec::new(),
    };
    if config.music_dir != record.music_dir {
        survey.warnings.push(UndoWarning::DifferentRoot {
            recorded: record.music_dir.clone(),
            configured: config.music_dir.clone(),
        });
    }

    for (at, step) in record.steps.iter().enumerate() {
        if step.done {
            survey.recorded.push(at);
            continue;
        }
        match look(record, step, at) {
            Outcome::NotRun => survey.outstanding.push(at),
            Outcome::Ran(receipt) => survey.found.push(Found {
                at,
                step: step.step.to_string(),
                receipt,
            }),
            Outcome::CannotTell(problem) => survey.blocked.push(problem),
            Outcome::Unclear(warning) => {
                survey.outstanding.push(at);
                survey.warnings.push(warning);
            }
        }
    }

    Ok(survey)
}

/// What looking at the disk says about one step the journal does not claim.
enum Outcome {
    /// This record's action has not happened to it.
    NotRun,
    /// It has, with the receipt that puts it back — `None` when putting it back
    /// means executing the step again, which needs no receipt.
    Ran(Option<StepReceipt>),
    /// Both ends are on disk, or neither. This is the case that must not be
    /// guessed at.
    CannotTell(Problem),
    /// It cannot be told either way and does not have to be, because doing it
    /// again is harmless.
    Unclear(UndoWarning),
}

/// Whether the *commit's* work has landed on one step, as the disk shows it.
///
/// Deliberately phrased in the commit's direction whichever record this is:
/// "the destination is there and the source is not" is a fact about the disk,
/// and what it means for a given record is [`look`]'s business.
enum Landed {
    /// The source is where it was and the destination is free.
    No,
    /// The destination is there and the source is not.
    Yes,
    /// Both are there, or neither is.
    CannotTell(Trouble, Utf8PathBuf, &'static str),
}

/// Whether one unjournaled step's work has happened, by looking at the paths it
/// names.
///
/// A step that was executed needs a receipt to be reversed, and the one
/// reconstructed here deliberately claims less than a real one: no hash, and the
/// facts are read from the file as it is *now* rather than as it was before it
/// moved. That is the most that can honestly be said, and it is enough to move
/// the file back.
fn look(record: &Record, step: &StepRecord, at: usize) -> Outcome {
    // The one step whose work cannot be recognized after the fact: `MkDir` is
    // idempotent, so a directory that is there may have been created by this
    // transaction or may have been there all along. Doing it again is harmless,
    // so it is left outstanding and the directory is reported rather than
    // removed on a guess.
    if let FsStep::MkDir { at: dir } = &step.step {
        let abs = dir.to_abs(&record.music_dir);
        if record.direction == Direction::Forward && abs.symlink_metadata().is_ok() {
            return Outcome::Unclear(UndoWarning::DirKept {
                at: abs,
                holds: vec!["it may or may not have been created by this transaction".to_owned()],
            });
        }
    }

    let landed = match landed(record, step) {
        Landed::CannotTell(what, path, detail) => {
            return Outcome::CannotTell(Problem {
                at: path,
                what,
                detail: format!("`{}` {detail}", step.step),
                step: Some(at),
            });
        }
        Landed::Yes => true,
        Landed::No => false,
    };

    match (record.direction, landed) {
        // A commit's step that has landed is a step that ran.
        (Direction::Forward, true) => match reconstruct(record, step) {
            Ok(receipt) => Outcome::Ran(Some(receipt)),
            Err(detail) => Outcome::CannotTell(Problem {
                at: record.music_dir.clone(),
                what: Trouble::Unreadable,
                detail: format!(
                    "`{}` ran, and its receipt cannot be worked out: {detail}",
                    step.step
                ),
                step: Some(at),
            }),
        },
        // An undo's step that has *not* landed is a step it had already
        // reversed. Its own record would have said so if the crash had not come
        // between the reversal and the line about it.
        (Direction::Reverse, false) => Outcome::Ran(None),
        _ => Outcome::NotRun,
    }
}

/// [`Landed`] for one step.
fn landed(record: &Record, step: &StepRecord) -> Landed {
    let root = &record.music_dir;
    let there = |path: &Utf8PathBuf| path.symlink_metadata().is_ok();

    match &step.step {
        // Its directory being there is the state before it ran.
        FsStep::MkDir { at } => {
            if there(&at.to_abs(root)) {
                Landed::Yes
            } else {
                Landed::No
            }
        }

        FsStep::RenameFile { from, to } | FsStep::CopyDelete { from, to } => {
            let (from_abs, to_abs) = (from.to_abs(root), to.to_abs(root));
            match (there(&from_abs), there(&to_abs)) {
                (true, false) => Landed::No,
                (false, true) => Landed::Yes,
                (true, true) => Landed::CannotTell(
                    Trouble::Occupied,
                    to_abs,
                    "has both ends on disk, so whether it ran cannot be told by looking",
                ),
                (false, false) => Landed::CannotTell(
                    Trouble::Missing,
                    from_abs,
                    "has neither end on disk, so what happened to it cannot be told by looking",
                ),
            }
        }

        FsStep::RemoveFile { target, backup } => {
            let target_abs = target.to_abs(root);
            let Some(backup) = backup else {
                // An unbacked delete that ran is gone for good; one that has not
                // run is still where it was.
                return if there(&target_abs) {
                    Landed::No
                } else {
                    Landed::CannotTell(
                        Trouble::Missing,
                        target_abs,
                        "removed the file with no backup, so it cannot be put back",
                    )
                };
            };
            match (there(&target_abs), there(backup)) {
                (true, false) => Landed::No,
                (false, true) => Landed::Yes,
                (true, true) => Landed::CannotTell(
                    Trouble::Occupied,
                    target_abs,
                    "has the file and its backup both on disk, so whether it ran cannot \
                     be told by looking",
                ),
                (false, false) => Landed::CannotTell(
                    Trouble::Missing,
                    target_abs,
                    "has neither the file nor its backup on disk",
                ),
            }
        }

        FsStep::RmDirIfEmpty { at } => {
            if there(&at.to_abs(root)) {
                Landed::No
            } else {
                Landed::Yes
            }
        }
    }
}

/// The receipt for a step that ran without being journaled.
fn reconstruct(record: &Record, step: &StepRecord) -> std::result::Result<StepReceipt, String> {
    let root = &record.music_dir;
    match &step.step {
        FsStep::RenameFile { to, .. } | FsStep::CopyDelete { to, .. } => {
            moved_receipt(step, &to.to_abs(root))
        }
        FsStep::RemoveFile {
            backup: Some(backup),
            ..
        } => removed_receipt(step, backup),
        FsStep::RmDirIfEmpty { at } => Ok(StepReceipt {
            step: step.step.clone(),
            done: Done::DirsRemoved {
                dirs: removed_dirs(record, at),
            },
            warnings: Vec::new(),
        }),
        // Neither is reachable: `look` sends an unbacked delete to
        // `CannotTell`, and a `MkDir` never gets this far.
        FsStep::RemoveFile { backup: None, .. } | FsStep::MkDir { .. } => {
            Err("its receipt cannot be reconstructed".to_owned())
        }
    }
}

/// The receipt for a move that ran without being journaled.
///
/// [`Method::Copy`] rather than `Rename`, even though a rename is what almost
/// certainly happened: the way back is the one thing this receipt is for, and a
/// `rename` back across a filesystem boundary fails with `EXDEV` where a copy
/// works either way. The facts come from the file as it is now, so the copy
/// restores exactly the mode and mtime it has. A directory is the exception —
/// there is nothing to copy — and only a case-only rename produces one.
fn moved_receipt(step: &StepRecord, to: &Utf8PathBuf) -> std::result::Result<StepReceipt, String> {
    let facts = Facts::of(to).map_err(|err| err.to_string())?;
    Ok(StepReceipt {
        step: step.step.clone(),
        done: Done::Moved {
            method: if to.is_dir() {
                Method::Rename
            } else {
                Method::Copy
            },
            // Not knowable, and the honest answer: a directory this step created
            // is left where it is rather than removed on a guess.
            dirs: Vec::new(),
            facts,
        },
        warnings: Vec::new(),
    })
}

/// The receipt for a delete that ran without being journaled.
fn removed_receipt(
    step: &StepRecord,
    backup: &Utf8PathBuf,
) -> std::result::Result<StepReceipt, String> {
    let facts = Facts::of(backup).map_err(|err| err.to_string())?;
    Ok(StepReceipt {
        step: step.step.clone(),
        done: Done::Removed {
            method: Method::Copy,
            facts,
        },
        warnings: Vec::new(),
    })
}

/// The directories an unjournaled `RmDirIfEmpty` removed, innermost first.
///
/// It walks up from the step's own directory for as long as the directories are
/// missing, which is the set that step *could* have removed — it stops at the
/// first one that is not empty, so everything above the deepest surviving
/// ancestor was either removed by it or was never there. Their modes are gone
/// with them; recreating one gives it the default, which the snapshot tests
/// compare and which is why a rollback prefers a journaled receipt whenever
/// there is one.
fn removed_dirs(record: &Record, at: &RelPath) -> Vec<RemovedDir> {
    let mut dirs = Vec::new();
    let mut current = Some(at.clone());
    while let Some(dir) = current {
        if dir.to_abs(&record.music_dir).symlink_metadata().is_ok() {
            break;
        }
        current = dir.parent();
        dirs.push(RemovedDir {
            at: dir,
            mode: None,
        });
    }
    dirs
}

/// Which side of the transaction each affected playlist is on.
fn playlists(record: &Record) -> Vec<Playlist> {
    record
        .playlist_edits
        .iter()
        .map(|edit| Playlist {
            file_name: edit.file_name.clone(),
            real_path: edit.real_path.clone(),
            state: state_of(record, edit),
        })
        .collect()
}

/// [`PlaylistState`] for one playlist: the backup is the side the transaction
/// started from, the edits applied to it are the side it was heading for, and
/// anything else is somebody else's edit.
///
/// An interrupted *undo* is heading the other way, so the question is asked the
/// other way round — applying the edits to what is there now has to give the
/// backup's bytes, which is the same test [`undo`][super::undo] makes.
fn state_of(record: &Record, edit: &rewrite::PlaylistEdit) -> PlaylistState {
    let Ok(before) = std::fs::read(record.backup_dir.join(&edit.file_name)) else {
        return PlaylistState::Unknown;
    };
    let Ok(current) = std::fs::read(&edit.real_path) else {
        return PlaylistState::Unknown;
    };
    if current == before {
        return PlaylistState::AsItWas;
    }
    let answer = match record.direction {
        Direction::Forward => rewrite::after(&before, edit).map(|after| after == current),
        Direction::Reverse => rewrite::after(&current, edit).map(|after| after == before),
    };
    match answer {
        Ok(true) => PlaylistState::Rewritten,
        Ok(false) | Err(Error::Rewrite(_)) => PlaylistState::Changed,
        Err(_) => PlaylistState::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Acting.

/// Put a half-finished transaction back where it started.
///
/// The default and the safe one — see the [module docs][self]. It is
/// [`undo`][super::undo]'s own engine, over the steps this transaction actually
/// reached: the record is marked [`Status::Reverted`] and a new record is
/// written for the rollback, so a recovery is itself inspectable and undoable.
///
/// # Errors
///
/// [`Error::Recover`] when something cannot be told apart by looking and
/// `--force` was not given, [`Error::Undo`] for a step that cannot be put back,
/// and [`Error::Journal`] if the rollback's own record cannot be made durable.
pub fn roll_back(
    store: &Store,
    record: &Record,
    config: &Config,
    options: &Options<'_>,
) -> Result<Reversed> {
    let survey = survey(record, config)?;
    if !survey.is_clear() && !options.force {
        return Err(RecoverError::Blocked {
            txid: record.txid.clone(),
            problems: survey.blocked,
        }
        .into());
    }
    let repaired = survey.repair(store, record)?;

    // The same precondition pass an `undo` does — a file that was modified after
    // the crash must not be moved back on top of that — with the status gate
    // left out, because "it never finished" is this function's whole premise.
    let action = Action::of(repaired.direction);
    let mut check = undo::inspect(&repaired, config, action);
    check.problems.extend(survey.blocked);
    if !check.is_clear() && !options.force {
        return Err(RecoverError::Blocked {
            txid: record.txid.clone(),
            problems: check.problems,
        }
        .into());
    }
    undo::put_back(store, &repaired, config, &check, options)
}

/// Finish a half-finished transaction instead of reversing it.
///
/// Does the steps it had not got to, writes the playlists it had not written,
/// and marks its own record `complete` — there is no new record, because this is
/// the same transaction carrying on rather than a new one undoing it. Undoing it
/// afterwards is an ordinary `mpdfm undo`.
///
/// # Errors
///
/// As [`roll_back`], plus [`Error::NotImplemented`] for a record that changes
/// MPD's saved queue (task 14), and [`Error::Rewrite`] or [`Error::Io`] if a
/// playlist cannot be written.
pub fn roll_forward(
    store: &Store,
    record: &Record,
    config: &Config,
    options: &Options<'_>,
) -> Result<Finished> {
    let survey = survey(record, config)?;
    if !survey.is_clear() && !options.force {
        return Err(RecoverError::Blocked {
            txid: record.txid.clone(),
            problems: survey.blocked,
        }
        .into());
    }
    if !record.state_edits.is_empty() {
        return Err(Error::not_implemented(
            "finishing a transaction that rewrites MPD's saved queue",
            "14-mpd-state-queue.md",
        ));
    }

    let mut mine = survey.repair(store, record)?;
    let action = Action::continuing(record.direction);
    // An interrupted undo carries on reversing, and a reversal works from the
    // receipts of the transaction being undone — an undo's own record keeps
    // none, because a reversal produces none. So the steps this pass reads are
    // that transaction's; the ones it *writes* are this record's, and the two
    // are in the same order because an undo's record is built from them.
    let target = match record.direction {
        Direction::Forward => None,
        Direction::Reverse => Some(reversing(store, record)?),
    };
    let mut warnings = survey.warnings.clone();
    let skipped: Vec<Problem> = survey.blocked.clone();
    let skips: Vec<usize> = skipped.iter().filter_map(|problem| problem.step).collect();
    for problem in &skipped {
        if let Some(position) = problem.step {
            mine.steps[position].skipped(problem);
            store.append_step(&record.txid, position, &mine.steps[position])?;
            warnings.push(UndoWarning::Skipped {
                step: position + 1,
                problem: problem.clone(),
            });
        }
    }

    let outstanding: Vec<usize> = undo::positions(&mine, action, false)
        .into_iter()
        .filter(|position| !skips.contains(position))
        .collect();
    let pass = undo::Pass {
        store,
        steps: target
            .as_ref()
            .map_or_else(|| mine.steps.clone(), |target| target.steps.clone()),
        action,
        root: record.music_dir.clone(),
        fs: crate::ops::exec_fs::Options::from_config(config),
    };
    let steps = match pass.run(&mut mine, &outstanding, &mut warnings) {
        Ok(steps) => steps,
        Err(err) => {
            mine.finish(Status::Failed, SystemTime::now());
            store.write(&mine)?;
            return Err(err);
        }
    };

    match target {
        // Finish the commit: write the playlists it had not written.
        None => finish_playlists(record, options.force, &mut warnings)?,
        // Finish the undo: put the playlists back to what the transaction it was
        // undoing had copied, and mark that transaction reverted now that its
        // reversal is complete.
        Some(target) => finish_reversal(store, record, &target)?,
    }

    mine.finish(Status::Complete, SystemTime::now());
    store.write(&mine)?;
    store.forget_steps(&record.txid);

    Ok(Finished {
        txid: record.txid.clone(),
        action,
        steps,
        reconstructed: survey.found.iter().map(|found| found.at).collect(),
        skipped,
        record: mine,
        warnings,
    })
}

/// A transaction a recovery finished.
#[derive(Debug, Clone)]
pub struct Finished {
    /// The transaction, which keeps its own id: this is it carrying on, not a
    /// new transaction.
    pub txid: TxId,
    /// What was done to the steps it had not reached.
    pub action: Action,
    /// How many of them.
    pub steps: usize,
    /// The steps whose receipts had to be reconstructed by looking at the disk.
    pub reconstructed: Vec<usize>,
    /// The steps `--force` left alone.
    pub skipped: Vec<Problem>,
    /// Its record, `complete`.
    pub record: Record,
    /// Everything worth telling the user that did not stop it.
    pub warnings: Vec<UndoWarning>,
}

impl Finished {
    /// One line for the end of `mpdfm recover`.
    #[must_use]
    pub fn headline(&self) -> String {
        let skipped = if self.skipped.is_empty() {
            String::new()
        } else {
            format!(", {} skipped", self.skipped.len())
        };
        format!(
            "finished {} ({} more step(s){skipped}) — undo it with `mpdfm undo {}`",
            self.txid, self.steps, self.txid,
        )
    }
}

/// Write the playlists a commit had not got to.
///
/// Each one is compared against both sides first: a playlist that already holds
/// what the transaction would have written is left alone, one that still holds
/// the backup's bytes is written, and one that holds neither has been edited by
/// somebody else since and stops the recovery unless `force` says otherwise.
fn finish_playlists(record: &Record, force: bool, warnings: &mut Vec<UndoWarning>) -> Result<()> {
    for edit in &record.playlist_edits {
        let before =
            std::fs::read(record.backup_dir.join(&edit.file_name)).map_err(|source| Error::Io {
                path: record.backup_dir.join(&edit.file_name).to_string(),
                source,
            })?;
        let after = rewrite::after(&before, edit)?;
        let current = std::fs::read(&edit.real_path).map_err(|source| Error::Io {
            path: edit.real_path.to_string(),
            source,
        })?;
        if current == after {
            continue;
        }
        if current != before {
            let problem = Problem {
                at: edit.real_path.clone(),
                what: Trouble::PlaylistChanged,
                detail: format!(
                    "{} has changed since the transaction's backup was taken",
                    edit.file_name
                ),
                step: None,
            };
            if !force {
                return Err(RecoverError::Blocked {
                    txid: record.txid.clone(),
                    problems: vec![problem],
                }
                .into());
            }
            warnings.push(UndoWarning::PlaylistOverwritten {
                file_name: edit.file_name.clone(),
                kept: record.backup_dir.clone(),
            });
        }
        rewrite::replace(edit, &after)?;
    }
    Ok(())
}

/// The transaction an interrupted undo was reversing.
///
/// Its record holds the receipts the reversal was working from and the backups
/// the playlists have to go back to, so an undo cannot be finished without it.
///
/// # Errors
///
/// [`RecoverError::Chain`] when that record cannot be read, which is the one
/// state where finishing an undo is impossible and rolling it back is not.
fn reversing(store: &Store, record: &Record) -> Result<Record> {
    let Some(of) = &record.undo_of else {
        return Err(RecoverError::Chain {
            txid: record.txid.clone(),
            of: record.txid.clone(),
            source: Box::new(JournalError::BadTxId {
                input: String::new(),
                why: "the record does not say which transaction it was undoing",
            }),
        }
        .into());
    };
    store.load(of).map_err(|source| {
        RecoverError::Chain {
            txid: record.txid.clone(),
            of: of.clone(),
            source: Box::new(source),
        }
        .into()
    })
}

/// Finish an interrupted undo: the playlists go back to what the transaction it
/// was undoing had copied, and that transaction is marked reverted now that the
/// reversal has finished.
fn finish_reversal(store: &Store, record: &Record, target: &Record) -> Result<()> {
    rewrite::restore(&target.playlist_edits, &target.backup_dir)?;

    let mut reverted = target.clone();
    reverted.status = Status::Reverted;
    reverted.undone_by = Some(record.txid.clone());
    store.write(&reverted)?;
    Ok(())
}
