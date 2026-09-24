# 09 — Playlist rewriting

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 06, 07, 08
- **Status:** not started

## Goal

Given a set of path moves, update exactly the playlist lines that point at them
and nothing else.

## Details

```rust
struct PathMove { from: RelPath, to: RelPath }

fn plan_playlist_edits(index: &PlaylistIndex, moves: &[PathMove])
    -> Vec<PlaylistEdit>;

struct PlaylistEdit {
    playlist: usize,
    real_path: Utf8PathBuf,
    line_edits: Vec<LineEdit>,      // { entry, old: String, new: String }
}

fn apply(edits: &[PlaylistEdit], backup_dir: &Utf8Path) -> Result<()>;
```

Rules, in order of importance:

1. Rewrite a line only when its `RelPath` **equals** a moved path. No prefix
   matching, no substring replacement, no fuzzy matching. Directory moves are
   expanded into per-file moves by the planner before reaching here.
2. A CUE reference keeps its `/trackNNNN` suffix across the move:
   `pop/old/a.flac.cue/track0017` → `pop/new/a.flac.cue/track0017`.
3. Everything else in the file is byte-preserved (task 06 guarantees this).
4. Back up each affected playlist into the transaction's backup directory
   *before* writing, preserving its name.
5. Write through resolved symlinks (`Radios.m3u`).
6. Deletions: a deleted track's playlist lines are removed, and the preceding
   `#EXTINF` line that belongs to it goes with it. Report the count prominently
   in the preview — this is destructive to the playlist, unlike a move.
7. Two playlists referencing the same moved file: both are rewritten.

## Acceptance criteria

- [ ] moving one track rewrites its line in every playlist that references it
      and leaves every other line byte-identical
- [ ] moving a whole album directory rewrites all of its tracks' lines
- [ ] `hiphop/MF DOOM` move does not touch lines under `hiphop/MF DOOM Instrumentals`
- [ ] CUE virtual-track line is rewritten with its suffix intact
- [ ] radio URLs, `#EXTINF`, comments and blanks are untouched (diff the bytes)
- [ ] the symlinked `Radios.m3u` case: target file modified, symlink preserved
- [ ] deleting a track removes its line *and* its `#EXTINF`, and nothing else
- [ ] backups are written before the first modification and are byte-identical
      to the originals
- [ ] a failure while writing playlist 3 of 5 leaves playlists 1–2 written,
      3–5 untouched, and enough journal state to undo (feeds task 11)
- [ ] integration test: after a commit, every previously resolvable playlist
      entry still resolves to an existing file

## Files

`crates/core/src/playlist/rewrite.rs`

## Pitfalls

- The temptation to do a `str::replace` on the whole file will eventually
  corrupt a playlist where one path is a prefix of another. Match on parsed
  entries only.
- An `#EXTINF` line does not always precede its track; only remove one that
  immediately precedes the removed entry.
