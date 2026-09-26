# 05 — Library scanner

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 02, 03
- **Status:** done

## Goal

Walk `music_directory` and produce an in-memory model of tracks, auxiliary
files and album directories, fast enough to run at startup on ~3 100 files.

## Details

As built. The sketch this task started from keyed `by_dir` on `RelPath`, which
cannot name the library root — a `RelPath` is never empty — and the root is both
where the browser starts and where a stray top-level track lives. `DirPath` is
that key: a `RelPath`, or the root. The index also records each directory's
subdirectories, because a directory that holds only other directories (a genre, a
multi-disc set's root) is invisible to a files-only index and those are exactly
the rows the browser draws first.

```rust
enum Kind { Audio(Format), Image, Cue, Playlist, Sidecar, Other }
enum Format { Mp3, Flac, M4a }

struct Entry { rel: RelPath, kind: Kind, size: u64, mtime: SystemTime }

struct DirPath(Option<RelPath>);                        // a directory, or the root
struct Dir { files: Vec<usize>, subdirs: Vec<DirPath> } // one directory's contents
struct AlbumDir { dir: DirPath, set_root: Option<DirPath>, audio: usize }

struct Library {
    root: Utf8PathBuf,
    entries: Vec<Entry>,              // sorted by rel, byte order
    by_dir: BTreeMap<DirPath, Dir>,   // every directory walked, the root included
    album_dirs: Vec<AlbumDir>,        // derived
    warnings: Vec<ScanWarning>,       // sorted by the path each names
}

enum ScanWarning { NotUtf8, Unnamable, Symlink, Unreadable }

Library::scan(&root)?                                    // the only door in
library.entries()  entry(i)  index_of(&rel)  get(&rel)  len()  is_empty()
library.indices_in(&dir)  files_in(&dir)  subdirs_in(&dir)  dir(&dir)  dirs()
library.album_dirs()  album_dir(&dir)  discs_of(&set_root)
library.counts()  warnings()  root()  dir_count()
library::audio_reads()  library::record_audio_read()     // the no-tag-I/O seam
disc_number("CD 1 - Mercury - Acts 1") == Some(1)
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

- [x] scans the fixture library and classifies every file correctly
- [x] a full scan of a 3 000-file tree completes in well under 1 s warm
      (benchmark it and write the number in this file) — the test's synthetic
      3 000-file, 161-directory tree scans warm in **9.6 ms** debug and
      **3.8 ms** release. The real `~/Music` (3 132 files, 318 directories,
      236 album dirs, 0 warnings): **21.8 ms** debug, **8.2 ms** release warm;
      37 ms for the first scan in a fresh process. Two orders of magnitude of
      headroom, so the cache at `~/.cache/mpdfm/scan.json` that `PLAN.md` §7
      leaves optional is not needed.
- [x] non-UTF-8 filename → `ScanWarning::NotUtf8`, scan continues
- [x] unreadable directory → warning, scan continues
- [x] symlinked directory is not traversed, and is reported
- [x] `album_dirs()` identifies the multi-disc fixture's disc directories and
      links them to the set root
- [x] `by_dir` lookups are used by the browser without re-walking the disk —
      the test deletes the music directory after scanning and then asks the
      model every question the browser asks
- [x] no tag I/O happens during `scan()` (assert with a counter in tests)

## Files

`crates/core/src/library/{mod.rs,scan.rs,model.rs}`,
`crates/core/tests/library.rs`, `crates/core/src/lib.rs`,
`crates/core/Cargo.toml` (`walkdir` is no longer optional — the scanner needs it
in a release build, so only `tempfile` is left behind the `testing` feature)

## Decisions

**`DirPath`, not `RelPath`, keys the index.** See above. `Ord` puts the root
first and is otherwise `RelPath`'s byte order, so `by_dir` iterates a library
top-down and each directory's `subdirs` comes out sorted without a second sort.

**Warnings are data, not errors.** The only failure that stops a scan is a root
that is missing, unreadable or not a directory — every later question is about
the tree under it. An unreadable album, a name that is not UTF-8, a name a
`RelPath` rejects (a backslash is legal on ext4 and illegal in a playlist line), a
symlink: each is a `ScanWarning` naming its path, and the other 3 000 files are
still modelled. Warnings are sorted by that path, because readdir order is
arbitrary on ext4 and a user reads this output.

**A symlink is reported and left out of the model.** Following one risks a loop
(a link to an ancestor) or an escape (a link out of the library, which safety
invariant 5 forbids writing to), and modelling one without following it would
offer task 08 a path it cannot safely move. The one exception is `root` itself,
which `walkdir` follows by default: a `music_directory` that is a symlink is an
ordinary setup, and that link is resolved once, before the walk. A directory whose
*name* cannot be represented is pruned with `skip_current_dir` — MPDFM could not
name anything inside it either, and one warning beats one per file.

**Multi-disc detection is one narrow rule:** `disc_number` — a disc word (`cd`,
`disc`, `disk`) at the start of the directory name, then separators, then digits —
and the parent must be a directory the scan saw and not the root. Structure alone
cannot do it, because a genre directory also holds nothing but album directories;
`jazz/` must not come out as a set root. On the real library this finds 12 disc
directories in 6 sets and nothing spurious. A set root holds no audio of its own,
so it is not an `AlbumDir`; it is reached through `discs_of()`.

**Sort order is explicit: byte order over the canonical path.** Not readdir
order, which is arbitrary on ext4 and would make two scans of an unchanged library
disagree; not case-folded, because the filesystem is case-sensitive (`paths`); not
locale-aware, because that would make the model depend on `$LC_ALL`. The fixture
and the real library both zero-pad track numbers, so byte order reads correctly in
a browser; a natural sort belongs in the *view*, not in the model's identity.

**`audio_reads()` is the seam that keeps "no tag I/O" true.** The counter is
vacuous on its own today — nothing in core opens an audio file yet — so the test
proves the property a second way: it `chmod 000`s every audio file and asserts the
scan still classifies all of them and warns about nothing, which only holds
because the walk never does more than `stat`. Task 16's lazy tag reader has one
obligation, `record_audio_read()` on every file it opens, and the counter half of
that test starts doing real work the moment it exists.

**`Counts`** exists for `mpdfm scan` (task 15) and gives `PLAN.md` §3's inventory
table straight from the model. On the real library it also found the three files
nothing classifies — two yt-dlp `.parts` downloads and an extension-less credits
file — which is what `Kind::Other` is for.

## Pitfalls

- Sort with a stable, explicit comparator (case-sensitive byte order, or a
  natural sort for track numbers) rather than relying on readdir order, which
  is arbitrary on ext4.
- Store `mtime`/`size` now: task 12's undo wants to verify a file hasn't changed
  before reversing an operation.
- `walkdir`'s `metadata()` is `lstat` while `follow_links` is off, which is what
  makes a `chmod 000` file still scannable — and what makes the size and mtime
  the ones of the file itself rather than of a link's target.
- An entry whose mtime cannot be read is reported rather than modelled with
  `UNIX_EPOCH`. A timestamp that is not true would make task 12's precondition
  check pass when it should fail. (Unreachable on Linux; the branch exists so it
  cannot be reached by accident later.)
- The tests that make the filesystem hostile (`chmod 000`, symlinks) restore it
  afterwards so the fixture can still delete itself, and each checks first that
  the hostility took effect — run as root, `chmod 000` stops nothing, and they say
  they were skipped rather than passing vacuously.
