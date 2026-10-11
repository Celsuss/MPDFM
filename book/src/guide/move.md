# Moving and renaming

`mpdfm move SRC DST` moves or renames a file or directory **and** rewrites every
reference to it — in every `.m3u` playlist and in MPD's saved queue — as one
transaction.

```console
$ mpdfm move --dry-run "rock/Old Name" "rock/New Name"   # look first
$ mpdfm move "rock/Old Name" "rock/New Name"             # shows the preview, asks, commits
$ mpdfm undo                                             # changed your mind
```

## The preview

Before anything is written you see the moves, each playlist line that changes, and
anything that would stop the move. `--dry-run` prints that preview and stops.

## What can stop a move

- **A conflict.** A move that would land on an existing file is refused before
  anything happens; MPDFM never overwrites. Moving a directory onto one that
  exists needs `--merge`, which moves what does not collide.
- **The library changed since the preview.** The commit is refused (exit code `2`)
  rather than acting on stale information.
- **A path that is not valid UTF-8.** It is reported and skipped.

## Across file systems

A move between devices is a copy followed by a delete. `--verify` hashes every
copy and reads it back, at the cost of a second read of each file.

## How playlists are rewritten

A playlist line is rewritten only on an **exact** path match. URLs, comments,
`#EXTINF` lines and anything MPDFM does not understand are written back byte for
byte. Every playlist it touches is backed up first — see
[Safety and recovery](../safety.md).

See also the [`move` reference](../reference/cli/move.md).
