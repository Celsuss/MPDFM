# 30 — Cover art

- **Phase:** M5 · Extras
- **Depends on:** 16, 17, 22
- **Status:** not started

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

- [ ] `cover show` reports embedded art type, MIME and dimensions, plus loose files
- [ ] `cover set` embeds into every audio file in the album and preserves all
      other tags
- [ ] `cover extract` writes a byte-identical copy of the embedded image
- [ ] `cover remove` removes only the front-cover picture, leaving other pictures
- [ ] all cover operations are undoable via `mpdfm undo`
- [ ] kitty/sixel/iterm detection is conservative; an unknown terminal gets the
      placeholder and prints no escape sequences (assert on captured output)
- [ ] a 5 MB PNG is handled without loading it repeatedly per frame
- [ ] downscaling (if implemented) is opt-in and preserves aspect ratio
- [ ] a FLAC and an mp3 both work

## Files

`crates/core/src/art/{mod.rs,read.rs,write.rs}`,
`src/tui/widgets/image.rs`, `src/cli/cover.rs`

## Pitfalls

- Embedding a 5 MB image into 14 files adds 70 MB to the library. Warn with the
  size delta before committing.
- Cache decoded art per path in the TUI; re-decoding on every frame will be
  visibly slow.
