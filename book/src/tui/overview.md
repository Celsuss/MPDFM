# Using the TUI

`mpdfm` with no subcommand opens it.

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

## Basics

It is vim-style: `j`/`k` and `h`/`l` walk the tree and the files.

| Key | Does |
| --- | --- |
| `space` | mark |
| `e` | edit tags |
| `m` | stage a move |
| `o` | stage an organize |
| `p` | show what is pending |
| `c` | commit |
| `u` | undo |
| `/` | search |
| `:` | command line |
| `?` | show the keymap you actually have |

## Nothing happens until you commit

Nothing a key does touches the library until you commit, and the pending view
draws the very preview `mpdfm move --dry-run` prints. The terminal is always given
back, including after a panic or a `kill`.

## Terminal options

- `--no-alt-screen` draws in the current screen so frames stay in scrollback.
- `--log FILE` appends the TUI's diagnostics to a file (never the terminal, which
  would corrupt the frame).

Keys can be rebound — see [Key bindings](keys.md).
