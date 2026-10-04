//! The daemon, for the three commands that change the library.
//!
//! Core owns no connection (`docs/PLAN.md` D6): it takes what MPD said as a
//! [`Live`] on the way in and a closure on the way out. This is the front-end
//! half of that arrangement, in one place because `move`, `undo` and `recover`
//! all need the same two things and must not disagree about them:
//!
//! - **the live queue**, so the preview can warn that a file about to move is
//!   playing — and, just as important, so it *does not* rewrite MPD's state file
//!   behind a running daemon that would overwrite it on shutdown (task 14);
//! - **a way to say `update`**, so MPD's database stops pointing at paths that
//!   have moved.
//!
//! # Nothing here is fatal
//!
//! A daemon that is stopped, refusing, wedged or not MPD at all costs one
//! [`DEFAULT_TIMEOUT`] and a line on stderr. The filesystem is the source of
//! truth, and `--no-mpd` opens no socket whatsoever.

use std::cell::RefCell;

use mpdfm_core::config::Config;
use mpdfm_core::library::DirPath;
use mpdfm_core::mpd::{self, DEFAULT_TIMEOUT, Mpd};
use mpdfm_core::ops::Live;
use mpdfm_core::paths::RelPath;

use super::Cli;

/// A connection to MPD, or the fact that there is not one.
///
/// The connection is behind a [`RefCell`] because [`Live`] borrows the queue for
/// as long as the preview and the commit need it, while `update` has to talk on
/// the same socket afterwards. One connection, two uses, no second handshake.
pub struct Link {
    client: Option<RefCell<Mpd>>,
    queue: Option<Vec<RelPath>>,
}

impl Link {
    /// Connect, ask for the queue, and report anything that went wrong on
    /// stderr.
    ///
    /// Never fails. `--no-mpd` and `mpd_enabled = false` produce the same thing
    /// a refused connection does — a link that answers `None` to everything —
    /// with the difference that they contact nothing at all.
    pub fn open(cli: &Cli, config: &Config) -> Self {
        let mut client = match mpd::connect_if_enabled(config, DEFAULT_TIMEOUT, None) {
            Ok(Some(client)) => {
                cli.trace(format!(
                    "mpd {} at {}",
                    client.version(),
                    config.mpd_address
                ));
                Some(client)
            }
            Ok(None) => {
                cli.trace("mpd: switched off, nothing contacted");
                None
            }
            Err(err) => {
                // A warning rather than a trace: whether MPD answered changes
                // what the commit does to the saved queue, so the user should
                // see it without asking for `-v`.
                eprintln!("mpdfm: warning: {err}");
                None
            }
        };

        // Asked for once, here, so the preview and the commit are given the same
        // answer. A queue read that fails leaves `None`, which means "MPD did
        // not answer" — the state file is then the queue, which is the
        // conservative reading and the one task 14 settled on.
        let queue = client
            .as_mut()
            .and_then(|client| match client.queue_paths() {
                Ok(queue) => Some(queue),
                Err(err) => {
                    eprintln!("mpdfm: warning: MPD's queue could not be read: {err}");
                    None
                }
            });

        Self {
            client: client.map(RefCell::new),
            queue,
        }
    }

    /// What MPD was holding, for [`Plan::validate_with`][mpdfm_core::ops::Plan::validate_with]
    /// and for [`commit::Options::live`][mpdfm_core::ops::commit::Options::live].
    ///
    /// The same value must reach both, or commit refuses as stale.
    #[must_use]
    pub fn live(&self) -> Live<'_> {
        Live {
            queue: self.queue.as_deref(),
        }
    }

    /// Whether there is a daemon to tell about a commit.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.client.is_some()
    }

    /// Ask MPD to rescan these directories.
    ///
    /// Shaped to be wrapped in a closure and handed to core as an
    /// [`Updater`][mpdfm_core::ops::commit::Updater]; the `String` is the error
    /// as the user should read it.
    ///
    /// # Errors
    ///
    /// Whatever the daemon said, rendered. Never fatal: the caller turns it into
    /// a warning, because the library and the playlists are already consistent
    /// and MPD catches up on its next update either way.
    pub fn update(&self, dirs: &[DirPath]) -> Result<(), String> {
        let Some(client) = &self.client else {
            return Ok(());
        };
        // The job ids are dropped on purpose: `update` is a promise, not a
        // result. MPD's database is still stale when the call returns, so there
        // is nothing here worth reporting beyond "queued" — and nothing MPDFM
        // could truthfully say about whether it finished.
        client
            .borrow_mut()
            .update_dirs(dirs)
            .map(|_jobs| ())
            .map_err(|err| err.to_string())
    }
}
