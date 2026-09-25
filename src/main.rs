//! `mpdfm` — MPD File Manager.
//!
//! Thin front-end over `mpdfm-core`: parse arguments, dispatch, format output.
//! With no subcommand, launch the TUI.

mod cli;
mod tui;

use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("mpdfm: {err:#}");
            cli::EXIT_ERROR
        }
    }
}
