//! Tag fixtures: the cases tasks 16 to 18 need and that `ffmpeg` will not make.
//!
//! The five committed templates in `crates/core/tests/data/` cover the ordinary
//! shapes. What they cannot cover is anything that needs a tag *writer* to
//! construct — three `ARTIST` comments in one FLAC, a numeric `TCON`, an embedded
//! cover — and anything deliberately broken. Those are built here, from a copy of
//! a template, now that `lofty` is a dependency of core.
//!
//! Each function takes an **absolute path to a file that already exists** (a
//! fixture track) and rewrites its tag in place. They panic on failure, like the
//! rest of [`testing`][super]: a fixture that cannot be built is a broken test.
//!
//! ```
//! use mpdfm_core::testing::{Fixture, tags};
//!
//! let fx = Fixture::builder().flac_album("jazz/Kind of Blue").build();
//! let track = fx.abs("jazz/Kind of Blue/01 So What.flac");
//! tags::set_multi_valued(&track, "ARTIST", &["Miles Davis", "John Coltrane"]);
//!
//! let read = mpdfm_core::tags::read_tags(&track).expect("the fixture is readable");
//! assert_eq!(read.artist.all(), ["Miles Davis", "John Coltrane"]);
//! ```

use std::borrow::Cow;

use camino::Utf8Path;
use lofty::config::{ParseOptions, WriteOptions};
use lofty::flac::FlacFile;
use lofty::id3::v2::{
    AttachedPictureFrame, CommentFrame, Frame, FrameId, Id3v2Tag, TextInformationFrame,
};
use lofty::mp4::Mp4File;
use lofty::mpeg::MpegFile;
use lofty::ogg::OggPictureStorage as _;
use lofty::ogg::tag::VorbisComments;
use lofty::picture::{MimeType, Picture, PictureType};

use lofty::probe::Probe;
use lofty::{TextEncoding, file::FileType};

/// Give a FLAC comment several values, as a legitimate FLAC may have.
///
/// The case a naive reader truncates to the first and a naive writer collapses
/// into one: `docs/tasks/16-tag-read.md` names it as an acceptance criterion, and
/// there is no way to produce it with `ffmpeg -metadata`.
///
/// # Panics
///
/// If `path` is not a FLAC, or cannot be read or written.
pub fn set_multi_valued(path: &Utf8Path, key: &str, values: &[&str]) {
    let mut file = flac(path);
    let comments = vorbis_comments(&mut file);
    let _ = comments.remove(key).count();
    for value in values {
        comments.push(key.to_owned(), (*value).to_owned());
    }
    save(path, &file);
}

/// Set one FLAC comment to one value.
///
/// # Panics
///
/// If `path` is not a FLAC, or cannot be read or written.
pub fn set_comment(path: &Utf8Path, key: &str, value: &str) {
    set_multi_valued(path, key, &[value]);
}

/// Add an ID3v2 `COMM` frame with a **description**.
///
/// ID3v2 tells several comments apart by their description, and a real file in
/// this library has three: `COMM:` holding `vtwin88cube`, `COMM:Catalog Number`
/// and `COMM:MusicMatch_Preference`. Only the first is the comment; the other two
/// must read as [`TagSet::extra`][crate::tags::TagSet::extra] and survive a
/// `--clear comment`, which is what this builds the test for.
///
/// # Panics
///
/// If `path` is not an mp3, or cannot be read or written.
pub fn add_described_comment(path: &Utf8Path, description: &str, content: &str) {
    let mut file = mp3(path);
    id3v2(&mut file).insert(Frame::Comment(CommentFrame::new(
        TextEncoding::UTF8,
        *b"eng",
        Cow::Owned(description.to_owned()),
        Cow::Owned(content.to_owned()),
    )));
    save(path, &file);
}

/// Write a raw ID3v2 text frame, bytes and all, with no interpretation.
///
/// This is how a numeric genre reference is made: `set_frame(track, "TCON",
/// "(17)")`. Nothing in MPDFM would ever write that — it is what a tagger from
/// 2003 left behind, and a reader has to resolve it to `Rock`.
///
/// # Panics
///
/// If `path` is not an mp3, if `id` is not a valid frame id, or if the file
/// cannot be read or written.
pub fn set_frame(path: &Utf8Path, id: &str, value: &str) {
    let mut file = mp3(path);
    let tag = id3v2(&mut file);
    let id = FrameId::new(Cow::Owned(id.to_owned()))
        .unwrap_or_else(|err| panic!("{id:?} is not a frame id: {err}"))
        .into_owned();
    tag.insert(Frame::Text(TextInformationFrame::new(
        id,
        TextEncoding::UTF8,
        value.to_owned(),
    )));
    save(path, &file);
}

/// Embed a cover picture, so a write can be shown not to drop it.
///
/// The bytes are a real, tiny PNG — [`COVER_PNG`] — because `lofty` records the
/// MIME type and a test that asserted the picture survived would otherwise be
/// asserting that a byte string survived.
///
/// # Panics
///
/// If `path` is not an mp3 or a FLAC, or cannot be read or written.
pub fn embed_cover(path: &Utf8Path, data: &[u8]) {
    let picture = Picture::unchecked(data.to_vec())
        .pic_type(PictureType::CoverFront)
        .mime_type(MimeType::Png)
        .description("front cover")
        .build();

    match detect(path) {
        FileType::Mpeg => {
            let mut file = mp3(path);
            id3v2(&mut file).insert(Frame::Picture(AttachedPictureFrame::new(
                TextEncoding::UTF8,
                picture,
            )));
            save(path, &file);
        }
        FileType::Flac => {
            let mut file = flac(path);
            vorbis_comments(&mut file)
                .insert_picture(picture, None)
                .unwrap_or_else(|err| panic!("cannot embed a cover in {path}: {err}"));
            save(path, &file);
        }
        other => panic!("{path} is {other:?}; only mp3 and FLAC carry a cover here"),
    }
}

/// A 1×1 transparent PNG — a real image, 67 bytes, decodable by anything.
pub const COVER_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// Write one of the committed templates to `path` **whatever `path` is called**.
///
/// The mislabelling helper: `write_as(dir.join("track.flac"), AudioTemplate::Mp3v24)`
/// produces a file that is an mp3 and claims to be a FLAC, which is a thing
/// scene releases do and which a reader that trusted the extension would refuse.
/// Also the way to put a specific ID3v2 version at a specific path.
///
/// # Panics
///
/// If the file cannot be written.
pub fn write_as(path: &Utf8Path, template: super::AudioTemplate) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|err| panic!("cannot create {parent}: {err}"));
    }
    std::fs::write(path, template.bytes())
        .unwrap_or_else(|err| panic!("cannot write {path}: {err}"));
}

/// Give an mp3's `COMM` frame a language code that is not three ASCII letters.
///
/// 18 of the real library's mp3s hold `\x00\x00\x00` there, which no conforming
/// ID3v2 writer will emit — so without a repair a `--genre` edit on one of them
/// fails with a message about frame languages. This is how that case is kept
/// tested.
///
/// The comment this writes, so a test can assert it survived the repair.
pub const FIXTURE_COMMENT: &str = "MPDFM fixture comment";

/// # Panics
///
/// If `path` is not an mp3, or cannot be read or written.
pub fn set_comment_language(path: &Utf8Path, language: [u8; 3]) {
    // Written with `lofty` first, then patched in place: `lofty` refuses to emit
    // a language that is not three ASCII letters, which is the whole point of the
    // case. An ID3v2 `COMM` frame is a ten-byte header, then one encoding byte,
    // then the three language bytes, so they are at a known offset.
    let mut file = mp3(path);
    id3v2(&mut file).insert(Frame::Comment(CommentFrame::new(
        TextEncoding::UTF8,
        *b"eng",
        Cow::Borrowed(""),
        Cow::Borrowed(FIXTURE_COMMENT),
    )));
    save(path, &file);

    let mut bytes = std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"));
    let at = bytes
        .windows(4)
        .position(|window| window == b"COMM")
        .unwrap_or_else(|| panic!("{path} has no COMM frame to patch"))
        + 11;
    bytes[at..at + 3].copy_from_slice(&language);
    std::fs::write(path, &bytes).unwrap_or_else(|err| panic!("cannot write {path}: {err}"));
}

/// Put a second copy of an mp3's ID3v2 tag in front of the first.
///
/// Two stacked ID3v2 tags is not legal and two of the real library's files have
/// it. `lofty` reads them as one merged tag and writes the result back over the
/// *first*, so the second survives with the old values and still wins on the next
/// read — a write that reports success and changes nothing. [`write`][crate::tags::write]
/// catches that by reading the file back, and this is what proves it.
///
/// # Panics
///
/// If `path` does not begin with an ID3v2 tag, or cannot be read or written.
pub fn stack_id3v2(path: &Utf8Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"));
    let length =
        id3v2_length(&bytes).unwrap_or_else(|| panic!("{path} does not begin with an ID3v2 tag"));

    let mut stacked = Vec::with_capacity(bytes.len() + length);
    stacked.extend_from_slice(&bytes[..length]);
    stacked.extend_from_slice(&bytes);
    std::fs::write(path, &stacked).unwrap_or_else(|err| panic!("cannot write {path}: {err}"));
}

/// Insert `count` zero bytes between an mp3's ID3v2 tag and its first MPEG frame.
///
/// Two albums in the real library have just over 2 KiB of this, which is padding
/// `lofty` needs to be told to search past; the ones with megabytes of it are
/// damaged downloads that no probe can identify. Both sides of
/// `tags::read::MAX_JUNK_BYTES` are worth a test, and this is how either is built.
///
/// # Panics
///
/// If `path` does not begin with an ID3v2 tag, or cannot be read or written.
pub fn pad_with_junk(path: &Utf8Path, count: usize) {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"));
    let length =
        id3v2_length(&bytes).unwrap_or_else(|| panic!("{path} does not begin with an ID3v2 tag"));

    let mut padded = Vec::with_capacity(bytes.len() + count);
    padded.extend_from_slice(&bytes[..length]);
    padded.extend(std::iter::repeat_n(0u8, count));
    padded.extend_from_slice(&bytes[length..]);
    std::fs::write(path, &padded).unwrap_or_else(|err| panic!("cannot write {path}: {err}"));
}

/// How many bytes an ID3v2 tag at the front of `bytes` occupies, header and
/// footer included.
fn id3v2_length(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(b"ID3") || bytes.len() < 10 {
        return None;
    }
    let size = bytes[6..10].iter().fold(0usize, |total, byte| {
        (total << 7) | usize::from(byte & 0x7f)
    });
    Some(10 + size + if bytes[5] & 0x10 == 0 { 0 } else { 10 })
}

/// Cut a file short, so that its tag runs past the end of it.
///
/// Keeps `keep` bytes. A file truncated inside its ID3v2 header is what the
/// "a truncated file returns a typed error naming the path" criterion is about,
/// and it is also what a half-finished `scp` leaves behind.
///
/// # Panics
///
/// If the file cannot be read or written, or is already shorter than `keep`.
pub fn truncate(path: &Utf8Path, keep: usize) {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"));
    assert!(
        bytes.len() > keep,
        "{path} is already {} bytes, shorter than {keep}",
        bytes.len()
    );
    std::fs::write(path, &bytes[..keep]).unwrap_or_else(|err| panic!("cannot write {path}: {err}"));
}

/// Everything the file's tag holds, as sorted lines.
///
/// The before-and-after comparison `docs/tasks/17-tag-write.md` asks for: a write
/// of one field must leave every other line of this identical, embedded artwork
/// and ReplayGain included. Pictures appear as `@PICTURE <type> <mime> <digest>`
/// so that a comparison covers their bytes without printing them.
///
/// Sorted rather than in file order, because the order frames are written in is
/// not a promise `lofty` makes and not one MPDFM needs; what must not change is
/// the set of items and their values.
///
/// # Panics
///
/// If the file cannot be read.
#[must_use]
pub fn dump(path: &Utf8Path) -> String {
    let mut lines = Vec::new();
    match detect(path) {
        FileType::Mpeg => {
            let file = mp3(path);
            if let Some(tag) = file.id3v2() {
                lines.push(format!("@VERSION {:?}", tag.original_version()));
                for frame in tag {
                    lines.push(frame_line(frame));
                }
            }
        }
        FileType::Flac => {
            let file = flac(path);
            if let Some(tag) = file.vorbis_comments() {
                lines.push(format!("@VENDOR {}", tag.vendor()));
                for (key, value) in tag.items() {
                    lines.push(format!("{key}={value}"));
                }
                for (picture, _) in tag.pictures() {
                    lines.push(picture_line(picture));
                }
            }
            // A FLAC's `METADATA_BLOCK_PICTURE`s come back on the file rather
            // than on the comment block, so both have to be looked at or a cover
            // would seem to vanish on the way through a round trip.
            for (picture, _) in file.pictures() {
                lines.push(picture_line(picture));
            }
        }
        FileType::Mp4 => {
            let file = mp4(path);
            if let Some(tag) = file.ilst() {
                for atom in tag {
                    let name = match atom.ident() {
                        lofty::mp4::AtomIdent::Fourcc(fourcc) => fourcc
                            .iter()
                            .map(|byte| {
                                if *byte == 0xa9 {
                                    '\u{a9}'
                                } else {
                                    char::from(*byte)
                                }
                            })
                            .collect::<String>(),
                        lofty::mp4::AtomIdent::Freeform { mean, name } => {
                            format!("----:{mean}:{name}")
                        }
                    };
                    for data in atom.data() {
                        lines.push(match data {
                            lofty::mp4::AtomData::UTF8(text)
                            | lofty::mp4::AtomData::UTF16(text) => format!("{name}={text}"),
                            other => format!("{name}=<{other:?}>"),
                        });
                    }
                }
            }
        }
        other => panic!("{path} is {other:?}, which MPDFM does not tag"),
    }
    lines.sort();
    lines.join("\n")
}

/// The audio stream's bytes, hashed — the samples, with no tag anywhere near
/// them.
///
/// A tag write must not touch a single sample, and comparing whole files cannot
/// show that because the tag is in the file too and is *supposed* to change. So
/// this walks each container's structure and hashes only the part that is audio:
///
/// | | |
/// |---|---|
/// | mp3 | the MPEG frames, with the ID3v2 tag at the front and the ID3v1 tag at the back removed |
/// | flac | everything after the last `METADATA_BLOCK_HEADER` |
/// | m4a | the payload of the top-level `mdat` atom |
///
/// Done by hand rather than by stripping the tags with `lofty` and hashing what
/// is left: both mp4 and FLAC pad their metadata region, so a strip-and-hash
/// gives a different answer for the same audio depending on how big the tag it
/// replaced was, and the test would fail for a reason that has nothing to do with
/// the audio.
///
/// # Panics
///
/// If the file cannot be read, or its container does not parse far enough to find
/// the audio — which for a fixture is a broken test rather than a case to handle.
#[must_use]
pub fn audio_digest(path: &Utf8Path) -> u64 {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"));
    let audio = match detect(path) {
        FileType::Mpeg => mpeg_audio(&bytes),
        FileType::Flac => flac_audio(&bytes),
        FileType::Mp4 => mp4_audio(&bytes),
        other => panic!("{path} is {other:?}, which MPDFM does not tag"),
    };
    let audio = audio.unwrap_or_else(|| panic!("cannot find the audio stream of {path}"));
    assert!(!audio.is_empty(), "{path} has no audio in it");
    super::digest(audio)
}

/// The MPEG frames of an mp3: past the ID3v2 tag at the front, short of the
/// ID3v1 tag at the back.
fn mpeg_audio(bytes: &[u8]) -> Option<&[u8]> {
    let mut start = 0;
    if bytes.starts_with(b"ID3") && bytes.len() >= 10 {
        // A synchsafe integer: four bytes, seven bits each.
        let size = bytes[6..10].iter().fold(0usize, |total, byte| {
            (total << 7) | usize::from(byte & 0x7f)
        });
        start = 10 + size;
        // Bit 4 of the flags byte says there is a footer, which is ten more.
        if bytes[5] & 0x10 != 0 {
            start += 10;
        }
    }
    let mut end = bytes.len();
    if end >= start + 128 && &bytes[end - 128..end - 125] == b"TAG" {
        end -= 128;
    }
    bytes.get(start..end)
}

/// A FLAC's audio frames: everything after the metadata block whose header says
/// it is the last one.
fn flac_audio(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(b"fLaC") {
        return None;
    }
    let mut at = 4;
    loop {
        let header = bytes.get(at..at + 4)?;
        let last = header[0] & 0x80 != 0;
        let length =
            usize::from(header[1]) << 16 | usize::from(header[2]) << 8 | usize::from(header[3]);
        at = at.checked_add(4)?.checked_add(length)?;
        if last {
            return bytes.get(at..);
        }
    }
}

/// An MP4's `mdat` payload, found by walking the top-level atoms.
fn mp4_audio(bytes: &[u8]) -> Option<&[u8]> {
    let mut at = 0;
    while at + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(at + 4..at + 8)?;
        // `0` means "to the end of the file"; `1` means a 64-bit size follows,
        // which a fixture this small never has.
        let size = if size == 0 { bytes.len() - at } else { size };
        if kind == b"mdat" {
            return bytes.get(at + 8..at + size);
        }
        at = at.checked_add(size.max(8))?;
    }
    None
}

// ---------------------------------------------------------------------------

/// One frame, as a comparable line. Text where there is text, a digest where
/// there is not.
fn frame_line(frame: &Frame<'_>) -> String {
    let id = frame.id_str();
    match frame {
        Frame::Text(text) => format!("{id}={}", text.value),
        Frame::UserText(user) => format!("TXXX:{}={}", user.description, user.content),
        Frame::Comment(comment) => format!("COMM:{}={}", comment.description, comment.content),
        Frame::Timestamp(stamp) => format!("{id}={}", stamp.timestamp),
        Frame::Picture(picture) => picture_line(&picture.picture),
        Frame::Url(url) => format!("{id}={}", url.url()),
        Frame::UserUrl(user) => format!("WXXX:{}={}", user.description, user.content),
        Frame::UnsynchronizedText(lyrics) => {
            format!("{id}:{}={}", lyrics.description, lyrics.content)
        }
        Frame::Binary(binary) => format!("{id}=@{:016x}", super::digest(&binary.data)),
        // Anything else by its `Debug`, which is more than a comparison needs and
        // never less. A frame that only differs in its flags shows up here, which
        // is what a before-and-after wants to know.
        other => format!("{id}=<{other:?}>"),
    }
}

/// `@PICTURE <type> <mime> <digest>` — enough to notice artwork that changed or
/// went away, without putting a PNG in the assertion message.
fn picture_line(picture: &Picture) -> String {
    format!(
        "@PICTURE {:?} {} @{:016x}",
        picture.pic_type(),
        picture
            .mime_type()
            .map_or("?", lofty::picture::MimeType::as_str),
        super::digest(picture.data())
    )
}

/// What the bytes say the file is.
fn detect(path: &Utf8Path) -> FileType {
    Probe::open(path)
        .unwrap_or_else(|err| panic!("cannot open {path}: {err}"))
        .guess_file_type()
        .unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
        .file_type()
        .unwrap_or_else(|| panic!("{path} is not a container lofty recognizes"))
}

fn mp3(path: &Utf8Path) -> MpegFile {
    read_file(path)
}

fn flac(path: &Utf8Path) -> FlacFile {
    read_file(path)
}

fn mp4(path: &Utf8Path) -> Mp4File {
    read_file(path)
}

fn read_file<F: AudioFileExt>(path: &Utf8Path) -> F {
    let mut reader = std::fs::File::open(path)
        .map(std::io::BufReader::new)
        .unwrap_or_else(|err| panic!("cannot open {path}: {err}"));
    F::read_from(&mut reader, ParseOptions::new())
        .unwrap_or_else(|err| panic!("cannot parse {path}: {err}"))
}

/// [`AudioFile`][lofty::file::AudioFile] under a name this module can bound on
/// without importing the trait into every helper.
trait AudioFileExt: lofty::file::AudioFile {}
impl<F: lofty::file::AudioFile> AudioFileExt for F {}

/// The mp3's ID3v2 tag, created if it has none.
fn id3v2(file: &mut MpegFile) -> &mut Id3v2Tag {
    if file.id3v2().is_none() {
        let _ = file.set_id3v2(Id3v2Tag::default());
    }
    file.id3v2_mut().expect("just inserted")
}

/// The FLAC's comment block, created if it has none.
fn vorbis_comments(file: &mut FlacFile) -> &mut VorbisComments {
    if file.vorbis_comments().is_none() {
        let _ = file.set_vorbis_comments(VorbisComments::default());
    }
    file.vorbis_comments_mut().expect("just inserted")
}

fn save<F: lofty::file::AudioFile>(path: &Utf8Path, file: &F) {
    save_with(path, file, WriteOptions::default());
}

fn save_with<F: lofty::file::AudioFile>(path: &Utf8Path, file: &F, options: WriteOptions) {
    file.save_to_path(path, options)
        .unwrap_or_else(|err| panic!("cannot write the tag of {path}: {err}"));
}
