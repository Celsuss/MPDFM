//! Terminal UI. Built in milestone M3 (tasks 20–26).

use std::process::ExitCode;

use anyhow::Result;

use crate::cli::Cli;

/// Launch the interactive browser. Stub until task 20.
pub fn run(cli: &Cli) -> Result<ExitCode> {
    cli.trace("tui: would enter the alternate screen");
    println!(
        "mpdfm {}: the TUI arrives in task 20. Until then, see `mpdfm --help`.",
        env!("CARGO_PKG_VERSION")
    );
    Ok(crate::cli::EXIT_OK)
}
