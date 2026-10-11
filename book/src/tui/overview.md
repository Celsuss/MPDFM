# Using the TUI

`mpdfm` with no subcommand opens it. It does everything the command line does to a
library — move, rename, re-tag, organize, undo — but you walk the library instead of
typing paths, and you see the whole plan before anything is written.

```text
MPDFM  ~/Music
┌ music ────────────────┐┌ hiphop/MF DOOM - Mm..Food (2004) [V0] scene-ta┐┌ details ───────────────┐
│⚠ ▾ /                  ││  ⚠ 01 Beef Rap.mp3                     ·     ·││01 Beef Rap.mp3         │
│    ▸ coding-music     ││    02 Hoe Cakes.mp3                    ·     ·││                        │
│    ▸ electronic       ││    03 Potholderz (feat. Count Bas…     ·     ·││reading…                │
│⚠   ▾ hiphop           ││    eac.log                                    ││                        │
│⚠       MF DOOM - Mm..…││    folder.jpg                                 ││Kind    mp3             │
│⚠       Snoop Dogg & W…││    info.nfo                                   ││Size    1 kB            │
│    ▸ jazz             ││    Mm..Food.m3u                               ││                        │
│⚠   ▸ pop              ││    mm..food.sfv                               ││⚠ 2 lines in 2 playlists│
└───────────────────────┘└ 1/8 ──────────────────────────────────────────┘└────────────────────────┘
```

The tree, the album's files, and the details pane, whose `⚠` marks say which
entries a playlist refers to and what is wrong with them.

## The idea in four steps

Every job in the TUI has the same shape:

1. **Find** what you want — walk the tree, [search or filter](search.md).
2. **Select** it — [mark](browsing.md#marking) files, or just leave the cursor on one.
3. **Stage** an operation — [move, rename, delete, organize](staging.md) or
   [edit tags](tags.md). Staging only adds to a plan.
4. **Review and commit** — the [pending view](pending.md) shows every file that
   moves and every playlist line that changes. `c` commits; `u` undoes.

Nothing a key does touches the library until step 4, and the pending view draws the
very preview `mpdfm move --dry-run` prints. The terminal is always given back,
including after a panic or a `kill`.

## A first session

Moving an album, from nothing:

1. Walk to the album with `j`/`k` and `l`/`h` (the tree follows you).
2. Press `r`. The command line opens as `:move hiphop/MF DOOM - Mm..Food`. Edit the
   path to the new name and press `enter`.
3. Press `p`. You see the move and the playlist lines it rewrites. Expand a playlist
   with `enter` to read the exact `-`/`+` lines.
4. Press `c` to commit. A message gives the transaction id.
5. Wrong? Press `u`.

## Pages in this section

- [Browsing and marking](browsing.md) — the panes, moving around, selecting.
- [Staging moves, renames, deletes and organize](staging.md)
- [Editing tags](tags.md) — the tag editor.
- [The pending view and committing](pending.md) — review, commit, undo.
- [Search, filter and library-wide find](search.md)
- [Commands, status bar and messages](commands.md) — the `:` line and the chrome.
- [Key bindings](keys.md) — every default, and how to rebind.

## Terminal options

- `--no-alt-screen` draws in the current screen so frames stay in scrollback.
- `--log FILE` appends the TUI's diagnostics to a file (never the terminal, which
  would corrupt the frame).

Press `?` at any time for the keymap you actually have.
