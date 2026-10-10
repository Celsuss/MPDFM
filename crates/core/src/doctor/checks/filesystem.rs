//! Names and directories — everything answerable from the scan alone, with no
//! file opened.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use unicode_normalization::UnicodeNormalization as _;

use super::super::{Check, Info, Item, shell_quote};
use crate::library::{DirPath, Kind, Library, ScanWarning};

/// Run one check of this group.
pub(crate) fn run(info: Info, library: &Library) -> Check {
    let items = match info.name {
        "bad-names" => bad_names(library),
        "unreadable-paths" => library
            .warnings()
            .iter()
            .filter(|warning| matches!(warning, ScanWarning::Unreadable { .. }))
            .map(|warning| Item::new(warning.path(), warning.to_string()))
            .collect(),
        "normalization-twins" => twins(library, Fold::Normalization),
        "case-collisions" => twins(library, Fold::Case),
        "unfiled-audio" => unfiled_audio(library),
        "empty-dirs" => empty_dirs(library),
        "orphan-aux" => orphan_aux(library),
        "no-audio-dirs" => no_audio_dirs(library),
        "partial-downloads" => partial_downloads(library),
        _ => super::unknown(info),
    };
    Check::ran(info, items)
}

/// Names the scan had to skip. Every one of these is a file no move can
/// promise about, which is why it is a problem rather than a curiosity.
fn bad_names(library: &Library) -> Vec<Item> {
    library
        .warnings()
        .iter()
        .filter_map(|warning| match warning {
            ScanWarning::NotUtf8 { lossy } => Some(Item::new(
                lossy.as_str(),
                "not valid UTF-8, so MPDFM skips it and MPD may not show it",
            )),
            ScanWarning::Unnamable { path, reason } => Some(Item::new(
                path.as_str(),
                format!("cannot be a library path: {reason}"),
            )),
            ScanWarning::Symlink { .. } | ScanWarning::Unreadable { .. } => None,
        })
        .collect()
}

/// How two sibling names are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    /// NFC: equal means the same characters, composed differently.
    Normalization,
    /// NFC and then lower case: equal means the same name on a case-insensitive
    /// filesystem. Pairs that are already normalization twins are left to that
    /// check, so one pair is not reported twice.
    Case,
}

/// Every name in `library`'s directories that collides with a sibling under
/// `fold`.
///
/// Siblings means the files *and* subdirectories of one directory, because a
/// file `Live` and a directory `live` collide just the same on a filesystem
/// that cannot tell them apart. One item per colliding group.
fn twins(library: &Library, fold: Fold) -> Vec<Item> {
    let mut items = Vec::new();
    for (dir, contents) in library.dirs() {
        let names = contents
            .files()
            .iter()
            .filter_map(|index| library.entry(*index))
            .map(|entry| entry.file_name())
            .chain(contents.subdirs().iter().filter_map(DirPath::file_name));

        let mut groups: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for name in names {
            let nfc: String = name.nfc().collect();
            let key = match fold {
                Fold::Normalization => nfc,
                Fold::Case => nfc.to_lowercase(),
            };
            groups.entry(key).or_default().insert(name);
        }

        for names in groups.into_values().filter(|names| names.len() > 1) {
            if fold == Fold::Case {
                let distinct: BTreeSet<String> = names.iter().map(|n| n.nfc().collect()).collect();
                if distinct.len() < 2 {
                    continue; // normalization twins, reported by that check
                }
            }
            let listed: Vec<String> = names
                .iter()
                .map(|name| match dir.as_rel() {
                    Some(dir) => format!("{dir}/{name}"),
                    None => (*name).to_owned(),
                })
                .collect();
            let detail = match fold {
                Fold::Normalization => {
                    "look identical and are different names: a playlist naming one does not \
                     find the other"
                }
                Fold::Case => {
                    "differ only in case, so they collide on a case-insensitive filesystem"
                }
            };
            items.push(Item::new(listed.join("  ≡  "), detail));
        }
    }
    items
}

/// Tracks sitting in the root of the music directory, outside any album.
fn unfiled_audio(library: &Library) -> Vec<Item> {
    library
        .files_in(&DirPath::root())
        .filter(|entry| entry.is_audio())
        .map(|entry| {
            Item::new(entry.rel.as_str(), "not in any directory").with_fix(format!(
                "mpdfm organize {}",
                shell_quote(entry.rel.as_str())
            ))
        })
        .collect()
}

/// Directories holding nothing at all.
fn empty_dirs(library: &Library) -> Vec<Item> {
    library
        .dirs()
        .filter(|(dir, contents)| !dir.is_root() && contents.is_empty())
        .map(|(dir, _)| {
            // Absolute, because `rmdir` runs wherever the user is and not in the
            // music directory; quoted, because almost every album directory in
            // this library has a space in it.
            let abs = dir.to_abs(library.root());
            Item::new(dir.as_str(), "holds no files and no subdirectories")
                .with_fix(format!("rmdir {}", shell_quote(abs.as_str())))
        })
        .collect()
}

/// Whether `dir`, or anything below it, holds audio.
fn has_audio_below(library: &Library, dir: &DirPath) -> bool {
    library.entries().iter().any(|entry| {
        // `starts_with_dir` is *strictly* inside, so the directory's own
        // files are a second test.
        let at = entry.dir();
        entry.is_audio() && (at == *dir || at.starts_with_dir(dir))
    })
}

/// Whether `dir` belongs to a release, so an audioless directory inside it is
/// part of that release — its `Scans`, its `Covers` — and not an orphan.
///
/// Three shapes count: an album directory; the root of a multi-disc set; and a
/// **wrapper**, a directory with no audio of its own and exactly one
/// subdirectory with audio below it, which is how a torrent arrives
/// (`Fakear - Animal [FRG]/` holding the album and `My Uploads/`). The third is
/// a guess, and it errs towards silence: an artist directory with one album and
/// one cover-only directory reads as a wrapper too, and that orphan goes
/// unreported rather than a release's own clutter being called one.
fn is_release(library: &Library, dir: &DirPath) -> bool {
    if library.album_dir(dir).is_some()
        || library
            .album_dirs()
            .iter()
            .any(|album| album.set_root.as_ref() == Some(dir))
    {
        return true;
    }
    let with_audio = library
        .subdirs_in(dir)
        .iter()
        .filter(|sub| has_audio_below(library, sub))
        .count();
    !dir.is_root() && with_audio == 1
}

/// Leaf directories with files and no audio, and whether each hangs off a
/// release — the split between `no-audio-dirs` and `orphan-aux`.
fn audioless_leaves(library: &Library) -> Vec<(&DirPath, usize, bool)> {
    library
        .dirs()
        .filter(|(dir, contents)| {
            !dir.is_root()
                && !contents.is_empty()
                && contents.subdirs().is_empty()
                && !library.files_in(dir).any(|entry| entry.is_audio())
        })
        .map(|(dir, contents)| {
            let attached = dir
                .parent()
                .is_some_and(|parent| is_release(library, &parent));
            (dir, contents.files().len(), attached)
        })
        .collect()
}

/// Leaf directories holding files but no audio, **inside a release**: the
/// `Scans`, `Covers` and `My Uploads` of an album. Normal, so a note — the real
/// library has three and every one is right. (Task 15 counted five; the other
/// two are `cover.jpg`-only directories in an artist directory, which
/// [`orphan_aux`] now reports, because no release is around them.)
///
/// A directory with subdirectories is excluded, because that is what a genre
/// directory and a multi-disc set root both look like and neither is a defect.
fn no_audio_dirs(library: &Library) -> Vec<Item> {
    audioless_leaves(library)
        .into_iter()
        .filter(|(_, _, attached)| *attached)
        .map(|(dir, files, _)| {
            Item::new(dir.as_str(), format!("{files} file(s), none of them audio"))
        })
        .collect()
}

/// Cover art, `.nfo`, `.sfv` and the rest, in a directory with no audio in or
/// below it and no release around it. Every file in such a directory is listed,
/// a stray `Thumbs.db` included: there is no album for any of it to belong to.
///
/// Either an album was moved by something that did not take its aux files, or
/// this is a stray. A release's own `Scans` directory is not an orphan — it is
/// a [`no_audio_dirs`] note — and neither is a multi-disc set root's
/// `folder.jpg`, which has audio below it. No fix is offered: whether to delete
/// someone's cover art is not an obvious call.
fn orphan_aux(library: &Library) -> Vec<Item> {
    let mut items = Vec::new();
    for (dir, contents) in library.dirs() {
        if dir.is_root() || has_audio_below(library, dir) {
            continue;
        }
        if dir
            .parent()
            .is_some_and(|parent| is_release(library, &parent))
        {
            continue;
        }
        let aux: Vec<&str> = contents
            .files()
            .iter()
            .filter_map(|index| library.entry(*index))
            .map(|entry| entry.file_name())
            .collect();
        if !aux.is_empty() {
            items.push(Item::new(dir.as_str(), aux.join(", ")));
        }
    }
    items
}

/// Partial downloads that look like they are still being written.
///
/// A `.parts` file is [`Kind::Other`]: scanned, counted, moved with its album,
/// never interpreted. That is the right answer and `doctor` has no complaint
/// about one — except this: moving a file something is still downloading into is
/// a real way to confuse the downloader, and MPDFM has no way to know whether it
/// is. A `.parts` newer than everything else in its directory is the best
/// available guess, so it is said out loud before somebody moves that album.
fn partial_downloads(library: &Library) -> Vec<Item> {
    // The newest mtime in each directory, from everything that is not itself a
    // partial download.
    let mut newest: BTreeMap<DirPath, SystemTime> = BTreeMap::new();
    for entry in library.entries() {
        if is_partial(entry.rel.extension()) {
            continue;
        }
        newest
            .entry(entry.dir())
            .and_modify(|at| *at = (*at).max(entry.mtime))
            .or_insert(entry.mtime);
    }

    library
        .entries()
        .iter()
        .filter(|entry| is_partial(entry.rel.extension()))
        .filter(|entry| newest.get(&entry.dir()).is_none_or(|at| entry.mtime > *at))
        .map(|entry| {
            Item::new(
                entry.rel.as_str(),
                "newer than everything else in its directory — a download in progress? \
                 Moving it now would confuse whatever is writing it.",
            )
        })
        .collect()
}

/// Whether this extension names a partial download.
fn is_partial(extension: Option<&str>) -> bool {
    extension.is_some_and(|ext| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "parts" | "part" | "crdownload"
        ) && Kind::from_extension(Some(ext)) == Kind::Other
    })
}
