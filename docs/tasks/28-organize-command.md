# 28 — `mpdfm organize` and its TUI integration

- **Phase:** M4 · Organize by template
- **Depends on:** 10, 15, 24, 27
- **Status:** done

## Goal

Apply a template to a selection or the whole library as one reviewable,
reversible transaction — the bulk end of decision D3.

## Details

```
mpdfm organize [PATH]... [--template <T>] [--dry-run] [--yes]
               [--only-missing] [--no-aux] [--limit N]
```

Behaviour:

- Build a `Plan` of `MoveFile`/`MoveDir` ops from task 27's mapping, then run it
  through the normal `validate` → preview → `commit` pipeline. No special path,
  no separate execution engine.
- Default template comes from config (`organize_template`).
- `--only-missing` restricts to files that are currently not where the template
  says they should be *and* have complete tags — the practical way to run this
  incrementally on a 2 800-file library.
- `--limit N` for a cautious first run.
- Report the unplaceable files separately at the end, as a to-do list: this is
  effectively "which albums need tagging before they can be organized", which is
  the loop the user will actually work through.

TUI: `o` in the browser opens the organize view — template input with live
preview of the first ~20 resulting paths as you type, then staging into the
pending view (task 24) where the full diff and playlist changes are reviewed and
committed.

Given the scale (a full-library organize could move 2 800 files and rewrite every
playlist), the preview must be scrollable and the commit must show progress.

## Acceptance criteria

- [x] `organize --dry-run` on the real library (read-only) completes and reports
      counts of placeable, already-correct and unplaceable files
- [x] organizing a fixture album moves audio + aux and rewrites all playlist
      references; every previously resolvable reference still resolves
- [x] `undo` after an organize restores a byte-identical fixture
- [x] unplaceable files are listed and untouched
- [x] `--only-missing` skips files already at their destination
- [x] `--limit 5` stages exactly 5 files' worth of moves
- [x] collisions block the commit and are listed with both sources
- [x] the TUI organize view previews live as the template is typed and an
      invalid template shows an inline error
- [x] a 2 000-op plan previews without the UI stalling, and commit shows progress

## Hand-verified against the real library

Read-only, as `docs/PLAN.md` §8 allows: `mpdfm organize --dry-run --no-mpd`
with the default template, release build. **1 852 tracks and 135 aux files
placeable, 0 already in place, 957 unplaceable** (task 27's 956 plus the one
ADTS-in-`.mp3` file whose tags cannot be read), 0 collisions, 31 split-album
warnings and 6 case-only warnings. The to-do list is 957 files in 138
directories. 0.4 s end to end with the tags in the page cache. Exit 0, nothing
written.

## Measured at scale

- **CLI** (`two_thousand_files_organize_and_undo_byte_identically`): 2 000
  tracks + 200 aux files in 200 albums, one playlist naming every track. Organize
  and commit took ~3 s end to end in a debug build; the journal is **2.9 MB**.
  `undo` restores the fixture byte for byte.
- **TUI** (`a_two_thousand_file_organize_stages_and_scrolls_without_stalling`):
  `enter` maps, validates and stages 2 000 ops in **206 ms** release (663 ms
  debug), and the pending view then scrolls at **3.7 ms per frame** release
  (18.8 ms debug). Per-frame cost grows with the plan, since the pending view
  lays out the whole preview each frame; it is well inside a 60 Hz frame at
  this size, and the first place to look if a larger plan ever stutters.

## Decisions

**Per-file `MoveFile`s, never `MoveDir`.** Task 27's mapping has already
decided where every aux file goes (`Mapping::operations`). A directory move
would be a second, coarser opinion that also sweeps up whatever the scan could
not model.

**Mapping conflicts refuse the whole run**, in the CLI (exit 2, every source
listed) and in the TUI (an error panel over the still-open organize view). The
mapping drops colliding files from its moves, so staging the rest would commit
cleanly and leave them behind without a word.

**An unreadable file does not refuse the run**, unlike `tag set`: nothing is
written to it, it simply stays where it is and goes on the to-do list.

**`--only-missing` maps with `Template::strict()`**: every `{x?}` and
`{x|default}` becomes required, so a file is only placed when its tags are
complete. Files already in place are skipped in either mode; the strictness is
the difference.

**`--limit N` maps again on the first N moving tracks** rather than cutting
the full mapping, so an album the limit splits keeps its aux files where they
are, as any split album does.

**Output order.** Preview, then split and case warnings, then collisions, then
the counts in bold, then the prompt, which names the file count ("Move 1 987
files?"). The to-do list comes last, grouped by directory and reason.

**The TUI view edits like the `:` line.** It takes `[command]`'s bindings, so
every letter types itself. Only the first 20 files are rendered per keystroke;
the full mapping runs on `enter`. Directories among the marks are expanded
recursively, and with nothing marked or under the cursor, `o` means the
directory on screen.

**Not exposed:** `SplitAux::FollowMajority` and lower-casing extensions. Both
are `Options` fields in core, waiting for a flag if wanted.

## Found along the way

Bare `mpdfm undo` picks the newest transaction by `TxId`, but two ids minted by
*different processes* in the same second sort by their random salt. So
`mpdfm tag set …; mpdfm organize …; mpdfm undo` within one second can choose the
tag write. Here it refused safely, because the files had moved. The comment on
`TxId` says only retention reads the order, and that is not true. Not fixed here:
`tests/cli_organize.rs` undoes by explicit txid. Worth its own task.

## Files

`src/cli/organize.rs`, `src/tui/views/organize.rs`, `tests/cli_organize.rs`

Also touched: `crates/core/src/organize/{template.rs,plan.rs}`
(`Template::strict`, `Mapping::operations`, `Mapping::tracks_moving`),
`src/cli/{mod.rs,move.rs,tag.rs}`, `src/tui/{app.rs,msg.rs,work.rs,views/mod.rs}`.

## Pitfalls

- A full-library organize is the single most dangerous thing this tool can do.
  Default to `--dry-run`-like caution: require confirmation, show the count
  prominently, and make sure the backup/journal path has been tested at that size.
- Test the transaction at scale once (a generated 2 000-file fixture) — journal
  size and commit time are worth knowing before the user finds out.
