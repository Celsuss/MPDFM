# 23 — Tag editor view

- **Phase:** M3 · TUI
- **Depends on:** 16, 17, 18, 21, 22
- **Status:** not started

## Goal

The form the user will spend most of their time in: edit one file's tags, or a
whole album's, with `<multiple>` handled honestly.

## Details

```
┌ Edit tags — 14 files selected ───────────────────────────────┐
│ Title        <multiple>        (per-file — use actions)      │
│ Artist       MF DOOM                                          │
│ Album artist MF DOOM                                          │
│ Album        Mm..Food                                         │
│ Year         2004                                             │
│ Track        <multiple>        (per-file)                      │
│ Disc         1/1                                              │
│ Genre      ▸ Hip Hop_                                         │
│ Comment      <multiple>                                       │
│                                                               │
│ Actions: [T] title from filename  [N] renumber  [C] clear     │
│ modified: genre                                               │
│ [w] stage  [W] stage & commit  [esc] cancel                   │
└───────────────────────────────────────────────────────────────┘
```

Requirements:

- Fields are a vertical list; `j`/`k` moves, `i`/enter edits, `esc` leaves the
  field. A simple single-line text input per field (history not needed).
- A field showing `<multiple>` is visually distinct and only becomes "modified"
  when the user actually types in it (task 18's rule). Modified fields are
  highlighted and listed at the bottom so it is obvious what will be written.
- `w` stages the edit into the pending plan (task 24); `W` stages and commits
  immediately. Never write on field exit.
- Per-file actions (`title from filename`, `renumber tracks`) show their own
  preview list before staging.
- Validation as you type: year must be a number or a date; track must be `n` or
  `n/total`. Show the error inline, refuse to stage until fixed.
- `esc` with unsaved modifications asks for confirmation.
- For a single file, also display read-only `AudioInfo` (duration, bitrate,
  sample rate, format) and the file's path.

## Acceptance criteria

- [ ] editing one file's genre and staging produces one `WriteTags` op
- [ ] a `<multiple>` field left alone produces no change (the critical test)
- [ ] typing into a `<multiple>` field marks it modified and writes to all files
- [ ] invalid year/track shows an inline error and blocks staging
- [ ] `esc` with modifications prompts; `esc` without modifications exits directly
- [ ] `W` commits and the change is visible in the browser immediately afterwards
- [ ] `undo` from the browser reverses it
- [ ] editing 200 selected files across two albums works and previews correctly
- [ ] a non-writable file in the selection is reported before staging, naming it
- [ ] UTF-8 input (accented characters, CJK) can be typed and is written correctly

## Files

`src/tui/views/tagedit.rs`, `src/tui/widgets/input.rs`

## Pitfalls

- The empty-string vs cleared-field distinction must be visible in the UI, or the
  user cannot tell what `w` will do.
- Keep the form's state separate from `TagSet` so cancelling is trivially correct.
