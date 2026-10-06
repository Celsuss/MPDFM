//! `mpdfm tag show`, `mpdfm tag set` and `mpdfm tag diff` — the M2 deliverable.
//!
//! Like `move`, this file is assembly: tasks 16, 17 and 18 did the work and this
//! is the sequence that puts it in front of a person and asks.
//!
//! ```text
//! 1  resolve each PATH against music_dir, and expand a directory to its tracks
//! 2  read every one of their tags
//! 3  work out the edits — flags and named actions, through the bulk view
//! 4  validate — writes nothing
//! 5  print the FULL preview
//! 6  refused?    → exit 2, nothing written
//!    --dry-run?  → exit 0, nothing written
//!    --yes?      → commit
//!    otherwise   → ask; no → exit 3, nothing written
//! 7  commit, and print the txid
//! ```
//!
//! # All or nothing
//!
//! The task's pitfall asks the question outright, and the answer is the same as
//! everywhere else in MPDFM: **the whole batch is refused if any file in it
//! cannot be written.** One read-only track in a 400-file album means no file is
//! touched, not 213 of them.
//!
//! It comes out of the existing machinery rather than being bolted on here. Every
//! file becomes an [`Operation::WriteTags`], the plan is validated as one, and a
//! [`Conflict`][mpdfm_core::ops::Conflict] anywhere in it makes
//! `Effects::is_committable` false — the same rule that refuses a move whose
//! destination is occupied. A file whose tags cannot even be *read* is refused
//! here, before a plan exists, for the same reason: a bulk view built from nine
//! of ten files would be a bulk view of a selection the user did not make.
//!
//! # `diff` is `set` without the writing
//!
//! Same arguments, and it prints the per-file before-and-after that the preview's
//! field-level summary deliberately leaves out. `set --dry-run` stops at the same
//! point; `diff` is the spelling for when what you want is to look.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use anyhow::Result;
use mpdfm_core::config::Config;
use mpdfm_core::library::{DirPath, Library};
use mpdfm_core::ops::commit::{self, CommitError, Previewed};
use mpdfm_core::ops::{Committed, Effects, Operation, Plan};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::{self, BulkView, Edit, Field, TagDelta, TagSet, Values};
use mpdfm_core::{Error, library::Kind};

use super::{Cli, TagFields, TagSetArgs, TagShowArgs};
use crate::output::{self, Exit, Out, Style};

/// `mpdfm tag show` — what is in these files right now.
///
/// # Errors
///
/// [`RootProblem`][mpdfm_core::config::RootProblem] for a missing library root, a
/// [`PathError`][mpdfm_core::paths::PathError] for a `PATH` that cannot name
/// something inside it, and [`TagError`][mpdfm_core::tags::TagError] for a file
/// whose tags cannot be read — `show` reports those per file and carries on, so
/// this is only for a failure that stops the command.
pub fn show(cli: &Cli, config: &Config, out: &Out, args: &TagShowArgs) -> Result<ExitCode> {
    let root = config.require_music_dir()?;
    let library = Library::scan(root)?;
    let files = select(cli, &library, &args.paths, args.recursive, root)?;
    cli.trace(format!("tag show: {} file(s)", files.len()));

    let read = tags::read_many(&files, root);
    if out.json {
        output::json(&show_json(root, &read))?;
        return Ok(Exit::Ok.into());
    }

    let mut failed = 0;
    for (index, (rel, result)) in read.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("{}", out.paint(Style::Bold, rel.as_str()));
        match result {
            Ok(tags) => print_tags(root, rel, tags, out),
            Err(err) => {
                failed += 1;
                println!("  {} {err}", out.paint(Style::Red, "x"));
            }
        }
    }

    // A file that could not be read is not a failure of the command — the other
    // thirteen were printed — but it is not a success either.
    Ok(if failed == 0 { Exit::Ok } else { Exit::Error }.into())
}

/// `mpdfm tag set`, and `mpdfm tag diff` when `look_only`.
///
/// # Errors
///
/// As [`show`], plus anything [`commit_with`][commit::commit_with] raises that is
/// not a refusal — a refusal comes back as [`Exit::Conflict`] with the reasons
/// printed.
pub fn set(
    cli: &Cli,
    config: &Config,
    out: &Out,
    args: &TagSetArgs,
    look_only: bool,
) -> Result<ExitCode> {
    // Step 1 — the files.
    let root = config.require_music_dir()?;
    let library = Library::scan(root)?;
    let files = select(cli, &library, &args.paths, args.recursive, root)?;
    cli.trace(format!(
        "tag {} {} file(s): {} {}",
        if look_only { "diff" } else { "set" },
        files.len(),
        args.fields.edits().join(" "),
        args.fields.actions().join(" ")
    ));
    anyhow::ensure!(
        !args.fields.is_empty(),
        "nothing to do: pass a field (--genre, --album, …), --clear <field>, or an \
         action such as --renumber-tracks. `mpdfm tag show` prints what is there now."
    );

    // Step 2 — their tags. A file that cannot be read refuses the batch: a bulk
    // view of nine of ten files is a view of a selection nobody asked for.
    let read = tags::read_many(&files, root);
    let unreadable: Vec<String> = read
        .iter()
        .filter_map(|(_, result)| result.as_ref().err().map(ToString::to_string))
        .collect();
    anyhow::ensure!(
        unreadable.is_empty(),
        "these file(s) could not be read, so nothing was changed:\n{}",
        unreadable
            .iter()
            .map(|err| format!("  - {err}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let selection: Vec<(RelPath, TagSet)> = read
        .into_iter()
        .map(|(rel, result)| (rel, result.expect("checked above")))
        .collect();

    // Step 3 — what to write.
    let view = BulkView::of(&selection);
    let deltas = edits(&view, &args.fields)?;
    if deltas.is_empty() {
        if !out.json {
            println!("Nothing to change: every selected file already says that.");
        }
        return Ok(Exit::Ok.into());
    }

    // Step 4 — what it would do. Nothing has been touched.
    let plan = Plan::of(
        deltas
            .into_iter()
            .map(|(target, changes)| Operation::WriteTags { target, changes })
            .collect(),
    );
    let (index, index_warnings) = PlaylistIndex::load(&config.playlist_dir);
    for warning in &index_warnings {
        cli.trace(warning);
    }
    let effects = plan.validate(&library, &index, config);

    // Steps 5 and 6.
    if look_only && !out.json {
        print_diff(&plan, &selection, out);
    }
    match decide(cli, out, args, look_only, &effects)? {
        Verdict::Stop(exit) => {
            if out.json {
                output::json(&report(&plan, &effects, None, exit))?;
            }
            return Ok(exit.into());
        }
        Verdict::Commit => {}
    }

    // Step 7 — the only part that writes. MPD is told to rescan the directories
    // the edits touched, because a tag write advances the mtime and MPD's index
    // is otherwise a version behind until its next update.
    let link = super::mpd::Link::open(cli, config);
    let update = |dirs: &[DirPath]| link.update(dirs);
    let options = commit::Options {
        update: link.connected().then_some(&update as commit::Updater<'_>),
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
                output::json(&report(&plan, &effects, Some(&committed), Exit::Ok))?;
            } else {
                print_committed(&committed, out);
            }
            Ok(Exit::Ok.into())
        }
        Err(err) if refused_without_writing(&err) => {
            if out.json {
                let mut document = report(&plan, &effects, None, Exit::Conflict);
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

// ---------------------------------------------------------------------------
// Step 1 — which files.

/// Expand the command line's paths into the audio files to act on.
///
/// A file is itself; a directory is the audio files in it, and with `recursive`
/// the ones below it as well. The result is **sorted by path and deduplicated**,
/// which matters for more than tidiness: that order is the one `--renumber-tracks`
/// numbers in, so it has to be the order the user saw — and a path named twice
/// must not become two operations on one file, which the planner would refuse as
/// a duplicate edit.
///
/// A path that is in the library but holds no audio is not an error; a path that
/// is not in the library at all is, because the user named something that is not
/// there.
fn select(
    cli: &Cli,
    library: &Library,
    paths: &[camino::Utf8PathBuf],
    recursive: bool,
    root: &camino::Utf8Path,
) -> Result<Vec<RelPath>> {
    let mut selected: BTreeSet<RelPath> = BTreeSet::new();
    for raw in paths {
        let rel = super::inside(raw, root)?;
        let dir = DirPath::from(rel.clone());

        if library.dir(&dir).is_some() {
            let before = selected.len();
            collect(library, &dir, recursive, &mut selected);
            cli.trace(format!(
                "{rel}/ contributed {} audio file(s)",
                selected.len() - before
            ));
            continue;
        }

        anyhow::ensure!(
            library.get(&rel).is_some(),
            "{rel} is not in the library; run `mpdfm scan` if it should be"
        );
        anyhow::ensure!(
            Kind::of(&rel).is_audio(),
            "{rel} is not an audio file MPDFM can tag"
        );
        selected.insert(rel);
    }

    anyhow::ensure!(
        !selected.is_empty(),
        "no audio files were selected{}",
        if recursive {
            ""
        } else {
            " — pass -r to include subdirectories"
        }
    );
    Ok(selected.into_iter().collect())
}

/// Every audio file in `dir`, and below it when `recursive`.
fn collect(library: &Library, dir: &DirPath, recursive: bool, into: &mut BTreeSet<RelPath>) {
    for entry in library.files_in(dir) {
        if entry.is_audio() {
            into.insert(entry.rel.clone());
        }
    }
    if recursive {
        for below in library.subdirs_in(dir) {
            collect(library, below, recursive, into);
        }
    }
}

// ---------------------------------------------------------------------------
// Step 3 — what to write.

/// Turn the flags into one [`TagDelta`] per file.
///
/// The field flags go through [`BulkView::delta_for`], which is what makes a
/// field nobody named stay unwritten; the actions each produce their own set, and
/// the sets are merged per file so that `--genre X --renumber-tracks` is one
/// operation per file holding both.
///
/// # Errors
///
/// A field given both a value and a `--clear`, a per-file field typed across a
/// selection of more than one, or a value a field cannot hold.
fn edits(view: &BulkView, fields: &TagFields) -> Result<Vec<(RelPath, TagDelta)>> {
    let mut asked: BTreeMap<Field, Edit> = BTreeMap::new();
    for (name, value) in named(fields) {
        let field = field(name)?;
        asked.insert(field, Edit::Set(Values::typed(value)));
    }
    for name in &fields.clear {
        let field = field(name)?;
        anyhow::ensure!(
            !asked.contains_key(&field),
            "--{field} and --clear {field} ask for opposite things"
        );
        asked.insert(field, Edit::Clear);
    }

    let per_file = view.per_file_in(&asked);
    anyhow::ensure!(
        per_file.is_empty(),
        "{} cannot be set to one value across {} files — every file wants its own.\n\
         Use --renumber-tracks or --title-from-filename, or name a single file.",
        per_file
            .iter()
            .map(|field| format!("--{field}"))
            .collect::<Vec<_>>()
            .join(" and "),
        view.len()
    );

    let mut sets = vec![view.delta_for(&asked)];
    if fields.renumber_tracks {
        sets.push(view.renumber_tracks());
    }
    if fields.title_from_filename {
        sets.push(view.titles_from_filenames());
    }
    if fields.album_artist_from_artist {
        sets.push(view.album_artist_from_artist());
    }
    if fields.strip_comment {
        sets.push(view.strip_comment());
    }
    if fields.trim_whitespace {
        sets.push(view.trim_whitespace());
    }
    Ok(merge(sets))
}

/// The `--field value` flags that were given, in display order.
fn named(fields: &TagFields) -> Vec<(&'static str, &str)> {
    [
        ("title", &fields.title),
        ("artist", &fields.artist),
        ("albumartist", &fields.album_artist),
        ("album", &fields.album),
        ("year", &fields.year),
        ("track", &fields.track),
        ("disc", &fields.disc),
        ("genre", &fields.genre),
        ("comment", &fields.comment),
        ("composer", &fields.composer),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.as_deref().map(|value| (name, value)))
    .collect()
}

/// The field a `--clear` argument names.
fn field(name: &str) -> Result<Field> {
    Field::parse(name).ok_or_else(|| {
        anyhow::anyhow!(
            "{name:?} is not a field MPDFM edits. The ten are: {}",
            mpdfm_core::tags::FIELDS
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Fold several per-file edit sets into one delta per file.
///
/// Later sets win on a field they share, which is the order the flags are
/// applied in — an explicit `--track 3/9` alongside `--renumber-tracks` means the
/// renumbering, because that is the more specific request and the one that cannot
/// be expressed any other way.
fn merge(sets: Vec<Vec<(RelPath, TagDelta)>>) -> Vec<(RelPath, TagDelta)> {
    let mut merged: BTreeMap<RelPath, TagDelta> = BTreeMap::new();
    for set in sets {
        for (rel, delta) in set {
            let into = merged.entry(rel).or_default();
            for (field, edit) in delta.edits() {
                *into = std::mem::take(into).with(*field, edit.clone());
            }
        }
    }
    merged
        .into_iter()
        .filter(|(_, delta)| !delta.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Steps 5 and 6 — show it, and find out whether to go on.

/// What to do after the preview has been rendered.
enum Verdict {
    /// Stop here with this status. Nothing has been written.
    Stop(Exit),
    /// Go ahead.
    Commit,
}

/// Print the preview and work out whether to commit.
///
/// The same three gates as `move`, in the same order: a refused plan is refused
/// whatever `--yes` says, looking stops before the question is asked, and only
/// then does anybody get asked.
fn decide(
    cli: &Cli,
    out: &Out,
    args: &TagSetArgs,
    look_only: bool,
    effects: &Effects,
) -> Result<Verdict> {
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

    if look_only || args.dry_run {
        if !out.json {
            println!();
            println!(
                "{}",
                out.paint(
                    Style::Dim,
                    if look_only {
                        "tag diff: nothing was changed."
                    } else {
                        "--dry-run: nothing was changed."
                    }
                )
            );
        }
        return Ok(Verdict::Stop(Exit::Ok));
    }

    if args.yes {
        cli.trace("--yes: committing without asking");
        return Ok(Verdict::Commit);
    }

    println!();
    if out.confirm("Write these tags?")? {
        Ok(Verdict::Commit)
    } else {
        println!("{}", out.paint(Style::Dim, "Nothing was changed."));
        Ok(Verdict::Stop(Exit::Declined))
    }
}

/// `tag diff`'s per-file before-and-after.
///
/// The preview that follows it counts files per changed field, which is the right
/// summary for a four-hundred-file edit and the wrong one for checking what will
/// happen to each file. This is the other half.
fn print_diff(plan: &Plan, selection: &[(RelPath, TagSet)], out: &Out) {
    let was: BTreeMap<&RelPath, &TagSet> =
        selection.iter().map(|(rel, tags)| (rel, tags)).collect();

    for op in plan.ops() {
        let Operation::WriteTags { target, changes } = op else {
            continue;
        };
        println!("{}", out.paint(Style::Bold, target.as_str()));
        for (field, edit) in changes.edits() {
            let before = was
                .get(target)
                .map(|tags| tags.get(*field).joined())
                .unwrap_or_default();
            let after = match edit {
                Edit::Set(values) => values.joined(),
                Edit::Clear => String::new(),
            };
            println!(
                "  {field:<12} {} → {}",
                out.paint(Style::Dim, &shown(&before)),
                shown(&after)
            );
        }
    }
    println!();
}

/// A value, or a mark for the absence of one — so an empty line is not mistaken
/// for a value that is a space.
fn shown(value: &str) -> String {
    if value.is_empty() {
        "<none>".to_owned()
    } else {
        format!("{value:?}")
    }
}

/// The lines after a successful commit.
fn print_committed(committed: &Committed, out: &Out) {
    println!();
    for warning in &committed.warnings {
        println!("  {} {warning}", out.paint(Style::Yellow, "!"));
    }
    println!("{}", out.paint(Style::Green, &committed.headline()));
    println!("Undo it with `mpdfm undo {}`.", committed.txid);
}

/// Whether this failure left the library exactly as it was — see `move`'s copy of
/// this, which it shares the reasoning with.
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

/// `tag show --json`: one object per file, with the error in place of the tags
/// for a file that could not be read.
fn show_json(
    root: &camino::Utf8Path,
    read: &[(RelPath, Result<TagSet, mpdfm_core::tags::TagError>)],
) -> serde_json::Value {
    let files: Vec<serde_json::Value> = read
        .iter()
        .map(|(rel, result)| match result {
            Ok(tags) => {
                // The audio properties cost a second pass over the file, so they
                // are read here rather than by `read_many` — `--json` is the one
                // caller that wants them for every file at once.
                let audio = tags::read(&rel.to_abs(root)).ok().map(|(_, info)| info);
                serde_json::json!({
                    "path": rel.as_str(),
                    "tags": tags,
                    "audio": audio,
                })
            }
            Err(err) => serde_json::json!({
                "path": rel.as_str(),
                "error": err.to_string(),
            }),
        })
        .collect();
    serde_json::json!({ "files": files })
}

/// `tag set --json`: the plan, the effects and what was done with them.
fn report(
    plan: &Plan,
    effects: &Effects,
    committed: Option<&Committed>,
    exit: Exit,
) -> serde_json::Value {
    serde_json::json!({
        "edits": plan.ops().iter().filter_map(|op| match op {
            Operation::WriteTags { target, changes } => Some(serde_json::json!({
                "path": target.as_str(),
                "fields": changes.edits().iter().map(|(field, edit)| serde_json::json!({
                    "field": field.as_str(),
                    "value": edit.values().map(Values::joined),
                })).collect::<Vec<_>>(),
            })),
            _ => None,
        }).collect::<Vec<_>>(),
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

// ---------------------------------------------------------------------------

/// One file's fields, as `tag show` prints them.
fn print_tags(root: &camino::Utf8Path, rel: &RelPath, tags: &TagSet, out: &Out) {
    match tags::read(&rel.to_abs(root)) {
        Ok((_, info)) => println!("  {}", out.paint(Style::Dim, &info.to_string())),
        Err(err) => println!("  {} {err}", out.paint(Style::Yellow, "!")),
    }

    if tags.is_empty() {
        println!("  {}", out.paint(Style::Dim, "(no tags)"));
        return;
    }
    for (field, values) in tags.present() {
        println!("  {field:<12} {}", values.joined());
    }
    // Everything MPDFM does not model, marked so it is clear it is shown and not
    // editable — and that a write will leave it alone.
    for (key, value) in &tags.extra {
        println!("  {} {key:<10} {value}", out.paint(Style::Dim, "+"));
    }
}
