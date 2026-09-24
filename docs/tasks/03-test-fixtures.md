# 03 — Test fixture library and the real-path guard

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 01, 02
- **Status:** not started

## Goal

A builder that materializes a realistic throwaway music library in a temp
directory, so every later task can be tested against the ugly cases instead of
a clean one. Plus a hard guard that no test can touch the real `~/Music` or
`~/.config/mpd`.

## Details

```rust
let fx = Fixture::builder()
    .album("hiphop", "MF DOOM - Mm..Food (2004) [V0] scene-tag", &["01 Beef Rap.mp3", ...])
    .aux("hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag", &["folder.jpg", "info.nfo", "x.sfv"])
    .multi_disc("pop", "Imagine Dragons - Mercury (2 CD)", &["CD 1 - Acts 1", "CD 2 - Acts 2"])
    .non_ascii_album("electronic", "KREAM - So Hï [c0D2h71bFFI]")
    .flac_album("jazz", "Some Album")
    .playlist("Hip hop.m3u", &[...])
    .playlist_with_urls("Radios.m3u", &[...])       // #EXTM3U, comments, #EXTINF, http URLs
    .symlinked_playlist("Radios.m3u", target_outside_dir)
    .cue_reference("pop", "album.flac.cue/track0017")
    .broken_reference("Pop.m3u", "pop/gone/missing.mp3")
    .state_file_queue(&[...])                        // N:relpath lines
    .build();

fx.music_dir()  fx.playlist_dir()  fx.state_file()  fx.data_dir()
fx.snapshot()   // full recursive digest: paths + file hashes + playlist bytes
```

Real audio files: generate once with `ffmpeg -f lavfi -i "sine=frequency=440:duration=1"`
into mp3/flac/m4a and cache under `target/fixtures/`, or commit ~5 KB samples
under `tests/data/`. They must carry real tags so tag tests (16–18) can use the
same fixture.

`fx.snapshot()` is the workhorse for undo tests: commit → undo → snapshots must
be equal.

## Acceptance criteria

- [ ] `Fixture::build()` produces a tree with: non-ASCII names, spaces,
      brackets, `&`, `+`, an apostrophe, a multi-disc album, aux files, a FLAC
      album, a symlinked playlist, a CUE virtual-track reference, a radio-URL
      playlist, and one deliberately broken reference
- [ ] the fixture is deleted when dropped, and nothing is left in `/tmp` after
      `cargo test`
- [ ] a guard function asserts every path an operation touches is inside the
      fixture; a test proves the guard fires when handed `~/Music`
- [ ] `snapshot()` detects a one-byte change in any audio file or playlist
- [ ] fixture construction takes < 2 s so tests stay fast

## Files

`crates/core/src/testing/mod.rs` (behind a `testing` feature or `#[cfg(test)]`
plus a `dev-dependencies` re-export), `tests/data/`

## Pitfalls

- Build this *before* the move engine, not after. It is the only reason the
  later tasks can be verified rather than hoped about.
- Don't gate on `ffmpeg` being installed at test time if it can be avoided —
  prefer committed tiny fixtures, or skip-with-warning if `ffmpeg` is absent.
