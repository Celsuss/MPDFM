//! Writing a [`TagDelta`] into a file without losing anything that was already
//! in it.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::tags::{self, Field, TagDelta, WriteOpts};
//!
//! # fn main() -> Result<(), mpdfm_core::tags::TagError> {
//! let track = Utf8Path::new("/home/me/Music/hiphop/MF DOOM/01 Beef Rap.mp3");
//! let delta = TagDelta::new()
//!     .set(Field::Genre, "Hip Hop")
//!     .clear(Field::Comment);
//!
//! let opts = WriteOpts::new().backing_up_to("/tmp/backup/01 Beef Rap.mp3");
//! let backup = tags::write(track, &delta, &opts)?;
//!
//! // And byte for byte back again.
//! tags::restore(&backup)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Only the fields in the delta
//!
//! This is the property the module exists to have, and it is worth saying what it
//! costs to get. The naive implementation — read a [`TagSet`], change a field,
//! write the `TagSet` back — loses embedded artwork, ReplayGain, MusicBrainz ids,
//! lyrics, chapters and every `TXXX` a release group left behind, because a
//! `TagSet` is a *view* and writing a view back deletes whatever the view does
//! not show. The real library has 126 files with `TXXX:REPLAYGAIN_TRACK_GAIN`
//! alone.
//!
//! So a write never constructs a tag. It takes the tag that is in the file and
//! makes four changes to it:
//!
//! ```text
//! 1  remove the native keys of each edited field      TCON, or GENRE, or ©gen
//! 2  split  →  (remainder, generic tag)               lofty's lossless round trip
//! 3  insert the new values into the generic tag       only the edited fields
//! 4  merge  →  the tag that goes back in the file     remainder re-added untouched
//! ```
//!
//! Step 1 is what stops a frame the split could not interpret — a `TDRC` holding
//! something that is not a timestamp — from surviving alongside the new value and
//! leaving the file with two years in it. Steps 2 and 4 are
//! [`SplitTag`]/[`MergeTag`], which is `lofty`'s documented mechanism for exactly
//! this and which carries everything it cannot represent through in the
//! remainder.
//!
//! What is *not* promised is the byte layout: `lofty` re-encodes the whole tag on
//! any write, so a frame's text encoding, its flags and the tag's padding are its
//! to choose. Every item, every value and every picture survives; which of
//! Latin-1, UTF-16 and UTF-8 a frame is stored in afterwards is not something
//! MPDFM controls or needs to.
//!
//! # Atomic, and backed up before anything is written
//!
//! The order is fixed, and it is the order that makes a power cut harmless:
//!
//! ```text
//! 1  preflight: the file exists, is the right kind, and is writable
//! 2  copy the original into the backup directory, fsync it
//! 3  copy the original to a temp name in the same directory
//! 4  rewrite the temp file's tag, fsync it, give it the original's mode
//! 5  rename the temp file over the original
//! ```
//!
//! Nothing before step 5 is visible, and step 5 is one `rename`. A failure at any
//! step leaves the original exactly as it was and removes the temp file. Step 2 is
//! before step 3 on purpose: a tag write MPDFM cannot reverse is one it declines
//! to make, so a backup that cannot be taken is [`TagError::NoBackup`] and
//! nothing is written.
//!
//! **The backup is the whole original file.** The task offered the alternative of
//! storing the serialized original tag blob plus a hash, and it was not taken:
//! restoring a blob cannot promise the file comes back byte for byte, because the
//! padding and the frame order around it are not in the blob — and
//! `docs/tasks/19-cli-tags.md` asks for exactly that promise, for every file in a
//! bulk edit. The copy costs one file's worth of I/O, which a tag write was going
//! to spend anyway in step 3; retention prunes it with the rest of the
//! transaction's backups (task 11).
//!
//! # Three frames `lofty` will not write back
//!
//! Writing a copy of all 2 808 real audio files found 25 that lose exactly one
//! frame MPDFM does not model, and it is worth naming them because the cause is
//! the frame rather than the edit:
//!
//! | | | |
//! |---|---|---|
//! | 12 | `TDRC=2020-25-12` | there is no month 25, and `lofty` will not re-emit a timestamp that does not verify |
//! | 12 | `WXXX` with an empty description | a user-URL frame with nothing to identify it does not survive the round trip |
//! | 1 | `TDRL` on an ID3v2.3 file | a v2.4-only frame, discarded by the v2.3 writer as the spec requires |
//!
//! Each is already malformed or already in the wrong container version, and
//! nothing MPDFM could put in its place would be the user's data. They are
//! reported here rather than repaired, and `doctor` (task 29) is where a library
//! holding them should be told about it.
//!
//! What *is* repaired is the one case where refusing would be worse: see
//! [`repair_languages`].
//!
//! # mtime advances
//!
//! Deliberately. MPD notices a changed file by its mtime, so a write that
//! preserved the old one would leave MPD showing the tags the user just fixed.
//! `commit` also asks for a rescan, which makes it immediate rather than
//! eventual; the mtime is what makes it correct even when MPD is not running.

use camino::{Utf8Path, Utf8PathBuf};
use lofty::TextEncoding;
use lofty::config::WriteOptions;
use lofty::file::AudioFile as _;
use lofty::id3::v2::{
    Frame, FrameId, Id3v2Tag, Id3v2Version, TextInformationFrame, TimestampFrame,
};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::prelude::{Accessor as _, TagExt as _};
use lofty::tag::TagType;
use lofty::tag::items::Timestamp;

use super::model::{Field, Values, parse_pair, render_pair};
use super::{Parsed, TagError, read};

/// What to do to one field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edit {
    /// Replace the field's value with these. An empty [`Values`] is **not** how
    /// a field is removed — see [`Edit::Clear`] — because "set this to nothing"
    /// and "take this out of the file" are different requests and a bulk editor
    /// that conflated them would clear a field the user merely did not type in.
    Set(Values),
    /// Remove the field from the file: the frame goes away rather than being
    /// written empty.
    Clear,
}

impl Edit {
    /// The values this edit writes, or `None` for a clear.
    #[must_use]
    pub fn values(&self) -> Option<&Values> {
        match self {
            Self::Set(values) => Some(values),
            Self::Clear => None,
        }
    }

    /// How the preview renders this edit.
    #[must_use]
    pub fn rendered(&self) -> String {
        match self {
            Self::Set(values) => format!("{:?}", values.joined()),
            Self::Clear => "<cleared>".to_owned(),
        }
    }
}

impl std::fmt::Display for Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered())
    }
}

/// The fields one write changes, and nothing about the ones it does not.
///
/// Ordered by [`Field`] so that two deltas asking for the same thing compare
/// equal — which matters because a delta travels inside an
/// [`Operation`][crate::ops::Operation] and commit re-validates the plan by
/// comparing it against a fresh one.
///
/// ```
/// use mpdfm_core::tags::{Field, TagDelta};
///
/// let delta = TagDelta::new()
///     .set(Field::Genre, "Hip Hop")
///     .set(Field::Artist, "Madvillain; MF DOOM")   // two values
///     .clear(Field::Comment);
///
/// assert_eq!(delta.len(), 3);
/// assert!(delta.get(Field::Album).is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TagDelta {
    /// Sorted by field, one entry per field.
    edits: Vec<(Field, Edit)>,
}

impl TagDelta {
    /// A delta that changes nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a field, splitting the value on [`SEPARATOR`][super::SEPARATOR] the
    /// way a user typing into one box means it.
    #[must_use]
    pub fn set(self, field: Field, value: &str) -> Self {
        self.with(field, Edit::Set(Values::typed(value)))
    }

    /// Set a field to values that are already separate — from a bulk action
    /// rather than from a line of text.
    #[must_use]
    pub fn set_values(self, field: Field, values: Values) -> Self {
        self.with(field, Edit::Set(values))
    }

    /// Remove a field from the file.
    #[must_use]
    pub fn clear(self, field: Field) -> Self {
        self.with(field, Edit::Clear)
    }

    /// Add or replace one field's edit, keeping the list sorted by field.
    #[must_use]
    pub fn with(mut self, field: Field, edit: Edit) -> Self {
        match self.edits.binary_search_by_key(&field, |(key, _)| *key) {
            Ok(at) => self.edits[at] = (field, edit),
            Err(at) => self.edits.insert(at, (field, edit)),
        }
        self
    }

    /// Every edit, in field order.
    #[must_use]
    pub fn edits(&self) -> &[(Field, Edit)] {
        &self.edits
    }

    /// What this delta does to one field, if anything.
    #[must_use]
    pub fn get(&self, field: Field) -> Option<&Edit> {
        self.edits
            .binary_search_by_key(&field, |(key, _)| *key)
            .ok()
            .map(|at| &self.edits[at].1)
    }

    /// How many fields it changes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    /// Whether it changes nothing, in which case there is nothing to write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// `genre="Hip Hop", comment=<cleared>` — the preview's rendering.
    #[must_use]
    pub fn rendered(&self) -> String {
        self.edits
            .iter()
            .map(|(field, edit)| format!("{field}={}", edit.rendered()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for TagDelta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered())
    }
}

// ---------------------------------------------------------------------------

/// Which ID3v2 revision a write leaves an mp3 in.
///
/// `keep` is the default because the choice is not MPDFM's to make: a library
/// that plays through an old receiver or a `libid3tag` build wants v2.3, and
/// downgrading a v2.4 file to v2.3 **discards the v2.4-only frames** (`TSOP`,
/// `TSOA`, `TSOT`, `TMOO`, `TDRL` and the rest), which is a loss nobody asked
/// for. Keeping what the file already has makes a tag edit a tag edit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Id3Version {
    /// Leave the file in the revision it is already in. The default.
    #[default]
    Keep,
    /// Always write ID3v2.3, discarding frames it cannot hold.
    V23,
    /// Always write ID3v2.4.
    V24,
}

impl Id3Version {
    /// The name `config.toml` spells it with.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::V23 => "v23",
            Self::V24 => "v24",
        }
    }

    /// Read the setting back, accepting the spellings a person would write.
    ///
    /// ```
    /// use mpdfm_core::tags::Id3Version;
    ///
    /// assert_eq!(Id3Version::parse("v2.3"), Some(Id3Version::V23));
    /// assert_eq!(Id3Version::parse("KEEP"), Some(Id3Version::Keep));
    /// assert_eq!(Id3Version::parse("v1"), None);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "keep" => Some(Self::Keep),
            "v23" | "v2.3" | "2.3" | "id3v2.3" => Some(Self::V23),
            "v24" | "v2.4" | "2.4" | "id3v2.4" => Some(Self::V24),
            _ => None,
        }
    }

    /// Whether to write v2.3, given what the file is in now.
    ///
    /// ID3v2.2 cannot be written by `lofty` at all, so a v2.2 file is kept as
    /// v2.4 under `keep` rather than silently becoming something else: the
    /// frames have already been upgraded in memory by the time anyone looks.
    fn use_v23(self, original: Id3v2Version) -> bool {
        match self {
            Self::Keep => original == Id3v2Version::V3,
            Self::V23 => true,
            Self::V24 => false,
        }
    }
}

/// Everything a write is allowed to do beyond the delta itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteOpts {
    /// Which ID3v2 revision an mp3 is left in.
    pub id3_version: Id3Version,

    /// Where to copy the original before touching it. `None` writes with **no
    /// way back**, which only a caller that has its own copy should ask for;
    /// every path through `commit` supplies one.
    pub backup: Option<Utf8PathBuf>,

    /// Failure injection, for the two failures a test cannot cause from
    /// outside. Production passes [`Inject::Nothing`].
    pub inject: Inject,
}

impl WriteOpts {
    /// The cautious defaults: keep the ID3 version, take no backup, inject
    /// nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same, copying the original to `backup` first.
    #[must_use]
    pub fn backing_up_to(mut self, backup: impl Into<Utf8PathBuf>) -> Self {
        self.backup = Some(backup.into());
        self
    }

    /// The same, with an ID3 version policy — [`Config::id3_version`][crate::config::Config::id3_version].
    #[must_use]
    pub fn with_id3_version(mut self, version: Id3Version) -> Self {
        self.id3_version = version;
        self
    }
}

/// A failure only a test can cause, at each boundary of the sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Inject {
    /// Behave normally.
    #[default]
    Nothing,

    /// Fail once the backup is taken, before the temp file is made. The
    /// original is untouched and a backup exists with nothing to restore.
    AfterBackup,

    /// Fail with the temp file written and durable, before the `rename` that
    /// publishes it. This is the power-cut case: the original must still be the
    /// original, and no temp file may be left behind.
    BeforeRename,
}

/// Where a file's original bytes went, so the write can be undone.
///
/// The whole original file, copied — see the module documentation for why that
/// rather than a serialized tag blob.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TagBackup {
    /// The file that was written.
    pub target: Utf8PathBuf,
    /// The copy of it as it was.
    pub copy: Utf8PathBuf,
    /// How big it was, as a cheap check that the copy is the copy.
    pub size: u64,
}

// ---------------------------------------------------------------------------

/// Write `delta` into `abs`, leaving everything else in the file alone.
///
/// # Errors
///
/// [`TagError::Unreadable`] or [`TagError::Corrupt`] for a file that cannot be
/// parsed, [`TagError::ReadOnly`] for one MPDFM may not write — raised in
/// preflight, before any temp file exists — [`TagError::BadEdit`] for a value a
/// field cannot hold, [`TagError::NoBackup`] when a backup was asked for and
/// could not be taken, and [`TagError::NotWritten`] when the new tag could not
/// be encoded or the temp file could not be published. In every case the
/// original is exactly as it was.
pub fn write(abs: &Utf8Path, delta: &TagDelta, opts: &WriteOpts) -> Result<TagBackup, TagError> {
    // Step 1 — everything that can be refused without writing.
    let meta = preflight(abs, delta)?;
    // Parsed after the cheap checks, so a file MPDFM cannot edit at all costs one
    // 36-byte read rather than a whole tag parse.
    let (parsed, _, _) = read::open(abs, read::parse_options(false))?;

    // Step 2 — the backup, before the original is touched in any way.
    let backup = take_backup(abs, meta.len(), opts)?;
    if opts.inject == Inject::AfterBackup {
        return Err(TagError::Injected {
            path: abs.to_owned(),
            at: "after the backup was taken",
        });
    }

    // Steps 3 to 5 — the temp file, and the one rename that publishes it.
    let temp = temp_path(abs);
    let result = rewrite(abs, &temp, parsed, delta, opts, meta.permissions());
    if result.is_err() {
        // A failed write leaves nothing behind. An injected crash leaves nothing
        // behind either: the point of the case is that the *original* survives,
        // and a stray temp file would only confuse the next run.
        std::fs::remove_file(&temp).ok();
        result?;
    }

    Ok(backup)
}

/// Put back what a [`TagBackup`] holds.
///
/// Atomically, through a temp file beside the target, so an interrupted restore
/// leaves either the written file or the original and never half of either.
///
/// This is the standalone route, for a caller that wrote a tag on its own.
/// A tag write inside a transaction is reversed through its journal receipt
/// instead ([`exec_fs::revert`][crate::ops::exec_fs::revert]), which additionally
/// checks that the file is still the one the receipt describes before putting
/// anything back.
///
/// # Errors
///
/// [`TagError::Unreadable`] when the copy is not there or cannot be read, and
/// [`TagError::NotWritten`] when the target cannot be replaced.
pub fn restore(backup: &TagBackup) -> Result<(), TagError> {
    let meta = std::fs::metadata(&backup.copy)
        .map_err(|source| TagError::unreadable(&backup.copy, source))?;
    if meta.len() != backup.size {
        return Err(TagError::Corrupt {
            path: backup.copy.clone(),
            reason: format!(
                "the backup is {} bytes and should be {}",
                meta.len(),
                backup.size
            ),
        });
    }

    let temp = temp_path(&backup.target);
    let publish = || -> std::io::Result<()> {
        std::fs::copy(&backup.copy, &temp)?;
        fsync(&temp)?;
        std::fs::rename(&temp, &backup.target)?;
        fsync_dir(&backup.target);
        Ok(())
    };
    publish().map_err(|source| {
        std::fs::remove_file(&temp).ok();
        TagError::NotWritten {
            path: backup.target.clone(),
            reason: source.to_string(),
        }
    })
}

// ---------------------------------------------------------------------------

/// Everything a write can be refused for without writing anything.
///
/// Separate from [`write`] because the *preview* has to ask the same question
/// (`docs/tasks/19-cli-tags.md`: "setting a field on a read-only file fails
/// preflight, and no other file in the batch is modified"), and it has to ask it
/// about every file in a bulk edit. So it is kept cheap: a `stat`, the mode bits,
/// a 36-byte container sniff, and the delta's own arithmetic — no tag parse.
///
/// Returns the file's metadata, which [`write`] goes on to use for the backup's
/// size and the temp file's mode.
///
/// # Errors
///
/// [`TagError::Unreadable`] for a file that is not there, [`TagError::Unsupported`]
/// for one that is not a container MPDFM edits, [`TagError::ReadOnly`] for one it
/// may not write, and [`TagError::BadEdit`] for a value a field cannot hold.
pub fn preflight(abs: &Utf8Path, delta: &TagDelta) -> Result<std::fs::Metadata, TagError> {
    let meta = std::fs::metadata(abs).map_err(|source| TagError::unreadable(abs, source))?;
    if !meta.is_file() {
        return Err(TagError::Unsupported {
            path: abs.to_owned(),
            detected: Some("not a regular file".to_owned()),
        });
    }
    if read_only(&meta) {
        return Err(TagError::ReadOnly {
            path: abs.to_owned(),
        });
    }
    // A container identified only by its name is one `lofty` cannot write into,
    // because the writer is handed an open file and has no name to fall back on.
    // Refusing here rather than letting the write fail is the difference between
    // "this file is damaged" and "no format could be determined", and it happens
    // before a backup is taken.
    if read::detect(abs)?.1 == read::Detected::Extension {
        return Err(TagError::Damaged {
            path: abs.to_owned(),
            reason: format!(
                "no container header or audio frame was found in its first {} KiB",
                read::MAX_JUNK_BYTES / 1024
            ),
        });
    }
    check_edits(abs, delta)?;
    Ok(meta)
}

/// Whether the delta asks for anything this file cannot hold.
///
/// Checked before the backup, so a `--track one` costs no I/O and names the
/// file. A value that is merely odd — a year of `MMIV`, a genre nobody has heard
/// of — is not an error: MPDFM writes what it is told, and the preview is where
/// the user sees it.
fn check_edits(abs: &Utf8Path, delta: &TagDelta) -> Result<(), TagError> {
    for (field, edit) in delta.edits() {
        let Edit::Set(values) = edit else { continue };
        if values.is_empty() {
            return Err(TagError::BadEdit {
                path: abs.to_owned(),
                reason: format!("{field} was set to nothing; use a clear to remove it"),
            });
        }
        if *field == Field::Year {
            if values.is_multi() {
                return Err(TagError::BadEdit {
                    path: abs.to_owned(),
                    reason: "a year takes one date, not several".to_owned(),
                });
            }
            let value = values.first().unwrap_or_default();
            // A date rather than any string, because `TDRC` is a timestamp frame
            // and `lofty`'s ID3v2.3 writer discards a `TDRC` that is not one —
            // so a year MPDFM accepted and could not store would be a silently
            // dropped edit. Reading is the other way round and deliberately so:
            // whatever is in the file is shown (`TagSet::date`), because the user
            // cannot fix what they cannot see.
            if timestamp(value).is_none() {
                return Err(TagError::BadEdit {
                    path: abs.to_owned(),
                    reason: format!("year must be a date like 2004 or 2019-03-15, not {value:?}"),
                });
            }
        }
        if matches!(field, Field::Track | Field::Disc) {
            if values.is_multi() {
                return Err(TagError::BadEdit {
                    path: abs.to_owned(),
                    reason: format!("{field} takes one number, not {}", values.len()),
                });
            }
            let value = values.first().unwrap_or_default();
            if parse_pair(value).is_none() {
                return Err(TagError::BadEdit {
                    path: abs.to_owned(),
                    reason: format!("{field} must be a number or `number/total`, not {value:?}"),
                });
            }
        }
    }
    Ok(())
}

/// Copy the original aside, or say why the write is not happening.
fn take_backup(abs: &Utf8Path, size: u64, opts: &WriteOpts) -> Result<TagBackup, TagError> {
    let Some(copy) = &opts.backup else {
        return Ok(TagBackup {
            target: abs.to_owned(),
            // No copy. `restore` refuses this, which is the honest answer for a
            // write the caller asked to make unrecoverable.
            copy: Utf8PathBuf::new(),
            size,
        });
    };

    let no_backup = |reason: String| TagError::NoBackup {
        path: abs.to_owned(),
        reason,
    };
    if let Some(parent) = copy.parent() {
        std::fs::create_dir_all(parent).map_err(|err| no_backup(format!("{parent}: {err}")))?;
    }
    std::fs::copy(abs, copy).map_err(|err| no_backup(format!("{copy}: {err}")))?;
    fsync(copy).map_err(|err| no_backup(format!("{copy}: {err}")))?;

    Ok(TagBackup {
        target: abs.to_owned(),
        copy: copy.clone(),
        size,
    })
}

/// Steps 3 to 5: the copy, the new tag, and the rename.
fn rewrite(
    abs: &Utf8Path,
    temp: &Utf8Path,
    parsed: Parsed,
    delta: &TagDelta,
    opts: &WriteOpts,
    permissions: std::fs::Permissions,
) -> Result<(), TagError> {
    let io = |reason: String| TagError::NotWritten {
        path: abs.to_owned(),
        reason,
    };

    // The audio is copied rather than re-encoded: `lofty` rewrites the tag
    // region of a file it is handed, so the temp file has to *be* the file.
    std::fs::copy(abs, temp).map_err(|err| io(format!("{temp}: {err}")))?;
    std::fs::set_permissions(temp.as_std_path(), permissions)
        .map_err(|err| io(format!("{temp}: {err}")))?;

    let v23 = use_v23(&parsed, opts);
    let edited = apply(parsed, delta, v23);
    save(&edited, temp, write_options(v23)).map_err(io)?;
    verify(abs, temp, delta, v23)?;
    fsync(temp).map_err(|err| io(format!("{temp}: {err}")))?;

    if opts.inject == Inject::BeforeRename {
        return Err(TagError::Injected {
            path: abs.to_owned(),
            at: "after the new tag was durable, before the rename",
        });
    }

    std::fs::rename(temp.as_std_path(), abs.as_std_path())
        .map_err(|err| io(format!("{abs}: {err}")))?;
    fsync_dir(abs);
    Ok(())
}

/// Read the rewritten file back and check it says what the delta asked for.
///
/// On the **temp file**, before the rename, so a write that did not take effect
/// leaves the original exactly as it was — which is the contract every other
/// failure in this module keeps.
///
/// It is not paranoia about `lofty`. Two of the real library's mp3s carry **two
/// stacked ID3v2 tags**, an ID3v2.3 one followed immediately by an ID3v2.4 one.
/// `lofty` reads them as one merged tag and writes the merged result back over the
/// *first*, so the second survives with the old value in it and still wins on the
/// next read: the write reports success and the file does not change. "I pressed
/// save and nothing happened" is the worst thing a tag editor can do, so it is
/// checked rather than assumed, for every field of every write. The cost is one
/// tag read — about a millisecond.
///
/// # Errors
///
/// [`TagError::NotWritten`] naming the field, what was asked for and what the
/// file says instead.
fn verify(abs: &Utf8Path, temp: &Utf8Path, delta: &TagDelta, v23: bool) -> Result<(), TagError> {
    let written = read::read_tags(temp).map_err(|err| TagError::NotWritten {
        path: abs.to_owned(),
        reason: format!("the rewritten file could not be read back: {err}"),
    })?;

    for (field, edit) in delta.edits() {
        let got = written.get(*field);
        let wrong = |wanted: String| {
            Err(TagError::NotWritten {
                path: abs.to_owned(),
                reason: format!(
                    "{field} still reads {:?} instead of {wanted:?} — the file may hold \
                     two stacked tags",
                    got.joined()
                ),
            })
        };
        match edit {
            Edit::Clear if !got.is_empty() => return wrong(String::new()),
            Edit::Set(values) => {
                let wanted = expected(*field, values, v23);
                if got.joined() != wanted {
                    return wrong(wanted);
                }
            }
            Edit::Clear => {}
        }
    }
    Ok(())
}

/// What a requested value will read back as once it has been written.
///
/// Canonicalized the way the container stores it, so that a user who typed
/// `--track 05/12` is not told the write failed because the file now says `5/12`.
/// [`verify`] compares against this, and [`bulk`][super::bulk] uses it to tell an
/// edit that changes something from one that asks for what is already there.
pub(super) fn canonical(field: Field, values: &Values) -> String {
    expected(field, values, false)
}

/// [`canonical`], told whether the frame it lands in can hold several values.
fn expected(field: Field, values: &Values, v23: bool) -> String {
    match field {
        Field::Track | Field::Disc => values
            .first()
            .and_then(parse_pair)
            .map(render_pair)
            .unwrap_or_default(),
        Field::Year => values
            .first()
            .and_then(timestamp)
            .map(|stamp| stamp.to_string())
            .unwrap_or_default(),
        // A v2.3 frame holds one string, so several values arrive joined — which
        // is what `joined` produces for the read-back too.
        _ => {
            let _ = v23;
            values.joined()
        }
    }
}

/// Whether this file is to be written as ID3v2.3, given the policy and what it
/// is in now.
fn use_v23(parsed: &Parsed, opts: &WriteOpts) -> bool {
    let original = match parsed {
        Parsed::Mpeg(file) => file
            .id3v2()
            .map_or(Id3v2Version::V4, Id3v2Tag::original_version),
        Parsed::Flac(_) | Parsed::Mp4(_) => Id3v2Version::V4,
    };
    opts.id3_version.use_v23(original)
}

/// `lofty`'s write options: the ID3 version, and nothing else.
///
/// `remove_others` stays off, so an mp3 that also carries an ID3v1 tag or an APE
/// tag keeps both — a tag editor that quietly stripped the ID3v1 tag MPD might be
/// reading would be changing what the user hears.
fn write_options(v23: bool) -> WriteOptions {
    WriteOptions::new()
        .use_id3v23(v23)
        // `lofty` re-probes the file on the way in, so the junk budget has to be
        // the same one the read used or a padded file would read fine and refuse
        // to be written. See `read::MAX_JUNK_BYTES`.
        .parse_options(read::parse_options(false))
}

/// Edit the file's own tag in place, in its own vocabulary.
///
/// # Why not `lofty`'s generic tag
///
/// `lofty` offers [`SplitTag`]/[`MergeTag`] as a lossless round trip, and for
/// *values* it is one. Measured against 85 files copied out of the real library,
/// it is not lossless about the **frames MPDFM was not asked to change**, which is
/// the property this module exists to have. Changing one `genre` through the
/// generic tag:
///
/// | | |
/// |---|---|
/// | `TCMP=PMEDIA` | **gone** — it maps to a compilation *flag*, and a value that is not a flag is discarded on the way back |
/// | `TXXX:ITUNESADVISORY=PMEDIA` | **gone**, the same way |
/// | `TRCK=07` | became `TRCK=7` — the split parses the number and the merge reprints it |
/// | `USLT` | re-encoded from Latin-1 to UTF-8 |
/// | m4a `disk` | re-encoded from the file's own byte layout into `lofty`'s |
/// | FLAC `title=` | re-cased to `TITLE=`, and every other key with it |
/// | FLAC `encoder=` | **gone** — the merge turns the first `EncoderSoftware` item into the vendor string |
///
/// None of that is a bug in `lofty`. It is a consequence of normalizing a tag
/// through a format-independent model and back, and it is exactly what a tag
/// editor must not do to the other nine fields. So each container is edited in
/// its own terms: remove the field's native keys, put the new value back the way
/// that container spells it, and touch nothing else in the tag.
///
/// Three functions rather than one because that is three vocabularies; what they
/// share — which keys belong to which field — is [`read::native_keys`], so the
/// reader and the writer cannot disagree about what a field *is*.
fn apply(parsed: Parsed, delta: &TagDelta, v23: bool) -> Parsed {
    match parsed {
        Parsed::Mpeg(mut file) => {
            let mut native = file.id3v2().cloned().unwrap_or_default();
            id3v2(&mut native, delta, v23);
            let _ = file.set_id3v2(native);
            Parsed::Mpeg(file)
        }
        Parsed::Flac(mut file) => {
            let mut native = file.vorbis_comments().cloned().unwrap_or_default();
            vorbis(&mut native, delta);
            let _ = file.set_vorbis_comments(native);
            Parsed::Flac(file)
        }
        Parsed::Mp4(mut file) => {
            let mut native = file.ilst().cloned().unwrap_or_default();
            ilst(&mut native, delta);
            let _ = file.set_ilst(native);
            Parsed::Mp4(file)
        }
    }
}

/// Edit an mp3's ID3v2 frames.
///
/// Three of the ten fields are not plain text frames and are handled by `lofty`'s
/// own [`Accessor`] or by the frame type the spec asks for, rather than by
/// pretending they are:
///
/// - **comment** goes through [`Accessor::set_comment`], which touches only the
///   `COMM` frame with an *empty* description. A `COMM:iTunNORM` and a
///   `COMM:encoded by` are different frames that happen to share an id; removing
///   every `COMM` to change the comment would take them with it, and they are
///   [`TagSet::extra`][crate::tags::TagSet::extra] rather than the comment;
/// - **year** is written as a `TDRC` **timestamp frame**, not a text frame. It
///   matters: `lofty`'s ID3v2.3 writer splits a `TDRC` timestamp into `TYER`,
///   `TDAT` and `TIME`, and *discards* a `TDRC` that is not one — so writing the
///   year as text would lose it on every v2.3 file;
/// - **track** and **disc** are one frame holding `n/total`, which is what
///   [`render_pair`][crate::tags::render_pair] already spells.
///
/// A replaced frame keeps the **text encoding the file used for it**, so changing
/// a Latin-1 `TPE1` in a 2003 scene release does not silently promote that one
/// frame to UTF-8 while its neighbours stay as they were.
fn id3v2(native: &mut Id3v2Tag, delta: &TagDelta, v23: bool) {
    repair_languages(native);
    for (field, edit) in delta.edits() {
        let keys = read::native_keys(TagType::Id3v2, *field);
        let primary = keys.first().copied().unwrap_or_default();

        if *field == Field::Comment {
            match edit.values() {
                Some(values) => native.set_comment(joined(values, v23)),
                None => native.remove_comment(),
            }
            continue;
        }

        // The encoding this file used, before the frame goes away.
        let encoding = frame_id(primary)
            .and_then(|id| native.get(&id))
            .and_then(|frame| match frame {
                Frame::Text(text) => Some(text.encoding),
                Frame::Timestamp(stamp) => Some(stamp.encoding),
                _ => None,
            })
            .unwrap_or(TextEncoding::UTF8);
        for key in keys {
            if let Some(id) = frame_id(key) {
                let _ = native.remove(&id).count();
            }
        }

        let Some(values) = edit.values() else {
            continue;
        };
        let Some(id) = frame_id(primary) else {
            continue;
        };
        let frame = if *field == Field::Year {
            // Checked by `check_edits`, so the parse cannot fail here.
            let Some(stamp) = values.first().and_then(timestamp) else {
                continue;
            };
            Frame::Timestamp(TimestampFrame::new(id, encoding, stamp))
        } else {
            Frame::Text(TextInformationFrame::new(id, encoding, joined(values, v23)))
        };
        let _ = native.insert(frame);
    }
}

/// Edit an m4a's `ilst` atoms.
///
/// `trkn` and `disk` are packed binary atoms rather than text, so they go through
/// [`Accessor`], which knows their byte layout; everything else is a UTF-8 text
/// atom, and a multi-valued field is one atom holding several of them — which is
/// how the format expresses it, rather than one atom holding a joined string.
fn ilst(native: &mut Ilst, delta: &TagDelta) {
    for (field, edit) in delta.edits() {
        for key in read::native_keys(TagType::Mp4Ilst, *field) {
            if let Some(ident) = fourcc(key) {
                let _ = native.remove(&ident).count();
            }
        }

        let Some(values) = edit.values() else {
            continue;
        };
        if matches!(field, Field::Track | Field::Disc) {
            // Checked by `check_edits`.
            let Some((number, total)) = values.first().and_then(parse_pair) else {
                continue;
            };
            match field {
                Field::Track => {
                    native.set_track(number);
                    if let Some(total) = total {
                        native.set_track_total(total);
                    }
                }
                _ => {
                    native.set_disk(number);
                    if let Some(total) = total {
                        native.set_disk_total(total);
                    }
                }
            }
            continue;
        }

        let Some(ident) = read::native_keys(TagType::Mp4Ilst, *field)
            .first()
            .and_then(|key| fourcc(key))
        else {
            continue;
        };
        let data = values
            .all()
            .iter()
            .map(|value| AtomData::UTF8(value.clone()))
            .collect();
        if let Some(atom) = Atom::from_collection(ident, data) {
            native.insert(atom);
        }
    }
}

/// Replace a `COMM` or `USLT` language code that is not three ASCII letters.
///
/// ID3v2 says a comment's language is a three-character ISO-639-2 code. 18 of the
/// real library's mp3s have `\x00\x00\x00` there instead, and **no conforming
/// writer will emit that** — `lofty` refuses the whole tag, so without this a
/// `--genre` edit on those files fails with a message about frame languages.
///
/// So the code is set to `XXX`, which is ID3v2's own "unknown language", and the
/// comment's text, description and encoding are left alone. It is the one thing in
/// this module that changes something the delta did not name, and it is the
/// narrow choice: the alternative is eighteen files the editor cannot touch
/// because of three bytes that were never valid.
fn repair_languages(native: &mut Id3v2Tag) {
    /// ID3v2's code for "the language is not known".
    const UNKNOWN: [u8; 3] = *b"XXX";

    native.retain_mut(|frame| {
        let language = match frame {
            Frame::Comment(comment) => &mut comment.language,
            Frame::UnsynchronizedText(lyrics) => &mut lyrics.language,
            _ => return true,
        };
        if !language.iter().all(u8::is_ascii_alphabetic) {
            *language = UNKNOWN;
        }
        true
    });
}

/// An ID3v2 frame id, or `None` for a key that is not a valid one.
fn frame_id(key: &str) -> Option<FrameId<'static>> {
    FrameId::new(std::borrow::Cow::Owned(key.to_owned()))
        .ok()
        .map(FrameId::into_owned)
}

/// A timestamp `lofty` will write as a `TDRC` frame.
fn timestamp(value: &str) -> Option<Timestamp> {
    value.trim().parse().ok()
}

/// Several values as one ID3v2 frame's text.
///
/// ID3v2.4 separates values inside a frame with a NUL, and `lofty` splits on it
/// when reading, so a multi-valued field round-trips exactly. **ID3v2.3 has no
/// such thing**: there is one string per frame and that is all the container can
/// hold, so the values are joined with [`SEPARATOR`][crate::tags::SEPARATOR] —
/// which is what every other tagger does, and what a reader will show as one
/// value because that is now what it is. A user writing three artists to a v2.3
/// file is trading them for one string; the alternative is refusing the edit,
/// which helps nobody.
fn joined(values: &Values, v23: bool) -> String {
    if v23 {
        values.joined()
    } else {
        values.all().join("\0")
    }
}

/// Edit a FLAC's comments in place, without the generic tag.
///
/// The one container that does **not** take the four-step route, because
/// measuring it showed `SplitTag`/`MergeTag` does two things to a FLAC that a
/// field edit must not do:
///
/// - it **re-cases every comment key** it understands. The fixture FLACs — and
///   everything `ffmpeg` writes — spell them `title`, `artist`, `date`; the merge
///   writes back `TITLE`, `ARTIST`, `DATE`. Comment keys are case-insensitive by
///   spec, so nothing *means* anything different, but rewriting the spelling of
///   nine fields in order to change one is not "only the fields in the delta";
/// - it **loses an `ENCODER` comment**. The split carries the vendor string out as
///   `EncoderSoftware` so a merge can put it back, and the merge takes the first
///   `EncoderSoftware` it finds to be the vendor — so a file that has both a
///   vendor string and an `ENCODER` comment comes back with only the vendor.
///   Every `ffmpeg`-produced FLAC in this library has both.
///
/// None of that is a problem in `lofty`: the round trip is lossless about
/// *values*, which is what it promises. It is a problem here because a tag editor
/// is judged on what it does to the fields it was not asked about. The native
/// comment API is complete enough to avoid the question — remove the keys, push
/// the new values, touch nothing else — so that is what this does.
///
/// A field already in the file keeps the file's own spelling of its key; a field
/// being added uses the canonical one from [`read::native_keys`].
fn vorbis(native: &mut VorbisComments, delta: &TagDelta) {
    for (field, edit) in delta.edits() {
        let keys = read::native_keys(TagType::VorbisComments, *field);
        let Some((&primary, totals)) = keys.split_first() else {
            continue;
        };

        // The spelling this file uses, before it is removed.
        let spelling = native
            .items()
            .find(|(key, _): &(&str, &str)| key.eq_ignore_ascii_case(primary))
            .map_or_else(|| primary.to_owned(), |(key, _)| key.to_owned());
        let total_spelling = totals
            .iter()
            .find_map(|total| {
                native
                    .items()
                    .find(|(key, _): &(&str, &str)| key.eq_ignore_ascii_case(total))
                    .map(|(key, _)| key.to_owned())
            })
            .or_else(|| totals.first().map(|total| (*total).to_owned()));

        for key in keys {
            let _ = native.remove(key).count();
        }

        let Some(values) = edit.values() else {
            continue;
        };
        if matches!(field, Field::Track | Field::Disc) {
            // Checked by `check_edits`, so the parse cannot fail here.
            if let Some((number, total)) = values.first().and_then(parse_pair) {
                native.push(spelling, number.to_string());
                if let (Some(total), Some(key)) = (total, total_spelling) {
                    native.push(key, total.to_string());
                }
            }
            continue;
        }
        for value in values.all() {
            // One comment per value: three artists are three `ARTIST` lines.
            native.push(spelling.clone(), value.clone());
        }
    }
}

/// An `ilst` atom identifier from the spelling [`read::native_keys`] uses.
fn fourcc(key: &str) -> Option<AtomIdent<'static>> {
    let bytes: Vec<u8> = key
        .chars()
        .map(|c| {
            if c == '\u{a9}' {
                0xa9
            } else {
                u8::try_from(c).unwrap_or(b'?')
            }
        })
        .collect();
    <[u8; 4]>::try_from(bytes.as_slice())
        .ok()
        .map(AtomIdent::Fourcc)
}

/// Write the parsed file's tag into `path`, which already holds its audio.
///
/// # An mp3 is written through its ID3v2 tag, not through the file
///
/// Writing the *tag* splices one region at the front of the file and leaves every
/// byte after it alone. Writing the *file* re-encodes every tag it parsed, and
/// 25 of the real library's mp3s also carry an APEv2 tag whose header flags
/// `lofty` then corrects — the originals have the "has footer" bit clear with a
/// footer present, which is malformed, and `lofty` is right about it. MPDFM was
/// asked to change a genre. Going through the tag also guarantees that an mp3's
/// ID3v1 tag survives untouched, which matters because MPD reads it when there is
/// nothing else.
///
/// **It can fail, and then the file is written instead.** Writing a tag on its own
/// needs the container identified from the bytes with no file name to fall back
/// on, and 18 of the real library's mp3s begin with a run of zero bytes before the
/// first MPEG sync word, which no content probe can get past. `MpegFile` already
/// knows what it parsed, so for those the whole file is written: slightly more is
/// re-encoded, every value still survives, and the alternative is refusing to edit
/// eighteen files that play perfectly well.
///
/// # FLAC and MP4 are written through the file
///
/// There the tag is not a region that can be spliced. A FLAC's cover art lives in
/// its own metadata blocks beside the comment block, and an `ilst` sits inside the
/// `moov` atom whose sizes have to be fixed up around it. Both round-trip cleanly
/// against the real library, byte for byte outside the field that changed.
fn save(parsed: &Parsed, path: &Utf8Path, options: WriteOptions) -> Result<(), String> {
    let saved = match parsed {
        Parsed::Mpeg(file) => {
            // An absent tag is written as an empty one, which `lofty` renders as
            // no tag at all — which is what a delta that cleared every field
            // means.
            let tag = file.id3v2().cloned().unwrap_or_default();
            tag.save_to_path(path, options)
                .or_else(|_| file.save_to_path(path, options))
        }
        Parsed::Flac(file) => file.save_to_path(path, options),
        Parsed::Mp4(file) => file.save_to_path(path, options),
    };
    saved.map_err(|err| chain(&err))
}

/// An error and everything under it, as one line.
///
/// `lofty`'s encoding error renders as `failed to write MPEG file` and keeps what
/// actually went wrong in its source, which is the only part worth telling a user
/// about.
fn chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(next) = source {
        message.push_str(": ");
        message.push_str(&next.to_string());
        source = next.source();
    }
    message
}

/// Whether the mode bits say MPDFM may not write this file.
///
/// Advisory, as mode bits always are: a file this says is writable can still be
/// refused by an ACL or a read-only mount, and the write then fails at the copy
/// with the operating system's own message. The point of checking here is that
/// the overwhelmingly common case — a file copied off a CD with `444` — is
/// refused before a backup is taken and before a temp file exists, which is what
/// task 19 needs to refuse a whole batch for.
fn read_only(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o200 == 0
    }
    #[cfg(not(unix))]
    {
        meta.permissions().readonly()
    }
}

/// A hidden temp name beside `path`, so the publishing `rename` is on one
/// filesystem and cannot be an `EXDEV` copy.
fn temp_path(path: &Utf8Path) -> Utf8PathBuf {
    let name = path.file_name().unwrap_or("tag");
    let unique = std::process::id();
    path.with_file_name(format!(".{name}.mpdfm-tag-{unique}"))
}

/// Make a file's contents durable.
fn fsync(path: &Utf8Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// Make a rename durable. Best effort: a directory that cannot be opened for
/// this is not a reason to fail a write that has already succeeded.
fn fsync_dir(path: &Utf8Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        dir.sync_all().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_is_ordered_by_field_and_holds_one_edit_each() {
        let delta = TagDelta::new()
            .set(Field::Genre, "Hip Hop")
            .set(Field::Album, "Mm..Food")
            .clear(Field::Comment)
            .set(Field::Album, "Mm..Food (2004)");

        let fields: Vec<Field> = delta.edits().iter().map(|(field, _)| *field).collect();
        assert_eq!(fields, [Field::Album, Field::Genre, Field::Comment]);
        assert_eq!(
            delta.get(Field::Album),
            Some(&Edit::Set(Values::one("Mm..Food (2004)")))
        );
        assert_eq!(delta.get(Field::Comment), Some(&Edit::Clear));
        assert_eq!(delta.get(Field::Title), None);
    }

    #[test]
    fn two_deltas_asking_for_the_same_thing_are_equal() {
        // Commit compares a re-validated plan against the previewed one, so a
        // delta built in a different order must not look like a different plan.
        let one = TagDelta::new()
            .set(Field::Genre, "Jazz")
            .clear(Field::Comment);
        let other = TagDelta::new()
            .clear(Field::Comment)
            .set(Field::Genre, "Jazz");
        assert_eq!(one, other);
    }

    #[test]
    fn a_typed_value_with_the_separator_in_it_becomes_several() {
        let delta = TagDelta::new().set(Field::Artist, "Madvillain; MF DOOM");
        assert_eq!(
            delta.get(Field::Artist),
            Some(&Edit::Set(Values::of(["Madvillain", "MF DOOM"])))
        );
    }

    #[test]
    fn an_edit_renders_the_way_the_preview_shows_it() {
        let delta = TagDelta::new()
            .set(Field::Genre, "Hip Hop")
            .clear(Field::Comment);
        assert_eq!(delta.rendered(), r#"genre="Hip Hop", comment=<cleared>"#);
    }

    #[test]
    fn a_number_field_refuses_anything_that_is_not_a_number() {
        let path = Utf8Path::new("/music/a.mp3");
        let bad = TagDelta::new().set(Field::Track, "one");
        let err = check_edits(path, &bad).expect_err("`one` is not a track number");
        assert!(err.to_string().contains("number/total"), "{err}");
        assert_eq!(err.path(), path);

        // And a number with a total is fine.
        assert!(check_edits(path, &TagDelta::new().set(Field::Track, "5/12")).is_ok());
        assert!(check_edits(path, &TagDelta::new().set(Field::Track, "5")).is_ok());
    }

    #[test]
    fn setting_a_field_to_nothing_is_refused_rather_than_treated_as_a_clear() {
        let path = Utf8Path::new("/music/a.mp3");
        let empty = TagDelta::new().set_values(Field::Genre, Values::none());
        let err = empty_err(path, &empty);
        assert!(err.contains("use a clear"), "{err}");
    }

    fn empty_err(path: &Utf8Path, delta: &TagDelta) -> String {
        check_edits(path, delta)
            .expect_err("an empty set is not a clear")
            .to_string()
    }

    #[test]
    fn a_year_must_be_a_date_that_can_actually_be_stored() {
        let path = Utf8Path::new("/music/a.mp3");
        for good in ["2004", "2019-03-15", "1999-05"] {
            assert!(
                check_edits(path, &TagDelta::new().set(Field::Year, good)).is_ok(),
                "{good} should be a date"
            );
        }
        let err = check_edits(path, &TagDelta::new().set(Field::Year, "MMIV"))
            .expect_err("MMIV is not a date");
        assert!(err.to_string().contains("2019-03-15"), "{err}");
    }

    #[test]
    fn the_id3_version_policy_keeps_what_the_file_has() {
        assert!(Id3Version::Keep.use_v23(Id3v2Version::V3));
        assert!(!Id3Version::Keep.use_v23(Id3v2Version::V4));
        // v2.2 cannot be written at all, so `keep` leaves it at v2.4.
        assert!(!Id3Version::Keep.use_v23(Id3v2Version::V2));

        assert!(Id3Version::V23.use_v23(Id3v2Version::V4));
        assert!(!Id3Version::V24.use_v23(Id3v2Version::V3));
    }

    #[test]
    fn an_atom_identifier_round_trips_through_its_spelling() {
        assert_eq!(
            fourcc("\u{a9}nam"),
            Some(AtomIdent::Fourcc([0xa9, b'n', b'a', b'm']))
        );
        assert_eq!(fourcc("aART"), Some(AtomIdent::Fourcc(*b"aART")));
        assert_eq!(fourcc("too long"), None);
    }

    #[test]
    fn the_temp_name_is_hidden_and_beside_the_original() {
        let temp = temp_path(Utf8Path::new("/music/album/01 Beef Rap.mp3"));
        assert_eq!(temp.parent(), Some(Utf8Path::new("/music/album")));
        assert!(temp.file_name().unwrap().starts_with(".01 Beef Rap.mp3."));
    }

    #[test]
    fn a_backup_with_no_copy_cannot_be_restored() {
        let backup = TagBackup {
            target: Utf8PathBuf::from("/music/a.mp3"),
            copy: Utf8PathBuf::new(),
            size: 10,
        };
        assert!(restore(&backup).is_err());
    }
}
