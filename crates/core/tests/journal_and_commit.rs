//! The journal and the two-phase commit's acceptance tests (task 11), one per
//! criterion.
//!
//! Task 08 tested that one filesystem step is safe and task 10 that a plan is
//! worked out before any of it runs. This tests the part that actually changes
//! the library, and it tests it the only way that means anything: by killing the
//! commit at every boundary it has and showing that what is left on disk is
//! enough to put the library back.
//!
//! [`roll_back`] is that proof. It is task 12's algorithm in fifteen lines —
//! restore the playlists from the backup directory, then reverse every step the
//! record marks `done`, in reverse order — and it runs against the record as the
//! crashed process left it, with nothing in memory to help. If a crash-injection
//! test passes, the record it produced is sufficient; if `undo` later needs
//! something that is not in there, one of these tests is where it will show up.
//!
//! No test here touches the real library: everything happens inside a
//! [`Fixture`]'s temp directory, and the journal, the backups and MPD's state
//! file are the fixture's own.

#![cfg(unix)]

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::config::Config;
use mpdfm_core::journal::record::{Record, Status, TxId};
use mpdfm_core::journal::store::{Store, syncs};
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::ops::commit::{self, CommitError, CommitWarning, Inject, Options, Previewed};
use mpdfm_core::ops::exec_fs::{self, FsStep};
use mpdfm_core::ops::{Committed, Effects, Operation, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::playlist::rewrite;
use mpdfm_core::testing::{Fixture, Snapshot, SnapshotEntryKind, names};

/// A fixture, with the three things a preview needs scanned from it.
///
/// Rebuilt rather than updated after a commit ([`World::rescan`]), the same way
/// the CLI and the TUI will: the model is derived from the disk, and a commit has
/// just changed the disk.
struct World {
    fx: Fixture,
    library: Library,
    index: PlaylistIndex,
    config: Config,
}

impl World {
    fn new(fx: Fixture) -> Self {
        let config = fx.config();
        Self {
            library: Library::scan(fx.music_dir()).expect("the fixture scans"),
            index: PlaylistIndex::load(fx.playlist_dir()).0,
            config,
            fx,
        }
    }

    fn realistic() -> Self {
        Self::new(Fixture::realistic())
    }

    fn rescan(&mut self) {
        self.library = Library::scan(self.fx.music_dir()).expect("the fixture scans");
        self.index = PlaylistIndex::load(self.fx.playlist_dir()).0;
    }

    fn effects(&self, plan: &Plan) -> Effects {
        plan.validate(&self.library, &self.index, &self.config)
    }

    /// Preview a plan and commit it, with the production options.
    fn commit(&self, plan: &Plan) -> mpdfm_core::Result<Committed> {
        self.commit_with(plan, &Options::default())
    }

    /// Preview a plan and commit it, with these options.
    fn commit_with(&self, plan: &Plan, options: &Options<'_>) -> mpdfm_core::Result<Committed> {
        let effects = self.effects(plan);
        assert!(
            effects.conflicts.is_empty(),
            "the test's own plan does not validate: {:?}",
            effects.conflicts
        );
        commit::commit_with(&self.previewed(plan, &effects), &self.config, options)
    }

    fn previewed<'a>(&'a self, plan: &'a Plan, effects: &'a Effects) -> Previewed<'a> {
        Previewed {
            plan,
            library: &self.library,
            effects,
        }
    }

    fn store(&self) -> Store {
        Store::at(self.fx.data_dir())
    }

    /// The record on disk, which is the only thing a crash leaves behind.
    fn record(&self, txid: &TxId) -> Record {
        self.store()
            .load(txid)
            .unwrap_or_else(|err| panic!("the record for {txid} should be readable: {err}"))
    }

    /// Everything a transaction may change: the library and the playlists. Not
    /// the data directory — that is where the journal lives, and it is *meant* to
    /// grow.
    fn state(&self) -> State {
        State {
            music: Snapshot::capture(self.fx.music_dir()),
            playlists: Snapshot::capture(self.fx.playlist_dir()),
        }
    }
}

/// The two trees a commit is allowed to touch, captured.
struct State {
    music: Snapshot,
    playlists: Snapshot,
}

impl State {
    /// Byte-for-byte identical, or a failure naming what differs.
    fn assert_same(&self, other: &Self) {
        self.music.assert_same(&other.music);
        self.playlists.assert_same(&other.playlists);
    }
}

/// Task 12's rollback, in miniature, from the record alone.
///
/// The order is the reverse of the commit's: the playlists were written last, so
/// they go back first, and the steps are reversed innermost-last so that a
/// directory is recreated before the files that lived in it are moved back into
/// it. Restoring a playlist the commit never got to write is a no-op worth doing
/// rather than a state worth reasoning about — which is exactly why `undo` can be
/// unconditional about it.
fn roll_back(fx: &Fixture, record: &Record) {
    rewrite::restore(&record.playlist_edits, &record.backup_dir).unwrap_or_else(|err| {
        panic!(
            "the playlists should restore from {}: {err}",
            record.backup_dir
        )
    });

    for step in record.steps.iter().rev().filter(|step| step.done) {
        let receipt = step
            .receipt()
            .unwrap_or_else(|| panic!("`{}` is marked done with no receipt", step.step));
        exec_fs::revert(&receipt, fx.music_dir())
            .unwrap_or_else(|err| panic!("`{}` should be reversible: {err}", step.step));
    }
}

/// How many of a snapshot's entries are not directories.
fn files_in(snapshot: &Snapshot) -> usize {
    snapshot
        .entries()
        .filter(|(_, entry)| !matches!(entry.kind, SnapshotEntryKind::Dir))
        .count()
}

fn rel(path: &str) -> RelPath {
    RelPath::parse(path).unwrap_or_else(|err| panic!("{path:?} is not a RelPath: {err}"))
}

/// The album move nearly every test here uses: eight files, two playlists, one
/// directory created and one left empty behind.
fn album_move() -> Plan {
    Plan::of(vec![Operation::MoveDir {
        from: rel(names::MF_DOOM_ALBUM),
        to: rel("hiphop/MF DOOM/Mm..Food (2004)"),
    }])
}

/// A second, unrelated album move, for the tests that need two transactions.
fn other_album_move() -> Plan {
    Plan::of(vec![Operation::MoveDir {
        from: rel(names::SNOOP_ALBUM),
        to: rel("hiphop/Snoop Dogg/Mac + Devin (2011)"),
    }])
}

/// Where a step's text appears in a committed record, for the message of a
/// failure that needs to say which step.
fn step_lines(record: &Record) -> Vec<String> {
    record
        .steps
        .iter()
        .map(|step| format!("{} done={}", step.step, step.done))
        .collect()
}

// ---------------------------------------------------------------------------
// The happy path, which every criterion below is a failure mode of.

#[test]
fn a_committed_transaction_moves_the_files_and_rewrites_the_playlists() {
    let world = World::realistic();
    let committed = world.commit(&album_move()).expect("the plan commits");

    assert!(
        world
            .fx
            .abs("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3")
            .is_file(),
        "the track should be at its new path"
    );
    assert!(
        !world.fx.abs(names::MF_DOOM_ALBUM).exists(),
        "the emptied album directory should be gone"
    );

    // The aux files travel with the album (`docs/PLAN.md` §6).
    for aux in ["folder.jpg", "info.nfo", "eac.log", "Mm..Food.m3u"] {
        assert!(
            world
                .fx
                .abs(&format!("hiphop/MF DOOM/Mm..Food (2004)/{aux}"))
                .is_file(),
            "{aux} should have moved with the album"
        );
    }

    let playlist = std::fs::read_to_string(world.fx.playlist_path(names::HIP_HOP_PLAYLIST))
        .expect("the playlist is readable");
    assert!(
        playlist.contains("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3"),
        "the playlist line should name the new path:\n{playlist}"
    );
    assert_eq!(
        committed.record.summary.playlists_affected, 2,
        "both playlists that name the track should have been rewritten"
    );
}

#[test]
fn a_completed_record_lists_every_step_as_done() {
    let world = World::realistic();
    let committed = world.commit(&album_move()).expect("the plan commits");
    let record = world.record(&committed.txid);

    assert_eq!(record.status, Status::Complete);
    assert!(record.finished_at.is_some(), "a finished record is stamped");
    assert!(!record.steps.is_empty(), "the plan expands into steps");
    assert!(
        record.all_done(),
        "every step of a completed transaction is done:\n{}",
        step_lines(&record).join("\n")
    );
    for step in &record.steps {
        assert!(
            step.receipt().is_some(),
            "`{}` is done, so it has a receipt to reverse it with",
            step.step
        );
        assert!(step.error.is_none(), "`{}` did not fail", step.step);
    }
    assert!(record.is_undoable(), "it should be undoable");
    assert_eq!(
        record.ops,
        album_move().ops(),
        "the record keeps the user's plan"
    );
}

#[test]
fn committing_then_rolling_back_from_the_record_restores_the_library_exactly() {
    let world = World::realistic();
    let before = world.state();

    let committed = world.commit(&album_move()).expect("the plan commits");
    let after = world.state();
    assert!(
        !after.music.entries().eq(before.music.entries()),
        "the commit should have changed something"
    );

    roll_back(&world.fx, &world.record(&committed.txid));
    before.assert_same(&world.state());
}

// ---------------------------------------------------------------------------
// Criterion: the journal file exists with `status: pending` before any file is
// moved.

#[test]
fn the_pending_record_is_on_disk_before_the_first_file_moves() {
    let world = World::realistic();
    let before = world.state();

    let err = world
        .commit_with(
            &album_move(),
            &Options {
                inject: Inject::AfterPending,
                ..Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");

    let txid = injected_txid(&err);
    let record = world.record(&txid);
    assert_eq!(record.status, Status::Pending);
    assert!(
        record.finished_at.is_none(),
        "a pending record has not finished"
    );
    assert!(
        !record.steps.is_empty() && record.steps.iter().all(|step| !step.done),
        "the record names every step and claims none of them:\n{}",
        step_lines(&record).join("\n")
    );
    before.assert_same(&world.state());
}

#[test]
fn a_pending_record_names_the_roots_and_the_backups_it_depends_on() {
    let world = World::realistic();
    let err = world
        .commit_with(
            &album_move(),
            &Options {
                inject: Inject::AfterPending,
                ..Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");

    let txid = injected_txid(&err);
    let record = world.record(&txid);
    assert_eq!(record.music_dir, world.fx.music_dir());
    assert_eq!(record.playlist_dir, world.fx.playlist_dir());
    assert_eq!(record.backup_dir, world.store().backup_dir(&txid));
    assert!(record.backup_dir.is_dir(), "the backup directory exists");
    assert!(!record.backup_pruned);

    // The playlists were copied before the record was written, so a rollback from
    // this record has something to restore from.
    for playlist in [names::HIP_HOP_PLAYLIST, names::MF_DOOM_PLAYLIST] {
        assert!(
            record.backup_dir.join(playlist).is_file(),
            "{playlist} should have been backed up before the record was written"
        );
    }
    assert_eq!(
        record.state_backup.as_deref(),
        Some("state"),
        "MPD's state file is backed up too"
    );
}

// ---------------------------------------------------------------------------
// Criterion: `fsync` is called on journal writes and on `backup_dir`.

#[test]
fn the_record_and_the_backup_directory_are_both_synced() {
    let world = World::realistic();
    let committed = world.commit(&album_move()).expect("the plan commits");
    let store = world.store();

    assert!(
        syncs::was_synced(&store.record_path(&committed.txid)),
        "the record's bytes must be durable before the first mutation"
    );
    assert!(
        syncs::was_synced(store.journal_dir()),
        "and so must its name, or the file can come back missing"
    );
    assert!(
        syncs::was_synced(&store.backup_dir(&committed.txid)),
        "the backup directory is synced when it is created"
    );
    assert!(
        syncs::was_synced(&store.steps_path(&committed.txid)),
        "and every completed step is synced as it is appended"
    );
}

#[test]
fn the_step_log_is_folded_into_the_record_and_removed_when_it_is_no_longer_needed() {
    let mut world = World::realistic();
    let store = world.store();

    let crashed = world
        .commit_with(
            &album_move(),
            &Options {
                inject: Inject::AfterStep(2),
                ..Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let crashed = injected_txid(&crashed);

    // The record on disk still says every step is pending; the log is what knows
    // better, and `load` is where the two meet.
    let raw = read_json(&store.record_path(&crashed));
    let steps = raw["steps"].as_array().expect("an array of steps");
    assert!(
        steps
            .iter()
            .all(|step| step["done"] == serde_json::json!(false)),
        "the record itself was written before the first step ran"
    );
    assert_eq!(
        store
            .read_steps(&crashed)
            .expect("the log is readable")
            .len(),
        3,
        "and the log has one line per step that finished"
    );
    assert_eq!(world.record(&crashed).completed().count(), 3);

    roll_back(&world.fx, &world.record(&crashed));
    world.rescan();

    let completed = world.commit(&album_move()).expect("the retry commits");
    assert!(
        !store.steps_path(&completed.txid).exists(),
        "a completed record lists every step itself, so the log is removed"
    );
    assert!(world.record(&completed.txid).all_done());
}

#[test]
fn a_step_log_line_that_a_crash_cut_in_half_is_ignored() {
    let world = World::realistic();
    let store = world.store();
    let before = world.state();

    let crashed = world
        .commit_with(
            &album_move(),
            &Options {
                inject: Inject::AfterStep(3),
                ..Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let crashed = injected_txid(&crashed);

    // What losing power in the middle of the append would leave: the last line
    // half written. The step it described was never acknowledged, so reading it as
    // not-done is the safe direction — and the rest of the log still counts.
    let path = store.steps_path(&crashed);
    let log = std::fs::read_to_string(&path).expect("readable");
    let whole = log.rfind('\n').expect("more than one line");
    let torn = &log[..whole];
    let cut = torn.len() - (torn.len() - torn.rfind('\n').expect("at least two lines")) / 2;
    std::fs::write(&path, &log[..cut]).expect("writable");

    let record = world.record(&crashed);
    assert_eq!(
        record.completed().count(),
        3,
        "three whole lines survived of four:\n{}",
        step_lines(&record).join("\n")
    );

    // The step whose line was lost did happen, though: its destination is there
    // and its source is not. That is the one state a rollback cannot work out from
    // receipts alone, and it is `recover`'s to notice (task 12) — a planned step
    // whose destination exists and whose source does not is a step that ran.
    let lost = &record.steps[3];
    assert!(!lost.done, "the record cannot know about it");
    let FsStep::RenameFile { from, to } = &lost.step else {
        panic!("step 4 of an album move is a file move, not {}", lost.step)
    };
    assert!(
        !from.to_abs(world.fx.music_dir()).exists() && to.to_abs(world.fx.music_dir()).is_file(),
        "`{}` ran without being recorded, which is exactly what `recover` has to \
         detect by looking",
        lost.step
    );
    // Nothing was lost: every file is still somewhere, which is the property that
    // makes recovery possible at all. (Directories are not conserved — the move
    // created two of them, and that is what the `MkDir` receipt is for.)
    assert_eq!(
        files_in(&before.music),
        files_in(&world.state().music),
        "every file is still there, some of them in their new places"
    );
}

// ---------------------------------------------------------------------------
// Criterion: re-validation catches a file that was modified between preview and
// commit.

#[test]
fn a_file_written_to_between_preview_and_commit_refuses_the_commit() {
    let world = World::realistic();
    let plan = album_move();
    let effects = world.effects(&plan);

    // The same size, different bytes, and therefore a newer mtime: nothing about
    // the plan's *steps* changes, which is the whole reason this check exists.
    world.fx.flip_byte(&world.fx.abs(names::MF_DOOM_TRACK));
    let before = world.state();

    let err = commit::commit(&world.previewed(&plan, &effects), &world.config)
        .expect_err("the preview is stale");

    let message = err.to_string();
    assert!(
        message.contains(names::MF_DOOM_TRACK) && message.contains("has changed since the preview"),
        "the message should name the file that changed:\n{message}"
    );
    before.assert_same(&world.state());
    assert_eq!(
        world.store().list().expect("the journal lists"),
        Vec::new(),
        "a refused commit writes no record"
    );
}

#[test]
fn a_file_that_vanished_between_preview_and_commit_refuses_the_commit() {
    let world = World::realistic();
    let plan = album_move();
    let effects = world.effects(&plan);

    std::fs::remove_file(world.fx.abs(names::MF_DOOM_TRACK)).expect("the fixture is writable");

    let err = commit::commit(&world.previewed(&plan, &effects), &world.config)
        .expect_err("the preview is stale");
    let message = err.to_string();
    assert!(
        message.contains(names::MF_DOOM_TRACK),
        "the message should name the missing file:\n{message}"
    );
}

#[test]
fn a_playlist_line_that_changed_since_the_preview_refuses_the_commit() {
    let world = World::realistic();
    let plan = album_move();
    let effects = world.effects(&plan);

    // The very line the plan was going to rewrite now names something else.
    let path = world.fx.playlist_path(names::HIP_HOP_PLAYLIST);
    let body = std::fs::read_to_string(&path).expect("readable");
    std::fs::write(
        &path,
        body.replace(names::MF_DOOM_TRACK, names::KREAM_TRACK),
    )
    .expect("writable");
    let before = world.state();

    let err = commit::commit(&world.previewed(&plan, &effects), &world.config)
        .expect_err("the preview is stale");
    let message = err.to_string();
    assert!(
        message.contains("playlist"),
        "the message should say the playlist edits changed:\n{message}"
    );
    before.assert_same(&world.state());
    assert_eq!(
        world.store().list().expect("lists"),
        Vec::new(),
        "and nothing was journaled"
    );
}

#[test]
fn an_unrelated_line_added_to_a_playlist_does_not_stop_the_commit() {
    let world = World::realistic();
    let plan = album_move();
    let effects = world.effects(&plan);

    // Another program appends a track MPDFM is not moving. The plan still
    // rewrites exactly the lines it said it would, so there is nothing stale
    // about it — and re-validation that refused this would make MPDFM unusable
    // alongside a running MPD.
    let path = world.fx.playlist_path(names::HIP_HOP_PLAYLIST);
    let mut body = std::fs::read_to_string(&path).expect("readable");
    body.push_str(&format!("{}\n", names::KREAM_TRACK));
    std::fs::write(&path, body).expect("writable");

    commit::commit(&world.previewed(&plan, &effects), &world.config).expect("it commits");

    let after = std::fs::read_to_string(&path).expect("readable");
    assert!(
        after.contains(names::KREAM_TRACK),
        "the line that was added since the preview survives byte-for-byte:\n{after}"
    );
    assert!(after.contains("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3"));
}

#[test]
fn a_destination_that_appeared_between_preview_and_commit_refuses_the_commit() {
    let world = World::realistic();
    let plan = album_move();
    let effects = world.effects(&plan);

    // Somebody creates the destination in a file manager.
    std::fs::create_dir_all(world.fx.abs("hiphop/MF DOOM/Mm..Food (2004)"))
        .expect("the fixture is writable");

    let err = commit::commit(&world.previewed(&plan, &effects), &world.config)
        .expect_err("re-validation finds the destination occupied");
    assert!(
        err.to_string().contains("already exists"),
        "the conflict a fresh scan found should be in the message:\n{err}"
    );
}

// ---------------------------------------------------------------------------
// Criterion: crash injection after each of steps 3–6 leaves a record from which
// the starting state can be fully restored. One test per injection point.

/// The transaction id out of an injected failure, which is how a test finds the
/// record a crashed commit left.
fn injected_txid(err: &mpdfm_core::Error) -> TxId {
    match err {
        mpdfm_core::Error::Commit(CommitError::Injected { txid, .. }) => txid.clone(),
        other => panic!("expected an injected crash, got {other}"),
    }
}

/// Crash at `inject`, then put the library back from the record alone.
fn crash_and_roll_back(inject: Inject) -> (World, Record) {
    let mut world = World::realistic();
    let before = world.state();

    let err = world
        .commit_with(
            &album_move(),
            &Options {
                inject,
                ..Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let record = world.record(&injected_txid(&err));

    roll_back(&world.fx, &record);
    before.assert_same(&world.state());

    world.rescan();
    (world, record)
}

#[test]
fn a_crash_right_after_the_pending_record_is_recoverable() {
    let (_world, record) = crash_and_roll_back(Inject::AfterPending);
    assert_eq!(record.status, Status::Pending);
    assert!(record.steps.iter().all(|step| !step.done));
}

#[test]
fn a_crash_in_the_middle_of_the_filesystem_steps_is_recoverable() {
    // Step 0 is the `MkDir`, so stopping after step 2 leaves a created directory
    // and two moved files to put back.
    let (_world, record) = crash_and_roll_back(Inject::AfterStep(2));
    assert_eq!(record.status, Status::Pending);
    assert_eq!(
        record.completed().count(),
        3,
        "three steps had run:\n{}",
        step_lines(&record).join("\n")
    );
    assert!(
        !record.all_done(),
        "and the rest had not:\n{}",
        step_lines(&record).join("\n")
    );
}

#[test]
fn a_crash_after_every_step_but_before_the_playlists_is_recoverable() {
    let (_world, record) = crash_and_roll_back(Inject::AfterSteps);
    assert_eq!(record.status, Status::Pending);
    assert!(record.all_done(), "the filesystem half finished");
}

#[test]
fn a_crash_between_two_playlist_writes_is_recoverable() {
    // One playlist rewritten, one not: the state the backups exist for.
    let (_world, record) = crash_and_roll_back(Inject::BeforePlaylistWrite(1));
    assert_eq!(record.status, Status::Pending);
    assert!(record.all_done());
    assert_eq!(
        record.playlist_edits.len(),
        2,
        "the record names both playlists, including the one it never reached"
    );
}

#[test]
fn a_crash_after_the_edits_but_before_the_record_is_completed_is_recoverable() {
    let (_world, record) = crash_and_roll_back(Inject::AfterEdits);
    assert_eq!(
        record.status,
        Status::Pending,
        "the work is done and the record does not know it yet — which is the \
         direction that is safe to be wrong in"
    );
    assert!(record.all_done());
}

#[test]
fn a_crash_after_the_record_is_completed_is_recoverable() {
    let (_world, record) = crash_and_roll_back(Inject::AfterComplete);
    assert_eq!(record.status, Status::Complete);
    assert!(record.all_done());
    assert!(
        record.is_undoable(),
        "this one is an ordinary `mpdfm undo`, not a recovery"
    );
}

// ---------------------------------------------------------------------------
// A step that really fails, rather than one that is interrupted.

#[test]
fn a_failed_step_leaves_a_failed_record_with_the_receipts_of_what_did_happen() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut world = World::realistic();
    let before = world.state();
    let plan = Plan::of(vec![Operation::MoveDir {
        from: rel(names::MF_DOOM_ALBUM),
        to: rel("staging/MF DOOM/Mm..Food (2004)"),
    }]);

    // `staging/` is writable by its mode bits and not writable in fact: a
    // directory with the write bit and no search bit cannot have a file created
    // in it. That is the one place `exec_fs::check` and `execute_with`
    // deliberately disagree (task 08), so it is the only way to reach a step that
    // fails for a reason the preview could not have seen — every other
    // precondition is shared code and would have been a conflict.
    let staging = world.fx.abs("staging");
    std::fs::create_dir(&staging).expect("the fixture is ours");
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o200))
        .expect("the fixture is ours");

    let effects = world.effects(&plan);
    assert!(
        effects.conflicts.is_empty(),
        "the preview sees nothing wrong, which is the premise of this test: {:?}",
        effects.conflicts
    );

    let err = commit::commit(&world.previewed(&plan, &effects), &world.config)
        .expect_err("the write probe refuses the first step");

    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
        .expect("the fixture is ours");

    let (txid, position) = match &err {
        mpdfm_core::Error::Commit(CommitError::Step { txid, position, .. }) => {
            (txid.clone(), *position)
        }
        other => panic!("expected a failed step, got {other}"),
    };
    assert!(
        err.to_string().contains(&format!("mpdfm recover {txid}")),
        "the message has to say what to do next:\n{err}"
    );

    let record = world.record(&txid);
    assert_eq!(record.status, Status::Failed);
    assert!(record.finished_at.is_some());
    let failed = &record.steps[position - 1];
    assert!(
        failed.error.is_some() && !failed.done,
        "the step that failed says so:\n{}",
        step_lines(&record).join("\n")
    );

    roll_back(&world.fx, &record);
    std::fs::remove_dir(&staging).expect("nothing was put in it");
    before.assert_same(&world.state());
    world.rescan();
}

#[test]
fn a_playlist_that_cannot_be_written_leaves_a_failed_record_and_rolls_back() {
    use std::os::unix::fs::PermissionsExt as _;

    let world = World::realistic();
    let before = world.state();

    // Read and search but not write: every playlist can be read, checked and
    // backed up, and none of them can be replaced.
    let dir = world.fx.playlist_dir().to_owned();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500))
        .expect("the fixture is ours");
    let err = world
        .commit(&album_move())
        .expect_err("the playlists cannot be rewritten");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
        .expect("the fixture is ours");

    let txid = match &err {
        mpdfm_core::Error::Commit(CommitError::Edits { txid, .. }) => txid.clone(),
        other => panic!("expected a playlist failure, got {other}"),
    };
    let record = world.record(&txid);
    assert_eq!(record.status, Status::Failed);
    assert!(
        record.all_done(),
        "the filesystem half had finished:\n{}",
        step_lines(&record).join("\n")
    );

    roll_back(&world.fx, &record);
    before.assert_same(&world.state());
}

// ---------------------------------------------------------------------------
// Deletes, which are the only steps that put bytes in the backup directory.

#[test]
fn a_delete_backs_the_file_up_inside_the_transaction_and_can_be_put_back() {
    let world = World::realistic();
    let before = world.state();
    let plan = Plan::of(vec![Operation::Delete {
        target: rel(names::SWITCHANGEL_M4A),
    }]);

    let committed = world.commit(&plan).expect("the delete commits");
    assert!(
        !world.fx.abs(names::SWITCHANGEL_M4A).exists(),
        "the file is gone from the library"
    );

    let record = world.record(&committed.txid);
    let backup = record.backup_dir.join("files").join(names::SWITCHANGEL_M4A);
    assert!(
        backup.is_file(),
        "its bytes are under the transaction's own directory, at {backup}"
    );
    assert!(
        record.steps.iter().any(|step| matches!(
            &step.step,
            FsStep::RemoveFile { backup: Some(path), .. } if path == &backup
        )),
        "the record names the backup it took:\n{}",
        step_lines(&record).join("\n")
    );
    assert!(
        !record
            .steps
            .iter()
            .any(|step| step.step.to_string().contains("/backups/pending/")),
        "no step may still point at the planner's placeholder transaction:\n{}",
        step_lines(&record).join("\n")
    );

    roll_back(&world.fx, &record);
    before.assert_same(&world.state());
}

// ---------------------------------------------------------------------------
// Criterion: MPD being unreachable does not fail the commit, and is recorded.

/// A config that wants MPD told, which the fixture's own does not.
fn wants_mpd_update(world: &mut World) {
    world.config.mpd_enabled = true;
    world.config.trigger_update_after_commit = true;
}

#[test]
fn mpd_being_unreachable_does_not_fail_the_commit_and_is_recorded() {
    let mut world = World::realistic();
    wants_mpd_update(&mut world);

    let refused = |_: &[DirPath]| Err("connection refused (is mpd running?)".to_owned());
    let committed = world
        .commit_with(
            &album_move(),
            &Options {
                update: Some(&refused),
                ..Options::default()
            },
        )
        .expect("an unreachable MPD is a warning, not a failed transaction");

    assert_eq!(committed.record.status, Status::Complete);
    let record = world.record(&committed.txid);
    assert!(record.mpd_update_requested, "the ask is recorded");
    assert_eq!(
        record.mpd_update_failed.as_deref(),
        Some("connection refused (is mpd running?)"),
        "and so is the reason it did not get through"
    );
    assert!(
        committed
            .warnings
            .iter()
            .any(|warning| matches!(warning, CommitWarning::Mpd(_))),
        "the user is told: {:?}",
        committed.warnings
    );
}

#[test]
fn a_reachable_mpd_is_asked_to_rescan_the_directories_that_changed() {
    let mut world = World::realistic();
    wants_mpd_update(&mut world);

    let asked: std::sync::Mutex<Vec<DirPath>> = std::sync::Mutex::new(Vec::new());
    let update = |dirs: &[DirPath]| {
        asked
            .lock()
            .expect("no panic in the test")
            .extend(dirs.iter().cloned());
        Ok(())
    };
    let committed = world
        .commit_with(
            &album_move(),
            &Options {
                update: Some(&update),
                ..Options::default()
            },
        )
        .expect("the plan commits");

    let asked = asked.into_inner().expect("no panic in the test");
    assert_eq!(
        asked,
        vec![DirPath::parse("hiphop").expect("a valid directory")],
        "one recursive update covers both ends of the move"
    );
    assert_eq!(world.record(&committed.txid).mpd_update_dirs, asked);
    assert!(
        committed.warnings.is_empty(),
        "nothing went wrong: {:?}",
        committed.warnings
    );
}

#[test]
fn mpd_is_not_asked_when_the_configuration_says_not_to() {
    let world = World::realistic();
    let update = |_: &[DirPath]| panic!("MPD must not be asked when it is disabled");
    let committed = world
        .commit_with(
            &album_move(),
            &Options {
                update: Some(&update),
                ..Options::default()
            },
        )
        .expect("the plan commits");

    assert!(!committed.record.mpd_update_requested);
}

// ---------------------------------------------------------------------------
// Criterion: retention pruning removes old backup dirs but not journal records.

#[test]
fn retention_removes_old_backup_directories_and_keeps_their_records() {
    let fx = Fixture::builder().album("a", &["01.mp3"]).build();
    let store = Store::at(fx.data_dir());
    store.create_dirs().expect("the data directory is ours");

    let ids: Vec<TxId> = [
        "20260101T000000Z-0001",
        "20260102T000000Z-0002",
        "20260103T000000Z-0003",
    ]
    .iter()
    .map(|id| TxId::parse(id).expect("a valid id"))
    .collect();
    for txid in &ids {
        let backup_dir = store.create_backup_dir(txid).expect("writable");
        std::fs::write(backup_dir.join("Pop.m3u"), b"a/01.mp3\n").expect("writable");
        let mut record = Record::opening(
            txid.clone(),
            std::time::SystemTime::now(),
            fx.music_dir().to_owned(),
            fx.playlist_dir().to_owned(),
            backup_dir,
        );
        record.finish(Status::Complete, std::time::SystemTime::now());
        store.write(&record).expect("writable");
    }

    let pruned = store.prune(1).expect("the journal lists");

    assert_eq!(
        pruned.pruned,
        vec![ids[1].clone(), ids[0].clone()],
        "the two oldest transactions' backups go, newest first in the report"
    );
    assert!(
        store.backup_dir(&ids[2]).is_dir(),
        "the newest transaction keeps its backups"
    );
    for txid in &ids[..2] {
        assert!(
            !store.backup_dir(txid).exists(),
            "{txid}'s backup directory should be gone"
        );
        let record = store.load(txid).expect("the record is still there");
        assert!(
            record.backup_pruned,
            "and it should say why it cannot be undone"
        );
        assert!(!record.is_undoable());
    }
    assert_eq!(
        store.list().expect("the journal lists").len(),
        3,
        "no record is ever removed by retention"
    );
}

#[test]
fn retention_never_prunes_a_transaction_that_still_needs_recovering() {
    let fx = Fixture::builder().album("a", &["01.mp3"]).build();
    let store = Store::at(fx.data_dir());
    store.create_dirs().expect("the data directory is ours");

    let txid = TxId::parse("20260101T000000Z-0001").expect("a valid id");
    let backup_dir = store.create_backup_dir(&txid).expect("writable");
    let record = Record::opening(
        txid.clone(),
        std::time::SystemTime::now(),
        fx.music_dir().to_owned(),
        fx.playlist_dir().to_owned(),
        backup_dir,
    );
    store.write(&record).expect("writable");

    let pruned = store.prune(0).expect("the journal lists");

    assert!(pruned.pruned.is_empty(), "nothing was pruned");
    assert_eq!(pruned.kept.len(), 1);
    assert!(
        pruned.kept[0].why.contains("pending"),
        "the reason has to be the useful one: {}",
        pruned.kept[0]
    );
    assert!(
        store.backup_dir(&txid).is_dir(),
        "a pending transaction's backups are what `recover` restores from"
    );
}

#[test]
fn a_commit_prunes_the_transactions_beyond_the_retention_limit() {
    let mut world = World::realistic();
    world.config.backup_keep = 1;

    let first = world.commit(&album_move()).expect("the first plan commits");
    world.rescan();
    let second = world
        .commit(&other_album_move())
        .expect("the second commits");

    assert_eq!(
        second.pruned.pruned,
        vec![first.txid.clone()],
        "the previous transaction is one too many: {:?}",
        second.pruned
    );
    assert!(!world.store().backup_dir(&first.txid).exists());
    assert!(world.store().backup_dir(&second.txid).is_dir());
    assert!(
        world.record(&first.txid).backup_pruned,
        "its record stays, marked"
    );
}

// ---------------------------------------------------------------------------
// Criterion: records are forward-compatible, and a version mismatch is refused.

/// A committed transaction's record file, for the tests that edit the JSON.
fn a_record_on_disk() -> (Fixture, Store, TxId, Utf8PathBuf) {
    let world = World::realistic();
    let committed = world.commit(&album_move()).expect("the plan commits");
    let store = world.store();
    let path = store.record_path(&committed.txid);
    (world.fx, store, committed.txid, path)
}

fn read_json(path: &Utf8Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).expect("the record is readable"))
        .expect("the record is JSON")
}

#[test]
fn a_field_this_version_does_not_know_survives_being_read_and_written_back() {
    let (_fx, store, txid, path) = a_record_on_disk();

    let mut json = read_json(&path);
    json.as_object_mut()
        .expect("a record is an object")
        .insert("mpd_partition".to_owned(), serde_json::json!("living room"));
    std::fs::write(&path, serde_json::to_vec_pretty(&json).expect("encodes")).expect("writable");

    let record = store.load(&txid).expect("an unknown field is not an error");
    assert_eq!(
        record.unknown.get("mpd_partition"),
        Some(&serde_json::json!("living room")),
        "the field is kept rather than dropped on the floor"
    );

    // And writing the record back — which `undo` does when it marks it reverted —
    // must not strip it.
    store.write(&record).expect("writable");
    assert_eq!(
        read_json(&path).get("mpd_partition"),
        Some(&serde_json::json!("living room")),
        "an older MPDFM must not quietly delete a newer one's fields"
    );
}

#[test]
fn a_record_from_another_format_version_is_refused_with_both_versions_named() {
    let (_fx, store, txid, path) = a_record_on_disk();

    let mut json = read_json(&path);
    json.as_object_mut()
        .expect("a record is an object")
        .insert("version".to_owned(), serde_json::json!(2));
    std::fs::write(&path, serde_json::to_vec_pretty(&json).expect("encodes")).expect("writable");

    let err = store.load(&txid).expect_err("version 2 is not version 1");
    let message = err.to_string();
    assert!(
        message.contains("version 2") && message.contains("version 1"),
        "the message has to name both versions:\n{message}"
    );
    assert!(
        message.contains(path.as_str()),
        "and the record it is about:\n{message}"
    );
}

#[test]
fn a_record_with_no_version_at_all_is_refused_rather_than_assumed() {
    let (_fx, store, txid, path) = a_record_on_disk();

    let mut json = read_json(&path);
    json.as_object_mut().expect("an object").remove("version");
    std::fs::write(&path, serde_json::to_vec_pretty(&json).expect("encodes")).expect("writable");

    assert!(
        store
            .load(&txid)
            .unwrap_err()
            .to_string()
            .contains("version none"),
        "a record with no version is not a version 1 record"
    );
}

#[test]
fn a_record_round_trips_through_json_with_its_receipts_intact() {
    let (_fx, store, txid, path) = a_record_on_disk();
    let record = store.load(&txid).expect("readable");

    // Written by the commit, read back, written again: the file is the same.
    let first = std::fs::read_to_string(&path).expect("readable");
    store.write(&record).expect("writable");
    assert_eq!(
        first,
        std::fs::read_to_string(&path).expect("readable"),
        "a record that is read and written back is byte-identical"
    );

    let receipt = record.steps[1].receipt().expect("step 2 ran");
    assert_eq!(receipt.step, record.steps[1].step);
    assert!(
        matches!(receipt.done, exec_fs::Done::Moved { .. }),
        "the second step of an album move is a file move: {:?}",
        receipt.done
    );
}

// ---------------------------------------------------------------------------
// The things commit refuses outright, before it does anything at all.

#[test]
fn a_plan_with_a_conflict_is_refused_and_writes_no_record() {
    let world = World::realistic();
    // The destination is occupied by the album itself.
    let plan = Plan::of(vec![Operation::MoveDir {
        from: rel(names::MF_DOOM_ALBUM),
        to: rel(names::SNOOP_ALBUM),
    }]);
    let effects = world.effects(&plan);
    assert!(!effects.conflicts.is_empty(), "the preview refuses it");
    let before = world.state();

    let err = commit::commit(&world.previewed(&plan, &effects), &world.config)
        .expect_err("a refused plan is not committed");
    assert!(
        matches!(err, mpdfm_core::Error::Commit(CommitError::Refused { .. })),
        "expected a refusal, got {err}"
    );
    before.assert_same(&world.state());
    assert_eq!(world.store().list().expect("lists"), Vec::new());
}

#[test]
fn an_empty_plan_is_refused_rather_than_journaled() {
    let world = World::realistic();
    let plan = Plan::new();
    let effects = world.effects(&plan);

    let err = commit::commit(&world.previewed(&plan, &effects), &world.config)
        .expect_err("there is nothing to do");
    assert!(
        matches!(err, mpdfm_core::Error::Commit(CommitError::Nothing)),
        "expected `nothing to commit`, got {err}"
    );
    assert_eq!(world.store().list().expect("lists"), Vec::new());
}

/// MPD's saved queue is backed up and journaled exactly like a playlist, which
/// is what makes step 2's ordering hold for it too: the copy exists before the
/// record that promises it, and the record is what `undo` believes. Task 14's own
/// tests are in `tests/mpd_state.rs`; this one is here because the *sequence* is
/// this file's subject.
#[test]
fn a_commit_backs_up_mpds_saved_queue_before_it_writes_the_record() {
    let world = World::realistic();
    let plan = album_move();
    let before = std::fs::read(world.fx.state_file()).expect("the fixture has one");

    let committed = world.commit(&plan).expect("the album moves");

    let record = &committed.record;
    assert!(
        !record.state_edits.is_empty(),
        "the queued MF DOOM track moved, so the saved queue had to change"
    );
    let name = record
        .state_backup
        .as_ref()
        .expect("the state file was copied into the backup directory");
    assert_eq!(
        std::fs::read(record.backup_dir.join(name)).expect("the copy is readable"),
        before,
        "the backup holds the file as it was before the commit"
    );
    assert_ne!(
        std::fs::read(world.fx.state_file()).expect("still there"),
        before,
        "and the file itself was rewritten"
    );
}

// ---------------------------------------------------------------------------
// Listing, which is what `undo --list` and `recover` are built on.

#[test]
fn the_journal_lists_newest_first_and_knows_what_is_unfinished() {
    let mut world = World::realistic();

    let crashed = world
        .commit_with(
            &album_move(),
            &Options {
                inject: Inject::AfterPending,
                ..Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let crashed = injected_txid(&crashed);
    world.rescan();
    let completed = world.commit(&album_move()).expect("the retry commits");

    // Both ids carry the same timestamp — two commits inside one second — so
    // their relative order is the suffix's business, not something to assert.
    // `prune` is where the ordering itself is tested, against ids a day apart.
    let store = world.store();
    let listed = store.list().expect("lists");
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&crashed) && listed.contains(&completed.txid));

    let (records, problems) = store.records().expect("lists");
    assert!(
        problems.is_empty(),
        "both records are readable: {problems:?}"
    );
    assert_eq!(records.len(), 2);
    let headlines: Vec<String> = records.iter().map(Record::headline).collect();
    assert!(
        headlines.iter().any(|line| line.contains("complete"))
            && headlines.iter().any(|line| line.contains("pending")),
        "a headline says what happened:\n{}",
        headlines.join("\n")
    );

    let unfinished = store.unfinished().expect("lists");
    assert_eq!(unfinished.len(), 1);
    assert_eq!(unfinished[0].txid, crashed);
}

#[test]
fn an_unreadable_record_does_not_hide_the_others() {
    let (_fx, store, txid, path) = a_record_on_disk();
    std::fs::write(&path, b"{ not json").expect("writable");

    let (records, problems) = store.records().expect("the directory still lists");
    assert!(records.is_empty());
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].to_string().contains(txid.as_str()),
        "the problem names the record:\n{}",
        problems[0]
    );
}

#[test]
fn asking_for_a_transaction_that_is_not_there_says_so() {
    let fx = Fixture::builder().album("a", &["01.mp3"]).build();
    let store = Store::at(fx.data_dir());
    let txid = TxId::parse("20260101T000000Z-ffff").expect("a valid id");

    let err = store.load(&txid).expect_err("there is no such transaction");
    assert!(err.to_string().contains("there is no transaction"), "{err}");
}
