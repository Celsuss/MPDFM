//! [`FsStep`] — the smallest filesystem change MPDFM makes — and the receipt that
//! is enough to put each one back.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::ops::exec_fs::{self, FsStep, Options};
//! use mpdfm_core::paths::RelPath;
//!
//! let root = Utf8Path::new("/home/me/Music");
//! let options = Options::default();
//! let step = FsStep::RenameFile {
//!     from: RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)/01 Beef Rap.mp3")?,
//!     to: RelPath::parse("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3")?,
//! };
//!
//! // Nothing is written by `check` — it is the question the preview asks.
//! for warning in exec_fs::check(&step, root, &options)? {
//!     eprintln!("mpdfm: warning: {warning}");
//! }
//!
//! let receipt = exec_fs::execute_with(&step, root, &options)?;
//! exec_fs::revert(&receipt, root)?; // and the track is byte-for-byte back
//! # Ok::<(), mpdfm_core::Error>(())
//! ```
//!
//! # Why the unit is this small
//!
//! A user-level operation — "move this album" — is expanded into one step per
//! file before anything is executed (task 10). It costs a longer journal, and it
//! buys the only property that matters when a move dies halfway through: every
//! file that did move has a receipt, so the transaction can be reversed exactly,
//! and the fourteen files that did not are still where they were. A single
//! `MoveDir` step that failed on file nine would leave a half-moved album and
//! nothing to reverse it with.
//!
//! # The rules, and why each one is here
//!
//! **Nothing is ever overwritten.** A destination that already exists is
//! [`FsError::Exists`], for a file and for a directory alike. The one deliberate
//! exception is a case-only rename, where the destination *is* the source on a
//! case-insensitive filesystem — see below.
//!
//! **A cross-device move is copy → `fsync` → verify → `rename` → unlink, in that
//! order.** The copy lands on a hidden temp name beside the destination, so the
//! destination never exists in a partial state, and the source is unlinked only
//! once the destination is durable under its final name. `EXDEV` is detected by
//! error kind from a `rename` that failed, never by comparing device numbers up
//! front: a bind mount, an overlay or an automounted network share can all make
//! `st_dev` say something that is not true of `rename`.
//!
//! **A case-only rename goes through a temp name.** `Artist` → `artist` is a
//! no-op `rename` on ext4 and a lost file on a case-insensitive filesystem that
//! decides the two names are the same entry. Two renames through
//! `.Artist.mpdfm-case-…` work on both, so MPDFM always does it that way rather
//! than asking the filesystem what it is. Case is folded ASCII-only, for the same
//! reason [`crate::paths`] does not fold it at all: Unicode case depends on the
//! locale and on the filesystem, and `Ä` → `ä` is therefore an ordinary rename
//! here, which a case-insensitive filesystem refuses as a collision rather than
//! silently merging.
//!
//! **An empty directory is removed with `remove_dir`, walking up to but never
//! past `root`.** Never `remove_dir_all`, and never "delete the `.DS_Store` and
//! then it will be empty": a directory holding clutter is not empty, it is left
//! alone, and [`FsWarning::OnlyClutter`] says so. Deleting a file the user did
//! not ask to delete is the bug this rule exists to prevent.
//!
//! **A delete is a move into the transaction's backup directory.** Only a
//! [`FsStep::RemoveFile`] with `backup: None` actually unlinks anything, its
//! receipt says the bytes are gone, and [`revert`] refuses it. `delete_enabled =
//! false` refuses every `RemoveFile` outright.
//!
//! **Every path is checked against the root it belongs to, with symlinks
//! resolved, on the way in and again on the way back.** [`crate::paths::contains`]
//! is that check (safety invariant 5), and [`revert`] repeats it because a
//! receipt may have come from a journal file written by a previous run and is not
//! to be trusted just because MPDFM wrote it.
//!
//! **Write permission is proved, not guessed.** Before the first mutation of a
//! step, both parent directories are probed by creating and removing a hidden
//! temp file. Mode bits lie in the presence of ACLs, of a read-only mount and of
//! `root`, and "we found out at file nine of fourteen" is not an acceptable way
//! to discover a directory is not writable. [`check`], which must not write
//! anything, falls back to the mode bits and is documented as advisory.
//!
//! # What this module does not do
//!
//! No transaction, no journal, no ordering. [`execute_with`] takes one step,
//! does it, and hands back a receipt; deciding the order of steps belongs to
//! [`plan`][super::plan] and making the record durable before the first one runs
//! to [`commit`][super::commit]. Calling [`execute_with`] anywhere but from a
//! commit violates safety invariant 1, so the only callers are that module and
//! the tests here.

use std::io::Read as _;
use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};

use crate::config::Config;
use crate::paths::{self, PathError, RelPath};

/// One filesystem change: the unit that is executed, journaled and reverted.
///
/// Every path inside the library is a [`RelPath`], so a step means the same thing
/// after `music_directory` has been moved or renamed — the journal records the
/// root separately. The one absolute path is a backup destination, which lives in
/// MPDFM's own data directory and not in the library at all.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FsStep {
    /// Create `at` and any missing directory above it.
    ///
    /// A directory that is already there is not an error — that idempotence is
    /// what lets a directory merge be expressed as `MkDir` plus one
    /// [`FsStep::RenameFile`] per file.
    MkDir {
        /// The directory to create.
        at: RelPath,
    },

    /// Move one entry from `from` to `to` by `rename`, falling back to a
    /// copy-then-unlink if the filesystem reports `EXDEV`.
    ///
    /// "File" is the usual case, but `rename` moves a directory just as well and
    /// a case-only directory rename has to be exactly one step (expanding it
    /// per-file would collide with itself on a case-insensitive filesystem). A
    /// directory step is therefore accepted; it is the planner's job not to emit
    /// one for a directory whose contents need individual receipts.
    RenameFile {
        /// Where it is now.
        from: RelPath,
        /// Where it should be.
        to: RelPath,
    },

    /// Move one file across a filesystem boundary: copy to a temp name beside
    /// `to`, make it durable, verify it, `rename` it into place, then unlink
    /// `from`.
    ///
    /// [`FsStep::RenameFile`] arrives here by itself when `rename` says `EXDEV`.
    /// This variant exists for the planner that already knows the destination is
    /// on another mount, and it is what the tests use to exercise the path
    /// without needing a second filesystem.
    CopyDelete {
        /// Where it is now.
        from: RelPath,
        /// Where it should be.
        to: RelPath,
    },

    /// Remove one file, by moving it into the transaction's backup directory.
    ///
    /// `backup` is absolute because the backup directory is outside the library
    /// (`$XDG_DATA_HOME/mpdfm/backups/<txid>/`), and it must lie inside
    /// [`Options::backup_root`]. `None` unlinks the file for good, which
    /// [`revert`] cannot undo and [`FsWarning::NoBackup`] warns about.
    RemoveFile {
        /// The file to remove.
        target: RelPath,
        /// Where its bytes are kept so undo can restore them.
        backup: Option<Utf8PathBuf>,
    },

    /// Remove `at` if it is empty, then its parent if that is now empty, and so
    /// on up — stopping at the first directory that is not empty, and never
    /// touching `root` itself.
    RmDirIfEmpty {
        /// The deepest directory to try.
        at: RelPath,
    },
}

impl std::fmt::Display for FsStep {
    /// A one-line rendering, for an error message or a journal listing.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MkDir { at } => write!(f, "mkdir {at}"),
            Self::RenameFile { from, to } => write!(f, "rename {from} -> {to}"),
            Self::CopyDelete { from, to } => write!(f, "copy+delete {from} -> {to}"),
            Self::RemoveFile {
                target,
                backup: Some(backup),
            } => write!(f, "remove {target} (backup {backup})"),
            Self::RemoveFile {
                target,
                backup: None,
            } => write!(f, "remove {target} (no backup)"),
            Self::RmDirIfEmpty { at } => write!(f, "rmdir-if-empty {at}"),
        }
    }
}

// ---------------------------------------------------------------------------

/// What the caller allows, and what it wants checked.
///
/// [`Default`] is the cautious answer to every question: no deletion, no backup
/// root, no verification hash, no injected failure. A caller that forgets to pass
/// the configuration therefore cannot delete anything, which is the right way for
/// that mistake to show up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// [`Config::delete_enabled`]. When false, every [`FsStep::RemoveFile`] is
    /// refused with [`FsError::DeleteDisabled`], backup or no backup.
    pub delete_enabled: bool,

    /// `--verify`: hash the bytes on the way through a cross-device copy and
    /// read the destination back to compare. The size is always compared; this
    /// buys the case where a filesystem reports a full write of the wrong bytes,
    /// at the cost of reading the copy a second time.
    pub verify: bool,

    /// The root a [`FsStep::RemoveFile`] backup path must lie inside — MPDFM's
    /// data directory. Backing up outside it is refused: it is one of the three
    /// roots safety invariant 5 allows MPDFM to write to.
    pub backup_root: Option<Utf8PathBuf>,

    /// Failure injection, for the two failures a test cannot cause from outside.
    /// Production passes [`Inject::Nothing`].
    pub inject: Inject,
}

impl Options {
    /// The options a command line would produce: deletion as configured, backups
    /// under the configured data directory, no verification, no injection.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            delete_enabled: config.delete_enabled,
            verify: false,
            backup_root: Some(config.data_dir.clone()),
            inject: Inject::Nothing,
        }
    }

    /// The same options with `--verify` on.
    #[must_use]
    pub fn verifying(mut self) -> Self {
        self.verify = true;
        self
    }
}

/// A failure that only a test needs, because it cannot be produced from outside
/// the process.
///
/// The same device holds the source and the destination in every test, and no
/// test can pull the power out mid-copy, so both are injected here rather than
/// left unexercised — the precedent is `playlist::write::Stop`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Inject {
    /// Behave normally.
    #[default]
    Nothing,

    /// Make `rename` report `EXDEV`, so a [`FsStep::RenameFile`] takes the
    /// cross-device path it would take if the destination were on another mount.
    CrossDevice,

    /// Stop a cross-device move once the copy is complete and durable but before
    /// the `rename` that publishes it — and **leave the temp file behind**, which
    /// is what a power cut at that instant would leave. A test can then see that
    /// nothing is visible under the destination's own name and that the source is
    /// untouched. Every real failure cleans its temp file up.
    CrashAfterCopy,
}

// ---------------------------------------------------------------------------

/// What one [`FsStep`] actually did — enough to undo it, and enough for task 12
/// to check first whether undoing it is still safe.
///
/// Serializable because the journal is the only thing that survives a crash
/// (task 11): a receipt that could not be written down would make the step it
/// describes irreversible the moment the process died.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepReceipt {
    /// The step as it was asked for.
    pub step: FsStep,
    /// What happened, which is not always what was asked: a `RenameFile` across
    /// a filesystem boundary is recorded as [`Method::Copy`].
    pub done: Done,
    /// Anything worth telling the user that did not stop the step.
    pub warnings: Vec<FsWarning>,
}

/// The outcome of a step, in the shape its own reversal needs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Done {
    /// [`FsStep::MkDir`]: the directories created, outermost first. Empty when
    /// everything was already there, in which case there is nothing to undo.
    DirsCreated {
        /// Created by this step, outermost first.
        dirs: Vec<RelPath>,
    },

    /// [`FsStep::RenameFile`] or [`FsStep::CopyDelete`]: the entry is now at the
    /// step's `to`.
    Moved {
        /// How it got there.
        method: Method,
        /// Parent directories this step had to create, outermost first.
        dirs: Vec<RelPath>,
        /// What the entry was before it moved.
        facts: Facts,
    },

    /// [`FsStep::RemoveFile`]: the file is gone from the library.
    Removed {
        /// [`Method::Unlink`] means the bytes are gone and this cannot be
        /// reverted; anything else means they are at the step's `backup`.
        method: Method,
        /// What the file was.
        facts: Facts,
    },

    /// [`FsStep::RmDirIfEmpty`]: the directories removed, innermost first.
    DirsRemoved {
        /// Removed by this step, innermost first.
        dirs: Vec<RemovedDir>,
    },
}

/// How an entry got from one path to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Method {
    /// `rename`, which is atomic and keeps the inode, the mode and the mtime.
    /// A case-only rename is two renames through a temp name, and still this.
    Rename,
    /// Copy to a temp name, make it durable, `rename` it into place, unlink the
    /// source. Mode and mtime are copied deliberately, so that MPD's change
    /// detection sees the same file it saw before.
    Copy,
    /// Unlinked with no backup. The bytes are gone; [`revert`] refuses.
    Unlink,
}

/// What an entry was, recorded before it was touched.
///
/// This is what task 12 compares against before reversing a step: a destination
/// whose size or mtime no longer matches was changed by someone else after the
/// commit, and undoing it blindly would throw that change away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Facts {
    /// Length in bytes.
    pub size: u64,
    /// Last modification time, or `None` if the filesystem would not say.
    #[serde(with = "epoch", default)]
    pub mtime: Option<SystemTime>,
    /// Permission bits, on unix.
    pub mode: Option<u32>,
    /// FNV-1a of the contents — the same number `testing::digest` produces for
    /// the same bytes — recorded only when [`Options::verify`] made MPDFM read
    /// them anyway. Never a substitute for a cryptographic hash.
    pub hash: Option<u64>,
}

/// [`Facts::mtime`] on the way into and out of a journal record.
///
/// `serde`'s own `SystemTime` representation refuses any time before 1970, which
/// a file on disk is allowed to have — `touch -d 1969` is not an error — and a
/// commit that could not journal such a file would be unable to move it. Whole
/// seconds plus nanoseconds, signed, so every representable mtime round-trips.
mod epoch {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer};

    /// Seconds and nanoseconds since the epoch, as the journal spells them.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Stamp {
        /// Whole seconds, negative before 1970.
        secs: i64,
        /// Nanoseconds after `secs`, always forwards in time.
        nanos: u32,
    }

    /// Write `None` as `null`, and a time as its two numbers.
    pub(super) fn serialize<S: Serializer>(
        mtime: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let stamp = mtime.map(|mtime| match mtime.duration_since(UNIX_EPOCH) {
            Ok(since) => Stamp {
                secs: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
                nanos: since.subsec_nanos(),
            },
            // Before the epoch: `before` is how far back, so the whole second it
            // falls inside is one earlier whenever there is a remainder.
            Err(err) => {
                let before = err.duration();
                let secs = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
                match before.subsec_nanos() {
                    0 => Stamp {
                        secs: -secs,
                        nanos: 0,
                    },
                    nanos => Stamp {
                        secs: -secs - 1,
                        nanos: 1_000_000_000 - nanos,
                    },
                }
            }
        });
        stamp.serialize(serializer)
    }

    /// Read back what [`serialize`] wrote.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        let Some(stamp) = Option::<Stamp>::deserialize(deserializer)? else {
            return Ok(None);
        };
        let nanos = Duration::new(0, stamp.nanos);
        Ok(Some(match u64::try_from(stamp.secs) {
            Ok(secs) => UNIX_EPOCH + Duration::from_secs(secs) + nanos,
            Err(_) => {
                let before = Duration::from_secs(stamp.secs.unsigned_abs());
                UNIX_EPOCH - before + nanos
            }
        }))
    }
}

/// A directory [`FsStep::RmDirIfEmpty`] removed, with the mode to recreate it
/// with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemovedDir {
    /// Where it was.
    pub at: RelPath,
    /// Its permission bits, on unix.
    pub mode: Option<u32>,
}

// ---------------------------------------------------------------------------

/// Something worth telling the user that did not stop the step.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FsWarning {
    /// The destination differs only by case from something already in that
    /// directory. On ext4 these are two entries and the move is fine; on a
    /// case-insensitive filesystem they are one, and the step would have been
    /// refused as [`FsError::Exists`] instead. Reported either way, because a
    /// library with both `Artist/` and `artist/` in it is a mistake waiting to
    /// be copied onto a filesystem that cannot hold it.
    CaseCollision {
        /// The destination.
        at: RelPath,
        /// The name already there, as the directory spells it.
        existing: String,
    },

    /// The directory was left in place because it holds nothing but clutter.
    /// MPDFM does not delete files it was not asked to delete, so it does not
    /// empty a directory to be able to remove it.
    OnlyClutter {
        /// The directory that stayed.
        at: RelPath,
        /// What is in it.
        files: Vec<String>,
    },

    /// The file is being removed with no backup, so undo cannot restore it.
    NoBackup {
        /// The file.
        target: RelPath,
    },

    /// A symlink inside a directory being moved. It is reported and left where
    /// it is: MPDFM does not move what it has not resolved (the scanner takes
    /// the same line, `library::ScanWarning::Symlink`).
    Symlink {
        /// The link.
        at: RelPath,
    },

    /// An entry inside a directory being moved whose name MPDFM cannot
    /// represent, so it stays behind (`docs/PLAN.md` safety invariant 8).
    Unnamable {
        /// The path, rendered lossily for the message only.
        path: String,
        /// Why it is not a [`RelPath`].
        reason: PathError,
    },
}

impl std::fmt::Display for FsWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CaseCollision { at, existing } => write!(
                f,
                "{at} differs from {existing} only by case; on a case-insensitive \
                 filesystem they are the same entry"
            ),
            Self::OnlyClutter { at, files } => write!(
                f,
                "{at} was left in place: it still holds {}",
                files.join(", ")
            ),
            Self::NoBackup { target } => {
                write!(
                    f,
                    "{target} is being removed with no backup; undo cannot restore it"
                )
            }
            Self::Symlink { at } => write!(f, "{at} is a symlink and was left where it is"),
            Self::Unnamable { path, reason } => write!(f, "{path} was left behind: {reason}"),
        }
    }
}

/// Why a step was refused, or failed.
///
/// Every variant names the path it is about, because an error from the middle of
/// a fourteen-file album move that does not say which file is not worth raising.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// The path is not inside the root it belongs to, symlinks resolved. Safety
    /// invariant 5; this is the one error that means "MPDFM has a bug or is being
    /// lied to", and it fails closed.
    #[error("{path} is outside {root}")]
    Outside {
        /// The path that was asked for.
        path: Utf8PathBuf,
        /// The root it had to be inside.
        root: Utf8PathBuf,
    },

    /// The source of a move, or the target of a delete, is not there.
    #[error("{path} does not exist")]
    Missing {
        /// The path that was expected to exist.
        path: Utf8PathBuf,
    },

    /// The destination is already taken. MPDFM never overwrites.
    #[error("{path} already exists")]
    Exists {
        /// The destination.
        path: Utf8PathBuf,
    },

    /// A step that only applies to a file was given something else.
    #[error("{path} is not a regular file")]
    NotAFile {
        /// The path.
        path: Utf8PathBuf,
    },

    /// A step that only applies to a directory was given something else.
    #[error("{path} is not a directory")]
    NotADirectory {
        /// The path.
        path: Utf8PathBuf,
    },

    /// A directory MPDFM has to write in cannot be written to. Raised before the
    /// step mutates anything.
    #[error("{dir}: write permission is missing")]
    NotWritable {
        /// The directory.
        dir: Utf8PathBuf,
    },

    /// A file MPDFM has to read cannot be read. Raised before the step mutates
    /// anything.
    #[error("{path}: read permission is missing ({source})")]
    NotReadable {
        /// The file.
        path: Utf8PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },

    /// A directory that had to be empty is not. Raised by [`revert`] when
    /// something has appeared in a directory a step created.
    #[error("{path} is not empty: it holds {}", remaining.join(", "))]
    NotEmpty {
        /// The directory.
        path: Utf8PathBuf,
        /// What is in it.
        remaining: Vec<String>,
    },

    /// `delete_enabled = false`.
    #[error("refusing to delete {path}: delete_enabled is false")]
    DeleteDisabled {
        /// The file that would have been deleted.
        path: Utf8PathBuf,
    },

    /// A backup path was given with no [`Options::backup_root`] to check it
    /// against, so there is no way to know it is somewhere MPDFM may write.
    #[error("cannot back up {path}: no backup root is configured")]
    NoBackupRoot {
        /// The file that would have been backed up.
        path: Utf8PathBuf,
    },

    /// The copy does not match the original. The destination is removed and the
    /// source is left alone.
    #[error("the copy of {path} does not match the original: {detail}")]
    Verify {
        /// The source.
        path: Utf8PathBuf,
        /// What differed, and by how much.
        detail: String,
    },

    /// The destination is complete and durable, but the source could not be
    /// removed — so the file now exists twice. Reported precisely because the
    /// remedy (remove the source by hand) is different from every other failure.
    #[error("{to} is in place but {from} could not be removed: {source}")]
    SourceNotRemoved {
        /// The source that is still there.
        from: Utf8PathBuf,
        /// The destination that is complete.
        to: Utf8PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },

    /// The receipt does not describe a step that can be reversed.
    #[error("{step} cannot be reverted: {why}")]
    NotRevertible {
        /// The step, rendered.
        step: String,
        /// Why not.
        why: &'static str,
    },

    /// An I/O failure, with the path that caused it.
    #[error("{path}: {source}")]
    Io {
        /// The path being read, written or removed.
        path: Utf8PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// A path that could not be built — a reparenting that does not apply.
    #[error(transparent)]
    Path(#[from] PathError),

    /// [`Inject::CrashAfterCopy`] fired. Never raised in production.
    #[error("simulated crash after the copy of {path}, before the rename")]
    Injected {
        /// The source of the copy.
        path: Utf8PathBuf,
    },
}

// ---------------------------------------------------------------------------

/// Everything [`execute_with`] would refuse, and everything it would warn about,
/// **without writing anything**.
///
/// This is what task 10's preview calls: a `Conflict` is this returning `Err`, a
/// `Warning` is an entry in the returned list. Because it may not write, the
/// write-permission check here reads mode bits instead of probing — a directory
/// with no write bit set at all is refused, and one that is writable only by
/// another user passes here and is refused by [`execute_with`] before it touches
/// anything. Mode bits are the advisory answer; the probe is the true one.
///
/// # Errors
///
/// Any [`FsError`] the step would raise before mutating: containment, a missing
/// source, an occupied destination, the wrong kind of entry, a directory with no
/// write bit, an unreadable source, or a delete that the configuration forbids.
pub fn check(step: &FsStep, root: &Utf8Path, options: &Options) -> Result<Vec<FsWarning>, FsError> {
    inspect(step, root, options)
}

/// Execute one step with the cautious defaults — see [`Options`], which among
/// other things means deletion is refused.
///
/// # Errors
///
/// As [`execute_with`].
pub fn execute(step: &FsStep, root: &Utf8Path) -> Result<StepReceipt, FsError> {
    execute_with(step, root, &Options::default())
}

/// Execute one step and hand back the receipt that reverses it.
///
/// The order is fixed: every check that can be made without writing, then the
/// write-permission probe on both parent directories, then the mutation. Nothing
/// in the library has changed when this returns `Err` — with the two exceptions
/// the error itself names, [`FsError::SourceNotRemoved`] (the destination is in
/// place, the source is still there too) and [`FsError::Injected`] (a test asked
/// for a crash).
///
/// # Errors
///
/// Any [`FsError`]: see [`check`] for the ones raised before anything is written,
/// and [`FsError::Io`], [`FsError::Verify`] or [`FsError::SourceNotRemoved`] for a
/// failure during the work.
pub fn execute_with(
    step: &FsStep,
    root: &Utf8Path,
    options: &Options,
) -> Result<StepReceipt, FsError> {
    let warnings = inspect(step, root, options)?;
    probe(step, root)?;

    let done = match step {
        FsStep::MkDir { at } => Done::DirsCreated {
            dirs: create_dirs(root, at)?,
        },
        FsStep::RenameFile { from, to } => {
            move_within_root(root, from, to, Method::Rename, options)?
        }
        FsStep::CopyDelete { from, to } => move_within_root(root, from, to, Method::Copy, options)?,
        FsStep::RemoveFile { target, backup } => {
            remove_file(root, target, backup.as_deref(), options)?
        }
        FsStep::RmDirIfEmpty { at } => rmdir_upward(root, at)?,
    };

    Ok(StepReceipt {
        step: step.clone(),
        done,
        warnings,
    })
}

/// Put back what a receipt says was done.
///
/// Reverting the steps of a transaction in reverse order restores the tree
/// exactly: a move goes back the way it came (by `rename`, or by a copy that
/// restores the recorded mode and mtime), a created directory is removed if it is
/// still empty, a removed directory is recreated with its mode, and a backed-up
/// file is moved back out of the backup directory.
///
/// Every path is checked against `root` again. A receipt is data read back from a
/// journal file, and a journal file is not evidence of anything.
///
/// Whether reverting is still *safe* — whether the destination is still the file
/// the receipt describes — is [`Facts`] and task 12's question, deliberately not
/// this function's: `undo` has to be able to report every changed file at once
/// and offer to skip them, which it cannot do if the first one raises here.
///
/// # Errors
///
/// [`FsError::NotRevertible`] for a receipt that describes nothing reversible (an
/// unbacked delete, or a receipt whose step and outcome do not match),
/// [`FsError::NotEmpty`] when something has appeared in a directory that has to
/// go away, and otherwise as [`execute_with`].
pub fn revert(receipt: &StepReceipt, root: &Utf8Path) -> Result<(), FsError> {
    match (&receipt.step, &receipt.done) {
        (FsStep::MkDir { .. }, Done::DirsCreated { dirs }) => remove_created(root, dirs),

        (
            FsStep::RenameFile { from, to } | FsStep::CopyDelete { from, to },
            Done::Moved {
                method,
                dirs,
                facts,
            },
        ) => {
            let from_abs = guard(root, from)?;
            let to_abs = guard(root, to)?;
            move_back(&to_abs, &from_abs, *method, facts)?;
            remove_created(root, dirs)
        }

        (FsStep::RemoveFile { target, backup }, Done::Removed { method, facts }) => {
            let target_abs = guard(root, target)?;
            let (Some(backup), Method::Rename | Method::Copy) = (backup, method) else {
                return Err(FsError::NotRevertible {
                    step: receipt.step.to_string(),
                    why: "the file was unlinked with no backup, so its bytes are gone",
                });
            };
            move_back(backup, &target_abs, *method, facts)
        }

        (FsStep::RmDirIfEmpty { .. }, Done::DirsRemoved { dirs }) => recreate(root, dirs),

        _ => Err(FsError::NotRevertible {
            step: receipt.step.to_string(),
            why: "the receipt's outcome does not belong to its step",
        }),
    }
}

// ---------------------------------------------------------------------------

/// Whether a directory move may land in a directory that already exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Merge {
    /// An existing destination directory is [`FsError::Exists`]. The default,
    /// because "move A into B" when B is already there is far more often a typo
    /// than a merge.
    #[default]
    Refuse,
    /// Move the files that do not collide and report the ones that do. Not one
    /// file is overwritten either way.
    Allow,
}

/// One file of a directory move whose destination is already taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The file that stays where it is.
    pub from: RelPath,
    /// The path that is already occupied.
    pub to: RelPath,
}

/// A directory move, expanded into the steps that perform it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirMove {
    /// In execution order: every directory created, then every file moved, then
    /// the source directories offered up for removal deepest-first.
    pub steps: Vec<FsStep>,
    /// Files that were left out because their destination exists. Task 10 turns
    /// each into a `Conflict`, which blocks the commit; executing the steps
    /// anyway moves everything else and touches none of these.
    pub collisions: Vec<Collision>,
    /// Symlinks and unnamable entries that will stay behind.
    pub warnings: Vec<FsWarning>,
}

/// Expand "move this directory there" into per-file steps.
///
/// The steps are ordered so that nothing depends on a directory that does not
/// exist yet and nothing is removed before it is empty: every [`FsStep::MkDir`]
/// first, outermost first; then one [`FsStep::RenameFile`] per file; then one
/// [`FsStep::RmDirIfEmpty`] per source directory, deepest first. The last group
/// also takes away the directories *above* `from` if the move emptied them, which
/// is what stops a reorganization from leaving `hiphop/` behind with nothing in
/// it.
///
/// A [`FsStep::RenameFile`] that turns out to cross a filesystem boundary handles
/// itself, so the same expansion is right whether the destination is on this mount
/// or another one.
///
/// This walks the source directory. Task 10 has a `Library` in hand and could
/// list the files from the model instead; it calls this rather than reimplementing
/// it, so that the merge and collision rules exist exactly once, next to the code
/// that has to honour them.
///
/// # Errors
///
/// [`FsError::Outside`] for either end, [`FsError::Missing`] or
/// [`FsError::NotADirectory`] for the source, [`FsError::Exists`] when the
/// destination is a file, or when it is a directory and `merge` is
/// [`Merge::Refuse`], and [`FsError::Io`] if the source cannot be walked.
pub fn expand_dir_move(
    from: &RelPath,
    to: &RelPath,
    root: &Utf8Path,
    merge: Merge,
) -> Result<DirMove, FsError> {
    let from_abs = guard(root, from)?;
    let to_abs = guard(root, to)?;

    let meta = std::fs::symlink_metadata(&from_abs).map_err(|_| FsError::Missing {
        path: from_abs.clone(),
    })?;
    if !meta.is_dir() {
        return Err(FsError::NotADirectory { path: from_abs });
    }
    match std::fs::symlink_metadata(&to_abs) {
        // A file is never merged into, whatever was asked for.
        Ok(meta) if !meta.is_dir() || merge == Merge::Refuse => {
            return Err(FsError::Exists { path: to_abs });
        }
        _ => {}
    }

    let mut dirs = vec![to.clone()];
    let mut files = Vec::new();
    let mut sources = vec![from.clone()];
    let mut collisions = Vec::new();
    let mut warnings = Vec::new();

    // Driven by hand for `skip_current_dir`, as the scanner is: an entry MPDFM
    // cannot name takes its whole subtree out of the move, because MPDFM could
    // not name anything inside it either.
    let mut walk = walkdir::WalkDir::new(from_abs.as_std_path())
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(result) = walk.next() {
        let found = match result {
            Ok(found) => found,
            Err(err) => return Err(walk_error(&from_abs, err)),
        };
        let rel = match RelPath::from_abs_os(found.path(), root) {
            Ok(rel) => rel,
            Err(reason) => {
                warnings.push(FsWarning::Unnamable {
                    path: found.path().to_string_lossy().into_owned(),
                    reason,
                });
                if found.file_type().is_dir() {
                    walk.skip_current_dir();
                }
                continue;
            }
        };
        if found.file_type().is_symlink() {
            warnings.push(FsWarning::Symlink { at: rel });
            continue;
        }
        let dest = rel.reparent(from, to)?;
        if found.file_type().is_dir() {
            dirs.push(dest);
            sources.push(rel);
            continue;
        }
        if std::fs::symlink_metadata(dest.to_abs(root)).is_ok() {
            collisions.push(Collision {
                from: rel,
                to: dest,
            });
            continue;
        }
        files.push((rel, dest));
    }

    let mut steps: Vec<FsStep> = dirs.into_iter().map(|at| FsStep::MkDir { at }).collect();
    steps.extend(
        files
            .into_iter()
            .map(|(from, to)| FsStep::RenameFile { from, to }),
    );
    // Reversed, so the deepest source directory is offered first: the upward walk
    // each step performs makes the shallower ones no-ops, and `from` itself last.
    steps.extend(
        sources
            .into_iter()
            .rev()
            .map(|at| FsStep::RmDirIfEmpty { at }),
    );

    Ok(DirMove {
        steps,
        collisions,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Checking, before anything is written.

/// Every check that can be made without writing. See [`check`].
fn inspect(step: &FsStep, root: &Utf8Path, options: &Options) -> Result<Vec<FsWarning>, FsError> {
    let mut warnings = Vec::new();
    match step {
        FsStep::MkDir { at } => {
            let abs = guard(root, at)?;
            match std::fs::symlink_metadata(&abs) {
                // Already there, which is not an error: see `FsStep::MkDir`.
                Ok(meta) if meta.is_dir() => return Ok(warnings),
                Ok(_) => return Err(FsError::Exists { path: abs }),
                Err(_) => {}
            }
            writable_by_mode(&deepest_existing(root, at))?;
            collision_warning(root, at, &mut warnings);
        }

        FsStep::RenameFile { from, to } | FsStep::CopyDelete { from, to } => {
            let from_abs = guard(root, from)?;
            let to_abs = guard(root, to)?;
            let meta = std::fs::symlink_metadata(&from_abs).map_err(|_| FsError::Missing {
                path: from_abs.clone(),
            })?;
            if matches!(step, FsStep::CopyDelete { .. }) && !meta.is_file() {
                return Err(FsError::NotAFile { path: from_abs });
            }
            let case_only = is_case_only(&from_abs, &to_abs);
            if !case_only && std::fs::symlink_metadata(&to_abs).is_ok() {
                return Err(FsError::Exists { path: to_abs });
            }
            if meta.is_file() {
                // Opening for reading writes nothing, and a file MPDFM cannot
                // read is worth finding out about before the first rename rather
                // than after the ninth. A `rename` does not read the file, but it
                // may become a copy at any moment (`EXDEV`), so the check is the
                // same for both steps.
                drop(
                    std::fs::File::open(&from_abs).map_err(|source| FsError::NotReadable {
                        path: from_abs.clone(),
                        source,
                    })?,
                );
            }
            writable_by_mode(&parent_of(root, from))?;
            writable_by_mode(&deepest_existing_parent(root, to))?;
            if !case_only {
                collision_warning(root, to, &mut warnings);
            }
        }

        FsStep::RemoveFile { target, backup } => {
            let target_abs = guard(root, target)?;
            if !options.delete_enabled {
                return Err(FsError::DeleteDisabled { path: target_abs });
            }
            let meta = std::fs::symlink_metadata(&target_abs).map_err(|_| FsError::Missing {
                path: target_abs.clone(),
            })?;
            if !meta.is_file() {
                return Err(FsError::NotAFile { path: target_abs });
            }
            match backup {
                Some(backup) => {
                    let Some(backup_root) = &options.backup_root else {
                        return Err(FsError::NoBackupRoot { path: target_abs });
                    };
                    if !paths::contains(backup_root, backup) {
                        return Err(FsError::Outside {
                            path: backup.clone(),
                            root: backup_root.clone(),
                        });
                    }
                    if std::fs::symlink_metadata(backup).is_ok() {
                        return Err(FsError::Exists {
                            path: backup.clone(),
                        });
                    }
                    // The backup directory is created and synced by the commit
                    // before the first step runs (task 11, step 2), so a missing
                    // one is a bug in the caller rather than something to create
                    // here.
                    let parent = backup.parent().ok_or_else(|| FsError::Missing {
                        path: backup.clone(),
                    })?;
                    if !parent.is_dir() {
                        return Err(FsError::Missing {
                            path: parent.to_owned(),
                        });
                    }
                    writable_by_mode(parent)?;
                }
                None => warnings.push(FsWarning::NoBackup {
                    target: target.clone(),
                }),
            }
            writable_by_mode(&parent_of(root, target))?;
        }

        FsStep::RmDirIfEmpty { at } => {
            let abs = guard(root, at)?;
            // Already gone — a previous step's upward walk took it. Nothing to
            // do, and not an error: "if empty" includes "if there".
            let Ok(meta) = std::fs::symlink_metadata(&abs) else {
                return Ok(warnings);
            };
            if !meta.is_dir() {
                return Err(FsError::NotADirectory { path: abs });
            }
            writable_by_mode(&parent_of(root, at))?;
            let names = read_names(&abs)?;
            if !names.is_empty() && names.iter().all(|name| is_clutter(name)) {
                warnings.push(FsWarning::OnlyClutter {
                    at: at.clone(),
                    files: names,
                });
            }
        }
    }
    Ok(warnings)
}

/// Prove that every directory this step has to write in is writable, by writing
/// in it. See the [module docs][self] on why the mode bits are not enough.
fn probe(step: &FsStep, root: &Utf8Path) -> Result<(), FsError> {
    match step {
        FsStep::MkDir { at } => probe_writable(&deepest_existing(root, at)),
        FsStep::RenameFile { from, to } | FsStep::CopyDelete { from, to } => {
            probe_writable(&parent_of(root, from))?;
            probe_writable(&deepest_existing_parent(root, to))
        }
        FsStep::RemoveFile { target, backup } => {
            probe_writable(&parent_of(root, target))?;
            match backup.as_deref().and_then(Utf8Path::parent) {
                Some(dir) => probe_writable(dir),
                None => Ok(()),
            }
        }
        FsStep::RmDirIfEmpty { at } => {
            if at.to_abs(root).is_dir() {
                probe_writable(&parent_of(root, at))
            } else {
                Ok(())
            }
        }
    }
}

/// Create a hidden file in `dir` and remove it again.
///
/// The only portable way to ask "may I write here?" — `access(2)` answers for the
/// real uid rather than the effective one, and mode bits know nothing about ACLs
/// or a read-only mount. The file exists for microseconds, is hidden, and has an
/// extension MPD does not index.
fn probe_writable(dir: &Utf8Path) -> Result<(), FsError> {
    let probe = temp_beside(&dir.join("probe"), "probe");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            Err(FsError::NotWritable {
                dir: dir.to_owned(),
            })
        }
        Err(source) => Err(FsError::Io {
            path: probe,
            source,
        }),
    }
}

/// The advisory half of the write check, for [`check`], which may not write.
///
/// A directory with no write bit set for anybody is refused — including when the
/// process could write to it anyway, as `root` can. That is deliberate: the answer
/// has to be the same in the preview and in the commit, and refusing to write into
/// a directory whose owner marked it read-only is the conservative reading.
fn writable_by_mode(dir: &Utf8Path) -> Result<(), FsError> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.permissions().readonly() => Err(FsError::NotWritable {
            dir: dir.to_owned(),
        }),
        Ok(_) => Ok(()),
        Err(source) => Err(FsError::Io {
            path: dir.to_owned(),
            source,
        }),
    }
}

/// Warn when `at`'s name differs only by case from something already beside it.
///
/// Best effort: a directory that cannot be read produces no warning, because the
/// step's own checks will have something more useful to say about it.
fn collision_warning(root: &Utf8Path, at: &RelPath, warnings: &mut Vec<FsWarning>) {
    let abs = at.to_abs(root);
    let Some(parent) = abs.parent() else {
        return;
    };
    let Ok(names) = read_names(parent) else {
        return;
    };
    let name = at.file_name();
    for existing in names {
        if existing != name && existing.eq_ignore_ascii_case(name) {
            warnings.push(FsWarning::CaseCollision {
                at: at.clone(),
                existing,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Doing the work.

/// A move whose two ends are both inside the library.
fn move_within_root(
    root: &Utf8Path,
    from: &RelPath,
    to: &RelPath,
    method: Method,
    options: &Options,
) -> Result<Done, FsError> {
    let from_abs = from.to_abs(root);
    let to_abs = to.to_abs(root);
    let dirs = create_parents(root, to)?;
    match move_entry(&from_abs, &to_abs, method, options) {
        Ok((method, facts)) => Ok(Done::Moved {
            method,
            dirs,
            facts,
        }),
        Err(err) => {
            // The step did not happen, so there is no receipt to remove these
            // with later. A directory tree that grew half a move's worth of empty
            // directories is its own bug report.
            undo_created(root, &dirs);
            Err(err)
        }
    }
}

/// Move one entry, by whichever mechanism the filesystem allows.
///
/// `EXDEV` is the reason the returned [`Method`] may not be the one asked for.
fn move_entry(
    from: &Utf8Path,
    to: &Utf8Path,
    method: Method,
    options: &Options,
) -> Result<(Method, Facts), FsError> {
    let meta = std::fs::symlink_metadata(from).map_err(|source| FsError::Io {
        path: from.to_owned(),
        source,
    })?;
    let facts = Facts {
        size: meta.len(),
        mtime: meta.modified().ok(),
        mode: mode_of(&meta),
        hash: None,
    };

    if is_case_only(from, to) {
        two_step_rename(from, to)?;
        return Ok((Method::Rename, facts));
    }

    if method == Method::Rename && options.inject != Inject::CrossDevice {
        match std::fs::rename(from, to) {
            Ok(()) => {
                sync_dir(to.parent());
                return Ok((Method::Rename, facts));
            }
            // The one error that is not a failure: the destination is on another
            // filesystem, which only `rename` itself can be trusted to say.
            Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {}
            Err(source) => {
                return Err(FsError::Io {
                    path: to.to_owned(),
                    source,
                });
            }
        }
    }

    let hash = copy_then_unlink(from, to, &facts, options)?;
    Ok((Method::Copy, Facts { hash, ..facts }))
}

/// `Artist` → `artist`, through a name neither of them is.
///
/// A single `rename` is a no-op on a filesystem that folds case, and on some it
/// loses the file. Two renames work everywhere, so MPDFM does not ask which kind
/// of filesystem it is on.
fn two_step_rename(from: &Utf8Path, to: &Utf8Path) -> Result<(), FsError> {
    let staging = temp_beside(from, "case");
    std::fs::rename(from, &staging).map_err(|source| FsError::Io {
        path: from.to_owned(),
        source,
    })?;
    if let Err(source) = std::fs::rename(&staging, to) {
        // Back under its own name. Leaving it at the staging name would hide the
        // track from MPD and from the user at once.
        let _ = std::fs::rename(&staging, from);
        return Err(FsError::Io {
            path: to.to_owned(),
            source,
        });
    }
    sync_dir(to.parent());
    Ok(())
}

/// The cross-device path: copy, make durable, verify, publish, then unlink.
///
/// The copy lands on a hidden sibling of `to` — `.<name>.mpdfm-tmp-<pid>.<n>.tmp`
/// rather than the `dest.mpdfm-tmp` the task file sketched, so that MPD, which
/// reads this directory, cannot pick a half-copied track up as a track. A sibling
/// is what makes the final `rename` atomic; a temp file anywhere else would not
/// be.
fn copy_then_unlink(
    from: &Utf8Path,
    to: &Utf8Path,
    facts: &Facts,
    options: &Options,
) -> Result<Option<u64>, FsError> {
    let temp = temp_beside(to, "tmp");
    let hash = match copy_into(from, &temp, facts, options) {
        Ok(hash) => hash,
        Err(err) => {
            // Nothing of the source has been touched, so the only thing to clean
            // up is the partial copy.
            let _ = std::fs::remove_file(&temp);
            return Err(err);
        }
    };

    if options.inject == Inject::CrashAfterCopy {
        // Deliberately leaves the temp file behind: that is what losing power
        // here would leave, and the point is that `to` does not exist.
        return Err(FsError::Injected {
            path: from.to_owned(),
        });
    }

    if let Err(source) = std::fs::rename(&temp, to) {
        let _ = std::fs::remove_file(&temp);
        return Err(FsError::Io {
            path: to.to_owned(),
            source,
        });
    }
    // The destination's *name* has to survive a power cut before the source is
    // allowed to go; the contents were synced in `copy_into`.
    sync_dir(to.parent());

    if let Err(source) = std::fs::remove_file(from) {
        return Err(FsError::SourceNotRemoved {
            from: from.to_owned(),
            to: to.to_owned(),
            source,
        });
    }
    sync_dir(from.parent());
    Ok(hash)
}

/// Copy `from` to `temp`, stamp it with `facts`, make it durable, and check it.
fn copy_into(
    from: &Utf8Path,
    temp: &Utf8Path,
    facts: &Facts,
    options: &Options,
) -> Result<Option<u64>, FsError> {
    let io = |path: &Utf8Path| {
        let path = path.to_owned();
        move |source| FsError::Io { path, source }
    };

    let mut source = std::fs::File::open(from).map_err(|source| FsError::NotReadable {
        path: from.to_owned(),
        source,
    })?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(io(temp))?;

    let hash = copy_bytes(&mut source, &mut file, options.verify).map_err(io(temp))?;
    // mtime first, mode second: a `0444` file is still writable through a handle
    // opened before the mode was set, and stamping the time does not disturb the
    // bits. Both are preserved on purpose — MPD notices a changed mtime, and a
    // move should not look to it like an edit.
    if let Some(mtime) = facts.mtime {
        file.set_modified(mtime).map_err(io(temp))?;
    }
    if let Some(mode) = facts.mode {
        set_mode(&file, mode).map_err(io(temp))?;
    }
    file.sync_all().map_err(io(temp))?;
    drop(file);

    let written = std::fs::symlink_metadata(temp).map_err(io(temp))?.len();
    if written != facts.size {
        return Err(FsError::Verify {
            path: from.to_owned(),
            detail: format!("{written} bytes written, {} expected", facts.size),
        });
    }
    if let Some(expected) = hash {
        let actual = hash_file(temp)?;
        if actual != expected {
            return Err(FsError::Verify {
                path: from.to_owned(),
                detail: format!("hash {actual:#018x}, expected {expected:#018x}"),
            });
        }
    }
    Ok(hash)
}

/// Remove one file, into the backup directory when there is one.
fn remove_file(
    root: &Utf8Path,
    target: &RelPath,
    backup: Option<&Utf8Path>,
    options: &Options,
) -> Result<Done, FsError> {
    let target_abs = target.to_abs(root);
    let io = |source| FsError::Io {
        path: target_abs.clone(),
        source,
    };

    match backup {
        // A backup is just a move whose destination is MPDFM's own directory — on
        // another filesystem as often as not, which `move_entry` handles.
        Some(backup) => {
            let (method, facts) = move_entry(&target_abs, backup, Method::Rename, options)?;
            Ok(Done::Removed { method, facts })
        }
        None => {
            let meta = std::fs::symlink_metadata(&target_abs).map_err(io)?;
            let facts = Facts {
                size: meta.len(),
                mtime: meta.modified().ok(),
                mode: mode_of(&meta),
                hash: None,
            };
            std::fs::remove_file(&target_abs).map_err(io)?;
            sync_dir(target_abs.parent());
            Ok(Done::Removed {
                method: Method::Unlink,
                facts,
            })
        }
    }
}

/// Remove `at` and then every parent that the removal emptied, stopping at the
/// first one that is not empty and never reaching `root`.
///
/// Only the failure to remove `at` itself is an error. Above that MPDFM is
/// tidying up, and a parent it may not remove is not a reason to fail a move that
/// has already happened.
fn rmdir_upward(root: &Utf8Path, at: &RelPath) -> Result<Done, FsError> {
    let mut removed: Vec<RemovedDir> = Vec::new();
    // `RelPath::parent` returns `None` at the top level, which is what keeps the
    // walk inside the library: `root` itself has no `RelPath` and is never a
    // candidate.
    let mut current = Some(at.clone());
    while let Some(dir) = current {
        let abs = dir.to_abs(root);
        let Ok(meta) = std::fs::symlink_metadata(&abs) else {
            break;
        };
        if !meta.is_dir() || !read_names(&abs).is_ok_and(|names| names.is_empty()) {
            break;
        }
        let mode = mode_of(&meta);
        if let Err(source) = std::fs::remove_dir(&abs) {
            if removed.is_empty() {
                return Err(FsError::Io { path: abs, source });
            }
            break;
        }
        removed.push(RemovedDir {
            at: dir.clone(),
            mode,
        });
        current = dir.parent();
    }
    if let Some(last) = removed.last() {
        sync_dir(Some(&parent_of(root, &last.at)));
    }
    Ok(Done::DirsRemoved { dirs: removed })
}

// ---------------------------------------------------------------------------
// Directories.

/// Create `at` and everything missing above it, reporting what was created.
///
/// `create_dir_all` would do the same thing without saying which directories it
/// made, and a directory MPDFM created but cannot name is one `undo` would leave
/// behind.
fn create_dirs(root: &Utf8Path, at: &RelPath) -> Result<Vec<RelPath>, FsError> {
    let mut missing = Vec::new();
    let mut current = Some(at.clone());
    while let Some(dir) = current {
        if dir.to_abs(root).symlink_metadata().is_ok() {
            break;
        }
        current = dir.parent();
        missing.push(dir);
    }
    missing.reverse();

    let mut created = Vec::new();
    for dir in missing {
        let abs = dir.to_abs(root);
        match std::fs::create_dir(&abs) {
            Ok(()) => created.push(dir),
            // Someone else got there first, which is not a problem: the directory
            // is what was wanted. It is not recorded, because undoing a move must
            // not remove a directory this run did not create.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                undo_created(root, &created);
                return Err(FsError::Io { path: abs, source });
            }
        }
    }
    if let Some(outermost) = created.first() {
        sync_dir(Some(&parent_of(root, outermost)));
    }
    Ok(created)
}

/// [`create_dirs`] for the directory a file is moving into. A top-level
/// destination has no parent to create: `root` is already there.
fn create_parents(root: &Utf8Path, to: &RelPath) -> Result<Vec<RelPath>, FsError> {
    match to.parent() {
        Some(parent) => create_dirs(root, &parent),
        None => Ok(Vec::new()),
    }
}

/// Undo [`create_dirs`] after a step failed. Best effort: the error that made the
/// step fail is the one worth reporting.
fn undo_created(root: &Utf8Path, dirs: &[RelPath]) {
    for dir in dirs.iter().rev() {
        let _ = std::fs::remove_dir(dir.to_abs(root));
    }
}

/// Undo [`create_dirs`] for a revert, where a directory that is no longer empty
/// has to be reported rather than quietly left.
fn remove_created(root: &Utf8Path, dirs: &[RelPath]) -> Result<(), FsError> {
    for dir in dirs.iter().rev() {
        let abs = dir.to_abs(root);
        match std::fs::remove_dir(&abs) {
            Ok(()) => {}
            // Already gone: another step's upward walk took it, which is fine.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                return Err(FsError::NotEmpty {
                    remaining: read_names(&abs).unwrap_or_default(),
                    path: abs,
                });
            }
            Err(source) => return Err(FsError::Io { path: abs, source }),
        }
    }
    Ok(())
}

/// Put back what [`rmdir_upward`] removed, outermost first, with their modes.
fn recreate(root: &Utf8Path, dirs: &[RemovedDir]) -> Result<(), FsError> {
    for dir in dirs.iter().rev() {
        let abs = dir.at.to_abs(root);
        match std::fs::create_dir(&abs) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(FsError::Io { path: abs, source }),
        }
        if let Some(mode) = dir.mode {
            set_mode_of(&abs, mode).map_err(|source| FsError::Io {
                path: abs.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

/// Move an entry back where it came from, restoring its mode and mtime if the way
/// back is a copy.
fn move_back(
    current: &Utf8Path,
    original: &Utf8Path,
    method: Method,
    facts: &Facts,
) -> Result<(), FsError> {
    if std::fs::symlink_metadata(current).is_err() {
        return Err(FsError::Missing {
            path: current.to_owned(),
        });
    }
    let case_only = is_case_only(current, original);
    if !case_only && std::fs::symlink_metadata(original).is_ok() {
        return Err(FsError::Exists {
            path: original.to_owned(),
        });
    }
    for dir in [current.parent(), original.parent()].into_iter().flatten() {
        probe_writable(dir)?;
    }

    match method {
        Method::Rename if case_only => two_step_rename(current, original),
        Method::Rename => std::fs::rename(current, original).map_err(|source| FsError::Io {
            path: original.to_owned(),
            source,
        }),
        // The recorded facts describe the file as it was before the move, which
        // is exactly what the copy back has to reproduce.
        Method::Copy => {
            copy_then_unlink(current, original, facts, &Options::default())?;
            Ok(())
        }
        Method::Unlink => Err(FsError::NotRevertible {
            step: format!("remove {original}"),
            why: "the file was unlinked with no backup, so its bytes are gone",
        }),
    }
}

// ---------------------------------------------------------------------------
// Small things.

/// Names that do not make a directory worth keeping, but are still the user's
/// files and are never deleted to make a directory removable.
const CLUTTER: &[&str] = &[".ds_store", "thumbs.db", "desktop.ini", ".directory"];

/// How much of a file is read at a time when copying or hashing.
const COPY_CHUNK: usize = 64 * 1024;

/// FNV-1a's starting value.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a's multiplier.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The longest a temp file's copy of the original name may be, in bytes, leaving
/// room under a 255-byte `NAME_MAX` for the marker this module appends. Scene
/// release names get close enough to the limit for this to matter.
const MAX_TEMP_STEM: usize = 180;

/// Distinguishes one temp name from the next; `create_new` is what actually makes
/// them exclusive.
static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether a name is clutter, case-insensitively — `.DS_Store` arrives spelled
/// both ways, depending on which filesystem it came from.
fn is_clutter(name: &str) -> bool {
    CLUTTER.contains(&name.to_ascii_lowercase().as_str())
}

/// Whether `from` and `to` name the same entry up to ASCII case, and are not the
/// same string. Deliberately ASCII-only: see the [module docs][self].
fn is_case_only(from: &Utf8Path, to: &Utf8Path) -> bool {
    from != to && from.as_str().eq_ignore_ascii_case(to.as_str())
}

/// The absolute path of `rel` inside `root`, refused if it is not really inside
/// it (safety invariant 5).
fn guard(root: &Utf8Path, rel: &RelPath) -> Result<Utf8PathBuf, FsError> {
    let abs = rel.to_abs(root);
    if paths::contains(root, &abs) {
        Ok(abs)
    } else {
        Err(FsError::Outside {
            path: abs,
            root: root.to_owned(),
        })
    }
}

/// The directory `rel` lives in, which for a top-level entry is `root`.
fn parent_of(root: &Utf8Path, rel: &RelPath) -> Utf8PathBuf {
    rel.parent()
        .map_or_else(|| root.to_owned(), |parent| parent.to_abs(root))
}

/// The deepest existing directory at or above `at`, `root` if there is none.
fn deepest_existing(root: &Utf8Path, at: &RelPath) -> Utf8PathBuf {
    let mut current = Some(at.clone());
    while let Some(dir) = current {
        let abs = dir.to_abs(root);
        if abs.is_dir() {
            return abs;
        }
        current = dir.parent();
    }
    root.to_owned()
}

/// [`deepest_existing`] for the directory a file is moving into.
fn deepest_existing_parent(root: &Utf8Path, to: &RelPath) -> Utf8PathBuf {
    to.parent()
        .map_or_else(|| root.to_owned(), |parent| deepest_existing(root, &parent))
}

/// A hidden, unused sibling of `path`, tagged with what it is for.
///
/// Hidden and `.tmp`-suffixed so that MPD, which reads the music directory, does
/// not index it; tagged with the pid so two MPDFM processes cannot pick the same
/// name; and checked for existence so a leftover from a crashed run with the same
/// pid is stepped over rather than overwritten.
fn temp_beside(path: &Utf8Path, tag: &str) -> Utf8PathBuf {
    let name = truncate(path.file_name().unwrap_or("entry"), MAX_TEMP_STEM);
    let pid = std::process::id();
    loop {
        let counter = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp = path.with_file_name(format!(".{name}.mpdfm-{tag}-{pid}.{counter}.tmp"));
        if std::fs::symlink_metadata(&temp).is_err() {
            return temp;
        }
    }
}

/// The first `max` bytes of `name`, cut on a character boundary.
fn truncate(name: &str, max: usize) -> &str {
    if name.len() <= max {
        return name;
    }
    let mut end = max;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// Every name in `dir`, sorted. Non-UTF-8 names are rendered lossily: they are
/// here to be counted and reported, never to be written back.
fn read_names(dir: &Utf8Path) -> Result<Vec<String>, FsError> {
    let io = |source| FsError::Io {
        path: dir.to_owned(),
        source,
    };
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        names.push(
            entry
                .map_err(io)?
                .file_name()
                .to_string_lossy()
                .into_owned(),
        );
    }
    names.sort();
    Ok(names)
}

/// Copy every byte, folding FNV-1a as it goes when the hash is wanted.
///
/// Hashing on the way through is why `--verify` costs one extra read of the
/// destination rather than two reads of everything.
fn copy_bytes(
    reader: &mut std::fs::File,
    writer: &mut std::fs::File,
    hash: bool,
) -> std::io::Result<Option<u64>> {
    use std::io::Write as _;

    if !hash {
        std::io::copy(reader, writer)?;
        return Ok(None);
    }
    let mut digest = FNV_OFFSET;
    let mut buffer = vec![0_u8; COPY_CHUNK];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest = fold(digest, &buffer[..read]);
        writer.write_all(&buffer[..read])?;
    }
    Ok(Some(digest))
}

/// FNV-1a of a whole file, read in chunks so a FLAC image costs no memory.
fn hash_file(path: &Utf8Path) -> Result<u64, FsError> {
    let io = |source| FsError::Io {
        path: path.to_owned(),
        source,
    };
    let mut file = std::fs::File::open(path).map_err(io)?;
    let mut buffer = vec![0_u8; COPY_CHUNK];
    let mut digest = FNV_OFFSET;
    loop {
        let read = file.read(&mut buffer).map_err(io)?;
        if read == 0 {
            return Ok(digest);
        }
        digest = fold(digest, &buffer[..read]);
    }
}

/// One FNV-1a round per byte. Streaming or not, the result is the same number
/// `testing::digest` gives for the same bytes — which is what lets a test check
/// this module's arithmetic against an independent implementation.
fn fold(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// `fsync` a directory, so that a name created or removed in it survives a power
/// cut. Best effort: the operation it belongs to has already succeeded, and
/// reporting a failure here would tell the caller something untrue.
fn sync_dir(dir: Option<&Utf8Path>) {
    if let Some(dir) = dir
        && let Ok(handle) = std::fs::File::open(dir)
    {
        let _ = handle.sync_all();
    }
}

/// Permission bits, on platforms that have them.
fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// Set the permission bits of an open file.
fn set_mode(file: &std::fs::File, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, mode);
        Ok(())
    }
}

/// Set the permission bits of a path.
fn set_mode_of(path: &Utf8Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// A `walkdir` failure, with the path it happened on.
fn walk_error(root: &Utf8Path, err: walkdir::Error) -> FsError {
    let path = err.path().map_or_else(
        || root.to_owned(),
        |path| Utf8PathBuf::from(path.to_string_lossy().into_owned()),
    );
    let source = err
        .into_io_error()
        .unwrap_or_else(|| std::io::Error::other("the directory could not be walked"));
    FsError::Io { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arithmetic and the naming. Everything that needs a filesystem is in
    /// `crates/core/tests/move_executor.rs`, against the fixture library.
    #[test]
    fn a_step_renders_as_one_line() {
        let rel = |s: &str| RelPath::parse(s).expect("a valid path");
        let cases = [
            (FsStep::MkDir { at: rel("a/b") }, "mkdir a/b"),
            (
                FsStep::RenameFile {
                    from: rel("a/x.mp3"),
                    to: rel("b/x.mp3"),
                },
                "rename a/x.mp3 -> b/x.mp3",
            ),
            (
                FsStep::CopyDelete {
                    from: rel("a/x.mp3"),
                    to: rel("b/x.mp3"),
                },
                "copy+delete a/x.mp3 -> b/x.mp3",
            ),
            (
                FsStep::RemoveFile {
                    target: rel("a/x.mp3"),
                    backup: Some(Utf8PathBuf::from("/data/backups/tx/x.mp3")),
                },
                "remove a/x.mp3 (backup /data/backups/tx/x.mp3)",
            ),
            (
                FsStep::RemoveFile {
                    target: rel("a/x.mp3"),
                    backup: None,
                },
                "remove a/x.mp3 (no backup)",
            ),
            (
                FsStep::RmDirIfEmpty { at: rel("a/b") },
                "rmdir-if-empty a/b",
            ),
        ];
        for (step, expected) in cases {
            assert_eq!(step.to_string(), expected);
        }
    }

    #[test]
    fn a_case_only_rename_is_ascii_and_never_the_same_path() {
        let case_only = |from: &str, to: &str| is_case_only(Utf8Path::new(from), Utf8Path::new(to));

        assert!(case_only("/m/Artist", "/m/artist"));
        assert!(case_only("/m/a/01 Track.mp3", "/m/a/01 track.mp3"));
        // Not a rename at all.
        assert!(!case_only("/m/Artist", "/m/Artist"));
        // A real move that happens to differ in case as well.
        assert!(!case_only("/m/Artist/x.mp3", "/m/artist/y.mp3"));
        // Non-ASCII case is not folded — MPDFM folds no Unicode case anywhere, so
        // this is an ordinary rename, which a case-insensitive filesystem will
        // refuse as a collision rather than silently merge.
        assert!(!case_only("/m/Tänd", "/m/tÄnd"));
    }

    #[test]
    fn clutter_is_matched_however_it_is_spelled() {
        for name in [
            ".DS_Store",
            ".ds_store",
            "Thumbs.db",
            "THUMBS.DB",
            "desktop.ini",
        ] {
            assert!(is_clutter(name), "{name} should count as clutter");
        }
        for name in ["01 Beef Rap.mp3", "folder.jpg", "info.nfo", "ds_store"] {
            assert!(!is_clutter(name), "{name} is not clutter");
        }
    }

    /// The hash is only worth having if it is the same number every time, on every
    /// machine — so it is checked against the independent implementation in
    /// `testing::digest`, in one pass and in chunks.
    #[test]
    fn the_streaming_hash_agrees_with_the_test_suite_s_digest() {
        let cases: &[&[u8]] = &[b"", b"a", b"\x00\xff\x00", b"ID3\x04\x00 fake frame"];
        for bytes in cases {
            assert_eq!(fold(FNV_OFFSET, bytes), crate::testing::digest(bytes));
        }

        let long: Vec<u8> = (0..300_000_u32)
            .map(|n| u8::try_from(n % 251).unwrap())
            .collect();
        let mut chunked = FNV_OFFSET;
        for chunk in long.chunks(COPY_CHUNK) {
            chunked = fold(chunked, chunk);
        }
        assert_eq!(chunked, crate::testing::digest(&long));
    }

    #[test]
    fn a_temp_name_is_hidden_unique_and_not_a_track() {
        let target = Utf8Path::new("/m/hiphop/album/01 Beef Rap.mp3");
        let first = temp_beside(target, "tmp");
        let second = temp_beside(target, "tmp");
        assert_ne!(first, second);
        for temp in [&first, &second] {
            assert_eq!(
                temp.parent(),
                target.parent(),
                "the temp file must be a sibling"
            );
            let name = temp.file_name().expect("a name");
            assert!(name.starts_with(".01 Beef Rap.mp3.mpdfm-tmp-"), "{name}");
            assert!(name.ends_with(".tmp"), "{name}");
            assert_ne!(name, target.file_name().expect("a name"));
        }
    }

    #[test]
    fn a_temp_name_stays_inside_name_max() {
        // A scene release name long enough that appending the marker blindly would
        // exceed NAME_MAX and fail with ENAMETOOLONG.
        let long = "x".repeat(250);
        let temp = temp_beside(Utf8Path::new(&format!("/m/a/{long}.mp3")), "tmp");
        let name = temp.file_name().expect("a name");
        assert!(name.len() <= 255, "{} bytes is too long", name.len());
        assert!(name.ends_with(".tmp"));
    }

    #[test]
    fn truncating_a_name_cuts_on_a_character_boundary() {
        assert_eq!(truncate("01 Beef Rap.mp3", 180), "01 Beef Rap.mp3");
        // `ï` is two bytes: cutting at 4 would split it.
        assert_eq!(truncate("So Hï", 4), "So H");
        assert_eq!(truncate("So Hï", 5), "So H");
        assert_eq!(truncate("So Hï", 6), "So Hï");
        assert_eq!(truncate("ノスタルジア", 2), "");
    }
}
