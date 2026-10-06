# 19 — Tag CLI commands

- **Phase:** M2 · Tag editing
- **Depends on:** 15, 16, 17, 18
- **Status:** done

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

Four more actions came out of task 18 and are exposed with the two the task
names: `--album-artist-from-artist`, `--strip-comment`, `--trim-whitespace`.
`--album-artist` also accepts `--albumartist`, because that is what `tag show`
prints the field as.

`tag diff` is `tag set` with the same arguments and no writing. It prints the
**per-file** before-and-after that the preview's field-level summary deliberately
leaves out — `"Soundtrack" → "Hip Hop"` for each path, with `<none>` where there
is no value yet — and then the preview `set` would have shown. `--dry-run` stops
at the same point; `diff` is the spelling for when what you want is to look.

Which files an argument selects:

- a file is itself;
- a directory contributes the audio files **in** it, and with `-r` the ones below
  it as well. `-r` is on `show` too, because a multi-disc album directory holds no
  audio of its own and `tag show` on one should not be silently empty;
- the result is sorted by path and deduplicated. That order is the one
  `--renumber-tracks` numbers in, so it has to be the order the user saw — and a
  path named twice must not become two operations on one file, which the planner
  refuses as a duplicate edit.

## Acceptance criteria

- [x] `tag show` on an mp3 and a FLAC prints the same field set
- [x] `tag show --json` round-trips through a JSON parser
- [x] `tag set --genre "Hip Hop" -r <album dir>` previews N files and commits one
      transaction
- [x] `mpdfm undo` after a bulk tag set restores every file byte-for-byte
- [x] `--dry-run` writes nothing
- [x] `--clear genre` removes the frame
- [x] `--renumber-tracks` on an album dir numbers by displayed order
- [x] setting a field on a read-only file fails preflight, and no other file in
      the batch is modified
- [x] non-TTY without `--yes` refuses

## Hand-verified against the real library

Read-only first, with the release binary against `~/Music`:

- `tag show` on a real scene mp3 prints the ten fields plus `TPUB` and
  `TXXX:LABELNO` as extras, and the audio line `mp3 4:25, 320 kbps, 44100 Hz, 2 ch`;
- `tag diff --genre "Hip Hop" --clear comment` on a 12-track album shows 24
  per-file changes and writes nothing;
- `tag diff --renumber-tracks` on the same album shows `track "9" → "9/12"` per
  file and one `TAG track <per file>  12 files` row.

Then for real, on a **copy** of that album in a scratch directory with its own
three roots (`docs/PLAN.md` §8 — the library itself is only ever read):
`tag set --genre … --album-artist … --clear comment --renumber-tracks -r` committed
one transaction over 12 files, `tag show` confirmed the new values and that the two
described `COMM` frames were still there as extras, and `mpdfm undo --yes` restored
all twelve **byte-for-byte** (`sha256sum -c`, 12 OK).

Two bugs came out of that run, both now fixed and covered:

- **`paths::contains` refused a root that does not exist yet.** It canonicalized
  `root` and failed closed when it could not, and MPDFM's data directory is
  created on first use and deliberately left uncanonicalized — so on a machine
  that has never committed anything, *every* preview that plans a backup was
  refused with "is outside the library". Both sides now go through the same
  longest-existing-ancestor resolution. This affected `Delete` too; nothing had
  exercised it yet because no command stages one.
- **The reader and the writer disagreed about what the comment is** — see task 16.
  Task 17's read-back check is what turned that into a refusal instead of a
  silent no-op.

## Files

`src/cli/tag.rs`, `tests/cli_tag.rs`

Also touched: `src/cli/mod.rs` (the three subcommands and their arguments),
`crates/core/src/paths.rs` (the containment fix above),
`crates/core/src/tags/read.rs` (the comment fix above),
`crates/core/src/journal/record.rs` and `ops/render.rs` (a transaction's summary
line now mentions retagged files, so `undo --list` does not say "nothing" about a
tag edit), and `tags/model.rs` / `tags/bulk.rs` (`Display` through
`Formatter::pad`, so a field table lines up — `write_str` silently ignores the
format width).

## Pitfalls

- Partial failure in a batch: decide up front whether it is all-or-nothing.
  Recommended: preflight every file, and refuse the whole batch if any file is
  unwritable, matching the transactional model elsewhere.

  **Decided: all-or-nothing**, and it falls out of the existing machinery rather
  than being bolted on. Every file becomes an `Operation::WriteTags`, the plan is
  validated as one, and a `Conflict` anywhere in it makes
  `Effects::is_committable` false — the same rule that refuses a move whose
  destination is occupied. One read-only track in a 400-file album means no file
  is touched. A file whose tags cannot even be *read* is refused earlier still,
  before a plan exists: a bulk view built from nine of ten files would be a view
  of a selection the user did not make.
