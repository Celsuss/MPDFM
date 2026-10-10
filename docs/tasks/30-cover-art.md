# 30 — Cover art

- **Phase:** M5 · Extras
- **Depends on:** 16, 17, 22
- **Status:** done

## Goal

See, extract and set album art — both embedded and the loose `folder.jpg`-style
files already sitting in 200+ directories of this library.

## Details

**Read/inspect**
- embedded art via `lofty`'s picture API: type (front cover, back, other), MIME,
  dimensions, byte size
- loose image files in the album directory, with a preference order
  (`cover.*`, `folder.*`, `front.*`, then any image)
- report mismatches: embedded art present but no loose file, or vice versa

**Display in the TUI**
Terminal image support is inconsistent, so detect and degrade:
1. Kitty graphics protocol (`$TERM` / `$KITTY_WINDOW_ID`)
2. Sixel, where the terminal advertises it
3. iTerm2 inline images
4. fallback: a bordered placeholder showing dimensions, MIME and size

Detection must be conservative — printing graphics escapes to a terminal that
doesn't support them corrupts the display. Default to the placeholder unless
support is positively identified, with a config override
(`image_protocol = "auto" | "kitty" | "sixel" | "iterm" | "none"`).

**Write**
- `set cover` from a file path or from a loose image in the directory, embedding
  it into every audio file of the album (as front cover, replacing any existing
  front cover)
- `extract cover` to `cover.jpg` in the album directory
- `remove cover` (embedded)
- optionally re-encode/downscale very large art (>1 MB is common in scene
  releases and bloats every file in the album) — opt-in, and note that this
  needs an image crate (`image`), the only heavy dependency in the project

All writes go through `Operation::WriteTags` (task 17) so they are previewed,
journaled and undoable like everything else.

**CLI**
```
mpdfm cover show <PATH>
mpdfm cover set <ALBUM_DIR> <IMAGE>
mpdfm cover extract <ALBUM_DIR> [--name cover.jpg]
mpdfm cover remove <ALBUM_DIR>
```

## Acceptance criteria

- [x] `cover show` reports embedded art type, MIME and dimensions, plus loose files
- [x] `cover set` embeds into every audio file in the album and preserves all
      other tags
- [x] `cover extract` writes a byte-identical copy of the embedded image
- [x] `cover remove` removes only the front-cover picture, leaving other pictures
- [x] all cover operations are undoable via `mpdfm undo`
- [x] kitty/sixel/iterm detection is conservative; an unknown terminal gets the
      placeholder and prints no escape sequences (assert on captured output)
- [x] a 5 MB PNG is handled without loading it repeatedly per frame
- [x] downscaling (if implemented) is opt-in and preserves aspect ratio
- [x] a FLAC and an mp3 both work (and an m4a, and an ID3v2.3 mp3)

## Decisions

**A cover is a tag edit.** `set` and `remove` are a `CoverEdit` inside the
`TagDelta` of an ordinary `Operation::WriteTags`, so they get task 17's whole
machinery for free: preflight, the whole-file backup, the read-back check, the
journal and byte-for-byte undo. The delta names the image by path, size and FNV
hash rather than carrying its bytes (a 5 MB cover journaled once per track would
be 70 MB of JSON), and the write refuses an image whose bytes are not the ones
the preview described. On disk a delta with no cover is still the bare list it
always was, so journals written before this task read back unchanged.

**Extraction is its own operation**, `Operation::ExtractArt` / `FsStep::ExtractArt`
/ `Done::Created`, because it is the one step that creates a file out of nothing.
It never overwrites (published by `link`, which fails on an existing name), and
undoing it removes the file only if it still hashes to what was written — so a
cover the user has replaced since is left alone even under `undo --force`.

**"Front cover" per container.** ID3v2 and FLAC share the 21 picture types and
a cover edit touches only type 3. A FLAC keeps pictures in two places (`PICTURE`
blocks and `METADATA_BLOCK_PICTURE` comments); both are read and both are
cleared. An m4a's `covr` has no type, so every `covr` image is its front cover.
For *reporting*, a picture typed "other" counts as the cover when there is no
front cover — 421 of the real library's pictures are typed that way, and
treating them as no cover made 95 albums "loose only" instead of 18.

**Downscaling** is `cover set --max-dimension PX`: decode, fit inside PX×PX with
the aspect ratio kept, re-encode as JPEG q90, and stage the result in
`$data_dir/art/<hash>.jpg` so the delta can name it. Without the flag the image
is embedded untouched, and an image over 1 MB gets a hint. `image` is pinned to
`=0.25.6` with only the `jpeg` and `png` features: no rayon, no other codecs.

**TUI.** Detection reads environment variables only — never a terminal query,
whose answer would arrive in the middle of the key stream. kitty uses its Unicode
placeholders, so the image lives in the cells and disappears when anything is
drawn over them; its row/column diacritics were read out of the local `kitten`
binary. Sixel and iTerm2 are written after the frame at the pane's position, over
cells marked skip, and only when the picture, the area or the terminal size
changes, and never while an overlay is open. Decoding is on a worker, once per
entry (shrunk to 512 px); the pane-sized payload is encoded once per size and
cached.

## Measured against the real library (read-only)

`cover show` over all 236 album directories: 3.2 s in total, every one exit 0,
one track whose tags cannot be read (reported in its own slot). 2 299 embedded
pictures: 1 876 front covers, 421 typed "other", one "bright coloured fish" and
one "publisher logo"; 2 153 JPEG and 146 PNG; **57 over 1 MB**. Mismatches: 27
albums embed a cover with no image beside them, 18 have an image no track embeds,
12 embed different covers on different tracks, 2 have some tracks with none.

Nothing was written to the library. `set`, `extract` and `remove` were exercised
against fixtures and a scratch copy only.

## Files

`crates/core/src/art/{mod.rs,read.rs,write.rs}`,
`src/tui/widgets/image.rs`, `src/cli/cover.rs`

Also touched: `tags/write.rs` (`TagDelta::cover`, the cover in preflight, write
and read-back), `ops/{op,exec_fs,plan,commit,effects,render}.rs` (`ExtractArt`,
`Done::Created`, `Summary::files_created`, the `cover` TAG row), `journal/{undo,
recover,record}.rs`, `config.rs` (`image_protocol`), `testing/tags.rs`
(`embed_picture`, `jpeg`, `png`), the TUI's app, worker and terminal seam, and
the tests `crates/core/tests/art.rs` and `tests/cli_cover.rs`.

## Pitfalls

- Embedding a 5 MB image into 14 files adds 70 MB to the library. Warn with the
  size delta before committing.
- Cache decoded art per path in the TUI; re-decoding on every frame will be
  visibly slow.
