//! Argument definitions and dispatch.
//!
//! One module per command, and this file is the switchboard: it parses the
//! arguments, resolves the configuration **once** so that every command
//! downstream shares one canonical library root, and dispatches.
//!
//! [`crate::output`] holds the exit codes and the output mode, because those are
//! a contract shared by every command rather than a detail of any one of them.

mod completions;
mod config;
mod cover;
mod doctor;
mod examples;
#[path = "move.rs"]
mod r#move;
mod mpd;
mod organize;
mod scan;
mod tag;
mod undo;

use std::process::ExitCode;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{ArgAction, Args, Parser, Subcommand};
use mpdfm_core::config::{ConfigWarning, Env, Overrides};
use mpdfm_core::paths::{self, RelPath};

use crate::output::Out;

/// The exit-code table `mpdfm --help` ends with.
///
/// The codes are part of the contract, not an implementation detail: a script
/// has to be able to tell "I refused" from "I broke". [`Exit`] is where they are
/// defined; this is where a user finds them.
const EXIT_CODES: &str = "Exit codes:
  0  success
  1  unexpected error
  2  refused before anything was written (a conflict, or the library changed
     since the preview)
  3  you declined at the prompt

Commands that change the library ask first. Pass --yes to skip the prompt,
which is required when stdin is not a terminal.";

/// Edit tags and re-organize an MPD music library without breaking playlists.
#[derive(Debug, Parser)]
#[command(
    name = "mpdfm",
    version,
    about,
    long_about = None,
    after_help = EXIT_CODES
)]
pub struct Cli {
    #[command(flatten)]
    pub globals: Globals,

    #[command(flatten)]
    pub tui: TuiArgs,

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

/// Flags for the TUI, which is what `mpdfm` with no subcommand runs.
///
/// Not `global = true`, unlike [`Globals`]: they mean nothing to a subcommand, and
/// a `--log` that silently did nothing on `mpdfm scan` would be worse than one
/// that is rejected. Both exist for debugging a program that owns the screen and
/// therefore cannot be debugged by printing to it.
#[derive(Debug, Args)]
#[command(next_help_heading = "TUI options")]
pub struct TuiArgs {
    /// Draw in the current screen instead of the alternate one.
    ///
    /// The frames stay in scrollback, so the last one before a crash can be read
    /// afterwards — which is exactly what the alternate screen throws away.
    #[arg(long)]
    pub no_alt_screen: bool,

    /// Append the TUI's diagnostics to FILE.
    ///
    /// A file and never the terminal: while the TUI is drawing, anything written to
    /// stdout or stderr lands in the middle of a frame. `-v` is silent inside the
    /// TUI for the same reason.
    #[arg(long, value_name = "FILE")]
    pub log: Option<Utf8PathBuf>,
}

/// Subcommands. `mpdfm` with none launches the TUI.
// Argument enums are parsed once per process; the size difference between
// variants is not worth an extra allocation and a `Box` clap cannot flatten.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show where MPDFM is looking and why.
    #[command(after_help = examples::CONFIG)]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Survey the library: counts by format, album dirs, warnings.
    #[command(after_help = examples::SCAN)]
    Scan,

    /// Report library and playlist health.
    #[command(after_help = examples::DOCTOR)]
    Doctor(DoctorArgs),

    /// Move or rename a file or directory, rewriting every reference to it.
    #[command(after_help = examples::MOVE)]
    Move(MoveArgs),

    /// Re-file tracks into template-derived paths, rewriting every reference.
    #[command(after_help = examples::ORGANIZE)]
    Organize(OrganizeArgs),

    /// Read and write audio tags.
    #[command(after_help = examples::TAG)]
    Tag {
        #[command(subcommand)]
        command: TagCommand,
    },

    /// See, extract and set album art.
    #[command(after_help = examples::COVER)]
    Cover {
        #[command(subcommand)]
        command: CoverCommand,
    },

    /// Reverse a committed transaction.
    #[command(after_help = examples::UNDO)]
    Undo(UndoArgs),

    /// Finish or roll back a transaction that a crash left pending.
    #[command(after_help = examples::RECOVER)]
    Recover(RecoverArgs),

    /// Print a shell completion script (bash, zsh, fish, elvish, powershell).
    #[command(after_help = examples::COMPLETIONS)]
    Completions(CompletionsArgs),

    /// Print the mpdfm(1) man page, or write it and one per subcommand to a directory.
    #[command(after_help = examples::MAN)]
    Man(ManArgs),
}

/// `mpdfm move <SRC> <DST>`
///
/// `SRC` and `DST` are both **relative to the music directory**, or absolute
/// paths inside it. `DST` is the full destination path — `move a/x b/x` — with
/// one exception, for a file: a `DST` that ends in `/` or is an existing
/// directory is where it goes *into*, so `move a/x.mp3 b/` is `b/x.mp3`, as
/// with `mv`. For a directory a trailing slash is ignored, because shell
/// completion adds one to every directory name and moving onto an existing
/// directory already means `--merge`.
#[derive(Debug, Args)]
pub struct MoveArgs {
    /// Source, relative to the music dir or absolute inside it.
    pub src: Utf8PathBuf,

    /// Destination, relative to the music dir or absolute inside it.
    pub dst: Utf8PathBuf,

    /// Show the preview and write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt. Required when stdin is not a terminal.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Let a directory move land in a directory that already exists, moving
    /// what does not collide. Nothing is ever overwritten either way.
    #[arg(long)]
    pub merge: bool,

    /// Hash every cross-device copy and read it back. Costs a second read of
    /// each copied file.
    #[arg(long)]
    pub verify: bool,
}

/// `mpdfm organize [PATH]...`
///
/// Each `PATH` is a file or a directory, relative to the music directory or
/// absolute inside it; a directory means every audio file below it. No `PATH`
/// means the whole library. Files the template cannot place — a tag it needs is
/// missing — are left where they are and listed at the end.
#[derive(Debug, Args)]
pub struct OrganizeArgs {
    /// Files or directories to organize (default: the whole library).
    pub paths: Vec<Utf8PathBuf>,

    /// Path template (default: `organize_template` from the config), e.g.
    /// "{genre}/{albumartist}/{year} - {album}/{track:02} {title}".
    #[arg(long, short = 't', value_name = "TEMPLATE")]
    pub template: Option<String>,

    /// Show the preview and write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt. Required when stdin is not a terminal.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Only move files whose tags are complete for the template — no `{x?}`
    /// segment left out, no `{x|default}` used — and that are not already in
    /// place. The incremental way to work through a large library.
    #[arg(long)]
    pub only_missing: bool,

    /// Move the tracks only; cover art, .nfo, .cue and other aux files stay
    /// where they are.
    #[arg(long)]
    pub no_aux: bool,

    /// Stage at most N tracks' worth of moves, for a cautious first run.
    #[arg(long, value_name = "N")]
    pub limit: Option<std::num::NonZeroUsize>,

    /// Hash every cross-device copy and read it back. Costs a second read of
    /// each copied file.
    #[arg(long)]
    pub verify: bool,
}

/// `mpdfm doctor [--check NAME]... [--full] [--deep]`
///
/// Every check runs unless `--check` names some. A name is a check's own
/// (`broken-references`) or a group's (`references`, `tags`, `filesystem`,
/// `duplicates`); `--json` lists every check's name under `checks`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Run only this check, or this group of checks; repeatable, or
    /// comma-separated.
    #[arg(long, value_name = "NAME", value_delimiter = ',')]
    pub check: Vec<String>,

    /// List every finding instead of the first ten of each check.
    #[arg(long)]
    pub full: bool,

    /// Also find byte-identical audio files. Reads every same-sized file in
    /// full, which on a large library is minutes of I/O.
    #[arg(long)]
    pub deep: bool,
}

/// `mpdfm undo [TXID]`
#[derive(Debug, Args)]
pub struct UndoArgs {
    /// Transaction to reverse (default: the most recent undoable one).
    pub txid: Option<String>,

    /// List every transaction and whether it can be undone, and do nothing
    /// else.
    #[arg(long, conflicts_with_all = ["txid", "force", "yes"])]
    pub list: bool,

    /// Reverse everything that is still safe to reverse, skipping and reporting
    /// whatever has changed since.
    #[arg(long)]
    pub force: bool,

    /// Skip the confirmation prompt. Required when stdin is not a terminal.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// `mpdfm recover [TXID]`
#[derive(Debug, Args)]
pub struct RecoverArgs {
    /// Transaction to deal with (default: every unfinished one, newest first).
    pub txid: Option<String>,

    /// Finish the transaction instead of rolling it back. Rolling back is the
    /// default because it is the safe one.
    #[arg(long)]
    pub forward: bool,

    /// Act on everything that is still safe to act on, skipping and reporting
    /// whatever cannot be told apart by looking.
    #[arg(long)]
    pub force: bool,

    /// Skip the confirmation prompt. Required when stdin is not a terminal.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// `mpdfm completions <SHELL>`
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// The shell to generate for.
    pub shell: clap_complete::Shell,
}

/// `mpdfm man [--dir DIR]`
#[derive(Debug, Args)]
pub struct ManArgs {
    /// Write `mpdfm.1` and one `mpdfm-<subcommand>.1` per subcommand into DIR
    /// (created if missing) instead of printing `mpdfm.1`.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<Utf8PathBuf>,
}

/// `mpdfm cover …`
///
/// `ALBUM_DIR` is a directory relative to the music directory, or absolute
/// inside it; the audio files directly in it are the album. Every write is one
/// transaction, previewed first and reversible with `mpdfm undo`.
#[derive(Debug, Subcommand)]
pub enum CoverCommand {
    /// Show embedded and loose art for a track or an album directory, and what
    /// disagrees between them.
    #[command(after_help = examples::COVER_SHOW)]
    Show(CoverShowArgs),

    /// Embed an image as the front cover of every track in an album, replacing
    /// any front cover already there.
    #[command(after_help = examples::COVER_SET)]
    Set(CoverSetArgs),

    /// Write the album's embedded cover out as an image file beside its tracks.
    #[command(after_help = examples::COVER_EXTRACT)]
    Extract(CoverExtractArgs),

    /// Remove the embedded front cover from every track in an album, leaving any
    /// other pictures.
    #[command(after_help = examples::COVER_REMOVE)]
    Remove(CoverRemoveArgs),
}

/// `mpdfm cover show <PATH>`
#[derive(Debug, Args)]
pub struct CoverShowArgs {
    /// A track, or an album directory.
    pub path: Utf8PathBuf,
}

/// The flags every cover write takes.
#[derive(Debug, Args)]
pub struct CoverWriteFlags {
    /// Show the preview and write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt. Required when stdin is not a terminal.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// `mpdfm cover set <ALBUM_DIR> [IMAGE]`
#[derive(Debug, Args)]
pub struct CoverSetArgs {
    /// The album directory.
    pub album_dir: Utf8PathBuf,

    /// The image: a path, or the name of an image in the album directory.
    /// Default: the album's own cover.jpg, folder.jpg or front.jpg, in that
    /// order of preference.
    pub image: Option<Utf8PathBuf>,

    /// Re-encode an image larger than PX on its longer side as a JPEG that
    /// fits, keeping its aspect ratio. Off unless given: re-encoding is lossy.
    #[arg(long, value_name = "PX")]
    pub max_dimension: Option<std::num::NonZeroU32>,

    #[command(flatten)]
    pub flags: CoverWriteFlags,
}

/// `mpdfm cover extract <ALBUM_DIR>`
#[derive(Debug, Args)]
pub struct CoverExtractArgs {
    /// The album directory.
    pub album_dir: Utf8PathBuf,

    /// File name to write in the album directory (default: cover.jpg, or
    /// cover.png for a PNG).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    #[command(flatten)]
    pub flags: CoverWriteFlags,
}

/// `mpdfm cover remove <ALBUM_DIR>`
#[derive(Debug, Args)]
pub struct CoverRemoveArgs {
    /// The album directory.
    pub album_dir: Utf8PathBuf,

    #[command(flatten)]
    pub flags: CoverWriteFlags,
}

/// `mpdfm config …`
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print every resolved setting with the source that supplied it.
    #[command(after_help = examples::CONFIG_SHOW)]
    Show,
}

/// `mpdfm tag …`
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum TagCommand {
    /// Print the tags of a file, or of every audio file in a directory.
    #[command(after_help = examples::TAG_SHOW)]
    Show(TagShowArgs),

    /// Write tags to files or whole directories, previewing the change first.
    #[command(after_help = examples::TAG_SET)]
    Set(TagSetArgs),

    /// Show what `tag set` would change, and write nothing.
    #[command(after_help = examples::TAG_DIFF)]
    Diff(TagSetArgs),
}

/// `mpdfm tag show <PATH>...`
///
/// Each `PATH` is a file or a directory, relative to the music directory or
/// absolute inside it. A directory contributes its own audio files; `-r` goes
/// below it as well.
#[derive(Debug, Args)]
pub struct TagShowArgs {
    /// Files or directories to read.
    #[arg(required = true)]
    pub paths: Vec<Utf8PathBuf>,

    /// Include audio files in subdirectories.
    #[arg(long, short = 'r')]
    pub recursive: bool,
}

/// `mpdfm tag set <PATH>...`, and `mpdfm tag diff <PATH>...`, which takes the
/// same arguments and writes nothing.
#[derive(Debug, Args)]
pub struct TagSetArgs {
    /// Files or directories to write.
    #[arg(required = true)]
    pub paths: Vec<Utf8PathBuf>,

    #[command(flatten)]
    pub fields: TagFields,

    /// Include audio files in subdirectories.
    #[arg(long, short = 'r')]
    pub recursive: bool,

    /// Show the preview and write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt. Required when stdin is not a terminal.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// The fields `tag set` can write, and the actions it can run.
#[derive(Debug, Args)]
pub struct TagFields {
    /// Track title.
    #[arg(long, value_name = "VALUE")]
    pub title: Option<String>,
    /// Track artist.
    #[arg(long, value_name = "VALUE")]
    pub artist: Option<String>,
    /// Album artist.
    #[arg(long, alias = "albumartist", value_name = "VALUE")]
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

    /// Number the selected files 1..n in the order they are listed, and set the
    /// total on each.
    #[arg(long)]
    pub renumber_tracks: bool,

    /// Take each file's title from its own name, stripping a leading track
    /// number and the extension.
    #[arg(long)]
    pub title_from_filename: bool,

    /// Give each file its own artist as its album artist.
    #[arg(long)]
    pub album_artist_from_artist: bool,

    /// Remove the comment from every selected file.
    #[arg(long)]
    pub strip_comment: bool,

    /// Strip leading and trailing space from every text field that has any.
    #[arg(long)]
    pub trim_whitespace: bool,
}

impl TagFields {
    /// Whether nothing at all was asked for.
    pub fn is_empty(&self) -> bool {
        self.edits().is_empty() && self.actions().is_empty()
    }

    /// The named actions that were asked for, as the flags spell them.
    pub fn actions(&self) -> Vec<&'static str> {
        [
            (self.renumber_tracks, "--renumber-tracks"),
            (self.title_from_filename, "--title-from-filename"),
            (self.album_artist_from_artist, "--album-artist-from-artist"),
            (self.strip_comment, "--strip-comment"),
            (self.trim_whitespace, "--trim-whitespace"),
        ]
        .into_iter()
        .filter_map(|(asked, name)| asked.then_some(name))
        .collect()
    }

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

    // Pure functions of the argument definitions: they need no library, no
    // configuration and no journal, and must work on a machine where none of
    // those exist yet — which is exactly where a package build runs them.
    match &cli.command {
        Some(Command::Completions(args)) => return completions::script(args),
        Some(Command::Man(args)) => return completions::man(args),
        _ => {}
    }
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
        return crate::tui::run(&cli, &settings);
    };

    // Worked out once, here, so that no command can reach a different answer
    // than the confirmation prompt did about whether anyone is listening.
    let out = Out::detect(cli.globals.json);

    match command {
        Command::Config { command } => match command {
            ConfigCommand::Show => config::show(&cli, &settings, &warnings),
        },
        Command::Scan => scan::run(&cli, &settings, &out),
        Command::Doctor(args) => doctor::run(&cli, &settings, &out, args),
        Command::Move(args) => r#move::run(&cli, &settings, &out, args),
        Command::Organize(args) => organize::run(&cli, &settings, &out, args),
        Command::Undo(args) => undo::run(&cli, &settings, &out, args),
        Command::Recover(args) => undo::recover(&cli, &settings, &out, args),
        Command::Tag { command } => match command {
            TagCommand::Show(args) => tag::show(&cli, &settings, &out, args),
            TagCommand::Set(args) => tag::set(&cli, &settings, &out, args, false),
            TagCommand::Diff(args) => tag::set(&cli, &settings, &out, args, true),
        },
        Command::Completions(_) | Command::Man(_) => unreachable!("handled before resolution"),
        Command::Cover { command } => match command {
            CoverCommand::Show(args) => cover::show(&cli, &settings, &out, args),
            CoverCommand::Set(args) => cover::set(&cli, &settings, &out, args),
            CoverCommand::Extract(args) => cover::extract(&cli, &settings, &out, args),
            CoverCommand::Remove(args) => cover::remove(&cli, &settings, &out, args),
        },
    }
}

/// Turn a `SRC`/`DST` argument into a path relative to the music directory.
///
/// Three things the user might type, and what each means:
///
/// - `hiphop/MF DOOM` — relative to `music_dir`, **not** to the working
///   directory. That is what MPD's own paths are relative to, so it is the one
///   reading that makes `mpdfm move` agree with what is in the playlists;
/// - `/home/me/Music/hiphop/MF DOOM` — absolute, and accepted when it is inside
///   the library, because that is what shell completion and a file manager both
///   produce;
/// - either of the above with a trailing `/`, which is ignored.
///
/// # Errors
///
/// A [`PathError`][mpdfm_core::paths::PathError] for a path that cannot name
/// something inside the library — one with a `..` in it, an absolute path
/// elsewhere on the disk, a name that is not valid UTF-8. This is a usability
/// check and not the safety boundary: every filesystem step is guarded again
/// against the root in [`exec_fs`][mpdfm_core::ops::exec_fs], which is where
/// safety invariant 5 actually lives.
fn inside(raw: &Utf8Path, music_dir: &Utf8Path) -> Result<RelPath> {
    let trimmed = raw.as_str().trim_end_matches('/');
    anyhow::ensure!(!trimmed.is_empty(), "{raw} does not name anything");
    let path = Utf8Path::new(trimmed);

    if !path.is_absolute() {
        return RelPath::parse(trimmed)
            .with_context(|| format!("{raw} is not a path inside {music_dir}"));
    }

    // Lexically under the root is the common case, and `music_dir` has already
    // been canonicalized by `config::resolve`.
    if let Ok(rel) = RelPath::from_abs(path, music_dir) {
        return Ok(rel);
    }

    // Not lexically under it: a path typed through a symlink, or with a `..` in
    // the middle. Resolve what exists of it and measure again, which is what
    // `paths::contains` does to decide — so the two cannot disagree.
    anyhow::ensure!(
        paths::contains(music_dir, path),
        "{path} is not inside the music directory {music_dir}"
    );
    let resolved = resolve_existing(path);
    RelPath::from_abs(&resolved, music_dir)
        .with_context(|| format!("{raw} is not a path inside {music_dir}"))
}

/// `path` with its existing part canonicalized, so a symlinked or `..`-laden
/// spelling can be measured against the root.
///
/// A destination does not exist yet, so the file name is kept as typed and only
/// the directory above it is resolved. Anything that cannot be canonicalized at
/// all comes back unchanged, and the caller's `from_abs` then refuses it.
fn resolve_existing(path: &Utf8Path) -> Utf8PathBuf {
    if let Ok(real) = path.canonicalize_utf8() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize_utf8()
            .map_or_else(|_| path.to_owned(), |real| real.join(name)),
        _ => path.to_owned(),
    }
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
            "config",
            "scan",
            "doctor",
            "move",
            "organize",
            "tag",
            "cover",
            "undo",
            "recover",
            "completions",
            "man",
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
    fn tag_set_takes_several_paths_and_the_named_actions() {
        let cli = Cli::try_parse_from([
            "mpdfm",
            "tag",
            "set",
            "a/1.mp3",
            "a/2.mp3",
            "--renumber-tracks",
            "-r",
            "--yes",
        ])
        .unwrap();
        let Some(Command::Tag {
            command: TagCommand::Set(args),
        }) = cli.command
        else {
            panic!("expected `tag set`");
        };
        assert_eq!(args.paths.len(), 2);
        assert!(args.recursive && args.yes);
        assert_eq!(args.fields.actions(), ["--renumber-tracks"]);
        assert!(args.fields.edits().is_empty());
    }

    #[test]
    fn tag_diff_takes_the_same_arguments_as_tag_set() {
        let cli =
            Cli::try_parse_from(["mpdfm", "tag", "diff", "a/1.mp3", "--genre", "Jazz"]).unwrap();
        let Some(Command::Tag {
            command: TagCommand::Diff(args),
        }) = cli.command
        else {
            panic!("expected `tag diff`");
        };
        assert_eq!(args.fields.edits(), ["genre=Jazz"]);
    }

    #[test]
    fn the_tui_flags_parse_and_default_to_the_quiet_alternate_screen() {
        let bare = Cli::try_parse_from(["mpdfm"]).unwrap();
        assert!(bare.command.is_none(), "no subcommand means the TUI");
        assert!(!bare.tui.no_alt_screen);
        assert!(bare.tui.log.is_none());

        let debugging =
            Cli::try_parse_from(["mpdfm", "--no-alt-screen", "--log", "/tmp/mpdfm.log"]).unwrap();
        assert!(debugging.tui.no_alt_screen);
        assert_eq!(debugging.tui.log, Some(Utf8PathBuf::from("/tmp/mpdfm.log")));
    }

    #[test]
    fn the_tui_flags_are_not_global_so_a_subcommand_rejects_them() {
        // They would do nothing on a subcommand, and a flag that silently does
        // nothing is worse than one that is refused.
        Cli::try_parse_from(["mpdfm", "scan", "--log", "/tmp/x"])
            .expect_err("--log belongs to the TUI");
        Cli::try_parse_from(["mpdfm", "scan", "--no-alt-screen"])
            .expect_err("--no-alt-screen belongs to the TUI");
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
            command: TagCommand::Set(args),
        }) = cli.command
        else {
            panic!("expected `tag set`");
        };
        assert_eq!(args.fields.edits(), ["genre=Hip Hop", "comment=<cleared>"]);
        assert_eq!(args.paths, [Utf8PathBuf::from("rock/a.mp3")]);
    }
    /// Every command in the tree, depth first, with the words it is invoked by.
    fn walk(cmd: &clap::Command, path: &str, out: &mut Vec<(String, clap::Command)>) {
        for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            let name = format!("{path} {}", sub.get_name());
            out.push((name.clone(), sub.clone()));
            walk(sub, &name, out);
        }
    }

    #[test]
    fn every_command_has_a_description_and_an_example() {
        let mut all = Vec::new();
        walk(&Cli::command(), "mpdfm", &mut all);
        assert!(all.len() >= 18, "the walk found only {all:?}");

        for (name, cmd) in &all {
            let about = cmd.get_about().map(ToString::to_string).unwrap_or_default();
            assert!(
                about.len() > 15,
                "`{name}` has no real description: {about:?}"
            );

            let after = cmd
                .get_after_help()
                .map(ToString::to_string)
                .unwrap_or_default();
            assert!(
                after.starts_with("Examples:"),
                "`{name}` has no example in its --help"
            );
            // An example must name the command it is an example of.
            assert!(
                after.contains(name.as_str()) || cmd.has_subcommands(),
                "`{name}`'s examples never run it:\n{after}"
            );
        }
    }

    #[test]
    fn every_example_in_the_help_parses() {
        // An example that does not parse is worse than none: the user types it.
        // Each is split the way a shell would, quotes included, and `#` comments
        // dropped.
        let mut all = Vec::new();
        walk(&Cli::command(), "mpdfm", &mut all);
        let mut seen = 0;
        for (name, cmd) in &all {
            let after = cmd
                .get_after_help()
                .map(ToString::to_string)
                .unwrap_or_default();
            for line in after.lines().skip(1).map(str::trim) {
                if !line.starts_with("mpdfm ") {
                    continue;
                }
                let line = line.split(" #").next().unwrap_or(line);
                let argv = shell_words(line);
                // `mpdfm man | man -l -` and `> file` are shell, not arguments.
                let argv: Vec<_> = argv
                    .into_iter()
                    .take_while(|w| w != "|" && w != ">")
                    .collect();
                Cli::try_parse_from(&argv).unwrap_or_else(|e| {
                    panic!("`{line}` (in `{name} --help`) does not parse:\n{e}")
                });
                seen += 1;
            }
        }
        assert!(seen >= 30, "only {seen} examples were checked");
    }

    /// Enough of a shell's word splitting for the examples: whitespace, and
    /// double quotes.
    fn shell_words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut cur = String::new();
        let mut quoted = false;
        let mut started = false;
        for c in line.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    started = true;
                }
                c if c.is_whitespace() && !quoted => {
                    if started {
                        words.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                c => {
                    cur.push(c);
                    started = true;
                }
            }
        }
        if started {
            words.push(cur);
        }
        words
    }
}
