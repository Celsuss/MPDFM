# mpdfm config

```text
Show where MPDFM is looking and why

Usage: mpdfm config [OPTIONS] <COMMAND>

Commands:
  show  Print every resolved setting with the source that supplied it
  help  Print this message or the help of the given subcommand(s)

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
  mpdfm config show
```

## mpdfm config show

```text
Print every resolved setting with the source that supplied it

Usage: mpdfm config show [OPTIONS]

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
  mpdfm config show
  mpdfm --music-dir /srv/music config show    # what a flag would change
  mpdfm config show --json
```

