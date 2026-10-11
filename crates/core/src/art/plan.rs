//! Planning cover writes: which tracks an edit touches, what it costs, and which
//! loose image belongs to which track.
//!
//! The CLI's `cover set | remove | extract` and the TUI's cover view are two
//! ways of asking for the same operations, so the arithmetic lives here once:
//!
//! | | |
//! |---|---|
//! | [`embed`] | a [`CoverEdit::Set`][super::CoverEdit::Set] for every track that does not already carry exactly that front cover |
//! | [`remove`] | a [`CoverEdit::Remove`][super::CoverEdit::Remove] for every track that has a front cover |
//! | [`extract`] | one [`Operation::ExtractArt`] for a track's preferred picture |
//! | [`pair`] | which loose image each track of a directory of singles goes with, by name |
//!
//! Every function takes the tracks it is about, not a directory: an album embeds
//! one cover into all of its tracks, but a directory of singles holds a cover
//! per track, and the caller is the one who knows which tracks it means.

use crate::library::DirPath;
use crate::ops::Operation;
use crate::paths::RelPath;
use crate::tags::TagDelta;

use super::read::{Embedded, Loose, preferred};
use super::{CoverImage, ImageFormat, human_bytes};

/// An image over this size is worth a hint that it could be downscaled: where a
/// cover stops being a cover and starts being a scan.
pub const LARGE_COVER: u64 = 1_000_000;

/// The operations a cover request comes to, and what the user should be told
/// about them before they are committed.
///
/// No operations means there is nothing to do — every track already says what
/// was asked — which is an answer and not an error.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Planned {
    /// The operations, one per file written.
    pub ops: Vec<Operation>,
    /// What the preview should say alongside them: the bytes it costs, a
    /// picture whose name disagrees with its bytes.
    pub notes: Vec<String>,
}

impl Planned {
    /// Whether there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Embed `image` as the front cover of each of `tracks`, given the pictures each
/// one has now.
///
/// A track whose one front cover is already exactly this image is left out. The
/// note says how much the library grows by, net of the front covers replaced:
/// a cover is embedded once **per track**, so a 5 MB scan into fourteen tracks
/// is 70 MB.
#[must_use]
pub fn embed(tracks: &[(RelPath, Vec<Embedded>)], image: &CoverImage) -> Planned {
    let mut targets = Vec::new();
    let mut growth: i128 = 0;
    for (rel, pictures) in tracks {
        let fronts: Vec<&Embedded> = pictures.iter().filter(|p| p.kind.is_front()).collect();
        if let [front] = fronts.as_slice()
            && front.image.hash == image.image.hash
        {
            continue;
        }
        growth += i128::from(image.image.size)
            - fronts
                .iter()
                .map(|p| i128::from(p.image.size))
                .sum::<i128>();
        targets.push(rel.clone());
    }
    if targets.is_empty() {
        return Planned::default();
    }
    let notes = vec![format!(
        "embedding a {} image into {} adds {} to the library{}",
        human_bytes(image.image.size),
        plural(targets.len(), "file"),
        signed_bytes(growth),
        if growth < 0 {
            " (it replaces larger covers)"
        } else {
            ""
        }
    )];
    let delta = TagDelta::new().set_cover(image.clone());
    Planned {
        ops: targets
            .into_iter()
            .map(|target| Operation::WriteTags {
                target,
                changes: delta.clone(),
            })
            .collect(),
        notes,
    }
}

/// Remove the front cover from each of `tracks` that has one. Every other
/// picture stays.
#[must_use]
pub fn remove(tracks: &[(RelPath, Vec<Embedded>)]) -> Planned {
    let mut freed = 0u64;
    let targets: Vec<RelPath> = tracks
        .iter()
        .filter_map(|(rel, pictures)| {
            let fronts: u64 = pictures
                .iter()
                .filter(|p| p.kind.is_front())
                .map(|p| p.image.size)
                .sum();
            freed += fronts;
            (fronts > 0).then(|| rel.clone())
        })
        .collect();
    if targets.is_empty() {
        return Planned::default();
    }
    let notes = vec![format!(
        "removing the front cover from {} frees {}; other pictures stay",
        plural(targets.len(), "file"),
        human_bytes(freed)
    )];
    let delta = TagDelta::new().remove_cover();
    Planned {
        ops: targets
            .into_iter()
            .map(|target| Operation::WriteTags {
                target,
                changes: delta.clone(),
            })
            .collect(),
        notes,
    }
}

/// Why an extraction cannot be planned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractError {
    /// None of the tracks embeds a picture.
    #[error("no track in {dir} embeds a picture to extract")]
    NoPicture {
        /// The directory.
        dir: DirPath,
    },
    /// The name is not a plain file name.
    #[error("{name:?} is a path, not a file name")]
    NotAName {
        /// What was given.
        name: String,
    },
    /// The file the picture would be written to already holds it.
    #[error("{to} already holds that picture")]
    AlreadyThere {
        /// The image file.
        to: RelPath,
    },
}

/// Write the preferred picture of the first of `tracks` that has one out as an
/// image in `dir`, named `name` or `cover.<ext>` by its bytes.
///
/// `existing` is the hash of whatever is at the destination now, if anything:
/// the same bytes are [`ExtractError::AlreadyThere`] rather than a conflict,
/// and different ones are the plan's own conflict to report.
///
/// # Errors
///
/// [`ExtractError`].
pub fn extract(
    dir: &DirPath,
    tracks: &[(RelPath, Vec<Embedded>)],
    name: Option<&str>,
    existing: impl FnOnce(&RelPath) -> Option<u64>,
) -> Result<Planned, ExtractError> {
    let Some((from, picture)) = tracks
        .iter()
        .find_map(|(rel, pictures)| preferred(pictures).map(|p| (rel, p)))
    else {
        return Err(ExtractError::NoPicture { dir: dir.clone() });
    };

    let name = name.map_or_else(
        || format!("cover.{}", picture.image.format.extension()),
        ToOwned::to_owned,
    );
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(ExtractError::NotAName { name });
    }
    let to = dir
        .join(&name)
        .map_err(|_| ExtractError::NotAName { name: name.clone() })?;
    if existing(&to) == Some(picture.image.hash) {
        return Err(ExtractError::AlreadyThere { to });
    }

    let mut notes = vec![format!(
        "the {} of {} — {}, written byte for byte",
        picture.kind,
        from.file_name(),
        picture.image
    )];
    if !name_fits(&name, picture.image.format) {
        notes.push(format!(
            "the picture is {} but {name} says otherwise; a player trusting the name \
             may not open it",
            picture.image.format
        ));
    }
    Ok(Planned {
        ops: vec![Operation::ExtractArt {
            from: from.clone(),
            to,
            picture: picture.image,
        }],
        notes,
    })
}

/// Whether `name`'s extension is one a player would open `format` by.
fn name_fits(name: &str, format: ImageFormat) -> bool {
    let named = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    match format {
        ImageFormat::Jpeg => matches!(named.as_deref(), Some("jpg" | "jpeg")),
        ImageFormat::Tiff => matches!(named.as_deref(), Some("tif" | "tiff")),
        format => named.as_deref() == Some(format.extension()),
    }
}

/// The name a loose image would have as the directory's canonical cover:
/// `cover.` and its own extension, lower-cased, when that is one its bytes can
/// be opened by — `cover.jpeg` stays `cover.jpeg` — and otherwise the extension
/// its **bytes** call for, so a `folder.jpg` that is a PNG becomes `cover.png`.
#[must_use]
pub fn canonical_name(image: &Loose) -> String {
    let own = image
        .name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| name_fits(&format!("cover.{ext}"), image.image.format));
    format!(
        "cover.{}",
        own.unwrap_or_else(|| image.image.format.extension().to_owned())
    )
}

// ---------------------------------------------------------------------------
// Pairing singles with their covers.

/// One track to pair: where it is, and its title tag when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToPair<'a> {
    /// The track.
    pub rel: &'a RelPath,
    /// Its title tag.
    pub title: Option<&'a str>,
}

/// What [`pair`] decided for one track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paired {
    /// This image, by its index in the `loose` list.
    Image(usize),
    /// No image's name matches.
    NoMatch,
    /// More than one image matches equally well, so none is chosen. The indices
    /// of the candidates.
    Ambiguous(Vec<usize>),
}

/// Which loose image each track goes with, by name.
///
/// A directory of singles usually holds `Song A.jpg` beside `01 Song A.mp3`.
/// Names are compared as [`name_key`]s — lower case, punctuation folded to
/// spaces, a leading track number dropped — so `01 - Song A.mp3`, `Song A.flac`
/// and a track titled "Song A" all go with `song_a.JPG`.
///
/// Three tiers, and the first that finds anything decides:
///
/// 1. the track's file name or title is the **same** key as the image's;
/// 2. it **ends with** the image's key as whole words — `Artist - Song A` goes
///    with `Song A.jpg`;
/// 3. with anything in brackets dropped and the spaces taken out
///    ([`compact_key`]), it is the same as the image's or ends with it — which
///    is what a downloaded single looks like:
///    `Artist_-_Song_Title_(AMV)_[dQw4w9WgXcQ].mp3` beside `songtitle.jpg`.
///
/// More than one candidate in the deciding tier is [`Paired::Ambiguous`],
/// never a guess. Only JPEG and PNG are candidates, because nothing else can be
/// embedded.
#[must_use]
pub fn pair(tracks: &[ToPair<'_>], loose: &[Loose]) -> Vec<Paired> {
    let images: Vec<(usize, String, String)> = loose
        .iter()
        .enumerate()
        .filter(|(_, file)| file.image.format.embeddable())
        .map(|(at, file)| {
            let stem = stem(&file.name);
            (at, name_key(stem), compact_key(stem))
        })
        .filter(|(_, key, _)| !key.is_empty())
        .collect();

    tracks
        .iter()
        .map(|track| {
            let names: Vec<&str> = std::iter::once(stem(track.rel.file_name()))
                .chain(track.title)
                .collect();
            let keys: Vec<String> = names
                .iter()
                .map(|name| name_key(name))
                .filter(|key| !key.is_empty())
                .collect();
            let compact: Vec<String> = names
                .iter()
                .map(|name| compact_key(name))
                .filter(|key| !key.is_empty())
                .collect();

            let exact = |track: &str, image: &str| track == image;
            let words = |track: &str, image: &str| {
                image.len() >= MIN_SUFFIX && track.ends_with(&format!(" {image}"))
            };
            let packed =
                |track: &str, image: &str| image.len() >= MIN_PACKED && track.ends_with(image);

            // Name keys are compared with name keys and compact keys with
            // compact keys: a space-free key inside a spaced one is no match.
            let only_keys = |test: &dyn Fn(&str, &str) -> bool| -> Vec<usize> {
                images
                    .iter()
                    .filter(|(_, key, _)| keys.iter().any(|track| test(track, key)))
                    .map(|(at, _, _)| *at)
                    .collect()
            };
            let only_compact = |test: &dyn Fn(&str, &str) -> bool| -> Vec<usize> {
                images
                    .iter()
                    .filter(|(_, _, key)| compact.iter().any(|track| test(track, key)))
                    .map(|(at, _, _)| *at)
                    .collect()
            };
            let found = [only_keys(&exact), only_keys(&words), only_compact(&packed)]
                .into_iter()
                .find(|found| !found.is_empty())
                .unwrap_or_default();
            match found.as_slice() {
                [] => Paired::NoMatch,
                [one] => Paired::Image(*one),
                _ => Paired::Ambiguous(found),
            }
        })
        .collect()
}

/// The shortest image name that may match the end of a track's name: below
/// this, `a.jpg` would go with every song whose title ends in "a".
const MIN_SUFFIX: usize = 3;

/// The same, for [`compact_key`]s, which have lost the word boundaries that
/// keep a short name honest.
const MIN_PACKED: usize = 4;

/// A looser key than [`name_key`]: anything in `()`, `[]` or `{}` dropped — a
/// `(Remix)`, an `(AMV)`, a downloader's `[dQw4w9WgXcQ]` — and the spaces
/// taken out, so `maverickhunter.jpg` and `Maverick_Hunter_(AMV)` compare
/// equal.
///
/// ```
/// use mpdfm_core::art::plan::compact_key;
///
/// assert_eq!(compact_key("Artist_-_Song_Title_(AMV)_[dQw4w9WgXcQ]"), "artistsongtitle");
/// assert_eq!(compact_key("song-title"), "songtitle");
/// ```
#[must_use]
pub fn compact_key(name: &str) -> String {
    let mut kept = String::with_capacity(name.len());
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            c if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    name_key(&kept).replace(' ', "")
}

/// A file name without its extension.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// What two names are compared by: lower case, every run of anything that is
/// not a letter or a digit folded to one space, and a leading track number —
/// `01`, `1-03`, `07.` — dropped when something follows it.
///
/// ```
/// use mpdfm_core::art::plan::name_key;
///
/// assert_eq!(name_key("01 - Song A"), "song a");
/// assert_eq!(name_key("1-03. Song_A"), "song a");
/// assert_eq!(name_key("Song A (Remix)"), "song a remix");
/// assert_eq!(name_key("1999"), "1999");
/// ```
#[must_use]
pub fn name_key(name: &str) -> String {
    let words: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    // A track number is one or two leading words of digits (`1-03` is a disc
    // and a track), and only when there is a name after them: a single called
    // "1999" is named 1999.
    let numbers = words
        .iter()
        .take(2)
        .take_while(|word| word.chars().all(|c| c.is_ascii_digit()) && word.len() <= 3)
        .count();
    let skip = if numbers < words.len() { numbers } else { 0 };
    words[skip..].join(" ")
}

/// `+70.0 MB` or `-1.2 MB`.
fn signed_bytes(n: i128) -> String {
    let size = human_bytes(u64::try_from(n.unsigned_abs()).unwrap_or(u64::MAX));
    if n < 0 { format!("-{size}") } else { size }
}

/// `1 file`, `14 files`.
fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::art::{ImageFacts, PictureKind};

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).expect("a relative path")
    }

    fn facts(hash: u64, size: u64) -> ImageFacts {
        ImageFacts {
            format: ImageFormat::Jpeg,
            width: Some(600),
            height: Some(600),
            size,
            hash,
        }
    }

    fn front(hash: u64, size: u64) -> Embedded {
        Embedded {
            kind: PictureKind::FRONT,
            description: String::new(),
            declared: None,
            image: facts(hash, size),
        }
    }

    fn image(hash: u64, size: u64) -> CoverImage {
        CoverImage {
            source: Utf8PathBuf::from("/music/a/cover.jpg"),
            image: facts(hash, size),
        }
    }

    fn loose(name: &str, format: ImageFormat) -> Loose {
        Loose {
            name: name.to_owned(),
            rank: 3,
            image: ImageFacts {
                format,
                ..facts(1, 10)
            },
        }
    }

    #[test]
    fn embedding_skips_tracks_that_already_carry_that_cover() {
        let tracks = vec![
            (rel("a/01.mp3"), vec![front(7, 100)]),
            (rel("a/02.mp3"), vec![front(8, 300)]),
            (rel("a/03.mp3"), vec![]),
        ];
        let planned = embed(&tracks, &image(7, 100));
        let targets: Vec<&str> = planned.ops.iter().map(|op| op.source().as_str()).collect();
        assert_eq!(targets, ["a/02.mp3", "a/03.mp3"]);
        // 100 - 300 + 100 + 0.
        assert!(
            planned.notes[0].contains("into 2 files adds -100 B"),
            "{:?}",
            planned.notes
        );
        assert!(embed(&tracks[..1], &image(7, 100)).is_empty());
    }

    #[test]
    fn removing_touches_only_tracks_with_a_front_cover() {
        let back = Embedded {
            kind: PictureKind::BACK,
            ..front(9, 50)
        };
        let tracks = vec![
            (rel("a/01.mp3"), vec![front(7, 100), back.clone()]),
            (rel("a/02.mp3"), vec![back]),
        ];
        let planned = remove(&tracks);
        assert_eq!(planned.ops.len(), 1);
        assert!(planned.notes[0].contains("frees 100 B"));
        assert!(remove(&tracks[1..]).is_empty());
    }

    #[test]
    fn an_extraction_is_named_by_its_bytes_and_refuses_a_path_or_a_copy() {
        let dir = DirPath::parse("a").unwrap();
        let tracks = vec![
            (rel("a/01.mp3"), vec![]),
            (rel("a/02.mp3"), vec![front(7, 100)]),
        ];
        let planned = extract(&dir, &tracks, None, |_| None).unwrap();
        match &planned.ops[..] {
            [Operation::ExtractArt { from, to, .. }] => {
                assert_eq!(from.as_str(), "a/02.mp3");
                assert_eq!(to.as_str(), "a/cover.jpg");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            extract(&dir, &tracks, Some("x/y.jpg"), |_| None),
            Err(ExtractError::NotAName { .. })
        ));
        assert!(matches!(
            extract(&dir, &tracks, None, |_| Some(7)),
            Err(ExtractError::AlreadyThere { .. })
        ));
        assert!(matches!(
            extract(&dir, &tracks[..1], None, |_| None),
            Err(ExtractError::NoPicture { .. })
        ));
        let named = extract(&dir, &tracks, Some("front.png"), |_| None).unwrap();
        assert!(
            named
                .notes
                .iter()
                .any(|note| note.contains("says otherwise"))
        );
    }

    #[test]
    fn a_name_key_drops_numbers_case_and_punctuation() {
        assert_eq!(name_key("01 Song A"), "song a");
        assert_eq!(name_key("Song   A!"), "song a");
        assert_eq!(name_key("07."), "07");
        assert_eq!(name_key(""), "");
    }

    #[test]
    fn singles_pair_with_their_covers_by_file_name_or_title() {
        let tracks = [
            rel("s/01 Song A.mp3"),
            rel("s/02 - song_b.flac"),
            rel("s/Artist - Song C.mp3"),
            rel("s/04 Untitled.mp3"),
            rel("s/05 Track.mp3"),
        ];
        let titles = [None, None, None, None, Some("Song D")];
        let images = [
            loose("Song A.jpg", ImageFormat::Jpeg),
            loose("Song B.PNG", ImageFormat::Png),
            loose("song c.jpeg", ImageFormat::Jpeg),
            loose("Song D.jpg", ImageFormat::Jpeg),
            loose("cover.jpg", ImageFormat::Jpeg),
        ];
        let to_pair: Vec<ToPair<'_>> = tracks
            .iter()
            .zip(titles)
            .map(|(rel, title)| ToPair { rel, title })
            .collect();
        assert_eq!(
            pair(&to_pair, &images),
            [
                Paired::Image(0),
                Paired::Image(1),
                Paired::Image(2),
                Paired::NoMatch,
                Paired::Image(3),
            ]
        );
    }

    #[test]
    fn two_equally_good_images_are_ambiguous_and_an_unembeddable_one_is_never_chosen() {
        let track = rel("s/01 Song A.mp3");
        let to_pair = [ToPair {
            rel: &track,
            title: None,
        }];
        let twins = [
            loose("Song A.jpg", ImageFormat::Jpeg),
            loose("song-a.png", ImageFormat::Png),
        ];
        assert_eq!(pair(&to_pair, &twins), [Paired::Ambiguous(vec![0, 1])]);
        let webp = [loose("Song A.webp", ImageFormat::Webp)];
        assert_eq!(pair(&to_pair, &webp), [Paired::NoMatch]);
    }

    #[test]
    fn a_downloaded_single_goes_with_its_squashed_cover_name() {
        let tracks = [
            rel("s/Artist_-_Song_Title_(AMV)_[dQw4w9WgXcQ].mp3"),
            rel("s/Artist_-_Ghost_[abcDEF12345].mp3"),
            rel("s/Artist_-_Night_Drive_(Official_Video)_[xyz_-012345].mp3"),
        ];
        let to_pair: Vec<ToPair<'_>> = tracks
            .iter()
            .map(|rel| ToPair { rel, title: None })
            .collect();
        let images = [
            loose("ghost.jpg", ImageFormat::Jpeg),
            loose("nightdrive.jpg", ImageFormat::Jpeg),
            loose("songtitle.png", ImageFormat::Png),
        ];
        assert_eq!(
            pair(&to_pair, &images),
            [Paired::Image(2), Paired::Image(0), Paired::Image(1)]
        );
    }

    #[test]
    fn a_closer_tier_wins_over_a_looser_one() {
        // `Song A (Remix)` is exactly `song a remix`; the bracket-dropping tier
        // would also have offered `Song A.jpg`, and must not get the chance.
        let track = rel("s/01 Song A (Remix).mp3");
        let to_pair = [ToPair {
            rel: &track,
            title: None,
        }];
        let images = [
            loose("Song A.jpg", ImageFormat::Jpeg),
            loose("Song A Remix.jpg", ImageFormat::Jpeg),
        ];
        assert_eq!(pair(&to_pair, &images), [Paired::Image(1)]);
    }

    #[test]
    fn the_canonical_name_keeps_an_extension_that_fits_the_bytes() {
        let jpeg = |name: &str| loose(name, ImageFormat::Jpeg);
        assert_eq!(canonical_name(&jpeg("front.JPEG")), "cover.jpeg");
        assert_eq!(canonical_name(&jpeg("folder.jpg")), "cover.jpg");
        assert_eq!(
            canonical_name(&jpeg("scan.png")),
            "cover.jpg",
            "the bytes win"
        );
    }

    #[test]
    fn a_short_name_does_not_match_the_end_of_every_title() {
        let track = rel("s/01 Viva la Vida.mp3");
        let to_pair = [ToPair {
            rel: &track,
            title: None,
        }];
        assert_eq!(
            pair(&to_pair, &[loose("a.jpg", ImageFormat::Jpeg)]),
            [Paired::NoMatch]
        );
        assert_eq!(
            pair(&to_pair, &[loose("La Vida.jpg", ImageFormat::Jpeg)]),
            [Paired::Image(0)]
        );
    }
}
