//! The transaction journal: the record that makes a commit durable, inspectable
//! and reversible, and an interrupted one recoverable.
//!
//! ```no_run
//! use mpdfm_core::journal::{Store, TxId};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (config, _warnings) = mpdfm_core::config::resolve(&Default::default(),
//!                                                       &mpdfm_core::config::Env::from_process());
//! let store = Store::at(&config.data_dir);
//!
//! // What a previous run left halfway through — `mpdfm recover`'s input (task 12).
//! for record in store.unfinished()? {
//!     println!("{} is still {}", record.txid, record.status);
//! }
//!
//! // And the whole list, newest first, for `mpdfm undo --list`.
//! let (records, _unreadable) = store.records()?;
//! for record in &records {
//!     println!("{}", record.headline());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What the journal is for
//!
//! [`ops::commit`][crate::ops::commit] writes a [`Record`] with
//! [`Status::Pending`] and `fsync`s it **before the first file moves**, updates it
//! after every step, and marks it [`Status::Complete`] at the end. That ordering
//! is the whole design (`docs/PLAN.md` safety invariant 2): whatever instant the
//! power goes out at, what is on disk afterwards is a record that describes at
//! least everything that happened. Task 12 reverses it.
//!
//! The split inside this module is the usual one: [`record`] is data and knows
//! nothing about where it is kept, [`store`] is the directory layout and the
//! durability discipline and knows nothing about what a transaction means.
//! Neither of them executes anything.
//!
//! [`undo`] and [`recover`] do. They are here rather than in
//! [`ops`][crate::ops] because what they act on is a record: `undo` reverses a
//! transaction that finished, `recover` deals with one that did not, and both
//! work from the file on disk with nothing in memory to help them. Between them
//! they are the other half of safety invariant 9 — every committed transaction
//! is undoable, and undo verifies its preconditions rather than blindly
//! reversing.

pub mod record;
pub mod recover;
pub mod store;
pub mod undo;

pub use record::{Direction, Receipt, Record, Status, StepRecord, TxId, VERSION};
pub use store::{Kept, Pruned, Store};
pub use undo::{Reversed, UndoError};

use camino::Utf8PathBuf;

/// Why a journal record could not be read or written.
///
/// Every variant names the file, because an error about "the journal" that does
/// not say which record is not worth raising.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// There is no record with that id. Either the id is wrong or the journal has
    /// been cleared out by hand.
    #[error("there is no transaction {txid} (looked in {path})")]
    Missing {
        /// The id that was asked for.
        txid: TxId,
        /// Where its record would have been.
        path: Utf8PathBuf,
    },

    /// A string that cannot be a transaction id. Checked because an id becomes a
    /// path under the journal and backup directories.
    #[error("{input:?} is not a transaction id: {why}")]
    BadTxId {
        /// What was offered.
        input: String,
        /// Why it was refused.
        why: &'static str,
    },

    /// The record was written by a different version of the format. Refused
    /// rather than half-understood: a record is the instructions for putting a
    /// library back, and guessing at them is worse than declining.
    #[error(
        "{path} is a version {found} journal record; this MPDFM writes version \
         {expected}. Undo it with the MPDFM that wrote it, or move the record aside."
    )]
    Version {
        /// The record.
        path: Utf8PathBuf,
        /// The version it claims, or `none`.
        found: String,
        /// The version this build understands.
        expected: u32,
    },

    /// The record is not the JSON this understands — truncated, hand-edited, or
    /// written by something else entirely.
    #[error("{path} is not a journal record MPDFM can read: {source}")]
    Malformed {
        /// The record.
        path: Utf8PathBuf,
        /// What the parser said.
        #[source]
        source: serde_json::Error,
    },

    /// The record could not be turned into JSON at all. A bug rather than a user
    /// error; it is an error instead of a panic because it happens at the point a
    /// commit must stop, before anything is mutated.
    #[error("{path} could not be encoded: {source}")]
    Encode {
        /// Where it was going.
        path: Utf8PathBuf,
        /// What the encoder said.
        #[source]
        source: serde_json::Error,
    },

    /// An I/O failure, with the path that caused it. Includes a failing `fsync`,
    /// which here is fatal: see the [`store`] module docs.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory being read, written or synced.
        path: Utf8PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },
}
