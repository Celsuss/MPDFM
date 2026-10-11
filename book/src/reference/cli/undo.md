# mpdfm undo

```text
Reverse a committed transaction

Usage: mpdfm undo [OPTIONS] [TXID]

Arguments:
  [TXID]  Transaction to reverse (default: the most recent undoable one)

Options:
      --list   List every transaction and whether it can be undone, and do nothing else
      --force  Reverse everything that is still safe to reverse, skipping and reporting whatever has changed since
  -y, --yes    Skip the confirmation prompt. Required when stdin is not a terminal
  -h, --help   Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm undo --list                         # what can be undone
  mpdfm undo                                # the most recent
  mpdfm undo 20260924T224500Z-00a3f1        # a specific one
```

