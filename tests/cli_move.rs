//! `mpdfm move`, driven as the user drives it (task 15).
//!
//! One test per acceptance criterion, plus the ones the flags brought with them.
//! Every test runs the built binary against a [`Fixture`] in a temp directory;
//! none of them can reach the real library, and [`World::assert_hermetic`] is
//! what says so.
//!
//! The shape of the writing tests is the one `docs/PLAN.md` §8 asks for:
//! snapshot the music and playlist directories, run the command, and demand
//! either that nothing changed (`--dry-run`, a conflict, a declined prompt) or
//! that undoing it gets back to byte-identical.

#![cfg(unix)]

mod harness;

use harness::World;
use mpdfm_core::testing::{DOTFILES_PLAYLISTS, Fixture, names};

/// Where every test here moves the MF DOOM album to.
const DESTINATION: &str = "hiphop/MF DOOM/Mm..Food (2004)";

/// The committed playlist set — the same seventeen shapes as the real
/// `~/.config/mpd/playlists` — over the albums they reference.
///
/// This is the fixture tier's answer to the criterion about "a *copy* of the
/// user's real playlists": the templates are byte-exact reproductions of the
/// real set (`crates/core/src/testing/playlists.rs`), so a move across them
/// meets the same BOM, CRLF, missing-final-newline, duplicate-entry, CUE and
/// radio-URL cases the real ones have. The real files themselves are the user's
/// and are never touched by a test — see the task's "Verifying it by hand".
fn with_the_committed_playlists() -> World {
    World::new(
        Fixture::builder()
            .album(
                names::MF_DOOM_ALBUM,
                &[
                    "01 Beef Rap.mp3",
                    "02 Hoe Cakes.mp3",
                    "03 Potholderz (feat. Count Bass D).mp3",
                ],
            )
            .aux(names::MF_DOOM_ALBUM, &["folder.jpg", "info.nfo", "eac.log"])
            .album(
                names::SNOOP_ALBUM,
                &["01.Smokin' On.mp3", "02.Young, Wild & Free.mp3"],
            )
            .multi_disc(
                names::MERCURY_ALBUM,
                &["CD 1 - Mercury - Acts 1", "CD 2 - Mercury - Acts 2"],
            )
            .cue_reference(
                names::MERCURY_CD1,
                "Imagine Dragons - Mercury - Acts 1.flac.cue/track0017",
            )
            .non_ascii_album(names::KREAM_ALBUM)
            .flac_album(names::KIND_OF_BLUE_ALBUM)
            .album(
                "coding-music/SwitchAngel",
                &["Coding_Trance.mp3", "Coding_Trance_Reprise.m4a"],
            )
            // Before `symlinked_playlist`: this overwrites by name.
            .real_playlists()
            .symlinked_playlist(names::RADIOS_PLAYLIST, DOTFILES_PLAYLISTS)
            .state_file_queue(&[names::MF_DOOM_TRACK, names::KIND_OF_BLUE_TRACK])
            .build(),
    )
}

/// Every playlist reference that resolves, and every one that does not.
///
/// Read out of `mpdfm doctor --json`, so the invariant is checked through the
/// same command a user would check it with.
fn references(world: &World) -> (u64, Vec<String>) {
    let report = world.json(&["doctor"]);
    let broken = report["checks"]["broken-references"]["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .filter_map(|item| item["what"].as_str().map(ToOwned::to_owned))
        .collect();
    (
        report["references"]
            .as_u64()
            .expect("references is a number"),
        broken,
    )
}

// ---------------------------------------------------------------------------
// Criterion: `mpdfm move --dry-run` writes nothing.
// ---------------------------------------------------------------------------

#[test]
fn a_dry_run_writes_nothing() {
    let world = World::realistic();
    world.assert_hermetic();
    let before = world.snapshot();

    let run = world.run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--dry-run"]);
    run.assert_code(0)
        .assert_stdout("PENDING")
        .assert_stdout("--dry-run: nothing was changed.");

    world.snapshot().assert_same(&before);
    // And the journal is untouched: a dry run is not a transaction.
    world
        .run(&["undo", "--list"])
        .assert_code(0)
        .assert_stdout("the journal is empty");
}

/// The pitfall this file exists for: the preview shows the *full* picture, not a
/// summary line. The whole design rests on the user seeing which playlist lines
/// change.
#[test]
fn the_preview_names_every_playlist_that_changes() {
    let world = World::realistic();

    let run = world.run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--dry-run"]);
    run.assert_code(0);

    // The operation row, with what it costs.
    run.assert_stdout("MOVE").assert_stdout(DESTINATION);
    // The per-playlist rows. `MF_DOOM_TRACK` is referenced by two playlists on
    // purpose (task 03), and both of them have to appear.
    run.assert_stdout("Playlists")
        .assert_stdout(names::HIP_HOP_PLAYLIST)
        .assert_stdout(names::MF_DOOM_PLAYLIST)
        .assert_stdout("line rewritten");
    // And MPD's saved queue, which holds the same track.
    run.assert_stdout("MPD saved queue");
}

// ---------------------------------------------------------------------------
// Criterion: conflicts exit 2 and print what conflicted.
// ---------------------------------------------------------------------------

#[test]
fn a_conflict_exits_2_and_says_what_conflicted() {
    let world = World::realistic();
    let before = world.snapshot();

    // The destination is an existing directory, and nothing in this plan is
    // taking it out of the way.
    let run = world.run(&["move", names::MF_DOOM_ALBUM, "jazz", "--yes"]);
    run.assert_code(2)
        .assert_stdout("REFUSED")
        .assert_stdout("Conflicts (1)")
        .assert_stdout("jazz already exists")
        .assert_stdout("Nothing was changed.");

    world.snapshot().assert_same(&before);
}

#[test]
fn a_source_that_is_not_there_is_a_conflict_that_names_it() {
    let world = World::realistic();

    world
        .run(&["move", "hiphop/not an album", DESTINATION, "--yes"])
        .assert_code(2)
        .assert_stdout("hiphop/not an album is not in the library");
}

/// `--yes` does not override a conflict. The exit code is the refusal's, not the
/// prompt's.
#[test]
fn yes_does_not_make_a_refused_plan_commit() {
    let world = World::realistic();
    let before = world.snapshot();

    world
        .answer(&["move", names::MF_DOOM_ALBUM, "jazz", "--yes"], "y")
        .assert_code(2);

    world.snapshot().assert_same(&before);
}

// ---------------------------------------------------------------------------
// Criteria: the prompt. Declining exits 3; a non-TTY run refuses.
// ---------------------------------------------------------------------------

#[test]
fn declining_the_prompt_exits_3_and_changes_nothing() {
    let world = World::realistic();
    let before = world.snapshot();

    let run = world.answer(&["move", names::MF_DOOM_ALBUM, DESTINATION], "n");
    run.assert_code(3).assert_stdout("Nothing was changed.");
    // The question was asked, and on stderr so a piped preview stays clean.
    run.assert_stderr("Commit this? [y/N]");

    world.snapshot().assert_same(&before);
}

/// Anything that is not `y`/`yes` is a no, end-of-file included.
#[test]
fn only_yes_means_yes() {
    for reply in ["", "no", "sure", "Y E S", "yeah"] {
        let world = World::realistic();
        let before = world.snapshot();
        world
            .answer(&["move", names::MF_DOOM_ALBUM, DESTINATION], reply)
            .assert_code(3);
        world.snapshot().assert_same(&before);
    }

    // And the two that do.
    for reply in ["y", "YES"] {
        let world = World::realistic();
        world
            .answer(&["move", names::MF_DOOM_ALBUM, DESTINATION], reply)
            .assert_code(0);
        assert!(world.abs(DESTINATION).is_dir(), "{reply:?} should commit");
    }
}

#[test]
fn a_non_interactive_run_without_yes_refuses_rather_than_prompting() {
    let world = World::realistic();
    let before = world.snapshot();

    // `World::run` gives the process an empty pipe for stdin, which is what a
    // script or a CI job gives it. The failure this guards against is hanging
    // forever on a prompt nobody can answer.
    let run = world.run(&["move", names::MF_DOOM_ALBUM, DESTINATION]);
    run.assert_code(1)
        .assert_stderr("stdin is not a terminal")
        .assert_stderr("--yes");
    // The preview was still printed: a script's log should say what MPDFM
    // declined to do, not just that it declined.
    run.assert_stdout("PENDING");

    world.snapshot().assert_same(&before);
}

/// `--json` is not interactive either, however stdin was opened.
#[test]
fn a_json_run_without_yes_refuses_rather_than_prompting() {
    let world = World::realistic();
    let before = world.snapshot();

    let run = World::answer(
        &world,
        &["move", names::MF_DOOM_ALBUM, DESTINATION, "--json"],
        "y",
    );
    run.assert_code(1).assert_stderr("not interactive");

    world.snapshot().assert_same(&before);
}

// ---------------------------------------------------------------------------
// Criterion: committing, and the txid.
// ---------------------------------------------------------------------------

#[test]
fn a_commit_moves_the_album_rewrites_the_references_and_prints_its_txid() {
    let world = World::realistic();
    world.assert_hermetic();
    let before = world.snapshot();

    let run = world.run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"]);
    run.assert_code(0);
    world.snapshot().assert_differs(&before);

    // The files are where they were asked to go, aux files included, and the
    // source is gone.
    assert!(
        world
            .abs(&format!("{DESTINATION}/01 Beef Rap.mp3"))
            .is_file()
    );
    assert!(world.abs(&format!("{DESTINATION}/folder.jpg")).is_file());
    assert!(world.abs(&format!("{DESTINATION}/Mm..Food.m3u")).is_file());
    assert!(!world.abs(names::MF_DOOM_ALBUM).exists());

    // Both playlists that named the track now name its new path.
    for playlist in [names::HIP_HOP_PLAYLIST, names::MF_DOOM_PLAYLIST] {
        let text = std::fs::read_to_string(world.fx.playlist_path(playlist))
            .expect("the playlist is readable");
        assert!(
            text.contains(&format!("{DESTINATION}/01 Beef Rap.mp3")),
            "{playlist} was not rewritten:\n{text}"
        );
        assert!(
            !text.contains(names::MF_DOOM_ALBUM),
            "{playlist} still names the old path:\n{text}"
        );
    }

    // And MPD's saved queue, which the daemon was not holding because
    // `mpd_enabled` is false — so the file on disk *is* the queue (task 14).
    let state = std::fs::read_to_string(world.fx.state_file()).expect("the state file is readable");
    assert!(
        state.contains(&format!("{DESTINATION}/01 Beef Rap.mp3")),
        "the saved queue was not rewritten:\n{state}"
    );

    // The txid, printed as a command that can be pasted.
    let txid = world.json(&["undo", "--list"])["transactions"][0]["txid"]
        .as_str()
        .expect("the journal has a transaction")
        .to_owned();
    run.assert_stdout(&txid)
        .assert_stdout(&format!("Undo it with `mpdfm undo {txid}`."));
}

/// `--verify` changes what a commit *costs*, never what it does.
///
/// Within one filesystem every step is a rename, so there is nothing to hash —
/// the flag is for the cross-device copy path, which
/// `crates/core/tests/move_executor.rs` exercises directly. What is worth
/// asserting here is that passing it does not change the outcome, including the
/// one thing that would catch a mis-wired option: the move is still fully
/// reversible afterwards.
///
/// Two fixtures cannot be compared against each other — the symlinked
/// `Radios.m3u` points at its own temp directory — so the comparison is each
/// world against its own starting state.
#[test]
fn verify_commits_the_same_thing_and_stays_reversible() {
    for extra in [&[][..], &["--verify"][..]] {
        let world = World::realistic();
        let before = world.snapshot();

        let mut args = vec!["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"];
        args.extend_from_slice(extra);
        world.run(&args).assert_code(0);

        world.snapshot().assert_differs(&before);
        assert!(
            world
                .abs(&format!("{DESTINATION}/01 Beef Rap.mp3"))
                .is_file()
        );

        world.answer(&["undo"], "y").assert_code(0);
        world.snapshot().assert_same(&before);
    }
}

// ---------------------------------------------------------------------------
// `--merge`
// ---------------------------------------------------------------------------

/// Two albums, with one file each, and no name in common.
fn two_albums() -> World {
    World::new(
        Fixture::builder()
            .album("jazz/one", &["01 first.mp3"])
            .album("jazz/two", &["02 second.mp3"])
            .playlist("Jazz.m3u", &["jazz/one/01 first.mp3"])
            .build(),
    )
}

#[test]
fn an_existing_destination_needs_merge_said_out_loud() {
    let world = two_albums();
    let before = world.snapshot();

    world
        .run(&["move", "jazz/one", "jazz/two", "--yes"])
        .assert_code(2)
        .assert_stdout("jazz/two already exists");
    world.snapshot().assert_same(&before);
}

#[test]
fn merge_moves_what_does_not_collide() {
    let world = two_albums();

    world
        .run(&["move", "jazz/one", "jazz/two", "--yes", "--merge"])
        .assert_code(0);

    assert!(world.abs("jazz/two/01 first.mp3").is_file());
    assert!(world.abs("jazz/two/02 second.mp3").is_file());
    assert!(!world.abs("jazz/one").exists());

    let text = std::fs::read_to_string(world.fx.playlist_path("Jazz.m3u")).expect("readable");
    assert_eq!(text, "jazz/two/01 first.mp3\n");
}

/// `--merge` is not `--force`: a file whose destination is taken is still a
/// conflict, and not one file is overwritten.
#[test]
fn merge_still_refuses_a_file_that_would_be_overwritten() {
    let world = World::new(
        Fixture::builder()
            .album("jazz/one", &["01 same name.mp3", "02 unique.mp3"])
            .album("jazz/two", &["01 same name.mp3"])
            .build(),
    );
    let before = world.snapshot();

    world
        .run(&["move", "jazz/one", "jazz/two", "--yes", "--merge"])
        .assert_code(2)
        .assert_stdout("jazz/two/01 same name.mp3 already exists");

    world.snapshot().assert_same(&before);
}

// ---------------------------------------------------------------------------
// Criterion: `--json` parses and says what the text output said.
// ---------------------------------------------------------------------------

#[test]
fn the_json_output_says_what_the_preview_said() {
    let world = World::realistic();
    let text = world.run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--dry-run"]);
    let json = world.json(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--dry-run"]);
    text.assert_code(0);

    // The `Effects` core serialized are the same value the renderer rendered,
    // so the summary's numbers have to be readable out of the text.
    let summary = &json["effects"]["summary"];
    let audio = summary["audio_moved"].as_u64().expect("a number");
    let files = summary["files_moved"].as_u64().expect("a number");
    text.assert_stdout(&format!("{audio} audio + {} aux files", files - audio));

    let playlists = summary["playlists_affected"].as_u64().expect("a number");
    assert_eq!(
        playlists,
        json["effects"]["playlist_edits"]
            .as_array()
            .expect("an array")
            .len() as u64
    );
    for edit in json["effects"]["playlist_edits"]
        .as_array()
        .expect("an array")
    {
        text.assert_stdout(edit["file_name"].as_str().expect("a file name"));
    }

    // A dry run committed nothing, and says so in the machine-readable form too.
    assert!(json["committed"].is_null(), "{json:#}");
    assert_eq!(json["exit"], 0, "{json:#}");

    // And a refusal carries the conflicts and the exit code a script acts on.
    let refused = world.run(&["move", names::MF_DOOM_ALBUM, "jazz", "--json", "--yes"]);
    refused.assert_code(2);
    let refused = refused.json();
    assert_eq!(refused["exit"], 2, "{refused:#}");
    assert_eq!(
        refused["effects"]["conflicts"]
            .as_array()
            .expect("an array")
            .len(),
        1,
        "{refused:#}"
    );
}

#[test]
fn a_committed_json_run_carries_the_txid() {
    let world = World::realistic();
    let json = world.json(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"]);

    let txid = json["committed"]["txid"]
        .as_str()
        .expect("a committed run has a txid");
    assert!(!txid.is_empty(), "{json:#}");
    // Three tracks and five aux files: the `.jpg`, `.nfo`, `.sfv`, `.log` and
    // the album-internal `.m3u` all travel with the album (task 05).
    assert_eq!(json["committed"]["summary"]["files_moved"], 8, "{json:#}");
    assert_eq!(json["committed"]["summary"]["audio_moved"], 3, "{json:#}");

    // The id it printed is the one `undo` takes.
    world.answer(&["undo", txid], "y").assert_code(0);
}

// ---------------------------------------------------------------------------
// Criterion: the end-to-end run over the committed playlist set.
// ---------------------------------------------------------------------------

/// Criterion: move an album, verify every reference still resolves, undo, verify
/// byte-identical.
///
/// "Every reference still resolves" is asserted as the invariant
/// `docs/PLAN.md` §8 states it — *every entry that resolved before still
/// resolves* — rather than as "nothing is broken", because this playlist set
/// deliberately contains references that never resolved. A move must not change
/// that set in either direction: not break a good one, and not quietly fix a bad
/// one.
#[test]
fn every_reference_that_resolved_before_the_move_still_resolves_and_undo_restores_the_bytes() {
    let world = with_the_committed_playlists();
    world.assert_hermetic();

    let before = world.snapshot();
    let (total_before, broken_before) = references(&world);
    assert!(
        total_before > 20,
        "the committed set has references to move"
    );
    assert!(
        !broken_before.is_empty(),
        "the committed set has a reference that never resolved"
    );

    world
        .run(&["move", names::MF_DOOM_ALBUM, DESTINATION, "--yes"])
        .assert_code(0);

    let (total_after, broken_after) = references(&world);
    assert_eq!(
        total_after, total_before,
        "a move must not add or drop a reference"
    );
    assert_eq!(
        broken_after, broken_before,
        "a move must neither break a reference that resolved nor fix one that did not"
    );

    // And back. Byte-identical, which covers the BOM, the CRLF file, the one
    // with no trailing newline, and the symlinked `Radios.m3u` staying a
    // symlink.
    world.answer(&["undo"], "y").assert_code(0);
    world.snapshot().assert_same(&before);

    let (total_undone, broken_undone) = references(&world);
    assert_eq!(total_undone, total_before);
    assert_eq!(broken_undone, broken_before);
}

// ---------------------------------------------------------------------------
// How `SRC` and `DST` are read
// ---------------------------------------------------------------------------

#[test]
fn an_absolute_path_inside_the_library_means_the_same_as_a_relative_one() {
    let world = World::realistic();
    let absolute = world.abs(names::MF_DOOM_ALBUM);

    world
        .run(&["move", absolute.as_str(), DESTINATION, "--dry-run"])
        .assert_code(0)
        .assert_stdout(&format!("{} → {DESTINATION}", names::MF_DOOM_ALBUM));
}

#[test]
fn a_path_outside_the_library_is_refused_before_anything_is_scanned() {
    let world = World::realistic();

    world
        .run(&["move", names::MF_DOOM_ALBUM, "/tmp/elsewhere", "--dry-run"])
        .assert_code(1)
        .assert_stderr("/tmp/elsewhere");

    world
        .run(&["move", names::MF_DOOM_ALBUM, "../escape", "--dry-run"])
        .assert_code(1)
        .assert_stderr("../escape");
}

/// Shell completion adds trailing slashes to directories and nobody means
/// anything by them.
#[test]
fn a_trailing_slash_is_ignored() {
    let world = World::realistic();

    world
        .run(&[
            "move",
            &format!("{}/", names::MF_DOOM_ALBUM),
            &format!("{DESTINATION}/"),
            "--dry-run",
        ])
        .assert_code(0)
        .assert_stdout("PENDING (1 op)");
}

#[test]
fn renaming_one_file_moves_only_that_file() {
    let world = World::realistic();

    let to = format!("{}/01 Beef Rap (remaster).mp3", names::MF_DOOM_ALBUM);
    world
        .run(&["move", names::MF_DOOM_TRACK, &to, "--yes"])
        .assert_code(0);

    assert!(world.abs(&to).is_file());
    assert!(!world.abs(names::MF_DOOM_TRACK).exists());
    // Its album-mates stayed put: this was a file move and the user named a
    // file.
    assert!(
        world
            .abs(&format!("{}/02 Hoe Cakes.mp3", names::MF_DOOM_ALBUM))
            .is_file()
    );
}
