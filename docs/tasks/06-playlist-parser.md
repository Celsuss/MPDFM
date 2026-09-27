# 06 — Byte-preserving m3u parser and writer

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 02, 03
- **Status:** done

## Goal

Read an MPD playlist into a model that can be written back **byte-identically**,
so that rewriting one line never disturbs the other 59.

## Details

```rust
enum Entry {
    Blank,
    Comment(String),                                  // "# Liquid Drum & Bass"
    ExtM3u,                                           // "#EXTM3U"
    ExtInf { duration: i64, title: String, raw: String },
    Url(String),                                      // http:// https:// (and any scheme)
    Track { rel: RelPath, cue: Option<String>, raw: String },
    Unparsed(String),                                 // anything we don't understand
}

struct Playlist {
    path: Utf8PathBuf,          // the .m3u as found (may be a symlink)
    real_path: Utf8PathBuf,     // symlink resolved — this is what we write
    name: String,               // "Coding flow" (MPD's playlist name)
    entries: Vec<Entry>,
    line_ending: LineEnding,    // preserve LF vs CRLF
    trailing_newline: bool,
    bom: bool,
}
```

Real cases from this library that must round-trip:

- `#EXTM3U` header, `# Lofi / Downtempo` comment, `#EXTINF:-1,SomaFM - Groove Salad`,
  `http://ice1.somafm.com/groovesalad-256-mp3`
- plain relative tracks: `coding-music/SwitchAngel/Coding_Trance.mp3`
- a CUE virtual track: `pop/…/Imagine Dragons - Mercury - Acts 1.flac.cue/track0017`
  → `rel` = the `.cue` file, `cue` = `Some("track0017")`
- `Radios.m3u` is a **symlink** to `~/workspace/dotfiles/mpd/playlists/Radios.m3u`;
  resolve it and write the target, never replace the link with a regular file

CUE detection rule: if a path component ending in `.cue` (case-insensitive) is
followed by exactly one more component, treat the tail as the virtual track id.
Keep `raw` so serialization is exact regardless.

Writer: serialize to a temp file in the same directory as `real_path`, `fsync`,
then `rename`. Preserve the original file mode. Never write through a symlink by
truncation.

`Unparsed` exists so that an absolute path or an unrecognized line is preserved
untouched rather than dropped — dropping a line the user wrote would be a data
loss bug.

## Acceptance criteria

- [x] property test: for all 17 real playlists (copied into a fixture),
      `write(parse(bytes)) == bytes` exactly — the seventeen committed
      reproductions in `crates/core/tests/data/playlists/` are copied into a
      fixture by `FixtureBuilder::real_playlists` and round-tripped both in memory
      and through the disk. Also run read-only over the **actual**
      `~/.config/mpd/playlists`: 17 files, **231 track entries** (1 of them a CUE
      virtual track), 4 URLs, 4 `#EXTINF`, 2 comments, 1 blank, 1 `#EXTM3U`, **0
      unparsed lines and 0 round-trip failures**, in 2.5 ms debug / 0.17 ms
      release. The 231 matches `PLAN.md` §3's inventory exactly.
- [x] the CUE virtual track parses into `rel` + `cue` and re-serializes identically
- [x] radio URLs, `#EXTINF`, `#EXTM3U`, comments and blank lines round-trip
- [x] CRLF input stays CRLF; a file with no trailing newline keeps none
- [x] a UTF-8 BOM is preserved
- [x] writing a symlinked playlist modifies the link target and leaves the
      symlink in place (verify with `symlink_metadata`)
- [x] an absolute path line becomes `Unparsed` and survives a write untouched
- [x] the writer is atomic: a simulated failure mid-write leaves the original
      intact — `Stop::BeforeRename` fails the write after the temp file is written
      and `fsync`ed, and the original is byte-identical with no temp file left
      behind. The unwritable-directory case is tested too
- [x] file mode of the original is preserved

## Files

`crates/core/src/playlist/{mod.rs,parse.rs,write.rs}`,
`crates/core/src/lib.rs` (`Error::Playlist`),
`crates/core/tests/playlist.rs` (the acceptance tests),
`crates/core/tests/data/playlists/` (seventeen committed fixture playlists, a
`README.md` describing each and a `.gitattributes` that stops git from
"helpfully" normalizing them),
`crates/core/src/testing/playlists.rs` and `FixtureBuilder::real_playlists`
(embedding them and copying them into a fixture).

No new dependencies.

## Decisions

**One `LineEnding` per file, and a mixed file is LF.** `Crlf` only when the file
has at least one terminator and *every* one is `\r\n`; otherwise `Lf`. A file that
mixes them — `Whitespace.m3u` in the fixture set is one — is read as LF, and the
`\r` of the odd line stays inside that line's text, where it makes the line
`Unparsed`. That keeps the round trip exact without a per-line ending the model
would then have to carry everywhere, and it never invents a `RelPath` with a
control byte in it. The alternative considered and rejected was refusing to parse
a mixed file at all: it is safe, but it would stop `mpdfm move` from updating a
playlist it could have updated perfectly well.

**A whitespace-only line is `Unparsed`, not a track.** Spaces are a legal file
name on ext4, so `pop/a.mp3 ` with a trailing space *is* a track — dropping the
space would point the line at a different file. But a line of nothing but spaces
or tabs is an accident, and reading it as a track would put a path of spaces in
the index (task 07) and have `doctor` report it as a missing file for ever.

**`#EXTM3U` is matched exactly; everything else `#` is a comment.** It is the one
string the writer produces from nothing, so `#extm3u`, `#EXTM3U ` and `#EXTM3Ux`
must not become it — they would be rewritten into the canonical spelling behind
the user's back. Same reasoning for `#EXTINF`: a duration that is not an integer,
or a line with no comma, stays a `Comment`, which round-trips perfectly well.

**`RelPath::parse` is what decides whether a line is a track.** It rejects
absolute paths, `./`, `..`, doubled separators, backslashes and NULs, and task 02
guarantees that what it accepts renders back byte-identically. So `Unparsed` is
exactly "not a track identity", `Track`'s `raw` can be rebuilt from its parts
(`Entry::track`, which is how task 09 rewrites a line without desyncing the bytes
from the fields), and a test asserts that equality for every parsed track.

**The writer refuses a `real_path` that is still a symlink.** `Playlist::load`
resolves the link, so a symlink reaching the writer means the playlist came from
`Playlist::from_bytes` — a caller bug. Resolving it there instead would be
friendlier and wrong: the caller checks containment (safety invariant 5) on
`real_path`, and quietly following a link past that check is how a tool ends up
writing outside the configured roots.

**The temp file is `.<name>.mpdfm-<pid>.<n>.tmp`, opened with `create_new`.**
Hidden and not a playlist extension, because MPD reads this very directory and
would otherwise offer the user a playlist called `.Radios.m3u.mpdfm-1234.0`;
`create_new` so a write can never truncate something that matters; a sibling of
the target so the `rename` is atomic rather than a cross-filesystem copy. The
directory is `fsync`ed after the rename, and a failure *there* is ignored — the
rename has already happened, and reporting an error would tell the caller a lie.

**`ParseError` has one variant.** Bytes that are not UTF-8 are reported with the
offset of the first bad byte and nothing else is rejected: every other oddity in a
playlist is preserved instead. Non-UTF-8 is where MPDFM stops (safety invariant 8)
because an m3u whose encoding it guessed at would be rewritten into a file the
user never wrote.

**The fixture playlists are reproductions, not copies.** The real playlists are
the user's and do not belong in the repository; the committed seventeen have the
same count, the same shapes and the same album paths as
`mpdfm_core::testing::names`, and the real directory is what the numbers above
were measured against. A unit test asserts the four shapes an editor would
silently "fix" (CRLF, BOM, no trailing newline, trailing whitespace) are still
in the committed bytes.

## Pitfalls

- MPD's playlist name is the filename without `.m3u`, and it may contain spaces
  (`Coding flow`, `En kall Stockholms natt`). Don't slugify it.
- `.m3u8` may appear; treat it the same but keep the extension.
- Do not "tidy" anything — no sorting, no dedup, no whitespace trimming. The
  parser's job is fidelity, not cleanliness.
- The `.cue` suffix test must not slice the last four *bytes* off a name: a
  directory ending in `ノスタルジア` has no character boundary there, and indexing
  would panic. `str::get` returns `None` instead.
- A `Playlist` with every entry removed writes an **empty file**, not a lone
  newline, even though `trailing_newline` is still true.
- `Playlist::load` resolves the symlink *before* reading, so a dangling
  `Radios.m3u` is reported as the link's own path rather than as a missing file
  somewhere in a dotfiles repo.
- Permissions are copied onto the temp file before the rename. Without that, a
  `0600` playlist would come back as whatever the process umask says.
