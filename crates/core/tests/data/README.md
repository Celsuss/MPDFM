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

## What is deliberately missing, and where it went

Cases that need a tag *writer* to construct. `lofty` became a dependency of core
in task 16, so these are now built at test time from a copy of a template by
`mpdfm_core::testing::tags`:

| Case | Helper |
| --- | --- |
| a multi-valued FLAC `ARTIST` | `tags::set_multi_valued` |
| a numeric `TCON` genre reference, e.g. `(17)` | `tags::set_frame` |
| an embedded cover, for task 17 | `tags::embed_cover` |
| a truncated / corrupt file | `tags::truncate` |
| an mp3 named `.flac` | `tags::write_as` |

Built rather than committed because each one is a template plus one call, and a
sixth and seventh committed binary would be two more files nobody can diff.
`tags::dump` and `tags::audio_digest` are there too: the before-and-after
comparison task 17 needs, and the audio stream on its own so that "the audio is
bit-identical" can be asserted without the tag — which is supposed to change —
being part of the comparison.
