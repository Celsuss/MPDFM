# 31 — Documentation

- **Phase:** M6 · Polish
- **Depends on:** 15, 19, 26, 28
- **Status:** not started

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

- [ ] README covers install, quick start, the safety model, and a TUI screenshot
- [ ] every config key in `config.rs` appears in `config.example.toml` (a test
      can assert this by parsing both)
- [ ] man page generates in the build and `man ./mpdfm.1` renders correctly
- [ ] `docs/SAFETY.md` contains a manual recovery procedure someone can follow
      without the binary working
- [ ] zsh completion installs and completes subcommands and flags
- [ ] `--help` for every subcommand has a real description and an example
- [ ] CHANGELOG has an entry for the first release

## Files

`README.md`, `docs/{config.example.toml,keys.example.toml,SAFETY.md}`,
`CHANGELOG.md`, `build.rs` or an `xtask` for man/completions

## Pitfalls

- The safety documentation is the part most likely to be skipped and most likely
  to matter. Write it while the invariants are fresh.
