# 08 — Filesystem move executor

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 02, 05
- **Status:** not started

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

- [ ] moving a file within the same fs uses `rename` (verify inode is unchanged)
- [ ] cross-device path is exercised in tests (a second `TempDir` on a different
      mount, or an injected "force copy" flag) and the source is unlinked only
      after the destination is complete
- [ ] interrupting a cross-device move leaves no partial destination visible
      (temp name + rename proves this)
- [ ] destination-exists produces a conflict, not an overwrite — tested for both
      files and directories
- [ ] directory merge mode moves non-colliding files and reports colliding ones
- [ ] case-only rename works and is tested
- [ ] empty source directories are removed up to but never beyond the root
- [ ] a read-only destination directory fails preflight with a clear message
- [ ] `revert` of each `FsStep` restores the previous state byte-for-byte
- [ ] a step whose target is outside the root is rejected, including via symlink
- [ ] deleting with `delete_enabled = false` is refused

## Files

`crates/core/src/ops/exec_fs.rs`

## Pitfalls

- `std::fs::rename` across devices fails with `EXDEV`; detect it by error kind
  rather than by comparing device numbers up front (mount layout can lie).
- `fsync` the destination **directory** too, not just the file, or the rename may
  not survive a power loss.
- Do not preserve mtime on a move (rename keeps it anyway); *do* preserve mode
  and mtime on a cross-device copy, so MPD's change detection behaves the same.
