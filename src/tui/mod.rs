//! Terminal UI. Built in milestone M3 (tasks 20–26).

use std::process::ExitCode;

use anyhow::Result;

use crate::cli::Cli;
use crate::output::Exit;

/// Launch the interactive browser. Stub until task 20.
pub fn run(cli: &Cli) -> Result<ExitCode> {
    cli.trace("tui: would enter the alternate screen");
    println!(
        "mpdfm {}: the TUI arrives in task 20. Until then, see `mpdfm --help`.",
        env!("CARGO_PKG_VERSION")
    );
    Ok(Exit::Ok.into())
}
