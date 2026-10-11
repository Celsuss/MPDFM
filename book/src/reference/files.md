# Files and exit codes

## Where MPDFM keeps things

| Path | Holds |
| --- | --- |
| `~/.config/mpdfm/config.toml` | settings (all optional) |
| `~/.config/mpdfm/keys.toml` | key bindings (all optional) |
| `~/.local/share/mpdfm/journal/` | one record per transaction |
| `~/.local/share/mpdfm/backups/` | playlists, the state file, and deleted or retagged originals |

`XDG_CONFIG_HOME` and `XDG_DATA_HOME` are respected.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | success |
| `1` | unexpected error |
| `2` | refused before anything was written (a conflict, or the library changed since the preview) |
| `3` | you declined at the prompt |

Commands that change the library ask first. Pass `--yes` to skip the prompt, which
is required when stdin is not a terminal.
