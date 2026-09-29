//! The playlist rewriter's acceptance tests (task 09), one per criterion.
//!
//! This module edits files the user wrote by hand, so the tests are about the
//! ways that goes wrong: touching a line that only *looks* like the one that
//! moved, dropping the CUE suffix, normalizing a comment or a BOM on the way
//! past, replacing a symlink with a regular file, removing an `#EXTINF` that
//! belonged to the track above, and leaving a half-rewritten set of playlists
//! with nothing to put them back with.
//!
//! Almost every assertion is made on **bytes**, not on parsed lines: the promise
//! is that the file is unchanged apart from the lines that had to change, and
//! only the bytes can say that.

#![cfg(unix)]

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::library::Library;
use mpdfm_core::ops::exec_fs::{self, FsStep, Options};
use mpdfm_core::playlist::rewrite::{self, Inject, PathMove, PlaylistEdit};
use mpdfm_core::playlist::{Entry, PlaylistIndex};
use mpdfm_core::testing::{DOTFILES_PLAYLISTS, Fixture, names};

// ---------------------------------------------------------------------------
// Helpers

/// Index a fixture's playlist directory, failing the test on any warning.
fn index(fx: &Fixture) -> PlaylistIndex {
    let (index, warnings) = PlaylistIndex::load(fx.playlist_dir());
    assert!(
        warnings.is_empty(),
        "the fixture's playlist directory should index cleanly: {warnings:?}"
    );
    index
}

/// The transaction backup directory a commit would hand to `apply`.
fn backup_dir(fx: &Fixture) -> Utf8PathBuf {
    fx.data_dir().join("backups/20260928T120000Z-t3st")
}

fn read(path: &Utf8Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
}

fn text(path: &Utf8Path) -> String {
    String::from_utf8(read(path)).unwrap_or_else(|err| panic!("{path} is not UTF-8: {err}"))
}

/// Every playlist in the directory, as raw bytes, keyed by file name.
///
/// Captured before an edit and compared after, so a test can say "these two
/// files changed and the other fifteen did not" without listing the fifteen.
fn playlist_bytes(fx: &Fixture) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<(String, Vec<u8>)> = std::fs::read_dir(fx.playlist_dir())
        .unwrap_or_else(|err| panic!("cannot list the playlist directory: {err}"))
        .map(|entry| {
            let entry = entry.expect("a readable directory entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = Utf8PathBuf::from_path_buf(entry.path()).expect("a UTF-8 fixture path");
            (name, read(&path))
        })
        .collect();
    all.sort();
    all
}

/// Which playlists differ between two captures, by name.
fn changed(before: &[(String, Vec<u8>)], after: &[(String, Vec<u8>)]) -> Vec<String> {
    assert_eq!(
        before.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        after.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "a rewrite must not add or remove playlist files"
    );
    before
        .iter()
        .zip(after)
        .filter(|((_, old), (_, new))| old != new)
        .map(|((name, _), _)| name.clone())
        .collect()
}

/// The lines of a file, so a diff can name them.
fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The indices at which two files' lines differ, plus a line-count check.
fn differing_lines(old: &[u8], new: &[u8]) -> Vec<usize> {
    let (old, new) = (lines(old), lines(new));
    assert_eq!(
        old.len(),
        new.len(),
        "a rewrite must not add or remove lines:\n  before: {old:#?}\n  after:  {new:#?}"
    );
    (0..old.len()).filter(|&i| old[i] != new[i]).collect()
}

/// Plan the edits for these moves, and apply them.
fn apply(fx: &Fixture, moves: &[PathMove]) -> Vec<PlaylistEdit> {
    let edits = rewrite::plan_playlist_edits(&index(fx), moves);
    rewrite::apply(&edits, &backup_dir(fx)).expect("the edits should apply");
    edits
}

/// The album fixture the prefix criterion needs: two directories, one of whose
/// names is a string prefix of the other's.
fn doom_fixture() -> Fixture {
    Fixture::builder()
        .album(
            "hiphop/MF DOOM",
            &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"],
        )
        .album("hiphop/MF DOOM Instrumentals", &["01 Beef Rap.mp3"])
        .playlist(
            "Doom.m3u",
            &[
                "#EXTM3U",
                "hiphop/MF DOOM/01 Beef Rap.mp3",
                "hiphop/MF DOOM Instrumentals/01 Beef Rap.mp3",
                "hiphop/MF DOOM/02 Hoe Cakes.mp3",
                "# the last one",
                "hiphop/MF DOOM/03 Potholderz.mp3",
            ],
        )
        .build()
}

// ---------------------------------------------------------------------------
// Criterion: moving one track rewrites its line in every playlist that
// references it, and leaves every other line byte-identical.

#[test]
fn moving_one_track_rewrites_it_in_every_playlist_that_names_it() {
    let fx = Fixture::realistic();
    let before = playlist_bytes(&fx);

    let from = fx.rel(names::MF_DOOM_TRACK);
    let to = fx.rel("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3");
    let edits = apply(&fx, &[PathMove::moved(from, to.clone())]);

    // The fixture puts this track in two playlists on purpose.
    let mut touched: Vec<&str> = edits.iter().map(|edit| edit.file_name.as_str()).collect();
    touched.sort_unstable();
    assert_eq!(touched, [names::HIP_HOP_PLAYLIST, names::MF_DOOM_PLAYLIST]);

    let after = playlist_bytes(&fx);
    assert_eq!(
        changed(&before, &after),
        [names::HIP_HOP_PLAYLIST, names::MF_DOOM_PLAYLIST],
        "no other playlist may be touched"
    );

    // In each of the two, exactly one line differs, and it is the moved one.
    for (name, old) in &before {
        if !touched.contains(&name.as_str()) {
            continue;
        }
        let new = &after
            .iter()
            .find(|(n, _)| n == name)
            .expect("still there")
            .1;
        let differing = differing_lines(old, new);
        assert_eq!(differing.len(), 1, "{name}: expected one changed line");
        assert_eq!(lines(new)[differing[0]], to.as_str());
    }
}

#[test]
fn every_line_a_move_does_not_name_is_byte_identical_afterwards() {
    let fx = Fixture::builder()
        .album("hiphop/MF DOOM", &["01 Beef Rap.mp3"])
        .playlist(
            "Mixed.m3u",
            &[
                "#EXTM3U",
                "# Lofi / Downtempo",
                "#EXTINF:-1,Lofi Radio",
                "https://play.streamafrica.net/lofiradio",
                "",
                "   ",
                "#EXTINF:210,MF DOOM - Beef Rap",
                "hiphop/MF DOOM/01 Beef Rap.mp3",
                "C:\\Music\\not a path.mp3",
            ],
        )
        .build();

    let path = fx.playlist_path("Mixed.m3u");
    let before = read(&path);

    apply(
        &fx,
        &[PathMove::moved(
            fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
            fx.rel("hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3"),
        )],
    );

    let after = read(&path);
    assert_eq!(
        differing_lines(&before, &after),
        [7],
        "only the track line may change — the URL, the #EXTINFs, the comment, \
         the blank, the whitespace line and the Windows path stay put"
    );
    assert_eq!(
        lines(&after)[7],
        "hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3"
    );
}

// ---------------------------------------------------------------------------
// Criterion: moving a whole album directory rewrites all of its tracks' lines,
// and `hiphop/MF DOOM` does not touch `hiphop/MF DOOM Instrumentals`.

#[test]
fn moving_an_album_directory_rewrites_all_of_its_tracks() {
    let fx = doom_fixture();
    let path = fx.playlist_path("Doom.m3u");
    let before = read(&path);

    let from = fx.rel("hiphop/MF DOOM");
    let to = fx.rel("hiphop/MF DOOM/Mm..Food (2004)");
    let moves = rewrite::expand_dir_move(&index(&fx), &from, &to).expect("all three are under it");
    assert_eq!(moves.len(), 3, "three referenced tracks are inside it");

    apply(&fx, &moves);

    let after = read(&path);
    assert_eq!(
        differing_lines(&before, &after),
        [1, 3, 5],
        "the album's three lines, and only those"
    );
    let after = lines(&after);
    assert_eq!(after[1], "hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3");
    assert_eq!(after[3], "hiphop/MF DOOM/Mm..Food (2004)/02 Hoe Cakes.mp3");
    assert_eq!(after[5], "hiphop/MF DOOM/Mm..Food (2004)/03 Potholderz.mp3");
}

#[test]
fn a_directory_move_leaves_a_sibling_whose_name_it_prefixes_alone() {
    let fx = doom_fixture();
    let before = lines(&read(&fx.playlist_path("Doom.m3u")));

    let from = fx.rel("hiphop/MF DOOM");
    let to = fx.rel("hiphop/Daniel Dumile/Mm..Food");
    let moves = rewrite::expand_dir_move(&index(&fx), &from, &to).expect("all three are under it");
    apply(&fx, &moves);

    let after = lines(&read(&fx.playlist_path("Doom.m3u")));
    assert_eq!(
        after[2], before[2],
        "`MF DOOM Instrumentals` is a string prefix match and not a path one; \
         a `str::replace` would have rewritten this line"
    );
    assert_eq!(
        after[2], "hiphop/MF DOOM Instrumentals/01 Beef Rap.mp3",
        "and it still says what it said"
    );
}

// ---------------------------------------------------------------------------
// Criterion: a CUE virtual-track line is rewritten with its suffix intact.

#[test]
fn a_cue_virtual_track_keeps_its_suffix_across_the_move() {
    let fx = Fixture::realistic();
    let path = fx.playlist_path(names::POP_PLAYLIST);
    let before = read(&path);

    // The sheet moves; the `/track0017` is not a file and cannot.
    let to = fx.rel("pop/Imagine Dragons/Acts 1.flac.cue");
    apply(
        &fx,
        &[PathMove::moved(fx.rel(names::MERCURY_CUE), to.clone())],
    );

    let after = read(&path);
    let differing = differing_lines(&before, &after);
    assert_eq!(differing.len(), 1, "one CUE line references the sheet");
    assert_eq!(
        lines(&after)[differing[0]],
        format!("{to}/track0017"),
        "the sheet moved and the virtual-track component went with it"
    );
}

// ---------------------------------------------------------------------------
// Criterion: the symlinked `Radios.m3u` case — target modified, symlink kept.

#[test]
fn a_symlinked_playlist_is_written_through_and_stays_a_symlink() {
    let fx = Fixture::builder()
        .album("hiphop/MF DOOM", &["01 Beef Rap.mp3"])
        .playlist(
            names::RADIOS_PLAYLIST,
            &[
                "#EXTM3U",
                "#EXTINF:-1,Bassdrive",
                "http://ice.bassdrive.net/stream",
                "hiphop/MF DOOM/01 Beef Rap.mp3",
            ],
        )
        .symlinked_playlist(names::RADIOS_PLAYLIST, DOTFILES_PLAYLISTS)
        .build();

    let link = fx.playlist_path(names::RADIOS_PLAYLIST);
    let target = fx
        .root()
        .join(DOTFILES_PLAYLISTS)
        .join(names::RADIOS_PLAYLIST);
    let before = read(&target);

    let edits = apply(
        &fx,
        &[PathMove::moved(
            fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
            fx.rel("hiphop/MF DOOM/2004/01 Beef Rap.mp3"),
        )],
    );

    assert_eq!(
        edits[0].real_path, target,
        "the edit must name the resolved file, not the link"
    );
    assert_eq!(
        edits[0].file_name,
        names::RADIOS_PLAYLIST,
        "the backup keeps the name the user knows"
    );

    let meta = std::fs::symlink_metadata(&link).expect("the link is still there");
    assert!(
        meta.is_symlink(),
        "the link was replaced by a regular file — the dotfiles repo would have \
         stopped seeing the playlist"
    );
    assert_eq!(
        std::fs::read_link(&link).expect("readable link"),
        std::path::Path::new(target.as_str()),
        "and it still points where it pointed"
    );

    let after = read(&target);
    assert_eq!(differing_lines(&before, &after), [3]);
    assert_eq!(read(&link), after, "reading through the link sees the edit");
}

// ---------------------------------------------------------------------------
// Criterion: deleting a track removes its line *and* its `#EXTINF`, and nothing
// else.

#[test]
fn deleting_a_track_takes_the_ext_inf_that_belongs_to_it() {
    let fx = Fixture::builder()
        .album(
            "hiphop/MF DOOM",
            &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3", "03 Potholderz.mp3"],
        )
        .playlist(
            "Doom.m3u",
            &[
                "#EXTM3U",
                "#EXTINF:210,MF DOOM - Beef Rap",
                "hiphop/MF DOOM/01 Beef Rap.mp3",
                "#EXTINF:190,MF DOOM - Hoe Cakes",
                "hiphop/MF DOOM/02 Hoe Cakes.mp3",
                "# no #EXTINF for this one",
                "hiphop/MF DOOM/03 Potholderz.mp3",
            ],
        )
        .build();

    let edits = apply(
        &fx,
        &[
            PathMove::deleted(fx.rel("hiphop/MF DOOM/02 Hoe Cakes.mp3")),
            PathMove::deleted(fx.rel("hiphop/MF DOOM/03 Potholderz.mp3")),
        ],
    );

    assert_eq!(
        (edits[0].rewrites(), edits[0].removals()),
        (0, 3),
        "two tracks and the one #EXTINF that immediately preceded one of them"
    );

    assert_eq!(
        text(&fx.playlist_path("Doom.m3u")),
        "#EXTM3U\n\
         #EXTINF:210,MF DOOM - Beef Rap\n\
         hiphop/MF DOOM/01 Beef Rap.mp3\n\
         # no #EXTINF for this one\n",
        "the comment above the third track is not an #EXTINF and stays"
    );
}

#[test]
fn a_deletion_never_reaches_past_the_line_directly_above_it() {
    let fx = Fixture::builder()
        .album("hiphop/MF DOOM", &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"])
        .playlist(
            "Doom.m3u",
            &[
                "#EXTINF:210,MF DOOM - Beef Rap",
                "hiphop/MF DOOM/01 Beef Rap.mp3",
                "hiphop/MF DOOM/02 Hoe Cakes.mp3",
            ],
        )
        .build();

    apply(
        &fx,
        &[PathMove::deleted(fx.rel("hiphop/MF DOOM/02 Hoe Cakes.mp3"))],
    );

    assert_eq!(
        text(&fx.playlist_path("Doom.m3u")),
        "#EXTINF:210,MF DOOM - Beef Rap\nhiphop/MF DOOM/01 Beef Rap.mp3\n",
        "the #EXTINF two lines up titles the first track and is not the \
         deleted track's to take"
    );
}

// ---------------------------------------------------------------------------
// Criterion: backups are written before the first modification and are
// byte-identical to the originals.

#[test]
fn every_backup_exists_before_the_first_playlist_is_modified() {
    let fx = Fixture::realistic();
    let before = playlist_bytes(&fx);

    let edits = rewrite::plan_playlist_edits(
        &index(&fx),
        &[PathMove::moved(
            fx.rel(names::MF_DOOM_TRACK),
            fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
        )],
    );
    assert_eq!(edits.len(), 2);

    let dir = backup_dir(&fx);
    let err = rewrite::apply_with(&edits, &dir, Inject::FailBeforeWriting(0))
        .expect_err("the injected failure fires before the very first write");
    assert!(err.to_string().contains("simulated failure"), "{err}");

    assert_eq!(
        changed(&before, &playlist_bytes(&fx)),
        Vec::<String>::new(),
        "the failure was before the first write, so nothing may have changed"
    );
    for edit in &edits {
        let backup = dir.join(&edit.file_name);
        let original = before
            .iter()
            .find(|(name, _)| *name == edit.file_name)
            .expect("the playlist was captured");
        assert_eq!(
            read(&backup),
            original.1,
            "{} was not backed up byte-for-byte",
            edit.file_name
        );
    }
}

#[test]
fn a_backup_is_never_written_over() {
    let fx = Fixture::realistic();
    let dir = backup_dir(&fx);
    let moves = [PathMove::moved(
        fx.rel(names::MF_DOOM_TRACK),
        fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
    )];

    let edits = rewrite::plan_playlist_edits(&index(&fx), &moves);
    rewrite::apply(&edits, &dir).expect("the first pass works");

    // Planned afresh against what is now on disk, so the edits are current and
    // the only thing standing in the way is the backup already there — which is
    // the only copy of what the first pass replaced.
    let back = rewrite::plan_playlist_edits(
        &index(&fx),
        &[PathMove::moved(
            fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
            fx.rel(names::MF_DOOM_TRACK),
        )],
    );
    let err = rewrite::apply(&back, &dir)
        .expect_err("a second apply into the same backup directory is refused");
    assert!(err.to_string().contains("refusing to overwrite"), "{err}");
}

// ---------------------------------------------------------------------------
// Criterion: a failure while writing playlist 3 of 5 leaves 1–2 written, 3–5
// untouched, and enough state to undo.

#[test]
fn a_failure_partway_through_leaves_the_rest_untouched_and_all_of_it_undoable() {
    let names_of = ["A.m3u", "B.m3u", "C.m3u", "D.m3u", "E.m3u"];
    let mut builder = Fixture::builder().album("hiphop/MF DOOM", &["01 Beef Rap.mp3"]);
    for name in names_of {
        builder = builder.playlist(
            name,
            &[
                "#EXTM3U",
                &format!("# {name}"),
                "hiphop/MF DOOM/01 Beef Rap.mp3",
            ],
        );
    }
    let fx = builder.build();
    let before = playlist_bytes(&fx);

    let edits = rewrite::plan_playlist_edits(
        &index(&fx),
        &[PathMove::moved(
            fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
            fx.rel("hiphop/MF DOOM/2004/01 Beef Rap.mp3"),
        )],
    );
    assert_eq!(edits.len(), 5, "all five name the track");

    let dir = backup_dir(&fx);
    let err = rewrite::apply_with(&edits, &dir, Inject::FailBeforeWriting(2))
        .expect_err("the third write fails");
    assert!(err.to_string().contains("C.m3u"), "{err}");

    assert_eq!(
        changed(&before, &playlist_bytes(&fx)),
        ["A.m3u", "B.m3u"],
        "the two before the failure are written, the three after it are not"
    );

    // Everything needed to undo is on disk: five backups, each byte-identical
    // to the file as it was before the commit started.
    for (name, original) in &before {
        assert_eq!(&read(&dir.join(name)), original, "{name}");
    }

    rewrite::restore(&edits, &dir).expect("the backups put all five back");
    assert_eq!(
        changed(&before, &playlist_bytes(&fx)),
        Vec::<String>::new(),
        "undo restores the written ones and is a no-op on the rest"
    );
}

// ---------------------------------------------------------------------------
// Criterion: after a commit, every previously resolvable playlist entry still
// resolves to an existing file.

#[test]
fn after_a_commit_every_entry_that_resolved_before_still_resolves() {
    let fx = Fixture::realistic();
    let root = fx.music_dir();

    let library = Library::scan(root).expect("the fixture scans");
    let before_broken: Vec<String> = index(&fx)
        .broken(&library)
        .into_iter()
        .map(|(_, path)| path.to_string())
        .collect();
    assert_eq!(
        before_broken,
        [names::BROKEN_REFERENCE],
        "the fixture has exactly one reference that never resolved"
    );

    // A whole album directory: the filesystem half and the playlist half, in the
    // order `commit` will do them (task 11).
    let from = fx.rel(names::MF_DOOM_ALBUM);
    let to = fx.rel("hiphop/MF DOOM/Mm..Food (2004)");
    let moves = rewrite::expand_dir_move(&index(&fx), &from, &to).expect("all are under it");
    let edits = rewrite::plan_playlist_edits(&index(&fx), &moves);

    let options = Options::default();
    exec_fs::execute_with(&FsStep::MkDir { at: to.clone() }, root, &options)
        .expect("the destination can be created");
    for path_move in &moves {
        let step = FsStep::RenameFile {
            from: path_move.from.clone(),
            to: path_move.to.clone().expect("a move, not a delete"),
        };
        exec_fs::execute_with(&step, root, &options).expect("the track moves");
    }
    rewrite::apply(&edits, &backup_dir(&fx)).expect("the playlists are rewritten");

    let library = Library::scan(root).expect("the moved fixture scans");
    let after_broken: Vec<String> = index(&fx)
        .broken(&library)
        .into_iter()
        .map(|(_, path)| path.to_string())
        .collect();
    assert_eq!(
        after_broken, before_broken,
        "a move must not break a reference that resolved, and must not fix one \
         that never did"
    );
}

// ---------------------------------------------------------------------------
// Planning is pure, and refuses to guess.

#[test]
fn planning_writes_nothing() {
    let fx = Fixture::realistic();
    let before = fx.snapshot();

    let index = index(&fx);
    let moves = rewrite::expand_dir_move(
        &index,
        &fx.rel(names::MF_DOOM_ALBUM),
        &fx.rel("hiphop/MF DOOM/Mm..Food"),
    )
    .expect("all are under it");
    let edits = rewrite::plan_playlist_edits(&index, &moves);
    assert!(!edits.is_empty(), "there is something to plan");

    fx.snapshot().assert_same(&before);
}

#[test]
fn an_edit_planned_against_a_playlist_that_has_since_changed_is_refused() {
    let fx = Fixture::realistic();
    let edits = rewrite::plan_playlist_edits(
        &index(&fx),
        &[PathMove::moved(
            fx.rel(names::MF_DOOM_TRACK),
            fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
        )],
    );

    // The user edits one of the two playlists between the preview and the
    // commit — the exact race the `old` line exists to catch.
    let meddled = fx.playlist_path(names::MF_DOOM_PLAYLIST);
    std::fs::write(&meddled, b"hiphop/something else.mp3\n").expect("writable");
    let before = playlist_bytes(&fx);

    let err = rewrite::apply(&edits, &backup_dir(&fx)).expect_err("the plan is stale");
    assert!(err.to_string().contains("the plan expected"), "{err}");
    assert_eq!(
        changed(&before, &playlist_bytes(&fx)),
        Vec::<String>::new(),
        "a stale edit is caught before the first playlist — including the one \
         that had not changed — is written"
    );
    assert!(
        !backup_dir(&fx).exists(),
        "and before any backup is taken, since there is nothing to back up"
    );
}

#[test]
fn a_path_no_playlist_references_plans_no_edits() {
    let fx = Fixture::realistic();
    let edits = rewrite::plan_playlist_edits(
        &index(&fx),
        &[PathMove::moved(
            fx.rel(names::KREAM_TRACK),
            fx.rel("electronic/KREAM/So Hi.mp3"),
        )],
    );
    assert!(
        edits.is_empty(),
        "the saved queue references this track, no playlist does: {edits:?}"
    );
}

#[test]
fn a_line_listed_twice_is_rewritten_twice() {
    let fx = Fixture::builder()
        .album("hiphop/MF DOOM", &["01 Beef Rap.mp3"])
        .playlist(
            "Duplicates.m3u",
            &[
                "hiphop/MF DOOM/01 Beef Rap.mp3",
                "hiphop/MF DOOM/01 Beef Rap.mp3",
            ],
        )
        .build();

    apply(
        &fx,
        &[PathMove::moved(
            fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
            fx.rel("hiphop/MF DOOM/2004/01 Beef Rap.mp3"),
        )],
    );

    assert_eq!(
        text(&fx.playlist_path("Duplicates.m3u")),
        "hiphop/MF DOOM/2004/01 Beef Rap.mp3\nhiphop/MF DOOM/2004/01 Beef Rap.mp3\n",
        "a deduplicating rewrite would quietly drop a line the user wrote"
    );
}

#[test]
fn a_rewritten_line_is_still_a_track_the_index_can_find() {
    let fx = Fixture::realistic();
    let to = fx.rel("pop/Imagine Dragons/Acts 1.flac.cue");
    apply(
        &fx,
        &[PathMove::moved(fx.rel(names::MERCURY_CUE), to.clone())],
    );

    // Reload from disk: the bytes that were written have to parse back into the
    // same identity the plan used, or the next operation would miss them.
    let reloaded = index(&fx);
    let refs = reloaded.refs_to(&to);
    assert_eq!(refs.len(), 1, "the moved sheet is referenced once");
    let entry = reloaded.entry(refs[0]).expect("a live reference");
    assert_eq!(entry.rel(), Some(&to));
    assert_eq!(entry.cue(), Some("track0017"));
    assert!(matches!(entry, Entry::Track { .. }));
}

#[test]
fn expanding_a_directory_move_never_includes_a_file_outside_it() {
    let fx = doom_fixture();
    let from = fx.rel("hiphop/MF DOOM");
    let to = fx.rel("hiphop/Daniel Dumile");
    let moves = rewrite::expand_dir_move(&index(&fx), &from, &to).expect("well-formed");

    let sources: Vec<&str> = moves.iter().map(|m| m.from.as_str()).collect();
    assert_eq!(
        sources,
        [
            "hiphop/MF DOOM/01 Beef Rap.mp3",
            "hiphop/MF DOOM/02 Hoe Cakes.mp3",
            "hiphop/MF DOOM/03 Potholderz.mp3",
        ],
        "sorted, and without the Instrumentals album"
    );
    for path_move in &moves {
        let landing = path_move.to.as_ref().expect("a move");
        assert!(
            landing.starts_with_dir(&to),
            "{landing} should have landed under {to}"
        );
    }
}

#[test]
fn the_same_path_named_twice_is_resolved_once_rather_than_applied_twice() {
    let fx = doom_fixture();
    let from = fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3");
    let edits = rewrite::plan_playlist_edits(
        &index(&fx),
        &[
            PathMove::moved(from.clone(), fx.rel("a/first.mp3")),
            PathMove::moved(from, fx.rel("b/second.mp3")),
        ],
    );

    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].line_edits.len(), 1, "one line, one edit");
    assert_eq!(
        edits[0].line_edits[0].new.as_deref(),
        Some("a/first.mp3"),
        "the first move wins, deterministically; task 10 reports the clash as a \
         conflict before it ever gets here"
    );
}

#[test]
fn a_relpath_that_only_looks_like_a_prefix_is_not_a_prefix() {
    let fx = doom_fixture();
    // The exact-match rule, stated directly: moving the *file* `hiphop/MF DOOM`
    // (which does not exist) touches nothing, even though every album line
    // starts with those bytes.
    let edits = rewrite::plan_playlist_edits(
        &index(&fx),
        &[PathMove::moved(
            fx.rel("hiphop/MF DOOM"),
            fx.rel("hiphop/Daniel Dumile"),
        )],
    );
    assert!(
        edits.is_empty(),
        "no playlist line *is* `hiphop/MF DOOM`: {edits:?}"
    );
}

#[test]
fn a_move_and_a_delete_in_one_pass_are_both_applied() {
    let fx = doom_fixture();
    apply(
        &fx,
        &[
            PathMove::moved(
                fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3"),
                fx.rel("hiphop/MF DOOM/2004/01 Beef Rap.mp3"),
            ),
            PathMove::deleted(fx.rel("hiphop/MF DOOM/03 Potholderz.mp3")),
        ],
    );

    assert_eq!(
        text(&fx.playlist_path("Doom.m3u")),
        "#EXTM3U\n\
         hiphop/MF DOOM/2004/01 Beef Rap.mp3\n\
         hiphop/MF DOOM Instrumentals/01 Beef Rap.mp3\n\
         hiphop/MF DOOM/02 Hoe Cakes.mp3\n\
         # the last one\n"
    );
}

#[test]
fn a_move_with_nothing_to_do_creates_no_backup_directory() {
    let fx = Fixture::realistic();
    let dir = backup_dir(&fx);
    rewrite::apply(&[], &dir).expect("an empty edit list is not an error");
    assert!(
        dir.exists(),
        "the transaction directory is still created, so the journal has \
         somewhere to point at"
    );
    assert_eq!(
        std::fs::read_dir(&dir).expect("readable").count(),
        0,
        "and it is empty"
    );
}

#[test]
fn a_move_that_lands_a_path_back_where_it_started_rewrites_nothing_visible() {
    let fx = doom_fixture();
    let path = fx.playlist_path("Doom.m3u");
    let before = read(&path);
    let same = fx.rel("hiphop/MF DOOM/01 Beef Rap.mp3");

    apply(&fx, &[PathMove::moved(same.clone(), same)]);

    assert_eq!(
        read(&path),
        before,
        "an identity move rewrites the line with the bytes it already had"
    );
}
