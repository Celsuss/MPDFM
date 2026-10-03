//! A recorded MPD conversation, replayed without a daemon.
//!
//! The MPD client (task 13) has to be tested in CI, where no daemon runs, and
//! against the exact bytes a real one sent, not against what the client's author
//! believed it sends. So the conversations live as text files under
//! `crates/core/tests/transcripts/`, captured from MPD 0.24.0 on this machine,
//! and [`Transcript`] replays one as a stream [`Mpd::handshake`][crate::mpd::Mpd::handshake]
//! can run on.
//!
//! ```
//! use mpdfm_core::mpd::Mpd;
//! use mpdfm_core::testing::Transcript;
//!
//! let script = Transcript::parse(
//!     "S: OK MPD 0.24.0\n\
//!      C: status\n\
//!      S: state: play\n\
//!      S: playlistlength: 2\n\
//!      S: OK\n",
//! );
//! let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");
//! assert_eq!(mpd.status().expect("a status").queue_len, 2);
//!
//! // And the client sent exactly what the transcript recorded it sending.
//! mpd.into_inner().assert_sent_the_script();
//! ```
//!
//! # The format
//!
//! One directive per line:
//!
//! | Prefix | Meaning |
//! | --- | --- |
//! | `S: text` | the daemon sends `text` and a newline |
//! | `S:` | the daemon sends an empty line |
//! | <code>S&#124; text</code> | the daemon sends `text` with **no** newline — a response cut short |
//! | `C: text` | the client is expected to send `text` and a newline |
//! | `#…` or blank | a comment |
//!
//! The server's bytes are concatenated in order and handed to the client as it
//! reads; the client's are concatenated and compared at the end by
//! [`TranscriptStream::assert_sent_the_script`]. The comparison is of the whole
//! conversation rather than turn by turn, which is what makes a transcript file
//! readable — it records the exchange in the order it happened, and the test
//! still fails if the client sends a different command, a differently quoted
//! argument, or one command too many.
//!
//! The stream ends where the transcript does: a read past the end returns end of
//! file, which is how a daemon that disconnects mid-response is recorded — a
//! <code>S&#124;</code> fragment and then nothing.

use std::io::{Read, Write};

/// A recorded conversation: what the daemon said, and what the client is expected
/// to have said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    server: Vec<u8>,
    client: Vec<u8>,
}

impl Transcript {
    /// Parse a transcript.
    ///
    /// # Panics
    ///
    /// On a line that is not one of the documented directives, naming the line
    /// number. A malformed transcript is a broken test, not a case to handle —
    /// the convention of [`testing`][crate::testing].
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut script = Self::default();
        for (number, line) in text.lines().enumerate() {
            // A transcript's own trailing whitespace is invisible in an editor
            // and would otherwise end up in the bytes under test, so the
            // directives that take text keep it and the rest is ignored.
            let trimmed = line.trim_end_matches(['\r']);
            if trimmed.trim().is_empty() || trimmed.starts_with('#') {
                continue;
            }
            match split_directive(trimmed) {
                Some(("S:", text)) => {
                    script.server.extend_from_slice(text.as_bytes());
                    script.server.push(b'\n');
                }
                Some(("S|", text)) => script.server.extend_from_slice(text.as_bytes()),
                Some(("C:", text)) => {
                    script.client.extend_from_slice(text.as_bytes());
                    script.client.push(b'\n');
                }
                _ => panic!(
                    "transcript line {}: expected `S: `, `S| ` or `C: `, got {trimmed:?}",
                    number + 1
                ),
            }
        }
        script
    }

    /// A stream that replays this transcript.
    #[must_use]
    pub fn stream(&self) -> TranscriptStream {
        TranscriptStream {
            server: self.server.clone(),
            read: 0,
            written: Vec::new(),
            expected: self.client.clone(),
        }
    }

    /// Everything the daemon said, as bytes.
    #[must_use]
    pub fn server_bytes(&self) -> &[u8] {
        &self.server
    }

    /// Everything the client is expected to say, as bytes.
    #[must_use]
    pub fn client_bytes(&self) -> &[u8] {
        &self.client
    }
}

/// The stream side of a [`Transcript`]: reads give the daemon's bytes, writes are
/// recorded for comparison.
#[derive(Debug, Clone)]
pub struct TranscriptStream {
    server: Vec<u8>,
    read: usize,
    written: Vec<u8>,
    expected: Vec<u8>,
}

impl TranscriptStream {
    /// What the client has sent so far.
    #[must_use]
    pub fn written(&self) -> &[u8] {
        &self.written
    }

    /// Whether every byte of the daemon's side was consumed. A `false` here after
    /// a test that expected a complete exchange means the client stopped reading
    /// early — it mistook one response for the end of another.
    #[must_use]
    pub fn drained(&self) -> bool {
        self.read == self.server.len()
    }

    /// Assert the client sent exactly what the transcript recorded.
    ///
    /// This is the assertion task 13 asks for by name: it is on the wire bytes,
    /// so a quoting mistake fails here even when the fake daemon would have
    /// answered anyway.
    ///
    /// # Panics
    ///
    /// When the bytes differ, printing both sides line by line.
    pub fn assert_sent_the_script(&self) {
        if self.written == self.expected {
            return;
        }
        let shown = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(|line| format!("    {line}\n"))
                .collect::<String>()
        };
        panic!(
            "the client did not send what the transcript recorded.\n  \
             expected:\n{}  actual:\n{}",
            shown(&self.expected),
            shown(&self.written),
        );
    }
}

impl Read for TranscriptStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = &self.server[self.read..];
        let take = left.len().min(buf.len());
        buf[..take].copy_from_slice(&left[..take]);
        self.read += take;
        Ok(take)
    }
}

impl Write for TranscriptStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Split a directive from its text, where the space after the directive is the
/// separator and not part of the text.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let (directive, rest) = line.split_at_checked(2)?;
    match rest.strip_prefix(' ') {
        Some(text) => Some((directive, text)),
        // `S:` on its own — an empty line from the daemon.
        None if rest.is_empty() => Some((directive, "")),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directives_build_the_two_sides() {
        let script = Transcript::parse(
            "# a comment\n\
             \n\
             S: OK MPD 0.24.0\n\
             C: ping\n\
             S: OK\n",
        );
        assert_eq!(script.server_bytes(), b"OK MPD 0.24.0\nOK\n");
        assert_eq!(script.client_bytes(), b"ping\n");
    }

    #[test]
    fn a_fragment_has_no_newline_and_an_empty_line_has_nothing_else() {
        let script = Transcript::parse("S: one\nS:\nS| half");
        assert_eq!(script.server_bytes(), b"one\n\nhalf");
    }

    #[test]
    fn reads_end_at_the_end_of_the_recording() {
        let mut stream = Transcript::parse("S: OK\n").stream();
        let mut all = String::new();
        stream.read_to_string(&mut all).expect("the recording");
        assert_eq!(all, "OK\n");
        assert!(stream.drained());
    }

    #[test]
    #[should_panic(expected = "transcript line 2")]
    fn a_line_that_is_not_a_directive_names_itself() {
        let _ = Transcript::parse("S: fine\nwhat is this\n");
    }

    #[test]
    #[should_panic(expected = "did not send what the transcript recorded")]
    fn the_wire_assertion_fails_on_a_different_command() {
        let script = Transcript::parse("C: status\n");
        let mut stream = script.stream();
        stream.write_all(b"stats\n").expect("recording a write");
        stream.assert_sent_the_script();
    }
}
