# mpdfm cover

```text
See, extract and set album art

Usage: mpdfm cover [OPTIONS] <COMMAND>

Commands:
  show     Show embedded and loose art for a track or an album directory, and what disagrees between them
  set      Embed an image as the front cover of every track in an album, replacing any front cover already there
  extract  Write the album's embedded cover out as an image file beside its tracks
  remove   Remove the embedded front cover from every track in an album, leaving any other pictures
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm cover show "rock/Album"
  mpdfm cover set --dry-run "rock/Album"
```

## mpdfm cover show

```text
Show embedded and loose art for a track or an album directory, and what disagrees between them

Usage: mpdfm cover show [OPTIONS] <PATH>

Arguments:
  <PATH>  A track, or an album directory

Options:
  -h, --help  Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm cover show "rock/Album"
  mpdfm cover show "rock/Album/01.flac" --json
```

## mpdfm cover set

```text
Embed an image as the front cover of every track in an album, replacing any front cover already there

Usage: mpdfm cover set [OPTIONS] <ALBUM_DIR> [IMAGE]

Arguments:
  <ALBUM_DIR>  The album directory
  [IMAGE]      The image: a path, or the name of an image in the album directory. Default: the album's own cover.jpg, folder.jpg or front.jpg, in that order of preference

Options:
      --max-dimension <PX>  Re-encode an image larger than PX on its longer side as a JPEG that fits, keeping its aspect ratio. Off unless given: re-encoding is lossy
      --dry-run             Show the preview and write nothing
  -y, --yes                 Skip the confirmation prompt. Required when stdin is not a terminal
  -h, --help                Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm cover set --dry-run "rock/Album"                  # uses cover.jpg / folder.jpg / front.jpg
  mpdfm cover set "rock/Album" scans/front.png --max-dimension 1000
```

## mpdfm cover extract

```text
Write the album's embedded cover out as an image file beside its tracks

Usage: mpdfm cover extract [OPTIONS] <ALBUM_DIR>

Arguments:
  <ALBUM_DIR>  The album directory

Options:
      --name <NAME>  File name to write in the album directory (default: cover.jpg, or cover.png for a PNG)
      --dry-run      Show the preview and write nothing
  -y, --yes          Skip the confirmation prompt. Required when stdin is not a terminal
  -h, --help         Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm cover extract --dry-run "rock/Album"
  mpdfm cover extract "rock/Album" --name folder.jpg
```

## mpdfm cover remove

```text
Remove the embedded front cover from every track in an album, leaving any other pictures

Usage: mpdfm cover remove [OPTIONS] <ALBUM_DIR>

Arguments:
  <ALBUM_DIR>  The album directory

Options:
      --dry-run  Show the preview and write nothing
  -y, --yes      Skip the confirmation prompt. Required when stdin is not a terminal
  -h, --help     Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm cover remove --dry-run "rock/Album"
```

