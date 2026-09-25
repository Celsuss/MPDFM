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
| 03 | [Test fixture library](tasks/03-test-fixtures.md) | 01, 02 | not started |
| 04 | [Config and mpd.conf discovery](tasks/04-config-discovery.md) | 01, 02 | not started |
| 05 | [Library scanner](tasks/05-library-scanner.md) | 02, 03 | not started |
| 06 | [Byte-preserving m3u parser](tasks/06-playlist-parser.md) | 02, 03 | not started |
| 07 | [Playlist index](tasks/07-playlist-index.md) | 06 | not started |
| 08 | [Filesystem move executor](tasks/08-move-executor.md) | 02, 05 | not started |
| 09 | [Playlist rewriting](tasks/09-playlist-rewrite.md) | 06, 07, 08 | not started |
| 10 | [Plan, validation and preview](tasks/10-plan-and-preview.md) | 05, 07, 08, 09 | not started |
| 11 | [Two-phase commit and journal](tasks/11-journal-and-commit.md) | 08, 09, 10 | not started |
| 12 | [Undo and recover](tasks/12-undo-and-recover.md) | 11 | not started |
| 13 | [Minimal MPD client](tasks/13-mpd-client.md) | 04 | not started |
| 14 | [MPD saved-queue rewriting](tasks/14-mpd-state-queue.md) | 06, 11, 13 | not started |
| 15 | [CLI: scan, doctor, move, undo](tasks/15-cli-move-and-doctor.md) | 04, 05, 07, 10–14 | not started |

**M1 is done when:** `mpdfm move` on a copy of the real library relocates an
album, all 231 playlist references still resolve, the saved queue is consistent,
and `mpdfm undo` returns everything byte-identical — with tests proving it,
including crash injection at every commit phase.

## M2 — Tag editing

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 16 | [Tag reading](tasks/16-tag-read.md) | 05 | not started |
| 17 | [Tag writing](tasks/17-tag-write.md) | 11, 16 | not started |
| 18 | [Bulk tag semantics](tasks/18-tag-bulk.md) | 16, 17 | not started |
| 19 | [Tag CLI](tasks/19-cli-tags.md) | 15–18 | not started |

**M2 is done when:** mp3 and FLAC tags can be read and written single and in
bulk, nothing else in the file changes (embedded art and ReplayGain survive), and
a bad bulk edit is fully undoable.

## M3 — TUI

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 20 | [TUI shell and event loop](tasks/20-tui-shell.md) | 01, 04 | not started |
| 21 | [Configurable vim keymap](tasks/21-keymap.md) | 20 | not started |
| 22 | [Library browser view](tasks/22-browser-view.md) | 05, 16, 20, 21 | not started |
| 23 | [Tag editor view](tasks/23-tagedit-view.md) | 16–18, 21, 22 | not started |
| 24 | [Pending ops view](tasks/24-pending-view.md) | 10–12, 20, 21 | not started |
| 25 | [Search and filter](tasks/25-search-and-filter.md) | 05, 16, 22 | not started |
| 26 | [Status bar, help, errors](tasks/26-tui-chrome.md) | 13, 20, 21 | not started |

**M3 is done when:** the whole M1+M2 feature set is usable from the TUI, the
pending view shows the same preview the CLI does, and the terminal is always
restored — including on panic.

## M4 — Organize by template

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 27 | [Template engine](tasks/27-template-engine.md) | 02, 05, 16 | not started |
| 28 | [`mpdfm organize`](tasks/28-organize-command.md) | 10, 15, 24, 27 | not started |

## M5 — Extras

| # | Task | Depends on | Status |
| --- | --- | --- | --- |
| 29 | [Library doctor](tasks/29-doctor.md) | 07, 15, 16 | not started |
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
6. **Session 7:** 13, 14, 15 — MPD client, saved queue, CLI. **M1 complete.**

## Open questions to revisit

- Chained moves (`a → b`, `b → c`) in one plan: order them or reject them?
  Decide in task 10 and record the choice there.
- FLAC multi-valued fields: `Vec<String>` throughout, or join-and-remember?
  Decide in task 16; it affects the tag editor UI.
- How much of `--deep` duplicate detection is worth it on a 2 800-file library
  (task 29) — measure before building the hash pass.
- Whether to add a `mpdfm find` CLI command mirroring the TUI query parser
  (task 25) — cheap once the parser is in core.
