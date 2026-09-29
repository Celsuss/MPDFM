//! [`Playlist`] — an MPD `.m3u` read into a model that writes back
//! **byte-identically**, so that rewriting one line never disturbs the other 59.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::paths::RelPath;
//! use mpdfm_core::playlist::{Entry, Playlist};
//!
//! let mut playlist = Playlist::load(Utf8Path::new("/home/me/.config/mpd/playlists/Pop.m3u"))?;
//! assert_eq!(playlist.name(), "Pop");
//!
//! // Rewrite exactly the lines that name one moved file. Every other line —
//! // comments, blanks, radio URLs, the tracks that did not move — is written
//! // back byte-for-byte.
//! let from = RelPath::parse("pop/old/a.flac.cue")?;
//! let to = RelPath::parse("pop/new/a.flac.cue")?;
//! for entry in playlist.entries_mut() {
//!     if entry.rel() == Some(&from) {
//!         *entry = Entry::track(to.clone(), entry.cue().map(str::to_owned));
//!     }
//! }
//! playlist.write()?;
//! # Ok::<(), mpdfm_core::Error>(())
//! ```
//!
//! # Fidelity is the whole job
//!
//! A playlist is a file the *user* wrote. Their comments, their blank lines, the
//! radio URLs MPDFM knows nothing about, the BOM their editor put there — none of
//! it is MPDFM's to normalize, and a tool that "tidied" any of it while renaming
//! an album would have lost data the user cared about. So:
//!
//! - every [`Entry`] carries the exact bytes of its line, and the writer emits
//!   those bytes back. [`Entry::Unparsed`] exists precisely so that a line
//!   MPDFM does not understand — an absolute path, a `./` path, a Windows path —
//!   is preserved rather than dropped;
//! - the line ending, the presence of a trailing newline and the presence of a
//!   BOM are properties of the file, recorded on the way in and reproduced on the
//!   way out;
//! - nothing is sorted, deduplicated or trimmed. `Duplicates.m3u` in the fixture
//!   set lists one track twice on purpose, and it comes back out twice.
//!
//! The property test in `crates/core/tests/playlist.rs` reads all seventeen
//! committed fixture playlists and asserts `write(parse(bytes)) == bytes` for
//! each, which is the only form of this promise worth having.
//!
//! # Symlinks
//!
//! `Radios.m3u` on the real machine is a symlink into a dotfiles repository.
//! [`Playlist::load`] records the link as [`Playlist::path`] and its resolved
//! target as [`Playlist::real_path`], and the writer replaces the *target*, so
//! the link survives and the dotfiles repo sees the edit. [`Playlist::write`]
//! refuses outright to write to a `real_path` that is still a symlink — see
//! [`Playlist::from_bytes`].
//!
//! Containment (safety invariant 5) is the caller's to check, on
//! [`Playlist::real_path`], since only the caller knows the configured roots.
//! [`crate::paths::contains`] is that check.

mod index;
mod parse;
pub mod rewrite;
mod write;

pub use index::{IndexWarning, PlaylistIndex, Ref};
pub use parse::ParseError;

use camino::{Utf8Path, Utf8PathBuf};

use crate::paths::RelPath;
use crate::{Error, Result};

/// The extensions MPD's playlist directory holds, lowercase and without the dot.
///
/// `.m3u8` is the same format — it is `.m3u` that promises to be UTF-8, which
/// every file MPDFM writes is anyway. The *extension* is kept as found; only
/// [`playlist_name`] looks past it.
pub const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8"];

/// One line of a playlist.
///
/// Every variant renders back to the exact bytes it was parsed from —
/// [`Entry::line`] is that rendering — which is what makes the round-trip
/// property hold. The parsed-out fields (`duration`, `rel`, `cue`) are for
/// tasks 07 and 09 to match on; `raw` is what actually gets written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An empty line. A line of spaces or tabs is *not* this — it is
    /// [`Entry::Unparsed`], because its bytes have to come back.
    Blank,

    /// A `#` line that is not one of the `#EXT` directives below, e.g.
    /// `# Liquid Drum & Bass`. Holds the whole line, `#` included.
    Comment(String),

    /// Exactly `#EXTM3U`. Anything that only looks like it — trailing space,
    /// different case — is a [`Entry::Comment`], so it round-trips unchanged.
    ExtM3u,

    /// `#EXTINF:-1,SomaFM - Groove Salad`.
    ///
    /// Task 09 removes the `#EXTINF` that immediately precedes a deleted track,
    /// which is why the duration and title are parsed out at all.
    ExtInf {
        /// Seconds, or `-1` for a stream of unknown length.
        duration: i64,
        /// Everything after the first comma, verbatim.
        title: String,
        /// The whole line, which is what is written back.
        raw: String,
    },

    /// A line with a URL scheme — `http://`, `https://`, or any other. Held as
    /// the whole line; MPDFM never parses or rewrites one.
    Url(String),

    /// A track: a path relative to `music_directory`, optionally naming a
    /// virtual track inside a CUE sheet.
    ///
    /// For `…/album.flac.cue/track0017`, `rel` is the `.cue` file and `cue` is
    /// `Some("track0017")` — so moving the sheet moves the reference with it
    /// (task 09).
    Track {
        /// The file this line identifies: the audio file, or the `.cue` sheet
        /// when `cue` is set.
        rel: RelPath,
        /// The `trackNNNN` component of a CUE virtual track, if this is one.
        cue: Option<String>,
        /// The whole line, which is what is written back.
        raw: String,
    },

    /// A line MPDFM has no reading of: an absolute path, a `./` path, a
    /// backslash path, a line with a stray carriage return in it.
    ///
    /// It exists so such a line is preserved rather than dropped. Dropping a
    /// line the user wrote is a data-loss bug, and "I did not recognize it" is
    /// not a licence to delete it.
    Unparsed(String),
}

impl Entry {
    /// A track line built from its parts, with `raw` derived from them.
    ///
    /// This is how task 09 rewrites a line: build a new entry from the moved
    /// path and the CUE suffix, rather than editing `raw` and hoping the fields
    /// still agree with it.
    ///
    /// ```
    /// use mpdfm_core::paths::RelPath;
    /// use mpdfm_core::playlist::Entry;
    ///
    /// let rel = RelPath::parse("pop/new/a.flac.cue")?;
    /// let entry = Entry::track(rel, Some("track0017".to_owned()));
    /// assert_eq!(entry.line(), "pop/new/a.flac.cue/track0017");
    /// # Ok::<(), mpdfm_core::paths::PathError>(())
    /// ```
    #[must_use]
    pub fn track(rel: RelPath, cue: Option<String>) -> Self {
        let raw = match &cue {
            Some(cue) => format!("{rel}/{cue}"),
            None => rel.to_string(),
        };
        Self::Track { rel, cue, raw }
    }

    /// The exact bytes of this line, without its line ending.
    ///
    /// The writer emits nothing but these, so a change that made this disagree
    /// with the parsed input would break the round-trip property — which is what
    /// `round_trips_every_committed_playlist` is there to catch.
    #[must_use]
    pub fn line(&self) -> &str {
        match self {
            Self::Blank => "",
            Self::ExtM3u => EXTM3U,
            Self::Comment(text)
            | Self::Url(text)
            | Self::Unparsed(text)
            | Self::ExtInf { raw: text, .. }
            | Self::Track { raw: text, .. } => text,
        }
    }

    /// The file this line identifies, for a track line; `None` for everything
    /// else. A URL is not a path and never appears here.
    #[must_use]
    pub fn rel(&self) -> Option<&RelPath> {
        match self {
            Self::Track { rel, .. } => Some(rel),
            _ => None,
        }
    }

    /// The `trackNNNN` suffix of a CUE virtual track.
    #[must_use]
    pub fn cue(&self) -> Option<&str> {
        match self {
            Self::Track { cue, .. } => cue.as_deref(),
            _ => None,
        }
    }
}

/// `#EXTM3U`, the one directive matched exactly.
const EXTM3U: &str = "#EXTM3U";

/// Which bytes end a line in this file.
///
/// One value for the whole file, because that is what every real playlist has.
/// A file that mixes endings is read as [`LineEnding::Lf`], and the stray `\r`
/// stays in the line it belongs to (as an [`Entry::Unparsed`]) rather than being
/// silently dropped — see `parse.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\n`. What MPD writes, and what everything on this machine uses.
    #[default]
    Lf,
    /// `\r\n`. Appears in files that have been through a Windows editor.
    Crlf,
}

impl LineEnding {
    /// The bytes themselves.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// One playlist file, parsed so that it can be written back byte-identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    /// The `.m3u` as found, which may be a symlink.
    path: Utf8PathBuf,
    /// Symlink resolved — this is what the writer replaces.
    real_path: Utf8PathBuf,
    /// MPD's name for it: the file name without the `.m3u` suffix.
    name: String,
    entries: Vec<Entry>,
    line_ending: LineEnding,
    trailing_newline: bool,
    bom: bool,
}

impl Playlist {
    /// Read and parse the playlist at `path`, resolving it if it is a symlink.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be read or a symlink cannot be resolved —
    /// a dangling link is this, and task 07 turns it into a warning and carries
    /// on. [`Error::Playlist`] if the bytes are not UTF-8.
    pub fn load(path: &Utf8Path) -> Result<Self> {
        // Resolved before the read, so a dangling symlink is reported as the
        // link's own path rather than as a missing file somewhere else.
        let real_path = real_path_of(path)?;
        let bytes = std::fs::read(&real_path).map_err(|source| Error::Io {
            path: real_path.to_string(),
            source,
        })?;
        let parsed = parse::parse(path, &bytes)?;
        Ok(Self {
            name: playlist_name(file_name_of(path)).to_owned(),
            path: path.to_owned(),
            real_path,
            entries: parsed.entries,
            line_ending: parsed.line_ending,
            trailing_newline: parsed.trailing_newline,
            bom: parsed.bom,
        })
    }

    /// Parse bytes that are already in hand, with no disk access at all.
    ///
    /// `real_path` is set to `path` verbatim: **no symlink is resolved**. That
    /// makes this the right constructor for bytes from a backup or a test, and
    /// the wrong one for a playlist sitting on disk — [`Playlist::write`] refuses
    /// to write to a symlink, so a mistake here is an error rather than a
    /// replaced link.
    ///
    /// # Errors
    ///
    /// [`ParseError::NotUtf8`] if the bytes are not UTF-8. MPDFM never guesses an
    /// encoding (`docs/PLAN.md` safety invariant 8).
    pub fn from_bytes(path: &Utf8Path, bytes: &[u8]) -> std::result::Result<Self, ParseError> {
        let parsed = parse::parse(path, bytes)?;
        Ok(Self {
            name: playlist_name(file_name_of(path)).to_owned(),
            path: path.to_owned(),
            real_path: path.to_owned(),
            entries: parsed.entries,
            line_ending: parsed.line_ending,
            trailing_newline: parsed.trailing_newline,
            bom: parsed.bom,
        })
    }

    /// The playlist as found — the symlink, when it is one.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// The file the writer replaces: [`Playlist::path`] with symlinks resolved.
    #[must_use]
    pub fn real_path(&self) -> &Utf8Path {
        &self.real_path
    }

    /// MPD's name for this playlist — the file name without its `.m3u` suffix.
    ///
    /// Spaces and accents are kept exactly as the file name has them
    /// (`Coding flow`, `En kall Stockholms natt`): this is the string MPD's
    /// `load` command takes, so slugifying it would name a playlist that does
    /// not exist.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Every line, in file order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every line, mutably — how task 09 rewrites, and removes, lines.
    ///
    /// Build replacement track entries with [`Entry::track`] so that the parsed
    /// fields and the bytes that get written cannot drift apart.
    pub fn entries_mut(&mut self) -> &mut Vec<Entry> {
        &mut self.entries
    }

    /// Which bytes end each line of this file.
    #[must_use]
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Whether the last line is terminated. A file that ends without a newline
    /// keeps ending without one.
    #[must_use]
    pub fn trailing_newline(&self) -> bool {
        self.trailing_newline
    }

    /// Whether the file starts with a UTF-8 BOM, which is written back if so.
    #[must_use]
    pub fn has_bom(&self) -> bool {
        self.bom
    }

    /// The file's exact bytes, as [`Playlist::write`] would write them.
    ///
    /// For an unmodified playlist this is byte-identical to what was parsed.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        write::to_bytes(self)
    }

    /// Replace [`Playlist::real_path`] with [`Playlist::to_bytes`], atomically.
    ///
    /// Temp file in the same directory, `fsync`, `rename` — so a crash leaves
    /// either the old file or the new one, never a half-written playlist
    /// (`docs/PLAN.md` safety invariant 6). The original's permission bits are
    /// preserved.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the directory cannot be written to, the write or the
    /// rename fails, or `real_path` is a symlink — which means the playlist was
    /// built by [`Playlist::from_bytes`] rather than loaded, and writing it would
    /// replace the user's link with a regular file.
    pub fn write(&self) -> Result<()> {
        write::replace_file(&self.real_path, &self.to_bytes(), write::Stop::Never)
    }
}

/// MPD's playlist name for a file name: the name without its `.m3u`/`.m3u8`
/// suffix, or the whole name if it has neither.
///
/// The match is case-insensitive, because a `.M3U` from a scene release is still
/// a playlist. The name itself is never otherwise touched — see
/// [`Playlist::name`].
///
/// ```
/// use mpdfm_core::playlist::playlist_name;
///
/// assert_eq!(playlist_name("En kall Stockholms natt.m3u"), "En kall Stockholms natt");
/// assert_eq!(playlist_name("Mixed bag.m3u8"), "Mixed bag");
/// assert_eq!(playlist_name("notes.txt"), "notes.txt");
/// ```
#[must_use]
pub fn playlist_name(file_name: &str) -> &str {
    let Some((stem, ext)) = file_name.rsplit_once('.') else {
        return file_name;
    };
    if PLAYLIST_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
        stem
    } else {
        file_name
    }
}

/// Whether a file name is one MPD would load as a playlist.
///
/// Task 07 uses this to skip the `.jpg` and `.txt` that end up in a playlist
/// directory, rather than trying to parse them.
#[must_use]
pub fn is_playlist_name(file_name: &str) -> bool {
    file_name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && PLAYLIST_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// `path` with every symlink resolved, so the writer can replace the file the
/// link points at instead of the link.
///
/// The parent is canonicalized separately from the file name: canonicalizing the
/// whole path would fail for a playlist that does not exist yet, which a caller
/// creating one has every right to hand us.
fn real_path_of(path: &Utf8Path) -> Result<Utf8PathBuf> {
    let io = |source: std::io::Error| Error::Io {
        path: path.to_string(),
        source,
    };
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => {
            let resolved = std::fs::canonicalize(path).map_err(io)?;
            Utf8PathBuf::from_path_buf(resolved).map_err(|lossy| {
                io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("symlink target is not valid UTF-8: {}", lossy.display()),
                ))
            })
        }
        // Not a link, or not there at all: the path is already the real one. A
        // playlist directory that is itself a link was resolved by the config
        // layer (task 04) before this was ever called.
        _ => Ok(path.to_owned()),
    }
}

/// The last component of `path`, or the whole thing if it has no components —
/// which cannot happen for a file that was just read, and yields an empty name
/// rather than a panic if it ever does.
fn file_name_of(path: &Utf8Path) -> &str {
    path.file_name().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_playlist_name_keeps_its_spaces_and_accents() {
        assert_eq!(playlist_name("Coding flow.m3u"), "Coding flow");
        assert_eq!(
            playlist_name("En kall Stockholms natt.m3u"),
            "En kall Stockholms natt"
        );
        assert_eq!(playlist_name("Mixed bag.m3u8"), "Mixed bag");
        assert_eq!(playlist_name("SCENE.M3U"), "SCENE");
        // Not a playlist suffix, so nothing is stripped.
        assert_eq!(playlist_name("cover.jpg"), "cover.jpg");
        assert_eq!(playlist_name("no-extension"), "no-extension");
        // A dotfile is all suffix and no stem; it keeps its name.
        assert_eq!(playlist_name(".m3u"), "");
    }

    #[test]
    fn only_m3u_files_are_playlists() {
        assert!(is_playlist_name("Pop.m3u"));
        assert!(is_playlist_name("Mixed bag.m3u8"));
        assert!(is_playlist_name("SCENE.M3U"));
        assert!(!is_playlist_name("folder.jpg"));
        assert!(!is_playlist_name("README"));
        assert!(!is_playlist_name(".m3u"));
    }

    #[test]
    fn a_track_entry_renders_its_parts() {
        let rel = RelPath::parse("pop/new/a.flac.cue").expect("valid");
        let plain = Entry::track(rel.clone(), None);
        assert_eq!(plain.line(), "pop/new/a.flac.cue");
        assert_eq!(plain.cue(), None);

        let virtual_track = Entry::track(rel.clone(), Some("track0017".to_owned()));
        assert_eq!(virtual_track.line(), "pop/new/a.flac.cue/track0017");
        assert_eq!(virtual_track.rel(), Some(&rel));
        assert_eq!(virtual_track.cue(), Some("track0017"));
    }

    #[test]
    fn non_track_entries_have_no_path() {
        for entry in [
            Entry::Blank,
            Entry::ExtM3u,
            Entry::Comment("# hi".to_owned()),
            Entry::Url("http://ice.bassdrive.net/stream".to_owned()),
            Entry::Unparsed("/absolute/a.mp3".to_owned()),
        ] {
            assert_eq!(entry.rel(), None, "{entry:?} should have no path");
            assert_eq!(entry.cue(), None, "{entry:?} should have no cue suffix");
        }
    }

    #[test]
    fn line_endings_are_the_bytes_they_name() {
        assert_eq!(LineEnding::default(), LineEnding::Lf);
        assert_eq!(LineEnding::Lf.as_str(), "\n");
        assert_eq!(LineEnding::Crlf.as_str(), "\r\n");
    }
}
