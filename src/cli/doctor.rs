//! `mpdfm doctor` — what is wrong with the library and the playlists.
//!
//! The M1 half of a command task 29 finishes. Five checks, chosen because each
//! one is answerable from what tasks 05 and 07 already build and because each
//! one names something a person would want to fix:
//!
//! | check | what it means |
//! |---|---|
//! | `broken-references` | a playlist line names a file that is not there |
//! | `unrewritable-entries` | a playlist line MPDFM cannot resolve, so a move would leave it behind |
//! | `unreadable-playlists` | a playlist MPDFM could not index at all |
//! | `empty-dirs` / `no-audio-dirs` | directories with nothing, or nothing but clutter, in them |
//! | `unreferenced-audio` | tracks no playlist mentions — **informational** |
//!
//! The tag, duplicate and hygiene checks are deliberately not here; they need
//! task 16's tag reading and are task 29's subject. `--check <name>`, `--full`
//! and `--deep` arrive with them.
//!
//! # A doctor that cries wolf gets ignored
//!
//! So the split between a *problem* and a *note* is enforced by the type
//! ([`Severity`]), and the default is `Note`. "Referenced by no playlist" is
//! normal for almost every track in a 2 800-file library, and a `.parts` file is
//! a download in progress rather than a defect — both are worth saying once,
//! with a count, and neither is worth the word "problem".
//!
//! Exit status is **0 whatever is found**. `doctor` reports; it does not fail.
//! A caller that wants to act on the findings reads `--json`, where `problems`
//! is the number that matters.
//!
//! Running this against the real `~/Music` is **authorized**: like `scan`, it
//! only reads.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
use std::time::SystemTime;

use anyhow::Result;
use camino::Utf8Path;
use mpdfm_core::config::Config;
use mpdfm_core::library::{DirPath, Kind, Library};
use mpdfm_core::playlist::{Entry, PlaylistIndex};

use crate::cli::Cli;
use crate::output::{self, Exit, Out, Style};

/// How many items of one check the text output lists.
///
/// `unreferenced-audio` is 2 578 items long on the real library; printing it in
/// full would bury every other check. Task 29's `--full` lifts the cap; until
/// then `--json` is the complete answer.
const ITEM_LIMIT: usize = 10;

/// Run the checks and report.
///
/// # Errors
///
/// [`RootProblem`][mpdfm_core::config::RootProblem] if `music_dir` is missing,
/// and [`Error::Io`][mpdfm_core::Error::Io] if the root cannot be read. A
/// playlist directory that cannot be read is a finding, not an error — that is
/// what the `unreadable-playlists` check is.
pub fn run(cli: &Cli, config: &Config, out: &Out) -> Result<ExitCode> {
    let root = config.require_music_dir()?;
    cli.trace(format!("checking {root} against {}", config.playlist_dir));

    let library = Library::scan(root)?;
    let (index, index_warnings) = PlaylistIndex::load(&config.playlist_dir);
    cli.trace(format!(
        "{} files, {} playlists, {} references",
        library.len(),
        index.len(),
        index.reference_count()
    ));

    let checks = vec![
        broken_references(&library, &index),
        unrewritable_entries(&index, root),
        unreadable_playlists(&index_warnings),
        empty_dirs(&library),
        no_audio_dirs(&library),
        unreferenced_audio(&library, &index),
        partial_downloads(&library),
    ];

    if out.json {
        output::json(&to_json(config, &library, &index, &checks))?;
    } else {
        print(config, &library, &index, &checks, out);
    }
    Ok(Exit::Ok.into())
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// Whether a finding is something to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    /// Worth fixing. Counted in the headline.
    Problem,
    /// Worth knowing. Never counted as a problem, however many there are.
    Note,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Problem => "problem",
            Self::Note => "note",
        }
    }
}

/// One finding.
struct Item {
    /// What it is about, as the user would name it.
    what: String,
    /// Why it is being mentioned.
    detail: String,
    /// A command that would fix it, when there is an obvious one.
    fix: Option<String>,
}

/// One check, and everything it found.
struct Check {
    /// Its stable machine-readable name, which `--json` keys by and task 29's
    /// `--check <name>` will select on.
    name: &'static str,
    severity: Severity,
    /// What a clean result means, for the one-line summary.
    about: &'static str,
    items: Vec<Item>,
}

impl Check {
    fn new(name: &'static str, severity: Severity, about: &'static str, items: Vec<Item>) -> Self {
        Self {
            name,
            severity,
            about,
            items,
        }
    }

    fn is_problem(&self) -> bool {
        self.severity == Severity::Problem && !self.items.is_empty()
    }
}

/// Print the findings, worst first within each check and checks in the order
/// they were run.
fn print(config: &Config, library: &Library, index: &PlaylistIndex, checks: &[Check], out: &Out) {
    // Padded *before* painting: an escape sequence has no width on screen and
    // every width in a format string, so padding a painted label aligns the two
    // lines only when colour happens to be off.
    println!(
        "{}  {}",
        out.paint(Style::Bold, &format!("{:9}", "LIBRARY")),
        library.root()
    );
    println!(
        "{}  {}",
        out.paint(Style::Bold, &format!("{:9}", "PLAYLISTS")),
        config.playlist_dir
    );
    println!(
        "{}",
        out.paint(
            Style::Dim,
            &format!(
                "{} files, {} playlists, {} references",
                library.len(),
                index.len(),
                index.reference_count()
            )
        )
    );

    for check in checks {
        println!();
        let count = check.items.len();
        let heading = format!("{} ({count})", check.name);
        let style = match (check.severity, count) {
            (_, 0) => Style::Green,
            (Severity::Problem, _) => Style::Red,
            (Severity::Note, _) => Style::Dim,
        };
        println!("{}  {}", out.paint(style, &heading), check.about);

        for item in check.items.iter().take(ITEM_LIMIT) {
            let marker = match check.severity {
                Severity::Problem => 'x',
                Severity::Note => '-',
            };
            println!("  {marker} {}", item.what);
            if !item.detail.is_empty() {
                println!("      {}", out.paint(Style::Dim, &item.detail));
            }
            if let Some(fix) = &item.fix {
                println!("      {}", out.paint(Style::Dim, &format!("fix: {fix}")));
            }
        }
        if count > ITEM_LIMIT {
            println!(
                "  {}",
                out.paint(
                    Style::Dim,
                    &format!("… and {} more (--json lists them all)", count - ITEM_LIMIT)
                )
            );
        }
    }

    let problems: usize = checks
        .iter()
        .filter(|check| check.severity == Severity::Problem)
        .map(|check| check.items.len())
        .sum();
    println!();
    if problems == 0 {
        println!("{}", out.paint(Style::Green, "No problems found."));
    } else {
        println!(
            "{}",
            out.paint(
                Style::Red,
                &format!("{problems} problem(s) found across {} check(s).", {
                    checks.iter().filter(|c| c.is_problem()).count()
                })
            )
        );
    }
}

fn to_json(
    config: &Config,
    library: &Library,
    index: &PlaylistIndex,
    checks: &[Check],
) -> serde_json::Value {
    let findings: serde_json::Map<String, serde_json::Value> = checks
        .iter()
        .map(|check| {
            (
                check.name.to_owned(),
                serde_json::json!({
                    "severity": check.severity.as_str(),
                    "about": check.about,
                    "count": check.items.len(),
                    "items": check.items.iter().map(|item| serde_json::json!({
                        "what": item.what,
                        "detail": item.detail,
                        "fix": item.fix,
                    })).collect::<Vec<_>>(),
                }),
            )
        })
        .collect();

    serde_json::json!({
        "music_dir": library.root(),
        "playlist_dir": config.playlist_dir,
        "files": library.len(),
        "playlists": index.len(),
        "references": index.reference_count(),
        "problems": checks
            .iter()
            .filter(|check| check.severity == Severity::Problem)
            .map(|check| check.items.len())
            .sum::<usize>(),
        "checks": findings,
    })
}

// ---------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------

/// Playlist lines naming a file that is not in the library.
///
/// The real library has exactly one, and it is the acceptance criterion for this
/// command. [`PlaylistIndex::broken`] answers it against the scanned model
/// rather than against the disk, which is deliberate: a reference to a file the
/// scan skipped — a symlinked track, a name that is not UTF-8 — is reported
/// here too, because a file MPDFM will not move is a file it cannot promise
/// about.
fn broken_references(library: &Library, index: &PlaylistIndex) -> Check {
    let items = index
        .broken(library)
        .into_iter()
        .map(|(reference, path)| {
            let playlist = index
                .playlist(reference.playlist)
                .map_or("?", |playlist| playlist.name());
            Item {
                // The entry index is 0-based and a user counts lines from one.
                what: format!("{playlist}:{} → {path}", reference.entry + 1),
                detail: "names a file that is not in the library".to_owned(),
                fix: None,
            }
        })
        .collect();

    Check::new(
        "broken-references",
        Severity::Problem,
        "playlist lines whose file is missing",
        items,
    )
}

/// Playlist lines MPDFM cannot resolve against the music directory.
///
/// An absolute path, a `./` path, a backslash path. The parser keeps them as
/// [`Entry::Unparsed`] so they round-trip byte-for-byte, and that is the right
/// answer — but it also means a move will not rewrite them, so the line will be
/// pointing at the old location afterwards and it will look like MPDFM broke it.
/// Saying so now is the point.
///
/// A line that is merely odd — a stray carriage return, say — is also
/// `Unparsed` and is *not* reported: it names no path, so no move can strand it.
fn unrewritable_entries(index: &PlaylistIndex, root: &Utf8Path) -> Check {
    let mut items = Vec::new();
    for playlist in index.playlists() {
        for (line, entry) in playlist.entries().iter().enumerate() {
            let Entry::Unparsed(text) = entry else {
                continue;
            };
            let Some(reason) = unresolvable_path(text) else {
                continue;
            };
            // An absolute path that happens to point inside the library has an
            // obvious fix, so offer the exact line it should be.
            let fix = Utf8Path::new(text.trim())
                .strip_prefix(root)
                .ok()
                .map(|rel| format!("replace it with `{rel}`"));
            items.push(Item {
                what: format!("{}:{} → {text}", playlist.name(), line + 1),
                detail: format!("{reason}; a move will not rewrite this line"),
                fix,
            });
        }
    }

    Check::new(
        "unrewritable-entries",
        Severity::Problem,
        "playlist lines that are not paths relative to the music directory",
        items,
    )
}

/// Why a line cannot be a path relative to the music directory, or `None` if it
/// does not look like a path at all.
fn unresolvable_path(line: &str) -> Option<&'static str> {
    let text = line.trim();
    if text.starts_with('/') {
        Some("an absolute path, which is outside the music directory")
    } else if text.starts_with("~/") {
        Some("a path that relies on shell expansion")
    } else if text.starts_with("./") || text.starts_with("../") || text.contains("/../") {
        Some("a path with `.` or `..` in it")
    } else if text.contains('\\') {
        Some("a path with a backslash in it")
    } else {
        None
    }
}

/// Playlists MPDFM could not index.
///
/// Its own check rather than a warning on stderr, because the consequence is the
/// same as a broken reference and worse: a playlist MPDFM cannot read is one it
/// cannot fix, so a move will silently leave every line in it stale.
fn unreadable_playlists(warnings: &[mpdfm_core::playlist::IndexWarning]) -> Check {
    let items = warnings
        .iter()
        .map(|warning| Item {
            what: warning.path().to_owned(),
            detail: warning.to_string(),
            fix: None,
        })
        .collect();

    Check::new(
        "unreadable-playlists",
        Severity::Problem,
        "playlists that could not be read, so a move cannot fix them",
        items,
    )
}

/// Directories holding nothing at all.
fn empty_dirs(library: &Library) -> Check {
    let items = library
        .dirs()
        .filter(|(dir, contents)| !dir.is_root() && contents.is_empty())
        .map(|(dir, _)| Item {
            what: dir.to_string(),
            detail: "holds no files and no subdirectories".to_owned(),
            // Quoted, because almost every album directory in this library has
            // a space in it and an unquoted suggestion would not run.
            fix: Some(format!("rmdir {:?}", dir.as_str())),
        })
        .collect();

    Check::new(
        "empty-dirs",
        Severity::Problem,
        "directories with nothing in them",
        items,
    )
}

/// Leaf directories holding files but no audio.
///
/// Cover art and an `.nfo` with no album around them: either the album was moved
/// by something that did not take its aux files, or this is a stray. A directory
/// with subdirectories is excluded, because that is what a genre directory and a
/// multi-disc set root both look like and neither is a defect.
fn no_audio_dirs(library: &Library) -> Check {
    let items = library
        .dirs()
        .filter(|(dir, contents)| {
            !dir.is_root()
                && !contents.is_empty()
                && contents.subdirs().is_empty()
                && !library.files_in(dir).any(|entry| entry.is_audio())
        })
        .map(|(dir, contents)| Item {
            what: dir.to_string(),
            detail: format!("{} file(s), none of them audio", contents.files().len()),
            fix: None,
        })
        .collect();

    Check::new(
        "no-audio-dirs",
        Severity::Note,
        "leaf directories with files but no tracks",
        items,
    )
}

/// Audio files no playlist mentions.
///
/// Informational, and the check most likely to be misread: on the real library
/// this is 2 500-odd files, because 17 playlists hold 231 references between
/// them. It is here because it is the work queue for "what have I never filed",
/// not because anything is broken.
///
/// A track described by a referenced CUE sheet counts as referenced: a playlist
/// line `album.flac.cue/track0017` keys under the sheet, and the sheet is what
/// carries `album.flac` with it.
fn unreferenced_audio(library: &Library, index: &PlaylistIndex) -> Check {
    let mut referenced: BTreeSet<&str> = index.paths().iter().map(|rel| rel.as_str()).collect();
    // `…/album.flac.cue` referenced means `…/album.flac` is referenced too.
    let through_cue: Vec<&str> = referenced
        .iter()
        .filter_map(|path| path.strip_suffix(".cue"))
        .collect();
    referenced.extend(through_cue);

    let items = library
        .entries()
        .iter()
        .filter(|entry| entry.is_audio() && !referenced.contains(entry.rel.as_str()))
        .map(|entry| Item {
            what: entry.rel.to_string(),
            detail: String::new(),
            fix: None,
        })
        .collect();

    Check::new(
        "unreferenced-audio",
        Severity::Note,
        "tracks that appear in no playlist (normal, not a defect)",
        items,
    )
}

/// Partial downloads that look like they are still being written.
///
/// A `.parts` file is [`Kind::Other`]: scanned, counted, moved with its album,
/// never interpreted. That is the right answer and `doctor` has no complaint
/// about one — except this: moving a file something is still downloading into is
/// a real way to confuse the downloader, and MPDFM has no way to know whether it
/// is. A `.parts` newer than everything else in its directory is the best
/// available guess, so it is said out loud before somebody moves that album.
fn partial_downloads(library: &Library) -> Check {
    // The newest mtime in each directory, from everything that is not itself a
    // partial download.
    let mut newest: BTreeMap<DirPath, SystemTime> = BTreeMap::new();
    for entry in library.entries() {
        if is_partial(entry.rel.extension()) {
            continue;
        }
        newest
            .entry(entry.dir())
            .and_modify(|at| *at = (*at).max(entry.mtime))
            .or_insert(entry.mtime);
    }

    let items = library
        .entries()
        .iter()
        .filter(|entry| is_partial(entry.rel.extension()))
        .filter(|entry| newest.get(&entry.dir()).is_none_or(|at| entry.mtime > *at))
        .map(|entry| Item {
            what: entry.rel.to_string(),
            detail: "newer than everything else in its directory — a download in \
                     progress? Moving it now would confuse whatever is writing it."
                .to_owned(),
            fix: None,
        })
        .collect();

    Check::new(
        "partial-downloads",
        Severity::Note,
        "incomplete downloads that may still be being written",
        items,
    )
}

/// Whether this extension names a partial download.
fn is_partial(extension: Option<&str>) -> bool {
    extension.is_some_and(|ext| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "parts" | "part" | "crdownload"
        ) && Kind::from_extension(Some(ext)) == Kind::Other
    })
}
