//! `mpdfm organize`, driven as the user drives it (task 28).
//!
//! One test per acceptance criterion that a fixture can answer. The one it
//! cannot — a dry run over the real library — is in the task's "Hand-verified"
//! section, because the real library is read-only to tests (`docs/PLAN.md` §8).
//!
//! The fixture's mp3 template carries one set of tags (`MF DOOM`, `Mm..Food`,
//! track 1, `Beef Rap`), so two untouched mp3s always collide. The tests that
//! want something placeable tag it first with `mpdfm tag set`, through the
//! binary, and take their "before" snapshot *after* that — what has to come
//! back byte-identical is the library the organize found, not the one the
//! fixture built.

#![cfg(unix)]

mod harness;

use std::time::Instant;

use harness::World;
use mpdfm_core::testing::{DOTFILES_PLAYLISTS, Fixture, names};

/// Where the default template puts the MF DOOM album, once tagged.
const MF_DOOM_DEST: &str = "Hip-Hop/MF DOOM/2004 - Mm..Food";

/// The committed playlist set over the albums it references — `cli_move`'s
/// fixture, so an organize meets the same BOM, CRLF and CUE cases a move does.
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
            .non_ascii_album(names::KREAM_ALBUM)
            .flac_album(names::KIND_OF_BLUE_ALBUM)
            .real_playlists()
            .symlinked_playlist(names::RADIOS_PLAYLIST, DOTFILES_PLAYLISTS)
            .state_file_queue(&[names::MF_DOOM_TRACK, names::KIND_OF_BLUE_TRACK])
            .build(),
    )
}

/// Give every track in `dir` a distinct number and a title from its name, on
/// top of whatever else the template carries, plus `extra` flags.
fn tag(world: &World, dir: &str, extra: &[&str]) {
    let mut args = vec![
        "tag",
        "set",
        dir,
        "--renumber-tracks",
        "--title-from-filename",
        "--yes",
    ];
    args.extend_from_slice(extra);
    world.run(&args).assert_code(0);
}

/// Every playlist reference that resolves, and every one that does not, read
/// out of `mpdfm doctor --json` as in `cli_move`.
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

/// The txid out of the `Undo it with `mpdfm undo <txid>`.` line.
fn txid(stdout: &str) -> String {
    let start = stdout.find("mpdfm undo ").expect("the undo line") + "mpdfm undo ".len();
    stdout[start..]
        .split('`')
        .next()
        .expect("the txid")
        .to_owned()
}

fn count(document: &serde_json::Value, name: &str) -> u64 {
    document["counts"][name]
        .as_u64()
        .unwrap_or_else(|| panic!("counts.{name} is a number: {document:#}"))
}

// ---------------------------------------------------------------------------
// Criterion: a dry run reports placeable, already-correct and unplaceable.
// ---------------------------------------------------------------------------

#[test]
fn a_dry_run_reports_the_three_counts_and_writes_nothing() {
    let world = with_the_committed_playlists();
    world.assert_hermetic();
    tag(&world, names::MF_DOOM_ALBUM, &[]);
    tag(&world, names::SNOOP_ALBUM, &["--clear", "genre"]);
    let before = world.snapshot();

    let run = world.run(&[
        "organize",
        names::MF_DOOM_ALBUM,
        names::SNOOP_ALBUM,
        "--dry-run",
    ]);
    run.assert_code(0)
        .assert_stdout("PENDING")
        .assert_stdout(MF_DOOM_DEST)
        .assert_stdout("3 track(s) to move + 3 aux file(s)")
        .assert_stdout("0 already in place")
        .assert_stdout("2 cannot be placed")
        .assert_stdout("--dry-run: nothing was changed.")
        // The to-do list, last, by directory.
        .assert_stdout("To tag before they can be organized")
        .assert_stdout(&format!(
            "{}/  2 file(s): missing genre",
            names::SNOOP_ALBUM
        ));
    let todo = run.stdout.find("To tag before").unwrap();
    let dry = run.stdout.find("--dry-run: nothing").unwrap();
    assert!(
        dry < todo,
        "the to-do list comes at the end:\n{}",
        run.stdout
    );

    world.snapshot().assert_same(&before);
    world
        .run(&["undo", "--list"])
        .assert_stdout("tag")
        .assert_code(0);
}

#[test]
fn the_json_report_carries_the_counts_and_the_todo_list() {
    let world = with_the_committed_playlists();
    tag(&world, names::MF_DOOM_ALBUM, &[]);
    tag(&world, names::SNOOP_ALBUM, &["--clear", "album"]);

    let document = world.json(&[
        "organize",
        names::MF_DOOM_ALBUM,
        names::SNOOP_ALBUM,
        "--dry-run",
    ]);
    assert_eq!(count(&document, "placeable"), 3);
    assert_eq!(count(&document, "aux"), 3);
    assert_eq!(count(&document, "in_place"), 0);
    assert_eq!(count(&document, "unplaceable"), 2);
    assert_eq!(document["unplaceable"][0]["dir"], names::SNOOP_ALBUM);
    assert_eq!(document["unplaceable"][0]["reason"], "missing album");
    assert_eq!(document["exit"], 0);
}

// ---------------------------------------------------------------------------
// Criteria: an organize moves audio + aux and keeps every reference resolving;
// `undo` restores a byte-identical fixture.
// ---------------------------------------------------------------------------

#[test]
fn organizing_an_album_moves_its_aux_files_and_keeps_every_reference() {
    let world = with_the_committed_playlists();
    world.assert_hermetic();
    tag(&world, names::MF_DOOM_ALBUM, &[]);
    let (resolved_before, broken_before) = references(&world);
    let before = world.snapshot();

    let run = world.run(&["organize", names::MF_DOOM_ALBUM, "--yes"]);
    run.assert_code(0)
        .assert_stdout("complete  6 file(s) moved")
        .assert_stdout("Undo it with `mpdfm undo");

    for name in [
        "01 Beef Rap.mp3",
        "02 Hoe Cakes.mp3",
        "03 Potholderz (feat. Count Bass D).mp3",
        "folder.jpg",
        "info.nfo",
        "eac.log",
    ] {
        let moved = world.abs(&format!("{MF_DOOM_DEST}/{name}"));
        assert!(moved.is_file(), "{moved} should exist after the organize");
    }
    assert!(
        !world.abs(names::MF_DOOM_ALBUM).exists(),
        "the emptied album directory is gone"
    );

    // Every reference that resolved before still does, and nothing new broke.
    let (resolved_after, broken_after) = references(&world);
    assert_eq!(resolved_after, resolved_before);
    assert_eq!(broken_after, broken_before);
    let playlist = std::fs::read_to_string(world.fx.playlist_path(names::HIP_HOP_PLAYLIST))
        .expect("the playlist reads");
    assert!(playlist.contains(MF_DOOM_DEST), "{playlist}");

    // And undo puts it all back, byte for byte. By its own txid, not the
    // bare `undo`: the tag write above was committed by another process in the
    // same second, and two such ids do not sort in the order they happened.
    world
        .run(&["undo", &txid(&run.stdout), "--yes"])
        .assert_code(0);
    world.snapshot().assert_same(&before);
}

// ---------------------------------------------------------------------------
// Criterion: unplaceable files are listed and untouched.
// ---------------------------------------------------------------------------

#[test]
fn unplaceable_files_are_listed_and_left_where_they_are() {
    let world = with_the_committed_playlists();
    tag(&world, names::MF_DOOM_ALBUM, &[]);
    tag(&world, names::SNOOP_ALBUM, &["--clear", "album"]);

    let run = world.run(&[
        "organize",
        names::MF_DOOM_ALBUM,
        names::SNOOP_ALBUM,
        "--yes",
    ]);
    run.assert_code(0).assert_stdout(&format!(
        "{}/  2 file(s): missing album",
        names::SNOOP_ALBUM
    ));

    assert!(world.abs(names::SNOOP_TRACK).is_file(), "it stayed put");
    assert!(
        world.abs(MF_DOOM_DEST).is_dir(),
        "the placeable album moved"
    );
}

// ---------------------------------------------------------------------------
// Criterion: `--only-missing` skips files already at their destination.
// ---------------------------------------------------------------------------

#[test]
fn only_missing_skips_what_is_already_in_place() {
    let world = with_the_committed_playlists();
    tag(&world, names::MF_DOOM_ALBUM, &[]);
    world
        .run(&["organize", names::MF_DOOM_ALBUM, "--yes"])
        .assert_code(0);
    let before = world.snapshot();

    let run = world.run(&["organize", MF_DOOM_DEST, "--only-missing", "--yes"]);
    run.assert_code(0)
        .assert_stdout("3 already in place")
        .assert_stdout("There is nothing to do.");
    world.snapshot().assert_same(&before);
}

/// The other half of "complete tags": a `|default` that the lenient run would
/// use is a missing tag under `--only-missing`.
#[test]
fn only_missing_does_not_place_by_a_default() {
    let world = with_the_committed_playlists();
    tag(&world, names::SNOOP_ALBUM, &["--clear", "genre"]);
    let template = "{genre|Unsorted}/{album}/{track:02} {title}";

    let lenient = world.json(&["organize", names::SNOOP_ALBUM, "-t", template, "--dry-run"]);
    assert_eq!(count(&lenient, "placeable"), 2, "{lenient:#}");

    let strict = world.json(&[
        "organize",
        names::SNOOP_ALBUM,
        "-t",
        template,
        "--only-missing",
        "--dry-run",
    ]);
    assert_eq!(count(&strict, "placeable"), 0, "{strict:#}");
    assert_eq!(count(&strict, "unplaceable"), 2);
    assert_eq!(strict["unplaceable"][0]["reason"], "missing genre");
}

// ---------------------------------------------------------------------------
// Criterion: `--limit 5` stages exactly 5 files' worth of moves.
// ---------------------------------------------------------------------------

#[test]
fn limit_stages_exactly_that_many_tracks() {
    let tracks: Vec<String> = (1..=8).map(|n| format!("{n:02} Track.mp3")).collect();
    let tracks: Vec<&str> = tracks.iter().map(String::as_str).collect();
    let world = World::new(
        Fixture::builder()
            .album("hiphop/Eight", &tracks)
            .aux("hiphop/Eight", &["folder.jpg"])
            .build(),
    );
    tag(&world, "hiphop/Eight", &[]);

    let document = world.json(&["organize", "--limit", "5", "--dry-run"]);
    assert_eq!(count(&document, "placeable"), 8);
    assert_eq!(count(&document, "staged"), 5);
    // Three tracks stay behind, so the album is split and its cover stays.
    assert_eq!(count(&document, "aux"), 0);
    let ops = document["effects"]["ops"].as_array().expect("ops").len();
    let renames = document["effects"]["fs_steps"]
        .as_array()
        .expect("fs_steps")
        .iter()
        .filter(|step| step.get("RenameFile").is_some())
        .count();
    assert_eq!((ops, renames), (5, 5), "{document:#}");

    let before = world.snapshot();
    world
        .run(&["organize", "--limit", "5", "--yes"])
        .assert_code(0);
    world.snapshot().assert_differs(&before);
    assert!(world.abs("hiphop/Eight/folder.jpg").is_file());
    assert!(world.abs("hiphop/Eight/06 Track.mp3").is_file());
}

// ---------------------------------------------------------------------------
// Criterion: collisions block the commit and name both sources.
// ---------------------------------------------------------------------------

#[test]
fn a_collision_blocks_the_commit_and_names_both_sources() {
    // Two untouched fixture mp3s carry the same tags, so they render to the
    // same path.
    let world = World::new(
        Fixture::builder()
            .album("rock/Twins", &["a.mp3", "b.mp3"])
            .album("rock/Fine", &["c.mp3"])
            .build(),
    );
    let before = world.snapshot();

    let run = world.run(&["organize", "rock/Twins", "--yes"]);
    run.assert_code(2)
        .assert_stdout("taken twice")
        .assert_stdout("rock/Twins/a.mp3")
        .assert_stdout("rock/Twins/b.mp3")
        .assert_stdout("Hip-Hop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3")
        .assert_stdout("Nothing was changed.");
    world.snapshot().assert_same(&before);

    let document = world.run(&["organize", "--dry-run", "--json"]);
    document.assert_code(2);
    let document = document.json();
    let sources = &document["conflicts"][0]["sources"];
    assert_eq!(
        sources.as_array().map(Vec::len),
        Some(3),
        "every source is named: {document:#}"
    );
}

// ---------------------------------------------------------------------------
// The template.
// ---------------------------------------------------------------------------

#[test]
fn a_bad_template_is_an_error_that_says_where() {
    let world = with_the_committed_playlists();
    world
        .run(&["organize", "-t", "{genre/{title}", "--dry-run"])
        .assert_code(1)
        .assert_stderr("--template does not parse")
        .assert_stderr("unclosed");
}

#[test]
fn the_template_defaults_to_the_configured_one() {
    let world = with_the_committed_playlists();
    tag(&world, names::MF_DOOM_ALBUM, &[]);
    let document = world.json(&["organize", names::MF_DOOM_ALBUM, "--dry-run"]);
    assert_eq!(
        document["template"],
        mpdfm_core::config::DEFAULT_ORGANIZE_TEMPLATE
    );
}

#[test]
fn without_yes_and_without_a_terminal_it_refuses_to_ask() {
    let world = with_the_committed_playlists();
    tag(&world, names::MF_DOOM_ALBUM, &[]);
    let before = world.snapshot();

    world
        .run(&["organize", names::MF_DOOM_ALBUM])
        .assert_code(1)
        .assert_stderr("--yes");
    world.snapshot().assert_same(&before);

    // At a terminal the question names how many files are about to move.
    world
        .answer(&["organize", names::MF_DOOM_ALBUM], "n")
        .assert_code(3)
        .assert_stderr("Move 6 files? [y/N]");
    world.snapshot().assert_same(&before);
}

// ---------------------------------------------------------------------------
// Pitfall: the transaction at scale.
// ---------------------------------------------------------------------------

/// 2 000 tracks in 200 albums, one playlist naming every one of them, organized
/// and undone. Records the journal's size and the commit's time — the numbers
/// the task wants known before the user finds them out — and holds the line on
/// both loosely enough to survive a loaded test runner.
#[test]
fn two_thousand_files_organize_and_undo_byte_identically() {
    const ALBUMS: usize = 200;
    const PER_ALBUM: usize = 10;
    let names: Vec<String> = (1..=PER_ALBUM)
        .map(|n| format!("{n:02} Track.mp3"))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut builder = Fixture::builder();
    let mut lines = Vec::new();
    for album in 0..ALBUMS {
        let dir = format!("bulk/Album {album:03}");
        builder = builder.album(&dir, &names).aux(&dir, &["folder.jpg"]);
        lines.extend(names.iter().map(|name| format!("{dir}/{name}")));
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let world = World::new(builder.playlist("Everything.m3u", &lines).build());
    let before = world.snapshot();

    // A template every file can satisfy without being re-tagged: the fixture's
    // genre, then where it already is.
    let template = "{genre}/{original_dir}/{filename}";
    let started = Instant::now();
    let run = world.run(&["organize", "-t", template, "--yes"]);
    let took = started.elapsed();
    run.assert_code(0).assert_stdout(&format!(
        "{} track(s) to move + {ALBUMS} aux file(s)",
        ALBUMS * PER_ALBUM
    ));

    let journal: u64 = walkdir(world.fx.data_dir());
    eprintln!(
        "organize of {} files: {} ms end to end, journal {} kB ({} build)",
        ALBUMS * (PER_ALBUM + 1),
        took.as_millis(),
        journal / 1024,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    assert!(world.abs("Hip-Hop/bulk/Album 199/10 Track.mp3").is_file());
    let playlist = std::fs::read_to_string(world.fx.playlist_path("Everything.m3u")).unwrap();
    assert_eq!(
        playlist.matches("Hip-Hop/bulk/").count(),
        ALBUMS * PER_ALBUM
    );

    world.run(&["undo", "--yes"]).assert_code(0);
    world.snapshot().assert_same(&before);
}

/// Total bytes of every file under `dir`.
fn walkdir(dir: &camino::Utf8Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = dir.read_dir_utf8() else {
            continue;
        };
        for entry in entries.flatten() {
            let meta = entry.metadata().expect("metadata");
            if meta.is_dir() {
                stack.push(entry.path().to_path_buf());
            } else {
                total += meta.len();
            }
        }
    }
    total
}
