# First run

Check what MPDFM found, once:

```console
$ mpdfm config show               # where it found your library, and why
```

MPDFM reads `music_directory`, `playlist_directory` and the state file from your
`mpd.conf`, so on a normal MPD setup there is nothing to configure. If `config
show` says otherwise, the [configuration reference](../reference/config.md)
documents every setting, and `--music-dir` / `--playlist-dir` override anything
for a single run.

Then look around without changing anything:

```console
$ mpdfm scan                      # counts by format, album dirs, warnings
$ mpdfm doctor --full             # every finding, not the first ten
```

Paths in every command are **relative to your music directory**, which is what
MPD's own paths are.

## The ground rules

- Every command takes `--json`.
- Every command that edits tags, moves or re-files takes `--dry-run`, which prints
  the preview and writes nothing.
- Every command that changes the library asks before it commits. `--yes` skips
  the question, and is required when stdin is not a terminal.
- Try your first real command on a **copy** of part of your library.
  See [Safety and recovery](../safety.md) for exactly what is backed up.
