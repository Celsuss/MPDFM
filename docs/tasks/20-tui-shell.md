# 20 — TUI shell and event loop

- **Phase:** M3 · TUI
- **Depends on:** 01, 04
- **Status:** done

## Goal

A ratatui application that starts, draws, resizes, handles errors and — above
all — **always restores the terminal**, including on panic.

## Details

- `crossterm`: alternate screen, raw mode, mouse capture off by default,
  bracketed paste off.
- A `TerminalGuard` with a `Drop` impl that leaves the alternate screen and
  disables raw mode, plus a `std::panic::set_hook` that restores the terminal
  *before* printing the panic, so a crash never leaves the user with a dead shell.
- Event loop: block on `crossterm::event::read()` in a thread, funnel
  `Event`s into a channel alongside internal messages
  (`Msg::Tick`, `Msg::ScanDone`, `Msg::MpdStatus`, `Msg::TaskDone`). Draw only
  when something changed, plus a slow tick (~1 s) for the MPD status indicator.
- Long operations (scan, tag read for a window, commit) run on a worker thread
  and report progress via the channel; the UI never blocks on I/O.
- `App` holds: `Config`, `Library`, `PlaylistIndex`, `Plan` (the staged ops),
  `Focus`, a view stack (`Vec<View>` so overlays pop cleanly), and a status/toast
  queue.
- Screen-size floor: below ~60×15 draw a "terminal too small" message rather
  than a broken layout.
- `SIGWINCH`/resize redraws; `SIGTERM` exits cleanly through the guard.
- `--no-alt-screen` and `--log <file>` flags for debugging (logging must go to a
  file, never stdout, while the TUI owns the terminal).

## Acceptance criteria

- [x] `mpdfm` opens, draws a frame, and `q` exits with the terminal restored
      (`stty -a` unchanged before and after) — `tests/tui_terminal.rs`,
      `q_quits_and_leaves_the_terminal_exactly_as_it_was`
- [x] an induced panic restores the terminal and prints a readable backtrace —
      `an_induced_panic_restores_the_terminal_before_it_prints_the_backtrace`,
      which also asserts the message is printed *after* the alternate screen is
      left
- [x] `SIGTERM` exits cleanly — `sigterm_exits_cleanly_through_the_guard`, and
      `a_real_sigterm_arrives_as_a_shutdown_message_instead_of_killing_the_process`
      in `src/tui/event.rs` for the handler itself
- [x] resizing to 40×10 shows the too-small message and recovers on resize back —
      `a_resize_below_the_floor_shows_the_message_and_recovers_when_it_grows_back`
      through a real pty, plus
      `a_terminal_below_the_floor_says_so_and_recovers_when_it_grows` headless
- [x] a 3 000-file scan runs on a worker; the UI stays responsive and shows
      progress — `a_three_thousand_file_scan_runs_on_a_worker_while_the_ui_keeps_drawing`
- [x] idle CPU is ~0% (no busy redraw loop) — **0 to 1 clock ticks over 30 s**,
      0.00–0.03% of one core; `just verify-tui` measures it, see below
- [x] no `println!`/`dbg!` reaches the terminal while the TUI is active —
      `the_log_file_gets_the_session_and_the_terminal_gets_none_of_it` runs with
      `-v --log` and asserts every logged line is in the file and none of them on
      the screen
- [x] the view stack pops overlays without losing underlying state —
      `an_overlay_pops_without_losing_what_was_underneath`

## Files

`src/tui/{mod.rs,app.rs,terminal.rs,event.rs,msg.rs}`, as planned, plus two the
plan did not name:

- **`src/tui/work.rs`** — the worker threads. They were going to be a section of
  `app.rs`, and they are a different concern: nothing in them borrows from `App`,
  which is what makes the "don't hold a lock across a draw" pitfall structurally
  impossible rather than merely avoided.
- **`src/tui/log.rs`** — `--log`. Where diagnostics go is not a terminal-setup
  question even though it is caused by one.

Tests: `tests/tui_terminal.rs` (the pty checks) and `scripts/verify-tui.sh`
(`just verify-tui`, the CPU measurement).

One addition to core: **`Library::scan_reporting`**, the walk with a progress
callback, in `crates/core/src/library/`. `Library::scan` is now that function with
an empty callback, so there is one walk and not two. The callback fires every 256
files, which turns a 3 100-file library into twelve messages rather than 3 100.

## Pitfalls

- Restoring the terminal in `main`'s happy path only is the classic bug. Guard +
  panic hook, both tested.
- Don't hold a lock on `Library` across a draw; clone the small view data the
  widgets need instead.

## How it is tested

The split is by what needs a real terminal and what does not, because the second
kind is cheap and the first kind is not.

**Headless, in `src/tui/`.** The loop takes its messages from an `Events`, and
`Events::scripted` builds one from a `Vec<Msg>` with no thread and no tty. A test
scripts keys, resizes and worker results, runs the loop against a `TestBackend`,
and asserts on the buffer. That covers the frame, the size floor, the view stack,
the cursor, the toast, the MPD indicator, and — the one that is easy to lose
later — that a message which changes nothing visible asks for no redraw.

**Through a pseudo-terminal, in `tests/tui_terminal.rs`.** Raw mode and the
alternate screen are properties of a real terminal; a `Vec<u8>` cannot be left in
raw mode. `script(1)` allocates a pty, and inside it each test does
`stty -g > before`, runs the binary, `stty -g > after`, and compares. That is the
criterion "`stty -a` unchanged" mechanised, and it is a stronger check than
looking for escape sequences in the output — which is the point, because it caught
a real bug (below). Without `script` installed those tests report a skip rather
than a failure.

**Two debugging seams**, in the spirit of `MPDFM_ASSUME_TTY` (task 15): the
scripted `Events` above, and `MPDFM_TUI_PANIC=draw|event`, which panics at a named
point so that the panic hook can be tested against a real terminal rather than
asserted about. Setting it by accident ends the process with a panic that restores
the terminal first, which is the behaviour under test.

## Verifying it by hand

`just verify-tui [seconds] [binary]` runs the two checks that are about a real
process over real time rather than about a function:

- **idle CPU**, read from the process's own `utime + stime` in `/proc` either side
  of a 30-second sleep. A polling loop would show up here and nothing else would;
  the recipe fails above 1% of a core, which is two orders of magnitude above what
  a correct event loop costs and well below what a busy one does;
- **the terminal's modes** across a `SIGTERM`, which is the same thing
  `tests/tui_terminal.rs` asserts, repeated outside `cargo test` so it can be run
  against a release binary on a machine that is misbehaving.

Running the TUI against the real `~/Music` is **authorized**: see task 15's
"Verifying it by hand". The only thing the shell does to the library is the startup
scan, which `stat`s and opens nothing.

## What the hand verification found

Run on 2026-10-06 against the release binary and the author's real library.

**The numbers.** The startup scan of `~/Music` is **3 132 files in 318
directories in 9–11 ms** warm, reported in twelve progress messages that are
visibly on screen as it goes. The library has grown since task 15 measured it
(2 809 files, 318 directories); the directory count is unchanged, so the growth is
inside existing albums. 11 ms is far below the threshold at which a scan behind a
keystroke is noticeable, which settles the question task 15 left open.

An idle session costs **0 to 1 clock ticks over 30 seconds** across repeated runs
— 0.00% to 0.03% of one core — in **4 threads** (main, input, tick, signals) and
**7.2–7.4 MB** resident. The whole budget is the one-second timer, and a tick that
finds nothing to retire asks for no frame, so most of them are a wakeup and a
`match`. There is no other periodic wakeup in the program.

**A real bug, caught by the pty test and not by anything else.**
`TerminalGuard::restore` wrote every escape sequence — show the cursor, leave the
alternate screen — and never called `disable_raw_mode`. The screen therefore looked
perfectly restored while the shell had no echo and no line editing, which is the
failure mode that needs a blind `reset` to fix. Only the panic hook, which went
through a different function, was correct. Every assertion about the output bytes
passed; it was comparing `stty -g` that failed. Both paths now go through one
`restore_terminal`, and raw mode and the sequences are one job that cannot be done
by halves.

**Two things the real library showed that the fixture did not.** The root
directory's label was empty — `DirPath::root()` displays as the empty string,
which is right for joining a path and wrong for a pane title, so the browser was
titled `  ` and the status bar started with a stray `·`. And `--no-mpd` reported
`○ offline`, which reads like a daemon that is down rather than one nobody asked:
the snapshot now carries whether MPD was enabled at all, and the bar says
`· mpd off`.
