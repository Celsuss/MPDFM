# mpdfm organize

```text
Re-file tracks into template-derived paths, rewriting every reference

Usage: mpdfm organize [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  Files or directories to organize (default: the whole library)

Options:
  -t, --template <TEMPLATE>  Path template (default: `organize_template` from the config), e.g. "{genre}/{albumartist}/{year} - {album}/{track:02} {title}"
      --dry-run              Show the preview and write nothing
  -y, --yes                  Skip the confirmation prompt. Required when stdin is not a terminal
      --only-missing         Only move files whose tags are complete for the template — no `{x?}` segment left out, no `{x|default}` used — and that are not already in place. The incremental way to work through a large library
      --no-aux               Move the tracks only; cover art, .nfo, .cue and other aux files stay where they are
      --limit <N>            Stage at most N tracks' worth of moves, for a cautious first run
      --verify               Hash every cross-device copy and read it back. Costs a second read of each copied file
  -h, --help                 Print help

Global options:
      --music-dir <DIR>     Music library root; overrides config and mpd.conf
      --playlist-dir <DIR>  Directory holding MPD's .m3u playlists
      --config <FILE>       MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml)
      --no-mpd              Never talk to the MPD daemon
  -v, --verbose...          Print more detail; repeat for more still
      --json                Machine-readable JSON output

Examples:
  mpdfm organize --dry-run --limit 20                       # a cautious first look
  mpdfm organize --dry-run rock/ -t "{albumartist}/{album}/{track:02} {title}"
  mpdfm organize --only-missing                             # work through it in stages
```

