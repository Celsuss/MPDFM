//! Tag reading and writing: one model, three containers, nothing lost.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::tags::{self, Field, TagDelta};
//!
//! # fn main() -> Result<(), mpdfm_core::Error> {
//! let track = Utf8Path::new("/home/me/Music/hiphop/MF DOOM/01 Beef Rap.mp3");
//!
//! // What is in there now — exactly as the file spells it.
//! let (tags, info) = tags::read(track)?;
//! println!("{} — {} [{info}]", tags.artist, tags.title);
//! for (key, value) in &tags.extra {
//!     println!("  {key} = {value}");   // and MPDFM will not touch any of these
//! }
//!
//! // A change to one field. Every one of those extras survives it.
//! let delta = TagDelta::new().set(Field::Genre, "Hip Hop");
//! # let _ = delta;
//! # Ok(())
//! # }
//! ```
//!
//! | | |
//! |---|---|
//! | [`model`] | [`TagSet`], [`Field`], [`AudioInfo`] — the shape, with no `lofty` in it |
//! | [`read`] | bytes → [`TagSet`], container detected by content |
//! | [`write`] | a [`TagDelta`] → bytes, atomically, preserving everything else |
//! | [`bulk`] | one field across many files, with an honest `<multiple>` |
//!
//! # The property this module exists to have
//!
//! **A write touches the fields in its delta and nothing else.** Embedded
//! artwork, ReplayGain, MusicBrainz ids, lyrics, chapter frames, a release
//! group's `TXXX` — all of it survives a `--genre` edit byte for byte. A tag
//! editor that silently drops embedded art is worse than no tag editor, which is
//! why [`write`] goes through `lofty`'s [`SplitTag`][lofty::prelude::SplitTag]
//! round trip rather than rebuilding a tag from a [`TagSet`]: a `TagSet` is a
//! *view*, and writing a view back is how data gets lost.
//!
//! # `lofty` stops here
//!
//! Nothing outside this module names a `lofty` type. [`TagError`] carries the
//! path and a message rather than a `lofty` error, [`AudioInfo::format`] is our
//! own [`Format`][crate::library::Format], and the one place a container is
//! branched on is [`read::open`]. That is what lets the browser, the editor and
//! the template engine be written once.

pub mod bulk;
pub mod model;
pub mod read;
pub mod write;

pub use bulk::{BulkView, FieldValue, MULTIPLE, merge, title_from_filename};
pub use model::{
    AudioInfo, FIELDS, Field, NumberPair, SEPARATOR, TagSet, Values, parse_pair, render_pair,
};
pub use read::{TagLayout, read, read_many, read_tags, read_tags_with_layout};
pub use write::{Edit, Id3Version, TagBackup, TagDelta, WriteOpts, restore, write};

use camino::{Utf8Path, Utf8PathBuf};

/// A file `lofty` has parsed, kept in the shape a write needs it.
///
/// Boxed because an `MpegFile` and an `Mp4File` are very different sizes and this
/// enum is returned by value; `clippy::large_enum_variant` is right about it.
pub(crate) enum Parsed {
    /// An mp3, whose primary tag is ID3v2.
    Mpeg(Box<lofty::mpeg::MpegFile>),
    /// A FLAC, whose primary tag is its Vorbis comment block.
    Flac(Box<lofty::flac::FlacFile>),
    /// An m4a, whose primary tag is its `ilst` atom.
    Mp4(Box<lofty::mp4::Mp4File>),
}

/// Why a file's tags could not be read or written.
///
/// Every variant names the path. A bulk edit over an album reports one of these
/// per file that failed and goes on with the rest, so an error that does not say
/// which file is an error the user cannot act on.
#[derive(Debug, thiserror::Error)]
pub enum TagError {
    /// The file could not be opened or read.
    #[error("{path}: {source}")]
    Unreadable {
        /// The file.
        path: Utf8PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },

    /// The bytes are not a container MPDFM edits.
    ///
    /// `detected` is what the content *looks* like when `lofty` recognized it as
    /// something else — a `.mp3` that is really an Ogg Vorbis file — and `None`
    /// when it is not audio at all. Either way the extension is not consulted,
    /// so this is a statement about the bytes.
    #[error(
        "{path} is not an audio file MPDFM can edit{}",
        .detected.as_ref().map_or_else(String::new, |what| format!(" (it looks like {what})"))
    )]
    Unsupported {
        /// The file.
        path: Utf8PathBuf,
        /// What the bytes look like, when they look like anything.
        detected: Option<String>,
    },

    /// The container is right and the tag inside it is damaged — a truncated
    /// file, a frame whose length runs past the end.
    #[error("{path}: the tag is damaged ({reason})")]
    Corrupt {
        /// The file.
        path: Utf8PathBuf,
        /// What the parser said.
        reason: String,
    },

    /// The file is there and MPDFM may not write to it. Raised in preflight,
    /// before any temp file is created.
    #[error("{path}: write permission is missing")]
    ReadOnly {
        /// The file.
        path: Utf8PathBuf,
    },

    /// The container cannot be identified from the file's own bytes, so its tag
    /// cannot be rewritten.
    ///
    /// Reading one of these works — the extension is believed, and the tags are
    /// usually intact — so the editor can show the user what is in a damaged file.
    /// Writing is refused in preflight rather than attempted and failed, because
    /// what is wrong is the file and not the edit.
    #[error("{path}: {reason}, so its tag cannot be rewritten")]
    Damaged {
        /// The file.
        path: Utf8PathBuf,
        /// What could not be found in it.
        reason: String,
    },

    /// The edit itself does not make sense — `--track one`, a field a container
    /// cannot hold.
    #[error("{path}: {reason}")]
    BadEdit {
        /// The file.
        path: Utf8PathBuf,
        /// What is wrong with the edit.
        reason: String,
    },

    /// The new tag could not be encoded, or the temp file could not be written.
    /// The original is untouched.
    #[error("{path}: the tag could not be written ({reason})")]
    NotWritten {
        /// The file.
        path: Utf8PathBuf,
        /// What went wrong.
        reason: String,
    },

    /// A backup was asked for and could not be taken, so the write did not
    /// happen. A tag write with no way back is one MPDFM declines to make.
    #[error("{path}: no backup could be taken ({reason}), so nothing was written")]
    NoBackup {
        /// The file.
        path: Utf8PathBuf,
        /// Why not.
        reason: String,
    },

    /// [`write::Inject`] fired. Never raised in production.
    #[error("simulated failure while writing {path}, {at}")]
    Injected {
        /// The file.
        path: Utf8PathBuf,
        /// Where it stopped.
        at: &'static str,
    },
}

impl TagError {
    /// The file this is about.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        match self {
            Self::Unreadable { path, .. }
            | Self::Unsupported { path, .. }
            | Self::Corrupt { path, .. }
            | Self::Damaged { path, .. }
            | Self::ReadOnly { path }
            | Self::BadEdit { path, .. }
            | Self::NotWritten { path, .. }
            | Self::NoBackup { path, .. }
            | Self::Injected { path, .. } => path,
        }
    }

    /// [`TagError::Unreadable`] for `path`.
    pub(crate) fn unreadable(path: &Utf8Path, source: std::io::Error) -> Self {
        Self::Unreadable {
            path: path.to_owned(),
            source,
        }
    }

    /// [`TagError::Corrupt`] for `path`, from whatever `lofty` said.
    pub(crate) fn corrupt(path: &Utf8Path, source: impl std::fmt::Display) -> Self {
        Self::Corrupt {
            path: path.to_owned(),
            reason: source.to_string(),
        }
    }
}
