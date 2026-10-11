# Changelog

All notable changes to MPDFM are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **A cover view in the TUI.** `C` (or `:cover`) on a folder lists its images and
  each track's embedded front cover. Assign an image to the marked tracks, or to
  every track, with `s`; draft removals with `x`; then `w` stages it all to the
  pending view as ordinary, undoable tag writes. `z` downscales large images
  first.
- **Singles are paired with their covers by name.** In a folder of singles, `n`
  drafts each track's own image, matching `01 - Song A.mp3` to `song_a.jpg` and
  a downloaded `Artist_-_Song_Title_(AMV)_[id].mp3` to `songtitle.jpg`. A track
  nothing matches is named, and `h` / `l` on it picks its image by hand.
- **Tidying loose images from the TUI:** `e` writes the embedded cover out, `C`
  renames an image to `cover.<ext>`, `d` deletes one and `D` deletes every image
  but the one under the cursor, all staged like any other operation.
- `:cover set <image>`, `:cover remove` and `:cover extract [name]` mirror
  `mpdfm cover` from the TUI's command line.
- The details pane shows a folder's best image, and how many there are.

### Fixed

- The `keys.toml` warning panel keeps `esc to dismiss` in its border, where a
  long list of action names can no longer push it out of sight.

- **A move can no longer hide a track from MPD.** Moving one file to a name that
  is not a folder — `:move Singles` in the TUI, `mpdfm move song.mp3 Singles` —
  renamed it to a file called `Singles` with no audio extension, and rewrote the
  playlists to follow it, so MPD refused the line with "No such directory". A
  file move that drops or changes a track's (or CUE sheet's) extension is now
  refused before anything is written.
- **A trailing `/` or an existing folder moves a file into it**, as with `mv`:
  `:move coding-music/Singles/` and `mpdfm move song.mp3 coding-music/Singles/`
  both give `coding-music/Singles/song.mp3`, creating the folder if needed. A
  directory still goes to exactly the path given, so `--merge` and shell
  completion's trailing `/` mean what they did. `:move dir/` with several marks
  is no longer refused for its trailing slash.
- **`mpdfm doctor` reports a playlist or saved-queue line that names a file which
  is not a track** (an extensionless file, an image), which MPD cannot play but
  which used to pass as "in the library".

## [0.1.0] - Unreleased

The first release. Everything below is new.

### Added

- **Safe moves.** `mpdfm move SRC DST` moves or renames a file or directory and
  rewrites every playlist line and the entries of MPD's saved queue that refer to
  it, as one transaction: previewed, backed up and journaled before the first
  change, never overwriting. `--dry-run`, `--merge` and `--verify` (hash every
  cross-device copy) are available.
- **Undo and recovery.** Every committed transaction is reversible byte for byte
  with `mpdfm undo`. A transaction a crash left unfinished is surveyed against the
  disk and rolled back (or, with `--forward`, finished) by `mpdfm recover`.
  `docs/SAFETY.md` has the same recovery by hand.
- **Tag editing** for mp3 (ID3v2), FLAC (Vorbis comments) and m4a, single files or
  in bulk, via `mpdfm tag show | set | diff`. A field that differs across a
  selection is never written unless you change it; artwork, ReplayGain,
  MusicBrainz ids, lyrics and unknown frames are left exactly as they were.
  Bulk actions: `--renumber-tracks`, `--title-from-filename`,
  `--album-artist-from-artist`, `--strip-comment`, `--trim-whitespace`.
- **Re-organization by template.** `mpdfm organize` re-files tracks (and their
  cover art and sidecar files) into paths derived from tags, e.g.
  `{genre}/{albumartist}/{year} - {album}/{track:02} {title}`, with
  `--only-missing` and `--limit` for working through a large library in stages.
  Characters FAT and NTFS refuse are replaced unless `organize_portable_names` is
  off.
- **Library doctor.** `mpdfm doctor` reports broken playlist references, missing
  and inconsistent tags, filesystem oddities and (with `--deep`) byte-identical
  duplicates, without reporting what is not wrong.
- **Cover art.** `mpdfm cover show | set | extract | remove`, all previewed and
  undoable.
- **A vim-style TUI** (`mpdfm` with no subcommand): a library browser, a tag
  editor, a pending-operations view that draws the same preview as `--dry-run`,
  search and filter over one query language, a status bar, a help overlay generated
  from the live keymap, and cover art in kitty, iTerm2/WezTerm and sixel terminals.
  The terminal is restored after a panic or a `kill`.
- **Configuration.** `config.toml` (every setting optional; `mpdfm config show`
  prints each one's value and where it came from), falling back to `mpd.conf`.
  `keys.toml` rebinds any TUI action. Both are documented in `docs/`.
- **`mpdfm completions <shell>`** and **`mpdfm man [--dir DIR]`**, both generated
  from the argument definitions. `--help` for every command has a description and
  an example.
- **`--json`** on every command, with documented exit codes: `0` success, `1`
  error, `2` refused before anything was written, `3` declined.

[Unreleased]: https://github.com/Celsuss/MPDFM/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Celsuss/MPDFM/releases/tag/v0.1.0
