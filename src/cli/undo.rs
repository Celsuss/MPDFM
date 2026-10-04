//! `mpdfm undo` and `mpdfm recover` — the other half of the promise.
//!
//! A move is only safe if it can be taken back, so these two commands are part
//! of the same deliverable as `move` and not a follow-up to it. The engines are
//! [`journal::undo`] and [`journal::recover`] (task 12); what is here is the
//! same shape `move` has — *look, then ask, then act* — over a journal record
//! instead of over a plan.
//!
//! | | |
//! |---|---|
//! | `undo --list` | every transaction, newest first, and whether it can be undone |
//! | `undo [TXID]` | reverse a transaction that finished (the newest undoable one by default) |
//! | `recover [TXID]` | deal with one a crash left halfway |
//!
//! # Why `undo` and `recover` are different commands
//!
//! `undo` reverses a transaction that **finished**: every step has a receipt
//! saying what it did, so putting it back is mechanical. `recover` is for one
//! that did **not** finish, where the first job is working out what actually
//! happened — and that answer comes from comparing the record against the disk,
//! not from the record alone. Folding them together would mean the command could
//! not tell the user which of the two situations they are in, and they are not
//! equally safe.
//!
//! # Both of them ask
//!
//! For the reason `move` does: these are the code paths that can lose data, and
//! [`Check::render`] / [`Survey::render`] exist so the user can see what is
//! about to be reversed. `--yes` skips the question, never the report.

use std::process::ExitCode;

use anyhow::{Context as _, Result};
use mpdfm_core::config::Config;
use mpdfm_core::journal::record::{Record, TxId};
use mpdfm_core::journal::store::Store;
use mpdfm_core::journal::{Reversed, recover, undo};
use mpdfm_core::library::DirPath;
use mpdfm_core::ops::commit::Updater;

use super::{Cli, RecoverArgs, UndoArgs, mpd};
use crate::output::{self, Exit, Out, Style};

/// `mpdfm undo [TXID] | undo --list`.
///
/// # Errors
///
/// [`JournalError`][mpdfm_core::journal::JournalError] if the journal cannot be
/// read, [`UndoError`][mpdfm_core::journal::UndoError] for a transaction that
/// cannot be undone at all, and anything [`undo::undo`] raises on the way back. A transaction that is merely
/// *blocked* by something that has changed since is not an error: it comes back
/// as [`Exit::Conflict`] with the report printed, and `--force` is the answer.
pub fn run(cli: &Cli, config: &Config, out: &Out, args: &UndoArgs) -> Result<ExitCode> {
    let store = Store::at(&config.data_dir);
    cli.trace(format!("journal at {}", store.journal_dir()));

    if args.list {
        return list(&store, out);
    }

    let record = match &args.txid {
        Some(txid) => {
            let txid = TxId::parse(txid)?;
            store.load(&txid)?
        }
        // Newest first, skipping what cannot be undone — an already-reverted
        // transaction, one whose backups were pruned, and one that never
        // finished, which is `recover`'s.
        None => undo::latest(&store)?,
    };
    cli.trace(format!("undoing {}", record.txid));

    // Look first. `undo::undo` checks again before it acts, which is deliberate
    // duplication: this call is what the user is shown, that one is what the
    // writes are gated on, and a file that changes in between must stop it.
    let check = undo::check(&record, config)?;
    if !out.json {
        println!("{}", check.render());
    }

    if !check.is_clear() && !args.force {
        if out.json {
            output::json(&serde_json::json!({
                "txid": record.txid.as_str(),
                "action": check.action.to_string(),
                "blocked": check.problems.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "exit": Exit::Conflict.code(),
            }))?;
        }
        return Ok(Exit::Conflict.into());
    }

    if !args.yes && !out.confirm(&format!("Undo {}?", record.txid))? {
        if !out.json {
            println!("{}", out.paint(Style::Dim, "Nothing was changed."));
        }
        return Ok(Exit::Declined.into());
    }

    let link = mpd::Link::open(cli, config);
    let update = |dirs: &[DirPath]| link.update(dirs);
    let options = undo::Options {
        force: args.force,
        update: link.connected().then_some(&update as Updater<'_>),
    };

    let reversed = undo::undo(&store, &record, config, &options)?;
    report_reversed(&reversed, out)?;
    Ok(Exit::Ok.into())
}

/// `mpdfm recover [TXID]`.
///
/// With no `TXID` it walks every transaction a previous run left `pending` or
/// `failed`, newest first — which is the list a front-end should be checking at
/// startup (`docs/PLAN.md` §5).
///
/// # Errors
///
/// As [`run`], plus [`RecoverError`][mpdfm_core::journal::recover::RecoverError]
/// for a transaction whose state cannot be told apart by looking.
pub fn recover(cli: &Cli, config: &Config, out: &Out, args: &RecoverArgs) -> Result<ExitCode> {
    let store = Store::at(&config.data_dir);

    let records: Vec<Record> = match &args.txid {
        Some(txid) => vec![store.load(&TxId::parse(txid)?)?],
        None => recover::pending(&store)?,
    };

    if records.is_empty() {
        if out.json {
            output::json(&serde_json::json!({ "pending": [], "recovered": [] }))?;
        } else {
            println!("Nothing to recover: no transaction was left unfinished.");
        }
        return Ok(Exit::Ok.into());
    }
    cli.trace(format!("{} unfinished transaction(s)", records.len()));

    let link = mpd::Link::open(cli, config);
    let update = |dirs: &[DirPath]| link.update(dirs);
    let options = undo::Options {
        force: args.force,
        update: link.connected().then_some(&update as Updater<'_>),
    };

    let mut outcomes = Vec::new();
    let mut exit = Exit::Ok;
    for record in &records {
        let survey = recover::survey(record, config)?;
        if !out.json {
            println!("{}", survey.render());
            println!();
        }

        if !survey.is_clear() && !args.force {
            outcomes.push(serde_json::json!({
                "txid": record.txid.as_str(),
                "blocked": survey.blocked.iter().map(ToString::to_string).collect::<Vec<_>>(),
            }));
            exit = Exit::Conflict;
            continue;
        }

        let question = if args.forward {
            format!("Finish {}?", record.txid)
        } else {
            format!("Roll {} back?", record.txid)
        };
        if !args.yes && !out.confirm(&question)? {
            if !out.json {
                println!("{}", out.paint(Style::Dim, "Nothing was changed."));
            }
            return Ok(Exit::Declined.into());
        }

        // Rolling back is the default and the safe one: finishing a transaction
        // the user never saw the preview of is the more surprising of the two,
        // so it needs `--forward` said out loud (task 12).
        if args.forward {
            let finished = recover::roll_forward(&store, record, config, &options)?;
            if !out.json {
                println!("{}", out.paint(Style::Green, &finished.headline()));
            }
            outcomes.push(serde_json::json!({
                "txid": finished.txid.as_str(),
                "action": "roll-forward",
                "steps": finished.steps,
                "headline": finished.headline(),
            }));
        } else {
            let reversed = recover::roll_back(&store, record, config, &options)?;
            if !out.json {
                println!("{}", out.paint(Style::Green, &reversed.headline()));
            }
            outcomes.push(serde_json::json!({
                "txid": reversed.txid.as_str(),
                "of": reversed.of.as_str(),
                "action": "roll-back",
                "steps": reversed.steps,
                "headline": reversed.headline(),
            }));
        }
    }

    if out.json {
        output::json(&serde_json::json!({
            "pending": records.iter().map(|record| record.txid.as_str()).collect::<Vec<_>>(),
            "recovered": outcomes,
            "exit": exit.code(),
        }))?;
    }
    Ok(exit.into())
}

/// `mpdfm undo --list`.
///
/// A record that cannot be read is listed as unreadable rather than hidden or
/// raised: one bad record must not make the other forty invisible.
fn list(store: &Store, out: &Out) -> Result<ExitCode> {
    let listing = undo::list(store).context("the journal could not be listed")?;

    if out.json {
        output::json(&serde_json::json!({
            "transactions": listing.rows.iter().map(|row| serde_json::json!({
                "txid": row.txid.as_str(),
                "started_at": row.started_at,
                "status": row.status.to_string(),
                "direction": row.direction.to_string(),
                "summary": row.summary,
                "undoable": row.undoable(),
                "why_not": row.why_not,
            })).collect::<Vec<_>>(),
            "unreadable": listing.unreadable,
        }))?;
    } else {
        // `Listing`'s own `Display` is the table, trailing newline included —
        // this listing is the whole of the command's output.
        print!("{listing}");
    }
    Ok(Exit::Ok.into())
}

/// What an undo did, and the fact that it is itself undoable.
fn report_reversed(reversed: &Reversed, out: &Out) -> Result<()> {
    if out.json {
        output::json(&serde_json::json!({
            "txid": reversed.txid.as_str(),
            "of": reversed.of.as_str(),
            "action": reversed.action.to_string(),
            "steps": reversed.steps,
            "skipped": reversed.skipped.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "warnings": reversed.warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "headline": reversed.headline(),
            "exit": Exit::Ok.code(),
        }))?;
        return Ok(());
    }

    println!();
    for warning in &reversed.warnings {
        println!("  {} {warning}", out.paint(Style::Yellow, "!"));
    }
    println!("{}", out.paint(Style::Green, &reversed.headline()));
    Ok(())
}
