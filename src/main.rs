//! `mpdfm` — MPD File Manager.
//!
//! Thin front-end over `mpdfm-core`: parse arguments, dispatch, format output.
//! With no subcommand, launch the TUI.
//!
//! Every exit code a command can produce is in [`output::Exit`]; anything that
//! escapes as an `Err` is printed here and becomes [`Exit::Error`], because an
//! error that reached this far was not one a command had an answer for.

mod cli;
mod output;
mod tui;

use std::process::ExitCode;

use output::Exit;

fn main() -> ExitCode {
    match cli::run() {
        Ok(code) => code,
        Err(err) => {
            // `{err:#}` so an `anyhow` context chain reads as "what failed:
            // why", rather than losing everything but the outermost layer.
            eprintln!("mpdfm: {err:#}");
            Exit::Error.into()
        }
    }
}
