# 25 — Search and filter

- **Phase:** M3 · TUI
- **Depends on:** 05, 16, 22
- **Status:** not started

## Goal

Find things in a 2 800-track library without scrolling: incremental search
within a view, and a filter that narrows the whole library.

## Details

- `/` — incremental search in the current listing. Matches as you type, `n`/`N`
  cycle matches, `enter` keeps the position, `esc` restores it. Case-insensitive
  unless the pattern contains an uppercase letter (smart case, like vim).
- `f` — filter mode, which narrows the listing to matches and stays active until
  `esc`. Shown in the status bar so an active filter is never invisible.
- Matching targets: filename, and when tags are loaded, `artist`, `album`,
  `title`, `genre`. A `field:value` syntax for precision:
  `artist:doom`, `genre:jazz`, `album:"mm..food"`, `ext:flac`, `missing:genre`.
- A library-wide search (`F` or `:find <query>`) walks all entries, reading tags
  on demand with a progress indicator, and presents results as a flat virtual
  directory that can be marked and operated on like any other listing —
  "select every file with no genre, set genre" is the workflow that makes this
  worth building.
- Substring matching by default. A fuzzy matcher is optional; if added, keep
  substring as the fallback and make ranking stable.

## Acceptance criteria

- [ ] `/` matches incrementally, `n`/`N` cycle, `esc` restores the original position
- [ ] smart case: `doom` matches `MF DOOM`, `DOOM` does not match `doom`
- [ ] `f` filter narrows the listing and shows an indicator in the status bar
- [ ] `artist:doom`, `genre:jazz`, `ext:flac` and `missing:genre` all work
- [ ] quoted values with spaces parse (`album:"mm..food"`)
- [ ] library-wide search over ~2 800 files completes with progress and does not
      block the UI
- [ ] results from a library-wide search can be marked and staged into a plan
- [ ] non-ASCII queries match non-ASCII names (test with `So Hï`)
- [ ] an empty result set renders a clear "no matches" state

## Files

`src/tui/views/search.rs`, `crates/core/src/query.rs`

## Pitfalls

- `missing:genre` requires tags, so it forces a full tag read. Do it on a worker
  with progress and cache the results for the session.
- Put the query parser in core, not the TUI, so `:find` and a future CLI
  `mpdfm find` share it.
