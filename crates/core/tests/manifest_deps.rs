//! Invariant: `mpdfm-core` never depends on a front-end crate.
//!
//! The dependency direction is one-way (`docs/PLAN.md` D2). Catching a stray
//! `clap` or `ratatui` here is cheaper than untangling it later.

const FORBIDDEN: &[&str] = &["clap", "crossterm", "ratatui", "anyhow"];

#[test]
fn core_has_no_terminal_or_cli_dependency() {
    let manifest = include_str!("../Cargo.toml");

    // Only the dependency sections matter; prose in comments may mention them.
    let mut in_deps = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line.contains("dependencies]");
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split(['=', '.', ' ']).next().unwrap_or_default();
        assert!(
            !FORBIDDEN.contains(&name),
            "mpdfm-core must not depend on `{name}`: front-end crates belong in the binary"
        );
    }
}
