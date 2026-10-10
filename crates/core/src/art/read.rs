//! Reading art: the pictures inside a track, the images beside it, and where the
//! two disagree.
//!
//! # Loose files, in the order a player looks for them
//!
//! Over 200 of the library's album directories already hold a `folder.jpg` or a
//! `cover.jpg`, and MPD's own `albumart` command, rmpc and every other client
//! that reads loose art look for them by name. [`loose`] lists every image in a
//! directory and ranks them the way those clients do:
//!
//! ```text
//! 0  cover.*      what MPD's `albumart` asks for first
//! 1  folder.*     what Windows and most rippers write
//! 2  front.*      what scene releases call it
//! 3  anything else that is an image, by name
//! ```
//!
//! The format is read from the bytes, not the extension, so a `folder.jpg` that
//! is really a PNG is reported as a PNG — and a "cover.jpg" that is a text file
//! (the fixture library has those, on purpose) is reported as not an image at
//! all rather than as a JPEG nobody can open.
//!
//! # Cost
//!
//! Every image is read in full, because its hash is what lets two of them be
//! compared and what [`Mismatch::Different`] is built on. That is a few hundred
//! kilobytes per album and happens only when somebody asks — `cover show`, or the
//! TUI's details pane, on a worker thread.

use camino::{Utf8Path, Utf8PathBuf};
use lofty::id3::v2::Frame;
use lofty::ogg::OggPictureStorage as _;
use lofty::picture::Picture;

use crate::library::{DirPath, Library};
use crate::paths::RelPath;
use crate::tags::{Parsed, TagError, read as tag_read};

use super::{ImageFacts, ImageFormat, PictureKind};

/// One picture embedded in a track.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Embedded {
    /// What it is of.
    pub kind: PictureKind,
    /// The description the frame carries, often empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The MIME type the **frame** claims, which is not always what the bytes
    /// are — [`ImageFacts::format`] is the bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<String>,
    /// What the picture is.
    pub image: ImageFacts,
}

/// One image file lying in an album directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Loose {
    /// Its name in the directory.
    pub name: String,
    /// Where it sits in the preference order — 0 for `cover.*`, 3 for any other
    /// image. Lower is preferred.
    pub rank: u8,
    /// What it is.
    pub image: ImageFacts,
}

impl Loose {
    /// Whether the bytes are an image at all.
    #[must_use]
    pub fn is_image(&self) -> bool {
        self.image.format != ImageFormat::Unknown
    }
}

/// Every picture embedded in `abs`, in file order.
///
/// # Errors
///
/// As [`tags::read`][crate::tags::read]: the file cannot be opened, is not a
/// container MPDFM reads, or its tag is damaged.
pub fn embedded(abs: &Utf8Path) -> Result<Vec<Embedded>, TagError> {
    let (parsed, _, _) = tag_read::open(abs, tag_read::parse_options(false))?;
    Ok(pictures(&parsed)
        .into_iter()
        .map(|(kind, picture)| describe(kind, picture))
        .collect())
}

/// The bytes of the first embedded picture `wanted` accepts, with what it is.
///
/// `None` when nothing matched. Used by `extract`, which names the picture by
/// its size and hash, and by the TUI, which wants the front cover.
///
/// # Errors
///
/// As [`embedded`].
pub fn picture_data(
    abs: &Utf8Path,
    wanted: impl Fn(&Embedded) -> bool,
) -> Result<Option<(Embedded, Vec<u8>)>, TagError> {
    let (parsed, _, _) = tag_read::open(abs, tag_read::parse_options(false))?;
    Ok(pictures(&parsed).into_iter().find_map(|(kind, picture)| {
        let found = describe(kind, picture);
        wanted(&found).then(|| (found, picture.data().to_vec()))
    }))
}

/// Which picture a display or an extraction should prefer: the front cover, or
/// failing that the first picture of any kind.
///
/// Many taggers write a single picture as [`PictureKind::OTHER`], and a file
/// whose only art is typed "other" still has a cover in every sense a person
/// means.
#[must_use]
pub fn preferred(pictures: &[Embedded]) -> Option<&Embedded> {
    pictures
        .iter()
        .find(|picture| picture.kind.is_front())
        .or_else(|| pictures.first())
}

/// Every image file directly in `dir`, best first.
///
/// "An image file" is decided by extension here, because the point is to list
/// what a person would call the album's art; each one's [`ImageFacts::format`]
/// then says what its bytes actually are.
///
/// # Errors
///
/// The directory cannot be listed. A single image that cannot be read is left
/// out rather than failing the listing.
pub fn loose(dir: &Utf8Path) -> std::io::Result<Vec<Loose>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(rank) = rank(&name) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(bytes) = std::fs::read(dir.join(&name)) else {
            continue;
        };
        found.push(Loose {
            name,
            rank,
            image: ImageFacts::of(&bytes),
        });
    }
    found.sort_by(|a, b| (a.rank, &a.name).cmp(&(b.rank, &b.name)));
    Ok(found)
}

/// Extensions a loose image file is recognized by.
const IMAGE_EXTENSIONS: [&str; 8] = ["jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff"];

/// Where `name` sits in the preference order, or `None` for a file that is not
/// an image by its extension.
fn rank(name: &str) -> Option<u8> {
    let (stem, ext) = name.rsplit_once('.')?;
    if !IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some(match stem.to_ascii_lowercase().as_str() {
        "cover" => 0,
        "folder" => 1,
        "front" => 2,
        _ => 3,
    })
}

// ---------------------------------------------------------------------------

/// One album directory's art, embedded and loose, and what disagrees.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AlbumArt {
    /// The album directory.
    pub dir: DirPath,
    /// Each audio file in it, with its pictures or why they could not be read.
    pub tracks: Vec<TrackArt>,
    /// Every image file in it, best first.
    pub loose: Vec<Loose>,
    /// What disagrees.
    pub mismatches: Vec<Mismatch>,
}

/// One track's pictures.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrackArt {
    /// The track.
    pub rel: RelPath,
    /// Its pictures, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pictures: Vec<Embedded>,
    /// Why they could not be read, when they could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TrackArt {
    /// The front cover, if the track has one.
    #[must_use]
    pub fn front(&self) -> Option<&Embedded> {
        self.pictures.iter().find(|picture| picture.kind.is_front())
    }

    /// What a player would show as this track's cover: the front cover, or
    /// failing that a picture typed "other" — which is what a great many
    /// taggers write when they were not asked. 421 of the real library's
    /// pictures are typed that way, and calling those albums cover-less would be
    /// crying wolf. A back cover or a band photo is not the cover.
    #[must_use]
    pub fn cover(&self) -> Option<&Embedded> {
        self.front().or_else(|| {
            self.pictures
                .iter()
                .find(|picture| picture.kind == PictureKind::OTHER)
        })
    }
}

/// Embedded and loose art that do not agree with each other.
///
/// Reported, never repaired: which of the two is right is the user's call, and
/// `cover set` and `cover extract` are the two ways of making it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mismatch {
    /// Tracks embed a cover — see [`TrackArt::cover`] — and the directory has
    /// no image beside them,
    /// so a client that reads loose art — MPD's `albumart` — shows nothing.
    /// `cover extract` writes one.
    EmbeddedOnly,
    /// The directory has an image and no track embeds a cover, so a
    /// client that reads embedded art — most phones, most car stereos — shows
    /// nothing. `cover set` embeds it.
    LooseOnly {
        /// The image that would be used.
        file: String,
    },
    /// Some tracks embed a cover and some do not.
    SomeTracksBare {
        /// Tracks with no cover.
        bare: usize,
        /// Tracks read.
        of: usize,
    },
    /// The tracks' covers are not all the same image.
    Different {
        /// How many different images there are.
        distinct: usize,
    },
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmbeddedOnly => f.write_str(
                "the tracks embed a cover but there is no image file beside them \
                 (`mpdfm cover extract` writes one)",
            ),
            Self::LooseOnly { file } => write!(
                f,
                "{file} is here but no track embeds a cover (`mpdfm cover set` embeds it)"
            ),
            Self::SomeTracksBare { bare, of } => {
                write!(f, "{bare} of {of} tracks embed no cover")
            }
            Self::Different { distinct } => {
                write!(f, "the tracks embed {distinct} different covers")
            }
        }
    }
}

/// Survey the album directory `dir`: every track's pictures, every loose
/// image, and the mismatches between them.
///
/// Only the tracks **directly** in `dir`: a multi-disc set's discs are albums of
/// their own, each with its own art, and folding them together would report a
/// mismatch between two discs that are each perfectly consistent.
///
/// # Errors
///
/// The directory cannot be listed. A track whose tags cannot be read is a
/// [`TrackArt::error`] rather than a failure of the survey.
pub fn album(library: &Library, dir: &DirPath) -> std::io::Result<AlbumArt> {
    let root = library.root();
    let tracks: Vec<TrackArt> = library
        .files_in(dir)
        .filter(|entry| entry.is_audio())
        .map(|entry| match embedded(&entry.rel.to_abs(root)) {
            Ok(pictures) => TrackArt {
                rel: entry.rel.clone(),
                pictures,
                error: None,
            },
            Err(err) => TrackArt {
                rel: entry.rel.clone(),
                pictures: Vec::new(),
                error: Some(err.to_string()),
            },
        })
        .collect();
    let loose = loose(&dir.to_abs(root))?;
    let mismatches = mismatches(&tracks, &loose);
    Ok(AlbumArt {
        dir: dir.clone(),
        tracks,
        loose,
        mismatches,
    })
}

/// What disagrees between `tracks` and `loose`.
#[must_use]
pub fn mismatches(tracks: &[TrackArt], loose: &[Loose]) -> Vec<Mismatch> {
    let read: Vec<&TrackArt> = tracks
        .iter()
        .filter(|track| track.error.is_none())
        .collect();
    let fronts: Vec<&Embedded> = read.iter().filter_map(|track| track.cover()).collect();
    let image = loose.iter().find(|file| file.is_image());

    let mut found = Vec::new();
    match (fronts.is_empty(), image) {
        (false, None) => found.push(Mismatch::EmbeddedOnly),
        (true, Some(file)) if !read.is_empty() => found.push(Mismatch::LooseOnly {
            file: file.name.clone(),
        }),
        _ => {}
    }
    if !fronts.is_empty() && fronts.len() < read.len() {
        found.push(Mismatch::SomeTracksBare {
            bare: read.len() - fronts.len(),
            of: read.len(),
        });
    }
    let mut distinct: Vec<u64> = fronts.iter().map(|front| front.image.hash).collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() > 1 {
        found.push(Mismatch::Different {
            distinct: distinct.len(),
        });
    }
    found
}

// ---------------------------------------------------------------------------

/// Every picture in a parsed file, with its kind, in file order.
///
/// The one place a container is branched on for reading art, so the display, the
/// extraction and the writer's read-back cannot disagree about what is in a file:
///
/// - an **mp3**'s `APIC` frames, from its ID3v2 tag;
/// - a **FLAC**'s `PICTURE` blocks, and also any `METADATA_BLOCK_PICTURE`
///   comment inside its Vorbis comment block — `lofty` keeps the two apart, and
///   some taggers write the second;
/// - an **m4a**'s `covr` images, every one of which is the front cover (see the
///   [module docs][super]).
pub(crate) fn pictures(parsed: &Parsed) -> Vec<(PictureKind, &Picture)> {
    let kind = |picture: &Picture| PictureKind::from_code(picture.pic_type().as_u8());
    match parsed {
        Parsed::Mpeg(file) => file
            .id3v2()
            .into_iter()
            .flat_map(|tag| tag.into_iter())
            .filter_map(|frame| match frame {
                Frame::Picture(apic) => Some((kind(&apic.picture), apic.picture.as_ref())),
                _ => None,
            })
            .collect(),
        Parsed::Flac(file) => file
            .pictures()
            .iter()
            .chain(
                file.vorbis_comments()
                    .map(|comments| comments.pictures())
                    .unwrap_or_default(),
            )
            .map(|(picture, _)| (kind(picture), picture))
            .collect(),
        Parsed::Mp4(file) => file
            .ilst()
            .and_then(lofty::mp4::Ilst::pictures)
            .into_iter()
            .flatten()
            .map(|picture| (PictureKind::FRONT, picture))
            .collect(),
    }
}

/// One picture, as the rest of MPDFM sees it.
fn describe(kind: PictureKind, picture: &Picture) -> Embedded {
    Embedded {
        kind,
        description: picture.description().unwrap_or_default().to_owned(),
        declared: picture
            .mime_type()
            .map(|mime| mime.as_str().to_owned())
            .filter(|mime| !mime.is_empty()),
        image: ImageFacts::of(picture.data()),
    }
}

/// An absolute path for a loose image, for a caller that has a [`Loose`] and
/// the directory it came from.
#[must_use]
pub fn loose_path(dir: &Utf8Path, file: &Loose) -> Utf8PathBuf {
    dir.join(&file.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loose_images_rank_the_way_players_look_for_them() {
        assert_eq!(rank("cover.jpg"), Some(0));
        assert_eq!(rank("Folder.JPG"), Some(1));
        assert_eq!(rank("front.png"), Some(2));
        assert_eq!(rank("scan-01.jpeg"), Some(3));
        assert_eq!(rank("album.nfo"), None);
        assert_eq!(
            rank("cover"),
            None,
            "no extension, no way to call it an image"
        );
    }

    fn track(front: Option<u64>) -> TrackArt {
        TrackArt {
            rel: RelPath::parse("a/01.mp3").unwrap(),
            pictures: front
                .map(|hash| Embedded {
                    kind: PictureKind::FRONT,
                    description: String::new(),
                    declared: None,
                    image: ImageFacts {
                        format: ImageFormat::Jpeg,
                        width: None,
                        height: None,
                        size: 1,
                        hash,
                    },
                })
                .into_iter()
                .collect(),
            error: None,
        }
    }

    fn folder_jpg() -> Loose {
        Loose {
            name: "folder.jpg".to_owned(),
            rank: 1,
            image: ImageFacts::of(b"\xff\xd8\xff\xe0 a jpeg, by its first bytes"),
        }
    }

    #[test]
    fn embedded_art_with_nothing_beside_it_is_a_mismatch() {
        assert_eq!(
            mismatches(&[track(Some(1)), track(Some(1))], &[]),
            [Mismatch::EmbeddedOnly]
        );
    }

    #[test]
    fn a_loose_image_no_track_embeds_is_a_mismatch() {
        assert_eq!(
            mismatches(&[track(None)], &[folder_jpg()]),
            [Mismatch::LooseOnly {
                file: "folder.jpg".to_owned()
            }]
        );
    }

    #[test]
    fn agreeing_art_is_not_reported() {
        assert!(mismatches(&[track(Some(7)), track(Some(7))], &[folder_jpg()]).is_empty());
        assert!(
            mismatches(&[track(None)], &[]).is_empty(),
            "no art is not a mismatch"
        );
    }

    #[test]
    fn partial_and_differing_covers_are_named() {
        let found = mismatches(
            &[track(Some(1)), track(Some(2)), track(None)],
            &[folder_jpg()],
        );
        assert_eq!(
            found,
            [
                Mismatch::SomeTracksBare { bare: 1, of: 3 },
                Mismatch::Different { distinct: 2 }
            ]
        );
    }

    #[test]
    fn a_picture_typed_other_counts_as_the_cover_and_a_back_cover_does_not() {
        let typed = |kind| {
            let mut track = track(Some(1));
            track.pictures[0].kind = kind;
            track
        };
        assert!(mismatches(&[typed(PictureKind::OTHER)], &[folder_jpg()]).is_empty());
        assert_eq!(
            mismatches(&[typed(PictureKind::BACK)], &[folder_jpg()]),
            [Mismatch::LooseOnly {
                file: "folder.jpg".to_owned()
            }]
        );
    }

    #[test]
    fn a_text_file_named_like_a_cover_does_not_count_as_one() {
        let fake = Loose {
            name: "cover.jpg".to_owned(),
            rank: 0,
            image: ImageFacts::of(b"MPDFM fixture aux file"),
        };
        assert!(!fake.is_image());
        assert_eq!(
            mismatches(&[track(Some(1))], &[fake]),
            [Mismatch::EmbeddedOnly]
        );
    }
}
