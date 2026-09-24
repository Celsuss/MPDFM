# MPDFM — MPD File Manager

A terminal application for managing an [MPD](https://www.musicpd.org/) music
library: edit metadata, and re-organize directories **without breaking your
playlists**.

> Status: design complete, implementation not started.
> See [`docs/PLAN.md`](docs/PLAN.md) and [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Why

MPD identifies every track by its path relative to `music_directory`, and that
path is duplicated outside the audio file in at least three places:

- every `.m3u` in your `playlist_directory`
- the saved queue in MPD's `state` file
- MPD's own database

`mv` silently breaks the first two. The database recovers on a rescan; the
playlists and the saved queue do not. MPDFM makes moving a file and updating
every reference to it a single atomic, reversible operation.

## Planned features

- **Metadata editing** for mp3 (ID3v2) and FLAC (Vorbis comments), single files
  or in bulk, with honest handling of fields that differ across a selection
- **Safe re-organization** — move and rename by hand, or apply a template like
  `{genre}/{albumartist}/{year} - {album}/{track:02} {title}`
- **Playlist integrity** — affected playlist lines are rewritten; radio URLs,
  `#EXTINF` metadata and comments are preserved byte-for-byte
- **Stage, preview, commit** — nothing touches disk until you have seen the full
  diff, including which playlist lines change
- **Undo** — every commit is journaled and reversible
- **Library doctor** — broken references, missing tags, inconsistent albums
- **Cover art** — view, extract and embed
- Vim-style keybindings, a TUI built on ratatui, and a scriptable CLI

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
