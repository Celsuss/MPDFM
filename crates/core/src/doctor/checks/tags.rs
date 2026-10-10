//! What the files' tags say, and whether it agrees with itself.
//!
//! Every audio file is read once, into a `Table`, before any of these run —
//! with [`read_tags_with_layout`], which skips the audio properties and is the
//! cheap read.
//!
//! # Only an album can be inconsistent
//!
//! `inconsistent-albums` and `track-numbers` speak only about a directory that
//! is an album **by its own tags**: at least three tracks, and at least three
//! quarters of those that have an `album` agreeing on it. The real library has
//! plenty of directories that are not albums — a YouTube dump of singles, an
//! artist's loose tracks — and every track in those has its own album and its
//! own track 1. That is not inconsistency, and reporting it would bury the one
//! misspelt track in an otherwise perfect album, which is the thing worth
//! finding.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use super::super::{Check, Info, Item, Progress, shell_quote};
use crate::library::{DirPath, Entry, Format, Library};
use crate::tags::{TagError, TagLayout, TagSet, Values, read_tags_with_layout};

/// Every audio file's tags, read once.
#[derive(Debug)]
pub(crate) struct Table {
    /// One row per audio file, in library entry order.
    rows: Vec<Row>,
}

/// One audio file's tags, or why they could not be read.
#[derive(Debug)]
struct Row {
    /// Index into [`Library::entries`].
    entry: usize,
    read: Result<(TagSet, TagLayout), TagError>,
}

impl Table {
    /// Read every audio file in `library`.
    pub(crate) fn read(library: &Library, progress: &mut dyn FnMut(Progress)) -> Self {
        let audio: Vec<usize> = (0..library.len())
            .filter(|at| library.entry(*at).is_some_and(Entry::is_audio))
            .collect();
        let total = audio.len();
        let mut rows = Vec::with_capacity(total);
        for (done, entry) in audio.into_iter().enumerate() {
            if done % 64 == 0 {
                progress(Progress::ReadingTags { done, total });
            }
            let abs = library.entries()[entry].rel.to_abs(library.root());
            rows.push(Row {
                entry,
                read: read_tags_with_layout(&abs),
            });
        }
        progress(Progress::ReadingTags { done: total, total });
        Self { rows }
    }

    /// Every file whose tags were read, with them.
    pub(crate) fn tagged<'a>(
        &'a self,
        library: &'a Library,
    ) -> impl Iterator<Item = (&'a Entry, &'a TagSet, TagLayout)> + 'a {
        self.rows.iter().filter_map(|row| {
            let (tags, layout) = row.read.as_ref().ok()?;
            Some((&library.entries()[row.entry], tags, *layout))
        })
    }
}

/// Run one check of this group.
pub(crate) fn run(info: Info, library: &Library, table: &Table) -> Check {
    let items = match info.name {
        "unreadable-tags" => table
            .rows
            .iter()
            .filter_map(|row| {
                let err = row.read.as_ref().err()?;
                let rel = &library.entries()[row.entry].rel;
                Some(Item::new(rel.as_str(), reason(err)))
            })
            .collect(),
        "untagged" => table
            .tagged(library)
            .filter(|(_, tags, layout)| tags.is_empty() && !layout.id3v1)
            .map(|(entry, _, _)| Item::new(entry.rel.as_str(), "no tag of any kind"))
            .collect(),
        "missing-tags" => missing_tags(library, table),
        "inconsistent-albums" => inconsistent_albums(library, table),
        "track-numbers" => track_numbers(library, table),
        "id3v1-only" => table
            .tagged(library)
            .filter(|(entry, _, layout)| {
                entry.format() == Some(Format::Mp3) && layout.id3v1 && !layout.primary
            })
            .map(|(entry, _, _)| {
                Item::new(
                    entry.rel.as_str(),
                    "MPD reads the ID3v1 tag; MPDFM edits ID3v2 and shows this file as untagged",
                )
            })
            .collect(),
        "implausible-years" => implausible_years(library, table),
        _ => super::unknown(info),
    };
    Check::ran(info, items)
}

/// [`TagError`]'s message without the path, which the item already names.
fn reason(err: &TagError) -> String {
    let path = err.path().as_str();
    let message = err.to_string();
    message
        .strip_prefix(path)
        .map(|rest| rest.trim_start_matches([':', ' ']).to_owned())
        .filter(|rest| !rest.is_empty())
        .unwrap_or(message)
}

/// Whether a field holds something other than blanks.
fn has(values: &Values) -> bool {
    values.all().iter().any(|value| !value.trim().is_empty())
}

/// Tracks missing a title, artist, album or genre.
///
/// A file with no tag at all is `untagged` and not listed here as missing all
/// four, and neither is an ID3v1-only file whose fields MPDFM cannot see — one
/// finding per file, under the check that says what is really wrong.
fn missing_tags(library: &Library, table: &Table) -> Vec<Item> {
    table
        .tagged(library)
        .filter(|(_, tags, layout)| !tags.is_empty() && layout.primary)
        .filter_map(|(entry, tags, _)| {
            let missing: Vec<&str> = [
                ("title", &tags.title),
                ("artist", &tags.artist),
                ("album", &tags.album),
                ("genre", &tags.genre),
            ]
            .into_iter()
            .filter(|(_, values)| !has(values))
            .map(|(name, _)| name)
            .collect();
            if missing.is_empty() {
                return None;
            }
            let item = Item::new(entry.rel.as_str(), format!("no {}", missing.join(", ")));
            // The one missing field with an obvious value.
            Some(if missing.contains(&"title") {
                item.with_fix(format!(
                    "mpdfm tag set {} --title-from-filename",
                    shell_quote(entry.rel.as_str())
                ))
            } else {
                item
            })
        })
        .collect()
}

/// The tracks of each album directory, with their tags.
fn by_album_dir<'a>(
    library: &'a Library,
    table: &'a Table,
) -> BTreeMap<DirPath, Vec<(&'a Entry, &'a TagSet)>> {
    let mut dirs: BTreeMap<DirPath, Vec<(&Entry, &TagSet)>> = BTreeMap::new();
    for (entry, tags, _) in table.tagged(library) {
        dirs.entry(entry.dir()).or_default().push((entry, tags));
    }
    dirs
}

/// The value most tracks agree on, how many do, and how many have a value at
/// all — or `None` when fewer than two do.
fn dominant<'a>(
    values: impl Iterator<Item = Option<String>> + 'a,
) -> Option<(String, usize, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut with = 0;
    for value in values.flatten() {
        with += 1;
        *counts.entry(value).or_default() += 1;
    }
    let (value, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
    (with >= 2).then_some((value, count, with))
}

/// Whether `count` of `with` is a clear enough majority to call the rest
/// outliers: three quarters, and at least three tracks.
fn is_clear(count: usize, with: usize) -> bool {
    with >= 3 && count * 4 >= with * 3
}

/// A field's first non-blank value, trimmed.
fn first(values: &Values) -> Option<String> {
    values
        .all()
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// The album a directory's tracks agree on, if they clearly do.
fn album_of(tracks: &[(&Entry, &TagSet)]) -> Option<String> {
    let (album, count, with) = dominant(tracks.iter().map(|(_, tags)| first(&tags.album)))?;
    is_clear(count, with).then_some(album)
}

/// How one album-level field is read off a track, for comparison.
type Reading = fn(&TagSet) -> Option<String>;

/// Album directories where a few tracks disagree with the rest.
///
/// One item per directory and field, naming the outliers. The fix sets the
/// majority's value on the whole directory, which changes only the outliers —
/// `tag set` on a directory means its own tracks and not its subdirectories.
fn inconsistent_albums(library: &Library, table: &Table) -> Vec<Item> {
    let mut items = Vec::new();
    for (dir, tracks) in by_album_dir(library, table) {
        if dir.is_root() || album_of(&tracks).is_none() {
            continue;
        }
        let fields: [(&str, &str, Reading); 3] = [
            ("album", "--album", |tags| first(&tags.album)),
            ("albumartist", "--album-artist", |tags| {
                first(&tags.album_artist)
            }),
            ("year", "--year", |tags| {
                tags.year().map(|year| year.to_string())
            }),
        ];
        for (name, flag, value) in fields {
            let Some((majority, count, with)) =
                dominant(tracks.iter().map(|(_, tags)| value(tags)))
            else {
                continue;
            };
            if count == with || !is_clear(count, with) {
                continue;
            }
            let outliers: Vec<String> = tracks
                .iter()
                .filter_map(|(entry, tags)| {
                    let theirs = value(tags)?;
                    (theirs != majority).then(|| format!("{} has {theirs:?}", entry.file_name()))
                })
                .collect();
            // A year is set as a year: the majority's full dates may differ, and
            // the field is the one that disagreed.
            items.push(
                Item::new(
                    dir.as_str(),
                    format!(
                        "{count} of {with} tracks have {name} {majority:?}; {}",
                        outliers.join("; ")
                    ),
                )
                .with_fix(format!(
                    "mpdfm tag set {} {flag} {}",
                    shell_quote(dir.as_str()),
                    shell_quote(&majority)
                )),
            );
        }
    }
    items
}

/// Album directories with a track number twice, or a gap below the highest.
///
/// Per disc, so a two-disc album in one directory has two track 1s and that is
/// right. A gap is only looked for **below the highest number present**: a
/// partial album — tracks 1, 2 and 3 of a 15-track `1/15` — is how much of this
/// library arrived, and is not a defect.
fn track_numbers(library: &Library, table: &Table) -> Vec<Item> {
    let mut items = Vec::new();
    for (dir, tracks) in by_album_dir(library, table) {
        if dir.is_root() || album_of(&tracks).is_none() {
            continue;
        }
        let mut discs: BTreeMap<u32, BTreeMap<u32, Vec<&str>>> = BTreeMap::new();
        let mut unnumbered = Vec::new();
        // A file with nothing MPDFM can read is `untagged` or `id3v1-only`
        // already. It is not named again here as a track with no number, but it
        // still explains a gap, so it counts towards suppressing them.
        let mut blank = 0;
        for (entry, tags) in &tracks {
            if tags.is_empty() {
                blank += 1;
                continue;
            }
            match tags.track {
                Some((number, _)) => discs
                    .entry(tags.disc.map_or(1, |(disc, _)| disc))
                    .or_default()
                    .entry(number)
                    .or_default()
                    .push(entry.file_name()),
                None => unnumbered.push(entry.file_name()),
            }
        }
        // Numbered tracks are what make the gaps meaningful; an album with none
        // at all is a `missing` story told better by `tag show`.
        if discs.is_empty() {
            continue;
        }

        let mut problems = Vec::new();
        let several = discs.len() > 1;
        for (disc, numbers) in &discs {
            let on = if several {
                format!(" on disc {disc}")
            } else {
                String::new()
            };
            for (number, files) in numbers.iter().filter(|(_, files)| files.len() > 1) {
                problems.push(format!("track {number}{on} is {}", files.join(" and ")));
            }
            // A gap is only a gap when every track has a number: otherwise the
            // unnumbered ones are the likelier explanation, and they are named
            // below instead.
            if !unnumbered.is_empty() || blank > 0 {
                continue;
            }
            let highest = numbers.keys().max().copied().unwrap_or(0);
            let gaps: Vec<String> = (1..highest)
                .filter(|number| !numbers.contains_key(number))
                .map(|number| number.to_string())
                .collect();
            if !gaps.is_empty() {
                problems.push(format!("no track {}{on}", gaps.join(", ")));
            }
        }
        match unnumbered.len() {
            0 => {}
            // Few enough to name; past that, a list nobody reads.
            1..=5 => problems.push(format!("no track number on {}", unnumbered.join(", "))),
            count => problems.push(format!(
                "no track number on {count} of {} tracks",
                tracks.len()
            )),
        }
        if !problems.is_empty() {
            items.push(Item::new(dir.as_str(), problems.join("; ")));
        }
    }
    items
}

/// The earliest year a recording in this library could plausibly be from.
const EARLIEST_YEAR: u32 = 1860;

/// Dates that do not start with a plausible year.
///
/// `2004`, `2019-03-15` and `2004-00-00` all pass — the last is a common tagger
/// artifact and its year is fine. `04`, `0`, `20004` and a year in the future
/// do not. Every value of a multi-valued date is checked.
fn implausible_years(library: &Library, table: &Table) -> Vec<Item> {
    let latest = this_year() + 1;
    table
        .tagged(library)
        .filter_map(|(entry, tags, _)| {
            let bad: Vec<&String> = tags
                .date
                .all()
                .iter()
                .filter(|date| !date.trim().is_empty())
                .filter(|date| {
                    let year = TagSet {
                        date: Values::one(date.trim()),
                        ..TagSet::default()
                    }
                    .year();
                    !year.is_some_and(|year| (EARLIEST_YEAR..=latest).contains(&year))
                })
                .collect();
            (!bad.is_empty()).then(|| {
                let shown: Vec<String> = bad.iter().map(|date| format!("{date:?}")).collect();
                Item::new(entry.rel.as_str(), format!("date {}", shown.join(", ")))
            })
        })
        .collect()
}

/// The current year, near enough: within a day of New Year it may be one off,
/// which the `+ 1` of tolerance above absorbs.
fn this_year() -> u32 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    // The mean Gregorian year, in seconds.
    1970 + u32::try_from(seconds / 31_556_952).unwrap_or(0)
}

/// What `same-song` compares: artist and title, NFC and lower case, or `None`
/// when either is missing — a track with no title is not the same song as
/// another track with no title.
pub(crate) fn song_key(tags: &TagSet) -> Option<(String, String)> {
    use unicode_normalization::UnicodeNormalization as _;
    let fold = |values: &Values| -> Option<String> {
        first(values).map(|value| value.nfc().collect::<String>().to_lowercase())
    };
    Some((fold(&tags.artist)?, fold(&tags.title)?))
}
