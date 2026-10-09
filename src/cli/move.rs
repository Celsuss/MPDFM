//! `mpdfm move` — the milestone deliverable.
//!
//! Everything under this file is assembly: tasks 05, 07, 10, 11, 13 and 14 did
//! the work, and `move` is the sequence that puts them in front of a person and
//! asks. The sequence is the part worth reading:
//!
//! ```text
//! 1  resolve SRC and DST against music_dir
//! 2  scan, index, and ask MPD what it is holding
//! 3  validate — writes nothing
//! 4  print the FULL preview
//! 5  refused?    → exit 2, nothing written
//!    --dry-run?  → exit 0, nothing written
//!    --yes?      → commit
//!    otherwise   → ask; no → exit 3, nothing written
//! 6  commit, and print the txid
//! ```
//!
//! # The preview is the whole design
//!
//! Step 4 prints what [`Effects::render`] produced, in full, every time — not a
//! summary line, not the first ten rows. MPDFM's claim is that it will not break
//! your playlists, and the only evidence a user can act on is seeing which
//! playlist lines change before anything happens. A prompt that hid that would
//! be asking them to trust the program instead of to check it, which is the
//! opposite of the point (`docs/tasks/15-cli-move-and-doctor.md`, Pitfalls).
//!
//! # And the txid
//!
//! Step 6 prints the transaction id on every success, because `mpdfm undo
//! <txid>` has to be copy-pasteable. [`Committed::headline`] is that line.

use std::process::ExitCode;

use anyhow::Result;
use mpdfm_core::config::Config;
use mpdfm_core::library::Library;
use mpdfm_core::ops::commit::{self, CommitError, Previewed};
use mpdfm_core::ops::{Committed, Effects, Merge, Operation, Plan, Prefs};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::{Error, library::DirPath};

use super::{Cli, MoveArgs, mpd};
use crate::output::{self, Exit, Out, Style};

/// Plan a move, show it, and do it if the user agrees.
///
/// # Errors
///
/// [`RootProblem`][mpdfm_core::config::RootProblem] for a missing library root,
/// a [`PathError`][mpdfm_core::paths::PathError] for a `SRC` or `DST` that
/// cannot name something inside it, and anything
/// [`commit_with`][commit::commit_with] raises that is not a refusal — a refusal
/// comes back as [`Exit::Conflict`] with the reasons printed.
pub fn run(cli: &Cli, config: &Config, out: &Out, args: &MoveArgs) -> Result<ExitCode> {
    // Step 1 — the two paths, measured against the library root.
    let root = config.require_music_dir()?;
    let from = super::inside(&args.src, root)?;
    let to = super::inside(&args.dst, root)?;
    cli.trace(format!(
        "move {from} -> {to} (dry_run={} yes={} merge={} verify={})",
        args.dry_run, args.yes, args.merge, args.verify
    ));

    // Step 2 — the three inputs a preview is computed from.
    let library = Library::scan(root)?;
    let (index, index_warnings) = PlaylistIndex::load(&config.playlist_dir);
    for warning in &index_warnings {
        // Never routine: a playlist MPDFM could not read is a playlist it cannot
        // fix, so a move is about to leave every line in it stale.
        eprintln!("mpdfm: warning: {warning}");
    }
    let link = mpd::Link::open(cli, config);

    // Step 3 — what it would do. Nothing has been touched and nothing will be
    // until somebody says yes.
    let plan = Plan::of(vec![operation(&library, from, to)]);
    let prefs = Prefs {
        merge: if args.merge {
            Merge::Allow
        } else {
            Merge::Refuse
        },
    };
    let effects = plan.validate_with(&library, &index, config, &link.live(), prefs);

    // Step 4 and 5 — show it, and find out whether to go on.
    match decide(cli, out, args, &effects)? {
        Verdict::Stop(exit) => {
            if out.json {
                output::json(&report(&effects, None, exit))?;
            }
            return Ok(exit.into());
        }
        Verdict::Commit => {}
    }

    // Step 6 — the only part that writes.
    let update = |dirs: &[DirPath]| link.update(dirs);
    let options = commit::Options {
        verify: args.verify,
        inject: commit::Inject::Nothing,
        update: link.connected().then_some(&update as commit::Updater<'_>),
        // Must be the values the preview was given, or commit refuses as stale.
        live: link.live(),
        prefs,
        // Nothing to watch and nobody to ask: `mpdfm move` has already printed
        // the preview, and a progress line interleaved with it would be noise.
        // The pending view is where both of those are answered (task 24).
        ..commit::Options::default()
    };

    let previewed = Previewed {
        plan: &plan,
        library: &library,
        effects: &effects,
    };
    match commit::commit_with(&previewed, config, &options) {
        Ok(committed) => {
            if out.json {
                output::json(&report(&effects, Some(&committed), Exit::Ok))?;
            } else {
                print_committed(&committed, out);
            }
            Ok(Exit::Ok.into())
        }
        // A commit that was refused before it wrote anything is the same answer
        // as a refused preview, and gets the same exit code. The library is
        // exactly as it was, so this is worth retrying after a rescan rather
        // than a failure.
        Err(err) if refused_without_writing(&err) => {
            if out.json {
                output::json(&refusal(&effects, &err))?;
            } else {
                eprintln!("mpdfm: {err}");
            }
            Ok(Exit::Conflict.into())
        }
        Err(err) => Err(err.into()),
    }
}

/// Whether this is `move <file>` or `move <dir>`.
///
/// Decided by looking, not by a flag: the user named a thing, and whether that
/// thing is a directory is a fact about the library rather than a choice. A
/// source that is neither becomes a [`Operation::MoveFile`] so that the planner
/// reports [`Conflict::SourceMissing`][mpdfm_core::ops::Conflict::SourceMissing]
/// against it — which names the path, which is what the user needs to see.
fn operation(library: &Library, from: RelPath, to: RelPath) -> Operation {
    if library.dir(&DirPath::from(from.clone())).is_some() {
        Operation::MoveDir { from, to }
    } else {
        Operation::MoveFile { from, to }
    }
}

/// What to do after the preview has been rendered.
enum Verdict {
    /// Stop here with this status. Nothing has been written.
    Stop(Exit),
    /// Go ahead.
    Commit,
}

/// Print the preview and work out whether to commit.
///
/// The order of the three gates matters and is the order of the task's exit
/// codes: a refused plan is refused whatever `--yes` says, `--dry-run` stops
/// before the question is even asked, and only then does anybody get asked.
fn decide(cli: &Cli, out: &Out, args: &MoveArgs, effects: &Effects) -> Result<Verdict> {
    if !out.json {
        println!("{}", effects.render(out.width));
    }

    if !effects.conflicts.is_empty() {
        if !out.json {
            println!();
            println!("{}", out.paint(Style::Red, "Nothing was changed."));
        }
        return Ok(Verdict::Stop(Exit::Conflict));
    }

    if effects.fs_steps.is_empty() {
        if !out.json {
            println!();
            println!("There is nothing to do.");
        }
        return Ok(Verdict::Stop(Exit::Ok));
    }

    if args.dry_run {
        if !out.json {
            println!();
            println!(
                "{}",
                out.paint(Style::Dim, "--dry-run: nothing was changed.")
            );
        }
        return Ok(Verdict::Stop(Exit::Ok));
    }

    if args.yes {
        cli.trace("--yes: committing without asking");
        return Ok(Verdict::Commit);
    }

    println!();
    if out.confirm("Commit this?")? {
        Ok(Verdict::Commit)
    } else {
        println!("{}", out.paint(Style::Dim, "Nothing was changed."));
        Ok(Verdict::Stop(Exit::Declined))
    }
}

/// The lines after a successful commit: what happened, what to know, and how to
/// put it back.
fn print_committed(committed: &Committed, out: &Out) {
    println!();
    for warning in &committed.warnings {
        println!("  {} {warning}", out.paint(Style::Yellow, "!"));
    }
    println!("{}", out.paint(Style::Green, &committed.headline()));
    // Spelled out as a command rather than left for the user to assemble from
    // the headline: this is the line somebody reaches for when the move turns
    // out to have been a mistake, and it should be copy-pasteable.
    println!("Undo it with `mpdfm undo {}`.", committed.txid);
}

/// Whether this failure left the library exactly as it was.
///
/// Those are the ones that earn [`Exit::Conflict`] rather than
/// [`Exit::Error`] — a conflict re-validation found, a preview the disk has
/// moved on from, or a plan with nothing in it. Every other
/// [`CommitError`] stopped partway through and is a genuine failure with a
/// recovery to run.
fn refused_without_writing(err: &Error) -> bool {
    matches!(
        err,
        Error::Commit(
            CommitError::Refused { .. } | CommitError::Stale { .. } | CommitError::Nothing
        )
    )
}

// ---------------------------------------------------------------------------
// `--json`
// ---------------------------------------------------------------------------

/// The machine-readable form: the same [`Effects`] the preview rendered, plus
/// what was done with them.
///
/// `effects` is serialized by core, so the `--json` output and the text preview
/// cannot describe different plans — which is the reason [`Effects`] is one
/// value and not two renderings (see `ops::effects`).
fn report(effects: &Effects, committed: Option<&Committed>, exit: Exit) -> serde_json::Value {
    serde_json::json!({
        "effects": effects,
        "committed": committed.map(|committed| serde_json::json!({
            "txid": committed.txid.as_str(),
            "summary": committed.record.summary,
            "headline": committed.headline(),
            "warnings": committed.warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "pruned": committed.pruned.pruned.iter().map(|txid| txid.as_str()).collect::<Vec<_>>(),
        })),
        "exit": exit.code(),
    })
}

/// A commit that re-validation refused, with the reason the text output would
/// have printed.
fn refusal(effects: &Effects, err: &Error) -> serde_json::Value {
    let mut document = report(effects, None, Exit::Conflict);
    document["error"] = serde_json::Value::String(err.to_string());
    document
}
