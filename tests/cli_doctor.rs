//! `mpdfm doctor`, driven as the user drives it (tasks 15 and 29).
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
/// Those names are the contract `--check <name>` selects on, so the tests
/// address checks by name rather than by position.
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
    // Absolute, because `rmdir` runs wherever the user is; quoted, because the
    // name has a space in it and the suggestion has to run.
    assert_eq!(
        found["items"][0]["fix"],
        format!("rmdir '{}'", world.abs("jazz/nothing here")),
        "{found:#}"
    );
}

/// A genre directory holds no audio of its own and is not a finding; a
/// directory holding only cover art, with no album around it, is an orphan; an
/// album's own `Scans` directory is a note.
#[test]
fn doctor_tells_a_genre_directory_apart_from_orphan_cover_art() {
    let world = World::new(
        Fixture::builder()
            .album("jazz/album", &["01.mp3"])
            .aux("jazz/album/Scans", &["back.jpg"])
            .album("pop/one", &["01.mp3"])
            .album("pop/two", &["01.mp3"])
            .aux("pop/no album here", &["cover.jpg"])
            .build(),
    );

    let report = world.json(&["doctor"]);
    let orphans = check(&report, "orphan-aux");
    assert_eq!(
        items(&report, "orphan-aux"),
        ["pop/no album here"],
        "{orphans:#}"
    );
    assert_eq!(orphans["severity"], "warning", "{orphans:#}");

    let scans = check(&report, "no-audio-dirs");
    assert_eq!(
        items(&report, "no-audio-dirs"),
        ["jazz/album/Scans"],
        "{scans:#}"
    );
    assert_eq!(scans["severity"], "note", "{scans:#}");
    // Neither is a problem. `jazz/` and `pop/` hold no audio either and are not
    // mentioned at all, because they have albums below them.
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
        // clean check visible rather than silently absent — and a check that
        // did not run says so instead of claiming zero.
        if found["skipped"].is_string() {
            assert_eq!(count, 0, "{found:#}");
            text.assert_stdout(&format!("{name} (skipped)"));
        } else {
            text.assert_stdout(&format!("{name} ({count})"));
        }
    }

    // And the one number a caller would act on.
    let problems = report["problems"].as_u64().expect("problems is a number");
    text.assert_stdout(&format!("{problems} problem(s) found"));
}

// ---------------------------------------------------------------------------
// Task 29: selection, the cap, `--deep`, and fixes that run
// ---------------------------------------------------------------------------

/// Criterion: `--check tags` runs only the tag checks.
#[test]
fn check_tags_runs_only_the_tag_checks() {
    let world = World::realistic();
    let report = world.json(&["doctor", "--check", "tags"]);
    let checks = report["checks"].as_object().expect("checks is an object");
    assert!(checks.contains_key("missing-tags"), "{report:#}");
    for (name, found) in checks {
        assert_eq!(found["group"], "tags", "{name} is not a tag check");
    }

    // Names and groups mix, comma-separated or repeated.
    let mixed = world.json(&[
        "doctor",
        "--check",
        "broken-references,duplicates",
        "--check",
        "empty-dirs",
    ]);
    let mut names: Vec<&String> = mixed["checks"].as_object().unwrap().keys().collect();
    names.sort();
    assert_eq!(
        names,
        [
            "broken-references",
            "empty-dirs",
            "identical-files",
            "same-song"
        ]
    );
}

#[test]
fn an_unknown_check_is_refused_and_the_real_names_are_listed() {
    let world = World::realistic();
    let run = world.run(&["doctor", "--check", "tag"]);
    run.assert_code(1)
        .assert_stderr("no check or group is called `tag`")
        .assert_stderr("missing-tags");
}

/// Criterion: output is capped by default and `--full` shows everything.
#[test]
fn the_text_output_is_capped_and_full_lifts_the_cap() {
    let tracks: Vec<String> = (1..=14).map(|n| format!("{n:02}.mp3")).collect();
    let names: Vec<&str> = tracks.iter().map(String::as_str).collect();
    let world = World::new(Fixture::builder().album("jazz/album", &names).build());

    let capped = world.run(&["doctor", "--check", "unreferenced-audio"]);
    capped
        .assert_stdout("unreferenced-audio (14)")
        .assert_stdout("… and 4 more (--full lists them all)");
    assert!(
        capped.stdout.contains("jazz/album/10.mp3"),
        "{}",
        capped.stdout
    );
    assert!(
        !capped.stdout.contains("jazz/album/11.mp3"),
        "{}",
        capped.stdout
    );

    let full = world.run(&["doctor", "--check", "unreferenced-audio", "--full"]);
    assert!(full.stdout.contains("jazz/album/14.mp3"), "{}", full.stdout);
    assert!(!full.stdout.contains("more (--full"), "{}", full.stdout);

    // `--json` is complete either way.
    let report = world.json(&["doctor", "--check", "unreferenced-audio"]);
    assert_eq!(items(&report, "unreferenced-audio").len(), 14);
}

/// Criterion: `--json` output is stable and parseable.
#[test]
fn the_json_report_is_stable_and_has_a_fixed_shape() {
    let world = World::realistic();
    let first = world.run(&["doctor", "--json"]);
    let second = world.run(&["doctor", "--json"]);
    first.assert_code(0);
    assert_eq!(
        first.stdout, second.stdout,
        "two runs over one library differ"
    );

    let report = first.json();
    for key in [
        "music_dir",
        "playlist_dir",
        "files",
        "playlists",
        "references",
        "problems",
        "warnings",
        "notes",
        "checks",
    ] {
        assert!(report.get(key).is_some(), "no `{key}` in {report:#}");
    }
    for (name, found) in report["checks"].as_object().unwrap() {
        for key in ["group", "severity", "about", "count", "skipped", "items"] {
            assert!(found.get(key).is_some(), "{name} has no `{key}`: {found:#}");
        }
        for item in found["items"].as_array().unwrap() {
            assert!(
                item["what"].is_string() && item["detail"].is_string(),
                "{item:#}"
            );
            assert!(item["fix"].is_null() || item["fix"].is_string(), "{item:#}");
        }
    }
}

/// Criterion: `--deep` duplicate detection finds a deliberately duplicated
/// fixture file, and says on stderr that it is reading.
#[test]
fn deep_finds_a_duplicated_file_and_says_it_is_reading() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3", "02.mp3"])
        .build();
    fx.flip_byte(&fx.abs("jazz/album/02.mp3"));
    std::fs::copy(fx.abs("jazz/album/01.mp3"), fx.abs("jazz/copy.mp3")).unwrap();
    let world = World::new(fx);

    let shallow = world.json(&["doctor", "--check", "identical-files"]);
    assert!(
        check(&shallow, "identical-files")["skipped"].is_string(),
        "{shallow:#}"
    );

    let run = world.run(&["doctor", "--deep", "--check", "identical-files", "--json"]);
    run.assert_code(0)
        .assert_stderr("--deep: comparing the contents of 3");
    let report = run.json();
    assert_eq!(
        items(&report, "identical-files"),
        ["jazz/album/01.mp3"],
        "{report:#}"
    );
}

/// Criterion: the suggested commands are valid and copy-pasteable — so they
/// are run, through a shell, exactly as printed.
///
/// `mpdfm` in them is the binary under test with this fixture's `--config`
/// and `--yes`, which a user at a terminal would not need.
#[test]
fn every_suggested_fix_runs_as_printed() {
    let dir = "hiphop/MF DOOM - Mm..Food (2004) 12'' $HOME";
    let fx = Fixture::builder()
        .album(dir, &["01 a.mp3", "02 b.mp3", "03 c.mp3", "04 d.mp3"])
        .track("Smokin' On.mp3")
        .build();
    // Next to the album, so removing it does not leave its parent empty in turn.
    std::fs::create_dir(fx.abs("hiphop/it's empty")).unwrap();
    let odd = format!("{dir}/03 c.mp3");
    mpdfm_core::tags::write(
        &fx.abs(&odd),
        &mpdfm_core::tags::TagDelta::new()
            .set(mpdfm_core::tags::Field::Album, "Mm.. Food")
            .clear(mpdfm_core::tags::Field::Title),
        &mpdfm_core::tags::WriteOpts::new(),
    )
    .unwrap();
    let world = World::new(fx);

    let report = world.json(&["doctor"]);
    let fixes: Vec<String> = report["checks"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|found| found["items"].as_array().unwrap().clone())
        .filter_map(|item| item["fix"].as_str().map(ToOwned::to_owned))
        .collect();
    // One of each kind there is: rmdir, tag set --album, tag set
    // --title-from-filename, organize.
    assert_eq!(fixes.len(), 4, "{fixes:#?}");

    let binary = assert_cmd::cargo::cargo_bin("mpdfm");
    for fix in &fixes {
        let script = format!(
            "mpdfm() {{ {bin:?} --config {config:?} \"$@\" --yes; }}\n{fix}\n",
            bin = binary.display(),
            config = world.config_file().as_str(),
        );
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .env_clear()
            .envs(world.env())
            .output()
            .expect("sh runs");
        assert!(
            output.status.success(),
            "`{fix}` failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // And each one fixed what it was offered for.
    let after = world.json(&["doctor"]);
    for name in ["empty-dirs", "inconsistent-albums", "unfiled-audio"] {
        assert_eq!(
            check(&after, name)["count"],
            0,
            "{name} after its fix: {after:#}"
        );
    }
    assert!(
        !items(&after, "missing-tags")
            .iter()
            .any(|what| what.ends_with("03 c.mp3")),
        "{after:#}"
    );
}
