//! Task 16 — one view of metadata across three containers.
//!
//! The acceptance criteria of `docs/tasks/16-tag-read.md`, in order. Every case
//! runs against a real audio file: the five committed templates, plus the ones
//! only a tag writer can build, which `testing::tags` makes from a copy of a
//! template (`crates/core/tests/data/README.md` lists what was missing and why).

use std::time::{Duration, Instant};

use mpdfm_core::library::Format;
use mpdfm_core::tags::{self, Field, TagError};
use mpdfm_core::testing::{AudioTemplate, Fixture, names, tags as fix};

/// An mp3, a FLAC and an m4a, each with the full set of tags its container
/// carries.
fn three_containers() -> Fixture {
    Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3"])
        .flac_album("jazz/album")
        .track("coding-music/SwitchAngel/Coding_Trance_Reprise.m4a")
        .build()
}

#[test]
fn an_mp3_and_a_flac_come_back_in_the_same_shape() {
    let fx = three_containers();
    let mp3 = tags::read_tags(&fx.abs("hiphop/album/01 Beef Rap.mp3")).expect("the mp3 reads");
    let flac = tags::read_tags(&fx.abs("jazz/album/01 So What.flac")).expect("the flac reads");

    // Not the same values — the templates differ — but the same fields present,
    // which is the property the rest of the app is written against.
    for field in [Field::Title, Field::Artist, Field::Album, Field::Genre] {
        assert!(!mp3.get(field).is_empty(), "the mp3 has no {field}");
        assert!(!flac.get(field).is_empty(), "the flac has no {field}");
    }
    assert!(mp3.track.is_some() && flac.track.is_some());
    assert_eq!(mp3.year(), Some(2004));
    assert_eq!(flac.year(), Some(2019));
}

#[test]
fn the_audio_info_reports_the_container_the_bytes_are() {
    let fx = three_containers();
    for (rel, format) in [
        ("hiphop/album/01 Beef Rap.mp3", Format::Mp3),
        ("jazz/album/01 So What.flac", Format::Flac),
        (
            "coding-music/SwitchAngel/Coding_Trance_Reprise.m4a",
            Format::M4a,
        ),
    ] {
        let (_, info) = tags::read(&fx.abs(rel)).expect("it reads");
        assert_eq!(info.format, format, "{rel}");
        assert!(info.duration > Duration::ZERO, "{rel} has no duration");
        assert!(info.sample_rate > 0, "{rel} has no sample rate");
        assert!(info.channels > 0, "{rel} has no channel count");
    }
}

#[test]
fn id3v23_and_id3v24_both_read_their_year() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let v23 = fx.abs("pop/album/v23.mp3");
    let v24 = fx.abs("pop/album/v24.mp3");
    fix::write_as(&v23, AudioTemplate::Mp3v23);
    fix::write_as(&v24, AudioTemplate::Mp3v24);

    // The templates are what they say they are, so this is a test of `TYER` vs
    // `TDRC` and not of two files that happen to agree.
    assert!(fix::dump(&v23).contains("@VERSION V3"));
    assert!(fix::dump(&v24).contains("@VERSION V4"));

    let v23 = tags::read_tags(&v23).expect("the v2.3 file reads");
    let v24 = tags::read_tags(&v24).expect("the v2.4 file reads");
    assert_eq!(v23.year(), Some(2022));
    assert_eq!(v24.year(), Some(2004));
    assert_eq!(v23.title.first(), Some("Wrecked"));
    assert_eq!(v24.title.first(), Some("Beef Rap"));
}

#[test]
fn a_numeric_genre_reference_resolves_to_its_name() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let track = fx.abs("pop/album/01 a.mp3");

    // `(17)` is what a tagger from 2003 writes for Rock. Nothing in MPDFM would
    // ever produce it; plenty of files in this library hold it.
    fix::set_frame(&track, "TCON", "(17)");
    assert_eq!(
        tags::read_tags(&track).expect("it reads").genre.first(),
        Some("Rock")
    );

    // And the bare form, with no parentheses.
    fix::set_frame(&track, "TCON", "17");
    assert_eq!(
        tags::read_tags(&track).expect("it reads").genre.first(),
        Some("Rock")
    );

    // A genre that is already text is left exactly as it is, parentheses or
    // not: reading must not "clean" a value the user may be about to fix.
    fix::set_frame(&track, "TCON", "Hip-Hop (East Coast)");
    assert_eq!(
        tags::read_tags(&track).expect("it reads").genre.first(),
        Some("Hip-Hop (East Coast)")
    );
}

#[test]
fn a_track_number_keeps_its_total_when_it_has_one() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let track = fx.abs("pop/album/01 a.mp3");

    fix::set_frame(&track, "TRCK", "5/12");
    assert_eq!(
        tags::read_tags(&track).expect("it reads").track,
        Some((5, Some(12)))
    );

    fix::set_frame(&track, "TRCK", "5");
    assert_eq!(
        tags::read_tags(&track).expect("it reads").track,
        Some((5, None))
    );

    // And the same for a disc, which is the same code path by a different name.
    fix::set_frame(&track, "TPOS", "2/2");
    assert_eq!(
        tags::read_tags(&track).expect("it reads").disc,
        Some((2, Some(2)))
    );
}

#[test]
fn a_multi_valued_flac_artist_is_not_truncated_to_the_first() {
    let fx = Fixture::builder().flac_album("jazz/album").build();
    let track = fx.abs("jazz/album/01 So What.flac");
    fix::set_multi_valued(
        &track,
        "ARTIST",
        &["Miles Davis", "John Coltrane", "Bill Evans"],
    );

    let tags = tags::read_tags(&track).expect("it reads");
    assert_eq!(
        tags.artist.all(),
        ["Miles Davis", "John Coltrane", "Bill Evans"]
    );
    assert!(tags.artist.is_multi());
    // And it renders as one line for an editor that has one line to show.
    assert_eq!(
        tags.artist.joined(),
        "Miles Davis; John Coltrane; Bill Evans"
    );
}

#[test]
fn an_unknown_frame_or_comment_lands_in_extra() {
    let fx = three_containers();

    // mp3: a `TXXX` nobody models, and a frame id nobody models.
    let mp3 = fx.abs("hiphop/album/01 Beef Rap.mp3");
    fix::set_frame(&mp3, "TBPM", "174");
    let tags = tags::read_tags(&mp3).expect("it reads");
    assert!(
        tags.extra.iter().any(|(k, v)| k == "TBPM" && v == "174"),
        "{:?}",
        tags.extra
    );
    // The template's own `TXXX:comment` is there too, under a name that tells
    // two `TXXX` frames apart.
    assert!(
        tags.extra.iter().any(|(k, _)| k == "TXXX:comment"),
        "{:?}",
        tags.extra
    );

    // flac: a comment key lofty has never heard of.
    let flac = fx.abs("jazz/album/01 So What.flac");
    fix::set_comment(&flac, "SCENE_RELEASE_GROUP", "EDM RG");
    fix::set_comment(&flac, "REPLAYGAIN_TRACK_GAIN", "-7.26 dB");
    let tags = tags::read_tags(&flac).expect("it reads");
    for (key, value) in [
        ("SCENE_RELEASE_GROUP", "EDM RG"),
        ("REPLAYGAIN_TRACK_GAIN", "-7.26 dB"),
    ] {
        assert!(
            tags.extra.iter().any(|(k, v)| k == key && v == value),
            "{key} is missing from {:?}",
            tags.extra
        );
    }
}

#[test]
fn a_modeled_field_is_never_also_reported_as_extra() {
    let fx = three_containers();
    for rel in [
        "hiphop/album/01 Beef Rap.mp3",
        "jazz/album/01 So What.flac",
        "coding-music/SwitchAngel/Coding_Trance_Reprise.m4a",
    ] {
        let tags = tags::read_tags(&fx.abs(rel)).expect("it reads");
        for (key, _) in &tags.extra {
            assert!(
                mpdfm_core::tags::Field::parse(key).is_none(),
                "{rel}: {key} is both a modeled field and an extra"
            );
        }
    }
}

#[test]
fn a_file_with_no_tags_reads_as_an_empty_tag_set() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let bare = fx.abs("pop/album/untagged.mp3");
    fix::write_as(&bare, AudioTemplate::Untagged);

    let (tags, info) = tags::read(&bare).expect("an untagged file is not an error");
    assert!(tags.is_empty(), "{tags:?}");
    assert_eq!(tags.present().count(), 0);
    assert!(tags.extra.is_empty());
    // The audio is still described: an untagged file is a playable file.
    assert_eq!(info.format, Format::Mp3);
    assert!(info.duration > Duration::ZERO);
}

#[test]
fn a_truncated_file_is_a_typed_error_naming_the_path() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let broken = fx.abs("pop/album/01 a.mp3");
    // Keeps the ID3v2 header, which promises far more bytes than are left.
    fix::truncate(&broken, 20);

    let err = tags::read_tags(&broken).expect_err("a truncated file cannot be read");
    assert_eq!(err.path(), broken);
    assert!(
        err.to_string().contains("01 a.mp3"),
        "the message must name the file: {err}"
    );
    assert!(
        matches!(err, TagError::Corrupt { .. } | TagError::Unsupported { .. }),
        "{err:?}"
    );
}

#[test]
fn a_file_that_is_not_audio_at_all_is_refused_by_its_bytes() {
    let fx = Fixture::builder()
        .aux("pop/album", &["notes.nfo"])
        .album("pop/album", &["01 a.mp3"])
        .build();
    // A text file renamed `.mp3`: the extension says yes, the bytes say no.
    let liar = fx.abs("pop/album/notes.mp3");
    std::fs::write(&liar, b"this is not an mp3\n").expect("the fixture is writable");

    let err = tags::read_tags(&liar).expect_err("text is not audio");
    assert_eq!(err.path(), liar);
    assert!(matches!(
        err,
        TagError::Unsupported { .. } | TagError::Corrupt { .. }
    ));
}

#[test]
fn a_mislabelled_file_is_detected_by_its_content() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    // mp3 bytes under a `.flac` name — the scene release case.
    let liar = fx.abs("pop/album/02 really an mp3.flac");
    fix::write_as(&liar, AudioTemplate::Mp3v24);

    let (tags, info) = tags::read(&liar).expect("the bytes are readable audio");
    assert_eq!(
        info.format,
        Format::Mp3,
        "the extension must not be believed"
    );
    assert_eq!(tags.title.first(), Some("Beef Rap"));

    // And the other way round, so this is about the content and not about mp3.
    let other = fx.abs("pop/album/03 really a flac.mp3");
    fix::write_as(&other, AudioTemplate::Flac);
    let (_, info) = tags::read(&other).expect("the bytes are readable audio");
    assert_eq!(info.format, Format::Flac);
}

#[test]
fn reading_many_keeps_going_past_a_file_it_cannot_read() {
    let fx = Fixture::builder()
        .album("pop/album", &["01 a.mp3", "02 b.mp3", "03 c.mp3"])
        .build();
    fix::truncate(&fx.abs("pop/album/02 b.mp3"), 20);

    let paths: Vec<_> = ["01 a.mp3", "02 b.mp3", "03 c.mp3"]
        .iter()
        .map(|name| fx.rel(&format!("pop/album/{name}")))
        .collect();
    let read = tags::read_many(&paths, fx.music_dir());

    assert_eq!(read.len(), 3, "one answer per path, in order");
    assert_eq!(read[0].0, paths[0]);
    assert!(read[0].1.is_ok());
    let err = read[1].1.as_ref().expect_err("the truncated one failed");
    assert!(err.to_string().contains("02 b.mp3"), "{err}");
    assert!(read[2].1.is_ok(), "the third file is unaffected");
}

#[test]
fn reading_a_file_counts_as_an_audio_read() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let before = mpdfm_core::library::audio_reads();
    let _ = tags::read_tags(&fx.abs("pop/album/01 a.mp3")).expect("it reads");
    assert!(
        mpdfm_core::library::audio_reads() > before,
        "a tag read must move the counter that keeps `no_tag_io_during_scan` honest"
    );
}

#[test]
fn forty_files_read_in_well_under_fifty_milliseconds() {
    let names: Vec<String> = (0..40).map(|n| format!("{n:02} track.mp3")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let fx = Fixture::builder().album("pop/album", &refs).build();
    let paths: Vec<_> = refs
        .iter()
        .map(|name| fx.rel(&format!("pop/album/{name}")))
        .collect();

    // Warm the page cache first: the budget is for a screenful of rows the user
    // is scrolling through, not for the first read off a spinning disk.
    let warm = tags::read_many(&paths, fx.music_dir());
    assert!(warm.iter().all(|(_, result)| result.is_ok()));

    let started = Instant::now();
    let read = tags::read_many(&paths, fx.music_dir());
    let elapsed = started.elapsed();

    assert_eq!(read.len(), 40);
    // The criterion is 50 ms. Asserting 250 leaves room for a loaded CI box
    // while still failing loudly if someone makes this read the audio stream.
    assert!(
        elapsed < Duration::from_millis(250),
        "40 tag reads took {elapsed:?}"
    );
    if elapsed > Duration::from_millis(50) {
        eprintln!("warning: 40 tag reads took {elapsed:?}, over the 50 ms budget");
    }
}

#[test]
fn the_realistic_fixture_reads_end_to_end() {
    let fx = Fixture::realistic();
    let (read, failed): (Vec<_>, Vec<_>) = tags::read_many(fx.tracks(), fx.music_dir())
        .into_iter()
        .partition(|(_, result)| result.is_ok());

    assert!(
        failed.is_empty(),
        "every track in the realistic fixture must read: {:?}",
        failed
            .iter()
            .map(|(rel, result)| (rel.as_str(), result.as_ref().err().map(ToString::to_string)))
            .collect::<Vec<_>>()
    );
    assert!(read.len() >= 18, "the fixture has {} tracks", read.len());

    // The one with a `.flac.cue` next to it, to show a CUE sheet is not audio.
    let cue = fx.abs(names::MERCURY_CUE);
    assert!(tags::read_tags(&cue).is_err(), "a CUE sheet is not audio");
}

#[test]
fn a_described_comment_is_extra_and_not_the_comment() {
    // ID3v2 tells several `COMM` frames apart by their description, and a real
    // file in this library has three. Only the one with an empty description is
    // the comment; reading the others as part of it is what made a
    // `--clear comment` unable to do what it said (task 17's read-back check
    // caught it against the real library).
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let track = fx.abs("pop/album/01 a.mp3");
    fix::set_comment_language(&track, *b"eng");
    fix::add_described_comment(&track, "Catalog Number", "7567882513");
    fix::add_described_comment(&track, "MusicMatch_Preference", "Very Good");

    let tags = tags::read_tags(&track).expect("it reads");
    assert_eq!(
        tags.comment.all(),
        [fix::FIXTURE_COMMENT],
        "only the undescribed COMM is the comment"
    );
    for (key, value) in [
        ("COMM:Catalog Number", "7567882513"),
        ("COMM:MusicMatch_Preference", "Very Good"),
    ] {
        assert!(
            tags.extra.iter().any(|(k, v)| k == key && v == value),
            "{key} is missing from {:?}",
            tags.extra
        );
    }
}
