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

# Pass arguments through, e.g. `just run scan --json`.
run *ARGS:
    cargo run --quiet -- {{ARGS}}

# The task-20 checks `cargo test` cannot make: how much CPU an idle session uses,
# and the terminal's modes across a real pseudo-terminal. The pty half also runs as
# `cargo test --test tui_terminal`; what is only here is the CPU number, because 30
# seconds of sitting still does not belong in a test suite. Needs `script`.
verify-tui seconds="30" bin="./target/release/mpdfm":
    @sh scripts/verify-tui.sh {{seconds}} {{bin}}
