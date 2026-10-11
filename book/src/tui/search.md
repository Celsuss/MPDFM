# Search, filter and library-wide find

Three tools, all typing into the same line at the bottom of the screen.

| Key | Does |
| --- | --- |
| `/` | **search** the current listing, incrementally |
| `f` | **filter**: narrow the listing to matches until `esc` |
| `F` | **find** across the whole library; hits become a flat listing |

## Search: `/`

Matches as you type and moves the cursor to the first match. `ctrl-n` / `ctrl-p`
cycle matches while you are still typing; after `enter`, `n` / `N` do. `enter`
keeps the position, `esc` returns to where you were.

## Filter: `f`

Hides everything that does not match, and stays on until you press `esc`. An active
filter is always shown in the status bar, so it is never invisible.

## Find in the whole library: `F` or `:find <query>`

Walks every file, reading tags as needed on a worker with a progress line (`esc`
cancels). The results appear as a flat listing you can **mark and stage from** like
any other — for example, `F missing:genre`, then `a` to mark every hit, then `e`
and set the genre.

## The query language

Bare words match the file name, the path, and the title, artist, album and genre
tags. Several words must all match. Matching is case-insensitive unless the pattern
contains an uppercase letter (smart case), and non-ASCII text matches as typed.

| Term | Matches |
| --- | --- |
| `doom` | the word anywhere |
| `artist:doom`, `album:"mm..food"` | that tag; quote values with spaces |
| `genre:jazz`, `genre:"hip hop"` | genre |
| `ext:flac` | file extension |
| `missing:genre` | files with no value for that tag |

An empty result says "no matches" rather than showing a blank pane.
