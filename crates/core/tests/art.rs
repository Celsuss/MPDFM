//! Task 30 — cover art: reading it, embedding it, extracting it, removing it,
//! and undoing all three.
//!
//! The write criteria are checked the way task 17 checks a field edit: a full
//! tag dump before and after ([`fix::dump`]), where the only lines allowed to
//! differ are the `@PICTURE` line of the front cover, and the audio stream hashed
//! with every tag stripped ([`fix::audio_digest`]).

#![cfg(unix)]

use camino::Utf8Path;
use mpdfm_core::art::{self, CoverImage, ImageFormat, Mismatch, PictureKind};
use mpdfm_core::config::Config;
use mpdfm_core::journal::store::Store;
use mpdfm_core::journal::undo;
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::ops::commit::{self, Previewed};
use mpdfm_core::ops::exec_fs::Done;
use mpdfm_core::ops::{Committed, Conflict, Effects, Operation, Plan};
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::{self, Field, TagDelta};
use mpdfm_core::testing::{Fixture, Snapshot, tags as fix};

const MP3_ALBUM: &str = "hiphop/MF DOOM - Mm..Food (2004)";
const MP3S: [&str; 2] = ["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"];
const FLAC_ALBUM: &str = "jazz/Miles Davis - Kind of Blue";

/// An mp3 album whose tracks carry a front cover, a back cover and a BPM frame
/// a write must not touch; a FLAC album with ReplayGain and a back cover; an
/// m4a; and a real `cover.jpg` to embed.
fn world() -> Fixture {
    let fx = Fixture::builder()
        .album(MP3_ALBUM, &MP3S)
        .flac_album(FLAC_ALBUM)
        .track("coding-music/SwitchAngel/Coding_Trance_Reprise.m4a")
        .build();

    for name in MP3S {
        let mp3 = fx.abs(&format!("{MP3_ALBUM}/{name}"));
        fix::set_frame(&mp3, "TBPM", "92");
        fix::embed_picture(&mp3, &fix::jpeg(32, 32), PictureKind::FRONT.code());
        fix::embed_picture(&mp3, &fix::png(8, 16), PictureKind::BACK.code());
    }
    for flac in flacs(&fx) {
        fix::set_comment(&flac, "REPLAYGAIN_TRACK_GAIN", "-7.26 dB");
        fix::embed_picture(&flac, &fix::png(8, 16), PictureKind::BACK.code());
    }
    std::fs::write(
        fx.abs(&format!("{FLAC_ALBUM}/cover.jpg")),
        fix::jpeg(60, 40),
    )
    .unwrap();
    fx
}

/// Every FLAC in the FLAC album, absolute.
fn flacs(fx: &Fixture) -> Vec<camino::Utf8PathBuf> {
    let dir = fx.abs(FLAC_ALBUM);
    let mut found: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.ends_with(".flac").then(|| dir.join(name))
        })
        .collect();
    found.sort();
    found
}

/// The lines of one dump the other does not have, marked `-` and `+`.
fn tag_diff(before: &str, after: &str) -> Vec<String> {
    let (b, a): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    b.iter()
        .filter(|line| !a.contains(*line))
        .map(|line| format!("-{line}"))
        .chain(
            a.iter()
                .filter(|line| !b.contains(*line))
                .map(|line| format!("+{line}")),
        )
        .collect()
}

/// A fixture with what a preview needs scanned from it.
struct Txn {
    fx: Fixture,
    library: Library,
    index: PlaylistIndex,
    config: Config,
}

impl Txn {
    fn new(fx: Fixture) -> Self {
        Self {
            library: Library::scan(fx.music_dir()).unwrap(),
            index: PlaylistIndex::load(fx.playlist_dir()).0,
            config: fx.config(),
            fx,
        }
    }

    fn rescan(&mut self) {
        self.library = Library::scan(self.fx.music_dir()).unwrap();
    }

    fn effects(&self, plan: &Plan) -> Effects {
        plan.validate(&self.library, &self.index, &self.config)
    }

    fn commit(&mut self, plan: &Plan) -> Committed {
        let effects = self.effects(plan);
        assert!(
            effects.conflicts.is_empty(),
            "the test's own plan does not validate: {:?}",
            effects.conflicts
        );
        let committed = commit::commit_with(
            &Previewed {
                plan,
                library: &self.library,
                effects: &effects,
            },
            &self.config,
            &commit::Options::default(),
        )
        .expect("the test's own plan commits");
        self.rescan();
        committed
    }

    fn undo(&mut self, committed: &Committed) {
        let store = Store::at(self.fx.data_dir());
        let record = store.load(&committed.txid).unwrap();
        undo::undo(&store, &record, &self.config, &undo::Options::default())
            .unwrap_or_else(|err| panic!("{} should undo: {err}", committed.txid));
        self.rescan();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot::capture(self.fx.music_dir())
    }

    /// One `WriteTags` per audio file in `dir`, each carrying `delta`.
    fn album_edit(&self, dir: &str, delta: &TagDelta) -> Plan {
        let dir = DirPath::parse(dir).unwrap();
        Plan::of(
            self.library
                .files_in(&dir)
                .filter(|entry| entry.is_audio())
                .map(|entry| Operation::WriteTags {
                    target: entry.rel.clone(),
                    changes: delta.clone(),
                })
                .collect(),
        )
    }
}

fn front(abs: &Utf8Path) -> Option<art::Embedded> {
    art::embedded(abs)
        .unwrap()
        .into_iter()
        .find(|picture| picture.kind.is_front())
}

// ---------------------------------------------------------------------------
// Read.

#[test]
fn show_reports_each_embedded_pictures_type_mime_and_dimensions() {
    let fx = world();
    let pictures = art::embedded(&fx.abs(&format!("{MP3_ALBUM}/{}", MP3S[0]))).unwrap();
    let described: Vec<(PictureKind, ImageFormat, String)> = pictures
        .iter()
        .map(|p| (p.kind, p.image.format, p.image.dimensions()))
        .collect();
    assert_eq!(
        described,
        [
            (
                PictureKind::FRONT,
                ImageFormat::Jpeg,
                "32\u{d7}32".to_owned()
            ),
            (PictureKind::BACK, ImageFormat::Png, "8\u{d7}16".to_owned()),
        ]
    );
    assert!(pictures.iter().all(|p| p.image.size > 0));

    // The FLAC's picture is in its own PICTURE block, and is read from there.
    let flac = art::embedded(&flacs(&fx)[0]).unwrap();
    assert_eq!(flac.len(), 1);
    assert_eq!(flac[0].kind, PictureKind::BACK);
}

#[test]
fn loose_images_are_listed_best_first_and_judged_by_their_bytes() {
    let fx = world();
    let dir = fx.abs(FLAC_ALBUM);
    std::fs::write(dir.join("folder.jpg"), fix::png(4, 4)).unwrap(); // a PNG, misnamed
    std::fs::write(dir.join("scan-01.jpg"), b"not an image").unwrap();
    std::fs::write(dir.join("album.nfo"), b"release notes").unwrap();

    let loose = art::loose(&dir).unwrap();
    let names: Vec<&str> = loose.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["cover.jpg", "folder.jpg", "scan-01.jpg"]);
    assert_eq!(loose[0].image.format, ImageFormat::Jpeg);
    assert_eq!(loose[0].image.dimensions(), "60\u{d7}40");
    assert_eq!(
        loose[1].image.format,
        ImageFormat::Png,
        "the bytes, not the name"
    );
    assert!(!loose[2].is_image());
}

#[test]
fn an_album_reports_embedded_art_with_no_loose_file_and_the_reverse() {
    let fx = world();
    let library = Library::scan(fx.music_dir()).unwrap();

    // mp3s embed a front cover and the directory holds no image.
    let mp3s = art::album(&library, &DirPath::parse(MP3_ALBUM).unwrap()).unwrap();
    assert_eq!(mp3s.tracks.len(), 2);
    assert_eq!(mp3s.mismatches, [Mismatch::EmbeddedOnly]);

    // FLACs carry only a back cover, and cover.jpg sits beside them.
    let flac = art::album(&library, &DirPath::parse(FLAC_ALBUM).unwrap()).unwrap();
    assert_eq!(
        flac.mismatches,
        [Mismatch::LooseOnly {
            file: "cover.jpg".to_owned()
        }]
    );
}

// ---------------------------------------------------------------------------
// Set.

#[test]
fn set_embeds_into_every_track_and_preserves_every_other_tag_flac_and_mp3() {
    let mut txn = Txn::new(world());
    let (image, bytes) = CoverImage::load(&txn.fx.abs(&format!("{FLAC_ALBUM}/cover.jpg"))).unwrap();

    for dir in [FLAC_ALBUM, MP3_ALBUM] {
        let plan = txn.album_edit(dir, &TagDelta::new().set_cover(image.clone()));
        let targets: Vec<_> = plan
            .ops()
            .iter()
            .map(|op| txn.fx.abs(op.source().as_str()))
            .collect();
        let before: Vec<(String, u64)> = targets
            .iter()
            .map(|abs| (fix::dump(abs), fix::audio_digest(abs)))
            .collect();

        let effects = txn.effects(&plan);
        let preview = effects.render(100);
        assert!(preview.contains("cover = cover.jpg"), "{preview}");
        assert_eq!(effects.summary.tags_written, targets.len());
        txn.commit(&plan);

        for (abs, (dump, audio)) in targets.iter().zip(before) {
            let after = fix::dump(abs);
            let diff = tag_diff(&dump, &after);
            // Exactly one picture line in, at most one out (the old front
            // cover), and nothing else — the back cover, ReplayGain and TBPM
            // are all still there.
            assert!(
                diff.iter().all(|line| line.contains("@PICTURE")),
                "{abs}: {diff:#?}"
            );
            assert_eq!(
                diff.iter().filter(|l| l.starts_with('+')).count(),
                1,
                "{abs}"
            );
            assert_eq!(fix::audio_digest(abs), audio, "{abs}: the audio changed");

            let pictures = art::embedded(abs).unwrap();
            let fronts: Vec<_> = pictures.iter().filter(|p| p.kind.is_front()).collect();
            assert_eq!(fronts.len(), 1, "{abs}: one front cover, replacing the old");
            assert_eq!(fronts[0].image.hash, image.image.hash);
            assert!(
                pictures.iter().any(|p| p.kind == PictureKind::BACK),
                "{abs}"
            );
            let (_, embedded) = art::read::picture_data(abs, |p| p.kind.is_front())
                .unwrap()
                .unwrap();
            assert_eq!(embedded, bytes);
        }
    }
}

#[test]
fn a_cover_and_a_field_edit_in_one_delta_both_land() {
    let mut txn = Txn::new(world());
    let (image, _) = CoverImage::load(&txn.fx.abs(&format!("{FLAC_ALBUM}/cover.jpg"))).unwrap();
    let plan = txn.album_edit(
        MP3_ALBUM,
        &TagDelta::new()
            .set(Field::Genre, "Hip Hop")
            .set_cover(image),
    );
    let effects = txn.effects(&plan);
    assert_eq!(
        effects.summary.fields_changed, 4,
        "two fields on each of two files"
    );
    txn.commit(&plan);
    let abs = txn.fx.abs(&format!("{MP3_ALBUM}/{}", MP3S[0]));
    assert_eq!(
        tags::read_tags(&abs).unwrap().genre.first(),
        Some("Hip Hop")
    );
    assert_eq!(front(&abs).unwrap().image.dimensions(), "60\u{d7}40");
}

#[test]
fn an_m4a_takes_a_cover_too() {
    let mut txn = Txn::new(world());
    let (image, _) = CoverImage::load(&txn.fx.abs(&format!("{FLAC_ALBUM}/cover.jpg"))).unwrap();
    let rel = txn
        .fx
        .rel("coding-music/SwitchAngel/Coding_Trance_Reprise.m4a");
    let abs = txn.fx.abs(rel.as_str());
    let audio = fix::audio_digest(&abs);
    txn.commit(&Plan::of(vec![Operation::WriteTags {
        target: rel,
        changes: TagDelta::new().set_cover(image.clone()),
    }]));
    assert_eq!(front(&abs).unwrap().image.hash, image.image.hash);
    assert_eq!(fix::audio_digest(&abs), audio);
}

#[test]
fn an_image_that_changed_since_the_preview_is_refused_and_nothing_is_written() {
    let txn = Txn::new(world());
    let source = txn.fx.abs(&format!("{FLAC_ALBUM}/cover.jpg"));
    let (image, _) = CoverImage::load(&source).unwrap();
    let plan = txn.album_edit(MP3_ALBUM, &TagDelta::new().set_cover(image));
    let effects = txn.effects(&plan);
    assert!(effects.conflicts.is_empty());

    // Same length, different bytes: only the hash can tell.
    let mut changed = std::fs::read(&source).unwrap();
    let last = changed.len() - 3;
    changed[last] ^= 0xff;
    std::fs::write(&source, changed).unwrap();

    let before = txn.snapshot();
    let result = commit::commit_with(
        &Previewed {
            plan: &plan,
            library: &txn.library,
            effects: &effects,
        },
        &txn.config,
        &commit::Options::default(),
    );
    let err = result.expect_err("a different image must not be embedded");
    assert!(
        err.to_string().contains("changed since the preview"),
        "{err}"
    );
    // A failed commit rolls back: every track is as it was.
    assert!(
        txn.snapshot().diff(&before).is_empty(),
        "{:?}",
        txn.snapshot().diff(&before)
    );
}

#[test]
fn only_jpeg_and_png_are_embedded() {
    let fx = world();
    let gif = fx.abs("not-a-cover.gif");
    std::fs::write(&gif, b"GIF89a\x01\x00\x01\x00").unwrap();
    let err = CoverImage::load(&gif).expect_err("a GIF is refused");
    assert!(err.to_string().contains("only JPEG and PNG"), "{err}");
}

#[test]
fn a_five_megabyte_png_embeds_into_a_flac() {
    let mut txn = Txn::new(world());
    let big = txn.fx.data_dir().join("big.png");
    let bytes = fix::png(1300, 1300);
    assert!(bytes.len() > 5_000_000, "{} bytes", bytes.len());
    std::fs::write(&big, &bytes).unwrap();
    let (image, _) = CoverImage::load(&big).unwrap();

    let flac = flacs(&txn.fx)[0].clone();
    let rel = mpdfm_core::paths::RelPath::from_abs(&flac, txn.fx.music_dir()).unwrap();
    txn.commit(&Plan::of(vec![Operation::WriteTags {
        target: rel,
        changes: TagDelta::new().set_cover(image.clone()),
    }]));
    let (_, embedded) = art::read::picture_data(&flac, |p| p.kind.is_front())
        .unwrap()
        .unwrap();
    assert_eq!(embedded, bytes);
}

// ---------------------------------------------------------------------------
// Remove.

#[test]
fn remove_takes_out_the_front_cover_and_leaves_the_other_pictures() {
    let mut txn = Txn::new(world());
    let plan = txn.album_edit(MP3_ALBUM, &TagDelta::new().remove_cover());
    txn.commit(&plan);
    for name in MP3S {
        let pictures = art::embedded(&txn.fx.abs(&format!("{MP3_ALBUM}/{name}"))).unwrap();
        let kinds: Vec<PictureKind> = pictures.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, [PictureKind::BACK], "{name}");
    }
}

#[test]
fn remove_on_a_flac_finds_a_front_cover_in_the_comment_block_too() {
    let mut txn = Txn::new(world());
    let flac = flacs(&txn.fx)[0].clone();
    // `embed_cover` puts it in the Vorbis comment block, the other place a
    // FLAC keeps pictures.
    fix::embed_cover(&flac, fix::COVER_PNG);
    txn.rescan();
    assert!(front(&flac).is_some());
    let rel = mpdfm_core::paths::RelPath::from_abs(&flac, txn.fx.music_dir()).unwrap();
    txn.commit(&Plan::of(vec![Operation::WriteTags {
        target: rel,
        changes: TagDelta::new().remove_cover(),
    }]));
    assert!(front(&flac).is_none());
    assert_eq!(
        art::embedded(&flac).unwrap().len(),
        1,
        "the back cover stays"
    );
}

// ---------------------------------------------------------------------------
// Extract.

#[test]
fn extract_writes_a_byte_identical_copy_from_an_mp3_and_a_flac() {
    let mut txn = Txn::new(world());
    // Give the FLAC a front cover first, through the same machinery.
    let (image, cover) = CoverImage::load(&txn.fx.abs(&format!("{FLAC_ALBUM}/cover.jpg"))).unwrap();
    let plan = txn.album_edit(FLAC_ALBUM, &TagDelta::new().set_cover(image));
    txn.commit(&plan);

    for (track, to) in [
        (
            format!("{MP3_ALBUM}/{}", MP3S[0]),
            format!("{MP3_ALBUM}/cover.jpg"),
        ),
        (
            mpdfm_core::paths::RelPath::from_abs(&flacs(&txn.fx)[0], txn.fx.music_dir())
                .unwrap()
                .to_string(),
            format!("{FLAC_ALBUM}/front.jpg"),
        ),
    ] {
        let abs = txn.fx.abs(&track);
        let (picture, bytes) = art::read::picture_data(&abs, |p| p.kind.is_front())
            .unwrap()
            .unwrap();
        let plan = Plan::of(vec![Operation::ExtractArt {
            from: txn.fx.rel(&track),
            to: txn.fx.rel(&to),
            picture: picture.image,
        }]);
        let effects = txn.effects(&plan);
        assert_eq!(effects.summary.files_created, 1);
        assert!(
            effects.render(100).contains("COVER"),
            "{}",
            effects.render(100)
        );
        let committed = txn.commit(&plan);
        assert!(matches!(
            committed.record.steps[0].receipt.as_ref().map(|r| &r.done),
            Some(Done::Created { .. })
        ));
        assert_eq!(std::fs::read(txn.fx.abs(&to)).unwrap(), bytes, "{to}");
    }
    assert_eq!(
        std::fs::read(txn.fx.abs(&format!("{FLAC_ALBUM}/front.jpg"))).unwrap(),
        cover
    );
}

#[test]
fn extract_never_overwrites_an_image_that_is_already_there() {
    let txn = Txn::new(world());
    let track = format!("{MP3_ALBUM}/{}", MP3S[0]);
    let picture = front(&txn.fx.abs(&track)).unwrap();
    std::fs::write(txn.fx.abs(&format!("{MP3_ALBUM}/cover.jpg")), b"mine").unwrap();
    let mut txn = txn;
    txn.rescan();
    let plan = Plan::of(vec![Operation::ExtractArt {
        from: txn.fx.rel(&track),
        to: txn.fx.rel(&format!("{MP3_ALBUM}/cover.jpg")),
        picture: picture.image,
    }]);
    let effects = txn.effects(&plan);
    assert!(
        effects
            .conflicts
            .iter()
            .any(|c| matches!(c, Conflict::DestinationExists { .. })),
        "{:?}",
        effects.conflicts
    );
}

// ---------------------------------------------------------------------------
// Undo.

#[test]
fn every_cover_operation_is_undone_byte_for_byte() {
    let mut txn = Txn::new(world());
    let pristine = txn.snapshot();
    let (image, _) = CoverImage::load(&txn.fx.abs(&format!("{FLAC_ALBUM}/cover.jpg"))).unwrap();
    let track = format!("{MP3_ALBUM}/{}", MP3S[0]);
    let picture = front(&txn.fx.abs(&track)).unwrap();

    let plans = [
        txn.album_edit(MP3_ALBUM, &TagDelta::new().set_cover(image.clone())),
        txn.album_edit(FLAC_ALBUM, &TagDelta::new().set_cover(image)),
        txn.album_edit(MP3_ALBUM, &TagDelta::new().remove_cover()),
        Plan::of(vec![Operation::ExtractArt {
            from: txn.fx.rel(&track),
            to: txn.fx.rel(&format!("{MP3_ALBUM}/cover.jpg")),
            picture: picture.image,
        }]),
    ];
    for plan in plans {
        let before = txn.snapshot();
        let committed = txn.commit(&plan);
        assert!(
            !txn.snapshot().diff(&before).is_empty(),
            "the commit changed something"
        );
        txn.undo(&committed);
        let diff = txn.snapshot().diff(&before);
        assert!(diff.is_empty(), "undo of {} left: {diff:?}", committed.txid);
    }
    assert!(txn.snapshot().diff(&pristine).is_empty());
}

#[test]
fn undo_leaves_an_extracted_cover_alone_once_the_user_has_changed_it() {
    let mut txn = Txn::new(world());
    let track = format!("{MP3_ALBUM}/{}", MP3S[0]);
    let to = format!("{MP3_ALBUM}/cover.jpg");
    let picture = front(&txn.fx.abs(&track)).unwrap();
    let committed = txn.commit(&Plan::of(vec![Operation::ExtractArt {
        from: txn.fx.rel(&track),
        to: txn.fx.rel(&to),
        picture: picture.image,
    }]));
    std::fs::write(txn.fx.abs(&to), fix::jpeg(5, 5)).unwrap();

    let store = Store::at(txn.fx.data_dir());
    let record = store.load(&committed.txid).unwrap();
    assert!(undo::undo(&store, &record, &txn.config, &undo::Options::default()).is_err());
    assert!(txn.fx.abs(&to).exists(), "the user's file is still there");
}

#[test]
fn an_id3v23_file_takes_a_cover_and_stays_id3v23() {
    let mut txn = Txn::new(world());
    let rel = format!("{MP3_ALBUM}/{}", MP3S[1]);
    let abs = txn.fx.abs(&rel);
    fix::write_as(&abs, mpdfm_core::testing::AudioTemplate::Mp3v23);
    assert!(
        fix::dump(&abs).contains("@VERSION V3"),
        "the fixture is v2.3"
    );
    txn.rescan();
    let (image, bytes) = CoverImage::load(&txn.fx.abs(&format!("{FLAC_ALBUM}/cover.jpg"))).unwrap();
    txn.commit(&Plan::of(vec![Operation::WriteTags {
        target: txn.fx.rel(&rel),
        changes: TagDelta::new().set_cover(image),
    }]));
    assert!(
        fix::dump(&abs).contains("@VERSION V3"),
        "{}",
        fix::dump(&abs)
    );
    let (_, embedded) = art::read::picture_data(&abs, |p| p.kind.is_front())
        .unwrap()
        .unwrap();
    assert_eq!(embedded, bytes);
}
