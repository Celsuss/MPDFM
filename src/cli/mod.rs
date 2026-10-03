//! Argument definitions and dispatch.
//!
//! Command bodies land in later tasks; every one of them currently reports
//! `not implemented` and the task that owns it, rather than panicking.

mod config;

use std::process::ExitCode;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use clap::{ArgAction, Args, Parser, Subcommand};
use mpdfm_core::Error;
use mpdfm_core::config::{ConfigWarning, Env, Overrides};

/// Success.
pub const EXIT_OK: ExitCode = ExitCode::SUCCESS;
/// An unexpected error. Conflict (2) and declined (3) codes arrive with task 15.
pub const EXIT_ERROR: ExitCode = ExitCode::FAILURE;

/// Edit tags and re-organize an MPD music library without breaking playlists.
#[derive(Debug, Parser)]
#[command(name = "mpdfm", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub globals: Globals,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Flags that apply to every subcommand. Grouped so they do not interleave with
/// a subcommand's own options in `--help`.
#[derive(Debug, Args)]
#[command(next_help_heading = "Global options")]
pub struct Globals {
    /// Music library root; overrides config and mpd.conf.
    #[arg(long, value_name = "DIR", global = true)]
    pub music_dir: Option<Utf8PathBuf>,

    /// Directory holding MPD's .m3u playlists.
    #[arg(long, value_name = "DIR", global = true)]
    pub playlist_dir: Option<Utf8PathBuf>,

    /// MPDFM config file (default: $XDG_CONFIG_HOME/mpdfm/config.toml).
    #[arg(long, value_name = "FILE", global = true)]
    pub config: Option<Utf8PathBuf>,

    /// Never talk to the MPD daemon.
    #[arg(long, global = true)]
    pub no_mpd: bool,

    /// Print more detail; repeat for more still.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Subcommands. `mpdfm` with none launches the TUI.
// Argument enums are parsed once per process; the size difference between
// variants is not worth an extra allocation and a `Box` clap cannot flatten.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show where MPDFM is looking and why.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Survey the library: counts by format, album dirs, warnings.
    Scan,

    /// Report library and playlist health.
    Doctor,

    /// Move or rename a file or directory, rewriting every reference to it.
    Move {
        /// Source, relative to the music dir or absolute inside it.
        src: Utf8PathBuf,
        /// Destination, relative to the music dir or absolute inside it.
        dst: Utf8PathBuf,
        /// Show the preview and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Re-file tracks into template-derived paths.
    Organize {
        /// Subtree to organize (default: the whole library).
        path: Option<Utf8PathBuf>,
        /// Path template, e.g. "{genre}/{albumartist}/{year} - {album}/{track:02} {title}".
        #[arg(long, short = 't', value_name = "TEMPLATE")]
        template: String,
        /// Show the preview and write nothing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Read and write audio tags.
    Tag {
        #[command(subcommand)]
        command: TagCommand,
    },

    /// Reverse a committed transaction.
    Undo {
        /// Transaction to reverse (default: the most recent).
        txid: Option<String>,
    },

    /// Finish or roll back a transaction that a crash left pending.
    Recover,
}

/// `mpdfm config …`
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print every resolved setting with the source that supplied it.
    Show,
}

/// `mpdfm tag …`
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum TagCommand {
    /// Print the tags of a file, or of every audio file in a directory.
    Show {
        /// File or directory to read.
        path: Utf8PathBuf,
    },

    /// Write tags.
    Set {
        /// File or directory to write.
        path: Utf8PathBuf,
        #[command(flatten)]
        fields: TagFields,
    },
}

/// The fields `tag set` can write. Task 19 owns the full set.
#[derive(Debug, Args)]
pub struct TagFields {
    /// Track title.
    #[arg(long, value_name = "VALUE")]
    pub title: Option<String>,
    /// Track artist.
    #[arg(long, value_name = "VALUE")]
    pub artist: Option<String>,
    /// Album artist.
    #[arg(long, value_name = "VALUE")]
    pub album_artist: Option<String>,
    /// Album name.
    #[arg(long, value_name = "VALUE")]
    pub album: Option<String>,
    /// Release year.
    #[arg(long, value_name = "VALUE")]
    pub year: Option<String>,
    /// Track number.
    #[arg(long, value_name = "VALUE")]
    pub track: Option<String>,
    /// Disc number.
    #[arg(long, value_name = "VALUE")]
    pub disc: Option<String>,
    /// Genre.
    #[arg(long, value_name = "VALUE")]
    pub genre: Option<String>,
    /// Comment.
    #[arg(long, value_name = "VALUE")]
    pub comment: Option<String>,
    /// Composer.
    #[arg(long, value_name = "VALUE")]
    pub composer: Option<String>,
    /// Remove a field entirely; repeatable.
    #[arg(long, value_name = "FIELD")]
    pub clear: Vec<String>,
}

impl TagFields {
    /// The requested edits as `field=value` (or `field=<cleared>`) pairs, in the
    /// order they are displayed.
    pub fn edits(&self) -> Vec<String> {
        let named = [
            ("title", &self.title),
            ("artist", &self.artist),
            ("albumartist", &self.album_artist),
            ("album", &self.album),
            ("year", &self.year),
            ("track", &self.track),
            ("disc", &self.disc),
            ("genre", &self.genre),
            ("comment", &self.comment),
            ("composer", &self.composer),
        ];
        named
            .into_iter()
            .filter_map(|(name, value)| value.as_ref().map(|v| format!("{name}={v}")))
            .chain(self.clear.iter().map(|f| format!("{f}=<cleared>")))
            .collect()
    }
}

impl Cli {
    /// Write a line to stderr when `-v` was given.
    pub fn trace(&self, msg: impl std::fmt::Display) {
        if self.globals.verbose > 0 {
            eprintln!("mpdfm: {msg}");
        }
    }
}

impl Globals {
    /// The part of the configuration the command line supplies.
    fn overrides(&self) -> Overrides {
        Overrides {
            music_dir: self.music_dir.clone(),
            playlist_dir: self.playlist_dir.clone(),
            config_file: self.config.clone(),
            no_mpd: self.no_mpd,
        }
    }

    /// The globals, as given on the command line alone — traced before any file
    /// is read, so `-v` shows the input to resolution as well as its result.
    fn describe(&self) -> String {
        fn show(value: &Option<Utf8PathBuf>) -> &str {
            value.as_deref().map_or("-", Utf8Path::as_str)
        }
        format!(
            "music_dir={} playlist_dir={} config={} no_mpd={} json={} verbose={}",
            show(&self.music_dir),
            show(&self.playlist_dir),
            show(&self.config),
            self.no_mpd,
            self.json,
            self.verbose,
        )
    }
}

/// Print the warnings resolution collected.
///
/// They go to stderr so that `--json` output on stdout stays machine-readable,
/// and the routine ones (see [`ConfigWarning::is_routine`]) wait for `-v` so a
/// normal run is quiet.
fn report(cli: &Cli, warnings: &[ConfigWarning]) {
    for warning in warnings {
        if warning.is_routine() {
            cli.trace(warning);
        } else {
            eprintln!("mpdfm: warning: {warning}");
        }
    }
}

/// Parse arguments and run.
pub fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    cli.trace(cli.globals.describe());

    // Resolved once, before anything dispatches: every command downstream
    // shares one canonical library root rather than working one out for itself.
    let (settings, warnings) =
        mpdfm_core::config::resolve(&cli.globals.overrides(), &Env::from_process());
    // `config show` renders the warnings itself, as part of the picture it
    // exists to give; every other command gets them on stderr.
    if !matches!(cli.command, Some(Command::Config { .. })) {
        report(&cli, &warnings);
    }
    cli.trace(format!(
        "music_dir={} (from {})",
        settings.music_dir, settings.sources.music_dir
    ));

    let Some(command) = &cli.command else {
        return crate::tui::run(&cli);
    };

    let (what, task) = match command {
        Command::Config { command } => match command {
            ConfigCommand::Show => return config::show(&cli, &settings, &warnings),
        },
        Command::Scan => ("mpdfm scan", "15-cli-move-and-doctor.md"),
        Command::Doctor => ("mpdfm doctor", "15-cli-move-and-doctor.md"),
        Command::Move {
            src,
            dst,
            dry_run,
            yes,
        } => {
            cli.trace(format!("move {src} -> {dst} (dry_run={dry_run} yes={yes})"));
            ("mpdfm move", "15-cli-move-and-doctor.md")
        }
        Command::Organize {
            path,
            template,
            dry_run,
        } => {
            let path = path.as_deref().map_or(".", Utf8Path::as_str);
            cli.trace(format!(
                "organize {path} as {template:?} (dry_run={dry_run})"
            ));
            ("mpdfm organize", "28-organize-command.md")
        }
        Command::Tag { command } => match command {
            TagCommand::Show { path } => {
                cli.trace(format!("tag show {path}"));
                ("mpdfm tag show", "19-cli-tags.md")
            }
            TagCommand::Set { path, fields } => {
                cli.trace(format!("tag set {path}: {}", fields.edits().join(" ")));
                ("mpdfm tag set", "19-cli-tags.md")
            }
        },
        // The engine is `mpdfm_core::journal::{undo, recover}` as of task 12;
        // what is missing is the command around it, which task 15 owns together
        // with `move` and the confirmation prompt.
        Command::Undo { txid } => {
            cli.trace(format!("undo {}", txid.as_deref().unwrap_or("<latest>")));
            ("mpdfm undo", "15-cli-move-and-doctor.md")
        }
        Command::Recover => ("mpdfm recover", "15-cli-move-and-doctor.md"),
    };

    Err(Error::not_implemented(what, task).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn args_are_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_planned_subcommand_is_reachable() {
        let cmd = Cli::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        for expected in [
            "config", "scan", "doctor", "move", "organize", "tag", "undo", "recover",
        ] {
            assert!(names.contains(&expected), "missing subcommand `{expected}`");
        }
    }

    #[test]
    fn config_show_is_reachable_and_takes_the_global_flags() {
        let cli = Cli::try_parse_from(["mpdfm", "config", "show", "--json"]).unwrap();
        assert!(cli.globals.json);
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::Show
            })
        ));
    }

    #[test]
    fn globals_become_the_overrides_resolution_starts_from() {
        let cli = Cli::try_parse_from([
            "mpdfm",
            "--music-dir",
            "/srv/music",
            "--config",
            "~/alt.toml",
            "--no-mpd",
            "config",
            "show",
        ])
        .unwrap();

        assert_eq!(
            cli.globals.overrides(),
            Overrides {
                music_dir: Some(Utf8PathBuf::from("/srv/music")),
                playlist_dir: None,
                config_file: Some(Utf8PathBuf::from("~/alt.toml")),
                no_mpd: true,
            }
        );
    }

    #[test]
    fn json_is_global_so_it_works_after_a_subcommand() {
        let cli = Cli::try_parse_from(["mpdfm", "scan", "--json"]).unwrap();
        assert!(cli.globals.json);
        assert!(matches!(cli.command, Some(Command::Scan)));
    }

    #[test]
    fn tag_set_lists_only_the_requested_edits() {
        let cli = Cli::try_parse_from([
            "mpdfm",
            "tag",
            "set",
            "rock/a.mp3",
            "--genre",
            "Hip Hop",
            "--clear",
            "comment",
        ])
        .unwrap();
        let Some(Command::Tag {
            command: TagCommand::Set { fields, .. },
        }) = cli.command
        else {
            panic!("expected `tag set`");
        };
        assert_eq!(fields.edits(), ["genre=Hip Hop", "comment=<cleared>"]);
    }
}
