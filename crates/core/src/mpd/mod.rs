//! Just enough of MPD's protocol to keep its database fresh and to warn about
//! conflicts — a blocking client on `std::net::TcpStream`, no async runtime, no
//! dependency (`docs/PLAN.md` "Why no MPD client crate", decision D6).
//!
//! ```no_run
//! use mpdfm_core::mpd::{self, DEFAULT_TIMEOUT};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (config, _warnings) = mpdfm_core::config::resolve(
//!     &Default::default(),
//!     &mpdfm_core::config::Env::from_process(),
//! );
//!
//! match mpd::connect_if_enabled(&config, DEFAULT_TIMEOUT, None) {
//!     Ok(None) => println!("MPD is switched off; nothing was contacted"),
//!     Ok(Some(mut mpd)) => {
//!         println!("MPD {} at {}", mpd.version(), config.mpd_address);
//!         println!("{} songs queued", mpd.queue_paths()?.len());
//!     }
//!     // Not a failure: the filesystem is the source of truth.
//!     Err(err) => eprintln!("warning: {err}"),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What this module is for
//!
//! Three things, and nothing else:
//!
//! - after a commit, ask MPD to `update` the directories that changed, so its
//!   database stops pointing at paths that have moved (task 11);
//! - before a commit, read the queue and warn when a file about to move is in it
//!   (task 14);
//! - show whether the daemon is reachable and what it is playing (task 26).
//!
//! # Three rules that shape the whole module
//!
//! **Nothing here is fatal.** Connection refused, a timeout, a daemon that
//! disappears mid-response, a refusal, even a greeting from something that is not
//! MPD at all: every one of them is an [`MpdError`] the caller reports and
//! carries on from. MPDFM works with MPD stopped, and `--no-mpd` makes
//! [`connect_if_enabled`] open no socket whatsoever.
//!
//! **Nothing here blocks for long.** The connection is opened with a short
//! timeout ([`DEFAULT_TIMEOUT`]) which then applies to every read and write, so
//! the worst a wedged daemon can cost is that timeout, once. The TUI treats what
//! it last heard as cached, best-effort information and refreshes it on demand.
//!
//! **`update` is a promise, not a result.** It returns a [`JobId`] and the
//! database is still stale when the call returns, so MPDFM reports "update
//! queued" and never "MPD updated".
//!
//! # Layout
//!
//! [`proto`] is the wire format as pure functions over strings — quoting,
//! classifying a line, reading a `status`. [`client`] is the socket and the
//! commands. The split is what makes the parser testable from the recorded
//! transcripts in `tests/transcripts/` with no daemon in CI, and
//! [`Mpd::handshake`] is the seam: it takes anything that reads and writes.
//!
//! [`state`] is the odd one out: MPD's *state file*, which is a file on disk
//! rather than anything on the wire. It lives here because the saved queue it
//! holds is MPD's data and because deciding whether to rewrite it depends on
//! whether the daemon above is answering — see its module documentation.

pub mod client;
pub mod proto;
pub mod state;

pub use client::{DEFAULT_TIMEOUT, Mpd, Stream, connect_if_enabled};
pub use proto::{
    Ack, AckCode, JobId, Line, PlayState, Response, Status, Version, classify, command_line, quote,
};
pub use state::{MpdState, StateError, StateLine};

/// Why talking to MPD did not work.
///
/// Every variant is non-fatal — see the module documentation. They are separate
/// variants rather than one string because the callers do different things with
/// them: a [`MpdError::Connect`] is the ordinary "MPD is not running" that a
/// status indicator shows as a dash, an [`MpdError::Ack`] with
/// [`AckCode::Permission`] is worth telling the user about once, and an
/// [`MpdError::Greeting`] means something else is listening on that port, which
/// is a configuration mistake worth naming as one.
#[derive(Debug, thiserror::Error)]
pub enum MpdError {
    /// The socket could not be opened: nothing is listening, the port is
    /// filtered, or the socket file is not there.
    #[error("cannot reach MPD at {addr}: {source}")]
    Connect {
        /// The address that was tried.
        addr: String,
        /// Why it did not open.
        #[source]
        source: std::io::Error,
    },

    /// A configured hostname that does not resolve.
    #[error("cannot resolve MPD's host {host}: {source}")]
    Resolve {
        /// The hostname as configured.
        host: String,
        /// Why it did not resolve.
        #[source]
        source: std::io::Error,
    },

    /// An address shape this build cannot connect to, such as an abstract Unix
    /// socket. Named rather than reported as a missing file, because the fix is
    /// to configure a different address.
    #[error("cannot use the address {addr}: {why}")]
    Unsupported {
        /// The address as configured.
        addr: String,
        /// What is wrong with it.
        why: &'static str,
    },

    /// The daemon did not answer within the timeout. The connection is no longer
    /// trustworthy; open a new one.
    #[error("MPD did not answer in time")]
    Timeout,

    /// The connection ended before the response did — MPD was restarted or
    /// killed mid-command. What was read is incomplete and is thrown away.
    #[error("MPD closed the connection mid-response")]
    Disconnected,

    /// Something answered, but it is not MPD: the first line was not
    /// `OK MPD <version>`.
    #[error("that is not MPD: it greeted us with {line:?}")]
    Greeting {
        /// What it said instead.
        line: String,
    },

    /// MPD refused the command. Carries its error code — see [`AckCode`].
    #[error(transparent)]
    Ack(#[from] Ack),

    /// A response MPDFM could not read: an unknown line shape, a malformed
    /// `ACK`, a number that is not one, or a line that is not UTF-8.
    #[error("MPD said something unexpected: {message}")]
    Protocol {
        /// What was wrong, quoting the line.
        message: String,
    },

    /// An argument the protocol cannot carry — a filename with a newline in it,
    /// which ext4 allows and MPD's tokenizer has no escape for.
    #[error("cannot send {arg:?} to MPD: it {why}")]
    BadArgument {
        /// The argument as it would have been sent.
        arg: String,
        /// Why it cannot be.
        why: &'static str,
    },

    /// A socket read or write that failed for any other reason.
    #[error("MPD connection failed: {source}")]
    Io {
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl MpdError {
    /// Whether this is the everyday "MPD is not running" case, which a front-end
    /// shows as an absence rather than as an error.
    #[must_use]
    pub fn is_unreachable(&self) -> bool {
        matches!(
            self,
            Self::Connect { .. } | Self::Resolve { .. } | Self::Timeout | Self::Disconnected
        )
    }

    /// The MPD error code, when the daemon refused a command.
    #[must_use]
    pub fn ack_code(&self) -> Option<AckCode> {
        match self {
            Self::Ack(ack) => Some(ack.code),
            _ => None,
        }
    }
}
