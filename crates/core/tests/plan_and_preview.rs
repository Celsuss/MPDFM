//! The planner's acceptance tests (task 10), one per criterion.
//!
//! Task 08 tested that one filesystem step is safe. This tests the layer that
//! decides *which* steps, and the two questions it has to get right before any
//! of them run: does this plan have an execution order, and is there anything in
//! the way. Every conflict class gets a test that it refuses the commit, and
//! every warning class a test that it does not — that split is the whole of
//! `docs/PLAN.md` D7, and a warning that quietly became a conflict would make
//! MPDFM refuse to move an album because it noticed a broken reference in the
//! next directory.
//!
//! The preview is snapshot-tested rather than asserted field by field, because
//! it is a thing a person reads: a change to it should have to be looked at.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use camino::Utf8Path;
use mpdfm_core::config::Config;
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::ops::exec_fs::{self, FsStep};
use mpdfm_core::ops::{Conflict, Effects, Operation, Plan, Warning};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::playlist::rewrite;
use mpdfm_core::testing::{Fixture, Snapshot, names};

/// The three things `validate` needs, for a fixture.
struct World {
    library: Library,
    index: PlaylistIndex,
    config: Config,
}

impl World {
    fn of(fx: &Fixture) -> Self {
        let config = fx.config();
        Self {
            library: Library::scan(fx.music_dir()).expect("the fixture scans"),
            index: PlaylistIndex::load(fx.playlist_dir()).0,
            config,
        }
    }

    fn validate(&self, plan: &Plan) -> Effects {
        plan.validate(&self.library, &self.index, &self.config)
    }
}

fn rel(path: &str) -> RelPath {
    RelPath::parse(path).unwrap_or_else(|err| panic!("{path:?} is not a RelPath: {err}"))
}

fn move_dir(from: &str, to: &str) -> Operation {
    Operation::MoveDir {
        from: rel(from),
        to: rel(to),
    }
}

fn move_file(from: &str, to: &str) -> Operation {
    Operation::MoveFile {
        from: rel(from),
        to: rel(to),
    }
}

fn delete(target: &str) -> Operation {
    Operation::Delete {
        target: rel(target),
    }
}

/// Every destination a plan's steps would create or vacate, as strings, in the
/// order they execute. What a test asserts about when it cares about expansion.
fn step_lines(effects: &Effects) -> Vec<String> {
    effects.fs_steps.iter().map(FsStep::to_string).collect()
}

/// The first conflict, failing the test with the whole preview if there is none.
fn sole_conflict(effects: &Effects) -> &Conflict {
    effects.conflicts.first().unwrap_or_else(|| {
        panic!(
            "expected a conflict; got this instead:\n{}",
            effects.render(100)
        )
    })
}

/// Assert that a warning of this shape is present and that it did not block.
fn warns(effects: &Effects, matches: impl Fn(&Warning) -> bool) {
    assert!(
        effects.warnings.iter().any(&matches),
        "expected a warning; got:\n{}",
        effects.render(100)
    );
    assert!(
        effects.conflicts.is_empty(),
        "a warning must not block the commit; got:\n{}",
        effects.render(100)
    );
    assert!(effects.is_committable());
}

// ---------------------------------------------------------------------------
// `validate` is pure.

#[test]
fn validating_writes_nothing_anywhere() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);
    let before = Snapshot::capture(fx.root());

    // One of everything, including the cases that go down the error paths: a
    // refused delete, a missing source, an occupied destination.
    let plan = Plan::of(vec![
        move_dir(names::MF_DOOM_ALBUM, "hiphop/MF DOOM/Mm..Food"),
        move_file(names::SNOOP_TRACK, "hiphop/singles/Smokin On.mp3"),
        delete(names::KREAM_TRACK),
        move_file("nowhere/gone.mp3", "somewhere/gone.mp3"),
        move_file(names::MERCURY_TRACK, names::KIND_OF_BLUE_TRACK),
    ]);

    let effects = world.validate(&plan);
    let _ = effects.render(80);

    // The whole fixture: music, playlists, state file, data directory.
    before.assert_same(&Snapshot::capture(fx.root()));
}

// ---------------------------------------------------------------------------
// Expansion.

#[test]
fn a_directory_move_expands_into_every_file_under_it_aux_files_included() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let plan = Plan::of(vec![move_dir(
        names::MF_DOOM_ALBUM,
        "hiphop/MF DOOM/Mm..Food (2004)",
    )]);
    let effects = world.validate(&plan);

    assert!(effects.conflicts.is_empty(), "{:?}", effects.conflicts);

    // Three tracks and five aux files, and not one of them left behind.
    let moved: Vec<&str> = effects
        .fs_steps
        .iter()
        .filter_map(|step| match step {
            FsStep::RenameFile { from, .. } => Some(from.file_name()),
            _ => None,
        })
        .collect();
    assert_eq!(moved.len(), 8, "{moved:?}");
    for name in [
        "01 Beef Rap.mp3",
        "02 Hoe Cakes.mp3",
        "03 Potholderz (feat. Count Bass D).mp3",
        "folder.jpg",
        "info.nfo",
        "mm..food.sfv",
        "eac.log",
        "Mm..Food.m3u",
    ] {
        assert!(moved.contains(&name), "{name} was left behind: {moved:?}");
    }

    assert_eq!(effects.summary.files_moved, 8);
    assert_eq!(effects.summary.audio_moved, 3);
    assert_eq!(effects.summary.files_deleted, 0);

    // Directories first, files next, removals last — nothing depends on a
    // directory that does not exist yet.
    let lines = step_lines(&effects);
    let last_mkdir = lines.iter().rposition(|l| l.starts_with("mkdir")).unwrap();
    let first_rename = lines.iter().position(|l| l.starts_with("rename")).unwrap();
    let first_rmdir = lines
        .iter()
        .position(|l| l.starts_with("rmdir-if-empty"))
        .unwrap();
    assert!(last_mkdir < first_rename);
    assert!(first_rename < first_rmdir);
}

#[test]
fn a_directory_move_leaves_the_similarly_named_directory_beside_it_alone() {
    let fx = Fixture::builder()
        .album("hiphop/MF DOOM", &["01 Beef Rap.mp3"])
        .album("hiphop/MF DOOM Instrumentals", &["01 Beef Rap.mp3"])
        .playlist(
            "Hip hop.m3u",
            &[
                "hiphop/MF DOOM/01 Beef Rap.mp3",
                "hiphop/MF DOOM Instrumentals/01 Beef Rap.mp3",
            ],
        )
        .build();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![move_dir(
        "hiphop/MF DOOM",
        "hiphop/Daniel Dumile",
    )]));

    assert_eq!(effects.summary.files_moved, 1);
    assert_eq!(effects.summary.lines_rewritten, 1);
    for step in &effects.fs_steps {
        assert!(
            !step.to_string().contains("Instrumentals"),
            "the neighbour was touched: {step}"
        );
    }
}

#[test]
fn a_file_move_takes_its_playlist_lines_and_nothing_elses() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    // The MF DOOM track is named by two playlists.
    let effects = world.validate(&Plan::of(vec![move_file(
        names::MF_DOOM_TRACK,
        "hiphop/singles/01 Beef Rap.mp3",
    )]));

    assert_eq!(effects.summary.files_moved, 1);
    assert_eq!(effects.summary.playlists_affected, 2);
    assert_eq!(effects.summary.lines_rewritten, 2);
    assert_eq!(effects.summary.lines_removed, 0);
    assert_eq!(effects.ops[0].playlists, 2);
}

// ---------------------------------------------------------------------------
// Conflicts. Each one blocks the commit.

#[test]
fn an_occupied_destination_is_a_conflict() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![move_file(
        names::MF_DOOM_TRACK,
        names::SNOOP_TRACK,
    )]));

    assert!(matches!(
        sole_conflict(&effects),
        Conflict::DestinationExists { op: 0, .. }
    ));
    assert!(!effects.is_committable());
}

#[test]
fn two_operations_with_the_same_destination_are_a_conflict() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![
        move_file(names::MF_DOOM_TRACK, "hiphop/singles/track.mp3"),
        move_file(names::SNOOP_TRACK, "hiphop/singles/track.mp3"),
    ]));

    assert!(
        effects.conflicts.iter().any(|conflict| matches!(
            conflict,
            Conflict::DuplicateDestination {
                first: 0,
                second: 1,
                ..
            }
        )),
        "{:?}",
        effects.conflicts
    );
    assert!(!effects.is_committable());
    // Both rows are marked, so the pending view can point at either.
    assert!(effects.ops.iter().all(|op| op.refused));
}

#[test]
fn a_missing_source_is_a_conflict() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![move_file(
        "hiphop/never existed.mp3",
        "hiphop/somewhere.mp3",
    )]));

    assert!(matches!(
        sole_conflict(&effects),
        Conflict::SourceMissing { op: 0, .. }
    ));
    assert!(!effects.is_committable());
}

#[test]
fn a_destination_that_leaves_the_library_is_a_conflict() {
    // The music directory holds a symlink out to a directory beside it; a move
    // into the link would write outside the root, which invariant 5 forbids.
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3"])
        .build();
    std::fs::create_dir_all(fx.root().join("elsewhere").as_std_path()).expect("create elsewhere");
    std::os::unix::fs::symlink(
        fx.root().join("elsewhere").as_std_path(),
        fx.music_dir().join("out").as_std_path(),
    )
    .expect("create the escaping symlink");

    let world = World::of(&fx);
    let effects = world.validate(&Plan::of(vec![move_file(
        "hiphop/album/01 Beef Rap.mp3",
        "out/01 Beef Rap.mp3",
    )]));

    assert!(
        effects
            .conflicts
            .iter()
            .any(|conflict| matches!(conflict, Conflict::OutsideRoot { op: 0, .. })),
        "{:?}",
        effects.conflicts
    );
    assert!(!effects.is_committable());
}

#[test]
fn a_directory_that_cannot_be_written_to_is_a_conflict() {
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3"])
        .album("hiphop/locked", &["02 Hoe Cakes.mp3"])
        .build();
    let locked = fx.abs("hiphop/locked");
    let restore = std::fs::metadata(locked.as_std_path())
        .expect("stat")
        .permissions();
    std::fs::set_permissions(locked.as_std_path(), std::fs::Permissions::from_mode(0o555))
        .expect("make it read-only");

    let world = World::of(&fx);
    let effects = world.validate(&Plan::of(vec![move_file(
        "hiphop/album/01 Beef Rap.mp3",
        "hiphop/locked/01 Beef Rap.mp3",
    )]));

    std::fs::set_permissions(locked.as_std_path(), restore).expect("put the mode back");

    assert!(
        effects
            .conflicts
            .iter()
            .any(|conflict| matches!(conflict, Conflict::NotWritable { op: 0, .. })),
        "{:?}",
        effects.conflicts
    );
    assert!(!effects.is_committable());
}

#[test]
fn a_delete_with_deletion_disabled_is_a_conflict() {
    let fx = Fixture::realistic();
    let mut world = World::of(&fx);
    world.config.delete_enabled = false;

    let effects = world.validate(&Plan::of(vec![delete(names::KREAM_TRACK)]));

    assert!(matches!(
        sole_conflict(&effects),
        Conflict::DeleteDisabled { op: 0, .. }
    ));
    assert!(!effects.is_committable());
    // And nothing was planned for it: a refused delete expands to no steps.
    assert!(effects.fs_steps.is_empty());
}

#[test]
fn a_destination_that_differs_only_by_case_from_an_existing_file_is_a_conflict_here() {
    // On a case-insensitive filesystem the destination *is* the existing entry,
    // so `exec_fs` refuses it as occupied and this is a conflict. ext4 is
    // case-sensitive, so the same shape is staged here by making the destination
    // genuinely exist under the other spelling.
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3"])
        .album("hiphop/Album", &["02 Hoe Cakes.mp3"])
        .build();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![move_file(
        "hiphop/album/01 Beef Rap.mp3",
        "hiphop/Album/02 Hoe Cakes.mp3",
    )]));

    assert!(matches!(
        sole_conflict(&effects),
        Conflict::DestinationExists { op: 0, .. }
    ));
    assert!(!effects.is_committable());
}

// ---------------------------------------------------------------------------
// Chained moves. The roadmap's open question, decided in `ops::plan`.

#[test]
fn a_chain_is_sorted_so_that_the_destination_is_vacated_first() {
    let fx = Fixture::builder()
        .album("hiphop/a", &["01 Beef Rap.mp3"])
        .album("hiphop/b", &["02 Hoe Cakes.mp3"])
        .build();
    let world = World::of(&fx);

    // Staged in the order that would fail if it were executed as written.
    let plan = Plan::of(vec![
        move_dir("hiphop/a", "hiphop/b"),
        move_dir("hiphop/b", "hiphop/c"),
    ]);
    let effects = world.validate(&plan);

    assert!(
        effects.conflicts.is_empty(),
        "a chain is ordered, not refused:\n{}",
        effects.render(100)
    );
    assert!(effects.is_committable());

    // `b → c` runs first; the rows are in execution order and carry the index
    // the user staged them at.
    assert_eq!(effects.ops[0].index, 1);
    assert_eq!(effects.ops[1].index, 0);

    // And executing them in that order actually works.
    let options = exec_fs::Options::from_config(&world.config);
    for step in &effects.fs_steps {
        exec_fs::execute_with(step, fx.music_dir(), &options)
            .unwrap_or_else(|err| panic!("{step} failed: {err}"));
    }
    assert!(fx.abs("hiphop/b/01 Beef Rap.mp3").is_file());
    assert!(fx.abs("hiphop/c/02 Hoe Cakes.mp3").is_file());
    assert!(!fx.abs("hiphop/a").exists());
}

#[test]
fn a_ring_of_moves_has_no_order_and_is_refused() {
    let fx = Fixture::builder()
        .album("hiphop/a", &["01 Beef Rap.mp3"])
        .album("hiphop/b", &["02 Hoe Cakes.mp3"])
        .build();
    let world = World::of(&fx);

    // A swap: neither can go first.
    let effects = world.validate(&Plan::of(vec![
        move_dir("hiphop/a", "hiphop/b"),
        move_dir("hiphop/b", "hiphop/a"),
    ]));

    let Conflict::Cycle { ops, .. } = sole_conflict(&effects) else {
        panic!("expected a cycle, got {:?}", effects.conflicts);
    };
    assert_eq!(ops, &[0, 1]);
    assert!(!effects.is_committable());
    assert!(
        effects.fs_steps.is_empty(),
        "half a swap must never be planned"
    );
    // Both rows survive, so the user can unstage one of them.
    assert_eq!(effects.ops.len(), 2);
    assert!(effects.ops.iter().all(|op| op.refused));
}

#[test]
fn the_two_hop_reading_of_a_chain_is_a_missing_source() {
    let fx = Fixture::builder()
        .album("hiphop/a", &["01 Beef Rap.mp3"])
        .build();
    let world = World::of(&fx);

    // "move a to b, then move that on to c" — `b` does not exist yet, and
    // planning against the library as it is now says so.
    let effects = world.validate(&Plan::of(vec![
        move_dir("hiphop/a", "hiphop/b"),
        move_dir("hiphop/b", "hiphop/c"),
    ]));

    assert!(
        effects
            .conflicts
            .iter()
            .any(|conflict| matches!(conflict, Conflict::SourceMissing { op: 1, .. })),
        "{:?}",
        effects.conflicts
    );
    assert!(!effects.is_committable());
}

// ---------------------------------------------------------------------------
// Warnings. None of them blocks the commit.

#[test]
fn a_delete_warns_that_playlist_lines_go_away_and_commits_anyway() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![delete(names::MF_DOOM_TRACK)]));

    warns(&effects, |warning| {
        matches!(warning, Warning::PlaylistLinesRemoved { .. })
    });
    assert!(effects.summary.lines_removed > 0);
    assert_eq!(effects.summary.lines_rewritten, 0);
}

#[test]
fn a_file_in_mpds_queue_warns_and_commits_anyway() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let mut effects = world.validate(&Plan::of(vec![move_file(
        names::KREAM_TRACK,
        "electronic/singles/01 So Hï.mp3",
    )]));
    // Task 13 is what can ask MPD; the class is exercised here so that the
    // preview's handling of it cannot rot before then.
    effects.warnings.push(Warning::InMpdQueue {
        path: rel(names::KREAM_TRACK),
    });

    warns(&effects, |warning| {
        matches!(warning, Warning::InMpdQueue { .. })
    });
    assert!(effects.render(140).contains("MPD's current queue"));
    // And at 80 columns, where the message has to be shortened, the verdict is
    // still the part that survives.
    assert!(effects.render(80).contains("will need a requeue"));
}

#[test]
fn an_already_broken_reference_in_an_affected_directory_warns_and_commits_anyway() {
    // The realistic fixture's one broken reference is `pop/gone/missing.mp3`.
    let fx = Fixture::builder()
        .album("pop/gone", &["01 Beef Rap.mp3"])
        .playlist(
            "Pop.m3u",
            &["pop/gone/01 Beef Rap.mp3", "pop/gone/missing.mp3"],
        )
        .build();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![move_file(
        "pop/gone/01 Beef Rap.mp3",
        "pop/here/01 Beef Rap.mp3",
    )]));

    warns(&effects, |warning| {
        matches!(warning, Warning::BrokenReferenceNearby { .. })
    });
}

#[test]
fn a_case_difference_on_a_case_sensitive_filesystem_warns_and_commits_anyway() {
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"])
        .build();
    let world = World::of(&fx);

    // `01 beef rap.mp3` beside `01 Beef Rap.mp3`: two entries on ext4, one on
    // APFS. The move goes ahead and says so.
    let effects = world.validate(&Plan::of(vec![move_file(
        "hiphop/album/02 Hoe Cakes.mp3",
        "hiphop/album/01 beef rap.mp3",
    )]));

    warns(&effects, |warning| {
        matches!(warning, Warning::CaseDifference { .. })
    });
}

#[test]
fn a_name_that_is_not_utf8_stays_behind_with_a_warning_and_the_rest_commits() {
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3"])
        .non_utf8_file("hiphop/album", b"cover\xff.jpg")
        .build();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![move_dir("hiphop/album", "hiphop/renamed")]));

    warns(&effects, |warning| {
        matches!(warning, Warning::SkippedUnnamable { .. })
    });
    // The track still moves; only the unnamable entry stays.
    assert_eq!(effects.summary.files_moved, 1);
}

#[test]
fn a_symlink_inside_a_moved_directory_stays_behind_with_a_warning() {
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3"])
        .build();
    std::os::unix::fs::symlink(
        fx.abs("hiphop/album/01 Beef Rap.mp3").as_std_path(),
        fx.abs("hiphop/album/link.mp3").as_std_path(),
    )
    .expect("create the symlink");

    let world = World::of(&fx);
    let effects = world.validate(&Plan::of(vec![move_dir("hiphop/album", "hiphop/renamed")]));

    warns(&effects, |warning| {
        matches!(warning, Warning::SkippedSymlink { .. })
    });
}

#[test]
fn splitting_an_album_warns_and_commits_anyway() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    // One of three MF DOOM tracks leaves; the other two stay.
    let effects = world.validate(&Plan::of(vec![move_file(
        names::MF_DOOM_TRACK,
        "hiphop/singles/01 Beef Rap.mp3",
    )]));

    warns(&effects, |warning| {
        matches!(warning, Warning::AlbumSplit { .. })
    });
}

#[test]
fn moving_a_whole_album_does_not_warn_that_it_is_split() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![move_dir(
        names::MF_DOOM_ALBUM,
        "hiphop/MF DOOM/Mm..Food (2004)",
    )]));

    assert!(
        !effects
            .warnings
            .iter()
            .any(|warning| matches!(warning, Warning::AlbumSplit { .. })),
        "{:?}",
        effects.warnings
    );
}

// ---------------------------------------------------------------------------
// The preview.

/// A plan with a bit of everything in it, so one snapshot covers the layout.
fn showcase(world: &World) -> Effects {
    world.validate(&Plan::of(vec![
        move_dir(names::MF_DOOM_ALBUM, "hiphop/MF DOOM/Mm..Food (2004)"),
        delete(names::SNOOP_TRACK),
    ]))
}

#[test]
fn the_preview_renders_the_same_way_at_eighty_columns() {
    let fx = Fixture::realistic();
    let effects = showcase(&World::of(&fx));
    insta::assert_snapshot!("preview_80", effects.render(80));
}

#[test]
fn the_preview_renders_the_same_way_at_a_hundred_and_twenty_columns() {
    let fx = Fixture::realistic();
    let effects = showcase(&World::of(&fx));
    insta::assert_snapshot!("preview_120", effects.render(120));
}

#[test]
fn a_refused_plan_renders_its_conflicts() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);
    let effects = world.validate(&Plan::of(vec![
        move_file(names::MF_DOOM_TRACK, names::SNOOP_TRACK),
        move_file("hiphop/never existed.mp3", "hiphop/somewhere.mp3"),
    ]));
    insta::assert_snapshot!("preview_refused_80", effects.render(80));
}

#[test]
fn an_empty_plan_says_so() {
    let fx = Fixture::realistic();
    let effects = World::of(&fx).validate(&Plan::new());

    assert_eq!(effects.render(80), "NOTHING PENDING");
    assert!(!effects.is_committable());
    assert!(effects.summary.is_empty());
}

#[test]
fn no_rendered_line_is_ever_wider_than_it_was_asked_for() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);
    // The fixture's longest names are the Snoop and Mercury directories, which
    // is what makes narrow widths worth checking at all.
    let effects = world.validate(&Plan::of(vec![
        move_dir(names::SNOOP_ALBUM, "hiphop/Snoop Dogg/Mac + Devin (2011)"),
        move_dir(names::MERCURY_CD1, "pop/Mercury/CD 1"),
        delete(names::MERCURY_CUE),
    ]));

    for width in [40, 60, 80, 100, 120, 200] {
        for line in effects.render(width).lines() {
            assert!(
                line.chars().count() <= width,
                "at width {width} this line is {}: {line:?}",
                line.chars().count()
            );
        }
    }
}

#[test]
fn the_preview_survives_a_round_trip_through_json() {
    let fx = Fixture::realistic();
    let effects = showcase(&World::of(&fx));

    let json = serde_json::to_string(&effects).expect("Effects serializes");
    let back: Effects = serde_json::from_str(&json).expect("Effects deserializes");

    assert_eq!(back, effects);
    assert_eq!(back.render(80), effects.render(80));
}

// ---------------------------------------------------------------------------
// The summary is what commit does.

/// Execute an `Effects` for real and report what actually changed: files moved,
/// files deleted, playlist lines rewritten, playlist lines removed.
fn commit(fx: &Fixture, config: &Config, effects: &Effects) -> (usize, usize, usize, usize) {
    let options = exec_fs::Options::from_config(config);
    let mut moved = 0;
    let mut deleted = 0;

    // Task 11's second phase, stood in for: every backup directory exists and is
    // durable before the first step runs. `exec_fs` refuses to create one itself,
    // so that a step can never be the thing that invents a place to put the only
    // copy of a file.
    for step in &effects.fs_steps {
        if let FsStep::RemoveFile {
            backup: Some(backup),
            ..
        } = step
            && let Some(parent) = backup.parent()
        {
            std::fs::create_dir_all(parent.as_std_path()).expect("make the backup directory");
        }
    }

    for step in &effects.fs_steps {
        let receipt = exec_fs::execute_with(step, fx.music_dir(), &options)
            .unwrap_or_else(|err| panic!("{step} failed: {err}"));
        match receipt.step {
            FsStep::RenameFile { .. } | FsStep::CopyDelete { .. } => moved += 1,
            FsStep::RemoveFile { .. } => deleted += 1,
            _ => {}
        }
    }

    let backup_dir = fx.data_dir().join("playlist-backups");
    rewrite::apply(&effects.playlist_edits, &backup_dir).expect("the playlist edits apply");

    let rewritten = effects
        .playlist_edits
        .iter()
        .map(mpdfm_core::playlist::rewrite::PlaylistEdit::rewrites)
        .sum();
    let removed = effects
        .playlist_edits
        .iter()
        .map(mpdfm_core::playlist::rewrite::PlaylistEdit::removals)
        .sum();
    (moved, deleted, rewritten, removed)
}

#[test]
fn every_summary_count_is_what_the_commit_actually_did() {
    // Not one shape but every shape the planner produces, each committed for
    // real against its own fixture and compared with what the preview promised.
    let plans: Vec<(&str, Vec<Operation>)> = vec![
        (
            "one album directory",
            vec![move_dir(names::MF_DOOM_ALBUM, "hiphop/MF DOOM/Mm..Food")],
        ),
        (
            "one track out of an album",
            vec![move_file(names::MF_DOOM_TRACK, "hiphop/singles/Beef.mp3")],
        ),
        ("one delete", vec![delete(names::MF_DOOM_TRACK)]),
        (
            "a delete and a move together",
            vec![
                delete(names::SNOOP_TRACK),
                move_file(names::KREAM_TRACK, "electronic/singles/So Hi.mp3"),
            ],
        ),
        (
            "a disc out of a multi-disc set",
            vec![move_dir(names::MERCURY_CD1, "pop/Mercury Acts 1")],
        ),
        (
            "two albums at once",
            vec![
                move_dir(names::MF_DOOM_ALBUM, "hiphop/MF DOOM/Mm..Food"),
                move_dir(names::KREAM_ALBUM, "electronic/KREAM/So Hi"),
            ],
        ),
    ];

    for (what, ops) in plans {
        let fx = Fixture::realistic();
        let world = World::of(&fx);
        let effects = world.validate(&Plan::of(ops));
        assert!(
            effects.is_committable(),
            "{what} should commit:\n{}",
            effects.render(100)
        );

        let (moved, deleted, rewritten, removed) = commit(&fx, &world.config, &effects);
        assert_eq!(effects.summary.files_moved, moved, "{what}: files moved");
        assert_eq!(
            effects.summary.files_deleted, deleted,
            "{what}: files deleted"
        );
        assert_eq!(
            effects.summary.lines_rewritten, rewritten,
            "{what}: lines rewritten"
        );
        assert_eq!(
            effects.summary.lines_removed, removed,
            "{what}: lines removed"
        );
        assert_eq!(
            effects.summary.playlists_affected,
            effects.playlist_edits.len(),
            "{what}: playlists"
        );
    }
}

#[test]
fn after_a_commit_every_playlist_entry_that_resolved_before_still_resolves() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![
        move_dir(names::MF_DOOM_ALBUM, "hiphop/MF DOOM/Mm..Food (2004)"),
        move_dir(names::MERCURY_CD1, "pop/Mercury/Acts 1"),
    ]));
    assert!(effects.is_committable(), "{}", effects.render(100));

    let before: Vec<RelPath> = world
        .index
        .paths()
        .into_iter()
        .filter(|path| world.library.get(path).is_some())
        .cloned()
        .collect();
    assert!(!before.is_empty());

    commit(&fx, &world.config, &effects);

    let after_library = Library::scan(fx.music_dir()).expect("rescan");
    let after_index = PlaylistIndex::load(fx.playlist_dir()).0;
    assert_eq!(
        after_index.reference_count(),
        world.index.reference_count(),
        "no reference was lost"
    );
    assert!(
        after_index.broken(&after_library).len() <= world.index.broken(&world.library).len(),
        "the commit broke a reference that used to resolve"
    );
    assert_eq!(after_index.paths().len(), world.index.paths().len());
}

// ---------------------------------------------------------------------------
// The library model and the planner agree.

#[test]
fn the_planner_counts_the_same_files_the_scanner_found() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let album = DirPath::parse(names::MF_DOOM_ALBUM).expect("an album directory");
    let in_model = world.library.files_in(&album).count();

    let effects = world.validate(&Plan::of(vec![move_dir(
        names::MF_DOOM_ALBUM,
        "hiphop/MF DOOM/Mm..Food",
    )]));

    assert_eq!(effects.summary.files_moved, in_model);
    assert_eq!(effects.ops[0].files, in_model);
    assert_eq!(
        effects.ops[0].bytes,
        world.library.files_in(&album).map(|e| e.size).sum::<u64>()
    );
}

#[test]
fn the_backup_a_delete_plans_lands_inside_the_data_directory() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);

    let effects = world.validate(&Plan::of(vec![delete(names::MF_DOOM_TRACK)]));
    let backup = effects
        .fs_steps
        .iter()
        .find_map(|step| match step {
            FsStep::RemoveFile { backup, .. } => backup.clone(),
            _ => None,
        })
        .expect("a delete plans a backup");

    assert!(
        backup.starts_with(fx.data_dir()),
        "{backup} is not under {}",
        fx.data_dir()
    );
    // Mirrored, so two `cover.jpg` deletes from two albums keep both files.
    assert!(backup.as_str().ends_with(names::MF_DOOM_TRACK));
}

#[test]
fn a_delete_is_never_planned_without_one() {
    let fx = Fixture::realistic();
    let world = World::of(&fx);
    let effects = world.validate(&Plan::of(vec![delete(names::MF_DOOM_TRACK)]));

    for step in &effects.fs_steps {
        if let FsStep::RemoveFile { target, backup } = step {
            assert!(backup.is_some(), "{target} would be unlinked for good");
        }
    }
}

/// `Utf8Path` is used in one assertion above; naming it here keeps the import
/// honest rather than sprinkling fully-qualified paths through the test.
#[allow(dead_code)]
fn _uses(path: &Utf8Path) -> &str {
    path.as_str()
}
