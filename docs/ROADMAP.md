# MPDFM — Roadmap

Status board and task index. Design rationale lives in `PLAN.md`; each task's
full detail and acceptance criteria live in `docs/tasks/NN-*.md`.

Status values: `not started` · `in progress` · `done` · `blocked`

---

## M1 — Trustworthy move engine

The part that can destroy data, built and tested before any UI exists. Ends with
a CLI that can scan, diagnose, move safely and undo.

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 01 | [Project scaffold](tasks/01-project-scaffold.md) | — | done |
| 02 | [`RelPath` and normalization](tasks/02-relpath-and-normalization.md) | 01 | done |
| 03 | [Test fixture library](tasks/03-test-fixtures.md) | 01, 02 | done |
| 04 | [Config and mpd.conf discovery](tasks/04-config-discovery.md) | 01, 02 | done |
| 05 | [Library scanner](tasks/05-library-scanner.md) | 02, 03 | done |
| 06 | [Byte-preserving m3u parser](tasks/06-playlist-parser.md) | 02, 03 | done |
| 07 | [Playlist index](tasks/07-playlist-index.md) | 06 | done |
| 08 | [Filesystem move executor](tasks/08-move-executor.md) | 02, 05 | done |
| 09 | [Playlist rewriting](tasks/09-playlist-rewrite.md) | 06, 07, 08 | done |
| 10 | [Plan, validation and preview](tasks/10-plan-and-preview.md) | 05, 07, 08, 09 | done |
| 11 | [Two-phase commit and journal](tasks/11-journal-and-commit.md) | 08, 09, 10 | done |
| 12 | [Undo and recover](tasks/12-undo-and-recover.md) | 11 | done |
| 13 | [Minimal MPD client](tasks/13-mpd-client.md) | 04 | done |
| 14 | [MPD saved-queue rewriting](tasks/14-mpd-state-queue.md) | 06, 11, 13 | done |
| 15 | [CLI: scan, doctor, move, undo](tasks/15-cli-move-and-doctor.md) | 04, 05, 07, 10–14 | done |

**M1 is done when:** `mpdfm move` on a copy of the real library relocates an
album, all 231 playlist references still resolve, the saved queue is consistent,
and `mpdfm undo` returns everything byte-identical — with tests proving it,
including crash injection at every commit phase.

## M2 — Tag editing

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 16 | [Tag reading](tasks/16-tag-read.md) | 05 | done |
| 17 | [Tag writing](tasks/17-tag-write.md) | 11, 16 | done |
| 18 | [Bulk tag semantics](tasks/18-tag-bulk.md) | 16, 17 | done |
| 19 | [Tag CLI](tasks/19-cli-tags.md) | 15–18 | done |

**M2 is done when:** mp3 and FLAC tags can be read and written single and in
bulk, nothing else in the file changes (embedded art and ReplayGain survive), and
a bad bulk edit is fully undoable. **Done.** Verified against every one of the
2 808 real audio files, on copies: 2 792 written with only the named field changed
and the audio stream bit-identical, 16 refused in preflight as damaged, and 25
losing one malformed frame `lofty` will not re-emit. Numbers in task 17.

## M3 — TUI

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 20 | [TUI shell and event loop](tasks/20-tui-shell.md) | 01, 04 | done |
| 21 | [Configurable vim keymap](tasks/21-keymap.md) | 20 | done |
| 22 | [Library browser view](tasks/22-browser-view.md) | 05, 16, 20, 21 | done |
| 23 | [Tag editor view](tasks/23-tagedit-view.md) | 16–18, 21, 22 | done |
| 24 | [Pending ops view](tasks/24-pending-view.md) | 10–12, 20, 21 | done |
| 25 | [Search and filter](tasks/25-search-and-filter.md) | 05, 16, 22 | done |
| 26 | [Status bar, help, errors](tasks/26-tui-chrome.md) | 13, 20, 21 | done |

**M3 is done when:** the whole M1+M2 feature set is usable from the TUI, the
pending view shows the same preview the CLI does, and the terminal is always
restored — including on panic. The last of those is **done** as of task 20, and
tested three ways: a guard, a panic hook, and `SIGTERM` turned into an ordinary
message. Measured on the real library: a 3 132-file startup scan in 11 ms on a
worker thread, and under 0.1% of one core when the session is idle. Task 22 adds
the browser, which walks those 318 directories at 130 µs a frame and reads tags
for the rows on screen and no others — 35 files for a 170-track directory. Task 23
adds the tag editor, measured on a copy of a real 70-track album: the selection is
read in 13 ms warm, and `W` stages and commits all 70 files in 210 ms, with `u`
putting every byte back in 100 ms. Task 24 adds the pending view, which is where
"the same preview the CLI does" stops being a promise: it draws
`Effects::lines`, `mpdfm move --dry-run` prints `Effects::render`, and the two are
one function — asserted as a string comparison at four widths. A 400-operation
plan with its 800-line playlist diff unfolded scrolls at 530 µs a frame. Task 25
adds `/`, `f`, `F` and `:find` over one query grammar in core
(`crates/core/src/query.rs`), so the TUI and a future `mpdfm find` cannot drift:
`ext:flac` over the real 3 132-file library opens no files at all, and
`missing:genre` — which has to open all 2 809 audio files — finds its 629 answers
in 334 ms on a worker, with the slowest frame drawn during the walk at 192 µs. Its
hits are a flat listing the existing mark, stage and tag-edit keys work on
unchanged, which is what makes "select every file with no genre, set genre" two
keypresses.
Task 26 closes M3 with the chrome: a status bar whose elision order is a
declared list (and tested at every width from 60 to 200), help generated from
the live keymap for every mode, a message line that wraps rather than truncates,
`:messages`, an error panel with the path and the next step, a cancellable scan,
and a first-run screen naming every place `music_dir` could have come from. The
MPD indicator goes offline within one tick of the daemon stopping, against a real
socket in a test, and a poll that never answers turns it off after two seconds
without a second thread joining the first.

## M4 — Organize by template

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 27 | [Template engine](tasks/27-template-engine.md) | 02, 05, 16 | done |
| 28 | [`mpdfm organize`](tasks/28-organize-command.md) | 10, 15, 24, 27 | done |

## M5 — Extras

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 29 | [Library doctor](tasks/29-doctor.md) | 07, 15, 16 | done |
| 30 | [Cover art](tasks/30-cover-art.md) | 16, 17, 22 | not started |

## M6 — Polish

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 31 | [Documentation](tasks/31-docs-and-readme.md) | 15, 19, 26, 28 | not started |
| 32 | [Packaging and release](tasks/32-packaging-and-release.md) | 31 | not started |

---

## Dependency graph (M1)

```
01 scaffold
 ├── 02 relpath ──┬── 03 fixtures ──┬── 05 scanner ──┐
 │                │                 └── 06 parser ── 07 index ──┐
 │                └── 04 config ── 13 mpd client ────┐          │
 │                                                    │          │
 └── 08 move executor ◀── 02, 05                      │          │
      └── 09 playlist rewrite ◀── 06, 07, 08          │          │
           └── 10 plan + preview ◀── 05, 07, 08, 09 ◀─┘          │
                └── 11 journal + commit                          │
                     ├── 12 undo + recover                       │
                     └── 14 saved queue ◀── 06, 13               │
                          └── 15 CLI ◀── all of the above ◀──────┘
```

## Suggested session ordering

Tasks are sized so that a session can finish two or three of the small ones, or
one of the large ones (08, 10, 11, 22, 27).

1. **Session 2:** 01, 02, 03 — scaffold, path type, fixtures. Ends with a
   compiling project and a fixture library the rest of the work leans on.
2. **Session 3:** 04, 05, 06 — config, scanner, playlist parser.
3. **Session 4:** 07, 08 — index and the move executor.
4. **Session 5:** 09, 10 — playlist rewriting and the preview.
5. **Session 6:** 11, 12 — commit, journal, undo, with crash-injection tests.
6. **Session 7:** 13, 14, 15 — MPD client, saved queue, CLI. **M1 complete:
   `mpdfm scan`, `doctor`, `move --dry-run`/`--merge`/`--verify`, `undo`,
   `undo --list` and `recover` all work, verified by hand against the real
   library (read-only) and against a real copy (writing).**

## Open questions to revisit

- ~~Chained moves (`a → b`, `b → c`) in one plan: order them or reject them?~~
  **Decided in task 10: ordered.** They are topologically sorted; a ring is a
  `Conflict::Cycle`; every operation's source must exist in the library as it is
  now, which removes the two-hop reading. Rationale in `ops::plan`'s module docs.
- ~~Redo: a command of its own, or undo-of-an-undo?~~ **Decided in task 12:
  undo-of-an-undo.** Every record carries a `direction`, an undo writes one of
  its own that says `reverse`, and undoing *that* executes its steps forward
  again. The direction flips every time, so `mpdfm undo` twice in a row is a
  redo and there is no second command that can disagree with the first.
- ~~Journal flush: after every step, or in small batches?~~ **Decided in task 11:
  after every step, appended to a second file.** Rewriting the whole record each
  time is quadratic (115 s extrapolated for the 4 500 steps a real whole-library
  reorganization produces); one appended line per step is flat and keeps the
  invariant batching would have broken. Measurements in task 11's decisions.
- ~~Is a scan cache needed to keep the TUI responsive (`PLAN.md` §7,
  `~/.cache/mpdfm/scan.json`, "only if scanning proves slow")?~~ **No.** Measured
  in task 15 with the release binary: the real library's 3 132 files across 27 GB
  scan in **10 ms**. Task 20 can rescan on a keystroke and the cache should stay
  unbuilt. Numbers in task 15's hand-verification section.
- ~~FLAC multi-valued fields: `Vec<String>` throughout, or join-and-remember?~~
  **Decided in task 16: an ordered list in every text field** (`tags::Values`).
  Join-and-remember needs the same two pieces of information and then has to
  guess which semicolons were separators on the way back out. The tag editor still
  shows one row per field, through `Values::joined`; 16 of the 2 808 real files
  are multi-valued.
- Whether `doctor` should report the 25 files holding frames no ID3v2 writer will
  emit — an invalid `TDRC`, a `WXXX` with no description, a v2.4-only frame in a
  v2.3 tag. Task 17 measured them and leaves them alone; naming them is task 29's
  business, not the writer's.
- ~~How much of `--deep` duplicate detection is worth it on a 2 800-file library
  (task 29) — measure before building the hash pass.~~ **Cheap, because size
  goes first.** Only 6 of the real library's 2 809 audio files share a size with
  another, so `--deep` hashes 6 files and adds ~30 ms; none of them are
  identical. It stays opt-in anyway, because a library of rips from one source
  can share sizes far more often. Numbers in task 29.
- Whether to add a `mpdfm find` CLI command mirroring the TUI query parser
  (task 25) — cheap once the parser is in core.
