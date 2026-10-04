# 15 — Wire up the M1 CLI end to end

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 04, 05, 07, 10, 11, 12, 13, 14
- **Status:** not started

## Goal

The milestone deliverable: a CLI that can inspect the library and move things
safely, with `--dry-run`, confirmation, and undo. This is what proves the engine
before any UI exists.

## Details

```
mpdfm scan [--json]
    counts by format, album dirs, warnings (non-UTF-8, unreadable, symlinks)

mpdfm doctor [--json]
    broken playlist references (1 expected in this library)
    playlist entries pointing outside music_dir
    audio files referenced by nothing (informational)
    empty album dirs, dirs with no audio
    (the fuller checks land in task 29)

mpdfm move <SRC> <DST> [--dry-run] [--yes] [--merge] [--verify]
    SRC/DST relative to music_dir, or absolute inside it
    prints the Effects preview (task 10 renderer); asks to confirm unless --yes
    refuses on conflicts and exits 2

mpdfm undo [TXID] | undo --list
mpdfm recover
```

Exit codes: `0` success, `1` unexpected error, `2` conflicts blocked the
operation, `3` user declined. Honour `--json` for machine-readable `Effects` and
results. Respect `NO_COLOR` and non-TTY output (no colour, no prompts — require
`--yes`).

Write the integration tests at this level too: a test that drives the actual
binary against a fixture library via `assert_cmd`, because that is the same path
the user takes.

## Acceptance criteria

- [ ] `mpdfm scan` on the real library reports 2 440 mp3 / 364 flac / 5 m4a and
      the aux counts (numbers may drift; assert against a fixture, eyeball the
      real one — re-measured 2026-10-04, flac was 357 when this task was written)
- [ ] `mpdfm doctor` reports exactly the one known broken reference
- [ ] `mpdfm move --dry-run` writes nothing (snapshot before/after)
- [ ] `mpdfm move` then `mpdfm undo` returns a fixture to a byte-identical state
- [ ] conflicts exit 2 and print what conflicted
- [ ] declining the prompt exits 3 and changes nothing
- [ ] non-TTY without `--yes` refuses rather than hanging on a prompt
- [ ] `--json` output parses and contains the same counts as the text output
- [ ] end-to-end test on a *copy* of the user's real playlists: move an album,
      verify all 231 references still resolve, undo, verify byte-identical
- [ ] a guard proves no test touched `~/Music` or `~/.config/mpd`

## Verifying it by hand

Agreed with the user before this task started, because the milestone is the
first one with a binary a person can point at a real library.

**Tests only ever run against a `Fixture`.** The fixture set reproduces the real
library's shapes, so no automated test needs the real one, and the guard
(`testing::real_library_roots`, which refuses `~/Music`, `~/.config/mpd`,
`~/.local/share/mpdfm`) enforces it. Note what that guard does *not* cover: it
lives behind the `testing` feature, so it is not in the loop when the `mpdfm`
binary is run by hand. The binary does what its configuration tells it to.

So, by tier:

1. **Fixture** — every test, and all `move`/`undo`/`recover` exercising.
2. **Read-only against the real library** — `scan`, `doctor`, and
   `move --dry-run` write nothing, and running them against `~/Music` is
   *authorized*. It is the only way to find the name nothing in the fixture set
   anticipated, and to see whether a 27 GB / 2 809-file scan is fast enough to
   sit behind a TUI keystroke.
3. **A real copy, for anything that writes.** Copy one genre directory, not the
   whole 27 GB. **Not `cp -al`**: a hardlink tree looks cheap and is exactly
   wrong here, because a rename is fine through a hardlink but a tag write
   (tasks 17–18) would go straight into the user's real bytes.

Nothing outside a fixture or an explicit copy gets written without asking first.

## What the real library actually holds

Measured read-only on 2026-10-04, so `scan`'s output can be eyeballed against
something rather than just *looked* plausible:

| | | task 05 calls it |
|---|---|---|
| audio | 2 440 mp3, 364 flac, 5 m4a | `Audio` |
| directories | 318, and **no symlinks anywhere under `~/Music`** | — |
| playlists | 17, all `.m3u` (no `.m3u8`); `Radios.m3u` is a symlink into `~/workspace/dotfiles` | — |
| playlist entries | 235, of which 4 are stream URLs → **231 file references** | — |
| cover art | 184 `.jpg`, 34 `.jpeg`, 18 `.png` | `Image` |
| album-internal | 23 `.m3u` **inside** the library | `Playlist` |
| scene clutter | 21 `.nfo`, 23 `.txt`, 4 `.sfv`, 2 `.log`, 2 `.pdf` (booklets), 2 `.sfk` (editor peak files) | `Sidecar` |
| CUE | 4 `.cue` sheets | `Cue` |
| in progress | 2 `.parts` — partial downloads | `Other` |

Every one of those lands in the right `Kind` already: `Kind::from_extension`
knows `jpeg` as well as `jpg`, and the `sfk`/`pdf` spellings; a `.m3u` inside the
library is `Kind::Playlist` and never mistaken for one of MPD's own. So this
table is a **check that was passed**, not a list of gaps — but it is the number
`scan` has to agree with, and the fixture set's choice of shapes was validated by
measuring rather than by assuming.

The two `.parts` are the one genuinely new thing. `Kind::Other` is the right
answer — scanned, counted, moved with its album, never interpreted — and
`doctor` should not have an opinion about them either. Worth a thought while
writing `move`: relocating a file something is still downloading into is a real
way to confuse the downloader, and MPDFM has no way to know. Not a reason to
refuse; possibly a reason for `doctor` to mention a `.parts` that is newer than
everything around it.

## Files

`src/cli/{scan.rs,doctor.rs,move.rs,undo.rs}`, `src/output.rs`,
`tests/cli_move.rs`, `tests/cli_undo.rs`

## Pitfalls

- Confirmation prompts must show the *full* preview, not a summary line. The
  whole design rests on the user seeing which playlist lines change.
- Print the txid on every successful commit so `undo <txid>` is copy-pasteable.
