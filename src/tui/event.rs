//! The channel every event arrives on, and the threads that feed it.
//!
//! Three producers, one consumer, one `mpsc` channel:
//!
//! | thread | blocks on | sends |
//! | --- | --- | --- |
//! | input | `crossterm::event::read()` | [`Msg::Input`] |
//! | tick | `thread::sleep` | [`Msg::Tick`] |
//! | signals | `signal_hook`'s iterator | [`Msg::Shutdown`] |
//!
//! Plus any number of short-lived workers, which get a [`Sender`] clone and send
//! [`Msg::Progress`], [`Msg::ScanDone`] or [`Msg::TaskDone`] when they are done.
//!
//! # Why blocking reads and not polling
//!
//! `crossterm::event::poll(timeout)` in a loop is the shape most examples use,
//! and it is a timer that wakes the process several times a second forever. The
//! acceptance criterion for this task is idle CPU at about zero over 30 seconds,
//! and the way to get it is for every thread to be *blocked* when nothing is
//! happening: the input thread inside `read()`, the UI thread inside `recv()`. The
//! only periodic wakeup in the whole program is the one-second tick, and the loop
//! does not necessarily redraw on it (see [`App::update`][super::app::App::update]).
//!
//! # Why the threads are never joined
//!
//! The input thread is blocked in `read()` and there is no portable way to
//! interrupt it; a `join` would hang until the user pressed another key, which
//! would be a hang on the way out — the exact failure this task is about. So every
//! thread here is detached and exits on its own when its `send` fails, which is
//! what happens as soon as the loop drops the receiver. Nothing they do touches
//! the terminal, so the restoring is already finished by the time the process
//! ends; the threads are simply unmapped with it. `main` returning is what ends
//! the process, and it does not wait for them.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use super::msg::Msg;

/// How often the slow tick fires. One second, as task 26's MPD indicator wants.
pub const TICK: Duration = Duration::from_secs(1);

/// The consumer's end of the channel.
///
/// Holds no sender, which is deliberate: [`Events::recv`] can then report the end
/// of the stream rather than block forever on a channel nobody will write to
/// again. In production that end never comes — the tick thread outlives the loop —
/// and in a test it is what stops a loop with a bug in it from hanging the suite.
pub struct Events {
    rx: Receiver<Msg>,
}

impl Events {
    /// Start the input, tick and signal threads and return the receiving end
    /// together with a sender for workers to clone.
    ///
    /// # Errors
    ///
    /// If the signal handlers cannot be registered. That is not a cosmetic
    /// failure: without them a `kill` would leave the terminal in raw mode, which
    /// is precisely what this task promises cannot happen, so it is better to
    /// refuse to start than to start without the promise.
    pub fn start(tick: Duration) -> Result<(Self, Sender<Msg>)> {
        let (tx, rx) = mpsc::channel();

        spawn_signals(tx.clone())?;
        spawn_input(tx.clone());
        spawn_tick(tx.clone(), tick);

        Ok((Self { rx }, tx))
    }

    /// Block until something happens. `None` once every sender is gone.
    pub fn recv(&self) -> Option<Msg> {
        self.rx.recv().ok()
    }

    /// Take a message if one is already waiting, without blocking.
    ///
    /// The loop drains with this before drawing, so that a burst — a held-down
    /// `j`, a resize the terminal reports twice, a run of progress messages —
    /// costs one frame rather than one frame each.
    pub fn try_recv(&self) -> Option<Msg> {
        // Empty and disconnected are the same answer to the one question being
        // asked here — "is there another message right now?" — and the loop reads
        // the disconnection from the next blocking `recv`.
        self.rx.try_recv().ok()
    }

    /// A fixed script, with no threads and no terminal: the seam the loop's tests
    /// run through.
    ///
    /// The sender is dropped before this returns, so `recv` reports the end of the
    /// script. A test whose app fails to quit therefore fails on an assertion
    /// rather than hanging.
    #[cfg(test)]
    pub fn scripted(msgs: Vec<Msg>) -> Self {
        let (tx, rx) = mpsc::channel();
        for msg in msgs {
            tx.send(msg).expect("the receiver is alive");
        }
        drop(tx);
        Self { rx }
    }
}

/// Read the terminal forever, one blocking read at a time.
fn spawn_input(tx: Sender<Msg>) {
    thread::Builder::new()
        .name("mpdfm-input".to_owned())
        .spawn(move || {
            loop {
                // `read` blocks until the terminal has something, which is what
                // keeps this thread off the CPU. Resize arrives here as
                // `Event::Resize`: `crossterm` owns the `SIGWINCH` handler.
                let Ok(event) = crossterm::event::read() else {
                    // The terminal went away — the pty closed under us. There is
                    // no recovering from that and nothing to draw on any more, so
                    // it is a shutdown: leaving through the guard restores a
                    // terminal that may still exist on the other side of a
                    // detached session, and a loop left blocked on a channel
                    // nobody will write to again would be a hang on the way out.
                    let _ = tx.send(Msg::Shutdown);
                    break;
                };
                if tx.send(Msg::Input(event)).is_err() {
                    break;
                }
            }
        })
        .expect("spawning the input thread");
}

/// Send [`Msg::Tick`] every `period`.
fn spawn_tick(tx: Sender<Msg>, period: Duration) {
    thread::Builder::new()
        .name("mpdfm-tick".to_owned())
        .spawn(move || {
            loop {
                thread::sleep(period);
                if tx.send(Msg::Tick).is_err() {
                    break;
                }
            }
        })
        .expect("spawning the tick thread");
}

/// Turn a termination signal into [`Msg::Shutdown`].
///
/// `signal_hook`'s iterator does the unsafe part properly: the installed handler
/// only writes to a self-pipe, and this thread does the work outside signal
/// context, where sending on a channel is allowed. Doing it by hand with
/// `libc::signal` and a handler that touched a channel would be undefined
/// behaviour.
///
/// `SIGINT` is included for completeness and is mostly moot: in raw mode the
/// terminal does not generate it, so `ctrl-c` arrives as a key event. A `kill
/// -INT` from another shell still works.
///
/// # Errors
///
/// If a handler cannot be registered.
fn spawn_signals(tx: Sender<Msg>) -> Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP])
        .context("cannot install the handlers that make SIGTERM exit cleanly")?;

    thread::Builder::new()
        .name("mpdfm-signals".to_owned())
        .spawn(move || {
            for _signal in &mut signals {
                // Which signal it was does not change the answer: stop, through
                // the guard. A second one while the first is being handled is the
                // same message again, and the loop is already leaving.
                if tx.send(Msg::Shutdown).is_err() {
                    break;
                }
            }
        })
        .context("spawning the signal thread")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripted_source_hands_back_its_messages_in_order_and_then_ends() {
        let events = Events::scripted(vec![Msg::Tick, Msg::Shutdown]);
        assert_eq!(events.recv().map(|m| m.name()), Some("tick"));
        assert_eq!(events.recv().map(|m| m.name()), Some("shutdown"));
        assert!(
            events.recv().is_none(),
            "the script should end rather than block"
        );
    }

    #[test]
    fn try_recv_drains_what_is_waiting_and_then_reports_nothing() {
        let events = Events::scripted(vec![Msg::Tick, Msg::Tick]);
        assert!(events.try_recv().is_some());
        assert!(events.try_recv().is_some());
        assert!(events.try_recv().is_none());
    }

    /// The `SIGTERM` criterion, as far as it can be tested without a pty: the
    /// handler is installed, a real signal is delivered to this process, and it
    /// comes out of the channel as [`Msg::Shutdown`] rather than killing us.
    ///
    /// That the resulting shutdown restores the terminal is `terminal.rs`'s
    /// business, and the end-to-end check is in `docs/tasks/20-tui-shell.md`.
    #[test]
    fn a_real_sigterm_arrives_as_a_shutdown_message_instead_of_killing_the_process() {
        let (tx, rx) = mpsc::channel();
        spawn_signals(tx).expect("handlers should install");

        // Raised rather than `kill`ed so the test needs no pid plumbing. The
        // default disposition for SIGTERM is to terminate; reaching the assertion
        // below at all is the evidence that the handler replaced it.
        signal_hook::low_level::raise(SIGTERM).expect("raising SIGTERM");

        let msg = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("SIGTERM should arrive on the channel");
        assert_eq!(msg.name(), "shutdown");
    }

    #[test]
    fn the_tick_is_slow_enough_to_be_free_and_fast_enough_to_be_live() {
        // Task 26 refreshes the MPD indicator on it and wants ~2 s worst-case
        // latency; anything under a second would be a wakeup nobody asked for.
        assert_eq!(TICK, Duration::from_secs(1));
    }

    #[test]
    fn the_tick_thread_stops_when_the_receiver_goes_away() {
        let (tx, rx) = mpsc::channel();
        spawn_tick(tx, Duration::from_millis(10));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .map(|m| m.name())
                .ok(),
            Some("tick")
        );
        drop(rx);
        // Nothing to join — the thread is detached — so what is asserted is that
        // dropping the receiver is all it takes, and that this test does not hang
        // or panic in a background thread as a result.
        thread::sleep(Duration::from_millis(50));
    }
}
