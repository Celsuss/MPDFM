# Working with MPD

MPD is **optional** and never fatal.

- **Configuration.** MPDFM reads `music_directory`, `playlist_directory` and the
  state file from your `mpd.conf`. See [Configuration](../reference/config.md) for
  the order in which sources are consulted.
- **MPD running.** After a commit MPDFM asks it to rescan, and warns about moved
  files that are in the live queue. It speaks a minimal protocol client, just
  enough for `update` and status.
- **MPD not running** (or `--no-mpd`). MPDFM rewrites the saved queue in MPD's
  state file itself.

Either way, playlists in the playlist directory are rewritten in the same
transaction as the move.
