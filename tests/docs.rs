//! The documentation that can be wrong, checked against the code that makes it
//! true (task 31).
//!
//! `docs/config.example.toml` promises "every option with its default". A promise
//! like that is a test: every key `config.rs` reads must appear, every default the
//! file states must be the one the program has, and a line copied out of it must
//! not be a warning. The man page and the completions are generated from the
//! argument definitions and cannot drift; this is the half that is prose.

#![cfg(unix)]

use std::collections::BTreeSet;

use mpdfm_core::config::{CONFIG_KEYS, Config, ConfigWarning, Env, Overrides, resolve};
use mpdfm_core::testing::Fixture;

const EXAMPLE: &str = include_str!("../docs/config.example.toml");

/// The example with every `# key = value` line uncommented, and only those: the
/// prose around them stays prose.
fn uncommented(text: &str, skip: &[&str]) -> String {
    text.lines()
        .filter_map(|line| {
            let body = line.strip_prefix("# ")?;
            let (key, _) = body.split_once(" = ")?;
            let is_assignment = CONFIG_KEYS.contains(&key) && !skip.contains(&key);
            is_assignment.then(|| body.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolved(fx: &Fixture, config_toml: &str) -> (Config, Vec<ConfigWarning>) {
    let file = fx.root().join("config.toml");
    std::fs::write(&file, config_toml).expect("the fixture root is writable");
    let env = Env::empty().with("HOME", fx.root().as_str());
    resolve(
        &Overrides {
            music_dir: Some(fx.music_dir().to_owned()),
            playlist_dir: Some(fx.playlist_dir().to_owned()),
            config_file: Some(file),
            no_mpd: false,
        },
        &env,
    )
}

#[test]
fn every_config_key_is_in_the_example() {
    let missing: Vec<&str> = CONFIG_KEYS
        .iter()
        .copied()
        .filter(|key| {
            !EXAMPLE.lines().any(|line| {
                line.trim_start_matches("# ")
                    .starts_with(&format!("{key} = "))
            })
        })
        .collect();
    assert!(
        missing.is_empty(),
        "docs/config.example.toml does not document: {missing:?}"
    );
}

#[test]
fn the_example_names_no_key_the_program_does_not_read() {
    let documented: BTreeSet<&str> = EXAMPLE
        .lines()
        .filter_map(|line| line.strip_prefix("# "))
        .filter_map(|body| body.split_once(" = "))
        .map(|(key, _)| key)
        .filter(|key| key.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .collect();
    let known: BTreeSet<&str> = CONFIG_KEYS.iter().copied().collect();
    let stale: Vec<_> = documented.difference(&known).collect();
    assert!(
        stale.is_empty(),
        "documented but unknown to config.rs: {stale:?}"
    );
}

#[test]
fn a_line_copied_out_of_the_example_is_not_a_warning_and_changes_nothing() {
    let fx = Fixture::builder().build();

    // The roots are the fixture's, and `data_dir` / `state_file` expand a variable
    // this environment does not set; their *presence* is the other tests' job.
    let skip = ["music_dir", "playlist_dir", "data_dir", "state_file"];
    let text = uncommented(EXAMPLE, &skip);
    assert!(
        text.lines().count() >= CONFIG_KEYS.len() - skip.len(),
        "the helper found too few assignments:\n{text}"
    );

    let (with, warnings) = resolved(&fx, &text);
    // The one thing allowed to be said is that this machine has no mpd.conf.
    let loud: Vec<_> = warnings
        .iter()
        .filter(|w| !matches!(w, ConfigWarning::NoMpdConf { .. }))
        .collect();
    assert!(loud.is_empty(), "{loud:?}");

    // "Every option with its default": the file as uncommented is the program's
    // behaviour with no file at all.
    let (without, _) = resolved(&fx, "");
    assert_eq!(
        format!("{:?}", with.mpd_address),
        format!("{:?}", without.mpd_address)
    );
    assert_eq!(with.mpd_enabled, without.mpd_enabled);
    assert_eq!(with.rewrite_saved_queue, without.rewrite_saved_queue);
    assert_eq!(
        with.trigger_update_after_commit,
        without.trigger_update_after_commit
    );
    assert_eq!(with.delete_enabled, without.delete_enabled);
    assert_eq!(with.backup_keep, without.backup_keep);
    assert_eq!(with.organize_template, without.organize_template);
    assert_eq!(
        with.organize_portable_names,
        without.organize_portable_names
    );
    assert_eq!(
        format!("{:?}", with.id3_version),
        format!("{:?}", without.id3_version)
    );
    assert_eq!(
        format!("{:?}", with.image_protocol),
        format!("{:?}", without.image_protocol)
    );
}
