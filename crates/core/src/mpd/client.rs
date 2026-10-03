//! The client: a blocking socket, a buffered reader, and one method per command.
//!
//! [`Mpd`] is generic over its byte stream so that the tests can drive it from a
//! recorded transcript ([`testing::Transcript`][crate::testing::Transcript])
//! instead of a daemon. Production uses the default, [`Stream`], which is a TCP
//! connection or a Unix socket depending on what the configuration named.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::config::{Config, MpdAddress};
use crate::library::DirPath;
use crate::paths::RelPath;

use super::MpdError;
use super::proto::{self, JobId, Line, Response, Status, Version};

/// How long to wait for the daemon before giving up, when a caller has no
/// opinion of its own.
///
/// Short on purpose: MPD is on the loopback interface, the TUI must not stall
/// waiting for it, and an unreachable daemon is a condition MPDFM is designed to
/// carry on through (`docs/PLAN.md` D6).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// The byte stream
// ---------------------------------------------------------------------------

/// A connection to the daemon: TCP, or a Unix socket.
///
/// An enum rather than a `Box<dyn ReadWrite>` because the two are the only
/// possibilities [`MpdAddress`] can produce, and because both need
/// `set_read_timeout`, which no trait in `std` exposes.
#[derive(Debug)]
pub enum Stream {
    /// A TCP connection, with read and write timeouts set.
    Tcp(TcpStream),
    /// A Unix socket connection, with read and write timeouts set.
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

/// Opens the socket an address names, with timeouts set on it.
fn open(addr: &MpdAddress, timeout: Duration) -> Result<Stream, MpdError> {
    match addr {
        MpdAddress::Tcp { host, port } => {
            // Name resolution is the one step with no timeout of its own. For
            // the address this tool actually uses — `127.0.0.1` — it does no
            // I/O at all, and a hostname that needs a slow DNS server is the
            // user's own configuration.
            let candidates =
                (host.as_str(), *port)
                    .to_socket_addrs()
                    .map_err(|source| MpdError::Resolve {
                        host: host.clone(),
                        source,
                    })?;
            let mut last = None;
            for candidate in candidates {
                match TcpStream::connect_timeout(&candidate, timeout) {
                    Ok(stream) => {
                        stream
                            .set_read_timeout(Some(timeout))
                            .and_then(|()| stream.set_write_timeout(Some(timeout)))
                            .map_err(|source| MpdError::Connect {
                                addr: addr.to_string(),
                                source,
                            })?;
                        // Every command is one small line; waiting to coalesce
                        // it with the next one only adds latency.
                        let _ = stream.set_nodelay(true);
                        return Ok(Stream::Tcp(stream));
                    }
                    Err(err) => last = Some(err),
                }
            }
            Err(MpdError::Connect {
                addr: addr.to_string(),
                source: last.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::AddrNotAvailable,
                        "the host resolved to no addresses",
                    )
                }),
            })
        }

        #[cfg(unix)]
        MpdAddress::Unix(path) => {
            if path.as_str().starts_with('@') {
                // Linux's abstract namespace. MPD supports it; `std` has no
                // stable way to connect to one, so say so rather than failing
                // with a confusing "no such file".
                return Err(MpdError::Unsupported {
                    addr: addr.to_string(),
                    why: "abstract unix sockets are not supported; use a filesystem path",
                });
            }
            // `UnixStream::connect` has no timeout parameter, and does not need
            // one: connecting to a local socket either succeeds immediately or
            // fails immediately. The timeout that matters is on the reads.
            let stream =
                UnixStream::connect(path.as_std_path()).map_err(|source| MpdError::Connect {
                    addr: addr.to_string(),
                    source,
                })?;
            stream
                .set_read_timeout(Some(timeout))
                .and_then(|()| stream.set_write_timeout(Some(timeout)))
                .map_err(|source| MpdError::Connect {
                    addr: addr.to_string(),
                    source,
                })?;
            Ok(Stream::Unix(stream))
        }

        #[cfg(not(unix))]
        MpdAddress::Unix(_) => Err(MpdError::Unsupported {
            addr: addr.to_string(),
            why: "unix sockets need a unix target",
        }),
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// [`proto::MAX_LINE`] as the bounded reader wants it.
const MAX_LINE_U64: u64 = proto::MAX_LINE as u64;

/// Buffered line reads and unbuffered line writes over one stream.
#[derive(Debug)]
struct Wire<S> {
    reader: BufReader<S>,
    /// Reused between lines so that reading a 2 800-song queue does not allocate
    /// once per line.
    buf: Vec<u8>,
}

impl<S: Read + Write> Wire<S> {
    fn new(stream: S) -> Self {
        Self {
            reader: BufReader::new(stream),
            buf: Vec::with_capacity(256),
        }
    }

    /// Send one already-terminated command line.
    fn send(&mut self, line: &str) -> Result<(), MpdError> {
        let stream = self.reader.get_mut();
        stream.write_all(line.as_bytes()).map_err(io_error)?;
        stream.flush().map_err(io_error)
    }

    /// Read one line, without its terminator.
    ///
    /// End of stream is an error: the daemon ends a response with `OK` or `ACK`,
    /// so a closed connection in the middle of one means MPD went away (it was
    /// restarted, or `mpd --kill` ran) and the response is incomplete.
    fn read_line(&mut self) -> Result<&str, MpdError> {
        self.buf.clear();
        // Bounded, so that a service that is not MPD and never sends a newline
        // cannot make this allocate without end.
        let read = (&mut self.reader)
            .take(MAX_LINE_U64)
            .read_until(b'\n', &mut self.buf)
            .map_err(io_error)?;
        if read == 0 {
            return Err(MpdError::Disconnected);
        }
        if self.buf.last() != Some(&b'\n') {
            // Either the cap was hit, or the stream ended mid-line. Both mean
            // there is no complete line to parse.
            return Err(if read >= proto::MAX_LINE {
                MpdError::Protocol {
                    message: format!("a response line exceeded {} bytes", proto::MAX_LINE),
                }
            } else {
                MpdError::Disconnected
            });
        }
        self.buf.pop();
        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }
        std::str::from_utf8(&self.buf).map_err(|_| MpdError::Protocol {
            message: format!(
                "a response line is not valid UTF-8: {:?}",
                String::from_utf8_lossy(&self.buf)
            ),
        })
    }

    /// Read lines until `OK` or `ACK`.
    fn response(&mut self) -> Result<Response, MpdError> {
        let mut pairs = Vec::new();
        loop {
            let line = self.read_line()?;
            match proto::classify(line)? {
                Line::Ok => return Ok(Response::from_pairs(pairs)),
                Line::Ack(ack) => return Err(MpdError::Ack(ack)),
                Line::Pair { key, value } => pairs.push((key.to_owned(), value.to_owned())),
            }
        }
    }
}

/// A socket read or write that failed. A timeout is its own variant because it is
/// the ordinary consequence of a daemon that is busy or wedged, and the caller's
/// answer to it — carry on without MPD — is the same either way but the message
/// is not.
fn io_error(err: std::io::Error) -> MpdError {
    match err.kind() {
        // A socket with a read timeout reports it as `WouldBlock` on Linux and
        // `TimedOut` elsewhere.
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => MpdError::Timeout,
        _ => MpdError::Io { source: err },
    }
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// A connection to MPD, with one method per command MPDFM needs.
///
/// ```no_run
/// use mpdfm_core::mpd::{self, Mpd, DEFAULT_TIMEOUT};
/// use mpdfm_core::paths::RelPath;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let (config, _warnings) = mpdfm_core::config::resolve(
///     &Default::default(),
///     &mpdfm_core::config::Env::from_process(),
/// );
///
/// // `None` when `--no-mpd` or `mpd_enabled = false`: no socket is opened.
/// if let Some(mut mpd) = mpd::connect_if_enabled(&config, DEFAULT_TIMEOUT, None)? {
///     println!("MPD {}", mpd.version());
///     let status = mpd.status()?;
///     println!("{} with {} songs queued", status.state, status.queue_len);
///
///     // After a commit: ask for a rescan of what changed. This returns as soon
///     // as the job is queued — the database is not current yet.
///     let job = mpd.update(Some(&RelPath::parse("hiphop")?))?;
///     println!("update {job} queued");
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Every failure is the caller's to shrug off
///
/// Nothing here is fatal. The filesystem is the source of truth; MPD's database
/// is a cache MPDFM asks it to refresh, and its queue is advisory information for
/// a warning. So each method returns an [`MpdError`] and no method retries,
/// reconnects or blocks longer than the timeout the connection was opened with —
/// a caller that wants a fresher answer asks again.
#[derive(Debug)]
pub struct Mpd<S = Stream> {
    wire: Wire<S>,
    version: Version,
}

impl Mpd<Stream> {
    /// Connect and complete the greeting.
    ///
    /// `timeout` applies to the connection attempt and to every read and write
    /// afterwards, so a daemon that stops mid-response fails rather than hanging.
    ///
    /// # Errors
    ///
    /// [`MpdError::Connect`] when the socket cannot be opened — nothing is
    /// listening, or the port is firewalled — [`MpdError::Resolve`] for a
    /// hostname that does not resolve, [`MpdError::Timeout`] when the connection
    /// is accepted but nothing is said, and [`MpdError::Greeting`] when whatever
    /// answered is not MPD.
    pub fn connect(addr: &MpdAddress, timeout: Duration) -> Result<Self, MpdError> {
        Self::connect_with_password(addr, timeout, None)
    }

    /// Connect, greet, and authenticate when a password is given.
    ///
    /// MPDFM's own configuration holds no password — `mpdfm config show` prints
    /// every setting, and a password is not something to print — so the caller
    /// supplies it. This setup needs none.
    ///
    /// # Errors
    ///
    /// As [`Mpd::connect`], plus [`MpdError::Ack`] with
    /// [`AckCode::Password`][super::AckCode::Password] when the password is
    /// wrong. The error message never contains the password.
    pub fn connect_with_password(
        addr: &MpdAddress,
        timeout: Duration,
        password: Option<&str>,
    ) -> Result<Self, MpdError> {
        let mut mpd = Self::handshake(open(addr, timeout)?)?;
        if let Some(password) = password {
            mpd.password(password)?;
        }
        Ok(mpd)
    }
}

impl<S: Read + Write> Mpd<S> {
    /// Read the greeting from an already-open stream.
    ///
    /// This is the seam the transcript tests use: give it anything that reads and
    /// writes and the whole protocol runs over it.
    ///
    /// # Errors
    ///
    /// [`MpdError::Greeting`] when the first line is not `OK MPD <version>`,
    /// [`MpdError::Disconnected`] when there is no first line at all.
    pub fn handshake(stream: S) -> Result<Self, MpdError> {
        let mut wire = Wire::new(stream);
        let version = Version::parse(wire.read_line()?)?;
        Ok(Self { wire, version })
    }

    /// The protocol version the daemon greeted us with.
    #[must_use]
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// Send a command and collect its response.
    ///
    /// The escape hatch for everything this module does not model: arguments are
    /// quoted, the response is handed over as pairs, and an `ACK` is an error.
    ///
    /// # Errors
    ///
    /// [`MpdError::Ack`] when MPD refuses the command, [`MpdError::BadArgument`]
    /// for an argument that cannot be transmitted, and the I/O variants for a
    /// connection that broke.
    pub fn command(&mut self, name: &str, args: &[&str]) -> Result<Response, MpdError> {
        self.wire.send(&proto::command_line(name, args)?)?;
        self.wire.response()
    }

    /// `password` — authenticate.
    ///
    /// # Errors
    ///
    /// [`MpdError::Ack`] with [`AckCode::Password`][super::AckCode::Password]
    /// when it is wrong.
    pub fn password(&mut self, password: &str) -> Result<(), MpdError> {
        // Built by hand so that a failure cannot put the password in a message:
        // `command` would copy the argument into `MpdError::BadArgument`.
        if password.contains(['\n', '\r', '\0']) {
            return Err(MpdError::BadArgument {
                arg: "<password>".to_owned(),
                why: "contains a character the protocol cannot transmit",
            });
        }
        self.wire
            .send(&format!("password {}\n", proto::quote(password)))?;
        self.wire.response().map(|_| ())
    }

    /// `ping` — check the connection is still there.
    ///
    /// # Errors
    ///
    /// The I/O variants when it is not.
    pub fn ping(&mut self) -> Result<(), MpdError> {
        self.command("ping", &[]).map(|_| ())
    }

    /// `status` — what the player is doing.
    ///
    /// # Errors
    ///
    /// [`MpdError::Protocol`] when the response is not one this build can read,
    /// and the I/O variants for a connection that broke.
    pub fn status(&mut self) -> Result<Status, MpdError> {
        let response = self.command("status", &[])?;
        Status::from_response(&response)
    }

    /// `currentsong` — the playing track, as a library path.
    ///
    /// `None` when the player is stopped with nothing loaded, **and** when what
    /// is loaded is not a library file: the real setup's `Radios.m3u` holds
    /// `https://` stream URLs, and a stream has no path under `music_directory`
    /// to move or rewrite. [`Mpd::current_uri`] is the one to call when the raw
    /// value matters.
    ///
    /// # Errors
    ///
    /// The I/O variants for a connection that broke.
    pub fn current_song(&mut self) -> Result<Option<RelPath>, MpdError> {
        Ok(self.current_uri()?.as_deref().and_then(library_path))
    }

    /// `currentsong` — the playing track's URI exactly as MPD spells it, which
    /// may be a stream URL rather than a library path.
    ///
    /// # Errors
    ///
    /// The I/O variants for a connection that broke.
    pub fn current_uri(&mut self) -> Result<Option<String>, MpdError> {
        let response = self.command("currentsong", &[])?;
        Ok(response.find("file").map(str::to_owned))
    }

    /// `playlistinfo` — the current queue as library paths.
    ///
    /// Entries that are not library files — stream URLs — are left out, because
    /// what task 14 does with this list is intersect it with the paths a move is
    /// about to change, and a stream can never be one of them. Call
    /// [`Mpd::queue_uris`] for everything, including those.
    ///
    /// # Errors
    ///
    /// The I/O variants for a connection that broke.
    pub fn queue_paths(&mut self) -> Result<Vec<RelPath>, MpdError> {
        let response = self.command("playlistinfo", &[])?;
        Ok(response.values("file").filter_map(library_path).collect())
    }

    /// `playlistinfo` — every queue entry's URI, in queue order, library files
    /// and stream URLs alike.
    ///
    /// # Errors
    ///
    /// The I/O variants for a connection that broke.
    pub fn queue_uris(&mut self) -> Result<Vec<String>, MpdError> {
        let response = self.command("playlistinfo", &[])?;
        Ok(response.values("file").map(str::to_owned).collect())
    }

    /// `update` — queue a database update of `dir`, or of the whole library when
    /// `dir` is `None`.
    ///
    /// The returned [`JobId`] means the work is **scheduled**, not done; see
    /// [`JobId`]. Updating the whole library takes long enough to notice, so pass
    /// a directory whenever one is known — [`Mpd::update_dirs`] does that from
    /// what a commit touched.
    ///
    /// # Errors
    ///
    /// [`MpdError::Ack`] when MPD refuses — [`AckCode::Permission`][super::AckCode::Permission]
    /// for a daemon that does not allow updates — and [`MpdError::Protocol`] when
    /// the response carries no job id.
    pub fn update(&mut self, dir: Option<&RelPath>) -> Result<JobId, MpdError> {
        self.update_command("update", dir)
    }

    /// `rescan` — like [`Mpd::update`], but re-reads tags of files MPD thinks
    /// are unchanged.
    ///
    /// # Errors
    ///
    /// As [`Mpd::update`].
    pub fn rescan(&mut self, dir: Option<&RelPath>) -> Result<JobId, MpdError> {
        self.update_command("rescan", dir)
    }

    /// `update` for each of the directories a commit changed.
    ///
    /// Takes them as [`DirPath`]s because that is what
    /// [`ops::commit::affected_dirs`][crate::ops::commit::affected_dirs] produces,
    /// already reduced to the shallowest ancestors that cover the transaction —
    /// `update` is recursive, so naming a directory and its child would be the
    /// same work twice. [`DirPath::root`] means the whole library, which is the
    /// `None` of [`Mpd::update`].
    ///
    /// Stops at the first failure and returns it, with the ids of the updates
    /// that were queued lost — the caller's answer to a failed update is a
    /// warning either way, and MPD's next update catches up regardless.
    ///
    /// # Errors
    ///
    /// As [`Mpd::update`].
    pub fn update_dirs(&mut self, dirs: &[DirPath]) -> Result<Vec<JobId>, MpdError> {
        dirs.iter().map(|dir| self.update(dir.as_rel())).collect()
    }

    /// The stream, for a caller that wants to inspect or close it. The protocol
    /// is a plain TCP conversation with no shutdown handshake, so dropping the
    /// client is a clean goodbye; MPD notices the close.
    pub fn into_inner(self) -> S {
        self.wire.reader.into_inner()
    }

    /// Shared by `update` and `rescan`, which differ only in the word sent.
    fn update_command(&mut self, name: &str, dir: Option<&RelPath>) -> Result<JobId, MpdError> {
        let response = match dir {
            Some(dir) => self.command(name, &[dir.as_str()])?,
            None => self.command(name, &[])?,
        };
        response
            .number("updating_db")?
            .map(JobId)
            .ok_or_else(|| MpdError::Protocol {
                message: format!("`{name}` answered without an `updating_db` job id"),
            })
    }
}

/// A queue entry as a library path, or `None` when it is not one.
///
/// A stream URL fails [`RelPath::parse`] — `https://…` has an empty component
/// between the slashes — and that rejection is exactly the test wanted here, so
/// it is used as one instead of a scheme check that would have to list every
/// scheme MPD can play.
fn library_path(uri: &str) -> Option<RelPath> {
    RelPath::parse(uri).ok()
}

// ---------------------------------------------------------------------------
// The configured entry point
// ---------------------------------------------------------------------------

/// Connect to the daemon the configuration names, or not at all.
///
/// `Ok(None)` when [`Config::mpd_enabled`] is false — which is what `--no-mpd`
/// sets — and in that case **no socket is opened and no name is resolved**. That
/// is the whole point of the flag: a user whose MPD is on a host that hangs
/// instead of refusing needs a way to be sure MPDFM will not touch it.
///
/// # Errors
///
/// Every variant [`Mpd::connect_with_password`] raises. All of them are non-fatal
/// by design: the caller reports and carries on (`docs/PLAN.md` D6).
pub fn connect_if_enabled(
    config: &Config,
    timeout: Duration,
    password: Option<&str>,
) -> Result<Option<Mpd>, MpdError> {
    if !config.mpd_enabled {
        return Ok(None);
    }
    Mpd::connect_with_password(&config.mpd_address, timeout, password).map(Some)
}
