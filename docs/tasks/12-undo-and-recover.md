# 12 — `mpdfm undo` and `mpdfm recover`

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 11
- **Status:** not started

## Goal

Reverse any completed transaction, and clean up after an interrupted one, with
verification rather than blind faith.

## Details

```
mpdfm undo                # the most recent complete transaction
mpdfm undo <txid>
mpdfm undo --list         # id, time, summary, undoable?
mpdfm recover             # inspect pending records; roll back or roll forward
```

Undo algorithm:

1. Load the record; refuse unless `status` is `complete` (or `pending` — that is
   `recover`'s path).
2. **Precondition check**: for each completed step, verify the destination still
   exists and still matches the recorded size/mtime (and hash if recorded). If
   a file was modified or moved again since, stop and report exactly which,
   offering `--force` to skip the changed ones. Never silently clobber.
3. Reverse the `fs_steps` in reverse order via `exec_fs::revert`.
4. Restore playlists and the state file from `backup_dir` (a full file restore,
   which is more robust than reversing line edits).
5. Mark the record `reverted`, and write a *new* journal record for the undo
   itself so that undo is itself undoable.
6. Trigger an MPD update if connected.

Recover, for a `pending` record: report what was done, then offer to roll back
(the default and safest) or continue. Detect and refuse the case where the
backup directory has been pruned or deleted.

Redo is out of scope; undoing an undo covers it via step 5.

## Acceptance criteria

- [ ] commit → undo ⇒ `Fixture::snapshot()` equals the pre-commit snapshot
      exactly, for: single file move, album dir move, delete, multi-playlist
      rewrite, and a mixed plan
- [ ] undo after the user modified a moved file stops with a clear report and
      changes nothing; `--force` skips just that file and reports it
- [ ] undo of a delete restores the file from the backup dir with original
      mode and mtime
- [ ] `undo --list` shows transactions newest-first with human summaries
- [ ] undoing an undo re-applies the original change
- [ ] `recover` on each crash-injection fixture from task 11 restores the
      starting state
- [ ] undo with a pruned backup dir refuses with a specific message
- [ ] undo of an already-`reverted` record refuses

## Files

`crates/core/src/journal/undo.rs`, `crates/core/src/journal/recover.rs`

## Pitfalls

- Restoring playlists from backup can lose *unrelated* edits the user made to a
  playlist between the commit and the undo. Detect that (compare the current
  file against the post-commit expectation) and warn before overwriting.
- Reverse order matters: a `RmDirIfEmpty` must be undone by recreating the
  directory *before* the files that lived in it are moved back.
