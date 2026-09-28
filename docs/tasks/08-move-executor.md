# 08 — Filesystem move executor

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 02, 05
- **Status:** done

## Goal

The low-level primitive: move, rename and delete files and directories
correctly, including the awkward cases, with each step individually reversible.

## Details

```rust
enum FsStep {
    MkDir      { at: RelPath },
    RenameFile { from: RelPath, to: RelPath },
    CopyDelete { from: RelPath, to: RelPath },   // cross-device
    RemoveFile { target: RelPath, backup: Option<Utf8PathBuf> },
    RmDirIfEmpty { at: RelPath },
}

fn execute(step: &FsStep, root: &Utf8Path) -> Result<StepReceipt, FsError>;
fn revert(receipt: &StepReceipt, root: &Utf8Path) -> Result<(), FsError>;
```

A `MoveDir` at the plan level expands into per-file steps, so a partial failure
is recoverable and every moved file is individually journaled.

Required behaviours:

- **Preflight**: destination must not exist (unless merging is explicitly
  requested); parent dirs are created; source must exist; write permission on
  both parent dirs is checked *before* anything is moved.
- **Same filesystem** → `rename` (atomic).
- **Cross filesystem** → copy to `dest.mpdfm-tmp`, `fsync`, compare size (and
  hash when `--verify`), `rename` into place, then unlink the source. Never
  unlink before the destination is durable.
- **Directory merge**: if the destination directory exists, either refuse
  (default) or merge file-by-file, failing on any individual file collision.
- **Case-only rename** (`Artist` → `artist`): handle via a two-step rename
  through a temporary name so it works on case-insensitive filesystems too.
- **Case-insensitive collision warning**: destination differs from an existing
  entry only by case → warning (a conflict on a case-insensitive fs).
- **Empty directory cleanup**: after moving files out, remove directories that
  are now empty, walking upward but never past `music_directory`. Directories
  containing only ignorable files (e.g. `.DS_Store`) are *not* silently emptied.
- **Delete** moves the file into the transaction's backup directory rather than
  unlinking it, so undo can restore it. Honour `delete_enabled = false`.
- **Read-only file or directory** → clear error naming the path and the missing
  permission, before any mutation.
- Every path is validated with `paths::contains(root, …)` after symlink
  resolution.

## Acceptance criteria

- [x] moving a file within the same fs uses `rename` (verify inode is unchanged)
- [x] cross-device path is exercised in tests — everything on this machine is one
      ext4 filesystem (`~/Music`, `~/.config/mpd/playlists` and
      `~/.local/share` all report device `fd00`), so a second mount would have to
      be faked anyway: `Inject::CrossDevice` makes `rename` report `EXDEV`, which
      is the *only* thing another mount changes, and the `CopyDelete` step reaches
      the same code directly. The source is unlinked after the destination is
      durable, which the next criterion is what actually proves
- [x] interrupting a cross-device move leaves no partial destination visible —
      `Inject::CrashAfterCopy` stops after the copy is synced and **leaves the temp
      file**, so the test asserts that nothing exists under the destination's own
      name, that the source is intact, and that the leftover is hidden and
      `.tmp`-suffixed
- [x] destination-exists produces a conflict, not an overwrite — tested for both
      files and directories, for a file onto a directory and a directory onto a
      file, and for `expand_dir_move` with `Merge::Refuse`
- [x] directory merge mode moves non-colliding files and reports colliding ones
- [x] case-only rename works and is tested — for a file, for a directory, and back
      again through `revert`
- [x] empty source directories are removed up to but never beyond the root
- [x] a read-only destination directory fails preflight with a clear message —
      from `check` and from `execute_with` alike, both naming the directory and the
      missing permission
- [x] `revert` of each `FsStep` restores the previous state byte-for-byte —
      `Fixture::snapshot()` before and after a plan that uses all five variants
- [x] a step whose target is outside the root is rejected, including via symlink —
      and a `RemoveFile` whose *backup* would land outside the data directory
- [x] deleting with `delete_enabled = false` is refused

## Files

`crates/core/src/ops/exec_fs.rs` (the executor),
`crates/core/src/ops/mod.rs` (the new ops layer, which tasks 10 and 11 fill in),
`crates/core/src/lib.rs` (`pub mod ops` and `Error::Fs`),
`crates/core/tests/move_executor.rs` (the acceptance tests).

No new dependencies.

## Decisions

**Write permission is proved, not read off the mode bits.** Before the first
mutation of a step, `execute_with` creates and removes a hidden temp file in both
parent directories. `access(2)` answers for the real uid rather than the effective
one, and mode bits know nothing about ACLs or a read-only mount — and "we found
out at file nine of fourteen" is not an acceptable way to learn a directory is not
writable. `check`, which task 10's `validate` calls and which may not write
anything, falls back to the mode bits: a directory with no write bit set for
anybody is refused, *including* when the process could write to it anyway as root,
so that the preview and the commit give the same answer. That is the one place the
two checks can disagree, and the probe is the authoritative one.

**`EXDEV` turns a `RenameFile` into a copy rather than failing it.** The mount
layout cannot be trusted up front (a bind mount, an overlay or an automount can
all make `st_dev` say something that is not true of `rename`), so the step tries
the rename and reads the error kind. The receipt records `Method::Copy` when that
happened, because a copy is reversed differently from a rename — the way back has
to restore the mode and the mtime by hand. `CopyDelete` still exists as its own
step for a planner that already knows, and for the tests.

**Merge is a property of the expansion, not of a step.** A step never merges
anything: `MkDir` on an existing directory is a no-op and every file move refuses
an occupied destination, which is what makes "merge" expressible at all.
`expand_dir_move(.., Merge::Allow)` therefore emits steps for the files that do
not collide and returns the rest as `Collision`s for task 10 to raise as conflicts.
Executing the steps anyway moves everything else and overwrites nothing, which is
exactly the behaviour the acceptance criterion asks for.

**The case-only rename dance is unconditional.** `Artist` → `artist` goes through
`.Artist.mpdfm-case-<pid>.<n>.tmp` on every filesystem, rather than asking the
filesystem whether it folds case — the answer can differ per mount and there is no
portable way to ask. Folding is ASCII-only, for the same reason `paths.rs` folds no
case at all: Unicode case depends on the locale. `Tänd` → `tÄnd` is therefore an
ordinary rename, which a case-insensitive filesystem refuses as a collision rather
than silently merging. The real library has no case-near-duplicate siblings today
(checked), so this is protection for a future move onto exFAT or APFS rather than a
bug being fixed.

**`RmDirIfEmpty` walks upward, and never deletes clutter to get there.** It uses
`remove_dir`, never `remove_dir_all`, so a directory that still holds something
stops the walk; a directory holding nothing but `.DS_Store`, `Thumbs.db`,
`desktop.ini` or `.directory` produces `FsWarning::OnlyClutter` and stays. The
tempting implementation — remove the clutter, then the directory is empty — deletes
a file the user did not ask to delete, which is the failure this whole crate exists
to avoid. (The real library contains none of those four names today; the fixture
gets one on purpose.) Only the failure to remove `at` itself is an error: above
that MPDFM is tidying up, and a parent it may not remove is not a reason to fail a
move that has already happened.

**The receipt records facts; checking them is task 12's job.** `Facts` carries the
size, mtime, mode and — under `--verify` — the FNV-1a hash of every entry before it
moved, and `revert` deliberately does *not* compare them. `undo` has to be able to
report every file that changed since the commit at once and offer `--force`, which
it cannot do if the first one raises from inside `revert`.

**The temp name is not the `dest.mpdfm-tmp` of the sketch above.** It is
`.<name>.mpdfm-tmp-<pid>.<n>.tmp`: a sibling of the destination (which is what
makes the final `rename` atomic), hidden and `.tmp`-suffixed so MPD — which reads
the music directory — cannot pick a half-copied track up as a track, tagged with
the pid so two MPDFM processes cannot collide, and truncated so that appending the
marker to a long scene-release name cannot exceed `NAME_MAX`. The longest file name
in the real library is 125 bytes, comfortably inside the limit, but task 27 can
generate names that are not.

**`Options::default()` refuses to delete.** The cautious answer to every question:
no deletion, no backup root, no verification, no injected failure. A caller that
forgets to pass the configuration therefore cannot delete anything, which is the
right way for that mistake to surface. `Options::from_config` is the real
constructor.

Verified read-only against the actual `~/Music` (3 132 files, 318 directories):
`check` accepts 2 809 plausible track moves with 0 refusals and 0 warnings in 70 ms
total — 25 µs per step, including the `readdir` that looks for a case-insensitive
collision — and a real 25-file album expands into 27 steps (one `MkDir`, 25 file
moves, one `RmDirIfEmpty`) with no collisions. Nothing was written: `check` and
`expand_dir_move` only read.

## Pitfalls

- `std::fs::rename` across devices fails with `EXDEV`; detect it by error kind
  rather than by comparing device numbers up front (mount layout can lie).
- `fsync` the destination **directory** too, not just the file, or the rename may
  not survive a power loss.
- Do not preserve mtime on a move (rename keeps it anyway); *do* preserve mode
  and mtime on a cross-device copy, so MPD's change detection behaves the same.
