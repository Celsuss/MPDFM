# Staging moves, renames, deletes and organize

These keys **stage** an operation: they add it to the pending plan and change
nothing on disk. Stage as many as you like, in any order, then review and commit
them together in the [pending view](pending.md) as one transaction.

All of them act on the marks, or on the row under the cursor when nothing is marked.
Playlists and MPD's saved queue are rewritten as part of the same transaction, just
as for [`mpdfm move`](../guide/move.md).

## Move: `m`

Mark things, walk to the directory they belong in, press `m`. They are staged to
move **into the directory you are looking at**. Moving something to where it
already is stages nothing.

## Rename or move to an exact path: `r` and `:move`

`r` opens the command line already filled in as `:move <path of the row>`. Edit the
path and press `enter`; `esc` cancels. You can also type `:move <dst>` yourself.

With one thing selected, `<dst>` is its new path (a rename). With several, `<dst>` is
a directory they move into.

## Delete: `d`

Stages a delete of the marked **files**. A marked directory is left out with a
message rather than expanded, because deleting a tree should not follow from one
keystroke. Deletes can be switched off entirely with `delete_enabled = false` in the
[configuration](../reference/config.md), in which case the pending view refuses to
commit them. Deleted files are kept in the journal's backup and can be undone.

## Organize: `o` or `:organize`

Opens a template line and a **live preview** of where the first twenty files would
go, updated on every keystroke. A template that does not parse shows the error with
a `^` under the offending character. Files whose tags the template cannot fill are
listed as staying put.

Press `enter` to compute the whole mapping and stage it; `esc` cancels. The syntax
is the same as the command line's — see
[Organizing to a template](../guide/organize.md#template-syntax). `:organize
<template>` opens the view with the template already filled in.

## Changing your mind

Nothing is on disk yet, so [`dd` in the pending view](pending.md) drops one
operation and `x` discards the whole plan. Quitting with operations staged asks
first.
