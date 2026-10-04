//! The two-phase commit: the only place in MPDFM that mutates the library.
//!
//! ```no_run
//! use mpdfm_core::library::Library;
//! use mpdfm_core::ops::commit::{self, Previewed};
//! use mpdfm_core::ops::{Operation, Plan};
//! use mpdfm_core::paths::RelPath;
//! use mpdfm_core::playlist::PlaylistIndex;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (config, _warnings) = mpdfm_core::config::resolve(&Default::default(),
//!                                                       &mpdfm_core::config::Env::from_process());
//! let library = Library::scan(config.require_music_dir()?)?;
//! let (index, _) = PlaylistIndex::load(&config.playlist_dir);
//!
//! let plan = Plan::of(vec![Operation::MoveDir {
//!     from: RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)")?,
//!     to: RelPath::parse("hiphop/MF DOOM/Mm..Food (2004)")?,
//! }]);
//! let effects = plan.validate(&library, &index, &config);
//! println!("{}", effects.render(80));   // and the user says yes
//!
//! let committed = commit::commit(
//!     &Previewed { plan: &plan, library: &library, effects: &effects },
//!     &config,
//! )?;
//! println!("transaction {} — undo it with `mpdfm undo {}`",
//!          committed.txid, committed.txid);
//! # Ok(())
//! # }
//! ```
//!
//! # The order is the whole point
//!
//! ```text
//! 1  re-validate against a fresh scan        nothing mutated
//! 2  create backup_dir, copy playlists       nothing mutated
//!    and the state file into it, fsync
//! 3  write the journal record, status         nothing mutated
//!    pending, fsync the file and the dir
//! 4  execute the fs steps, flushing the       the library changes
//!    journal after each one
//! 5  apply the playlist edits (task 09)       the playlists and MPD's
//!    and the state edits (task 14)             saved queue change
//! 6  status: complete, fsync                  nothing mutated
//! 7  ask MPD to rescan                        a warning at worst
//! ```
//!
//! Steps 1 to 3 can fail as often as they like: nothing has been touched, so the
//! answer is "no" and the library is exactly as it was. From step 4 onwards every
//! failure leaves a record that says what had been done by then, which is what
//! `mpdfm recover` (task 12) reads. There is no step at which the library has
//! changed and the journal does not know about it — that is the invariant this
//! ordering exists to produce, and [`Inject`] is how the tests prove it by
//! stopping the commit dead at each boundary.
//!
//! The one place that invariant narrows to "and the journal can find out by
//! looking" is a journal write that itself fails, or a crash during one: the step
//! had already happened. That is the same state a torn line in the step log leaves,
//! and `recover` resolves it the same way — a planned step whose destination exists
//! and whose source does not is a step that ran. Which is why the receipt records
//! what a step *did*, and never what it was going to do.
//!
//! # Why re-validation, when the preview just ran
//!
//! Between the preview and the user saying yes there is a human, and humans take
//! their time. A scan that was accurate when it was rendered can be wrong by the
//! time it is committed: another program has written to a track, MPD has saved a
//! playlist, someone has moved an album in a file manager. Step 1 re-runs
//! [`Plan::validate`] against a fresh scan and refuses with [`Drift`] if anything
//! it depends on has changed — including a file whose *contents* changed, which
//! no step of the plan would have noticed, because committing a move of a file
//! that is not the file the user looked at is exactly the sort of surprise this
//! crate exists to avoid.
//!
//! # What this module does not do
//!
//! It does not reverse anything. A failed transaction is left with its record
//! marked [`Status::Failed`] and its completed steps' receipts in it, for `mpdfm
//! recover` to roll back or roll forward (task 12). Rolling back automatically
//! would mean the one code path that can lose data — reversing a half-finished
//! transaction — ran unsupervised, with no chance for the user to look first.

use std::time::SystemTime;

use camino::Utf8Path;

use crate::config::Config;
use crate::journal::record::{Record, Status, StepRecord, TxId};
use crate::journal::store::{Kept, Pruned, Store};
use crate::library::{DirPath, Entry, Library};
use crate::mpd::state;
use crate::paths::RelPath;
use crate::playlist::PlaylistIndex;
use crate::playlist::rewrite::{self, LineEdit};
use crate::{Error, Result};

use super::effects::{Conflict, Effects};
use super::exec_fs::{self, FsError, FsStep, FsWarning};
use super::op::Plan;
use super::plan::{Live, PENDING_TX, Prefs};

/// Everything the preview was computed from, and what it produced.
///
/// All three are needed, and for different reasons: the [`Plan`] is what the
/// journal records and what re-validation re-runs, the [`Effects`] are what the
/// user actually said yes to, and the [`Library`] is the state of the files at that
/// moment — which is the only thing a fresh scan can be compared against.
///
/// The [`PlaylistIndex`] the preview used is deliberately *not* here. What matters
/// about the playlists is the edits it produced, which are in the [`Effects`], and
/// those are checked line by line against the files themselves before anything is
/// mutated ([`rewrite::prepare`]). Comparing the whole index would additionally
/// refuse a playlist that gained an unrelated line, which is not a reason to
/// refuse anything.
#[derive(Debug, Clone, Copy)]
pub struct Previewed<'a> {
    /// What the user staged.
    pub plan: &'a Plan,
    /// The library as the preview saw it.
    pub library: &'a Library,
    /// What was shown to the user and agreed to.
    pub effects: &'a Effects,
}

/// Asks MPD to rescan the directories a commit changed.
///
/// A function rather than a client, because core does not own a connection and
/// task 13 does: the CLI passes something that talks to the daemon, a test passes
/// something that fails on purpose, and this module needs to know neither.
pub type Updater<'a> = &'a dyn Fn(&[DirPath]) -> std::result::Result<(), String>;

/// What a commit is allowed to do beyond the plan itself.
///
/// [`Default`] is the production answer with no MPD: no verification hashing, no
/// injected failure, no rescan.
#[derive(Default)]
pub struct Options<'a> {
    /// `--verify`: hash every cross-device copy and read it back. Costs a second
    /// read of each copied file.
    pub verify: bool,

    /// Stop the commit dead at one of its boundaries, as a power cut would.
    /// Production passes [`Inject::Nothing`].
    pub inject: Inject,

    /// How to ask MPD to rescan, when
    /// [`Config::trigger_update_after_commit`] and [`Config::mpd_enabled`] are
    /// both on. `None` means there is nothing to ask with, which is not a
    /// failure.
    pub update: Option<Updater<'a>>,

    /// What MPD was holding when the preview was made.
    ///
    /// **It must be the same value the preview was given.** Step 1 re-validates
    /// the plan, and whether MPD answered decides whether the saved queue is
    /// rewritten or merely warned about — so passing a different answer here
    /// shows up as [`Drift::Playlists`] and refuses the commit, which is the
    /// honest outcome for a daemon that started up while the user was reading
    /// the preview.
    pub live: Live<'a>,

    /// What the user asked for about the plan as a whole — `--merge`.
    ///
    /// **It must be the same value the preview was given**, for the same reason
    /// as `live`: re-validating with [`Merge::Refuse`][super::exec_fs::Merge]
    /// a plan the user previewed with `--merge` expands into a different number
    /// of steps, which is [`Drift::Steps`].
    pub prefs: Prefs,
}

impl std::fmt::Debug for Options<'_> {
    /// Hand-written because a function pointer has no useful `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("verify", &self.verify)
            .field("inject", &self.inject)
            .field("update", &self.update.map(|_| "<fn>"))
            .field("live", &self.live)
            .field("prefs", &self.prefs)
            .finish()
    }
}

/// A failure only a test can cause, at each boundary of the sequence.
///
/// This is the crash-injection harness `docs/PLAN.md` §8 asks for. Every variant
/// stops the commit *between* two steps with the journal in whatever state that
/// step left it, and returns [`CommitError::Injected`] — it does not clean up,
/// does not mark the record failed and does not reverse anything, because a power
/// cut does none of those either. What the record says afterwards is what task 12
/// has to be able to work from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Inject {
    /// Behave normally.
    #[default]
    Nothing,

    /// After the `pending` record is durable, before the first step (3 → 4).
    /// Nothing in the library has changed, and the record proves a commit was
    /// about to start.
    AfterPending,

    /// After the step at this position has run and been journaled. Out of range
    /// means "never", so the same test can assert the uninjected run.
    AfterStep(usize),

    /// After every step, before the playlists are touched (4 → 5).
    AfterSteps,

    /// Before writing the playlist at this position in
    /// [`Effects::playlist_edits`], with every backup already taken. Passed
    /// through to [`rewrite::Inject::FailBeforeWriting`].
    BeforePlaylistWrite(usize),

    /// After the playlist and state edits, before the record says `complete`
    /// (5 → 6). The library and the playlists are consistent; only the record
    /// does not know it yet.
    AfterEdits,

    /// After the record says `complete`, before MPD is told (6 → 7).
    AfterComplete,
}

/// A committed transaction.
#[derive(Debug, Clone)]
pub struct Committed {
    /// Its id — what `mpdfm undo <txid>` takes.
    pub txid: TxId,
    /// The record as it was left, `complete`.
    pub record: Record,
    /// Everything worth telling the user that did not stop the transaction.
    pub warnings: Vec<CommitWarning>,
    /// What retention removed afterwards.
    pub pruned: Pruned,
}

impl Committed {
    /// One line for the end of `mpdfm move`.
    #[must_use]
    pub fn headline(&self) -> String {
        self.record.headline()
    }
}

/// Something a commit should mention, which did not stop it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommitWarning {
    /// A note from one of the filesystem steps — a case-insensitive collision, a
    /// directory left in place because it still holds clutter.
    #[error("{0}")]
    Fs(FsWarning),

    /// MPD could not be told to rescan. The library and the playlists are already
    /// consistent; MPD's index catches up on its next update either way.
    #[error("MPD was not told to rescan ({0}); run `mpc update` when it is back")]
    Mpd(String),

    /// Retention wanted to remove an old backup directory and did not.
    #[error("an old backup was kept: {0}")]
    Retention(Kept),

    /// Retention could not run at all.
    #[error("old backups were not pruned: {0}")]
    Pruning(String),
}

/// Why a transaction was refused, or where it stopped.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// The preview this was asked to commit is not committable. The caller should
    /// not have got this far; it is an error rather than a panic because a TUI
    /// that lets the user press `c` on a refused plan should say why, not die.
    #[error(
        "this plan cannot be committed:\n{}",
        .conflicts.iter().map(|conflict| format!("  - {conflict}")).collect::<Vec<_>>().join("\n")
    )]
    Refused {
        /// Why not.
        conflicts: Vec<Conflict>,
    },

    /// The plan does nothing. Committing it would write a journal record about
    /// having changed nothing.
    #[error("there is nothing to commit")]
    Nothing,

    /// The disk no longer matches the preview the user agreed to.
    #[error(
        "the library has changed since the preview, so this plan was not committed:\n{}\n\
         Re-scan and look at the plan again.",
        .drift.iter().map(|drift| format!("  - {drift}")).collect::<Vec<_>>().join("\n")
    )]
    Stale {
        /// Everything that is not as it was.
        drift: Vec<Drift>,
    },

    /// A filesystem step failed. The transaction stopped there, its record says
    /// `failed`, and every step that did happen has its receipt in it.
    #[error(
        "transaction {txid} stopped at step {position} ({step}): {source}\n\
         Run `mpdfm recover {txid}` to put it back."
    )]
    Step {
        /// The transaction.
        txid: TxId,
        /// Which step, counting from one as the record lists them.
        position: usize,
        /// The step, rendered.
        step: String,
        /// What the filesystem said.
        #[source]
        source: Box<FsError>,
    },

    /// A playlist could not be written. The filesystem half is done, the record
    /// says `failed`, and the playlist backups are in the transaction's backup
    /// directory.
    #[error(
        "transaction {txid} moved the files but could not rewrite the playlists: {source}\n\
         Run `mpdfm recover {txid}` to put it back."
    )]
    Edits {
        /// The transaction.
        txid: TxId,
        /// What went wrong.
        #[source]
        source: Box<Error>,
    },

    /// [`Inject`] fired. Never raised in production.
    #[error("simulated crash in transaction {txid}, {at}")]
    Injected {
        /// The transaction, whose record is what the test then inspects.
        txid: TxId,
        /// Where it stopped.
        at: &'static str,
    },
}

/// One way the disk stopped matching the preview.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Drift {
    /// A file the plan moves has been written to since the preview. The move
    /// itself would still work; what the user looked at is no longer what they
    /// would be moving.
    #[error("{path} has changed since the preview ({detail})")]
    Modified {
        /// The file.
        path: RelPath,
        /// What differs, as a phrase.
        detail: String,
    },

    /// A file the plan moves is no longer in the library.
    #[error("{path} is no longer there")]
    Vanished {
        /// The file.
        path: RelPath,
    },

    /// The plan expands into a different number of steps than it did.
    #[error("the plan now needs {now} filesystem step(s) instead of {previewed}")]
    Steps {
        /// How many the preview had.
        previewed: usize,
        /// How many a fresh scan produces.
        now: usize,
    },

    /// A step is not the step it was.
    #[error("step {at} was `{previewed}` and is now `{now}`")]
    Step {
        /// Its position, counting from one.
        at: usize,
        /// What the preview had.
        previewed: String,
        /// What a fresh scan produces.
        now: String,
    },

    /// The playlists no longer need the same edits — one of them has been
    /// written to since the preview.
    #[error("the playlist edits have changed since the preview ({detail})")]
    Playlists {
        /// What differs.
        detail: String,
    },

    /// Re-validation found something in the way that the preview did not.
    #[error("{0}")]
    Conflict(#[from] Conflict),
}

// ---------------------------------------------------------------------------

/// Commit a previewed plan with the production defaults — see [`Options`].
///
/// # Errors
///
/// [`Error::Commit`] for a plan that is refused, stale, or stopped partway
/// through; [`Error::Config`] if the library root has gone away since startup;
/// [`Error::Journal`] if the record cannot be made durable; [`Error::Io`] or
/// [`Error::Rewrite`] if a backup cannot be taken.
pub fn commit(previewed: &Previewed<'_>, config: &Config) -> Result<Committed> {
    commit_with(previewed, config, &Options::default())
}

/// [`commit`], with verification, MPD and the crash injection the acceptance
/// tests need.
///
/// # Errors
///
/// As [`commit`], plus [`CommitError::Injected`] when `options.inject` fires.
#[expect(
    clippy::too_many_lines,
    reason = "the commit sequence is one function on purpose: its ordering is the \
              invariant, and splitting it into seven would hide the order it \
              exists to guarantee"
)]
pub fn commit_with(
    previewed: &Previewed<'_>,
    config: &Config,
    options: &Options<'_>,
) -> Result<Committed> {
    // Step 0 — what the user agreed to has to be committable at all.
    if !previewed.effects.conflicts.is_empty() {
        return Err(CommitError::Refused {
            conflicts: previewed.effects.conflicts.clone(),
        }
        .into());
    }
    if previewed.effects.fs_steps.is_empty() {
        return Err(CommitError::Nothing.into());
    }
    // Step 1 — re-validate against a fresh scan.
    let root = config.require_music_dir()?.to_owned();
    let fresh_library = Library::scan(&root)?;
    let (fresh_index, _warnings) = PlaylistIndex::load(&config.playlist_dir);
    let fresh = previewed.plan.validate_with(
        &fresh_library,
        &fresh_index,
        config,
        &options.live,
        options.prefs,
    );
    let drift = drift(previewed, &fresh_library, &fresh);
    if !drift.is_empty() {
        return Err(CommitError::Stale { drift }.into());
    }

    let store = Store::at(&config.data_dir);
    let txid = TxId::now();
    let started = SystemTime::now();
    let mut warnings = Vec::new();

    // Step 2 — the backups, before the record and long before the first mutation.
    // A backup directory with no record is an orphan in MPDFM's own data
    // directory; a record naming backups that are not there would be a promise it
    // cannot keep, and `undo` believes the record.
    store.create_dirs()?;
    let backup_dir = store.create_backup_dir(&txid)?;
    let steps = retarget_backups(&fresh.fs_steps, config, &backup_dir);
    create_backup_parents(&steps)?;
    // Reading and checking every playlist here rather than at step 5 is what
    // makes a playlist that was edited since the preview stop the transaction
    // before the first file moves.
    let playlists = rewrite::prepare(&fresh.playlist_edits)?;
    playlists.back_up(&backup_dir)?;
    // The same reasoning for MPD's saved queue: a state file the daemon has
    // saved since the preview must stop the transaction here, with nothing moved,
    // rather than at step 5 with the library already rearranged.
    let saved_queue = prepare_state_file(config, &fresh.state_edits)?;
    let state_backup = back_up_state_file(config, &backup_dir)?;

    // Step 3 — the record, durable, before anything is mutated.
    let mut record = Record::opening(
        txid.clone(),
        started,
        root.clone(),
        config.playlist_dir.clone(),
        backup_dir.clone(),
    );
    record.ops = previewed.plan.ops().to_vec();
    record.steps = steps.iter().cloned().map(StepRecord::planned).collect();
    record.playlist_edits = fresh.playlist_edits.clone();
    record.state_edits = fresh.state_edits.clone();
    record.state_backup = state_backup;
    record.summary = fresh.summary;
    store.write(&record)?;
    if options.inject == Inject::AfterPending {
        return Err(injected(&txid, "after the pending record was written"));
    }

    // Step 4 — the filesystem, one journaled step at a time.
    let mut fs_options = exec_fs::Options::from_config(config);
    if options.verify {
        fs_options = fs_options.verifying();
    }
    for (position, step) in steps.iter().enumerate() {
        match exec_fs::execute_with(step, &root, &fs_options) {
            Ok(receipt) => {
                warnings.extend(receipt.warnings.iter().cloned().map(CommitWarning::Fs));
                record.steps[position].completed(receipt);
                // After the step, not before: a record that claimed a step was
                // done before it was would send `recover` looking for a file that
                // is still where it started. Appended rather than written whole,
                // which is what keeps a three-thousand-step transaction's journal
                // linear instead of quadratic — see `journal::store`.
                store.append_step(&txid, position, &record.steps[position])?;
            }
            Err(err) => {
                record.steps[position].failed(&err);
                record.finish(Status::Failed, SystemTime::now());
                store.write(&record)?;
                store.forget_steps(&txid);
                return Err(CommitError::Step {
                    txid,
                    position: position + 1,
                    step: step.to_string(),
                    source: Box::new(err),
                }
                .into());
            }
        }
        if options.inject == Inject::AfterStep(position) {
            return Err(injected(&txid, "after a filesystem step"));
        }
    }
    if options.inject == Inject::AfterSteps {
        return Err(injected(&txid, "after the filesystem steps"));
    }

    // Step 5 — the playlists, and MPD's saved queue.
    let inject = match options.inject {
        Inject::BeforePlaylistWrite(at) => rewrite::Inject::FailBeforeWriting(at),
        _ => rewrite::Inject::Nothing,
    };
    // The saved queue goes last of the two: its bytes were computed and verified
    // at step 2, so the only thing that can fail here is the write itself, and a
    // failed state-file write with every playlist already correct is the smaller
    // half to recover.
    let edits = playlists
        .write_with(inject)
        .and_then(|()| saved_queue.as_ref().map_or(Ok(()), state::Prepared::write));
    if let Err(err) = edits {
        // An injected failure is a crash, not a failure: it leaves the record
        // exactly as a power cut would, which is the state task 12 has to cope
        // with. A real one is recorded.
        if matches!(options.inject, Inject::BeforePlaylistWrite(_))
            && matches!(err, Error::Rewrite(rewrite::RewriteError::Injected { .. }))
        {
            return Err(injected(&txid, "between two playlist writes"));
        }
        record.finish(Status::Failed, SystemTime::now());
        store.write(&record)?;
        store.forget_steps(&txid);
        return Err(CommitError::Edits {
            txid,
            source: Box::new(err),
        }
        .into());
    }
    if options.inject == Inject::AfterEdits {
        return Err(injected(&txid, "after the playlist edits"));
    }

    // Step 6 — and it is done. The record now lists every step as the log did,
    // so the log has nothing left to say.
    record.finish(Status::Complete, SystemTime::now());
    store.write(&record)?;
    store.forget_steps(&txid);
    if options.inject == Inject::AfterComplete {
        return Err(injected(&txid, "after the record was completed"));
    }

    // Step 7 — MPD, which cannot fail a transaction that is already consistent.
    if let Some(update) = options.update
        && config.mpd_enabled
        && config.trigger_update_after_commit
    {
        let dirs = affected_dirs(&steps);
        record.mpd_update_requested = true;
        record.mpd_update_dirs = dirs.clone();
        if let Err(message) = update(&dirs) {
            record.mpd_update_failed = Some(message.clone());
            warnings.push(CommitWarning::Mpd(message));
        }
        // Not fatal: the transaction is complete either way, and a record that
        // cannot say whether MPD was told is a smaller problem than a commit that
        // failed after finishing.
        if let Err(err) = store.write(&record) {
            warnings.push(CommitWarning::Mpd(err.to_string()));
        }
    }

    // Retention, which is housekeeping and never fails a commit.
    let pruned = match store.prune(config.backup_keep) {
        Ok(pruned) => {
            warnings.extend(pruned.kept.iter().cloned().map(CommitWarning::Retention));
            pruned
        }
        Err(err) => {
            warnings.push(CommitWarning::Pruning(err.to_string()));
            Pruned::default()
        }
    };

    Ok(Committed {
        txid,
        record,
        warnings,
        pruned,
    })
}

/// [`CommitError::Injected`], for the one-line call sites above.
fn injected(txid: &TxId, at: &'static str) -> Error {
    CommitError::Injected {
        txid: txid.clone(),
        at,
    }
    .into()
}

// ---------------------------------------------------------------------------
// Step 1 — what changed since the preview.

/// Everything about the disk that no longer matches what the user agreed to.
///
/// All of it, not the first one: a user who is told "and four other things have
/// changed" rescans once instead of four times.
fn drift(previewed: &Previewed<'_>, fresh_library: &Library, fresh: &Effects) -> Vec<Drift> {
    let mut drift: Vec<Drift> = fresh
        .conflicts
        .iter()
        .cloned()
        .map(Drift::Conflict)
        .collect();

    let before = &previewed.effects.fs_steps;
    let now = &fresh.fs_steps;
    if before.len() != now.len() {
        drift.push(Drift::Steps {
            previewed: before.len(),
            now: now.len(),
        });
    }
    for (at, (was, is)) in before.iter().zip(now).enumerate() {
        if was != is {
            drift.push(Drift::Step {
                at: at + 1,
                previewed: was.to_string(),
                now: is.to_string(),
            });
        }
    }

    if previewed.effects.playlist_edits != fresh.playlist_edits {
        drift.push(Drift::Playlists {
            detail: playlist_detail(&previewed.effects.playlist_edits, &fresh.playlist_edits),
        });
    }
    if previewed.effects.state_edits != fresh.state_edits {
        drift.push(Drift::Playlists {
            detail: "MPD's saved queue no longer needs the same lines changed".to_owned(),
        });
    }

    // The files themselves. Nothing above would notice a track that was written
    // to without changing name: the steps are paths, and a plan made of paths is
    // the same plan whatever the bytes say.
    for path in sources(before) {
        match (previewed.library.get(&path), fresh_library.get(&path)) {
            (Some(was), Some(is)) => {
                if let Some(detail) = difference(was, is) {
                    drift.push(Drift::Modified { path, detail });
                }
            }
            (Some(_), None) => drift.push(Drift::Vanished { path }),
            // A path the preview did not have either is not something that
            // changed; `validate` has already reported it as a conflict.
            (None, _) => {}
        }
    }

    drift
}

/// Every path a plan reads from — the ones whose bytes the user is agreeing to
/// move, deduplicated and in step order.
fn sources(steps: &[FsStep]) -> Vec<RelPath> {
    let mut seen = std::collections::BTreeSet::new();
    steps
        .iter()
        .filter_map(|step| match step {
            FsStep::RenameFile { from, .. } | FsStep::CopyDelete { from, .. } => Some(from),
            FsStep::RemoveFile { target, .. } => Some(target),
            FsStep::MkDir { .. } | FsStep::RmDirIfEmpty { .. } => None,
        })
        .filter(|path| seen.insert((*path).clone()))
        .cloned()
        .collect()
}

/// How one library entry differs from the same entry rescanned, as a phrase, or
/// `None` when it does not.
fn difference(was: &Entry, is: &Entry) -> Option<String> {
    if was.size != is.size {
        return Some(format!("it was {} bytes and is now {}", was.size, is.size));
    }
    if was.mtime != is.mtime {
        return Some("its modification time is newer".to_owned());
    }
    None
}

/// What differs between two sets of playlist edits, for the message.
fn playlist_detail(before: &[rewrite::PlaylistEdit], now: &[rewrite::PlaylistEdit]) -> String {
    if before.len() != now.len() {
        return format!(
            "{} playlist(s) were affected and now {} are",
            before.len(),
            now.len()
        );
    }
    for (was, is) in before.iter().zip(now) {
        if was.file_name != is.file_name {
            return format!("{} is affected instead of {}", is.file_name, was.file_name);
        }
        if was.line_edits != is.line_edits {
            return format!("{} has been edited since the preview", is.file_name);
        }
    }
    "they are no longer the same edits".to_owned()
}

// ---------------------------------------------------------------------------
// Step 2 — the backups.

/// Point every delete's backup at this transaction's directory.
///
/// The planner cannot: it has no transaction id, so it writes
/// [`PENDING_TX`] where the id goes ([`plan`][super::plan]) and the substitution
/// happens here, where the id exists. A step whose backup is somewhere else
/// entirely is left alone — it is not this scheme's to rewrite, and
/// [`exec_fs::check`] will have an opinion about it.
fn retarget_backups(steps: &[FsStep], config: &Config, backup_dir: &Utf8Path) -> Vec<FsStep> {
    let pending = config.data_dir.join("backups").join(PENDING_TX);
    steps
        .iter()
        .map(|step| match step {
            FsStep::RemoveFile {
                target,
                backup: Some(backup),
            } => FsStep::RemoveFile {
                target: target.clone(),
                backup: Some(match backup.strip_prefix(&pending) {
                    Ok(rest) => backup_dir.join(rest),
                    Err(_) => backup.clone(),
                }),
            },
            other => other.clone(),
        })
        .collect()
}

/// Create the directories a delete's backup needs.
///
/// A backup mirrors the library path under `<backup_dir>/files/`, so deleting
/// `hiphop/x/cover.jpg` needs `<backup_dir>/files/hiphop/x/` to exist —
/// [`exec_fs`] deliberately does not create it, because a step that invents
/// directories outside the library is a step that can put one in the wrong place.
///
/// # Errors
///
/// [`Error::Io`] if one cannot be created.
fn create_backup_parents(steps: &[FsStep]) -> Result<()> {
    for step in steps {
        let FsStep::RemoveFile {
            backup: Some(backup),
            ..
        } = step
        else {
            continue;
        };
        let Some(parent) = backup.parent() else {
            continue;
        };
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_string(),
            source,
        })?;
    }
    Ok(())
}

/// Read MPD's saved queue and work out the bytes `state_edits` leaves in it,
/// writing nothing.
///
/// `None` when there is nothing to do: no edits (which is also what a reachable
/// daemon produces — see [`Live`]), the rewrite switched off, or no `state_file`
/// configured. A non-empty edit list with no state file to apply it to cannot
/// happen, since the edits were planned from that very file; it is treated as
/// nothing to do rather than as a reason to fail a transaction.
///
/// # Errors
///
/// [`Error::Io`] if the file cannot be read and [`Error::State`] if an edit no
/// longer matches it — MPD saved its state between the preview and the commit.
fn prepare_state_file(
    config: &Config,
    state_edits: &[LineEdit],
) -> Result<Option<state::Prepared>> {
    if state_edits.is_empty() || !config.rewrite_saved_queue {
        return Ok(None);
    }
    let Some(state_file) = &config.state_file else {
        return Ok(None);
    };
    state::prepare(state_file, state_edits).map(Some)
}

/// Copy MPD's state file into the transaction's backup directory.
///
/// Taken whenever [`Config::rewrite_saved_queue`] is on and the file is there,
/// whether or not there is anything to change in it — a few kilobytes of
/// insurance against the file MPDFM is configured to edit.
/// [`Record::state_edits`] stays the authority on whether it was actually
/// changed, and undo must not restore a state file that no edit named.
///
/// # Errors
///
/// [`Error::Io`] if the file exists and cannot be copied.
fn back_up_state_file(config: &Config, backup_dir: &Utf8Path) -> Result<Option<String>> {
    if !config.rewrite_saved_queue {
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

// ---------------------------------------------------------------------------
// Step 7 — what to tell MPD.

/// The directories MPD should rescan: the shallowest ones that cover everything
/// the transaction touched.
///
/// `update` is recursive, so naming a directory and its child is asking for the
/// same work twice; and a directory the transaction removed must not be named at
/// all, which is why a source directory is represented by its parent.
#[must_use]
pub fn affected_dirs(steps: &[FsStep]) -> Vec<DirPath> {
    let mut dirs: Vec<DirPath> = Vec::new();
    for step in steps {
        match step {
            FsStep::MkDir { at } => dirs.push(DirPath::from(at.clone())),
            FsStep::RenameFile { from, to } | FsStep::CopyDelete { from, to } => {
                dirs.push(DirPath::of(from));
                dirs.push(DirPath::of(to));
            }
            FsStep::RemoveFile { target, .. } => dirs.push(DirPath::of(target)),
            // The directory may well be gone; its parent covers it.
            FsStep::RmDirIfEmpty { at } => {
                dirs.push(
                    DirPath::from(at.clone())
                        .parent()
                        .unwrap_or_else(DirPath::root),
                );
            }
        }
    }

    dirs.sort_unstable();
    dirs.dedup();
    // The root covers everything, so it is the whole answer when it is in there.
    if dirs.iter().any(DirPath::is_root) {
        return vec![DirPath::root()];
    }
    // Sorted, so an ancestor always comes before what it contains.
    let mut minimal: Vec<DirPath> = Vec::new();
    for dir in dirs {
        if !minimal.iter().any(|kept| dir.starts_with_dir(kept)) {
            minimal.push(dir);
        }
    }
    minimal
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::parse(path).expect("a valid path")
    }

    fn dir(path: &str) -> DirPath {
        DirPath::parse(path).expect("a valid directory")
    }

    #[test]
    fn a_backup_path_is_moved_from_the_pending_name_to_the_transaction() {
        let mut config = Config {
            data_dir: Utf8PathBuf::from("/data/mpdfm"),
            ..blank_config()
        };
        config.delete_enabled = true;
        let steps = [
            FsStep::RemoveFile {
                target: rel("a/x.mp3"),
                backup: Some(Utf8PathBuf::from(
                    "/data/mpdfm/backups/pending/files/a/x.mp3",
                )),
            },
            // Somebody else's idea of a backup path: left exactly as it is.
            FsStep::RemoveFile {
                target: rel("a/y.mp3"),
                backup: Some(Utf8PathBuf::from("/elsewhere/y.mp3")),
            },
        ];

        let moved = retarget_backups(&steps, &config, Utf8Path::new("/data/mpdfm/backups/tx1"));

        assert_eq!(
            moved[0].to_string(),
            "remove a/x.mp3 (backup /data/mpdfm/backups/tx1/files/a/x.mp3)"
        );
        assert_eq!(moved[1], steps[1]);
    }

    #[test]
    fn mpd_is_asked_about_the_shallowest_directories_that_cover_the_work() {
        let steps = [
            FsStep::MkDir {
                at: rel("hiphop/MF DOOM"),
            },
            FsStep::RenameFile {
                from: rel("hiphop/MF DOOM - Mm..Food/01.mp3"),
                to: rel("hiphop/MF DOOM/Mm..Food/01.mp3"),
            },
            FsStep::RmDirIfEmpty {
                at: rel("hiphop/MF DOOM - Mm..Food"),
            },
        ];

        // `hiphop/MF DOOM/Mm..Food` is inside `hiphop/MF DOOM`, and
        // `hiphop/MF DOOM - Mm..Food`'s parent is `hiphop`, which covers both.
        assert_eq!(affected_dirs(&steps), vec![dir("hiphop")]);
    }

    #[test]
    fn a_top_level_move_asks_mpd_to_rescan_everything() {
        let steps = [FsStep::RenameFile {
            from: rel("a.mp3"),
            to: rel("b.mp3"),
        }];
        assert_eq!(affected_dirs(&steps), vec![DirPath::root()]);
    }

    /// A `Config` with nothing but the fields a unit test here sets. The real
    /// constructor is `config::resolve`, and `testing::Fixture::config` is what
    /// the integration tests use.
    fn blank_config() -> Config {
        let (config, _warnings) = crate::config::resolve(
            &crate::config::Overrides::default(),
            &crate::config::Env::empty(),
        );
        config
    }
}
