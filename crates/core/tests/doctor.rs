//! Task 29: every `doctor` check, each with a fixture that triggers it and,
//! wherever the mistake is plausible, a near miss that must not.
//!
//! The near misses are the half that matters. "A doctor that cries wolf gets
//! ignored" is the task's first pitfall, and every heuristic here has a shape in
//! the real library that looks like a defect and is not: a directory of loose
//! singles has a different album on every track, a partial album has gaps above
//! its last track, an album's `Scans` directory has no audio in it.

#![cfg(unix)]

use camino::Utf8Path;
use mpdfm_core::doctor::{
    self, Check, Inputs, Options, Queue, Report, Selection, Severity, checks,
};
use mpdfm_core::library::Library;
use mpdfm_core::mpd::state::MpdState;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::{self, Field, TagDelta, WriteOpts};
use mpdfm_core::testing::tags::{set_comment, write_as};
use mpdfm_core::testing::{AudioTemplate, Fixture, names};

/// Run every check over a fixture.
fn report(fx: &Fixture) -> Report {
    report_with(fx, Options::default())
}

/// Run the checks `options` selects over a fixture.
fn report_with(fx: &Fixture, options: Options) -> Report {
    let library = Library::scan(fx.music_dir()).expect("the fixture scans");
    let (index, index_warnings) = PlaylistIndex::load(fx.playlist_dir());
    let queue = MpdState::load(fx.state_file())
        .map_or_else(|err| Queue::Unreadable(err.to_string()), Queue::Loaded);
    let inputs = Inputs {
        library: &library,
        index: &index,
        index_warnings: &index_warnings,
        queue,
    };
    doctor::run(&inputs, &options, &mut |_| {})
}

/// One check's findings, by name.
fn check<'a>(report: &'a Report, name: &str) -> &'a Check {
    report
        .check(name)
        .unwrap_or_else(|| panic!("no `{name}` check in {report:#?}"))
}

/// What one check named, as its `what` strings.
fn whats(report: &Report, name: &str) -> Vec<String> {
    check(report, name)
        .items
        .iter()
        .map(|item| item.what.clone())
        .collect()
}

/// Set some fields on one fixture track.
fn set(fx: &Fixture, rel: &str, edits: &[(Field, &str)]) {
    let delta = edits.iter().fold(TagDelta::new(), |delta, (field, value)| {
        delta.set(*field, value)
    });
    tags::write(&fx.abs(rel), &delta, &WriteOpts::new())
        .unwrap_or_else(|err| panic!("{rel} should be writable: {err}"));
}

/// Remove one field from a fixture track.
fn clear(fx: &Fixture, rel: &str, field: Field) {
    tags::write(
        &fx.abs(rel),
        &TagDelta::new().clear(field),
        &WriteOpts::new(),
    )
    .unwrap_or_else(|err| panic!("{rel} should be writable: {err}"));
}

/// An mp3 carrying an ID3v1 tag and nothing else: the untagged template with a
/// 128-byte `TAG` block on the end, built by hand because no writer MPDFM links
/// will produce a v1-only file.
fn write_id3v1_only(path: &Utf8Path, title: &str) {
    let mut bytes = AudioTemplate::Untagged.bytes().to_vec();
    let mut tag = vec![0_u8; 128];
    tag[..3].copy_from_slice(b"TAG");
    tag[3..3 + title.len()].copy_from_slice(title.as_bytes());
    tag[127] = 255; // genre: none
    bytes.extend_from_slice(&tag);
    std::fs::write(path, bytes).expect("the fixture is writable");
}

/// A four-track album whose tracks are numbered 1–4 and otherwise agree, so a
/// test can break exactly one thing about it.
fn numbered_album(dir: &str) -> Fixture {
    let tracks = ["01 a.mp3", "02 b.mp3", "03 c.mp3", "04 d.mp3"];
    let fx = Fixture::builder().album(dir, &tracks).build();
    for (at, name) in tracks.iter().enumerate() {
        set(
            &fx,
            &format!("{dir}/{name}"),
            &[
                (Field::Track, &format!("{}/4", at + 1)),
                (Field::Title, name),
            ],
        );
    }
    fx
}

// ---------------------------------------------------------------------------
// Every check, at once
// ---------------------------------------------------------------------------

/// A library with one of everything wrong with it.
fn troubled() -> Fixture {
    let fx = Fixture::builder()
        .album(
            "jazz/album",
            &["01 a.mp3", "02 b.mp3", "03 c.mp3", "04 d.mp3"],
        )
        .album("pop/same", &["01 Beef Rap.mp3"])
        // Two albums make `pop` a genre; with one it would read as a release's
        // wrapper, and the orphan below as that release's own clutter.
        .album("pop/other", &["01 x.mp3"])
        .aux("pop/no album here", &["cover.jpg", "info.nfo"])
        .aux("jazz/album/Scans", &["back.jpg"])
        .flac_album("classical/odd")
        .track("unfiled.mp3")
        .cue_reference("rock/sheet", "rock.flac.cue/track0017")
        .playlist(
            "Jazz.m3u",
            &[
                "jazz/album/01 a.mp3",
                "jazz/album/01 a.mp3",
                "jazz/gone.mp3",
                "rock/sheet/rock.flac.cue/track0003",
                "/somewhere/else.mp3",
            ],
        )
        .playlist_raw("Broken.m3u", &[0xff, 0xfe, b'\n'])
        .state_file_queue(&["jazz/album/01 a.mp3", "jazz/also-gone.mp3"])
        .non_utf8_file("weird", b"bad-\xff.mp3")
        .build();

    for (at, name) in ["01 a.mp3", "02 b.mp3", "03 c.mp3", "04 d.mp3"]
        .iter()
        .enumerate()
    {
        let number = if at == 3 { 2 } else { at + 1 }; // 1 2 3 2: a dup and no gap
        set(
            &fx,
            &format!("jazz/album/{name}"),
            &[(Field::Track, &number.to_string()), (Field::Title, name)],
        );
    }
    set(
        &fx,
        "jazz/album/04 d.mp3",
        &[(Field::Album, "Mm..Food (Remastered)")],
    );
    set_comment(&fx.abs("classical/odd/01 So What.flac"), "DATE", "20004");
    clear(&fx, "jazz/album/03 c.mp3", Field::Genre);

    write_as(&fx.abs("jazz/untagged.mp3"), AudioTemplate::Untagged);
    write_id3v1_only(&fx.abs("jazz/v1.mp3"), "Cameras");
    std::fs::write(fx.abs("jazz/garbage.mp3"), b"not audio at all").unwrap();

    std::fs::create_dir(fx.abs("jazz/empty")).unwrap();
    std::fs::create_dir(fx.abs("twins")).unwrap();
    std::fs::create_dir(fx.abs("twins/Hi\u{308}")).unwrap();
    std::fs::create_dir(fx.abs("twins/H\u{ef}")).unwrap();
    std::fs::create_dir(fx.abs("twins/Live")).unwrap();
    std::fs::create_dir(fx.abs("twins/live")).unwrap();

    // Byte-identical to every other mp3 in the fixture, which is the point.
    std::fs::copy(
        fx.abs("pop/same/01 Beef Rap.mp3"),
        fx.abs("pop/same/copy.mp3"),
    )
    .unwrap();

    let partial = fx.abs("jazz/album/05 e.mp3.parts");
    std::fs::write(&partial, b"half").unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&partial)
        .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(later)))
        .unwrap();

    let locked = fx.abs("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("x.mp3"), b"").unwrap();
    set_mode(&locked, 0o000);
    fx
}

fn set_mode(path: &Utf8Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Criterion: every check is implemented and has a fixture that triggers it.
#[test]
fn every_catalogued_check_is_implemented_and_fires_on_a_library_that_earns_it() {
    let fx = troubled();
    let report = report_with(
        &fx,
        Options {
            selection: Selection::all(),
            deep: true,
        },
    );
    // Running as root makes a mode-000 directory readable, and then there is
    // nothing for `unreadable-paths` to find; everything else must fire.
    let root = std::fs::read_dir(fx.abs("locked")).is_ok();
    set_mode(&fx.abs("locked"), 0o755); // so the fixture can be cleaned up

    assert_eq!(report.checks.len(), checks::ALL.len());
    let silent: Vec<&str> = report
        .checks
        .iter()
        .filter(|check| check.items.is_empty())
        .map(|check| check.info.name)
        .collect();
    let expected: Vec<&str> = if root {
        vec!["unreadable-paths"]
    } else {
        vec![]
    };
    assert_eq!(silent, expected, "{report:#?}");
}

/// The other half: a tidy library is not a problem, or even a warning.
#[test]
fn a_tidy_library_has_no_problems_and_no_warnings() {
    let fx = numbered_album("jazz/album");
    let report = report(&fx);
    assert_eq!(report.count(Severity::Problem), 0, "{report:#?}");
    assert_eq!(report.count(Severity::Warning), 0, "{report:#?}");
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

/// Criterion: on the realistic fixture, exactly the one broken reference — and
/// not the CUE virtual track, whose sheet, track and audio all exist.
#[test]
fn the_realistic_fixture_has_exactly_one_broken_reference() {
    let report = report(&Fixture::realistic());
    let broken = whats(&report, "broken-references");
    assert_eq!(broken.len(), 1, "{broken:#?}");
    assert!(broken[0].contains(names::BROKEN_REFERENCE), "{broken:#?}");
    assert_eq!(report.count(Severity::Problem), 1, "{report:#?}");
}

#[test]
fn a_cue_track_the_sheet_does_not_have_is_broken() {
    let fx = Fixture::builder()
        .cue_reference("rock/sheet", "rock.flac.cue/track0017")
        .playlist(
            "Rock.m3u",
            &[
                "rock/sheet/rock.flac.cue/track0017",
                "rock/sheet/rock.flac.cue/track0003",
            ],
        )
        .build();
    let report = report(&fx);
    let found = check(&report, "broken-references");
    assert_eq!(found.items.len(), 1, "{found:#?}");
    assert!(found.items[0].what.ends_with("track0003"), "{found:#?}");
    assert!(found.items[0].what.starts_with("Rock:2"), "{found:#?}");
    assert!(found.items[0].detail.contains("no track 3"), "{found:#?}");
}

#[test]
fn a_cue_track_whose_audio_file_is_gone_is_broken() {
    let fx = Fixture::builder()
        .aux("rock/sheet", &["placeholder.txt"])
        .playlist("Rock.m3u", &["rock/sheet/rock.cue/track0001"])
        .build();
    std::fs::write(
        fx.abs("rock/sheet/rock.cue"),
        "FILE \"gone.flac\" WAVE\r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n",
    )
    .unwrap();

    let report = report(&fx);
    let found = check(&report, "broken-references");
    assert_eq!(found.items.len(), 1, "{found:#?}");
    assert!(
        found.items[0].detail.contains("rock/sheet/gone.flac"),
        "{found:#?}"
    );
}

#[test]
fn a_line_outside_the_music_directory_says_so() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3"])
        .playlist("Jazz.m3u", &["/srv/elsewhere/01.mp3"])
        .build();
    let report = report(&fx);
    let found = check(&report, "unrewritable-entries");
    assert_eq!(found.items.len(), 1);
    assert!(
        found.items[0]
            .detail
            .contains("outside the music directory"),
        "{found:#?}"
    );
    assert_eq!(found.items[0].fix, None);
}

#[test]
fn a_saved_queue_entry_whose_file_is_gone_is_reported_and_a_good_one_is_not() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3"])
        .cue_reference("rock/sheet", "rock.flac.cue/track0017")
        .state_file_queue(&[
            "jazz/album/01.mp3",
            "jazz/album/02.mp3",
            "rock/sheet/rock.flac.cue/track0017",
        ])
        .build();
    let report = report(&fx);
    assert_eq!(
        whats(&report, "broken-queue-entries"),
        ["queue position 1 → jazz/album/02.mp3"]
    );
    // MPD drops it on load; nothing is broken in the sense a playlist is.
    assert_eq!(
        check(&report, "broken-queue-entries").info.severity,
        Severity::Warning
    );
}

#[test]
fn no_state_file_skips_the_queue_check_rather_than_calling_it_clean() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    let library = Library::scan(fx.music_dir()).unwrap();
    let (index, warnings) = PlaylistIndex::load(fx.playlist_dir());
    let inputs = Inputs {
        library: &library,
        index: &index,
        index_warnings: &warnings,
        queue: Queue::NotConfigured,
    };
    let report = doctor::run(&inputs, &Options::default(), &mut |_| {});
    let queue = check(&report, "broken-queue-entries");
    assert!(queue.skipped.is_some(), "{queue:#?}");
}

#[test]
fn the_same_track_twice_in_one_playlist_is_a_note_with_its_lines() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3", "02.mp3"])
        .playlist(
            "Jazz.m3u",
            &[
                "jazz/album/01.mp3",
                "jazz/album/02.mp3",
                "jazz/album/01.mp3",
            ],
        )
        // The same track in two *different* playlists is not a duplicate.
        .playlist("Other.m3u", &["jazz/album/02.mp3"])
        .build();
    let report = report(&fx);
    let found = check(&report, "duplicate-entries");
    assert_eq!(found.items.len(), 1, "{found:#?}");
    assert_eq!(found.items[0].what, "Jazz → jazz/album/01.mp3");
    assert_eq!(found.items[0].detail, "on lines 1, 3");
    assert_eq!(found.info.severity, Severity::Note);
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

/// Criterion: one track with a different `album` spelling in an otherwise
/// consistent directory is caught, and the fix names the majority's spelling.
#[test]
fn one_track_with_a_different_album_spelling_is_caught() {
    let fx = numbered_album("hiphop/MF DOOM - Mm..Food (2004)");
    set(
        &fx,
        "hiphop/MF DOOM - Mm..Food (2004)/03 c.mp3",
        &[(Field::Album, "Mm.. Food")],
    );

    let report = report(&fx);
    let found = check(&report, "inconsistent-albums");
    assert_eq!(found.items.len(), 1, "{found:#?}");
    let item = &found.items[0];
    assert_eq!(item.what, "hiphop/MF DOOM - Mm..Food (2004)");
    assert!(
        item.detail
            .contains("3 of 4 tracks have album \"Mm..Food\""),
        "{item:#?}"
    );
    assert!(
        item.detail.contains("03 c.mp3 has \"Mm.. Food\""),
        "{item:#?}"
    );
    assert_eq!(
        item.fix.as_deref(),
        Some("mpdfm tag set 'hiphop/MF DOOM - Mm..Food (2004)' --album 'Mm..Food'")
    );
}

/// The near miss: a directory of loose singles, every one from a different
/// album, is not an inconsistent album — and neither is a two-and-two split.
#[test]
fn a_directory_of_singles_is_not_an_inconsistent_album() {
    let fx = numbered_album("swedish/singles");
    for (name, album) in [
        ("01 a.mp3", "A"),
        ("02 b.mp3", "B"),
        ("03 c.mp3", "C"),
        ("04 d.mp3", "D"),
    ] {
        set(
            &fx,
            &format!("swedish/singles/{name}"),
            &[(Field::Album, album), (Field::Track, "1")],
        );
    }
    let even = numbered_album("pop/split");
    for (name, album) in [
        ("01 a.mp3", "A"),
        ("02 b.mp3", "A"),
        ("03 c.mp3", "B"),
        ("04 d.mp3", "B"),
    ] {
        set(
            &even,
            &format!("pop/split/{name}"),
            &[(Field::Album, album)],
        );
    }

    for fx in [fx, even] {
        let report = report(&fx);
        assert!(
            check(&report, "inconsistent-albums").items.is_empty(),
            "{report:#?}"
        );
        // Not an album, so its four track 1s are not duplicates either.
        assert!(
            check(&report, "track-numbers").items.is_empty(),
            "{report:#?}"
        );
    }
}

#[test]
fn a_duplicated_track_number_and_a_gap_are_reported() {
    let fx = numbered_album("jazz/album");
    set(&fx, "jazz/album/03 c.mp3", &[(Field::Track, "2")]);
    let report = report(&fx);
    assert_eq!(whats(&report, "track-numbers"), ["jazz/album"]);
    let detail = &check(&report, "track-numbers").items[0].detail;
    assert_eq!(detail, "track 2 is 02 b.mp3 and 03 c.mp3; no track 3");
}

/// The near miss: tracks 1–3 of a `/15` album are a partial album, not three
/// tracks short; and two discs in one directory both have a track 1.
#[test]
fn a_partial_album_and_a_second_disc_are_not_track_number_problems() {
    let fx = numbered_album("jazz/album");
    for (name, track, disc) in [
        ("01 a.mp3", "1/15", "1"),
        ("02 b.mp3", "2/15", "1"),
        ("03 c.mp3", "1/15", "2"),
        ("04 d.mp3", "2/15", "2"),
    ] {
        set(
            &fx,
            &format!("jazz/album/{name}"),
            &[(Field::Track, track), (Field::Disc, disc)],
        );
    }
    let report = report(&fx);
    assert!(
        check(&report, "track-numbers").items.is_empty(),
        "{report:#?}"
    );
}

#[test]
fn an_untagged_file_is_untagged_and_not_also_missing_four_fields() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    write_as(&fx.abs("jazz/album/01.mp3"), AudioTemplate::Untagged);
    let report = report(&fx);
    assert_eq!(whats(&report, "untagged"), ["jazz/album/01.mp3"]);
    assert!(
        check(&report, "missing-tags").items.is_empty(),
        "{report:#?}"
    );
    assert!(check(&report, "id3v1-only").items.is_empty(), "{report:#?}");
}

#[test]
fn an_id3v1_only_mp3_is_told_apart_from_an_untagged_one() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    write_id3v1_only(&fx.abs("jazz/album/01.mp3"), "Cameras");
    let report = report(&fx);
    assert_eq!(whats(&report, "id3v1-only"), ["jazz/album/01.mp3"]);
    assert!(check(&report, "untagged").items.is_empty(), "{report:#?}");
}

#[test]
fn missing_fields_are_named_and_a_missing_title_has_a_fix() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01 So What.mp3"])
        .build();
    clear(&fx, "jazz/album/01 So What.mp3", Field::Genre);
    clear(&fx, "jazz/album/01 So What.mp3", Field::Title);
    let report = report(&fx);
    let found = check(&report, "missing-tags");
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.items[0].detail, "no title, genre");
    assert_eq!(
        found.items[0].fix.as_deref(),
        Some("mpdfm tag set 'jazz/album/01 So What.mp3' --title-from-filename")
    );
}

#[test]
fn a_file_whose_tags_cannot_be_read_is_reported() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    std::fs::write(fx.abs("jazz/album/01.mp3"), b"this is not an mp3").unwrap();
    let report = report(&fx);
    assert_eq!(whats(&report, "unreadable-tags"), ["jazz/album/01.mp3"]);
}

/// The dates are planted as free-text FLAC `DATE` comments, the way another
/// tagger leaves them: MPDFM's own writer refuses a year that is not a date.
#[test]
fn implausible_years_are_reported_and_full_dates_are_not() {
    let fx = Fixture::builder()
        .flac_album("jazz/album")
        .album("jazz/album", &["04.flac"])
        .build();
    for (name, date) in [
        ("01 So What.flac", "20004"),
        ("02 Freddie Freeloader + alt take.flac", "1066"),
        ("03 Blue in Green.flac", "2019-03-15"),
        ("04.flac", "2004-00-00"),
    ] {
        set_comment(&fx.abs(&format!("jazz/album/{name}")), "DATE", date);
    }
    let report = report(&fx);
    assert_eq!(
        whats(&report, "implausible-years"),
        [
            "jazz/album/01 So What.flac",
            "jazz/album/02 Freddie Freeloader + alt take.flac"
        ]
    );
    assert_eq!(
        check(&report, "implausible-years").items[0].detail,
        "date \"20004\""
    );
}

// ---------------------------------------------------------------------------
// Filesystem
// ---------------------------------------------------------------------------

#[test]
fn a_name_that_is_not_utf8_is_a_problem() {
    let fx = Fixture::builder()
        .non_utf8_file("weird", b"bad-\xff.mp3")
        .build();
    let report = report(&fx);
    let found = check(&report, "bad-names");
    assert_eq!(found.items.len(), 1, "{found:#?}");
    assert!(found.items[0].what.contains("bad-"), "{found:#?}");
}

#[test]
fn names_that_differ_only_in_normalization_or_case_are_reported_once_each() {
    let fx = Fixture::builder()
        .album("electronic/KREAM - So H\u{ef}", &["01.mp3"])
        .build();
    std::fs::create_dir(fx.abs("electronic/KREAM - So Hi\u{308}")).unwrap();
    std::fs::create_dir(fx.abs("electronic/Live")).unwrap();
    std::fs::create_dir(fx.abs("electronic/live")).unwrap();

    let report = report(&fx);
    let twins = whats(&report, "normalization-twins");
    assert_eq!(twins.len(), 1, "{twins:#?}");
    assert!(twins[0].contains("So H\u{ef}") && twins[0].contains("So Hi\u{308}"));
    // `Live`/`live` is a case collision; `So Hï`/`So Hï` is not also one.
    assert_eq!(
        whats(&report, "case-collisions"),
        ["electronic/Live  ≡  electronic/live"]
    );
}

#[test]
fn a_track_in_the_root_is_unfiled_with_an_organize_fix() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3"])
        .track("Smokin' On.mp3")
        .build();
    let report = report(&fx);
    let found = check(&report, "unfiled-audio");
    assert_eq!(found.items.len(), 1);
    assert_eq!(
        found.items[0].fix.as_deref(),
        Some(r"mpdfm organize 'Smokin'\'' On.mp3'")
    );
}

/// Cover art with no album anywhere near it is an orphan; an album's own
/// `Scans` directory is a note; a multi-disc set root's `folder.jpg` is
/// nothing at all.
#[test]
fn orphan_aux_is_told_apart_from_an_albums_own_clutter() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3"])
        .aux("jazz/album/Scans", &["back.jpg"])
        .multi_disc("pop/Set", &["CD 1", "CD 2"])
        .aux("pop/Set", &["folder.jpg"])
        .aux("pop/Set/Covers", &["front.jpg"])
        .aux("pop/no album here", &["cover.jpg", "info.nfo"])
        .album("pop/Another", &["01.mp3"])
        // A torrent's wrapper: one album and its uploader's notes.
        .album("rock/Wrapper [FRG]/Album", &["01.mp3"])
        .aux(
            "rock/Wrapper [FRG]/My Uploads",
            &["Torrent downloaded from.txt"],
        )
        .build();
    let report = report(&fx);
    assert_eq!(whats(&report, "orphan-aux"), ["pop/no album here"]);
    assert_eq!(
        check(&report, "orphan-aux").items[0].detail,
        "cover.jpg, info.nfo"
    );
    assert_eq!(
        whats(&report, "no-audio-dirs"),
        [
            "jazz/album/Scans",
            "pop/Set/Covers",
            "rock/Wrapper [FRG]/My Uploads"
        ]
    );
}

#[test]
fn an_empty_directory_gets_an_absolute_quoted_rmdir() {
    let fx = Fixture::builder().album("jazz/album", &["01.mp3"]).build();
    std::fs::create_dir(fx.abs("jazz/nothing here")).unwrap();
    let report = report(&fx);
    let found = check(&report, "empty-dirs");
    assert_eq!(found.items.len(), 1);
    assert_eq!(
        found.items[0].fix.as_deref(),
        Some(format!("rmdir '{}'", fx.abs("jazz/nothing here")).as_str())
    );
    assert_eq!(found.info.severity, Severity::Warning);
}

// ---------------------------------------------------------------------------
// Duplicates
// ---------------------------------------------------------------------------

#[test]
fn the_same_artist_and_title_in_two_places_is_a_note() {
    let fx = Fixture::builder()
        .album("hiphop/Mm..Food", &["01 Beef Rap.mp3"])
        .album("hiphop/Singles", &["Beef Rap.mp3"])
        .build();
    let report = report(&fx);
    let found = check(&report, "same-song");
    assert_eq!(found.items.len(), 1, "{found:#?}");
    assert_eq!(found.items[0].what, "hiphop/Mm..Food/01 Beef Rap.mp3");
    assert_eq!(found.items[0].detail, "also hiphop/Singles/Beef Rap.mp3");
    assert_eq!(found.info.severity, Severity::Note);
}

/// Criterion: `--deep` finds a deliberately duplicated file, and nothing else
/// — the other tracks share a size with it and differ by one byte.
#[test]
fn deep_finds_a_deliberately_duplicated_file() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3", "02.mp3", "03.mp3"])
        .build();
    fx.flip_byte(&fx.abs("jazz/album/02.mp3"));
    std::fs::copy(fx.abs("jazz/album/01.mp3"), fx.abs("jazz/copy of 01.mp3")).unwrap();
    // 03 differs from 01 in its last byte only — same size, nearly the same
    // hash input — and must not be called identical.
    let mut bytes = std::fs::read(fx.abs("jazz/album/03.mp3")).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x02;
    std::fs::write(fx.abs("jazz/album/03.mp3"), bytes).unwrap();

    let deep = report_with(
        &fx,
        Options {
            selection: Selection::parse(&["identical-files"]).unwrap(),
            deep: true,
        },
    );
    let found = check(&deep, "identical-files");
    assert_eq!(found.items.len(), 1, "{found:#?}");
    assert_eq!(found.items[0].what, "jazz/album/01.mp3");
    assert_eq!(
        found.items[0].detail,
        "identical bytes to jazz/copy of 01.mp3"
    );

    // And without `--deep` it says it did not look, rather than "none".
    let shallow = report(&fx);
    let skipped = check(&shallow, "identical-files");
    assert!(
        skipped.skipped.is_some() && skipped.items.is_empty(),
        "{skipped:#?}"
    );
}

#[test]
fn deep_reports_its_progress() {
    let fx = Fixture::builder()
        .album("jazz/album", &["01.mp3", "02.mp3"])
        .build();
    let library = Library::scan(fx.music_dir()).unwrap();
    let (index, warnings) = PlaylistIndex::load(fx.playlist_dir());
    let inputs = Inputs {
        library: &library,
        index: &index,
        index_warnings: &warnings,
        queue: Queue::NotConfigured,
    };
    let mut seen = Vec::new();
    let options = Options {
        selection: Selection::parse(&["duplicates"]).unwrap(),
        deep: true,
    };
    doctor::run(&inputs, &options, &mut |progress| seen.push(progress));
    assert!(
        seen.contains(&doctor::Progress::Hashing { done: 2, total: 2 }),
        "{seen:?}"
    );
    assert!(
        seen.contains(&doctor::Progress::ReadingTags { done: 2, total: 2 }),
        "{seen:?}"
    );
}

// ---------------------------------------------------------------------------
// Selection and cost
// ---------------------------------------------------------------------------

/// Criterion: `--check tags` runs only the tag checks.
#[test]
fn selecting_tags_runs_only_the_tag_checks() {
    let report = report_with(
        &Fixture::realistic(),
        Options {
            selection: Selection::parse(&["tags"]).unwrap(),
            deep: false,
        },
    );
    assert!(!report.checks.is_empty());
    for check in &report.checks {
        assert_eq!(check.info.group, doctor::Group::Tags, "{}", check.info.name);
    }
}
