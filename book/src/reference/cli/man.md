# mpdfm man

```text
Print the mpdfm(1) man page, or write it and one per subcommand to a directory

Usage: mpdfm man [OPTIONS]

Options:
      --dir <DIR>  Write `mpdfm.1` and one `mpdfm-<subcommand>.1` per subcommand into DIR (created if missing) instead of printing `mpdfm.1`
  -h, --help       Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm man | man -l -                       # read it without installing
  mpdfm man --dir ~/.local/share/man/man1    # mpdfm.1 and one page per subcommand
```

