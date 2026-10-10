# 26 — Status bar, help overlay and error surface

- **Phase:** M3 · TUI
- **Depends on:** 13, 20, 21
- **Status:** done

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

- [x] status bar shows path, marks, pending count and MPD state, and updates live
- [x] the MPD indicator flips to offline when the daemon is stopped, within ~2 s,
      with no UI stall
- [x] `?` opens a scrollable help generated from the keymap; a remap changes it
- [x] a commit posts a toast with the txid and the undo hint
- [x] an error opens a panel that must be dismissed, showing the full message
      and the path
- [x] `:messages` lists recent messages
- [x] long messages wrap rather than truncate
- [x] the first-run/no-library screen appears when `music_dir` is missing
- [x] the status bar stays correct at 60 columns (elide the least important parts
      first, in a defined order)

## Files

`src/tui/widgets/{statusbar.rs,help.rs,toast.rs,progress.rs}`,
`src/tui/views/error.rs`

Also touched: `src/tui/app.rs` (the wiring: gathering the bar's values, the
dynamic message-line height, the scroll shared by help, `:messages` and the
error panel, the stale-poll clock, the first-run screen, `esc` on a scan),
`src/tui/widgets/mod.rs` (`wrap`, the one word-wrapper every message goes
through), `src/tui/command.rs` (`:messages`), `src/tui/msg.rs` and
`src/tui/work.rs` (`ScanCancelled`, the scan's cancel flag),
`src/tui/views/search.rs` (`Finding::progress`, for the bar), `src/tui/mod.rs`
(where the config could have come from), and in core
`crates/core/src/library/{scan.rs,model.rs}` (`Library::scan_cancellable`) with
its test in `crates/core/tests/library.rs`.

## Pitfalls

- The MPD poll must be on a worker with a short timeout; a hung daemon must not
  freeze the UI.
- Decide the elision order for the status bar explicitly, or narrow terminals get
  a jumbled bar.

## As built

Two frames from `app.rs`, at 100 and at 60 columns, with one move staged, MPD
playing, and a commit's toast on the bottom line:

```text
electronic · 1 marked · 1 pending · sort name · focus files                ● playing 01 Doomsday.mp3
committed 20260924T224500Z-a3f1 — 14 file(s) moved, 2 playlist(s) · u to undo
```

```text
electronic · 1 marked · 1 pending   ● playing 01 Doomsday.m…
committed 20260924T224500Z-a3f1 — 14 file(s) moved, 2
playlist(s) · u to undo
```

The second is the message line growing to two rows, and the body giving up the
row, rather than the txid losing its end.

## The status bar: a declared elision order

The pitfall says decide it, so it is a list — `statusbar::ELISION` — and the
bar takes one step of it at a time, least important first, until it fits:

| step | part | what happens |
| --- | --- | --- |
| 1 | sort | dropped — the listing's order shows it |
| 2 | focus | dropped — the highlighted cursor shows it |
| 3 | song | shortened, then dropped |
| 4 | path | shortened from the left (`…Food (2004) [320]`), then dropped |
| 5 | warnings | `⚠ 2 warnings` → `⚠2` |
| 6 | marked, pending | `14 marked` → `14m`, `3 pending` → `3p` |
| 7 | filter, find | shortened to a floor, **never dropped** |
| 8 | MPD | `● playing` → `●` |

`VISUAL` and `WRITING` are never touched. MPD sits on the right, on its own, so
"is the daemon up" is a glance at the same place every time.

The test that holds it, `parts_go_in_the_declared_order_and_do_not_come_back`,
narrows a worst-case bar one column at a time from 200 to 60 and asserts that
the parts disappear in exactly that order and never reappear at a narrower
width — so dragging a terminal narrower never flickers between layouts. A
second test asserts the bar is never wider than its row at every width from 1
to 200, with a CJK song name in it.

One bug worth recording because it is this project's favourite: the separator
` · ` is **four bytes and three cells**, and the first version measured it with
`len()`. The bar came out eight cells short at full width. It is `SEP_W` now,
with a test that `width(SEP) == SEP_W`.

The path is back on the bar. Task 22 had left it off because the listing's
title carries it; the task asks for it, and with an elision order there is no
longer a cost — it is the fourth thing to go.

## The MPD indicator, and a poll that never comes back

The poll was already a worker with 250 ms socket timeouts (task 20). That is a
timeout per *read*, not per *poll*: a daemon trickling a byte every 200 ms never
trips it, and a name lookup has no timeout at all. So the shell keeps its own
clock, `MPD_STALE`: a poll out for two seconds turns the light to `○ offline`.
The stuck thread is not joined by another one every tick — `mpd_in_flight`
stays set — and when it does answer, the answer is believed.

`Light` (in `statusbar.rs`) is also what decides whether a poll is worth a
frame: two snapshots with equal lights look identical, so an elapsed-time field
ticking on every poll costs nothing.

Tested against real sockets: `FakeMpd` in `app.rs`'s tests is a few lines of
`TcpListener` that answer `status` and `currentsong` like a playing daemon.
`the_mpd_indicator_goes_offline_when_the_daemon_stops_without_stalling_the_ui`
polls it, stops it, polls again, and asserts the bar reads `○ offline`, that
`TICK` plus the answer is under two seconds, and that `update(Tick)` itself took
under 20 ms on the UI thread. A second test does the same against a port that
accepts and never says a word. A third drives the stale clock with
`tick_at(asked + MPD_STALE)` rather than sleeping.

Against the real daemon on this machine, launched in a pty on the real
library: `○ mpd ?` during the 32 ms scan, then `● paused MF_DOOM_-_Lofi_Villain…`.

## Help: every mode, the current one first

`widgets::help::lines` reads `KeyMap::help()` for **all** modes, with a header
per mode, and puts the one it was opened from first — `?` in the tag editor is a
question about the tag editor. Then the `:` commands from `command::USAGE` (the
table the parser is tested against, so `:messages` appeared in the help by being
added to the parser), then `ctrl-c`, which is not in the keymap and cannot be.

The help, `:messages` and the error panel scroll through one function
(`App::scroll_action`) and draw through one (`scrolled`), so all three take the
same keys and say `N more — j / k to scroll` in the same words.

## Messages: a queue and a history

`widgets::toast::Toasts` holds two lists for two questions. The **queue** is
task 20's: one message on the line at a time, each with its own clock that
starts when it reaches the front. The **history** is everything ever queued
*and every error* — an error is never a toast, it is a panel, and once the
panel is dismissed `:messages` (or vim's `:mes`) is the only place it is still
written down. Both are bounded (16 and 200); the history numbers its entries
for the session, so the numbers do not restart when the oldest fall off.

**Wrapped, never truncated.** `widgets::wrap` is the one word-wrapper for
toasts, the error panel and `:messages`, and none of them uses `Paragraph`'s:
the number of rows a message takes decides the layout, and a count made by one
wrapper and a rendering made by another are two answers that drift.
`App::message_rows` asks the toast how many rows it wraps to (up to three), and
both the frame and `App::body` use that answer, so the listing's scroll
arithmetic agrees with what was drawn. Past three rows the last one says
`… :messages for the rest`. A word with no spaces — a path — is broken at the
edge of the line rather than cut off.

**A commit's toast** is `committed <txid> — <summary> · u to undo`, posted
whether or not the pending view was watching: it is what `:messages` remembers
the commit by, and the txid is what `mpdfm undo` takes. The undo key is read
off the keymap.

## The error panel

`views::error::ErrorReport` is the message, the path, and the next step, each
optional but the first. The path and the next step are on lines of their own,
hung under a label, so a long path wraps as one path:

```text
┌ error ──────────────────────────────────────────────────────────────────┐
│transaction 20260924T224500Z-a3f1 stopped at step 3 of 8: renaming       │
│hiphop/MF DOOM - Mm..Food/01 Beef Rap.mp3 failed: Permission denied (os  │
│error 13)                                                                │
│                                                                         │
│path  /home/user/Music/hiphop/MF DOOM - Mm..Food                         │
│next  run `mpdfm recover 20260924T224500Z-a3f1` from a shell before      │
│      anything else                                                      │
└ esc to dismiss ─────────────────────────────────────────────────────────┘
```

Every `fail` that knew a path or a next step now says it: a scan that could not
read the library (the music directory, and `R`), a selection the tag editor
could not read (the directory, and unmark them), a commit that stopped (the
journal, and `mpdfm recover`), an undo that was refused (`mpdfm undo --list`).
A plain string still converts, because most of core's errors already name the
path in their sentence. The panel scrolls when even wrapping is not enough —
forty unreadable files is not a thing any panel holds.

## Progress, and a scan `esc` can stop

`widgets::progress::Progress` is one shape for a scan, a commit and a library
search: a label, a bar **only when the total is known**, a detail, and the key
that stops it **only when it can be stopped** — which the owner decides, not the
widget. A commit gets a bar (it knows its steps) and the key only before its
first change; a search gets a bar and the key; a scan gets a count and the key.

A scan could not be stopped before this task. `Library::scan_cancellable` in
core reads an `AtomicBool` once per directory entry — one relaxed load per
`lstat`, nothing next to the `lstat` — and returns `Ok(None)` when it is set:
**nothing**, not the part of the tree it had reached, because half a library is
a model a move would trust to know every file that references it. The worker
turns that into `Msg::ScanCancelled`, which is its own message precisely so that
being asked to stop does not open the panel a failure opens. The listing on
screen is kept — it is the last library that was walked whole.

`esc` in the browser now means, most recent first: stop a search, **stop a
scan**, abandon a visual range, clear the filter, leave a result set, pop.

## The first-run screen

The task calls the file `mpdfm.toml`; MPDFM's config is
`$XDG_CONFIG_HOME/mpdfm/config.toml` (task 04), and the screen names whichever
file is actually in play — `--config` when it was given.

A `music_dir` that does not exist, or that holds no files, replaces the body
with this instead of an empty pane or an error panel:

```text
┌ no library ────────────────────────────────────────────────────────────────┐
│There is no library to show: music_dir does not exist.                      │
│                                                                            │
│  music_dir  /home/user/Music                                               │
│  set by     /home/user/.config/mpd/mpd.conf (music_directory)              │
│                                                                            │
│MPDFM takes music_dir from the first of these that sets it:                 │
│                                                                            │
│  1. --music-dir on the command line                                        │
│  2. music_dir = "..." in /home/user/.config/mpdfm/config.toml              │
│  3. music_directory in the first mpd.conf on MPD's own search path:        │
│       /home/user/.config/mpd/mpd.conf                                      │
│       /home/user/.mpdconf                                                  │
│       /etc/mpd.conf  ← the one MPD reads                                   │
│                                                                            │
│Fix whichever applies, then R to scan again.                                │
│`mpdfm config show` prints every setting and where it came from.            │
└────────────────────────────────────────────────────────────────────────────┘
```

Every path is the real one — the search path is `mpd_conf_candidates` for this
`$HOME` — so it can be copied and opened. Which candidate exists is worked out
once, in `App::locate_config`, not in the draw: the screen is redrawn every
frame and a frame does no I/O. `R` is all it takes once the user has fixed it,
which the test does by creating the directory and pressing it.

## How it is tested

**The widgets**, each on its own, with no app: `statusbar.rs` (7 — the order,
the floor, the width at every column count, the path keeping its end, the three
lights), `toast.rs` (6 — wrapping, the `:messages` pointer, errors remembered
not queued, bounded history), `progress.rs` (4 — the bar only with a total, no
key past a commit's first change, narrowing never cuts the key), `help.rs` (3),
`views/error.rs` (3), and `wrap` itself (3, over the CJK and `So Hï` names).

**The app** (`app.rs`, 14 new): one per criterion, through `App::update` and a
`TestBackend` — `the_status_bar_shows_path_marks_pending_and_mpd_and_follows_them`,
the three MPD tests above,
`the_help_has_every_modes_keys_with_the_current_mode_first` (with a remap in
`[pending]` showing in the pending section),
`a_commit_posts_a_toast_with_the_txid_and_how_to_undo_it` (a real commit on a
worker, and the txid parsed back with `TxId::parse`),
`an_error_has_to_be_dismissed_and_shows_the_message_the_path_and_the_next_step`
(a minute of ticks, `j` and `space` do not dismiss it; `esc` does; `:messages`
still has it), `an_error_too_long_for_its_panel_scrolls_rather_than_being_cut`,
`colon_messages_lists_what_was_said_newest_first`,
`a_long_toast_wraps_onto_more_rows_rather_than_being_cut` (at 60 columns, the
two bottom rows joined are the message exactly),
`a_missing_music_dir_opens_the_first_run_screen_and_r_tries_again` (a real scan
on a worker, both ways), `an_empty_music_dir_says_so_rather_than_showing_an_empty_pane`,
`at_sixty_columns_the_status_bar_keeps_what_matters_and_stays_on_its_row`, and
`esc_calls_a_scan_off_and_the_line_says_it_can`.

**Core** (`crates/core/tests/library.rs`):
`a_scan_that_is_called_off_returns_nothing_rather_than_half_a_library` — set
before the walk, never set, and set from inside the progress callback, which is
the walking thread and so exactly the moment a worker's flag would be seen.
