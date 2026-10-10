//! Playlists and MPD's saved queue, against the library.
//!
//! # The CUE track the plan called broken
//!
//! `docs/PLAN.md` §3 recorded one broken reference in the real library, a CUE
//! virtual track: `…/Imagine Dragons - Mercury - Acts 1.flac.cue/track0017`.
//! Tasks 07 and 15 both looked and found it is not broken — the sheet exists, is
//! a multi-`FILE` sheet, contains track 17, and the file track 17 names is on
//! disk. [`PlaylistIndex::broken`] only answers "is the sheet there?"; this
//! module answers the rest, by opening the sheet. On the real library that finds
//! nothing, and that is the right answer.
//!
//! [`PlaylistIndex::broken`]: crate::playlist::PlaylistIndex::broken

use std::collections::{BTreeMap, BTreeSet};

use camino::Utf8Path;

use super::super::{Check, Info, Inputs, Item, Queue};
use crate::library::{DirPath, Library};
use crate::paths::RelPath;
use crate::playlist::{Entry, IndexWarning, PlaylistIndex};

/// Run one check of this group.
pub(crate) fn run(info: Info, inputs: &Inputs<'_>) -> Check {
    let Inputs {
        library,
        index,
        index_warnings,
        queue,
    } = inputs;
    match info.name {
        "broken-references" => Check::ran(info, broken_references(library, index)),
        "unrewritable-entries" => Check::ran(info, unrewritable_entries(index, library.root())),
        "unreadable-playlists" => Check::ran(info, unreadable_playlists(index_warnings)),
        "broken-queue-entries" => match queue {
            Queue::NotConfigured => Check::skipped(info, "no MPD state file is configured"),
            Queue::Unreadable(why) => Check::skipped(info, format!("the state file: {why}")),
            Queue::Loaded(state) => Check::ran(
                info,
                state
                    .lines()
                    .iter()
                    .filter_map(|line| Some((line.index()?, line.rel()?)))
                    .filter(|(_, rel)| !resolves(library, rel))
                    .map(|(position, rel)| {
                        Item::new(
                            format!("queue position {position} → {rel}"),
                            "names a file that is not in the library; MPD drops it on load",
                        )
                    })
                    .collect(),
            ),
        },
        "duplicate-entries" => Check::ran(info, duplicate_entries(index)),
        "unreferenced-audio" => Check::ran(info, unreferenced_audio(library, index)),
        _ => super::unknown(info),
    }
}

/// Whether a queue path names something in the library: a file, or a virtual
/// track `…/x.cue/trackNNNN` whose sheet is there.
fn resolves(library: &Library, rel: &RelPath) -> bool {
    if library.get(rel).is_some() {
        return true;
    }
    let dir = DirPath::of(rel);
    dir.as_rel().is_some_and(|sheet| {
        sheet.as_str().to_ascii_lowercase().ends_with(".cue")
            && rel.file_name().starts_with("track")
            && library.get(sheet).is_some()
    })
}

/// `playlist:line → path`, the way a user finds the line.
fn locate(index: &PlaylistIndex, playlist: usize, entry: usize, target: &str) -> String {
    let name = index
        .playlist(playlist)
        .map_or("?", |playlist| playlist.name());
    // The entry index is 0-based and a user counts lines from one.
    format!("{name}:{} → {target}", entry + 1)
}

/// Playlist lines naming a file that is not in the library, or a CUE track the
/// sheet does not have.
///
/// The file half is [`PlaylistIndex::broken`], against the scanned model rather
/// than the disk: a reference to a file the scan skipped — a symlinked track, a
/// name that is not UTF-8 — is reported too, because a file MPDFM will not move
/// is a file it cannot promise about.
fn broken_references(library: &Library, index: &PlaylistIndex) -> Vec<Item> {
    let mut found: Vec<(crate::playlist::Ref, Item)> = index
        .broken(library)
        .into_iter()
        .map(|(reference, path)| {
            let target = index
                .entry(reference)
                .map_or_else(|| path.to_string(), |entry| entry.line().to_owned());
            (
                reference,
                Item::new(
                    locate(index, reference.playlist, reference.entry, &target),
                    "names a file that is not in the library",
                ),
            )
        })
        .collect();

    // Sheets are read once each, however many lines name tracks in them.
    let mut sheets: BTreeMap<&RelPath, Option<Sheet>> = BTreeMap::new();
    for (reference, sheet_rel, track) in index.cue_refs() {
        if library.get(sheet_rel).is_none() {
            continue; // already reported above, as a missing file
        }
        let sheet = sheets
            .entry(sheet_rel)
            .or_insert_with(|| Sheet::read(&sheet_rel.to_abs(library.root())));
        let Some(sheet) = sheet else {
            continue; // unreadable: nothing worth claiming either way
        };
        let Some(reason) = sheet.problem(track, sheet_rel, library) else {
            continue;
        };
        let line = format!("{sheet_rel}/{track}");
        found.push((
            reference,
            Item::new(
                locate(index, reference.playlist, reference.entry, &line),
                reason,
            ),
        ));
    }

    found.sort_by_key(|(reference, _)| *reference);
    found.into_iter().map(|(_, item)| item).collect()
}

/// A CUE sheet, as far as checking a reference into it needs.
#[derive(Debug)]
struct Sheet {
    /// Every `TRACK`, in sheet order: its number, and the `FILE` it belongs to.
    tracks: Vec<(u32, Option<String>)>,
}

impl Sheet {
    /// Read a sheet, or `None` when it cannot be read at all.
    ///
    /// Lossy UTF-8: real sheets are often Latin-1, and only the `FILE` names are
    /// used as text, where a mangled name simply fails to match and is then not
    /// reported (see [`Sheet::problem`]).
    fn read(abs: &Utf8Path) -> Option<Self> {
        let bytes = std::fs::read(abs).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let mut file = None;
        let mut tracks = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            let mut words = line.split_whitespace();
            match words.next().map(str::to_ascii_uppercase).as_deref() {
                Some("FILE") => file = quoted(line["FILE".len()..].trim()),
                Some("TRACK") => {
                    if let Some(number) = words.next().and_then(|n| n.parse().ok()) {
                        tracks.push((number, file.clone()));
                    }
                }
                _ => {}
            }
        }
        Some(Self { tracks })
    }

    /// Why `trackNNNN` in this sheet would not play, or `None` if it would — or
    /// if this cannot tell.
    ///
    /// Two readings of the number are accepted, because MPD numbers the virtual
    /// tracks of a sheet in sheet order and real sheets number their `TRACK`s
    /// from 1 in that same order; a sheet where the two disagree is unusual
    /// enough that either one resolving is taken as resolving. Only when **no**
    /// reading finds the track is it reported.
    ///
    /// A track found but whose `FILE` is missing is reported too — unless the
    /// `FILE` name is not something MPDFM can resolve inside the library, in
    /// which case nothing is claimed.
    fn problem(&self, track: &str, sheet: &RelPath, library: &Library) -> Option<String> {
        let number: u32 = track.strip_prefix("track")?.parse().ok()?;
        let by_order = usize::try_from(number)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|at| self.tracks.get(at));
        let by_number = self.tracks.iter().find(|(n, _)| *n == number);
        let candidates: Vec<&(u32, Option<String>)> =
            by_order.into_iter().chain(by_number).collect();

        if candidates.is_empty() {
            return Some(format!(
                "the CUE sheet has {} track(s) and no track {number}",
                self.tracks.len()
            ));
        }
        let dir = DirPath::of(sheet);
        let mut missing = None;
        for (_, file) in candidates {
            let Some(rel) = file.as_deref().and_then(|name| dir.join(name).ok()) else {
                return None; // a FILE MPDFM cannot place: claim nothing
            };
            if library.get(&rel).is_some() {
                return None;
            }
            missing = Some(rel);
        }
        missing.map(|rel| format!("the CUE sheet's track {number} plays {rel}, which is missing"))
    }
}

/// The file name in a `FILE "name" WAVE` line: the quoted part, or the first
/// word when it is not quoted.
fn quoted(rest: &str) -> Option<String> {
    if let Some(inner) = rest.strip_prefix('"') {
        return inner.split_once('"').map(|(name, _)| name.to_owned());
    }
    rest.split_whitespace().next().map(ToOwned::to_owned)
}

/// Playlist lines MPDFM cannot resolve against the music directory.
///
/// An absolute path, a `./` path, a backslash path. The parser keeps them as
/// [`Entry::Unparsed`] so they round-trip byte-for-byte, and that is the right
/// answer — but it also means a move will not rewrite them, so the line will be
/// pointing at the old location afterwards and it will look like MPDFM broke it.
/// Saying so now is the point.
///
/// A line that is merely odd — a stray carriage return, say — is also
/// `Unparsed` and is *not* reported: it names no path, so no move can strand it.
fn unrewritable_entries(index: &PlaylistIndex, root: &Utf8Path) -> Vec<Item> {
    let mut items = Vec::new();
    for (at, playlist) in index.playlists().iter().enumerate() {
        for (line, entry) in playlist.entries().iter().enumerate() {
            let Entry::Unparsed(text) = entry else {
                continue;
            };
            let Some(mut reason) = unresolvable_path(text) else {
                continue;
            };
            // An absolute path that happens to point inside the library has an
            // obvious fix, so offer the exact line it should be.
            let inside = Utf8Path::new(text.trim()).strip_prefix(root).ok();
            if text.trim().starts_with('/') && inside.is_none() {
                reason = "an absolute path outside the music directory";
            }
            let item = Item::new(
                locate(index, at, line, text),
                format!("{reason}; a move will not rewrite this line"),
            );
            items.push(match inside {
                Some(rel) => item.with_fix(format!("replace it with `{rel}`")),
                None => item,
            });
        }
    }
    items
}

/// Why a line cannot be a path relative to the music directory, or `None` if it
/// does not look like a path at all.
fn unresolvable_path(line: &str) -> Option<&'static str> {
    let text = line.trim();
    if text.starts_with('/') {
        Some("an absolute path")
    } else if text.starts_with("~/") {
        Some("a path that relies on shell expansion")
    } else if text.starts_with("./") || text.starts_with("../") || text.contains("/../") {
        Some("a path with `.` or `..` in it")
    } else if text.contains('\\') {
        Some("a path with a backslash in it")
    } else {
        None
    }
}

/// Playlists MPDFM could not index.
///
/// Its own check rather than a warning on stderr, because the consequence is the
/// same as a broken reference and worse: a playlist MPDFM cannot read is one it
/// cannot fix, so a move will silently leave every line in it stale.
fn unreadable_playlists(warnings: &[IndexWarning]) -> Vec<Item> {
    warnings
        .iter()
        .map(|warning| Item::new(warning.path(), warning.to_string()))
        .collect()
}

/// The same track line more than once in one playlist.
///
/// A note: a playlist that plays a favourite twice may mean to. Lines compare
/// by what they name — the file and the CUE track — so `#EXTINF` and comments
/// never count.
fn duplicate_entries(index: &PlaylistIndex) -> Vec<Item> {
    let mut items = Vec::new();
    for playlist in index.playlists() {
        let mut lines: BTreeMap<(&RelPath, Option<&str>), Vec<usize>> = BTreeMap::new();
        for (at, entry) in playlist.entries().iter().enumerate() {
            if let Some(rel) = entry.rel() {
                lines.entry((rel, entry.cue())).or_default().push(at + 1);
            }
        }
        for ((rel, cue), at) in lines {
            if at.len() < 2 {
                continue;
            }
            let target = cue.map_or_else(|| rel.to_string(), |cue| format!("{rel}/{cue}"));
            let numbers: Vec<String> = at.iter().map(ToString::to_string).collect();
            items.push(Item::new(
                format!("{} → {target}", playlist.name()),
                format!("on lines {}", numbers.join(", ")),
            ));
        }
    }
    items
}

/// Audio files no playlist mentions.
///
/// Informational, and the check most likely to be misread: on the real library
/// this is 2 500-odd files, because 17 playlists hold 231 references between
/// them. It is here because it is the work queue for "what have I never filed",
/// not because anything is broken.
///
/// A track described by a referenced CUE sheet counts as referenced: a playlist
/// line `album.flac.cue/track0017` keys under the sheet, and the sheet is what
/// carries `album.flac` with it.
fn unreferenced_audio(library: &Library, index: &PlaylistIndex) -> Vec<Item> {
    let mut referenced: BTreeSet<&str> = index.paths().iter().map(|rel| rel.as_str()).collect();
    // `…/album.flac.cue` referenced means `…/album.flac` is referenced too.
    let through_cue: Vec<&str> = referenced
        .iter()
        .filter_map(|path| path.strip_suffix(".cue"))
        .collect();
    referenced.extend(through_cue);

    library
        .entries()
        .iter()
        .filter(|entry| entry.is_audio() && !referenced.contains(entry.rel.as_str()))
        .map(|entry| Item::new(entry.rel.as_str(), ""))
        .collect()
}
