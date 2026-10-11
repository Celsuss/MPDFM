# mpdfm recover

```text
Finish or roll back a transaction that a crash left pending

Usage: mpdfm recover [OPTIONS] [TXID]

Arguments:
  [TXID]  Transaction to deal with (default: every unfinished one, newest first)

Options:
      --forward  Finish the transaction instead of rolling it back. Rolling back is the default because it is the safe one
      --force    Act on everything that is still safe to act on, skipping and reporting whatever cannot be told apart by looking
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
  mpdfm recover                              # survey, then roll back what a crash left
  mpdfm recover --forward                    # finish it instead
  mpdfm recover 20260924T224500Z-00a3f1

If mpdfm itself will not run, docs/SAFETY.md has the same recovery by hand.
```

