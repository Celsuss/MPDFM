# 02 — `RelPath`: the canonical track identity

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 01
- **Status:** not started

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

- [ ] `RelPath::parse` rejects `/abs`, `../x`, `./x` (or normalizes `./x` — pick
      one and document it), `a//b`, `a/`, `""`, and backslash separators
- [ ] round-trip: for every valid input, `parse(s).to_string() == s`
- [ ] `starts_with_dir` is component-wise: `hiphop/MF DOOM` does not match
      `hiphop/MF DOOM Extra/`
- [ ] `reparent` maps `a/b/c.mp3` under move `a/b → x/y` to `x/y/c.mp3`
- [ ] `contains` rejects a symlink inside the root that points outside it
- [ ] `from_abs` on a non-UTF-8 path returns `NotUtf8` and never panics
- [ ] unit tests include the real non-ASCII names from this library
      (e.g. `electronic/kream/KREAM - So Hï [c0D2h71bFFI].mp3`)

## Files

`crates/core/src/paths.rs`

## Pitfalls

- Case sensitivity: do **not** lowercase for comparison. ext4 is
  case-sensitive, so `Artist/` and `artist/` are different directories. Case
  folding belongs only in the collision *warning* logic of tasks 08 and 27.
- Unicode normalization: NFC vs NFD can make two visually identical names
  different byte strings. Compare bytes, but have `doctor` (task 29) report
  suspected NFC/NFD near-duplicates.
