# 11 — Two-phase commit and the journal

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 08, 09, 10
- **Status:** not started

## Goal

Make every committed change durable, inspectable and reversible, and make an
interrupted commit recoverable rather than a mystery.

## Details

Journal record, one JSON file per transaction at
`$XDG_DATA_HOME/mpdfm/journal/<txid>.json` (`txid` = sortable timestamp + short
random suffix):

```jsonc
{
  "txid": "20260924T224500Z-a3f1",
  "version": 1,
  "status": "pending" | "complete" | "reverted" | "failed",
  "started_at": "...", "finished_at": "...",
  "music_dir": "/home/celsuss/Music",
  "playlist_dir": "/home/celsuss/.config/mpd/playlists",
  "backup_dir": "/home/celsuss/.local/share/mpdfm/backups/20260924T224500Z-a3f1",
  "ops": [ ... ],                  // the user-level Plan
  "steps": [                       // expanded, with per-step completion
    { "step": { "RenameFile": { "from": "...", "to": "..." } },
      "done": true, "receipt": { ... } }
  ],
  "playlist_edits": [ ... ],       // includes backup file names
  "state_edits": [ ... ],
  "mpd_update_requested": true
}
```

Commit sequence (the order is the whole point):

1. Re-validate the plan against a fresh scan. If the disk changed since the
   preview, abort and tell the user rather than committing a stale plan.
2. Create `backup_dir`; copy every affected playlist and the MPD state file into
   it; `fsync`.
3. Write the journal record with `status: pending`; `fsync` the file and its
   directory. **Nothing has been mutated yet.**
4. Execute `fs_steps` in order, updating and flushing the journal after each
   step (or in small batches — measure; correctness before speed).
5. Apply playlist edits (task 09) and state edits (task 14).
6. Set `status: complete`, `fsync`.
7. If MPD is reachable, request `update` for the affected directories (task 13).
   A failure here is a warning, never a failed transaction.

Retention: keep the newest `backup_keep` (default 50) transactions; prune older
backup directories but keep their journal records (they are small) marked
`backup_pruned: true` so `undo` can explain why it cannot proceed.

## Acceptance criteria

- [ ] the journal file exists with `status: pending` before any file is moved
      (test by injecting a failure right after step 3)
- [ ] `fsync` is called on journal writes and on `backup_dir`
- [ ] re-validation catches a file that was modified between preview and commit
- [ ] crash injection after each of steps 3–6 leaves a record from which task 12
      can fully restore the starting state; one test per injection point
- [ ] a completed transaction's record lists every step with `done: true`
- [ ] MPD being unreachable does not fail the commit, and is recorded
- [ ] retention pruning removes old backup dirs but not journal records
- [ ] journal records are forward-compatible: an unknown field is preserved, and
      a `version` mismatch is refused with a clear message

## Files

`crates/core/src/journal/{mod.rs,record.rs,store.rs}`,
`crates/core/src/ops/commit.rs`

## Pitfalls

- Write the journal with the same temp-file + rename + fsync discipline as
  everything else; a torn journal is worse than no journal.
- Don't put anything unserializable in a step. The journal is the only thing that
  survives a crash.
