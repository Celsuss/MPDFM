# mpdfm doctor

```text
Report library and playlist health

Usage: mpdfm doctor [OPTIONS]

Options:
      --check <NAME>  Run only this check, or this group of checks; repeatable, or comma-separated
      --full          List every finding instead of the first ten of each check
      --deep          Also find byte-identical audio files. Reads every same-sized file in full, which on a large library is minutes of I/O
  -h, --help          Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm doctor
  mpdfm doctor --check references          # one group of checks
  mpdfm doctor --full                      # every finding, not the first ten
  mpdfm doctor --deep                      # also find byte-identical files (slow)
```

