# 18 — Bulk tag semantics

- **Phase:** M2 · Tag editing
- **Depends on:** 16, 17
- **Status:** done

## Goal

Edit a field across many files at once with honest handling of fields that
currently differ (decision D3 / the `<multiple>` model).

## Details

```rust
enum FieldValue { Absent, Same(String), Multiple }     // across the selection

struct BulkView { fields: BTreeMap<Field, FieldValue>, files: Vec<RelPath> }

fn view(tags: &[(RelPath, TagSet)]) -> BulkView;
fn delta_for(view_edits: &BTreeMap<Field, Edit>) -> Vec<(RelPath, TagDelta)>;
```

Rules:

- A field identical across all selected files shows its value; editing it writes
  that value to all.
- A field that differs shows `<multiple>` and is **not written** unless the user
  actively changes it. An untouched `<multiple>` field must never be flattened —
  that is the classic bulk-editor data-loss bug.
- A field absent everywhere shows empty; absent in some shows `<multiple>`.
- Per-file fields (`track`, `title`) are editable in bulk only via explicit
  operations, not by typing one value: offer `renumber tracks` (by current sort
  order) and `title from filename` as named actions.
- Clearing a `<multiple>` field requires an explicit "clear" action, distinct
  from leaving it alone.
- Useful bulk actions worth having, each generating a normal `TagDelta` set:
  `album artist = artist`, `genre = <value>`, `year = <value>`,
  `album = <value>`, `renumber tracks`, `strip comment`, `trim whitespace`.

Output is a `Vec<(RelPath, TagDelta)>` folded into the `Plan` (task 10), so bulk
edits get the same preview, commit and undo as everything else.

## How the `<multiple>` rule is actually enforced

Not by special-casing `<multiple>`. The bug this task is about —  select fourteen
tracks, change the genre, and find they all have the first one's title — comes from
one mistake: deriving what to write from what is *shown*. So nothing does.

`BulkView` is read-only, and `BulkView::delta_for` is **given** the fields the user
changed. A field that is not in that map produces no `TagDelta` entry for any file,
whatever it looked like on screen; a `<multiple>` field simply never arrives in the
edits unless somebody typed in it. There is no code path that could flatten one,
which is a stronger statement than "the code remembers not to".

`delta_for` also leaves out the files an edit would not change: a `--genre` across
an album that is already tagged right previews as nothing to do rather than as
fourteen rewrites, and a file whose delta comes out empty is dropped from the
result entirely. Comparison is against what the value will *read back* as
(`write::canonical`), so `--track 05/12` on a file that says `5/12` is correctly
nothing to do.

`delta_for` applies whatever it is given, including a per-file field: it is the
primitive, and refusing belongs to the front-end that knows whether a human is
looking. `BulkView::per_file_in` is that check, shared by the CLI and the TUI so
they refuse for the same reason and with the same list — and it allows a typed
`title` for a selection of **one**, where typing a title is exactly what the user
means.

## `title from filename`

Three steps, and deliberately nothing else:

1. the extension;
2. a leading track number — one to three digits, optionally bracketed — followed
   by at least one `.`, `-`, `_` or space;
3. `_` becomes a space **only if the rest of the name has no spaces at all**, which
   is the scene convention and not something a human-typed name does.

Every step is skipped if it would leave nothing, so `630.mp3` keeps its name. Four
digits is a year and not a track number (`1984 Title` is left alone), an opened
bracket must be closed (`(Remember the days of the) Old school yard` is left
alone), and nothing is stripped without a separator (`01Title` is left alone).

Tested against the real library's actual names: `01.Smokin' On.mp3`,
`10. Hands.mp3`, `[24] If you want to sing out, sing out - Cat Stevens.flac`,
`04_lost_frequencies_ft._sandro_cavazza_-_beautiful_life_(deluxe_mix).mp3`,
`03 ノスタルジア.mp3`, `04.630.mp3`.

## Acceptance criteria

- [x] 3 files with the same album and different titles → album `Same`, title `Multiple`
- [x] leaving a `Multiple` field untouched produces no `TagDelta` entry for it
      (property test over random selections — the critical one)
- [x] editing a `Multiple` field writes the new value to every selected file
- [x] explicit clear removes the field from all selected files
- [x] `renumber tracks` assigns 1..n in the displayed order and sets the total
- [x] `title from filename` strips a leading track number and extension
      (`01.Smokin' On.mp3` → `Smokin' On`) — tested against real names from this
      library, including `05_lost_frequencies_ft._jake_reese_-_….mp3`
- [x] a selection mixing mp3 and FLAC works and writes format-appropriate tags
- [x] the generated plan previews as one `TAG` line per changed field with a
      file count

The property test is `leaving_a_multiple_field_alone_writes_nothing`: 500 rounds
of a 1-to-6-file selection with random values in all ten fields, editing one field
per round and asserting that every delta holds that field and nothing else. The
generator is a three-line xorshift with a fixed seed rather than a dependency — a
test that needs a seed printed to be reproduced is a test nobody reproduces.

## Files

`crates/core/src/tags/bulk.rs`, `crates/core/tests/tags_bulk.rs`

Also touched: `tags/write.rs` (`canonical`, so "would this edit change anything?"
is answered by the same code that checks a write took effect), and
`tags/model.rs` (`Field::is_per_file`).

## Pitfalls

- `title from filename` needs to be conservative and previewable; scene naming
  varies wildly (`01.Title`, `05_artist_-_title`, `9 Title (feat. X)`). Show the
  results before writing and let the user cancel.
