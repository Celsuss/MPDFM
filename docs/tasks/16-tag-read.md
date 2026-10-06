# 16 — Tag reading

- **Phase:** M2 · Tag editing
- **Depends on:** 05
- **Status:** done

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

  **Decided: an ordered list, in every text field.** `tags::Values` is a
  `Vec<String>`, empty for an absent field, and `title`/`artist`/`album_artist`/
  `album`/`date`/`genre`/`comment`/`composer` are all one. Joining with `; ` and
  remembering the flag needs the same two pieces of information and then has to
  guess which semicolons were separators when it writes them back; a value that
  genuinely holds a semicolon reads as one value here and only a *write* splits
  on the separator, because only then did a user type it. `Values::joined` is the
  one-line rendering for an edit box, so the TUI (task 23) still shows one row per
  field. 16 of the 2 808 real files are multi-valued, so this is not a theoretical
  case.
- m4a → atoms, via lofty's `ItemKey` abstraction, best-effort.
- A file with no tags at all reads as an empty `TagSet`, not an error.
- A corrupt tag is an error naming the file; the scan continues.

Reading must be lazy and cheap enough to fetch for a screenful of rows (task 22
will call this for ~40 visible files at a time).

**The year keeps its original string.** `TagSet::date` is a `Values` holding
exactly what the file says — `2004`, `2019-03-15`, `MMIV` — and `TagSet::year()`
narrows it to a `u32` on demand. Nothing stores the narrowed form, so no write
can destroy the month and day, which is what the pitfall below asks for without
the indirection of stashing the original in `extra`.

**A field means the same thing to the reader and to the writer.** Found the hard
way in task 19, against a real album: ID3v2 tells several `COMM` frames apart by
their description, and `01.Smokin' On.mp3` has three — `COMM:` holding
`vtwin88cube`, `COMM:Catalog Number` and `COMM:MusicMatch_Preference`. Reading all
three as "the comment" made `--clear comment` unable to do what it said, because
the writer only touches the frame the spec calls the comment: the one with an
empty description. So that is what `TagSet::comment` is, and a described `COMM` is
an `extra` like any other frame MPDFM does not model. (Task 17's read-back check
is what caught it rather than letting it ship.)

**`extra` is read from the native tag, not from `lofty`'s generic one.** The ten
modeled fields come through `SplitTag` — that is where `TRCK 5/12` becomes a
number and a total, where `(17)` becomes `Rock`, where a v2.3 `TYER` has already
been upgraded to `TDRC`, and where a v2.4 multi-value frame has already been
split, and re-deriving any of it would be re-deriving it worse. `extra` instead
walks the frame list, the comment list or the atom list, because the generic tag
synthesizes items that are not in the file (a FLAC's vendor string arrives as
`EncoderSoftware`) and drops the native spelling of the ones that are. `extra`
exists to show the user what is in their file, so it is read from the file.
Binary items — artwork, `POPM`, `GEOB` — are deliberately not listed: there is no
string to show, and a write preserves them regardless.

## Acceptance criteria

- [x] reads mp3 and FLAC fixtures and returns the same `TagSet` shape for both
- [x] ID3v2.3 and ID3v2.4 files both read correctly, including `TYER` vs `TDRC`
- [x] numeric `TCON` genre resolves to its text name
- [x] `TRCK` `5/12` yields `(5, Some(12))`; bare `5` yields `(5, None)`
- [x] a multi-valued FLAC `ARTIST` is preserved, not truncated to the first
- [x] unknown frames/comments land in `extra` and are round-tripped by task 17
- [x] a file with no tags returns an empty `TagSet`
- [x] a truncated/corrupt file returns a typed error naming the path
- [x] a mislabelled file (mp3 bytes named `.flac`) is detected by content
- [x] reading tags for 40 files takes < 50 ms warm

## Hand-verified against the real library

Read-only, as `docs/PLAN.md` §8 allows: **2 808 of 2 809 audio files read**, in
3.1 s with a debug build (1.1 ms each, so the 40-row budget is met with room to
spare). The one failure is honest and worth keeping:
`.../Madvillain - Money Folder (Remix).mp3` is an **ADTS AAC stream named
`.mp3`**, which MPD plays happily and which content detection catches. It comes
back as `TagError::Unsupported` naming the file and what the bytes look like,
rather than as a fourth `Format`: `docs/PLAN.md` D5 scopes v1 to mp3 and FLAC
with m4a for free, and a file like this belongs in `doctor`'s report (task 29),
not in the tag model.

16 files carry a multi-valued field. The most common `extra` keys are `TPUB`
(866), `TENC` (579), `TCOP` (558), `TSSE` (543), `TSRC` (497) and `TIT1` (409),
plus 126 `TXXX:REPLAYGAIN_TRACK_GAIN` — which is the set task 17 must leave
untouched, and the reason it is read and shown rather than ignored.

## Files

`crates/core/src/tags/{mod.rs,model.rs,read.rs}`,
`crates/core/tests/tags_read.rs`

Also touched: `lib.rs` (`Error::Tag`), `library/model.rs` (`Format` gained
`serde`), `crates/core/Cargo.toml` (`lofty` becomes a dependency of core), and
`testing/tags.rs` — the fixture helpers `crates/core/tests/data/README.md` said
tasks 16–18 would need once there was a tag writer: a multi-valued FLAC comment,
a raw `TCON` frame, an embedded cover, a mislabelled copy of a template, a
truncated file, plus `dump` and `audio_digest` for task 17's before-and-after
comparisons.

## Pitfalls

- Don't normalize or "clean" values on read. The editor must show exactly what is
  in the file, or the user can't tell what they are fixing.
- `year` is frequently a full `YYYY-MM-DD` in FLAC. Keep the original string in
  `extra` if you narrow it to a `u32`, so a write doesn't destroy the month/day.
