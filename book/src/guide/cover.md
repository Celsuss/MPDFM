# Cover art

```console
$ mpdfm cover show "rock/Album"                # embedded and loose art, and what disagrees
$ mpdfm cover set --dry-run "rock/Album"       # embed the album's cover.jpg in every track
$ mpdfm cover extract "rock/Album"             # or write the embedded one out as cover.jpg
$ mpdfm cover remove "rock/Album"              # remove the embedded front cover
```

- `show` lists embedded and loose art for a track or album directory and what
  disagrees between them.
- `set` embeds an image as the front cover of every track in the album, replacing
  any front cover already there.
- `extract` writes the album's embedded cover out as an image beside its tracks.
- `remove` removes the embedded front cover from every track, leaving any other
  pictures.

Cover art is decoded for JPEG and PNG. In the [TUI](../tui/overview.md) it is
drawn in kitty, iTerm2/WezTerm and sixel terminals, and as a plain placeholder
everywhere else (never inside tmux). The TUI's [cover view](../tui/covers.md)
(`C`) does all four of these per track rather than per album, which is what a
folder of singles needs, and pairs singles with their images by name.

See also the [`cover` reference](../reference/cli/cover.md).
