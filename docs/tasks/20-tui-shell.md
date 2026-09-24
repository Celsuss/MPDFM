# 20 — TUI shell and event loop

- **Phase:** M3 · TUI
- **Depends on:** 01, 04
- **Status:** not started

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

- [ ] `mpdfm` opens, draws a frame, and `q` exits with the terminal restored
      (`stty -a` unchanged before and after)
- [ ] an induced panic restores the terminal and prints a readable backtrace
- [ ] `SIGTERM` exits cleanly
- [ ] resizing to 40×10 shows the too-small message and recovers on resize back
- [ ] a 3 000-file scan runs on a worker; the UI stays responsive and shows
      progress
- [ ] idle CPU is ~0% (no busy redraw loop) — verify with `top` over 30 s
- [ ] no `println!`/`dbg!` reaches the terminal while the TUI is active
- [ ] the view stack pops overlays without losing underlying state

## Files

`src/tui/{mod.rs,app.rs,terminal.rs,event.rs,msg.rs}`

## Pitfalls

- Restoring the terminal in `main`'s happy path only is the classic bug. Guard +
  panic hook, both tested.
- Don't hold a lock on `Library` across a draw; clone the small view data the
  widgets need instead.
