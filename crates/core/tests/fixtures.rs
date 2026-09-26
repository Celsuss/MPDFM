//! The fixture library's own tests (task 03).
//!
//! These live in `tests/` rather than next to the code on purpose: reaching
//! `mpdfm_core::testing` from outside the crate is exactly how tasks 05 onwards
//! will use it, so if the feature plumbing breaks, it breaks here first.
//!
//! The fixture is what every later task's evidence rests on, so it is worth
//! testing that it really contains the cases it claims to, that it cleans up
//! after itself, and that its two safety tools — the containment guard and the
//! snapshot — actually fire.

#![cfg(unix)]

use std::panic::AssertUnwindSafe;

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::testing::{
    DOTFILES_PLAYLISTS, Fixture, SnapshotEntryKind, names, real_library_roots,
};

/// Every path in the fixture, relative to its root.
fn all_paths(fx: &Fixture) -> Vec<String> {
    fx.snapshot()
        .entries()
        .map(|(path, _)| path.to_owned())
        .collect()
}

/// Run `f` expecting it to panic, without spraying its backtrace over the test
/// output.
fn assert_panics(what: &str, f: impl FnOnce()) {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    assert!(result.is_err(), "{what} should have panicked");
}

fn read(path: &Utf8Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}")))
        .expect("fixture text files are UTF-8")
}

#[test]
fn realistic_library_has_every_ugly_case() {
    let fx = Fixture::realistic();
    let paths = all_paths(&fx);
    let has = |needle: &str| paths.iter().any(|path| path.contains(needle));

    // Names: the characters that break naive quoting, splitting and prefixing.
    assert!(paths.iter().any(|p| !p.is_ascii()), "no non-ASCII name");
    assert!(has(" "), "no name with a space");
    assert!(has("["), "no name with a bracket");
    assert!(has(" & "), "no name with an ampersand");
    assert!(has(" + "), "no name with a plus");
    assert!(has("Smokin' On"), "no name with an apostrophe");
    assert!(has("Mm..Food"), "no name with `..` inside a component");

    // A multi-disc set: the discs hold the audio, the set root holds none.
    for disc in [names::MERCURY_CD1, names::MERCURY_CD2] {
        assert!(
            fx.abs(&format!("{disc}/01 Wrecked.mp3")).is_file(),
            "{disc}"
        );
    }
    let set_root = fx.abs(names::MERCURY_ALBUM);
    let audio_in_set_root = std::fs::read_dir(&set_root)
        .expect("the set root should exist")
        .filter_map(Result::ok)
        .any(|entry| entry.path().is_file());
    assert!(
        !audio_in_set_root,
        "{set_root} should hold only disc directories"
    );

    // Aux clutter that has to travel with the album, including a `.m3u` that
    // lives in the music directory and is not one of MPD's playlists.
    for aux in [
        "folder.jpg",
        "info.nfo",
        "mm..food.sfv",
        "eac.log",
        "Mm..Food.m3u",
    ] {
        let path = fx.abs(&format!("{}/{aux}", names::MF_DOOM_ALBUM));
        assert!(path.is_file(), "{path} is missing");
    }
    assert!(
        fx.aux_files()
            .iter()
            .any(|rel| rel.as_str().ends_with(".nfo")),
        "aux files were not recorded"
    );

    // A FLAC album, and an mp3 album, and the m4a-free mix in between.
    assert!(fx.abs(names::KIND_OF_BLUE_TRACK).is_file());
    assert!(
        fx.tracks()
            .iter()
            .filter(|rel| rel.extension() == Some("flac"))
            .count()
            >= 3,
        "expected a FLAC album"
    );
    assert!(fx.tracks().iter().any(|rel| rel.extension() == Some("mp3")));
    // One m4a, so task 05 has to classify all three containers.
    assert!(fx.abs(names::SWITCHANGEL_M4A).is_file());
    assert!(
        fx.tracks().iter().any(|rel| rel.extension() == Some("m4a")),
        "expected an m4a track"
    );

    // A playlist that is a symlink to a file outside the playlist directory.
    let link = fx.playlist_path(names::RADIOS_PLAYLIST);
    let meta = std::fs::symlink_metadata(&link).expect("the link should exist");
    assert!(meta.is_symlink(), "{link} should be a symlink");
    let target = Utf8PathBuf::from_path_buf(std::fs::read_link(&link).unwrap()).unwrap();
    assert!(
        !target.starts_with(fx.playlist_dir()),
        "{target} is not outside the playlist dir"
    );
    assert!(fx.contains_path(&target), "{target} escaped the fixture");
    assert!(target.starts_with(fx.root().join(DOTFILES_PLAYLISTS)));

    // A radio-URL playlist: every kind of line that is not a track.
    let radios = read(&link);
    assert!(radios.starts_with("#EXTM3U\n"));
    assert!(radios.contains("\n# Lofi / Downtempo\n"));
    assert!(radios.contains("\n#EXTINF:-1,Bassdrive\n"));
    assert!(radios.contains("\nhttp://ice1.somafm.com/groovesalad-256-mp3\n"));
    assert!(radios.contains("\n\n"), "a blank line should survive");

    // A CUE virtual track: the `.cue` exists, the `trackNNNN` below it does not.
    assert_eq!(fx.cue_references(), [names::MERCURY_CUE_TRACK]);
    assert!(fx.abs(names::MERCURY_CUE).is_file());
    assert!(
        !fx.abs(names::MERCURY_CUE_TRACK).exists(),
        "trackNNNN is not a file"
    );
    assert!(read(&fx.abs(names::MERCURY_CUE)).contains("TRACK 17 AUDIO"));
    assert!(
        fx.abs(names::MERCURY_CUE).with_extension("").is_file(),
        "the CUE's FLAC is missing"
    );

    // Exactly one broken reference, and it is in a playlist.
    assert_eq!(fx.broken_references(), [names::BROKEN_REFERENCE]);
    assert!(!fx.abs(names::BROKEN_REFERENCE).exists());
    assert!(read(&fx.playlist_path(names::POP_PLAYLIST)).contains(names::BROKEN_REFERENCE));

    // One track referenced from two playlists (the task 07 / 09 edge case).
    for playlist in [names::HIP_HOP_PLAYLIST, names::MF_DOOM_PLAYLIST] {
        assert!(
            read(&fx.playlist_path(playlist)).contains(names::MF_DOOM_TRACK),
            "{playlist} should reference the shared track"
        );
    }

    // The saved queue, in MPD's `N:relpath` form.
    let state = read(fx.state_file());
    assert!(state.contains("\nplaylist_begin\n0:"), "no saved queue");
    assert!(state.contains(&format!("1:{}\n", names::MF_DOOM_TRACK)));
    assert!(state.ends_with("playlist_end\n"));

    // Every recorded track really is on disk, and nothing is a stray `RelPath`.
    for rel in fx.tracks() {
        let path = rel.to_abs(fx.music_dir());
        assert!(path.is_file(), "{path} was recorded but not written");
    }
}

#[test]
fn the_four_roots_are_where_the_config_will_look() {
    let fx = Fixture::realistic();

    assert!(fx.music_dir().is_dir());
    assert!(fx.playlist_dir().is_dir());
    assert!(fx.data_dir().is_dir());
    assert!(fx.state_file().is_file());

    // The playlist directory is not inside the music directory, and the data
    // directory is somewhere else again — as in the real setup.
    assert!(!fx.playlist_dir().starts_with(fx.music_dir()));
    assert!(!fx.data_dir().starts_with(fx.music_dir()));
    assert!(!fx.state_file().starts_with(fx.playlist_dir()));
    for root in [fx.music_dir(), fx.playlist_dir(), fx.data_dir()] {
        assert!(root.starts_with(fx.root()));
    }
}

#[test]
fn the_tree_is_gone_when_the_fixture_drops() {
    let fx = Fixture::realistic();
    let root = fx.root().to_owned();
    assert!(root.is_dir());

    drop(fx);

    assert!(
        !root.exists(),
        "{root} was left behind in the temp directory"
    );
}

#[test]
fn the_guard_refuses_the_real_library() {
    let fx = Fixture::realistic();

    // The case that matters: a test handed the user's own music directory.
    let mut checked = 0;
    for root in real_library_roots() {
        assert!(
            !fx.contains_path(&root),
            "{root} should not be inside the fixture"
        );
        assert_panics(&format!("assert_inside({root})"), || {
            fx.assert_inside(&root)
        });
        assert_panics(&format!("assert_inside({root}/deeper)"), || {
            fx.assert_inside(&root.join("hiphop/01.mp3"));
        });
        checked += 1;
    }
    assert!(checked > 0, "$HOME was unset, so nothing was checked");

    // And an absolute path with no relationship to anything.
    assert_panics("assert_inside(/etc/passwd)", || {
        fx.assert_inside(Utf8Path::new("/etc/passwd"));
    });
    // A relative path would be resolved against the working directory.
    assert_panics("assert_inside(relative)", || {
        fx.assert_inside(Utf8Path::new("hiphop/01.mp3"));
    });
    // Another fixture is not this one.
    let other = Fixture::builder().build();
    assert_panics("assert_inside(other fixture)", || {
        fx.assert_inside(other.root())
    });
}

#[test]
fn the_guard_accepts_what_an_operation_legitimately_touches() {
    let fx = Fixture::realistic();

    fx.assert_all_inside([
        fx.root(),
        fx.music_dir(),
        fx.playlist_dir(),
        fx.data_dir(),
        fx.state_file(),
    ]);
    fx.assert_inside(&fx.abs(names::MF_DOOM_TRACK));
    // A destination that does not exist yet — every move has one.
    fx.assert_inside(&fx.abs("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3"));
    // The symlinked playlist, as the link and as its target.
    let link = fx.playlist_path(names::RADIOS_PLAYLIST);
    fx.assert_inside(&link);
    fx.assert_inside(&Utf8PathBuf::from_path_buf(std::fs::read_link(&link).unwrap()).unwrap());
}

#[test]
fn snapshot_detects_a_one_byte_change_anywhere() {
    for target in [
        Target::Audio(names::MF_DOOM_TRACK),
        Target::Audio(names::KIND_OF_BLUE_TRACK),
        Target::Playlist(names::HIP_HOP_PLAYLIST),
        Target::Playlist(names::RADIOS_PLAYLIST), // through the symlink
    ] {
        let fx = Fixture::realistic();
        let before = fx.snapshot();
        before.assert_same(&fx.snapshot());

        let path = match target {
            Target::Audio(rel) => fx.abs(rel),
            Target::Playlist(name) => fx.playlist_path(name),
        };
        fx.flip_byte(&path);

        let diff = before.diff(&fx.snapshot());
        assert_eq!(diff.len(), 1, "{path}: {diff:#?}");
        assert!(diff[0].starts_with('~'), "{path}: {diff:#?}");
    }
}

enum Target {
    Audio(&'static str),
    Playlist(&'static str),
}

#[test]
fn snapshot_notices_the_tree_changing_shape() {
    let fx = Fixture::realistic();
    let before = fx.snapshot();

    // The mistake task 06 must not make: writing a symlinked playlist by
    // replacing the link with a regular file.
    let link = fx.playlist_path(names::RADIOS_PLAYLIST);
    let contents = std::fs::read(&link).unwrap();
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, &contents).unwrap();

    let diff = before.diff(&fx.snapshot());
    assert_eq!(diff.len(), 1, "{diff:#?}");
    assert!(
        diff[0].contains("link ->") && diff[0].contains("text,"),
        "{diff:#?}"
    );
    assert!(matches!(
        fx.snapshot()
            .get("playlists/Radios.m3u")
            .map(|entry| &entry.kind),
        Some(SnapshotEntryKind::Text { .. })
    ));
}

#[test]
fn snapshot_notices_a_file_appearing_or_moving() {
    let fx = Fixture::realistic();
    let before = fx.snapshot();

    let from = fx.abs(names::MF_DOOM_TRACK);
    let to = fx.abs(&format!("{}/renamed.mp3", names::MF_DOOM_ALBUM));
    std::fs::rename(&from, &to).unwrap();

    let diff = before.diff(&fx.snapshot());
    assert_eq!(diff.len(), 2, "{diff:#?}");
    assert!(diff.iter().any(|line| line.starts_with('-')), "{diff:#?}");
    assert!(diff.iter().any(|line| line.starts_with('+')), "{diff:#?}");
}

#[test]
fn snapshot_does_not_care_about_mtime() {
    let fx = Fixture::realistic();
    let before = fx.snapshot();

    // Undo restores contents, not timestamps; a snapshot that compared mtimes
    // would fail every undo test for the wrong reason.
    let path = fx.abs(names::MF_DOOM_TRACK);
    let contents = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, &contents).unwrap();

    before.assert_same(&fx.snapshot());
}

#[test]
fn snapshot_renders_stably_for_insta() {
    let fx = Fixture::realistic();
    let rendered = fx.snapshot().to_string();

    // The temp directory's random name must not leak into the rendering, or
    // every snapshot test would be different on every run.
    assert!(
        !rendered.contains(fx.root().as_str()),
        "the root leaked into the rendering"
    );
    assert!(rendered.contains("<root>/dotfiles/mpd/playlists/Radios.m3u"));
    assert!(
        rendered.contains("    | #EXTM3U"),
        "playlist text should be readable"
    );
    assert_eq!(rendered, fx.snapshot().to_string());
}

#[test]
fn building_the_library_is_fast_enough_to_do_per_test() {
    let started = std::time::Instant::now();
    let fx = Fixture::realistic();
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "building the fixture took {elapsed:?}"
    );
    // Sanity: the budget is only meaningful if it really built the library.
    assert!(fx.tracks().len() > 15, "only {} tracks", fx.tracks().len());
}

#[test]
fn the_builder_adds_only_what_it_is_asked_for() {
    let fx = Fixture::builder()
        .track("single.mp3")
        .playlist("Solo.m3u", &["single.mp3"])
        .build();

    assert_eq!(fx.tracks().len(), 1);
    assert!(fx.aux_files().is_empty());
    assert_eq!(fx.playlists(), ["Solo.m3u"]);
    assert!(fx.cue_references().is_empty());
    assert!(fx.broken_references().is_empty());
    // The state file exists even when no queue was asked for, so task 14 always
    // has something to rewrite.
    assert!(read(fx.state_file()).contains("playlist_begin\nplaylist_end\n"));
}

#[test]
fn playlist_raw_writes_exact_bytes() {
    // The cases a list of lines cannot express, which task 06 has to round-trip.
    let fx = Fixture::builder()
        .playlist_raw("Crlf.m3u", b"#EXTM3U\r\npop/a.mp3\r\n")
        .playlist_raw("NoNewline.m3u", b"pop/a.mp3")
        .playlist_raw("Bom.m3u", b"\xef\xbb\xbfpop/a.mp3\n")
        .build();

    assert_eq!(
        std::fs::read(fx.playlist_path("Crlf.m3u")).unwrap(),
        b"#EXTM3U\r\npop/a.mp3\r\n"
    );
    assert_eq!(
        std::fs::read(fx.playlist_path("NoNewline.m3u")).unwrap(),
        b"pop/a.mp3"
    );
    assert_eq!(
        std::fs::read(fx.playlist_path("Bom.m3u")).unwrap(),
        b"\xef\xbb\xbfpop/a.mp3\n"
    );
}

#[test]
fn a_symlinked_playlist_can_be_made_from_nothing() {
    let fx = Fixture::builder()
        .symlinked_playlist("Radios.m3u", DOTFILES_PLAYLISTS)
        .build();

    let link = fx.playlist_path("Radios.m3u");
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(read(&link), "#EXTM3U\n");
    assert_eq!(fx.playlists(), ["Radios.m3u"]);
}

#[test]
fn a_non_utf8_name_can_be_planted_and_is_still_snapshotted() {
    // `So H\xEF.mp3` in latin-1: the scanner has to report and skip it, and a
    // snapshot still has to record that it is there (safety invariant 8).
    let fx = Fixture::builder()
        .non_utf8_file("electronic", b"So H\xEF.mp3")
        .build();

    let names: Vec<_> = std::fs::read_dir(fx.music_dir().join("electronic"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1);
    assert!(
        camino::Utf8Path::from_path(std::path::Path::new(&names[0])).is_none(),
        "the planted name should not be UTF-8"
    );
    assert!(
        fx.snapshot()
            .entries()
            .any(|(path, _)| path.contains("So H")),
        "the snapshot lost the non-UTF-8 file"
    );
}

#[test]
fn a_broken_reference_must_really_be_broken() {
    let fx = Fixture::builder().track("pop/a.mp3").build();
    // Guard against a test that "adds a broken reference" to a file that exists,
    // which would quietly stop testing anything.
    assert_panics("broken_reference to an existing file", || {
        let _ = Fixture::builder()
            .track("pop/a.mp3")
            .broken_reference("Pop.m3u", "pop/a.mp3");
    });
    assert!(fx.abs("pop/a.mp3").is_file());
}

#[test]
fn a_bad_path_is_rejected_when_it_is_written_not_later() {
    assert_panics("album with a doubled separator", || {
        let _ = Fixture::builder().album("pop//album", &["01.mp3"]);
    });
    assert_panics("a track that is not audio", || {
        let _ = Fixture::builder().album("pop/album", &["cover.jpg"]);
    });
    assert_panics("a playlist name that is a path", || {
        let _ = Fixture::builder().playlist("sub/dir.m3u", &[]);
    });
}
