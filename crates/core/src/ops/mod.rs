//! Operations: the layer between "the user asked for this" and "the disk changed".
//!
//! [`exec_fs`] is the bottom of it — one filesystem change at a time, each with a
//! receipt that is enough to put it back. The layers above arrive with their own
//! tasks: the `Operation` enum and the pure `Plan::validate` that expands a
//! directory move into [`exec_fs::FsStep`]s and reports conflicts before anything
//! is touched (task 10), and the journaled two-phase `commit` that executes them
//! (task 11).
//!
//! The split is deliberate. `exec_fs` knows how to move one file correctly and
//! nothing about transactions; the journal knows about transactions and nothing
//! about `EXDEV`. Neither can quietly grow the other's bugs.

pub mod exec_fs;
