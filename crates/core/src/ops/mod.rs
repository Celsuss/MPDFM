//! Operations: the layer between "the user asked for this" and "the disk changed".
//!
//! Three levels, each one expansion away from the next:
//!
//! | | | |
//! |---|---|---|
//! | [`Operation`] | what the user asked for | "move this album there" |
//! | [`FsStep`][exec_fs::FsStep] | one filesystem change | "rename this file" |
//! | [`StepReceipt`][exec_fs::StepReceipt] | what that change did | "and here is how to put it back" |
//!
//! [`Plan::validate`] does the first expansion, purely: it produces [`Effects`],
//! which holds every step, every playlist line and every reason the whole thing
//! might be refused, and writes nothing. [`commit`] does the second, one journaled
//! step at a time, keeping the receipts. Task 12 walks the receipts backwards.
//!
//! The split is deliberate. [`exec_fs`] knows how to move one file correctly and
//! nothing about transactions; [`plan`] knows about ordering a whole plan and
//! nothing about `EXDEV`; [`commit`] and the [`journal`][crate::journal] know
//! about crashes and nothing about either. None of them can quietly grow another's
//! bugs.
//!
//! ```no_run
//! use mpdfm_core::library::Library;
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
//!
//! // Nothing has been touched yet, and nothing will be until this is shown to
//! // someone who says yes.
//! let effects = plan.validate(&library, &index, &config);
//! println!("{}", effects.render(80));
//!
//! if effects.is_committable() {
//!     // `commit::commit` takes it from here — see that module's example.
//! }
//! # Ok(())
//! # }
//! ```

pub mod commit;
pub mod effects;
pub mod exec_fs;
pub mod op;
pub mod plan;
pub mod render;

pub use commit::{CommitError, CommitWarning, Committed, Previewed};
pub use effects::{Conflict, Effects, OpEffect, Summary, Warning};
pub use op::{Operation, Plan};
