# 22 — Library browser view

- **Phase:** M3 · TUI
- **Depends on:** 05, 16, 20, 21
- **Status:** not started

## Goal

The main screen: navigate ~2 800 tracks across genre and album directories,
mark files, and see enough metadata to know what you are looking at — without
lag.

## Details

Layout: two panes. Left is the directory tree (or parent listing), right is the
contents of the selected directory. A third, narrow column on wide terminals
shows details of the focused entry (tags, bitrate, duration, and which playlists
reference it — the last is genuinely useful before a move).

```
┌ ~/Music ───────────┬ hiphop/MF DOOM - Mm..Food (2004) ──────────┬ Details ────┐
│   electronic       │ ● 01 Beef Rap.mp3          3:24  320k      │ Title  Beef │
│ ▸ hiphop           │ ● 02 Hoe Cakes.mp3         4:02  320k      │ Artist MF…  │
│   japanese         │   03 Potholderz.mp3        2:58  320k      │ Album  Mm…  │
│   lofi             │   folder.jpg                                │ Genre  —    │
│   pop              │   info.nfo                                  │ ⚠ in 2 pl. │
└────────────────────┴─────────────────────────────────────────────┴─────────────┘
 2 marked · 3 ops pending          MPD ● connected            ? help  : command
```

Requirements:

- **Virtualized rendering**: only the visible rows are laid out, and tags are
  read only for visible rows (task 16's lazy API) with results cached per path.
  Scrolling through a 400-file directory must not stall.
- Marks are a `HashSet<RelPath>` that survives navigation, so you can mark across
  directories and then stage one move. Show the count in the status bar.
- Visual mode (`v`) marks a contiguous range.
- Show non-audio files (dimmed) — the user must see that `folder.jpg` and
  `info.nfo` will travel with the album.
- Flag entries referenced by playlists, and flag directories containing such
  entries, so the consequence of a move is visible before staging it.
- Sort options: name (natural, so `2` sorts before `10`), track number, mtime,
  size. Remember the choice per session.
- Scan warnings (non-UTF-8 files) surface as a badge, not a silent omission.

## Acceptance criteria

- [ ] navigate into and out of directories with `l`/`h`/enter/backspace
- [ ] a 400-entry directory scrolls smoothly; measure frame time and record it
- [ ] tags are fetched only for visible rows (assert with a counter)
- [ ] marks persist across directory changes and are shown in the status bar
- [ ] `v` range-marks; `a` marks all in the current view; `A` clears
- [ ] non-audio files are listed and visually distinguished
- [ ] the details pane lists which playlists reference the focused track
- [ ] natural sort puts `02` before `10`
- [ ] non-ASCII filenames render correctly, including wide CJK characters from
      the `japanese/` and `chinese/` directories (column alignment must not break)
- [ ] an empty directory and a permission-denied directory both render sensibly

## Files

`src/tui/views/browser.rs`, `src/tui/widgets/{filelist.rs,details.rs}`

## Pitfalls

- East Asian wide characters and emoji break naive width math. Use
  `unicode-width` for column calculations, and test with the real
  `japanese/`, `chinese/` and `KREAM - So Hï` entries.
- Don't re-scan the library on every navigation; the `Library` model from task 05
  already has `by_dir`.
