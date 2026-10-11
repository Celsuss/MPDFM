# Commands, status bar and messages

## The command line: `:`

`:` opens a line at the bottom. It mirrors the command-line interface — same names,
same arguments — so what you know from one carries to the other. A command that does
not parse stays open with the reason under it, so you can edit rather than retype.
`esc` closes it; `ctrl-u` clears it.

| Command | Does |
| --- | --- |
| `:move <dst>` (`:mv`) | stage a move or rename of the marks / cursor row |
| `:organize <template>` | open the organize view with this template |
| `:find <query>` (`:f`) | find across the whole library |
| `:undo [txid]` | reverse a committed transaction, the latest by default |
| `:messages` (`:mes`) | everything said this session, errors included |
| `:set sort=<name\|track\|mtime\|size>` | change the browser's sort for this session |
| `:q` / `:q!` | quit, asking first if operations are staged / quit regardless |

`:doctor` is recognised but not yet wired into the TUI; run
[`mpdfm doctor`](../reference/cli/doctor.md) from a shell for now.

## Status bar

The bottom line is always visible and, from left to right, shows the current path,
how many items are marked, how many operations are pending, any active filter, the
MPD state and the current song when one is playing. MPD shows as `● connected`,
`○ offline` or `◐ updating`, refreshed about once a second without ever blocking the
interface. MPD being offline does not stop you working: the library is read from
disk, and playlist and queue changes are still made on the files.

On a narrow terminal, the least important parts are dropped first.

## Help: `?`

A scrollable overlay generated from your live keymap, grouped by mode, with the
command list. Because it is generated, it shows your rebindings and cannot document
a key that no longer exists.

## Messages and errors

- **Messages** — "committed 20260924T224500Z-a3f1 — 14 files, 2 playlists · u to
  undo" — appear on the message line and fade. `:messages` shows the history.
- **Errors** open a panel that must be dismissed, with the full message, the path
  involved and the suggested next step. They are never cut to one line.
- **Progress** for scans, finds and commits is a slim line with counts.

## Quitting

`q` quits; with operations staged it asks first. `ctrl-c` always works (it asks if
anything is staged, and leaves on the second press) and cannot be rebound. If
`music_directory` cannot be found, a first-run screen explains where MPDFM looked
instead of showing an empty pane.
