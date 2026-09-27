# Committed playlist fixtures

Seventeen `.m3u` files — the same count as the real library's
`~/.config/mpd/playlists` — reproducing every *shape* that appears in it, plus
the shapes that would break a naive parser. `mpdfm_core::testing` embeds them
with `include_bytes!` and `FixtureBuilder::real_playlists` copies them into a
fixture's playlist directory, where task 06's round-trip property test reads each
one, parses it and writes it back: `write(parse(bytes)) == bytes`, exactly, for
all seventeen.

They are **reproductions, not copies**. The real playlists are the user's and are
not in this repository; what is committed here are the same constructs — the same
album paths as `mpdfm_core::testing::names`, the same CUE virtual track, the same
radio URLs — in files small enough to read in a diff.

| File | What it is there for |
| --- | --- |
| `Coding flow.m3u` | the everyday case: plain relative paths, LF, trailing newline. A name with a space in it, which must not be slugified |
| `Hip hop.m3u` | a blank line and a `#` comment between tracks |
| `Pop.m3u` | `#EXTM3U`, a CUE virtual track, and the one reference that never resolved |
| `Radios.m3u` | the radio playlist: `#EXTINF:-1,Name`, comments, blanks, `http` and `https` URLs |
| `En kall Stockholms natt.m3u` | a name and paths that are not ASCII, including Japanese |
| `Jazz.m3u` | CRLF throughout, with a trailing CRLF |
| `Chill.m3u` | no trailing newline |
| `Bangers.m3u` | a UTF-8 BOM |
| `Mixed bag.m3u8` | the `.m3u8` spelling, and `#PLAYLIST:`/`#EXTVLCOPT:` directives MPDFM has no opinion about |
| `Empty.m3u` | zero bytes |
| `Absolute paths.m3u` | an absolute path, a `./` path and a backslash path — none of them a track identity, all of them preserved |
| `Duplicates.m3u` | the same track twice, which task 07 must report as two references |
| `Whitespace.m3u` | a trailing space after a path, a spaces-only line, a tab-only line, and one stray CRLF line inside an otherwise-LF file |
| `Only comments.m3u` | nothing but a header and comments, including a bare `#` |
| `Cue sheets.m3u` | several CUE virtual tracks, one spelled `.CUE`, and the sheet itself with no track suffix |
| `Windows.m3u` | a BOM, CRLF *and* no trailing newline at once |
| `Saved queue.m3u` | ten tracks in every container the library holds — the shape MPD's saved queue has |

## Editing these

Four of them are not what an editor will hand back if you open and save them:
`Jazz.m3u` and `Windows.m3u` are CRLF, `Bangers.m3u` and `Windows.m3u` start with
a BOM, `Chill.m3u` and `Windows.m3u` have no trailing newline, and `Whitespace.m3u`
has trailing whitespace and one CRLF line among LF ones. `.gitattributes` stops git
from touching them, and `shapes_that_cannot_be_typed_survive_in_the_repository` in
`crates/core/src/testing/playlists.rs` fails if an editor does.
