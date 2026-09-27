# 07 — Playlist index: which playlists reference this path?

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 06
- **Status:** done

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

- [x] `refs_to` finds a track referenced from two different playlists
- [x] a track listed twice in one playlist yields two refs
- [x] `refs_under_dir("hiphop/MF DOOM")` excludes `hiphop/MF DOOM Instrumentals/…`
- [x] `broken()` reports exactly the one deliberately broken fixture reference,
      and a CUE virtual track whose sheet is missing is reported under the
      sheet's path (`…Acts 1.flac.cue`) rather than under the `…/track0017`
      spelling, which was never a file — **but see "The known bad entry is not a
      missing file" below: on the real library `broken()` correctly reports 0**
- [x] URLs and comments never appear in `by_path`
- [x] loading a playlist dir containing a dangling symlink warns and continues
- [x] index of the real 17-playlist directory builds in < 50 ms — **0.16 ms warm**
      (0.25 ms cold) for 231 references over 212 distinct paths, release; the
      committed fixture set is 0.45 ms warm, debug

## Files

`crates/core/src/playlist/index.rs` (the index, `Ref` and `IndexWarning`),
`crates/core/src/playlist/mod.rs` (the re-exports),
`crates/core/tests/playlist_index.rs` (the acceptance tests).

No new dependencies.

## Decisions

**The known bad entry is not a missing file.** `PLAN.md` §3 recorded "1 broken
reference out of 231", the CUE virtual track in `Pop.m3u`. Checked read-only
against the real library while building this: the sheet
(`…/CD 1 - Mercury - Acts 1/Imagine Dragons - Mercury - Acts 1.flac.cue`) exists,
it is a multi-`FILE` sheet with 18 tracks, `TRACK 17` is in it, and the file that
track names (`17 - Wrecked (Live From the Bunker).flac`) exists too. Nothing is
absent. All 231 references resolve on disk, and `broken()` correctly reports 0.

So the original acceptance criterion rested on a wrong premise, and the fix is
not to make `broken()` report a file that is there. "The file this line names is
absent" is the only question a *move* has to answer, and it is the one `broken()`
answers. Whether MPD can *play* a virtual track inside a multi-`FILE` sheet is a
different question with a different oracle — MPD's own database, task 13 — and it
belongs to `doctor` (task 29). `cue_refs()` exists to hand it over: the
reference, the sheet it is keyed under, and the `trackNNNN` suffix. One heuristic
was considered and rejected: treating `X.flac.cue` as broken unless `X.flac`
exists. It would have "caught" this entry and been wrong about why — this sheet
never had a single-file image, and never needed one.

**`Ref` is two indices, not two strings.** A reference is a playlist index and a
line index, so it is `Copy`, sorts in the order a person reads a report, and
cannot disagree with the playlist it came from. The cost is that references are
only valid for the index that produced them, which is the right trade here
because the index is rebuilt after every operation anyway — and the accessors
(`playlist`, `entry`) return `Option` rather than panicking, so a reference held
across a reload is a wrong answer that is visible instead of a crash.

**Playlists are indexed in file-name order.** `readdir` order is arbitrary on
ext4, and a `Ref` that meant a different playlist on the next run would make
task 10's preview unreviewable and task 11's journal wrong. Sorting the names
before loading is the whole fix. For the same reason `refs_under_dir` sorts by
path and `broken` sorts by reference: a `HashMap` has no order to inherit.

**`load` returns warnings, never an error.** A directory with one dangling
symlink in it is still a playlist directory; refusing to index the other sixteen
would be the wrong trade, and whether the root is usable at all was already
settled by `Config::require_playlist_dir` (task 04). Every `IndexWarning` means
"this playlist is not in the index", so a caller that must be certain a move
rewrites every reference checks the warnings before committing: a playlist MPDFM
could not read is a playlist it cannot fix.

**A non-playlist file is skipped silently; a non-UTF-8 *playlist* name warns.**
Warning about the `.jpg` someone keeps next to their playlists is noise. A name
that is not valid UTF-8 is skipped either way (safety invariant 8), but when its
lossy rendering still looks like a playlist it is worth a warning, because that
one is a file MPD would load and MPDFM will not. Subdirectories are skipped
outright — MPD ignores them too.

**Containment is not checked here.** The real `Radios.m3u` is a symlink into a
dotfiles repository, which is outside every configured root and still a playlist
that has to be indexed. The guard (safety invariant 5) belongs to the commit
path, on `Playlist::real_path`, where the roots are known — checking it here
would drop the one playlist most likely to be edited.

**`broken` compares against the `Library` model, not the disk.** One walk already
answered the question, and a second `stat` per reference would be 231 syscalls
for a worse answer. Two consequences, both intended: a CUE virtual track counts
as resolved when its `.cue` sheet exists, because the sheet is what the index
keyed it under; and a reference to something the scan skipped — a symlinked
track, a name that is not UTF-8 — is reported as broken, because a file MPDFM
will not move is a file it cannot make promises about.

**The index is consumed to rewrite it.** `into_playlists` hands the playlists
over and drops the index rather than exposing `&mut Playlist`, because the moment
a line changes, `by_path` describes a file that no longer exists in that shape —
and a lookup against a stale index is precisely how a tool rewrites the wrong
line. Reloading is 0.45 ms; there is no cache and no invalidation logic to get
wrong.

**`playlists_touching` tries each path both ways.** A caller planning a move
holds a mix of files and directories, and the two lookups are mutually exclusive
for any given path, so trying both costs nothing and guessing wrong would leave a
playlist out of task 11's backup set.

## Pitfalls

- The playlist directory may contain non-playlist files; ignore by extension.
- MPD resolves playlist paths against `music_directory`, *not* against the
  playlist directory. Never join them the other way around.
- `refs_under_dir` is strictly inside the directory: a path equal to `dir` is a
  file move, and `refs_to` is the lookup for that one. `RelPath::starts_with_dir`
  draws that line in one place, so both agree.
- A `Ref` from an earlier index is stale after any rewrite. Reload; do not keep
  one across an operation.
- `Absolute paths.m3u` contains `~/Music/…`, which *is* a valid `RelPath` — the
  tilde is an ordinary character in a file name. It indexes as a track and shows
  up in `broken`, which is what MPD would make of it too.
