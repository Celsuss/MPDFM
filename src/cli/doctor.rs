//! `mpdfm doctor` — what is wrong with the library and the playlists.
//!
//! The checks live in core ([`mpdfm_core::doctor`]), where the split between a
//! problem, a warning and a note is written down. This module loads what they
//! look at, runs the ones `--check` selected, and renders the [`Report`]:
//!
//! - **counts first** — every selected check, grouped, with how many it found,
//!   so a clean check is visibly clean rather than silently absent;
//! - **then the findings**, ten per check unless `--full`, each with the
//!   command that would fix it when there is an obvious one.
//!
//! `--json` is always complete, whatever `--full` says: a script reading it has
//! no reason to want a sample.
//!
//! Exit status is **0 whatever is found**. `doctor` reports; it does not fail.
//! A caller that wants to act on the findings reads `--json`, where `problems`
//! is the number that matters.
//!
//! Running this against the real `~/Music` is **authorized**: like `scan`, it
//! only reads — `--deep` included, which reads a great deal.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Result;
use mpdfm_core::config::Config;
use mpdfm_core::doctor::{
    self, Check, Group, Inputs, Options, Progress, Queue, Report, Selection, Severity,
};
use mpdfm_core::library::Library;
use mpdfm_core::mpd::state::MpdState;
use mpdfm_core::playlist::PlaylistIndex;

use crate::cli::{Cli, DoctorArgs};
use crate::output::{self, Exit, Out, Style};

/// How many findings of one check the text output lists without `--full`.
///
/// `unreferenced-audio` is 2 500-odd items long on the real library; printing
/// it in full would bury every other check.
const ITEM_LIMIT: usize = 10;

/// Run the checks and report.
///
/// # Errors
///
/// An unknown `--check` name; [`RootProblem`][mpdfm_core::config::RootProblem]
/// if `music_dir` is missing; and [`Error::Io`][mpdfm_core::Error::Io] if the
/// root cannot be read. A playlist or a state file that cannot be read is a
/// finding, not an error.
pub fn run(cli: &Cli, config: &Config, out: &Out, args: &DoctorArgs) -> Result<ExitCode> {
    let selection = Selection::parse(&args.check)?;
    let root = config.require_music_dir()?;
    cli.trace(format!("checking {root} against {}", config.playlist_dir));

    let library = Library::scan(root)?;
    let (index, index_warnings) = PlaylistIndex::load(&config.playlist_dir);
    let queue = match &config.state_file {
        None => Queue::NotConfigured,
        Some(path) => match MpdState::load(path) {
            Ok(state) => Queue::Loaded(state),
            Err(err) => Queue::Unreadable(err.to_string()),
        },
    };
    cli.trace(format!(
        "{} files, {} playlists, {} references",
        library.len(),
        index.len(),
        index.reference_count()
    ));

    let inputs = Inputs {
        library: &library,
        index: &index,
        index_warnings: &index_warnings,
        queue,
    };
    let options = Options {
        selection,
        deep: args.deep,
    };
    let started = std::time::Instant::now();
    let mut meter = Meter::new(out);
    let report = doctor::run(&inputs, &options, &mut |progress| meter.show(progress));
    meter.finish();
    cli.trace(format!("checked in {:.2?}", started.elapsed()));

    if out.json {
        output::json(&to_json(config, &library, &index, &report))?;
    } else {
        print(config, &library, &index, &report, out, args.full);
    }
    Ok(Exit::Ok.into())
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// The progress line on stderr.
///
/// Live and overwritten in place when stderr is a terminal; otherwise one line
/// when the `--deep` pass starts, so a log still says why the run took minutes,
/// and nothing for the tag read, which takes about a second.
struct Meter {
    live: bool,
    drawn: bool,
    announced: bool,
}

impl Meter {
    fn new(out: &Out) -> Self {
        Self {
            live: std::io::stderr().is_terminal() && !out.json,
            drawn: false,
            announced: false,
        }
    }

    fn show(&mut self, progress: Progress) {
        let (what, done, total) = match progress {
            Progress::ReadingTags { done, total } => ("reading tags", done, total),
            Progress::Hashing { done, total } => ("--deep: comparing contents", done, total),
        };
        if self.live {
            eprint!("\r\u{1b}[2Kmpdfm: {what} {done}/{total}");
            std::io::stderr().flush().ok();
            self.drawn = true;
        } else if matches!(progress, Progress::Hashing { .. }) && !self.announced {
            eprintln!("mpdfm: --deep: comparing the contents of {total} same-sized audio files");
            self.announced = true;
        }
    }

    fn finish(&mut self) {
        if self.drawn {
            eprint!("\r\u{1b}[2K");
            std::io::stderr().flush().ok();
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The marker and colour a severity is drawn with.
fn look(severity: Severity) -> (char, Style) {
    match severity {
        Severity::Problem => ('x', Style::Red),
        Severity::Warning => ('!', Style::Yellow),
        Severity::Note => ('-', Style::Dim),
    }
}

/// `<name> (<count>)`, or `<name> (skipped)` — the heading a check is found by,
/// in the summary and above its findings.
fn heading(check: &Check) -> String {
    match &check.skipped {
        Some(_) => format!("{} (skipped)", check.info.name),
        None => format!("{} ({})", check.info.name, check.items.len()),
    }
}

/// Print the summary, then the findings, then the verdict.
fn print(
    config: &mpdfm_core::config::Config,
    library: &Library,
    index: &PlaylistIndex,
    report: &Report,
    out: &Out,
    full: bool,
) {
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

    // Counts first: every selected check, so a clean one is visibly clean.
    let width = report
        .checks
        .iter()
        .map(|check| heading(check).chars().count())
        .max()
        .unwrap_or(0);
    for group in Group::ALL {
        let checks: Vec<&Check> = report
            .checks
            .iter()
            .filter(|check| check.info.group == group)
            .collect();
        if checks.is_empty() {
            continue;
        }
        println!();
        println!("{}", out.paint(Style::Bold, &group.as_str().to_uppercase()));
        for check in checks {
            let style = match (check.info.severity, check.items.len()) {
                _ if check.skipped.is_some() => Style::Dim,
                (_, 0) => Style::Green,
                (severity, _) => look(severity).1,
            };
            let about = check.skipped.as_deref().unwrap_or(check.info.about);
            println!(
                "  {}  {}",
                out.paint(style, &format!("{:width$}", heading(check))),
                out.paint(Style::Dim, about)
            );
        }
    }

    // Then the paths.
    for check in report.checks.iter().filter(|check| !check.items.is_empty()) {
        let (marker, style) = look(check.info.severity);
        println!();
        println!(
            "{}  {}",
            out.paint(style, &heading(check)),
            out.paint(Style::Dim, check.info.severity.as_str())
        );
        let shown = if full { check.items.len() } else { ITEM_LIMIT };
        for item in check.items.iter().take(shown) {
            println!("  {marker} {}", item.what);
            if !item.detail.is_empty() {
                println!("      {}", out.paint(Style::Dim, &item.detail));
            }
            if let Some(fix) = &item.fix {
                println!("      {} {fix}", out.paint(Style::Dim, "fix:"));
            }
        }
        if check.items.len() > shown {
            println!(
                "  {}",
                out.paint(
                    Style::Dim,
                    &format!(
                        "… and {} more (--full lists them all)",
                        check.items.len() - shown
                    )
                )
            );
        }
    }

    let problems = report.count(Severity::Problem);
    let others = format!(
        "{} warning(s), {} note(s).",
        report.count(Severity::Warning),
        report.count(Severity::Note)
    );
    println!();
    if problems == 0 {
        println!(
            "{} {}",
            out.paint(Style::Green, "No problems found."),
            out.paint(Style::Dim, &others)
        );
    } else {
        let failing = report
            .checks
            .iter()
            .filter(|check| check.info.severity == Severity::Problem && !check.items.is_empty())
            .count();
        println!(
            "{} {}",
            out.paint(
                Style::Red,
                &format!("{problems} problem(s) found across {failing} check(s).")
            ),
            out.paint(Style::Dim, &others)
        );
    }
}

/// The whole report as one JSON document.
///
/// Stable by construction: `checks` is an object keyed by check name, which
/// `serde_json` keeps sorted, and every check's items come out of core in a
/// fixed order. Every selected check is present, a clean one with `count: 0`
/// and a skipped one with `skipped` set to the reason.
fn to_json(
    config: &Config,
    library: &Library,
    index: &PlaylistIndex,
    report: &Report,
) -> serde_json::Value {
    let checks: serde_json::Map<String, serde_json::Value> = report
        .checks
        .iter()
        .map(|check| {
            (
                check.info.name.to_owned(),
                serde_json::json!({
                    "group": check.info.group,
                    "severity": check.info.severity,
                    "about": check.info.about,
                    "count": check.items.len(),
                    "skipped": check.skipped,
                    "items": check.items,
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
        "problems": report.count(Severity::Problem),
        "warnings": report.count(Severity::Warning),
        "notes": report.count(Severity::Note),
        "checks": checks,
    })
}
