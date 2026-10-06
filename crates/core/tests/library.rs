//! The scanner's acceptance tests (task 05), one per criterion.
//!
//! They run against the fixture library rather than a hand-made directory,
//! because the cases that break a scanner are the ones the real library has:
//! non-ASCII names, a `.m3u` sitting inside an album, a multi-disc set, scene
//! clutter that must not be dropped. Reaching `mpdfm_core::testing` from outside
//! the crate is also how every later task will use it.
//!
//! Three of these tests make the filesystem hostile — an unreadable directory,
//! unreadable files, symlinks — and then put it back, so the fixture can still
//! clean itself up. Each one checks first that the hostility took effect: run as
//! root, `chmod 000` stops nothing, and a test that silently proved nothing is
//! worse than one that says it was skipped.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

use camino::Utf8Path;
use mpdfm_core::library::{DirPath, Format, Kind, Library, ScanProgress, ScanWarning};
use mpdfm_core::testing::{Fixture, names};

/// Scan a fixture's music directory, failing the test if the root itself is
/// unusable.
fn scan(fx: &Fixture) -> Library {
    Library::scan(fx.music_dir()).expect("the fixture's music directory should scan")
}

fn set_mode(path: &Utf8Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|err| panic!("cannot chmod {path}: {err}"));
}

/// The entry kind at a path, by identity.
fn kind_of(library: &Library, fx: &Fixture, rel: &str) -> Kind {
    library
        .get(&fx.rel(rel))
        .unwrap_or_else(|| panic!("{rel} should be in the library"))
        .kind
}

#[test]
fn scans_the_fixture_and_classifies_every_file() {
    let fx = Fixture::realistic();
    let library = scan(&fx);

    assert!(
        library.warnings().is_empty(),
        "the fixture is clean, so a scan of it should warn about nothing: {:?}",
        library.warnings()
    );

    // Nothing is dropped and nothing is invented: exactly the files the builder
    // wrote, in byte order.
    let mut expected: Vec<String> = fx
        .tracks()
        .iter()
        .chain(fx.aux_files())
        .map(ToString::to_string)
        .collect();
    expected.sort();
    let found: Vec<String> = library
        .entries()
        .iter()
        .map(|entry| entry.rel.to_string())
        .collect();
    assert_eq!(found, expected);

    // Audio, by container.
    assert_eq!(
        kind_of(&library, &fx, names::MF_DOOM_TRACK),
        Kind::Audio(Format::Mp3)
    );
    assert_eq!(
        kind_of(&library, &fx, names::KIND_OF_BLUE_TRACK),
        Kind::Audio(Format::Flac)
    );
    assert_eq!(
        kind_of(&library, &fx, names::SWITCHANGEL_M4A),
        Kind::Audio(Format::M4a)
    );
    // A non-ASCII name is an ordinary track, not a special case.
    assert_eq!(
        kind_of(&library, &fx, names::KREAM_TRACK),
        Kind::Audio(Format::Mp3)
    );

    // The clutter that has to travel with an album, each in its own kind — and
    // the `.m3u` inside the album classified as clutter, not as one of MPD's
    // playlists.
    let album = names::MF_DOOM_ALBUM;
    for (name, expected) in [
        ("folder.jpg", Kind::Image),
        ("info.nfo", Kind::Sidecar),
        ("mm..food.sfv", Kind::Sidecar),
        ("eac.log", Kind::Sidecar),
        ("Mm..Food.m3u", Kind::Playlist),
    ] {
        assert_eq!(
            kind_of(&library, &fx, &format!("{album}/{name}")),
            expected,
            "wrong kind for {name}"
        );
    }
    assert_eq!(kind_of(&library, &fx, names::MERCURY_CUE), Kind::Cue);

    // The builder's own idea of what is audio agrees with the scanner's.
    for track in fx.tracks() {
        assert!(
            library
                .get(track)
                .expect("a track should be scanned")
                .is_audio(),
            "{track} should be audio"
        );
    }
    for aux in fx.aux_files() {
        assert!(
            !library
                .get(aux)
                .expect("an aux file should be scanned")
                .is_audio(),
            "{aux} should not be audio"
        );
    }

    // Counts, derived from the fixture rather than hard-coded, so adding a file
    // to `realistic()` does not falsify this.
    let counts = library.counts();
    let with_extension = |ext: &str| {
        fx.tracks()
            .iter()
            .filter(|rel| rel.extension() == Some(ext))
            .count()
    };
    assert_eq!(counts.mp3, with_extension("mp3"));
    assert_eq!(counts.flac, with_extension("flac"));
    assert_eq!(counts.m4a, with_extension("m4a"));
    assert_eq!(counts.audio(), fx.tracks().len());
    assert_eq!(counts.total(), library.len());
    assert_eq!(counts.other, 0, "every fixture file should be classified");
}

#[test]
fn size_and_mtime_are_recorded_for_the_undo_check() {
    // Task 12 verifies a file has not changed before reversing a move, and can
    // only do that against what the scan saw.
    let fx = Fixture::realistic();
    let library = scan(&fx);

    let path = fx.abs(names::MF_DOOM_TRACK);
    let metadata = fs::metadata(&path).expect("the fixture track exists");
    let entry = library
        .get(&fx.rel(names::MF_DOOM_TRACK))
        .expect("the track should be scanned");

    assert_eq!(entry.size, metadata.len());
    assert!(entry.size > 0, "a fixture track is not empty");
    assert_eq!(entry.mtime, metadata.modified().expect("ext4 has mtimes"));
}

#[test]
fn a_name_that_is_not_utf8_is_reported_and_the_scan_continues() {
    let fx = Fixture::builder()
        .album(names::MF_DOOM_ALBUM, &["01 Beef Rap.mp3"])
        // Latin-1 `ï` — the byte a real `So Hï.mp3` would have in a filesystem
        // whose names were never UTF-8.
        .non_utf8_file("electronic", b"So H\xEF.mp3")
        .build();

    let library = scan(&fx);

    let lossy = library
        .warnings()
        .iter()
        .find_map(|warning| match warning {
            ScanWarning::NotUtf8 { lossy } => Some(lossy.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a NotUtf8 warning, got {:?}", library.warnings()));
    assert!(
        lossy.contains("So H"),
        "the warning should name it: {lossy}"
    );

    // The scan continued: the readable file is still modelled, and the unnamable
    // one is not.
    assert_eq!(library.len(), 1);
    assert!(library.get(&fx.rel(names::MF_DOOM_TRACK)).is_some());
    assert!(
        library
            .entries()
            .iter()
            .all(|entry| entry.rel.as_str() != "electronic/So H?.mp3"),
        "a lossy rendering must never become an identity"
    );
}

#[test]
fn a_directory_that_cannot_be_read_is_reported_and_the_scan_continues() {
    let fx = Fixture::realistic();
    let closed = fx.abs(names::KREAM_ALBUM);
    set_mode(&closed, 0o000);

    if fs::read_dir(&closed).is_ok() {
        // Running as root: the chmod proves nothing, so assert nothing.
        set_mode(&closed, 0o755);
        eprintln!("skipped: running as root, an unreadable directory cannot be simulated");
        return;
    }

    let library = scan(&fx);
    set_mode(&closed, 0o755);

    let reported = library.warnings().iter().any(|warning| match warning {
        ScanWarning::Unreadable { path, .. } => path == closed.as_str(),
        _ => false,
    });
    assert!(
        reported,
        "expected an Unreadable warning for {closed}, got {:?}",
        library.warnings()
    );

    // The rest of the library is intact, and the directory itself is still known
    // — it is its contents that are missing, and the warning says so.
    assert!(library.get(&fx.rel(names::MF_DOOM_TRACK)).is_some());
    let album = DirPath::parse(names::KREAM_ALBUM).unwrap();
    assert!(library.dir(&album).is_some());
    assert!(library.files_in(&album).next().is_none());
    assert!(library.album_dir(&album).is_none());
}

#[test]
fn a_symlink_is_reported_and_never_traversed() {
    let fx = Fixture::realistic();
    // A link to a directory inside the library, and a link to one file. Both are
    // legal on ext4 and neither may end up in the model: following the first is
    // how a walk loops, and MPDFM does not move what it has not resolved.
    let dir_link = fx.music_dir().join("electronic/link-to-jazz");
    let file_link = fx.music_dir().join("electronic/link-to-track.mp3");
    std::os::unix::fs::symlink(fx.abs(names::KIND_OF_BLUE_ALBUM), &dir_link).expect("symlink");
    std::os::unix::fs::symlink(fx.abs(names::MF_DOOM_TRACK), &file_link).expect("symlink");

    let library = scan(&fx);

    let reported: Vec<&str> = library
        .warnings()
        .iter()
        .filter_map(|warning| match warning {
            ScanWarning::Symlink { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reported,
        ["electronic/link-to-jazz", "electronic/link-to-track.mp3"],
        "both links should be reported, got {:?}",
        library.warnings()
    );

    // Not traversed: nothing under the link, and the link itself is neither an
    // entry nor a directory of the library.
    assert!(
        library
            .entries()
            .iter()
            .all(|entry| !entry.rel.as_str().starts_with("electronic/link-to")),
        "a symlink must not become an entry"
    );
    assert!(
        library
            .dir(&DirPath::parse("electronic/link-to-jazz").unwrap())
            .is_none()
    );
    assert!(
        !library
            .subdirs_in(&DirPath::parse("electronic").unwrap())
            .iter()
            .any(|dir| dir.as_str() == "electronic/link-to-jazz")
    );

    // And the link's target is still modelled exactly once, through its real
    // path.
    assert!(library.get(&fx.rel(names::KIND_OF_BLUE_TRACK)).is_some());
    assert_eq!(
        library
            .entries()
            .iter()
            .filter(|entry| entry.file_name() == "01 So What.flac")
            .count(),
        1
    );
}

#[test]
fn album_dirs_find_the_discs_of_the_multi_disc_set_and_link_them_to_it() {
    let fx = Fixture::realistic();
    let library = scan(&fx);

    let set = DirPath::parse(names::MERCURY_ALBUM).unwrap();
    let cd1 = DirPath::parse(names::MERCURY_CD1).unwrap();
    let cd2 = DirPath::parse(names::MERCURY_CD2).unwrap();

    for disc in [&cd1, &cd2] {
        let album = library
            .album_dir(disc)
            .unwrap_or_else(|| panic!("{disc} should be an album directory"));
        assert!(album.audio > 0);
        assert_eq!(
            album.set_root.as_ref(),
            Some(&set),
            "{disc} should point back at the set root"
        );
    }
    assert_eq!(
        library
            .discs_of(&set)
            .map(|album| album.dir.as_str())
            .collect::<Vec<_>>(),
        [cd1.as_str(), cd2.as_str()]
    );

    // The set root holds no audio of its own, so it is not an album directory —
    // it is reached through its discs. Its own subdirectories are the discs.
    assert!(library.album_dir(&set).is_none());
    assert_eq!(
        library
            .subdirs_in(&set)
            .iter()
            .map(DirPath::as_str)
            .collect::<Vec<_>>(),
        [cd1.as_str(), cd2.as_str()]
    );

    // An ordinary album is an album directory belonging to no set: its parent is
    // a genre directory, which is not part of the release.
    let ordinary = library
        .album_dir(&DirPath::parse(names::MF_DOOM_ALBUM).unwrap())
        .expect("the MF DOOM album is an album directory");
    assert!(!ordinary.is_disc());
    assert!(
        library
            .discs_of(&DirPath::parse("hiphop").unwrap())
            .next()
            .is_none()
    );

    // Every album directory the fixture has, and nothing that only holds
    // clutter or other directories.
    let mut albums: Vec<&str> = library
        .album_dirs()
        .iter()
        .map(|album| album.dir.as_str())
        .collect();
    albums.sort_unstable();
    assert_eq!(
        albums,
        [
            "coding-music/SwitchAngel",
            names::KREAM_ALBUM,
            names::MF_DOOM_ALBUM,
            names::SNOOP_ALBUM,
            names::KIND_OF_BLUE_ALBUM,
            names::MERCURY_CD1,
            names::MERCURY_CD2,
        ]
    );
}

#[test]
fn the_browser_reads_the_index_and_not_the_disk() {
    let fx = Fixture::realistic();
    let library = scan(&fx);

    // The harshest possible version of "without re-walking the disk": there is no
    // disk left to walk.
    fs::remove_dir_all(fx.music_dir()).expect("the music directory can be removed");

    // The top level is the genre directories, in order, with no files.
    let root = DirPath::root();
    assert_eq!(
        library
            .subdirs_in(&root)
            .iter()
            .map(DirPath::as_str)
            .collect::<Vec<_>>(),
        ["coding-music", "electronic", "hiphop", "jazz", "pop"]
    );
    assert!(library.files_in(&root).next().is_none());

    // One album's files, in order, with their kinds.
    let album = DirPath::parse(names::MF_DOOM_ALBUM).unwrap();
    assert_eq!(
        library
            .files_in(&album)
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>(),
        [
            "01 Beef Rap.mp3",
            "02 Hoe Cakes.mp3",
            "03 Potholderz (feat. Count Bass D).mp3",
            "Mm..Food.m3u",
            "eac.log",
            "folder.jpg",
            "info.nfo",
            "mm..food.sfv",
        ]
    );

    // The indices the browser would hand to the lazy tag reader resolve to the
    // same entries.
    for &index in library.indices_in(&album) {
        let entry = library
            .entry(index)
            .expect("an index from the library resolves");
        assert_eq!(library.index_of(&entry.rel), Some(index));
    }

    // And the derived views survive too.
    assert!(library.album_dir(&album).is_some());
    assert_eq!(library.album_dirs().len(), 7);
    assert!(library.get(&fx.rel(names::MERCURY_TRACK)).is_some());
}

#[test]
fn no_tag_io_happens_during_a_scan() {
    let fx = Fixture::realistic();

    // Two independent proofs, because either alone is weak. First: make every
    // audio file's *contents* unreadable. `stat` still works through a readable
    // directory, so a scan that only stats is untouched, while anything that
    // opened a file would fail and warn.
    for track in fx.tracks() {
        set_mode(&track.to_abs(fx.music_dir()), 0o000);
    }
    let probe = fx.tracks()[0].to_abs(fx.music_dir());
    let as_root = fs::read(&probe).is_ok();

    // Second: the counter every audio-content read in core increments. Today
    // nothing does; task 16's lazy tag reader will, which is what keeps this
    // assertion meaningful once tags exist.
    let before = mpdfm_core::library::audio_reads();
    let library = scan(&fx);
    let reads = mpdfm_core::library::audio_reads() - before;

    for track in fx.tracks() {
        set_mode(&track.to_abs(fx.music_dir()), 0o644);
    }

    assert_eq!(reads, 0, "a scan must not read any audio file's contents");
    if as_root {
        eprintln!("note: running as root, so only the counter half of this test proved anything");
        return;
    }
    assert!(
        library.warnings().is_empty(),
        "a scan of unreadable files should still warn about nothing: {:?}",
        library.warnings()
    );
    assert_eq!(library.counts().audio(), fx.tracks().len());
    // `stat` answered, so the data task 12 needs is there even for a file whose
    // contents could not be read.
    assert!(library.entries().iter().all(|entry| entry.size > 0));
}

/// The benchmark named in the acceptance criteria. Numbers from this machine are
/// recorded in `docs/tasks/05-library-scanner.md`; the assertion is loose on
/// purpose — it is a regression fence, not the measurement.
#[test]
fn a_three_thousand_file_library_scans_in_well_under_a_second() {
    const GENRES: usize = 10;
    const ALBUMS: usize = 15;
    const TRACKS: usize = 18;
    const AUX: &[&str] = &["folder.jpg", "info.nfo"];
    let expected = GENRES * ALBUMS * (TRACKS + AUX.len());

    let built = Instant::now();
    let mut builder = Fixture::builder();
    for genre in 0..GENRES {
        for album in 0..ALBUMS {
            // Scene-style names, so the walk pays the real cost of long
            // non-trivial paths rather than of `a/b/c`.
            let dir =
                format!("genre-{genre:02}/Artist {album:02} - Album {album:02} (2011) [V0] scene");
            let names: Vec<String> = (1..=TRACKS)
                .map(|track| format!("{track:02} Track Tänd {track:02}.mp3"))
                .collect();
            let tracks: Vec<&str> = names.iter().map(String::as_str).collect();
            builder = builder.album(&dir, &tracks).aux(&dir, AUX);
        }
    }
    let fx = builder.build();
    let built = built.elapsed();

    // The first scan populates the page cache; the one that is timed is the warm
    // one, which is what MPDFM's startup scan is on every run but the first.
    let cold = Instant::now();
    let first = scan(&fx);
    let cold = cold.elapsed();
    assert_eq!(first.len(), expected);

    let started = Instant::now();
    let library = scan(&fx);
    let elapsed = started.elapsed();

    assert_eq!(library.len(), expected);
    assert_eq!(library.album_dirs().len(), GENRES * ALBUMS);
    assert_eq!(library.dir_count(), 1 + GENRES + GENRES * ALBUMS);
    assert!(library.warnings().is_empty());

    eprintln!(
        "scan of {} files in {} dirs: {:.1} ms warm ({:.1} ms cold, {:.0} ms to build the tree)",
        library.len(),
        library.dir_count(),
        elapsed.as_secs_f64() * 1000.0,
        cold.as_secs_f64() * 1000.0,
        built.as_secs_f64() * 1000.0,
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "a warm scan of {} files took {elapsed:?}, which is not well under a second",
        library.len()
    );
}

/// Task 20 draws a progress line while the scan runs on a worker thread. The
/// property it needs is that the counts arrive *during* the walk and that the
/// last one is the truth — not that any particular number of calls happens.
#[test]
fn a_scan_reports_its_progress_as_it_goes_and_ends_on_the_total() {
    let fx = Fixture::realistic();

    let mut seen: Vec<ScanProgress> = Vec::new();
    let library = Library::scan_reporting(fx.music_dir(), &mut |progress| {
        seen.push(progress.clone());
    })
    .expect("the fixture's music directory should scan");

    let last = seen
        .last()
        .expect("a non-empty library should report progress");
    assert_eq!(last.files, library.len());
    assert_eq!(last.dirs, library.dir_count());
    // Every report names a directory that is really in the library, which is
    // what makes it safe to put on screen.
    for progress in &seen {
        assert!(
            library.dir(&progress.dir).is_some(),
            "progress named {} , which is not a directory of the library",
            progress.dir
        );
    }

    // Monotonic, so a progress line never counts backwards.
    for pair in seen.windows(2) {
        assert!(
            pair[1].files > pair[0].files,
            "{:?} then {:?}",
            pair[0],
            pair[1]
        );
        assert!(pair[1].dirs >= pair[0].dirs);
    }
}

/// The counterpart: a walk with nothing to report says nothing, rather than
/// reporting a zero that would make a progress line flash up and vanish.
#[test]
fn an_empty_library_reports_no_progress_at_all() {
    let fx = Fixture::builder().build();
    let mut calls = 0usize;
    let library = Library::scan_reporting(fx.music_dir(), &mut |_| calls += 1)
        .expect("an empty fixture should scan");
    assert!(library.is_empty());
    assert_eq!(calls, 0);
}
