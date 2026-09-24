# 16 — Tag reading

- **Phase:** M2 · Tag editing
- **Depends on:** 05
- **Status:** not started

## Goal

One unified view of metadata across ID3v2 (mp3) and Vorbis comments (FLAC), so
the rest of the app never branches on container format.

## Details

```rust
struct TagSet {
    title: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    album: Option<String>,
    year: Option<u32>,            // TDRC / DATE, may be a full date
    track: Option<(u32, Option<u32>)>,   // number, total
    disc: Option<(u32, Option<u32>)>,
    genre: Option<String>,
    comment: Option<String>,
    composer: Option<String>,
    // everything we don't model, so writes can preserve it:
    extra: Vec<(String, String)>,
}

struct AudioInfo { duration: Duration, bitrate: u32, sample_rate: u32, channels: u8, format: Format }

fn read(abs: &Utf8Path) -> Result<(TagSet, AudioInfo), TagError>;
fn read_many(paths: &[RelPath], root: &Utf8Path) -> Vec<(RelPath, Result<TagSet>)>;
```

Use `lofty`'s `Probe` so the container is detected by content, not extension —
some of these scene releases will have wrong extensions.

Field mapping notes:

- mp3 → ID3v2 frames: `TIT2` title, `TPE1` artist, `TPE2` album artist,
  `TALB` album, `TDRC`/`TYER` year, `TRCK` `n/total`, `TPOS` disc, `TCON` genre.
  `TCON` may be a numeric genre reference like `(17)` or `17` — resolve to text.
- flac → Vorbis comments: `TITLE`, `ARTIST`, `ALBUMARTIST`, `ALBUM`, `DATE`,
  `TRACKNUMBER`, `TRACKTOTAL`/`TOTALTRACKS`, `DISCNUMBER`, `GENRE`. Field names
  are case-insensitive and **multi-valued** — a FLAC can legitimately have three
  `ARTIST` entries. Represent that: either join with `; ` and remember it was
  multi-valued, or keep `Vec<String>` for the fields where it matters. Decide and
  document; do not silently drop the second value.
- m4a → atoms, via lofty's `ItemKey` abstraction, best-effort.
- A file with no tags at all reads as an empty `TagSet`, not an error.
- A corrupt tag is an error naming the file; the scan continues.

Reading must be lazy and cheap enough to fetch for a screenful of rows (task 22
will call this for ~40 visible files at a time).

## Acceptance criteria

- [ ] reads mp3 and FLAC fixtures and returns the same `TagSet` shape for both
- [ ] ID3v2.3 and ID3v2.4 files both read correctly, including `TYER` vs `TDRC`
- [ ] numeric `TCON` genre resolves to its text name
- [ ] `TRCK` `5/12` yields `(5, Some(12))`; bare `5` yields `(5, None)`
- [ ] a multi-valued FLAC `ARTIST` is preserved, not truncated to the first
- [ ] unknown frames/comments land in `extra` and are round-tripped by task 17
- [ ] a file with no tags returns an empty `TagSet`
- [ ] a truncated/corrupt file returns a typed error naming the path
- [ ] a mislabelled file (mp3 bytes named `.flac`) is detected by content
- [ ] reading tags for 40 files takes < 50 ms warm

## Files

`crates/core/src/tags/{mod.rs,model.rs,read.rs}`

## Pitfalls

- Don't normalize or "clean" values on read. The editor must show exactly what is
  in the file, or the user can't tell what they are fixing.
- `year` is frequently a full `YYYY-MM-DD` in FLAC. Keep the original string in
  `extra` if you narrow it to a `u32`, so a write doesn't destroy the month/day.
