# 18 — Bulk tag semantics

- **Phase:** M2 · Tag editing
- **Depends on:** 16, 17
- **Status:** not started

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

## Acceptance criteria

- [ ] 3 files with the same album and different titles → album `Same`, title `Multiple`
- [ ] leaving a `Multiple` field untouched produces no `TagDelta` entry for it
      (property test over random selections — the critical one)
- [ ] editing a `Multiple` field writes the new value to every selected file
- [ ] explicit clear removes the field from all selected files
- [ ] `renumber tracks` assigns 1..n in the displayed order and sets the total
- [ ] `title from filename` strips a leading track number and extension
      (`01.Smokin' On.mp3` → `Smokin' On`) — tested against real names from this
      library, including `05_lost_frequencies_ft._jake_reese_-_….mp3`
- [ ] a selection mixing mp3 and FLAC works and writes format-appropriate tags
- [ ] the generated plan previews as one `TAG` line per changed field with a
      file count

## Files

`crates/core/src/tags/bulk.rs`

## Pitfalls

- `title from filename` needs to be conservative and previewable; scene naming
  varies wildly (`01.Title`, `05_artist_-_title`, `9 Title (feat. X)`). Show the
  results before writing and let the user cancel.
