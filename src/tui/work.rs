//! Everything that must not happen on the drawing thread.
//!
//! The rule the task sets is absolute: **the UI never blocks on I/O.** A scan of
//! the real library is tens of milliseconds warm and a good deal more cold; a
//! connect to an MPD that is behind a hung network is however long the timeout is.
//! Either one on the UI thread is a frozen screen, and a frozen screen is
//! indistinguishable from a crash.
//!
//! So each of them is a detached thread that ends by sending one [`Msg`]. The
//! shape is always the same, and worth stating once:
//!
//! - **the worker owns its inputs.** Paths and configuration are cloned into it.
//!   Nothing here borrows from [`App`][super::app::App], so nothing here can be
//!   holding a lock while the UI wants to draw — which is the pitfall the task
//!   names;
//! - **the worker reports a `Result`, not a verdict.** It does not know what a
//!   failure should look like on screen;
//! - **a failed send is the end of the job.** It means the loop has gone, and the
//!   worker's answer is of no interest to anybody.
//!
//! # The two that write
//!
//! [`commit`] and [`undo`] are the only workers that change anything, and they
//! are here for a stronger reason than responsiveness: a tag write copies the
//! whole original file into the transaction's backup before touching it
//! (`docs/tasks/17-tag-write.md`), so `W` on two hundred marked files is seconds
//! of I/O. On the drawing thread that is a frozen screen in the middle of the one
//! operation the user most wants to see finish.
//!
//! They take **clones** of the plan, the library and the effects the user agreed
//! to rather than borrowing the app's model, which is what lets the UI keep
//! drawing — and redrawing from a model nobody else is holding — while the files
//! are rewritten. Commit re-validates what it is given and refuses on
//! [`Drift`][mpdfm_core::ops::commit::Drift] if the answer has changed since, so
//! the clone cannot become a stale write.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use mpdfm_core::config::Config;
use mpdfm_core::journal::record::TxId;
use mpdfm_core::journal::store::Store;
use mpdfm_core::journal::undo;
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::mpd::{self, Mpd};
use mpdfm_core::ops::commit::{self, Previewed};
use mpdfm_core::ops::{Effects, Live, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::query::{self, Flow, Query};
use mpdfm_core::tags;

use super::log::Log;
use super::msg::{
    FoundOutcome, MpdSnapshot, MpdState, Msg, NotCommitted, Reads, ScanOutcome, TaskOutcome,
    TrackInfo,
};

/// How long MPD gets to answer the status poll.
///
/// Shorter than core's [`DEFAULT_TIMEOUT`][mpdfm_core::mpd::DEFAULT_TIMEOUT],
/// because this one runs every second and its
/// result is one character in the status bar. A daemon that cannot answer in
/// 250 ms is, for the purposes of that character, offline — and the next tick will
/// ask again.
const MPD_POLL_TIMEOUT: Duration = Duration::from_millis(250);

/// Walk the library and load the playlist index, reporting progress as it goes.
///
/// Sends [`Msg::Progress`] every few hundred files and exactly one
/// [`Msg::ScanDone`] at the end, whatever happened — or, when `cancel` was set
/// before the walk finished, exactly one [`Msg::ScanCancelled`] instead.
pub fn scan(
    tx: Sender<Msg>,
    music_dir: Utf8PathBuf,
    playlist_dir: Utf8PathBuf,
    cancel: Arc<AtomicBool>,
    log: Arc<Log>,
) {
    // Kept behind, because the thread takes the original: a scan that will not
    // start has to be answered too, or the app waits for a `ScanDone` forever.
    let fallback = tx.clone();
    let spawned = thread::Builder::new()
        .name("mpdfm-scan".to_owned())
        .spawn(move || {
            let started = Instant::now();
            log.line(format!("scan: walking {music_dir}"));

            // The closure runs on this thread, once per few hundred files, and
            // does nothing but forward. `scan_reporting` is synchronous; this is
            // the thread it is synchronous on.
            let progress_tx = tx.clone();
            let library = Library::scan_cancellable(
                &music_dir,
                &mut |progress| {
                    // A full channel is not possible (it is unbounded) and a
                    // closed one means the UI has gone; either way there is
                    // nothing to do but carry on and let the final send fail too.
                    let _ = progress_tx.send(Msg::Progress(progress.clone()));
                },
                &cancel,
            );

            let outcome = match library {
                Ok(None) => {
                    log.line("scan: called off");
                    let _ = tx.send(Msg::ScanCancelled);
                    return;
                }
                Ok(Some(library)) => {
                    log.line(format!(
                        "scan: {} files in {} dirs, {} warnings",
                        library.len(),
                        library.dir_count(),
                        library.warnings().len()
                    ));
                    let (index, playlist_warnings) = PlaylistIndex::load(&playlist_dir);
                    log.line(format!(
                        "scan: {} playlists, {} references, {} warnings",
                        index.len(),
                        index.reference_count(),
                        playlist_warnings.len()
                    ));
                    ScanOutcome {
                        library: Ok(library),
                        index: Some(index),
                        playlist_warnings,
                        elapsed: started.elapsed(),
                    }
                }
                Err(err) => {
                    // `{err:#}` is not available on a core error, which is not
                    // `anyhow`; its `Display` already carries the path.
                    log.line(format!("scan: failed: {err}"));
                    ScanOutcome {
                        library: Err(err.to_string()),
                        index: None,
                        playlist_warnings: Vec::new(),
                        elapsed: started.elapsed(),
                    }
                }
            };
            let _ = tx.send(Msg::ScanDone(Box::new(outcome)));
        });

    if let Err(err) = spawned {
        // A thread that will not spawn is not a reason to take the terminal down,
        // but the user has to be told that the library they are looking at is not
        // going to arrive.
        let _ = fallback.send(Msg::ScanDone(Box::new(ScanOutcome {
            library: Err(format!("cannot start the scan thread: {err}")),
            index: None,
            playlist_warnings: Vec::new(),
            elapsed: Duration::ZERO,
        })));
    }
}

/// Read the tags and audio properties of the rows the browser is showing.
///
/// The one worker whose cost is proportional to what is on screen rather than to
/// the library: the browser asks for the visible rows and no others
/// (`docs/tasks/22-browser-view.md`), so this is forty files at the very most and
/// usually a handful.
///
/// Exactly one [`Msg::TaskDone`] comes back, whatever happened, because the app
/// holds a "a read is out" flag and would otherwise never ask again. A file that
/// will not read is an `Err` in its own slot and not an end of the batch: the
/// other rows on screen still get their numbers.
pub fn read_tags(tx: Sender<Msg>, paths: Vec<RelPath>, root: Utf8PathBuf, log: Arc<Log>) {
    read(tx, paths, root, log, "tags", TaskOutcome::Tags);
}

/// Read every file the tag editor was opened on.
///
/// [`read_tags`] answers a different question: the bulk view has to see the
/// whole selection before it can say `<multiple>` honestly, so this is the one
/// read whose cost is the user's selection rather than the screen — a few
/// hundred files at the outside (`docs/tasks/23-tagedit-view.md`), which is why
/// it is a worker and not part of the keypress that opens the form.
pub fn read_selection(tx: Sender<Msg>, paths: Vec<RelPath>, root: Utf8PathBuf, log: Arc<Log>) {
    read(tx, paths, root, log, "selection", TaskOutcome::Selection);
}

/// The body both reads share: one thread, one message, whatever happened.
///
/// `wrap` is the only difference between them, and it is a function pointer
/// rather than two copies of the thread because the part that must not drift is
/// "exactly one answer comes back" — the app holds a flag or a half-open view
/// that a missing answer would strand.
fn read(
    tx: Sender<Msg>,
    paths: Vec<RelPath>,
    root: Utf8PathBuf,
    log: Arc<Log>,
    what: &'static str,
    wrap: fn(Reads) -> TaskOutcome,
) {
    let fallback = tx.clone();
    let spawned = thread::Builder::new()
        .name(format!("mpdfm-{what}"))
        .spawn(move || {
            let started = Instant::now();
            let reads = read_window(&paths, &root);
            if log.is_on() {
                let failed = reads.iter().filter(|(_, read)| read.is_err()).count();
                log.line(format!(
                    "{what}: read {} file(s) in {} µs, {failed} failed",
                    reads.len(),
                    started.elapsed().as_micros()
                ));
            }
            let _ = tx.send(Msg::TaskDone(Box::new(wrap(reads))));
        });

    if let Err(err) = spawned {
        let _ = fallback.send(Msg::TaskDone(Box::new(TaskOutcome::Failed {
            what: format!("read {what}"),
            message: format!("cannot start the tag thread: {err}"),
        })));
    }
}

/// The body of [`read_tags`], synchronously.
///
/// Separated so a test can count what one window costs — in
/// [`library::audio_reads`][mpdfm_core::library::audio_reads] and in wall-clock —
/// without a thread in the way.
#[must_use]
pub fn read_window(paths: &[RelPath], root: &Utf8Path) -> Reads {
    paths
        .iter()
        .map(|rel| {
            let read = tags::read(&rel.to_abs(root))
                .map(|(tags, info)| TrackInfo { tags, info })
                .map_err(|err| err.to_string());
            (rel.clone(), read)
        })
        .collect()
}

/// Walk the whole library for what a query matches.
///
/// The third worker whose cost is the library rather than the screen, and the
/// one the task makes an acceptance criterion of: `missing:genre` over 2 800
/// files opens every one of them, which is seconds of I/O, and the UI has to
/// keep drawing throughout.
///
/// Sends [`Msg::Finding`] as it goes and exactly one [`TaskOutcome::Found`] at
/// the end — including when it was called off, because what was found before
/// the stop is still a result the user asked for, and because the app holds a
/// "a search is running" state that a missing answer would strand.
///
/// `cancel` is shared and not sent, exactly as a commit's is: by the time a
/// message had been received the walk would be in the middle of a synchronous
/// pass over the library.
pub fn find(
    tx: Sender<Msg>,
    query: Query,
    library: Library,
    cancel: Arc<AtomicBool>,
    log: Arc<Log>,
) {
    let fallback = tx.clone();
    let spawned = thread::Builder::new()
        .name("mpdfm-find".to_owned())
        .spawn(move || {
            let started = Instant::now();
            log.line(format!("find: `{query}` over {} entries", library.len()));

            let progress_tx = tx.clone();
            let found = query::find(&query, &library, &mut |progress| {
                if cancel.load(Ordering::Relaxed) {
                    return Flow::Stop;
                }
                // A closed channel means the UI has gone; the next send fails
                // too and the thread ends either way.
                let _ = progress_tx.send(Msg::Finding(*progress));
                Flow::Go
            });

            log.line(format!(
                "find: `{query}` {} hit(s), {} read, {} failed, {} µs{}",
                found.hits.len(),
                found.read,
                found.failed.len(),
                started.elapsed().as_micros(),
                if found.cancelled { ", cancelled" } else { "" }
            ));
            let _ = tx.send(Msg::TaskDone(Box::new(TaskOutcome::Found(Box::new(
                FoundOutcome {
                    query: query.raw().to_owned(),
                    found,
                },
            )))));
        });

    if let Err(err) = spawned {
        let _ = fallback.send(Msg::TaskDone(Box::new(TaskOutcome::Failed {
            what: "find".to_owned(),
            message: format!("cannot start the search thread: {err}"),
        })));
    }
}

/// Ask MPD what it is doing, once.
///
/// `want_queue` also asks for the live queue, which the preview of a staged plan
/// needs and nothing else does: it decides whether a move *rewrites* MPD's saved
/// queue or warns that the daemon will have to be requeued
/// ([`Live`]). It is a parameter rather than always-on because it is the one part
/// of a poll whose cost is the length of the user's queue, and this runs every
/// second.
///
/// One connect, one `status`, one `currentsong`, then the socket is dropped. A
/// persistent connection would be fewer syscalls and one more thing to get wrong:
/// a daemon that is restarted under us needs no special case if the connection
/// never outlives the question. The filesystem is the source of truth
/// (`docs/PLAN.md` D6), so nothing about this is load-bearing — an MPD that never
/// answers costs one character in the status bar and nothing else.
pub fn poll_mpd(tx: Sender<Msg>, config: Config, want_queue: bool, log: Arc<Log>) {
    let fallback = tx.clone();
    let spawned = thread::Builder::new()
        .name("mpdfm-mpd".to_owned())
        .spawn(move || {
            let snapshot = match mpd::connect_if_enabled(&config, MPD_POLL_TIMEOUT, None) {
                Ok(None) => MpdSnapshot {
                    state: None,
                    enabled: false,
                    problem: Some("MPD is switched off for this run".to_owned()),
                    queue: None,
                },
                Ok(Some(mut mpd)) => match read_state(&mut mpd) {
                    Ok(state) => MpdSnapshot {
                        state: Some(state),
                        enabled: true,
                        problem: None,
                        // A queue that cannot be read is no queue: the plan is
                        // then previewed as though MPD were not running, which
                        // rewrites the saved queue on disk — the cautious half.
                        queue: want_queue.then(|| mpd.queue_paths().ok()).flatten(),
                    },
                    Err(err) => MpdSnapshot {
                        state: None,
                        enabled: true,
                        problem: Some(err.to_string()),
                        queue: None,
                    },
                },
                Err(err) => {
                    // Logged rather than toasted: an MPD that is not running is a
                    // normal state of the world, not an event.
                    if log.is_on() {
                        log.line(format!("mpd: {err}"));
                    }
                    MpdSnapshot {
                        state: None,
                        enabled: true,
                        problem: Some(err.to_string()),
                        queue: None,
                    }
                }
            };
            let _ = tx.send(Msg::MpdStatus(Box::new(snapshot)));
        });

    if let Err(err) = spawned {
        // Answered anyway, so that the app clears its "a poll is in flight" flag
        // and asks again on the next tick rather than showing a stale indicator
        // for the rest of the session.
        let _ = fallback.send(Msg::MpdStatus(Box::new(MpdSnapshot {
            state: None,
            enabled: true,
            problem: Some(format!("cannot start the MPD poll thread: {err}")),
            queue: None,
        })));
    }
}

/// The two commands the status bar needs, on a connection that is already open.
fn read_state(mpd: &mut mpd::Mpd) -> Result<MpdState, mpd::MpdError> {
    let status = mpd.status()?;
    // Only asked for when there is something to ask about: `currentsong` on a
    // stopped daemon is a round trip for an empty answer.
    let song = match status.state {
        mpd::PlayState::Stop => None,
        _ => mpd.current_uri()?,
    };
    Ok(MpdState {
        play_state: status.state,
        song,
        updating: status.updating_db.is_some(),
    })
}

/// Everything a commit needs, cloned out of the app.
///
/// A struct because there are seven of them and a call with seven positional
/// arguments is a call whose arguments can be swapped without anybody noticing.
/// All of it is **owned**: the worker borrows nothing from
/// [`App`][super::app::App], so the UI keeps drawing from a model nobody else
/// holds while the files are rewritten.
pub struct CommitJob {
    /// What the user staged.
    pub plan: Plan,
    /// The library the preview was made against.
    pub library: Library,
    /// The preview the user agreed to.
    pub effects: Effects,
    /// The resolved configuration.
    pub config: Config,
    /// MPD's live queue as the preview was given it, or `None` if the daemon
    /// did not answer. **The same value**, or commit refuses as drift — see
    /// [`MpdSnapshot::queue`][super::msg::MpdSnapshot::queue].
    pub queue: Option<Vec<RelPath>>,
    /// Set from the UI thread to call the commit off. Read once, at the one
    /// boundary where there is still nothing to put back; see
    /// [`commit::Options::cancel`].
    pub cancel: Arc<AtomicBool>,
    /// Where diagnostics go.
    pub log: Arc<Log>,
}

/// Commit a staged plan: write the files, rewrite the playlists, journal it all.
///
/// One [`TaskOutcome::Committed`] comes back whatever happened, and a
/// [`Msg::Committing`] every time the transaction advances — which is what the
/// pending view's progress indicator draws. Nothing is written when the plan is
/// refused: commit's own step 0 checks the conflicts it was handed before it
/// opens the journal, so a failure here is either "nothing happened and this is
/// why" or a partial transaction the record can be recovered from, and the
/// message says which.
pub fn commit(tx: Sender<Msg>, job: CommitJob) {
    let fallback = tx.clone();
    let spawned = thread::Builder::new()
        .name("mpdfm-commit".to_owned())
        .spawn(move || {
            let CommitJob {
                plan,
                library,
                effects,
                config,
                queue,
                cancel,
                log,
            } = job;
            let started = Instant::now();
            log.line(format!(
                "commit: {} operation(s), {} step(s)",
                plan.len(),
                effects.fs_steps.len()
            ));

            // MPD is told which directories changed: a tag write advances the
            // mtime, and the daemon's index is otherwise a version behind until
            // its next update of its own.
            let mpd = MpdLink::open(&config, &log);
            let update = |dirs: &[DirPath]| mpd.update(dirs);
            // Forwarded and not acted on: this runs on the committing thread,
            // and a progress callback that did anything slower than a `send`
            // would be a progress indicator that slowed the commit down.
            let watcher = tx.clone();
            let watch = |progress| {
                let _ = watcher.send(Msg::Committing(progress));
            };
            let stop = || cancel.load(Ordering::Relaxed);
            let options = commit::Options {
                update: mpd.connected().then_some(&update as commit::Updater<'_>),
                // The queue the preview was given, or the commit is drift.
                live: Live {
                    queue: queue.as_deref(),
                },
                progress: Some(&watch as commit::Watcher<'_>),
                cancel: Some(&stop as commit::Canceller<'_>),
                ..commit::Options::default()
            };
            let previewed = Previewed {
                plan: &plan,
                library: &library,
                effects: &effects,
            };

            let outcome = match commit::commit_with(&previewed, &config, &options) {
                Ok(committed) => {
                    log.line(format!(
                        "commit: {} in {} ms",
                        committed.txid,
                        started.elapsed().as_millis()
                    ));
                    Ok(Box::new(committed))
                }
                Err(mpdfm_core::Error::Commit(commit::CommitError::Cancelled)) => {
                    log.line("commit: cancelled before anything was changed");
                    Err(NotCommitted::Cancelled)
                }
                Err(err) => {
                    log.line(format!("commit: failed: {err}"));
                    Err(NotCommitted::Failed(err.to_string()))
                }
            };
            let _ = tx.send(Msg::TaskDone(Box::new(TaskOutcome::Committed(outcome))));
        });

    if let Err(err) = spawned {
        let _ = fallback.send(Msg::TaskDone(Box::new(TaskOutcome::Committed(Err(
            NotCommitted::Failed(format!("cannot start the commit thread: {err}")),
        )))));
    }
}

/// Reverse the most recent undoable transaction.
///
/// The newest first, skipping what cannot be undone — which is
/// [`undo::latest`]'s rule and `mpdfm undo` with no argument. A transaction that
/// is merely *blocked* because something has changed since comes back as an
/// error naming what changed; nothing is forced from here, because `--force`
/// skips steps and skipping a step is not a thing to do to somebody by accident.
///
/// `txid` names a transaction — `:undo <txid>`, and the `u` the pending view
/// offers with the id it has just committed — or `None` for the most recent
/// undoable one.
pub fn undo(tx: Sender<Msg>, config: Config, txid: Option<String>, log: Arc<Log>) {
    let fallback = tx.clone();
    let spawned = thread::Builder::new()
        .name("mpdfm-undo".to_owned())
        .spawn(move || {
            let outcome = reverse(&config, txid.as_deref(), &log);
            if let Err(err) = &outcome {
                log.line(format!("undo: {err}"));
            }
            let _ = tx.send(Msg::TaskDone(Box::new(TaskOutcome::Undone(outcome))));
        });

    if let Err(err) = spawned {
        let _ = fallback.send(Msg::TaskDone(Box::new(TaskOutcome::Undone(Err(format!(
            "cannot start the undo thread: {err}"
        ))))));
    }
}

/// The body of [`undo`], synchronously, with every error already rendered.
fn reverse(
    config: &Config,
    txid: Option<&str>,
    log: &Log,
) -> Result<Box<mpdfm_core::journal::Reversed>, String> {
    let store = Store::at(&config.data_dir);
    let record = match txid {
        // The same two doors `mpdfm undo [txid]` has, and the same errors: an id
        // that does not parse, a transaction that is not there, and one that
        // cannot be undone are all things to read rather than things to guess
        // past.
        Some(txid) => {
            let txid = TxId::parse(txid).map_err(|err| err.to_string())?;
            store.load(&txid).map_err(|err| err.to_string())?
        }
        None => undo::latest(&store).map_err(|err| err.to_string())?,
    };
    log.line(format!("undo: reversing {}", record.txid));

    let mpd = MpdLink::open(config, log);
    let update = |dirs: &[DirPath]| mpd.update(dirs);
    let options = undo::Options {
        force: false,
        update: mpd.connected().then_some(&update as commit::Updater<'_>),
    };
    undo::undo(&store, &record, config, &options)
        .map(Box::new)
        .map_err(|err| err.to_string())
}

/// The TUI's half of the MPD arrangement, for the one thing a commit needs from
/// the daemon: `update`.
///
/// Not [`cli::mpd::Link`][crate::cli::mpd::Link], which reports what went wrong
/// on stderr — here that is the alternate screen, and a warning printed over a
/// drawn frame is a corrupted frame. This logs instead.
///
/// It also deliberately does **not** read the queue. The preview was made with
/// [`Live::default`][mpdfm_core::ops::Live], commit re-validates with whatever it
/// is given, and a queue read here would make the two disagree — which is
/// [`Drift`][commit::Drift] and a refused commit, not extra information.
struct MpdLink {
    /// Behind a [`RefCell`] because core takes the updater as a `Fn`, and talking
    /// on a socket needs `&mut`. One connection, one use.
    client: Option<RefCell<Mpd>>,
}

impl MpdLink {
    /// Connect, or note why there is no connection. Never fails: an MPD that is
    /// not running is a normal state of the world and the filesystem is the
    /// source of truth (`docs/PLAN.md` D6).
    fn open(config: &Config, log: &Log) -> Self {
        let client = match mpd::connect_if_enabled(config, mpd::DEFAULT_TIMEOUT, None) {
            Ok(client) => client,
            Err(err) => {
                log.line(format!("mpd: {err}"));
                None
            }
        };
        Self {
            client: client.map(RefCell::new),
        }
    }

    /// Whether there is a daemon to tell about a commit.
    fn connected(&self) -> bool {
        self.client.is_some()
    }

    /// Ask MPD to rescan these directories.
    fn update(&self, dirs: &[DirPath]) -> Result<(), String> {
        let Some(client) = &self.client else {
            return Ok(());
        };
        client
            .borrow_mut()
            .update_dirs(dirs)
            .map(|_jobs| ())
            .map_err(|err| err.to_string())
    }
}
