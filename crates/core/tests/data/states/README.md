# Committed MPD state files

Five files reproducing the shapes `~/.config/mpd/state` takes, for task 14's
round-trip property: `write(parse(bytes)) == bytes`, exactly, for all five.
`tests/mpd_state.rs` embeds them with `include_bytes!`.

They are **reproductions, not copies**. The real state file is the user's and is
not in this repository; what is committed here is the same construct — the same
key head that MPD 0.24.0 writes, including the trailing space after
`lastloadedplaylist:` and the two `audio_device_state` lines with no space after
their first colon — with a queue made of the paths in `mpdfm_core::testing::names`
so a test can assert about both a queue line and the file it points at.

| File | What it is there for |
| --- | --- |
| `state` | the everyday case: the real key head, `current: 3`, and a ten-song queue in every container the library holds |
| `empty-queue` | a daemon with nothing queued — the markers are there and nothing is between them, and there is no `current:` line |
| `no-queue` | no `playlist_begin` at all, plus a key from a newer MPD that this build must preserve without understanding |
| `long-format` | a stream in MPD's long format (`song_begin:`, `Time:`, `Title:`, `song_end`) and a `Prio:` line — the continuation lines that belong to the entry above them |
| `no-trailing-newline` | a last line with no terminator, which must come back without one |

## Editing these

`no-trailing-newline` is not what an editor will hand back if you open and save
it. `.gitattributes` stops git from touching any of them;
`the_committed_shapes_survive_in_the_repository` in `tests/mpd_state.rs` fails if
an editor does.

## Where the real format comes from

MPD 0.24.0's `src/queue/QueueSave.cxx` and `src/PlaylistState.cxx`:

- `queue_save` writes `"{index}:"` and then either the bare URI (a plain
  database song) or `song_save`'s multi-line form, followed by `Prio: N` when the
  priority is not zero;
- `playlist_state_save` writes `current: ` followed by
  `queue.OrderToPosition(current)` — a **queue position**, not a song id, which
  is why removing an entry has to renumber it.
