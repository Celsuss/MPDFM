# 05 — Library scanner

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 02, 03
- **Status:** not started

## Goal

Walk `music_directory` and produce an in-memory model of tracks, auxiliary
files and album directories, fast enough to run at startup on ~3 100 files.

## Details

```rust
enum Kind { Audio(Format), Image, Cue, Playlist, Sidecar, Other }
enum Format { Mp3, Flac, M4a }

struct Entry { rel: RelPath, kind: Kind, size: u64, mtime: SystemTime }

struct Library {
    root: Utf8PathBuf,
    entries: Vec<Entry>,                    // sorted by rel
    by_dir: BTreeMap<RelPath, Vec<usize>>,  // directory → entry indices
    warnings: Vec<ScanWarning>,             // non-UTF-8, unreadable, symlink loops
}
```

Classification by extension, case-insensitively: `mp3`/`flac`/`m4a` are audio;
`jpg`/`jpeg`/`png`/`gif` images; `cue`; `m3u`/`m3u8` playlists; `nfo`/`sfv`/
`txt`/`log`/`pdf`/`sfk` sidecars. Everything else is `Other`. **Nothing is
silently dropped** — a reorg must move the whole album directory including the
`.nfo` and `.sfv` clutter, so unknown files still appear in the model.

`AlbumDir` is a derived view: a directory that directly contains audio files.
Expose `album_dirs()`, and for multi-disc sets note the parent relationship
(`pop/…(2 CD)/CD 1 - …` is an album dir whose parent is also part of the set) —
task 27 needs this.

Tags are **not** read during the scan. Tag reading is lazy and on demand
(task 16); reading 2 800 files' tags at startup would be wasteful. Design the
API so the TUI can request tags for the visible window only.

Do not follow symlinks by default (avoid loops); record them as warnings.

## Acceptance criteria

- [ ] scans the fixture library and classifies every file correctly
- [ ] a full scan of a 3 000-file tree completes in well under 1 s warm
      (benchmark it and write the number in this file)
- [ ] non-UTF-8 filename → `ScanWarning::NotUtf8`, scan continues
- [ ] unreadable directory → warning, scan continues
- [ ] symlinked directory is not traversed, and is reported
- [ ] `album_dirs()` identifies the multi-disc fixture's disc directories and
      links them to the set root
- [ ] `by_dir` lookups are used by the browser without re-walking the disk
- [ ] no tag I/O happens during `scan()` (assert with a counter in tests)

## Files

`crates/core/src/library/{mod.rs,scan.rs,model.rs}`

## Pitfalls

- Sort with a stable, explicit comparator (case-sensitive byte order, or a
  natural sort for track numbers) rather than relying on readdir order, which
  is arbitrary on ext4.
- Store `mtime`/`size` now: task 12's undo wants to verify a file hasn't changed
  before reversing an operation.
