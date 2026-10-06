//! Task 17 — writing tags without losing anything else.
//!
//! The acceptance criteria of `docs/tasks/17-tag-write.md`, one test each. Two
//! of them need more than a before-and-after of the file's bytes, because a tag
//! write is *supposed* to change those:
//!
//! - [`tags::dump`][mpdfm_core::testing::tags::dump] is the full tag dump the
//!   "every other frame is byte-identical" criterion asks for: every item, every
//!   value, and a digest of every picture's bytes, sorted. Two dumps that match
//!   mean nothing in the tag changed but what the delta named;
//! - [`tags::audio_digest`][mpdfm_core::testing::tags::audio_digest] is the audio
//!   stream with every tag stripped, hashed. Comparing whole files would be
//!   comparing the tag too.
//!
//! The last three criteria are about the transaction rather than the file, so
//! they go through `commit` and `undo` exactly as a move does.

#![cfg(unix)]

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::config::Config;
use mpdfm_core::journal::record::{Status, TxId};
use mpdfm_core::journal::store::Store;
use mpdfm_core::journal::undo;
use mpdfm_core::library::Library;
use mpdfm_core::ops::commit::{self, Previewed};
use mpdfm_core::ops::exec_fs::{Done, FsStep};
use mpdfm_core::ops::{Committed, Effects, Operation, Plan};
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::{self, Field, Id3Version, TagDelta, TagError, WriteOpts};
use mpdfm_core::testing::{AudioTemplate, Fixture, Snapshot, tags as fix};

/// A fixture with one mp3, one FLAC and one m4a, each carrying something a write
/// must not disturb.
fn world() -> Fixture {
    let fx = Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3"])
        .flac_album("jazz/album")
        .track("coding-music/SwitchAngel/Coding_Trance_Reprise.m4a")
        .build();

    let mp3 = fx.abs("hiphop/album/01 Beef Rap.mp3");
    fix::set_frame(&mp3, "TBPM", "174");
    fix::embed_cover(&mp3, fix::COVER_PNG);

    let flac = fx.abs("jazz/album/01 So What.flac");
    fix::set_comment(&flac, "REPLAYGAIN_TRACK_GAIN", "-7.26 dB");
    fix::set_comment(&flac, "MUSICBRAINZ_ALBUMID", "cafe-f00d");
    fix::set_multi_valued(&flac, "artist", &["Miles Davis", "John Coltrane"]);
    fix::embed_cover(&flac, fix::COVER_PNG);

    fx
}

/// Every track `world` builds.
const TRACKS: [&str; 3] = [
    "hiphop/album/01 Beef Rap.mp3",
    "jazz/album/01 So What.flac",
    "coding-music/SwitchAngel/Coding_Trance_Reprise.m4a",
];

/// Write `delta` into `rel`, backing it up inside the fixture's data directory.
fn write(fx: &Fixture, rel: &str, delta: &TagDelta) -> tags::TagBackup {
    write_with(fx, rel, delta, WriteOpts::new())
}

fn write_with(fx: &Fixture, rel: &str, delta: &TagDelta, opts: WriteOpts) -> tags::TagBackup {
    let abs = fx.abs(rel);
    let backup = fx.data_dir().join("backups/test/tags").join(rel);
    let opts = WriteOpts {
        backup: Some(backup),
        ..opts
    };
    tags::write(&abs, delta, &opts).unwrap_or_else(|err| panic!("{rel} should be writable: {err}"))
}

/// The lines of `before` that `after` does not have, and the other way round.
fn tag_diff(before: &str, after: &str) -> Vec<String> {
    let (before, after): (Vec<&str>, Vec<&str>) =
        (before.lines().collect(), after.lines().collect());
    before
        .iter()
        .filter(|line| !after.contains(*line))
        .map(|line| format!("-{line}"))
        .chain(
            after
                .iter()
                .filter(|line| !before.contains(*line))
                .map(|line| format!("+{line}")),
        )
        .collect()
}

// ---------------------------------------------------------------------------

#[test]
fn setting_one_field_changes_that_field_and_nothing_else() {
    let fx = world();
    for rel in TRACKS {
        let abs = fx.abs(rel);
        let before = fix::dump(&abs);

        write(&fx, rel, &TagDelta::new().set(Field::Genre, "Hip Hop"));

        let after = fix::dump(&abs);
        let changed = tag_diff(&before, &after);
        assert_eq!(
            changed.len(),
            2,
            "{rel}: a genre edit changed more than the genre:\n{}",
            changed.join("\n")
        );
        assert!(
            changed.iter().any(|line| line.starts_with('-')
                && (line.contains("TCON") || line.contains("genre") || line.contains("gen"))),
            "{rel}: {changed:?}"
        );
        assert_eq!(
            tags::read_tags(&abs).expect("it reads").genre.first(),
            Some("Hip Hop")
        );
    }
}

#[test]
fn everything_mpdfm_does_not_model_survives_a_write() {
    let fx = world();

    write(
        &fx,
        "hiphop/album/01 Beef Rap.mp3",
        &TagDelta::new().set(Field::Genre, "Hip Hop"),
    );
    let mp3 = tags::read_tags(&fx.abs("hiphop/album/01 Beef Rap.mp3")).expect("it reads");
    assert!(
        mp3.extra.iter().any(|(k, v)| k == "TBPM" && v == "174"),
        "{:?}",
        mp3.extra
    );
    assert!(
        mp3.extra.iter().any(|(k, _)| k == "TXXX:comment"),
        "{:?}",
        mp3.extra
    );

    write(
        &fx,
        "jazz/album/01 So What.flac",
        &TagDelta::new().set(Field::Album, "Kind of Blue"),
    );
    let flac = tags::read_tags(&fx.abs("jazz/album/01 So What.flac")).expect("it reads");
    for key in ["REPLAYGAIN_TRACK_GAIN", "MUSICBRAINZ_ALBUMID", "encoder"] {
        assert!(
            flac.extra.iter().any(|(k, _)| k == key),
            "{key} did not survive: {:?}",
            flac.extra
        );
    }
}

#[test]
fn embedded_cover_art_survives_a_tag_write() {
    let fx = world();
    for rel in ["hiphop/album/01 Beef Rap.mp3", "jazz/album/01 So What.flac"] {
        let abs = fx.abs(rel);
        let cover = |dump: &str| {
            dump.lines()
                .filter(|line| line.starts_with("@PICTURE"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let before = cover(&fix::dump(&abs));
        assert_eq!(before.len(), 1, "{rel} should start with one cover");

        write(&fx, rel, &TagDelta::new().set(Field::Genre, "Hip Hop"));

        assert_eq!(
            cover(&fix::dump(&abs)),
            before,
            "{rel} lost or changed its cover art"
        );
    }
}

#[test]
fn the_audio_stream_is_bit_identical_after_a_write() {
    let fx = world();
    for rel in TRACKS {
        let abs = fx.abs(rel);
        let before = fix::audio_digest(&abs);
        write(
            &fx,
            rel,
            &TagDelta::new()
                .set(Field::Title, "Something Else")
                .set(Field::Track, "7/9"),
        );
        assert_eq!(
            fix::audio_digest(&abs),
            before,
            "{rel}: the audio stream changed"
        );
    }
}

#[test]
fn a_v23_file_stays_v23_and_v24_upgrades_it() {
    let fx = Fixture::builder().album("pop/album", &["01 a.mp3"]).build();
    let kept = fx.abs("pop/album/kept.mp3");
    let upgraded = fx.abs("pop/album/upgraded.mp3");
    fix::write_as(&kept, AudioTemplate::Mp3v23);
    fix::write_as(&upgraded, AudioTemplate::Mp3v23);

    let delta = TagDelta::new().set(Field::Genre, "Pop");
    write(&fx, "pop/album/kept.mp3", &delta);
    write_with(
        &fx,
        "pop/album/upgraded.mp3",
        &delta,
        WriteOpts::new().with_id3_version(Id3Version::V24),
    );

    assert!(
        fix::dump(&kept).contains("@VERSION V3"),
        "the default policy must leave a v2.3 file as v2.3:\n{}",
        fix::dump(&kept)
    );
    assert!(
        fix::dump(&upgraded).contains("@VERSION V4"),
        "id3_version = \"v24\" must upgrade:\n{}",
        fix::dump(&upgraded)
    );
    // Both still read the same, which is the point of the setting being about
    // storage rather than about meaning.
    for path in [&kept, &upgraded] {
        let tags = tags::read_tags(path).expect("it reads");
        assert_eq!(tags.genre.first(), Some("Pop"));
        assert_eq!(tags.year(), Some(2022));
        assert_eq!(tags.track, Some((5, Some(12))));
    }

    // And a v2.4 file left alone by `keep` stays v2.4.
    let already = fx.abs("pop/album/already.mp3");
    fix::write_as(&already, AudioTemplate::Mp3v24);
    write(&fx, "pop/album/already.mp3", &delta);
    assert!(fix::dump(&already).contains("@VERSION V4"));
}

#[test]
fn clearing_a_field_removes_it_rather_than_writing_an_empty_string() {
    let fx = world();
    for rel in TRACKS {
        let abs = fx.abs(rel);
        assert!(
            !tags::read_tags(&abs).expect("it reads").genre.is_empty(),
            "{rel} should start with a genre"
        );

        write(&fx, rel, &TagDelta::new().clear(Field::Genre));

        let tags = tags::read_tags(&abs).expect("it reads");
        assert!(tags.genre.is_empty(), "{rel}: {:?}", tags.genre);
        // Not "there and empty": the frame is gone from the dump entirely.
        let dump = fix::dump(&abs);
        for key in ["TCON=", "genre=", "GENRE=", "\u{a9}gen="] {
            assert!(!dump.contains(key), "{rel} still holds {key}:\n{dump}");
        }
    }
}

#[test]
fn a_multi_valued_flac_field_is_written_as_several_comments() {
    let fx = world();
    let abs = fx.abs("jazz/album/01 So What.flac");

    write(
        &fx,
        "jazz/album/01 So What.flac",
        &TagDelta::new().set(Field::Artist, "Madvillain; MF DOOM; Madlib"),
    );

    let tags = tags::read_tags(&abs).expect("it reads");
    assert_eq!(tags.artist.all(), ["Madvillain", "MF DOOM", "Madlib"]);
    // Three comments, not one with semicolons in it.
    let dump = fix::dump(&abs);
    assert_eq!(
        dump.lines()
            .filter(|line| line.to_ascii_lowercase().starts_with("artist="))
            .count(),
        3,
        "{dump}"
    );
}

#[test]
fn an_untouched_multi_valued_field_is_left_as_it_was() {
    let fx = world();
    let abs = fx.abs("jazz/album/01 So What.flac");

    write(
        &fx,
        "jazz/album/01 So What.flac",
        &TagDelta::new().set(Field::Genre, "Jazz"),
    );

    assert_eq!(
        tags::read_tags(&abs).expect("it reads").artist.all(),
        ["Miles Davis", "John Coltrane"],
        "a genre edit collapsed the artist"
    );
}

#[test]
fn a_comment_keeps_the_spelling_the_file_uses() {
    let fx = world();
    let abs = fx.abs("jazz/album/01 So What.flac");
    // The fixture's FLACs are written by ffmpeg, which spells its keys in lower
    // case. Rewriting nine keys in order to change one is not "only the fields
    // in the delta", so the file's own spelling is kept.
    assert!(fix::dump(&abs).contains("title="), "{}", fix::dump(&abs));

    write(
        &fx,
        "jazz/album/01 So What.flac",
        &TagDelta::new().set(Field::Title, "So What"),
    );

    let dump = fix::dump(&abs);
    assert!(dump.contains("title=So What"), "{dump}");
    assert!(!dump.contains("TITLE="), "{dump}");
}

#[test]
fn a_read_only_file_fails_preflight_before_a_temp_file_exists() {
    use std::os::unix::fs::PermissionsExt as _;

    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs = fx.abs(rel);
    let was = std::fs::metadata(abs.as_std_path())
        .expect("the fixture is there")
        .permissions();
    std::fs::set_permissions(abs.as_std_path(), std::fs::Permissions::from_mode(0o444))
        .expect("the fixture is ours");
    let before = Snapshot::capture(fx.music_dir());

    let backup = fx.data_dir().join("backups/test/tags").join(rel);
    let opts = WriteOpts::new().backing_up_to(backup.clone());
    let err = tags::write(&abs, &TagDelta::new().set(Field::Genre, "Hip Hop"), &opts)
        .expect_err("a read-only file must be refused");

    assert!(matches!(err, TagError::ReadOnly { .. }), "{err:?}");
    assert_eq!(err.path(), abs);
    // No backup, and no temp file: the refusal came before either.
    assert!(!backup.exists(), "a refused write took a backup anyway");
    before.assert_same(&Snapshot::capture(fx.music_dir()));
    std::fs::set_permissions(abs.as_std_path(), was).expect("the fixture is ours");
}

#[test]
fn an_injected_failure_leaves_the_original_exactly_as_it_was() {
    for inject in [
        tags::write::Inject::AfterBackup,
        tags::write::Inject::BeforeRename,
    ] {
        let fx = world();
        let rel = "hiphop/album/01 Beef Rap.mp3";
        let abs = fx.abs(rel);
        let before = Snapshot::capture(fx.music_dir());
        let dump_before = fix::dump(&abs);

        let opts = WriteOpts {
            backup: Some(fx.data_dir().join("backups/test/tags").join(rel)),
            inject,
            ..WriteOpts::new()
        };
        let err = tags::write(&abs, &TagDelta::new().set(Field::Genre, "Hip Hop"), &opts)
            .expect_err("the injected failure must stop the write");
        assert!(matches!(err, TagError::Injected { .. }), "{err:?}");

        // Byte for byte, including the absence of a temp file beside it.
        before.assert_same(&Snapshot::capture(fx.music_dir()));
        assert_eq!(fix::dump(&abs), dump_before);
    }
}

#[test]
fn a_restore_puts_the_file_back_byte_for_byte() {
    let fx = world();
    let rel = "jazz/album/01 So What.flac";
    let before = Snapshot::capture(fx.music_dir());

    let backup = write(
        &fx,
        rel,
        &TagDelta::new()
            .set(Field::Genre, "Hip Hop")
            .clear(Field::Album)
            .set(Field::Artist, "Someone Else"),
    );
    assert!(
        !before.diff(&Snapshot::capture(fx.music_dir())).is_empty(),
        "the write changed nothing"
    );

    tags::restore(&backup).expect("the backup restores");
    before.assert_same(&Snapshot::capture(fx.music_dir()));
}

#[test]
fn a_bad_edit_is_refused_before_anything_is_touched() {
    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs = fx.abs(rel);
    let before = Snapshot::capture(fx.music_dir());

    let backup = fx.data_dir().join("backups/test/tags").join(rel);
    let err = tags::write(
        &abs,
        &TagDelta::new().set(Field::Track, "one"),
        &WriteOpts::new().backing_up_to(backup.clone()),
    )
    .expect_err("`one` is not a track number");

    assert!(matches!(err, TagError::BadEdit { .. }), "{err:?}");
    assert!(err.to_string().contains("01 Beef Rap.mp3"), "{err}");
    assert!(!backup.exists());
    before.assert_same(&Snapshot::capture(fx.music_dir()));
}

// ---------------------------------------------------------------------------
// Inside a transaction.

/// A fixture with the three things a preview needs scanned from it.
struct Txn {
    fx: Fixture,
    library: Library,
    index: PlaylistIndex,
    config: Config,
}

impl Txn {
    fn new(fx: Fixture) -> Self {
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

    fn effects(&self, plan: &Plan) -> Effects {
        plan.validate(&self.library, &self.index, &self.config)
    }

    fn commit(&self, plan: &Plan) -> Committed {
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
        commit::commit_with(&previewed, &self.config, &commit::Options::default())
            .expect("the test's own plan commits")
    }

    fn store(&self) -> Store {
        Store::at(self.fx.data_dir())
    }

    fn undo(&mut self, txid: &TxId) -> undo::Reversed {
        let record = self
            .store()
            .load(txid)
            .unwrap_or_else(|err| panic!("the record for {txid} should load: {err}"));
        let reversed = undo::undo(
            &self.store(),
            &record,
            &self.config,
            &undo::Options::default(),
        )
        .unwrap_or_else(|err| panic!("{txid} should undo: {err}"));
        self.rescan();
        reversed
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot::capture(self.fx.music_dir())
    }
}

#[test]
fn a_tag_edit_commits_as_one_journaled_transaction() {
    let txn = Txn::new(world());
    let target = txn.fx.rel("hiphop/album/01 Beef Rap.mp3");
    let plan = Plan::of(vec![Operation::WriteTags {
        target: target.clone(),
        changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
    }]);

    let effects = txn.effects(&plan);
    assert_eq!(effects.summary.tags_written, 1);
    assert_eq!(effects.summary.fields_changed, 1);
    assert_eq!(effects.summary.files_moved, 0);
    // The preview names the field and the count, not the path.
    let preview = effects.render(80);
    assert!(preview.contains("TAG"), "{preview}");
    assert!(preview.contains(r#"genre = "Hip Hop""#), "{preview}");
    assert!(preview.contains("1 file"), "{preview}");

    let committed = txn.commit(&plan);
    assert_eq!(committed.record.status, Status::Complete);
    assert_eq!(committed.record.steps.len(), 1);
    assert!(matches!(
        committed.record.steps[0].receipt.as_ref().map(|r| &r.done),
        Some(Done::TagsWritten { .. })
    ));
    assert_eq!(
        tags::read_tags(&txn.fx.abs("hiphop/album/01 Beef Rap.mp3"))
            .expect("it reads")
            .genre
            .first(),
        Some("Hip Hop")
    );
}

#[test]
fn a_mixed_plan_commits_and_undoes_the_tag_edit_with_the_moves() {
    let mut txn = Txn::new(world());
    let before = txn.snapshot();

    let plan = Plan::of(vec![
        Operation::WriteTags {
            target: txn.fx.rel("hiphop/album/01 Beef Rap.mp3"),
            changes: TagDelta::new()
                .set(Field::Genre, "Hip Hop")
                .clear(Field::Comment),
        },
        Operation::MoveFile {
            from: txn.fx.rel("jazz/album/01 So What.flac"),
            to: txn.fx.rel("jazz/album/01 So What (remaster).flac"),
        },
        Operation::WriteTags {
            target: txn
                .fx
                .rel("coding-music/SwitchAngel/Coding_Trance_Reprise.m4a"),
            changes: TagDelta::new().set(Field::Album, "Coding"),
        },
    ]);

    let effects = txn.effects(&plan);
    assert_eq!(effects.summary.tags_written, 2);
    assert_eq!(effects.summary.files_moved, 1);

    let committed = txn.commit(&plan);
    txn.rescan();
    assert_eq!(
        tags::read_tags(&txn.fx.abs("hiphop/album/01 Beef Rap.mp3"))
            .expect("it reads")
            .genre
            .first(),
        Some("Hip Hop")
    );
    assert!(
        txn.fx
            .abs("jazz/album/01 So What (remaster).flac")
            .is_file()
    );
    assert!(!before.diff(&txn.snapshot()).is_empty(), "nothing changed");

    // And one undo puts all three back.
    let reversed = txn.undo(&committed.txid);
    assert!(
        reversed.steps >= 3,
        "all three operations' steps should be reversed, not {}",
        reversed.steps
    );
    before.assert_same(&txn.snapshot());
}

#[test]
fn undoing_a_tag_edit_restores_every_file_byte_for_byte() {
    let mut txn = Txn::new(world());
    let before = txn.snapshot();

    let plan = Plan::of(
        TRACKS
            .iter()
            .map(|rel| Operation::WriteTags {
                target: txn.fx.rel(rel),
                changes: TagDelta::new()
                    .set(Field::Genre, "Hip Hop")
                    .set(Field::Year, "1999-05-04")
                    .clear(Field::Comment),
            })
            .collect::<Vec<_>>(),
    );

    let committed = txn.commit(&plan);
    assert!(!before.diff(&txn.snapshot()).is_empty());

    txn.undo(&committed.txid);
    before.assert_same(&txn.snapshot());
}

#[test]
fn a_tag_edit_of_a_file_that_is_also_being_moved_runs_before_the_move() {
    let mut txn = Txn::new(world());
    let from = txn.fx.rel("hiphop/album/01 Beef Rap.mp3");
    let to = txn.fx.rel("hiphop/album/01 Beef Rap (clean).mp3");

    let plan = Plan::of(vec![
        // Staged the "wrong" way round on purpose: the move first.
        Operation::MoveFile {
            from: from.clone(),
            to: to.clone(),
        },
        Operation::WriteTags {
            target: from.clone(),
            changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
        },
    ]);

    let effects = txn.effects(&plan);
    assert!(
        effects.conflicts.is_empty(),
        "a tag edit and a move of the same file is an ordering question, not a \
         conflict: {:?}",
        effects.conflicts
    );
    // The tag write comes first in execution order, because the move takes its
    // file away.
    let first_tag = effects
        .fs_steps
        .iter()
        .position(|step| matches!(step, FsStep::WriteTags { .. }))
        .expect("there is a tag step");
    let the_move = effects
        .fs_steps
        .iter()
        .position(|step| matches!(step, FsStep::RenameFile { .. }))
        .expect("there is a move step");
    assert!(first_tag < the_move, "{:?}", effects.fs_steps);

    let committed = txn.commit(&plan);
    txn.rescan();
    let moved = txn.fx.abs("hiphop/album/01 Beef Rap (clean).mp3");
    assert_eq!(
        tags::read_tags(&moved).expect("it reads").genre.first(),
        Some("Hip Hop"),
        "the tag edit must have landed before the move"
    );
    assert_eq!(committed.record.status, Status::Complete);
}

#[test]
fn two_edits_of_the_same_file_are_refused_rather_than_run_in_order() {
    let txn = Txn::new(world());
    let target = txn.fx.rel("hiphop/album/01 Beef Rap.mp3");
    let plan = Plan::of(vec![
        Operation::WriteTags {
            target: target.clone(),
            changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
        },
        Operation::WriteTags {
            target: target.clone(),
            changes: TagDelta::new().set(Field::Album, "Mm..Food"),
        },
    ]);

    let effects = txn.effects(&plan);
    let message = effects
        .conflicts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.contains("both edit the tags of"), "{message}");
    assert!(!effects.is_committable());
}

#[test]
fn a_tag_edit_of_something_that_cannot_be_tagged_is_refused_in_the_preview() {
    let fx = Fixture::builder()
        .album("pop/album", &["01 a.mp3"])
        .aux("pop/album", &["notes.nfo"])
        .build();
    let txn = Txn::new(fx);

    let plan = Plan::of(vec![Operation::WriteTags {
        target: txn.fx.rel("pop/album/notes.nfo"),
        changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
    }]);

    let effects = txn.effects(&plan);
    let message = effects
        .conflicts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.contains("cannot be tagged"), "{message}");
    assert!(message.contains("notes.nfo"), "{message}");
    assert!(!effects.is_committable());
}

#[test]
fn a_read_only_file_refuses_the_whole_batch_before_anything_is_written() {
    use std::os::unix::fs::PermissionsExt as _;

    let txn = Txn::new(world());
    let locked = txn.fx.abs("jazz/album/01 So What.flac");
    // Captured and put back rather than guessed at: the fixture's files are
    // written with whatever the test runner's umask produces.
    let was = std::fs::metadata(locked.as_std_path())
        .expect("the fixture is there")
        .permissions();
    std::fs::set_permissions(locked.as_std_path(), std::fs::Permissions::from_mode(0o444))
        .expect("the fixture is ours");
    let before = txn.snapshot();

    let plan = Plan::of(
        TRACKS
            .iter()
            .map(|rel| Operation::WriteTags {
                target: txn.fx.rel(rel),
                changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
            })
            .collect::<Vec<_>>(),
    );

    let effects = txn.effects(&plan);
    assert!(
        !effects.is_committable(),
        "one unwritable file must refuse the batch"
    );
    let message = effects
        .conflicts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.contains("01 So What.flac"), "{message}");

    // And the other two files are untouched: nothing was written at all.
    before.assert_same(&txn.snapshot());
    std::fs::set_permissions(locked.as_std_path(), was).expect("the fixture is ours");
}

#[test]
fn the_id3_version_setting_is_resolved_from_the_configuration() {
    let fx = Fixture::builder().build();
    assert_eq!(fx.config().id3_version, Id3Version::Keep);

    for (value, want) in [
        ("keep", Id3Version::Keep),
        ("v23", Id3Version::V23),
        ("v24", Id3Version::V24),
    ] {
        assert_eq!(Id3Version::parse(value), Some(want));
        assert_eq!(want.as_str(), value);
    }
}

#[test]
fn the_realistic_fixture_survives_a_tag_edit_of_every_track() {
    // The pitfall task 17 names is "test against real files, not just
    // synthesized ones". `Fixture::realistic` is the closest a hermetic test
    // gets: the real library's names, the real mix of containers, the real
    // clutter beside them. Reading the actual library is a hand-verification
    // step, recorded in the task.
    let mut txn = Txn::new(Fixture::realistic());
    let tracks: Vec<_> = txn.fx.tracks().to_vec();
    let before = txn.snapshot();

    let plan = Plan::of(
        tracks
            .iter()
            .map(|rel| Operation::WriteTags {
                target: rel.clone(),
                changes: TagDelta::new().set(Field::Genre, "Reorganized"),
            })
            .collect::<Vec<_>>(),
    );

    let effects = txn.effects(&plan);
    assert!(
        effects.conflicts.is_empty(),
        "every track in the realistic fixture should be taggable: {:?}",
        effects.conflicts
    );
    assert_eq!(effects.summary.tags_written, tracks.len());

    let committed = txn.commit(&plan);
    for rel in &tracks {
        assert_eq!(
            tags::read_tags(&rel.to_abs(txn.fx.music_dir()))
                .expect("it reads")
                .genre
                .first(),
            Some("Reorganized"),
            "{rel}"
        );
    }

    txn.undo(&committed.txid);
    before.assert_same(&txn.snapshot());
}

/// The one thing a tag write must deliberately *not* preserve.
#[test]
fn the_mtime_advances_so_mpd_notices() {
    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs: Utf8PathBuf = fx.abs(rel);
    let abs: &Utf8Path = &abs;
    let before = std::fs::metadata(abs)
        .and_then(|meta| meta.modified())
        .expect("the fixture has an mtime");

    // The clock may not have moved since the fixture was built, so the write is
    // what has to produce a newer time rather than merely a different one.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&fx, rel, &TagDelta::new().set(Field::Genre, "Hip Hop"));

    let after = std::fs::metadata(abs)
        .and_then(|meta| meta.modified())
        .expect("it still has one");
    assert!(
        after > before,
        "MPD detects a changed file by its mtime, so a tag write must advance it"
    );
}

// ---------------------------------------------------------------------------
// What the real library turned out to hold.
//
// Each of these is a shape found by writing a copy of every one of the 2 808
// real audio files (see the task's hand-verification section). They are fixtures
// now so that they stay fixed.

#[test]
fn a_comment_language_that_is_not_a_language_is_repaired_rather_than_refused() {
    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs = fx.abs(rel);
    // 18 real mp3s hold `\x00\x00\x00` here, which no conforming ID3v2 writer
    // will emit: without the repair the whole tag fails to write.
    fix::set_comment_language(&abs, [0, 0, 0]);

    write(&fx, rel, &TagDelta::new().set(Field::Genre, "Hip Hop"));

    let tags = tags::read_tags(&abs).expect("it reads");
    assert_eq!(tags.genre.first(), Some("Hip Hop"));
    // The comment's text is kept; only the invalid language code changed.
    assert_eq!(tags.comment.first(), Some(fix::FIXTURE_COMMENT));
}

#[test]
fn a_file_with_two_stacked_id3v2_tags_is_refused_rather_than_silently_unchanged() {
    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs = fx.abs(rel);
    fix::stack_id3v2(&abs);
    let before = Snapshot::capture(fx.music_dir());

    let backup = fx.data_dir().join("backups/test/tags").join(rel);
    let err = tags::write(
        &abs,
        &TagDelta::new().set(Field::Genre, "Hip Hop"),
        &WriteOpts::new().backing_up_to(backup),
    )
    .expect_err("a write that cannot take effect must not report success");

    assert!(matches!(err, TagError::NotWritten { .. }), "{err:?}");
    assert!(err.to_string().contains("two stacked tags"), "{err}");
    // And the original is exactly as it was, as every other refusal leaves it.
    before.assert_same(&Snapshot::capture(fx.music_dir()));
}

#[test]
fn padding_between_the_tag_and_the_audio_is_searched_past() {
    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs = fx.abs(rel);
    // Two real albums have just over 2 KiB here, which is more than `lofty`
    // searches by default and well inside what MPDFM asks it to.
    fix::pad_with_junk(&abs, 4 * 1024);

    assert_eq!(
        tags::read_tags(&abs)
            .expect("a padded file still reads")
            .title
            .first(),
        Some("Beef Rap")
    );
    write(&fx, rel, &TagDelta::new().set(Field::Genre, "Hip Hop"));
    assert_eq!(
        tags::read_tags(&abs).expect("it reads").genre.first(),
        Some("Hip Hop")
    );
}

#[test]
fn a_file_whose_audio_cannot_be_found_reads_but_is_not_written() {
    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs = fx.abs(rel);
    // Megabytes of zeros is not padding, it is a damaged download — 14 of the
    // real library's mp3s are like this.
    fix::pad_with_junk(&abs, 256 * 1024);
    let before = Snapshot::capture(fx.music_dir());

    // Reading still works, because the tag is intact and is how the user will
    // recognize the file.
    let tags = tags::read_tags(&abs).expect("a damaged file's tag still reads");
    assert_eq!(tags.title.first(), Some("Beef Rap"));

    let backup = fx.data_dir().join("backups/test/tags").join(rel);
    let err = tags::write(
        &abs,
        &TagDelta::new().set(Field::Genre, "Hip Hop"),
        &WriteOpts::new().backing_up_to(backup.clone()),
    )
    .expect_err("a file whose container cannot be identified must not be written");

    assert!(matches!(err, TagError::Damaged { .. }), "{err:?}");
    assert!(err.to_string().contains("cannot be rewritten"), "{err}");
    // Refused in preflight: before a backup, and before a temp file.
    assert!(!backup.exists(), "a refused write took a backup anyway");
    before.assert_same(&Snapshot::capture(fx.music_dir()));
}

#[test]
fn a_damaged_file_is_a_preview_conflict_and_not_a_failed_commit() {
    let fx = world();
    fix::pad_with_junk(&fx.abs("hiphop/album/01 Beef Rap.mp3"), 256 * 1024);
    let txn = Txn::new(fx);
    let before = txn.snapshot();

    let plan = Plan::of(
        TRACKS
            .iter()
            .map(|rel| Operation::WriteTags {
                target: txn.fx.rel(rel),
                changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
            })
            .collect::<Vec<_>>(),
    );

    let effects = txn.effects(&plan);
    assert!(!effects.is_committable(), "the batch must be refused");
    let message = effects
        .conflicts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(message.contains("cannot be tagged"), "{message}");
    assert!(message.contains("01 Beef Rap.mp3"), "{message}");
    before.assert_same(&txn.snapshot());
}

#[test]
fn clearing_the_comment_leaves_a_described_comment_alone() {
    let fx = world();
    let rel = "hiphop/album/01 Beef Rap.mp3";
    let abs = fx.abs(rel);
    fix::set_comment_language(&abs, *b"eng");
    fix::add_described_comment(&abs, "Catalog Number", "7567882513");

    write(&fx, rel, &TagDelta::new().clear(Field::Comment));

    let tags = tags::read_tags(&abs).expect("it reads");
    assert!(tags.comment.is_empty(), "{:?}", tags.comment);
    assert!(
        tags.extra
            .iter()
            .any(|(k, v)| k == "COMM:Catalog Number" && v == "7567882513"),
        "a described comment is not the comment: {:?}",
        tags.extra
    );
}
