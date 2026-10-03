# 12 — `mpdfm undo` and `mpdfm recover`

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 11
- **Status:** done

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

- [x] commit → undo ⇒ `Fixture::snapshot()` equals the pre-commit snapshot
      exactly, for: single file move, album dir move, delete, multi-playlist
      rewrite, and a mixed plan — one test each, comparing a `Snapshot` of the
      music *and* playlist directories taken before the commit. The
      multi-playlist case uses a purpose-built fixture with one track named by
      four playlists and a fifth that does not name it
- [x] undo after the user modified a moved file stops with a clear report and
      changes nothing; `--force` skips just that file and reports it — the
      report is `undo::Check`, which names every changed file at once rather
      than the first, and `--force` leaves that step alone, records it as
      `skipped: …` in the undo's own record, and returns it in
      `Reversed::skipped`. The test then asserts the one remaining difference
      from the starting snapshot is the album that held the edited file
- [x] undo of a delete restores the file from the backup dir with original mode
      and mtime — asserted directly, not through the snapshot, which
      deliberately records neither (task 03). The file is *moved* back out of
      the backup directory rather than copied, so the backup is gone afterwards
- [x] `undo --list` shows transactions newest-first with human summaries —
      `undo::list` → `Listing`, with a `why_not` column that is the same phrase
      `undo` refuses with (`Record::why_not_undoable`), so the list and the
      refusal cannot drift apart. An undo's own row says what it undid
- [x] undoing an undo re-applies the original change — and undoing *that* takes
      it back again; the test runs the cycle twice and compares both snapshots
- [x] `recover` on each crash-injection fixture from task 11 restores the
      starting state — one test per injection point (`AfterPending`,
      `AfterStep(n)`, `AfterSteps`, `BeforePlaylistWrite(n)`, `AfterEdits`),
      each surveyed first and then rolled back, each asserted byte-identical.
      `AfterComplete` is `undo`'s job and `recover` refuses it by name. There is
      a sixth: the torn step-log line task 11's test produced, which `recover`
      finds by looking at the disk
- [x] undo with a pruned backup dir refuses with a specific message — naming
      retention, `backup_keep` and the directory, and `--force` is not a way
      past it. A backup directory something *else* removed gets a different
      message, because retention should not be blamed for it
- [x] undo of an already-`reverted` record refuses — naming the undo that
      reverted it, so the user knows which transaction to undo to get the change
      back

Also tested, because they are the same promises in another shape: undo of a
`pending` record refuses and points at `recover`; an interrupted undo is itself
recoverable (and finishing one marks the transaction it was undoing reverted
only once the reversal is complete); a playlist edited between the commit and
the undo stops it, and `--force` keeps the overwritten bytes in the undo's own
backup directory; MPD is asked to rescan and an unreachable daemon is a warning;
a record whose `music_dir` is not the configured one is undone against its own
root with a warning; and a record whose root has gone away is refused.

## Files

`crates/core/src/journal/undo.rs` (the preconditions, the report, the engine and
the undo's own record), `crates/core/src/journal/recover.rs` (what a crash
actually left, and the two ways out of it),
`crates/core/tests/undo_and_recover.rs` (the acceptance tests).

Touched elsewhere:

- `crates/core/src/journal/record.rs` — `Direction`, `undo_of` and `undone_by`
  on a record; `reconstructed` on a step; `summary_phrase` and
  `why_not_undoable` split out of `headline` so the list, the refusal and the
  headline share one wording. Every new field is `#[serde(default)]`, so
  `VERSION` stays 1: a record task 11 wrote reads back as `forward` with no
  `undo_of`, which is exactly what it is;
- `crates/core/src/ops/exec_fs.rs` — `Facts::of` (what an entry is *now*, for
  comparing against what a receipt recorded) and `hash_file` made public, since
  verifying a recorded hash means reading the file again;
- `crates/core/src/playlist/rewrite.rs` — `after` (the bytes a commit leaves,
  given the bytes it had: the other half of the "is this still what we left
  here?" comparison), `back_up_now` (copy a playlist as it *is*, whatever it
  says) and `replace` (the atomic write, for the one caller that computes the
  bytes elsewhere);
- `crates/core/src/lib.rs` — `Error::Undo`, `Error::Recover`;
- `src/cli/mod.rs` — `undo` and `recover` now point at task 15, which owns the
  commands themselves; the engine they will call is this task's.

## Decisions

**Undo is a transaction too, and journals itself like one.** Safety invariant 2
is not only about commits: an undo takes its own backups, writes its own
`pending` record and `fsync`s it before it touches anything, appends each
reversal as it happens, and marks itself `complete` at the end. That costs one
record and one backup directory per undo and buys three things — an interrupted
undo is recoverable (there is a test), the bytes `--force` overwrites are kept
rather than lost, and undo is itself undoable, which is where redo comes from.

**Redo is undo-of-an-undo, and `Direction` is what makes that work.** A record
says whether its steps were *executed* or *reversed*. Undoing a `forward` record
reverses its steps; undoing a `reverse` record executes them again. The
direction flips with every undo, so `mpdfm undo <the undo>` re-applies the
original change and there is no `redo` command to disagree with `undo` about
what it means. The alternative — synthesising inverse receipts at undo time —
would have needed a second implementation of every reversal, which is the thing
most worth not having two of.

**One engine, two commands.** `undo::Pass` is the only place either direction's
work happens, and `undo::put_back` is the only place a reversal is sequenced.
`recover`'s rollback is that function over the steps a half-finished transaction
actually reached; its roll-forward is the same `Pass` over the ones it had not.
So "rolling back a crashed commit" and "undoing a finished one" cannot diverge,
which they would have if recovery had grown its own copy of the ordering rules.

**`recover` looks at the disk; `undo` does not.** A `complete` record is a
finished account of what happened, so undo can trust it and check only whether
the *library* still matches. A `pending` record can be exactly one line short of
the truth — the commit journals a step after doing it, and a power cut can tear
the last line — so recovery classifies every unjournaled step by looking: a step
whose destination is there and whose source is not is a step that ran. Its
receipt is reconstructed from the file as it is now, flagged `reconstructed: true`
and appended to the transaction's own log *before* anything moves, so a second
crash finds a record that knows what the first one left.

A reconstructed receipt deliberately claims less than a real one: no hash, no
record of which directories the step created, and `Method::Copy` rather than
`Rename` — a rename back across a filesystem boundary fails with `EXDEV` where a
copy works either way, and the way back is the only thing the receipt is for.

**The one thing that is never guessed.** A step with both ends on disk, or
neither, cannot be classified by looking. It is reported, it stops the recovery,
and `--force` is what says "deal with the rest and leave that one alone".
`MkDir` is the same problem in a milder form — it is idempotent, so a directory
that is there may or may not be this transaction's — and the answer is the same:
it is left outstanding, and the directory is reported rather than removed on a
guess.

**Playlists are restored whole; the drift is detected first.** Reversing line
edits would preserve an unrelated edit made since the commit, and it was
rejected anyway: a restore cannot drift out of step with a line-arithmetic bug,
and it does not care how far through the writes the transaction got. What the
task file asks for instead is detection, and the comparison is symmetric —
the backup is one side of the transaction, `rewrite::after` applied to it is the
other, and a file that matches neither was edited by somebody else. The question
is asked the other way round for an undo's own record (apply the edits to what is
there now and it must give the backup), which is how a redo tells drift from its
own work. A changed playlist stops the undo; `--force` restores it and the bytes
it overwrote are in the undo's own backup directory, named in the warning.

**A changed *file* stops the undo, and a chmod does not.** The precondition is
size, then mtime, then the hash when `--verify` recorded one — all three of which
a move preserves, so a mismatch means somebody wrote to the file. Mode bits are
deliberately not compared: a `chmod` is not a change to the file, and refusing to
undo a move because of one would be theatre. Nor is a directory compared by size
or mtime, since a directory's mtime moves whenever anything inside it does and
only a case-only rename makes a directory a step's subject.

**A step that is already back is a note, not a refusal.** A destination that is
gone whose source is there means there is nothing left to reverse — which is what
a re-run after an undo that failed halfway finds, and what undoing by hand
leaves. It is skipped with a warning, so the second `undo` finishes the job
instead of demanding `--force` for every step the first one managed.

**A directory that is not empty any more is kept, not emptied.** `FsError::NotEmpty`
from a reversal is a warning (`DirKept`), not a failed undo: the files are back
where they belong and something that is not this transaction's is in the way.
Deleting it to tidy up is the bug that rule exists to prevent. The check cannot
raise this in advance, either, because the steps reversed *before* a directory
are exactly what empty it.

**The record's root wins over the configuration's.** Every path in a record is
relative to the `music_dir` the record names, and undoing against a different
root would move files nobody asked about. A configuration that now says
something else gets a warning, not a refusal; a record whose own root has gone
away is refused.

**The transaction is marked `reverted` before the undo is marked `complete`.** A
crash between the two leaves a `pending` undo whose log says every step is done,
which `recover` finishes by writing one record. The other order would leave a
transaction claiming to be in effect when it is not — and `undo` believes the
record.

## Pitfalls

- Restoring playlists from backup can lose *unrelated* edits the user made to a
  playlist between the commit and the undo. Detect that (compare the current
  file against the post-commit expectation) and warn before overwriting.
- Reverse order matters: a `RmDirIfEmpty` must be undone by recreating the
  directory *before* the files that lived in it are moved back.
