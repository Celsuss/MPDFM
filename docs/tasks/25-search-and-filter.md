# 25 — Search and filter

- **Phase:** M3 · TUI
- **Depends on:** 05, 16, 22
- **Status:** done

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

- [x] `/` matches incrementally, `n`/`N` cycle, `esc` restores the original position
- [x] smart case: `doom` matches `MF DOOM`, `DOOM` does not match `doom`
- [x] `f` filter narrows the listing and shows an indicator in the status bar
- [x] `artist:doom`, `genre:jazz`, `ext:flac` and `missing:genre` all work
- [x] quoted values with spaces parse (`album:"mm..food"`)
- [x] library-wide search over ~2 800 files completes with progress and does not
      block the UI
- [x] results from a library-wide search can be marked and staged into a plan
- [x] non-ASCII queries match non-ASCII names (test with `So Hï`)
- [x] an empty result set renders a clear "no matches" state

## Files

`src/tui/views/search.rs`, `crates/core/src/query.rs`

Also touched: `crates/core/tests/query.rs` (the walk, against a real library on
disk), `src/tui/views/browser.rs` (the filter, the flat result set, and the one
definition of "this row matches"), `src/tui/app.rs` (the line, the three keys,
`n`/`N`, the worker and its progress, the status bar), `src/tui/action.rs`
(`find_library`), `src/tui/keys.rs` (`F`), `src/tui/command.rs` (`:find`),
`src/tui/msg.rs` and `src/tui/work.rs` (the progress message, the answer, the
cancel flag), `Cargo.toml` and `crates/core/Cargo.toml`
(`unicode-normalization` — see below), `docs/keys.example.toml`.

## Pitfalls

- `missing:genre` requires tags, so it forces a full tag read. Do it on a worker
  with progress and cache the results for the session.
- Put the query parser in core, not the TUI, so `:find` and a future CLI
  `mpdfm find` share it.

## As built

Two frames from `app.rs`'s own tests, at 100 columns. `F ext:mp3`, then `a` to
mark the lot, then `/doom` still being typed:

```text
MPDFM  /tmp/mpdfm-fixture-4IBR1x/music
┌ music ────────────────┐┌ find: ext:mp3 ────────────────────────────────┐┌ details ───────────────┐
│  ▾ /                  ││●   electronic/KREAM - So Hï/01 So…     ·     ·││01 Beef Rap.mp3         │
│    ▸ electronic       ││●   electronic/KREAM - So Hï/02 Tä…     ·     ·││                        │
│    ▸ hiphop           ││●   electronic/KREAM - So Hï/03 ノ…     ·     ·││reading…                │
│    ▸ jazz             ││●   hiphop/MF DOOM - Mm..Food/01 B…     ·     ·││                        │
│                       ││●   hiphop/MF DOOM - Mm..Food/02 H…     ·     ·││Kind    mp3             │
│                       ││●   hiphop/MF DOOM - Mm..Food/03 P…     ·     ·││Size    1 kB            │
│                       ││●   hiphop/mf doom - operation doo…     ·     ·││                        │
│                       ││                                               ││in no playlist          │
└───────────────────────┘└ 4/7 ──────────────────────────────────────────┘└────────────────────────┘
7 marked · 0 pending · sort name · focus files · find `ext:mp3` (7) · ○ mpd ?
/doom · 4 matches
```

And `f mp3` inside an album, with the cover art narrowed away:

```text
┌ music ────────────────┐┌ hiphop/MF DOOM - Mm..Food ────────────────────┐┌ details ───────────────┐
│  ▾ /                  ││    01 Beef Rap.mp3                     ·     ·││01 Beef Rap.mp3         │
│    ▸ electronic       ││    02 Hoe Cakes.mp3                    ·     ·││                        │
│    ▾ hiphop           ││    03 Potholderz.mp3                   ·     ·││reading…                │
│        MF DOOM - Mm..…││                                               ││Kind    mp3             │
│        mf doom - oper…││                                               ││Size    1 kB            │
│    ▸ jazz             ││                                               ││                        │
└───────────────────────┘└ 1/3 ──────────────────────────────────────────┘└────────────────────────┘
0 marked · 0 pending · sort name · focus files · filter `mp3` · ○ mpd ?
```

## One grammar, in core

`crates/core/src/query.rs` is the whole language: a `Query` is a list of `Term`s,
**all of which must match**, and a `Subject` is the thing being matched — a name,
maybe a path, maybe tags. There is no `or` and no negation, because the thing
this exists for is narrowing.

`/`, `f`, `F`, `:find` and a future `mpdfm find` all go through `query::parse`,
which is the task's second pitfall answered by construction rather than by
discipline. Nothing in `src/tui` parses a pattern.

Two decisions inside it are worth naming.

**A colon that is not a key is text.** `AC:DC` searches for `AC:DC` rather than
failing, because a file name is allowed to contain a colon and a search box that
refused one would be wrong about the library. A quoted token is text whatever is
in it, so `"artist:doom"` looks for that string.

**Smart case is per term.** `doom Beef` ignores case for the first word and not
for the second, which is what the user asked for by typing it that way. Uppercase
is `char::is_uppercase` and not an ASCII test, so `Ä` makes a pattern
case-sensitive exactly as `A` does.

## `So Hï`, and a dependency

The criterion names `So Hï`, and that file in the real library is

```text
4b 52 45 41 4d 20 2d 20 53 6f 20 48 69 cc 88 …    KREAM - So Hi<U+0308> […].mp3
```

— an `i` followed by a combining diaeresis, where a keyboard produces the single
code point `ï` (`c3 af`). A substring match over bytes cannot see through that,
so the search found **nothing**, which is exactly the class of bug this project
exists to not have.

So `Pattern` folds both the needle and the haystack to NFC, which is what
`unicode-normalization` is in the tree for. It is reached only for a **non-ASCII
pattern**: an ASCII one with a capital in it is still a plain `contains` with
nothing allocated, which keeps a filter that is re-evaluated for every row of
every frame cheap. Lowercasing happens before normalizing, because
`to_lowercase` can itself change which composition a string is in and the point
is that both sides come out the same.

## What "matches" means for a row, and why it fills in

`Browser::hit` is the single definition — the filter drops rows with it, `/`
finds the next row with it, and the match count counts with it, so the three
cannot disagree. A row is matched against **the tags that have actually been
read**, which is the task's own "filename, and when tags are loaded, `artist`,
`album`, `title`, `genre`".

That has a consequence worth being deliberate about: `f genre:jazz` matches
nothing on the keystroke that types it, and then *fills in* as the reads land.
The alternative — keep every row until its tags are known — shows the whole
directory on the keystroke that was meant to narrow it, which looks broken, and
makes the count on the line untrue of what is on screen.

For that to converge at all, `Browser::wanted` asks for the whole **unfiltered**
directory when the query needs tags, rather than for the visible window. A row
that has been filtered out is not in the listing, so windowing the listing would
mean never asking about the rows whose answer decides whether they belong in it.
`Browser::tags_arrived` re-applies the filter when the answers come, and only
then — a filter on names alone costs no extra work.

## A result set is a listing, not a view

`F` does not open a screen of its own. `Browser::show_results` replaces what the
listing is built from — a flat list of entry indices, in path order — and
everything that already worked on a listing works on it unchanged: marking, the
visual range, `a` / `A`, the details pane, the tag editor, staging a move or a
delete. That is the whole reason the hits live in the browser and not in a view
beside it, and it is what makes "select every file with no genre, set genre" one
keypress after `F missing:genre`.

Three details the flat listing needs:

- **the row is the path**, not the file name: forty hits called `01 Beef Rap.mp3`
  from forty directories would otherwise be forty identical lines. A bare word in
  a result set matches the path for the same reason — it is what is on screen —
  while a bare word in a directory matches only the row name, or a filter typed
  inside `hiphop/MF DOOM` would narrow the directory to all of itself. That
  distinction is `Subject::row` versus `Subject::file`;
- **the keyboard moves to the hits**, because the tree cursor *is* a request for a
  directory, so the first `j` in the tree would throw the result set away;
- **a rescan drops it.** The hits are indices into a model that has been replaced.
  The filter survives a rescan: it is a pattern, and the user did not stop meaning
  it because the library was walked again.

## The walk: what it opens, and what it does not

`query::find` is a pass over the in-memory model plus **one open per file the
query cannot decide without one**. Three outcomes per entry:

1. every term already matches on the name and the path — a hit, nothing opened;
2. `Query::may_match` says no, on the terms that need no tags — skipped, nothing
   opened;
3. undecided — opened, and matched again with its tags.

That is what makes `ext:flac` over the real library cost zero reads, and what
makes `ext:flac artist:doom` open only the FLACs. `missing:genre` is the pitfall
the task names and opens everything, which is why it is a worker with a
percentage on it — the denominator is known before the first file is opened,
unlike a scan's, so the percentage is honest.

A hit carries the `TagSet` **and** the `AudioInfo` from the same open, and
`App::on_found` puts both into the browser's cache before showing the hits. So
the rows the user is about to act on are never read twice, which is the task's
"cache the results for the session". A file whose tags will not read is reported
rather than dropped — a search that could not open nine files found a result set
that may be missing nine, and only the user can decide whether that matters.

`esc` reaches a running walk through a shared `AtomicBool`, the same mechanism a
commit's cancellation uses and for the same reason: a message would have to be
received by a thread that is in the middle of a synchronous pass. A cancelled
walk still answers — what it found before the stop is a result the user asked
for — and says `stopped after N files` so nobody mistakes it for the whole
library.

## Three keys, one line

`/`, `f` and `F` differ in what happens to the query, not in how it is typed, so
they are one `Prompt` with a `Kind` on it. The editing is `widgets::input`'s, the
same one the `:` line and every tag field use, so backspacing over a `ï` cannot
work in one of them and not the others.

| key | while typing | `enter` | `esc` |
| --- | --- | --- | --- |
| `/` | the cursor follows the first match **from where the line opened** | keeps the position | puts the cursor back |
| `f` | the listing narrows | leaves the filter on | puts the *previous* filter back |
| `F` | nothing | starts the walk | nothing was started |

Searching from the position the line opened at, rather than from wherever the
last keystroke left the cursor, is what makes `/` feel incremental instead of
jumpy: `h`, `ho`, `hoe` all answer the same question.

`esc` restores two things, which is why `Prompt::restore` is an `Option<Query>`
and not a flag: `f` typed while a filter is already in force must put *that*
filter back, and "there was none" is one of the values.

`n` / `N` cycle the last submitted `/` pattern, held on `App` because the line is
gone by the time `n` is pressed — that is the whole reason `n` exists. They wrap,
the way vim's do. `ctrl-n` / `ctrl-p` do the same without leaving the line, which
`n` and `N` cannot do while they are letters being typed into it.

## `esc` in the browser, in one place

There are now five things `esc` can mean, and the order is the order the user
means them — most recent first:

1. stop a library search that is running;
2. abandon an open visual range;
3. clear the filter;
4. leave a result set;
5. pop the view stack.

Anything else makes a key that appears not to work. `App::cancel_in_browser` is
the one place that decides it.

## How it is tested

**The grammar** (`crates/core/src/query.rs`, 15 tests): every term, smart case
both ways, the per-term case rule, quoting, the colon that is text, every parse
error and the fields its message lists, and the composed/decomposed `So Hï` in
both directions.

**The walk, against a library on disk** (`crates/core/tests/query.rs`, 9 tests).
`Found::read` and not the global `library::audio_reads` counter is what the "it
opened nothing" assertions count — the counter is process-wide and these tests
run in parallel, so a delta across one call is not that call's.
`a_query_that_needs_no_tags_opens_no_files`,
`missing_genre_finds_the_untagged_files_and_only_the_audio_ones` (which asserts
the count is *every audio file but the two that were tagged*),
`two_terms_narrow_rather_than_widen` (only the FLACs were opened),
`a_search_reports_as_it_goes_and_can_be_called_off`, and
`a_file_whose_tags_will_not_read_is_reported_rather_than_silently_dropped`,
against a deliberately truncated FLAC.

The fixture there writes the tags it means rather than assuming: the fixture's
audio templates come with tags of their own — every mp3 says `MF DOOM`, every
FLAC says `KREAM` — and a test that assumed otherwise would be asserting against
the template.

**The line** (`src/tui/views/search.rs`, 8 tests): the three prefixes, character
cursor arithmetic over a `ï`, what `esc` has to put back, a complaint that lasts
until the text changes, the `no matches` / `1 match` / `9 matches` wording, and a
walk that reports and can be called off exactly once.

**The whole path** (`src/tui/app.rs`, 17 new tests). A `Fixture`, a `TestBackend`
and real worker threads, with every pattern typed one keystroke at a time through
`App::update` — because "a letter reaches the line instead of the verb it is
bound to" is half of what this has to get right, and `n` is `search_next` in the
browser and an `n` in here.

`slash_matches_as_you_type_and_esc_puts_the_cursor_back`,
`backspacing_the_pattern_walks_the_cursor_back_with_it`,
`n_and_capital_n_cycle_the_matches_and_wrap`,
`smart_case_is_vims_rule_in_the_browser_too` (two directories whose names differ
only in case: `doom` matches both, `DOOM` matches one, `Doom` neither),
`f_narrows_the_listing_and_the_status_bar_says_so`,
`esc_on_the_filter_line_puts_the_previous_filter_back`,
`a_filter_that_matches_nothing_says_no_matches_rather_than_looking_empty` (which
also asserts the screen does *not* say "empty directory"),
`a_tag_query_filters_once_the_tags_have_been_read`,
`a_result_set_can_be_marked_and_staged_like_any_other_listing`,
`a_library_search_can_be_called_off_with_esc`,
`colon_find_is_the_same_door_as_capital_f`, and
`a_rescan_drops_a_result_set_because_its_indices_are_of_the_old_model`.

## Measured

`a_library_wide_search_over_three_thousand_files_keeps_the_ui_drawing`: 2 800
files, `missing:genre` — the worst case, where every audio file has to be opened
— with a frame drawn after every message the worker sends.

| | |
| --- | --- |
| the walk | **21 ms**, 45 progress reports |
| the slowest single frame | **192 µs** |

The number that matters is the second one. The walk's cost is I/O and will be
whatever the disk is; the acceptance criterion is that no frame waited on it, and
192 µs is a frame that did not.

Against the **real library** (3 132 files, 318 directories, read-only, warm
cache):

| query | hits | files opened | time |
| --- | --- | --- | --- |
| `ext:flac` | 364 | 0 | < 1 ms |
| `doom` | 469 | 2 388 | 192 ms |
| `missing:genre` | 629 | 2 809 | 334 ms |
| `genre:jazz` | 27 | 2 809 | 197 ms |
| `artist:"MF DOOM"` | 149 | 2 809 | 193 ms |
| `So Hï` | 1 | 2 808 | 199 ms |

One file in that library has a tag `lofty` will not parse; it appears in
`Found::failed` and in the toast as `⚠ 1 unreadable`, and not as a missing hit
nobody was told about. The 629 files with no genre are the reason this feature
exists.
