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

- [ ] `mpdfm scan` on the real library reports 2 440 mp3 / 357 flac / 5 m4a and
      the aux counts (numbers may drift; assert against a fixture, eyeball the
      real one)
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

## Files

`src/cli/{scan.rs,doctor.rs,move.rs,undo.rs}`, `src/output.rs`,
`tests/cli_move.rs`, `tests/cli_undo.rs`

## Pitfalls

- Confirmation prompts must show the *full* preview, not a summary line. The
  whole design rests on the user seeing which playlist lines change.
- Print the txid on every successful commit so `undo <txid>` is copy-pasteable.
