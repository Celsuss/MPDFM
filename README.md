# MPDFM — MPD File Manager

A terminal application for managing an [MPD](https://www.musicpd.org/) music
library: edit metadata, and re-organize directories **without breaking your
playlists**.

> Status: **M1 and M2 complete** — the move engine, the tag editor and their CLI
> work and are tested. `mpdfm scan`, `mpdfm doctor`, `mpdfm move`,
> `mpdfm tag show/set/diff`, `mpdfm undo` and `mpdfm recover` are usable today,
> all with `--dry-run` where they write, a confirmation prompt, `--json` and one
> journaled, reversible transaction per commit. The TUI (M3) is next.
> See [`docs/PLAN.md`](docs/PLAN.md) and [`docs/ROADMAP.md`](docs/ROADMAP.md).

```console
$ mpdfm tag diff --genre "Hip Hop" --renumber-tracks -r "hiphop/Mac + Devin Go To High School"
hiphop/Mac + Devin Go To High School/01.Smokin' On.mp3
  genre        "Soundtrack" → "Hip Hop"
  track        "1" → "1/12"
...
PENDING (12 ops)
TAG    genre = "Hip Hop"  12 files
TAG    track <per file>   12 files

tag diff: nothing was changed.
```

## Why

MPD identifies every track by its path relative to `music_directory`, and that
path is duplicated outside the audio file in at least three places:

- every `.m3u` in your `playlist_directory`
- the saved queue in MPD's `state` file
- MPD's own database

`mv` silently breaks the first two. The database recovers on a rescan; the
playlists and the saved queue do not. MPDFM makes moving a file and updating
every reference to it a single atomic, reversible operation.

## Features

Built and tested:

- **Metadata editing** for mp3 (ID3v2), FLAC (Vorbis comments) and m4a (atoms),
  single files or in bulk, with honest handling of fields that differ across a
  selection — a `<multiple>` field is never written unless you change it
- **Nothing else in the file changes.** Embedded artwork, ReplayGain, MusicBrainz
  ids, lyrics and every unknown frame survive a tag edit untouched, and the audio
  stream is bit-identical. Verified against all 2 808 files in the author's
  library, on copies
- **Safe moves** — `mv` that updates every playlist, MPD's saved queue, and asks
  MPD to rescan
- **Undo** — every commit is journaled and reversible, byte-for-byte

Planned:

- **Template-driven re-organization** — apply a layout like
  `{genre}/{albumartist}/{year} - {album}/{track:02} {title}`
- **Library doctor** — broken references, missing tags, inconsistent albums
- **Cover art** — view, extract and embed
- Vim-style keybindings and a TUI built on ratatui, over the same core the CLI
  uses

## Design

| | |
| --- | --- |
| Language | Rust |
| TUI | ratatui + crossterm |
| Tags | lofty (no C dependencies) |
| MPD | optional, non-fatal; a minimal protocol client for `update` and status |
| Structure | `mpdfm-core` library + CLI + TUI over one API |

Read [`docs/PLAN.md`](docs/PLAN.md) for the full architecture, the safety
invariants, and the catalogue of edge cases.

## License

See [LICENSE](LICENSE).
