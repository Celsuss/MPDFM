# 33 — Fix the bugs earlier tasks reported

- **Phase:** M6 · Polish
- **Depends on:** 12, 17, 28, 29
- **Status:** not started

## Goal

Close the defects that tasks 01–32 found and deliberately left alone. Each is
small, each was recorded in a task's notes as "not fixed here", and none should
ship in 0.1.0. This task is only those; anything new found while doing it goes
in the notes below rather than widening the scope.

Collected by reading every note in `docs/tasks/`. Bugs that a task found *and
fixed* (the terminal restore in 20, the `<multiple>` and border-corruption cases
in 23 and 26, the `COMM` frames in 17 and 19, and so on) are not repeated here.

## The bugs

### 1. Bare `mpdfm undo` can pick the wrong transaction

Reported in `28-organize-command.md` ("Found along the way").

`mpdfm undo` chooses the newest transaction by `TxId`. Two ids minted by
different processes in the same second sort by their random salt, not by the
order they happened. So `mpdfm tag set …; mpdfm organize …; mpdfm undo` run
inside one second can undo the tag write instead of the organize. The case
reported was refused safely because the files had moved, but nothing guarantees
that outcome. The comment on `TxId` (`crates/core/src/journal/record.rs`) claims
only retention reads the order, which is false.

- Make "newest" mean the order transactions were committed, not an id comparison:
  a monotonic sequence the journal assigns, or the order of the records on disk.
  Keep `TxId` as an identity.
- Correct the comment on `TxId`.
- Check `undo --list`, retention/pruning and `recover` for the same assumption.
- Test: two transactions committed by separate processes in the same second,
  with the salts arranged so the older id sorts last, and bare `undo` must pick
  the later one. Without the fix the test must fail.
- `tests/cli_organize.rs` currently undoes by explicit txid to avoid this; add a
  bare-`undo` variant.

### 2. `mpdfm tag set --year 20004` silently stores `2000`

Reported in `29-doctor.md` ("Found along the way").

`check_edits` → `timestamp` in `crates/core/src/tags/write.rs` accepts a
five-digit year, and `lofty` keeps the first four digits, so the edit is changed
without a word. That breaks the rule the writer exists for: an edit is either
applied as given or refused.

- Reject a year that is not four digits, with the same refusal style the other
  field checks use. Cover `20004`, `200`, `0`, and a date whose year part is
  malformed (`20004-01-01`).
- Check the TUI tag editor, which has its own inline validation (task 23), and
  make it agree with the writer; one definition if they can share one.
- Check that the read-back verification would have caught this anyway. If it
  did not, that is a second finding for the notes.
- Do not change how an *existing* implausible year is read or reported; that is
  `doctor`'s `implausible-years` check.

### 3. The TUI's `:doctor` is a stub

Reported in `29-doctor.md` ("Found along the way"). This is an unfinished
feature rather than a defect, and is the one item here to cut if it is judged
larger than a bug fix.

`:doctor` in the TUI still answers "not yet" and points at task 29, which has
landed. `doctor::run` already takes the `Library` the TUI holds.

- Show the findings in a read-only view or panel, grouped as the CLI groups them
  (problems before notes), with the same counts.
- Run it on a worker, as the scan and tag reads are; it must not block a frame.
- Remove the "not yet" text and update `:help` and the README/man page if they
  mention it.

## Acceptance criteria

- [ ] Bare `undo` selects the most recently committed transaction regardless of
      id ordering, shown by a failing-before test with same-second ids
- [ ] The `TxId` documentation says what its ordering is and is not used for
- [ ] `tag set --year` refuses anything but a four-digit year, in the CLI and in
      the TUI editor, and never writes a value other than the one asked for
- [ ] `:doctor` in the TUI shows the real findings without blocking input
- [ ] No test was weakened or skipped to get here; `cargo test` passes
- [ ] Hand-verified read-only against `~/Music` where a command reads it, and on
      a fixture or a real copy where anything writes
- [ ] `CHANGELOG.md` records the fixes under `[Unreleased]`

## Files

Expected: `crates/core/src/journal/{record.rs,undo.rs}`,
`crates/core/src/tags/write.rs`, `src/tui/` (the editor's year check and the
`:doctor` command), `tests/cli_organize.rs`, `tests/cli_tags.rs`,
`CHANGELOG.md`.

## Pitfalls

- Changing how "newest" is decided must still read journals written by earlier
  versions of MPDFM; the record format is forward-compatible (task 11) and
  should stay so.
- Undo is the most dangerous command in the tool. Do not touch the safety checks
  (drift detection, `--force`) while fixing the selection.
