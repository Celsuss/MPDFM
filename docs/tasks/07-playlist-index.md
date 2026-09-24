# 07 — Playlist index: which playlists reference this path?

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 06
- **Status:** not started

## Goal

The lookup that makes safe moves possible: given any file or directory, find
every playlist line that points at it — and find references that are already
broken.

## Details

```rust
struct Ref { playlist: usize, entry: usize }

struct PlaylistIndex {
    playlists: Vec<Playlist>,
    by_path: HashMap<RelPath, Vec<Ref>>,     // exact track path → references
}

impl PlaylistIndex {
    fn load(playlist_dir: &Utf8Path) -> (Self, Vec<Warning>);
    fn refs_to(&self, rel: &RelPath) -> &[Ref];
    fn refs_under_dir(&self, dir: &RelPath) -> Vec<(RelPath, Vec<Ref>)>;  // component-wise prefix
    fn broken(&self, lib: &Library) -> Vec<(Ref, RelPath)>;               // referenced but absent
    fn playlists_touching(&self, paths: &[RelPath]) -> Vec<usize>;
}
```

A CUE reference indexes under the `.cue` file's `RelPath`, so moving the `.cue`
(or its whole album directory) updates the virtual-track line too.

`refs_under_dir` must be component-wise: moving `hiphop/MF DOOM` must not match
`hiphop/MF DOOM Instrumentals`.

One file can be referenced by several playlists (and twice in the same
playlist) — the index returns all of them and task 09 rewrites all of them.

Cheap by design: 17 playlists and 231 entries here, so a full reload per
operation is fine. Don't build a cache until a benchmark says to.

## Acceptance criteria

- [ ] `refs_to` finds a track referenced from two different playlists
- [ ] a track listed twice in one playlist yields two refs
- [ ] `refs_under_dir("hiphop/MF DOOM")` excludes `hiphop/MF DOOM Instrumentals/…`
- [ ] `broken()` on the real library reports exactly the one known bad entry
      (`pop/…Imagine Dragons - Mercury - Acts 1.flac.cue/track0017`)
- [ ] URLs and comments never appear in `by_path`
- [ ] loading a playlist dir containing a dangling symlink warns and continues
- [ ] index of the real 17-playlist directory builds in < 50 ms

## Files

`crates/core/src/playlist/index.rs`

## Pitfalls

- The playlist directory may contain non-playlist files; ignore by extension.
- MPD resolves playlist paths against `music_directory`, *not* against the
  playlist directory. Never join them the other way around.
