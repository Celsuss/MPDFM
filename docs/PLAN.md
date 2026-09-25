# MPDFM — Design Plan

MPD File Manager: a terminal application for editing music metadata and
re-organizing an MPD music library **without breaking the playlists, the saved
queue, or anything else that stores a path**.

Written during the design session of 2026-09-24. This document is the
architectural reference; `ROADMAP.md` holds the ordered task list and status.

---

## 1. Problem statement

MPD identifies every track by its path relative to `music_directory`. That
relative path is duplicated in at least three places outside the audio file:

| Location | Format | Count in this library |
| --- | --- | --- |
| `~/.config/mpd/playlists/*.m3u` | one relative path per line | 231 track entries across 17 playlists |
| `~/.config/mpd/state` | `N:relative/path` lines in the saved queue | 61 entries |
| `~/.config/mpd/database` | MPD's own index | ~2 800 tracks |

Moving or renaming a file with `mv` silently invalidates every one of those
references. The database recovers on rescan; **the playlists and the saved queue
do not**. MPDFM exists to make the move and the reference-rewrite a single
atomic, reversible operation.

The second goal is ordinary metadata editing (ID3v2 for mp3, Vorbis comments for
FLAC), single-file and in bulk, because bad tags are what make a library need
re-organizing in the first place.

---

## 2. Decisions

| # | Decision | Rationale |
| --- | --- | --- |
| D1 | **Rust**, `ratatui` + `lofty` | User reads Rust. `lofty` writes ID3v2, Vorbis comments and MP4 atoms with no C dependency; Go's equivalent needs cgo + system taglib, which would undercut the headline feature. |
| D2 | **Core library + CLI + TUI** | All logic lives in `mpdfm-core` with no terminal and no `clap`. The CLI makes the dangerous paths testable and scriptable; the TUI is a second front-end over the same API. |
| D3 | **Manual moves first, template-driven organize second** | Both share one safe-move engine, so no work is wasted. Manual is the primitive; templates are a plan generator on top. |
| D4 | **Rewrite playlists on disk, with backups + journal + undo** | MPD's protocol has no atomic path-replace and cannot see playlists it hasn't indexed. Direct file editing is the only approach that can be transactional. |
| D5 | **mp3 + FLAC in v1** (m4a free via lofty, untested) | 2 797 of ~2 800 audio files. A tool that skipped FLAC during a reorg would be a trap. |
| D6 | **MPD connection optional and non-fatal** | Filesystem is the source of truth. If `127.0.0.1:6600` answers we trigger `update` after a commit and warn about queue conflicts; with MPD stopped everything still works. |
| D7 | **Stage → preview → commit** | Nothing touches disk until confirmed. The preview shows filesystem changes *and* which playlist lines will be rewritten. Every commit is journaled and undoable. |
| D8 | **Vim-style keys, configurable** | Consistent with rmpc, which the user already drives. Remappable from config without recompiling. |
| D9 | **Tasks as markdown in `docs/tasks/`** | Versioned with the code, readable offline, no auth needed to resume work in a later session. |

Explicitly **out of scope** for now: MusicBrainz / AcoustID lookup, in-app
playlist authoring (create/reorder/dedupe), audio playback. Cover art and a
library `doctor` are in scope as later phases.

---

## 3. Observed facts about this library

Gathered from the real system, not assumed. These drive the edge cases in §6.

```
music_directory     ~/Music                        (ext4)
playlist_directory  ~/.config/mpd/playlists        (ext4, same fs)
mpd listens on      127.0.0.1:6600
audio files         2 440 mp3 · 357 flac · 5 m4a
aux files           183 jpg · 34 jpeg · 21 txt · 21 nfo · 18 png · 4 sfv · 4 cue · 2 pdf · 2 log · 2 gif
layout              genre at top level: rock/ hiphop/ lofi/ japanese/ electronic/ …
                    then scene-style album dirs, e.g.
                    "hiphop/Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube/01.Smokin' On.mp3"
                    multi-disc dirs exist: "…(2 CD)…/CD 1 - Mercury - Acts 1/"
playlists           17 files, relative paths, no leading "./"
                    Radios.m3u is a SYMLINK to ~/workspace/dotfiles/mpd/playlists/Radios.m3u
                    Radios.m3u contains #EXTM3U, "# comment" lines, #EXTINF:-1,Name and http(s):// URLs
                    Pop.m3u contains a CUE virtual track, currently broken:
                      "pop/…/Imagine Dragons - Mercury - Acts 1.flac.cue/track0017"
                    1 broken reference out of 231 total
filenames           1 023 contain non-ASCII characters; all are valid UTF-8
```

---

## 4. Architecture

```
MPDFM/
├── Cargo.toml                    workspace
├── crates/
│   └── core/                     mpdfm-core — no terminal, no clap, no I/O surprises
│       ├── config.rs             mpd.conf parsing + mpdfm's own config
│       ├── paths.rs              RelPath newtype, normalization, containment checks
│       ├── library/
│       │   ├── scan.rs           walk music_directory, classify entries
│       │   └── model.rs          Track, AuxFile, AlbumDir
│       ├── playlist/
│       │   ├── parse.rs          m3u → Vec<Entry>, byte-preserving
│       │   ├── write.rs          atomic write, symlink-aware
│       │   └── index.rs          RelPath → [reference] map
│       ├── tags/
│       │   ├── read.rs           lofty → TagSet
│       │   ├── write.rs          TagSet → file, preserving unknown frames
│       │   └── bulk.rs           multi-file common/<multiple> semantics
│       ├── ops/
│       │   ├── op.rs             Operation enum
│       │   ├── plan.rs           Plan, validate() → Effects (the preview)
│       │   └── exec.rs           commit(), two-phase with journal
│       ├── journal/              transaction records, backups, undo
│       ├── organize/             template parser + plan generator
│       ├── mpd.rs                minimal line-protocol client
│       ├── state.rs              MPD state-file queue rewriting
│       └── doctor.rs             library health checks
└── src/
    ├── main.rs                   clap subcommands; `mpdfm` with no args → TUI
    ├── cli/                      thin command wrappers, output formatting
    └── tui/
        ├── app.rs                state machine, event loop
        ├── keys.rs               configurable keymap → Action
        └── views/                browser · tagedit · pending · help · search
```

### Why no MPD client crate

We need exactly three commands (`update`, `status`, `currentsong`) over a
trivial newline-delimited text protocol. A hand-rolled ~150-line client on
`std::net::TcpStream` avoids an async runtime and a dependency whose maintenance
we would not control. See task `13`.

### Dependencies

| Crate | Version at planning time | Purpose |
| --- | --- | --- |
| `ratatui` | 0.30 | TUI widgets and layout |
| `crossterm` | 0.29 | terminal backend, events |
| `lofty` | 0.25 | tag read/write for mp3, flac, m4a |
| `clap` | 4.6 | CLI, derive feature |
| `camino` | 1.2 | `Utf8Path` — paths are UTF-8 by contract here |
| `walkdir` | 2.5 | directory traversal |
| `serde` + `serde_json` | 1.0 | config and journal |
| `toml` | 1.1 | config file |
| `anyhow` / `thiserror` | — | error handling: `thiserror` in core, `anyhow` at the edges |
| `tempfile` | 3.27 | atomic writes and test fixtures |
| `insta` | 1.48 | snapshot tests for previews and playlist round-trips |

Pin exact versions in `Cargo.toml` at scaffold time; the numbers above may have
moved.

---

## 5. Core model

### Identity: `RelPath`

The canonical identity of a track is its **path relative to
`music_directory`**, UTF-8, `/`-separated, no leading `./`, no `..`, never
absolute — because that is exactly what playlists, the state file and MPD all
store. A newtype enforces this so a raw `PathBuf` can never leak into a
playlist line.

### Playlist entries are byte-preserving

```rust
enum Entry {
    Blank,
    Comment(String),                                   // "# Liquid Drum & Bass"
    ExtM3u,                                            // "#EXTM3U"
    ExtInf { duration: i64, title: String },           // "#EXTINF:-1,Lofi Radio"
    Url(String),                                       // "http://ice1.somafm.com/…"
    Track { rel: RelPath, cue: Option<String>, raw: String },
}
```

Every variant keeps enough information to be written back **byte-identically**.
Only `Track` entries whose `rel` exactly matches a moved file are ever rewritten;
everything else round-trips untouched. `cue` holds the `trackNNNN` suffix of an
MPD CUE virtual track, so `album.flac.cue/track0017` moves as a unit.

### Operations and the two-phase commit

```rust
enum Operation {
    MoveFile   { from: RelPath, to: RelPath },
    MoveDir    { from: RelPath, to: RelPath },
    Delete     { target: RelPath },
    WriteTags  { target: RelPath, changes: TagDelta },
}

Plan { ops: Vec<Operation> }
  .validate(&Library, &PlaylistIndex) -> Effects    // pure; no disk writes
Effects {
    fs_changes:      Vec<FsChange>,
    playlist_edits:  Vec<(PlaylistPath, Vec<LineEdit>)>,
    state_edits:     Vec<LineEdit>,                  // MPD saved queue
    conflicts:       Vec<Conflict>,                  // blocks commit
    warnings:        Vec<Warning>,                   // informational
}
  .commit() -> TransactionId
```

`commit()` is deliberately two-phase so that a crash mid-operation is
recoverable:

1. Write journal record with status `pending`, including every backup path.
2. Copy affected playlists (and the state file) into
   `~/.local/share/mpdfm/backups/<txid>/`.
3. Execute filesystem operations in order, appending each completed step to the
   journal.
4. Rewrite playlists and the state file atomically (temp file + `rename` in the
   same directory; resolve symlinks first so `Radios.m3u` edits the dotfiles
   target, not the link).
5. Mark journal record `complete`.
6. If MPD is reachable, `update` the affected directories.

A `pending` record found at startup means a previous run died: offer
`mpdfm undo <txid>` or `mpdfm recover`.

### Safety invariants

These are the properties the test suite exists to defend. Any change that
violates one is a bug regardless of how convenient it is.

1. No filesystem mutation happens outside `commit()`.
2. The journal record is durable **before** the first mutation.
3. A playlist line is rewritten only on an exact normalized-`RelPath` match —
   never a prefix, substring or fuzzy match.
4. Non-track playlist lines (URLs, `#EXTINF`, comments, blanks) are preserved
   byte-for-byte.
5. No operation may read or write outside `music_directory`, `playlist_directory`
   and MPDFM's own data dir. Symlinks are resolved before this check.
6. Every file write is atomic: temp file in the destination directory, `fsync`,
   `rename`.
7. Cross-device moves are copy → `fsync` → verify size/hash → `rename` → unlink,
   never a partial `rename` failure left half-done.
8. A path that is not valid UTF-8 is reported and skipped, never guessed at.
9. Every committed transaction is undoable, and `undo` verifies preconditions
   before acting rather than blindly reversing.

---

## 6. Edge cases to handle

Each of these came from the real library or the real MPD file formats. The task
that owns each one is named.

| Edge case | Owner task |
| --- | --- |
| CUE virtual tracks: `album.flac.cue/track0017` | 06, 09 |
| Radio URLs and `#EXTINF` / `#EXTM3U` / `# comment` lines | 06 |
| Playlist that is a **symlink** into a dotfiles repo (`Radios.m3u`) | 06, 09 |
| MPD saved queue in `~/.config/mpd/state` also holds 61 paths | 14 |
| Moving a file that is in MPD's *current* queue (live, not saved) | 13, 14 |
| Aux files (jpg, nfo, sfv, cue, log, m3u, pdf) must travel with their album | 05, 08 |
| Multi-disc album subdirectories (`CD 1 - …`) | 05, 27 |
| Destination already exists → conflict, or directory merge | 08 |
| Empty directories left behind after a move | 08 |
| Cross-filesystem move (music on another mount) | 08 |
| Non-UTF-8 filenames | 02, 05 |
| Filenames with `/`, `:`, leading dots, or >255 bytes produced by a template | 27 |
| Case-insensitive collisions (`Artist` vs `artist`) | 08, 27 |
| ID3v2.3 vs v2.4, and preserving frames MPDFM doesn't model | 17 |
| A file that is read-only, or a directory without write permission | 08 |
| Already-broken playlist references (1 exists today) | 07, 29 |
| Two playlists referencing the same file | 07, 09 |
| Very long lists: ~2 800 tracks must scroll without lag | 22 |

---

## 7. Data locations MPDFM owns

```
~/.config/mpdfm/config.toml          settings; music_dir/playlist_dir override mpd.conf
~/.config/mpdfm/keys.toml            keymap
~/.local/share/mpdfm/journal/*.json  one record per transaction
~/.local/share/mpdfm/backups/<txid>/ playlist + state file copies
~/.cache/mpdfm/scan.json             optional scan cache (only if scanning proves slow)
```

Respect `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME` when set.

---

## 8. Testing strategy

The move engine is the part that can destroy data, so it is tested first and
hardest, before any UI exists.

- **Fixture builder** — construct a synthetic library in a `tempfile::TempDir`:
  genre dirs, scene-style album names, non-ASCII names, a multi-disc album, aux
  files, a symlinked playlist, a CUE virtual track, a radio URL playlist, and a
  deliberately broken reference. Real mp3/flac files are generated with
  `ffmpeg` (present on this machine) or committed as tiny fixtures.
- **Round-trip property**: parse → serialize any playlist ⇒ byte-identical.
- **Move invariants**: after any committed move, every playlist entry that
  resolved before still resolves, and the set of referenced inodes is unchanged.
- **Undo invariants**: commit → undo ⇒ filesystem and every playlist are
  byte-identical to the starting state.
- **Crash injection**: abort between phases of `commit()`; the journal must leave
  the library recoverable.
- **Snapshot tests** (`insta`) for `Effects` previews and CLI output.
- No test may touch the user's real `~/Music` or `~/.config/mpd`. A guard in the
  test harness asserts every path used is inside the temp dir.

---

## 9. Milestones

1. **M1 — Trustworthy move engine** (tasks 01–15): CLI can scan, report, move
   with `--dry-run`, rewrite playlists and the saved queue, and undo. Fully
   tested. *This is the next session's target.*
2. **M2 — Tag editing** (16–19): read/write mp3 + FLAC, single and bulk, from
   the CLI.
3. **M3 — TUI** (20–26): browser, tag editor, staging view, search, chrome.
4. **M4 — Organize by template** (27–28).
5. **M5 — Doctor and cover art** (29–30).
6. **M6 — Polish and packaging** (31–32).

Task files live in `docs/tasks/`; `ROADMAP.md` is the index and status board.
