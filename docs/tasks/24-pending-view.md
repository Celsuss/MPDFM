# 24 — Pending operations view (stage → preview → commit)

- **Phase:** M3 · TUI
- **Depends on:** 10, 11, 12, 20, 21
- **Status:** not started

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

- [ ] the preview shows every playlist line that will change, expandable
- [ ] output matches `mpdfm move --dry-run` for the same plan (compare strings in
      a test — the anti-divergence check)
- [ ] a conflicting plan cannot be committed and shows the reason per op
- [ ] `dd` drops one op and re-validates
- [ ] `x` discards everything after confirmation
- [ ] commit shows progress and leaves the library view refreshed
- [ ] post-commit `u` undoes and the browser reflects the reversal
- [ ] a simulated mid-commit failure shows the recovery instructions
- [ ] the MPD-current-queue warning appears when applicable
- [ ] pending ops survive view changes and are not lost on resize

## Files

`src/tui/views/pending.rs`, `src/tui/widgets/diff.rs`

## Pitfalls

- Do not truncate the playlist diff for long plans; make it scrollable instead.
  A hidden change is exactly what this whole project exists to prevent.
