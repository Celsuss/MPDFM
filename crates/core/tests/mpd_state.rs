//! MPD's saved queue in the state file (task 14), one test per acceptance
//! criterion.
//!
//! Two halves, because the task has two halves. The first runs the parser over
//! the five committed state files in `tests/data/states/` and demands
//! `write(parse(bytes)) == bytes` — the same property task 06 holds the playlist
//! parser to, on a file with a different and uglier shape. The second drives a
//! whole commit and undo against a [`Fixture`]'s own state file and checks the
//! decision the task actually turns on: **MPD's in-memory queue wins at
//! shutdown**, so when the daemon is answering MPDFM warns and does not write,
//! and when it is not, MPDFM writes and backs the file up.
//!
//! No test here touches the real `~/.config/mpd/state`: every one of them works
//! on committed bytes or on a fixture's temp directory.

#![cfg(unix)]

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::config::Config;
use mpdfm_core::journal::record::TxId;
use mpdfm_core::journal::store::Store;
use mpdfm_core::journal::undo;
use mpdfm_core::library::Library;
use mpdfm_core::mpd::state::{self, MpdState, StateLine};
use mpdfm_core::ops::commit::{self, Previewed};
use mpdfm_core::ops::{Committed, Effects, Live, Operation, Plan, Warning};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::playlist::rewrite::PathMove;
use mpdfm_core::testing::{Fixture, names};

// ---------------------------------------------------------------------------
// The committed state files
// ---------------------------------------------------------------------------

/// One committed state file: the name its README uses, and its exact bytes.
struct StateTemplate {
    name: &'static str,
    bytes: &'static [u8],
}

/// Every committed state file. `tests/data/states/README.md` says what each is
/// for.
const STATES: &[StateTemplate] = &[
    StateTemplate {
        name: "state",
        bytes: include_bytes!("data/states/state"),
    },
    StateTemplate {
        name: "empty-queue",
        bytes: include_bytes!("data/states/empty-queue"),
    },
    StateTemplate {
        name: "no-queue",
        bytes: include_bytes!("data/states/no-queue"),
    },
    StateTemplate {
        name: "long-format",
        bytes: include_bytes!("data/states/long-format"),
    },
    StateTemplate {
        name: "no-trailing-newline",
        bytes: include_bytes!("data/states/no-trailing-newline"),
    },
];

fn template(name: &str) -> &'static StateTemplate {
    STATES
        .iter()
        .find(|state| state.name == name)
        .unwrap_or_else(|| panic!("there is no committed state fixture called {name:?}"))
}

/// Parse one of the committed files, with no disk anywhere near it.
fn parse(name: &str) -> MpdState {
    let template = template(name);
    MpdState::from_bytes(Utf8Path::new("/c/mpd/state"), template.bytes)
        .unwrap_or_else(|err| panic!("{name} should parse: {err}"))
}

fn rel(path: &str) -> RelPath {
    RelPath::parse(path).unwrap_or_else(|err| panic!("{path} should be a library path: {err}"))
}

fn lines(state: &MpdState) -> Vec<&str> {
    state.lines().iter().map(StateLine::line).collect()
}

fn read(path: &Utf8Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
}

fn text(path: &Utf8Path) -> String {
    String::from_utf8(read(path)).unwrap_or_else(|err| panic!("{path} is not UTF-8: {err}"))
}

#[test]
fn round_trips_every_committed_state_file_byte_for_byte() {
    for template in STATES {
        let state = parse(template.name);
        assert_eq!(
            state.to_bytes(),
            template.bytes,
            "{} did not round-trip; lines were {:?}",
            template.name,
            lines(&state)
        );
    }
}

/// The one shape an editor would silently "fix". If this fails, a fixture was
/// saved by something that tidied it — not a reason to change the assertion.
#[test]
fn the_committed_shapes_survive_in_the_repository() {
    assert!(
        !template("no-trailing-newline").bytes.ends_with(b"\n"),
        "no-trailing-newline has no trailing newline"
    );
    assert!(
        template("state")
            .bytes
            .windows(21)
            .any(|window| window == b"lastloadedplaylist: \n"),
        "state keeps the trailing space MPD writes after lastloadedplaylist:"
    );
}

#[test]
fn the_real_shape_reads_as_a_queue_of_ten_with_a_current_position() {
    let state = parse("state");
    assert!(state.has_queue());
    assert_eq!(state.queue_paths().len(), 10);
    assert_eq!(state.current(), Some(3));
    assert_eq!(
        state.queue_paths()[2],
        &rel(names::MF_DOOM_TRACK),
        "the queue is in file order"
    );
}

#[test]
fn a_state_file_with_no_playlist_begin_section_is_handled_without_error() {
    let mut state = parse("no-queue");
    assert!(!state.has_queue());
    assert!(state.queue_paths().is_empty());

    // And a move against it is a no-op rather than a failure.
    let edits = state::rewrite(
        &mut state,
        &[PathMove::moved(rel("pop/a.mp3"), rel("pop/b.mp3"))],
    );
    assert!(edits.is_empty());
    assert_eq!(state.to_bytes(), template("no-queue").bytes);
}

#[test]
fn an_empty_queue_section_is_a_queue_with_nothing_in_it() {
    let state = parse("empty-queue");
    assert!(state.has_queue(), "the markers are there");
    assert!(state.queue_paths().is_empty());
    assert_eq!(state.current(), None);
}

#[test]
fn a_move_rewrites_only_the_matching_queue_line() {
    let mut state = parse("state");
    let before = lines(&state).join("\n");

    let edits = state::rewrite(
        &mut state,
        &[PathMove::moved(
            rel(names::MF_DOOM_TRACK),
            rel("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3"),
        )],
    );

    assert_eq!(edits.len(), 1, "one line, not two: {edits:?}");
    assert_eq!(edits[0].old, format!("2:{}", names::MF_DOOM_TRACK));
    assert_eq!(
        edits[0].new.as_deref(),
        Some("2:hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3"),
        "the position is kept and only the path changes"
    );

    // Everything else, line for line.
    let after = lines(&state);
    for (at, (was, is)) in before.lines().zip(&after).enumerate() {
        if at == edits[0].entry {
            continue;
        }
        assert_eq!(was, *is, "line {at} should not have changed");
    }
    assert_eq!(after.len(), before.lines().count(), "no line was added");
}

#[test]
fn the_keys_around_the_queue_are_untouched_by_a_move() {
    let mut state = parse("state");
    state::rewrite(
        &mut state,
        &[PathMove::moved(
            rel(names::KREAM_TRACK),
            rel("electronic/KREAM/01 So Hï.mp3"),
        )],
    );

    let after = String::from_utf8(state.to_bytes()).expect("still UTF-8");
    for key in [
        "sw_volume: 75\n",
        "audio_device_state:1:PipeWire Sound Server\n",
        "audio_device_state:1:visualizer\n",
        "state: pause\n",
        "current: 3\n",
        "replay_gain_mode: off\n",
        "lastloadedplaylist: \n",
    ] {
        assert!(after.contains(key), "{key:?} should still be there");
    }
}

#[test]
fn an_unknown_key_from_a_newer_mpd_is_preserved_without_being_understood() {
    let state = parse("no-queue");
    let after = String::from_utf8(state.to_bytes()).expect("UTF-8");
    assert!(after.contains("future_key_from_a_newer_mpd: 7\n"));
}

#[test]
fn removing_an_entry_renumbers_the_rest_and_fixes_current() {
    let mut state = parse("state");
    // `current: 3` names the Snoop track; the KREAM track at 0 goes.
    let was_current = state.queue_paths()[3].clone();

    let edits = state::rewrite(&mut state, &[PathMove::deleted(rel(names::KREAM_TRACK))]);

    assert_eq!(state.queue_paths().len(), 9);
    assert_eq!(
        state
            .lines()
            .iter()
            .filter_map(StateLine::index)
            .collect::<Vec<_>>(),
        (0..9).collect::<Vec<u32>>(),
        "the indices are consecutive again"
    );
    assert_eq!(
        state.current(),
        Some(2),
        "current: moved down with its song"
    );
    assert_eq!(
        state.queue_paths()[2],
        &was_current,
        "and still names the same song"
    );

    // The removal, the nine survivors that all shift down one, and `current:`.
    assert_eq!(edits.len(), 11);
    assert_eq!(edits.iter().filter(|edit| edit.is_removal()).count(), 1);
}

#[test]
fn the_lines_that_belong_to_a_long_format_entry_travel_with_it() {
    let mut state = parse("long-format");
    assert_eq!(state.queue_paths().len(), 2, "the stream is not a path");

    state::rewrite(&mut state, &[PathMove::deleted(rel(names::KREAM_TRACK))]);

    assert_eq!(
        lines(&state),
        vec![
            "state: play",
            "current: 0",
            "playlist_begin",
            "0:song_begin: https://ice.somafm.com/groovesalad-256-mp3",
            "Time: -1",
            "Title: Groove Salad",
            "song_end",
            &format!("1:{}", names::MF_DOOM_TRACK),
            "playlist_end",
        ],
        "the `Prio: 3` went with the entry it belonged to, and the stream was \
         renumbered without being rewritten"
    );
}

// ---------------------------------------------------------------------------
// A whole commit, against a fixture's own state file
// ---------------------------------------------------------------------------

/// A fixture with the three things a preview needs scanned from it, and its own
/// state file — the same harness tasks 11 and 12 use.
struct World {
    fx: Fixture,
    library: Library,
    index: PlaylistIndex,
    config: Config,
}

impl World {
    fn realistic() -> Self {
        let fx = Fixture::realistic();
        let config = fx.config();
        Self {
            library: Library::scan(fx.music_dir()).expect("the fixture scans"),
            index: PlaylistIndex::load(fx.playlist_dir()).0,
            config,
            fx,
        }
    }

    fn rescan(&mut self) {
        self.library = Library::scan(self.fx.music_dir()).expect("the fixture scans");
        self.index = PlaylistIndex::load(self.fx.playlist_dir()).0;
    }

    fn effects(&self, plan: &Plan, live: &Live<'_>) -> Effects {
        plan.validate_live(&self.library, &self.index, &self.config, live)
    }

    /// Preview with `live` and commit, which is the pairing commit insists on.
    fn commit(&self, plan: &Plan, live: &Live<'_>) -> (Effects, Committed) {
        let effects = self.effects(plan, live);
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
        let options = commit::Options {
            live: *live,
            ..commit::Options::default()
        };
        let committed = commit::commit_with(&previewed, &self.config, &options)
            .expect("the test's own plan commits");
        (effects, committed)
    }

    fn state_file(&self) -> &Utf8Path {
        self.fx.state_file()
    }

    fn store(&self) -> Store {
        Store::at(self.fx.data_dir())
    }

    fn undo(&mut self, txid: &TxId) {
        let record = self
            .store()
            .load(txid)
            .unwrap_or_else(|err| panic!("the record for {txid} should be readable: {err}"));
        undo::undo(
            &self.store(),
            &record,
            &self.config,
            &undo::Options::default(),
        )
        .unwrap_or_else(|err| panic!("{txid} should undo: {err}"));
        self.rescan();
    }
}

/// Moving the MF DOOM track, which the realistic fixture puts at position 1 of
/// the saved queue — where `current: 1` also points.
fn move_the_queued_track() -> Plan {
    Plan::of(vec![Operation::MoveFile {
        from: rel(names::MF_DOOM_TRACK),
        to: rel("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3"),
    }])
}

fn queue_warnings(effects: &Effects) -> Vec<&RelPath> {
    effects
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            Warning::InMpdQueue { path } => Some(path),
            _ => None,
        })
        .collect()
}

#[test]
fn when_mpd_is_not_reachable_the_state_file_is_rewritten_and_backed_up() {
    let world = World::realistic();
    let before = text(world.state_file());
    assert!(before.contains(&format!("1:{}\n", names::MF_DOOM_TRACK)));

    let (effects, committed) = world.commit(&move_the_queued_track(), &Live::default());

    assert_eq!(effects.state_edits.len(), 1, "{:?}", effects.state_edits);
    assert!(
        queue_warnings(&effects).is_empty(),
        "there is no live queue to warn about"
    );

    let after = text(world.state_file());
    assert!(
        after.contains("1:hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3\n"),
        "the queue line was rewritten:\n{after}"
    );
    assert!(!after.contains(names::MF_DOOM_TRACK));
    assert!(
        after.contains("current: 1\n"),
        "a move does not move current:"
    );

    // And the record knows, with the bytes it started from in the backup.
    let record = &committed.record;
    assert_eq!(record.state_edits, effects.state_edits);
    let backup = record
        .state_backup
        .as_ref()
        .expect("the state file was copied");
    assert_eq!(
        text(&record.backup_dir.join(backup)),
        before,
        "the backup holds the file as it was"
    );
}

#[test]
fn when_mpd_is_reachable_the_preview_names_each_moved_file_in_the_live_queue() {
    let world = World::realistic();
    let before = read(world.state_file());

    // What `playlistinfo` reported: the daemon is up and these are queued.
    let queue = [rel(names::KREAM_TRACK), rel(names::MF_DOOM_TRACK)];
    let live = Live {
        queue: Some(&queue),
    };

    let (effects, _committed) = world.commit(&move_the_queued_track(), &live);

    assert_eq!(
        queue_warnings(&effects),
        vec![&rel(names::MF_DOOM_TRACK)],
        "the warning names the file, and only the one that is queued"
    );
    assert!(
        effects.warnings.iter().any(|warning| {
            warning.to_string().contains(names::MF_DOOM_TRACK)
                && warning.to_string().contains("requeue")
        }),
        "the rendered warning says what the user has to do: {:?}",
        effects.warnings
    );
    assert!(
        effects.state_edits.is_empty(),
        "nothing is written behind a running daemon, which would clobber it"
    );
    assert_eq!(
        read(world.state_file()),
        before,
        "the state file is byte-identical"
    );
}

#[test]
fn rewrite_saved_queue_false_skips_the_file_entirely() {
    let mut world = World::realistic();
    world.config.rewrite_saved_queue = false;
    let before = read(world.state_file());

    let (effects, committed) = world.commit(&move_the_queued_track(), &Live::default());

    assert!(effects.state_edits.is_empty());
    assert!(queue_warnings(&effects).is_empty());
    assert_eq!(read(world.state_file()), before, "not a byte changed");
    assert_eq!(
        committed.record.state_backup, None,
        "and no copy was taken of a file MPDFM was told to leave alone"
    );
}

#[test]
fn undo_restores_the_state_file_exactly() {
    let mut world = World::realistic();
    let before = read(world.state_file());

    let (_effects, committed) = world.commit(&move_the_queued_track(), &Live::default());
    assert_ne!(read(world.state_file()), before, "the commit changed it");

    world.undo(&committed.txid);

    assert_eq!(read(world.state_file()), before, "undo put every byte back");
}

#[test]
fn a_commit_with_no_state_file_configured_is_not_a_failure() {
    let mut world = World::realistic();
    world.config.state_file = None;

    let (effects, committed) = world.commit(&move_the_queued_track(), &Live::default());

    assert!(effects.state_edits.is_empty());
    assert_eq!(committed.record.state_backup, None);
}

#[test]
fn a_state_file_that_is_not_there_is_not_worth_a_warning() {
    let mut world = World::realistic();
    world.config.state_file = Some(Utf8PathBuf::from("/nonexistent/mpd/state"));

    let effects = world.effects(&move_the_queued_track(), &Live::default());

    assert!(effects.state_edits.is_empty());
    assert!(
        !effects
            .warnings
            .iter()
            .any(|warning| matches!(warning, Warning::StateUnreadable { .. })),
        "a daemon that has never run has no state file: {:?}",
        effects.warnings
    );
}

#[test]
fn a_state_file_that_cannot_be_read_warns_and_does_not_refuse_the_plan() {
    let world = World::realistic();
    // Bytes MPD never wrote. The plan is still perfectly committable.
    std::fs::write(world.state_file(), b"state: \xff\n").expect("the fixture is writable");

    let effects = world.effects(&move_the_queued_track(), &Live::default());

    assert!(effects.conflicts.is_empty(), "{:?}", effects.conflicts);
    assert!(effects.state_edits.is_empty());
    assert!(
        effects
            .warnings
            .iter()
            .any(|warning| matches!(warning, Warning::StateUnreadable { .. })),
        "the user is told the queue was not examined: {:?}",
        effects.warnings
    );
}

#[test]
fn the_preview_reports_the_saved_queue_as_its_own_row() {
    let world = World::realistic();
    let effects = world.effects(&move_the_queued_track(), &Live::default());

    let rendered = effects.render(100);
    assert!(
        rendered.contains("MPD saved queue"),
        "the preview owns up to editing MPD's own file:\n{rendered}"
    );
}

#[test]
fn a_deleted_track_leaves_the_saved_queue_consecutive() {
    let world = World::realistic();
    let plan = Plan::of(vec![Operation::Delete {
        target: rel(names::KREAM_TRACK),
    }]);

    let (effects, _committed) = world.commit(&plan, &Live::default());
    assert!(!effects.state_edits.is_empty());

    let state = MpdState::load(world.state_file()).expect("the rewritten file parses");
    assert_eq!(
        state
            .lines()
            .iter()
            .filter_map(StateLine::index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "the KREAM track was at 0; the other three closed up"
    );
    assert!(
        !state
            .queue_paths()
            .iter()
            .any(|path| *path == &rel(names::KREAM_TRACK))
    );
    assert_eq!(
        state.current(),
        Some(0),
        "current: was 1 and the entry before it went"
    );
}
