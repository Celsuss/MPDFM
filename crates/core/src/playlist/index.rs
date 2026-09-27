//! [`PlaylistIndex`] — the lookup every safe move is built on: given a file or a
//! directory, which playlist lines point at it, and which of them already point
//! at nothing.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::library::Library;
//! use mpdfm_core::paths::RelPath;
//! use mpdfm_core::playlist::PlaylistIndex;
//!
//! let (index, warnings) = PlaylistIndex::load(Utf8Path::new("/home/me/.config/mpd/playlists"));
//! for warning in &warnings {
//!     eprintln!("mpdfm: warning: {warning}");
//! }
//!
//! // Everything a move of one album would have to rewrite.
//! let album = RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)")?;
//! for (track, refs) in index.refs_under_dir(&album) {
//!     println!("{track} is referenced {} time(s)", refs.len());
//! }
//!
//! // And the references that were broken before MPDFM ever touched anything.
//! let library = Library::scan(Utf8Path::new("/home/me/Music"))?;
//! for (reference, missing) in index.broken(&library) {
//!     let playlist = index.playlist(reference.playlist).expect("a reference names its playlist");
//!     println!("{}: line {} points at the absent {missing}", playlist.name(), reference.entry + 1);
//! }
//! # Ok::<(), mpdfm_core::Error>(())
//! ```
//!
//! # What makes a lookup correct here
//!
//! **Exact identity, never a string prefix.** A [`RelPath`] key is matched
//! whole, and [`PlaylistIndex::refs_under_dir`] compares component-wise, so
//! moving `hiphop/MF DOOM` leaves `hiphop/MF DOOM Instrumentals` alone. String
//! prefixing would rewrite the second album's lines while moving the first, and
//! safety invariant 3 exists to forbid exactly that.
//!
//! **A CUE virtual track indexes under its sheet.** The parser (task 06) already
//! splits `…/album.flac.cue/track0017` into the `.cue` file and the `track0017`
//! suffix, so the reference lands under the path of a file that really exists.
//! Moving the sheet — or the album directory holding it — therefore finds the
//! line, and [`PlaylistIndex::broken`] does not report a perfectly good virtual
//! track as missing just because `track0017` is not a file.
//!
//! **Only track lines are indexed.** URLs, `#EXTINF`, comments and blanks have no
//! [`Entry::rel`], so they cannot appear in the index, cannot be matched by a
//! move, and cannot be rewritten by task 09.
//!
//! **Every reference is kept, not deduplicated.** One file may be listed by
//! several playlists, and twice within one playlist (`Duplicates.m3u` is exactly
//! that case). The index returns all of them; a rewrite that fixed only the first
//! would leave a playlist half-broken.
//!
//! # Cheap by design
//!
//! 17 playlists and 231 entries: a full reload costs a few hundred microseconds,
//! so every operation reloads rather than invalidating a cache. A stale cache
//! would rewrite the wrong line — the one failure mode this whole crate exists to
//! avoid — and no benchmark yet says the cache would buy anything. `index.rs` is
//! the wrong place to get clever.
//!
//! # Warnings, not errors
//!
//! A playlist directory with one unreadable file in it is still a playlist
//! directory, and refusing to index the other sixteen would be the wrong trade.
//! [`PlaylistIndex::load`] therefore returns whatever it could read plus a
//! [`IndexWarning`] for each thing it could not — a dangling symlink, a file that
//! is not UTF-8 — and the caller decides whether to go ahead. Task 10's preview
//! shows them; a move that must be sure it is complete refuses to run while any
//! are outstanding.
//!
//! Containment (safety invariant 5) is deliberately *not* checked here: the real
//! `Radios.m3u` is a symlink into a dotfiles repository, which is outside every
//! configured root and still a playlist MPDFM must index. The check belongs to
//! the commit path, on [`Playlist::real_path`], where the roots are known.

use std::collections::{BTreeSet, HashMap};

use camino::{Utf8Path, Utf8PathBuf};

use super::{Entry, ParseError, Playlist, is_playlist_name};
use crate::Error;
use crate::library::Library;
use crate::paths::RelPath;

/// One playlist line that names a track: which playlist, and which line of it.
///
/// Both are indices — into [`PlaylistIndex::playlists`] and into that playlist's
/// [`Playlist::entries`] — rather than paths and strings, so a reference stays
/// small, stays copyable, and cannot disagree with the playlist it came from.
/// They are only valid for the index that produced them; an index is rebuilt
/// after every operation, and references from an older one are stale.
///
/// `entry` is a 0-based line index. Add one before showing it to a user, who
/// counts lines from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ref {
    /// Index into [`PlaylistIndex::playlists`].
    pub playlist: usize,
    /// Index into that playlist's [`Playlist::entries`].
    pub entry: usize,
}

/// Something in the playlist directory that could not be indexed, reported
/// rather than raised.
///
/// Every variant means "this file is not in the index", so a caller that needs
/// to be certain a move rewrites every reference checks the warnings before it
/// commits: a playlist MPDFM could not read is a playlist it cannot fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IndexWarning {
    /// A playlist name that is not valid UTF-8. Reported and skipped, never
    /// guessed at (`docs/PLAN.md` safety invariant 8). `lossy` is for the
    /// message only — it does not round-trip to the bytes on disk.
    ///
    /// Names that are not UTF-8 *and* not playlist names are ignored in silence,
    /// like any other non-playlist file: the lossy rendering is enough to tell
    /// the two apart.
    #[error("skipped a playlist name that is not valid UTF-8: {lossy}")]
    NotUtf8 {
        /// The path with invalid sequences replaced. Never write it to disk.
        lossy: String,
    },

    /// A playlist, or the directory itself, that could not be read. A dangling
    /// symlink is this one: `Radios.m3u` pointing at a dotfiles repository that
    /// is not checked out yet.
    #[error("cannot read {path}: {message}")]
    Unreadable {
        /// The path that could not be read — the link itself, when it is one.
        path: String,
        /// The operating system's complaint.
        message: String,
    },

    /// A playlist that was read but could not be parsed, which today means bytes
    /// that are not UTF-8.
    #[error("skipped {path}: {reason}")]
    Unparsable {
        /// The playlist, as the filesystem spells it.
        path: String,
        /// Why it could not be parsed.
        reason: ParseError,
    },
}

impl IndexWarning {
    /// The path the warning is about, which is what it is sorted and looked up
    /// by.
    ///
    /// For [`IndexWarning::NotUtf8`] this is the lossy rendering, the only
    /// spelling there is. It is a label, never an identity.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::NotUtf8 { lossy } => lossy,
            Self::Unreadable { path, .. } | Self::Unparsable { path, .. } => path,
        }
    }
}

/// Every playlist in one directory, with a reverse index from track identity to
/// the lines that name it.
///
/// Built by [`PlaylistIndex::load`] and then read-only. Task 09 rewrites lines by
/// consuming it with [`PlaylistIndex::into_playlists`]; nothing mutates a
/// playlist through the index, because the index would then describe a file that
/// no longer exists in that shape.
#[derive(Debug, Clone)]
pub struct PlaylistIndex {
    dir: Utf8PathBuf,
    playlists: Vec<Playlist>,
    /// Exact track path → every line naming it, in playlist-then-line order.
    by_path: HashMap<RelPath, Vec<Ref>>,
}

/// The answer [`PlaylistIndex::refs_to`] gives for a path nothing references.
/// A borrowed empty slice, so the common case allocates nothing.
const NO_REFS: &[Ref] = &[];

impl PlaylistIndex {
    /// Read and index every playlist in `playlist_dir`.
    ///
    /// Never fails: a directory that cannot be listed yields an empty index and
    /// one [`IndexWarning::Unreadable`], and so does each playlist that cannot be
    /// read or parsed. Whether the directory is a usable root at all is settled
    /// earlier, by [`Config::require_playlist_dir`][crate::config::Config::require_playlist_dir].
    ///
    /// Files that are not playlists are skipped by extension, and subdirectories
    /// are skipped outright — MPD ignores both, and warning about the `.jpg` a
    /// user keeps next to their playlists would be noise, not information.
    /// Playlists are indexed in file-name order (byte-wise), so that a `Ref` means
    /// the same thing on two runs; `readdir` order does not, on ext4 or anywhere
    /// else.
    #[must_use]
    pub fn load(playlist_dir: &Utf8Path) -> (Self, Vec<IndexWarning>) {
        let mut warnings = Vec::new();
        let mut playlists = Vec::new();

        for name in playlist_file_names(playlist_dir, &mut warnings) {
            let path = playlist_dir.join(&name);
            match Playlist::load(&path) {
                Ok(playlist) => playlists.push(playlist),
                Err(Error::Playlist(reason)) => warnings.push(IndexWarning::Unparsable {
                    path: path.to_string(),
                    reason,
                }),
                // `Playlist::load` raises `Io` for everything else — a dangling
                // symlink included. The catch-all keeps a new variant from
                // silently dropping a playlist out of the index without a word.
                Err(other) => warnings.push(IndexWarning::Unreadable {
                    path: path.to_string(),
                    message: message_of(&other),
                }),
            }
        }

        (Self::from_playlists(playlist_dir, playlists), warnings)
    }

    /// Index playlists that are already in hand, with no disk access at all.
    ///
    /// `dir` is recorded as the directory they came from; it is not read.
    #[must_use]
    pub fn from_playlists(dir: &Utf8Path, playlists: Vec<Playlist>) -> Self {
        let mut by_path: HashMap<RelPath, Vec<Ref>> = HashMap::new();
        for (playlist, entries) in playlists.iter().enumerate() {
            for (entry, line) in entries.entries().iter().enumerate() {
                // The one filter there is: a line with no `rel` is a URL, a
                // comment, a blank or a line MPDFM does not understand, and none
                // of those name a file.
                if let Some(rel) = line.rel() {
                    by_path
                        .entry(rel.clone())
                        .or_default()
                        .push(Ref { playlist, entry });
                }
            }
        }
        Self {
            dir: dir.to_owned(),
            playlists,
            by_path,
        }
    }

    /// The directory this index was built from.
    #[must_use]
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// Every playlist, in file-name order — the order `Ref::playlist` indexes.
    #[must_use]
    pub fn playlists(&self) -> &[Playlist] {
        &self.playlists
    }

    /// One playlist by index, or `None` for a reference from a stale index.
    #[must_use]
    pub fn playlist(&self, index: usize) -> Option<&Playlist> {
        self.playlists.get(index)
    }

    /// The line a reference names, or `None` for a reference from a stale index.
    #[must_use]
    pub fn entry(&self, reference: Ref) -> Option<&Entry> {
        self.playlists
            .get(reference.playlist)?
            .entries()
            .get(reference.entry)
    }

    /// How many playlists were indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.playlists.len()
    }

    /// Whether the directory held no playlist this index could read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.playlists.is_empty()
    }

    /// How many track lines there are in total — 231 in the real directory.
    /// Duplicates count once each, which is what makes it a line count.
    #[must_use]
    pub fn reference_count(&self) -> usize {
        self.by_path.values().map(Vec::len).sum()
    }

    /// Every distinct path referenced by any playlist, sorted.
    ///
    /// This is `doctor`'s starting point (task 29): the set of files the user's
    /// playlists claim exist.
    #[must_use]
    pub fn paths(&self) -> Vec<&RelPath> {
        let mut paths: Vec<&RelPath> = self.by_path.keys().collect();
        paths.sort_unstable();
        paths
    }

    /// Every reference that names a virtual track inside a CUE sheet, with the
    /// `trackNNNN` suffix the line spells it with.
    ///
    /// These are the references [`PlaylistIndex::broken`] cannot fully judge: it
    /// answers "is the sheet there?", and the sheet being there does not mean MPD
    /// can play the track inside it. Verifying that needs MPD's own database
    /// (task 13) and belongs to `doctor` (task 29); this is where it starts.
    ///
    /// Sorted by reference, so a report reads in playlist and line order.
    #[must_use]
    pub fn cue_refs(&self) -> Vec<(Ref, &RelPath, &str)> {
        let mut refs: Vec<(Ref, &RelPath, &str)> = self
            .by_path
            .iter()
            .flat_map(|(path, refs)| refs.iter().map(move |reference| (*reference, path)))
            .filter_map(|(reference, path)| {
                let cue = self.entry(reference)?.cue()?;
                Some((reference, path, cue))
            })
            .collect();
        refs.sort_unstable_by_key(|(reference, _, _)| *reference);
        refs
    }

    /// Every line naming exactly this file, in playlist-then-line order.
    ///
    /// Exact identity: no case folding, no Unicode normalization, no prefix
    /// matching (`paths.rs` says why). A path nothing references gives an empty
    /// slice.
    #[must_use]
    pub fn refs_to(&self, rel: &RelPath) -> &[Ref] {
        self.by_path.get(rel).map_or(NO_REFS, Vec::as_slice)
    }

    /// Every referenced file strictly inside the directory `dir`, with the lines
    /// naming it — what a directory move has to rewrite.
    ///
    /// The prefix is **component-wise**, so `hiphop/MF DOOM` matches
    /// `hiphop/MF DOOM/01.mp3` and not `hiphop/MF DOOM Instrumentals/01.mp3`.
    /// A file whose path *is* `dir` is not included: that is a file move, and
    /// [`PlaylistIndex::refs_to`] is the lookup for it.
    ///
    /// Sorted by path, because a `HashMap` is not, and a preview whose lines
    /// shuffled between runs would be unreviewable.
    #[must_use]
    pub fn refs_under_dir(&self, dir: &RelPath) -> Vec<(RelPath, Vec<Ref>)> {
        let mut under: Vec<(RelPath, Vec<Ref>)> = self
            .by_path
            .iter()
            .filter(|(path, _)| path.starts_with_dir(dir))
            .map(|(path, refs)| (path.clone(), refs.clone()))
            .collect();
        under.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        under
    }

    /// Every reference whose file is not in `library`, with the path it names.
    ///
    /// These are the references that were broken before MPDFM arrived, and MPDFM
    /// reports them and leaves them alone. Silently "cleaning up" a line the user
    /// wrote is how playlists get destroyed; `doctor` (task 29) offers to fix
    /// them, one at a time and only when asked.
    ///
    /// "Absent" here means absent from the filesystem, which is the only question
    /// a move has to answer. It is *not* the same as "MPD will not play it": the
    /// real library's one problem reference is a CUE virtual track whose sheet,
    /// track and audio file all exist (see `docs/tasks/07-playlist-index.md`), so
    /// this reports nothing for it. [`PlaylistIndex::cue_refs`] is where that
    /// second question starts.
    ///
    /// Two consequences of comparing against the [`Library`] model rather than
    /// the disk, both intended:
    ///
    /// - a CUE virtual track counts as resolved when its `.cue` sheet exists,
    ///   because the sheet is what the index keyed it under — and the sheet is
    ///   what a move has to carry;
    /// - a reference to a path the scan skipped — a symlinked track, a name that
    ///   is not UTF-8 — is reported here too. The scan warned about it already,
    ///   and a file MPDFM will not move is a file it cannot promise about.
    ///
    /// Sorted by reference, so the report reads in playlist and line order.
    #[must_use]
    pub fn broken(&self, library: &Library) -> Vec<(Ref, RelPath)> {
        let mut broken: Vec<(Ref, RelPath)> = self
            .by_path
            .iter()
            .filter(|(path, _)| library.get(path).is_none())
            .flat_map(|(path, refs)| refs.iter().map(|reference| (*reference, path.clone())))
            .collect();
        broken.sort_unstable();
        broken
    }

    /// Which playlists any of `paths` appears in, as indices in ascending order.
    ///
    /// Each path is tried both ways — as a file, and as a directory containing
    /// referenced files — because a caller planning a move holds a mix of the two
    /// and the wrong guess would leave a playlist out of the backup set. Task 11
    /// copies exactly these files before touching anything.
    #[must_use]
    pub fn playlists_touching(&self, paths: &[RelPath]) -> Vec<usize> {
        let mut touched = BTreeSet::new();
        for (path, refs) in &self.by_path {
            let hit = paths
                .iter()
                .any(|target| path == target || path.starts_with_dir(target));
            if hit {
                touched.extend(refs.iter().map(|reference| reference.playlist));
            }
        }
        touched.into_iter().collect()
    }

    /// Hand over the playlists so they can be rewritten.
    ///
    /// The index is consumed on purpose: once a line changes, `by_path` describes
    /// a file that no longer exists in that shape, and a lookup against it would
    /// rewrite the wrong line on the next pass. Reload afterwards — it is cheap
    /// (see the module docs).
    #[must_use]
    pub fn into_playlists(self) -> Vec<Playlist> {
        self.playlists
    }
}

/// The playlist file names in `dir`, sorted, with everything that is not a
/// playlist filtered out.
fn playlist_file_names(dir: &Utf8Path, warnings: &mut Vec<IndexWarning>) -> Vec<String> {
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(err) => {
            warnings.push(IndexWarning::Unreadable {
                path: dir.to_string(),
                message: err.to_string(),
            });
            return Vec::new();
        }
    };

    let mut names = Vec::new();
    for found in listing {
        let found = match found {
            Ok(found) => found,
            Err(err) => {
                warnings.push(IndexWarning::Unreadable {
                    path: dir.to_string(),
                    message: err.to_string(),
                });
                continue;
            }
        };

        // `file_type` here is an `lstat`, so a symlinked playlist is a symlink
        // and not a directory, and is followed below by `Playlist::load`.
        if found.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }

        let raw = found.file_name();
        match raw.to_str() {
            Some(name) if is_playlist_name(name) => names.push(name.to_owned()),
            Some(_) => {}
            None => {
                // Not nameable, so not loadable. Worth a warning only if it looks
                // like a playlist; the lossy rendering keeps enough of the
                // extension to tell.
                let lossy = raw.to_string_lossy().into_owned();
                if is_playlist_name(&lossy) {
                    warnings.push(IndexWarning::NotUtf8 {
                        lossy: dir.join(&lossy).to_string(),
                    });
                }
            }
        }
    }

    // Byte order, so `Ref::playlist` means the same thing on every run.
    names.sort_unstable();
    names
}

/// The operating system's half of a load failure, without repeating the path
/// that the warning already carries.
fn message_of(error: &Error) -> String {
    match error {
        Error::Io { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/home/me/.config/mpd/playlists";
    const MF_DOOM: &str = "hiphop/MF DOOM/01 Beef Rap.mp3";
    const INSTRUMENTALS: &str = "hiphop/MF DOOM Instrumentals/01 Beef Rap.mp3";
    const CUE: &str =
        "pop/Imagine Dragons - Mercury - Acts 1/Imagine Dragons - Mercury - Acts 1.flac.cue";

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).unwrap_or_else(|err| panic!("{s:?} is not a RelPath: {err}"))
    }

    /// A playlist built from the lines it would hold, parsed as the real one is.
    fn playlist(name: &str, lines: &[&str]) -> Playlist {
        let body = format!("{}\n", lines.join("\n"));
        Playlist::from_bytes(Utf8Path::new(DIR).join(name).as_path(), body.as_bytes())
            .unwrap_or_else(|err| panic!("{name} should parse: {err}"))
    }

    fn index(playlists: Vec<Playlist>) -> PlaylistIndex {
        PlaylistIndex::from_playlists(Utf8Path::new(DIR), playlists)
    }

    #[test]
    fn a_track_in_two_playlists_yields_a_reference_to_each() {
        let index = index(vec![
            playlist("Hip hop.m3u", &[MF_DOOM, INSTRUMENTALS]),
            playlist("MF Doom.m3u", &["# the good one", MF_DOOM]),
        ]);

        assert_eq!(
            index.refs_to(&rel(MF_DOOM)),
            [
                Ref {
                    playlist: 0,
                    entry: 0
                },
                Ref {
                    playlist: 1,
                    entry: 1
                },
            ]
        );
        assert_eq!(index.refs_to(&rel("nothing/references.mp3")), []);
    }

    #[test]
    fn a_duplicate_line_yields_two_references() {
        let index = index(vec![playlist("Duplicates.m3u", &[MF_DOOM, MF_DOOM])]);

        assert_eq!(
            index.refs_to(&rel(MF_DOOM)),
            [
                Ref {
                    playlist: 0,
                    entry: 0
                },
                Ref {
                    playlist: 0,
                    entry: 1
                },
            ]
        );
        assert_eq!(index.reference_count(), 2);
    }

    #[test]
    fn a_directory_prefix_is_component_wise() {
        let index = index(vec![playlist("Hip hop.m3u", &[MF_DOOM, INSTRUMENTALS])]);

        let under = index.refs_under_dir(&rel("hiphop/MF DOOM"));
        assert_eq!(
            under
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            [MF_DOOM],
            "the album with a longer name shares a string prefix, not a path prefix"
        );

        // The parent matches both, and a file is not inside itself.
        assert_eq!(index.refs_under_dir(&rel("hiphop")).len(), 2);
        assert!(index.refs_under_dir(&rel(MF_DOOM)).is_empty());
    }

    #[test]
    fn only_track_lines_are_indexed() {
        let index = index(vec![playlist(
            "Radios.m3u",
            &[
                "#EXTM3U",
                "# Liquid Drum & Bass",
                "#EXTINF:-1,Bassdrive",
                "http://ice.bassdrive.net/stream",
                "",
                "/absolute/path.mp3",
                "./relative.mp3",
                MF_DOOM,
            ],
        )]);

        assert_eq!(index.paths(), [&rel(MF_DOOM)]);
        assert_eq!(index.reference_count(), 1);
    }

    #[test]
    fn a_cue_virtual_track_indexes_under_its_sheet() {
        let index = index(vec![playlist(
            "Pop.m3u",
            &[&format!("{CUE}/track0017"), CUE],
        )]);

        // Both lines — the virtual track and the bare sheet — key on the file
        // that exists, so a move of the sheet finds them together.
        assert_eq!(index.refs_to(&rel(CUE)).len(), 2);
        assert_eq!(index.paths(), [&rel(CUE)]);
        assert_eq!(
            index
                .entry(Ref {
                    playlist: 0,
                    entry: 0
                })
                .and_then(Entry::cue),
            Some("track0017")
        );
    }

    #[test]
    fn cue_refs_names_the_virtual_tracks_and_not_the_plain_ones() {
        let index = index(vec![playlist(
            "Pop.m3u",
            &[MF_DOOM, &format!("{CUE}/track0017"), CUE],
        )]);

        let cue_refs = index.cue_refs();
        assert_eq!(cue_refs.len(), 1, "{cue_refs:?}");
        let (reference, path, track) = cue_refs[0];
        assert_eq!(
            reference,
            Ref {
                playlist: 0,
                entry: 1
            }
        );
        assert_eq!(
            path,
            &rel(CUE),
            "keyed under the sheet, like every reference"
        );
        assert_eq!(track, "track0017");
    }

    #[test]
    fn playlists_touching_takes_files_and_directories_at_once() {
        let index = index(vec![
            playlist("Hip hop.m3u", &[MF_DOOM]),
            playlist("Instrumentals.m3u", &[INSTRUMENTALS]),
            playlist("Radios.m3u", &["http://ice.bassdrive.net/stream"]),
        ]);

        assert_eq!(index.playlists_touching(&[rel(MF_DOOM)]), [0]);
        assert_eq!(index.playlists_touching(&[rel("hiphop")]), [0, 1]);
        assert_eq!(
            index.playlists_touching(&[rel(MF_DOOM), rel("hiphop/MF DOOM Instrumentals")]),
            [0, 1]
        );
        assert!(index.playlists_touching(&[rel("jazz")]).is_empty());
        assert!(index.playlists_touching(&[]).is_empty());
    }

    #[test]
    fn a_stale_reference_resolves_to_nothing_rather_than_panicking() {
        let index = index(vec![playlist("Hip hop.m3u", &[MF_DOOM])]);

        assert!(index.playlist(7).is_none());
        assert!(
            index
                .entry(Ref {
                    playlist: 0,
                    entry: 99
                })
                .is_none()
        );
        assert!(
            index
                .entry(Ref {
                    playlist: 7,
                    entry: 0
                })
                .is_none()
        );
    }

    #[test]
    fn an_empty_directory_indexes_to_nothing() {
        let index = index(Vec::new());

        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert_eq!(index.reference_count(), 0);
        assert_eq!(index.dir(), Utf8Path::new(DIR));
        assert!(index.refs_to(&rel(MF_DOOM)).is_empty());
    }

    #[test]
    fn a_missing_directory_warns_instead_of_failing() {
        let dir = Utf8Path::new("/nonexistent/mpdfm/playlists");
        let (index, warnings) = PlaylistIndex::load(dir);

        assert!(index.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].path(), dir.as_str());
        assert!(matches!(warnings[0], IndexWarning::Unreadable { .. }));
    }
}
