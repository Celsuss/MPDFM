# 14 — MPD saved queue in the state file

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 06, 11, 13
- **Status:** not started

## Goal

Keep MPD's *saved queue* working across a move. This is the reference store
people forget about: `~/.config/mpd/state` on this machine holds 61 queue
entries as `N:relative/path` lines, and a plain `mv` silently breaks all of them
the next time MPD restarts.

## Details

The state file is a flat key/value text file with a queue section:

```
state: play
current: 26
lastloadedplaylist:
playlist_begin
0:electronic/kream/KREAM - So Hï [c0D2h71bFFI].mp3
1:electronic/Lost_Frequencies_…/05_….mp3
…
playlist_end
```

Implement a parser with the same fidelity contract as task 06: everything that
is not a rewritten queue line is preserved byte-for-byte, including unknown keys
and `audio_device_state` lines.

```rust
struct MpdState { lines: Vec<StateLine>, /* … */ }
enum StateLine { Other(String), QueueEntry { index: u32, rel: RelPath, raw: String } }

fn rewrite(state: &mut MpdState, moves: &[PathMove]) -> Vec<LineEdit>;
```

Safety rules specific to this file:

- **MPD overwrites the state file on shutdown from memory.** So rewriting it
  while MPD is running achieves nothing — the daemon will clobber it. Therefore:
  - If MPD is **not** running → rewrite the file directly.
  - If MPD **is** running → the live queue in memory is what matters. Detect
    whether any moved file is in the current queue (`queue_paths`, task 13) and
    warn the user in the preview: those entries will point at nothing until they
    requeue. Optionally offer to also rewrite the on-disk file for the case where
    MPD is stopped before it next saves — but document that MPD will overwrite it.
  - Decide and document the default. Recommended default:
    `rewrite_saved_queue = true`, applied only when MPD is not reachable, plus a
    warning when it is.
- Deleted tracks: remove their queue lines and renumber the remaining indices
  consecutively, and adjust `current:` if it pointed at or after a removed entry.
- Back up the state file into the transaction backup dir (task 11) and restore
  it wholesale on undo.

## Acceptance criteria

- [ ] parse → serialize of the real state file is byte-identical
- [ ] a move rewrites only the matching queue lines; `state:`, `current:`,
      `audio_device_state` and unknown keys are untouched
- [ ] removing an entry renumbers subsequent indices and fixes `current:`
- [ ] when MPD is reachable, the preview warns for each moved file present in
      the live queue, naming them
- [ ] when MPD is not reachable, the file is rewritten and backed up
- [ ] undo restores the state file exactly
- [ ] `rewrite_saved_queue = false` skips the file entirely
- [ ] a state file with no `playlist_begin` section is handled without error

## Files

`crates/core/src/mpd/state.rs`

## Pitfalls

- Do not rewrite the state file while MPD is running and then claim success; the
  daemon's in-memory queue wins at shutdown. The honest behaviour is to warn.
- `current:` is an index into the queue, not a song id. Check MPD's actual
  semantics against a real file before renumbering.
