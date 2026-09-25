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
