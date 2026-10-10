//! Reading a [`TagSet`] and an [`AudioInfo`] out of a file, by looking at its
//! bytes rather than at its name.
//!
//! ```no_run
//! use camino::Utf8Path;
//!
//! let (tags, info) = mpdfm_core::tags::read(Utf8Path::new("/home/me/Music/a.mp3"))?;
//! println!("{} — {} [{info}]", tags.artist, tags.title);
//! # Ok::<(), mpdfm_core::tags::TagError>(())
//! ```
//!
//! # The container is decided by the content, and the name is only a fallback
//!
//! [`lofty::probe::Probe::guess_file_type`] first: this library holds scene
//! releases, and a `.flac` that is really an mp3 is a thing that happens, so a
//! reader that trusted the name would show an error for a file every other player
//! plays. What the bytes say is what [`AudioInfo::format`] reports, so a
//! mislabelled file is visible rather than merely tolerated.
//!
//! The extension is consulted **only when the bytes say nothing at all**, which on
//! this machine means a damaged file: 18 of the real library's mp3s have more
//! padding between their ID3v2 tag and their first MPEG frame than any probe will
//! search, and most of those are 55–95 % zero bytes — broken downloads. Reading
//! them is still worth doing, because their tags are intact and are how the user
//! will recognize what to replace. **Writing them is not**, and
//! [`write::preflight`][super::write::preflight] refuses it: see [`Detected`].
//!
//! # Two passes over one tag, and why
//!
//! The modeled fields come from `lofty`'s generic [`Tag`], reached through
//! [`SplitTag`], because that is where `TRCK 5/12` becomes a number and a total,
//! where a numeric `TCON` becomes `Rock`, where an ID3v2.3 `TYER` has already
//! been upgraded to `TDRC`, and where an ID3v2.4 multi-value frame has already
//! been split. Re-deriving any of that here would be re-deriving it worse.
//!
//! [`TagSet::extra`] comes from the **native** tag instead — the frame list, the
//! comment list, the atom list — because the generic `Tag` synthesizes items that
//! are not in the file (a FLAC's vendor string arrives as `EncoderSoftware` so
//! that a write can put it back) and drops the native spelling of the ones that
//! are. `extra` exists to show the user what is in their file, so it is read from
//! the file.
//!
//! # What is refused
//!
//! A container MPDFM does not edit is [`TagError::Unsupported`], naming the file
//! and what the bytes look like. Reading the real library finds exactly one:
//! an ADTS AAC stream named `.mp3`, which MPD plays happily. That is the honest
//! answer rather than a fourth [`Format`] — `docs/PLAN.md` D5 scopes v1 to mp3
//! and FLAC with m4a for free — and `doctor` (task 29) is where such a file
//! should be reported, not here.
//!
//! # Cost
//!
//! [`read_tags`] parses tags and **not** audio properties, which is the pass over
//! the frame headers. That is the call the browser makes for the forty rows on
//! screen (task 22); [`read`] is for the one file the editor has open. Both count
//! themselves in [`library::audio_reads`][crate::library::audio_reads], which is
//! what keeps `no_tag_io_during_scan` honest.

use camino::Utf8Path;
use lofty::config::ParseOptions;
use lofty::file::{AudioFile as _, FileType};
use lofty::flac::FlacFile;
use lofty::id3::v2::{Frame, Id3v2Tag};
use lofty::mp4::{AtomData, AtomIdent, Ilst, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::ogg::tag::VorbisComments;
use lofty::prelude::{ItemKey, SplitTag as _};
use lofty::probe::Probe;
use lofty::properties::FileProperties;
use lofty::tag::{Tag, TagType};

use crate::library::{self, Format};
use crate::paths::RelPath;

use super::model::{AudioInfo, Field, TagSet, Values};
use super::{Parsed, TagError};

/// Read one file's tags and audio properties.
///
/// # Errors
///
/// [`TagError::Unreadable`] when the file cannot be opened,
/// [`TagError::Unsupported`] when its bytes are not a container MPDFM edits, and
/// [`TagError::Corrupt`] when they are but cannot be parsed. Every one names the
/// path, because a scan that reports "a corrupt tag" without saying which file is
/// not worth reporting.
pub fn read(abs: &Utf8Path) -> Result<(TagSet, AudioInfo), TagError> {
    let (native, properties, format) = open(abs, parse_options(true))?;
    Ok((tag_set(native), audio_info(&properties, format)))
}

/// Read one file's tags, skipping the audio properties.
///
/// The cheap call: no pass over the frame headers, which is most of the cost of
/// opening a file. Use it wherever the duration and the bitrate are not going to
/// be shown.
///
/// # Errors
///
/// As [`read`].
pub fn read_tags(abs: &Utf8Path) -> Result<TagSet, TagError> {
    let (native, _, _) = open(abs, parse_options(false))?;
    Ok(tag_set(native))
}

/// Which tag blocks a file carries, beyond what its [`TagSet`] shows.
///
/// A [`TagSet`] is read from the file's *primary* tag only — ID3v2 for an mp3 —
/// so an mp3 carrying nothing but an ID3v1 tag reads as empty, exactly like one
/// with no tag at all. Those are different situations for `doctor` (task 29):
/// one has metadata MPD can see and MPDFM cannot edit, the other has none. This
/// is what tells them apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TagLayout {
    /// The container's own tag is there: ID3v2, a Vorbis comment block, `ilst`.
    pub primary: bool,
    /// An ID3v1 tag is there. Only ever true for an mp3.
    pub id3v1: bool,
}

/// [`read_tags`], plus which tag blocks the file carries.
///
/// The same single parse, so `doctor` pays nothing extra for knowing.
///
/// # Errors
///
/// As [`read`].
pub fn read_tags_with_layout(abs: &Utf8Path) -> Result<(TagSet, TagLayout), TagError> {
    let (native, _, _) = open(abs, parse_options(false))?;
    let layout = native.layout();
    Ok((tag_set(native), layout))
}

/// Read the tags of many files, keeping each one's own answer.
///
/// One entry per input path, in the order given, each holding either its
/// [`TagSet`] or the error that file alone produced — **the scan does not stop**.
/// One corrupt track in an album of fourteen must not take the other thirteen's
/// metadata away from the user, which is the whole reason this returns a list of
/// results rather than a result of a list.
#[must_use]
pub fn read_many(paths: &[RelPath], root: &Utf8Path) -> Vec<(RelPath, Result<TagSet, TagError>)> {
    paths
        .iter()
        .map(|rel| (rel.clone(), read_tags(&rel.to_abs(root))))
        .collect()
}

/// How a file's container was worked out.
///
/// The distinction is load-bearing rather than informational. `lofty` can only
/// write a tag into a file whose container it can identify **from the bytes** —
/// the writer is handed an open file and has no name to fall back on — so a file
/// that got here by its extension is one a write will fail on, after it has
/// already taken a backup. Knowing which happened is what lets the refusal come
/// out of preflight instead, with a message about the file rather than about
/// `lofty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Detected {
    /// The bytes said so. The normal case, and the only one a write accepts.
    Content,
    /// The bytes said nothing and the extension was believed.
    Extension,
}

/// What container `abs` holds, and how that was decided.
///
/// The cheap half of [`open`]: enough to refuse a file MPDFM cannot edit without
/// parsing a tag, which is what a bulk edit's preflight needs for every file in
/// the selection.
///
/// # Errors
///
/// [`TagError::Unreadable`] when the file cannot be opened and
/// [`TagError::Unsupported`] when neither its bytes nor its name make it a
/// container MPDFM edits.
pub(super) fn detect(abs: &Utf8Path) -> Result<(Format, Detected), TagError> {
    // Deliberately `Probe::new` and not `Probe::open`: the latter seeds the type
    // from the extension before anything is read, which would make the two cases
    // indistinguishable.
    let file = std::fs::File::open(abs).map_err(|source| TagError::unreadable(abs, source))?;
    let probe = Probe::new(std::io::BufReader::new(file))
        .options(parse_options(false))
        .guess_file_type()
        .map_err(|source| TagError::unreadable(abs, source))?;

    if let Some(file_type) = probe.file_type() {
        return Ok((format_of(abs, Some(file_type))?, Detected::Content));
    }
    let by_name = FileType::from_path(abs.as_std_path());
    Ok((format_of(abs, by_name)?, Detected::Extension))
}

/// How much junk MPDFM will search past looking for the start of the audio.
///
/// `lofty`'s own default is 1 KiB, and its documentation says some files need
/// more. Two albums in the real library have just over 2 KiB of zero padding
/// between their ID3v2 tag and their first MPEG frame, which is padding and not
/// damage — so the budget is 64 KiB, comfortably past that and still bounded.
/// Beyond it the file is not padded, it is broken: the ones that need megabytes
/// are 55–95 % zero bytes.
pub(super) const MAX_JUNK_BYTES: usize = 64 * 1024;

/// The parse options every read in this module uses.
pub(super) fn parse_options(properties: bool) -> ParseOptions {
    ParseOptions::new()
        .read_properties(properties)
        .max_junk_bytes(MAX_JUNK_BYTES)
}

/// The [`Format`] a `lofty` file type names, or the refusal for one MPDFM does
/// not edit.
fn format_of(abs: &Utf8Path, file_type: Option<FileType>) -> Result<Format, TagError> {
    match file_type {
        Some(FileType::Mpeg) => Ok(Format::Mp3),
        Some(FileType::Flac) => Ok(Format::Flac),
        Some(FileType::Mp4) => Ok(Format::M4a),
        Some(other) => Err(TagError::Unsupported {
            path: abs.to_owned(),
            detected: Some(format!("{other:?}").to_lowercase()),
        }),
        None => Err(TagError::Unsupported {
            path: abs.to_owned(),
            detected: None,
        }),
    }
}

// ---------------------------------------------------------------------------

/// Open `abs`, work out what it is, and hand back its native tag.
///
/// `options` decides whether the audio properties are parsed; the properties
/// come back either way and are [`FileProperties::default`] — every field zero —
/// when they were not asked for.
pub(super) fn open(
    abs: &Utf8Path,
    options: ParseOptions,
) -> Result<(Parsed, FileProperties, Format), TagError> {
    // `Probe::open` reports both "there is no such file" and "those bytes are
    // not a container I know", so its failure is read as the former only when the
    // path really cannot be opened; otherwise the file is there and unparseable.
    let probe = Probe::open(abs)
        .map_err(|source| TagError::corrupt(abs, source))?
        .options(options)
        .guess_file_type()
        .map_err(|source| TagError::unreadable(abs, source))?;

    let format = format_of(abs, probe.file_type())?;

    // Every path that gets this far opens the file and reads from it, which is
    // the number `library::audio_reads` counts.
    library::record_audio_read();
    let reader = &mut probe.into_inner();
    let corrupt = |source| TagError::corrupt(abs, source);

    Ok(match format {
        Format::Mp3 => {
            let file = MpegFile::read_from(reader, options).map_err(corrupt)?;
            let properties = FileProperties::from(*file.properties());
            (Parsed::Mpeg(Box::new(file)), properties, format)
        }
        Format::Flac => {
            let file = FlacFile::read_from(reader, options).map_err(corrupt)?;
            let properties = FileProperties::from(*file.properties());
            (Parsed::Flac(Box::new(file)), properties, format)
        }
        Format::M4a => {
            let file = Mp4File::read_from(reader, options).map_err(corrupt)?;
            let properties = FileProperties::from(file.properties().clone());
            (Parsed::Mp4(Box::new(file)), properties, format)
        }
    })
}

/// `lofty`'s properties, in our shape.
///
/// `0` for anything the container would not say, rather than an `Option` per
/// field: every caller of this renders it, and `0 kbps` reads the same as
/// `unknown` without making five call sites unwrap.
fn audio_info(properties: &FileProperties, format: Format) -> AudioInfo {
    AudioInfo {
        duration: properties.duration(),
        bitrate: properties
            .audio_bitrate()
            .or_else(|| properties.overall_bitrate())
            .unwrap_or(0),
        sample_rate: properties.sample_rate().unwrap_or(0),
        channels: properties.channels().unwrap_or(0),
        format,
    }
}

// ---------------------------------------------------------------------------

/// The whole of the read: `extra` from the native tag, the modeled fields from
/// the generic one.
fn tag_set(parsed: Parsed) -> TagSet {
    let (extra, generic) = match parsed.into_tag() {
        Some(NativeTagData::Id3v2(tag)) => (id3v2_extra(&tag), tag.split_tag().1),
        Some(NativeTagData::Vorbis(tag)) => (vorbis_extra(&tag), tag.split_tag().1),
        Some(NativeTagData::Ilst(tag)) => (ilst_extra(&tag), tag.split_tag().1),
        // No tag at all is an empty `TagSet`, not an error.
        None => return TagSet::default(),
    };

    TagSet {
        title: values(&generic, ItemKey::TrackTitle),
        artist: values(&generic, ItemKey::TrackArtist),
        album_artist: values(&generic, ItemKey::AlbumArtist),
        album: values(&generic, ItemKey::AlbumTitle),
        // `RecordingDate` is `TDRC` / `DATE`, which is where a v2.3 `TYER` has
        // already been upgraded to. `Year` is the FLAC-only `YEAR`, and is the
        // fallback rather than the first choice because a file with both means
        // the full date.
        date: {
            let date = values(&generic, ItemKey::RecordingDate);
            if date.is_empty() {
                values(&generic, ItemKey::Year)
            } else {
                date
            }
        },
        track: pair(&generic, ItemKey::TrackNumber, ItemKey::TrackTotal),
        disc: pair(&generic, ItemKey::DiscNumber, ItemKey::DiscTotal),
        genre: values(&generic, ItemKey::Genre),
        comment: undescribed(&generic, ItemKey::Comment),
        composer: values(&generic, ItemKey::Composer),
        extra,
    }
}

/// Every text value under `key`, in file order.
fn values(tag: &Tag, key: ItemKey) -> Values {
    Values::of(tag.get_strings(key).map(str::to_owned))
}

/// Every text value under `key` whose **description is empty**.
///
/// ID3v2 distinguishes several `COMM` frames by their description, and a real
/// file in this library has three: `COMM:` holding `vtwin88cube`,
/// `COMM:Catalog Number` and `COMM:MusicMatch_Preference`. Only the first is the
/// comment; the other two are metadata somebody's tagger left behind, and they
/// belong in [`TagSet::extra`] where a write leaves them alone.
///
/// Reading all three as "the comment" is what the writer could not then honour:
/// clearing the comment removes the `COMM` frame the spec says is the comment,
/// and the file would still read as having two. The reader and the writer have to
/// mean the same thing by a field, and this is where that is decided.
///
/// Vorbis comments and MP4 atoms have no description, so this is the same as
/// [`values`] for them.
fn undescribed(tag: &Tag, key: ItemKey) -> Values {
    Values::of(
        tag.get_items(key)
            .filter(|item| item.description().is_empty())
            .filter_map(|item| item.value().text())
            .map(str::to_owned),
    )
}

/// A number and its total, when the number is there and parses.
///
/// A total with no number is dropped: `TRCK /12` describes nothing, and
/// inventing a number for it would be inventing data.
fn pair(tag: &Tag, number: ItemKey, total: ItemKey) -> Option<(u32, Option<u32>)> {
    let number = tag.get_string(number)?.trim().parse().ok()?;
    let total = tag
        .get_string(total)
        .and_then(|total| total.trim().parse().ok());
    Some((number, total))
}

/// The native keys each modeled field is read out of and written to, per
/// container.
///
/// One table, used three ways: to read a field, to decide what counts as
/// [`TagSet::extra`] (anything not in it), and — by [`write`][super::write] — to
/// take a field's old spellings out of the way before a new value goes in. A
/// field added to [`Field`] and not added here would be read as itself *and*
/// reported as an extra, which is what `every_modeled_field_has_a_native_key`
/// checks.
///
/// Several keys per field where a container has more than one spelling in the
/// wild: a FLAC's total may be `TRACKTOTAL` or `TOTALTRACKS`, and an ID3v2.3
/// year is `TYER` plus `TDAT` where v2.4 has one `TDRC`.
pub(super) fn native_keys(tag_type: TagType, field: Field) -> &'static [&'static str] {
    match tag_type {
        TagType::Id3v2 => match field {
            Field::Title => &["TIT2"],
            Field::Artist => &["TPE1"],
            Field::AlbumArtist => &["TPE2"],
            Field::Album => &["TALB"],
            Field::Year => &["TDRC", "TYER", "TDAT", "TIME"],
            Field::Track => &["TRCK"],
            Field::Disc => &["TPOS"],
            Field::Genre => &["TCON"],
            Field::Comment => &["COMM"],
            Field::Composer => &["TCOM"],
        },
        TagType::VorbisComments => match field {
            Field::Title => &["TITLE"],
            Field::Artist => &["ARTIST"],
            Field::AlbumArtist => &["ALBUMARTIST", "ALBUM ARTIST"],
            Field::Album => &["ALBUM"],
            Field::Year => &["DATE", "YEAR"],
            Field::Track => &["TRACKNUMBER", "TRACKTOTAL", "TOTALTRACKS"],
            Field::Disc => &["DISCNUMBER", "DISCTOTAL", "TOTALDISCS"],
            Field::Genre => &["GENRE"],
            Field::Comment => &["COMMENT", "DESCRIPTION"],
            Field::Composer => &["COMPOSER"],
        },
        TagType::Mp4Ilst => match field {
            Field::Title => &["\u{a9}nam"],
            Field::Artist => &["\u{a9}ART"],
            Field::AlbumArtist => &["aART"],
            Field::Album => &["\u{a9}alb"],
            Field::Year => &["\u{a9}day"],
            Field::Track => &["trkn"],
            Field::Disc => &["disk"],
            Field::Genre => &["\u{a9}gen", "gnre"],
            Field::Comment => &["\u{a9}cmt"],
            Field::Composer => &["\u{a9}wrt"],
        },
        // Every other tag type is one MPDFM does not edit; nothing is modeled in
        // it, so a reader that reached here would report the whole tag as extra.
        _ => &[],
    }
}

/// Whether `key` is one of the ten modeled fields in this container.
///
/// Everything a file holds that this says `false` about is [`TagSet::extra`].
/// Case-insensitively, because FLAC comment keys are and ID3v2 frame ids are
/// upper-case by definition.
fn is_modeled(tag_type: TagType, key: &str) -> bool {
    crate::tags::model::FIELDS.iter().any(|field| {
        native_keys(tag_type, *field)
            .iter()
            .any(|known| known.eq_ignore_ascii_case(key))
    })
}

/// ID3v2's textual frames that are not one of the ten, under their frame id.
///
/// `TXXX` frames are listed as `TXXX:description`, which is how every other
/// tagger names them and the only way two of them can be told apart. Binary
/// frames — `APIC`, `GEOB`, `POPM` — are left out: they are preserved by a write
/// regardless, and there is no string to show.
fn id3v2_extra(tag: &Id3v2Tag) -> Vec<(String, String)> {
    let mut extra = Vec::new();
    for frame in tag {
        let id = frame.id_str();
        // A described `COMM` is not the comment — see `undescribed` — so it is
        // listed here even though `COMM` is a modeled id.
        if let Frame::Comment(comment) = frame
            && !comment.description.is_empty()
        {
            extra.push((
                format!("COMM:{}", comment.description),
                comment.content.to_string(),
            ));
            continue;
        }
        if is_modeled(TagType::Id3v2, id) {
            continue;
        }
        match frame {
            Frame::Text(text) => extra.push((id.to_owned(), text.value.to_string())),
            Frame::UserText(user) => extra.push((
                format!("TXXX:{}", user.description),
                user.content.to_string(),
            )),
            Frame::Url(url) => extra.push((id.to_owned(), url.url().to_owned())),
            Frame::UserUrl(user) => extra.push((
                format!("WXXX:{}", user.description),
                user.content.to_string(),
            )),
            Frame::UnsynchronizedText(lyrics) => extra.push((
                format!("USLT:{}", lyrics.description),
                lyrics.content.to_string(),
            )),
            Frame::Timestamp(stamp) => extra.push((id.to_owned(), stamp.timestamp.to_string())),
            _ => {}
        }
    }
    extra
}

/// A FLAC's comments that are not one of the ten, under their own key.
///
/// Read from the comment list itself rather than through the generic tag, so the
/// spelling is the file's — `MUSICBRAINZ_ALBUMID`, not a normalized name — and so
/// that `lofty`'s synthesized vendor-string item does not turn up as metadata
/// that is not in the file.
fn vorbis_extra(tag: &VorbisComments) -> Vec<(String, String)> {
    tag.items()
        .filter(|(key, _): &(&str, &str)| !is_modeled(TagType::VorbisComments, key))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// An MP4's atoms that are not one of the ten, under their identifier.
///
/// A freeform atom is spelled the way the container does,
/// `----:com.apple.iTunes:NAME`, which is what makes two of them distinguishable.
fn ilst_extra(tag: &Ilst) -> Vec<(String, String)> {
    let mut extra = Vec::new();
    for atom in tag {
        let name = match atom.ident() {
            AtomIdent::Fourcc(fourcc) => fourcc_name(fourcc),
            AtomIdent::Freeform { mean, name } => format!("----:{mean}:{name}"),
        };
        if is_modeled(TagType::Mp4Ilst, &name) {
            continue;
        }
        for data in atom.data() {
            match data {
                AtomData::UTF8(text) | AtomData::UTF16(text) => {
                    extra.push((name.clone(), text.clone()));
                }
                _ => {}
            }
        }
    }
    extra
}

/// An atom's four-byte identifier as text: `©nam`, `trkn`, `aART`.
///
/// Byte by byte rather than as UTF-8, because the leading `0xA9` of `©nam` is not
/// valid UTF-8 on its own — `String::from_utf8_lossy` turns the whole identifier
/// into a replacement character and loses the name.
fn fourcc_name(fourcc: &[u8; 4]) -> String {
    fourcc
        .iter()
        .map(|byte| {
            if *byte == 0xa9 {
                '\u{a9}'
            } else {
                char::from(*byte)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------

/// The native tag of a parsed file, taken out of it.
///
/// A separate enum from [`Parsed`] because the three remainder types of
/// [`SplitTag`] are different types: the split has to happen inside one match
/// arm per container, and this is what gets it there.
pub(super) enum NativeTagData {
    /// An ID3v2 tag, as mp3 carries.
    Id3v2(Id3v2Tag),
    /// A FLAC's Vorbis comment block.
    Vorbis(VorbisComments),
    /// An MP4 `ilst` atom.
    Ilst(Ilst),
}

impl Parsed {
    /// Which tag blocks the parsed file carries.
    pub(super) fn layout(&self) -> TagLayout {
        match self {
            Self::Mpeg(file) => TagLayout {
                primary: file.id3v2().is_some(),
                id3v1: file.id3v1().is_some(),
            },
            Self::Flac(file) => TagLayout {
                primary: file.vorbis_comments().is_some(),
                id3v1: false,
            },
            Self::Mp4(file) => TagLayout {
                primary: file.ilst().is_some(),
                id3v1: false,
            },
        }
    }

    /// The file's primary tag, or `None` when it has none.
    ///
    /// For a FLAC that is the Vorbis comment block and deliberately not an
    /// ID3v2 tag it may also carry: MPD reads the comments, so that is what the
    /// user is editing, and a stray ID3v2 chunk in a FLAC is reported by
    /// `doctor` rather than written to.
    pub(super) fn into_tag(self) -> Option<NativeTagData> {
        match self {
            Self::Mpeg(file) => file.id3v2().cloned().map(NativeTagData::Id3v2),
            Self::Flac(file) => file.vorbis_comments().cloned().map(NativeTagData::Vorbis),
            Self::Mp4(file) => file.ilst().cloned().map(NativeTagData::Ilst),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_total_with_no_number_describes_nothing() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::TrackTotal, "12".to_owned());
        assert_eq!(pair(&tag, ItemKey::TrackNumber, ItemKey::TrackTotal), None);

        tag.insert_text(ItemKey::TrackNumber, "5".to_owned());
        assert_eq!(
            pair(&tag, ItemKey::TrackNumber, ItemKey::TrackTotal),
            Some((5, Some(12)))
        );
    }

    #[test]
    fn a_number_that_is_not_a_number_is_not_invented() {
        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert_text(ItemKey::TrackNumber, "A".to_owned());
        assert_eq!(pair(&tag, ItemKey::TrackNumber, ItemKey::TrackTotal), None);
    }

    #[test]
    fn every_modeled_field_has_a_native_key_in_every_container() {
        // The table is both the mapping and the `extra` filter, so a field
        // missing from it would show up as itself *and* as an extra.
        for tag_type in [TagType::Id3v2, TagType::VorbisComments, TagType::Mp4Ilst] {
            for field in super::super::model::FIELDS {
                assert!(
                    !native_keys(tag_type, field).is_empty(),
                    "{tag_type:?} has no key for {field}"
                );
                let key = match (tag_type, field) {
                    (TagType::Id3v2, Field::Title) => "TIT2",
                    (TagType::Id3v2, Field::Artist) => "TPE1",
                    (TagType::Id3v2, Field::AlbumArtist) => "TPE2",
                    (TagType::Id3v2, Field::Album) => "TALB",
                    (TagType::Id3v2, Field::Year) => "TDRC",
                    (TagType::Id3v2, Field::Track) => "TRCK",
                    (TagType::Id3v2, Field::Disc) => "TPOS",
                    (TagType::Id3v2, Field::Genre) => "TCON",
                    (TagType::Id3v2, Field::Comment) => "COMM",
                    (TagType::Id3v2, Field::Composer) => "TCOM",
                    (TagType::VorbisComments, Field::Title) => "TITLE",
                    (TagType::VorbisComments, Field::Artist) => "ARTIST",
                    (TagType::VorbisComments, Field::AlbumArtist) => "ALBUMARTIST",
                    (TagType::VorbisComments, Field::Album) => "ALBUM",
                    (TagType::VorbisComments, Field::Year) => "DATE",
                    (TagType::VorbisComments, Field::Track) => "TRACKNUMBER",
                    (TagType::VorbisComments, Field::Disc) => "DISCNUMBER",
                    (TagType::VorbisComments, Field::Genre) => "GENRE",
                    (TagType::VorbisComments, Field::Comment) => "COMMENT",
                    (TagType::VorbisComments, Field::Composer) => "COMPOSER",
                    (TagType::Mp4Ilst, Field::Title) => "\u{a9}nam",
                    (TagType::Mp4Ilst, Field::Artist) => "\u{a9}ART",
                    (TagType::Mp4Ilst, Field::AlbumArtist) => "aART",
                    (TagType::Mp4Ilst, Field::Album) => "\u{a9}alb",
                    (TagType::Mp4Ilst, Field::Year) => "\u{a9}day",
                    (TagType::Mp4Ilst, Field::Track) => "trkn",
                    (TagType::Mp4Ilst, Field::Disc) => "disk",
                    (TagType::Mp4Ilst, Field::Genre) => "\u{a9}gen",
                    (TagType::Mp4Ilst, Field::Comment) => "\u{a9}cmt",
                    (TagType::Mp4Ilst, Field::Composer) => "\u{a9}wrt",
                    _ => unreachable!("every container covers every field"),
                };
                assert!(
                    is_modeled(tag_type, key),
                    "{tag_type:?} {field} ({key}) is not in the modeled list"
                );
            }
        }
    }

    #[test]
    fn an_atom_identifier_keeps_its_copyright_sign() {
        assert_eq!(fourcc_name(&[0xa9, b'n', b'a', b'm']), "\u{a9}nam");
        assert_eq!(fourcc_name(b"aART"), "aART");
    }

    #[test]
    fn an_unmodeled_key_is_extra() {
        assert!(!is_modeled(TagType::Id3v2, "TBPM"));
        assert!(!is_modeled(
            TagType::VorbisComments,
            "REPLAYGAIN_TRACK_GAIN"
        ));
        assert!(!is_modeled(TagType::Mp4Ilst, "----:com.apple.iTunes:FOO"));
        // Case-insensitively, because FLAC comment keys are.
        assert!(is_modeled(TagType::VorbisComments, "Artist"));
    }
}
