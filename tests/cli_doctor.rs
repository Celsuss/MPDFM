//! `mpdfm doctor`, driven as the user drives it (task 15).
//!
//! The headline criterion is "reports **exactly** the one known broken
//! reference", and the word that matters is *exactly*. The realistic fixture has
//! one deliberately broken reference and one CUE virtual track whose sheet,
//! track and audio file all exist — and the second of those is the false
//! positive a careless check would produce. So the tests assert that the count
//! is one and that the one is the right one.
//!
//! Each remaining check gets a fixture that triggers it and, where the mistake
//! is plausible, something that must *not* trigger it.

#![cfg(unix)]

mod harness;

use harness::World;
use mpdfm_core::testing::{Fixture, names};
use serde_json::Value;

/// One check's findings, by its stable `--json` name.
///
/// Those names are the contract `--check <name>` will select on in task 29, so
/// the tests address checks by name rather than by position.
fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .get(name)
        .unwrap_or_else(|| panic!("no `{name}` check in:\n{report:#}"))
}

/// Everything one check named, as the `what` strings.
fn items(report: &Value, name: &str) -> Vec<String> {
    check(report, name)["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .filter_map(|item| item["what"].as_str().map(ToOwned::to_owned))
        .collect()
}

/// Criterion: `mpdfm doctor` reports exactly the one known broken reference.
#[test]
fn doctor_finds_the_one_broken_reference_and_no_others() {
    let world = World::realistic();
    world.assert_hermetic();

    let run = world.run(&["doctor"]);
    run.assert_code(0);
    let report = world.json(&["doctor"]);

    let broken = check(&report, "broken-references");
    assert_eq!(broken["count"], 1, "{broken:#}");
    assert_eq!(broken["severity"], "problem", "{broken:#}");

    let found = items(&report, "broken-references");
    assert!(
        found[0].contains(names::BROKEN_REFERENCE),
        "the one broken reference should be {}, got {found:?}",
        names::BROKEN_REFERENCE
    );

    // The trap: the CUE virtual track resolves, because its sheet exists, and a
    // check comparing the whole `album.flac.cue/track0017` string against the
    // file list would report it as broken.
    assert!(
        !run.stdout.contains("track0017"),
        "the CUE virtual track is not broken:\n{}",
        run.stdout
    );

    assert_eq!(report["problems"], 1, "{report:#}");
}

/// `doctor` exits 0 whatever it finds: it reports, it does not fail.
#[test]
fn doctor_reports_rather_than_failing() {
    let dirty = World::realistic();
    dirty.run(&["doctor"]).assert_code(0);
    assert_eq!(dirty.json(&["doctor"])["problems"], 1);

    let clean = World::new(
        Fixture::builder()
            .album("jazz/album", &["01.mp3"])
            .playlist("Jazz.m3u", &["jazz/album/01.mp3"])
            .build(),
    );
    clean
        .run(&["doctor"])
        .assert_code(0)
        .assert_stdout("No problems found.");
    assert_eq!(clean.json(&["doctor"])["problems"], 0);
}

#[test]
fn doctor_reports_a_playlist_line_that_is_not_relative_to_the_music_dir() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3"])
        .playlist("Jazz.m3u", &["jazz/album/01.mp3"])
        .playlist_with_urls("Radios.m3u", &[])
        .build();

    // An absolute path pointing *into* the library: still unrewritable, and with
    // an obvious fix, which is the case worth suggesting one for. Plus a `..`
    // path, which has no fix worth guessing at.
    let absolute = fx.abs("jazz/album/01.mp3");
    std::fs::write(
        fx.playlist_path("Jazz.m3u"),
        format!("jazz/album/01.mp3\n{absolute}\n../outside.mp3\n"),
    )
    .expect("the fixture is writable");

    let world = World::new(fx);
    let run = world.run(&["doctor"]);
    run.assert_code(0);
    let report = world.json(&["doctor"]);

    let found = check(&report, "unrewritable-entries");
    assert_eq!(found["count"], 2, "{found:#}");
    assert_eq!(found["severity"], "problem", "{found:#}");
    run.assert_stdout("an absolute path")
        .assert_stdout("a path with `.` or `..` in it");
    // The absolute one points inside the library, so the relative spelling it
    // should have is offered.
    assert_eq!(
        found["items"][0]["fix"], "replace it with `jazz/album/01.mp3`",
        "{found:#}"
    );

    // A radio URL and an `#EXTINF` are not paths and must not be reported.
    assert!(
        !run.stdout.contains("http"),
        "a radio URL is not a problem:\n{}",
        run.stdout
    );
}

#[test]
fn doctor_reports_a_playlist_it_could_not_read() {
    let world = World::new(
        Fixture::builder()
            .album("jazz/album", &["01.mp3"])
            // Bytes that are not UTF-8: read, and not parsable (task 06).
            .playlist_raw("Broken.m3u", &[0xff, 0xfe, b'\n'])
            .build(),
    );

    let report = world.json(&["doctor"]);
    assert_eq!(
        check(&report, "unreadable-playlists")["count"],
        1,
        "{report:#}"
    );
    assert!(
        items(&report, "unreadable-playlists")[0].contains("Broken.m3u"),
        "{report:#}"
    );
}

#[test]
fn doctor_reports_an_empty_directory_with_a_command_that_would_remove_it() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    std::fs::create_dir(fx.abs("jazz/nothing here")).expect("the fixture is writable");

    let world = World::new(fx);
    let report = world.json(&["doctor"]);

    let found = check(&report, "empty-dirs");
    assert_eq!(found["count"], 1, "{found:#}");
    // Quoted, because the name has a space in it and the suggestion has to run.
    assert_eq!(
        found["items"][0]["fix"], "rmdir \"jazz/nothing here\"",
        "{found:#}"
    );
}

/// A genre directory holds no audio of its own and is not a finding; a leaf
/// directory holding only cover art is.
#[test]
fn doctor_tells_a_genre_directory_apart_from_orphan_cover_art() {
    let world = World::new(
        Fixture::builder()
            .album("jazz/album", &["01.mp3"])
            .aux("pop/no album here", &["cover.jpg"])
            .build(),
    );

    let report = world.json(&["doctor"]);
    let found = check(&report, "no-audio-dirs");
    assert_eq!(found["count"], 1, "{found:#}");
    assert_eq!(found["items"][0]["what"], "pop/no album here", "{found:#}");
    // A note, never a problem: this is normal enough that calling it one would
    // make the whole report less believable. `jazz/` and `pop/` hold no audio
    // either and are not mentioned at all, because they have subdirectories.
    assert_eq!(found["severity"], "note", "{found:#}");
    assert_eq!(report["problems"], 0, "{report:#}");
}

/// The track a referenced CUE sheet describes counts as referenced.
#[test]
fn doctor_does_not_call_a_cue_described_track_unreferenced() {
    let world = World::realistic();
    let unreferenced = items(&world.json(&["doctor"]), "unreferenced-audio");

    // `Pop.m3u` references `…/Acts 1.flac.cue/track0017`, so the `.flac` beside
    // the sheet is reachable through it and is not an orphan.
    assert!(
        !unreferenced
            .iter()
            .any(|path| path.ends_with("Imagine Dragons - Mercury - Acts 1.flac")),
        "the CUE-described FLAC is referenced through its sheet: {unreferenced:#?}"
    );
    // The check still does its job: plenty of the fixture is in no playlist.
    assert!(!unreferenced.is_empty(), "{unreferenced:#?}");
    // And it never counts as a problem, however long it gets.
    assert_eq!(world.json(&["doctor"])["problems"], 1);
}

#[test]
fn doctor_mentions_a_partial_download_that_is_newer_than_its_neighbours() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    let partial = fx.abs("jazz/album/02.mp3.parts");
    std::fs::write(&partial, b"half a track").expect("the fixture is writable");
    // Newer than everything around it, which is the only signal there is. An
    // hour ahead, because a fixture built in one go has files whose mtimes are
    // all the same second.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&partial)
        .and_then(|file| {
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(later)
                    .set_accessed(later),
            )
        })
        .expect("the fixture's mtime is settable");

    let world = World::new(fx);
    let report = world.json(&["doctor"]);

    let found = check(&report, "partial-downloads");
    assert_eq!(found["count"], 1, "{found:#}");
    assert_eq!(found["items"][0]["what"], "jazz/album/02.mp3.parts");
    // `Kind::Other`, counted and never interpreted — and not a problem.
    assert_eq!(found["severity"], "note", "{found:#}");
    assert_eq!(report["problems"], 0, "{report:#}");
}

/// Criterion: `--json` output parses and contains the same counts as the text
/// output.
#[test]
fn the_json_report_says_what_the_text_report_said() {
    let world = World::realistic();
    let text = world.run(&["doctor"]);
    let report = world.json(&["doctor"]);
    text.assert_code(0);

    let checks = report["checks"].as_object().expect("checks is an object");
    assert!(!checks.is_empty(), "{report:#}");
    for (name, found) in checks {
        let count = found["count"].as_u64().expect("count is a number");
        // The text heading is `<name> (<count>)`, which is also what makes a
        // clean check visible rather than silently absent.
        text.assert_stdout(&format!("{name} ({count})"));
    }

    // And the one number a caller would act on.
    let problems = report["problems"].as_u64().expect("problems is a number");
    text.assert_stdout(&format!("{problems} problem(s) found"));
}
