//! The playlist index's acceptance tests (task 07), one per criterion.
//!
//! The index is what every later task asks "would this move break anything?",
//! so these are about the two ways that answer can be wrong: missing a reference
//! that exists (a track in two playlists, a track listed twice, a CUE virtual
//! track) and inventing one that does not (a string prefix that is not a path
//! prefix, a radio URL that looks like a path if you squint).
//!
//! They run against the fixture library and the seventeen committed playlists,
//! because those have the shapes the real directory has — including the one
//! reference that has never resolved.

#![cfg(unix)]

use std::time::{Duration, Instant};

use mpdfm_core::library::Library;
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::{Entry, IndexWarning, PlaylistIndex, Ref};
use mpdfm_core::testing::{DOTFILES_PLAYLISTS, Fixture, PLAYLIST_TEMPLATES, names};

/// Index a fixture's playlist directory, failing the test on any warning — the
/// tests that are *about* warnings call [`PlaylistIndex::load`] themselves.
fn index(fx: &Fixture) -> PlaylistIndex {
    let (index, warnings) = PlaylistIndex::load(fx.playlist_dir());
    assert!(
        warnings.is_empty(),
        "the fixture's playlist directory should index cleanly: {warnings:?}"
    );
    index
}

/// The playlist a reference points into, by name.
fn playlist_of(index: &PlaylistIndex, reference: Ref) -> &str {
    index
        .playlist(reference.playlist)
        .unwrap_or_else(|| panic!("{reference:?} should name a playlist"))
        .name()
}

/// The exact line a reference points at.
fn line_of(index: &PlaylistIndex, reference: Ref) -> &str {
    index
        .entry(reference)
        .unwrap_or_else(|| panic!("{reference:?} should name a line"))
        .line()
}

/// How many lines of the committed playlists name exactly this path.
///
/// Counted from the bytes rather than written down, so it cannot drift when a
/// fixture playlist gains a track. A BOM and a CRLF are stripped because they
/// belong to the file, not to the path on the line.
fn lines_naming(track: &str) -> usize {
    PLAYLIST_TEMPLATES
        .iter()
        .map(|template| {
            String::from_utf8_lossy(template.bytes)
                .trim_start_matches('\u{feff}')
                .lines()
                .filter(|line| line.trim_end_matches('\r') == track)
                .count()
        })
        .sum()
}

#[test]
fn refs_to_finds_a_track_referenced_from_two_playlists() {
    let fx = Fixture::realistic();
    let index = index(&fx);
    let track = fx.rel(names::MF_DOOM_TRACK);

    let refs = index.refs_to(&track);
    assert_eq!(refs.len(), 2, "{track} is in two playlists: {refs:?}");

    let mut names_touched: Vec<&str> = refs
        .iter()
        .map(|reference| playlist_of(&index, *reference))
        .collect();
    names_touched.sort_unstable();
    assert_eq!(names_touched, ["Hip hop", "MF Doom"]);

    // Each reference points at the line that actually names the track, not just
    // at the right file.
    for reference in refs {
        assert_eq!(line_of(&index, *reference), names::MF_DOOM_TRACK);
    }

    // And the playlists that must be rewritten if it moves are those two.
    assert_eq!(
        index
            .playlists_touching(&[track])
            .iter()
            .map(|which| index.playlists()[*which].name())
            .collect::<Vec<_>>(),
        ["Hip hop", "MF Doom"]
    );
}

#[test]
fn a_track_listed_twice_in_one_playlist_yields_two_refs() {
    let fx = Fixture::builder().real_playlists().build();
    let index = index(&fx);
    let track = fx.rel(names::MF_DOOM_TRACK);

    let duplicates: Vec<Ref> = index
        .refs_to(&track)
        .iter()
        .copied()
        .filter(|reference| playlist_of(&index, *reference) == "Duplicates")
        .collect();

    assert_eq!(
        duplicates.len(),
        2,
        "Duplicates.m3u lists {track} twice: {duplicates:?}"
    );
    assert_ne!(
        duplicates[0].entry, duplicates[1].entry,
        "two references to the same line is a bug, not a duplicate"
    );
    for reference in duplicates {
        assert_eq!(line_of(&index, reference), names::MF_DOOM_TRACK);
    }
}

#[test]
fn refs_under_dir_excludes_an_album_that_merely_shares_a_string_prefix() {
    const DOOM: &str = "hiphop/MF DOOM";
    const INSTRUMENTALS: &str = "hiphop/MF DOOM Instrumentals";
    let doom_track = format!("{DOOM}/01 Beef Rap.mp3");
    let instrumental = format!("{INSTRUMENTALS}/01 Beef Rap (Instrumental).mp3");

    let fx = Fixture::builder()
        .album(DOOM, &["01 Beef Rap.mp3"])
        .album(INSTRUMENTALS, &["01 Beef Rap (Instrumental).mp3"])
        .playlist("Doom.m3u", &[&doom_track, &instrumental])
        .build();
    let index = index(&fx);

    let under = index.refs_under_dir(&fx.rel(DOOM));
    assert_eq!(
        under
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        [doom_track.as_str()],
        "a longer album name is not a subdirectory"
    );

    // The instrumentals are found by their own directory, and both by the genre.
    assert_eq!(index.refs_under_dir(&fx.rel(INSTRUMENTALS)).len(), 1);
    assert_eq!(index.refs_under_dir(&fx.rel("hiphop")).len(), 2);

    // Which is the property that matters: moving one album touches one line.
    assert_eq!(index.playlists_touching(&[fx.rel(DOOM)]), [0]);
    assert_eq!(
        index
            .refs_under_dir(&fx.rel(DOOM))
            .into_iter()
            .flat_map(|(_, refs)| refs)
            .map(|reference| line_of(&index, reference))
            .collect::<Vec<_>>(),
        [doom_track.as_str()]
    );
}

#[test]
fn broken_reports_exactly_the_one_reference_that_never_resolved() {
    let fx = Fixture::realistic();
    let index = index(&fx);
    let library = Library::scan(fx.music_dir()).expect("the fixture library should scan");

    let broken = index.broken(&library);
    assert_eq!(
        broken
            .iter()
            .map(|(_, path)| path.as_str())
            .collect::<Vec<_>>(),
        fx.broken_references(),
        "the fixture has exactly one reference that resolves to nothing"
    );

    let (reference, missing) = &broken[0];
    assert_eq!(playlist_of(&index, *reference), "Pop");
    assert_eq!(line_of(&index, *reference), missing.as_str());
    assert!(!fx.abs(missing.as_str()).exists());

    // Everything else in the same playlist resolves, the CUE virtual track
    // included: it is keyed under the `.cue` sheet, which is a file that exists.
    let cue = fx.rel(names::MERCURY_CUE);
    assert_eq!(index.refs_to(&cue).len(), 1);
    assert!(library.get(&cue).is_some());
    assert_eq!(
        index.entry(index.refs_to(&cue)[0]).and_then(Entry::cue),
        Some("track0017")
    );
}

/// The real library's one bad entry is a CUE reference, and it is bad because
/// the sheet itself is gone. The index keys the virtual track under the sheet,
/// so that is the path `broken` has to name — `…flac.cue`, not `…flac.cue/track0017`,
/// which was never a file and never could be.
#[test]
fn a_cue_virtual_track_whose_sheet_is_missing_is_broken_under_the_sheets_path() {
    let sheet = "pop/Imagine Dragons - Mercury - Acts 1.flac.cue";
    let fx = Fixture::builder()
        .album("pop/Imagine Dragons - Mercury", &["01 Wrecked.mp3"])
        .playlist("Pop.m3u", &[&format!("{sheet}/track0017")])
        .build();
    let index = index(&fx);
    let library = Library::scan(fx.music_dir()).expect("the fixture library should scan");

    let broken = index.broken(&library);
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert_eq!(broken[0].1.as_str(), sheet);
    assert_eq!(
        line_of(&index, broken[0].0),
        format!("{sheet}/track0017"),
        "the line keeps its virtual-track suffix; only the key drops it"
    );
}

/// The real library's one problem reference is a CUE virtual track whose sheet,
/// track and audio file all exist — so it is not a missing file, and `broken`
/// rightly says nothing about it. `cue_refs` is what hands it to `doctor`
/// (task 29) to check against MPD's own database instead.
#[test]
fn cue_refs_hands_the_virtual_tracks_to_doctor() {
    let fx = Fixture::realistic();
    let index = index(&fx);
    let library = Library::scan(fx.music_dir()).expect("the fixture library should scan");

    let cue_refs = index.cue_refs();
    assert_eq!(
        cue_refs
            .iter()
            .map(|(reference, path, track)| format!(
                "{}:{path}/{track}",
                playlist_of(&index, *reference)
            ))
            .collect::<Vec<_>>(),
        [format!("Pop:{}", names::MERCURY_CUE_TRACK)]
    );

    // The sheet exists, so the move engine has something to carry and `broken`
    // has nothing to report — the two questions really are different.
    let (_, sheet, _) = cue_refs[0];
    assert!(library.get(sheet).is_some());
    assert!(index.broken(&library).iter().all(|(_, path)| path != sheet));
}

#[test]
fn urls_and_comments_never_appear_in_the_index() {
    let fx = Fixture::builder().real_playlists().build();
    let index = index(&fx);

    for path in index.paths() {
        assert!(
            !path.as_str().contains("://"),
            "{path} came from a URL line"
        );
        assert!(
            !path.as_str().starts_with('#'),
            "{path} came from a comment"
        );
    }

    // Radios.m3u is nothing but `#EXT` lines, comments, blanks and URLs, so no
    // reference anywhere in the index belongs to it.
    let radios = index
        .playlists()
        .iter()
        .position(|playlist| playlist.name() == "Radios")
        .expect("the fixture set has a Radios.m3u");
    assert!(
        index
            .paths()
            .iter()
            .flat_map(|path| index.refs_to(path))
            .all(|reference| reference.playlist != radios),
        "a playlist of radio streams references no files"
    );

    // Every indexed line really is a track line, and every track line is indexed.
    let track_lines: usize = index
        .playlists()
        .iter()
        .flat_map(|playlist| playlist.entries())
        .filter(|entry| entry.rel().is_some())
        .count();
    assert_eq!(index.reference_count(), track_lines);
    assert!(track_lines > 0, "the fixture playlists do name tracks");
}

#[test]
fn a_dangling_symlink_warns_and_the_other_playlists_still_index() {
    let fx = Fixture::builder()
        .real_playlists()
        .symlinked_playlist("Radios.m3u", DOTFILES_PLAYLISTS)
        .build();

    // Break the link by deleting what it points at, the way an un-cloned
    // dotfiles repository would.
    let target = fx.root().join(DOTFILES_PLAYLISTS).join("Radios.m3u");
    std::fs::remove_file(&target).unwrap_or_else(|err| panic!("cannot remove {target}: {err}"));
    let link = fx.playlist_path("Radios.m3u");
    assert!(
        std::fs::symlink_metadata(&link).is_ok_and(|meta| meta.is_symlink()),
        "the dangling link should still be there"
    );
    assert!(!link.exists(), "and it should resolve to nothing");

    let (index, warnings) = PlaylistIndex::load(fx.playlist_dir());

    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        warnings[0].path(),
        link.as_str(),
        "the warning names the link, not the file it pointed at"
    );
    assert!(matches!(warnings[0], IndexWarning::Unreadable { .. }));

    // Carried on: the other sixteen are indexed, and so are their references —
    // every line of them, counted straight out of the committed bytes so the
    // assertion cannot drift when a fixture playlist gains a track.
    assert_eq!(index.len(), PLAYLIST_TEMPLATES.len() - 1);
    assert!(
        index
            .playlists()
            .iter()
            .all(|playlist| playlist.name() != "Radios")
    );
    assert_eq!(
        index.refs_to(&fx.rel(names::MF_DOOM_TRACK)).len(),
        lines_naming(names::MF_DOOM_TRACK)
    );
}

#[test]
fn files_that_are_not_playlists_are_ignored_by_extension() {
    let fx = Fixture::builder().real_playlists().build();
    for (name, bytes) in [
        ("folder.jpg", &b"\xff\xd8\xff\xe0not a playlist"[..]),
        ("notes.txt", b"remember to re-rip the Doom album"),
        ("README", b"these are playlists"),
    ] {
        let path = fx.playlist_dir().join(name);
        std::fs::write(&path, bytes).unwrap_or_else(|err| panic!("cannot write {path}: {err}"));
    }
    std::fs::create_dir(fx.playlist_dir().join("archive"))
        .expect("cannot create a subdirectory of the playlist directory");

    let index = index(&fx);

    assert_eq!(index.len(), PLAYLIST_TEMPLATES.len());
}

/// The benchmark named in the acceptance criteria. Numbers from this machine are
/// recorded in `docs/tasks/07-playlist-index.md`; the assertion is a regression
/// fence, not the measurement — the point of the criterion is that a full reload
/// per operation is affordable, so no cache is needed.
#[test]
fn the_seventeen_playlist_directory_indexes_in_well_under_fifty_milliseconds() {
    let fx = Fixture::builder().real_playlists().build();

    // The first load populates the page cache; the timed one is the warm load,
    // which is what every operation after the first pays.
    let cold = Instant::now();
    let (first, warnings) = PlaylistIndex::load(fx.playlist_dir());
    let cold = cold.elapsed();
    assert!(warnings.is_empty(), "{warnings:?}");

    let started = Instant::now();
    let (index, _) = PlaylistIndex::load(fx.playlist_dir());
    let elapsed = started.elapsed();

    assert_eq!(index.len(), PLAYLIST_TEMPLATES.len());
    assert_eq!(index.reference_count(), first.reference_count());

    eprintln!(
        "index of {} playlists and {} references: {:.2} ms warm ({:.2} ms cold)",
        index.len(),
        index.reference_count(),
        elapsed.as_secs_f64() * 1000.0,
        cold.as_secs_f64() * 1000.0,
    );
    assert!(
        elapsed < Duration::from_millis(50),
        "indexing {} playlists took {elapsed:?}",
        index.len()
    );
}

/// Every reference the index hands out must resolve inside the index it came
/// from — a `Ref` that points at nothing would make task 09 rewrite nothing and
/// report success.
#[test]
fn every_reference_resolves_to_the_line_it_indexes() {
    let fx = Fixture::builder().real_playlists().build();
    let index = index(&fx);

    for path in index.paths() {
        for reference in index.refs_to(path) {
            let entry = index
                .entry(*reference)
                .unwrap_or_else(|| panic!("{reference:?} should resolve"));
            assert_eq!(
                entry.rel(),
                Some(path),
                "{reference:?} indexes another path"
            );
        }
    }
}

/// Task 09 takes the playlists out to rewrite them, and the index is consumed
/// in the process rather than left describing lines that have changed.
#[test]
fn the_index_hands_its_playlists_over_intact() {
    let fx = Fixture::realistic();
    let index = index(&fx);
    let expected: Vec<(String, Vec<u8>)> = index
        .playlists()
        .iter()
        .map(|playlist| (playlist.name().to_owned(), playlist.to_bytes()))
        .collect();

    let playlists = index.into_playlists();

    assert_eq!(
        playlists
            .iter()
            .map(|playlist| (playlist.name().to_owned(), playlist.to_bytes()))
            .collect::<Vec<_>>(),
        expected
    );
    // Loaded, not built from bytes: the symlinked playlist knows its target, so
    // a rewrite edits the dotfiles file and leaves the link alone.
    let radios = playlists
        .iter()
        .find(|playlist| playlist.name() == "Radios")
        .expect("the realistic fixture has a symlinked Radios.m3u");
    assert_ne!(radios.path(), radios.real_path());
}

/// `RelPath` is byte-wise: two spellings of the same visible name are two
/// different tracks, and the index must not quietly merge them.
#[test]
fn identity_is_byte_wise_in_the_index_too() {
    let nfc = "electronic/So H\u{ef}.mp3";
    let nfd = "electronic/So Hi\u{308}.mp3";
    let fx = Fixture::builder()
        .playlist("Both.m3u", &[nfc, nfd, "Electronic/So H\u{ef}.mp3"])
        .build();
    let index = index(&fx);

    assert_eq!(index.paths().len(), 3);
    assert_eq!(index.refs_to(&fx.rel(nfc)).len(), 1);
    assert_eq!(index.refs_to(&fx.rel(nfd)).len(), 1);
    // And case is not folded: ext4 would call these two different directories.
    assert!(
        index
            .refs_under_dir(&RelPath::parse("Electronic").expect("a valid directory"))
            .len()
            == 1
    );
}
