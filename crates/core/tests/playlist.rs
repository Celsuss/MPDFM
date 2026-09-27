//! The playlist parser and writer's acceptance tests (task 06), one per
//! criterion.
//!
//! The property that matters is one line long — parse a playlist, write it back,
//! and the bytes are the same ones — and it is worth nothing unless it is checked
//! against files that have the ugly properties real playlists have. So these run
//! over the seventeen committed fixture playlists, copied into a fixture's
//! playlist directory: CRLF, a BOM, a missing trailing newline, an empty file, a
//! `.m3u8`, a CUE virtual track, radio URLs, an absolute path, names with spaces
//! and accents.
//!
//! Two of them make the filesystem hostile — a symlinked playlist, an unwritable
//! directory — and put it back afterwards so the fixture can still clean itself
//! up. The unwritable one checks first that the hostility took effect: run as
//! root, `chmod 0500` stops nothing, and a test that silently proved nothing is
//! worse than one that says it was skipped.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::{Entry, LineEnding, Playlist};
use mpdfm_core::testing::{DOTFILES_PLAYLISTS, Fixture, PLAYLIST_TEMPLATES, playlist_template};

/// A fixture whose playlist directory holds all seventeen committed playlists.
fn fixture() -> Fixture {
    Fixture::builder().real_playlists().build()
}

/// Load one of the committed playlists out of `fx`'s playlist directory.
fn load(fx: &Fixture, name: &str) -> Playlist {
    let path = fx.playlist_path(name);
    Playlist::load(&path).unwrap_or_else(|err| panic!("{name} should parse: {err}"))
}

fn read(path: &Utf8Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
}

fn mode_of(path: &Utf8Path) -> u32 {
    fs::metadata(path)
        .unwrap_or_else(|err| panic!("cannot stat {path}: {err}"))
        .permissions()
        .mode()
        & 0o777
}

fn set_mode(path: &Utf8Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|err| panic!("cannot chmod {path}: {err}"));
}

/// The entry at `index`, with a failure that says which playlist and line.
fn entry(playlist: &Playlist, index: usize) -> &Entry {
    playlist.entries().get(index).unwrap_or_else(|| {
        panic!(
            "{} has no line {index}: {:?}",
            playlist.name(),
            playlist.entries()
        )
    })
}

/// Every line of `playlist` as bytes, for a diff that reads like the file.
fn lines(playlist: &Playlist) -> Vec<&str> {
    playlist.entries().iter().map(Entry::line).collect()
}

#[test]
fn round_trips_every_committed_playlist_byte_for_byte() {
    let fx = fixture();
    assert_eq!(PLAYLIST_TEMPLATES.len(), 17, "the fixture set is seventeen");

    for template in PLAYLIST_TEMPLATES {
        let path = fx.playlist_path(template.name);
        fx.assert_inside(&path);
        assert_eq!(
            read(&path),
            template.bytes,
            "{} was not copied into the fixture verbatim",
            template.name
        );

        // parse → serialize, in memory.
        let playlist = load(&fx, template.name);
        assert_eq!(
            playlist.to_bytes(),
            template.bytes,
            "{} did not round-trip; lines were {:?}",
            template.name,
            lines(&playlist)
        );

        // parse → write → read, through the disk.
        playlist.write().expect("the write succeeds");
        assert_eq!(
            read(&path),
            template.bytes,
            "{} changed on disk after being written back",
            template.name
        );
        assert_eq!(playlist.name(), template.playlist_name());
    }
}

/// The same property, stated over the whole fixture at once: rewriting every
/// playlist changes *nothing* — not a byte, not a permission bit, not a symlink.
#[test]
fn rewriting_every_playlist_leaves_the_fixture_identical() {
    let fx = Fixture::builder()
        .real_playlists()
        .symlinked_playlist("Radios.m3u", DOTFILES_PLAYLISTS)
        .build();
    let before = fx.snapshot();

    for template in PLAYLIST_TEMPLATES {
        load(&fx, template.name)
            .write()
            .unwrap_or_else(|err| panic!("{} should write: {err}", template.name));
    }

    before.assert_same(&fx.snapshot());
}

#[test]
fn a_cue_virtual_track_parses_into_a_sheet_and_a_track_id() {
    let fx = fixture();
    let playlist = load(&fx, "Cue sheets.m3u");

    let sheet = "pop/Imagine Dragons - Mercury - Acts 1 & 2 (2022) (2 CD) (Japan Deluxe Edition) [rjk]/CD 1 - Mercury - Acts 1/Imagine Dragons - Mercury - Acts 1.flac.cue";

    // The reference is the `.cue` file plus a virtual track that is not a file,
    // so moving the sheet has to carry the suffix along (task 09).
    assert_eq!(
        entry(&playlist, 1),
        &Entry::Track {
            rel: RelPath::parse(sheet).expect("the sheet is a RelPath"),
            cue: Some("track0017".to_owned()),
            raw: format!("{sheet}/track0017"),
        }
    );
    // `.CUE` is the same thing: MPD's extension matching is case-insensitive.
    assert_eq!(
        entry(&playlist, 3).cue(),
        Some("track0002"),
        "an uppercase .CUE is still a sheet"
    );
    // The sheet on its own is an ordinary file reference, with no suffix.
    assert_eq!(entry(&playlist, 4).rel().map(RelPath::as_str), Some(sheet));
    assert_eq!(entry(&playlist, 4).cue(), None);

    assert_eq!(
        playlist.to_bytes(),
        playlist_template("Cue sheets.m3u").bytes
    );
}

#[test]
fn radio_urls_extinf_extm3u_comments_and_blanks_all_round_trip() {
    let fx = fixture();
    let playlist = load(&fx, "Radios.m3u");

    assert_eq!(entry(&playlist, 0), &Entry::ExtM3u);
    assert_eq!(
        entry(&playlist, 1),
        &Entry::Comment("# Lofi / Downtempo".to_owned())
    );
    assert_eq!(
        entry(&playlist, 2),
        &Entry::ExtInf {
            duration: -1,
            title: "Lofi Radio".to_owned(),
            raw: "#EXTINF:-1,Lofi Radio".to_owned(),
        }
    );
    assert_eq!(
        entry(&playlist, 3),
        &Entry::Url("https://play.streamafrica.net/lofiradio".to_owned())
    );
    assert_eq!(
        entry(&playlist, 5),
        &Entry::Url("http://ice1.somafm.com/groovesalad-256-mp3".to_owned())
    );
    assert_eq!(entry(&playlist, 6), &Entry::Blank);
    assert_eq!(
        entry(&playlist, 7),
        &Entry::Comment("# Liquid Drum & Bass".to_owned())
    );

    // Not one of these lines is a track, so the index (task 07) will hold
    // nothing from this file.
    assert!(playlist.entries().iter().all(|entry| entry.rel().is_none()));
    assert_eq!(playlist.to_bytes(), playlist_template("Radios.m3u").bytes);
}

#[test]
fn crlf_stays_crlf_and_a_file_with_no_trailing_newline_keeps_none() {
    let fx = fixture();

    let crlf = load(&fx, "Jazz.m3u");
    assert_eq!(crlf.line_ending(), LineEnding::Crlf);
    assert!(crlf.trailing_newline());

    let unterminated = load(&fx, "Chill.m3u");
    assert_eq!(unterminated.line_ending(), LineEnding::Lf);
    assert!(!unterminated.trailing_newline());

    // Both at once, with a BOM on top.
    let windows = load(&fx, "Windows.m3u");
    assert_eq!(windows.line_ending(), LineEnding::Crlf);
    assert!(!windows.trailing_newline());
    assert!(windows.has_bom());

    // A rewritten line takes the file's ending, and the file's shape is
    // otherwise untouched: only the one line changes.
    let mut edited = load(&fx, "Jazz.m3u");
    let old = read(edited.real_path());
    edited.entries_mut()[1] = Entry::track(
        RelPath::parse("jazz/Miles Davis - Kind of Blue (1959) [FLAC]/01 So What Else.flac")
            .expect("a RelPath"),
        None,
    );
    edited.write().expect("the write succeeds");

    let new = read(edited.real_path());
    assert!(
        new.ends_with(b"\r\n"),
        "the rewritten CRLF file must stay CRLF"
    );
    assert_eq!(
        new.iter().filter(|byte| **byte == b'\r').count(),
        old.iter().filter(|byte| **byte == b'\r').count(),
        "no line ending was added or lost"
    );
    let unchanged = |bytes: &[u8]| {
        String::from_utf8(bytes.to_owned())
            .expect("UTF-8")
            .split("\r\n")
            .filter(|line| !line.contains("So What"))
            .map(ToOwned::to_owned)
            .collect::<Vec<String>>()
    };
    assert_eq!(unchanged(&new), unchanged(&old), "another line changed");
}

#[test]
fn a_utf8_bom_is_preserved() {
    let fx = fixture();
    let template = playlist_template("Bangers.m3u");
    let playlist = load(&fx, template.name);

    assert!(playlist.has_bom());
    // The BOM is not part of the first line, so the header is still a header.
    assert_eq!(entry(&playlist, 0), &Entry::ExtM3u);

    playlist.write().expect("the write succeeds");
    let written = read(&fx.playlist_path(template.name));
    assert!(written.starts_with(b"\xef\xbb\xbf"), "the BOM was dropped");
    assert_eq!(written, template.bytes);
}

#[test]
fn writing_a_symlinked_playlist_edits_the_target_and_keeps_the_link() {
    let fx = Fixture::builder()
        .real_playlists()
        .symlinked_playlist("Radios.m3u", DOTFILES_PLAYLISTS)
        .build();

    let link = fx.playlist_path("Radios.m3u");
    let target: Utf8PathBuf = fx.root().join(DOTFILES_PLAYLISTS).join("Radios.m3u");
    assert!(
        fs::symlink_metadata(&link)
            .expect("the link exists")
            .is_symlink(),
        "the fixture should have made Radios.m3u a symlink"
    );

    // The link is what was found; the target is what gets written.
    let mut playlist = Playlist::load(&link).expect("loads through the link");
    assert_eq!(playlist.path(), link);
    assert_eq!(
        playlist.real_path().canonicalize_utf8().expect("resolves"),
        target.canonicalize_utf8().expect("resolves")
    );
    assert_eq!(playlist.name(), "Radios");

    // Permissions of the target, not of the link, are the ones preserved.
    set_mode(&target, 0o600);
    playlist
        .entries_mut()
        .push(Entry::Comment("# added by MPDFM".to_owned()));
    playlist.write().expect("the write succeeds");

    let link_metadata = fs::symlink_metadata(&link).expect("the link still exists");
    assert!(
        link_metadata.is_symlink(),
        "the symlink was replaced by a regular file"
    );
    assert_eq!(
        Utf8PathBuf::from_path_buf(fs::read_link(&link).expect("readable link"))
            .expect("UTF-8 target"),
        target,
        "the link now points somewhere else"
    );

    let expected = {
        let mut bytes = playlist_template("Radios.m3u").bytes.to_vec();
        bytes.extend_from_slice(b"# added by MPDFM\n");
        bytes
    };
    assert_eq!(read(&target), expected, "the target was not edited");
    assert_eq!(
        read(&link),
        expected,
        "the edit is not visible through the link"
    );
    assert_eq!(
        mode_of(&target),
        0o600,
        "the target's mode was not preserved"
    );

    // Nothing was written into the playlist directory itself — no temp file, and
    // no regular file shadowing the link.
    let mut names: Vec<String> = fs::read_dir(fx.playlist_dir())
        .expect("readable playlist directory")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    let expected_names: Vec<String> = {
        let mut names: Vec<String> = PLAYLIST_TEMPLATES
            .iter()
            .map(|template| template.name.to_owned())
            .collect();
        names.sort();
        names
    };
    assert_eq!(names, expected_names);
}

#[test]
fn an_absolute_path_line_is_unparsed_and_survives_a_write_untouched() {
    let fx = fixture();
    let template = playlist_template("Absolute paths.m3u");
    let playlist = load(&fx, template.name);

    assert_eq!(
        entry(&playlist, 0),
        &Entry::Unparsed(
            "/home/celsuss/Music/coding-music/SwitchAngel/Coding_Trance.mp3".to_owned()
        ),
        "an absolute path is not a track identity"
    );
    // A leading `./` is rejected rather than normalized (task 02), and a
    // backslash path is not a path MPDFM will write.
    assert!(matches!(entry(&playlist, 1), Entry::Unparsed(_)));
    assert!(matches!(entry(&playlist, 3), Entry::Unparsed(_)));
    // The one line that *is* a track identity still is.
    assert_eq!(
        entry(&playlist, 4).rel().map(RelPath::as_str),
        Some("coding-music/SwitchAngel/Coding_Trance.mp3")
    );

    // None of the unparsed lines is in the index's reach, and all of them come
    // back out byte-identical.
    playlist.write().expect("the write succeeds");
    assert_eq!(read(&fx.playlist_path(template.name)), template.bytes);
}

#[test]
fn a_write_that_fails_leaves_the_original_playlist_and_no_temp_files() {
    let fx = fixture();
    let path = fx.playlist_path("Pop.m3u");
    let original = read(&path);

    let mut playlist = load(&fx, "Pop.m3u");
    playlist.entries_mut().clear();

    // The write needs to create a temp file in this directory, and cannot.
    set_mode(fx.playlist_dir(), 0o500);
    let refused = playlist.write();
    let read_only = refused.is_err();
    set_mode(fx.playlist_dir(), 0o700);

    if !read_only {
        eprintln!("skipped: running as a user the directory mode does not stop");
        return;
    }

    assert_eq!(
        read(&path),
        original,
        "a failed write must leave the playlist byte-identical"
    );
    let strays: Vec<String> = fs::read_dir(fx.playlist_dir())
        .expect("readable playlist directory")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "temp files were left behind: {strays:?}");
}

#[test]
fn the_file_mode_of_the_original_is_preserved() {
    let fx = fixture();
    let path = fx.playlist_path("Coding flow.m3u");

    for mode in [0o600, 0o640, 0o664] {
        set_mode(&path, mode);
        let mut playlist = load(&fx, "Coding flow.m3u");
        playlist
            .entries_mut()
            .push(Entry::Comment(format!("# rewritten at {mode:o}")));
        playlist.write().expect("the write succeeds");
        assert_eq!(mode_of(&path), mode, "the mode changed on a rewrite");
    }
}

#[test]
fn a_playlist_name_is_the_file_name_and_is_never_slugified() {
    let fx = fixture();
    for (file, name) in [
        ("Coding flow.m3u", "Coding flow"),
        ("En kall Stockholms natt.m3u", "En kall Stockholms natt"),
        ("Mixed bag.m3u8", "Mixed bag"),
        ("Empty.m3u", "Empty"),
    ] {
        assert_eq!(load(&fx, file).name(), name);
    }
    // `.m3u8` keeps its extension on disk — only the name drops it.
    assert!(fx.playlist_path("Mixed bag.m3u8").is_file());
}

#[test]
fn a_dangling_symlink_is_an_error_that_names_the_link() {
    let fx = fixture();
    let link = fx.playlist_dir().join("Gone.m3u");
    std::os::unix::fs::symlink(fx.playlist_dir().join("not-here.m3u"), &link)
        .expect("can create a dangling link");

    let err = Playlist::load(&link).expect_err("a dangling link cannot be read");
    assert!(
        err.to_string().contains("Gone.m3u"),
        "the error should name the link: {err}"
    );
}

#[test]
fn bytes_that_are_not_utf8_are_reported_rather_than_guessed_at() {
    let fx = Fixture::builder()
        .playlist_raw("Latin1.m3u", b"pop/Bj\xf6rk/01 Human Behaviour.mp3\n")
        .build();

    let err = Playlist::load(&fx.playlist_path("Latin1.m3u"))
        .expect_err("latin-1 bytes are not a playlist MPDFM will read");
    assert!(
        err.to_string().contains("not valid UTF-8"),
        "unexpected error: {err}"
    );
}
