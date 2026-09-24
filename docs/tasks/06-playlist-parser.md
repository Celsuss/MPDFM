# 06 — Byte-preserving m3u parser and writer

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 02, 03
- **Status:** not started

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

- [ ] property test: for all 17 real playlists (copied into a fixture),
      `write(parse(bytes)) == bytes` exactly
- [ ] the CUE virtual track parses into `rel` + `cue` and re-serializes identically
- [ ] radio URLs, `#EXTINF`, `#EXTM3U`, comments and blank lines round-trip
- [ ] CRLF input stays CRLF; a file with no trailing newline keeps none
- [ ] a UTF-8 BOM is preserved
- [ ] writing a symlinked playlist modifies the link target and leaves the
      symlink in place (verify with `symlink_metadata`)
- [ ] an absolute path line becomes `Unparsed` and survives a write untouched
- [ ] the writer is atomic: a simulated failure mid-write leaves the original intact
- [ ] file mode of the original is preserved

## Files

`crates/core/src/playlist/{mod.rs,parse.rs,write.rs}`

## Pitfalls

- MPD's playlist name is the filename without `.m3u`, and it may contain spaces
  (`Coding flow`, `En kall Stockholms natt`). Don't slugify it.
- `.m3u8` may appear; treat it the same but keep the extension.
- Do not "tidy" anything — no sorting, no dedup, no whitespace trimming. The
  parser's job is fidelity, not cleanliness.
