# 35 — Managing cover art in the TUI

- **Phase:** M5 · Extras
- **Depends on:** 24, 30
- **Status:** done

## Goal

Manage cover art from the TUI, not only look at it. Most album folders hold one
`cover.jpg` / `cover.jpeg` / `cover.png`, but folders of singles hold several,
one per track, and `mpdfm cover set` can only embed one image into a whole
directory.

## What was built

- **`art::plan`** in core: `embed`, `remove` and `extract` take a list of
  tracks rather than a directory, and the CLI's `cover set | remove | extract` now
  call them, with unchanged behaviour. `pair` matches tracks to loose images by
  name; `canonical_name` is what `C` renames an image to.
- **The cover view** (`src/tui/views/cover.rs`, `Mode::Cover`, `[cover]` in
  `keys.toml`): `C` or `:cover` on a folder. Images on top, tracks below with
  their embedded cover now and their **draft**. `s` / `x` / `n` / `h` / `l` edit
  drafts; `w` stages them as `WriteTags` with a `CoverEdit`, one per track,
  sorted by track. `e`, `C`, `d`, `D` stage `ExtractArt`, `MoveFile` and
  `Delete` at once. `z` downscales to 1000 px through `art::downscale` /
  `art::stage`.
- The survey runs on a worker (`work::read_covers`), and again after every rescan,
  so a commit's result shows in the view.
- `:cover set <image> | remove | extract [name]` stage the same plans without the
  view.
- The details pane shows a folder's best loose image and `· N images`.

## Decisions

**With nothing marked, a verb means every track in the folder.** This is the
CLI's rule, and the common case is an album. Singles are handled with marks or
with `n`.

**Drafts, then `w`,** like the tag editor, because pairing is a guess: `n` fills
in drafts, the user reads them, fixes the leftovers with `h` / `l`, and only then
stages. The file operations have nothing to guess, so they stage straight away
like the browser's verbs.

**Pairing has three tiers, and the first that finds anything decides:** the same
name key (lower case, punctuation folded, a leading track number dropped, file
name or title tag); a whole-word suffix (`Artist - Song A` ↔ `Song A.jpg`); and
a compact key with bracketed parts dropped and spaces removed
(`Artist_-_Song_Title_(AMV)_[id]` ↔ `songtitle.jpg`). Two candidates in the
deciding tier are refused as ambiguous, never guessed. Only JPEG and PNG are
candidates.

**An image a staged embed reads cannot be renamed or deleted,** and an image
staged to go cannot be embedded. The edit reads its source at commit time, so
either order would fail partway through a transaction.

**`C` keeps a fitting extension** (`front.jpeg` → `cover.jpeg`) and refuses to
take a name that exists unless that file is staged for deletion, which is how
`D` then `C` works.

## Measured against the real library (read-only)

238 folders hold audio; 22 of them have two or more embeddable images. Most of
those are albums with back covers and CD scans, where nothing should pair, and
nothing did. The two singles folders paired completely, 6 of 6 tracks, all
correct, none ambiguous. Names there are downloader-style:
`Artist_-_Title_(AMV)_[videoid].mp3` beside `title.jpg`. The first two tiers
matched 3 of them; the compact tier was added for the rest.

Nothing was written to the library.

## Files

`crates/core/src/art/plan.rs`, `src/tui/views/cover.rs`, and the wiring in
`src/tui/{app,action,keys,command,msg,work}.rs` and `widgets/{image,help}.rs`;
`src/cli/cover.rs` now uses `art::plan`. Docs: `book/src/tui/covers.md`,
`docs/keys.example.toml`.
