# Undo and recover

## Undo

Every change is a journaled transaction, and every transaction can be reversed
byte for byte as long as what it touched has not changed since.

```console
$ mpdfm undo --list                         # what can be undone
$ mpdfm undo                                # the most recent
$ mpdfm undo 20260924T224500Z-00a3f1        # a specific one
```

If something has changed since, `undo` says so and refuses. `--force` reverses
everything that is still safe to reverse and reports what it skipped.

## Recover

If MPDFM is killed halfway through a commit, the journal knows. `recover` surveys
what was left and rolls it back (the safe default) or finishes it:

```console
$ mpdfm recover                              # survey, then roll back what a crash left
$ mpdfm recover --forward                    # finish it instead
$ mpdfm recover 20260924T224500Z-00a3f1
```

If `mpdfm` itself will not run, [Safety and recovery](../safety.md) has the same
recovery by hand.

See the [`undo`](../reference/cli/undo.md) and
[`recover`](../reference/cli/recover.md) references.
