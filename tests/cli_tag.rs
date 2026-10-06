//! Task 19 — `mpdfm tag show`, `mpdfm tag set` and `mpdfm tag diff`.
//!
//! Driven through the built binary, like every test in this directory, because
//! the prompt, the exit code and the two output streams are part of what the task
//! promises and none of them exists for an in-process call. [`World`] keeps every
//! run inside a fixture; see `tests/harness/mod.rs`.

#![cfg(unix)]

mod harness;

use camino::Utf8Path;
use harness::World;
use mpdfm_core::tags::{self, Field};
use mpdfm_core::testing::{Fixture, tags as fix};

/// An mp3 album, a FLAC album and an m4a, with things a write must not disturb.
fn world() -> World {
    let fx = Fixture::builder()
        .album(
            "hiphop/album",
            &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"],
        )
        .aux("hiphop/album", &["folder.jpg", "release.nfo"])
        .flac_album("jazz/album")
        .multi_disc("pop/set", &["CD 1", "CD 2"])
        .track("coding-music/SwitchAngel/Coding_Trance.mp3")
        .build();

    fix::set_frame(&fx.abs("hiphop/album/01 Beef Rap.mp3"), "TBPM", "174");
    fix::embed_cover(&fx.abs("hiphop/album/01 Beef Rap.mp3"), fix::COVER_PNG);
    fix::set_comment(
        &fx.abs("jazz/album/01 So What.flac"),
        "REPLAYGAIN_TRACK_GAIN",
        "-7.26 dB",
    );

    World::new(fx)
}

/// What a track's genre reads as now.
fn genre(world: &World, rel: &str) -> Option<String> {
    tags::read_tags(&world.abs(rel))
        .unwrap_or_else(|err| panic!("{rel} should read: {err}"))
        .genre
        .first()
        .map(ToOwned::to_owned)
}

// ---------------------------------------------------------------------------
// `tag show`

#[test]
fn show_prints_the_same_field_set_for_an_mp3_and_a_flac() {
    let world = world();
    world.assert_hermetic();

    let run = world.run(&["tag", "show", "hiphop/album/01 Beef Rap.mp3"]);
    run.assert_code(0);
    let flac = world.run(&["tag", "show", "jazz/album/01 So What.flac"]);
    flac.assert_code(0);

    // The same fields, named the same way, whatever the container.
    for field in [
        "title",
        "artist",
        "albumartist",
        "album",
        "year",
        "track",
        "genre",
    ] {
        run.assert_stdout(field);
        flac.assert_stdout(field);
    }
    // And the audio line, which is where the container does show up.
    run.assert_stdout("mp3 ");
    flac.assert_stdout("flac ");
    // Frames MPDFM does not model are shown, marked, and not editable.
    run.assert_stdout("TBPM");
    flac.assert_stdout("REPLAYGAIN_TRACK_GAIN");
}

#[test]
fn show_on_a_directory_lists_its_audio_files_and_not_its_clutter() {
    let world = world();
    let run = world.run(&["tag", "show", "hiphop/album"]);
    run.assert_code(0);

    for track in ["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"] {
        run.assert_stdout(track);
    }
    assert!(
        !run.stdout.contains("folder.jpg") && !run.stdout.contains("release.nfo"),
        "a cover and an nfo are not audio:\n{}",
        run.stdout
    );
}

#[test]
fn show_needs_recursive_to_look_below_a_directory() {
    let world = world();

    // A multi-disc set holds no audio of its own.
    let shallow = world.run(&["tag", "show", "pop/set"]);
    shallow.assert_code(1);
    shallow.assert_stderr("-r");

    let deep = world.run(&["tag", "show", "-r", "pop/set"]);
    deep.assert_code(0);
    deep.assert_stdout("CD 1/01 Wrecked.mp3");
    deep.assert_stdout("CD 2/01 Wrecked.mp3");
}

#[test]
fn show_json_round_trips_through_a_json_parser() {
    let world = world();
    let document = world.json(&["tag", "show", "-r", "hiphop/album", "jazz/album"]);

    let files = document["files"].as_array().expect("a files array");
    assert_eq!(files.len(), 6, "{document:#}");

    for file in files {
        assert!(file["path"].is_string(), "{file:#}");
        assert!(file["error"].is_null(), "{file:#}");
        // The tags are an object of fields, and the audio info is numbers a
        // script can compare rather than a rendered string.
        assert!(file["tags"].is_object(), "{file:#}");
        assert!(file["audio"]["duration_ms"].as_u64().is_some(), "{file:#}");
        assert!(file["audio"]["sample_rate"].as_u64().is_some(), "{file:#}");
        assert!(file["audio"]["format"].is_string(), "{file:#}");
    }

    // Multi-valued fields come back as arrays, so nothing has to be unsplit.
    let titles: Vec<&str> = files
        .iter()
        .filter_map(|file| file["tags"]["title"][0].as_str())
        .collect();
    assert_eq!(titles.len(), 6, "{document:#}");
}

#[test]
fn show_reports_a_file_it_cannot_read_and_still_prints_the_others() {
    let world = world();
    fix::truncate(&world.abs("hiphop/album/02 Hoe Cakes.mp3"), 20);

    let run = world.run(&["tag", "show", "hiphop/album"]);
    // Not a success — one file could not be read — and not a crash either.
    run.assert_code(1);
    run.assert_stdout("01 Beef Rap.mp3");
    run.assert_stdout("03 Potholderz.mp3");
    run.assert_stdout("02 Hoe Cakes.mp3");
}

// ---------------------------------------------------------------------------
// `tag set`

#[test]
fn set_previews_every_file_and_commits_one_transaction() {
    let world = world();
    world.assert_hermetic();
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "set",
        "--genre",
        "Hip Hop",
        "-r",
        "hiphop/album",
        "--yes",
    ]);
    run.assert_code(0);

    // One TAG row naming the field and the count, not three rows of paths.
    run.assert_stdout("TAG");
    run.assert_stdout(r#"genre = "Hip Hop""#);
    run.assert_stdout("3 files");
    // One transaction, and the line that reverses it.
    run.assert_stdout("Undo it with `mpdfm undo ");

    for track in ["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"] {
        assert_eq!(
            genre(&world, &format!("hiphop/album/{track}")).as_deref(),
            Some("Hip Hop"),
            "{track}"
        );
    }
    before.assert_differs(&world.snapshot());
}

#[test]
fn undo_after_a_bulk_tag_set_restores_every_file_byte_for_byte() {
    let world = world();
    let before = world.snapshot();

    world
        .run(&[
            "tag",
            "set",
            "--genre",
            "Hip Hop",
            "--year",
            "1999",
            "--clear",
            "comment",
            "-r",
            "hiphop/album",
            "jazz/album",
            "--yes",
        ])
        .assert_code(0);
    before.assert_differs(&world.snapshot());

    world.run(&["undo", "--yes"]).assert_code(0);
    before.assert_same(&world.snapshot());
}

#[test]
fn dry_run_writes_nothing() {
    let world = world();
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "set",
        "--genre",
        "Hip Hop",
        "-r",
        "hiphop/album",
        "--dry-run",
    ]);
    run.assert_code(0);
    run.assert_stdout("--dry-run: nothing was changed.");

    before.assert_same(&world.snapshot());
}

#[test]
fn clearing_a_field_removes_the_frame() {
    let world = world();
    let track = "hiphop/album/01 Beef Rap.mp3";
    assert!(genre(&world, track).is_some(), "it starts with a genre");

    world
        .run(&["tag", "set", "--clear", "genre", track, "--yes"])
        .assert_code(0);

    assert_eq!(genre(&world, track), None);
    let dump = fix::dump(&world.abs(track));
    assert!(
        !dump.contains("TCON"),
        "the frame is gone, not empty:\n{dump}"
    );
    // And the cover art is still there, which is the whole point of the writer.
    assert!(dump.contains("@PICTURE"), "{dump}");
}

#[test]
fn renumber_tracks_numbers_by_the_order_the_files_are_listed() {
    let world = world();

    let run = world.run(&[
        "tag",
        "set",
        "--renumber-tracks",
        "-r",
        "hiphop/album",
        "--yes",
    ]);
    run.assert_code(0);
    // Three files, three numbers: few enough to show, so they are shown.
    run.assert_stdout(r#"track = "1/3""#);
    run.assert_stdout(r#"track = "3/3""#);
    run.assert_stdout("3 files");

    for (position, track) in ["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"]
        .iter()
        .enumerate()
    {
        let tags = tags::read_tags(&world.abs(&format!("hiphop/album/{track}"))).expect("it reads");
        assert_eq!(
            tags.track,
            Some((u32::try_from(position + 1).expect("small"), Some(3))),
            "{track}"
        );
    }
}

#[test]
fn title_from_filename_takes_each_title_from_its_own_name() {
    let world = world();

    world
        .run(&[
            "tag",
            "set",
            "--title-from-filename",
            "-r",
            "hiphop/album",
            "--yes",
        ])
        .assert_code(0);

    for (track, title) in [
        ("01 Beef Rap.mp3", "Beef Rap"),
        ("02 Hoe Cakes.mp3", "Hoe Cakes"),
        ("03 Potholderz.mp3", "Potholderz"),
    ] {
        let tags = tags::read_tags(&world.abs(&format!("hiphop/album/{track}"))).expect("it reads");
        assert_eq!(tags.title.first(), Some(title), "{track}");
    }
}

#[test]
fn a_read_only_file_refuses_the_batch_and_no_other_file_is_modified() {
    use std::os::unix::fs::PermissionsExt as _;

    let world = world();
    let locked = world.abs("hiphop/album/02 Hoe Cakes.mp3");
    let was = std::fs::metadata(locked.as_std_path())
        .expect("it is there")
        .permissions();
    std::fs::set_permissions(locked.as_std_path(), std::fs::Permissions::from_mode(0o444))
        .expect("the fixture is ours");
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "set",
        "--genre",
        "Hip Hop",
        "-r",
        "hiphop/album",
        "--yes",
    ]);
    // Refused before anything was written, which is exit 2 and not 1.
    run.assert_code(2);
    run.assert_stdout("02 Hoe Cakes.mp3");
    run.assert_stdout("Nothing was changed.");

    before.assert_same(&world.snapshot());
    std::fs::set_permissions(locked.as_std_path(), was).expect("the fixture is ours");
}

#[test]
fn a_non_tty_run_without_yes_refuses_rather_than_hanging() {
    let world = world();
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "set",
        "--genre",
        "Hip Hop",
        "hiphop/album/01 Beef Rap.mp3",
    ]);
    run.assert_code(1);
    run.assert_stderr("--yes");
    run.assert_stderr("stdin is not a terminal");

    before.assert_same(&world.snapshot());
}

#[test]
fn declining_at_the_prompt_changes_nothing() {
    let world = world();
    let before = world.snapshot();

    let run = world.answer(
        &[
            "tag",
            "set",
            "--genre",
            "Hip Hop",
            "hiphop/album/01 Beef Rap.mp3",
        ],
        "n",
    );
    run.assert_code(3);
    run.assert_stdout("Nothing was changed.");
    before.assert_same(&world.snapshot());

    // And saying yes to the same question does write.
    let yes = world.answer(
        &[
            "tag",
            "set",
            "--genre",
            "Hip Hop",
            "hiphop/album/01 Beef Rap.mp3",
        ],
        "y",
    );
    yes.assert_code(0);
    assert_eq!(
        genre(&world, "hiphop/album/01 Beef Rap.mp3").as_deref(),
        Some("Hip Hop")
    );
}

#[test]
fn a_per_file_field_cannot_be_typed_across_a_selection() {
    let world = world();
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "set",
        "--title",
        "Beef Rap",
        "-r",
        "hiphop/album",
        "--yes",
    ]);
    run.assert_code(1);
    run.assert_stderr("--title");
    run.assert_stderr("--title-from-filename");
    before.assert_same(&world.snapshot());

    // One file is exactly where typing a title is what you mean.
    world
        .run(&[
            "tag",
            "set",
            "--title",
            "Beef Rap (clean)",
            "hiphop/album/01 Beef Rap.mp3",
            "--yes",
        ])
        .assert_code(0);
}

#[test]
fn asking_for_nothing_says_so_rather_than_committing_an_empty_transaction() {
    let world = world();
    let before = world.snapshot();

    let run = world.run(&["tag", "set", "-r", "hiphop/album", "--yes"]);
    run.assert_code(1);
    run.assert_stderr("--renumber-tracks");

    // And an edit that asks for what is already there is nothing to do, not a
    // transaction that rewrites three files.
    let already = world.run(&[
        "tag",
        "set",
        "--album",
        "Mm..Food",
        "-r",
        "hiphop/album",
        "--yes",
    ]);
    already.assert_code(0);
    already.assert_stdout("Nothing to change");
    before.assert_same(&world.snapshot());
}

#[test]
fn an_unknown_field_name_lists_the_ones_there_are() {
    let world = world();
    let run = world.run(&[
        "tag",
        "set",
        "--clear",
        "bpm",
        "hiphop/album/01 Beef Rap.mp3",
        "--yes",
    ]);
    run.assert_code(1);
    run.assert_stderr("\"bpm\" is not a field");
    run.assert_stderr("albumartist");
}

#[test]
fn a_value_a_field_cannot_hold_is_refused_before_anything_is_written() {
    let world = world();
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "set",
        "--track",
        "one",
        "hiphop/album/01 Beef Rap.mp3",
        "--yes",
    ]);
    run.assert_code(2);
    run.assert_stdout("cannot be tagged");
    before.assert_same(&world.snapshot());
}

#[test]
fn set_json_reports_the_edits_and_the_transaction() {
    let world = world();
    let document = world.json(&[
        "tag",
        "set",
        "--genre",
        "Hip Hop",
        "-r",
        "hiphop/album",
        "--yes",
    ]);

    let edits = document["edits"].as_array().expect("an edits array");
    assert_eq!(edits.len(), 3, "{document:#}");
    assert_eq!(edits[0]["fields"][0]["field"], "genre");
    assert_eq!(edits[0]["fields"][0]["value"], "Hip Hop");

    assert_eq!(document["effects"]["summary"]["tags_written"], 3);
    assert_eq!(document["exit"], 0);
    assert!(
        document["committed"]["txid"].as_str().is_some(),
        "{document:#}"
    );
}

// ---------------------------------------------------------------------------
// `tag diff`

#[test]
fn diff_shows_each_file_before_and_after_and_writes_nothing() {
    let world = world();
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "diff",
        "--genre",
        "Hip Hop",
        "--clear",
        "comment",
        "-r",
        "hiphop/album",
    ]);
    run.assert_code(0);

    // The per-file half: the path, the field, and the two values.
    run.assert_stdout("hiphop/album/01 Beef Rap.mp3");
    run.assert_stdout("genre");
    run.assert_stdout("\"Hip-Hop\" → \"Hip Hop\"");
    // And the field-level preview that `set` would have printed.
    run.assert_stdout("TAG");
    run.assert_stdout("tag diff: nothing was changed.");

    before.assert_same(&world.snapshot());
}

#[test]
fn diff_marks_a_field_that_is_not_there_yet() {
    let world = world();
    let run = world.run(&[
        "tag",
        "diff",
        "--composer",
        "Daniel Dumile",
        "hiphop/album/01 Beef Rap.mp3",
    ]);
    run.assert_code(0);
    run.assert_stdout("<none> → \"Daniel Dumile\"");
}

// ---------------------------------------------------------------------------

#[test]
fn a_mixed_selection_of_containers_is_one_transaction() {
    let world = world();
    let before = world.snapshot();

    let run = world.run(&[
        "tag",
        "set",
        "--album-artist",
        "Various",
        "-r",
        "hiphop/album",
        "jazz/album",
        "coding-music/SwitchAngel/Coding_Trance.mp3",
        "--yes",
    ]);
    run.assert_code(0);
    run.assert_stdout("7 files");

    for rel in [
        "hiphop/album/01 Beef Rap.mp3",
        "jazz/album/01 So What.flac",
        "coding-music/SwitchAngel/Coding_Trance.mp3",
    ] {
        let tags = tags::read_tags(&world.abs(rel)).expect("it reads");
        assert_eq!(tags.album_artist.first(), Some("Various"), "{rel}");
    }
    before.assert_differs(&world.snapshot());

    // One transaction for all seven, so one undo puts them all back.
    world.run(&["undo", "--yes"]).assert_code(0);
    before.assert_same(&world.snapshot());
}

#[test]
fn naming_a_file_twice_is_one_edit_and_not_a_duplicate() {
    let world = world();
    let track = "hiphop/album/01 Beef Rap.mp3";

    let run = world.run(&[
        "tag",
        "set",
        "--genre",
        "Hip Hop",
        track,
        track,
        "hiphop/album",
        "--yes",
    ]);
    run.assert_code(0);
    run.assert_stdout("3 files");
    assert_eq!(genre(&world, track).as_deref(), Some("Hip Hop"));
}

#[test]
fn a_path_that_is_not_in_the_library_says_so() {
    let world = world();
    let run = world.run(&["tag", "show", "hiphop/nope.mp3"]);
    run.assert_code(1);
    run.assert_stderr("is not in the library");
}

#[test]
fn a_file_that_is_not_audio_is_refused_by_name() {
    let world = world();
    let run = world.run(&["tag", "show", "hiphop/album/release.nfo"]);
    run.assert_code(1);
    run.assert_stderr("not an audio file");
}

#[test]
fn an_absolute_path_inside_the_library_works_like_a_relative_one() {
    let world = world();
    let absolute = world.abs("hiphop/album/01 Beef Rap.mp3");
    let run = world.run(&["tag", "show", absolute.as_str()]);
    run.assert_code(0);
    run.assert_stdout("Beef Rap");
}

#[test]
fn every_tag_command_stays_inside_the_fixture() {
    let world = world();
    world.assert_hermetic();
    world.fx.assert_inside(Utf8Path::new(
        world.abs("hiphop/album/01 Beef Rap.mp3").as_str(),
    ));
    let _ = Field::Genre;
}
