# Run `just` with no arguments for the full pre-commit check.
default: check

# fmt + clippy + test — everything CI runs.
check: fmt-check clippy test

# Fail if anything is unformatted.
fmt-check:
    cargo fmt --all --check

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

build:
    cargo build --workspace

# The man pages and shell completions, generated from the argument definitions into
# target/docs. `man -l target/docs/man/mpdfm.1` to read one.
docs:
    cargo build --quiet
    mkdir -p target/docs/completions
    ./target/debug/mpdfm man --dir target/docs/man
    for shell in bash zsh fish; do ./target/debug/mpdfm completions $shell > target/docs/completions/mpdfm.$shell; done

# The documentation site (mdBook) into book/book. The CLI reference is generated
# from the binary's --help, so it is built first.
book:
    cargo build --quiet
    ./scripts/gen-cli-reference.sh
    mdbook build book

# The same, served at http://localhost:3000 and rebuilt on change.
book-serve: book
    mdbook serve book

# Pass arguments through, e.g. `just run scan --json`.
run *ARGS:
    cargo run --quiet -- {{ARGS}}

# The task-20 checks `cargo test` cannot make: how much CPU an idle session uses,
# and the terminal's modes across a real pseudo-terminal. The pty half also runs as
# `cargo test --test tui_terminal`; what is only here is the CPU number, because 30
# seconds of sitting still does not belong in a test suite. Needs `script`.
verify-tui seconds="30" bin="./target/release/mpdfm":
    @sh scripts/verify-tui.sh {{seconds}} {{bin}}
