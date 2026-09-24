# 01 — Project scaffold

- **Phase:** M1 · Trustworthy move engine
- **Depends on:** nothing
- **Status:** not started

## Goal

A compiling Cargo workspace with the crate layout from `PLAN.md` §4, a `clap`
argument skeleton, and a `just`/`make` entry point for the checks we will run
every session.

## Details

Workspace root `Cargo.toml` with members `crates/core` (`mpdfm-core`) and the
root binary crate `mpdfm`. Core must not depend on `clap`, `crossterm` or
`ratatui` — enforce this by review, and ideally by a test that greps
`crates/core/Cargo.toml`.

`clap` subcommand shape (bodies can be `todo!()` at this stage):

```
mpdfm                                   → launch TUI (stub that prints and exits for now)
mpdfm scan [--json]
mpdfm doctor
mpdfm move <SRC> <DST> [--dry-run] [--yes]
mpdfm organize [PATH] --template <T> [--dry-run]
mpdfm tag show <PATH>
mpdfm tag set <PATH> --field=value...
mpdfm undo [TXID]
mpdfm recover
```

Global flags: `--music-dir`, `--playlist-dir`, `--config`, `--no-mpd`,
`-v/--verbose`, `--json`.

Pin exact dependency versions. Add `rust-toolchain.toml`, `rustfmt.toml`,
`clippy` settings (`-D warnings` in CI), and a `.gitignore` for `target/`.

## Acceptance criteria

- [ ] `cargo build` and `cargo test` succeed on a clean checkout
- [ ] `cargo clippy --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --check` is clean
- [ ] `mpdfm --help` lists every subcommand above
- [ ] `mpdfm move a b` exits non-zero with "not implemented" rather than panicking
- [ ] `just check` (or `make check`) runs fmt + clippy + test
- [ ] `mpdfm-core` has no terminal or CLI dependency in its manifest

## Files

`Cargo.toml`, `crates/core/{Cargo.toml,src/lib.rs}`, `src/main.rs`,
`src/cli/mod.rs`, `src/tui/mod.rs`, `justfile`, `rust-toolchain.toml`,
`rustfmt.toml`, `.gitignore`

## Pitfalls

- Don't let convenience types leak from the binary into core later; the
  direction of dependency is one-way and worth protecting from task 01.
- Prefer `thiserror` in core and `anyhow` only in `src/` so callers can match on
  error kinds.
