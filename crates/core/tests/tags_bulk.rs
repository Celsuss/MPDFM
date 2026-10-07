//! Task 18 — editing a field across many files without flattening the rest.
//!
//! The acceptance criteria of `docs/tasks/18-tag-bulk.md`. The one that matters
//! most is [`leaving_a_multiple_field_alone_writes_nothing`], a property test over
//! random selections: it is the classic bulk-editor data-loss bug, and the only
//! honest way to rule it out is to generate selections rather than to pick one.
//!
//! The later tests go through `read` and `write` against real audio in a
//! [`Fixture`], because "a selection mixing mp3 and FLAC works" is not a claim a
//! test over synthesized `TagSet`s can make.

#![cfg(unix)]

use std::collections::BTreeMap;

use mpdfm_core::config::Config;
use mpdfm_core::library::Library;
use mpdfm_core::ops::commit::{self, Previewed};
use mpdfm_core::ops::{Effects, Operation, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::{
    self, BulkView, Edit, Field, FieldValue, MULTIPLE, TagDelta, TagSet, Values,
    title_from_filename,
};
use mpdfm_core::testing::{Fixture, Snapshot, tags as fix};

fn rel(s: &str) -> RelPath {
    RelPath::parse(s).expect("test path")
}

/// A `TagSet` with just the fields a test names.
fn tagged(fields: &[(Field, &str)]) -> TagSet {
    let mut tags = TagSet::default();
    for (field, value) in fields {
        match field {
            Field::Title => tags.title = Values::one(*value),
            Field::Artist => tags.artist = Values::one(*value),
            Field::AlbumArtist => tags.album_artist = Values::one(*value),
            Field::Album => tags.album = Values::one(*value),
            Field::Year => tags.date = Values::one(*value),
            Field::Genre => tags.genre = Values::one(*value),
            Field::Comment => tags.comment = Values::one(*value),
            Field::Composer => tags.composer = Values::one(*value),
            Field::Track => tags.track = tags::parse_pair(value),
            Field::Disc => tags.disc = tags::parse_pair(value),
        }
    }
    tags
}

// ---------------------------------------------------------------------------

#[test]
fn three_files_with_one_album_and_three_titles_read_as_same_and_multiple() {
    let selection = vec![
        (
            rel("a/1.mp3"),
            tagged(&[(Field::Album, "Mm..Food"), (Field::Title, "Beef Rap")]),
        ),
        (
            rel("a/2.mp3"),
            tagged(&[(Field::Album, "Mm..Food"), (Field::Title, "Hoe Cakes")]),
        ),
        (
            rel("a/3.mp3"),
            tagged(&[(Field::Album, "Mm..Food"), (Field::Title, "Potholderz")]),
        ),
    ];
    let view = BulkView::of(&selection);

    assert_eq!(
        view.get(Field::Album),
        &FieldValue::Same(Values::one("Mm..Food"))
    );
    assert_eq!(view.get(Field::Title), &FieldValue::Multiple);
    assert_eq!(view.get(Field::Title).label(), MULTIPLE);
    // Absent everywhere shows empty, which is not the same as `<multiple>`.
    assert_eq!(view.get(Field::Genre), &FieldValue::Absent);
    assert_eq!(view.get(Field::Genre).label(), "");
    assert_eq!(view.len(), 3);
}

/// **The critical one.** A field the user did not touch must produce no write,
/// for any selection, whatever that field looks like.
#[test]
fn leaving_a_multiple_field_alone_writes_nothing() {
    // A tiny deterministic generator: a test that needs a seed to be reproduced
    // is a test nobody reproduces.
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    const VALUES: [&str; 4] = ["", "one", "two", "three"];
    const EVERY: [Field; 10] = mpdfm_core::tags::FIELDS;

    for round in 0..500 {
        // 1 to 6 files, each with a random value (or none) in every field.
        let count = 1 + (next() % 6) as usize;
        let selection: Vec<(RelPath, TagSet)> = (0..count)
            .map(|n| {
                let mut tags = TagSet::default();
                for field in EVERY {
                    let value = VALUES[(next() % VALUES.len() as u64) as usize];
                    if value.is_empty() {
                        continue;
                    }
                    let value = match field {
                        // The number fields only hold numbers.
                        Field::Track | Field::Disc => format!("{}", 1 + next() % 20),
                        Field::Year => format!("{}", 1970 + next() % 50),
                        _ => value.to_owned(),
                    };
                    tags = tagged_with(tags, field, &value);
                }
                (rel(&format!("a/{n}.mp3")), tags)
            })
            .collect();

        let view = BulkView::of(&selection);

        // Edit exactly one field — a different one each round — and nothing else.
        let edited = EVERY[round % EVERY.len()];
        let value = match edited {
            Field::Track | Field::Disc => "7/9".to_owned(),
            Field::Year => "1999".to_owned(),
            _ => "edited".to_owned(),
        };
        let mut edits = BTreeMap::new();
        edits.insert(edited, Edit::Set(Values::one(value.clone())));

        for (path, delta) in view.delta_for(&edits) {
            // Every delta holds that one field and no other, however the rest of
            // the selection's fields happened to come out.
            assert_eq!(
                delta.len(),
                1,
                "round {round}: {path} got {} fields from a one-field edit: {delta}",
                delta.len()
            );
            for untouched in EVERY.into_iter().filter(|field| *field != edited) {
                assert_eq!(
                    delta.get(untouched),
                    None,
                    "round {round}: {path} would have {untouched} written by an edit \
                     to {edited}; the view said {}",
                    view.get(untouched).label()
                );
            }
        }
    }
}

/// [`tagged`] for one field, on an existing set.
fn tagged_with(mut tags: TagSet, field: Field, value: &str) -> TagSet {
    match field {
        Field::Title => tags.title = Values::one(value),
        Field::Artist => tags.artist = Values::one(value),
        Field::AlbumArtist => tags.album_artist = Values::one(value),
        Field::Album => tags.album = Values::one(value),
        Field::Year => tags.date = Values::one(value),
        Field::Genre => tags.genre = Values::one(value),
        Field::Comment => tags.comment = Values::one(value),
        Field::Composer => tags.composer = Values::one(value),
        Field::Track => tags.track = tags::parse_pair(value),
        Field::Disc => tags.disc = tags::parse_pair(value),
    }
    tags
}

#[test]
fn editing_a_multiple_field_writes_the_new_value_to_every_file() {
    let selection = vec![
        (rel("a/1.mp3"), tagged(&[(Field::Genre, "Hip-Hop")])),
        (rel("a/2.mp3"), tagged(&[(Field::Genre, "Rap")])),
        (rel("a/3.mp3"), TagSet::default()),
    ];
    let view = BulkView::of(&selection);
    assert_eq!(view.get(Field::Genre), &FieldValue::Multiple);

    let deltas = view.set(Field::Genre, "Hip Hop");
    assert_eq!(
        deltas.len(),
        3,
        "every selected file, including the empty one"
    );
    for (_, delta) in &deltas {
        assert_eq!(
            delta.get(Field::Genre),
            Some(&Edit::Set(Values::one("Hip Hop")))
        );
    }
}

#[test]
fn an_explicit_clear_removes_the_field_from_every_file_that_has_it() {
    let selection = vec![
        (rel("a/1.mp3"), tagged(&[(Field::Comment, "ripped by X")])),
        (rel("a/2.mp3"), tagged(&[(Field::Comment, "visit Y")])),
        (rel("a/3.mp3"), TagSet::default()),
    ];
    let view = BulkView::of(&selection);
    assert_eq!(view.get(Field::Comment), &FieldValue::Multiple);

    // Leaving it alone writes nothing...
    assert!(view.delta_for(&BTreeMap::new()).is_empty());
    // ...and asking for it to go removes it from the two that have one.
    let cleared = view.strip_comment();
    assert_eq!(cleared.len(), 2);
    for (_, delta) in &cleared {
        assert_eq!(delta.get(Field::Comment), Some(&Edit::Clear));
    }
}

#[test]
fn renumbering_numbers_the_displayed_order_and_sets_the_total() {
    let selection: Vec<(RelPath, TagSet)> = ["b.mp3", "a.mp3", "c.mp3", "d.mp3"]
        .iter()
        .map(|name| (rel(&format!("a/{name}")), TagSet::default()))
        .collect();

    let numbered = BulkView::of(&selection).renumber_tracks();
    assert_eq!(numbered.len(), 4);
    for (position, (path, delta)) in numbered.iter().enumerate() {
        assert_eq!(path, &selection[position].0, "the order it was given");
        assert_eq!(
            delta
                .get(Field::Track)
                .and_then(Edit::values)
                .map(Values::joined)
                .as_deref(),
            Some(format!("{}/4", position + 1).as_str())
        );
        assert_eq!(delta.len(), 1, "and nothing else");
    }
}

#[test]
fn a_title_from_a_filename_strips_the_number_and_the_extension() {
    // Every naming shape in the real library, taken from it.
    for (name, want) in [
        ("01.Smokin' On.mp3", "Smokin' On"),
        ("05.Talent Show.mp3", "Talent Show"),
        (
            "03.You Can Put It In A Zag, I'mma Put It In A Blunt.mp3",
            "You Can Put It In A Zag, I'mma Put It In A Blunt",
        ),
        ("10. Hands.mp3", "Hands"),
        ("01 Beef Rap.mp3", "Beef Rap"),
        ("02 Sirens + Symphony.mp3", "Sirens + Symphony"),
        (
            "[24] If you want to sing out, sing out - Cat Stevens.flac",
            "If you want to sing out, sing out - Cat Stevens",
        ),
        (
            "[6] Lady d'Arbanville - Cat Stevens.flac",
            "Lady d'Arbanville - Cat Stevens",
        ),
        (
            "05_lost_frequencies_ft._jake_reese_-_sun_is_shining.mp3",
            "lost frequencies ft. jake reese - sun is shining",
        ),
        (
            "04_lost_frequencies_ft._sandro_cavazza_-_beautiful_life_(deluxe_mix).mp3",
            "lost frequencies ft. sandro cavazza - beautiful life (deluxe mix)",
        ),
        ("03 ノスタルジア.mp3", "ノスタルジア"),
        ("Coding_Trance.mp3", "Coding Trance"),
    ] {
        assert_eq!(title_from_filename(name).as_deref(), Some(want), "{name:?}");
    }

    // Conservative: a name that is only a number keeps it, and a bracketed
    // phrase is not a track number.
    assert_eq!(title_from_filename("04.630.mp3").as_deref(), Some("630"));
    assert_eq!(title_from_filename("630.mp3").as_deref(), Some("630"));
    assert_eq!(
        title_from_filename("[17] (Remember the days of the) Old school yard.flac").as_deref(),
        Some("(Remember the days of the) Old school yard")
    );
}

#[test]
fn a_title_from_a_filename_skips_a_file_that_already_has_that_title() {
    let selection = vec![
        (
            rel("a/01 Beef Rap.mp3"),
            tagged(&[(Field::Title, "Beef Rap")]),
        ),
        (rel("a/02 Hoe Cakes.mp3"), TagSet::default()),
    ];
    let deltas = BulkView::of(&selection).titles_from_filenames();

    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].0, rel("a/02 Hoe Cakes.mp3"));
    assert_eq!(
        deltas[0].1.get(Field::Title),
        Some(&Edit::Set(Values::one("Hoe Cakes")))
    );
}

// ---------------------------------------------------------------------------
// Against real audio.

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

    /// Read the tags of every track in the fixture, in library order.
    fn view(&self) -> BulkView {
        let read: Vec<(RelPath, TagSet)> = tags::read_many(self.fx.tracks(), self.fx.music_dir())
            .into_iter()
            .map(|(rel, result)| (rel, result.expect("every fixture track reads")))
            .collect();
        BulkView::of(&read)
    }

    fn plan(&self, deltas: Vec<(RelPath, TagDelta)>) -> Plan {
        Plan::of(
            deltas
                .into_iter()
                .map(|(target, changes)| Operation::WriteTags { target, changes })
                .collect(),
        )
    }

    fn effects(&self, plan: &Plan) -> Effects {
        plan.validate(&self.library, &self.index, &self.config)
    }

    fn commit(&self, plan: &Plan) -> mpdfm_core::ops::Committed {
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
}

/// An mp3 album, a FLAC album and an m4a, all in one selection.
fn mixed() -> Fixture {
    Fixture::builder()
        .album("hiphop/album", &["01 Beef Rap.mp3", "02 Hoe Cakes.mp3"])
        .flac_album("jazz/album")
        .track("coding-music/SwitchAngel/Coding_Trance.mp3")
        .build()
}

#[test]
fn a_selection_mixing_mp3_and_flac_writes_format_appropriate_tags() {
    let txn = Txn::new(mixed());
    let view = txn.view();
    assert!(view.len() >= 6, "{} tracks", view.len());

    let plan = txn.plan(view.set(Field::Genre, "Hip Hop"));
    let effects = txn.effects(&plan);
    assert_eq!(effects.summary.tags_written, plan.len());
    txn.commit(&plan);

    // Every one of them, whatever container it is.
    for rel in txn.fx.tracks() {
        let abs = rel.to_abs(txn.fx.music_dir());
        assert_eq!(
            tags::read_tags(&abs).expect("it reads").genre.first(),
            Some("Hip Hop"),
            "{rel}"
        );
        // And written in that container's own vocabulary, not as a stray frame.
        let dump = fix::dump(&abs);
        assert!(
            dump.contains("TCON=Hip Hop")
                || dump.contains("genre=Hip Hop")
                || dump.contains("GENRE=Hip Hop")
                || dump.contains("\u{a9}gen=Hip Hop"),
            "{rel}:\n{dump}"
        );
    }
}

#[test]
fn an_untouched_field_is_not_flattened_across_a_real_selection() {
    let txn = Txn::new(mixed());
    let before: Vec<(RelPath, TagSet)> = txn
        .view()
        .selection()
        .map(|(rel, tags)| (rel.clone(), tags.clone()))
        .collect();

    // The titles differ across the selection, which is exactly the field a
    // careless bulk editor would set them all to.
    let view = BulkView::of(&before);
    assert_eq!(view.get(Field::Title), &FieldValue::Multiple);

    txn.commit(&txn.plan(view.set(Field::Genre, "Hip Hop")));

    for (rel, was) in &before {
        let now = tags::read_tags(&rel.to_abs(txn.fx.music_dir())).expect("it reads");
        assert_eq!(now.title, was.title, "{rel} lost its own title");
        assert_eq!(now.album, was.album, "{rel} lost its own album");
        assert_eq!(now.track, was.track, "{rel} lost its own track number");
    }
}

#[test]
fn a_bulk_edit_previews_as_one_line_per_field_with_a_file_count() {
    let txn = Txn::new(mixed());
    let view = txn.view();
    let files = view.len();

    // One operation per file carrying both fields, which is what a real bulk
    // edit produces.
    let mut combined: BTreeMap<Field, Edit> = BTreeMap::new();
    combined.insert(Field::Genre, Edit::Set(Values::one("Hip Hop")));
    combined.insert(Field::AlbumArtist, Edit::Set(Values::one("Various")));
    let plan = txn.plan(view.delta_for(&combined));

    let preview = txn.effects(&plan).render(100);
    let tag_rows: Vec<&str> = preview
        .lines()
        .filter(|line| line.starts_with("TAG"))
        .collect();

    assert_eq!(
        tag_rows.len(),
        2,
        "one row per changed field, not per file:\n{preview}"
    );
    assert!(
        tag_rows
            .iter()
            .any(|row| row.contains(r#"genre = "Hip Hop""#)),
        "{preview}"
    );
    assert!(
        tag_rows
            .iter()
            .any(|row| row.contains(r#"albumartist = "Various""#)),
        "{preview}"
    );
    assert!(
        tag_rows
            .iter()
            .all(|row| row.contains(&format!("{files} files"))),
        "every row names how many files it touches:\n{preview}"
    );
}

#[test]
fn renumbering_an_album_previews_as_one_row_rather_than_one_per_number() {
    let names: Vec<String> = (1..=12).map(|n| format!("{n:02} track.mp3")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let txn = Txn::new(Fixture::builder().album("pop/album", &refs).build());

    let plan = txn.plan(txn.view().renumber_tracks());
    let effects = txn.effects(&plan);
    assert_eq!(effects.summary.tags_written, 12);

    let preview = effects.render(100);
    let tag_rows: Vec<&str> = preview
        .lines()
        .filter(|line| line.starts_with("TAG"))
        .collect();
    assert_eq!(tag_rows.len(), 1, "{preview}");
    assert!(
        tag_rows[0].contains("<per file>"),
        "twelve different numbers is not a value to print:\n{preview}"
    );
    assert!(tag_rows[0].contains("12 files"), "{preview}");

    txn.commit(&plan);
    for (position, name) in refs.iter().enumerate() {
        let abs = txn.fx.abs(&format!("pop/album/{name}"));
        assert_eq!(
            tags::read_tags(&abs).expect("it reads").track,
            Some((u32::try_from(position + 1).expect("small"), Some(12))),
            "{name}"
        );
    }
}

#[test]
fn a_bulk_edit_is_undone_by_one_undo() {
    use mpdfm_core::journal::store::Store;
    use mpdfm_core::journal::undo;

    let txn = Txn::new(mixed());
    let before = Snapshot::capture(txn.fx.music_dir());

    let mut edits: BTreeMap<Field, Edit> = BTreeMap::new();
    edits.insert(Field::Genre, Edit::Set(Values::one("Hip Hop")));
    edits.insert(Field::Year, Edit::Set(Values::one("1999")));
    edits.insert(Field::Comment, Edit::Clear);
    let committed = txn.commit(&txn.plan(txn.view().delta_for(&edits)));
    assert!(
        !before
            .diff(&Snapshot::capture(txn.fx.music_dir()))
            .is_empty()
    );

    let store = Store::at(txn.fx.data_dir());
    let record = store.load(&committed.txid).expect("the record loads");
    undo::undo(&store, &record, &txn.config, &undo::Options::default())
        .expect("a bulk tag edit undoes");

    before.assert_same(&Snapshot::capture(txn.fx.music_dir()));
}

/// Two edit sets over the same files fold into one operation per file, and the
/// later set wins the field they share.
///
/// `tags::merge` moved into core when the TUI's tag editor needed it (task 23):
/// the editor folds its typed fields together with the per-file actions the user
/// accepted, which is exactly what `mpdfm tag set --genre X --renumber-tracks`
/// does. One definition, so the two front-ends cannot disagree about which of
/// two edits to one field is the one that happens.
#[test]
fn merging_edit_sets_gives_one_operation_per_file_and_the_later_set_wins() {
    let one = rel("a/1.mp3");
    let two = rel("a/2.mp3");

    let merged = tags::merge(vec![
        vec![
            (one.clone(), TagDelta::new().set(Field::Genre, "Hip Hop")),
            (two.clone(), TagDelta::new().set(Field::Genre, "Hip Hop")),
        ],
        // A typed track, then a renumbering of the same field.
        vec![(one.clone(), TagDelta::new().set(Field::Track, "3/9"))],
        vec![(one.clone(), TagDelta::new().set(Field::Track, "1/2"))],
        // And a delta that folds down to nothing at all.
        vec![(two.clone(), TagDelta::new())],
    ]);

    assert_eq!(merged.len(), 2, "one entry per file: {merged:?}");
    let (first, delta) = &merged[0];
    assert_eq!(first, &one);
    assert_eq!(delta.len(), 2, "both fields in one operation");
    assert_eq!(
        delta.get(Field::Track),
        Some(&Edit::Set(Values::one("1/2"))),
        "the later set is the one that happens"
    );
    assert_eq!(merged[1].0, two);
    assert_eq!(merged[1].1.len(), 1, "the empty delta added nothing");
}

/// A file whose every edit folds away is left out entirely, so a merge cannot
/// produce an operation that would write nothing.
#[test]
fn merging_nothing_produces_nothing() {
    assert!(tags::merge(Vec::new()).is_empty());
    assert!(tags::merge(vec![vec![(rel("a/1.mp3"), TagDelta::new())]]).is_empty());
}
