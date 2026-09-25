# 02 — `RelPath`: the canonical track identity

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 01
- **Status:** done

## Goal

A newtype that makes it impossible to write a malformed path into a playlist,
plus the normalization and containment rules every other module relies on.

## Details

`RelPath` wraps a `Utf8PathBuf` (camino) that is guaranteed:

- relative, never absolute
- `/`-separated, no backslashes
- no `.` or `..` components
- no leading `./`, no trailing `/`, no empty or repeated separators
- valid UTF-8 by construction

Constructors:

```rust
RelPath::parse(s: &str) -> Result<RelPath, PathError>          // from a playlist line
RelPath::from_abs(abs: &Utf8Path, root: &Utf8Path) -> Result<RelPath, PathError>
RelPath::from_abs_os(abs: &Path, root: &Utf8Path) -> Result<RelPath, PathError>
impl Display for RelPath   // exactly the bytes to write back into an m3u
```

Helpers: `parent()`, `file_name()`, `extension()`, `starts_with_dir(&RelPath)`
(component-wise, so `hiphop/MF` does **not** match `hiphop/MFDOOM`),
`reparent(from, to)` for computing a moved path, and `to_abs(root)`.

Also in this module: `contains(root, candidate) -> bool` which resolves symlinks
and `..` before checking that an absolute path really lives under a root. This
is the guard for safety invariant 5.

Non-UTF-8 paths: `from_abs` returns `PathError::NotUtf8 { lossy: String }`. The
scanner collects these as warnings and skips them; MPDFM never guesses an
encoding.

## Acceptance criteria

- [x] `RelPath::parse` rejects `/abs`, `../x`, `./x` (rejected, not normalized —
      see Decisions), `a//b`, `a/`, `""`, and backslash separators
- [x] round-trip: for every valid input, `parse(s).to_string() == s`
- [x] `starts_with_dir` is component-wise: `hiphop/MF DOOM` does not match
      `hiphop/MF DOOM Extra/`
- [x] `reparent` maps `a/b/c.mp3` under move `a/b → x/y` to `x/y/c.mp3`
- [x] `contains` rejects a symlink inside the root that points outside it
- [x] `from_abs_os` on a non-UTF-8 path returns `NotUtf8` and never panics
- [x] unit tests include the real non-ASCII names from this library
      (e.g. `electronic/kream/KREAM - So Hï [c0D2h71bFFI].mp3`)

## Decisions

**A leading `./` is rejected, not normalized.** Every string `parse` accepts
therefore renders back byte-identically, with no exception to the round-trip
invariant. A playlist line spelled `./pop/a.mp3` is not a track line as far as
the task 06 parser is concerned, so it survives byte-for-byte under safety
invariant 4 rather than being silently respelled.

**`from_abs` is split in two.** As specced it can never return `NotUtf8`: both
of its parameters are already `&Utf8Path`, so the encoding failure happened
before the call. `from_abs` is kept verbatim for callers that already hold UTF-8;
`from_abs_os(&Path, &Utf8Path)` owns the `NotUtf8 { lossy }` case and is what the
scanner (task 05) calls on `walkdir` output.

**`contains` fails closed.** It returns `bool`, so every error — an
unresolvable root, a relative candidate, a `.`/`..` in a not-yet-existing tail —
answers *not contained*. A non-existent candidate is still resolved (its longest
existing ancestor is canonicalized and the tail re-appended) because move
destinations do not exist yet. Comparison happens on `std::path::Path`, so a
non-UTF-8 component above the root cannot skew the answer.

**Extra rejection: `InteriorNul`.** No filesystem accepts a NUL byte, and a
playlist line carrying one is corrupt rather than merely unusual.

## Files

`crates/core/src/paths.rs`, `crates/core/src/lib.rs` (`Error::Path`),
`crates/core/Cargo.toml` (`camino`, dev-dep `tempfile`)

## Pitfalls

- Case sensitivity: do **not** lowercase for comparison. ext4 is
  case-sensitive, so `Artist/` and `artist/` are different directories. Case
  folding belongs only in the collision *warning* logic of tasks 08 and 27.
- Unicode normalization: NFC vs NFD can make two visually identical names
  different byte strings. Compare bytes, but have `doctor` (task 29) report
  suspected NFC/NFD near-duplicates.
