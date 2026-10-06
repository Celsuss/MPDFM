//! The one shape metadata has, whatever container it came out of.
//!
//! [`TagSet`] is read from an ID3v2 tag, a FLAC Vorbis comment block or an MP4
//! `ilst` atom and looks the same either way, which is the whole point: the
//! editor, the bulk view, `organize`'s template engine and `tag show` all read
//! this and none of them asks what the file is (`docs/tasks/16-tag-read.md`).
//!
//! # Three decisions the task left open
//!
//! **Multi-valued fields are kept, not joined.** A FLAC may legitimately carry
//! three `ARTIST` comments and an ID3v2.4 frame may carry three values in one
//! frame, so every text field is a [`Values`] — an ordered list — rather than a
//! `String`. The alternative the task offered (join with `; ` and remember that
//! it was multi-valued) needs the same two pieces of information and then has to
//! guess which semicolons were separators when it writes them back. Rendering is
//! [`Values::joined`], and it happens at the edge, in the front-end.
//!
//! **The year keeps its original string.** `DATE=2019-03-15` is normal in FLAC
//! and `TDRC` may hold a full timestamp, so [`TagSet::date`] holds exactly what
//! the file says and [`TagSet::year`] narrows it to a `u32` on demand. Nothing
//! stores the narrowed form, so no write can destroy the month and day — which
//! is what the task's pitfall asks for, without the indirection of stashing the
//! original in [`TagSet::extra`].
//!
//! **Nothing is normalized on read.** No trimming, no case folding, no "fixing"
//! a stray `(17)`. The editor has to show what is in the file or the user cannot
//! tell what they are about to change. The one exception is a numeric `TCON`
//! genre reference, which is *resolved* rather than normalized — `(17)` means
//! `Rock` and showing the digits would be showing the encoding rather than the
//! value.

use std::time::Duration;

use crate::library::Format;

/// One field's value, as the file holds it.
///
/// Ordered, and empty when the field is absent — there is no separate `None`,
/// because "the frame is missing" and "the frame is there and holds nothing" are
/// not a distinction any of these containers reliably keeps, and a model that
/// claimed to keep it would be lying in one direction or the other.
///
/// ```
/// use mpdfm_core::tags::Values;
///
/// let one = Values::one("MF DOOM");
/// assert_eq!(one.first(), Some("MF DOOM"));
/// assert!(!one.is_multi());
///
/// let three = Values::of(["Madvillain", "MF DOOM", "Madlib"]);
/// assert_eq!(three.joined(), "Madvillain; MF DOOM; Madlib");
/// assert!(three.is_multi());
///
/// assert!(Values::default().is_empty());
/// ```
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct Values(Vec<String>);

/// What [`Values::joined`] puts between two values, and what the CLI splits a
/// `--artist` argument on.
///
/// Semicolon-space, because that is what every other tagger in this ecosystem
/// shows and because it cannot occur inside a value by accident often enough to
/// matter. A value that genuinely holds one is round-tripped untouched on read;
/// only a *write* that is given this separator splits on it, and only because
/// the user typed it.
pub const SEPARATOR: &str = "; ";

impl Values {
    /// No value at all — an absent field.
    #[must_use]
    pub fn none() -> Self {
        Self(Vec::new())
    }

    /// Exactly one value.
    #[must_use]
    pub fn one(value: impl Into<String>) -> Self {
        Self(vec![value.into()])
    }

    /// Every value, in file order. An empty iterator is an absent field.
    pub fn of<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(values.into_iter().map(Into::into).collect())
    }

    /// What the user typed, split on [`SEPARATOR`].
    ///
    /// Only a write goes through here: reading never splits, because a value
    /// that holds a semicolon is a value and not two.
    #[must_use]
    pub fn typed(value: &str) -> Self {
        if value.is_empty() {
            return Self::none();
        }
        if !value.contains(SEPARATOR) {
            return Self::one(value);
        }
        Self::of(value.split(SEPARATOR).map(str::trim))
    }

    /// Whether the field is absent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many values there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there is more than one — the case a naive editor loses.
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.0.len() > 1
    }

    /// The first value, or `None` for an absent field.
    #[must_use]
    pub fn first(&self) -> Option<&str> {
        self.0.first().map(String::as_str)
    }

    /// Every value, in file order.
    #[must_use]
    pub fn all(&self) -> &[String] {
        &self.0
    }

    /// The values as one line, for display and for a one-line edit box.
    #[must_use]
    pub fn joined(&self) -> String {
        self.0.join(SEPARATOR)
    }
}

impl std::fmt::Display for Values {
    /// Padded like a string — see [`Field`]'s implementation for why that is not
    /// `write_str`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(&self.joined())
    }
}

impl From<&str> for Values {
    fn from(value: &str) -> Self {
        Self::one(value)
    }
}

impl From<String> for Values {
    fn from(value: String) -> Self {
        Self::one(value)
    }
}

// ---------------------------------------------------------------------------

/// A field MPDFM models, and therefore a field it can be asked to write.
///
/// The ten the task names. Anything else a file holds is read into
/// [`TagSet::extra`] and left alone — MPDFM shows it and never touches it, which
/// is the only honest thing to do with a frame it does not understand.
///
/// [`Ord`] is the order a field table is printed in, so the list below is the
/// display order and not alphabetical.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// `TIT2` / `TITLE` / `©nam`.
    Title,
    /// `TPE1` / `ARTIST` / `©ART`.
    Artist,
    /// `TPE2` / `ALBUMARTIST` / `aART`.
    AlbumArtist,
    /// `TALB` / `ALBUM` / `©alb`.
    Album,
    /// `TDRC` or `TYER` / `DATE` or `YEAR` / `©day`. Held as written: this is
    /// the field that is a full date as often as it is a year.
    Year,
    /// `TRCK` / `TRACKNUMBER` + `TRACKTOTAL` / `trkn`, as `n` or `n/total`.
    Track,
    /// `TPOS` / `DISCNUMBER` + `DISCTOTAL` / `disk`, as `n` or `n/total`.
    Disc,
    /// `TCON` / `GENRE` / `©gen`.
    Genre,
    /// `COMM` / `COMMENT` / `©cmt`.
    Comment,
    /// `TCOM` / `COMPOSER` / `©wrt`.
    Composer,
}

/// Every field, in display order.
pub const FIELDS: [Field; 10] = [
    Field::Title,
    Field::Artist,
    Field::AlbumArtist,
    Field::Album,
    Field::Year,
    Field::Track,
    Field::Disc,
    Field::Genre,
    Field::Comment,
    Field::Composer,
];

impl Field {
    /// The name `tag show` prints and `--clear <field>` accepts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::AlbumArtist => "albumartist",
            Self::Album => "album",
            Self::Year => "year",
            Self::Track => "track",
            Self::Disc => "disc",
            Self::Genre => "genre",
            Self::Comment => "comment",
            Self::Composer => "composer",
        }
    }

    /// The field a name means, accepting the spellings a user is likely to
    /// type.
    ///
    /// `album-artist`, `album_artist` and `albumartist` are one field, because
    /// the flag is `--album-artist` and the display name is `albumartist` and
    /// nobody should have to remember which.
    ///
    /// ```
    /// use mpdfm_core::tags::Field;
    ///
    /// assert_eq!(Field::parse("album-artist"), Some(Field::AlbumArtist));
    /// assert_eq!(Field::parse("GENRE"), Some(Field::Genre));
    /// assert_eq!(Field::parse("bpm"), None);
    /// ```
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let folded: String = name
            .chars()
            .filter(|c| *c != '-' && *c != '_' && *c != ' ')
            .flat_map(char::to_lowercase)
            .collect();
        FIELDS
            .into_iter()
            .find(|field| field.as_str().replace('-', "") == folded)
            .or(match folded.as_str() {
                "date" => Some(Self::Year),
                "tracknumber" | "trackno" => Some(Self::Track),
                "discnumber" | "discno" | "disk" => Some(Self::Disc),
                _ => None,
            })
    }

    /// Whether this field is a number, or a number and a total.
    ///
    /// The two that are cannot be typed into a bulk edit box — one value across
    /// a selection would give every track the same number — so task 18 offers
    /// them as named actions instead. This is the test it makes that decision
    /// with.
    #[must_use]
    pub fn is_per_file(self) -> bool {
        matches!(self, Self::Track | Self::Title)
    }
}

impl std::fmt::Display for Field {
    /// Through [`Formatter::pad`][std::fmt::Formatter::pad], not `write_str`, so
    /// that `{field:<12}` lines a field table up. `write_str` bypasses the
    /// formatter's width and silently ignores it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// A number and the total it is one of: `TRCK 5/12` is `(5, Some(12))`, a bare
/// `TRCK 5` is `(5, None)`.
pub type NumberPair = (u32, Option<u32>);

/// `5/12`, or `5` when there is no total — the spelling `--track` takes and
/// `tag show` prints.
#[must_use]
pub fn render_pair(pair: NumberPair) -> String {
    match pair {
        (number, Some(total)) => format!("{number}/{total}"),
        (number, None) => number.to_string(),
    }
}

/// Read `5/12`, `5`, `05/12` or `5 / 12` back.
///
/// Returns `None` for anything else, which is how a `--track one` reaches the
/// user as a refusal rather than as a silently dropped edit.
#[must_use]
pub fn parse_pair(text: &str) -> Option<NumberPair> {
    let (number, total) = match text.split_once('/') {
        Some((number, total)) => (number, Some(total)),
        None => (text, None),
    };
    let number = number.trim().parse().ok()?;
    let total = match total {
        Some(total) if total.trim().is_empty() => None,
        Some(total) => Some(total.trim().parse().ok()?),
        None => None,
    };
    Some((number, total))
}

// ---------------------------------------------------------------------------

/// Everything MPDFM knows about one file's metadata.
///
/// A file with no tags at all reads as [`TagSet::default`] — every field empty —
/// and not as an error: an untagged mp3 is the normal state of half a scene
/// release, and a reader that refused it would make the editor unable to open
/// exactly the files that need editing.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TagSet {
    /// Track title.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub title: Values,
    /// Track artist, which is genuinely several values often enough to matter.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub artist: Values,
    /// Album artist — what MPD groups a compilation by.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub album_artist: Values,
    /// Album title.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub album: Values,
    /// The date, exactly as the file spells it: `2004`, `2019-03-15`, or
    /// whatever else is in there. See [`TagSet::year`] to narrow it.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub date: Values,
    /// Track number and total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<NumberPair>,
    /// Disc number and total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc: Option<NumberPair>,
    /// Genre, with a numeric `TCON` reference already resolved to its name.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub genre: Values,
    /// Comment.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub comment: Values,
    /// Composer.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub composer: Values,

    /// Every other textual item the file holds, under the name the container
    /// gives it — `TXXX:replaygain_track_gain`, `MUSICBRAINZ_ALBUMID`,
    /// `----:com.apple.iTunes:ENCODER` — in file order.
    ///
    /// Read so the editor can show it and `doctor` can report on it. **Never
    /// written**: a tag write touches the fields in its delta and nothing else
    /// ([`write`][super::write]), so everything here survives untouched without
    /// MPDFM having to understand it. Binary items — artwork, `POPM`, `GEOB` —
    /// are preserved the same way and are deliberately not listed here, because
    /// there is no string to show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<(String, String)>,
}

impl TagSet {
    /// Whether the file holds no metadata at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The year, narrowed from [`TagSet::date`].
    ///
    /// The leading four digits of the first date value, which is right for
    /// `2004`, for `2019-03-15` and for `2004-00-00`. Nothing stores this, so
    /// narrowing it cannot lose the rest of the date.
    ///
    /// ```
    /// use mpdfm_core::tags::{TagSet, Values};
    ///
    /// let tags = TagSet { date: Values::one("2019-03-15"), ..TagSet::default() };
    /// assert_eq!(tags.year(), Some(2019));
    /// assert_eq!(tags.date.first(), Some("2019-03-15"));
    /// ```
    #[must_use]
    pub fn year(&self) -> Option<u32> {
        let date = self.date.first()?;
        let digits: String = date.chars().take_while(char::is_ascii_digit).collect();
        (digits.len() == 4).then(|| digits.parse().ok())?
    }

    /// One field's value, as text the user could have typed.
    ///
    /// The uniform accessor every caller that works field-by-field needs — the
    /// bulk view, the editor's rows, `tag diff`. `track` and `disc` come back as
    /// `5/12`, which is the spelling [`parse_pair`] reads and `--track` takes,
    /// so a value read here can be written back unchanged.
    #[must_use]
    pub fn get(&self, field: Field) -> Values {
        match field {
            Field::Title => self.title.clone(),
            Field::Artist => self.artist.clone(),
            Field::AlbumArtist => self.album_artist.clone(),
            Field::Album => self.album.clone(),
            Field::Year => self.date.clone(),
            Field::Track => self
                .track
                .map(render_pair)
                .map_or_else(Values::none, Values::one),
            Field::Disc => self
                .disc
                .map(render_pair)
                .map_or_else(Values::none, Values::one),
            Field::Genre => self.genre.clone(),
            Field::Comment => self.comment.clone(),
            Field::Composer => self.composer.clone(),
        }
    }

    /// Every modeled field with a value, in display order.
    pub fn present(&self) -> impl Iterator<Item = (Field, Values)> + '_ {
        FIELDS
            .into_iter()
            .map(|field| (field, self.get(field)))
            .filter(|(_, values)| !values.is_empty())
    }
}

// ---------------------------------------------------------------------------

/// What the audio itself is, as distinct from what it claims to be called.
///
/// Read only when somebody asks for it: it costs a pass over the frame headers,
/// which is the expensive half of opening a file, and the browser's rows do not
/// need it. [`read`][super::read::read] returns it; [`read_tags`][super::read::read_tags]
/// does not and is the one a screenful of rows goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AudioInfo {
    /// How long it plays for. Serialized as `duration_ms`, so the unit is in the
    /// name rather than in the documentation.
    #[serde(rename = "duration_ms", with = "millis")]
    pub duration: Duration,
    /// Audio bitrate in kbps, or 0 when the container would not say.
    pub bitrate: u32,
    /// Sample rate in Hz, or 0 when the container would not say.
    pub sample_rate: u32,
    /// Channel count, or 0 when the container would not say.
    pub channels: u8,
    /// The container, **as detected from the bytes** and not from the name.
    pub format: Format,
}

impl AudioInfo {
    /// `3:42`, or `1:02:03` for something over an hour.
    #[must_use]
    pub fn duration_hms(&self) -> String {
        let total = self.duration.as_secs();
        let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
        if hours > 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes}:{seconds:02}")
        }
    }
}

impl std::fmt::Display for AudioInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}, {} kbps, {} Hz, {} ch",
            self.format,
            self.duration_hms(),
            self.bitrate,
            self.sample_rate,
            self.channels
        )
    }
}

/// [`AudioInfo::duration`] as whole milliseconds, so `--json` output is a
/// number a script can compare rather than `serde`'s two-field struct.
mod millis {
    use std::time::Duration;

    use serde::{Deserialize as _, Deserializer, Serializer};

    /// Write the duration as milliseconds.
    pub(super) fn serialize<S: Serializer>(
        duration: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    }

    /// Read back what [`serialize`] wrote.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_name_is_accepted_however_it_is_spelled() {
        for spelling in [
            "albumartist",
            "album-artist",
            "album_artist",
            "Album Artist",
        ] {
            assert_eq!(
                Field::parse(spelling),
                Some(Field::AlbumArtist),
                "{spelling}"
            );
        }
        assert_eq!(Field::parse("date"), Some(Field::Year));
        assert_eq!(Field::parse(""), None);
        assert_eq!(Field::parse("replaygain_track_gain"), None);
    }

    #[test]
    fn a_number_pair_round_trips_through_its_own_spelling() {
        for text in ["5/12", "5"] {
            let pair = parse_pair(text).expect("a pair");
            assert_eq!(render_pair(pair), text);
        }
        assert_eq!(parse_pair("05/12"), Some((5, Some(12))));
        assert_eq!(parse_pair("5 / 12"), Some((5, Some(12))));
        assert_eq!(parse_pair("5/"), Some((5, None)));
        assert_eq!(parse_pair("one"), None);
        assert_eq!(parse_pair("5/twelve"), None);
        assert_eq!(parse_pair(""), None);
    }

    #[test]
    fn a_year_is_narrowed_without_the_date_being_lost() {
        let full = TagSet {
            date: Values::one("2019-03-15"),
            ..TagSet::default()
        };
        assert_eq!(full.year(), Some(2019));
        assert_eq!(full.get(Field::Year).first(), Some("2019-03-15"));

        let bare = TagSet {
            date: Values::one("2004"),
            ..TagSet::default()
        };
        assert_eq!(bare.year(), Some(2004));

        // Not a date at all: no year, and the string is still there to show.
        let odd = TagSet {
            date: Values::one("MMIV"),
            ..TagSet::default()
        };
        assert_eq!(odd.year(), None);
        assert_eq!(odd.get(Field::Year).first(), Some("MMIV"));
    }

    #[test]
    fn values_are_only_split_when_a_user_typed_the_separator() {
        // Read: one value, semicolon and all.
        let read = Values::one("Bob; Carol");
        assert_eq!(read.len(), 1);

        // Typed: two values.
        assert_eq!(Values::typed("Bob; Carol").all(), ["Bob", "Carol"]);
        assert_eq!(Values::typed("Bob").all(), ["Bob"]);
        assert!(Values::typed("").is_empty());
    }

    #[test]
    fn a_pair_field_reads_back_as_what_a_flag_would_take() {
        let tags = TagSet {
            track: Some((5, Some(12))),
            disc: Some((1, None)),
            ..TagSet::default()
        };
        assert_eq!(tags.get(Field::Track).first(), Some("5/12"));
        assert_eq!(tags.get(Field::Disc).first(), Some("1"));
        assert!(TagSet::default().get(Field::Track).is_empty());
    }

    #[test]
    fn an_empty_tag_set_is_empty_and_lists_nothing() {
        let empty = TagSet::default();
        assert!(empty.is_empty());
        assert_eq!(empty.present().count(), 0);
    }

    #[test]
    fn a_duration_reads_as_minutes_until_it_is_an_hour_long() {
        let info = |secs| AudioInfo {
            duration: Duration::from_secs(secs),
            bitrate: 320,
            sample_rate: 44_100,
            channels: 2,
            format: Format::Mp3,
        };
        assert_eq!(info(0).duration_hms(), "0:00");
        assert_eq!(info(222).duration_hms(), "3:42");
        assert_eq!(info(3723).duration_hms(), "1:02:03");
    }
}
