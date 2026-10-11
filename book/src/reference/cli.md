# Command-line reference

These pages are generated from the program's own `--help` output when the site is
built, so they match the code. The same text is available as man pages
(`man mpdfm`, `man mpdfm-move`, …; see [Install](../getting-started/install.md)).

| Command | Does |
| --- | --- |
| [`config`](cli/config.md) | show where MPDFM is looking and why |
| [`scan`](cli/scan.md) | survey the library |
| [`doctor`](cli/doctor.md) | report library and playlist health |
| [`move`](cli/move.md) | move or rename, rewriting every reference |
| [`organize`](cli/organize.md) | re-file tracks into template-derived paths |
| [`tag`](cli/tag.md) | read and write audio tags |
| [`cover`](cli/cover.md) | see, extract and set album art |
| [`undo`](cli/undo.md) | reverse a committed transaction |
| [`recover`](cli/recover.md) | finish or roll back an interrupted transaction |
| [`completions`](cli/completions.md) | print a shell completion script |
| [`man`](cli/man.md) | print or write man pages |

Running `mpdfm` with no subcommand opens the [TUI](../tui/overview.md).
