# 04 — Config: mpd.conf discovery and MPDFM's own settings

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 01, 02
- **Status:** not started

## Goal

Work out `music_directory`, `playlist_directory`, the MPD state file and the
daemon address without the user configuring anything, while letting them
override all of it.

## Details

Resolution order for each value, first hit wins:

1. CLI flag (`--music-dir`, `--playlist-dir`)
2. `$XDG_CONFIG_HOME/mpdfm/config.toml` (default `~/.config/mpdfm/config.toml`)
3. mpd.conf, searched at `$XDG_CONFIG_HOME/mpd/mpd.conf`, `~/.mpdconf`,
   `~/.config/mpd/mpd.conf`, `/etc/mpd.conf`
4. hard defaults

The mpd.conf parser needs to handle the real format: `key "value"` pairs,
unquoted values, `#` comments, and `block { ... }` sections which must be
skipped (`audio_output` blocks) rather than confused for top-level keys. Expand
`~`. On this machine the user config gives `~/Music`,
`~/.config/mpd/playlists`, and `bind_to_address 127.0.0.1` / `port 6600`, while
`/etc/mpd.conf` gives different paths — so the search order matters and the
first file found wins rather than merging.

MPDFM config (all optional):

```toml
music_dir = "~/Music"
playlist_dir = "~/.config/mpd/playlists"
mpd_address = "127.0.0.1:6600"          # or a unix socket path
mpd_enabled = true
state_file = "~/.config/mpd/state"
rewrite_saved_queue = true
trigger_update_after_commit = true
delete_enabled = true                   # if false, MPDFM never deletes
backup_keep = 50                        # transactions to retain
organize_template = "{genre}/{albumartist}/{year} - {album}/{track:02} {title}"
```

Emit a `Config` struct plus a `Vec<ConfigWarning>` (e.g. "music_directory from
mpd.conf does not exist"), never a hard failure for a missing optional file.

## Acceptance criteria

- [ ] parses the real `~/.config/mpd/mpd.conf` on this machine and yields
      `~/Music` and `~/.config/mpd/playlists`
- [ ] `audio_output { ... }` blocks do not leak keys into the top level
- [ ] `~` and `$XDG_*` expansion tested
- [ ] missing mpd.conf, missing mpdfm config, and an unreadable file each
      produce a warning, not an error
- [ ] CLI flags override config which overrides mpd.conf
- [ ] `mpdfm config show` (or `scan --json`) prints resolved values and sources
- [ ] a config with an unknown key warns instead of failing

## Files

`crates/core/src/config.rs`, `crates/core/src/mpdconf.rs`,
`docs/config.example.toml`

## Pitfalls

- A nonexistent `music_directory` must be caught early with a clear message;
  every path operation downstream assumes the root exists.
- Resolve the root to an absolute canonical path once, at startup, and pass it
  around. Re-resolving invites TOCTOU-style inconsistency.
