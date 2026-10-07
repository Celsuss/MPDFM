//! Editing one field across many files, with an honest answer for the fields
//! that currently differ.
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use mpdfm_core::paths::RelPath;
//! use mpdfm_core::tags::{BulkView, Edit, Field, FieldValue, TagSet, Values};
//!
//! # fn main() -> Result<(), mpdfm_core::paths::PathError> {
//! let selection = vec![
//!     (RelPath::parse("a/1.mp3")?, TagSet { album: Values::one("Mm..Food"),
//!                                          title: Values::one("Beef Rap"),
//!                                          ..TagSet::default() }),
//!     (RelPath::parse("a/2.mp3")?, TagSet { album: Values::one("Mm..Food"),
//!                                          title: Values::one("Hoe Cakes"),
//!                                          ..TagSet::default() }),
//! ];
//! let view = BulkView::of(&selection);
//!
//! // One album, two titles.
//! assert_eq!(view.get(Field::Album), &FieldValue::Same(Values::one("Mm..Food")));
//! assert_eq!(view.get(Field::Title), &FieldValue::Multiple);
//!
//! // Editing the genre writes it to both files; `title` is not in the edits, so
//! // nothing is written to it — which is the whole point.
//! let mut edits = BTreeMap::new();
//! edits.insert(Field::Genre, Edit::Set(Values::one("Hip Hop")));
//! let deltas = view.delta_for(&edits);
//!
//! assert_eq!(deltas.len(), 2);
//! assert!(deltas.iter().all(|(_, delta)| delta.get(Field::Title).is_none()));
//! # Ok(())
//! # }
//! ```
//!
//! # The bug this module exists to prevent
//!
//! Select fourteen tracks, change the genre, press save, and find that all
//! fourteen now have the first one's title. That is the classic bulk-editor
//! data-loss bug, and it comes from one mistake: treating the *displayed* value of
//! a field as the value to write.
//!
//! So nothing here derives what to write from what is shown. [`BulkView`] is a
//! read-only picture of the selection, and [`BulkView::delta_for`] is given the
//! fields the user **actually changed** — a field that is not in that map produces
//! no [`TagDelta`] entry for any file, whatever it looks like on screen. A
//! `<multiple>` field is not special-cased; it simply never arrives in the edits
//! unless somebody typed in it.
//!
//! `leaving_a_multiple_field_alone_writes_nothing` in `crates/core/tests/tags_bulk.rs`
//! is the property test over random selections that holds this line.
//!
//! # Per-file fields are named actions, not typed values
//!
//! Typing one value into `track` across fourteen files would give them all the
//! same track number, and typing one into `title` is worse. Both are
//! [`Field::is_per_file`], and the front-ends refuse them
//! ([`BulkView::per_file_in`]); what they offer instead is
//! [`BulkView::renumber_tracks`] and [`BulkView::titles_from_filenames`], which
//! compute a *different* value for each file and are previewable like any other
//! edit.

use std::collections::BTreeMap;

use crate::paths::RelPath;

use super::model::{FIELDS, Field, TagSet, Values};
use super::write::{self, Edit, TagDelta};

/// What one field looks like across a whole selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// No file in the selection has it. Shows as empty, and editing it writes to
    /// every file.
    Absent,
    /// Every file has it, and they agree. Shows the value.
    Same(Values),
    /// They do not agree — including the case where some files have it and others
    /// do not, because "present in nine of fourteen" is not a value either.
    ///
    /// **Never written unless the user changes it.** See the module docs.
    Multiple,
}

/// What a `<multiple>` field shows instead of a value.
///
/// Angle brackets because they cannot be mistaken for a tag: no field in this
/// library starts with one, so a user seeing it knows it is MPDFM talking.
pub const MULTIPLE: &str = "<multiple>";

impl FieldValue {
    /// The field as it reads across `tags`.
    #[must_use]
    pub fn of(field: Field, tags: &[TagSet]) -> Self {
        let mut values = tags.iter().map(|tags| tags.get(field));
        let Some(first) = values.next() else {
            // An empty selection agrees about everything, vacuously.
            return Self::Absent;
        };
        if !values.all(|other| other == first) {
            return Self::Multiple;
        }
        if first.is_empty() {
            Self::Absent
        } else {
            Self::Same(first)
        }
    }

    /// What a field table prints: the value, nothing, or [`MULTIPLE`].
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Absent => String::new(),
            Self::Same(values) => values.joined(),
            Self::Multiple => MULTIPLE.to_owned(),
        }
    }

    /// Whether the selection disagrees about this field.
    #[must_use]
    pub fn is_multiple(&self) -> bool {
        *self == Self::Multiple
    }

    /// The agreed value, if there is one.
    #[must_use]
    pub fn same(&self) -> Option<&Values> {
        match self {
            Self::Same(values) => Some(values),
            Self::Absent | Self::Multiple => None,
        }
    }
}

impl std::fmt::Display for FieldValue {
    /// Padded like a string, so a bulk field table lines up — see [`Field`]'s
    /// implementation for why that is not `write_str`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(&self.label())
    }
}

// ---------------------------------------------------------------------------

/// A selection of files, and what their tags look like taken together.
///
/// Read-only. Nothing on it writes, and the only thing that produces writes —
/// [`BulkView::delta_for`] and the named actions — takes what the user asked for
/// as an argument rather than reading it back off this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkView {
    /// Every modeled field and how it reads across the selection.
    pub fields: BTreeMap<Field, FieldValue>,

    /// The files, in the order they were given — which is the order they are
    /// displayed in, and therefore the order [`BulkView::renumber_tracks`]
    /// numbers them in.
    pub files: Vec<RelPath>,

    /// Each file's tags, parallel to [`BulkView::files`].
    ///
    /// Kept because the named actions need them: `renumber tracks` has to know
    /// each file's current total, `trim whitespace` has to know which fields
    /// actually have space around them, and `title from filename` has to know
    /// whether the title it computes is the one already there. Private so that
    /// nothing can mistake them for a source of values to write.
    tags: Vec<TagSet>,
}

impl BulkView {
    /// Look at a selection.
    ///
    /// The files keep the order they are given; everything else is derived.
    #[must_use]
    pub fn of(tags: &[(RelPath, TagSet)]) -> Self {
        let files: Vec<RelPath> = tags.iter().map(|(rel, _)| rel.clone()).collect();
        let tags: Vec<TagSet> = tags.iter().map(|(_, tags)| tags.clone()).collect();
        let fields = FIELDS
            .into_iter()
            .map(|field| (field, FieldValue::of(field, &tags)))
            .collect();
        Self {
            fields,
            files,
            tags,
        }
    }

    /// How one field reads across the selection.
    #[must_use]
    pub fn get(&self, field: Field) -> &FieldValue {
        self.fields.get(&field).unwrap_or(&FieldValue::Absent)
    }

    /// How many files are selected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether nothing is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Each file with its tags, in display order.
    pub fn selection(&self) -> impl Iterator<Item = (&RelPath, &TagSet)> {
        self.files.iter().zip(&self.tags)
    }

    /// The fields in `edits` that must not be typed in bulk.
    ///
    /// One implementation for both front-ends, so `mpdfm tag set --title` across
    /// four hundred files and the TUI's editor refuse for the same reason and with
    /// the same list. Empty for a selection of one, where typing a title is
    /// exactly what the user means.
    #[must_use]
    pub fn per_file_in(&self, edits: &BTreeMap<Field, Edit>) -> Vec<Field> {
        if self.files.len() < 2 {
            return Vec::new();
        }
        edits
            .iter()
            .filter(|(field, edit)| field.is_per_file() && matches!(edit, Edit::Set(_)))
            .map(|(field, _)| *field)
            .collect()
    }

    /// Turn the fields the user changed into one [`TagDelta`] per file.
    ///
    /// Only the fields in `edits`, and only the files they would actually change:
    /// a file that already says what was asked for gets no entry, so a `--genre`
    /// across an album that is already tagged right previews as nothing to do
    /// rather than as fourteen rewrites.
    ///
    /// A file whose delta comes out empty is left out of the result entirely.
    #[must_use]
    pub fn delta_for(&self, edits: &BTreeMap<Field, Edit>) -> Vec<(RelPath, TagDelta)> {
        self.selection()
            .filter_map(|(rel, tags)| {
                let mut delta = TagDelta::new();
                for (field, edit) in edits {
                    if changes(tags, *field, edit) {
                        delta = delta.with(*field, edit.clone());
                    }
                }
                (!delta.is_empty()).then(|| (rel.clone(), delta))
            })
            .collect()
    }

    /// Set one field across the selection — the shape of `genre = <value>`,
    /// `year = <value>` and `album = <value>`.
    #[must_use]
    pub fn set(&self, field: Field, value: &str) -> Vec<(RelPath, TagDelta)> {
        self.delta_for(&one(field, Edit::Set(Values::typed(value))))
    }

    /// Remove one field across the selection.
    ///
    /// The explicit clear a `<multiple>` field needs: leaving such a field alone
    /// and asking for it to be emptied are different requests, and this is the
    /// second one.
    #[must_use]
    pub fn clear(&self, field: Field) -> Vec<(RelPath, TagDelta)> {
        self.delta_for(&one(field, Edit::Clear))
    }

    /// `album artist = artist`: give each file its own artist as its album
    /// artist.
    ///
    /// Per-file by nature — the artists may differ — which is why it is an action
    /// and not a typed value. A file with no artist is skipped rather than having
    /// its album artist cleared.
    #[must_use]
    pub fn album_artist_from_artist(&self) -> Vec<(RelPath, TagDelta)> {
        self.per_file(|_, tags| {
            let artist = tags.artist.clone();
            (!artist.is_empty()).then_some((Field::AlbumArtist, Edit::Set(artist)))
        })
    }

    /// `strip comment`: remove the comment from every file that has one.
    #[must_use]
    pub fn strip_comment(&self) -> Vec<(RelPath, TagDelta)> {
        self.clear(Field::Comment)
    }

    /// `renumber tracks`: number the files `1..n` **in the order they are
    /// displayed**, and give every one the same total.
    ///
    /// The displayed order is [`BulkView::files`], which is the order the caller
    /// passed in — so a browser that sorts by name and a `tag set` that walked a
    /// directory both get the order the user was looking at. Nothing is inferred
    /// from the existing numbers, because the reason to renumber is usually that
    /// they are wrong.
    #[must_use]
    pub fn renumber_tracks(&self) -> Vec<(RelPath, TagDelta)> {
        let total = self.files.len();
        let mut number = 0;
        self.per_file(move |_, _| {
            number += 1;
            Some((
                Field::Track,
                Edit::Set(Values::one(format!("{number}/{total}"))),
            ))
        })
    }

    /// `title from filename`: take each file's title from its own name.
    ///
    /// See [`title_from_filename`] for exactly what is stripped. Conservative on
    /// purpose, and previewable like every other edit — scene naming varies
    /// wildly enough that the only safe version of this is one the user looks at
    /// before committing.
    #[must_use]
    pub fn titles_from_filenames(&self) -> Vec<(RelPath, TagDelta)> {
        self.per_file(|rel, _| {
            title_from_filename(rel.file_name())
                .map(|title| (Field::Title, Edit::Set(Values::one(title))))
        })
    }

    /// `trim whitespace`: strip leading and trailing space from every text
    /// field that has any.
    ///
    /// Field by field and file by file, so a selection where one file has a
    /// trailing space in its album produces one entry and not fourteen. Interior
    /// spacing is left alone: `Mm..Food  (2004)` is how somebody typed it, and
    /// collapsing runs of spaces would be editing the value rather than tidying
    /// it.
    #[must_use]
    pub fn trim_whitespace(&self) -> Vec<(RelPath, TagDelta)> {
        self.selection()
            .filter_map(|(rel, tags)| {
                let mut delta = TagDelta::new();
                for field in FIELDS {
                    // `track` and `disc` are numbers; there is no space in them
                    // to trim, and `5 / 12` is already normalized on the way in.
                    if matches!(field, Field::Track | Field::Disc) {
                        continue;
                    }
                    let values = tags.get(field);
                    let trimmed = Values::of(values.all().iter().map(|value| value.trim()));
                    if trimmed != values && !trimmed.all().iter().all(|value| value.is_empty()) {
                        delta = delta.with(field, Edit::Set(trimmed));
                    }
                }
                (!delta.is_empty()).then(|| (rel.clone(), delta))
            })
            .collect()
    }

    /// One edit per file, computed from that file — the shape every named action
    /// has. A file the closure declines is left out.
    fn per_file<F>(&self, mut edit: F) -> Vec<(RelPath, TagDelta)>
    where
        F: FnMut(&RelPath, &TagSet) -> Option<(Field, Edit)>,
    {
        self.selection()
            .filter_map(|(rel, tags)| {
                let (field, edit) = edit(rel, tags)?;
                changes(tags, field, &edit)
                    .then(|| (rel.clone(), TagDelta::new().with(field, edit)))
            })
            .collect()
    }
}

/// Fold several per-file edit sets into one [`TagDelta`] per file.
///
/// A selection can be edited in more than one way at once — `--genre X` together
/// with `--renumber-tracks`, or a typed `album` together with the tag editor's
/// `title from filename` — and each of those produces its own set. One file must
/// still end up with **one** operation holding all of them, because the unit of
/// reversal is the file and [`Conflict::DuplicateEdit`][crate::ops::Conflict::DuplicateEdit]
/// refuses two edits of the same one.
///
/// **Later sets win on a field they share**, which is the order the caller applied
/// them in: an explicit `track 3/9` alongside a renumbering means the renumbering,
/// because that is the more specific request and the one that cannot be expressed
/// any other way. Files come back in path order, and a file whose delta folded
/// down to nothing is left out.
///
/// Shared by the CLI (`mpdfm tag set`) and the TUI's tag editor, so the two cannot
/// disagree about which of two edits to the same field is the one that happens.
///
/// ```
/// use mpdfm_core::paths::RelPath;
/// use mpdfm_core::tags::{self, Field, TagDelta};
///
/// # fn main() -> Result<(), mpdfm_core::paths::PathError> {
/// let rel = RelPath::parse("a/1.mp3")?;
/// let merged = tags::merge(vec![
///     vec![(rel.clone(), TagDelta::new().set(Field::Genre, "Hip Hop"))],
///     vec![(rel.clone(), TagDelta::new().set(Field::Track, "1/9"))],
/// ]);
///
/// assert_eq!(merged.len(), 1, "one file, one operation");
/// assert_eq!(merged[0].1.len(), 2, "holding both fields");
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn merge(sets: Vec<Vec<(RelPath, TagDelta)>>) -> Vec<(RelPath, TagDelta)> {
    let mut merged: BTreeMap<RelPath, TagDelta> = BTreeMap::new();
    for set in sets {
        for (rel, delta) in set {
            let into = merged.entry(rel).or_default();
            for (field, edit) in delta.edits() {
                *into = std::mem::take(into).with(*field, edit.clone());
            }
        }
    }
    merged
        .into_iter()
        .filter(|(_, delta)| !delta.is_empty())
        .collect()
}

/// A map holding one edit, for the single-field actions.
fn one(field: Field, edit: Edit) -> BTreeMap<Field, Edit> {
    let mut edits = BTreeMap::new();
    edits.insert(field, edit);
    edits
}

/// Whether this edit would actually change this file.
///
/// Compared against what the value will *read back* as once written
/// ([`write::canonical`]), so asking for `--track 05/12` on a file that says
/// `5/12` is correctly recognized as nothing to do.
fn changes(tags: &TagSet, field: Field, edit: &Edit) -> bool {
    let current = tags.get(field);
    match edit {
        Edit::Clear => !current.is_empty(),
        Edit::Set(values) => current.joined() != write::canonical(field, values),
    }
}

// ---------------------------------------------------------------------------

/// A title taken from a file name, or `None` when there is nothing left of it.
///
/// What is stripped, and nothing else:
///
/// 1. the extension;
/// 2. a leading track number — one to three digits, optionally in brackets —
///    followed by at least one separator (`.`, `-`, `_` or a space);
/// 3. `_` becomes a space **only if the rest of the name has no spaces at all**,
///    which is the scene convention and not something a human-typed name does.
///
/// Every step is skipped if it would leave nothing, so `630.mp3` keeps its name
/// instead of becoming empty.
///
/// ```
/// use mpdfm_core::tags::bulk::title_from_filename;
///
/// // The real library's naming, all of it:
/// assert_eq!(title_from_filename("01.Smokin' On.mp3").as_deref(), Some("Smokin' On"));
/// assert_eq!(title_from_filename("10. Hands.mp3").as_deref(), Some("Hands"));
/// assert_eq!(title_from_filename("01 Beef Rap.mp3").as_deref(), Some("Beef Rap"));
/// assert_eq!(
///     title_from_filename("[24] If you want to sing out - Cat Stevens.flac").as_deref(),
///     Some("If you want to sing out - Cat Stevens")
/// );
/// assert_eq!(
///     title_from_filename("05_lost_frequencies_ft._jake_reese_-_sun.mp3").as_deref(),
///     Some("lost frequencies ft. jake reese - sun")
/// );
///
/// // A name that is only a number keeps it.
/// assert_eq!(title_from_filename("630.mp3").as_deref(), Some("630"));
/// assert_eq!(title_from_filename(".mp3"), None);
/// ```
#[must_use]
pub fn title_from_filename(name: &str) -> Option<String> {
    // A name with no dot in it is all stem; one that is *only* an extension
    // (`.mp3`) has none, and there is no title to take from it.
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => name,
    };
    let stem = stem.trim();
    if stem.is_empty() {
        return None;
    }

    let title = strip_track_number(stem).unwrap_or(stem);
    let title = if title.contains(' ') || !title.contains('_') {
        title.to_owned()
    } else {
        title.replace('_', " ")
    };

    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// `stem` without a leading track number and its separator, or `None` when it
/// does not start with one.
fn strip_track_number(stem: &str) -> Option<&str> {
    let mut rest = stem;
    // An optional bracket around the number, which has to be closed if it was
    // opened — `[24] Title` yes, `(Remember the days) …` no.
    let closing = match rest.chars().next()? {
        '[' => Some(']'),
        '(' => Some(')'),
        _ => None,
    };
    if closing.is_some() {
        rest = &rest[1..];
    }

    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    rest = &rest[digits..];

    if let Some(closing) = closing {
        rest = rest.strip_prefix(closing)?;
    }

    // At least one separator, so `1984 Title` is not a track number followed by
    // a title — and `01Title` is not either, because nothing separates them.
    let separators = rest
        .chars()
        .take_while(|c| matches!(c, '.' | '-' | '_' | ' '))
        .count();
    if separators == 0 {
        return None;
    }
    let rest = rest[separators..].trim();
    (!rest.is_empty()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).expect("test path")
    }

    fn tagged(album: &str, title: &str) -> TagSet {
        TagSet {
            album: Values::one(album),
            title: Values::one(title),
            ..TagSet::default()
        }
    }

    #[test]
    fn a_field_the_selection_agrees_about_shows_its_value() {
        let selection = vec![
            (rel("a/1.mp3"), tagged("Mm..Food", "Beef Rap")),
            (rel("a/2.mp3"), tagged("Mm..Food", "Hoe Cakes")),
            (rel("a/3.mp3"), tagged("Mm..Food", "Potholderz")),
        ];
        let view = BulkView::of(&selection);

        assert_eq!(
            view.get(Field::Album),
            &FieldValue::Same(Values::one("Mm..Food"))
        );
        assert_eq!(view.get(Field::Title), &FieldValue::Multiple);
        assert_eq!(view.get(Field::Genre), &FieldValue::Absent);

        assert_eq!(view.get(Field::Album).label(), "Mm..Food");
        assert_eq!(view.get(Field::Title).label(), MULTIPLE);
        assert_eq!(view.get(Field::Genre).label(), "");
    }

    #[test]
    fn a_field_present_in_some_files_is_multiple_and_not_a_value() {
        let selection = vec![
            (rel("a/1.mp3"), tagged("Mm..Food", "Beef Rap")),
            (rel("a/2.mp3"), TagSet::default()),
        ];
        let view = BulkView::of(&selection);
        assert_eq!(view.get(Field::Album), &FieldValue::Multiple);
        assert_eq!(view.get(Field::Album).same(), None);
    }

    #[test]
    fn an_edit_that_asks_for_what_is_already_there_writes_nothing() {
        let selection = vec![
            (rel("a/1.mp3"), tagged("Mm..Food", "Beef Rap")),
            (rel("a/2.mp3"), tagged("Mm..Food", "Hoe Cakes")),
        ];
        let view = BulkView::of(&selection);

        assert!(view.set(Field::Album, "Mm..Food").is_empty());
        assert_eq!(view.set(Field::Album, "Mm..Food (2004)").len(), 2);
    }

    #[test]
    fn clearing_a_field_only_touches_the_files_that_have_it() {
        let selection = vec![
            (rel("a/1.mp3"), tagged("Mm..Food", "Beef Rap")),
            (rel("a/2.mp3"), TagSet::default()),
        ];
        let view = BulkView::of(&selection);

        let cleared = view.clear(Field::Album);
        assert_eq!(cleared.len(), 1);
        assert_eq!(cleared[0].0, rel("a/1.mp3"));
        assert_eq!(cleared[0].1.get(Field::Album), Some(&Edit::Clear));
    }

    #[test]
    fn renumbering_uses_the_order_it_was_given() {
        let selection = vec![
            (rel("a/c.mp3"), TagSet::default()),
            (rel("a/a.mp3"), TagSet::default()),
            (rel("a/b.mp3"), TagSet::default()),
        ];
        let numbered = BulkView::of(&selection).renumber_tracks();

        let got: Vec<(&str, String)> = numbered
            .iter()
            .map(|(rel, delta)| {
                (
                    rel.as_str(),
                    delta
                        .get(Field::Track)
                        .and_then(super::Edit::values)
                        .map(Values::joined)
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("a/c.mp3", "1/3".to_owned()),
                ("a/a.mp3", "2/3".to_owned()),
                ("a/b.mp3", "3/3".to_owned()),
            ]
        );
    }

    #[test]
    fn renumbering_skips_a_file_that_is_already_right() {
        let selection = vec![
            (
                rel("a/1.mp3"),
                TagSet {
                    track: Some((1, Some(2))),
                    ..TagSet::default()
                },
            ),
            (rel("a/2.mp3"), TagSet::default()),
        ];
        let numbered = BulkView::of(&selection).renumber_tracks();
        assert_eq!(numbered.len(), 1);
        assert_eq!(numbered[0].0, rel("a/2.mp3"));
    }

    #[test]
    fn album_artist_from_artist_skips_a_file_with_no_artist() {
        let selection = vec![
            (
                rel("a/1.mp3"),
                TagSet {
                    artist: Values::one("MF DOOM"),
                    ..TagSet::default()
                },
            ),
            (rel("a/2.mp3"), TagSet::default()),
        ];
        let deltas = BulkView::of(&selection).album_artist_from_artist();

        assert_eq!(deltas.len(), 1);
        assert_eq!(
            deltas[0].1.get(Field::AlbumArtist),
            Some(&Edit::Set(Values::one("MF DOOM")))
        );
    }

    #[test]
    fn trimming_touches_only_the_fields_that_have_space_around_them() {
        let selection = vec![
            (
                rel("a/1.mp3"),
                TagSet {
                    album: Values::one(" Mm..Food "),
                    title: Values::one("Beef Rap"),
                    ..TagSet::default()
                },
            ),
            (rel("a/2.mp3"), tagged("Mm..Food", "Hoe Cakes")),
        ];
        let deltas = BulkView::of(&selection).trim_whitespace();

        assert_eq!(deltas.len(), 1, "{deltas:?}");
        assert_eq!(deltas[0].0, rel("a/1.mp3"));
        assert_eq!(deltas[0].1.len(), 1);
        assert_eq!(
            deltas[0].1.get(Field::Album),
            Some(&Edit::Set(Values::one("Mm..Food")))
        );
    }

    #[test]
    fn trimming_never_empties_a_field() {
        // A field holding nothing but spaces: trimming it would be a clear, and a
        // clear is a different request.
        let selection = vec![(
            rel("a/1.mp3"),
            TagSet {
                album: Values::one("   "),
                ..TagSet::default()
            },
        )];
        assert!(BulkView::of(&selection).trim_whitespace().is_empty());
    }

    #[test]
    fn a_per_file_field_is_refused_in_bulk_and_allowed_for_one_file() {
        let edits = one(Field::Title, Edit::Set(Values::one("Beef Rap")));

        let many = BulkView::of(&[
            (rel("a/1.mp3"), TagSet::default()),
            (rel("a/2.mp3"), TagSet::default()),
        ]);
        assert_eq!(many.per_file_in(&edits), [Field::Title]);

        let one_file = BulkView::of(&[(rel("a/1.mp3"), TagSet::default())]);
        assert!(one_file.per_file_in(&edits).is_empty());

        // A clear is not a value typed into fourteen boxes, so it is allowed.
        let clearing = one(Field::Title, Edit::Clear);
        assert!(many.per_file_in(&clearing).is_empty());
    }

    #[test]
    fn an_empty_selection_produces_no_edits() {
        let view = BulkView::of(&[]);
        assert!(view.is_empty());
        assert_eq!(view.get(Field::Album), &FieldValue::Absent);
        assert!(view.set(Field::Genre, "Hip Hop").is_empty());
        assert!(view.renumber_tracks().is_empty());
    }

    #[test]
    fn a_track_number_is_only_stripped_when_something_separates_it() {
        assert_eq!(strip_track_number("01 Beef Rap"), Some("Beef Rap"));
        assert_eq!(strip_track_number("01.Beef Rap"), Some("Beef Rap"));
        assert_eq!(strip_track_number("01-Beef Rap"), Some("Beef Rap"));
        assert_eq!(strip_track_number("01_Beef Rap"), Some("Beef Rap"));
        assert_eq!(strip_track_number("[1] Beef Rap"), Some("Beef Rap"));

        // Nothing separates them, so the digits are part of the name.
        assert_eq!(strip_track_number("01Beef Rap"), None);
        // Four digits is a year, not a track number.
        assert_eq!(strip_track_number("1984 Beef Rap"), None);
        // An opened bracket that is never closed is not a track number.
        assert_eq!(strip_track_number("(Remember the days) Old school"), None);
        // And nothing would be left.
        assert_eq!(strip_track_number("01."), None);
    }
}
