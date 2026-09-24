# 10 — Plan, validation and the preview

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** 05, 07, 08, 09
- **Status:** not started

## Goal

The staging layer from decision D7: build up a list of intended operations,
compute everything that would change *without touching disk*, and render it for
confirmation.

## Details

```rust
enum Operation {
    MoveFile  { from: RelPath, to: RelPath },
    MoveDir   { from: RelPath, to: RelPath },
    Delete    { target: RelPath },
    WriteTags { target: RelPath, changes: TagDelta },   // filled in by M2
}

struct Plan { ops: Vec<Operation> }

impl Plan {
    fn validate(&self, lib: &Library, idx: &PlaylistIndex, cfg: &Config) -> Effects;
}

struct Effects {
    fs_steps:       Vec<FsStep>,          // fully expanded, in execution order
    playlist_edits: Vec<PlaylistEdit>,
    state_edits:    Vec<LineEdit>,        // task 14
    conflicts:      Vec<Conflict>,        // non-empty ⇒ commit refused
    warnings:       Vec<Warning>,         // informational
    summary:        Summary,              // counts: files, dirs, playlists, bytes
}
```

`validate` is pure: no writes, no network. It expands directory moves into
per-file steps, resolves the playlist edits, detects conflicts, and produces the
rendering data.

Conflicts (block commit): destination exists; two ops target the same
destination; an op's source is another op's destination in a way that can't be
ordered; source missing; path outside root; case-insensitive collision on a
case-insensitive fs; permission preflight failure; delete requested while
`delete_enabled = false`.

Warnings (allow commit): playlist lines will be removed; a file is in MPD's
current queue (task 13); an already-broken reference exists nearby; a
case-difference collision on a case-sensitive fs; non-UTF-8 files in the affected
directory were skipped; the album will be split across directories.

Rendering: `Effects::render(&self, width) -> String` produces the preview shown
both by `--dry-run` on the CLI and by the TUI's pending view (task 24), so the
two can never disagree:

```
PENDING (2 ops)
MOVE  hiphop/MF DOOM - Mm Food/ → hiphop/MF DOOM/2004 - Mm..Food/
      14 audio + 3 aux files, 2 playlists affected
TAG   genre: "" → "Hip Hop"  (14 files)

Playlists
  Coding flow.m3u     3 lines rewritten
  Hip hop.m3u         1 line rewritten
MPD saved queue       2 lines rewritten

! 1 file is in MPD's current queue and will need a requeue
```

Operations must be **order-independent to add** and deterministically ordered to
execute: sort so that parents are created before children and sources are moved
before a later op depends on the result.

## Acceptance criteria

- [ ] `validate` performs zero filesystem writes (assert via a fixture snapshot
      before/after)
- [ ] a directory move expands into the right per-file steps including aux files
- [ ] every conflict class above has a test and blocks `commit`
- [ ] every warning class above has a test and does not block `commit`
- [ ] two ops targeting the same destination is a conflict
- [ ] a chain (`a → b`, `b → c`) is either ordered correctly or reported as a
      conflict — decide and document which
- [ ] `render` output is snapshot-tested (`insta`) at 80 and 120 columns
- [ ] `Summary` counts match what `commit` actually does (property test)

## Files

`crates/core/src/ops/{op.rs,plan.rs,effects.rs,render.rs}`

## Pitfalls

- Keep `Effects` serializable; `--json` output and the journal both want it.
- Resist letting the TUI compute its own preview. One renderer, two callers.
