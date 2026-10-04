//! `mpdfm scan`, driven as the user drives it (task 15).
//!
//! The acceptance criterion this file answers is the counts one: `scan` reports
//! the formats, the directory total and the aux counts, and the `--json` form
//! says the same numbers as the text form. The criterion names the *real*
//! library's figures (2 440 mp3 / 364 flac / 5 m4a); those are eyeballed by hand
//! — see the task's "Verifying it by hand" — and asserted here against a
//! fixture, which is the tier the rule allows.

#![cfg(unix)]

mod harness;

use harness::World;
use mpdfm_core::testing::{Fixture, names};

#[test]
fn scan_counts_every_kind_the_fixture_holds() {
    let world = World::realistic();
    world.assert_hermetic();

    let run = world.run(&["scan"]);
    run.assert_code(0);

    // The realistic fixture's shape, which reproduces the real library's:
    // mp3-only albums, one FLAC album, one m4a, cover art, scene clutter, a CUE
    // sheet, and a `.m3u` *inside* the library that is not one of MPD's.
    let json = world.run(&["scan", "--json"]).json();
    let counts = &json["counts"];
    assert_eq!(counts["m4a"], 1, "the one m4a: {json:#}");
    assert!(
        counts["mp3"].as_u64().expect("mp3 is a number") >= 8,
        "{json:#}"
    );
    assert!(
        counts["flac"].as_u64().expect("flac is a number") >= 2,
        "{json:#}"
    );
    assert_eq!(counts["cue"], 1, "the one CUE sheet: {json:#}");
    assert_eq!(
        counts["playlists"], 1,
        "the album-internal `Mm..Food.m3u`, and not one of MPD's own: {json:#}"
    );
    assert!(
        counts["images"].as_u64().expect("images is a number") >= 1,
        "{json:#}"
    );
    assert!(
        counts["sidecars"].as_u64().expect("sidecars is a number") >= 3,
        "{json:#}"
    );

    // Every total adds up from its parts, in both renderings.
    let audio = counts["mp3"].as_u64().expect("mp3")
        + counts["flac"].as_u64().expect("flac")
        + counts["m4a"].as_u64().expect("m4a");
    assert_eq!(counts["audio"], audio, "{json:#}");
    assert_eq!(
        counts["files"].as_u64().expect("files"),
        audio
            + counts["images"].as_u64().expect("images")
            + counts["cue"].as_u64().expect("cue")
            + counts["playlists"].as_u64().expect("playlists")
            + counts["sidecars"].as_u64().expect("sidecars")
            + counts["other"].as_u64().expect("other"),
        "{json:#}"
    );
}

/// Criterion: `--json` output parses and contains the same counts as the text
/// output.
///
/// Asserted by reading the numbers back *out of the text* rather than by
/// trusting that both were built from the same value: the point of the criterion
/// is that the two renderings cannot disagree, and a test that only checks the
/// JSON would not notice if the table were wired to something else.
#[test]
fn the_json_counts_are_the_counts_the_table_printed() {
    let world = World::realistic();
    let text = world.run(&["scan"]);
    let json = world.run(&["scan", "--json"]).json();
    text.assert_code(0);

    // Table row label, and the JSON pointer it has to agree with.
    for (label, pointer) in [
        ("audio", "/counts/audio"),
        ("images", "/counts/images"),
        ("cue sheets", "/counts/cue"),
        ("playlists", "/counts/playlists"),
        ("sidecars", "/counts/sidecars"),
        ("other", "/counts/other"),
        ("files", "/counts/files"),
        ("directories", "/directories"),
        ("album dirs", "/album_dirs"),
    ] {
        let from_json = json
            .pointer(pointer)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("no number at {pointer} in:\n{json:#}"));
        let from_text = row(&text.stdout, label);
        assert_eq!(
            from_text, from_json,
            "`{label}` is {from_text} in the table and {from_json} in --json"
        );
    }

    // And the per-format breakdown, which the table carries as an aside.
    text.assert_stdout(&format!(
        "{} mp3, {} flac, {} m4a",
        json["counts"]["mp3"], json["counts"]["flac"], json["counts"]["m4a"]
    ));
}

/// The number on the `label` row of `scan`'s table.
fn row(stdout: &str, label: &str) -> u64 {
    stdout
        .lines()
        .find_map(|line| {
            let rest = line.strip_prefix(label)?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or_else(|| panic!("no `{label}` row in:\n{stdout}"))
}

#[test]
fn scan_reports_a_symlink_rather_than_following_it() {
    // The symlinked playlist lives in the playlist directory, which `scan` never
    // walks — so this needs a symlink inside the music directory, which is what
    // the scanner's warning is actually about.
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3"])
        .track("jazz/album/02.mp3")
        .build();
    std::os::unix::fs::symlink(fx.abs("jazz/album/01.mp3"), fx.abs("jazz/album/link.mp3"))
        .expect("the fixture is writable");

    let world = World::new(fx);
    world.assert_hermetic();

    let run = world.run(&["scan"]);
    run.assert_code(0).assert_stdout("symlink");

    let json = world.run(&["scan", "--json"]).json();
    let warnings = json["warnings"].as_array().expect("warnings is an array");
    assert_eq!(warnings.len(), 1, "{json:#}");
    assert_eq!(warnings[0]["kind"], "symlink", "{json:#}");
    assert_eq!(warnings[0]["path"], "jazz/album/link.mp3", "{json:#}");
    // Not followed means not counted: two tracks, not three.
    assert_eq!(json["counts"]["audio"], 2, "{json:#}");
}

#[test]
fn scan_refuses_a_music_dir_that_is_not_there() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    let world = World::new(fx);

    // The flag beats the config file, which is how a user overrides a root — and
    // a root that does not exist is the one configuration mistake worth an
    // error rather than a warning.
    let missing = world.fx.root().join("nowhere");
    let run = world.run(&["--music-dir", missing.as_str(), "scan"]);
    run.assert_code(1).assert_stderr("nowhere");
}

#[test]
fn scan_names_the_multi_disc_set_without_counting_its_root_as_an_album() {
    let world = World::new(
        Fixture::builder()
            .multi_disc(names::MERCURY_ALBUM, &["CD 1 - Acts 1", "CD 2 - Acts 2"])
            .build(),
    );

    let json = world.run(&["scan", "--json"]).json();
    // Two discs hold the audio; the set root holds none and so is not an album
    // directory (task 05).
    assert_eq!(json["album_dirs"], 2, "{json:#}");
    assert_eq!(json["discs"], 2, "{json:#}");
    assert_eq!(json["multi_disc_sets"], 1, "{json:#}");

    world
        .run(&["scan"])
        .assert_stdout("2 of them discs of 1 multi-disc set(s)");
}
