# 31 — Documentation

- **Phase:** M6 · Polish
- **Depends on:** 15, 19, 26, 28
- **Status:** done

## Goal

Make the project usable by someone who did not design it — including future
sessions of this work.

## Details

**README.md** — what it is, the safety model in three sentences (why it exists:
MPD stores paths in playlists *and* the saved queue), a screenshot or an asciinema
cast of the TUI, install instructions, quick start, and a prominent note that the
first thing to try is `mpdfm doctor` and `mpdfm move --dry-run`.

**docs/config.example.toml** and **docs/keys.example.toml** — every option with
its default and a one-line comment.

**Man page** — `mpdfm(1)` covering all subcommands, generated from `clap` with
`clap_mangen` so it cannot drift.

**docs/SAFETY.md** — the invariants from `PLAN.md` §5, what is backed up and
where, how to recover manually from a journal record if MPDFM itself is broken.
Someone reading this at 2 a.m. after an interrupted commit needs the manual
recovery steps.

**Shell completions** — bash/zsh/fish via `clap_complete`, plus install notes
(the user runs zsh).

**CHANGELOG.md** — keep-a-changelog format from the first release.

Also: update `docs/ROADMAP.md` statuses as tasks complete, and keep `PLAN.md` §3
(observed library facts) accurate if the library layout changes materially.

## Acceptance criteria

- [x] README covers install, quick start, the safety model, and a TUI screenshot
- [x] every config key in `config.rs` appears in `config.example.toml` (a test
      can assert this by parsing both)
- [x] man page generates in the build and `man ./mpdfm.1` renders correctly
- [x] `docs/SAFETY.md` contains a manual recovery procedure someone can follow
      without the binary working
- [x] zsh completion installs and completes subcommands and flags
- [x] `--help` for every subcommand has a real description and an example
- [x] CHANGELOG has an entry for the first release

## Files

`README.md`, `docs/{config.example.toml,keys.example.toml,SAFETY.md}`,
`CHANGELOG.md`, `build.rs` or an `xtask` for man/completions

## Pitfalls

- The safety documentation is the part most likely to be skipped and most likely
  to matter. Write it while the invariants are fresh.

## As built

- **Man page and completions are subcommands, not a `build.rs` step.** The clap
  definitions live in the binary crate, which a build script cannot link against,
  and a subcommand is the one thing a package build, `just docs` and a user can all
  run the same way: `mpdfm man [--dir DIR]` writes `mpdfm.1` plus one
  `mpdfm-<subcommand>.1` per command (20 pages), and `mpdfm completions <shell>`
  prints bash/zsh/fish/elvish/powershell. Tests assert every command has a page,
  that each is clean under `groff -ww`, and that the zsh script is syntactically
  valid and registers for `mpdfm` under `compinit`. `just docs` writes them to
  `target/docs`. Task 32 installs them.
- **`--help` examples** live in `src/cli/examples.rs`; a test requires one for every
  command at every depth and parses each example line with the real argument
  parser, so an example that stops working fails the build.
- **The prose docs are tested where they can be.** `tests/docs.rs` requires every
  key in `CONFIG_KEYS` to appear in `config.example.toml`, no unknown key to appear,
  and the file uncommented to resolve to exactly the defaults of no file at all.
  The keymap example was already covered by tests in `src/tui/keys.rs`.
- **The screenshot is a text frame**, drawn by the TUI's own renderer into a test
  backend (no terminal emulator or `asciinema` was available), against the test
  fixture library.
- **`docs/SAFETY.md`'s recovery procedure was run** against a real record: its `jq`
  commands were executed on a transaction made by the binary, and `mpdfm recover`
  was shown to roll that same record back. The manual `mv`/`cp` steps themselves
  were not exercised against a crash-injected transaction.
- `PLAN.md` §3 needed no change; the library layout has not changed.
