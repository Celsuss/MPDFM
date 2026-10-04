//! What a command prints, what it exits with, and how it asks.
//!
//! Three decisions live here because every command has to make all three the
//! same way, and because two of them are part of the CLI's contract rather than
//! a detail of one subcommand:
//!
//! - **[`Exit`]** — the exit codes. A script needs to tell "I refused" from "I
//!   broke", and the numbers are documented in `mpdfm --help` and in task 15.
//! - **[`Out`]** — whether this run is talking to a person or to a pipe. Colour,
//!   prompting and the preview's width all follow from it, and nothing but this
//!   module is allowed to work it out, so `--json` cannot be honoured in one
//!   command and forgotten in the next.
//! - **[`Out::confirm`]** — the prompt. It refuses rather than hangs when there
//!   is nobody to answer, which is the difference between a CLI you can put in a
//!   script and one that wedges a CI job.
//!
//! # Why the preview is not coloured
//!
//! [`Effects::render`][mpdfm_core::ops::Effects::render] produces one plain
//! string, in core, shared with the TUI's pending view (task 24). Colouring it
//! from out here would mean re-parsing it, so instead the CLI colours only what
//! it composes itself: its own headings and the one-line verdicts. There is
//! nothing to suppress inside the preview, and [`Out::paint`] is a no-op
//! whenever `NO_COLOR` is set, output is not a terminal, or `--json` was asked
//! for.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use anyhow::Result;

/// The default preview width, when nothing better is known.
///
/// 80 is the width [`Effects::render`][mpdfm_core::ops::Effects::render] is laid
/// out for, and it clamps at 40 of its own accord.
const DEFAULT_WIDTH: usize = 80;

/// Treat stdin as a terminal however it was actually opened.
///
/// Set by `tests/` so that the confirmation prompt — and with it the "declined"
/// exit code — can be exercised through the real binary, which has no
/// pseudo-terminal to be given. A prompt that cannot be tested is a prompt that
/// will eventually stop working, and this is the cheapest seam that avoids it.
///
/// It is safe if somebody sets it by accident: it only makes MPDFM *ask*. The
/// read then hits end-of-file immediately, which is not `y`, so the answer is no
/// and nothing is written. It cannot cause a hang.
pub const ASSUME_TTY: &str = "MPDFM_ASSUME_TTY";

/// How a command ended.
///
/// The numbers are a promise to whoever is scripting this, so they are written
/// down once, here, rather than reached for as integers at each `return`:
///
/// | | |
/// |---|---|
/// | 0 | it worked |
/// | 1 | something went wrong |
/// | 2 | refused before anything was written |
/// | 3 | the user said no |
///
/// The one worth explaining is **2**. It means MPDFM's own safety checks
/// stopped the operation and the library is exactly as it was — a [`Conflict`]
/// in the preview, or the library having changed since the preview was rendered
/// ([`Drift`]). Both are worth retrying after a rescan, and neither is a bug, so
/// they share a code that a script can act on.
///
/// [`Conflict`]: mpdfm_core::ops::Conflict
/// [`Drift`]: mpdfm_core::ops::commit::Drift
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Success.
    Ok,
    /// An unexpected error. `main` also uses this for anything that escapes as
    /// an `Err`.
    Error,
    /// Conflicts, or a stale preview, blocked the operation. Nothing was
    /// written.
    Conflict,
    /// The user declined at the prompt. Nothing was written.
    Declined,
}

impl Exit {
    /// The number the process exits with.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Error => 1,
            Self::Conflict => 2,
            Self::Declined => 3,
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

/// What this run may do to the terminal.
///
/// Resolved once in [`Out::detect`] and passed down, so that a command cannot
/// accidentally consult `stdout().is_terminal()` for itself and reach a
/// different answer than the prompt did.
#[derive(Debug, Clone, Copy)]
pub struct Out {
    /// `--json`: stdout is one machine-readable document and nothing else.
    pub json: bool,
    /// Whether to emit escape sequences.
    pub color: bool,
    /// Whether there is somebody at the other end of stdin to answer a prompt.
    pub interactive: bool,
    /// Columns to render the preview at.
    pub width: usize,
}

impl Out {
    /// Work out the output mode from the flags and the environment.
    ///
    /// Colour is off unless stdout is a terminal, `NO_COLOR` is unset (its mere
    /// presence disables colour, whatever its value — that is what the
    /// convention says), `TERM` is not `dumb`, and this is not a `--json` run.
    ///
    /// Prompting depends on **stdin**, not stdout: a run whose output is piped
    /// into `less` still has a user who can answer, and a run fed from
    /// `/dev/null` does not however pretty its stdout is.
    ///
    /// [`ASSUME_TTY`] overrides the stdin test, and is how the integration tests
    /// drive the prompt without a pseudo-terminal.
    #[must_use]
    pub fn detect(json: bool) -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let dumb = std::env::var("TERM").is_ok_and(|term| term == "dumb");

        Self {
            json,
            color: stdout_tty && !no_color && !dumb && !json,
            interactive: std::io::stdin().is_terminal() || std::env::var_os(ASSUME_TTY).is_some(),
            width: terminal_width(),
        }
    }

    /// `text` in `style`, or `text` unchanged when colour is off.
    #[must_use]
    pub fn paint(&self, style: Style, text: &str) -> String {
        if !self.color || style == Style::Plain {
            return text.to_owned();
        }
        format!("\u{1b}[{}m{text}\u{1b}[0m", style.sgr())
    }

    /// Ask a yes/no question, defaulting to no.
    ///
    /// The question goes to **stderr** so that `--json` and a piped preview stay
    /// clean, and only `y` or `yes` (any case, surrounding space ignored) counts
    /// as yes. End-of-file is no: a user who closed the pipe did not agree to
    /// anything.
    ///
    /// # Errors
    ///
    /// When there is nobody to answer — stdin is not a terminal, or this is a
    /// `--json` run. Hanging on a prompt in a script is the failure this
    /// function exists to prevent, so the caller is told to pass `--yes`
    /// instead. That is [`Exit::Error`] and not [`Exit::Declined`]: nobody
    /// declined, the question could not be put.
    pub fn confirm(&self, question: &str) -> Result<bool> {
        anyhow::ensure!(
            self.interactive && !self.json,
            "cannot ask for confirmation: {}. Re-run with --yes to proceed without \
             a prompt, or with --dry-run to see the preview and stop.",
            if self.json {
                "--json output is not interactive"
            } else {
                "stdin is not a terminal"
            }
        );

        eprint!("{question} [y/N] ");
        std::io::stderr().flush().ok();

        let mut answer = String::new();
        // A read that fails is the same answer as a read that says no: this is
        // the gate in front of the one code path that changes the library, so it
        // is never opened by an error.
        if std::io::stdin().read_line(&mut answer).is_err() {
            return Ok(false);
        }
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

/// The handful of styles the CLI composes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// No escape at all, whatever the mode.
    Plain,
    /// A heading.
    Bold,
    /// Detail that should not compete with the numbers next to it.
    Dim,
    /// Something is wrong.
    Red,
    /// Something is as it should be.
    Green,
    /// Something to look at, which is not an error.
    Yellow,
}

impl Style {
    /// The SGR parameter for this style.
    fn sgr(self) -> &'static str {
        match self {
            Self::Plain => "0",
            Self::Bold => "1",
            Self::Dim => "2",
            Self::Red => "31",
            Self::Green => "32",
            Self::Yellow => "33",
        }
    }
}

/// The terminal's width, as far as the environment admits it.
///
/// `COLUMNS` when it holds a sane number, [`DEFAULT_WIDTH`] otherwise. MPDFM
/// does not yet link a terminal library — `crossterm` arrives with the TUI in
/// task 20, and `terminal::size()` is the better answer once it is there — and
/// pulling one in for a single `ioctl` ahead of time is not worth it. The
/// renderer never produces a line longer than the width it is given, so the
/// failure mode of guessing low is a shortened path, not a mangled layout.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|columns| columns.trim().parse::<usize>().ok())
        .filter(|width| *width >= 20)
        .unwrap_or(DEFAULT_WIDTH)
}

/// Print a JSON document to stdout, as the whole of a command's output.
///
/// # Errors
///
/// If the value cannot be serialized, which would be a bug in the caller.
pub fn json(document: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(document)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-interactive `Out`, as a piped run gets.
    fn piped() -> Out {
        Out {
            json: false,
            color: false,
            interactive: false,
            width: 80,
        }
    }

    #[test]
    fn exit_codes_are_the_documented_ones() {
        assert_eq!(Exit::Ok.code(), 0);
        assert_eq!(Exit::Error.code(), 1);
        assert_eq!(Exit::Conflict.code(), 2);
        assert_eq!(Exit::Declined.code(), 3);
    }

    #[test]
    fn a_prompt_with_nobody_to_answer_it_refuses_instead_of_blocking() {
        let err = piped()
            .confirm("do the thing?")
            .expect_err("a non-interactive run must not be asked");
        let message = err.to_string();
        assert!(message.contains("--yes"), "{message}");
        assert!(message.contains("stdin is not a terminal"), "{message}");
    }

    #[test]
    fn json_output_is_never_prompted_even_on_a_terminal() {
        let out = Out {
            json: true,
            interactive: true,
            ..piped()
        };
        let err = out.confirm("do the thing?").expect_err("--json never asks");
        assert!(
            err.to_string().contains("not interactive"),
            "{}",
            err.to_string()
        );
    }

    #[test]
    fn colour_is_left_out_when_it_is_switched_off() {
        let plain = piped();
        assert_eq!(plain.paint(Style::Red, "x"), "x");

        let colored = Out {
            color: true,
            ..plain
        };
        assert_eq!(colored.paint(Style::Red, "x"), "\u{1b}[31mx\u{1b}[0m");
        // `Plain` is a no-op even with colour on, so a caller can choose the
        // style and not branch.
        assert_eq!(colored.paint(Style::Plain, "x"), "x");
    }
}
