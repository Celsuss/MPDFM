//! `--log <file>`: where the TUI's diagnostics go, which is never the terminal.
//!
//! While the TUI owns the screen, a stray `println!` or `eprintln!` writes
//! directly into the frame `ratatui` believes it drew, and the display stays
//! corrupt until the next full redraw — which may not come, because the loop only
//! redraws when something changed. So the rule for everything under
//! [`crate::tui`] is: no writing to stdout or stderr between
//! [`TerminalGuard::enter`][super::terminal::TerminalGuard::enter] and the
//! guard's `Drop`. A [`Log`] is what replaces it.
//!
//! The CLI's `-v` tracing goes to stderr ([`Cli::trace`][crate::cli::Cli::trace])
//! and therefore cannot be used past that point either; the shell logs the same
//! kind of line to the file instead, and `-v` without `--log` is silent inside
//! the TUI. That is not a gap: there is nowhere for it to go that is not the
//! user's screen.
//!
//! The file is opened in append mode, so two runs over one log read in order, and
//! nothing a previous session recorded is lost when a crash is being chased.
//! Writes are line-buffered and flushed immediately: the interesting log is the
//! one from the run that just died.

use std::fmt::Display;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use camino::Utf8Path;

/// A log file, or nothing at all.
///
/// Shared across threads by `Arc`: the scan worker logs to the same file as the
/// UI thread, which is the point — the question a log answers is usually "what
/// was the worker doing when the UI stopped moving". The `Mutex` is around the
/// handle and held for one `write_all`, so a worker can never interleave half a
/// line into the UI thread's.
#[derive(Debug, Default)]
pub struct Log {
    sink: Option<Mutex<std::fs::File>>,
}

impl Log {
    /// A log that discards everything. The default, and what every run without
    /// `--log` gets.
    pub fn off() -> Self {
        Self { sink: None }
    }

    /// Append to `path`, creating it if it is not there.
    ///
    /// # Errors
    ///
    /// If the file cannot be opened. This is reported *before* the terminal is
    /// taken over, because `--log /nonexistent/x` is a typo the user wants to see
    /// on their shell rather than guess at from a TUI that logged nothing.
    pub fn to_file(path: &Utf8Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("cannot open the log file {path}"))?;
        let log = Self {
            sink: Some(Mutex::new(file)),
        };
        log.line(format!(
            "--- mpdfm {} starting ---",
            env!("CARGO_PKG_VERSION")
        ));
        Ok(log)
    }

    /// Whether anything is being written. Lets a caller skip building a message
    /// that would be thrown away.
    pub fn is_on(&self) -> bool {
        self.sink.is_some()
    }

    /// Write one line, with the time in front of it.
    ///
    /// Infallible on purpose. A log is a debugging aid, and a failed write to it
    /// must not become an error in the thing being debugged — there is also
    /// nowhere to report it to, the terminal being taken. A poisoned lock is
    /// ignored for the same reason: another thread panicking is exactly when the
    /// log matters most.
    pub fn line(&self, message: impl Display) {
        let Some(sink) = &self.sink else {
            return;
        };
        let stamp = seconds_since_epoch();
        let mut guard = match sink.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = writeln!(guard, "{stamp} {message}");
        let _ = guard.flush();
    }
}

/// Seconds and milliseconds since the epoch.
///
/// Not a calendar date: MPDFM has no date formatting crate, and the only thing
/// this timestamp is used for is measuring gaps between lines in one session.
/// `docs/tasks/20-tui-shell.md` records how it is read.
fn seconds_since_epoch() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_that_is_off_swallows_everything_and_says_so() {
        let log = Log::off();
        assert!(!log.is_on());
        // No panic, no output, nothing to assert but that it returns.
        log.line("dropped on the floor");
    }

    #[test]
    fn a_log_file_is_appended_to_rather_than_truncated() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("mpdfm.log"))
            .expect("temp path is UTF-8");

        let first = Log::to_file(&path).expect("a log in a temp dir should open");
        assert!(first.is_on());
        first.line("one");
        drop(first);

        let second = Log::to_file(&path).expect("reopening should append");
        second.line("two");
        drop(second);

        let text = std::fs::read_to_string(&path).expect("read the log back");
        let lines: Vec<&str> = text.lines().collect();
        // Two session banners and two messages, in order.
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].contains("starting"), "{text}");
        assert!(lines[1].ends_with(" one"), "{text}");
        assert!(lines[2].contains("starting"), "{text}");
        assert!(lines[3].ends_with(" two"), "{text}");
        // Every line is stamped, so gaps between them can be measured.
        for line in &lines {
            let stamp = line.split(' ').next().unwrap_or_default();
            assert!(
                stamp.parse::<f64>().is_ok(),
                "line should start with a timestamp: {line}"
            );
        }
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_an_error_before_the_terminal_is_taken() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("no-such-dir/mpdfm.log"))
            .expect("temp path is UTF-8");

        let err = Log::to_file(&path).expect_err("a log under a missing directory cannot open");
        assert!(
            err.to_string().contains(path.as_str()),
            "the error should name the file: {err:#}"
        );
    }
}
