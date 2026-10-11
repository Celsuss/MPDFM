# The pending view and committing

Press `p` from anywhere to see the plan: every operation you have staged, in order.
This is the screen that exists so you can say yes knowing exactly what will happen.

```text
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
│ ! 1 file is in MPD's current queue; requeue after commit              │
└ [c] commit  [x] discard  [dd] drop op  [esc] back ────────────────────┘
```

(Illustrative.) It is the same renderer as `mpdfm move --dry-run`, so the two
cannot disagree.

## Reading it

- `j`/`k` move the cursor; `enter`, `space` or `l` **unfold** a playlist into every
  line that changes (`-` old, `+` new), or a refused operation into its reason;
  `h` folds it again. The diff is complete, not a count.
- **Conflicts** are shown in red with the reason on each operation, and commit is
  disabled until they are gone. Typical causes: a move onto an existing file, or a
  destination two operations both want.
- **Warnings** are not blockers — for instance, that a file is in MPD's current
  queue and will need requeueing.

## Acting on it

| Key | Does |
| --- | --- |
| `c` | commit the plan |
| `dd` | drop the operation under the cursor, then re-validate (dropping one can clear — or cause — a conflict) |
| `x` | discard the whole plan, after a confirmation |
| `u` | undo the transaction that was just committed |
| `esc` | back to the browser; the plan stays staged |

`dd` is doubled on purpose. `d` in the browser *stages a delete*, so a lone `d` here
would mean the opposite thing.

## Committing

A commit runs on a worker with a progress line, so the screen stays responsive on a
big organize. `esc` cancels it while it is still before the first change to the
library; after that it says so plainly, since half a transaction is not something to
abandon silently.

When it finishes you see the transaction id and `u` to undo, the library and
playlist index refresh, and you are told whether MPD's database update was queued.
If a commit fails part-way, the view shows what completed, what did not, and the
`mpdfm recover` command to run. See [Undo and recover](../guide/undo-recover.md) and
[Safety](../safety.md).

Staged operations survive moving between views and resizing. `u` also works from the
browser (undoing the latest transaction), and `:undo <txid>` reverses a specific one.
