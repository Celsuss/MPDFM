//! Putting a cover into a track, or taking it out.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::art::CoverImage;
//! use mpdfm_core::tags::TagDelta;
//!
//! # fn main() -> Result<(), mpdfm_core::Error> {
//! let (image, _bytes) = CoverImage::load(Utf8Path::new("/home/me/Music/a/folder.jpg"))?;
//! let delta = TagDelta::new().set_cover(image);   // one per track, inside a WriteTags
//! # let _ = delta;
//! # Ok(())
//! # }
//! ```
//!
//! # The image is named, not carried
//!
//! A [`CoverEdit::Set`] holds the image's **path** and its [`ImageFacts`] — size
//! and hash — rather than its bytes. A delta is journaled, and a 5 MB cover
//! written into the journal once per track would be 70 MB of JSON for one album.
//! The bytes are read when the write happens, and refused if they are not the
//! bytes the preview described: an edit previewed against one `cover.jpg` must
//! not embed a different one that was saved under the same name in between.
//!
//! Undo never needs them. A tag write's backup is the whole original file
//! (`docs/tasks/17-tag-write.md`), so putting a track back does not depend on
//! the image still existing anywhere.
//!
//! # Downscaling is opt-in
//!
//! Scene releases routinely ship 1–5 MB covers, and embedding one into fourteen
//! tracks adds fourteen copies of it to the library. [`downscale`] re-encodes an
//! oversized image as a JPEG no larger than a given number of pixels on its long
//! side, keeping its aspect ratio. Nothing calls it unless the user asked —
//! re-encoding is lossy, and the original is theirs.

use camino::{Utf8Path, Utf8PathBuf};
use lofty::id3::v2::{Frame, Id3v2Tag};
use lofty::ogg::OggPictureStorage as _;
use lofty::picture::{MimeType, Picture, PictureInformation, PictureType};

use crate::tags::{Parsed, TagError};

use super::{ArtError, ImageFacts, ImageFormat, PictureKind};

/// The largest picture a FLAC can hold: a metadata block's length is 24 bits,
/// and the picture block spends some of it on its own header.
pub const MAX_FLAC_PICTURE: u64 = (1 << 24) - 1024;

/// What a write does to a file's front cover.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverEdit {
    /// Embed this image as the front cover, replacing any front cover already
    /// there. Every other picture stays.
    Set(CoverImage),
    /// Remove the front cover. Every other picture stays.
    Remove,
}

impl CoverEdit {
    /// How the preview renders this edit: `front cover.jpg (image/jpeg 600×600,
    /// 84.2 kB)`, or `<removed>`.
    #[must_use]
    pub fn rendered(&self) -> String {
        match self {
            Self::Set(image) => format!(
                "{} ({})",
                image.source.file_name().unwrap_or(image.source.as_str()),
                image.image
            ),
            Self::Remove => "<removed>".to_owned(),
        }
    }
}

impl std::fmt::Display for CoverEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered())
    }
}

/// An image a [`CoverEdit::Set`] embeds: where it is, and what it was when the
/// edit was planned.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverImage {
    /// The image file, absolute. May be inside the library — a loose
    /// `folder.jpg` — or anywhere else.
    pub source: Utf8PathBuf,
    /// Its size, hash and format, as planned.
    pub image: ImageFacts,
}

impl CoverImage {
    /// Read the image at `path` and describe it, refusing anything that is not
    /// a JPEG or a PNG by its bytes.
    ///
    /// Returns the bytes too, for a caller about to downscale or display them.
    ///
    /// # Errors
    ///
    /// [`ArtError::Unreadable`] and [`ArtError::NotEmbeddable`].
    pub fn load(path: &Utf8Path) -> Result<(Self, Vec<u8>), ArtError> {
        let source = std::path::absolute(path)
            .ok()
            .and_then(|abs| Utf8PathBuf::from_path_buf(abs).ok())
            .unwrap_or_else(|| path.to_owned());
        let bytes = std::fs::read(&source).map_err(|err| ArtError::Unreadable {
            path: source.clone(),
            source: err,
        })?;
        let image = ImageFacts::of(&bytes);
        if !image.format.embeddable() {
            return Err(ArtError::NotEmbeddable {
                path: source,
                format: image.format,
            });
        }
        Ok((Self { source, image }, bytes))
    }

    /// The bytes, read now and checked against what was planned.
    ///
    /// # Errors
    ///
    /// A message for [`TagError::BadEdit`]: the image is gone, or it is not the
    /// image the edit was previewed with.
    pub(crate) fn bytes(&self) -> Result<Vec<u8>, String> {
        let bytes = std::fs::read(&self.source)
            .map_err(|err| format!("the cover image {} cannot be read: {err}", self.source))?;
        let now = ImageFacts::of(&bytes);
        if now.size != self.image.size || now.hash != self.image.hash {
            return Err(format!(
                "the cover image {} has changed since the preview ({} bytes, was {}); \
                 nothing was embedded",
                self.source, now.size, self.image.size
            ));
        }
        Ok(bytes)
    }
}

// ---------------------------------------------------------------------------
// The write, per container. Called from `tags::write`, after the field edits.

/// What preflight can refuse about a cover edit without reading the image: that
/// it is there, that it is the size the preview said, and that the container can
/// hold it.
pub(crate) fn check(abs: &Utf8Path, edit: &CoverEdit, flac: bool) -> Result<(), TagError> {
    let CoverEdit::Set(image) = edit else {
        return Ok(());
    };
    let bad = |reason: String| TagError::BadEdit {
        path: abs.to_owned(),
        reason,
    };
    let meta = std::fs::metadata(&image.source).map_err(|err| {
        bad(format!(
            "the cover image {} cannot be read: {err}",
            image.source
        ))
    })?;
    if meta.len() != image.image.size {
        return Err(bad(format!(
            "the cover image {} has changed since the preview",
            image.source
        )));
    }
    if !image.image.format.embeddable() {
        return Err(bad(format!(
            "{} is {}, and only JPEG and PNG are embedded",
            image.source, image.image.format
        )));
    }
    if flac && image.image.size > MAX_FLAC_PICTURE {
        return Err(bad(format!(
            "a FLAC picture can hold at most {}, and {} is {} — `--max-dimension` \
             shrinks it",
            super::human_bytes(MAX_FLAC_PICTURE),
            image.source,
            super::human_bytes(image.image.size)
        )));
    }
    Ok(())
}

/// Apply a cover edit to a parsed file, in that container's own terms.
///
/// `data` is the image for a [`CoverEdit::Set`], already checked against what was
/// planned; `None` for a removal.
pub(crate) fn apply(parsed: &mut Parsed, edit: &CoverEdit, data: Option<&[u8]>) {
    let picture = match (edit, data) {
        (CoverEdit::Set(image), Some(data)) => Some(front_cover(data, image.image.format)),
        _ => None,
    };
    match parsed {
        Parsed::Mpeg(file) => {
            let mut tag = file.id3v2().cloned().unwrap_or_default();
            id3v2(&mut tag, picture);
            let _ = file.set_id3v2(tag);
        }
        Parsed::Flac(file) => {
            // Both places a FLAC keeps pictures: its own `PICTURE` blocks and a
            // `METADATA_BLOCK_PICTURE` comment. A front cover left in either one
            // would be the cover a player shows after "replacing" it.
            file.remove_picture_type(PictureType::CoverFront);
            if let Some(mut comments) = file.vorbis_comments().cloned() {
                comments.remove_picture_type(PictureType::CoverFront);
                let _ = file.set_vorbis_comments(comments);
            }
            if let Some(picture) = picture {
                let info = PictureInformation::from_picture(&picture).unwrap_or_default();
                // Only an icon can be refused here, and this is a front cover.
                let _ = file.insert_picture(picture, Some(info));
            }
        }
        Parsed::Mp4(file) => {
            let mut ilst = file.ilst().cloned().unwrap_or_default();
            // `covr` has no picture type, so every image in it is the cover.
            ilst.remove_pictures();
            if let Some(picture) = picture {
                ilst.insert_picture(picture);
            }
            let _ = file.set_ilst(ilst);
        }
    }
}

/// Remove every front-cover `APIC` and, for a set, add the new one.
///
/// Matched by type on the frame rather than through `lofty`'s
/// `remove_picture_type`, which only sees pictures it owns and so would miss one
/// borrowed from the parse.
fn id3v2(tag: &mut Id3v2Tag, picture: Option<Picture>) {
    tag.retain(|frame| {
        !matches!(frame, Frame::Picture(apic)
            if apic.picture.pic_type() == PictureType::CoverFront)
    });
    if let Some(picture) = picture {
        // `insert_picture` and not `insert`: `insert` replaces any frame it
        // considers equal, and a picture with the same empty description is a
        // frame this edit was not asked to touch.
        let _ = tag.insert_picture(picture);
    }
}

/// A front-cover [`Picture`] holding `data`.
fn front_cover(data: &[u8], format: ImageFormat) -> Picture {
    let mime = match format {
        ImageFormat::Png => MimeType::Png,
        _ => MimeType::Jpeg,
    };
    Picture::unchecked(data.to_vec())
        .pic_type(PictureType::CoverFront)
        .mime_type(mime)
        .build()
}

/// Read the rewritten file back and check its front cover is what the edit
/// asked for — the same read-back every tag write gets (`tags::write::verify`),
/// for the same reason: a write that reports success and changed nothing is the
/// worst thing a tag editor can do.
///
/// # Errors
///
/// A message for [`TagError::NotWritten`].
pub(crate) fn verify(temp: &Utf8Path, edit: &CoverEdit) -> Result<(), String> {
    let pictures =
        super::read::embedded(temp).map_err(|err| format!("could not be read back: {err}"))?;
    let fronts: Vec<_> = pictures
        .iter()
        .filter(|picture| picture.kind == PictureKind::FRONT)
        .collect();
    match edit {
        CoverEdit::Remove if fronts.is_empty() => Ok(()),
        CoverEdit::Remove => Err(format!(
            "{} front cover(s) are still there after removing them",
            fronts.len()
        )),
        CoverEdit::Set(image) => match fronts.as_slice() {
            [front] if front.image.hash == image.image.hash => Ok(()),
            [_] => Err("the front cover reads back as a different image".to_owned()),
            many => Err(format!(
                "{} front covers read back instead of one",
                many.len()
            )),
        },
    }
}

// ---------------------------------------------------------------------------

/// Shrink `bytes` so its longer side is at most `max` pixels, keeping its aspect
/// ratio, and re-encode it as a JPEG.
///
/// `Ok(None)` when it already fits, so the original bytes — not a lossy copy of
/// them — are what gets embedded.
///
/// # Errors
///
/// [`ArtError::Decode`] when the image cannot be decoded or encoded.
pub fn downscale(bytes: &[u8], max: u32, path: &Utf8Path) -> Result<Option<Vec<u8>>, ArtError> {
    let decode = |reason: String| ArtError::Decode {
        path: path.to_owned(),
        reason,
    };
    let image = image::load_from_memory(bytes).map_err(|err| decode(err.to_string()))?;
    let max = max.max(1);
    if image.width() <= max && image.height() <= max {
        return Ok(None);
    }
    // `resize` fits the image *inside* the box, which is what keeps the aspect
    // ratio: a 3000×1500 scan becomes 1000×500, not 1000×1000.
    let smaller = image.resize(max, max, image::imageops::FilterType::Lanczos3);
    // A JPEG has no alpha channel, and a PNG cover often has one it does not use.
    let rgb = image::DynamicImage::ImageRgb8(smaller.to_rgb8());
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&rgb)
        .map_err(|err| decode(err.to_string()))?;
    Ok(Some(out.into_inner()))
}

/// Quality a downscaled cover is encoded at: visually lossless at cover sizes,
/// and a fifth of the size of the scan it came from.
const JPEG_QUALITY: u8 = 90;

/// Keep `bytes` in MPDFM's data directory, named by their hash, and return the
/// path.
///
/// Where a downscaled cover lives between the preview and the commit, and after:
/// a [`CoverImage`] names a file, and this one is outside the library, so a scan
/// never sees it and an album move never takes it along. Named by content, so
/// staging the same image twice is one file, and a stale copy cannot be confused
/// with a fresh one.
///
/// # Errors
///
/// [`ArtError::NotStaged`].
pub fn stage(data_dir: &Utf8Path, bytes: &[u8]) -> Result<Utf8PathBuf, ArtError> {
    let facts = ImageFacts::of(bytes);
    let dir = data_dir.join("art");
    let path = dir.join(format!("{:016x}.{}", facts.hash, facts.format.extension()));
    let not_staged = |source| ArtError::NotStaged {
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(&dir).map_err(not_staged)?;
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() == facts.size) {
        return Ok(path);
    }
    let temp = dir.join(format!(".{:016x}.{}.tmp", facts.hash, std::process::id()));
    std::fs::write(&temp, bytes).map_err(not_staged)?;
    std::fs::rename(&temp, &path).map_err(|err| {
        std::fs::remove_file(&temp).ok();
        not_staged(err)
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn downscaling_keeps_the_aspect_ratio() {
        let wide = png(300, 150);
        let smaller = downscale(&wide, 100, Utf8Path::new("wide.png"))
            .unwrap()
            .expect("300 px is over 100");
        let facts = ImageFacts::of(&smaller);
        assert_eq!(facts.format, ImageFormat::Jpeg);
        assert_eq!((facts.width, facts.height), (Some(100), Some(50)));
    }

    #[test]
    fn an_image_that_already_fits_is_left_alone() {
        let small = png(80, 60);
        assert_eq!(
            downscale(&small, 100, Utf8Path::new("small.png")).unwrap(),
            None
        );
    }

    #[test]
    fn something_that_is_not_an_image_cannot_be_downscaled() {
        let err =
            downscale(b"not an image", 100, Utf8Path::new("x.jpg")).expect_err("nothing to decode");
        assert!(err.to_string().starts_with("x.jpg: "), "{err}");
    }

    #[test]
    fn a_staged_image_is_named_by_its_content_and_staged_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let bytes = png(4, 4);
        let one = stage(root, &bytes).unwrap();
        let two = stage(root, &bytes).unwrap();
        assert_eq!(one, two);
        assert_eq!(std::fs::read(&one).unwrap(), bytes);
        assert!(one.as_str().ends_with(".png"), "{one}");
    }

    #[test]
    fn a_cover_edit_renders_the_way_the_preview_shows_it() {
        let image = CoverImage {
            source: Utf8PathBuf::from("/music/a/folder.jpg"),
            image: ImageFacts {
                format: ImageFormat::Jpeg,
                width: Some(600),
                height: Some(600),
                size: 84_200,
                hash: 1,
            },
        };
        assert_eq!(
            CoverEdit::Set(image).rendered(),
            "folder.jpg (image/jpeg 600\u{d7}600, 84.2 kB)"
        );
        assert_eq!(CoverEdit::Remove.rendered(), "<removed>");
    }
}
