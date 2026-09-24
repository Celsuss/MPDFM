# 26 — Status bar, help overlay and error surface

- **Phase:** M3 · TUI
- **Depends on:** 13, 20, 21
- **Status:** not started

## Goal

The parts that make the app legible: what is selected, what is staged, whether
MPD is alive, what keys exist, and what just went wrong.

## Details

**Status bar** (bottom, always visible): current path · marked count · pending op
count · active filter · MPD indicator (`● connected` / `○ offline` / `◐ updating`,
refreshed on a ~1 s tick, never blocking) · current song when playing.

**Help overlay** (`?`): generated from the live keymap (task 21) grouped by mode,
scrollable, with the command-mode commands listed. Because it is generated, it
cannot document a binding that no longer exists.

**Toasts / message line**: transient success and info messages
("committed 20260924T224500Z-a3f1 — 14 files, 2 playlists · u to undo"), with a
`:messages` command to see the last N. Errors are not transient: they open a
dismissible panel with the full message, the path involved, and the suggested
next step. Never swallow an error into a one-line truncation.

**Scan/commit progress**: a slim progress line with counts, cancellable where the
operation is (scan yes, mid-commit no).

**First-run state**: if `music_directory` cannot be found or is empty, show an
explanatory screen pointing at `mpdfm.toml` and the mpd.conf search path, rather
than an empty pane.

## Acceptance criteria

- [ ] status bar shows path, marks, pending count and MPD state, and updates live
- [ ] the MPD indicator flips to offline when the daemon is stopped, within ~2 s,
      with no UI stall
- [ ] `?` opens a scrollable help generated from the keymap; a remap changes it
- [ ] a commit posts a toast with the txid and the undo hint
- [ ] an error opens a panel that must be dismissed, showing the full message
      and the path
- [ ] `:messages` lists recent messages
- [ ] long messages wrap rather than truncate
- [ ] the first-run/no-library screen appears when `music_dir` is missing
- [ ] the status bar stays correct at 60 columns (elide the least important parts
      first, in a defined order)

## Files

`src/tui/widgets/{statusbar.rs,help.rs,toast.rs,progress.rs}`,
`src/tui/views/error.rs`

## Pitfalls

- The MPD poll must be on a worker with a short timeout; a hung daemon must not
  freeze the UI.
- Decide the elision order for the status bar explicitly, or narrow terminals get
  a jumbled bar.
