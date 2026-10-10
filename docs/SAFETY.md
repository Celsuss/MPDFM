# Safety

MPDFM edits two things you care about: your music files, and the references to
them. This page says what it promises, what it keeps so that it can put things
back, and — most importantly — **how to put things back by hand when MPDFM itself
cannot**.

**If you are here because something went wrong, skip to
[Recovering by hand](#recovering-by-hand).** Nothing above it is needed.

---

## In short

1. Every command that changes anything shows you what it will do first
   (`--dry-run` shows exactly the same preview and then stops).
2. Before the first change, MPDFM copies every playlist it is about to edit, and
   MPD's state file, into a backup directory, and writes a journal record saying
   what it is about to do. Both are on disk, `fsync`ed, before your library is
   touched.
3. Every committed transaction can be reversed byte for byte with `mpdfm undo`.
   One that a crash cut short is dealt with by `mpdfm recover`.

Try `mpdfm doctor` and `mpdfm move --dry-run` before anything else. Neither
writes a byte.

## The invariants

These are the properties the test suite exists to defend (`docs/PLAN.md` §5). A
change that violates one is a bug, however convenient.

1. **No filesystem mutation happens outside `commit()`.** Previews, `--dry-run`,
   the TUI's pending view and `doctor` are pure.
2. **The journal record is durable before the first mutation.**
3. **A playlist line is rewritten only on an exact match** of the normalized
   relative path — never a prefix, a substring or a fuzzy one.
4. **Non-track playlist lines are preserved byte for byte:** URLs, `#EXTINF`,
   comments, blanks, and any line MPDFM has no reading of.
5. **Nothing is read or written outside `music_directory`, `playlist_directory`
   and MPDFM's own data directory.** Symlinks are resolved before this check.
6. **Every file write is atomic:** a temp file in the destination directory,
   `fsync`, `rename`.
7. **A cross-device move is copy → `fsync` → verify → `rename` → unlink**, never a
   half-finished `rename`. `--verify` also hashes the copy.
8. **A path that is not valid UTF-8 is reported and skipped,** never guessed at.
9. **Every committed transaction is undoable,** and `undo` checks its
   preconditions before acting rather than blindly reversing.

Also: nothing is ever overwritten by a move. A collision is a conflict that stops
the commit before it starts. And if you set `delete_enabled = false`, no
operation that would remove a file is accepted at all.

## What is backed up, and where

MPDFM's data directory is `$XDG_DATA_HOME/mpdfm` (`~/.local/share/mpdfm` by
default; `mpdfm config show` prints the one in use).

```
<data_dir>/journal/<txid>.json     one record per transaction
<data_dir>/journal/<txid>.steps    each filesystem step, appended as it completes
<data_dir>/backups/<txid>/         what that transaction could not put back from the record alone
    <playlist>.m3u                 each playlist the transaction rewrites, as it was
    state                          MPD's state file (the saved queue), as it was
    files/<relative path>          a file that was deleted
    tags/<relative path>           a file whose tags were rewritten — the whole original
```

`<txid>` looks like `20260924T224500Z-00a3f1`: a UTC timestamp, then a short
suffix. The newest sorts last.

The **audio you moved** is not copied: a move is a `rename`, and the record says
where it went. Files that were **deleted** or had their **tags rewritten** are
kept whole under the transaction's backup directory, because those are the two
changes a record alone cannot undo.

Backups are kept for the newest `backup_keep` transactions (50 by default). Older
backup directories are removed; the journal records are never removed, so
`mpdfm undo --list` can still tell you *why* an old transaction can no longer be
undone. A transaction that is still `pending` or `failed` is never pruned.

## The order of a commit

```
1  re-check the plan against a fresh scan        nothing changed
2  copy the playlists and state file to backups  nothing changed
3  write the journal record, status "pending"    nothing changed
4  do the filesystem steps, journaling each one  the library changes
5  rewrite the playlists and MPD's state file    the references change
6  mark the record "complete"
7  ask MPD to rescan
```

Steps 1–3 can fail as often as they like and leave the library exactly as it was.
From step 4 on, the record says what had been done — or, at worst, is one step
behind, because a step is logged *after* it happens and a crash can fall between
the two. It is never ahead.

So a crash leaves the record `pending`, and the filesystem somewhere between the
start and the end.

## Detecting an interrupted commit

```console
$ mpdfm undo --list          # lists every transaction and its status
$ mpdfm recover              # surveys, then offers to roll back what a crash left
```

Without the binary:

```sh
grep -lE '"status": *"(pending|failed)"' ~/.local/share/mpdfm/journal/*.json
```

Any file it prints is a transaction that did not finish. (`failed` means MPDFM
stopped on an error rather than being killed; the recovery is identical.)

## Recovering by hand

**Use this when `mpdfm recover` will not run** — a bug, a version mismatch, a
binary you no longer have. It is the same procedure MPDFM follows, spelled out.

Everything you need is two things on disk: the journal record, and the backup
directory it names. You need nothing from MPDFM, but `jq` makes the record easier
to read. Throughout, set:

```sh
DATA=~/.local/share/mpdfm                       # or: the data_dir from config.toml
TX=20260924T224500Z-00a3f1                      # the transaction you found above
REC=$DATA/journal/$TX.json
BAK=$DATA/backups/$TX
```

### 0. Stop, and stop MPD

* **Do not run MPDFM, and do not commit anything else,** until you are done.
* **Stop MPD** (`systemctl --user stop mpd`, or `mpd --kill`). MPD overwrites its
  state file from memory when it shuts down, so a state file restored behind a
  running MPD is thrown away — and a playlist you restore may be saved over by it.
* Check the backup exists. If `$BAK` is not there, **stop**: the transaction
  never got past step 2, nothing was changed, and the record is just a
  leftover. Go to [step 6](#6-close-the-record).

```sh
ls "$BAK"
jq '{status, music_dir, playlist_dir, ops}' "$REC"
```

`music_dir` and `playlist_dir` in the record are the absolute paths this
transaction used. Every library path in `steps` is **relative to `music_dir`**.

### 1. Read what the transaction did

```sh
jq -r '.steps | to_entries[] | "\(.key)\t\(.value.done)\t\(.value.step)"' "$REC"
```

Each step is one of these, in the order it ran. `.done` is whether the journal
heard that it finished — which may be one step *behind* reality. The disk is the
authority: for each step, look.

| Step | What it did | Undo it by |
| --- | --- | --- |
| `MkDir { at }` | created a directory | `rmdir` it, if empty and it did not exist before |
| `RenameFile { from, to }` | moved a file or directory | `mv "to" "from"` |
| `CopyDelete { from, to }` | moved it across filesystems | `mv "to" "from"` (or `cp` then `rm` across the boundary) |
| `RemoveFile { target, backup }` | deleted a file, keeping it in `backup` | `mv "backup" "target"` |
| `WriteTags { target, backup }` | rewrote a file's tags, keeping the original in `backup` | `cp "backup" "target"` |
| `ExtractArt { from, to }` | wrote a new image file `to` | `rm "to"` (it did not exist before) |
| `RmDirIfEmpty { at }` | removed an emptied directory | `mkdir -p "at"` |

A step is **done** if the state it produced is on disk: a `RenameFile` whose
`from` is gone and whose `to` exists; a `RemoveFile` whose `target` is gone. A
step that is not done needs nothing undone. A step where *both* `from` and `to`
exist, or *neither* does, is not something to guess about — see
[If the disk does not match](#if-the-disk-does-not-match).

### 2. Put the playlists back

For every entry in `playlist_edits` — and **only** those — copy the backup over
the file the record names:

```sh
jq -r '.playlist_edits[] | "\(.file_name)\t\(.real_path)"' "$REC"
```

```sh
# for each line it prints:
cp -p "$BAK/<file_name>" "<real_path>"
```

`real_path` is where the playlist really lives. For a symlinked playlist that is
the file the link points at, not the link, and restoring it there is correct.

Restoring a playlist is safe whether or not its edit was ever applied: the
backup is what it was before.

Do **not** restore playlists the record does not list. The backup directory may
hold copies of playlists the transaction never changed, and MPD or you may have
edited those since.

### 3. Put MPD's saved queue back

Only if `state_edits` is not empty:

```sh
jq '(.state_edits | length), .state_backup' "$REC"   # how many edits, and the backup's name
```

```sh
cp -p "$BAK/state" ~/.config/mpd/state     # your state_file, from config.toml or mpd.conf
```

MPD must be stopped (step 0). If `state_edits` is empty, MPDFM did not change that
file, and you must leave it alone: MPD has rewritten it since.

### 4. Put the library back

Undo the **done** steps from the table above in **reverse order**: the last step
that ran is the first you undo. Work from `music_dir`:

```sh
cd "$(jq -r .music_dir "$REC")"
jq -r '.steps | to_entries | reverse[] | "\(.key)\t\(.value.step)"' "$REC"
```

An example, a directory move that was interrupted after the files had been moved
but before the old directory was removed:

```text
2  RmDirIfEmpty { at: "rock/A" }              -> not done: rock/A still exists   (nothing to undo)
1  RenameFile { from: "rock/A/01.flac",
                to:   "rock/B/01.flac" }      -> done                             mv rock/B/01.flac rock/A/01.flac
0  MkDir { at: "rock/B" }                     -> done                             rmdir rock/B
```

Never overwrite: if the place you are moving something back *to* already has a
file, stop and look. It means something else wrote there after the crash, and
only you can say which one is right. `mv -n` refuses to overwrite and is the
right habit for every command here.

A `WriteTags` backup is the *whole original file*, so `cp` over the target puts
its tags, artwork and audio back exactly. For a `RemoveFile`, `mv` the file out
of the backup and, if its directory was removed too (`RmDirIfEmpty`), `mkdir -p`
that first.

### 5. Check

```sh
mpdfm doctor --check references     # if the binary runs; every playlist line should resolve
```

Without the binary: every line in the restored playlists that names a file should
exist under `music_dir`:

```sh
grep -v '^#' "<real_path>" | grep -v '^[a-z]*://' | while IFS= read -r p; do
    [ -e "$(jq -r .music_dir "$REC")/$p" ] || echo "missing: $p"
done
```

Then start MPD again and let it rescan (`mpc update`).

### 6. Close the record

So that MPDFM stops reporting it, and so that nobody later runs `recover` on a
transaction you have already dealt with:

```sh
mkdir -p "$DATA/journal.handled"
mv "$DATA/journal/$TX".* "$DATA/journal.handled/"
```

Leave `$BAK` where it is until you are sure everything is right. It is the only
copy of the originals.

### If the disk does not match

The tidy case — each step done or not done — is what a crash between steps
leaves. Two other cases are rare and worth stopping for:

* **Both `from` and `to` exist.** Either the step was a copy that did not get to
  delete the source (cross-filesystem move), or something recreated one of them.
  Compare them (`cmp`). If identical, the copy completed: remove `from` or `to`
  as you prefer, then continue. If not, you are looking at two different files —
  keep both until you know which is which.
* **Neither exists.** Look in `$BAK/files/` and in your trash before concluding
  anything. Do not recreate a file from memory.

When in doubt, **do less**. A half-undone transaction with the journal and
backups still in place can be finished tomorrow; a guess cannot be taken back.

## What MPDFM does not protect against

* **A deliberate `mpdfm undo --force`,** which skips what has changed since and
  reports it. That is what it is for.
* **Anything you do to the library yourself** between a commit and its undo.
  `undo` checks that the files are still as the commit left them; if they are
  not, it stops and tells you which, instead of overwriting your edit.
* **A disk that lies.** `fsync` is as good as the drive's promise.
* **Backups older than `backup_keep` transactions.** After that, `undo` says the
  backup was pruned and refuses rather than pretending.
