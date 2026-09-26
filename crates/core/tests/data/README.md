# Committed audio templates

Five tiny real audio files. `mpdfm_core::testing` embeds them with
`include_bytes!` and stamps out copies of them to build a throwaway library, so
every test in the project runs against files that a real tag library can read
and rewrite.

They are **committed rather than generated at test time** on purpose: `cargo
test` must not need `ffmpeg` installed (task 03). `generate.sh` reproduces them
byte-for-byte-ish from ffmpeg — run it only to change a template, and commit the
result.

| File | Container | Tags | Why it exists |
| --- | --- | --- | --- |
| `sine-id3v24.mp3` | mp3 | ID3v2.4, `TRCK 1/15`, `TPOS 1/1` | the common case; number-and-total |
| `sine-id3v23.mp3` | mp3 | ID3v2.3, `TRCK 5/12`, `&` in the album | v2.3 vs v2.4 and `TYER` vs `TDRC` (task 16) |
| `sine.flac` | flac | Vorbis comments, `DATE=2019-03-15` | FLAC path; full date, not a bare year |
| `sine.m4a` | m4a | MP4 atoms | best-effort m4a (PLAN D5) |
| `untagged.mp3` | mp3 | none | must read as an empty `TagSet`, not an error |

All five are a 0.2–0.3 s 440 Hz mono sine — under 2.5 KB each, and audible if
you ever need to check one by ear.

## What is deliberately missing

Cases that need a tag *writer* to construct, and that tasks 16–18 should add
when they gain one (`lofty` is not a dependency of core until then):

- a multi-valued FLAC `ARTIST` (three values in one file)
- a numeric `TCON` genre reference, e.g. `(17)`
- a truncated / corrupt file, and an mp3 renamed `.flac`

The last two need no tag writer at all — `Fixture` can truncate or mislabel a
copy of a template — but the *assertions* about them belong to task 16, so the
helpers live there.
