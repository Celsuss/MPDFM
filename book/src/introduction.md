# MPDFM — MPD File Manager

MPDFM is a terminal application for managing an [MPD](https://www.musicpd.org/)
music library: edit tags, and re-organize directories **without breaking your
playlists**.

## Why it exists

MPD identifies every track by its path relative to `music_directory`, and that
path is copied into every `.m3u` in your playlist directory *and* into the saved
queue in MPD's `state` file, so a plain `mv` silently breaks both. MPDFM makes
moving a file and rewriting every reference to it **one** transaction: previewed
first, backed up and journaled before the first change, and reversible byte for
byte. If it is killed halfway, `mpdfm recover` finishes or rolls back what it
started — and [Safety and recovery](safety.md) says how to do the same by hand.

## Start here — nothing in these two writes a byte

```console
$ mpdfm doctor                                           # what is wrong, today
$ mpdfm move --dry-run "hiphop/MF DOOM" "hiphop/Doom"    # what a move would do
```

`doctor` reports broken playlist references, missing tags, inconsistent albums and
the like, and says plainly when the library is fine. `--dry-run` prints the same
preview that appears before every commit: the moves, each playlist line that
changes, and anything that would stop it. Run those on your real library first,
then try your first real command on a copy.

## Where to go next

- New here? [Install](getting-started/install.md) and do a [first run](getting-started/first-run.md).
- Want to do something? The **Guide** has a page each for
  [moving](guide/move.md), [tags](guide/tags.md), [organizing](guide/organize.md),
  [cover art](guide/cover.md) and [undo](guide/undo-recover.md).
- Prefer a full-screen interface? See [the TUI](tui/overview.md).
- Worried about your library? Read [Safety and recovery](safety.md).
- Looking up a flag? The [command-line reference](reference/cli.md) is generated
  from the program itself, so it always matches the version you have.

## What it will and will not do

- **MPD is optional.** With it running, MPDFM asks it to rescan after a commit and
  warns about moved files that are in the live queue. Without it, MPDFM rewrites
  the saved queue in the state file itself.
- **It never overwrites.** A move that would land on an existing file is a
  conflict, reported before anything happens.
- **It does not guess.** A path that is not valid UTF-8 is reported and skipped.
  A playlist line is rewritten only on an exact path match; URLs, comments,
  `#EXTINF` lines and anything it does not understand are written back byte for
  byte.
- **It has a delete switch.** `delete_enabled = false` makes every operation that
  would remove a file refuse.
