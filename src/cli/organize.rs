//! `mpdfm organize` — the bulk end of decision D3.
//!
//! Like `move` and `tag`, this file is assembly: task 27 decided where every
//! file goes, and tasks 10 and 11 already know how to show a plan and carry it
//! out. Organize is a [`Plan`] of [`Operation::MoveFile`]s like any other,
//! through the same `validate` → preview → `commit` — no special path, no
//! second execution engine, and `mpdfm undo` reverses it like a single move.
//!
//! ```text
//! 1  select: every audio file under each PATH (default: the whole library)
//! 2  read their tags; a file that will not read stays where it is
//! 3  map them through the template (strictly, with --only-missing)
//!    --limit N → keep the first N tracks that move, and map those again
//! 4  validate — writes nothing
//! 5  print the FULL preview, what blocks it, and the counts
//! 6  conflict?   → exit 2, nothing written
//!    --dry-run?  → exit 0, nothing written
//!    --yes?      → commit
//!    otherwise   → ask; no → exit 3, nothing written
//! 7  commit, and print the txid
//! 8  the to-do list: what could not be placed, by album directory
//! ```
//!
//! # The most dangerous command
//!
//! A whole-library organize can move every file and rewrite every playlist.
//! So the counts are printed last before the question, in bold, and the
//! question names the number of files — a prompt that says "Commit this?"
//! under two thousand lines of preview is a prompt nobody has read the top of.
//!
//! # Collisions block everything
//!
//! The mapping drops a colliding file from its moves rather than picking a
//! winner, so a plan built from it would commit cleanly and leave both behind
//! without a word. Here a [`Conflict`] in the mapping refuses the whole run,
//! with every source named — exactly as a conflict found by `validate` does.
//!
//! # The to-do list
//!
//! Printed last, after everything else, grouped by directory: an album missing
//! `genre` on fourteen tracks is one line to fix, not fourteen. It is the loop
//! the user will actually work through — tag, organize again, repeat.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use mpdfm_core::config::Config;
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::ops::commit::{self, Previewed};
use mpdfm_core::ops::{Committed, Effects, Plan, Prefs};
use mpdfm_core::organize::{self, Conflict, Mapping, NameRules, Options, Template, Warning};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::{self, TagSet};

use super::r#move::{print_committed, refused_without_writing};
use super::{Cli, OrganizeArgs, mpd, tag};
use crate::output::{self, Exit, Out, Style};

/// Re-file the selection by a template, show it, and do it if the user agrees.
///
/// # Errors
///
/// [`RootProblem`][mpdfm_core::config::RootProblem] for a missing library root,
/// a [`PathError`][mpdfm_core::paths::PathError] for a `PATH` that cannot name
/// something inside it, a template that does not parse, and anything
/// [`commit_with`][commit::commit_with] raises that is not a refusal.
pub fn run(cli: &Cli, config: &Config, out: &Out, args: &OrganizeArgs) -> Result<ExitCode> {
    // The template first: a typo in it is the cheapest thing to be told about,
    // and should not cost a tag read of the whole library.
    let template = template(config, args)?;
    let root = config.require_music_dir()?;
    cli.trace(format!(
        "organize by {:?} (dry_run={} yes={} only_missing={} no_aux={} limit={:?})",
        template.as_str(),
        args.dry_run,
        args.yes,
        args.only_missing,
        args.no_aux,
        args.limit
    ));

    // Step 1 — which files.
    let library = Library::scan(root)?;
    let files = select(cli, &library, args, root)?;

    // Step 2 — their tags. Unlike `tag set`, one unreadable file does not
    // refuse the run: it is not being written, only left where it is, and the
    // to-do list says why.
    let mut tracks: BTreeMap<RelPath, TagSet> = BTreeMap::new();
    let mut unreadable: Vec<(RelPath, String)> = Vec::new();
    for (rel, read) in tags::read_many(&files, root) {
        match read {
            Ok(tags) => {
                tracks.insert(rel, tags);
            }
            Err(err) => unreadable.push((rel, err.to_string())),
        }
    }
    cli.trace(format!(
        "organize: {} file(s) selected, {} unreadable",
        files.len(),
        unreadable.len()
    ));

    // Step 3 — where everything goes.
    let options = Options {
        rules: NameRules::from_config(config),
        ..Options::default()
    };
    let mapper = if args.only_missing {
        template.strict()
    } else {
        template.clone()
    };
    let full = organize::map(&mapper, &library, &tracks, &options);
    let staged = match args.limit {
        Some(limit) if limit.get() < full.tracks_moving() => {
            // Mapped again on just those tracks, rather than cut out of the full
            // mapping: an album the limit cuts in half is a split album, whose
            // aux files stay, and only a mapping of the smaller selection knows
            // that.
            let keep: BTreeSet<&RelPath> = full
                .moves
                .iter()
                .filter(|m| !m.aux)
                .take(limit.get())
                .map(|m| &m.from)
                .collect();
            let subset: BTreeMap<RelPath, TagSet> = tracks
                .iter()
                .filter(|(rel, _)| keep.contains(rel))
                .map(|(rel, tags)| (rel.clone(), tags.clone()))
                .collect();
            Some(organize::map(&mapper, &library, &subset, &options))
        }
        _ => None,
    };
    let mapping = staged.as_ref().unwrap_or(&full);

    // Step 4 — what it would do.
    let plan = Plan::of(mapping.operations(!args.no_aux));
    let (index, index_warnings) = PlaylistIndex::load(&config.playlist_dir);
    for warning in &index_warnings {
        eprintln!("mpdfm: warning: {warning}");
    }
    let link = mpd::Link::open(cli, config);
    let prefs = Prefs::default();
    let effects = plan.validate_with(&library, &index, config, &link.live(), prefs);

    let summary = Summary::of(&full, mapping, &unreadable, args);
    let todo = todo(&full, &unreadable);

    // Steps 5 and 6.
    match decide(cli, out, args, &template, mapping, &effects, &summary)? {
        Verdict::Stop(exit) => {
            if out.json {
                output::json(&report(
                    &template, &summary, mapping, &todo, &effects, None, exit,
                ))?;
            } else {
                print_todo(&todo, out);
            }
            return Ok(exit.into());
        }
        Verdict::Commit => {}
    }

    // Step 7 — the only part that writes.
    let update = |dirs: &[DirPath]| link.update(dirs);
    let options = commit::Options {
        verify: args.verify,
        update: link.connected().then_some(&update as commit::Updater<'_>),
        // Must be what the preview was given, or commit refuses as stale.
        live: link.live(),
        prefs,
        ..commit::Options::default()
    };
    let previewed = Previewed {
        plan: &plan,
        library: &library,
        effects: &effects,
    };
    let started = std::time::Instant::now();
    match commit::commit_with(&previewed, config, &options) {
        Ok(committed) => {
            cli.trace(format!(
                "organize: committed {} op(s) in {} ms",
                plan.len(),
                started.elapsed().as_millis()
            ));
            if out.json {
                output::json(&report(
                    &template,
                    &summary,
                    mapping,
                    &todo,
                    &effects,
                    Some(&committed),
                    Exit::Ok,
                ))?;
            } else {
                print_committed(&committed, out);
                print_todo(&todo, out);
            }
            Ok(Exit::Ok.into())
        }
        Err(err) if refused_without_writing(&err) => {
            if out.json {
                let mut document = report(
                    &template,
                    &summary,
                    mapping,
                    &todo,
                    &effects,
                    None,
                    Exit::Conflict,
                );
                document["error"] = serde_json::Value::String(err.to_string());
                output::json(&document)?;
            } else {
                eprintln!("mpdfm: {err}");
            }
            Ok(Exit::Conflict.into())
        }
        Err(err) => Err(err.into()),
    }
}

/// `--template`, or the configured `organize_template`, parsed.
///
/// The error says which of the two it was, because a bad template in
/// `config.toml` is fixed somewhere other than the command line.
fn template(config: &Config, args: &OrganizeArgs) -> Result<Template> {
    match &args.template {
        Some(source) => Template::parse(source).context("--template does not parse"),
        None => Template::parse(&config.organize_template).with_context(|| {
            format!(
                "organize_template from {} does not parse; pass --template to override it",
                config.sources.organize_template
            )
        }),
    }
}

/// Every audio file under each `PATH`, or under the whole library.
///
/// Always recursive: organizing `hiphop/` means everything filed under it, and
/// a flag to say so would only be a way to get it wrong.
fn select(
    cli: &Cli,
    library: &Library,
    args: &OrganizeArgs,
    root: &camino::Utf8Path,
) -> Result<Vec<RelPath>> {
    if !args.paths.is_empty() {
        return tag::select(cli, library, &args.paths, true, root);
    }
    let mut all = BTreeSet::new();
    tag::collect(library, &DirPath::root(), true, &mut all);
    anyhow::ensure!(
        !all.is_empty(),
        "the library has no audio files to organize"
    );
    Ok(all.into_iter().collect())
}

// ---------------------------------------------------------------------------
// The numbers.

/// What the run found, counted — the line the user should read before saying
/// yes.
#[derive(Debug, Clone, Copy)]
struct Summary {
    /// Tracks the template places somewhere they are not.
    placeable: usize,
    /// Of those, how many are in this plan (all of them, unless `--limit`).
    staged: usize,
    /// Aux files travelling with their albums in this plan.
    aux: usize,
    /// Tracks already where the template puts them.
    in_place: usize,
    /// Tracks the template cannot place, unreadable ones included.
    unplaceable: usize,
    /// Files named by a collision or an occupied destination.
    conflicting: usize,
}

impl Summary {
    fn of(
        full: &Mapping,
        staged: &Mapping,
        unreadable: &[(RelPath, String)],
        args: &OrganizeArgs,
    ) -> Self {
        Self {
            placeable: full.tracks_moving(),
            staged: staged.tracks_moving(),
            aux: if args.no_aux {
                0
            } else {
                staged.moves.len() - staged.tracks_moving()
            },
            in_place: full.in_place.len(),
            unplaceable: full.unplaceable().count() + unreadable.len(),
            conflicting: conflicting(staged),
        }
    }

    fn files(&self) -> usize {
        self.staged + self.aux
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "placeable": self.placeable,
            "staged": self.staged,
            "aux": self.aux,
            "in_place": self.in_place,
            "unplaceable": self.unplaceable,
            "conflicting": self.conflicting,
        })
    }
}

/// How many distinct files the mapping's conflicts name.
fn conflicting(mapping: &Mapping) -> usize {
    let mut named: BTreeSet<&RelPath> = BTreeSet::new();
    for conflict in &mapping.conflicts {
        match conflict {
            Conflict::Collision { sources, .. } => named.extend(sources),
            Conflict::Occupied { from, .. } => {
                named.insert(from);
            }
        }
    }
    named.len()
}

// ---------------------------------------------------------------------------
// Steps 5 and 6.

/// What to do after the preview has been printed.
enum Verdict {
    /// Stop here with this status. Nothing has been written.
    Stop(Exit),
    /// Go ahead.
    Commit,
}

/// Print the preview, what is wrong with it and the counts, and work out
/// whether to commit. The gates are `move`'s, in `move`'s order, with the
/// mapping's own conflicts in front of the planner's.
fn decide(
    cli: &Cli,
    out: &Out,
    args: &OrganizeArgs,
    template: &Template,
    mapping: &Mapping,
    effects: &Effects,
    summary: &Summary,
) -> Result<Verdict> {
    if !out.json {
        if !effects.fs_steps.is_empty() || !effects.conflicts.is_empty() {
            println!("{}", effects.render(out.width));
            println!();
        }
        print_warnings(mapping, out);
        print_conflicts(mapping, out);
        print_summary(template, summary, args, out);
    }

    if !mapping.is_committable() || !effects.conflicts.is_empty() {
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
    let files = summary.files();
    let plural = if files == 1 { "" } else { "s" };
    if out.confirm(&format!("Move {files} file{plural}?"))? {
        Ok(Verdict::Commit)
    } else {
        println!("{}", out.paint(Style::Dim, "Nothing was changed."));
        Ok(Verdict::Stop(Exit::Declined))
    }
}

/// Split albums and case-only differences: nothing that blocks, everything
/// worth reading first.
fn print_warnings(mapping: &Mapping, out: &Out) {
    let mut any = false;
    for warning in &mapping.warnings {
        if matches!(warning, Warning::Unplaceable { .. }) {
            // The to-do list, at the end.
            continue;
        }
        println!("  {} {warning}", out.paint(Style::Yellow, "!"));
        any = true;
    }
    if any {
        println!();
    }
}

/// Every collision, with every source — the acceptance criterion's "listed
/// with both sources".
fn print_conflicts(mapping: &Mapping, out: &Out) {
    if mapping.conflicts.is_empty() {
        return;
    }
    let count = mapping.conflicts.len();
    println!(
        "{}",
        out.paint(
            Style::Red,
            &format!(
                "{count} destination{} taken twice — nothing will be moved until {} fixed:",
                if count == 1 { " is" } else { "s are" },
                if count == 1 { "it is" } else { "they are" },
            )
        )
    );
    for conflict in &mapping.conflicts {
        match conflict {
            Conflict::Collision { to, sources } => {
                println!("  {} {to}", out.paint(Style::Red, "x"));
                for source in sources {
                    println!("      ← {source}");
                }
            }
            Conflict::Occupied { .. } => {
                println!("  {} {conflict}", out.paint(Style::Red, "x"));
            }
        }
    }
    println!();
}

/// The counts, in bold, right above the question.
fn print_summary(template: &Template, summary: &Summary, args: &OrganizeArgs, out: &Out) {
    println!(
        "{}",
        out.paint(Style::Bold, &format!("Organize by {}", template.as_str()))
    );
    let limited = if summary.staged < summary.placeable {
        format!(" ({} staged by --limit)", summary.staged)
    } else {
        String::new()
    };
    let aux = if args.no_aux {
        " (aux files stay: --no-aux)".to_owned()
    } else {
        format!(" + {} aux file(s)", summary.aux)
    };
    println!(
        "  {}",
        out.paint(
            Style::Bold,
            &format!("{} track(s) to move{limited}{aux}", summary.placeable)
        )
    );
    println!("  {} already in place", summary.in_place);
    println!(
        "  {} cannot be placed{}",
        summary.unplaceable,
        if summary.unplaceable > 0 {
            " — listed at the end"
        } else {
            ""
        }
    );
    if summary.conflicting > 0 {
        println!(
            "  {}",
            out.paint(
                Style::Red,
                &format!("{} in a collision", summary.conflicting)
            )
        );
    }
}

// ---------------------------------------------------------------------------
// Step 8 — the to-do list.

/// One line of the to-do list: an album directory, what is wrong with some of
/// its tracks, and how many.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Todo {
    dir: String,
    reason: String,
    files: Vec<String>,
}

/// The unplaceable tracks, grouped by directory and reason.
fn todo(mapping: &Mapping, unreadable: &[(RelPath, String)]) -> Vec<Todo> {
    let mut grouped: BTreeMap<(DirPath, String), Vec<String>> = BTreeMap::new();
    for (path, reason) in mapping.unplaceable() {
        grouped
            .entry((DirPath::of(path), reason.to_string()))
            .or_default()
            .push(path.file_name().to_owned());
    }
    for (path, err) in unreadable {
        grouped
            .entry((DirPath::of(path), format!("tags could not be read: {err}")))
            .or_default()
            .push(path.file_name().to_owned());
    }
    grouped
        .into_iter()
        .map(|((dir, reason), files)| Todo {
            dir: dir.to_string(),
            reason,
            files,
        })
        .collect()
}

fn print_todo(todo: &[Todo], out: &Out) {
    if todo.is_empty() {
        return;
    }
    let files: usize = todo.iter().map(|t| t.files.len()).sum();
    let dirs: BTreeSet<&str> = todo.iter().map(|t| t.dir.as_str()).collect();
    println!();
    println!(
        "{}",
        out.paint(
            Style::Bold,
            &format!(
                "To tag before they can be organized — {files} file(s) in {} director{}, left where they are:",
                dirs.len(),
                if dirs.len() == 1 { "y" } else { "ies" }
            )
        )
    );
    for item in todo {
        let dir = if item.dir.is_empty() { "." } else { &item.dir };
        println!(
            "  {dir}/  {}",
            out.paint(
                Style::Yellow,
                &format!("{} file(s): {}", item.files.len(), item.reason)
            )
        );
    }
}

// ---------------------------------------------------------------------------
// `--json`

/// The machine-readable form: what `decide` printed, as data, plus what was
/// done with it.
fn report(
    template: &Template,
    summary: &Summary,
    mapping: &Mapping,
    todo: &[Todo],
    effects: &Effects,
    committed: Option<&Committed>,
    exit: Exit,
) -> serde_json::Value {
    serde_json::json!({
        "template": template.as_str(),
        "counts": summary.json(),
        "conflicts": mapping.conflicts.iter().map(|c| match c {
            Conflict::Collision { to, sources } => serde_json::json!({
                "kind": "collision",
                "to": to.as_str(),
                "sources": sources.iter().map(RelPath::as_str).collect::<Vec<_>>(),
            }),
            Conflict::Occupied { from, to, existing } => serde_json::json!({
                "kind": "occupied",
                "from": from.as_str(),
                "to": to.as_str(),
                "existing": existing,
            }),
        }).collect::<Vec<_>>(),
        "warnings": mapping.warnings.iter()
            .filter(|w| !matches!(w, Warning::Unplaceable { .. }))
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "unplaceable": todo.iter().map(|t| serde_json::json!({
            "dir": t.dir,
            "reason": t.reason,
            "files": t.files,
        })).collect::<Vec<_>>(),
        "effects": effects,
        "committed": committed.map(|committed| serde_json::json!({
            "txid": committed.txid.as_str(),
            "summary": committed.record.summary,
            "headline": committed.headline(),
            "warnings": committed.warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
        })),
        "exit": exit.code(),
    })
}
