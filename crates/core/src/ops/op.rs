//! What the user asked for, before anything has been worked out about it.
//!
//! An [`Operation`] is coarse and declarative — "move this album there" — and a
//! [`Plan`] is a list of them. Neither knows how many files that is, whether the
//! destination is free, or which playlist lines it touches; that is
//! [`Plan::validate`]'s job, and it is in [`plan`][super::plan] because it is
//! considerably longer than this.
//!
//! Keeping the two apart is what makes staging work (`docs/PLAN.md` D7). A user
//! adds operations in any order, removes one, adds another; the answer to "what
//! would this do?" is recomputed from scratch each time rather than patched. So
//! an `Operation` has to be cheap to hold, cheap to compare, and free of anything
//! derived — no step counts, no byte totals, no conflict flags.

use crate::config::Config;
use crate::library::Library;
use crate::paths::RelPath;
use crate::playlist::PlaylistIndex;

use super::effects::Effects;
use super::plan::{Live, Prefs};

/// One thing the user asked for.
///
/// The granularity is the user's, not the filesystem's: moving an album is one
/// operation however many files are in it. [`Plan::validate`] expands it into
/// [`FsStep`][super::exec_fs::FsStep]s, which is where the per-file detail — and
/// the journal's per-file reversibility — comes from.
///
/// # Not here yet
///
/// `WriteTags { target, changes: TagDelta }` belongs in this enum and is left
/// out until M2 defines `TagDelta` (tasks 16–18). Adding a placeholder now would
/// mean guessing at the shape of a multi-valued FLAC field, which is an open
/// question in the roadmap. The preview's `TAG` row arrives with it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Operation {
    /// Move one file. Aux files stay where they are: this is a file move, and
    /// the user named the file.
    MoveFile {
        /// Where it is now.
        from: RelPath,
        /// Where it should be.
        to: RelPath,
    },

    /// Move a directory and everything under it, aux files included.
    MoveDir {
        /// Where it is now.
        from: RelPath,
        /// Where it should be.
        to: RelPath,
    },

    /// Delete one file, into the transaction's backup directory.
    ///
    /// This is the only operation that can lose a playlist line rather than
    /// rewrite one, and the only one the configuration can forbid outright
    /// ([`Config::delete_enabled`]).
    Delete {
        /// The file to remove.
        target: RelPath,
    },
}

impl Operation {
    /// The path this operation reads from — the one that has to exist.
    #[must_use]
    pub fn source(&self) -> &RelPath {
        match self {
            Self::MoveFile { from, .. } | Self::MoveDir { from, .. } => from,
            Self::Delete { target } => target,
        }
    }

    /// The path this operation writes to — the one that has to be free. `None`
    /// for a delete, which creates nothing.
    #[must_use]
    pub fn destination(&self) -> Option<&RelPath> {
        match self {
            Self::MoveFile { to, .. } | Self::MoveDir { to, .. } => Some(to),
            Self::Delete { .. } => None,
        }
    }

    /// Whether this operation vacates `path` — whether `path` is gone once it has
    /// run.
    ///
    /// For a directory move that is the directory *and* everything under it,
    /// which is what lets `a/ → b/` free up `a/` for a later operation. The test
    /// is [`RelPath::starts_with_dir`], component-wise, so `hiphop/MF DOOM` does
    /// not vacate anything under `hiphop/MF DOOM Instrumentals`.
    #[must_use]
    pub fn vacates(&self, path: &RelPath) -> bool {
        match self {
            Self::MoveFile { from, .. } | Self::Delete { target: from } => from == path,
            Self::MoveDir { from, .. } => path == from || path.starts_with_dir(from),
        }
    }

    /// The verb the preview labels this operation with.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::MoveFile { .. } => "MOVE",
            Self::MoveDir { .. } => "MOVE",
            Self::Delete { .. } => "DELETE",
        }
    }
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MoveFile { from, to } => write!(f, "move {from} -> {to}"),
            Self::MoveDir { from, to } => write!(f, "move {from}/ -> {to}/"),
            Self::Delete { target } => write!(f, "delete {target}"),
        }
    }
}

// ---------------------------------------------------------------------------

/// A set of staged operations, in the order the user added them.
///
/// The order they are *added* in carries no meaning — [`Plan::validate`] works
/// out the order they have to be *executed* in, so that `a → b, b → c` does the
/// right thing whichever way round it was staged. See
/// [`Conflict::Cycle`][super::effects::Conflict::Cycle] for the case where no
/// such order exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    ops: Vec<Operation>,
}

impl Plan {
    /// An empty plan.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A plan holding these operations.
    #[must_use]
    pub fn of(ops: Vec<Operation>) -> Self {
        Self { ops }
    }

    /// Stage one more operation.
    pub fn push(&mut self, op: Operation) -> &mut Self {
        self.ops.push(op);
        self
    }

    /// Unstage the operation at `index`, as the pending view's `d` key will.
    ///
    /// Returns what was removed, or `None` if there was nothing there.
    pub fn remove(&mut self, index: usize) -> Option<Operation> {
        (index < self.ops.len()).then(|| self.ops.remove(index))
    }

    /// Everything staged, in the order it was added.
    #[must_use]
    pub fn ops(&self) -> &[Operation] {
        &self.ops
    }

    /// How many operations are staged.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether nothing is staged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Work out everything this plan would change, touching nothing.
    ///
    /// Pure in the sense that matters: it reads the library, the playlist index
    /// and the filesystem, and writes to none of them. The write-permission check
    /// it performs is the advisory one ([`exec_fs::check`][super::exec_fs::check],
    /// mode bits); the authoritative probe writes a temp file and so belongs to
    /// commit, not to the preview.
    ///
    /// Nothing is raised: a plan that cannot be committed comes back with
    /// [`Effects::conflicts`] non-empty and everything else filled in anyway, so
    /// that the preview can show the user all of what is wrong at once instead of
    /// the first thing.
    #[must_use]
    pub fn validate(&self, lib: &Library, idx: &PlaylistIndex, cfg: &Config) -> Effects {
        self.validate_with(lib, idx, cfg, &Live::default(), Prefs::default())
    }

    /// [`Plan::validate`], told what MPD is holding in memory.
    ///
    /// The one answer that changes: with a live queue in hand the preview warns
    /// about MPD's saved queue instead of rewriting its file, because the daemon
    /// overwrites that file from memory when it stops. See [`Live`].
    ///
    /// A caller that does not talk to MPD — every test that is not about this,
    /// and `--no-mpd` — wants [`Plan::validate`], which is this with
    /// [`Live::default`].
    #[must_use]
    pub fn validate_live(
        &self,
        lib: &Library,
        idx: &PlaylistIndex,
        cfg: &Config,
        live: &Live<'_>,
    ) -> Effects {
        self.validate_with(lib, idx, cfg, live, Prefs::default())
    }

    /// [`Plan::validate_live`], told what the user asked for about the plan as a
    /// whole — today that is `--merge`. See [`Prefs`].
    ///
    /// Whatever is passed here must also be passed to
    /// [`commit::Options::prefs`][super::commit::Options::prefs], because commit
    /// re-validates and a different answer there is
    /// [`Drift`][super::commit::Drift].
    #[must_use]
    pub fn validate_with(
        &self,
        lib: &Library,
        idx: &PlaylistIndex,
        cfg: &Config,
        live: &Live<'_>,
        prefs: Prefs,
    ) -> Effects {
        super::plan::validate(self, lib, idx, cfg, live, prefs)
    }
}

impl FromIterator<Operation> for Plan {
    fn from_iter<I: IntoIterator<Item = Operation>>(iter: I) -> Self {
        Self {
            ops: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).expect("test path")
    }

    #[test]
    fn a_directory_move_vacates_what_is_under_it_and_nothing_alongside_it() {
        let op = Operation::MoveDir {
            from: rel("hiphop/MF DOOM"),
            to: rel("hiphop/Daniel Dumile"),
        };

        assert!(op.vacates(&rel("hiphop/MF DOOM")));
        assert!(op.vacates(&rel("hiphop/MF DOOM/01 Beef Rap.mp3")));
        assert!(!op.vacates(&rel("hiphop/MF DOOM Instrumentals/01 Beef.mp3")));
        assert!(!op.vacates(&rel("hiphop")));
    }

    #[test]
    fn a_file_move_vacates_only_itself() {
        let op = Operation::MoveFile {
            from: rel("a/one.mp3"),
            to: rel("b/one.mp3"),
        };

        assert!(op.vacates(&rel("a/one.mp3")));
        assert!(!op.vacates(&rel("a")));
        assert!(!op.vacates(&rel("a/one.mp3.bak")));
    }

    #[test]
    fn a_delete_has_no_destination() {
        let op = Operation::Delete {
            target: rel("a/one.mp3"),
        };

        assert_eq!(op.source(), &rel("a/one.mp3"));
        assert_eq!(op.destination(), None);
        assert!(op.vacates(&rel("a/one.mp3")));
    }

    #[test]
    fn removing_an_operation_that_is_not_there_changes_nothing() {
        let mut plan = Plan::of(vec![Operation::Delete {
            target: rel("a/one.mp3"),
        }]);

        assert_eq!(plan.remove(7), None);
        assert_eq!(plan.len(), 1);
        assert!(plan.remove(0).is_some());
        assert!(plan.is_empty());
    }
}
