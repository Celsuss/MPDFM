# 24 — Pending operations view (stage → preview → commit)

- **Phase:** M3 · TUI
- **Depends on:** 10, 11, 12, 20, 21
- **Status:** done

## Goal

The UI for decision D7 — the screen where the user sees exactly what is about to
happen to their library, including every playlist line, and says yes.

## Details

```
┌ PENDING (3 ops) ──────────────────────────────────────────────────────┐
│ MOVE  hiphop/MF DOOM - Mm Food/ → hiphop/MF DOOM/2004 - Mm..Food/     │
│       14 audio + 3 aux files                                          │
│ TAG   genre: "" → "Hip Hop"  (14 files)                               │
│ DEL   hiphop/MF DOOM - Mm Food/folder.nfo                             │
├ Affected playlists ───────────────────────────────────────────────────┤
│ ▾ Coding flow.m3u        3 lines rewritten                            │
│     - hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3                        │
│     + hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3                  │
│ ▸ Hip hop.m3u            1 line rewritten                             │
│ ▾ MPD saved queue        2 lines rewritten                            │
├ Warnings ─────────────────────────────────────────────────────────────┤
│ ! 1 file is in MPD's current queue; requeue after commit               │
└ [c] commit  [x] discard  [dd] drop op  [esc] back ────────────────────┘
```

Requirements:

- Renders `Effects` from task 10 — the *same* renderer the CLI's `--dry-run`
  uses, so the two cannot diverge.
- Expandable per-playlist line diffs (`-` old, `+` new). This is the screen that
  earns the user's trust; make the diff real and complete, not a count.
- Individual ops can be dropped (`dd`) and the whole plan discarded (`x`);
  dropping re-validates, since conflicts can appear or disappear.
- Conflicts render in red and disable commit, with the reason on each.
- Commit runs on a worker thread with a progress indicator (14 files is instant;
  a 2 000-file organize is not), and is cancellable before the first mutation.
- After commit: show the txid, offer `u` to undo right there, refresh the library
  and the playlist index, and report whether the MPD update was queued.
- On commit failure: show what completed, what didn't, and the recovery command.
- Staged ops survive navigating away and back; `q` with pending ops warns.

## Acceptance criteria

- [x] the preview shows every playlist line that will change, expandable
- [x] output matches `mpdfm move --dry-run` for the same plan (compare strings in
      a test — the anti-divergence check)
- [x] a conflicting plan cannot be committed and shows the reason per op
- [x] `dd` drops one op and re-validates
- [x] `x` discards everything after confirmation
- [x] commit shows progress and leaves the library view refreshed
- [x] post-commit `u` undoes and the browser reflects the reversal
- [x] a simulated mid-commit failure shows the recovery instructions
- [x] the MPD-current-queue warning appears when applicable
- [x] pending ops survive view changes and are not lost on resize

## Files

`src/tui/views/pending.rs`, `src/tui/widgets/diff.rs`

Also touched: `crates/core/src/ops/render.rs` (the one renderer now returns rows
as well as a string — see below), `crates/core/src/ops/commit.rs`
(`Options::progress` and `Options::cancel`, and the `Cancelled` error),
`src/tui/app.rs` (the view, the staging verbs, the commit and its progress, the
re-validation), `src/tui/keys.rs` (`dd` and the unfold keys in `[pending]`),
`src/tui/msg.rs` and `src/tui/work.rs` (the progress message, the cancel flag,
the live queue, `:undo <txid>`), `src/tui/command.rs` (`CommandLine::of`, for
`r`), `src/tui/widgets/mod.rs` (`fit_end`), `src/cli/move.rs`,
`docs/keys.example.toml`.

## Pitfalls

- Do not truncate the playlist diff for long plans; make it scrollable instead.
  A hidden change is exactly what this whole project exists to prevent.

## As built

A frame from `app.rs`'s own tests, at 100 columns, with two of the three
playlists unfolded:

```text
┌ pending ─────────────────────────────────────────────────────────────────────────────────────────┐
│PENDING (1 op)                                                                                    │
│MOVE   …op/MF DOOM - Mm..Food (2004) [V0] scene-tag → …ic/MF DOOM - Mm..Food (2004) [V0] scene-tag│
│        3 audio + 5 aux files, 4.8 kB, 2 playlists                                                │
│                                                                                                  │
│Playlists                                                                                         │
│▾ Hip hop.m3u     1 line rewritten                                                                │
│    - hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3                             │
│    + electronic/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3                         │
│▾ MF Doom.m3u     1 line rewritten                                                                │
│    - hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3                             │
│    + electronic/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3                         │
│▸ MPD saved queue 1 line rewritten                                                                │
└ c commit · dd drop op · x discard · enter expand · esc back ─────────────────────────────────────┘
1 marked · 1 pending · sort name · focus files · ○ mpd ?
```

Fold those two back up and every line above is `mpdfm move --dry-run`'s output
for the same plan, character for character — which is the next section.

## The anti-divergence check, made into something that can fail

The criterion asks for the TUI's output to match `mpdfm move --dry-run` for the
same plan, compared as strings. That is only meaningful if the two can actually
disagree, and the obvious way to satisfy it — have the view call
`Effects::render` and print the result — makes the check tautological *and* the
view useless: a string has nowhere to put a cursor and nothing to unfold.

So the renderer was split along the one seam that does not duplicate anything.
`Effects::lines(width)` returns `Vec<PreviewLine>`, each a `text` and a `kind`
saying what it is about — this operation, that playlist, this conflict — and
`Effects::render(width)` is **those lines joined with newlines**. The CLI keeps
the string; the pending view reads the kinds. There is one layout function, so
the only way the two can differ is if the view starts formatting something
itself, which is exactly what the test now catches:

```rust
// src/tui/views/pending.rs
assert_eq!(folded, effects.render(cells));   // at 40, 72, 80 and 132 columns
```

`folded` is the view's own body with every fold shut. Two small facts make it
work:

- **the kinds do not depend on the width.** Only the text does
  (`how_many_lines_there_are_and_what_they_are_about_does_not_depend_on_the_width`,
  in core). That is what lets the view work out what the cursor can land on, and
  which row is which playlist, without knowing how wide its pane is;
- **the expansion marker lives in the preview's own indent.** A playlist row is
  `"  Hip hop.m3u  1 line rewritten"`; the view writes `▸ ` over the two leading
  spaces rather than inserting it, so the row is the same width and every other
  row is byte-identical. The test undoes exactly that substitution and nothing
  else, and `only_a_playlist_row_is_ever_touched_by_that_normalization` holds it
  to the three rows it is allowed to touch.

One more thing came out of the split for free: `LineKind::Tag` carries *every*
staged operation its row stands for. A bulk tag edit is one `WriteTags` per file
shown as one row per field, so "the operation under the cursor" is fourteen
operations, and `dd` on that row has to drop all fourteen or the row would be
lying about what is left.

## Folding is the answer to the pitfall

Two kinds of row unfold, and the cursor can be on the rows they unfold into:

| row | unfolds into |
| --- | --- |
| a playlist, or MPD's saved queue | every line that changes: `-` what it says, `+` what it will say |
| a refused operation | the conflicts that name it, each with its reason |

A diff line being *selectable* is what makes a long plan scrollable without the
view holding a second scroll offset of its own: `j` walks into the diff, the
window follows the cursor a row at a time, and nothing is ever dropped to make
the plan fit. `a_long_diff_scrolls_rather_than_being_cut_short` asserts both
halves — one `-` row per changed line in a body taller than any pane, and a
six-row pane that reaches the first row and the last.

Refused operations unfold **by default**, with the cursor on the first of them.
A plan that cannot be committed is only useful to somebody who can see why, and
`Conflict::op()` has existed since task 10 for exactly this.

The destructive half is spelled out in words rather than signalled by absence: a
removed playlist line is a `-` row and then `entry 9 is removed, not rewritten`,
because losing a line the user wrote should not be indicated by the lack of a
`+`.

## Commit: progress, and one honest cancellation point

A commit was already on a worker (task 23). What this task adds is a
`commit::Options::progress` watcher — `Validating`, `BackingUp`, `Steps { done,
steps }`, `Playlists`, `Finishing` — forwarded down the channel as
`Msg::Committing` and drawn as `committing 1 operation · 37/412 files (9%)`. The
callback runs on the committing thread and does nothing but `send`, because a
progress indicator that slows the commit down is not an improvement.

Cancellation is the part worth being careful about. "Cancellable before the first
mutation" is a promise that can be kept exactly once, at one place:

```text
step 1  re-validate against a fresh scan     ← the slow part, nothing touched yet
        ──── cancel is asked here, once ────
step 2  take the backups                     ← the first thing on disk
```

`Options::cancel` is a `Fn() -> bool` consulted at that boundary and nowhere
else, and `CommitError::Cancelled` means *nothing was written*: no backup, no
record, nothing to recover. A flag polled throughout would make "cancelled"
indistinguishable from "failed partway" — half a transaction either way, with the
same recovery to do — so the view refuses instead: past that point, `esc` says
`too late to stop: the transaction is past the point where nothing had changed`.
The window is real rather than theoretical, because step 1 is a full rescan of
the library.

## What a finished transaction leaves on screen

`Report` is held by the view rather than flashed on the message line, because
every part of it is something to act on:

| | |
| --- | --- |
| committed | the txid, the journal's own headline, whether MPD was asked to rescan and how many directories, and every warning — with `u` offered right there, about **that** transaction by id and not "the latest" |
| failed | core's whole message: which step stopped it, and the `mpdfm recover <txid>` that puts it back. Wrapped by hand rather than shortened, because the recovery command is the last line of it |
| cancelled | that nothing changed and the plan is still staged |

Whether MPD was told is read off `record.mpd_update_requested` and
`record.mpd_update_failed` rather than inferred from the configuration, so a
daemon that refused the update says so.

The commit's rescan comes back through `Pending::revalidated`, which deliberately
leaves the report alone: the txid must not vanish out from under the `u` that was
offered with it.

## Dropping an operation re-validates, because conflicts move both ways

`dd` removes the operation from `App::plan` — descending, so removing one does
not move the next — and then validates the plan again from scratch. The view
never patches `Effects`.

That is not housekeeping. Dropping one operation can make a conflict *disappear*
(the two that wanted the same destination) and can equally make one *appear*: the
operation that was going to vacate a directory may be the one that went.
`dd_drops_one_operation_and_validates_what_is_left` stages a move that is refused
because it lands on an album nothing is moving, drops it, and asserts that what
is left can be committed — and its comment records the thing that made the test
harder to write than it looks: a move onto the *first* album's directory is
perfectly legal, because the planner orders a chain.

## Which door stages what

The view needed something to show, so three of the browser's verbs grew bodies.
None of them writes anything; all three stage and then show the preview.

| key | means |
| --- | --- |
| `m` | move the marks into the directory the browser is showing — mark, walk to where they belong, press the key |
| `r` | rename: opens the command line holding `:move <the path under the cursor>` |
| `d` | stage a delete of the marked **files** |
| `:move <dst>` | one source to that exact path, several into that directory |

`:move` with one source is also the rename, which is why `r` is a prefilled
command line rather than a prompt of its own: the destination of a rename is a
path, editing a path is what that line already does, and there is no second line
editor to get the Unicode arithmetic wrong in.

A delete is per *file*, because the unit of reversal is the file. A marked
directory is counted and reported rather than quietly expanded into everything
under it — deleting a tree is not a thing to infer from one keystroke.

Staging is the opposite order from the tag editor's, deliberately: `w` refuses to
stage an edit that cannot be committed, because the form is still open and the
user can fix it there, while a move that conflicts has nowhere else to be fixed.
This view *is* where it is fixed.

## The live queue, in one place

`Warning::InMpdQueue` only exists when MPD answered: the daemon writes its
in-memory queue over the state file when it stops, so an on-disk edit behind a
running MPD would be erased. That means the preview's answer depends on whether
the daemon is up — and commit **re-validates**, so a preview and a commit given
different answers is `Drift` and a refused commit.

So the live queue is held once, on `App`, filled in by the status poll and handed
to both `validate_live` and `commit::Options::live`. The poll only asks for it
while something is staged: it is the one part of a poll whose cost is the length
of the user's queue, this runs every second, and nothing but a preview reads it.
A queue that changes under an open preview re-validates it, which is rare enough
to be worth the one case it gets right.

## How it is tested

**The view, against a real fixture** (`src/tui/views/pending.rs`, 17 tests). The
`Effects` under test is always one `Plan::validate` produced from a `Fixture`,
never one written out by hand — the point of this screen is that it shows what
commit will do, and a hand-built `Effects` is one nothing will ever commit. The
anti-divergence comparison, the diff, the auto-unfolded refusals, the three
reports, `no_row_is_ever_wider_than_the_pane` over six widths, and
`a_pane_with_no_room_draws_nothing_rather_than_panicking`.

**The diff widget** (`src/tui/widgets/diff.rs`, 5 tests), including
`two_thousand_changed_lines_are_two_thousand_rows_and_none_of_them_is_dropped`
and a width check that goes down to zero cells.

**The renderer's two shapes** (`crates/core/src/ops/render.rs`, 4 new tests):
that the string is the lines joined, that the kinds are width-independent, that
every row names the operations it stands for.

**Progress and cancellation, against a real commit**
(`crates/core/tests/journal_and_commit.rs`, 3 new tests).
`a_commit_reports_its_progress_step_by_step` asserts one `Steps` per step
counting up to the number the record has — so a progress indicator cannot be a
fraction of the wrong number — and `a_cancelled_commit_changes_nothing_at_all`
snapshots the library and the playlists, cancels, and asserts both trees are
byte-identical afterwards and that the journal has nothing to recover.

**The whole path** (`src/tui/app.rs`, 14 new tests). A `Fixture`, a
`TestBackend` and real worker threads: `m` stages and the view appears, `esc` and
a resize do not lose the plan, `dd` through the keymap as two keypresses, `x` and
its confirmation, a conflicting plan that `c` refuses,
`c_commits_on_a_worker_and_leaves_the_browser_showing_the_result` (which reads
the playlist off disk afterwards and checks the rescanned library),
`u_after_a_commit_undoes_it_and_the_browser_shows_the_reversal`, and
`the_mpd_queue_warning_shows_when_the_daemon_is_holding_one_of_the_files`.

The mid-commit failure is simulated the way `commit::Inject` simulates one in
core: a real `CommitError::Step` value, rendered by core, handed to the view —
`a_commit_that_stopped_partway_shows_what_to_run_to_put_it_back` asserts the
screen carries `stopped at step 3` and
`mpdfm recover 20260101T101010Z-abcd`, and that the plan is still staged, because
nothing about a failure says the user has changed their mind.

## Measured

`a_four_hundred_line_diff_scrolls_at_well_under_a_frame_a_millisecond`: a
400-operation plan whose playlist diff is 800 unfolded rows, scrolled a row per
frame with nothing cached between frames — the preview is never patched, so every
frame lays the whole thing out again.

| | |
| --- | --- |
| release | **530 µs** per frame |
| unoptimized | 3.2 ms per frame |

Four hundred operations is a whole-library organize; the frame budget at 60 Hz is
16 ms. The number came down from 3.9 ms (debug) by laying the plan out **once**
per keypress instead of four times: `Pending::go` computes the layout, derives
both what the cursor can land on and where that row is, and hands the same list
to the scroll.
