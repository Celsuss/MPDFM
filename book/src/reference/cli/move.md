# mpdfm move

```text
Move or rename a file or directory, rewriting every reference to it

Usage: mpdfm move [OPTIONS] <SRC> <DST>

Arguments:
  <SRC>  Source, relative to the music dir or absolute inside it
  <DST>  Destination, relative to the music dir or absolute inside it

Options:
      --dry-run  Show the preview and write nothing
  -y, --yes      Skip the confirmation prompt. Required when stdin is not a terminal
      --merge    Let a directory move land in a directory that already exists, moving what does not collide. Nothing is ever overwritten either way
      --verify   Hash every cross-device copy and read it back. Costs a second read of each copied file
  -h, --help     Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm move --dry-run "hiphop/MF DOOM" "hiphop/Doom"    # look first
  mpdfm move "hiphop/MF DOOM" "hiphop/Doom"              # then do it
  mpdfm undo                                               # and take it back
```

