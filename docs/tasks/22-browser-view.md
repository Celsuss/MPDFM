# 22 — Library browser view

- **Phase:** M3 · TUI
- **Depends on:** 05, 16, 20, 21
- **Status:** done

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

- [x] navigate into and out of directories with `l`/`h`/enter/backspace
- [x] a 400-entry directory scrolls smoothly; measure frame time and record it
      — **130 µs per frame** released, 1.4 ms unoptimized
- [x] tags are fetched only for visible rows (assert with a counter)
- [x] marks persist across directory changes and are shown in the status bar
- [x] `v` range-marks; `a` marks all in the current view; `A` clears
- [x] non-audio files are listed and visually distinguished
- [x] the details pane lists which playlists reference the focused track
- [x] natural sort puts `02` before `10`
- [x] non-ASCII filenames render correctly, including wide CJK characters from
      the `japanese/` and `chinese/` directories (column alignment must not break)
- [x] an empty directory and a permission-denied directory both render sensibly

## Files

`src/tui/views/browser.rs`, `src/tui/widgets/{filelist.rs,details.rs}`

## Pitfalls

- East Asian wide characters and emoji break naive width math. Use
  `unicode-width` for column calculations, and test with the real
  `japanese/`, `chinese/` and `KREAM - So Hï` entries.
- Don't re-scan the library on every navigation; the `Library` model from task 05
  already has `by_dir`.

## How it is designed

Six decisions worth writing down, because each of them is a thing that could
reasonably have gone the other way.

**One cursor is derived and one is stored.** The tree pane's cursor *is* the
browser's current directory: the row it sits on is the directory whose contents
the listing shows, so the two cannot disagree and there is no "the tree says
`hiphop` and the listing says `jazz`" state to get into. The listing's cursor is
a stored index, because the row it points at has no other name. `Browser::dir` is
therefore the single piece of navigation state, and `tree_rows` is a pure
function of it plus the set of expanded nodes.

**A window is computed, never remembered.** `window(offset, cursor, height,
total)` takes the stored scroll offset as a *hint* and returns a range that is
guaranteed to contain the cursor. The hint is what makes scrolling sticky — the
view does not re-centre on every keypress — and the guarantee is what makes it
impossible to lose the cursor off the top or the bottom of a pane after a resize,
a change of sort, or a rescan that shortened the listing. Everything downstream
is given the range and nothing else: `FileList` is handed a slice as long as the
pane is tall and *cannot* touch a row that is scrolled off, so virtualization is
a property of the shape rather than of anybody's discipline.

**The listing is cached, and the reason is measured.** A frame asks for the
sorted listing five times — the widget, the scroll offset it records afterwards,
the details pane, the tag window. Re-deriving it each time cost **2 ms a frame**
on a 400-entry directory, which is most of the budget for a key that is held
down. `Browser::listing` holds it and every mutator that can change it ends in
`Browser::refresh`, so the one risk a cache carries is answered by there being a
single place that fills it. That plus an allocation-free `natural_cmp` took the
frame to 130 µs.

**Tags come from one open, not two, and the browser never does it.** The listing
shows a duration and a bitrate next to the name, which `tags::read_tags` does not
produce — task 16 notes that the cheap call is the one "a screenful of rows goes
through", and this task departs from that: reading the tags and then re-reading
for the properties would double the I/O for exactly the rows the user is looking
at. The measurement says it is affordable: a 35-row window of the real library's
biggest directory is **22 ms cold and 0.3 ms warm**, on a worker thread. On the
drawing thread 22 ms would be a visible stutter, which is why `Browser::wanted`
only *names* the paths and `work::read_tags` is what opens them. One batch is out
at a time and its answer is what asks for the next, so scrolling fast coalesces
into a few large reads instead of a thread per row.

**`track` order asks for the whole directory.** It is the one sort that needs data
the model does not have, and a sort computed from a window of itself would be a
lie. An album is a few dozen files, so choosing it reads all of them; until they
arrive the listing is in name order, which is what a half-read album looks like
anyway. A file with no track number sorts after the ones that have one rather
than being treated as track zero.

**`:set sort=` is the first setting that does anything.** Task 21 left `:set`
parsing and applying nothing, deliberately, because no task owned live settings.
`sort` is the natural first one: the task asks for it to be remembered *per
session*, which is exactly what a setting written to `config.toml` would get
wrong. Every other key still parses and still says that nothing applies it.

**Staging moved to task 24.** `stage_move`, `rename`, `stage_delete` and `:move`
pointed at this task; they now point at 24. Marking is this task's job and it is
done — `Browser::marks` hands out the marked paths in path order — but staging
needs somewhere to show what was staged and something to commit it with, and that
is the pending view. A plan nobody can see or commit is half a feature.

### The layout

Three panes, and the third one is a width rather than a flag so there is one
source of truth: the details pane is 26 cells wide at or above 90 columns and
zero below it, and `render_browser` draws it when it is non-zero. The tree takes
a quarter of the width clamped to 14–30. At the 60×15 floor that is 14 + listing,
which still fits a name.

The duration and bitrate columns are dropped on a narrow listing rather than
squeezed: a three-character name is not worth a bitrate.

### Where the width math lives

`src/tui/widgets/mod.rs`, in `fit`, `pad` and `pad_left`, and every string that
goes into a fixed-width column goes through one of them. The invariant is
`width(&pad(s, n)) == n` for *any* string and any width, and the one deliberate
limitation is that a two-cell character which would land half in and half out of
a column is dropped and the cell left blank — half a `ス` is not a character, and
a row one cell too long is a row that corrupts a border.

## How it is tested

The split follows tasks 20 and 21: what needs a real terminal, and what does not.

**Headless, in `src/tui/`.** Four files' worth.

- `widgets/mod.rs` proves the column arithmetic against the real names —
  `03 ノスタルジア.mp3` is 25 bytes, 13 characters and 19 cells, and
  `pad_is_exact_for_every_width_and_every_name` checks all three functions at
  every width from 0 to 30 on six names including a bare `ス`;
- `widgets/filelist.rs` renders into a `TestBackend` and asserts on **buffer
  cells, not on a reconstructed string**: `the_columns_line_up_whatever_the_name_is`
  checks that the bitrate column starts at cell 45 on an ASCII, a Latin-1 and a
  CJK row, and that the border is still at cell 49;
- `views/browser.rs` is the state: the window function against a stale offset at
  every cursor position, `natural_cmp`'s totality and antisymmetry over ten
  names, marks across directories, the visual range in both directions, the tag
  cache, and the two awkward directories;
- `app.rs` drives the whole thing through keys against a `TestBackend`, including
  the two criteria that are about the machinery rather than about a view:
  `only_the_visible_rows_have_their_tags_read` runs the **real worker thread**,
  waits for its answer on the channel and asserts the batch is exactly the
  visible rows, that scrolling to the end reads the other end and nothing in the
  middle, and that coming back reads nothing at all; and
  `a_four_hundred_entry_directory_draws_a_frame_in_well_under_a_millisecond`
  scrolls a row per frame over 400 frames and prints the mean.

Two harness subtleties found while writing those, both recorded in the tests:

- **the cells behind a wide character hold stale text.** `ratatui` writes a
  two-cell glyph into one `Cell` and leaves the next one alone — invisible on a
  real terminal, because the glyph is drawn over it, and `Buffer::diff` skips it.
  Reconstructing a row by concatenating every cell therefore reads text nobody
  can see, and it made an assertion about `03 ノスタルジア.mp3` fail on a frame
  that was perfectly correct. The `lines` helper now advances by each symbol's
  display width;
- **a sequence of keys has to be typed at a pace.** Fed from a file, the whole
  script is in the pty's input queue before the TUI has entered raw mode, and
  what the line discipline does with it then is not something a test should
  depend on — task 21 found the same thing with `enter`. `pty_typed` drives
  `script`'s stdin from a pipe with a third of a second between keys, and its
  feeder ends with a sleep that outlives the run so the pty never delivers EOF
  (a literal `ctrl-d`) while the loop is still reading.

**Through a pseudo-terminal, in `tests/tui_terminal.rs`.** One test,
`the_browser_walks_into_a_directory_and_reads_what_is_on_screen`, for the things
in-process tests cannot see: a worker thread opening real audio files while the
loop draws, a name with a `ï` in it surviving a real terminal, and a mark
reaching the status bar.

## Verifying it by hand

Run on 2026-10-07 against the release binary and the author's real library
(3 132 files, 318 directories, 17 playlists, 231 references), read-only, driven
through a pseudo-terminal and replayed into a grid so the column positions could
be checked rather than eyeballed.

**Startup.** 3 132 files in 318 directories in **11–13 ms** on the worker, 0 scan
warnings, 0 playlist warnings. The root listing shows all 22 genre directories
with `⚠` on the twelve a playlist points into and no marker on `nostalgia` and
`podcasts`, which have no subdirectories.

**Wide characters, on the real names.** `jazz/ghibli-jazz`
(`All_That_Jazz-Mononoke_Hime_⧸_もののけ姫.mp3`, `Ghibli_jazz_06-人生のメリーゴーランド…`),
`chinese/Kimberly_Chen` (`Kimberley Chen 陳芳語 - After the Rain｜例假日 …`, with a
fullwidth `｜`), and `lofi/170 Tracks … Beats⭐/…` — an emoji in both the
directory name and the pane title. In all three the six pane borders are in the
**same column on every one of the 35 body rows**, and every duration starts at
column 103. That is the task's first pitfall, checked on the files it names.

**Only the visible rows are read.** Entering the library's biggest directory —
170 tracks — read **35 files**, the height of the pane; `G` to the bottom read
**35 more**. 70 of 170, and none of the other 2 962 files in the library. Cold,
each batch took 22 ms and 18 ms on the worker; warm, a 10-file window took
**289 µs**.

**Marking and sorting.** `v j j v` in `jazz/ghibli-jazz` left three `●` glyphs on
the rows and `3 marked` on the status bar; `:set sort=track` reported `sorting by
track` and the bar changed to `sort track`. Those files carry no track numbers,
so the order did not change — which is the documented fallback, and visible in
the details pane's `Track  —`.

**Still free.** `just verify-tui`: **2 clock ticks over 30 s** (0.07% of one
core), 4 threads, **7.1 MB** resident — against 7.7 MB at task 21. `SIGTERM`
exit 0, terminal modes unchanged.

**One cosmetic fix came out of the real run**: `Bitrate` is seven characters and
the details pane's label column was seven cells, so it read `Bitrate141 kbps`.
The column is eight now.

**What could not be verified by hand:** the permission-denied directory. There
is no unreadable directory in the real library, and creating one would be a write
to it. It is covered in `views/browser.rs` against a fixture whose directory is
`chmod 000`, which also asserts the sensible outcome when the test happens to run
as root.
