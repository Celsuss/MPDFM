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

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use mpdfm_core::config::Config;
use mpdfm_core::library::Library;
use mpdfm_core::mpd::{self};
use mpdfm_core::playlist::PlaylistIndex;

use super::log::Log;
use super::msg::{MpdSnapshot, MpdState, Msg, ScanOutcome};

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
/// [`Msg::ScanDone`] at the end, whatever happened.
pub fn scan(tx: Sender<Msg>, music_dir: Utf8PathBuf, playlist_dir: Utf8PathBuf, log: Arc<Log>) {
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
            let library = Library::scan_reporting(&music_dir, &mut |progress| {
                // A full channel is not possible (it is unbounded) and a closed
                // one means the UI has gone; either way there is nothing to do
                // but carry on and let the final send fail too.
                let _ = progress_tx.send(Msg::Progress(progress.clone()));
            });

            let outcome = match library {
                Ok(library) => {
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

/// Ask MPD what it is doing, once.
///
/// One connect, one `status`, one `currentsong`, then the socket is dropped. A
/// persistent connection would be fewer syscalls and one more thing to get wrong:
/// a daemon that is restarted under us needs no special case if the connection
/// never outlives the question. The filesystem is the source of truth
/// (`docs/PLAN.md` D6), so nothing about this is load-bearing — an MPD that never
/// answers costs one character in the status bar and nothing else.
pub fn poll_mpd(tx: Sender<Msg>, config: Config, log: Arc<Log>) {
    let fallback = tx.clone();
    let spawned = thread::Builder::new()
        .name("mpdfm-mpd".to_owned())
        .spawn(move || {
            let snapshot = match mpd::connect_if_enabled(&config, MPD_POLL_TIMEOUT, None) {
                Ok(None) => MpdSnapshot {
                    state: None,
                    enabled: false,
                    problem: Some("MPD is switched off for this run".to_owned()),
                },
                Ok(Some(mut mpd)) => match read_state(&mut mpd) {
                    Ok(state) => MpdSnapshot {
                        state: Some(state),
                        enabled: true,
                        problem: None,
                    },
                    Err(err) => MpdSnapshot {
                        state: None,
                        enabled: true,
                        problem: Some(err.to_string()),
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
