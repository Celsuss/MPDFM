//! The library-wide search, against a real library on disk.
//!
//! The unit tests in `src/query.rs` cover the grammar; what is here is the walk
//! — how many files it opens, that it can be called off, and that the two
//! workflows the task exists for actually work: "find every file with no genre"
//! and "find everything by this artist".

use mpdfm_core::library::Library;
use mpdfm_core::query::{self, Flow};
use mpdfm_core::tags::{Field, TagDelta, WriteOpts};
use mpdfm_core::testing::Fixture;

/// A library with three albums in it, tagged on purpose.
///
/// The fixture's audio templates come with tags of their own — every mp3 says
/// `MF DOOM` and every FLAC says `KREAM` — so this writes what each album is
/// supposed to say rather than assuming. Two albums are left with **no genre**,
/// which is what `missing:genre` is here to find.
fn library() -> (Fixture, Library) {
    let fixture = Fixture::builder()
        .album(
            "hiphop/MF DOOM - Mm..Food",
            &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"],
        )
        .aux("hiphop/MF DOOM - Mm..Food", &["folder.jpg", "info.nfo"])
        .flac_album("jazz/Miles Davis - Kind of Blue")
        .non_ascii_album("electronic/KREAM - So Hï")
        .build();

    let tagged: &[(&str, TagDelta)] = &[
        (
            "hiphop/MF DOOM - Mm..Food",
            TagDelta::new()
                .set(Field::Artist, "MF DOOM")
                .set(Field::Genre, "Hip Hop"),
        ),
        (
            "jazz/Miles Davis - Kind of Blue",
            TagDelta::new()
                .set(Field::Artist, "Miles Davis")
                .clear(Field::Genre),
        ),
        (
            "electronic/KREAM - So Hï",
            TagDelta::new()
                .set(Field::Artist, "KREAM")
                .clear(Field::Genre),
        ),
    ];
    for (dir, delta) in tagged {
        for track in fixture.tracks() {
            if track.as_str().starts_with(dir) {
                mpdfm_core::tags::write(
                    &track.to_abs(fixture.music_dir()),
                    delta,
                    &WriteOpts::new(),
                )
                .expect("the fixture is ours to write");
            }
        }
    }

    let scanned = Library::scan(fixture.music_dir()).expect("the fixture scans");
    (fixture, scanned)
}

/// The paths a query matched, as strings, in order.
fn hits(query: &str, library: &Library) -> Vec<String> {
    found(query, library)
        .hits
        .into_iter()
        .map(|hit| hit.rel.as_str().to_owned())
        .collect()
}

/// Everything a query produced, including how many files it had to open.
///
/// `Found::read` and not
/// [`library::audio_reads`][mpdfm_core::library::audio_reads] is what the "it
/// opened nothing" assertions below count: the global counter is process-wide
/// and these tests run in parallel, so a delta across one call is not this
/// call's.
fn found(query: &str, library: &Library) -> query::Found {
    let parsed = query::parse(query).expect("the query parses");
    query::find_all(&parsed, library)
}

#[test]
fn a_query_that_needs_no_tags_opens_no_files() {
    let (_fixture, library) = library();

    let flacs = found("ext:flac", &library);
    assert_eq!(
        flacs.read, 0,
        "`ext:` is answered from the name; opening a file for it would be waste"
    );
    assert!(!flacs.hits.is_empty(), "the fixture has FLACs in it");
    assert!(
        flacs
            .hits
            .iter()
            .all(|hit| hit.rel.as_str().ends_with(".flac")),
        "{:?}",
        flacs.hits
    );
    assert!(
        flacs.hits.iter().all(|hit| hit.tags.is_none()),
        "nothing was opened, so nothing came back read"
    );
}

#[test]
fn missing_genre_finds_the_untagged_files_and_only_the_audio_ones() {
    let (_fixture, library) = library();

    let untagged = hits("missing:genre", &library);
    assert!(
        untagged.iter().all(|path| !path.contains("MF DOOM")),
        "the hip hop album has a genre: {untagged:?}"
    );
    assert!(
        untagged.iter().any(|path| path.contains("Kind of Blue")),
        "the FLAC album has none: {untagged:?}"
    );
    assert!(
        !untagged.iter().any(|path| path.ends_with(".jpg")),
        "a cover image has no genre to be missing: {untagged:?}"
    );

    // This is the workflow the task says makes the feature worth building.
    let total_audio = library.entries().iter().filter(|e| e.is_audio()).count();
    assert_eq!(
        untagged.len(),
        total_audio - 2,
        "every audio file but the two that were tagged"
    );
}

#[test]
fn a_tag_term_finds_what_the_name_does_not_say() {
    let (_fixture, library) = library();

    // The file names say nothing about the artist; the tags do.
    let by_artist = hits("artist:doom", &library);
    assert_eq!(by_artist.len(), 2, "{by_artist:?}");
    assert!(by_artist.iter().all(|path| path.ends_with(".mp3")));

    // And the hits carry what was read, so the caller need not read it again.
    let parsed = query::parse("artist:doom").expect("parses");
    let found = query::find_all(&parsed, &library);
    for hit in &found.hits {
        let tags = hit.tags.as_ref().expect("a tag term had to open the file");
        assert_eq!(tags.artist.first(), Some("MF DOOM"));
        assert!(
            hit.info.is_some(),
            "the audio properties come from the same open"
        );
    }
}

#[test]
fn a_bare_word_matches_the_path_so_an_untagged_library_is_still_searchable() {
    let (_fixture, library) = library();

    let doom = hits("doom", &library);
    assert_eq!(
        doom.len(),
        4,
        "two tracks and two aux files live under that directory: {doom:?}"
    );

    // Smart case, over the whole library: `MF DOOM` is spelled in capitals, so
    // a mixed-case pattern finds nothing and the exact one finds all four.
    assert!(
        hits("Doom", &library).is_empty(),
        "the directory is `MF DOOM`"
    );
    assert_eq!(hits("MF DOOM", &library).len(), 4);
}

#[test]
fn a_non_ascii_query_finds_a_non_ascii_name() {
    let (_fixture, library) = library();

    let hi = hits("so hï", &library);
    assert!(
        hi.iter().any(|path| path.ends_with("01 So Hï.mp3")),
        "{hi:?}"
    );
    assert_eq!(hits("ノスタルジア", &library).len(), 1);
}

#[test]
fn a_query_nothing_matches_comes_back_empty_rather_than_failing() {
    let (_fixture, library) = library();

    let found = query::find_all(&query::parse("artist:nobody").unwrap(), &library);
    assert!(found.hits.is_empty());
    assert!(found.failed.is_empty());
    assert!(!found.cancelled);
    assert_eq!(
        found.scanned,
        library.len(),
        "it still looked at everything"
    );
}

#[test]
fn a_search_reports_as_it_goes_and_can_be_called_off() {
    let (_fixture, library) = library();
    let query = query::parse("missing:genre").expect("parses");

    let mut seen = Vec::new();
    let found = query::find(&query, &library, &mut |progress| {
        seen.push(*progress);
        Flow::Go
    });
    assert!(
        seen.len() >= 2,
        "once at the start and once at the end, at least: {seen:?}"
    );
    let last = seen.last().expect("there is one");
    assert_eq!(last.scanned, library.len());
    assert_eq!(last.total, library.len());
    assert_eq!(last.percent(), 100);
    assert_eq!(last.hits, found.hits.len());
    assert!(last.read > 0, "`missing:` has to open files");

    // Stopping at the first report leaves the walk where it was, and what was
    // found before it is still returned.
    let stopped = query::find(&query, &library, &mut |_| Flow::Stop);
    assert!(stopped.cancelled);
    assert!(stopped.scanned < library.len());
}

#[test]
fn a_file_whose_tags_will_not_read_is_reported_rather_than_silently_dropped() {
    let (fixture, _) = library();
    // A truncated file: the container is recognizable and the tag is not.
    mpdfm_core::testing::tags::truncate(
        &fixture.abs("jazz/Miles Davis - Kind of Blue/01 So What.flac"),
        40,
    );

    let library = Library::scan(fixture.music_dir()).expect("it still scans");
    let found = query::find_all(&query::parse("missing:genre").unwrap(), &library);

    assert_eq!(found.failed.len(), 1, "{:?}", found.failed);
    let (rel, message) = &found.failed[0];
    assert!(rel.as_str().ends_with("01 So What.flac"), "{rel}");
    assert!(
        message.contains("So What"),
        "the message names the file: {message}"
    );
    assert!(
        !found.hits.iter().any(|hit| &hit.rel == rel),
        "a file nobody could read is not a hit"
    );
}

#[test]
fn two_terms_narrow_rather_than_widen() {
    let (_fixture, library) = library();

    let both = hits("ext:mp3 artist:doom", &library);
    assert_eq!(both.len(), 2, "{both:?}");

    // And the cheap half rejects before anything is opened: nothing but a FLAC
    // could still have matched, so nothing but a FLAC was opened.
    let none = found("ext:flac artist:doom", &library);
    assert!(none.hits.is_empty());
    let flacs = library
        .entries()
        .iter()
        .filter(|entry| entry.rel.as_str().ends_with(".flac"))
        .count();
    assert_eq!(
        none.read, flacs,
        "the mp3s were rejected on their extension"
    );
}
