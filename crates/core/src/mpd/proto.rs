//! The wire format: how an argument is quoted, what a response line means, and
//! the three responses MPDFM reads.
//!
//! Everything here is a pure function over a `&str`. No sockets, no timeouts,
//! nothing to mock — which is what lets the recorded transcripts in
//! `tests/transcripts/` exercise the whole parser in CI with no daemon running
//! (task 13's acceptance criteria), and what lets [`quote`] be tested against the
//! real album names rather than against an invented one.
//!
//! # The protocol in one paragraph
//!
//! A client sends one command per line. The daemon answers with zero or more
//! `key: value` lines and then either `OK` or `ACK [code@index] {command} message`.
//! The connection opens with `OK MPD <version>`. That is the whole framing, and
//! [`classify`] is the whole reader.

use std::fmt;

use super::MpdError;

/// Longest response line MPDFM will accept, in bytes.
///
/// MPD's own lines are short — the longest realistic one is a `file:` with a
/// scene-release directory in it, around 200 bytes. The cap exists for the case
/// where the configured port is answered by something that is not MPD at all and
/// streams bytes without ever sending a newline: a bounded read turns that into
/// an error instead of an allocation that grows until the process dies.
pub const MAX_LINE: usize = 64 * 1024;

/// What the daemon says first.
pub const GREETING: &str = "OK MPD ";

/// The line that ends a successful response.
pub const OK: &str = "OK";

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// The protocol version from the greeting, e.g. `0.24.0`.
///
/// Kept as numbers *and* as the string that was sent: the numbers are for
/// deciding whether a command exists ([`Version::at_least`]), the string is for
/// showing the user, because a distribution build may call itself something like
/// `0.24.0~git` and reprinting the components would quietly lose that.
///
/// ```
/// use mpdfm_core::mpd::Version;
///
/// let v = Version::parse("OK MPD 0.24.0")?;
/// assert_eq!((v.major, v.minor, v.patch), (0, 24, 0));
/// assert_eq!(v.to_string(), "0.24.0");
/// assert!(v.at_least(0, 21));
/// # Ok::<(), mpdfm_core::mpd::MpdError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// First component.
    pub major: u32,
    /// Second component, 0 when the greeting gave none.
    pub minor: u32,
    /// Third component, 0 when the greeting gave none.
    pub patch: u32,
    /// Exactly what followed `OK MPD `, trimmed.
    pub raw: String,
}

impl Version {
    /// Parse the greeting line.
    ///
    /// # Errors
    ///
    /// [`MpdError::Greeting`] when the line is not `OK MPD <version>`. That is
    /// the check that catches "something is listening on 6600, but it is not
    /// MPD" before any command is sent.
    pub fn parse(line: &str) -> Result<Self, MpdError> {
        let greeting = || MpdError::Greeting {
            line: line.to_owned(),
        };
        let raw = line.strip_prefix(GREETING).ok_or_else(greeting)?.trim();
        let mut parts = raw.split('.');
        // The major component must actually be a number; the rest may be absent.
        let major = leading_number(parts.next().unwrap_or_default()).ok_or_else(greeting)?;
        Ok(Self {
            major,
            minor: leading_number(parts.next().unwrap_or_default()).unwrap_or_default(),
            patch: leading_number(parts.next().unwrap_or_default()).unwrap_or_default(),
            raw: raw.to_owned(),
        })
    }

    /// Whether this version is at least `major.minor`.
    #[must_use]
    pub fn at_least(&self, major: u32, minor: u32) -> bool {
        (self.major, self.minor) >= (major, minor)
    }
}

/// The leading run of digits of `text`, or `None` when it starts with something
/// else. `0.24.0~git` parses as `(0, 24, 0)` this way.
fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

// ---------------------------------------------------------------------------
// Quoting
// ---------------------------------------------------------------------------

/// Quote one command argument.
///
/// MPD's tokenizer splits on whitespace unless an argument is wrapped in `"`,
/// inside which `"` and `\` are escaped with a backslash. The real library is
/// full of names that need it — `Snoop Dogg & Wiz Khalifa - Mac + Devin Go To
/// High School (Soundtrack) (2011) [320] vtwin88cube` has spaces, `&`, `+` and
/// brackets — so this quotes **every** argument rather than trying to work out
/// which ones can get away without it. One rule, no classification to get wrong.
///
/// ```
/// use mpdfm_core::mpd::quote;
///
/// assert_eq!(quote("hip hop/Some Album"), r#""hip hop/Some Album""#);
/// assert_eq!(quote(r#"a "quoted" name"#), r#""a \"quoted\" name""#);
/// assert_eq!(quote(r"back\slash"), r#""back\\slash""#);
/// // `&`, `+`, `[`, `]` and `'` are ordinary characters inside the quotes.
/// assert_eq!(quote("Mac + Devin [320] 'live' & more"), r#""Mac + Devin [320] 'live' & more""#);
/// ```
#[must_use]
pub fn quote(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for ch in arg.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Build a complete command line, newline included.
///
/// ```
/// use mpdfm_core::mpd::command_line;
///
/// assert_eq!(command_line("status", &[])?, "status\n");
/// assert_eq!(command_line("update", &["hip hop"])?, "update \"hip hop\"\n");
/// # Ok::<(), mpdfm_core::mpd::MpdError>(())
/// ```
///
/// # Errors
///
/// [`MpdError::BadArgument`] when an argument carries a byte the protocol has no
/// way to transmit: a newline, a carriage return or a NUL. A newline in a
/// filename is legal on ext4 and there is no escape for it in MPD's tokenizer, so
/// such a track can only be reported, never sent — refusing here keeps it from
/// being split into two commands the daemon would answer separately.
pub fn command_line(name: &str, args: &[&str]) -> Result<String, MpdError> {
    let mut line = String::from(name);
    for arg in args {
        if let Some(bad) = arg.find(['\n', '\r', '\0']) {
            return Err(MpdError::BadArgument {
                arg: (*arg).to_owned(),
                why: match arg.as_bytes()[bad] {
                    b'\n' => "contains a newline",
                    b'\r' => "contains a carriage return",
                    _ => "contains a NUL byte",
                },
            });
        }
        line.push(' ');
        line.push_str(&quote(arg));
    }
    line.push('\n');
    Ok(line)
}

// ---------------------------------------------------------------------------
// Response lines
// ---------------------------------------------------------------------------

/// One line of a response, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line<'a> {
    /// A `key: value` line.
    Pair {
        /// Everything before the first `:`.
        key: &'a str,
        /// Everything after it, minus the single separating space.
        value: &'a str,
    },
    /// `OK` — the response is complete.
    Ok,
    /// `ACK …` — the command was refused.
    Ack(Ack),
}

/// Classify one response line.
///
/// The value is **not** trimmed. `time: 2290:3157` splits at the first `:`
/// because a key never contains one, and only the one space MPD puts after the
/// colon is removed, because a filename may legitimately end in a space.
///
/// ```
/// use mpdfm_core::mpd::{Line, classify};
///
/// assert_eq!(classify("time: 2290:3157")?, Line::Pair { key: "time", value: "2290:3157" });
/// // MPD sends an empty value as `key: ` — the key is still there.
/// assert_eq!(classify("lastloadedplaylist: ")?, Line::Pair { key: "lastloadedplaylist", value: "" });
/// assert_eq!(classify("OK")?, Line::Ok);
/// # Ok::<(), mpdfm_core::mpd::MpdError>(())
/// ```
///
/// # Errors
///
/// [`MpdError::Protocol`] for a line that is none of the three shapes.
pub fn classify(line: &str) -> Result<Line<'_>, MpdError> {
    if line == OK {
        return Ok(Line::Ok);
    }
    if line.starts_with("ACK ") || line == "ACK" {
        return Ack::parse(line).map(Line::Ack);
    }
    let (key, value) = line.split_once(':').ok_or_else(|| MpdError::Protocol {
        message: format!("expected `key: value`, `OK` or `ACK`, got {line:?}"),
    })?;
    if key.is_empty() {
        return Err(MpdError::Protocol {
            message: format!("response line has an empty key: {line:?}"),
        });
    }
    Ok(Line::Pair {
        key,
        value: value.strip_prefix(' ').unwrap_or(value),
    })
}

// ---------------------------------------------------------------------------
// ACK
// ---------------------------------------------------------------------------

/// MPD's documented error codes, from `src/protocol/Ack.hxx`.
///
/// Matched on rather than compared as a number: task 11 wants to know whether a
/// post-commit `update` failed because the directory is gone
/// ([`AckCode::NoExist`], which a move can cause and which is harmless) or
/// because MPDFM is not allowed to update at all ([`AckCode::Permission`], which
/// is worth telling the user about once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckCode {
    /// 1 — a command-list error.
    NotList,
    /// 2 — a bad argument.
    Arg,
    /// 3 — wrong or missing password.
    Password,
    /// 4 — the password in use does not grant this command.
    Permission,
    /// 5 — no such command.
    Unknown,
    /// 50 — no such song, directory or playlist.
    NoExist,
    /// 51 — the queue or playlist is full.
    PlaylistMax,
    /// 52 — a system error on the daemon's side.
    System,
    /// 53 — a playlist that could not be loaded.
    PlaylistLoad,
    /// 54 — an update is already running.
    UpdateAlready,
    /// 55 — player synchronization error.
    PlayerSync,
    /// 56 — it already exists.
    Exist,
    /// A code this build does not know. Newer daemons may add some, and an
    /// unknown code is still an error worth reporting faithfully.
    Other(u32),
}

impl AckCode {
    /// The code as MPD numbers it.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Self::NotList => 1,
            Self::Arg => 2,
            Self::Password => 3,
            Self::Permission => 4,
            Self::Unknown => 5,
            Self::NoExist => 50,
            Self::PlaylistMax => 51,
            Self::System => 52,
            Self::PlaylistLoad => 53,
            Self::UpdateAlready => 54,
            Self::PlayerSync => 55,
            Self::Exist => 56,
            Self::Other(code) => code,
        }
    }

    /// Classify a numeric code.
    #[must_use]
    pub fn from_code(code: u32) -> Self {
        match code {
            1 => Self::NotList,
            2 => Self::Arg,
            3 => Self::Password,
            4 => Self::Permission,
            5 => Self::Unknown,
            50 => Self::NoExist,
            51 => Self::PlaylistMax,
            52 => Self::System,
            53 => Self::PlaylistLoad,
            54 => Self::UpdateAlready,
            55 => Self::PlayerSync,
            56 => Self::Exist,
            other => Self::Other(other),
        }
    }

    /// A short name for the code, for messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotList => "not in a command list",
            Self::Arg => "bad argument",
            Self::Password => "wrong password",
            Self::Permission => "not permitted",
            Self::Unknown => "unknown command",
            Self::NoExist => "no such entity",
            Self::PlaylistMax => "playlist is full",
            Self::System => "system error",
            Self::PlaylistLoad => "playlist could not be loaded",
            Self::UpdateAlready => "an update is already running",
            Self::PlayerSync => "player synchronization error",
            Self::Exist => "already exists",
            Self::Other(_) => "unrecognized error",
        }
    }
}

impl fmt::Display for AckCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.code(), self.as_str())
    }
}

/// A refused command: `ACK [50@0] {update} No such directory`.
///
/// ```
/// use mpdfm_core::mpd::{Ack, AckCode};
///
/// let ack = Ack::parse("ACK [50@0] {update} No such directory")?;
/// assert_eq!(ack.code, AckCode::NoExist);
/// assert_eq!(ack.command, "update");
/// assert_eq!(ack.message, "No such directory");
///
/// // The command name is empty when MPD could not work out what was asked.
/// let unknown = Ack::parse(r#"ACK [5@0] {} unknown command "nosuchcommand""#)?;
/// assert_eq!(unknown.code, AckCode::Unknown);
/// assert!(unknown.command.is_empty());
/// # Ok::<(), mpdfm_core::mpd::MpdError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    /// Which error it is.
    pub code: AckCode,
    /// Index of the failing command within a command list; 0 for a lone command.
    pub list_index: u32,
    /// The command MPD was running, which may be empty.
    pub command: String,
    /// MPD's own wording, kept verbatim so a report can quote the daemon.
    pub message: String,
}

impl Ack {
    /// Parse an `ACK` line.
    ///
    /// # Errors
    ///
    /// [`MpdError::Protocol`] when the line is an `ACK` MPDFM cannot read. The
    /// alternative — guessing a code — would turn "the daemon said something
    /// unexpected" into "the daemon said error 0", which is worse.
    pub fn parse(line: &str) -> Result<Self, MpdError> {
        let malformed = || MpdError::Protocol {
            message: format!("malformed ACK: {line:?}"),
        };
        let rest = line
            .strip_prefix("ACK ")
            .and_then(|rest| rest.trim_start().strip_prefix('['))
            .ok_or_else(malformed)?;
        let (inside, rest) = rest.split_once(']').ok_or_else(malformed)?;
        let (code, index) = inside.split_once('@').ok_or_else(malformed)?;
        let code: u32 = code.trim().parse().map_err(|_| malformed())?;
        let list_index: u32 = index.trim().parse().map_err(|_| malformed())?;

        let rest = rest.trim_start();
        let (command, message) = match rest.strip_prefix('{') {
            Some(rest) => {
                let (command, message) = rest.split_once('}').ok_or_else(malformed)?;
                (command, message.trim_start())
            }
            // No `{command}` at all: take the whole remainder as the message.
            None => ("", rest),
        };

        Ok(Self {
            code: AckCode::from_code(code),
            list_index,
            command: command.to_owned(),
            message: message.to_owned(),
        })
    }
}

impl fmt::Display for Ack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = if self.command.is_empty() {
            "the command".to_owned()
        } else {
            format!("`{}`", self.command)
        };
        if self.message.is_empty() {
            write!(f, "MPD refused {what}: error {}", self.code)
        } else {
            write!(
                f,
                "MPD refused {what}: {} (error {})",
                self.message, self.code
            )
        }
    }
}

impl std::error::Error for Ack {}

// ---------------------------------------------------------------------------
// A whole response
// ---------------------------------------------------------------------------

/// Every `key: value` pair of one successful response, in the order they arrived.
///
/// Order is kept because that is the only thing separating one song from the next
/// in `playlistinfo`: each record begins with a `file:` line, and the pairs after
/// it belong to that file. Nothing is deduplicated and nothing is dropped, so a
/// response can be re-read for a key this version does not know about yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pairs: Vec<(String, String)>,
}

impl Response {
    /// Wrap the pairs a reader collected.
    #[must_use]
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self { pairs }
    }

    /// The pairs, in order.
    #[must_use]
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// Whether the daemon answered with nothing but `OK` — what `currentsong`
    /// does when the player is stopped and the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The first value for `key`.
    #[must_use]
    pub fn find(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_str())
    }

    /// Every value for `key`, in order.
    pub fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.pairs
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, value)| value.as_str())
    }

    /// The first value for `key` as a number, `None` when the key is absent.
    ///
    /// # Errors
    ///
    /// [`MpdError::Protocol`] when the value is not a number, naming the key —
    /// an unparsable `playlistlength` is a protocol mismatch, not a missing key,
    /// and the two want different handling.
    pub fn number(&self, key: &str) -> Result<Option<u32>, MpdError> {
        match self.find(key) {
            None => Ok(None),
            Some(raw) => raw
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| MpdError::Protocol {
                    message: format!("`{key}` is not a number: {raw:?}"),
                }),
        }
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// What the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// Playing.
    Play,
    /// Paused, with a current song.
    Pause,
    /// Stopped.
    Stop,
}

impl PlayState {
    /// Parse the value of `state:`.
    fn parse(value: &str) -> Result<Self, MpdError> {
        match value.trim() {
            "play" => Ok(Self::Play),
            "pause" => Ok(Self::Pause),
            "stop" => Ok(Self::Stop),
            other => Err(MpdError::Protocol {
                message: format!("unknown player state {other:?}"),
            }),
        }
    }

    /// Whether a song is loaded — playing or paused.
    #[must_use]
    pub fn has_song(self) -> bool {
        matches!(self, Self::Play | Self::Pause)
    }
}

impl fmt::Display for PlayState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Stop => "stop",
        })
    }
}

/// The id of a database update job.
///
/// `update` and `rescan` are asynchronous: the id is MPD's promise to do the work,
/// not a report that it is done (task 13's first pitfall). Anything MPDFM prints
/// about it says "queued".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u32);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The part of `status` MPDFM cares about.
///
/// Deliberately a handful of fields rather than the whole response: volume,
/// crossfade and bitrate are not MPDFM's business, and a struct that modelled
/// them would have to grow every time MPD adds one. [`Response`] is still there
/// for anything else — [`crate::mpd::Mpd::command`] hands it over untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Playing, paused or stopped.
    pub state: PlayState,
    /// `playlistlength` — how many songs are in the queue.
    pub queue_len: u32,
    /// `playlist` — the queue's version counter, which changes on every edit.
    pub queue_version: u32,
    /// `song` — the current song's position in the queue.
    pub song: Option<u32>,
    /// `songid` — the current song's queue id, which survives a reorder.
    pub song_id: Option<u32>,
    /// `updating_db` — the job id of a database update in progress, if any.
    /// Task 11 uses it to say "an update is already running" rather than
    /// queueing a second one.
    pub updating_db: Option<JobId>,
}

impl Status {
    /// Read a `status` response.
    ///
    /// # Errors
    ///
    /// [`MpdError::Protocol`] when `state:` is missing or unrecognized, or when a
    /// numeric field is not a number. Everything else is optional: MPD omits
    /// `song` and `songid` when stopped, and `updating_db` whenever no update is
    /// running.
    pub fn from_response(response: &Response) -> Result<Self, MpdError> {
        let state = response.find("state").ok_or_else(|| MpdError::Protocol {
            message: "`status` response had no `state`".to_owned(),
        })?;
        Ok(Self {
            state: PlayState::parse(state)?,
            queue_len: response.number("playlistlength")?.unwrap_or_default(),
            queue_version: response.number("playlist")?.unwrap_or_default(),
            song: response.number("song")?,
            song_id: response.number("songid")?,
            updating_db: response.number("updating_db")?.map(JobId),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_greeting_gives_the_version() {
        let version = Version::parse("OK MPD 0.24.0").expect("the real greeting parses");
        assert_eq!((version.major, version.minor, version.patch), (0, 24, 0));
        assert_eq!(version.to_string(), "0.24.0");
    }

    #[test]
    fn a_version_suffix_is_kept_but_does_not_break_the_numbers() {
        let version = Version::parse("OK MPD 0.24.0~git").expect("a distro build parses");
        assert_eq!((version.major, version.minor, version.patch), (0, 24, 0));
        assert_eq!(version.to_string(), "0.24.0~git");
    }

    #[test]
    fn a_short_version_fills_the_missing_components_with_zero() {
        let version = Version::parse("OK MPD 1").expect("one component is enough");
        assert_eq!((version.major, version.minor, version.patch), (1, 0, 0));
        assert!(version.at_least(0, 21));
    }

    #[test]
    fn something_that_is_not_mpd_is_not_a_greeting() {
        // An HTTP server, an SSH banner, and MPD's own refusal to say a version.
        for line in [
            "HTTP/1.1 400 Bad Request",
            "SSH-2.0-OpenSSH_9.9",
            "OK MPD",
            "OK MPD version",
            "",
        ] {
            assert!(
                matches!(Version::parse(line), Err(MpdError::Greeting { .. })),
                "{line:?} should not parse as a greeting"
            );
        }
    }

    #[test]
    fn quoting_handles_the_real_album_names() {
        // Straight from `testing::names` — the names task 13 says to test with.
        assert_eq!(
            quote("hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag"),
            r#""hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag""#
        );
        assert_eq!(
            quote(
                "hiphop/Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube/01.Smokin' On.mp3"
            ),
            r#""hiphop/Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube/01.Smokin' On.mp3""#
        );
        assert_eq!(
            quote("electronic/KREAM - So Hï [c0D2h71bFFI]"),
            r#""electronic/KREAM - So Hï [c0D2h71bFFI]""#
        );
    }

    #[test]
    fn only_the_quote_and_the_backslash_are_escaped() {
        assert_eq!(quote(r#"say "hi"\now"#), r#""say \"hi\"\\now""#);
        assert_eq!(quote(""), r#""""#);
    }

    #[test]
    fn a_command_with_no_arguments_is_just_its_name() {
        assert_eq!(command_line("status", &[]).expect("no args"), "status\n");
    }

    #[test]
    fn an_argument_that_cannot_be_transmitted_is_refused() {
        for bad in ["two\nlines", "carriage\rreturn", "nul\0byte"] {
            assert!(
                matches!(
                    command_line("update", &[bad]),
                    Err(MpdError::BadArgument { .. })
                ),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_pair_splits_at_the_first_colon_only() {
        assert_eq!(
            classify("time: 2290:3157").expect("a pair"),
            Line::Pair {
                key: "time",
                value: "2290:3157"
            }
        );
    }

    #[test]
    fn a_value_keeps_every_space_after_the_first() {
        assert_eq!(
            classify("file:  leading space.mp3").expect("a pair"),
            Line::Pair {
                key: "file",
                value: " leading space.mp3"
            }
        );
    }

    #[test]
    fn an_empty_value_is_still_a_pair() {
        assert_eq!(
            classify("lastloadedplaylist: ").expect("a pair"),
            Line::Pair {
                key: "lastloadedplaylist",
                value: ""
            }
        );
        assert_eq!(
            classify("lastloadedplaylist:").expect("a pair"),
            Line::Pair {
                key: "lastloadedplaylist",
                value: ""
            }
        );
    }

    #[test]
    fn a_line_that_is_no_shape_at_all_is_a_protocol_error() {
        for line in ["gibberish", "", " "] {
            assert!(
                matches!(classify(line), Err(MpdError::Protocol { .. })),
                "{line:?} should not classify"
            );
        }
    }

    #[test]
    fn an_ack_carries_its_code_command_and_message() {
        let ack = Ack::parse("ACK [50@0] {update} No such directory").expect("an ACK");
        assert_eq!(
            ack,
            Ack {
                code: AckCode::NoExist,
                list_index: 0,
                command: "update".to_owned(),
                message: "No such directory".to_owned(),
            }
        );
        assert_eq!(
            ack.to_string(),
            "MPD refused `update`: No such directory (error 50 (no such entity))"
        );
    }

    #[test]
    fn an_unknown_ack_code_is_reported_as_itself() {
        let ack = Ack::parse("ACK [99@3] {whatever} brand new failure").expect("an ACK");
        assert_eq!(ack.code, AckCode::Other(99));
        assert_eq!(ack.code.code(), 99);
        assert_eq!(ack.list_index, 3);
    }

    #[test]
    fn a_malformed_ack_is_a_protocol_error_not_a_guess() {
        for line in [
            "ACK",
            "ACK 50",
            "ACK [50] {update} x",
            "ACK [fifty@0] {update} x",
            "ACK [50@0] {update x",
        ] {
            assert!(
                matches!(Ack::parse(line), Err(MpdError::Protocol { .. })),
                "{line:?} should not parse as an ACK"
            );
        }
    }

    #[test]
    fn status_reads_the_fields_it_needs_and_ignores_the_rest() {
        let response = Response::from_pairs(
            [
                ("volume", "75"),
                ("playlist", "644"),
                ("playlistlength", "2"),
                ("state", "pause"),
                ("lastloadedplaylist", ""),
                ("song", "0"),
                ("songid", "438"),
                ("audio", "48000:16:2"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        );
        let status = Status::from_response(&response).expect("a real status response");
        assert_eq!(status.state, PlayState::Pause);
        assert!(status.state.has_song());
        assert_eq!(status.queue_len, 2);
        assert_eq!(status.queue_version, 644);
        assert_eq!(status.song, Some(0));
        assert_eq!(status.song_id, Some(438));
        assert_eq!(status.updating_db, None);
    }

    #[test]
    fn a_stopped_player_has_no_song() {
        let response = Response::from_pairs(vec![
            ("state".to_owned(), "stop".to_owned()),
            ("playlistlength".to_owned(), "0".to_owned()),
        ]);
        let status = Status::from_response(&response).expect("a stopped status");
        assert_eq!(status.song, None);
        assert!(!status.state.has_song());
    }

    #[test]
    fn a_status_without_a_state_is_a_protocol_error() {
        let response = Response::from_pairs(vec![("volume".to_owned(), "75".to_owned())]);
        assert!(matches!(
            Status::from_response(&response),
            Err(MpdError::Protocol { .. })
        ));
    }

    #[test]
    fn a_non_numeric_number_names_its_key() {
        let response = Response::from_pairs(vec![
            ("state".to_owned(), "play".to_owned()),
            ("playlistlength".to_owned(), "lots".to_owned()),
        ]);
        let err = Status::from_response(&response).expect_err("`lots` is not a number");
        assert!(err.to_string().contains("playlistlength"), "{err}");
    }
}
