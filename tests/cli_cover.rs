//! Task 30 — `mpdfm cover show | set | extract | remove`, driven through the
//! built binary against a fixture, and undone with `mpdfm undo`.

#![cfg(unix)]

mod harness;

use harness::World;
use mpdfm_core::art::{self, PictureKind};
use mpdfm_core::tags;
use mpdfm_core::testing::{Fixture, tags as fix};

const MP3_ALBUM: &str = "hiphop/album";
const FLAC_ALBUM: &str = "jazz/album";

/// An mp3 album embedding a front and a back cover, with no loose image; a FLAC
/// album with a real `cover.jpg` beside it and no embedded art.
fn world() -> World {
    let fx = Fixture::builder()
        .album(MP3_ALBUM, &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"])
        .flac_album(FLAC_ALBUM)
        .build();
    for name in ["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"] {
        let abs = fx.abs(&format!("{MP3_ALBUM}/{name}"));
        fix::set_frame(&abs, "TBPM", "174");
        fix::embed_picture(&abs, &fix::jpeg(32, 32), PictureKind::FRONT.code());
        fix::embed_picture(&abs, &fix::png(8, 8), PictureKind::BACK.code());
    }
    std::fs::write(
        fx.abs(&format!("{FLAC_ALBUM}/cover.jpg")),
        fix::jpeg(60, 40),
    )
    .unwrap();
    World::new(fx)
}

fn fronts(world: &World, rel: &str) -> Vec<u64> {
    art::embedded(&world.abs(rel))
        .unwrap()
        .into_iter()
        .filter(|p| p.kind.is_front())
        .map(|p| p.image.hash)
        .collect()
}

fn flac_tracks(world: &World) -> Vec<String> {
    let dir = world.abs(FLAC_ALBUM);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".flac"))
        .map(|n| format!("{FLAC_ALBUM}/{n}"))
        .collect();
    names.sort();
    names
}

#[test]
fn show_reports_type_mime_dimensions_and_loose_files() {
    let world = world();
    world.assert_hermetic();

    let run = world.run(&["cover", "show", MP3_ALBUM]);
    run.assert_code(0)
        .assert_stdout("2 tracks")
        .assert_stdout("front cover")
        .assert_stdout("image/jpeg 32\u{d7}32")
        .assert_stdout("back cover")
        .assert_stdout("image/png 8\u{d7}8")
        .assert_stdout("(no image files)")
        .assert_stdout("no image file beside them");

    let flac = world.run(&["cover", "show", FLAC_ALBUM]);
    flac.assert_code(0)
        .assert_stdout("cover.jpg")
        .assert_stdout("image/jpeg 60\u{d7}40")
        .assert_stdout("no track embeds a cover");

    let json = world.json(&["cover", "show", &format!("{MP3_ALBUM}/01 Beef Rap.mp3")]);
    let pictures = &json["tracks"][0]["pictures"];
    assert_eq!(pictures[0]["kind"], 3);
    assert_eq!(pictures[0]["image"]["format"], "jpeg");
    assert_eq!(pictures[0]["image"]["width"], 32);
}

#[test]
fn set_embeds_into_every_track_keeps_other_tags_and_is_undone() {
    let world = world();
    let before = world.snapshot();
    let tracks = flac_tracks(&world);
    let dumps: Vec<String> = tracks.iter().map(|t| fix::dump(&world.abs(t))).collect();

    // No IMAGE: the album's own cover.jpg is used.
    let run = world.run(&["cover", "set", FLAC_ALBUM, "--yes"]);
    run.assert_code(0)
        .assert_stdout("cover = cover.jpg")
        .assert_stdout(&format!("into {} files adds", tracks.len()))
        .assert_stdout("mpdfm undo");

    let cover = std::fs::read(world.abs(&format!("{FLAC_ALBUM}/cover.jpg"))).unwrap();
    let hash = mpdfm_core::ops::exec_fs::hash_bytes(&cover);
    for (track, dump) in tracks.iter().zip(&dumps) {
        assert_eq!(fronts(&world, track), [hash], "{track}");
        let after = fix::dump(&world.abs(track));
        // Every line that was there is still there: the cover was added and
        // nothing else changed.
        for line in dump.lines() {
            assert!(after.contains(line), "{track} lost {line:?}");
        }
        assert_eq!(after.lines().count(), dump.lines().count() + 1, "{track}");
    }

    world.run(&["undo", "--yes"]).assert_code(0);
    world.snapshot().assert_same(&before);
}

#[test]
fn set_with_a_path_replaces_the_front_cover_and_keeps_the_back_cover() {
    let world = world();
    let image = world.abs(&format!("{FLAC_ALBUM}/cover.jpg"));
    world
        .run(&["cover", "set", MP3_ALBUM, image.as_str(), "--yes"])
        .assert_code(0);
    let rel = format!("{MP3_ALBUM}/01 Beef Rap.mp3");
    let kinds: Vec<PictureKind> = art::embedded(&world.abs(&rel))
        .unwrap()
        .iter()
        .map(|p| p.kind)
        .collect();
    assert_eq!(kinds.iter().filter(|k| k.is_front()).count(), 1);
    assert!(kinds.contains(&PictureKind::BACK));
    assert_eq!(
        tags::read_tags(&world.abs(&rel))
            .unwrap()
            .extra
            .iter()
            .find(|(k, _)| k == "TBPM")
            .map(|(_, v)| v.as_str()),
        Some("174")
    );
}

#[test]
fn set_warns_about_a_large_cover_and_downscales_only_when_asked() {
    let world = world();
    let big = world.abs(&format!("{FLAC_ALBUM}/scan.png"));
    std::fs::write(&big, fix::png(1300, 1300)).unwrap();

    let dry = world.run(&["cover", "set", FLAC_ALBUM, "scan.png", "--dry-run"]);
    dry.assert_code(0)
        .assert_stdout("over 1 MB")
        .assert_stdout("--max-dimension")
        .assert_stdout("nothing was changed");

    world
        .run(&[
            "cover",
            "set",
            FLAC_ALBUM,
            "scan.png",
            "--max-dimension",
            "500",
            "--yes",
        ])
        .assert_code(0)
        .assert_stdout("downscaled");
    let track = &flac_tracks(&world)[0];
    let front = art::embedded(&world.abs(track))
        .unwrap()
        .into_iter()
        .find(|p| p.kind.is_front())
        .unwrap();
    assert_eq!(
        (front.image.width, front.image.height),
        (Some(500), Some(500)),
        "square stays square"
    );
    assert_eq!(front.image.format, art::ImageFormat::Jpeg);
}

#[test]
fn extract_writes_a_byte_identical_cover_and_undo_removes_it() {
    let world = world();
    let before = world.snapshot();
    let track = world.abs(&format!("{MP3_ALBUM}/01 Beef Rap.mp3"));
    let (_, embedded) = art::read::picture_data(&track, |p| p.kind.is_front())
        .unwrap()
        .unwrap();

    world
        .run(&["cover", "extract", MP3_ALBUM, "--yes"])
        .assert_code(0)
        .assert_stdout("COVER");
    let written = world.abs(&format!("{MP3_ALBUM}/cover.jpg"));
    assert_eq!(std::fs::read(&written).unwrap(), embedded);

    // Again: already there, nothing to do.
    world
        .run(&["cover", "extract", MP3_ALBUM, "--yes"])
        .assert_code(0)
        .assert_stdout("already holds");

    world.run(&["undo", "--yes"]).assert_code(0);
    assert!(!written.exists());
    world.snapshot().assert_same(&before);
}

#[test]
fn extract_refuses_to_overwrite_a_different_image() {
    let world = world();
    let target = world.abs(&format!("{MP3_ALBUM}/front.jpg"));
    std::fs::write(&target, fix::jpeg(7, 7)).unwrap();
    let before = world.snapshot();
    world
        .run(&[
            "cover",
            "extract",
            MP3_ALBUM,
            "--name",
            "front.jpg",
            "--yes",
        ])
        .assert_code(2);
    world.snapshot().assert_same(&before);
}

#[test]
fn remove_takes_only_the_front_cover_and_is_undone() {
    let world = world();
    let before = world.snapshot();
    world
        .run(&["cover", "remove", MP3_ALBUM, "--yes"])
        .assert_code(0)
        .assert_stdout("cover = <removed>");
    for name in ["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"] {
        let kinds: Vec<PictureKind> = art::embedded(&world.abs(&format!("{MP3_ALBUM}/{name}")))
            .unwrap()
            .iter()
            .map(|p| p.kind)
            .collect();
        assert_eq!(kinds, [PictureKind::BACK], "{name}");
    }
    world.run(&["undo", "--yes"]).assert_code(0);
    world.snapshot().assert_same(&before);

    // An album with no front cover has nothing to remove.
    world
        .run(&["cover", "remove", FLAC_ALBUM, "--yes"])
        .assert_code(0)
        .assert_stdout("No track");
}

#[test]
fn a_write_without_yes_on_a_pipe_asks_and_is_refused() {
    let world = world();
    let before = world.snapshot();
    let run = world.run(&["cover", "remove", MP3_ALBUM]);
    assert_ne!(run.code, 0);
    world.snapshot().assert_same(&before);
}
