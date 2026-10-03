//! The journal on disk: where records live, how they are made durable, and what
//! retention removes.
//!
//! ```text
//! ~/.local/share/mpdfm/journal/<txid>.json     one record per transaction
//! ~/.local/share/mpdfm/journal/<txid>.steps    what each step did, appended as it happens
//! ~/.local/share/mpdfm/backups/<txid>/         playlists, the state file, deleted bytes
//! ```
//!
//! # Durability, which is the whole point
//!
//! A record is written the same way every other file MPDFM writes is (safety
//! invariant 6, task 06's writer): a temp file beside it, `fsync` on the bytes,
//! `rename` into place, `fsync` on the directory so the *name* survives too.
//! Both syncs matter, and for different reasons: without the first the file can
//! come back empty after a power loss, and without the second it can come back
//! missing. A torn journal is worse than no journal, because `recover` would act
//! on it.
//!
//! Unlike task 06's writer, a failing `fsync` here is an **error**, not a
//! best-effort shrug. The record is what makes the mutations that follow it
//! recoverable; a commit whose journal might not be on disk has no business
//! moving a file.
//!
//! # Why there is a second file
//!
//! The commit has to make each completed step durable *before* it starts the next
//! one, and rewriting the whole record to do that is quadratic: the record grows
//! with every step, so a 1 000-step transaction rewrites half a megabyte a
//! thousand times. Measured on this machine, that is 4.1 s for 1 000 steps and
//! would be some 45 s for the 3 300 a whole-library reorganization produces —
//! nearly all of it the journal rather than the moves.
//!
//! So a completed step is **appended** to `<txid>.steps`, one JSON object per
//! line, and `fsync`ed: a few hundred bytes and one sync per step, flat in the
//! number of steps. The record itself is rewritten only at the boundaries where
//! something other than a step changed — `pending`, `complete`, `failed`. The two
//! are joined on the way back in: [`Store::load`] folds every line of the log into
//! the record it belongs to ([`StepEntry::apply_to`]), so a caller — `undo`,
//! `recover`, a test — sees one [`Record`] with its steps marked and never has to
//! know the log exists.
//!
//! A line is whole or it is not there: it is written with one `write` and an
//! `fsync` before the next step begins, and a torn tail from a power cut mid-write
//! fails to parse and is dropped. That is the right answer, because the step it
//! described had not been acknowledged either — `recover` treats it as not done,
//! which is the direction that is safe to be wrong in.
//!
//! # Retention
//!
//! [`Store::prune`] keeps the newest [`Config::backup_keep`][crate::config::Config::backup_keep]
//! transactions' backup directories and removes the older ones, leaving every
//! journal record in place — a record is small next to the files it describes, and
//! it is the only thing that can explain to the user *why* a two-year-old
//! transaction cannot be undone
//! ([`Record::backup_pruned`]). Two kinds of transaction are never pruned however
//! old they are: one that is still `pending` or `failed`, whose backups are what
//! `recover` would use, and one whose record this build cannot read, which it has
//! no business making decisions about.

use camino::{Utf8Path, Utf8PathBuf};

use super::JournalError;
use super::record::{Record, StepEntry, StepRecord, TxId, VERSION};

/// The journal and backup directories under MPDFM's data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    journal: Utf8PathBuf,
    backups: Utf8PathBuf,
}

impl Store {
    /// The store under `data_dir`, which is
    /// [`Config::data_dir`][crate::config::Config::data_dir]. Nothing is created
    /// until [`Store::create_dirs`] is called.
    #[must_use]
    pub fn at(data_dir: &Utf8Path) -> Self {
        Self {
            journal: data_dir.join("journal"),
            backups: data_dir.join("backups"),
        }
    }

    /// Where the records are.
    #[must_use]
    pub fn journal_dir(&self) -> &Utf8Path {
        &self.journal
    }

    /// Where the backup directories are.
    #[must_use]
    pub fn backups_dir(&self) -> &Utf8Path {
        &self.backups
    }

    /// One transaction's record.
    #[must_use]
    pub fn record_path(&self, txid: &TxId) -> Utf8PathBuf {
        self.journal.join(txid.file_name())
    }

    /// One transaction's append-only step log.
    #[must_use]
    pub fn steps_path(&self, txid: &TxId) -> Utf8PathBuf {
        self.journal.join(format!("{txid}.steps"))
    }

    /// One transaction's backup directory.
    #[must_use]
    pub fn backup_dir(&self, txid: &TxId) -> Utf8PathBuf {
        self.backups.join(txid.as_str())
    }

    /// Create both directories, and make their names durable.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] if either cannot be created.
    pub fn create_dirs(&self) -> Result<(), JournalError> {
        for dir in [&self.journal, &self.backups] {
            create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Create one transaction's backup directory, and `fsync` it.
    ///
    /// This is commit's step 2, before anything is written into it: a backup
    /// whose *directory* is not durable can be lost whole by a power cut, which
    /// would leave a `pending` record pointing at nothing.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] if it cannot be created or synced.
    pub fn create_backup_dir(&self, txid: &TxId) -> Result<Utf8PathBuf, JournalError> {
        let dir = self.backup_dir(txid);
        create_dir_all(&dir)?;
        sync_dir(&dir)?;
        Ok(dir)
    }

    /// Write a record: temp file, `fsync`, `rename`, `fsync` the directory.
    ///
    /// Overwrites the record of the same transaction, which is what updating one
    /// means. The replacement is atomic, so a reader never sees a half-written
    /// record and a crash leaves either the previous state or the new one.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] if the record cannot be serialized, written, synced or
    /// renamed into place.
    pub fn write(&self, record: &Record) -> Result<(), JournalError> {
        use std::io::Write as _;

        create_dir_all(&self.journal)?;
        let path = self.record_path(&record.txid);
        let bytes = serde_json::to_vec_pretty(record).map_err(|source| JournalError::Encode {
            path: path.clone(),
            source,
        })?;

        let temp = self.journal.join(format!(
            ".{}.mpdfm-{}.tmp",
            record.txid.file_name(),
            std::process::id()
        ));
        let write = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            // The bytes, before the name: a record that exists but is empty is
            // exactly what this ordering prevents. Noted under the record's own
            // name rather than the temp file's, because what was made durable is
            // this record.
            file.sync_all()?;
            synced(&path);
            drop(file);
            std::fs::rename(&temp, &path)
        })();
        if let Err(source) = write {
            // The error that says why comes first; a leftover temp file would be
            // its own bug report, so it goes regardless.
            let _ = std::fs::remove_file(&temp);
            return Err(JournalError::Io { path, source });
        }
        // And now the name.
        sync_dir(&self.journal)
    }

    /// Read one transaction's record.
    ///
    /// # Errors
    ///
    /// [`JournalError::Missing`] when there is no such record,
    /// [`JournalError::Malformed`] when it is not the JSON this understands,
    /// [`JournalError::Version`] when it was written by a different format
    /// version, and [`JournalError::Io`] if it cannot be read.
    pub fn load(&self, txid: &TxId) -> Result<Record, JournalError> {
        let path = self.record_path(txid);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(JournalError::Missing {
                    txid: txid.clone(),
                    path,
                });
            }
            Err(source) => return Err(JournalError::Io { path, source }),
        };

        // The version is read before the record is, so that a future format whose
        // *shape* this build cannot parse still produces "written by version 2,
        // this is version 1" rather than a complaint about a missing field.
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|source| JournalError::Malformed {
                path: path.clone(),
                source,
            })?;
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        match version {
            Some(found) if found == u64::from(VERSION) => {}
            Some(found) => {
                return Err(JournalError::Version {
                    path,
                    found: found.to_string(),
                    expected: VERSION,
                });
            }
            None => {
                return Err(JournalError::Version {
                    path,
                    found: "none".to_owned(),
                    expected: VERSION,
                });
            }
        }

        let mut record: Record = serde_json::from_value(value)
            .map_err(|source| JournalError::Malformed { path, source })?;
        for entry in self.read_steps(txid)? {
            entry.apply_to(&mut record);
        }
        Ok(record)
    }

    /// Append what one step did, and make that durable.
    ///
    /// This is what a commit calls after every step — see the [module docs][self]
    /// for why it is an append rather than another whole record. The file is
    /// created on the first call; its *name* is made durable then, so every call
    /// after it syncs only the bytes.
    ///
    /// # Errors
    ///
    /// [`JournalError::Encode`] if the entry cannot be serialized, and
    /// [`JournalError::Io`] if it cannot be appended or synced — either of which
    /// stops the commit, because the next step must not run until this one is on
    /// disk.
    pub fn append_step(
        &self,
        txid: &TxId,
        at: usize,
        step: &StepRecord,
    ) -> Result<(), JournalError> {
        use std::io::Write as _;

        let path = self.steps_path(txid);
        let entry = StepEntry::of(at, step);
        let mut line = serde_json::to_vec(&entry).map_err(|source| JournalError::Encode {
            path: path.clone(),
            source,
        })?;
        line.push(b'\n');

        let io = |source| JournalError::Io {
            path: path.clone(),
            source,
        };
        let existed = path.exists();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(io)?;
        // One `write_all` of one line, so a crash can only ever truncate the tail
        // of this entry — never interleave it with another.
        file.write_all(&line).map_err(io)?;
        file.sync_data().map_err(io)?;
        synced(&path);
        if !existed {
            sync_dir(&self.journal)?;
        }
        Ok(())
    }

    /// Every entry of one transaction's step log, in the order it was appended.
    ///
    /// A line that does not parse is dropped, which is only possible for the last
    /// one and only after a crash mid-append: the step it described had not been
    /// acknowledged, so treating it as not done is the safe direction. Anything
    /// before it was `fsync`ed before the next step began.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] if the log exists and cannot be read.
    pub fn read_steps(&self, txid: &TxId) -> Result<Vec<StepEntry>, JournalError> {
        let path = self.steps_path(txid);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(JournalError::Io { path, source }),
        };
        Ok(bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect())
    }

    /// Remove a transaction's step log, once its record holds everything the log
    /// said.
    ///
    /// Best effort, and deliberately so: it is called *after* the terminal record
    /// is durable, so a log left behind describes steps the record already lists
    /// and folding it in again changes nothing.
    pub fn forget_steps(&self, txid: &TxId) {
        let _ = std::fs::remove_file(self.steps_path(txid));
    }

    /// Every transaction id in the journal, **newest first**.
    ///
    /// A [`TxId`] sorts by time, so this is a reverse lexicographic sort of the
    /// directory — no record has to be opened to order the list. Anything in the
    /// directory that is not a record (a temp file, something a user dropped
    /// there) is ignored rather than reported: the journal is MPDFM's own
    /// directory and an unreadable entry in it is not the user's problem.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] if the directory exists but cannot be listed. A
    /// journal directory that does not exist yet is an empty list, not an error.
    pub fn list(&self) -> Result<Vec<TxId>, JournalError> {
        let entries = match std::fs::read_dir(&self.journal) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(JournalError::Io {
                    path: self.journal.clone(),
                    source,
                });
            }
        };

        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| JournalError::Io {
                path: self.journal.clone(),
                source,
            })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if let Ok(txid) = TxId::parse(stem) {
                ids.push(txid);
            }
        }
        ids.sort_unstable_by(|left, right| right.cmp(left));
        Ok(ids)
    }

    /// Every record, newest first, with the ones that could not be read reported
    /// alongside rather than hidden or raised.
    ///
    /// This is what `undo --list` and `recover` enumerate: one unreadable record
    /// must not make the other forty invisible.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] if the journal directory cannot be listed at all.
    pub fn records(&self) -> Result<(Vec<Record>, Vec<JournalError>), JournalError> {
        let mut records = Vec::new();
        let mut problems = Vec::new();
        for txid in self.list()? {
            match self.load(&txid) {
                Ok(record) => records.push(record),
                Err(err) => problems.push(err),
            }
        }
        Ok((records, problems))
    }

    /// The transactions that are still `pending` or `failed`, newest first — the
    /// ones a previous run left halfway through.
    ///
    /// # Errors
    ///
    /// As [`Store::records`].
    pub fn unfinished(&self) -> Result<Vec<Record>, JournalError> {
        let (records, _problems) = self.records()?;
        Ok(records
            .into_iter()
            .filter(|record| record.status.is_unfinished())
            .collect())
    }

    /// Keep the newest `keep` transactions' backup directories and remove the
    /// rest, leaving every journal record in place.
    ///
    /// See the [module docs][self] for what is never pruned and why.
    ///
    /// # Errors
    ///
    /// [`JournalError::Io`] if the journal directory cannot be listed. A backup
    /// directory that cannot be removed, or a record that cannot be marked, is
    /// reported in [`Pruned::kept`] instead: retention is housekeeping, and
    /// housekeeping does not fail a commit.
    pub fn prune(&self, keep: u32) -> Result<Pruned, JournalError> {
        let mut outcome = Pruned::default();
        let ids = self.list()?;
        for txid in ids.into_iter().skip(keep as usize) {
            let dir = self.backup_dir(&txid);
            let mut record = match self.load(&txid) {
                Ok(record) => record,
                // A record this build cannot read is a record it cannot reason
                // about, so its backups stay too.
                Err(err) => {
                    outcome.kept.push(Kept {
                        txid,
                        why: err.to_string(),
                    });
                    continue;
                }
            };
            if record.status.is_unfinished() {
                outcome.kept.push(Kept {
                    txid,
                    why: format!(
                        "it is still {}; `mpdfm recover` needs its backups",
                        record.status
                    ),
                });
                continue;
            }
            if record.backup_pruned && !dir.exists() {
                continue;
            }
            if dir.exists()
                && let Err(source) = std::fs::remove_dir_all(&dir)
            {
                outcome.kept.push(Kept {
                    txid,
                    why: format!("{dir}: {source}"),
                });
                continue;
            }
            record.backup_pruned = true;
            if let Err(err) = self.write(&record) {
                // The directory is already gone, so the record is now wrong. Say
                // so loudly rather than leaving the caller to find out from an
                // `undo` that cannot explain itself.
                outcome.kept.push(Kept {
                    txid: txid.clone(),
                    why: format!(
                        "its backups were removed but the record could not be marked: {err}"
                    ),
                });
                continue;
            }
            let _ = sync_dir(&self.backups);
            outcome.pruned.push(txid);
        }
        Ok(outcome)
    }
}

/// What [`Store::prune`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Transactions whose backup directory was removed. Their records remain,
    /// marked [`Record::backup_pruned`].
    pub pruned: Vec<TxId>,
    /// Transactions old enough to prune whose backups were kept anyway, with the
    /// reason. Worth reporting — one of them is a transaction that still needs
    /// recovering.
    pub kept: Vec<Kept>,
}

/// One transaction retention left alone, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    /// The transaction.
    pub txid: TxId,
    /// Why its backups are still there.
    pub why: String,
}

impl std::fmt::Display for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.txid, self.why)
    }
}

// ---------------------------------------------------------------------------

/// `mkdir -p`, naming the directory in the error.
fn create_dir_all(dir: &Utf8Path) -> Result<(), JournalError> {
    std::fs::create_dir_all(dir).map_err(|source| JournalError::Io {
        path: dir.to_owned(),
        source,
    })
}

/// Make a directory's contents — the names in it — durable.
///
/// An error here is returned rather than swallowed: this is what makes a renamed
/// journal record survive a power loss, and a commit that cannot promise that
/// must not proceed.
fn sync_dir(dir: &Utf8Path) -> Result<(), JournalError> {
    let handle = std::fs::File::open(dir).map_err(|source| JournalError::Io {
        path: dir.to_owned(),
        source,
    })?;
    handle.sync_all().map_err(|source| JournalError::Io {
        path: dir.to_owned(),
        source,
    })?;
    synced(dir);
    Ok(())
}

/// Note that `path` was `fsync`ed, for the tests that have to prove it was.
///
/// There is no portable way to observe an `fsync` from outside the process, and
/// "the journal is durable before the first mutation" is the invariant the whole
/// task exists for — so the one place that calls `sync_all` on behalf of the
/// journal records what it synced, under the same `testing` feature as the fixture
/// builder. Production builds do not carry it.
fn synced(path: &Utf8Path) {
    #[cfg(feature = "testing")]
    syncs::record(path);
    #[cfg(not(feature = "testing"))]
    let _ = path;
}

/// The record of what has been `fsync`ed, for tests.
#[cfg(feature = "testing")]
pub mod syncs {
    use std::sync::{Mutex, OnceLock};

    use camino::{Utf8Path, Utf8PathBuf};

    /// Appended to for the lifetime of the process. Deliberately never cleared:
    /// the tests in one binary run in parallel, and a log that one test could
    /// reset underneath another would be worse than useless. Every assertion is
    /// "this path was synced", and every path is inside that test's own temp
    /// directory.
    fn log() -> &'static Mutex<Vec<Utf8PathBuf>> {
        static LOG: OnceLock<Mutex<Vec<Utf8PathBuf>>> = OnceLock::new();
        LOG.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// Note one `fsync`.
    pub(super) fn record(path: &Utf8Path) {
        if let Ok(mut log) = log().lock() {
            log.push(path.to_owned());
        }
    }

    /// Whether the journal has `fsync`ed this exact path.
    #[must_use]
    pub fn was_synced(path: &Utf8Path) -> bool {
        log()
            .lock()
            .is_ok_and(|log| log.iter().any(|synced| synced == path))
    }

    /// Whether the journal has `fsync`ed anything inside this directory,
    /// including the directory itself.
    #[must_use]
    pub fn anything_synced_under(dir: &Utf8Path) -> bool {
        log()
            .lock()
            .is_ok_and(|log| log.iter().any(|synced| synced.starts_with(dir)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_the_one_the_plan_documents() {
        let store = Store::at(Utf8Path::new("/home/me/.local/share/mpdfm"));
        let txid = TxId::parse("20260924T224500Z-a3f1").expect("a valid id");

        assert_eq!(
            store.record_path(&txid),
            "/home/me/.local/share/mpdfm/journal/20260924T224500Z-a3f1.json"
        );
        assert_eq!(
            store.backup_dir(&txid),
            "/home/me/.local/share/mpdfm/backups/20260924T224500Z-a3f1"
        );
    }

    #[test]
    fn an_absent_journal_directory_lists_as_empty_rather_than_failing() {
        let store = Store::at(Utf8Path::new("/nonexistent/mpdfm-does-not-live-here"));
        assert_eq!(store.list().expect("not an error"), Vec::new());
    }

    #[test]
    fn a_status_knows_whether_it_still_needs_attention() {
        use super::super::record::Status;

        assert!(Status::Pending.is_unfinished());
        assert!(Status::Failed.is_unfinished());
        assert!(!Status::Complete.is_unfinished());
        assert!(!Status::Reverted.is_unfinished());
    }
}
