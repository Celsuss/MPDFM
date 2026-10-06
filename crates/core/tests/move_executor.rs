//! The move executor's acceptance tests (task 08), one per criterion.
//!
//! This is the module that can destroy data, so the tests are about the ways it
//! could: overwriting something, unlinking a source before the destination is
//! real, leaving a half-copied file where MPD will find it, walking out of the
//! library through a symlink, deleting when it was told not to, and reverting to
//! something that is not quite what was there before.
//!
//! They run against the fixture library, whose names have the shapes the real one
//! has — non-ASCII, spaces, brackets, `&`, `+`, an apostrophe, `..` inside a
//! component — because a mover that works on `a/b.mp3` and not on
//! `hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3` is no use
//! here.

#![cfg(unix)]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::ops::exec_fs::{
    self, Collision, Done, FsError, FsStep, FsWarning, Inject, Merge, Method, Options, StepReceipt,
};
use mpdfm_core::testing::{Fixture, digest, names};

/// What a command line would pass, with deletion allowed and backups pointed at
/// the fixture's stand-in for `~/.local/share/mpdfm`.
fn options(fx: &Fixture) -> Options {
    Options {
        delete_enabled: true,
        verify: false,
        backup_root: Some(fx.data_dir().to_owned()),
        id3_version: Default::default(),
        inject: Inject::Nothing,
    }
}

/// A `RenameFile` between two paths spelled as a playlist would spell them.
fn rename(fx: &Fixture, from: &str, to: &str) -> FsStep {
    FsStep::RenameFile {
        from: fx.rel(from),
        to: fx.rel(to),
    }
}

/// Execute a step, failing the test with the step and the error if it is refused.
fn run(fx: &Fixture, step: &FsStep, options: &Options) -> StepReceipt {
    exec_fs::execute_with(step, fx.music_dir(), options)
        .unwrap_or_else(|err| panic!("{step} should have succeeded: {err}"))
}

/// Refuse a step, failing the test if it is *not* refused.
fn refuse(fx: &Fixture, step: &FsStep, options: &Options) -> FsError {
    exec_fs::execute_with(step, fx.music_dir(), options)
        .err()
        .unwrap_or_else(|| panic!("{step} should have been refused"))
}

/// The library root, spelled out where a test reads better for it.
fn root_of(fx: &Fixture) -> &Utf8Path {
    fx.music_dir()
}

/// Every name in a directory, sorted.
fn names_in(dir: &Utf8Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("cannot read {dir}: {err}"))
        .map(|entry| {
            entry
                .expect("a readable entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The names in a directory that are MPDFM temp files.
fn temp_names_in(dir: &Utf8Path) -> Vec<String> {
    names_in(dir)
        .into_iter()
        .filter(|name| name.contains(".mpdfm-"))
        .collect()
}

fn read(path: &Utf8Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
}

fn mode_of(path: &Utf8Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|err| panic!("cannot stat {path}: {err}"))
        .permissions()
        .mode()
        & 0o7777
}

fn mtime_of(path: &Utf8Path) -> SystemTime {
    std::fs::metadata(path)
        .unwrap_or_else(|err| panic!("cannot stat {path}: {err}"))
        .modified()
        .expect("an mtime")
}

fn chmod(path: &Utf8Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|err| panic!("cannot chmod {path}: {err}"));
}

/// Create a stand-in for the transaction backup directory task 11 will make.
fn backup_dir(fx: &Fixture) -> Utf8PathBuf {
    let dir = fx.data_dir().join("backups/20260928T120000Z-test");
    std::fs::create_dir_all(&dir).expect("can create the backup directory");
    dir
}

/// The paths created by a receipt, as strings, for a readable assertion.
fn created(done: &Done) -> Vec<&str> {
    match done {
        Done::DirsCreated { dirs } | Done::Moved { dirs, .. } => dirs
            .iter()
            .map(mpdfm_core::paths::RelPath::as_str)
            .collect(),
        other => panic!("{other:?} created no directories"),
    }
}

// ---------------------------------------------------------------------------

/// Criterion: moving a file within the same filesystem uses `rename` — verified
/// by the inode, which a copy would change.
#[test]
fn a_move_within_one_filesystem_is_a_rename() {
    let fx = Fixture::realistic();
    let to = "hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3";
    let before = std::fs::metadata(fx.abs(names::MF_DOOM_TRACK)).expect("the fixture track");

    let receipt = run(&fx, &rename(&fx, names::MF_DOOM_TRACK, to), &options(&fx));

    let Done::Moved { method, facts, .. } = &receipt.done else {
        panic!("expected a move, got {:?}", receipt.done);
    };
    assert_eq!(*method, Method::Rename);
    assert_eq!(
        created(&receipt.done),
        ["hiphop/MF DOOM", "hiphop/MF DOOM/Mm..Food (2004)"],
        "the parents it had to create, outermost first"
    );

    let after = std::fs::metadata(fx.abs(to)).expect("the moved track");
    assert_eq!(
        after.ino(),
        before.ino(),
        "a rename keeps the inode; a copy would not"
    );
    assert_eq!(after.len(), facts.size);
    assert_eq!(
        after.modified().ok(),
        facts.mtime,
        "a rename keeps the mtime"
    );
    assert!(
        !fx.abs(names::MF_DOOM_TRACK).exists(),
        "the source is gone, exactly once"
    );
    assert!(
        facts.hash.is_none(),
        "nothing was read, so nothing was hashed"
    );
}

/// Criterion: the cross-device path is exercised, and the source is unlinked only
/// after the destination is complete.
#[test]
fn a_cross_device_move_copies_verifies_and_only_then_unlinks() {
    let fx = Fixture::realistic();
    let to = "electronic/moved/01 So Hï.mp3";
    let source = fx.abs(names::KREAM_TRACK);
    let bytes = read(&source);
    let (mode, mtime, ino) = (
        mode_of(&source),
        mtime_of(&source),
        std::fs::metadata(&source).expect("the fixture track").ino(),
    );

    // `Inject::CrossDevice` makes `rename` report EXDEV, which is the only thing a
    // second mount would change — the fallback is the code under test.
    let mut opts = options(&fx).verifying();
    opts.inject = Inject::CrossDevice;
    let receipt = run(&fx, &rename(&fx, names::KREAM_TRACK, to), &opts);

    let Done::Moved { method, facts, .. } = &receipt.done else {
        panic!("expected a move, got {:?}", receipt.done);
    };
    assert_eq!(*method, Method::Copy, "EXDEV must fall back to copy+delete");
    assert_eq!(
        facts.hash,
        Some(digest(&bytes)),
        "--verify records the hash it compared"
    );

    let dest = fx.abs(to);
    assert_eq!(read(&dest), bytes, "every byte arrived");
    assert_ne!(
        std::fs::metadata(&dest).expect("the copy").ino(),
        ino,
        "a copy is a new inode"
    );
    assert_eq!(mode_of(&dest), mode, "the mode travels with the copy");
    assert_eq!(
        mtime_of(&dest),
        mtime,
        "and so does the mtime, or MPD sees the move as an edit"
    );
    assert!(!source.exists(), "the source is unlinked, last");
    assert_eq!(
        temp_names_in(dest.parent().expect("a parent")),
        Vec::<String>::new(),
        "no temp file is left behind"
    );

    // Reverting copies it back, and the copy back has to restore what a rename
    // would have kept by itself.
    exec_fs::revert(&receipt, root_of(&fx)).expect("a cross-device move reverts");
    assert!(!dest.exists(), "and the destination is gone again");
    assert_eq!(read(&source), bytes);
    assert_eq!(mode_of(&source), mode);
    assert_eq!(mtime_of(&source), mtime);

    // And the same path taken deliberately, by the step that names it.
    let step = FsStep::CopyDelete {
        from: fx.rel(names::SNOOP_TRACK),
        to: fx.rel("hiphop/moved/smokin.mp3"),
    };
    let receipt = run(&fx, &step, &options(&fx));
    assert!(matches!(
        receipt.done,
        Done::Moved {
            method: Method::Copy,
            ..
        }
    ));
}

/// Criterion: interrupting a cross-device move leaves no partial destination
/// visible — which is what the temp name plus the final `rename` buys.
#[test]
fn an_interrupted_cross_device_move_leaves_no_partial_destination() {
    let fx = Fixture::realistic();
    let to = "electronic/moved/01 So Hï.mp3";
    let source = fx.abs(names::KREAM_TRACK);
    let bytes = read(&source);

    let mut opts = options(&fx);
    opts.inject = Inject::CrashAfterCopy;
    // `CopyDelete` rather than `RenameFile`: the crash is injected into the copy,
    // and a `RenameFile` between two paths on one filesystem never gets there.
    let step = FsStep::CopyDelete {
        from: fx.rel(names::KREAM_TRACK),
        to: fx.rel(to),
    };
    let err = refuse(&fx, &step, &opts);
    assert!(matches!(err, FsError::Injected { .. }), "{err}");

    assert!(
        !fx.abs(to).exists(),
        "the destination must never exist in a half-copied state"
    );
    assert_eq!(read(&source), bytes, "and the source is untouched");

    // What a power cut at that instant leaves: the complete copy, under a name
    // that is hidden and that MPD does not read as a track.
    let dir = fx.abs(to).parent().expect("a parent").to_owned();
    let temps = temp_names_in(&dir);
    assert_eq!(temps.len(), 1, "expected one temp file, found {temps:?}");
    let temp = &temps[0];
    assert!(temp.starts_with('.'), "{temp} is not hidden from MPD");
    assert!(temp.ends_with(".tmp"), "{temp} does not end in .tmp");
    assert_eq!(read(&dir.join(temp)), bytes, "the copy itself was complete");
}

/// Criterion: a destination that exists is a conflict, not an overwrite — for a
/// file and for a directory.
#[test]
fn a_destination_that_exists_is_a_conflict_not_an_overwrite() {
    let fx = Fixture::realistic();
    let opts = options(&fx);
    let before = fx.snapshot();

    // A file onto a file.
    let onto_file = rename(&fx, names::MF_DOOM_TRACK, names::SNOOP_TRACK);
    assert!(matches!(
        refuse(&fx, &onto_file, &opts),
        FsError::Exists { .. }
    ));

    // A directory onto a directory, as one step...
    let onto_dir = rename(&fx, names::MF_DOOM_ALBUM, names::SNOOP_ALBUM);
    assert!(matches!(
        refuse(&fx, &onto_dir, &opts),
        FsError::Exists { .. }
    ));

    // ...and as the expansion a plan would use, which refuses by default.
    let err = exec_fs::expand_dir_move(
        &fx.rel(names::MF_DOOM_ALBUM),
        &fx.rel(names::SNOOP_ALBUM),
        fx.music_dir(),
        Merge::Refuse,
    )
    .expect_err("an existing destination directory must be refused");
    assert!(matches!(err, FsError::Exists { .. }), "{err}");

    // A file where a directory is, and a directory where a file is.
    assert!(matches!(
        refuse(
            &fx,
            &rename(&fx, names::MF_DOOM_TRACK, names::SNOOP_ALBUM),
            &opts
        ),
        FsError::Exists { .. }
    ));
    assert!(matches!(
        refuse(
            &fx,
            &FsStep::MkDir {
                at: fx.rel(names::MF_DOOM_TRACK)
            },
            &opts
        ),
        FsError::Exists { .. }
    ));

    before.assert_same(&fx.snapshot());
}

/// Criterion: directory merge mode moves what does not collide and reports what
/// does.
#[test]
fn merging_a_directory_moves_what_does_not_collide_and_reports_what_does() {
    let fx = Fixture::builder()
        .album("hiphop/from", &["01 One.mp3", "02 Two.mp3"])
        .aux("hiphop/from", &["folder.jpg"])
        .album("hiphop/from/bonus", &["03 Three.mp3"])
        .album("hiphop/into", &["02 Two.mp3"])
        .build();
    let root = fx.music_dir();
    let (from, to) = (fx.rel("hiphop/from"), fx.rel("hiphop/into"));

    let plan = exec_fs::expand_dir_move(&from, &to, root, Merge::Allow).expect("a merge expands");
    assert_eq!(
        plan.collisions,
        vec![Collision {
            from: fx.rel("hiphop/from/02 Two.mp3"),
            to: fx.rel("hiphop/into/02 Two.mp3"),
        }],
        "the one file already there is reported, not moved"
    );

    // Make the two files differ, so "not overwritten" is a real assertion.
    fx.flip_byte(&fx.abs("hiphop/into/02 Two.mp3"));
    let kept = read(&fx.abs("hiphop/into/02 Two.mp3"));

    for step in &plan.steps {
        run(&fx, step, &options(&fx));
    }

    assert_eq!(
        names_in(&fx.abs("hiphop/into")),
        ["01 One.mp3", "02 Two.mp3", "bonus", "folder.jpg"],
        "everything that did not collide arrived, aux files and subdirectory included"
    );
    assert!(fx.abs("hiphop/into/bonus/03 Three.mp3").is_file());
    assert_eq!(
        read(&fx.abs("hiphop/into/02 Two.mp3")),
        kept,
        "the colliding file was not overwritten"
    );
    assert_eq!(
        names_in(&fx.abs("hiphop/from")),
        ["02 Two.mp3"],
        "its source stayed put, so the source directory is not empty and stays"
    );
}

/// Criterion: a case-only rename works — for a file and for a directory, and back
/// again.
#[test]
fn a_case_only_rename_works_in_both_directions() {
    let fx = Fixture::builder()
        .album("Artist", &["01 Track.mp3"])
        .build();
    let root = fx.music_dir();
    let bytes = read(&fx.abs("Artist/01 Track.mp3"));

    let file = rename(&fx, "Artist/01 Track.mp3", "Artist/01 track.mp3");
    let file_receipt = run(&fx, &file, &options(&fx));
    assert!(matches!(
        file_receipt.done,
        Done::Moved {
            method: Method::Rename,
            ..
        }
    ));
    assert_eq!(names_in(&fx.abs("Artist")), ["01 track.mp3"]);
    assert_eq!(read(&fx.abs("Artist/01 track.mp3")), bytes);

    let dir = rename(&fx, "Artist", "artist");
    let dir_receipt = run(&fx, &dir, &options(&fx));
    assert_eq!(names_in(root), ["artist"]);
    assert_eq!(read(&fx.abs("artist/01 track.mp3")), bytes);
    assert_eq!(
        temp_names_in(root),
        Vec::<String>::new(),
        "the staging name is gone"
    );

    exec_fs::revert(&dir_receipt, root).expect("a case-only rename reverts");
    exec_fs::revert(&file_receipt, root).expect("a case-only rename reverts");
    assert_eq!(names_in(&fx.abs("Artist")), ["01 Track.mp3"]);
    assert_eq!(read(&fx.abs("Artist/01 Track.mp3")), bytes);
}

/// Criterion: empty source directories are removed up to but never beyond the
/// root.
#[test]
fn empty_source_directories_are_removed_up_to_but_never_beyond_the_root() {
    let fx = Fixture::builder()
        .album("electronic/deep/album", &["01 One.mp3"])
        .build();
    let root = fx.music_dir();
    run(
        &fx,
        &rename(&fx, "electronic/deep/album/01 One.mp3", "moved/01 One.mp3"),
        &options(&fx),
    );

    let step = FsStep::RmDirIfEmpty {
        at: fx.rel("electronic/deep/album"),
    };
    let receipt = run(&fx, &step, &options(&fx));
    let Done::DirsRemoved { dirs } = &receipt.done else {
        panic!("expected removals, got {:?}", receipt.done);
    };
    assert_eq!(
        dirs.iter().map(|dir| dir.at.as_str()).collect::<Vec<_>>(),
        ["electronic/deep/album", "electronic/deep", "electronic"],
        "the walk goes up while the parents are empty"
    );
    assert!(root.is_dir(), "the music directory itself is never removed");
    assert_eq!(names_in(root), ["moved"]);

    exec_fs::revert(&receipt, root).expect("the removals revert");
    assert!(fx.abs("electronic/deep/album").is_dir());
    assert_eq!(names_in(root), ["electronic", "moved"]);
}

/// A directory that only *looks* empty is left alone, and its clutter with it.
#[test]
fn a_directory_holding_only_clutter_is_not_silently_emptied() {
    let fx = Fixture::builder()
        .album("electronic/album", &["01 One.mp3"])
        .aux("electronic/album", &[".DS_Store"])
        .build();
    let root = fx.music_dir();
    run(
        &fx,
        &rename(&fx, "electronic/album/01 One.mp3", "moved/01 One.mp3"),
        &options(&fx),
    );

    let step = FsStep::RmDirIfEmpty {
        at: fx.rel("electronic/album"),
    };
    assert_eq!(
        exec_fs::check(&step, root, &options(&fx)).expect("the check passes"),
        vec![FsWarning::OnlyClutter {
            at: fx.rel("electronic/album"),
            files: vec![".DS_Store".to_owned()],
        }],
        "the user is told why the directory is still there"
    );

    let receipt = run(&fx, &step, &options(&fx));
    assert_eq!(receipt.done, Done::DirsRemoved { dirs: Vec::new() });
    assert!(
        fx.abs("electronic/album/.DS_Store").is_file(),
        "clutter is the user's file; MPDFM does not delete it to make a directory removable"
    );
}

/// Criterion: a read-only destination directory fails preflight with a clear
/// message — and so does an unreadable source.
#[test]
fn a_read_only_destination_directory_fails_preflight_with_a_clear_message() {
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 One.mp3"])
        .build();
    let root = fx.music_dir();
    let locked = fx.abs("hiphop/locked");
    std::fs::create_dir_all(&locked).expect("can create the destination");
    chmod(&locked, 0o500);

    let step = rename(&fx, "hiphop/album/01 One.mp3", "hiphop/locked/01 One.mp3");
    for err in [
        exec_fs::check(&step, root, &options(&fx)).expect_err("check refuses"),
        refuse(&fx, &step, &options(&fx)),
    ] {
        assert!(matches!(err, FsError::NotWritable { .. }), "{err}");
        let message = err.to_string();
        assert!(message.contains("hiphop/locked"), "{message}");
        assert!(message.contains("write permission is missing"), "{message}");
    }
    assert!(
        fx.abs("hiphop/album/01 One.mp3").is_file(),
        "nothing was moved"
    );

    // A file MPDFM cannot read is refused the same way, before anything is copied.
    let source = fx.abs("hiphop/album/01 One.mp3");
    chmod(&source, 0o000);
    let unreadable = FsStep::CopyDelete {
        from: fx.rel("hiphop/album/01 One.mp3"),
        to: fx.rel("hiphop/album/02 Two.mp3"),
    };
    match exec_fs::check(&unreadable, root, &options(&fx)) {
        Err(FsError::NotReadable { .. }) => {}
        Err(other) => panic!("expected NotReadable, got {other}"),
        // Root ignores the mode bits, so say so rather than pass vacuously.
        Ok(_) => eprintln!("skipped: running as a user the mode bits do not stop"),
    }

    // Left writable so the temp directory can be removed when the fixture drops.
    chmod(&source, 0o644);
    chmod(&locked, 0o700);
}

/// Criterion: `revert` of each `FsStep` restores the previous state
/// byte-for-byte.
#[test]
fn reverting_every_kind_of_step_restores_the_tree_byte_for_byte() {
    let fx = Fixture::realistic();
    let root = fx.music_dir();
    let backups = backup_dir(&fx);
    let opts = options(&fx);
    // Captured after the backup directory exists, because task 11 creates it
    // before the first step runs and undo does not remove it.
    let before = fx.snapshot();

    let mut steps = vec![
        FsStep::MkDir {
            at: fx.rel("hiphop/MF DOOM/Mm..Food (2004)"),
        },
        rename(
            &fx,
            names::MF_DOOM_TRACK,
            "hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3",
        ),
        FsStep::CopyDelete {
            from: fx.rel(names::SNOOP_TRACK),
            to: fx.rel("hiphop/MF DOOM/Mm..Food (2004)/01.Smokin' On.mp3"),
        },
        FsStep::RemoveFile {
            target: fx.rel(names::SWITCHANGEL_M4A),
            backup: Some(backups.join("Coding_Trance_Reprise.m4a")),
        },
    ];
    // Empty a whole album directory, then let the upward walk take it and the
    // genre directory above it.
    for track in fx.tracks().to_vec() {
        if track.starts_with_dir(&fx.rel(names::KREAM_ALBUM)) {
            steps.push(FsStep::RenameFile {
                from: track.clone(),
                to: fx.rel(&format!("moved/{}", track.file_name())),
            });
        }
    }
    steps.push(FsStep::RmDirIfEmpty {
        at: fx.rel(names::KREAM_ALBUM),
    });

    let receipts: Vec<StepReceipt> = steps.iter().map(|step| run(&fx, step, &opts)).collect();
    assert!(
        !before.diff(&fx.snapshot()).is_empty(),
        "the steps should have changed something"
    );
    assert!(!fx.abs("electronic").exists(), "the emptied genre is gone");

    for receipt in receipts.iter().rev() {
        exec_fs::revert(receipt, root)
            .unwrap_or_else(|err| panic!("{} should revert: {err}", receipt.step));
    }
    before.assert_same(&fx.snapshot());
}

/// Criterion: a step whose target is outside the root is rejected, including via a
/// symlink.
#[test]
fn a_step_that_leaves_the_root_is_rejected_including_through_a_symlink() {
    let fx = Fixture::realistic();
    let root = fx.music_dir();
    let opts = options(&fx);

    // A `RelPath` cannot spell `..`, so the only way out of the library is a link
    // — which is exactly the shape safety invariant 5 is about.
    let backups = backup_dir(&fx);
    let outside = fx.root().join("outside");
    std::fs::create_dir_all(&outside).expect("can create a directory outside the library");
    std::fs::write(outside.join("stranger.mp3"), b"not in the library").expect("can write it");
    std::os::unix::fs::symlink(outside.as_std_path(), root.join("escape").as_std_path())
        .expect("can create the link");

    let before = fx.snapshot();
    let steps = [
        rename(&fx, names::MF_DOOM_TRACK, "escape/01 Beef Rap.mp3"),
        rename(&fx, "escape/stranger.mp3", "hiphop/stranger.mp3"),
        FsStep::CopyDelete {
            from: fx.rel(names::MF_DOOM_TRACK),
            to: fx.rel("escape/copied.mp3"),
        },
        FsStep::MkDir {
            at: fx.rel("escape/new album"),
        },
        FsStep::RmDirIfEmpty {
            at: fx.rel("escape"),
        },
        FsStep::RemoveFile {
            target: fx.rel("escape/stranger.mp3"),
            backup: Some(backups.join("stranger.mp3")),
        },
    ];
    for step in &steps {
        let err = refuse(&fx, step, &opts);
        assert!(
            matches!(err, FsError::Outside { .. }),
            "{step} should be refused as outside the root, got {err}"
        );
        assert!(
            exec_fs::check(step, root, &opts).is_err(),
            "{step} should be refused by `check` too"
        );
    }

    // And a backup that would land outside MPDFM's own data directory, which is
    // the other root this module writes to.
    let smuggled = FsStep::RemoveFile {
        target: fx.rel(names::MF_DOOM_TRACK),
        backup: Some(outside.join("smuggled.mp3")),
    };
    assert!(matches!(
        refuse(&fx, &smuggled, &opts),
        FsError::Outside { .. }
    ));

    before.assert_same(&fx.snapshot());
}

/// Criterion: deleting with `delete_enabled = false` is refused.
#[test]
fn deleting_with_delete_enabled_false_is_refused() {
    let fx = Fixture::realistic();
    let backups = backup_dir(&fx);
    let before = fx.snapshot();
    let mut opts = options(&fx);
    opts.delete_enabled = false;

    let backed_up = FsStep::RemoveFile {
        target: fx.rel(names::MF_DOOM_TRACK),
        backup: Some(backups.join("01 Beef Rap.mp3")),
    };
    let unbacked = FsStep::RemoveFile {
        target: fx.rel(names::MF_DOOM_TRACK),
        backup: None,
    };
    for step in [&backed_up, &unbacked] {
        assert!(matches!(
            refuse(&fx, step, &opts),
            FsError::DeleteDisabled { .. }
        ));
        assert!(matches!(
            exec_fs::check(step, fx.music_dir(), &opts),
            Err(FsError::DeleteDisabled { .. })
        ));
    }

    // And the defaults refuse as well: a caller that forgets to pass the
    // configuration cannot delete anything by accident.
    assert!(matches!(
        exec_fs::execute(&unbacked, fx.music_dir()),
        Err(FsError::DeleteDisabled { .. })
    ));

    before.assert_same(&fx.snapshot());
}

/// A delete is a move into the backup directory, so undo can bring the file back
/// with its mode and mtime; without a backup it cannot, and says so twice.
#[test]
fn a_delete_backs_the_file_up_and_undo_restores_it() {
    let fx = Fixture::realistic();
    let root = fx.music_dir();
    let target = fx.abs(names::MF_DOOM_TRACK);
    let (bytes, mode, mtime) = (read(&target), mode_of(&target), mtime_of(&target));
    let backup = backup_dir(&fx).join("01 Beef Rap.mp3");

    let step = FsStep::RemoveFile {
        target: fx.rel(names::MF_DOOM_TRACK),
        backup: Some(backup.clone()),
    };
    let receipt = run(&fx, &step, &options(&fx));
    assert!(receipt.warnings.is_empty(), "{:?}", receipt.warnings);
    assert!(!target.exists(), "the track is out of the library");
    assert_eq!(
        read(&backup),
        bytes,
        "and its bytes are in the backup directory, not gone"
    );

    exec_fs::revert(&receipt, root).expect("a backed-up delete reverts");
    assert_eq!(read(&target), bytes);
    assert_eq!(mode_of(&target), mode);
    assert_eq!(mtime_of(&target), mtime);
    assert!(!backup.exists(), "the backup was moved, not copied");

    // No backup: allowed, warned about, and not revertible.
    let unbacked = FsStep::RemoveFile {
        target: fx.rel(names::SNOOP_TRACK),
        backup: None,
    };
    let receipt = run(&fx, &unbacked, &options(&fx));
    assert_eq!(
        receipt.warnings,
        vec![FsWarning::NoBackup {
            target: fx.rel(names::SNOOP_TRACK),
        }]
    );
    assert_eq!(
        receipt.done,
        Done::Removed {
            method: Method::Unlink,
            facts: match &receipt.done {
                Done::Removed { facts, .. } => *facts,
                other => panic!("expected a removal, got {other:?}"),
            },
        }
    );
    let err = exec_fs::revert(&receipt, root).expect_err("an unlinked file cannot come back");
    assert!(matches!(err, FsError::NotRevertible { .. }), "{err}");
}

/// A destination that differs from a neighbour only by case is a warning on a
/// case-sensitive filesystem, and the step still runs.
#[test]
fn a_destination_that_differs_only_by_case_from_a_neighbour_warns() {
    let fx = Fixture::builder()
        .album("Artist", &["01 One.mp3"])
        .album("other", &["02 Two.mp3"])
        .build();
    let root = fx.music_dir();

    let dir = FsStep::MkDir {
        at: fx.rel("artist"),
    };
    assert_eq!(
        exec_fs::check(&dir, root, &options(&fx)).expect("the check passes"),
        vec![FsWarning::CaseCollision {
            at: fx.rel("artist"),
            existing: "Artist".to_owned(),
        }]
    );

    let file = rename(&fx, "other/02 Two.mp3", "Artist/01 one.mp3");
    let receipt = run(&fx, &file, &options(&fx));
    assert_eq!(
        receipt.warnings,
        vec![FsWarning::CaseCollision {
            at: fx.rel("Artist/01 one.mp3"),
            existing: "01 One.mp3".to_owned(),
        }],
        "a warning, not a refusal: on ext4 these are two files"
    );
    assert_eq!(names_in(&fx.abs("Artist")), ["01 One.mp3", "01 one.mp3"]);
}

/// `check` answers every question without writing anything, which is what lets
/// task 10's preview be pure.
#[test]
fn check_writes_nothing_whatever_it_is_asked() {
    let fx = Fixture::realistic();
    let root = fx.music_dir();
    let opts = options(&fx);
    let before = fx.snapshot();

    let steps = [
        FsStep::MkDir {
            at: fx.rel("hiphop/MF DOOM/Mm..Food (2004)"),
        },
        rename(&fx, names::MF_DOOM_TRACK, "hiphop/moved.mp3"),
        rename(&fx, names::MF_DOOM_TRACK, names::SNOOP_TRACK),
        FsStep::CopyDelete {
            from: fx.rel(names::MERCURY_TRACK),
            to: fx.rel("pop/moved.mp3"),
        },
        FsStep::RemoveFile {
            target: fx.rel(names::KREAM_TRACK),
            backup: Some(fx.data_dir().join("backups/missing/x.mp3")),
        },
        FsStep::RemoveFile {
            target: fx.rel(names::KREAM_TRACK),
            backup: None,
        },
        FsStep::RmDirIfEmpty {
            at: fx.rel(names::MF_DOOM_ALBUM),
        },
        FsStep::RmDirIfEmpty {
            at: fx.rel("nothing/here"),
        },
    ];
    for step in &steps {
        // Whether it passes or is refused is each other test's business; that it
        // writes nothing is this one's.
        let _ = exec_fs::check(step, root, &opts);
    }
    before.assert_same(&fx.snapshot());
}

/// The expansion a plan uses: every directory created before it is needed, every
/// file moved individually, every emptied source directory offered up afterwards.
#[test]
fn a_directory_move_expands_into_reversible_per_file_steps() {
    let fx = Fixture::realistic();
    let root = fx.music_dir();
    let (from, to) = (
        fx.rel(names::MF_DOOM_ALBUM),
        fx.rel("hiphop/MF DOOM/2004 - Mm..Food"),
    );
    let before = fx.snapshot();

    let plan = exec_fs::expand_dir_move(&from, &to, root, Merge::Refuse).expect("it expands");
    assert!(plan.collisions.is_empty(), "{:?}", plan.collisions);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert_eq!(
        plan.steps.first(),
        Some(&FsStep::MkDir { at: to.clone() }),
        "the destination is created before anything moves into it"
    );
    assert_eq!(
        plan.steps.last(),
        Some(&FsStep::RmDirIfEmpty { at: from.clone() }),
        "and the source is offered up last"
    );
    let moves = plan
        .steps
        .iter()
        .filter(|step| matches!(step, FsStep::RenameFile { .. }))
        .count();
    assert_eq!(moves, 8, "three tracks and five aux files, one step each");

    let receipts: Vec<StepReceipt> = plan
        .steps
        .iter()
        .map(|step| run(&fx, step, &options(&fx)))
        .collect();
    assert!(
        !fx.abs(names::MF_DOOM_ALBUM).exists(),
        "the album directory is gone"
    );
    assert_eq!(
        names_in(&fx.abs("hiphop/MF DOOM/2004 - Mm..Food")).len(),
        8,
        "and everything arrived, the .nfo and the stray .m3u included"
    );

    for receipt in receipts.iter().rev() {
        exec_fs::revert(receipt, root)
            .unwrap_or_else(|err| panic!("{} should revert: {err}", receipt.step));
    }
    before.assert_same(&fx.snapshot());
}
