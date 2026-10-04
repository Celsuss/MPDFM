//! `mpdfm undo` and `mpdfm recover`, driven as the user drives them (task 15).
//!
//! The headline criterion is the round trip: `move` then `undo` returns a
//! fixture to a **byte-identical** state. That is asserted the way
//! `docs/PLAN.md` §8 asks — a [`Snapshot`][mpdfm_core::testing::Snapshot] of the
//! music and playlist directories before and after, which compares contents,
//! permission bits and symlink targets and prints only what differs.
//!
//! # How a half-finished transaction gets made
//!
//! `recover` needs one, and the binary has no way to crash on demand: crash
//! injection is core's [`Inject`], and exposing it as a flag would put a
//! "stop halfway" switch in a shipped tool. So these tests use core directly to
//! commit with the injection, which leaves a real `pending` record in the
//! fixture's data directory — and then run the **binary** against it. Core makes
//! the mess; the command under test cleans it up.

#![cfg(unix)]

mod harness;

use harness::World;
use mpdfm_core::journal::record::Status;
use mpdfm_core::journal::store::Store;
use mpdfm_core::library::Library;
use mpdfm_core::ops::commit::{self, Inject, Previewed};
use mpdfm_core::ops::{Operation, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::testing::names;

/// Where every test here moves the MF DOOM album to.
const DESTINATION: &str = "hiphop/MF DOOM/Mm..Food (2004)";

fn rel(path: &str) -> RelPath {
    RelPath::parse(path).unwrap_or_else(|err| panic!("{path:?} is not a RelPath: {err}"))
}

/// The album move, committed through **core** with `inject` firing partway.
///
/// Returns the transaction id the record was left under. The record is in the
/// fixture's own data directory, so the binary finds it through the same
/// `config.toml` every other test uses.
fn crash_a_commit(world: &World, inject: Inject) -> String {
    let config = {
        // The fixture's own `Config`, which names the same four roots the
        // `config.toml` does — so what core writes here is what the binary
        // reads back.
        let mut config = world.fx.config();
        config.trigger_update_after_commit = false;
        config
    };
    let library = Library::scan(world.fx.music_dir()).expect("the fixture scans");
    let index = PlaylistIndex::load(world.fx.playlist_dir()).0;
    let plan = Plan::of(vec![Operation::MoveDir {
        from: rel(names::MF_DOOM_ALBUM),
        to: rel(DESTINATION),
    }]);
    let effects = plan.validate(&library, &index, &config);
    assert!(effects.is_committable(), "{:?}", effects.conflicts);

    let err = commit::commit_with(
        &Previewed {
            plan: &plan,
            library: &library,
            effects: &effects,
        },
        &config,
        &commit::Options {
            inject,
            ..commit::Options::default()
        },
    )
    .expect_err("the injection fires");

    // The record the crash left, which is what `recover` reads.
    let store = Store::at(world.fx.data_dir());
    let unfinished = store.unfinished().expect("the journal is readable");
    assert_eq!(unfinished.len(), 1, "one crashed transaction: {err}");
    assert!(unfinished[0].status.is_unfinished(), "{:?}", unfinished[0]);
    unfinished[0].txid.as_str().to_owned()
}

// ---------------------------------------------------------------------------
// Criterion: `mpdfm move` then `mpdfm undo` returns a fixture to a
// byte-identical state.
// ---------------------------------------------------------------------------

#[test]
fn move_then_undo_is_byte_identical() {
    let world = World::realistic();
    world.assert_hermetic();
    let before = world.snapshot();
    let state_before = std::fs::read(world.fx.state_file()).expect("the state file is readable");

    world
        .run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"])
        .assert_code(0);
    world.snapshot().assert_differs(&before);

    let run = world.answer(&["undo"], "y");
    run.assert_code(0).assert_stdout("undid");

    world.snapshot().assert_same(&before);
    // MPD's saved queue is restored wholesale from the backup, not re-derived
    // (task 14), so it is byte-identical too.
    assert_eq!(
        std::fs::read(world.fx.state_file()).expect("readable"),
        state_before,
        "the state file was not restored byte-for-byte"
    );
}

/// With no `TXID`, `undo` takes the most recent undoable transaction — and
/// undoing an undo re-applies the change, which is the whole of MPDFM's redo.
#[test]
fn undo_with_no_argument_takes_the_latest_and_undoing_it_again_redoes_it() {
    let world = World::realistic();
    let before = world.snapshot();

    world
        .run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"])
        .assert_code(0);
    let moved = world.snapshot();

    world.answer(&["undo"], "y").assert_code(0);
    world.snapshot().assert_same(&before);

    // The undo is itself a transaction, so this one re-applies the move.
    world
        .answer(&["undo"], "y")
        .assert_code(0)
        .assert_stdout("re-applied");
    world.snapshot().assert_same(&moved);
}

#[test]
fn undo_takes_the_txid_the_commit_printed() {
    let world = World::realistic();
    let before = world.snapshot();

    let committed = world.json(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"]);
    let txid = committed["committed"]["txid"]
        .as_str()
        .expect("a committed run has a txid");

    world.answer(&["undo", txid], "y").assert_code(0);
    world.snapshot().assert_same(&before);
}

#[test]
fn undo_refuses_a_transaction_id_that_is_not_there() {
    let world = World::realistic();

    world
        .run(&["undo", "20260101T000000Z-000000", "--yes"])
        .assert_code(1)
        .assert_stderr("there is no transaction");

    // A string that cannot be a transaction id at all is refused before the
    // journal is looked at, because an id becomes a path under the data dir.
    world
        .run(&["undo", "../../etc/passwd", "--yes"])
        .assert_code(1)
        .assert_stderr("is not a transaction id");
}

#[test]
fn undo_with_an_empty_journal_says_so() {
    let world = World::realistic();

    world
        .run(&["undo", "--yes"])
        .assert_code(1)
        .assert_stderr("there is no completed transaction to undo");
}

// ---------------------------------------------------------------------------
// The prompt, which `undo` has for the same reason `move` does.
// ---------------------------------------------------------------------------

#[test]
fn declining_an_undo_exits_3_and_changes_nothing() {
    let world = World::realistic();
    world
        .run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"])
        .assert_code(0);
    let moved = world.snapshot();

    let run = world.answer(&["undo"], "n");
    run.assert_code(3).assert_stdout("Nothing was changed.");
    run.assert_stderr("[y/N]");

    world.snapshot().assert_same(&moved);
}

#[test]
fn a_non_interactive_undo_without_yes_refuses_rather_than_prompting() {
    let world = World::realistic();
    world
        .run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"])
        .assert_code(0);
    let moved = world.snapshot();

    let run = world.run(&["undo"]);
    run.assert_code(1).assert_stderr("stdin is not a terminal");
    // The report was still printed, so a log says what was declined.
    run.assert_stdout("UNDO");

    world.snapshot().assert_same(&moved);
}

// ---------------------------------------------------------------------------
// Undo verifies its preconditions rather than blindly reversing (safety
// invariant 9).
// ---------------------------------------------------------------------------

#[test]
fn undo_is_blocked_by_a_file_that_changed_since_and_exits_2() {
    let world = World::realistic();
    world
        .run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"])
        .assert_code(0);
    let moved = world.snapshot();

    // Somebody edited a moved track after the commit. Putting it back on top of
    // where it came from would move a file that is not the file the transaction
    // moved.
    let touched = world.abs(&format!("{DESTINATION}/02 Hoe Cakes.mp3"));
    std::fs::write(&touched, b"not the same bytes any more").expect("the fixture is writable");
    let after_edit = world.snapshot();

    let run = world.answer(&["undo"], "y");
    run.assert_code(2)
        .assert_stdout("Changed since the transaction")
        .assert_stdout("Nothing has been changed.")
        .assert_stdout("--force");

    // Refused means refused: not one file moved, and the question was never
    // even asked.
    world.snapshot().assert_same(&after_edit);

    // `--force` reverses everything that is still safe and reports what it
    // skipped.
    let forced = world.answer(&["undo", "--force"], "y");
    forced.assert_code(0).assert_stdout("undid");
    let after_force = world.snapshot();
    assert!(
        !after_force.music.diff(&moved.music).is_empty(),
        "--force should have put the rest back"
    );
    // The file that was edited stayed where it was, which is the point of
    // skipping it.
    assert!(touched.is_file(), "the edited file must not be moved");
}

// ---------------------------------------------------------------------------
// `undo --list`
// ---------------------------------------------------------------------------

#[test]
fn undo_list_shows_every_transaction_newest_first_and_whether_it_can_be_undone() {
    let world = World::realistic();

    world
        .run(&["undo", "--list"])
        .assert_code(0)
        .assert_stdout("the journal is empty");

    world
        .run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"])
        .assert_code(0);
    world
        .run(&[
            "move",
            names::KIND_OF_BLUE_TRACK,
            "jazz/Kind of Blue/01 So What.flac",
            "--yes",
        ])
        .assert_code(0);

    let listing = world.json(&["undo", "--list"]);
    let rows = listing["transactions"]
        .as_array()
        .expect("transactions is an array");
    assert_eq!(rows.len(), 2, "{listing:#}");
    // Newest first.
    assert!(
        rows[0]["started_at"].as_str() >= rows[1]["started_at"].as_str(),
        "{listing:#}"
    );
    for row in rows {
        assert_eq!(row["status"], "complete", "{row:#}");
        assert_eq!(row["direction"], "forward", "{row:#}");
        assert_eq!(row["undoable"], true, "{row:#}");
        assert!(row["why_not"].is_null(), "{row:#}");
    }

    // An already-reverted transaction says why it cannot be undone again.
    world.answer(&["undo"], "y").assert_code(0);
    let listing = world.json(&["undo", "--list"]);
    let reverted = listing["transactions"]
        .as_array()
        .expect("an array")
        .iter()
        .find(|row| row["status"] == "reverted")
        .unwrap_or_else(|| panic!("{listing:#}"));
    assert_eq!(reverted["undoable"], false, "{listing:#}");
    assert!(reverted["why_not"].is_string(), "{listing:#}");

    world
        .run(&["undo", "--list"])
        .assert_code(0)
        .assert_stdout("undoable");
}

// ---------------------------------------------------------------------------
// `mpdfm recover`
// ---------------------------------------------------------------------------

#[test]
fn recover_with_nothing_to_do_says_so() {
    let world = World::realistic();

    world
        .run(&["recover"])
        .assert_code(0)
        .assert_stdout("Nothing to recover");

    let json = world.json(&["recover"]);
    assert_eq!(json["pending"].as_array().expect("an array").len(), 0);
}

#[test]
fn recover_rolls_a_crashed_commit_back_to_byte_identical() {
    let world = World::realistic();
    world.assert_hermetic();
    let before = world.snapshot();

    // Stopped after every file had moved and before a single playlist was
    // rewritten — the worst moment, because the library and the playlists
    // disagree.
    let txid = crash_a_commit(&world, Inject::AfterSteps);
    world.snapshot().assert_differs(&before);

    let run = world.answer(&["recover"], "y");
    run.assert_code(0).assert_stdout(&txid);

    world.snapshot().assert_same(&before);
    // The record is resolved, so a second `recover` has nothing to do.
    world
        .run(&["recover"])
        .assert_code(0)
        .assert_stdout("Nothing to recover");
}

#[test]
fn recover_forward_finishes_a_crashed_commit_instead() {
    let world = World::realistic();
    let before = world.snapshot();

    let txid = crash_a_commit(&world, Inject::AfterSteps);

    let run = world.answer(&["recover", "--forward"], "y");
    run.assert_code(0).assert_stdout("finished");

    // The move completed: files moved, and the playlists the crash had not got
    // to are rewritten.
    assert!(
        world
            .abs(&format!("{DESTINATION}/01 Beef Rap.mp3"))
            .is_file()
    );
    let text =
        std::fs::read_to_string(world.fx.playlist_path(names::HIP_HOP_PLAYLIST)).expect("readable");
    assert!(text.contains(DESTINATION), "{text}");

    // And it is an ordinary transaction afterwards: undoing it gets back to the
    // start.
    let record = Store::at(world.fx.data_dir())
        .load(&mpdfm_core::journal::record::TxId::parse(&txid).expect("a txid"))
        .expect("the record is readable");
    assert_eq!(record.status, Status::Complete);

    world.answer(&["undo", &txid], "y").assert_code(0);
    world.snapshot().assert_same(&before);
}

#[test]
fn recover_asks_before_it_touches_anything() {
    let world = World::realistic();
    crash_a_commit(&world, Inject::AfterSteps);
    let crashed = world.snapshot();

    let run = world.answer(&["recover"], "n");
    run.assert_code(3).assert_stdout("Nothing was changed.");
    world.snapshot().assert_same(&crashed);

    let run = world.run(&["recover"]);
    run.assert_code(1).assert_stderr("stdin is not a terminal");
    world.snapshot().assert_same(&crashed);
}

/// A crash before the first file moved leaves nothing to put back, and
/// `recover` has to say so rather than finding nothing and complaining.
#[test]
fn recover_handles_a_transaction_that_had_not_started() {
    let world = World::realistic();
    let before = world.snapshot();

    crash_a_commit(&world, Inject::AfterPending);
    // Nothing was mutated: the record exists to prove a commit was *about* to
    // start.
    world.snapshot().assert_same(&before);

    world.answer(&["recover"], "y").assert_code(0);
    world.snapshot().assert_same(&before);
    world
        .run(&["recover"])
        .assert_code(0)
        .assert_stdout("Nothing to recover");
}

#[test]
fn recover_takes_a_txid_and_reports_it_in_json() {
    let world = World::realistic();
    let before = world.snapshot();
    let txid = crash_a_commit(&world, Inject::AfterSteps);

    let json = world.json(&["recover", &txid, "--yes"]);
    assert_eq!(json["pending"][0], txid.as_str(), "{json:#}");
    assert_eq!(json["recovered"][0]["of"], txid.as_str(), "{json:#}");
    assert_eq!(json["recovered"][0]["action"], "roll-back", "{json:#}");

    world.snapshot().assert_same(&before);
}
