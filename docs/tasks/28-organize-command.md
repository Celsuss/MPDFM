# 28 — `mpdfm organize` and its TUI integration

- **Phase:** M4 · Organize by template
- **Depends on:** 10, 15, 24, 27
- **Status:** not started

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

- [ ] `organize --dry-run` on the real library (read-only) completes and reports
      counts of placeable, already-correct and unplaceable files
- [ ] organizing a fixture album moves audio + aux and rewrites all playlist
      references; every previously resolvable reference still resolves
- [ ] `undo` after an organize restores a byte-identical fixture
- [ ] unplaceable files are listed and untouched
- [ ] `--only-missing` skips files already at their destination
- [ ] `--limit 5` stages exactly 5 files' worth of moves
- [ ] collisions block the commit and are listed with both sources
- [ ] the TUI organize view previews live as the template is typed and an
      invalid template shows an inline error
- [ ] a 2 000-op plan previews without the UI stalling, and commit shows progress

## Files

`src/cli/organize.rs`, `src/tui/views/organize.rs`, `tests/cli_organize.rs`

## Pitfalls

- A full-library organize is the single most dangerous thing this tool can do.
  Default to `--dry-run`-like caution: require confirmation, show the count
  prominently, and make sure the backup/journal path has been tested at that size.
- Test the transaction at scale once (a generated 2 000-file fixture) — journal
  size and commit time are worth knowing before the user finds out.
