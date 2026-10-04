//! Everything a [`Plan`][super::op::Plan] would change, worked out without
//! changing any of it.
//!
//! [`Effects`] is the single answer to "what would this do?", and it is the only
//! answer: the CLI's `--dry-run`, the CLI's `--json`, the TUI's pending view
//! (task 24) and the journal (task 11) all read this one value. Two renderings of
//! a plan that disagree is the failure mode this type exists to make impossible,
//! so [`Effects::render`] lives beside it in [`render`][super::render] rather
//! than in each front-end.
//!
//! # Conflicts refuse, warnings inform
//!
//! The split is the whole safety model in one sentence. A [`Conflict`] means
//! MPDFM cannot do what was asked without losing something it was not asked to
//! lose, so commit is refused ([`Effects::is_committable`]). A [`Warning`] means
//! the commit will do exactly what was asked and the user may not have realised
//! what that is — a playlist losing lines, an album ending up split over two
//! directories. Warnings never block; a user who is told and proceeds has
//! decided.
//!
//! Nothing here raises. `validate` collects every conflict it finds and carries
//! on, because a preview that stops at the first problem makes the user fix a
//! five-problem plan five times.

use camino::Utf8PathBuf;

use crate::library::DirPath;
use crate::paths::RelPath;
use crate::playlist::rewrite::{LineEdit, PlaylistEdit};

use super::exec_fs::FsStep;
use super::op::Operation;

/// What a plan would do.
///
/// Plain owned data throughout — no borrows of the library it was computed from,
/// no file handles — so it can be held across a redraw, serialized for `--json`,
/// and written into the journal that task 11 replays.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Effects {
    /// One entry per staged operation, in execution order, with what it costs.
    ///
    /// The preview's rows and the pending view's rows are these: the user staged
    /// operations and wants to see operations, not the 3 000 steps one of them
    /// expanded into. An operation a conflict refused is still here, marked
    /// [`OpEffect::refused`], because a row that vanishes is a row the user
    /// cannot put the cursor on to fix.
    pub ops: Vec<OpEffect>,

    /// Every filesystem change, fully expanded and in execution order. Handing
    /// these to [`exec_fs::execute_with`][super::exec_fs::execute_with] one after
    /// another is what commit does.
    pub fs_steps: Vec<FsStep>,

    /// The playlist lines that change, by playlist. Produced by
    /// [`plan_playlist_edits`][crate::playlist::rewrite::plan_playlist_edits],
    /// which is also what applies them — the preview and the write cannot
    /// disagree about which lines are affected because they are the same value.
    pub playlist_edits: Vec<PlaylistEdit>,

    /// The same, for MPD's saved queue in `state`, indexed into
    /// [`MpdState::lines`][crate::mpd::state::MpdState::lines].
    ///
    /// Empty whenever there is nothing to do to that file — and also, by design,
    /// whenever **MPD is reachable**: the daemon rewrites this file from memory
    /// when it shuts down, so an on-disk edit behind a running MPD would be
    /// erased. In that case the queue turns up as [`Warning::InMpdQueue`]
    /// instead, one per moved file that is in it. See
    /// [`Live`][super::Live] and `crate::mpd::state`.
    pub state_edits: Vec<LineEdit>,

    /// Reasons this plan will not be committed. Non-empty means refused.
    pub conflicts: Vec<Conflict>,

    /// Things the user should know that do not stop the commit.
    pub warnings: Vec<Warning>,

    /// The counts the preview leads with.
    pub summary: Summary,
}

impl Effects {
    /// Whether commit may proceed: no conflicts, and something to do.
    #[must_use]
    pub fn is_committable(&self) -> bool {
        self.conflicts.is_empty() && !self.fs_steps.is_empty()
    }
}

/// One staged operation, and what validating it worked out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpEffect {
    /// Its index in [`Plan::ops`][super::op::Plan::ops] — the order the user
    /// staged it in, which is how every [`Conflict`] names it. The position of
    /// this row is the order it will *execute* in, and the two differ whenever a
    /// chain had to be sorted.
    pub index: usize,
    /// What the user asked for.
    pub op: Operation,
    /// Files this operation moves or deletes.
    pub files: usize,
    /// Of those, the ones MPD indexes as music.
    pub audio: usize,
    /// Their total size.
    pub bytes: u64,
    /// Playlists that name at least one of them.
    pub playlists: usize,
    /// Whether a [`Conflict`] names this operation. The commit is refused as a
    /// whole either way; this says which row to look at.
    pub refused: bool,
}

// ---------------------------------------------------------------------------

/// Why a plan is refused.
///
/// Each names the staged operation it came from by index into
/// [`Plan::ops`][super::op::Plan::ops], so the pending view can put the cursor on
/// the offending row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum Conflict {
    /// The destination is occupied by something this plan does not move out of
    /// the way first. MPDFM never overwrites.
    #[error("operation {op}: {at} already exists")]
    DestinationExists {
        /// Index into the plan's operations.
        op: usize,
        /// The occupied path.
        at: RelPath,
    },

    /// Two staged operations want the same destination. Whichever ran second
    /// would find it occupied; refusing both is the honest answer.
    #[error("operations {first} and {second} both want to create {at}")]
    DuplicateDestination {
        /// The earlier operation.
        first: usize,
        /// The later one.
        second: usize,
        /// What they collide over.
        at: RelPath,
    },

    /// The operations depend on each other in a loop — a swap, `a → b` and
    /// `b → a`, or any longer ring — so no execution order exists. A chain that
    /// is merely out of order is not this: it is sorted and committed (see
    /// [`plan`][super::plan]).
    #[error("these operations depend on each other in a loop: {}", .paths.iter().map(RelPath::as_str).collect::<Vec<_>>().join(", "))]
    Cycle {
        /// The operations in the loop, by index.
        ops: Vec<usize>,
        /// The paths they pass between them, for the message.
        paths: Vec<RelPath>,
    },

    /// The source is not in the library. Either it never was, or the model is
    /// stale and a rescan is due.
    #[error("operation {op}: {at} is not in the library")]
    SourceMissing {
        /// Index into the plan's operations.
        op: usize,
        /// The path that is not there.
        at: RelPath,
    },

    /// A path that leaves the music directory — a `..`, an absolute path, a
    /// destination that climbs out. Safety invariant 5: MPDFM writes inside the
    /// three roots it was configured with and nowhere else.
    #[error("operation {op}: {path} is outside the library: {reason}")]
    OutsideRoot {
        /// Index into the plan's operations.
        op: usize,
        /// The path, rendered for the message only.
        path: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A directory MPDFM has to write in does not grant write permission.
    ///
    /// This is the advisory answer — the mode bits. Commit probes for real by
    /// creating a file; a directory that passes here can still fail there, and
    /// the preview says "looks writable", never "is writable".
    #[error("operation {op}: {dir} is not writable")]
    NotWritable {
        /// Index into the plan's operations.
        op: usize,
        /// The directory.
        dir: Utf8PathBuf,
    },

    /// A delete was staged while [`Config::delete_enabled`][crate::config::Config::delete_enabled]
    /// is false. The configuration is the answer, not a prompt.
    #[error("operation {op}: refusing to delete {target}: delete_enabled is false")]
    DeleteDisabled {
        /// Index into the plan's operations.
        op: usize,
        /// The file that would have been removed.
        target: RelPath,
    },

    /// The source could not be examined at all — an unreadable directory, an I/O
    /// error during expansion. Rendered from the underlying failure because
    /// there is nothing useful to add to it.
    #[error("operation {op}: {message}")]
    Unreadable {
        /// Index into the plan's operations.
        op: usize,
        /// What went wrong.
        message: String,
    },
}

impl Conflict {
    /// Every staged operation this is about: one for most, both ends of a
    /// [`Conflict::DuplicateDestination`], the whole ring of a
    /// [`Conflict::Cycle`].
    #[must_use]
    pub fn ops(&self) -> Vec<usize> {
        match self {
            Self::DestinationExists { op, .. }
            | Self::SourceMissing { op, .. }
            | Self::OutsideRoot { op, .. }
            | Self::NotWritable { op, .. }
            | Self::DeleteDisabled { op, .. }
            | Self::Unreadable { op, .. } => vec![*op],
            Self::DuplicateDestination { first, second, .. } => vec![*first, *second],
            Self::Cycle { ops, .. } => ops.clone(),
        }
    }

    /// The one operation to put the cursor on: the first of [`Conflict::ops`].
    #[must_use]
    pub fn op(&self) -> usize {
        self.ops().first().copied().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------

/// Something the user should know, which does not stop the commit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum Warning {
    /// A playlist loses lines. The one thing in a commit that destroys something
    /// the user wrote rather than relocating it, so the preview gives it its own
    /// line and its own count.
    #[error("{playlist}: {lines} line(s) will be removed, not rewritten")]
    PlaylistLinesRemoved {
        /// The playlist's file name.
        playlist: String,
        /// How many lines go.
        lines: usize,
    },

    /// A file being moved is in MPD's **live** queue.
    ///
    /// The daemon holds that queue in memory and writes it over the state file
    /// when it stops, so there is nothing MPDFM can edit that would survive:
    /// those entries point at nothing until they are requeued. Raised instead of
    /// an [`Effects::state_edits`] whenever MPD answered — see
    /// [`Live`][super::Live].
    #[error("{path} is in MPD's current queue and will need a requeue")]
    InMpdQueue {
        /// The file.
        path: RelPath,
    },

    /// MPD's state file is configured and could not be read, so its saved queue
    /// was neither examined nor rewritten.
    ///
    /// Informational rather than refusing: the queue is MPD's cache of what the
    /// user was listening to, and a move that leaves it stale is a worse outcome
    /// than a move that does not happen only in the eyes of someone who was not
    /// asking to move anything.
    #[error("{path}: MPD's saved queue was not examined: {reason}")]
    StateUnreadable {
        /// The state file, as configured.
        path: String,
        /// Why it could not be read.
        reason: String,
    },

    /// A playlist entry near the affected paths already does not resolve. Not
    /// caused by this plan and not fixed by it — worth saying now, because after
    /// the commit it will look like the commit broke it.
    #[error("{playlist} already has a reference that does not resolve: {path}")]
    BrokenReferenceNearby {
        /// The playlist's file name.
        playlist: String,
        /// The entry that does not resolve.
        path: String,
    },

    /// The destination differs from something already in that directory only by
    /// case. On ext4 both can exist and the move is fine; the warning is here
    /// because copying such a library onto a case-insensitive filesystem loses
    /// one of them. On a case-insensitive filesystem the destination genuinely
    /// exists and this is a [`Conflict::DestinationExists`] instead.
    #[error("{at} differs from {existing} only by case")]
    CaseDifference {
        /// The destination.
        at: RelPath,
        /// The name already there, as the directory spells it.
        existing: String,
    },

    /// An entry in an affected directory that MPDFM cannot name — invalid UTF-8,
    /// or a character a playlist path cannot hold — so it stays where it is
    /// while everything around it moves.
    #[error("{path} cannot be named by MPDFM and will stay where it is: {reason}")]
    SkippedUnnamable {
        /// The path, rendered lossily. Never write it back to disk.
        path: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A symlink inside a directory being moved. Reported and left in place:
    /// MPDFM does not move what it has not resolved.
    #[error("{at} is a symlink and will stay where it is")]
    SkippedSymlink {
        /// The link.
        at: RelPath,
    },

    /// Some but not all of an album directory's audio files are being moved out
    /// of it, so the album ends up in two places. Legal, occasionally intended,
    /// almost never what someone meant to do to a 14-track album.
    #[error("{album} will be split: {moved} of {total} tracks move away")]
    AlbumSplit {
        /// The album directory as it is now.
        album: DirPath,
        /// How many of its audio files move.
        moved: usize,
        /// How many it has.
        total: usize,
    },
}

// ---------------------------------------------------------------------------

/// The counts the preview leads with, and the ones commit is checked against.
///
/// Every field is derived from [`Effects::fs_steps`] and
/// [`Effects::playlist_edits`] rather than counted alongside them, so a summary
/// that disagrees with what commit does is a bug in one function instead of a
/// drift between two.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Files that change path.
    pub files_moved: usize,
    /// Of those, the ones MPD indexes as music.
    pub audio_moved: usize,
    /// Files removed into the backup directory.
    pub files_deleted: usize,
    /// Directories that will be created. An upper bound: `MkDir` is idempotent,
    /// so one whose directory already exists is a no-op at commit time.
    pub dirs_created: usize,
    /// Directories offered up for removal once they are empty. An upper bound:
    /// a directory that still holds something is left alone at commit time.
    pub dirs_removed: usize,
    /// Total size of everything that moves or is deleted.
    pub bytes: u64,
    /// Playlists with at least one changed line.
    pub playlists_affected: usize,
    /// Lines whose path is rewritten.
    pub lines_rewritten: usize,
    /// Lines that go away, `#EXTINF` included.
    pub lines_removed: usize,
}

impl Summary {
    /// Derive the counts from what was actually planned.
    pub(super) fn of(fs_steps: &[FsStep], playlist_edits: &[PlaylistEdit], bytes: u64) -> Self {
        let mut summary = Self {
            bytes,
            ..Self::default()
        };

        for step in fs_steps {
            match step {
                FsStep::MkDir { .. } => summary.dirs_created += 1,
                FsStep::RenameFile { to, .. } | FsStep::CopyDelete { to, .. } => {
                    summary.files_moved += 1;
                    if crate::library::Kind::of(to).is_audio() {
                        summary.audio_moved += 1;
                    }
                }
                FsStep::RemoveFile { .. } => summary.files_deleted += 1,
                FsStep::RmDirIfEmpty { .. } => summary.dirs_removed += 1,
            }
        }

        summary.playlists_affected = playlist_edits.len();
        for edit in playlist_edits {
            summary.lines_rewritten += edit.rewrites();
            summary.lines_removed += edit.removals();
        }

        summary
    }

    /// Whether this plan changes nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
