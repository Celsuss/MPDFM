//! `mpdfm scan` — survey the library and say what is in it.
//!
//! One walk, the counts it produced, and everything the walk could not model.
//! That is the whole command: it writes nothing, talks to nothing, and is the
//! cheapest way to find out whether MPDFM is pointed at the right directory and
//! agrees with the user about what is in it.
//!
//! The numbers are worth eyeballing rather than only trusting. On the library
//! this was written against they are 2 440 mp3 / 364 flac / 5 m4a across 318
//! directories (`docs/tasks/15-cli-move-and-doctor.md`), and a scan that
//! disagrees with that by a wide margin means the root is wrong, not that the
//! library changed.
//!
//! Running this against the real `~/Music` is **authorized**: see the task's
//! "Verifying it by hand". It is read-only — there is no code path in this file
//! that opens a file for writing.

use std::process::ExitCode;

use anyhow::Result;
use mpdfm_core::config::Config;
use mpdfm_core::library::{Counts, Library, ScanWarning};

use crate::cli::Cli;
use crate::output::{self, Exit, Out, Style};

/// How many warnings the text output lists before it stops.
///
/// A library with 400 symlinks in it has one problem, not 400, and burying the
/// counts under them helps nobody. `--json` carries all of them.
const WARNING_LIMIT: usize = 20;

/// Walk the library and report.
///
/// # Errors
///
/// [`RootProblem`][mpdfm_core::config::RootProblem] if `music_dir` is missing or
/// is not a directory, and [`Error::Io`][mpdfm_core::Error::Io] if the root
/// itself cannot be read. Everything the walk hits below the root is a
/// [`ScanWarning`] instead.
pub fn run(cli: &Cli, config: &Config, out: &Out) -> Result<ExitCode> {
    let root = config.require_music_dir()?;
    cli.trace(format!("scanning {root}"));

    let started = std::time::Instant::now();
    let library = Library::scan(root)?;
    // On stderr and only under `-v`, so neither output is time-dependent — but
    // recorded, because whether a 2 800-file scan can sit behind a TUI keystroke
    // is a question task 20 has to answer and this is where the number comes
    // from.
    cli.trace(format!(
        "scanned {} files in {} ms",
        library.len(),
        started.elapsed().as_millis()
    ));

    let survey = Survey::of(&library);
    if out.json {
        output::json(&survey.to_json(&library))?;
    } else {
        survey.print(&library, out);
    }
    Ok(Exit::Ok.into())
}

/// The counts `scan` reports, derived from the model in one place so the text
/// and the JSON cannot drift apart.
struct Survey {
    counts: Counts,
    dirs: usize,
    album_dirs: usize,
    discs: usize,
    disc_sets: usize,
}

impl Survey {
    fn of(library: &Library) -> Self {
        let albums = library.album_dirs();
        let discs = albums.iter().filter(|album| album.is_disc()).count();
        let disc_sets: std::collections::BTreeSet<_> = albums
            .iter()
            .filter_map(|album| album.set_root.as_ref())
            .collect();

        Self {
            counts: library.counts(),
            dirs: library.dir_count(),
            album_dirs: albums.len(),
            discs,
            disc_sets: disc_sets.len(),
        }
    }

    /// The table, plus whatever the walk could not model.
    fn print(&self, library: &Library, out: &Out) {
        let c = &self.counts;
        println!("{}  {}", out.paint(Style::Bold, "LIBRARY"), library.root());
        println!();

        let rows: &[(&str, usize, String)] = &[
            (
                "audio",
                c.audio(),
                format!("{} mp3, {} flac, {} m4a", c.mp3, c.flac, c.m4a),
            ),
            ("images", c.images, String::new()),
            ("cue sheets", c.cues, String::new()),
            ("playlists", c.playlists, String::new()),
            ("sidecars", c.sidecars, String::new()),
            ("other", c.other, String::new()),
            ("files", c.total(), String::new()),
            ("", 0, String::new()),
            ("directories", self.dirs, String::new()),
            ("album dirs", self.album_dirs, self.disc_note()),
        ];

        let label_width = rows
            .iter()
            .map(|(label, ..)| label.len())
            .max()
            .unwrap_or(0);
        for (label, count, note) in rows {
            if label.is_empty() {
                println!();
                continue;
            }
            let line = format!("{label:label_width$}  {count:>6}");
            if note.is_empty() {
                println!("{line}");
            } else {
                println!("{line}  {}", out.paint(Style::Dim, note));
            }
        }

        print_warnings(library.warnings(), out);
    }

    /// The multi-disc aside, when there is one.
    fn disc_note(&self) -> String {
        if self.disc_sets == 0 {
            return String::new();
        }
        format!(
            "{} of them discs of {} multi-disc set(s)",
            self.discs, self.disc_sets
        )
    }

    fn to_json(&self, library: &Library) -> serde_json::Value {
        let c = &self.counts;
        serde_json::json!({
            "music_dir": library.root(),
            "counts": {
                "mp3": c.mp3,
                "flac": c.flac,
                "m4a": c.m4a,
                "audio": c.audio(),
                "images": c.images,
                "cue": c.cues,
                "playlists": c.playlists,
                "sidecars": c.sidecars,
                "other": c.other,
                "files": c.total(),
            },
            "directories": self.dirs,
            "album_dirs": self.album_dirs,
            "discs": self.discs,
            "multi_disc_sets": self.disc_sets,
            "warnings": library.warnings().iter().map(warning_json).collect::<Vec<_>>(),
        })
    }
}

/// The warnings, grouped by kind with a count each, then listed.
///
/// On stdout rather than stderr: here they are part of the answer the user asked
/// for — "what is in my library" includes "and these three things I could not
/// read" — rather than something that happened on the way to it.
fn print_warnings(warnings: &[ScanWarning], out: &Out) {
    if warnings.is_empty() {
        return;
    }
    println!();
    println!(
        "{} ({})",
        out.paint(Style::Yellow, "Warnings"),
        warnings.len()
    );

    let mut by_kind: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for warning in warnings {
        *by_kind.entry(warning_kind(warning)).or_default() += 1;
    }
    for (kind, count) in &by_kind {
        println!("  {count:>6}  {kind}");
    }

    println!();
    for warning in warnings.iter().take(WARNING_LIMIT) {
        println!("  ! {warning}");
    }
    if warnings.len() > WARNING_LIMIT {
        println!(
            "  {}",
            out.paint(
                Style::Dim,
                &format!(
                    "… and {} more (--json lists them all)",
                    warnings.len() - WARNING_LIMIT
                )
            )
        );
    }
}

/// A stable machine-readable name for a warning's kind.
///
/// Spelled out rather than derived from the `Display` text, because `--json`
/// consumers key off these and a reworded message must not break them.
fn warning_kind(warning: &ScanWarning) -> &'static str {
    match warning {
        ScanWarning::NotUtf8 { .. } => "not-utf8",
        ScanWarning::Unnamable { .. } => "unnamable",
        ScanWarning::Symlink { .. } => "symlink",
        ScanWarning::Unreadable { .. } => "unreadable",
    }
}

fn warning_json(warning: &ScanWarning) -> serde_json::Value {
    serde_json::json!({
        "kind": warning_kind(warning),
        "path": warning.path(),
        "message": warning.to_string(),
    })
}
