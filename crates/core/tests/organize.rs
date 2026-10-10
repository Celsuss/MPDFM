//! The organize template engine's acceptance tests (task 27) that need a whole
//! library: aux files, multi-disc sets, collisions, splits and case.
//!
//! Token, modifier and sanitization tests live next to the code, in
//! `organize::template` and `organize::sanitize`. These run the mapping over the
//! fixture library — real album names from this library, real clutter — with
//! tags supplied directly, because the engine is pure: it never reads a tag
//! itself, and the fixture's generated audio carries none worth placing.

use std::collections::BTreeMap;

use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::organize::{
    self, Conflict, Mapping, Options, SplitAux, Template, Token, Unplaceable, Warning,
};
use mpdfm_core::paths::RelPath;
use mpdfm_core::tags::{TagSet, Values};
use mpdfm_core::testing::{Fixture, names};

const DEFAULT: &str = mpdfm_core::config::DEFAULT_ORGANIZE_TEMPLATE;

fn scan(fx: &Fixture) -> Library {
    Library::scan(fx.music_dir()).expect("the fixture scans")
}

fn run(template: &str, library: &Library, tracks: &BTreeMap<RelPath, TagSet>) -> Mapping {
    run_with(template, library, tracks, Options::default())
}

fn run_with(
    template: &str,
    library: &Library,
    tracks: &BTreeMap<RelPath, TagSet>,
    options: Options,
) -> Mapping {
    let template = Template::parse(template).expect("the template parses");
    organize::map(&template, library, tracks, &options)
}

/// Tags for every audio file directly in `dir`, numbered in name order.
fn tag_dir(
    library: &Library,
    dir: &str,
    tags: impl Fn(u32, &str) -> TagSet,
) -> BTreeMap<RelPath, TagSet> {
    let dir = DirPath::parse(dir).unwrap();
    library
        .files_in(&dir)
        .filter(|e| e.is_audio())
        .zip(1..)
        .map(|(e, n)| (e.rel.clone(), tags(n, e.file_name())))
        .collect()
}

fn mf_doom(n: u32, name: &str) -> TagSet {
    TagSet {
        title: Values::one(name.trim_end_matches(".mp3")),
        artist: Values::one("MF DOOM"),
        album: Values::one("Mm..Food"),
        date: Values::one("2004"),
        genre: Values::one("Hip Hop/Rap"),
        track: Some((n, Some(15))),
        ..TagSet::default()
    }
}

fn snoop(n: u32, name: &str) -> TagSet {
    TagSet {
        title: Values::one(name.split_once('.').unwrap().1.trim_end_matches(".mp3")),
        artist: Values::of(["Snoop Dogg", "Wiz Khalifa"]),
        album_artist: Values::one("Snoop Dogg & Wiz Khalifa"),
        album: Values::one("Mac + Devin Go To High School (Soundtrack)"),
        date: Values::one("2011-12-13"),
        genre: Values::one("Hip-Hop"),
        track: Some((n, Some(18))),
        ..TagSet::default()
    }
}

fn dest<'a>(mapping: &'a Mapping, from: &str) -> Option<&'a str> {
    mapping
        .destination(&RelPath::parse(from).unwrap())
        .map(RelPath::as_str)
}

fn strs(paths: &[RelPath]) -> Vec<&str> {
    paths.iter().map(RelPath::as_str).collect()
}

// ---------------------------------------------------------------------------

#[test]
fn the_snoop_soundtrack_is_organized_by_the_default_template() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    let tracks = tag_dir(&library, names::SNOOP_ALBUM, snoop);

    let mapping = run(DEFAULT, &library, &tracks);

    assert!(mapping.is_committable(), "{:?}", mapping.conflicts);
    assert_eq!(
        dest(&mapping, names::SNOOP_TRACK),
        Some(
            "Hip-Hop/Snoop Dogg & Wiz Khalifa/2011 - Mac + Devin Go To High School \
             (Soundtrack)/01 Smokin' On.mp3"
        )
    );
    assert_eq!(
        dest(
            &mapping,
            &format!("{}/02.Young, Wild & Free.mp3", names::SNOOP_ALBUM)
        ),
        Some(
            "Hip-Hop/Snoop Dogg & Wiz Khalifa/2011 - Mac + Devin Go To High School \
             (Soundtrack)/02 Young, Wild & Free.mp3"
        )
    );
    assert!(mapping.warnings.is_empty(), "{:?}", mapping.warnings);
}

#[test]
fn a_slash_in_the_genre_never_adds_a_directory_level() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    let tracks = tag_dir(&library, names::MF_DOOM_ALBUM, mf_doom);

    let mapping = run(DEFAULT, &library, &tracks);

    for m in mapping.moves.iter().filter(|m| !m.aux) {
        assert_eq!(m.to.components().count(), 4, "{}", m.to);
        assert!(
            m.to.as_str()
                .starts_with("Hip Hop_Rap/MF DOOM/2004 - Mm..Food/")
        );
    }
}

#[test]
fn a_file_missing_its_album_is_unplaceable_and_not_moved() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    let mut tracks = tag_dir(&library, names::SNOOP_ALBUM, snoop);
    let first = fx.rel(names::SNOOP_TRACK);
    tracks.get_mut(&first).unwrap().album = Values::none();

    let mapping = run(DEFAULT, &library, &tracks);

    assert_eq!(dest(&mapping, names::SNOOP_TRACK), None);
    assert!(!mapping.in_place.contains(&first));
    let unplaceable: Vec<_> = mapping.unplaceable().collect();
    assert_eq!(
        unplaceable,
        [(&first, &Unplaceable::Missing(vec![Token::Album]))]
    );
    // Its sibling still moves, and the album is reported as split rather than
    // half-moved in silence.
    assert!(mapping.warnings.iter().any(
        |w| matches!(w, Warning::AlbumSplit { dir, .. } if dir.as_str() == names::SNOOP_ALBUM)
    ));
}

#[test]
fn an_explicit_default_places_what_would_otherwise_be_unplaceable() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    let tracks = tag_dir(&library, names::SNOOP_ALBUM, |n, name| TagSet {
        album: Values::none(),
        ..snoop(n, name)
    });

    let mapping = run(
        "{albumartist}/{album|Singles}/{track:02} {title}",
        &library,
        &tracks,
    );

    assert_eq!(
        dest(&mapping, names::SNOOP_TRACK),
        Some("Snoop Dogg & Wiz Khalifa/Singles/01 Smokin' On.mp3")
    );
    assert_eq!(mapping.unplaceable().count(), 0);
}

#[test]
fn the_multi_disc_set_gets_distinct_ordered_paths_for_both_discs() {
    let fx = Fixture::builder()
        .multi_disc(
            names::MERCURY_ALBUM,
            &["CD 1 - Mercury - Acts 1", "CD 2 - Mercury - Acts 2"],
        )
        .cue_reference(
            names::MERCURY_CD1,
            "Imagine Dragons - Mercury - Acts 1.flac.cue/track0017",
        )
        .aux(names::MERCURY_ALBUM, &["folder.jpg"])
        .build();
    let library = scan(&fx);
    let mercury = |disc: u32| {
        move |n: u32, name: &str| TagSet {
            title: Values::one(name.split_once(' ').unwrap().1.rsplit_once('.').unwrap().0),
            album_artist: Values::one("Imagine Dragons"),
            album: Values::one("Mercury - Acts 1 & 2"),
            date: Values::one("2022"),
            genre: Values::one("Pop"),
            track: Some((n, None)),
            disc: Some((disc, Some(2))),
            ..TagSet::default()
        }
    };
    let mut tracks = tag_dir(&library, names::MERCURY_CD1, mercury(1));
    tracks.extend(tag_dir(&library, names::MERCURY_CD2, mercury(2)));

    let mapping = run(DEFAULT, &library, &tracks);
    assert!(mapping.is_committable(), "{:?}", mapping.conflicts);
    assert!(mapping.warnings.is_empty(), "{:?}", mapping.warnings);

    let album = "Pop/Imagine Dragons/2022 - Mercury - Acts 1 & 2";
    let audio: Vec<&str> = mapping
        .moves
        .iter()
        .filter(|m| !m.aux)
        .map(|m| m.to.as_str())
        .collect();
    let mut sorted = audio.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), audio.len(), "destinations are distinct");
    let disc1: Vec<_> = sorted.iter().filter(|p| p.contains("/Disc 1/")).collect();
    let disc2: Vec<_> = sorted.iter().filter(|p| p.contains("/Disc 2/")).collect();
    assert_eq!(disc1.len(), 3); // two mp3s and the cue sheet's flac
    assert_eq!(disc2.len(), 2);
    // Byte order is play order: every disc 1 track sorts before every disc 2.
    assert!(disc1.iter().all(|a| disc2.iter().all(|b| a < b)));
    assert_eq!(
        dest(&mapping, names::MERCURY_TRACK),
        Some(&*format!("{album}/Disc 1/01 Wrecked.mp3"))
    );

    // The cue sheet follows its disc, and the set's own cover follows the set.
    assert_eq!(
        dest(&mapping, names::MERCURY_CUE),
        Some(&*format!(
            "{album}/Disc 1/Imagine Dragons - Mercury - Acts 1.flac.cue"
        ))
    );
    assert_eq!(
        dest(&mapping, &format!("{}/folder.jpg", names::MERCURY_ALBUM)),
        Some(&*format!("{album}/folder.jpg"))
    );
}

#[test]
fn two_files_on_one_destination_are_a_conflict_naming_both() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    // Both Snoop tracks tagged as the same track.
    let tracks = tag_dir(&library, names::SNOOP_ALBUM, |_, name| snoop(1, name))
        .into_iter()
        .map(|(rel, tags)| {
            (
                rel,
                TagSet {
                    title: Values::one("Smokin' On"),
                    ..tags
                },
            )
        })
        .collect();

    let mapping = run(DEFAULT, &library, &tracks);

    assert!(!mapping.is_committable());
    let [Conflict::Collision { to, sources }] = mapping.conflicts.as_slice() else {
        panic!("{:?}", mapping.conflicts);
    };
    assert!(to.as_str().ends_with("/01 Smokin' On.mp3"));
    assert_eq!(
        strs(sources),
        [
            names::SNOOP_TRACK.to_owned(),
            format!("{}/02.Young, Wild & Free.mp3", names::SNOOP_ALBUM)
        ]
    );
    let message = mapping.conflicts[0].to_string();
    assert!(message.contains("01.Smokin' On.mp3") && message.contains("02.Young, Wild & Free.mp3"));
    // Neither moves, and nothing is suffixed.
    assert!(mapping.moves.iter().all(|m| !m.to.as_str().contains("(1)")));
    assert!(mapping.destination(&sources[0]).is_none());
    assert!(mapping.destination(&sources[1]).is_none());
}

#[test]
fn a_destination_taken_by_a_file_that_stays_is_a_conflict() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    // Put the Snoop track exactly where an MF DOOM track already is.
    let tracks = BTreeMap::from([(fx.rel(names::SNOOP_TRACK), snoop(1, "01.Smokin' On.mp3"))]);

    let mapping = run("{original_dir}/{filename}", &library, &tracks);
    assert_eq!(mapping.in_place, [fx.rel(names::SNOOP_TRACK)]);

    let template = format!("{}/01 Beef Rap", names::MF_DOOM_ALBUM);
    let mapping = run(&template, &library, &tracks);
    assert_eq!(
        mapping.conflicts,
        [Conflict::Occupied {
            from: fx.rel(names::SNOOP_TRACK),
            to: fx.rel(names::MF_DOOM_TRACK),
            existing: names::MF_DOOM_TRACK.to_owned(),
        }]
    );
    assert!(mapping.moves.is_empty());
}

#[test]
fn aux_files_follow_a_wholly_moved_album() {
    let fx = Fixture::builder()
        .album(
            names::MF_DOOM_ALBUM,
            &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"],
        )
        .aux(
            names::MF_DOOM_ALBUM,
            &[
                "folder.jpg",
                "info.nfo",
                "mm..food.sfv",
                "eac.log",
                "Mm..Food.m3u",
            ],
        )
        .aux(&format!("{}/Scans", names::MF_DOOM_ALBUM), &["back.jpg"])
        .build();
    let library = scan(&fx);
    let tracks = tag_dir(&library, names::MF_DOOM_ALBUM, mf_doom);

    let mapping = run(
        "{albumartist}/{album}/{track:02} {title}",
        &library,
        &tracks,
    );

    let aux: BTreeMap<&str, &str> = mapping
        .moves
        .iter()
        .filter(|m| m.aux)
        .map(|m| {
            (
                m.from.as_str().strip_prefix(names::MF_DOOM_ALBUM).unwrap(),
                m.to.as_str(),
            )
        })
        .collect();
    assert_eq!(
        aux,
        BTreeMap::from([
            ("/Mm..Food.m3u", "MF DOOM/Mm..Food/Mm..Food.m3u"),
            ("/Scans/back.jpg", "MF DOOM/Mm..Food/Scans/back.jpg"),
            ("/eac.log", "MF DOOM/Mm..Food/eac.log"),
            ("/folder.jpg", "MF DOOM/Mm..Food/folder.jpg"),
            ("/info.nfo", "MF DOOM/Mm..Food/info.nfo"),
            ("/mm..food.sfv", "MF DOOM/Mm..Food/mm..food.sfv"),
        ])
    );
    assert!(mapping.warnings.is_empty(), "{:?}", mapping.warnings);
}

#[test]
fn an_album_split_across_destinations_warns_and_leaves_its_aux_files() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    // One track tagged as a different album: the directory is being split.
    let tracks = tag_dir(&library, names::MF_DOOM_ALBUM, |n, name| TagSet {
        album: Values::one(if n == 3 { "Special Herbs" } else { "Mm..Food" }),
        ..mf_doom(n, name)
    });
    let template = "{albumartist}/{album}/{track:02} {title}";

    let mapping = run(template, &library, &tracks);

    assert_eq!(mapping.moves.iter().filter(|m| m.aux).count(), 0);
    assert_eq!(mapping.moves.len(), 3, "the tracks themselves still move");
    let [
        Warning::AlbumSplit {
            dir,
            destinations,
            aux,
            aux_moved,
        },
    ] = mapping.warnings.as_slice()
    else {
        panic!("{:?}", mapping.warnings);
    };
    assert_eq!(dir.as_str(), names::MF_DOOM_ALBUM);
    assert_eq!(
        destinations.iter().map(DirPath::as_str).collect::<Vec<_>>(),
        ["MF DOOM/Mm..Food", "MF DOOM/Special Herbs"]
    );
    assert_eq!(aux.len(), 5);
    assert!(!aux_moved);

    // Confirmed, the aux files go with the larger half.
    let confirmed = run_with(
        template,
        &library,
        &tracks,
        Options {
            split_aux: SplitAux::FollowMajority,
            ..Options::default()
        },
    );
    assert_eq!(
        dest(&confirmed, &format!("{}/folder.jpg", names::MF_DOOM_ALBUM)),
        Some("MF DOOM/Mm..Food/folder.jpg")
    );
}

#[test]
fn a_partly_selected_album_is_a_split_too() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    let mut tracks = tag_dir(&library, names::MF_DOOM_ALBUM, mf_doom);
    tracks.remove(&fx.rel(names::MF_DOOM_TRACK));

    let mapping = run(
        "{albumartist}/{album}/{track:02} {title}",
        &library,
        &tracks,
    );

    assert!(mapping.moves.iter().all(|m| !m.aux));
    let Some(Warning::AlbumSplit { destinations, .. }) = mapping.warnings.first() else {
        panic!("{:?}", mapping.warnings);
    };
    // Staying behind is one of the destinations.
    assert!(
        destinations
            .iter()
            .any(|d| d.as_str() == names::MF_DOOM_ALBUM)
    );
}

#[test]
fn case_only_destination_differences_are_detected() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    // `HIPHOP/` next to the existing `hiphop/`, which keeps MF DOOM.
    let tracks = tag_dir(&library, names::SNOOP_ALBUM, snoop);

    let mapping = run("{genre:upper}/{albumartist}/{title}", &library, &tracks);
    assert!(
        mapping.warnings.is_empty(),
        "HIP-HOP and hiphop differ by more than case: {:?}",
        mapping.warnings
    );

    let mapping = run("HIPHOP/{albumartist}/{title}", &library, &tracks);
    let cases: Vec<_> = mapping
        .warnings
        .iter()
        .filter_map(|w| match w {
            Warning::CaseDifference { at, other } => Some((at.as_str(), other.as_str())),
            _ => None,
        })
        .collect();
    // One warning for the directory, not one per track below it.
    assert_eq!(cases, [("HIPHOP", "hiphop")]);

    // Two new destinations differing only by case are caught as well.
    let mut tracks = tracks;
    tracks
        .get_mut(&fx.rel(names::SNOOP_TRACK))
        .unwrap()
        .album_artist = Values::one("SNOOP DOGG & WIZ KHALIFA");
    let mapping = run("rap/{albumartist}/{title}", &library, &tracks);
    let cases: Vec<_> = mapping
        .warnings
        .iter()
        .filter(|w| matches!(w, Warning::CaseDifference { .. }))
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        cases,
        ["rap/SNOOP DOGG & WIZ KHALIFA differs from rap/Snoop Dogg & Wiz Khalifa only by case"]
    );
}

#[test]
fn a_template_outside_the_music_directory_is_rejected() {
    for template in [
        "../{title}",
        "/{title}",
        "{genre}/../../{title}",
        "./{title}",
    ] {
        assert!(Template::parse(template).is_err(), "{template}");
    }
    let err: mpdfm_core::Error = Template::parse("../{title}").unwrap_err().into();
    assert!(
        err.to_string().contains("leave the music directory"),
        "{err}"
    );
}

#[test]
fn non_ascii_names_survive_untouched() {
    let fx = Fixture::realistic();
    let library = scan(&fx);
    let tracks = tag_dir(&library, names::KREAM_ALBUM, |n, name| TagSet {
        title: Values::one(name.split_once(' ').unwrap().1.trim_end_matches(".mp3")),
        artist: Values::one("KREAM"),
        album: Values::one("So Hï"),
        track: Some((n, None)),
        ..TagSet::default()
    });

    let mapping = run("{artist}/{album}/{track:02} {title}", &library, &tracks);

    assert_eq!(
        dest(&mapping, names::KREAM_TRACK),
        Some("KREAM/So Hï/01 So Hï.mp3")
    );
    assert!(
        mapping
            .moves
            .iter()
            .any(|m| m.to.as_str() == "KREAM/So Hï/03 ノスタルジア.mp3")
    );
}
