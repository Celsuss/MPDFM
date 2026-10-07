//! The terminal UI: the shell, the event loop, and the promise that the terminal
//! survives it.
//!
//! `mpdfm` with no subcommand lands here. The division of labour is:
//!
//! | module | answers |
//! | --- | --- |
//! | `terminal` | taking the terminal and — three ways — giving it back |
//! | `event` | the channel, and the threads that feed it |
//! | `msg` | everything the loop can be told |
//! | `work` | everything that must not happen on the drawing thread |
//! | `action` | everything the user can ask for, named |
//! | `keys` | which keys, in which mode, ask for it |
//! | `command` | the `:` line, and the commands it takes |
//! | `app` | the state, the loop, and the frame |
//! | `log` | where diagnostics go, which is never the screen |
//!
//! Task 20 owns the shell the views live in and the one guarantee that is hard to
//! add later — **the terminal is always restored**, see `terminal.rs` — task 21
//! owns the vocabulary they dispatch through, and task 22 the first of them.
//! Tasks 23–26 fill in the rest.
//!
//! # Start-up order
//!
//! Deliberate, and the reason this function is not just `App::new().run()`:
//!
//! 1. **the log is opened first.** `--log /nonexistent/x` has to be reportable on
//!    the user's shell, which means before the shell is taken away;
//! 2. **then the event threads**, including the signal handlers. A `SIGTERM`
//!    between entering raw mode and installing them would be the one window where
//!    the promise does not hold, so the window is closed before it opens;
//! 3. **then the terminal**, and from that point to the guard's drop nothing in
//!    this module writes to stdout or stderr;
//! 4. **then the first scan**, from inside the loop, after the first frame is on
//!    screen.
//!
//! # Testing seams
//!
//! Two environment variables, in the spirit of
//! [`ASSUME_TTY`][crate::output::ASSUME_TTY]: an interactive path that cannot be
//! tested is one that will eventually stop working.
//!
//! - **the loop takes its messages from an [`Events`]**, and
//!   `Events::scripted` builds one from a `Vec<Msg>` with no thread and no tty.
//!   That is how the loop's own tests drive it: a script of keys, resizes and
//!   worker results, and assertions on the buffer a `TestBackend` was drawn into;
//! - **[`PANIC_AT`]** induces a panic at a named point, which is how the
//!   panic-hook half of the restore promise is tested against a real terminal
//!   (`just verify-tui`, recorded in `docs/tasks/20-tui-shell.md`).
//!
//! `PANIC_AT` does nothing on an ordinary run, and the worst it can do when set by
//! accident is end the process with a panic that restores the terminal first —
//! which is the behaviour under test.

mod action;
mod app;
mod command;
mod event;
mod keys;
mod log;
mod msg;
mod terminal;
mod views;
mod widgets;
mod work;

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::config::{Config, Env, keys_file_path};

use crate::cli::Cli;
use crate::output::Exit;
use app::App;
use event::Events;
use log::Log;
use terminal::TerminalGuard;

/// Induce a panic at a named point, to prove the panic hook restores the terminal.
///
/// Values: `draw` panics inside the draw callback — the worst moment, with the
/// cursor hidden and the alternate screen active — and `event` panics while
/// handling the first message. Anything else is ignored.
pub const PANIC_AT: PanicAt = PanicAt("MPDFM_TUI_PANIC");

/// The name of [`PANIC_AT`]'s variable, with the one question anybody asks of it.
pub struct PanicAt(&'static str);

impl PanicAt {
    /// Whether the variable names `point`.
    pub fn is(&self, point: &str) -> bool {
        std::env::var(self.0).is_ok_and(|value| value == point)
    }

    /// The variable's name, for a message that explains itself.
    pub fn name(&self) -> &'static str {
        self.0
    }
}

/// Where `keys.toml` is, given what was on the command line.
///
/// Normally `$XDG_CONFIG_HOME/mpdfm/keys.toml`, next to `config.toml`. When
/// `--config` named a file somewhere else, the keymap is looked for beside *that*
/// file: a user pointing MPDFM at a second configuration means the whole
/// configuration, and a test can then put both in one temporary directory.
fn keys_path(cli: &Cli) -> Option<Utf8PathBuf> {
    match &cli.globals.config {
        Some(path) => Some(
            path.parent()
                .unwrap_or(Utf8Path::new("."))
                .join("keys.toml"),
        ),
        None => keys_file_path(&Env::from_process()),
    }
}

/// Launch the interactive browser.
///
/// # Errors
///
/// If the log file cannot be opened, if the signal handlers cannot be installed,
/// if the terminal cannot be taken, or if a draw fails. Nothing that happens to
/// the *library* is an error here: a `music_directory` that does not exist opens
/// the browser with an error panel on it, because the user's next move is to look
/// at the configuration, and a TUI that refuses to start is a worse place to do
/// that from than one that says what is wrong.
pub fn run(cli: &Cli, config: &Config) -> Result<ExitCode> {
    // 1. The log, before there is anywhere else for a complaint to go.
    let log = Arc::new(match &cli.tui.log {
        Some(path) => Log::to_file(path)?,
        None => Log::off(),
    });
    log.line(format!(
        "start: music_dir={} playlist_dir={} alt_screen={}",
        config.music_dir, config.playlist_dir, !cli.tui.no_alt_screen
    ));
    cli.trace(format!(
        "tui: starting (log={})",
        cli.tui.log.as_deref().map_or("off", Utf8Path::as_str)
    ));

    // 2. The keymap, which is the other file that can be wrong in a way worth
    //    reporting. Before raw mode for the same reason the log is: a warning
    //    about it has to be able to reach the user, and after this point the only
    //    way to reach them is a panel on a screen that does not exist yet.
    let (keymap, key_warnings) = match keys_path(cli) {
        Some(path) => {
            cli.trace(format!("tui: keys from {path}"));
            keys::load(&path)
        }
        None => (keys::KeyMap::defaults(), Vec::new()),
    };

    // 3. The threads, signal handlers included, before raw mode.
    let (events, tx) = Events::start(event::TICK)?;

    // 4. The terminal. From here to the guard's drop, nothing prints.
    let (guard, mut screen) = TerminalGuard::enter(!cli.tui.no_alt_screen)?;

    let mut app = App::new(config.clone(), keymap, tx, Arc::clone(&log));
    app.report_key_warnings(&key_warnings);
    let result = app.run(&mut screen, &events);

    // Explicit, so that whatever is printed after this — the `Err` on its way to
    // `main`, or nothing — reaches a terminal that is already a terminal again.
    // The drop would do it too; this only makes the ordering something a reader
    // can see rather than infer.
    drop(guard);
    log.line("stop");

    result.map(|()| Exit::Ok.into())
}
