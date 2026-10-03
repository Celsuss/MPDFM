//! `mpdfm undo` and `mpdfm recover`, one test per acceptance criterion (task 12).
//!
//! Task 11 proved that the record a crash leaves behind is *sufficient* — its
//! `roll_back` helper was this task's algorithm in fifteen lines. This tests the
//! real thing: the preconditions it checks before it touches anything, the
//! report it gives when they do not hold, the record it writes about itself, and
//! the symmetry that makes undoing an undo re-apply the change.
//!
//! The shape of nearly every test here is the one `docs/PLAN.md` §8 asks for:
//! snapshot the music and playlist directories, commit, undo, and demand that
//! the two snapshots are byte-identical. [`State::assert_same`] prints only what
//! differs.
//!
//! No test touches the real library: everything happens inside a [`Fixture`]'s
//! temp directory, and the journal, the backups and MPD's state file are the
//! fixture's own.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use camino::Utf8PathBuf;
use mpdfm_core::config::Config;
use mpdfm_core::journal::record::{Direction, Record, Status, TxId};
use mpdfm_core::journal::store::{Store, syncs};
use mpdfm_core::journal::{recover, undo};
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::ops::commit::{self, CommitError, Inject, Previewed};
use mpdfm_core::ops::{Committed, Effects, Operation, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::testing::{Fixture, Snapshot, names};

/// A fixture with the three things a preview needs scanned from it — the same
/// harness task 11's tests use, with the undo calls added.
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

    fn commit(&self, plan: &Plan) -> Committed {
        self.commit_with(plan, &commit::Options::default())
            .expect("the test's own plan commits")
    }

    fn commit_with(
        &self,
        plan: &Plan,
        options: &commit::Options<'_>,
    ) -> mpdfm_core::Result<Committed> {
        let effects = self.effects(plan);
        assert!(
            effects.conflicts.is_empty(),
            "the test's own plan does not validate: {:?}",
            effects.conflicts
        );
        let previewed = Previewed {
            plan,
            library: &self.library,
            effects: &effects,
        };
        commit::commit_with(&previewed, &self.config, options)
    }

    fn store(&self) -> Store {
        Store::at(self.fx.data_dir())
    }

    /// The record on disk, which is all an undo has to work from.
    fn record(&self, txid: &TxId) -> Record {
        self.store()
            .load(txid)
            .unwrap_or_else(|err| panic!("the record for {txid} should be readable: {err}"))
    }

    /// Undo a transaction with the production defaults.
    fn undo(&mut self, txid: &TxId) -> undo::Reversed {
        self.undo_with(txid, &undo::Options::default())
            .unwrap_or_else(|err| panic!("{txid} should undo: {err}"))
    }

    fn undo_with(
        &mut self,
        txid: &TxId,
        options: &undo::Options<'_>,
    ) -> mpdfm_core::Result<undo::Reversed> {
        let record = self.record(txid);
        let reversed = undo::undo(&self.store(), &record, &self.config, options);
        self.rescan();
        reversed
    }

    /// Everything a transaction may change. Not the data directory — that is
    /// where the journal lives, and it is *meant* to grow.
    fn state(&self) -> State {
        State {
            music: Snapshot::capture(self.fx.music_dir()),
            playlists: Snapshot::capture(self.fx.playlist_dir()),
        }
    }
}

/// The two trees an undo is allowed to touch, captured.
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

fn rel(path: &str) -> RelPath {
    RelPath::parse(path).unwrap_or_else(|err| panic!("{path:?} is not a RelPath: {err}"))
}

/// The album move most tests here use: eight files, two playlists, one directory
/// created and one left empty behind.
fn album_move() -> Plan {
    Plan::of(vec![Operation::MoveDir {
        from: rel(names::MF_DOOM_ALBUM),
        to: rel("hiphop/MF DOOM/Mm..Food (2004)"),
    }])
}

/// Where the album move puts one of its tracks.
const MOVED_TRACK: &str = "hiphop/MF DOOM/Mm..Food (2004)/02 Hoe Cakes.mp3";

fn step_lines(record: &Record) -> String {
    record
        .steps
        .iter()
        .enumerate()
        .map(|(at, step)| {
            format!(
                "  {at}: {} done={} reconstructed={} error={:?}",
                step.step, step.done, step.reconstructed, step.error
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Make a file different without changing anything the plan looked at.
fn append_to(path: &Utf8PathBuf) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|err| panic!("{path} should be writable: {err}"));
    file.write_all(b"edited by something else")
        .expect("the fixture is ours");
}

// ---------------------------------------------------------------------------
// Criterion: commit → undo ⇒ `Fixture::snapshot()` equals the pre-commit
// snapshot exactly, for a single file move, an album directory move, a delete, a
// multi-playlist rewrite, and a mixed plan. One test each.

#[test]
fn undoing_a_single_file_move_restores_the_tree_exactly() {
    let mut world = World::realistic();
    let before = world.state();
    let plan = Plan::of(vec![Operation::MoveFile {
        from: rel(names::MF_DOOM_TRACK),
        to: rel("hiphop/singles/Beef Rap.mp3"),
    }]);

    let committed = world.commit(&plan);
    assert!(world.fx.abs("hiphop/singles/Beef Rap.mp3").is_file());

    let reversed = world.undo(&committed.txid);

    before.assert_same(&world.state());
    assert_eq!(reversed.of, committed.txid);
    assert!(
        !world.fx.abs("hiphop/singles").exists(),
        "the directory the move created goes with it"
    );
}

#[test]
fn undoing_an_album_directory_move_restores_the_tree_exactly() {
    let mut world = World::realistic();
    let before = world.state();

    let committed = world.commit(&album_move());
    let after = world.state();
    assert!(
        !after.music.entries().eq(before.music.entries()),
        "the commit should have changed something"
    );

    world.undo(&committed.txid);

    before.assert_same(&world.state());
}

#[test]
fn undoing_a_delete_restores_the_file_and_the_playlist_line() {
    let mut world = World::realistic();
    let before = world.state();
    let plan = Plan::of(vec![Operation::Delete {
        target: rel(names::SNOOP_TRACK),
    }]);

    let committed = world.commit(&plan);
    assert!(!world.fx.abs(names::SNOOP_TRACK).exists());
    let playlist =
        std::fs::read_to_string(world.fx.playlist_path(names::HIP_HOP_PLAYLIST)).expect("readable");
    assert!(
        !playlist.contains(names::SNOOP_TRACK),
        "the delete removes the line, it does not rewrite it:\n{playlist}"
    );

    world.undo(&committed.txid);

    before.assert_same(&world.state());
}

#[test]
fn undoing_a_rewrite_of_several_playlists_restores_every_one_of_them() {
    // One track, four playlists that name it, and a fifth that does not.
    let track = "jazz/trio/01 take five.mp3";
    let mut world = World::new(
        Fixture::builder()
            .album("jazz/trio", &["01 take five.mp3", "02 blue rondo.mp3"])
            .playlist("Jazz.m3u", &[track, "jazz/trio/02 blue rondo.mp3"])
            .playlist("Favourites.m3u", &["#EXTM3U", "#EXTINF:1,Take Five", track])
            .playlist("Dinner.m3u", &[track])
            .playlist("Coding.m3u", &["", track, "# the end"])
            .playlist("Untouched.m3u", &["jazz/trio/02 blue rondo.mp3"])
            .build(),
    );
    let before = world.state();

    let plan = Plan::of(vec![Operation::MoveFile {
        from: rel(track),
        to: rel("jazz/Dave Brubeck Quartet/Take Five.mp3"),
    }]);
    let committed = world.commit(&plan);
    assert_eq!(
        committed.record.summary.playlists_affected, 4,
        "four playlists name it and one does not"
    );

    world.undo(&committed.txid);

    before.assert_same(&world.state());
}

#[test]
fn undoing_a_mixed_plan_restores_the_tree_exactly() {
    let mut world = World::realistic();
    let before = world.state();
    let plan = Plan::of(vec![
        Operation::MoveDir {
            from: rel(names::MF_DOOM_ALBUM),
            to: rel("hiphop/MF DOOM/Mm..Food (2004)"),
        },
        Operation::MoveFile {
            from: rel(names::KREAM_TRACK),
            to: rel("electronic/KREAM/So Hï.mp3"),
        },
        Operation::Delete {
            target: rel(names::SWITCHANGEL_M4A),
        },
    ]);

    let committed = world.commit(&plan);
    assert!(committed.record.summary.files_moved > 1);
    assert_eq!(committed.record.summary.files_deleted, 1);

    world.undo(&committed.txid);

    before.assert_same(&world.state());
}

// ---------------------------------------------------------------------------
// Criterion: undo after the user modified a moved file stops with a clear report
// and changes nothing; `--force` skips just that file and reports it.

#[test]
fn undo_after_the_user_modified_a_moved_file_stops_and_changes_nothing() {
    let mut world = World::realistic();
    let committed = world.commit(&album_move());

    // Somebody re-tags one of the tracks after the move.
    append_to(&world.fx.abs(MOVED_TRACK));
    let untouched = world.state();

    let record = world.record(&committed.txid);
    let check = undo::check(&record, &world.config).expect("the transaction itself is undoable");
    assert!(!check.is_clear(), "the check has to notice: {check}");
    assert_eq!(check.problems.len(), 1, "and only the one file: {check}");
    assert_eq!(check.problems[0].what, undo::Trouble::Modified);
    assert!(
        check.problems[0].at.as_str().ends_with("02 Hoe Cakes.mp3"),
        "the report names the file that changed: {check}"
    );
    assert!(
        check.render().contains("--force"),
        "and says what to do about it:\n{}",
        check.render()
    );

    let err = world
        .undo_with(&committed.txid, &undo::Options::default())
        .expect_err("a modified file stops the undo");
    let message = err.to_string();
    assert!(
        message.contains("02 Hoe Cakes.mp3") && message.contains("--force"),
        "the refusal has to name the file and the way past it:\n{message}"
    );

    untouched.assert_same(&world.state());
    assert_eq!(
        world.record(&committed.txid).status,
        Status::Complete,
        "a refused undo leaves the transaction exactly as it was"
    );
}

#[test]
fn force_skips_just_the_file_that_changed_and_reports_it() {
    let mut world = World::realistic();
    let before = world.state();
    let committed = world.commit(&album_move());
    append_to(&world.fx.abs(MOVED_TRACK));

    let reversed = world
        .undo_with(
            &committed.txid,
            &undo::Options {
                force: true,
                ..undo::Options::default()
            },
        )
        .expect("--force undoes everything else");

    assert_eq!(reversed.skipped.len(), 1, "{:?}", reversed.skipped);
    assert_eq!(reversed.skipped[0].what, undo::Trouble::Modified);
    assert!(
        reversed
            .warnings
            .iter()
            .any(|warning| matches!(warning, undo::UndoWarning::Skipped { .. })),
        "the user is told which steps were left alone: {:?}",
        reversed.warnings
    );

    // Every other file is back where it started, and the one that was edited is
    // where the commit left it.
    assert!(
        world.fx.abs(names::MF_DOOM_TRACK).is_file()
            && world
                .fx
                .abs(&format!("{}/folder.jpg", names::MF_DOOM_ALBUM))
                .is_file(),
        "the files that had not been touched went back"
    );
    assert!(
        world.fx.abs(MOVED_TRACK).is_file(),
        "and the edited one was left exactly where it was"
    );

    // Which is why the directory the move created is still there, with a warning
    // saying so rather than the file being deleted to tidy up.
    assert!(
        reversed
            .warnings
            .iter()
            .any(|warning| matches!(warning, undo::UndoWarning::DirKept { .. })),
        "a directory that still holds something is reported, not emptied: {:?}",
        reversed.warnings
    );
    assert!(world.fx.abs("hiphop/MF DOOM/Mm..Food (2004)").is_dir());

    // The record says which step it skipped, so the next person can see why.
    let mine = world.record(&reversed.txid);
    assert_eq!(
        mine.steps.iter().filter(|step| !step.done).count(),
        1,
        "exactly one step of the reversal did not happen:\n{}",
        step_lines(&mine)
    );
    assert!(
        mine.steps.iter().any(|step| step
            .error
            .as_deref()
            .is_some_and(|why| why.contains("skipped"))),
        "and it says why:\n{}",
        step_lines(&mine)
    );

    // Nothing was lost: the one file that differs is the one that was edited.
    let diff = before.music.diff(&world.state().music);
    assert!(
        diff.iter().all(|line| line.contains("MF DOOM")),
        "only the edited album is not back:\n{}",
        diff.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Criterion: undo of a delete restores the file from the backup dir with its
// original mode and mtime.

#[test]
fn undoing_a_delete_restores_the_mode_and_the_mtime_from_the_backup() {
    let mut world = World::realistic();
    let target = world.fx.abs(names::SWITCHANGEL_M4A);
    // A mode no fixture file has, so a restore that invents one is visible.
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640))
        .expect("the fixture is ours");
    let before = std::fs::metadata(&target).expect("it is there");

    let committed = world.commit(&Plan::of(vec![Operation::Delete {
        target: rel(names::SWITCHANGEL_M4A),
    }]));
    let record = world.record(&committed.txid);
    let backup = record.backup_dir.join("files").join(names::SWITCHANGEL_M4A);
    assert!(backup.is_file(), "its bytes are at {backup}");
    assert!(!target.exists());

    world.undo(&committed.txid);

    let after = std::fs::metadata(&target).expect("it is back");
    assert_eq!(
        after.permissions().mode() & 0o7777,
        0o640,
        "the mode comes back with the file"
    );
    assert_eq!(
        after.modified().expect("an mtime"),
        before.modified().expect("an mtime"),
        "and so does the mtime — MPD notices a changed one"
    );
    assert_eq!(after.len(), before.len());
    assert!(
        !backup.exists(),
        "the backup is the file, moved; undo moves it back rather than copying it"
    );
}

// ---------------------------------------------------------------------------
// Criterion: `undo --list` shows transactions newest-first with human summaries.

#[test]
fn the_list_shows_transactions_newest_first_with_summaries_and_what_is_undoable() {
    let mut world = World::realistic();
    let first = world.commit(&album_move());
    world.rescan();
    let second = world.commit(&Plan::of(vec![Operation::Delete {
        target: rel(names::SWITCHANGEL_M4A),
    }]));
    world.rescan();

    let listing = undo::list(&world.store()).expect("the journal lists");
    assert_eq!(listing.rows.len(), 2);
    assert!(listing.unreadable.is_empty());
    assert_eq!(
        listing.rows[0].txid, second.txid,
        "newest first: {}",
        listing
    );
    assert_eq!(listing.rows[1].txid, first.txid);
    assert!(listing.rows.iter().all(undo::Row::undoable));
    assert!(
        listing.rows[0].summary.contains("1 deleted")
            && listing.rows[1].summary.contains("file(s) moved"),
        "the summary is the one the preview led with:\n{listing}"
    );

    // Undo the delete, and the list says so from both ends.
    let reversed = world.undo(&second.txid);
    let listing = undo::list(&world.store()).expect("lists");
    assert_eq!(listing.rows.len(), 3, "the undo is a transaction too");
    assert_eq!(listing.rows[0].txid, reversed.txid);
    assert_eq!(listing.rows[0].direction, Direction::Reverse);
    assert!(
        listing.rows[0]
            .summary
            .contains(&format!("undo of {}", second.txid)),
        "an undo's row says what it undid:\n{listing}"
    );
    let undone = listing
        .rows
        .iter()
        .find(|row| row.txid == second.txid)
        .expect("still listed");
    assert!(!undone.undoable());
    assert!(
        undone
            .why_not
            .as_ref()
            .is_some_and(|why| why.contains("already undone")),
        "and the one that was undone says why it cannot be again: {:?}",
        undone.why_not
    );

    let rendered = listing.to_string();
    assert_eq!(rendered.lines().count(), 3);
    assert!(
        rendered.contains("undoable") && rendered.contains(first.txid.as_str()),
        "every transaction is in the table:\n{rendered}"
    );
}

#[test]
fn the_latest_undoable_transaction_is_what_undo_with_no_argument_takes() {
    let mut world = World::realistic();
    let first = world.commit(&album_move());
    world.rescan();

    assert_eq!(
        undo::latest(&world.store()).expect("there is one").txid,
        first.txid
    );

    // Once it is undone, the newest undoable transaction is the undo itself.
    let reversed = world.undo(&first.txid);
    assert_eq!(
        undo::latest(&world.store()).expect("there is one").txid,
        reversed.txid,
        "which is what makes `mpdfm undo` twice in a row a redo"
    );

    let empty = Fixture::builder().album("a", &["01.mp3"]).build();
    let err = undo::latest(&Store::at(empty.data_dir())).expect_err("nothing to undo");
    assert!(
        err.to_string().contains("no completed transaction"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// Criterion: undoing an undo re-applies the original change.

#[test]
fn undoing_an_undo_re_applies_the_original_change() {
    let mut world = World::realistic();
    let before = world.state();
    let committed = world.commit(&album_move());
    let after_commit = world.state();

    let undone = world.undo(&committed.txid);
    before.assert_same(&world.state());

    // Undo the undo: the record's direction is what says which way to go.
    let mine = world.record(&undone.txid);
    assert_eq!(mine.direction, Direction::Reverse);
    assert_eq!(mine.undo_of.as_ref(), Some(&committed.txid));
    assert_eq!(
        world.record(&committed.txid).undone_by.as_ref(),
        Some(&undone.txid),
        "and the transaction names the undo that reversed it"
    );

    let redone = world.undo(&undone.txid);
    assert_eq!(redone.action, undo::Action::Replay);
    after_commit.assert_same(&world.state());

    // And again, both ways, because the direction flips every time.
    world.undo(&redone.txid);
    before.assert_same(&world.state());
}

// ---------------------------------------------------------------------------
// Criterion: `recover` on each crash-injection fixture from task 11 restores the
// starting state. One test per injection point.

/// The transaction id out of an injected failure.
fn injected_txid(err: &mpdfm_core::Error) -> TxId {
    match err {
        mpdfm_core::Error::Commit(CommitError::Injected { txid, .. }) => txid.clone(),
        other => panic!("expected an injected crash, got {other}"),
    }
}

/// Crash the commit at `inject`, then `recover` it and demand the library is
/// back where it started.
fn recovers_from(inject: Inject) -> (World, Record) {
    let mut world = World::realistic();
    let before = world.state();

    let err = world
        .commit_with(
            &album_move(),
            &commit::Options {
                inject,
                ..commit::Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let txid = injected_txid(&err);
    let crashed = world.record(&txid);
    assert!(crashed.status.is_unfinished());

    let survey = recover::survey(&crashed, &world.config).expect("it can be surveyed");
    assert!(
        survey.is_clear(),
        "a crashed commit's state is readable from the disk:\n{survey}"
    );
    assert!(
        survey.render().contains("rolling back"),
        "the report says what each choice would do:\n{survey}"
    );

    recover::roll_back(
        &world.store(),
        &crashed,
        &world.config,
        &undo::Options::default(),
    )
    .unwrap_or_else(|err| panic!("{txid} should roll back: {err}"));
    world.rescan();

    before.assert_same(&world.state());
    let after = world.record(&txid);
    assert_eq!(
        after.status,
        Status::Reverted,
        "and the record says it was put back:\n{}",
        step_lines(&after)
    );
    assert!(
        after.undone_by.is_some(),
        "naming the rollback that did it, which is itself undoable"
    );
    (world, after)
}

#[test]
fn recover_rolls_back_a_crash_right_after_the_pending_record() {
    let (_world, record) = recovers_from(Inject::AfterPending);
    assert!(record.steps.iter().all(|step| !step.done));
}

#[test]
fn recover_rolls_back_a_crash_in_the_middle_of_the_filesystem_steps() {
    let (_world, record) = recovers_from(Inject::AfterStep(2));
    assert_eq!(record.steps.iter().filter(|step| step.done).count(), 3);
}

#[test]
fn recover_rolls_back_a_crash_after_every_step_but_before_the_playlists() {
    let (_world, record) = recovers_from(Inject::AfterSteps);
    assert!(record.all_done());
}

#[test]
fn recover_rolls_back_a_crash_between_two_playlist_writes() {
    let (_world, record) = recovers_from(Inject::BeforePlaylistWrite(1));
    assert_eq!(record.playlist_edits.len(), 2);
}

#[test]
fn recover_rolls_back_a_crash_after_the_edits_but_before_the_record_was_completed() {
    let (_world, record) = recovers_from(Inject::AfterEdits);
    assert!(record.all_done());
}

#[test]
fn a_crash_after_the_record_was_completed_is_undos_job_and_recover_says_so() {
    let mut world = World::realistic();
    let before = world.state();
    let err = world
        .commit_with(
            &album_move(),
            &commit::Options {
                inject: Inject::AfterComplete,
                ..commit::Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let txid = injected_txid(&err);
    let record = world.record(&txid);

    let refused = recover::survey(&record, &world.config)
        .expect_err("a complete transaction is not recover's");
    assert!(
        refused.to_string().contains(&format!("mpdfm undo {txid}")),
        "and it says whose it is:\n{refused}"
    );

    world.undo(&txid);
    before.assert_same(&world.state());
}

// ---------------------------------------------------------------------------
// The state task 11's torn-log test produced: a step that ran and was never
// journaled. Recover has to find it by looking.

#[test]
fn recover_finds_a_step_whose_journal_line_the_crash_cut_in_half() {
    let mut world = World::realistic();
    let before = world.state();
    let err = world
        .commit_with(
            &album_move(),
            &commit::Options {
                inject: Inject::AfterStep(3),
                ..commit::Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let txid = injected_txid(&err);

    // What losing power in the middle of the append leaves: the last line half
    // written, which `read_steps` drops.
    let path = world.store().steps_path(&txid);
    let log = std::fs::read_to_string(&path).expect("readable");
    let whole = log.rfind('\n').expect("more than one line");
    let torn = &log[..whole];
    let cut = torn.len() - (torn.len() - torn.rfind('\n').expect("two lines")) / 2;
    std::fs::write(&path, &log[..cut]).expect("writable");

    let crashed = world.record(&txid);
    assert_eq!(
        crashed.steps.iter().filter(|step| step.done).count(),
        3,
        "the record itself knows about three of the four steps that ran"
    );

    let survey = recover::survey(&crashed, &world.config).expect("surveyable");
    assert_eq!(
        survey.found.len(),
        1,
        "and looking at the disk finds the fourth:\n{survey}"
    );
    assert_eq!(survey.found[0].at, 3);
    assert_eq!(survey.reached(), 4);
    assert!(
        survey.render().contains("not journaled"),
        "the report says so, because a reconstructed receipt knows less than a \
         real one:\n{survey}"
    );

    recover::roll_back(
        &world.store(),
        &crashed,
        &world.config,
        &undo::Options::default(),
    )
    .expect("it rolls back");
    world.rescan();

    before.assert_same(&world.state());
    let after = world.record(&txid);
    assert!(
        after.steps[3].done && after.steps[3].reconstructed,
        "the record now says that step ran, and that MPDFM worked it out by \
         looking:\n{}",
        step_lines(&after)
    );
}

// ---------------------------------------------------------------------------
// Rolling forward instead: finishing what the crash interrupted.

#[test]
fn recover_can_finish_a_transaction_instead_of_rolling_it_back() {
    let mut world = World::realistic();
    let before = world.state();
    let err = world
        .commit_with(
            &album_move(),
            &commit::Options {
                inject: Inject::BeforePlaylistWrite(1),
                ..commit::Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let txid = injected_txid(&err);
    let crashed = world.record(&txid);

    // One playlist written, one not: the state the backups exist for.
    let survey = recover::survey(&crashed, &world.config).expect("surveyable");
    assert_eq!(survey.playlists.len(), 2);
    assert!(
        survey
            .playlists
            .iter()
            .any(|playlist| playlist.state == recover::PlaylistState::Rewritten)
            && survey
                .playlists
                .iter()
                .any(|playlist| playlist.state == recover::PlaylistState::AsItWas),
        "the survey says which side of the transaction each playlist is on:\n{survey}"
    );

    let finished = recover::roll_forward(
        &world.store(),
        &crashed,
        &world.config,
        &undo::Options::default(),
    )
    .expect("it finishes");
    world.rescan();

    assert_eq!(finished.txid, txid, "the transaction keeps its own id");
    assert_eq!(world.record(&txid).status, Status::Complete);
    for playlist in [names::HIP_HOP_PLAYLIST, names::MF_DOOM_PLAYLIST] {
        let body = std::fs::read_to_string(world.fx.playlist_path(playlist)).expect("readable");
        assert!(
            body.contains("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3"),
            "{playlist} should name the new path now:\n{body}"
        );
    }

    // And a transaction a recovery finished is an ordinary one: undoing it puts
    // the library back exactly.
    world.undo(&txid);
    before.assert_same(&world.state());
}

#[test]
fn an_undo_that_stopped_partway_can_be_finished_by_recover() {
    let mut world = World::realistic();
    let before = world.state();
    let committed = world.commit(&album_move());

    // `hiphop/` readable and not writable: every playlist can be restored and no
    // directory inside it can be recreated, so the reversal stops on its first
    // step — which is the `RmDirIfEmpty` the album move ended with.
    let hiphop = world.fx.abs("hiphop");
    std::fs::set_permissions(&hiphop, std::fs::Permissions::from_mode(0o500))
        .expect("the fixture is ours");
    let err = world
        .undo_with(&committed.txid, &undo::Options::default())
        .expect_err("the reversal cannot write in there");
    std::fs::set_permissions(&hiphop, std::fs::Permissions::from_mode(0o755))
        .expect("the fixture is ours");

    let stopped = match &err {
        mpdfm_core::Error::Undo(undo::UndoError::Step { txid, .. }) => txid.clone(),
        other => panic!("expected a step that could not be put back, got {other}"),
    };
    let interrupted = world.record(&stopped);
    assert_eq!(interrupted.status, Status::Failed);
    assert_eq!(interrupted.direction, Direction::Reverse);

    let survey = recover::survey(&interrupted, &world.config).expect("surveyable");
    assert_eq!(survey.direction, Direction::Reverse);
    assert!(
        survey.render().contains("undoes the rest"),
        "finishing an undo means undoing the rest of it:\n{survey}"
    );

    recover::roll_forward(
        &world.store(),
        &interrupted,
        &world.config,
        &undo::Options::default(),
    )
    .expect("the rest of the reversal goes through");
    world.rescan();

    before.assert_same(&world.state());
    assert_eq!(
        world.record(&committed.txid).status,
        Status::Reverted,
        "the transaction is only reverted once its reversal has finished"
    );
    assert_eq!(world.record(&stopped).status, Status::Complete);
}

// ---------------------------------------------------------------------------
// Criterion: undo with a pruned backup dir refuses with a specific message.

#[test]
fn undo_with_a_pruned_backup_directory_refuses_with_a_specific_message() {
    let mut world = World::realistic();
    let committed = world.commit(&album_move());
    let after_commit = world.state();

    // What retention does once `backup_keep` is exceeded.
    world.store().prune(0).expect("the journal lists");
    let record = world.record(&committed.txid);
    assert!(record.backup_pruned && !record.backup_dir.exists());

    let err = undo::check(&record, &world.config).expect_err("there is nothing to restore from");
    let message = err.to_string();
    assert!(
        message.contains("pruned")
            && message.contains("backup_keep")
            && message.contains(record.backup_dir.as_str()),
        "the message has to say why, and name the directory that is not there:\n{message}"
    );
    assert!(
        world
            .undo_with(
                &committed.txid,
                &undo::Options {
                    force: true,
                    ..undo::Options::default()
                }
            )
            .is_err(),
        "and --force is not a way past it: the bytes are gone"
    );
    after_commit.assert_same(&world.state());
}

#[test]
fn undo_with_a_backup_directory_somebody_removed_says_that_instead() {
    let world = World::realistic();
    let committed = world.commit(&album_move());
    let record = world.record(&committed.txid);
    std::fs::remove_dir_all(&record.backup_dir).expect("the fixture is ours");

    let err = undo::check(&record, &world.config).expect_err("there is nothing to restore from");
    assert!(
        err.to_string().contains("has been removed"),
        "retention is not blamed for something it did not do:\n{err}"
    );
}

// ---------------------------------------------------------------------------
// Criterion: undo of an already-`reverted` record refuses.

#[test]
fn undo_of_an_already_reverted_record_refuses_and_points_at_the_undo() {
    let mut world = World::realistic();
    let committed = world.commit(&album_move());
    let undone = world.undo(&committed.txid);
    let before_second = world.state();

    let record = world.record(&committed.txid);
    assert_eq!(record.status, Status::Reverted);
    let err = world
        .undo_with(&committed.txid, &undo::Options::default())
        .expect_err("undoing a reversal twice would reverse something that is not there");
    let message = err.to_string();
    assert!(
        message.contains("already been undone") && message.contains(undone.txid.as_str()),
        "the refusal has to say which transaction to undo instead:\n{message}"
    );
    before_second.assert_same(&world.state());
}

#[test]
fn undo_of_a_transaction_that_never_finished_points_at_recover() {
    let mut world = World::realistic();
    let err = world
        .commit_with(
            &album_move(),
            &commit::Options {
                inject: Inject::AfterStep(1),
                ..commit::Options::default()
            },
        )
        .expect_err("the injected crash stops the commit");
    let txid = injected_txid(&err);
    let before_undo = world.state();

    let err = world
        .undo_with(&txid, &undo::Options::default())
        .expect_err("a pending transaction is recover's");
    assert!(
        err.to_string().contains(&format!("mpdfm recover {txid}")),
        "because undo believes the record and this one is not finished:\n{err}"
    );
    before_undo.assert_same(&world.state());
}

// ---------------------------------------------------------------------------
// Pitfall: restoring a playlist from the backup can lose an unrelated edit made
// between the commit and the undo. It has to be detected, and nothing may be
// lost for good.

#[test]
fn a_playlist_edited_since_the_commit_stops_the_undo_and_force_keeps_a_copy() {
    let mut world = World::realistic();
    let committed = world.commit(&album_move());

    // MPD saves the playlist again with a track MPDFM knows nothing about.
    let playlist = world.fx.playlist_path(names::HIP_HOP_PLAYLIST);
    let mut body = std::fs::read_to_string(&playlist).expect("readable");
    body.push_str(&format!("{}\n", names::KREAM_TRACK));
    std::fs::write(&playlist, &body).expect("writable");
    let untouched = world.state();

    let record = world.record(&committed.txid);
    let check = undo::check(&record, &world.config).expect("undoable in principle");
    assert_eq!(check.problems.len(), 1, "{check}");
    assert_eq!(check.problems[0].what, undo::Trouble::PlaylistChanged);

    let err = world
        .undo_with(&committed.txid, &undo::Options::default())
        .expect_err("restoring the backup would throw that edit away");
    assert!(
        err.to_string().contains(names::HIP_HOP_PLAYLIST),
        "the refusal names the playlist:\n{err}"
    );
    untouched.assert_same(&world.state());

    let reversed = world
        .undo_with(
            &committed.txid,
            &undo::Options {
                force: true,
                ..undo::Options::default()
            },
        )
        .expect("--force restores it anyway");

    assert!(
        reversed
            .warnings
            .iter()
            .any(|warning| matches!(warning, undo::UndoWarning::PlaylistOverwritten { .. })),
        "and says that it did: {:?}",
        reversed.warnings
    );
    let kept = reversed.record.backup_dir.join(names::HIP_HOP_PLAYLIST);
    assert_eq!(
        std::fs::read_to_string(&kept).expect("the undo took its own copy first"),
        body,
        "nothing is lost for good: the bytes the undo overwrote are at {kept}"
    );
    let restored = std::fs::read_to_string(&playlist).expect("readable");
    assert!(
        !restored.contains(names::KREAM_TRACK) && restored.contains(names::MF_DOOM_TRACK),
        "the playlist is the one the commit backed up:\n{restored}"
    );
}

// ---------------------------------------------------------------------------
// An undo is a mutation like any other, so safety invariant 2 applies to it too.

#[test]
fn an_undo_writes_its_own_durable_record_and_can_be_found_in_the_journal() {
    let mut world = World::realistic();
    let committed = world.commit(&album_move());
    let reversed = world.undo(&committed.txid);
    let store = world.store();

    assert!(
        syncs::was_synced(&store.record_path(&reversed.txid)),
        "the undo's own record has to be durable before it touches anything"
    );
    assert!(
        syncs::was_synced(store.journal_dir()),
        "and so does its name"
    );
    assert!(
        syncs::was_synced(&store.backup_dir(&reversed.txid)),
        "and the directory its own backups went into"
    );

    let mine = world.record(&reversed.txid);
    assert_eq!(mine.status, Status::Complete);
    assert_eq!(mine.direction, Direction::Reverse);
    assert_eq!(mine.undo_of.as_ref(), Some(&committed.txid));
    assert!(
        mine.all_done(),
        "every step it reversed is in it:\n{}",
        step_lines(&mine)
    );
    assert!(
        mine.headline().contains("undo of"),
        "and its headline says what it is: {}",
        mine.headline()
    );
    assert_eq!(
        reversed.steps,
        mine.steps.len(),
        "which is as many steps as the transaction had"
    );
}

#[test]
fn an_undo_asks_mpd_to_rescan_the_directories_it_moved_things_between() {
    let mut world = World::realistic();
    world.config.mpd_enabled = true;
    world.config.trigger_update_after_commit = true;
    let committed = world.commit(&album_move());

    let asked: std::sync::Mutex<Vec<DirPath>> = std::sync::Mutex::new(Vec::new());
    let update = |dirs: &[DirPath]| {
        asked
            .lock()
            .expect("no panic in the test")
            .extend(dirs.iter().cloned());
        Ok(())
    };
    let reversed = world
        .undo_with(
            &committed.txid,
            &undo::Options {
                update: Some(&update),
                force: false,
            },
        )
        .expect("it undoes");

    assert_eq!(
        asked.into_inner().expect("no panic in the test"),
        vec![DirPath::parse("hiphop").expect("a valid directory")],
        "one recursive update covers both ends of the move"
    );
    assert!(world.record(&reversed.txid).mpd_update_requested);
}

#[test]
fn an_unreachable_mpd_does_not_fail_an_undo_that_has_already_happened() {
    let mut world = World::realistic();
    world.config.mpd_enabled = true;
    world.config.trigger_update_after_commit = true;
    let before = world.state();
    let committed = world.commit(&album_move());

    let refused = |_: &[DirPath]| Err("connection refused (is mpd running?)".to_owned());
    let reversed = world
        .undo_with(
            &committed.txid,
            &undo::Options {
                update: Some(&refused),
                force: false,
            },
        )
        .expect("the library is already consistent by the time MPD is told");

    before.assert_same(&world.state());
    assert!(
        reversed
            .warnings
            .iter()
            .any(|warning| matches!(warning, undo::UndoWarning::Mpd(_))),
        "the user is told: {:?}",
        reversed.warnings
    );
    assert_eq!(
        world.record(&reversed.txid).mpd_update_failed.as_deref(),
        Some("connection refused (is mpd running?)"),
        "and the record keeps the reason"
    );
}

#[test]
fn an_undo_against_a_different_library_root_uses_the_records_own_and_says_so() {
    let mut world = World::realistic();
    let committed = world.commit(&album_move());

    // The user has since pointed MPDFM at somewhere else. The record's root is
    // the one its paths mean, and undoing against any other would move files
    // nobody asked about.
    world.config.music_dir = Utf8PathBuf::from("/srv/some/other/library");
    let record = world.record(&committed.txid);
    let check = undo::check(&record, &world.config).expect("the record knows its own root");
    assert!(check.is_clear(), "{check}");
    assert!(
        check
            .warnings
            .iter()
            .any(|warning| matches!(warning, undo::UndoWarning::DifferentRoot { .. })),
        "but the user is told: {check}"
    );
}

#[test]
fn a_record_whose_library_root_has_gone_away_refuses() {
    let fx = Fixture::builder().album("a", &["01.mp3"]).build();
    let store = Store::at(fx.data_dir());
    store.create_dirs().expect("the data directory is ours");
    let txid = TxId::parse("20260101T000000Z-0001").expect("a valid id");
    let backup_dir = store.create_backup_dir(&txid).expect("writable");
    let mut record = Record::opening(
        txid.clone(),
        std::time::SystemTime::now(),
        Utf8PathBuf::from("/nonexistent/music-directory"),
        fx.playlist_dir().to_owned(),
        backup_dir,
    );
    record.finish(Status::Complete, std::time::SystemTime::now());
    store.write(&record).expect("writable");

    let err = undo::check(&record, &fx.config()).expect_err("there is nowhere to undo it");
    assert!(
        err.to_string().contains("/nonexistent/music-directory"),
        "{err}"
    );
}
