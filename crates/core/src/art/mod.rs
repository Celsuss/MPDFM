//! Cover art: what is embedded in a track, what is lying loose in its directory,
//! and the edits that put one into the other.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::art;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let track = Utf8Path::new("/home/me/Music/hiphop/MF DOOM/01 Beef Rap.mp3");
//! for picture in art::read::embedded(track)? {
//!     println!("{} {}", picture.kind, picture.image);   // front cover image/jpeg 600×600, 84.2 kB
//! }
//! for loose in art::read::loose(Utf8Path::new("/home/me/Music/hiphop/MF DOOM"))? {
//!     println!("{} {}", loose.name, loose.image);       // folder.jpg image/jpeg 500×500, 61.0 kB
//! }
//! # Ok(())
//! # }
//! ```
//!
//! | | |
//! |---|---|
//! | [`read`] | embedded pictures, loose image files, and what disagrees between them |
//! | [`write`] | [`CoverEdit`] — the cover half of a [`TagDelta`][crate::tags::TagDelta] — and the optional downscale |
//! | [`plan`] | the operations a cover request comes to, and which loose image goes with which single |
//!
//! # A cover is a tag edit
//!
//! Embedding a picture rewrites the file's tag, so it goes through exactly the
//! machinery a `--genre` edit does: a [`CoverEdit`] rides inside the
//! [`TagDelta`][crate::tags::TagDelta] of an
//! [`Operation::WriteTags`][crate::ops::Operation::WriteTags], the original file
//! is copied into the transaction's backup before anything is written, every
//! field the edit did not name survives, and `mpdfm undo` puts the file back byte
//! for byte (`docs/tasks/17-tag-write.md`). There is no second write path for art
//! to get wrong.
//!
//! Extracting one is the opposite shape — a new file appears in the album
//! directory and no audio file changes — so it is its own operation,
//! [`Operation::ExtractArt`][crate::ops::Operation::ExtractArt], and undoing it
//! removes the file it created (and only if it is still the file it created).
//!
//! # What "front cover" means in each container
//!
//! ID3v2 and FLAC share one list of 21 picture types ([`PictureKind`]), and a
//! cover edit touches **only** [`PictureKind::FRONT`]: a back cover, a CD scan or
//! a band photo in the same file is left exactly where it was. An m4a's `covr`
//! atom has no type at all, and every player reads it as the cover, so in an m4a
//! every `covr` image counts as the front cover — which is the only reading under
//! which "replace the front cover" means anything there.

pub mod plan;
pub mod read;
pub mod write;

pub use read::{AlbumArt, Embedded, Loose, Mismatch, album, embedded, loose};
pub use write::{CoverEdit, CoverImage, MAX_FLAC_PICTURE, downscale, stage};

use camino::Utf8PathBuf;

/// What a picture is *of*: ID3v2's `APIC` picture type, which FLAC's
/// `METADATA_BLOCK_PICTURE` shares.
///
/// A number rather than an enum because the list is the spec's and has 21
/// entries, of which MPDFM cares about one; the name is for display.
///
/// ```
/// use mpdfm_core::art::PictureKind;
///
/// assert_eq!(PictureKind::FRONT.name(), "front cover");
/// assert!(PictureKind::FRONT.is_front());
/// assert!(!PictureKind::BACK.is_front());
/// assert_eq!(PictureKind::from_code(20).name(), "publisher logo");
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct PictureKind(u8);

/// The 21 names, indexed by code.
const KIND_NAMES: [&str; 21] = [
    "other",
    "file icon",
    "other file icon",
    "front cover",
    "back cover",
    "leaflet",
    "media",
    "lead artist",
    "artist",
    "conductor",
    "band",
    "composer",
    "lyricist",
    "recording location",
    "during recording",
    "during performance",
    "screen capture",
    "bright coloured fish",
    "illustration",
    "band logo",
    "publisher logo",
];

impl PictureKind {
    /// "Other" — what most taggers write when they were not asked.
    pub const OTHER: Self = Self(0);
    /// The front cover. The one kind a cover edit touches.
    pub const FRONT: Self = Self(3);
    /// The back cover.
    pub const BACK: Self = Self(4);

    /// The kind with this spec code. Codes past the list are kept, and named as
    /// undefined, rather than folded into [`PictureKind::OTHER`]: a write must
    /// not change a picture it was not asked about, even its type byte.
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        Self(code)
    }

    /// The spec code.
    #[must_use]
    pub fn code(self) -> u8 {
        self.0
    }

    /// `front cover`, `back cover`, `media`.
    #[must_use]
    pub fn name(self) -> &'static str {
        KIND_NAMES
            .get(usize::from(self.0))
            .copied()
            .unwrap_or("undefined")
    }

    /// Whether this is the front cover.
    #[must_use]
    pub fn is_front(self) -> bool {
        self == Self::FRONT
    }
}

impl std::fmt::Display for PictureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.name())
    }
}

/// What an image's bytes say it is.
///
/// Sniffed from the first bytes and never taken from a name: a `folder.jpg`
/// that is a PNG is a PNG, and it is the bytes a player decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// `image/jpeg`.
    Jpeg,
    /// `image/png`.
    Png,
    /// `image/gif`.
    Gif,
    /// `image/bmp`.
    Bmp,
    /// `image/webp`.
    Webp,
    /// `image/tiff`.
    Tiff,
    /// Not an image format MPDFM recognizes.
    Unknown,
}

impl ImageFormat {
    /// What the first bytes of an image say it is.
    ///
    /// ```
    /// use mpdfm_core::art::ImageFormat;
    ///
    /// assert_eq!(ImageFormat::sniff(b"\xff\xd8\xff\xe0\0\x10JFIF"), ImageFormat::Jpeg);
    /// assert_eq!(ImageFormat::sniff(b"\x89PNG\r\n\x1a\n"), ImageFormat::Png);
    /// assert_eq!(ImageFormat::sniff(b"MPDFM fixture aux file"), ImageFormat::Unknown);
    /// ```
    #[must_use]
    pub fn sniff(bytes: &[u8]) -> Self {
        match bytes {
            [0xff, 0xd8, 0xff, ..] => Self::Jpeg,
            [0x89, b'P', b'N', b'G', ..] => Self::Png,
            [b'G', b'I', b'F', b'8', ..] => Self::Gif,
            [b'B', b'M', ..] => Self::Bmp,
            [
                b'R',
                b'I',
                b'F',
                b'F',
                _,
                _,
                _,
                _,
                b'W',
                b'E',
                b'B',
                b'P',
                ..,
            ] => Self::Webp,
            [b'I', b'I', 0x2a, 0, ..] | [b'M', b'M', 0, 0x2a, ..] => Self::Tiff,
            _ => Self::Unknown,
        }
    }

    /// The MIME type a picture frame records.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::Webp => "image/webp",
            Self::Tiff => "image/tiff",
            Self::Unknown => "application/octet-stream",
        }
    }

    /// The extension an extracted copy is given.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Webp => "webp",
            Self::Tiff => "tif",
            Self::Unknown => "bin",
        }
    }

    /// Whether MPDFM will embed an image of this format.
    ///
    /// JPEG and PNG, which are the two every player that shows art at all can
    /// decode — and the two the ID3v2 spec names. Embedding a WebP into
    /// fourteen tracks is a way to have fourteen tracks with no cover in most
    /// players, so it is refused rather than done.
    #[must_use]
    pub fn embeddable(self) -> bool {
        matches!(self, Self::Jpeg | Self::Png)
    }
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.mime())
    }
}

/// What an image is, without the image: enough to show it and to recognize the
/// same bytes again.
///
/// The hash is FNV-1a, the same number [`hash_file`][crate::ops::exec_fs::hash_file]
/// gives for the same bytes. It is what lets a cover edit that was previewed
/// against one `cover.jpg` refuse to embed a different one that has since been
/// saved under the same name, and what lets `extract` say *which* embedded
/// picture it means without carrying the picture around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ImageFacts {
    /// What the bytes are.
    pub format: ImageFormat,
    /// Pixels across, when the header could be read. JPEG and PNG only.
    pub width: Option<u32>,
    /// Pixels down, likewise.
    pub height: Option<u32>,
    /// Length in bytes.
    pub size: u64,
    /// FNV-1a of the bytes.
    pub hash: u64,
}

impl ImageFacts {
    /// Look at `bytes`. Reads headers only; nothing is decoded.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let format = ImageFormat::sniff(bytes);
        let info = match format {
            ImageFormat::Jpeg => lofty::picture::PictureInformation::from_jpeg(bytes).ok(),
            ImageFormat::Png => lofty::picture::PictureInformation::from_png(bytes).ok(),
            _ => None,
        }
        .filter(|info| info.width > 0 && info.height > 0);
        Self {
            format,
            width: info.map(|info| info.width),
            height: info.map(|info| info.height),
            size: bytes.len() as u64,
            hash: crate::ops::exec_fs::hash_bytes(bytes),
        }
    }

    /// `600×600`, or `?×?` when the header would not say.
    #[must_use]
    pub fn dimensions(&self) -> String {
        match (self.width, self.height) {
            (Some(width), Some(height)) => format!("{width}\u{d7}{height}"),
            _ => "?\u{d7}?".to_owned(),
        }
    }
}

impl std::fmt::Display for ImageFacts {
    /// `image/jpeg 600×600, 84.2 kB`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}, {}",
            self.format,
            self.dimensions(),
            human_bytes(self.size)
        )
    }
}

/// `84.2 kB`, `5.0 MB` — the same rounding the preview's byte totals use, so a
/// cover's size and the size it adds to an album read in one unit.
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    #[allow(clippy::cast_precision_loss)] // a size shown to one decimal place
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Why an image could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ArtError {
    /// The image file could not be read.
    #[error("{path}: {source}")]
    Unreadable {
        /// The image.
        path: Utf8PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },

    /// The bytes are an image MPDFM will not embed — or not an image at all.
    #[error(
        "{path} is {}, and only JPEG and PNG are embedded — every player that shows \
         cover art reads those two",
        if *.format == ImageFormat::Unknown { "not an image".to_owned() } else { format!("{}", .format) }
    )]
    NotEmbeddable {
        /// The image.
        path: Utf8PathBuf,
        /// What its bytes are.
        format: ImageFormat,
    },

    /// The image could not be decoded or re-encoded for a downscale.
    #[error("{path}: {reason}")]
    Decode {
        /// The image.
        path: Utf8PathBuf,
        /// What the codec said.
        reason: String,
    },

    /// A downscaled copy could not be kept in MPDFM's data directory.
    #[error("{path}: {source}")]
    NotStaged {
        /// Where it was being written.
        path: Utf8PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picture_kind_has_a_name_and_an_unknown_one_is_kept() {
        for code in 0..21 {
            assert_ne!(PictureKind::from_code(code).name(), "undefined", "{code}");
        }
        let odd = PictureKind::from_code(200);
        assert_eq!(odd.code(), 200, "the byte survives");
        assert_eq!(odd.name(), "undefined");
    }

    #[test]
    fn a_format_is_sniffed_from_bytes_and_named_by_its_mime_type() {
        assert_eq!(ImageFormat::sniff(b"GIF89a"), ImageFormat::Gif);
        assert_eq!(
            ImageFormat::sniff(b"RIFF\0\0\0\0WEBPVP8 "),
            ImageFormat::Webp
        );
        assert_eq!(ImageFormat::sniff(b""), ImageFormat::Unknown);
        assert_eq!(ImageFormat::Jpeg.extension(), "jpg");
        assert!(ImageFormat::Png.embeddable());
        assert!(!ImageFormat::Webp.embeddable());
    }

    #[test]
    fn bytes_read_the_way_the_preview_reads_them() {
        assert_eq!(human_bytes(67), "67 B");
        assert_eq!(human_bytes(84_200), "84.2 kB");
        assert_eq!(human_bytes(5_000_000), "5.0 MB");
    }

    #[test]
    fn facts_of_an_unreadable_header_say_so_instead_of_inventing_a_size() {
        let facts = ImageFacts::of(b"\xff\xd8\xff not really a jpeg");
        assert_eq!(facts.format, ImageFormat::Jpeg);
        assert_eq!(facts.dimensions(), "?\u{d7}?");
    }
}
