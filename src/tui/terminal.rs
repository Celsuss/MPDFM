//! Taking the terminal, and giving it back. Especially giving it back.
//!
//! This is the module the task singles out, because the classic bug is a program
//! that restores the terminal on the way out of `main` and nowhere else. A panic
//! does not go through there. Neither does `SIGTERM`. Either one leaves the user
//! staring at a shell with no echo, no line editing and no cursor, on a screen
//! that still holds the dead application's last frame — a shell that has to be
//! fixed with a blind `reset`.
//!
//! So there are three independent ways out of raw mode here, and all three go
//! through [`restore_terminal`] — raw mode *and* the escape sequences, never one
//! without the other:
//!
//! 1. **[`TerminalGuard`]'s `Drop`.** Covers returning, `?`, and unwinding.
//! 2. **A panic hook**, installed by [`TerminalGuard::enter`]. A panic runs the
//!    hook *before* it unwinds, so without this the backtrace would be printed
//!    into the alternate screen and then thrown away with it. The hook restores
//!    first and prints second, which is the whole point of having one.
//! 3. **`SIGTERM`/`SIGHUP`** are turned into [`Msg::Shutdown`][super::msg::Msg::Shutdown]
//!    by `event.rs`, so a `kill` leaves through case 1 rather than through the
//!    kernel's default disposition, which restores nothing.
//!
//! Running [`restore_terminal`] twice is harmless and the code does not go out of its way to
//! prevent it: `disable_raw_mode` on a cooked terminal is a no-op, and the escape
//! sequences are all idempotent. That matters, because during a panic the hook and
//! then the guard's `Drop` both run, and a restore path that only works once is a
//! restore path with a condition in it.
//!
//! # What is switched on
//!
//! Raw mode, the alternate screen, and a hidden cursor. Explicitly **not**: mouse
//! capture (it steals the terminal's own selection and scrollback, which is worse
//! than anything MPDFM would do with a click) and bracketed paste (nothing here
//! reads a paste). Both are disabled on the way out anyway, because a previous
//! program on this terminal may have left them on and `mpdfm` is a reasonable
//! place for a user to find that fixed.
//!
//! # `--no-alt-screen`
//!
//! Draws in the current screen instead. For debugging: the frames stay in
//! scrollback, so the last thing drawn before a crash can be read afterwards,
//! which is exactly what the alternate screen throws away.

use std::io::{Stdout, Write, stdout};

use anyhow::{Context as _, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// The terminal MPDFM draws on.
pub type Screen = Terminal<CrosstermBackend<Stdout>>;

/// Ownership of the terminal's modes, and the obligation to put them back.
///
/// Hold it for as long as the TUI is drawing and drop it to restore. It is not
/// `Clone` and not `Copy`: there is one terminal, and two things that both think
/// they own it is the bug this type exists to make unrepresentable.
#[derive(Debug)]
pub struct TerminalGuard {
    /// Whether the alternate screen was entered, and so has to be left.
    alt_screen: bool,
}

impl TerminalGuard {
    /// Switch the terminal into the modes the TUI needs, and hand back both the
    /// guard and a [`Screen`] to draw on.
    ///
    /// The panic hook is installed **first**, before raw mode is enabled, so that
    /// a panic anywhere after this line — including inside `enable_raw_mode`'s own
    /// error path — restores. The hook chains to whatever hook was already there,
    /// so a `RUST_BACKTRACE=1` backtrace still prints; it prints to a terminal
    /// that can show it.
    ///
    /// # Errors
    ///
    /// If the terminal cannot be switched into raw mode, which is what happens
    /// when stdout is not a terminal at all — a piped `mpdfm` has no TUI to run,
    /// and saying so is better than drawing escape sequences into a file. Also if
    /// the alternate screen cannot be entered, or the terminal's size cannot be
    /// read.
    pub fn enter(alt_screen: bool) -> Result<(Self, Screen)> {
        install_panic_hook(alt_screen);

        enable_raw_mode().context(
            "cannot put the terminal into raw mode. `mpdfm` with no subcommand is the \
             interactive browser and needs a terminal; run a subcommand instead, or see \
             `mpdfm --help`",
        )?;
        // From here on every failure has to restore what succeeded, so the guard
        // is built before anything else can go wrong and `?` does the rest.
        let guard = Self { alt_screen };

        enter(&mut stdout(), alt_screen).context("cannot set up the terminal for drawing")?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout()))
            .context("cannot read the terminal's size")?;
        Ok((guard, terminal))
    }

    /// Put the terminal back, now, without waiting for the drop.
    ///
    /// For the one case that needs it: printing something to the real screen —
    /// a final message, an error — after the TUI is finished but before the
    /// process ends. Calling it and then dropping the guard restores twice, which
    /// is fine (see the module docs).
    ///
    /// The same function the panic hook calls, which is the point: raw mode and
    /// the escape sequences are one job, and a restore path that did only the
    /// sequences would leave a shell with no echo on a screen that looks fine.
    pub fn restore(&self) {
        restore_terminal(self.alt_screen);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Switch the modes on, writing to `out`.
///
/// Takes the writer so the sequences can be asserted on in a test rather than
/// only observed by eye on a real terminal. Raw mode is not here: it is an
/// `ioctl` on the process's terminal and not a sequence anybody can capture.
fn enter<W: Write>(out: &mut W, alt_screen: bool) -> std::io::Result<()> {
    if alt_screen {
        execute!(out, EnterAlternateScreen)?;
    }
    execute!(out, Hide)?;
    out.flush()
}

/// Switch the modes off, writing to `out`. The exact inverse of [`enter`], plus
/// the two modes MPDFM never turns on but is happy to turn off.
///
/// Every step is attempted even if an earlier one failed, and nothing is
/// reported: this runs in a `Drop` and in a panic hook, where there is no caller
/// to tell and no terminal to tell them on. The order matters — the cursor is
/// shown *before* the alternate screen is left, so that a `--no-alt-screen` run,
/// which never leaves one, still ends with a visible cursor.
fn leave<W: Write>(out: &mut W, alt_screen: bool) {
    let _ = execute!(out, Show);
    let _ = execute!(out, DisableBracketedPaste);
    let _ = execute!(out, DisableMouseCapture);
    if alt_screen {
        let _ = execute!(out, LeaveAlternateScreen);
    }
    let _ = out.flush();
}

/// Restore the real terminal. The whole of what the panic hook and the guard
/// share.
fn restore_terminal(alt_screen: bool) {
    // Raw mode first: it has side effects on how everything after it is
    // interpreted, including the newlines in the panic message.
    let _ = disable_raw_mode();
    leave(&mut stdout(), alt_screen);
}

/// Make a panic restore the terminal before it prints anything.
///
/// Installed once per process. A second [`TerminalGuard::enter`] in one run would
/// otherwise stack a hook per call, and each would restore again — harmless, but
/// it would also mean the first `alt_screen` value is the one that sticks, which
/// is a surprise waiting to happen. One hook, one answer.
fn install_panic_hook(alt_screen: bool) {
    use std::sync::Once;
    static ONCE: Once = Once::new();

    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal(alt_screen);
            // The real hook now writes to a cooked terminal on the normal
            // screen, so the message and the backtrace survive the process.
            previous(info);
        }));
    });
}

/// Whether the terminal is big enough to draw the real layout in, and the floor
/// below which [`super::app`] draws a message instead.
///
/// 60×15 is where the browser's two panes plus the status bar stop being a
/// layout and start being a pile of one-character columns. The number is here
/// rather than in the view because the shell is what enforces it, and because a
/// test wants to name it.
pub const MIN_SIZE: (u16, u16) = (60, 15);

/// Whether `(width, height)` clears [`MIN_SIZE`].
pub fn fits(width: u16, height: u16) -> bool {
    width >= MIN_SIZE.0 && height >= MIN_SIZE.1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sequences, as an escape-free string, so a failure reads as text.
    fn written(alt_screen: bool, direction: fn(&mut Vec<u8>, bool)) -> String {
        let mut out = Vec::new();
        direction(&mut out, alt_screen);
        String::from_utf8(out)
            .expect("crossterm writes ASCII escapes")
            .replace('\u{1b}', "ESC")
    }

    #[test]
    fn entering_hides_the_cursor_and_takes_the_alternate_screen() {
        let text = written(true, |out, alt| {
            enter(out, alt).expect("writing to a Vec cannot fail")
        });
        // `CSI ?1049h` enters the alternate screen, `CSI ?25l` hides the cursor.
        assert!(text.contains("ESC[?1049h"), "{text}");
        assert!(text.contains("ESC[?25l"), "{text}");
    }

    #[test]
    fn leaving_undoes_everything_entering_did() {
        let text = written(true, leave);
        assert!(
            text.contains("ESC[?1049l"),
            "leave the alternate screen: {text}"
        );
        assert!(text.contains("ESC[?25h"), "show the cursor: {text}");

        // And in that order, so a run that ends on the normal screen ends with a
        // cursor on it rather than with one it hid on a screen it then left.
        let show = text.find("ESC[?25h").expect("cursor is shown");
        let leave_alt = text.find("ESC[?1049l").expect("alternate screen is left");
        assert!(show < leave_alt, "the cursor must be shown first: {text}");
    }

    #[test]
    fn mouse_capture_and_bracketed_paste_are_switched_off_on_the_way_out() {
        // Never switched on by MPDFM, always switched off: a terminal left in
        // either mode by a previous program is one `mpdfm` can fix for free.
        let text = written(true, leave);
        assert!(text.contains("ESC[?2004l"), "bracketed paste off: {text}");
        assert!(text.contains("ESC[?1006l"), "mouse capture off: {text}");

        let entered = written(true, |out, alt| {
            enter(out, alt).expect("writing to a Vec cannot fail")
        });
        assert!(
            !entered.contains("ESC[?2004h"),
            "paste must stay off: {entered}"
        );
        assert!(
            !entered.contains("ESC[?1000h"),
            "the mouse must stay ours: {entered}"
        );
    }

    #[test]
    fn no_alt_screen_touches_neither_screen_but_still_restores_the_cursor() {
        let entered = written(false, |out, alt| {
            enter(out, alt).expect("writing to a Vec cannot fail")
        });
        assert!(!entered.contains("1049"), "{entered}");
        assert!(
            entered.contains("ESC[?25l"),
            "the cursor is still hidden: {entered}"
        );

        let left = written(false, leave);
        assert!(!left.contains("1049"), "{left}");
        assert!(left.contains("ESC[?25h"), "and still shown again: {left}");
    }

    #[test]
    fn leaving_twice_writes_the_same_thing_twice_and_nothing_worse() {
        // The panic path restores and then the guard's `Drop` restores again.
        // Both are idempotent sequences, which is what makes that acceptable.
        let once = written(true, leave);
        let mut twice = Vec::new();
        leave(&mut twice, true);
        leave(&mut twice, true);
        let twice = String::from_utf8(twice)
            .expect("ASCII")
            .replace('\u{1b}', "ESC");
        assert_eq!(twice, format!("{once}{once}"));
    }

    #[test]
    fn the_size_floor_is_the_documented_one() {
        assert_eq!(MIN_SIZE, (60, 15));
        assert!(fits(60, 15));
        assert!(fits(200, 50));
        // The task's example of a terminal that gets the message instead.
        assert!(!fits(40, 10));
        // One short in either direction is still too small.
        assert!(!fits(59, 15));
        assert!(!fits(60, 14));
    }
}
