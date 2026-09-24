# 19 — Tag CLI commands

- **Phase:** M2 · Tag editing
- **Depends on:** 15, 16, 17, 18
- **Status:** not started

## Goal

Expose tag reading and writing from the command line — useful on its own, and
the testable surface for M2.

## Details

```
mpdfm tag show <PATH>... [--json]
    per-file field table, plus AudioInfo; a directory shows all its audio files

mpdfm tag set <PATH>... [--dry-run] [--yes]
    --title, --artist, --album-artist, --album, --year, --track, --disc,
    --genre, --comment, --composer
    --clear <field> (repeatable)
    --renumber-tracks
    --title-from-filename
    -r/--recursive for directories

mpdfm tag diff <PATH>...        # what would change, without writing
```

`tag set` on multiple files uses the bulk semantics from task 18 and prints the
same `Effects` preview as `move`, then commits as one journaled transaction —
so `mpdfm undo` reverses a bad bulk tag edit exactly as it reverses a move.

## Acceptance criteria

- [ ] `tag show` on an mp3 and a FLAC prints the same field set
- [ ] `tag show --json` round-trips through a JSON parser
- [ ] `tag set --genre "Hip Hop" -r <album dir>` previews N files and commits one
      transaction
- [ ] `mpdfm undo` after a bulk tag set restores every file byte-for-byte
- [ ] `--dry-run` writes nothing
- [ ] `--clear genre` removes the frame
- [ ] `--renumber-tracks` on an album dir numbers by displayed order
- [ ] setting a field on a read-only file fails preflight, and no other file in
      the batch is modified
- [ ] non-TTY without `--yes` refuses

## Files

`src/cli/tag.rs`, `tests/cli_tag.rs`

## Pitfalls

- Partial failure in a batch: decide up front whether it is all-or-nothing.
  Recommended: preflight every file, and refuse the whole batch if any file is
  unwritable, matching the transactional model elsewhere.
