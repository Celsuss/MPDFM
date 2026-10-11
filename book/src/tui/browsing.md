# Browsing and marking

## The panes

- **Tree** (left) — directories. The row under its cursor is the directory whose
  contents the middle pane shows, so the two can never disagree.
- **Listing** (middle) — the files in that directory, with duration and bitrate when
  there is room. Non-audio files (`folder.jpg`, `.nfo`, `.cue`, logs) are shown
  dimmed on purpose: they are what travels with an album when you move it.
- **Details** (right, 90 columns or wider) — tags, size and format of the focused
  entry, and **which playlists refer to it**. The cover art is drawn here in
  terminals that support it (kitty, iTerm2/WezTerm, sixel; a placeholder elsewhere,
  and never inside tmux).

`tab` switches focus between the tree and the listing.

A `⚠` next to an entry means a playlist refers to it (or, on a directory, to
something inside it), so you can see what a move would touch *before* staging it.
Files that are not valid UTF-8 are counted in a badge rather than silently left out.

## Moving around

| Key | Does |
| --- | --- |
| `j` / `k`, `↓` / `↑` | down / up |
| `l` / `→`, `enter` | into a directory, or open |
| `h` / `←`, `-`, `backspace` | out to the parent |
| `gg` / `G` | first / last row |
| `ctrl-d` / `ctrl-u` | half a page down / up |
| `tab` | switch pane |
| `R` | rescan the library from disk |

Sorting is by name (natural, so `2` comes before `10`) by default. Change it for the
session with `:set sort=name|track|mtime|size`. `track` sorts by the track-number tag.

## Marking

Marks are how you say "these". They **survive navigation**, so you can mark in one
directory, walk to another, and act on the lot. The count is in the status bar.

| Key | Does |
| --- | --- |
| `space` | mark / unmark the row |
| `v` | visual mode: mark a contiguous range as you move |
| `a` | mark everything in the current view |
| `A` | clear all marks |

Whenever you have marked nothing, an action applies to the row under the cursor
instead — so you rarely need to mark a single item.
