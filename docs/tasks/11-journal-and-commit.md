# 11 — Two-phase commit and the journal

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 08, 09, 10
- **Status:** done

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

- [x] the journal file exists with `status: pending` before any file is moved —
      `Inject::AfterPending` stops the commit between steps 3 and 4; the test
      reads the record back off disk and compares a `Snapshot` of the music and
      playlist directories against the one taken before
- [x] `fsync` is called on journal writes and on `backup_dir` — there is no
      portable way to observe an `fsync` from outside the process, so the one
      place that calls `sync_all` on the journal's behalf records what it synced,
      behind the same `testing` feature as the fixture builder
      (`journal::store::syncs`). The test asserts the record, the journal
      directory, the step log and the backup directory each appear
- [x] re-validation catches a file that was modified between preview and commit —
      and specifically a file whose *size is unchanged*
      (`Fixture::flip_byte`), which no step of the plan would otherwise notice.
      Also: a source that vanished, a destination that appeared, and a playlist
      whose planned line now reads differently. An *unrelated* line added to an
      affected playlist deliberately does **not** refuse: the plan still rewrites
      exactly the lines it said it would, and refusing would make MPDFM unusable
      beside a running MPD
- [x] crash injection after each of steps 3–6 leaves a record from which task 12
      can fully restore the starting state; one test per injection point — six
      points (`AfterPending`, `AfterStep(n)`, `AfterSteps`,
      `BeforePlaylistWrite(n)`, `AfterEdits`, `AfterComplete`), each one rolled
      back by `roll_back`, which is task 12's algorithm in fifteen lines working
      from the record alone, and each one asserted byte-identical to the starting
      snapshot afterwards
- [x] a completed transaction's record lists every step with `done: true` — and
      every one of them with a receipt that `exec_fs::revert` accepts
- [x] MPD being unreachable does not fail the commit, and is recorded — in
      `mpd_update_requested` plus `mpd_update_failed`, and as a `CommitWarning`
- [x] retention pruning removes old backup dirs but not journal records — tested
      directly against `Store::prune`, and again through a second commit with
      `backup_keep = 1`
- [x] journal records are forward-compatible: an unknown field is preserved
      (through a read *and the write back*, which is what `undo` does when it
      marks a record reverted), and a `version` mismatch is refused with a message
      naming both versions and the file. A record with no `version` at all is
      refused too, rather than assumed to be version 1

## Files

`crates/core/src/journal/{mod.rs,record.rs,store.rs}` (the record, the layout and
the durability discipline), `crates/core/src/ops/commit.rs` (the sequence),
`crates/core/tests/journal_and_commit.rs` (the acceptance tests).

Touched elsewhere:

- `crates/core/src/ops/exec_fs.rs` — `serde` on `StepReceipt`, `Done`, `Method`,
  `Facts`, `RemovedDir` and `FsWarning`, plus a `SystemTime` representation that
  survives a pre-1970 mtime (`serde`'s own refuses one, and `touch -d 1969` is not
  an error);
- `crates/core/src/playlist/rewrite.rs` — `apply` split into `prepare` →
  `Prepared::back_up` → `Prepared::write`, so the commit can make its record
  durable *between* the backups and the write. `apply` is now those three in a row
  and behaves exactly as it did;
- `crates/core/src/paths.rs` — `serde` on `PathError`, which travels inside
  `FsWarning::Unnamable`;
- `crates/core/src/lib.rs` — `pub mod journal`, `Error::Journal`, `Error::Commit`;
- `crates/core/Cargo.toml` — `serde_json` moves from a dev-dependency to a real
  one. The journal is JSON (`docs/PLAN.md` §7) and `serde_json::Value` is what
  holds a future version's unknown fields.

## Decisions

**Backups come before the journal record, not after.** The task's step order is
kept: `backup_dir` is created and filled at step 2, the record is written at step
3. A crash in between leaves an orphan directory in MPDFM's own data directory,
which is garbage; the other order would leave a record naming backups that are not
there, and `undo` believes the record. Garbage is cheaper than a lie.

**The playlists are read and checked at step 2, not at step 5.**
`rewrite::prepare` reads every affected playlist, verifies every `LineEdit::old`
against the file and computes the new bytes — before the first file moves. A
playlist that was edited since the preview therefore stops the transaction while
nothing has happened yet, instead of after eight files have been renamed. The
bytes computed then are the bytes written at step 5; re-reading at write time would
open a window where a playlist edited in between is written back from newer bytes
that were never checked.

**A completed step is appended to a second file, because rewriting the record is
quadratic.** Measured on this machine (fixture library, ext4): rewriting the whole
record after every step costs 4.8 ms for a 27-step album move, 229 ms for 202
steps, 5.6 s for 1 002, and — extrapolated from the same per-write cost at that
size — some 115 s for the 4 500 steps a whole-library reorganization of the real
library actually produces (236 album directories, 3 132 files). The record grows
with every step, so the total is quadratic in the number of steps.

So `<txid>.steps` holds one JSON object per completed step, appended and
`fsync`ed, and the record itself is rewritten only when something other than a
step changes: `pending`, `complete`, `failed`. `Store::load` folds the log into the
record, so every caller still sees one `Record` and task 12 never has to know the
log exists. The same commits now cost 2.0 ms, 17 ms, 163 ms and 2.2 s — flat per
step instead of quadratic, with the durability guarantee unchanged: every step is
on disk before the next one starts.

The task file offered batching instead ("or in small batches — measure"). Batching
would have been the cheaper change and a worse one: it breaks the single invariant
the whole sequence exists for, that there is no instant at which the library has
changed and the journal does not know. An append keeps it and is just as fast.

A torn tail — a line a power cut cut in half — fails to parse and is dropped,
which is correct: that step had not been acknowledged, and `recover` treating it as
not-done is the direction that is safe to be wrong in. There is a test for exactly
that, and it asserts what `recover` then has to notice for itself: a planned step
whose destination exists and whose source does not is a step that ran.

**A failed transaction is not rolled back automatically.** The record is marked
`failed`, every completed step keeps its receipt, and the error says
`mpdfm recover <txid>`. Rolling back on the spot would run the one code path that
can lose data — reversing a half-finished transaction — unsupervised, with no
chance for the user to look at it first. Task 12 owns that, and the crash-injection
tests prove the record is enough for it.

**An injected failure is a crash, not a failure.** `Inject` returns
`CommitError::Injected` and changes nothing else: it does not mark the record
failed, does not clean up and does not reverse anything, because a power cut does
none of those. That is what makes the six injection tests tests of *recovery* and
not of commit's own error handling — which the two "really failed" tests cover
separately (an unwritable directory, and playlists that cannot be rewritten).

**Re-validation compares the library, not just the plan.** `drift` re-runs
`Plan::validate` against a fresh scan and compares the steps, the playlist edits
and the conflicts — and then compares `size` and `mtime` for every file the plan
reads, between the `Library` the preview used and the fresh one. The second half is
the one that matters: the steps are paths, so a track that was rewritten in place
expands to exactly the same plan, and committing a move of a file that is not the
file the user looked at is the sort of surprise this crate exists to avoid. That is
why `Previewed` carries the `Library` the preview was computed from, rather than
only the `Effects`. The `PlaylistIndex` is deliberately not carried: what matters
about the playlists is the edits it produced, and those are checked line by line
against the files themselves before anything is mutated.

**MPD is a function, not a client.** `Options::update` is
`&dyn Fn(&[DirPath]) -> Result<(), String>`. Core owns no connection — task 13
does — so the CLI will pass something that talks to the daemon and the test passes
something that refuses. `affected_dirs` reduces the steps to the shallowest
directories that cover them, because `update` is recursive and naming a directory
and its child asks for the same work twice; a directory the transaction *removed*
is named by its parent instead, since MPD cannot rescan what is not there.

**A transaction id is sortable, and monotonic within a process.**
`20260924T224500Z-00a3f1`: a compact UTC timestamp, then a counter, then 16 bits
of hash. The counter is not decoration — retention orders transactions by a
lexicographic sort of the journal directory, and one second is not fine enough on
its own. Without it, two commits in the same second sorted arbitrarily and
`prune(1)` could remove the backups of the transaction that had just finished,
which is how the first version of that test failed. Two *processes* committing in
the same second can still sort either way; the hash keeps them from colliding
outright, and the worst the remaining ambiguity can do is prune the newer of the
two first.

**Retention never prunes what `recover` would need.** `Store::prune` skips a
record that is still `pending` or `failed` however old it is — its backups are
exactly what rolling it back needs — and skips a record this build cannot read,
which it has no business making decisions about. Both are reported in
`Pruned::kept` rather than silently, because one of them means a transaction is
still waiting to be recovered.

**The state file is backed up whenever `rewrite_saved_queue` is on.** Task 14 owns
reading and writing MPD's saved queue, so `state_edits` is always empty today and a
non-empty one is refused with `Error::NotImplemented` naming task 14 — silently
skipping it would lose the queue's lines. The *backup* is taken anyway: a few
kilobytes of insurance on the one file MPDFM is configured to edit. `state_edits`
stays the authority on whether it was changed, so undo must not restore a state
file that no edit named — flagged here because the record cannot enforce it.

**`Previewed` and `Committed`, rather than five arguments and a tuple.** The commit
needs three things that all describe the same moment (the plan, the library and the
effects) and hands back four (the id, the record, the warnings, what retention
did).
Naming both ends makes the call site read as what it is, and means task 13 and task
15 can add a field without touching every caller.

**The journal does not swallow a failing `fsync`.** Everywhere else in MPDFM a
failed directory sync is a shrug — the write has already happened and there is
nothing useful to say. Here it is an error, because the record is what makes the
mutations that follow it recoverable, and a commit that cannot promise the record
is on disk has no business moving a file.

## Verified

Against the real library, read-only (nothing was written outside the fixture temp
directories): 3 132 files in 318 directories scan in 82 ms; the biggest real album
(170 files) validates into 172 steps in 10 ms and its pending record would weigh
107 KB; a whole-library reorganization of all 236 album directories validates into
4 500 steps in 242 ms. The commit of a synthetic 4 502-step transaction takes 2.2 s
end to end, of which the journal is a few hundred microseconds per step.

## Pitfalls

- Write the journal with the same temp-file + rename + fsync discipline as
  everything else; a torn journal is worse than no journal.
- Don't put anything unserializable in a step. The journal is the only thing that
  survives a crash.
