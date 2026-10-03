//! The transaction record: what a commit is about to do, what it has done so
//! far, and everything undo needs to put it back.
//!
//! One JSON file per transaction, at
//! `$XDG_DATA_HOME/mpdfm/journal/<txid>.json`. It is written before the first
//! mutation and updated after every step, so at any instant the record is either
//! exactly what has happened or one step ahead of it — never behind (see
//! [`store`][super::store] for the durability discipline that makes that true).
//!
//! # The record is the only thing that survives
//!
//! Everything a crash leaves behind is in this file, so everything it describes
//! has to be data: a [`FsStep`] and a [`StepReceipt`] are serializable, and
//! nothing in a record is a handle, a closure or an index into something that was
//! in memory at the time. The one exception is [`PlaylistEdit::playlist`], which
//! is an index into the [`PlaylistIndex`][crate::playlist::PlaylistIndex] that
//! planned it; every field undo actually uses — the file name, the resolved path,
//! the lines — stands on its own next to it.
//!
//! # Forward compatibility
//!
//! A record is read back by whichever MPDFM is installed when the user runs
//! `undo`, which may be newer or older than the one that wrote it. Two rules
//! keep that honest:
//!
//! - [`Record::version`] is checked on load, and a record from a future format
//!   version is **refused with a message naming both versions** rather than
//!   half-understood;
//! - every field this version does not know is kept in [`Record::unknown`] and
//!   written back out, so a record a newer MPDFM wrote is not quietly stripped
//!   of its extra fields by an older one that merely read and re-saved it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use camino::Utf8PathBuf;

use crate::library::DirPath;
use crate::ops::effects::Summary;
use crate::ops::exec_fs::{Done, FsStep, FsWarning, StepReceipt};
use crate::ops::op::{Operation, Plan};
use crate::playlist::rewrite::{LineEdit, PlaylistEdit};

use super::JournalError;

/// The record format this build writes, and the only one it reads.
///
/// Bump it when a change would make an older MPDFM misread a record — not when a
/// field is added, which [`Record::unknown`] already handles.
pub const VERSION: u32 = 1;

/// A transaction's identity: a sortable UTC timestamp, a counter and a short
/// random suffix, e.g. `20260924T224500Z-00a3f1`.
///
/// Sortable because the newest transactions are the interesting ones and
/// `undo --list` and retention must not have to parse dates to order them — a
/// lexicographic sort of the journal directory is already newest-last. One second
/// is not fine enough on its own, so the counter comes next: it increases with
/// every id this process mints, which makes two commits inside one second sort in
/// the order they happened. The last four hex digits are there so that two
/// *processes* cannot name the same record; they are not a secret and carry no
/// meaning.
///
/// Two ids minted by two different processes inside the same second can still
/// sort in either order, since neither counter knows about the other. The only
/// thing that reads the order is retention, and the worst it can do with it is
/// prune the newer of two same-second transactions first.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct TxId(String);

/// How many ids this process has minted. Part of the id, so that a loop which
/// commits twice inside one clock tick gets two names *in order*.
static MINTED: AtomicU64 = AtomicU64::new(0);

impl TxId {
    /// A fresh id for a transaction starting now.
    #[must_use]
    pub fn now() -> Self {
        Self::at(SystemTime::now())
    }

    /// A fresh id for a transaction starting at `when`, for the tests that need
    /// to know what it will be.
    #[must_use]
    pub fn at(when: SystemTime) -> Self {
        let nanos = when
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| u64::from(since.subsec_nanos()));
        let minted = MINTED.fetch_add(1, Ordering::Relaxed);
        let salt = fnv(&[nanos, u64::from(std::process::id()), minted]);
        Self(format!(
            "{}-{:02x}{:04x}",
            compact(when),
            // Wraps after 256 commits in one process, which can only reorder two
            // transactions in the same second that are 256 apart.
            minted & 0xff,
            // 16 bits: two processes committing in the same second with the same
            // counter collide about once in 65 536, and it is short enough to read
            // out over the phone.
            salt & 0xffff
        ))
    }

    /// Read an id back — from a journal file name, or from a command line.
    ///
    /// Validated rather than trusted: an id becomes a path under the journal and
    /// backup directories, so `../../etc` must not be one. The accepted shape is
    /// ASCII alphanumerics, `-` and `_`, which every id this module mints is.
    ///
    /// # Errors
    ///
    /// [`JournalError::BadTxId`] for an empty id, one that is too long to be a
    /// file name, or one holding anything else.
    pub fn parse(s: &str) -> Result<Self, JournalError> {
        let bad = |why: &'static str| {
            Err(JournalError::BadTxId {
                input: s.to_owned(),
                why,
            })
        };
        if s.is_empty() {
            return bad("it is empty");
        }
        // Comfortably inside `NAME_MAX` once `.json` is appended, and far longer
        // than any id MPDFM mints.
        if s.len() > 128 {
            return bad("it is too long to be a file name");
        }
        if !s
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return bad("it may hold only letters, digits, `-` and `_`");
        }
        Ok(Self(s.to_owned()))
    }

    /// The id as it is spelled.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The file name of its record.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.json", self.0)
    }
}

impl std::fmt::Display for TxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<String> for TxId {
    type Error = JournalError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TxId> for String {
    fn from(value: TxId) -> Self {
        value.0
    }
}

/// Where a transaction got to.
///
/// The three terminal states are distinguished because the right thing to do
/// about them differs: a `complete` transaction can be undone, a `failed` or
/// `pending` one has to be recovered (rolled back or finished, task 12), and a
/// `reverted` one is already back where it started and must not be undone twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The record is durable and the work is either about to start, under way,
    /// or interrupted. Finding one of these at startup means a previous run died.
    Pending,
    /// Everything in the plan was done.
    Complete,
    /// It was done and has since been undone (task 12).
    Reverted,
    /// A step failed and the transaction stopped there. The steps marked done did
    /// happen; `recover` reverses them.
    Failed,
}

impl Status {
    /// Whether this transaction still needs attention — the two states that mean
    /// the library may be halfway through a change.
    #[must_use]
    pub fn is_unfinished(self) -> bool {
        matches!(self, Self::Pending | Self::Failed)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Pending => "pending",
            Self::Complete => "complete",
            Self::Reverted => "reverted",
            Self::Failed => "failed",
        };
        f.write_str(name)
    }
}

/// What a record's steps describe: work that was done, or work that was taken
/// back.
///
/// Every commit writes [`Direction::Forward`]. The record [`undo`][super::undo]
/// writes about *itself* is [`Direction::Reverse`], and undoing that one
/// re-applies the original change — which is why there is no separate `redo`
/// command. The direction is what tells undo which way to go, and it flips every
/// time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// The steps were executed. A commit, or an undo of an undo.
    #[default]
    Forward,
    /// The steps were reversed. An undo's own record.
    Reverse,
}

impl Direction {
    /// The other one: what undoing a record of this direction leaves behind.
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Forward => Self::Reverse,
            Self::Reverse => Self::Forward,
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Forward => "forward",
            Self::Reverse => "reverse",
        })
    }
}

/// One step of the plan, and whether it has happened.
///
/// `step` is the authority on what was asked for and `receipt` on what was done;
/// the step is not repeated inside the receipt, so the two cannot drift apart.
/// Rebuild what [`revert`][crate::ops::exec_fs::revert] wants with
/// [`StepRecord::receipt`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepRecord {
    /// The change this step makes.
    pub step: FsStep,
    /// Whether it has been executed. `false` with a `receipt` is impossible; a
    /// `true` without one is a record written by a broken build, and task 12
    /// refuses it rather than guessing.
    pub done: bool,
    /// What it did, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    /// Why it did not, for the one step that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether `receipt` was reconstructed by looking at the disk rather than
    /// written down by the step that ran.
    ///
    /// [`recover`][super::recover] does that for a step whose journal line a
    /// crash lost — a step whose destination is there and whose source is not
    /// is a step that ran, whatever the log says. Recorded because a
    /// reconstructed receipt knows less than a real one (no hash, and a method
    /// that is inferred rather than observed), and a rollback that relies on
    /// one should be able to say so.
    #[serde(default, skip_serializing_if = "not")]
    pub reconstructed: bool,
}

impl StepRecord {
    /// A step that has not run yet.
    #[must_use]
    pub fn planned(step: FsStep) -> Self {
        Self {
            step,
            done: false,
            receipt: None,
            error: None,
            reconstructed: false,
        }
    }

    /// The receipt in the shape [`revert`][crate::ops::exec_fs::revert] takes.
    #[must_use]
    pub fn receipt(&self) -> Option<StepReceipt> {
        self.receipt.as_ref().map(|receipt| StepReceipt {
            step: self.step.clone(),
            done: receipt.done.clone(),
            warnings: receipt.warnings.clone(),
        })
    }

    /// Record a completed step.
    pub fn completed(&mut self, receipt: StepReceipt) {
        self.done = true;
        self.receipt = Some(Receipt {
            done: receipt.done,
            warnings: receipt.warnings,
        });
        self.error = None;
        self.reconstructed = false;
    }

    /// Record a step that was found to have run, with a receipt worked out from
    /// the disk rather than observed — see [`StepRecord::reconstructed`].
    pub fn found_done(&mut self, receipt: StepReceipt) {
        self.completed(receipt);
        self.reconstructed = true;
    }

    /// Record a step that was found to have been *reversed* without being
    /// journaled, which leaves no receipt to keep: putting a reversed step back
    /// means executing it again, and executing a step needs nothing but the
    /// step.
    pub fn found_reversed(&mut self) {
        self.done = true;
        self.receipt = None;
        self.error = None;
        self.reconstructed = true;
    }

    /// Record a step that failed, with what the filesystem said.
    pub fn failed(&mut self, error: &dyn std::fmt::Display) {
        self.done = false;
        self.receipt = None;
        self.error = Some(error.to_string());
        self.reconstructed = false;
    }

    /// Record a step this transaction deliberately left alone, and why.
    ///
    /// `undo --force` is the one thing that does this: the step is not done —
    /// its work is still in effect — and the record says which it skipped and
    /// what made it skip them.
    pub fn skipped(&mut self, why: &dyn std::fmt::Display) {
        self.done = false;
        self.receipt = None;
        self.error = Some(format!("skipped: {why}"));
        self.reconstructed = false;
    }
}

/// One line of the append-only step log: what the step at `at` did.
///
/// A commit appends one of these after every step instead of rewriting the whole
/// record, because rewriting it is quadratic in the number of steps — see
/// [`store`][super::store]. It carries no [`FsStep`]: the record already lists
/// every step in order, and `at` indexes into it, so there is one copy of what was
/// asked for and the log says only what happened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepEntry {
    /// Which step, as an index into [`Record::steps`].
    pub at: usize,
    /// Whether it was executed.
    pub done: bool,
    /// What it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    /// Why it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether the receipt was reconstructed — see
    /// [`StepRecord::reconstructed`].
    #[serde(default, skip_serializing_if = "not")]
    pub reconstructed: bool,
}

impl StepEntry {
    /// The entry for the step at `at`, as it stands.
    #[must_use]
    pub fn of(at: usize, step: &StepRecord) -> Self {
        Self {
            at,
            done: step.done,
            receipt: step.receipt.clone(),
            error: step.error.clone(),
            reconstructed: step.reconstructed,
        }
    }

    /// Fold this entry into the record the log belongs to.
    ///
    /// An `at` past the end is ignored rather than raised: the only way to get one
    /// is a log that does not belong to this record, and refusing to read the
    /// record at all would be a worse answer than reading the part of it that is
    /// certainly true.
    pub fn apply_to(&self, record: &mut Record) {
        if let Some(step) = record.steps.get_mut(self.at) {
            step.done = self.done;
            step.receipt = self.receipt.clone();
            step.error = self.error.clone();
            step.reconstructed = self.reconstructed;
        }
    }
}

/// What a step did: a [`StepReceipt`] without the step, which
/// [`StepRecord::step`] already holds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Receipt {
    /// The outcome, in the shape its own reversal needs.
    pub done: Done,
    /// Anything worth telling the user that did not stop the step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<FsWarning>,
}

/// One transaction, as the journal keeps it.
///
/// Built by [`Record::opening`] before anything is touched, mutated in place as
/// the commit proceeds, and made durable by
/// [`Store::write`][super::store::Store::write] after every change.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// Its id, which is also its file name and its backup directory's name.
    pub txid: TxId,

    /// The record format. Refused on load when it is not [`VERSION`].
    pub version: u32,

    /// Where the transaction got to.
    pub status: Status,

    /// Whether [`Record::steps`] were executed or reversed.
    ///
    /// Defaults to [`Direction::Forward`], which every record a commit writes is
    /// — and which every record written before task 12 existed is too.
    #[serde(default)]
    pub direction: Direction,

    /// The transaction this one put back, when this record is
    /// [`undo`][super::undo]'s own rather than a commit's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_of: Option<TxId>,

    /// The transaction that put this one back, once one has. The explanation an
    /// [`Status::Reverted`] record owes the user who asks to undo it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone_by: Option<TxId>,

    /// When it started, as `2026-09-24T22:45:00Z`.
    pub started_at: String,

    /// When it reached a terminal status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,

    /// The library root every [`RelPath`][crate::paths::RelPath] in here is
    /// relative to. Recorded because a record outlives the configuration: undoing
    /// a transaction against a different `music_directory` would move files no
    /// one asked about.
    pub music_dir: Utf8PathBuf,

    /// The playlist directory, for the same reason.
    pub playlist_dir: Utf8PathBuf,

    /// This transaction's own directory, holding the playlist and state-file
    /// copies and the bytes of every deleted file.
    pub backup_dir: Utf8PathBuf,

    /// Whether retention has since removed [`Record::backup_dir`]. The record
    /// stays — it is a few kilobytes and it is the only explanation `undo` can
    /// give for refusing.
    #[serde(default)]
    pub backup_pruned: bool,

    /// What the user asked for.
    pub ops: Vec<Operation>,

    /// The expansion of those operations, in execution order, with per-step
    /// completion.
    pub steps: Vec<StepRecord>,

    /// The playlist lines this transaction changes. [`PlaylistEdit::file_name`]
    /// is the name of the copy in [`Record::backup_dir`].
    #[serde(default)]
    pub playlist_edits: Vec<PlaylistEdit>,

    /// The same for MPD's saved queue (task 14). Empty means the state file was
    /// not changed, whatever [`Record::state_backup`] says.
    #[serde(default)]
    pub state_edits: Vec<LineEdit>,

    /// The name of the state file's copy in [`Record::backup_dir`], when one was
    /// taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_backup: Option<String>,

    /// Whether a completed transaction asked MPD to rescan.
    #[serde(default)]
    pub mpd_update_requested: bool,

    /// Why that ask did not get through. A warning, never a failed transaction:
    /// the library and the playlists are already consistent by the time MPD is
    /// told about them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpd_update_failed: Option<String>,

    /// The directories MPD was asked to rescan.
    #[serde(default)]
    pub mpd_update_dirs: Vec<DirPath>,

    /// The counts the preview led with, kept so that `undo --list` can summarize
    /// a transaction without replaying it.
    #[serde(default)]
    pub summary: Summary,

    /// Every field this build does not know about, kept so that rewriting a
    /// record a newer MPDFM wrote does not strip it.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl Record {
    /// The record to write before anything is mutated: `pending`, with every
    /// step planned and none of them done.
    #[must_use]
    pub fn opening(
        txid: TxId,
        started: SystemTime,
        music_dir: Utf8PathBuf,
        playlist_dir: Utf8PathBuf,
        backup_dir: Utf8PathBuf,
    ) -> Self {
        Self {
            txid,
            version: VERSION,
            status: Status::Pending,
            direction: Direction::Forward,
            undo_of: None,
            undone_by: None,
            started_at: stamp(started),
            finished_at: None,
            music_dir,
            playlist_dir,
            backup_dir,
            backup_pruned: false,
            ops: Vec::new(),
            steps: Vec::new(),
            playlist_edits: Vec::new(),
            state_edits: Vec::new(),
            state_backup: None,
            mpd_update_requested: false,
            mpd_update_failed: None,
            mpd_update_dirs: Vec::new(),
            summary: Summary::default(),
            unknown: BTreeMap::new(),
        }
    }

    /// Give it a terminal status and the time it reached it.
    pub fn finish(&mut self, status: Status, when: SystemTime) {
        self.status = status;
        self.finished_at = Some(stamp(when));
    }

    /// The operations as a [`Plan`] again, for re-validation and for reporting.
    #[must_use]
    pub fn plan(&self) -> Plan {
        Plan::of(self.ops.clone())
    }

    /// Whether every step is marked done.
    #[must_use]
    pub fn all_done(&self) -> bool {
        self.steps.iter().all(|step| step.done)
    }

    /// The steps that happened, in the order they happened.
    pub fn completed(&self) -> impl Iterator<Item = &StepRecord> {
        self.steps.iter().filter(|step| step.done)
    }

    /// Whether [`undo`][crate::ops] could reverse this transaction: it finished,
    /// it has not already been reversed, and its backups are still there.
    #[must_use]
    pub fn is_undoable(&self) -> bool {
        self.status == Status::Complete && !self.backup_pruned
    }

    /// What the transaction did, in a few words: the counts the preview led
    /// with, which is enough to recognize it in a list without replaying it.
    #[must_use]
    pub fn summary_phrase(&self) -> String {
        let summary = &self.summary;
        let mut parts = Vec::new();
        if summary.files_moved > 0 {
            parts.push(format!("{} file(s) moved", summary.files_moved));
        }
        if summary.files_deleted > 0 {
            parts.push(format!("{} deleted", summary.files_deleted));
        }
        if summary.playlists_affected > 0 {
            parts.push(format!("{} playlist(s)", summary.playlists_affected));
        }
        if parts.is_empty() {
            parts.push("nothing".to_owned());
        }
        if let Some(of) = &self.undo_of {
            parts.push(format!(
                "{} {of}",
                match self.direction {
                    Direction::Reverse => "undo of",
                    Direction::Forward => "re-applied",
                }
            ));
        }
        parts.join(", ")
    }

    /// One line for `undo --list`: the id, when it started, and what it did.
    #[must_use]
    pub fn headline(&self) -> String {
        format!(
            "{}  {}  {}  {}",
            self.txid,
            self.started_at,
            self.status,
            self.summary_phrase()
        )
    }

    /// What is in the way of undoing this transaction, or `None` when nothing
    /// is.
    ///
    /// The phrase goes straight into `undo --list`'s last column and into the
    /// refusal [`undo`][super::undo] raises, so there is one explanation rather
    /// than two that can drift apart.
    #[must_use]
    pub fn why_not_undoable(&self) -> Option<String> {
        match self.status {
            Status::Complete if self.backup_pruned => {
                Some("its backups have been pruned".to_owned())
            }
            Status::Complete => None,
            Status::Reverted => Some(match &self.undone_by {
                Some(by) => format!("it was already undone by {by}"),
                None => "it has already been undone".to_owned(),
            }),
            Status::Pending | Status::Failed => {
                Some(format!("it is {}; run `mpdfm recover`", self.status))
            }
        }
    }
}

/// `!flag`, as a path `serde`'s `skip_serializing_if` can name.
fn not(flag: &bool) -> bool {
    !flag
}

// ---------------------------------------------------------------------------
// Time, without a dependency.

/// `2026-09-24T22:45:00Z` — a timestamp a person can read, in the one timezone
/// that does not need explaining.
#[must_use]
pub fn stamp(when: SystemTime) -> String {
    let (year, month, day, hour, minute, second) = civil(unix_secs(when));
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// `20260924T224500Z` — the same instant, in the form a file name can hold and a
/// byte-wise sort orders correctly.
#[must_use]
pub fn compact(when: SystemTime) -> String {
    let (year, month, day, hour, minute, second) = civil(unix_secs(when));
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Whole seconds since the epoch, negative before it.
fn unix_secs(when: SystemTime) -> i64 {
    match when.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(err) => {
            let before = err.duration();
            let secs = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
            // A remainder means the instant falls inside the previous whole
            // second, the same way `-0.5` floors to `-1`.
            if before.subsec_nanos() == 0 {
                -secs
            } else {
                -secs - 1
            }
        }
    }
}

/// Civil UTC date and time from a unix timestamp: Howard Hinnant's
/// `civil_from_days`, which is exact for every year this will ever see and needs
/// no table and no dependency.
fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };

    (
        year + i64::from(month <= 2),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
        u32::try_from(time / 3_600).unwrap_or(0),
        u32::try_from(time / 60 % 60).unwrap_or(0),
        u32::try_from(time % 60).unwrap_or(0),
    )
}

/// FNV-1a over a few numbers, for the id suffix. The same function
/// `testing::digest` uses, which is not a cryptographic hash and is not used as
/// one.
fn fnv(words: &[u64]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for word in words {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 2026-09-24T22:45:00Z, the instant the task file's example uses.
    const EXAMPLE: u64 = 1_790_289_900;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn a_timestamp_renders_both_ways_round_the_clock() {
        assert_eq!(stamp(at(EXAMPLE)), "2026-09-24T22:45:00Z");
        assert_eq!(compact(at(EXAMPLE)), "20260924T224500Z");
        assert_eq!(stamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // A leap day, and the last second of a year.
        assert_eq!(stamp(at(1_709_164_800)), "2024-02-29T00:00:00Z");
        assert_eq!(stamp(at(1_735_689_599)), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn a_time_before_the_epoch_does_not_wrap() {
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(stamp(before), "1969-12-31T23:59:59Z");
        // Half a second before the epoch is still in 1969.
        let half = UNIX_EPOCH - Duration::from_millis(500);
        assert_eq!(stamp(half), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn ids_minted_in_the_same_second_differ_and_sort_by_time() {
        let early = TxId::at(at(EXAMPLE));
        let same = TxId::at(at(EXAMPLE));
        let later = TxId::at(at(EXAMPLE + 1));

        assert_ne!(early, same, "the suffix is what keeps these apart");
        assert!(early.as_str().starts_with("20260924T224500Z-"));
        assert!(
            early < later && same < later,
            "a byte-wise sort has to order these by time: {early} {same} {later}"
        );
        assert!(
            early < same,
            "and by mint order inside one second, which is what retention relies \
             on: {early} {same}"
        );
        assert_eq!(early.file_name(), format!("{early}.json"));
    }

    #[test]
    fn an_id_that_could_escape_the_journal_directory_is_refused() {
        for bad in [
            "",
            "../../etc/passwd",
            "a/b",
            "tx id",
            "tx.json",
            &"x".repeat(129),
        ] {
            assert!(
                TxId::parse(bad).is_err(),
                "{bad:?} must not be accepted as a transaction id"
            );
        }
        assert!(TxId::parse("20260924T224500Z-a3f1").is_ok());
    }

    #[test]
    fn a_minted_id_parses_back() {
        let id = TxId::now();
        assert_eq!(TxId::parse(id.as_str()).expect("round-trips"), id);
    }
}
