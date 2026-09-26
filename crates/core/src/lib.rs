//! Core logic for MPDFM: library scanning, playlist parsing and rewriting, tag
//! editing, and the journaled two-phase commit that makes moves reversible.
//!
//! This crate knows nothing about terminals or command lines. Front-ends (the
//! `mpdfm` binary's CLI and TUI) are built on top of it, never the other way
//! around — see `docs/PLAN.md` §4.

pub mod paths;

// The fixture library (task 03). Behind a feature so a release build carries
// none of it, and so the embedded audio templates cost nothing in production.
#[cfg(feature = "testing")]
pub mod testing;

/// Anything that can go wrong in core.
///
/// Callers match on the variant, so new variants are added rather than folding
/// detail into a string.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A code path that is planned but not built yet. Carries the task that
    /// owns it so the message points at the spec.
    #[error("not implemented: {what} (see docs/tasks/{task})")]
    NotImplemented {
        /// What the caller asked for, e.g. `"mpdfm move"`.
        what: &'static str,
        /// The task file that will implement it, e.g. `"15-cli-move-and-doctor.md"`.
        task: &'static str,
    },

    /// A path that could not be made into a [`paths::RelPath`], or one that
    /// escaped a configured root.
    #[error(transparent)]
    Path(#[from] paths::PathError),

    /// An I/O failure, with the path that caused it.
    #[error("{path}: {source}")]
    Io {
        /// The path being read or written.
        path: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Result alias used throughout core.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Shorthand for [`Error::NotImplemented`].
    pub fn not_implemented(what: &'static str, task: &'static str) -> Self {
        Self::NotImplemented { what, task }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_implemented_names_its_task() {
        let err = Error::not_implemented("mpdfm move", "15-cli-move-and-doctor.md");
        assert_eq!(
            err.to_string(),
            "not implemented: mpdfm move (see docs/tasks/15-cli-move-and-doctor.md)"
        );
    }
}
