//! Where the library is, where the playlists are, and how MPDFM behaves —
//! worked out once at startup, with nothing for the user to configure.
//!
//! Four sources answer each question, and the first that gives a usable answer
//! wins:
//!
//! 1. a CLI flag (`--music-dir`, `--playlist-dir`)
//! 2. `$XDG_CONFIG_HOME/mpdfm/config.toml`
//! 3. mpd.conf, found by [`mpd_conf_candidates`]
//! 4. a built-in default
//!
//! **First file found, not a merge.** On this machine `~/.config/mpd/mpd.conf`
//! names `~/Music` while `/etc/mpd.conf` names `/var/lib/mpd/...` and has no
//! `music_directory` at all. Merging the two would hand MPDFM a playlist
//! directory from one library and a music directory from another. MPD itself
//! reads exactly one file, so MPDFM does too, and [`ConfigWarning::NoMpdConf`]
//! lists everywhere it looked when there is none.
//!
//! **Warnings, not failures.** A missing mpd.conf, a missing or unreadable
//! `config.toml`, an unknown setting, a value of the wrong type — each produces
//! a [`ConfigWarning`] and resolution carries on to the next source. The only
//! thing worth stopping for is a `music_dir` that does not exist, and even that
//! is deferred: [`resolve`] reports it, and [`Config::require_music_dir`] is
//! what the commands that actually touch the library call.
//!
//! # Expansion is deliberately asymmetric
//!
//! `~` is expanded everywhere. `$VAR` is expanded in MPDFM's own config and in
//! CLI flags, but **not** in mpd.conf, because MPD does not expand it either:
//! given `music_directory "$HOME/Music"`, MPD looks for a directory literally
//! named `$HOME`. Expanding it here would make MPDFM and MPD disagree about
//! where the library is, and that disagreement is the one this module exists to
//! prevent.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};

use crate::mpdconf::MpdConf;

/// The port MPD listens on unless told otherwise.
pub const DEFAULT_MPD_PORT: u16 = 6600;

/// How many committed transactions to keep backups for.
pub const DEFAULT_BACKUP_KEEP: u32 = 50;

/// The path template `organize` uses when none is configured.
pub const DEFAULT_ORGANIZE_TEMPLATE: &str =
    "{genre}/{albumartist}/{year} - {album}/{track:02} {title}";

/// Every key MPDFM's own `config.toml` understands. Anything else is reported
/// as [`ConfigWarning::UnknownKey`] — a typo should be visible, not silently
/// ineffective, and a key from a newer version should not stop an older binary.
pub const CONFIG_KEYS: &[&str] = &[
    "music_dir",
    "playlist_dir",
    "data_dir",
    "state_file",
    "mpd_address",
    "mpd_enabled",
    "rewrite_saved_queue",
    "trigger_update_after_commit",
    "delete_enabled",
    "backup_keep",
    "organize_template",
    "organize_portable_names",
    "id3_version",
];

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// The process environment, as a value.
///
/// Resolution depends on `$HOME` and the `$XDG_*` variables, and tests have to
/// be able to point those at a temp directory. Mutating the real environment
/// would not do — `std::env::set_var` is unsafe under edition 2024 and tests run
/// in parallel — so the environment is passed in instead.
///
/// [`Env::empty`] also has no system mpd.conf, which is what keeps a test from
/// ever reading the real `/etc/mpd.conf` (`docs/PLAN.md` §8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, String>,
    system_mpd_conf: Option<Utf8PathBuf>,
}

impl Env {
    /// The real environment, and the real `/etc/mpd.conf`.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
            system_mpd_conf: Some(Utf8PathBuf::from("/etc/mpd.conf")),
        }
    }

    /// No variables and no system mpd.conf. Build a test environment from here.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Set a variable.
    #[must_use]
    pub fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.vars.insert(name.to_owned(), value.into());
        self
    }

    /// Point the last mpd.conf candidate somewhere other than `/etc/mpd.conf`.
    #[must_use]
    pub fn with_system_mpd_conf(mut self, path: impl Into<Utf8PathBuf>) -> Self {
        self.system_mpd_conf = Some(path.into());
        self
    }

    /// A variable's value, treating the empty string as unset — which is how
    /// the XDG specification says an empty `$XDG_CONFIG_HOME` must be read.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// `$HOME`, if it is set and absolute.
    #[must_use]
    pub fn home(&self) -> Option<&Utf8Path> {
        self.var("HOME")
            .map(Utf8Path::new)
            .filter(|p| p.is_absolute())
    }

    /// `$XDG_CONFIG_HOME`, or `~/.config`.
    #[must_use]
    pub fn config_home(&self) -> Option<Utf8PathBuf> {
        self.xdg("XDG_CONFIG_HOME", ".config")
    }

    /// `$XDG_DATA_HOME`, or `~/.local/share`.
    #[must_use]
    pub fn data_home(&self) -> Option<Utf8PathBuf> {
        self.xdg("XDG_DATA_HOME", ".local/share")
    }

    /// An XDG base directory. A relative value is ignored rather than resolved
    /// against the working directory — the specification requires that, and a
    /// library root that moved with `cd` would be a disaster.
    fn xdg(&self, name: &str, fallback: &str) -> Option<Utf8PathBuf> {
        match self.var(name).map(Utf8PathBuf::from) {
            Some(dir) if dir.is_absolute() => Some(dir),
            _ => Some(self.home()?.join(fallback)),
        }
    }

    /// Expand `~` and, for [`Expand::TildeAndVars`], `$VAR` and `${VAR}`.
    ///
    /// ```
    /// use mpdfm_core::config::{Env, Expand};
    ///
    /// let env = Env::empty()
    ///     .with("HOME", "/home/me")
    ///     .with("XDG_CONFIG_HOME", "/home/me/.config");
    ///
    /// assert_eq!(env.expand("~/Music", Expand::Tilde)?, "/home/me/Music");
    /// assert_eq!(
    ///     env.expand("$XDG_CONFIG_HOME/mpd/playlists", Expand::TildeAndVars)?,
    ///     "/home/me/.config/mpd/playlists"
    /// );
    /// // mpd.conf gets no variable expansion, exactly as MPD gives it none:
    /// assert_eq!(env.expand("$HOME/Music", Expand::Tilde)?, "$HOME/Music");
    /// # Ok::<(), mpdfm_core::config::ExpandError>(())
    /// ```
    pub fn expand(&self, raw: &str, mode: Expand) -> Result<Utf8PathBuf, ExpandError> {
        // Tilde first: it is positional (start of string only), so resolving it
        // before any variable is substituted keeps its meaning independent of
        // what those variables happen to contain.
        let tilded = self.expand_tilde(raw)?;
        let expanded = match mode {
            Expand::Tilde => tilded,
            Expand::TildeAndVars => self.expand_vars(&tilded, raw)?,
        };
        Ok(Utf8PathBuf::from(expanded))
    }

    fn expand_tilde(&self, raw: &str) -> Result<String, ExpandError> {
        let rest = match raw {
            "~" => "",
            _ => match raw.strip_prefix("~/") {
                Some(rest) => rest,
                // `~user` is not supported by MPD either, and guessing another
                // account's home directory is not something to do quietly.
                None if raw.starts_with('~') => {
                    return Err(ExpandError::TildeUser {
                        raw: raw.to_owned(),
                    });
                }
                None => return Ok(raw.to_owned()),
            },
        };
        let home = self.home().ok_or_else(|| ExpandError::NoHome {
            raw: raw.to_owned(),
        })?;
        Ok(home.join(rest).into_string())
    }

    /// `raw` is carried through only so an error names what the user wrote
    /// rather than the half-expanded form.
    fn expand_vars(&self, text: &str, raw: &str) -> Result<String, ExpandError> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find('$') {
            out.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            let (name, tail) = match after.strip_prefix('{') {
                Some(braced) => match braced.split_once('}') {
                    Some((name, tail)) => (name, tail),
                    None => {
                        return Err(ExpandError::UnclosedBrace {
                            raw: raw.to_owned(),
                        });
                    }
                },
                None => {
                    let end = after
                        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            if name.is_empty() {
                // A bare `$` is a legal character in a filename.
                out.push('$');
                rest = after;
                continue;
            }
            let value = self.var(name).ok_or_else(|| ExpandError::UnknownVariable {
                raw: raw.to_owned(),
                name: name.to_owned(),
            })?;
            out.push_str(value);
            rest = tail;
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// How much substitution a value gets. See the [module docs][self] for why
/// mpd.conf gets less than MPDFM's own config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expand {
    /// `~` only — what MPD itself does.
    Tilde,
    /// `~`, `$VAR` and `${VAR}`.
    TildeAndVars,
}

/// Why a configured path could not be turned into a real one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpandError {
    /// `~` with no `$HOME` to expand it to.
    #[error("`{raw}` needs a home directory, but $HOME is unset")]
    NoHome {
        /// The value as written.
        raw: String,
    },

    /// `~someone/...`, which MPDFM does not resolve.
    #[error("`{raw}` uses `~user`, which MPDFM does not expand")]
    TildeUser {
        /// The value as written.
        raw: String,
    },

    /// `$VAR` naming something that is not set.
    #[error("`{raw}` refers to ${name}, which is not set")]
    UnknownVariable {
        /// The value as written.
        raw: String,
        /// The variable that is missing.
        name: String,
    },

    /// `${VAR` with no closing brace.
    #[error("`{raw}` has a `${{` with no closing `}}`")]
    UnclosedBrace {
        /// The value as written.
        raw: String,
    },

    /// Expanded to something that is not an absolute path. Resolving it against
    /// the working directory would make the library move with `cd`.
    #[error("`{raw}` is not an absolute path")]
    NotAbsolute {
        /// The value as written.
        raw: String,
    },
}

// ---------------------------------------------------------------------------
// Where a value came from
// ---------------------------------------------------------------------------

/// Which of the four sources supplied a setting. `mpdfm config show` prints it
/// next to the value, so "why is it looking there?" is answerable without
/// guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A command-line flag, named as the user would type it.
    Flag(&'static str),
    /// MPDFM's own `config.toml`.
    Config {
        /// The file it was read from.
        path: Utf8PathBuf,
        /// The key within it.
        key: &'static str,
    },
    /// The mpd.conf that was found.
    MpdConf {
        /// The file it was read from.
        path: Utf8PathBuf,
        /// The key within it, e.g. `music_directory`.
        key: &'static str,
    },
    /// Nothing configured it; this is MPDFM's built-in default.
    Default,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flag(flag) => write!(f, "{flag}"),
            Self::Config { path, key } | Self::MpdConf { path, key } => {
                write!(f, "{path} ({key})")
            }
            Self::Default => f.write_str("built-in default"),
        }
    }
}

// ---------------------------------------------------------------------------
// Warnings
// ---------------------------------------------------------------------------

/// Something worth telling the user about, none of which stops MPDFM starting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigWarning {
    /// No mpd.conf anywhere on the search path.
    #[error(
        "no mpd.conf found (looked in {}); using defaults",
        .searched.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
    )]
    NoMpdConf {
        /// Every path that was tried, in order.
        searched: Vec<Utf8PathBuf>,
    },

    /// MPDFM has no config file. Entirely normal — it is meant to work without
    /// one — but `config show` should still say so.
    #[error("no MPDFM config at {path}; using mpd.conf and defaults")]
    NoConfigFile {
        /// Where one would have been read from.
        path: Utf8PathBuf,
    },

    /// A file that exists but could not be read.
    #[error("{path}: {message}; ignoring it")]
    Unreadable {
        /// The file.
        path: Utf8PathBuf,
        /// The operating system's explanation.
        message: String,
    },

    /// `config.toml` is not valid TOML.
    #[error("{path}: {message}; ignoring it")]
    MalformedToml {
        /// The file.
        path: Utf8PathBuf,
        /// The parser's explanation.
        message: String,
    },

    /// A key MPDFM does not know. A typo, or a setting from a later version.
    #[error("{path}: unknown setting `{key}`; ignoring it")]
    UnknownKey {
        /// The file.
        path: Utf8PathBuf,
        /// The key as written.
        key: String,
    },

    /// A known key holding the wrong kind of value.
    #[error("{path}: `{key}` should be {expected}, found {found}; ignoring it")]
    WrongType {
        /// The file.
        path: Utf8PathBuf,
        /// The key.
        key: &'static str,
        /// What was wanted, e.g. `a string`.
        expected: &'static str,
        /// What was there, e.g. `integer`.
        found: &'static str,
    },

    /// A value that could not be used, so the next source was tried.
    #[error("{origin}: {message}; falling back")]
    BadValue {
        /// Where the unusable value came from.
        origin: Source,
        /// Why it could not be used.
        message: String,
    },

    /// A root directory that is not there, or not a directory.
    #[error(transparent)]
    Root(#[from] RootProblem),

    /// Something odd in the mpd.conf that was read.
    #[error("{path}: {warning}")]
    MpdConf {
        /// The file.
        path: Utf8PathBuf,
        /// What the parser noticed.
        warning: crate::mpdconf::MpdConfWarning,
    },
}

impl ConfigWarning {
    /// Whether this is the normal state of affairs rather than something the
    /// user should act on.
    ///
    /// Having no `config.toml` is the expected case — MPDFM is built to need
    /// none — so front-ends keep that one for verbose output and show the rest
    /// every run.
    #[must_use]
    pub fn is_routine(&self) -> bool {
        matches!(self, Self::NoConfigFile { .. })
    }
}

/// A root directory MPDFM cannot work with.
///
/// This is both a [`ConfigWarning`] at startup and the error
/// [`Config::require_music_dir`] returns: every path operation downstream
/// assumes the root exists, so the command that is about to walk the library is
/// the one that should refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{setting} `{path}` {defect} (set by {origin})")]
pub struct RootProblem {
    /// The setting's name, e.g. `music_dir`.
    pub setting: &'static str,
    /// The path as resolved.
    pub path: Utf8PathBuf,
    /// What is wrong with it.
    pub defect: RootDefect,
    /// Where the path came from, so the user knows which file to edit.
    pub origin: Source,
}

/// What is wrong with a root directory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RootDefect {
    /// Nothing is there.
    #[error("does not exist")]
    Missing,
    /// Something is there, but it is not a directory.
    #[error("is not a directory")]
    NotADirectory,
    /// It could not be inspected at all.
    #[error("cannot be read ({0})")]
    Unreadable(String),
}

// ---------------------------------------------------------------------------
// MPD's address
// ---------------------------------------------------------------------------

/// How to reach the MPD daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MpdAddress {
    /// A host and port.
    Tcp {
        /// Hostname or IP literal, without brackets.
        host: String,
        /// TCP port.
        port: u16,
    },
    /// A unix socket path, including the abstract `@name` form.
    Unix(Utf8PathBuf),
}

/// Why an address string could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// Nothing to parse.
    #[error("empty address")]
    Empty,
    /// The part after the last `:` is not a port number.
    #[error("`{port}` is not a port number")]
    BadPort {
        /// What was there instead.
        port: String,
    },
    /// `[::1` with no `]`.
    #[error("`{raw}` is missing the `]` of its IPv6 literal")]
    UnclosedBracket {
        /// The address as written.
        raw: String,
    },
}

impl MpdAddress {
    /// Parse an address, filling in `default_port` when the string carries none.
    ///
    /// Splitting the port out this way is what lets mpd.conf's separate
    /// `bind_to_address` and `port` keys combine, while an explicit port in the
    /// address still wins.
    ///
    /// ```
    /// use mpdfm_core::config::MpdAddress;
    ///
    /// let addr = MpdAddress::parse("127.0.0.1", 6600)?;
    /// assert_eq!(addr.to_string(), "127.0.0.1:6600");
    /// assert_eq!(MpdAddress::parse("[::1]:6601", 6600)?.to_string(), "[::1]:6601");
    /// assert!(matches!(
    ///     MpdAddress::parse("/run/mpd/socket", 6600)?,
    ///     MpdAddress::Unix(_)
    /// ));
    /// # Ok::<(), mpdfm_core::config::AddressError>(())
    /// ```
    pub fn parse(raw: &str, default_port: u16) -> Result<Self, AddressError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(AddressError::Empty);
        }
        // `@` is Linux's abstract socket namespace, which MPD supports.
        if raw.starts_with('/') || raw.starts_with('@') || raw.starts_with('~') {
            return Ok(Self::Unix(Utf8PathBuf::from(raw)));
        }
        if let Some(after_bracket) = raw.strip_prefix('[') {
            let (host, tail) =
                after_bracket
                    .split_once(']')
                    .ok_or_else(|| AddressError::UnclosedBracket {
                        raw: raw.to_owned(),
                    })?;
            let port = match tail.strip_prefix(':') {
                Some(port) => parse_port(port)?,
                _ => default_port,
            };
            return Ok(Self::Tcp {
                host: host.to_owned(),
                port,
            });
        }
        match raw.match_indices(':').count() {
            0 => Ok(Self::Tcp {
                host: raw.to_owned(),
                port: default_port,
            }),
            1 => {
                let (host, port) = raw.split_once(':').expect("one `:` was just counted");
                Ok(Self::Tcp {
                    host: host.to_owned(),
                    port: parse_port(port)?,
                })
            }
            // More than one `:` and no brackets: a bare IPv6 literal, which has
            // no room for a port.
            _ => Ok(Self::Tcp {
                host: raw.to_owned(),
                port: default_port,
            }),
        }
    }
}

fn parse_port(text: &str) -> Result<u16, AddressError> {
    text.parse().map_err(|_| AddressError::BadPort {
        port: text.to_owned(),
    })
}

impl std::fmt::Display for MpdAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Bracket an IPv6 literal so the output can be parsed back.
            Self::Tcp { host, port } if host.contains(':') => write!(f, "[{host}]:{port}"),
            Self::Tcp { host, port } => write!(f, "{host}:{port}"),
            Self::Unix(path) => f.write_str(path.as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// The resolved configuration
// ---------------------------------------------------------------------------

/// What the command line contributed. Everything else is read from files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `--music-dir`.
    pub music_dir: Option<Utf8PathBuf>,
    /// `--playlist-dir`.
    pub playlist_dir: Option<Utf8PathBuf>,
    /// `--config`; when absent, `$XDG_CONFIG_HOME/mpdfm/config.toml` is used.
    pub config_file: Option<Utf8PathBuf>,
    /// `--no-mpd`. Only ever forces the daemon off, never on.
    pub no_mpd: bool,
}

/// Everything MPDFM needs to know before it does anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The library root, canonical and absolute when it exists.
    pub music_dir: Utf8PathBuf,
    /// Where MPD keeps its `.m3u` files.
    pub playlist_dir: Utf8PathBuf,
    /// MPDFM's own journal and backups (`docs/PLAN.md` §7).
    pub data_dir: Utf8PathBuf,
    /// MPD's state file, holding the saved queue. `None` when no mpd.conf named
    /// one.
    pub state_file: Option<Utf8PathBuf>,
    /// How to reach the daemon.
    pub mpd_address: MpdAddress,
    /// Whether to talk to it at all.
    pub mpd_enabled: bool,
    /// Whether a move rewrites the saved queue in the state file.
    pub rewrite_saved_queue: bool,
    /// Whether a commit asks MPD to rescan.
    pub trigger_update_after_commit: bool,
    /// When false, MPDFM never deletes anything.
    pub delete_enabled: bool,
    /// How many transactions to retain backups for.
    pub backup_keep: u32,
    /// The default path template for `organize`.
    pub organize_template: String,
    /// Whether `organize` also replaces the characters FAT and NTFS refuse
    /// (`: * ? " < > | \` and control characters), so the library survives a
    /// copy onto a phone or a USB stick. On by default.
    pub organize_portable_names: bool,
    /// Which ID3v2 revision a tag write leaves an mp3 in.
    pub id3_version: crate::tags::Id3Version,
    /// Where each of the above came from.
    pub sources: Sources,
}

/// The [`Source`] of each field of [`Config`], by the same name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    /// Where [`Config::music_dir`] came from.
    pub music_dir: Source,
    /// Where [`Config::playlist_dir`] came from.
    pub playlist_dir: Source,
    /// Where [`Config::data_dir`] came from.
    pub data_dir: Source,
    /// Where [`Config::state_file`] came from.
    pub state_file: Source,
    /// Where [`Config::mpd_address`] came from.
    pub mpd_address: Source,
    /// Where [`Config::mpd_enabled`] came from.
    pub mpd_enabled: Source,
    /// Where [`Config::rewrite_saved_queue`] came from.
    pub rewrite_saved_queue: Source,
    /// Where [`Config::trigger_update_after_commit`] came from.
    pub trigger_update_after_commit: Source,
    /// Where [`Config::delete_enabled`] came from.
    pub delete_enabled: Source,
    /// Where [`Config::backup_keep`] came from.
    pub backup_keep: Source,
    /// Where [`Config::organize_template`] came from.
    pub organize_template: Source,
    /// Where [`Config::organize_portable_names`] came from.
    pub organize_portable_names: Source,
    /// Where [`Config::id3_version`] came from.
    pub id3_version: Source,
}

/// One row of `mpdfm config show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// The setting's name, as `config.toml` spells it.
    pub name: &'static str,
    /// Its resolved value, rendered.
    pub value: String,
    /// Where that value came from.
    pub source: Source,
}

impl Config {
    /// Every setting with its value and source, in the order
    /// `docs/config.example.toml` lists them.
    #[must_use]
    pub fn settings(&self) -> Vec<Setting> {
        let s = &self.sources;
        vec![
            Setting {
                name: "music_dir",
                value: self.music_dir.to_string(),
                source: s.music_dir.clone(),
            },
            Setting {
                name: "playlist_dir",
                value: self.playlist_dir.to_string(),
                source: s.playlist_dir.clone(),
            },
            Setting {
                name: "data_dir",
                value: self.data_dir.to_string(),
                source: s.data_dir.clone(),
            },
            Setting {
                name: "state_file",
                value: self
                    .state_file
                    .as_ref()
                    .map_or_else(|| "<none>".to_owned(), Utf8PathBuf::to_string),
                source: s.state_file.clone(),
            },
            Setting {
                name: "mpd_address",
                value: self.mpd_address.to_string(),
                source: s.mpd_address.clone(),
            },
            Setting {
                name: "mpd_enabled",
                value: self.mpd_enabled.to_string(),
                source: s.mpd_enabled.clone(),
            },
            Setting {
                name: "rewrite_saved_queue",
                value: self.rewrite_saved_queue.to_string(),
                source: s.rewrite_saved_queue.clone(),
            },
            Setting {
                name: "trigger_update_after_commit",
                value: self.trigger_update_after_commit.to_string(),
                source: s.trigger_update_after_commit.clone(),
            },
            Setting {
                name: "delete_enabled",
                value: self.delete_enabled.to_string(),
                source: s.delete_enabled.clone(),
            },
            Setting {
                name: "backup_keep",
                value: self.backup_keep.to_string(),
                source: s.backup_keep.clone(),
            },
            Setting {
                name: "organize_template",
                value: self.organize_template.clone(),
                source: s.organize_template.clone(),
            },
            Setting {
                name: "organize_portable_names",
                value: self.organize_portable_names.to_string(),
                source: s.organize_portable_names.clone(),
            },
            Setting {
                name: "id3_version",
                value: self.id3_version.as_str().to_owned(),
                source: s.id3_version.clone(),
            },
        ]
    }

    /// The library root, or the reason it cannot be used.
    ///
    /// Call this before anything that walks or writes the library. The path is
    /// not re-resolved — [`resolve`] canonicalized it once and that result is
    /// what everything downstream shares — only re-checked, so a library on a
    /// drive that was unplugged since startup is caught rather than half-moved.
    pub fn require_music_dir(&self) -> Result<&Utf8Path, RootProblem> {
        check_root("music_dir", &self.music_dir, &self.sources.music_dir)?;
        Ok(&self.music_dir)
    }

    /// The playlist directory, or the reason it cannot be used.
    pub fn require_playlist_dir(&self) -> Result<&Utf8Path, RootProblem> {
        check_root(
            "playlist_dir",
            &self.playlist_dir,
            &self.sources.playlist_dir,
        )?;
        Ok(&self.playlist_dir)
    }
}

/// `Ok(())` when `path` is an existing directory.
fn check_root(setting: &'static str, path: &Utf8Path, origin: &Source) -> Result<(), RootProblem> {
    let problem = |defect| RootProblem {
        setting,
        path: path.to_owned(),
        defect,
        origin: origin.clone(),
    };
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(problem(RootDefect::NotADirectory)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(problem(RootDefect::Missing)),
        Err(err) => Err(problem(RootDefect::Unreadable(err.to_string()))),
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Where MPDFM looks for an mpd.conf, in order, with duplicates removed.
///
/// This is MPD's own search order. Note that when `$XDG_CONFIG_HOME` is unset
/// the first and third entries are the same path, which is why the list is
/// deduplicated rather than tried twice.
#[must_use]
pub fn mpd_conf_candidates(env: &Env) -> Vec<Utf8PathBuf> {
    let mut paths = Vec::new();
    let mut push = |path: Option<Utf8PathBuf>| {
        if let Some(path) = path
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    };
    push(env.config_home().map(|dir| dir.join("mpd/mpd.conf")));
    push(env.home().map(|home| home.join(".mpdconf")));
    push(env.home().map(|home| home.join(".config/mpd/mpd.conf")));
    push(env.system_mpd_conf.clone());
    paths
}

/// Where MPDFM's own config lives when `--config` was not given.
#[must_use]
pub fn config_file_path(env: &Env) -> Option<Utf8PathBuf> {
    Some(env.config_home()?.join("mpdfm/config.toml"))
}

/// Where the TUI's keymap lives (task 21).
///
/// Next to `config.toml` and discovered the same way, so that `$XDG_CONFIG_HOME`
/// moves both. The file is read by the front-end rather than by resolution: a
/// keymap is not a setting, nothing outside the TUI has an opinion about it, and
/// `Config` carrying a `HashMap` of key bindings would put `crossterm`'s vocabulary
/// in core.
#[must_use]
pub fn keys_file_path(env: &Env) -> Option<Utf8PathBuf> {
    Some(env.config_home()?.join("mpdfm/keys.toml"))
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Work out the configuration.
///
/// Never fails: everything that could go wrong arrives as a [`ConfigWarning`],
/// and a root directory that does not exist is left for
/// [`Config::require_music_dir`] to refuse.
#[must_use]
pub fn resolve(overrides: &Overrides, env: &Env) -> (Config, Vec<ConfigWarning>) {
    let mut resolver = Resolver {
        env,
        warnings: Vec::new(),
        config: None,
        mpd: None,
    };
    resolver.load_config(overrides);
    resolver.load_mpd_conf();
    let config = resolver.build(overrides);
    (config, resolver.warnings)
}

/// A `config.toml` that was read and parsed.
struct LoadedConfig {
    path: Utf8PathBuf,
    table: toml::Table,
}

/// An mpd.conf that was read and parsed.
struct LoadedMpdConf {
    path: Utf8PathBuf,
    conf: MpdConf,
}

/// Carries the two parsed files and the warning list through resolution, so
/// that each setting's chain reads as the four sources in order.
struct Resolver<'a> {
    env: &'a Env,
    warnings: Vec<ConfigWarning>,
    config: Option<LoadedConfig>,
    mpd: Option<LoadedMpdConf>,
}

/// What reading a file produced.
enum Read {
    Text(String),
    Absent,
    Failed,
}

impl Resolver<'_> {
    fn read(&mut self, path: &Utf8Path) -> Read {
        match std::fs::read_to_string(path) {
            Ok(text) => Read::Text(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Read::Absent,
            Err(err) => {
                self.warnings.push(ConfigWarning::Unreadable {
                    path: path.to_owned(),
                    message: err.to_string(),
                });
                Read::Failed
            }
        }
    }

    fn load_config(&mut self, overrides: &Overrides) {
        let path = match &overrides.config_file {
            Some(raw) => {
                // A `--config` path gets the same expansion as one written in a
                // file: a shell usually expands `~`, but MPDFM may not be run
                // from one.
                match self.env.expand(raw.as_str(), Expand::TildeAndVars) {
                    Ok(path) => path,
                    Err(err) => {
                        self.warnings.push(ConfigWarning::BadValue {
                            origin: Source::Flag("--config"),
                            message: err.to_string(),
                        });
                        return;
                    }
                }
            }
            None => {
                let Some(path) = config_file_path(self.env) else {
                    return;
                };
                path
            }
        };

        let text = match self.read(&path) {
            Read::Text(text) => text,
            Read::Absent => {
                self.warnings.push(ConfigWarning::NoConfigFile { path });
                return;
            }
            Read::Failed => return,
        };

        let table: toml::Table = match text.parse() {
            Ok(table) => table,
            Err(err) => {
                self.warnings.push(ConfigWarning::MalformedToml {
                    path,
                    message: err.message().to_owned(),
                });
                return;
            }
        };

        for key in table.keys() {
            if !CONFIG_KEYS.contains(&key.as_str()) {
                self.warnings.push(ConfigWarning::UnknownKey {
                    path: path.clone(),
                    key: key.clone(),
                });
            }
        }
        self.config = Some(LoadedConfig { path, table });
    }

    fn load_mpd_conf(&mut self) {
        let candidates = mpd_conf_candidates(self.env);
        for path in &candidates {
            // An unreadable candidate is skipped rather than fatal, so a
            // root-owned `/etc/mpd.conf` cannot hide a perfectly good one in
            // the user's own config directory.
            let Read::Text(text) = self.read(path) else {
                continue;
            };
            let (conf, warnings) = MpdConf::parse(&text);
            self.warnings
                .extend(warnings.into_iter().map(|warning| ConfigWarning::MpdConf {
                    path: path.clone(),
                    warning,
                }));
            self.mpd = Some(LoadedMpdConf {
                path: path.clone(),
                conf,
            });
            return;
        }
        self.warnings.push(ConfigWarning::NoMpdConf {
            searched: candidates,
        });
    }

    /// A `config.toml` value, cloned out so the caller can still take `&mut
    /// self` to record a warning about it.
    fn toml_value(&self, key: &str) -> Option<(Utf8PathBuf, toml::Value)> {
        let loaded = self.config.as_ref()?;
        Some((loaded.path.clone(), loaded.table.get(key)?.clone()))
    }

    fn toml_str(&mut self, key: &'static str) -> Option<(String, Source)> {
        let (path, value) = self.toml_value(key)?;
        match value.as_str() {
            Some(text) => {
                let source = Source::Config { path, key };
                Some((text.to_owned(), source))
            }
            None => {
                self.wrong_type(path, key, "a string", &value);
                None
            }
        }
    }

    fn toml_bool(&mut self, key: &'static str) -> Option<(bool, Source)> {
        let (path, value) = self.toml_value(key)?;
        match value.as_bool() {
            Some(flag) => Some((flag, Source::Config { path, key })),
            None => {
                self.wrong_type(path, key, "true or false", &value);
                None
            }
        }
    }

    fn toml_u32(&mut self, key: &'static str) -> Option<(u32, Source)> {
        let (path, value) = self.toml_value(key)?;
        match value.as_integer().and_then(|n| u32::try_from(n).ok()) {
            Some(number) => Some((number, Source::Config { path, key })),
            None => {
                self.wrong_type(path, key, "a non-negative whole number", &value);
                None
            }
        }
    }

    fn wrong_type(
        &mut self,
        path: Utf8PathBuf,
        key: &'static str,
        expected: &'static str,
        value: &toml::Value,
    ) {
        self.warnings.push(ConfigWarning::WrongType {
            path,
            key,
            expected,
            found: value.type_str(),
        });
    }

    fn mpd_value(&self, key: &'static str) -> Option<(String, Source)> {
        let loaded = self.mpd.as_ref()?;
        let value = loaded.conf.get(key)?;
        Some((
            value.to_owned(),
            Source::MpdConf {
                path: loaded.path.clone(),
                key,
            },
        ))
    }

    /// Walk a setting's sources in order, taking the first that yields an
    /// absolute path and warning about each one that does not.
    fn first_usable_path(
        &mut self,
        candidates: Vec<(Source, String, Expand)>,
    ) -> Option<(Utf8PathBuf, Source)> {
        for (origin, raw, mode) in candidates {
            let expanded = self
                .env
                .expand(&raw, mode)
                .and_then(|path| match path.is_absolute() {
                    true => Ok(path),
                    false => Err(ExpandError::NotAbsolute { raw }),
                });
            match expanded {
                Ok(path) => return Some((path, origin)),
                Err(err) => self.warnings.push(ConfigWarning::BadValue {
                    origin,
                    message: err.to_string(),
                }),
            }
        }
        None
    }

    /// Resolve to an absolute canonical path, once, as the task's pitfalls
    /// require. A path that cannot be canonicalized is kept in its expanded form
    /// and reported, so the message names what the user actually configured.
    fn canonicalize(
        &mut self,
        setting: &'static str,
        path: Utf8PathBuf,
        origin: &Source,
    ) -> Utf8PathBuf {
        if let Err(problem) = check_root(setting, &path, origin) {
            self.warnings.push(problem.into());
            return path;
        }
        match std::fs::canonicalize(&path).map(Utf8PathBuf::from_path_buf) {
            Ok(Ok(canonical)) => canonical,
            // Canonical but not UTF-8, or it stopped being resolvable between
            // the check above and here. Neither is worth a second warning; the
            // expanded path is still the user's own spelling of the same place.
            Ok(Err(_)) | Err(_) => path,
        }
    }

    fn build(&mut self, overrides: &Overrides) -> Config {
        let env = self.env;

        let music_dir_default = env
            .home()
            .map_or_else(|| Utf8PathBuf::from("~/Music"), |home| home.join("Music"));
        let mut candidates = Vec::new();
        if let Some(dir) = &overrides.music_dir {
            candidates.push((
                Source::Flag("--music-dir"),
                dir.to_string(),
                Expand::TildeAndVars,
            ));
        }
        if let Some((raw, origin)) = self.toml_str("music_dir") {
            candidates.push((origin, raw, Expand::TildeAndVars));
        }
        if let Some((raw, origin)) = self.mpd_value("music_directory") {
            candidates.push((origin, raw, Expand::Tilde));
        }
        let (music_dir, music_dir_source) = self
            .first_usable_path(candidates)
            .unwrap_or((music_dir_default, Source::Default));
        let music_dir = self.canonicalize("music_dir", music_dir, &music_dir_source);

        let playlist_dir_default = env.config_home().map_or_else(
            || Utf8PathBuf::from("~/.config/mpd/playlists"),
            |dir| dir.join("mpd/playlists"),
        );
        let mut candidates = Vec::new();
        if let Some(dir) = &overrides.playlist_dir {
            candidates.push((
                Source::Flag("--playlist-dir"),
                dir.to_string(),
                Expand::TildeAndVars,
            ));
        }
        if let Some((raw, origin)) = self.toml_str("playlist_dir") {
            candidates.push((origin, raw, Expand::TildeAndVars));
        }
        if let Some((raw, origin)) = self.mpd_value("playlist_directory") {
            candidates.push((origin, raw, Expand::Tilde));
        }
        let (playlist_dir, playlist_dir_source) = self
            .first_usable_path(candidates)
            .unwrap_or((playlist_dir_default, Source::Default));
        let playlist_dir = self.canonicalize("playlist_dir", playlist_dir, &playlist_dir_source);

        // MPDFM's own directory is created on first use, so a missing one is
        // not a defect and it is not canonicalized.
        let data_dir_default = env.data_home().map_or_else(
            || Utf8PathBuf::from("~/.local/share/mpdfm"),
            |dir| dir.join("mpdfm"),
        );
        let mut candidates = Vec::new();
        if let Some((raw, origin)) = self.toml_str("data_dir") {
            candidates.push((origin, raw, Expand::TildeAndVars));
        }
        let (data_dir, data_dir_source) = self
            .first_usable_path(candidates)
            .unwrap_or((data_dir_default, Source::Default));

        let mut candidates = Vec::new();
        if let Some((raw, origin)) = self.toml_str("state_file") {
            candidates.push((origin, raw, Expand::TildeAndVars));
        }
        if let Some((raw, origin)) = self.mpd_value("state_file") {
            candidates.push((origin, raw, Expand::Tilde));
        }
        let (state_file, state_file_source) = match self.first_usable_path(candidates) {
            Some((path, origin)) => (Some(path), origin),
            None => (None, Source::Default),
        };

        let (mpd_address, mpd_address_source) = self.mpd_address();

        let (mpd_enabled, mpd_enabled_source) = if overrides.no_mpd {
            (false, Source::Flag("--no-mpd"))
        } else {
            self.toml_bool("mpd_enabled")
                .unwrap_or((true, Source::Default))
        };
        let (rewrite_saved_queue, rewrite_saved_queue_source) = self
            .toml_bool("rewrite_saved_queue")
            .unwrap_or((true, Source::Default));
        let (trigger_update_after_commit, trigger_update_after_commit_source) = self
            .toml_bool("trigger_update_after_commit")
            .unwrap_or((true, Source::Default));
        let (delete_enabled, delete_enabled_source) = self
            .toml_bool("delete_enabled")
            .unwrap_or((true, Source::Default));
        let (backup_keep, backup_keep_source) = self
            .toml_u32("backup_keep")
            .unwrap_or((DEFAULT_BACKUP_KEEP, Source::Default));
        let (organize_template, organize_template_source) = self
            .toml_str("organize_template")
            .unwrap_or_else(|| (DEFAULT_ORGANIZE_TEMPLATE.to_owned(), Source::Default));
        let (organize_portable_names, organize_portable_names_source) = self
            .toml_bool("organize_portable_names")
            .unwrap_or((true, Source::Default));
        let (id3_version, id3_version_source) = self.id3_version();

        Config {
            music_dir,
            playlist_dir,
            data_dir,
            state_file,
            mpd_address,
            mpd_enabled,
            rewrite_saved_queue,
            trigger_update_after_commit,
            delete_enabled,
            backup_keep,
            organize_template,
            organize_portable_names,
            id3_version,
            sources: Sources {
                music_dir: music_dir_source,
                playlist_dir: playlist_dir_source,
                data_dir: data_dir_source,
                state_file: state_file_source,
                mpd_address: mpd_address_source,
                mpd_enabled: mpd_enabled_source,
                rewrite_saved_queue: rewrite_saved_queue_source,
                trigger_update_after_commit: trigger_update_after_commit_source,
                delete_enabled: delete_enabled_source,
                backup_keep: backup_keep_source,
                organize_template: organize_template_source,
                organize_portable_names: organize_portable_names_source,
                id3_version: id3_version_source,
            },
        }
    }

    /// `id3_version` from the config, else `keep`.
    ///
    /// A value that is not one of the three is a [`ConfigWarning::BadValue`] and
    /// falls back to the default rather than refusing to start: the setting is
    /// about how an mp3 is written, and a typo in it must not stop the user from
    /// scanning their library.
    fn id3_version(&mut self) -> (crate::tags::Id3Version, Source) {
        let Some((raw, origin)) = self.toml_str("id3_version") else {
            return (crate::tags::Id3Version::default(), Source::Default);
        };
        match crate::tags::Id3Version::parse(&raw) {
            Some(version) => (version, origin),
            None => {
                self.warnings.push(ConfigWarning::BadValue {
                    origin,
                    message: format!(
                        "`id3_version` should be \"keep\", \"v23\" or \"v24\", found {raw:?}"
                    ),
                });
                (crate::tags::Id3Version::default(), Source::Default)
            }
        }
    }

    /// `mpd_address` from the config, else mpd.conf's `bind_to_address` and
    /// `port` combined, else localhost.
    fn mpd_address(&mut self) -> (MpdAddress, Source) {
        if let Some((raw, origin)) = self.toml_str("mpd_address") {
            match MpdAddress::parse(&raw, DEFAULT_MPD_PORT) {
                Ok(address) => return (address, origin),
                Err(err) => self.warnings.push(ConfigWarning::BadValue {
                    origin,
                    message: err.to_string(),
                }),
            }
        }

        // mpd.conf's port is a separate key, and is the default for a
        // `bind_to_address` that carries no port of its own.
        let port = match self.mpd_value("port") {
            Some((raw, origin)) => match raw.parse() {
                Ok(port) => port,
                Err(_) => {
                    self.warnings.push(ConfigWarning::BadValue {
                        origin,
                        message: AddressError::BadPort { port: raw }.to_string(),
                    });
                    DEFAULT_MPD_PORT
                }
            },
            None => DEFAULT_MPD_PORT,
        };

        if let Some((raw, origin)) = self.mpd_value("bind_to_address") {
            // `any` and `*` mean "listen on every interface". MPDFM is the one
            // connecting, so the address to connect to is the loopback one.
            let raw = match raw.as_str() {
                "any" | "*" => "127.0.0.1".to_owned(),
                _ => raw,
            };
            // A socket path in mpd.conf may be written with a `~`, and MPD
            // expands it.
            let raw = match raw.starts_with('~') {
                true => match self.env.expand(&raw, Expand::Tilde) {
                    Ok(path) => path.into_string(),
                    Err(err) => {
                        self.warnings.push(ConfigWarning::BadValue {
                            origin: origin.clone(),
                            message: err.to_string(),
                        });
                        raw
                    }
                },
                false => raw,
            };
            match MpdAddress::parse(&raw, port) {
                Ok(address) => return (address, origin),
                Err(err) => self.warnings.push(ConfigWarning::BadValue {
                    origin,
                    message: err.to_string(),
                }),
            }
        }

        (
            MpdAddress::Tcp {
                host: "127.0.0.1".to_owned(),
                port,
            },
            Source::Default,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpdconf::tests::{REAL_SYSTEM_CONF, REAL_USER_CONF};

    /// A fake `$HOME` in a temp directory, deleted when it drops.
    ///
    /// Every test builds its environment from one of these. Nothing here ever
    /// reads the user's real `~/.config/mpd` or `/etc/mpd.conf` — [`Env::empty`]
    /// starts with no system file, and [`Home::env`] points that candidate at a
    /// path inside the temp tree (`docs/PLAN.md` §8).
    struct Home {
        #[expect(dead_code, reason = "held for its Drop impl, which deletes the tree")]
        temp: tempfile::TempDir,
        root: Utf8PathBuf,
    }

    impl Home {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temp dir");
            // Canonical from the start: `/tmp` is a symlink on some systems, and
            // `resolve` canonicalizes, so expectations have to agree.
            let root = std::fs::canonicalize(temp.path()).expect("canonical temp dir");
            let root = Utf8PathBuf::from_path_buf(root).expect("temp dir path should be UTF-8");
            Self { temp, root }
        }

        /// `$HOME` pointed here, XDG unset, and the system mpd.conf redirected
        /// into the temp tree so `/etc/mpd.conf` is unreachable.
        fn env(&self) -> Env {
            Env::empty()
                .with("HOME", self.root.as_str())
                .with_system_mpd_conf(self.root.join("etc/mpd.conf"))
        }

        /// Create a directory, parents included, and return its path.
        fn dir(&self, rel: &str) -> Utf8PathBuf {
            let path = self.root.join(rel);
            std::fs::create_dir_all(&path).expect("create dir");
            path
        }

        /// Write a file, creating its parents, and return its path.
        fn write(&self, rel: &str, text: &str) -> Utf8PathBuf {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("create parent");
            std::fs::write(&path, text).expect("write file");
            path
        }

        /// The library and playlist directories the real mpd.conf names, so
        /// that resolution has something to canonicalize.
        fn with_real_dirs(&self) -> &Self {
            self.dir("Music");
            self.dir(".config/mpd/playlists");
            self
        }
    }

    /// `docs/config.example.toml` with its commented-out settings switched on,
    /// so the documentation and the parser cannot drift apart. Only lines that
    /// assign a key MPDFM knows are uncommented; the prose stays prose.
    fn uncommented_example() -> String {
        include_str!("../../../docs/config.example.toml")
            .lines()
            .map(|line| {
                let setting = line.strip_prefix("# ").filter(|rest| {
                    rest.split_once(" = ")
                        .is_some_and(|(key, _)| CONFIG_KEYS.contains(&key))
                });
                setting.unwrap_or(line)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Resolve with no command-line overrides.
    fn resolve_in(home: &Home) -> (Config, Vec<ConfigWarning>) {
        resolve(&Overrides::default(), &home.env())
    }

    fn rendered(warnings: &[ConfigWarning]) -> Vec<String> {
        warnings.iter().map(ToString::to_string).collect()
    }

    // -- the real setup on this machine -----------------------------------

    #[test]
    fn resolves_the_real_mpd_conf_to_the_real_library() {
        let home = Home::new();
        home.with_real_dirs();
        home.write(".config/mpd/mpd.conf", REAL_USER_CONF);

        let (config, warnings) = resolve_in(&home);

        assert_eq!(config.music_dir, home.root.join("Music"));
        assert_eq!(config.playlist_dir, home.root.join(".config/mpd/playlists"));
        assert_eq!(config.state_file, Some(home.root.join(".config/mpd/state")));
        assert_eq!(config.mpd_address.to_string(), "127.0.0.1:6600");

        let conf = home.root.join(".config/mpd/mpd.conf");
        assert_eq!(
            config.sources.music_dir,
            Source::MpdConf {
                path: conf.clone(),
                key: "music_directory"
            }
        );
        assert_eq!(
            config.sources.playlist_dir,
            Source::MpdConf {
                path: conf,
                key: "playlist_directory"
            }
        );

        // The only thing to report is that MPDFM has no config of its own,
        // which is the normal state of affairs.
        assert_eq!(
            warnings,
            [ConfigWarning::NoConfigFile {
                path: home.root.join(".config/mpdfm/config.toml")
            }]
        );
    }

    #[test]
    fn an_audio_output_block_does_not_supply_a_setting() {
        let home = Home::new();
        home.with_real_dirs();
        home.write(".config/mpd/mpd.conf", REAL_USER_CONF);

        let (config, _) = resolve_in(&home);

        // The fifo output's `path "/tmp/mpd.fifo"` and `name "visualizer"` must
        // not have reached anything.
        for setting in config.settings() {
            assert!(
                !setting.value.contains("/tmp/mpd.fifo") && !setting.value.contains("visualizer"),
                "`{}` picked up an audio_output value: {}",
                setting.name,
                setting.value
            );
        }
    }

    #[test]
    fn the_first_mpd_conf_found_wins_and_nothing_is_merged() {
        let home = Home::new();
        home.with_real_dirs();
        home.write(".config/mpd/mpd.conf", REAL_USER_CONF);
        home.write("etc/mpd.conf", REAL_SYSTEM_CONF);

        let (config, _) = resolve_in(&home);

        // The system file names a different library. If the two were merged,
        // its `playlist_directory` or `state_file` would show up here.
        assert_eq!(config.playlist_dir, home.root.join(".config/mpd/playlists"));
        assert_eq!(config.state_file, Some(home.root.join(".config/mpd/state")));
    }

    #[test]
    fn the_system_mpd_conf_is_used_when_there_is_no_user_one() {
        let home = Home::new();
        home.write("etc/mpd.conf", REAL_SYSTEM_CONF);

        let (config, _) = resolve_in(&home);

        assert_eq!(
            config.playlist_dir,
            Utf8PathBuf::from("/var/lib/mpd/playlists")
        );
        assert_eq!(
            config.state_file,
            Some(Utf8PathBuf::from("/var/lib/mpd/mpdstate"))
        );
        // It has no `music_directory` at all, so that one falls to the default.
        assert_eq!(config.music_dir, home.root.join("Music"));
        assert_eq!(config.sources.music_dir, Source::Default);
    }

    #[test]
    fn dot_mpdconf_sits_between_the_xdg_location_and_the_default_one() {
        let home = Home::new();
        home.dir("Old");
        home.dir("Music");
        home.write(".mpdconf", "music_directory \"~/Old\"\n");
        home.write(".config/mpd/mpd.conf", "music_directory \"~/Music\"\n");

        // With $XDG_CONFIG_HOME unset it defaults to ~/.config, so the first
        // candidate already *is* ~/.config/mpd/mpd.conf and wins.
        let (config, _) = resolve_in(&home);
        assert_eq!(config.music_dir, home.root.join("Music"));

        // Point $XDG_CONFIG_HOME at a directory with no mpd.conf and the two
        // come apart: ~/.mpdconf is next on the list.
        let env = home
            .env()
            .with("XDG_CONFIG_HOME", home.root.join("xdg").as_str());
        let (config, _) = resolve(&Overrides::default(), &env);
        assert_eq!(config.music_dir, home.root.join("Old"));
    }

    // -- expansion ---------------------------------------------------------

    #[test]
    fn expands_tilde() {
        let env = Env::empty().with("HOME", "/home/me");
        assert_eq!(
            env.expand("~/Music", Expand::Tilde).unwrap(),
            "/home/me/Music"
        );
        assert_eq!(env.expand("~", Expand::Tilde).unwrap(), "/home/me");
        // Only at the start, and only as a whole component.
        assert_eq!(env.expand("/srv/~/x", Expand::Tilde).unwrap(), "/srv/~/x");

        assert_eq!(
            env.expand("~other/Music", Expand::Tilde).unwrap_err(),
            ExpandError::TildeUser {
                raw: "~other/Music".into()
            }
        );
        assert_eq!(
            Env::empty().expand("~/Music", Expand::Tilde).unwrap_err(),
            ExpandError::NoHome {
                raw: "~/Music".into()
            }
        );
    }

    #[test]
    fn expands_xdg_variables_in_mpdfms_own_values() {
        let env = Env::empty()
            .with("HOME", "/home/me")
            .with("XDG_CONFIG_HOME", "/home/me/.cfg")
            .with("XDG_DATA_HOME", "/home/me/.data");

        assert_eq!(
            env.expand("$XDG_CONFIG_HOME/mpd/playlists", Expand::TildeAndVars)
                .unwrap(),
            "/home/me/.cfg/mpd/playlists"
        );
        assert_eq!(
            env.expand("${XDG_DATA_HOME}/mpdfm", Expand::TildeAndVars)
                .unwrap(),
            "/home/me/.data/mpdfm"
        );
        assert_eq!(
            env.expand("~/$USER_NONE", Expand::Tilde).unwrap(),
            "/home/me/$USER_NONE"
        );

        assert_eq!(
            env.expand("$XDG_NOPE/x", Expand::TildeAndVars).unwrap_err(),
            ExpandError::UnknownVariable {
                raw: "$XDG_NOPE/x".into(),
                name: "XDG_NOPE".into()
            }
        );
        assert_eq!(
            env.expand("${XDG_CONFIG_HOME/x", Expand::TildeAndVars)
                .unwrap_err(),
            ExpandError::UnclosedBrace {
                raw: "${XDG_CONFIG_HOME/x".into()
            }
        );
        // A `$` that names nothing is a legal filename character.
        assert_eq!(
            env.expand("/srv/$ money", Expand::TildeAndVars).unwrap(),
            "/srv/$ money"
        );
    }

    #[test]
    fn xdg_base_directories_fall_back_to_home() {
        let env = Env::empty().with("HOME", "/home/me");
        assert_eq!(env.config_home().unwrap(), "/home/me/.config");
        assert_eq!(env.data_home().unwrap(), "/home/me/.local/share");

        let set = env.clone().with("XDG_CONFIG_HOME", "/elsewhere/cfg");
        assert_eq!(set.config_home().unwrap(), "/elsewhere/cfg");

        // The specification says an empty or relative value must be ignored.
        let empty = env.clone().with("XDG_CONFIG_HOME", "");
        assert_eq!(empty.config_home().unwrap(), "/home/me/.config");
        let relative = env.with("XDG_CONFIG_HOME", "cfg");
        assert_eq!(relative.config_home().unwrap(), "/home/me/.config");
    }

    #[test]
    fn xdg_config_home_moves_both_searches() {
        let home = Home::new();
        home.dir("Music");
        home.dir("xdg/mpd/playlists");
        home.write("xdg/mpd/mpd.conf", "music_directory \"~/Music\"\n");
        home.write("xdg/mpdfm/config.toml", "backup_keep = 7\n");
        // The default locations hold a config that must NOT win.
        home.write(".config/mpd/mpd.conf", "music_directory \"/wrong\"\n");

        let env = home
            .env()
            .with("XDG_CONFIG_HOME", home.root.join("xdg").as_str());
        let (config, _) = resolve(&Overrides::default(), &env);

        assert_eq!(config.music_dir, home.root.join("Music"));
        assert_eq!(config.backup_keep, 7);
        // With no `playlist_directory` anywhere, the default follows XDG too.
        assert_eq!(config.playlist_dir, home.root.join("xdg/mpd/playlists"));
        assert_eq!(config.sources.playlist_dir, Source::Default);
    }

    #[test]
    fn mpd_conf_gets_no_variable_expansion_because_mpd_gives_it_none() {
        let home = Home::new();
        home.dir("Music");
        home.write(".config/mpd/mpd.conf", "music_directory \"$HOME/Music\"\n");

        let (config, warnings) = resolve_in(&home);

        // MPD would look for a directory literally named `$HOME`, so MPDFM
        // must not quietly disagree: the value is refused and the default used.
        assert_eq!(config.music_dir, home.root.join("Music"));
        assert_eq!(config.sources.music_dir, Source::Default);
        assert!(
            rendered(&warnings)
                .iter()
                .any(|w| w.contains("is not an absolute path")),
            "{warnings:#?}"
        );
    }

    // -- precedence --------------------------------------------------------

    #[test]
    fn flags_override_config_which_overrides_mpd_conf() {
        let home = Home::new();
        home.dir("FromFlag");
        home.dir("FromConfig");
        home.dir("FromMpdConf");
        home.write(
            ".config/mpd/mpd.conf",
            "music_directory \"~/FromMpdConf\"\nplaylist_directory \"~/FromMpdConf\"\nstate_file \"~/.config/mpd/state\"\n",
        );
        home.write(
            ".config/mpdfm/config.toml",
            "music_dir = \"~/FromConfig\"\nplaylist_dir = \"~/FromConfig\"\n",
        );

        let overrides = Overrides {
            music_dir: Some(home.root.join("FromFlag")),
            ..Overrides::default()
        };
        let (config, _) = resolve(&overrides, &home.env());

        // Flag beats config beats mpd.conf, field by field.
        assert_eq!(config.music_dir, home.root.join("FromFlag"));
        assert_eq!(config.sources.music_dir, Source::Flag("--music-dir"));
        assert_eq!(config.playlist_dir, home.root.join("FromConfig"));
        assert_eq!(
            config.sources.playlist_dir,
            Source::Config {
                path: home.root.join(".config/mpdfm/config.toml"),
                key: "playlist_dir"
            }
        );
        assert_eq!(config.state_file, Some(home.root.join(".config/mpd/state")));
    }

    #[test]
    fn the_config_flag_chooses_the_file() {
        let home = Home::new();
        home.dir("Elsewhere");
        home.write(
            ".config/mpdfm/config.toml",
            "music_dir = \"/default-file\"\n",
        );
        home.write("chosen.toml", "music_dir = \"~/Elsewhere\"\n");

        let overrides = Overrides {
            config_file: Some(Utf8PathBuf::from("~/chosen.toml")),
            ..Overrides::default()
        };
        let (config, _) = resolve(&overrides, &home.env());

        assert_eq!(config.music_dir, home.root.join("Elsewhere"));
    }

    #[test]
    fn a_relative_path_is_refused_so_the_next_source_wins() {
        let home = Home::new();
        home.dir("Music");
        home.write(".config/mpd/mpd.conf", "music_directory \"~/Music\"\n");
        home.write(".config/mpdfm/config.toml", "music_dir = \"Music\"\n");

        let (config, warnings) = resolve_in(&home);

        // Resolving it against the working directory would make the library
        // move with `cd`, so mpd.conf answers instead.
        assert_eq!(config.music_dir, home.root.join("Music"));
        assert!(matches!(config.sources.music_dir, Source::MpdConf { .. }));
        assert!(
            rendered(&warnings)
                .iter()
                .any(|w| w.contains("`Music` is not an absolute path")),
            "{warnings:#?}"
        );
    }

    // -- warnings, never failures -----------------------------------------

    #[test]
    fn an_empty_home_yields_defaults_and_warnings() {
        let home = Home::new();

        let (config, warnings) = resolve_in(&home);

        assert_eq!(config.music_dir, home.root.join("Music"));
        assert_eq!(config.playlist_dir, home.root.join(".config/mpd/playlists"));
        assert_eq!(config.data_dir, home.root.join(".local/share/mpdfm"));
        assert_eq!(config.state_file, None);
        assert_eq!(config.mpd_address.to_string(), "127.0.0.1:6600");
        assert!(config.mpd_enabled);
        assert_eq!(config.backup_keep, DEFAULT_BACKUP_KEEP);
        assert_eq!(config.organize_template, DEFAULT_ORGANIZE_TEMPLATE);

        let shown = rendered(&warnings);
        assert!(
            shown.iter().any(|w| w.starts_with("no MPDFM config at")),
            "{shown:#?}"
        );
        assert!(
            shown.iter().any(|w| w.starts_with("no mpd.conf found")),
            "{shown:#?}"
        );
        // Neither root exists, and both are reported rather than raised.
        assert!(
            shown.iter().any(|w| w.starts_with("music_dir")),
            "{shown:#?}"
        );
        assert!(
            shown.iter().any(|w| w.starts_with("playlist_dir")),
            "{shown:#?}"
        );
    }

    #[test]
    fn the_mpd_conf_search_path_is_reported_when_nothing_is_found() {
        let home = Home::new();

        let (_, warnings) = resolve_in(&home);

        let searched = warnings.iter().find_map(|w| match w {
            ConfigWarning::NoMpdConf { searched } => Some(searched.clone()),
            _ => None,
        });
        // With XDG unset the first and third candidates coincide, so the list
        // is deduplicated rather than repeating a path.
        assert_eq!(
            searched.expect("a NoMpdConf warning"),
            [
                home.root.join(".config/mpd/mpd.conf"),
                home.root.join(".mpdconf"),
                home.root.join("etc/mpd.conf"),
            ]
        );
    }

    #[test]
    fn an_unreadable_file_is_a_warning_not_an_error() {
        let home = Home::new();
        home.dir("Music");
        // A directory where a file is expected fails to read for every user,
        // including root — unlike a chmod, which a root-run CI would ignore.
        home.dir(".config/mpdfm/config.toml");
        home.dir(".config/mpd/mpd.conf");
        home.write(".mpdconf", "music_directory \"~/Music\"\n");

        let (config, warnings) = resolve_in(&home);

        let shown = rendered(&warnings);
        assert_eq!(
            shown.iter().filter(|w| w.contains("ignoring it")).count(),
            2,
            "{shown:#?}"
        );
        // The unreadable mpd.conf candidate was skipped, not treated as the
        // answer, so the next one on the search path still applies.
        assert_eq!(config.music_dir, home.root.join("Music"));
        assert!(matches!(config.sources.music_dir, Source::MpdConf { .. }));
    }

    #[test]
    fn an_unknown_key_warns_instead_of_failing() {
        let home = Home::new();
        home.write(
            ".config/mpdfm/config.toml",
            "mucis_dir = \"/typo\"\nbackup_keep = 7\n",
        );

        let (config, warnings) = resolve_in(&home);

        // The typo is reported and the rest of the file still applies.
        assert_eq!(config.backup_keep, 7);
        assert!(
            warnings.contains(&ConfigWarning::UnknownKey {
                path: home.root.join(".config/mpdfm/config.toml"),
                key: "mucis_dir".into(),
            }),
            "{warnings:#?}"
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_warns_and_falls_back() {
        let home = Home::new();
        home.write(
            ".config/mpdfm/config.toml",
            "backup_keep = \"fifty\"\nmpd_enabled = 1\nbackup_keep_note = 0\n",
        );

        let (config, warnings) = resolve_in(&home);

        assert_eq!(config.backup_keep, DEFAULT_BACKUP_KEEP);
        assert!(config.mpd_enabled);
        let shown = rendered(&warnings);
        assert!(
            shown
                .iter()
                .any(|w| w
                    .contains("`backup_keep` should be a non-negative whole number, found string")),
            "{shown:#?}"
        );
        assert!(
            shown
                .iter()
                .any(|w| w.contains("`mpd_enabled` should be true or false, found integer")),
            "{shown:#?}"
        );
    }

    #[test]
    fn malformed_toml_warns_and_leaves_mpd_conf_in_charge() {
        let home = Home::new();
        home.dir("Music");
        home.write(".config/mpd/mpd.conf", "music_directory \"~/Music\"\n");
        home.write(".config/mpdfm/config.toml", "music_dir = \n");

        let (config, warnings) = resolve_in(&home);

        assert_eq!(config.music_dir, home.root.join("Music"));
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ConfigWarning::MalformedToml { .. })),
            "{warnings:#?}"
        );
    }

    #[test]
    fn an_odd_mpd_conf_line_is_reported_against_its_file() {
        let home = Home::new();
        home.dir("Music");
        home.write(
            ".config/mpd/mpd.conf",
            "music_directory \"~/Music\"\nport\n",
        );

        let (config, warnings) = resolve_in(&home);

        assert_eq!(config.music_dir, home.root.join("Music"));
        let shown = rendered(&warnings);
        assert!(
            shown.iter().any(|w| {
                w.contains(".config/mpd/mpd.conf") && w.contains("`port` has no value")
            }),
            "{shown:#?}"
        );
    }

    // -- roots -------------------------------------------------------------

    #[test]
    fn a_missing_music_dir_is_reported_at_startup_and_refused_on_use() {
        let home = Home::new();
        home.write(".config/mpd/mpd.conf", "music_directory \"~/Gone\"\n");

        let (config, warnings) = resolve_in(&home);

        // The path is kept as configured, so the message names what the user
        // wrote rather than something canonicalized out of recognition.
        assert_eq!(config.music_dir, home.root.join("Gone"));
        let problem = config.require_music_dir().unwrap_err();
        assert_eq!(problem.setting, "music_dir");
        assert_eq!(problem.defect, RootDefect::Missing);
        assert!(problem.to_string().contains("mpd.conf (music_directory)"));
        assert!(
            warnings.contains(&ConfigWarning::Root(problem)),
            "{warnings:#?}"
        );
    }

    #[test]
    fn a_music_dir_that_is_a_file_is_refused() {
        let home = Home::new();
        home.write("NotADir", "");
        home.write(".config/mpd/mpd.conf", "music_directory \"~/NotADir\"\n");

        let (config, _) = resolve_in(&home);

        assert_eq!(
            config.require_music_dir().unwrap_err().defect,
            RootDefect::NotADirectory
        );
    }

    #[test]
    fn an_existing_root_is_resolved_once_to_a_canonical_path() {
        let home = Home::new();
        home.dir("real/Music");
        std::os::unix::fs::symlink(home.root.join("real"), home.root.join("link")).unwrap();
        home.write(".config/mpd/mpd.conf", "music_directory \"~/link/Music\"\n");

        let (config, _) = resolve_in(&home);

        // The symlink is resolved at startup, so every downstream comparison
        // against the root sees one spelling of it.
        assert_eq!(config.music_dir, home.root.join("real/Music"));
        assert_eq!(
            config.require_music_dir().unwrap(),
            home.root.join("real/Music")
        );
    }

    // -- the MPD address ---------------------------------------------------

    #[test]
    fn parses_the_shapes_an_address_comes_in() {
        let cases: &[(&str, &str)] = &[
            ("127.0.0.1", "127.0.0.1:6600"),
            ("127.0.0.1:6601", "127.0.0.1:6601"),
            ("localhost", "localhost:6600"),
            ("[::1]:6601", "[::1]:6601"),
            ("[::1]", "[::1]:6600"),
            ("::1", "[::1]:6600"),
            ("/run/mpd/socket", "/run/mpd/socket"),
            ("@mpd", "@mpd"),
        ];
        for (raw, expected) in cases {
            let parsed = MpdAddress::parse(raw, DEFAULT_MPD_PORT).expect(raw);
            assert_eq!(parsed.to_string(), *expected, "parsing {raw}");
            // Every rendering parses back to the same address.
            assert_eq!(
                MpdAddress::parse(expected, DEFAULT_MPD_PORT).unwrap(),
                parsed
            );
        }

        assert_eq!(
            MpdAddress::parse("", 6600).unwrap_err(),
            AddressError::Empty
        );
        assert_eq!(
            MpdAddress::parse("localhost:mpd", 6600).unwrap_err(),
            AddressError::BadPort { port: "mpd".into() }
        );
        assert_eq!(
            MpdAddress::parse("[::1:6600", 6600).unwrap_err(),
            AddressError::UnclosedBracket {
                raw: "[::1:6600".into()
            }
        );
    }

    #[test]
    fn mpd_conf_combines_bind_to_address_with_port() {
        let home = Home::new();
        home.write(
            ".config/mpd/mpd.conf",
            "bind_to_address \"192.168.1.5\"\nport \"6601\"\n",
        );

        let (config, _) = resolve_in(&home);

        assert_eq!(config.mpd_address.to_string(), "192.168.1.5:6601");
        assert!(matches!(
            config.sources.mpd_address,
            Source::MpdConf {
                key: "bind_to_address",
                ..
            }
        ));
    }

    #[test]
    fn a_socket_in_mpd_conf_wins_over_the_port_key() {
        let home = Home::new();
        home.write(
            ".config/mpd/mpd.conf",
            "bind_to_address \"~/.config/mpd/socket\"\nport \"6600\"\n",
        );

        let (config, _) = resolve_in(&home);

        assert_eq!(
            config.mpd_address,
            MpdAddress::Unix(home.root.join(".config/mpd/socket"))
        );
    }

    #[test]
    fn bind_to_address_any_becomes_the_loopback_address() {
        let home = Home::new();
        home.write(
            ".config/mpd/mpd.conf",
            "bind_to_address \"any\"\nport \"6600\"\n",
        );

        let (config, _) = resolve_in(&home);

        // MPD listens everywhere; MPDFM has to pick somewhere to connect.
        assert_eq!(config.mpd_address.to_string(), "127.0.0.1:6600");
    }

    #[test]
    fn the_config_address_overrides_mpd_conf_and_no_mpd_overrides_both() {
        let home = Home::new();
        home.write(
            ".config/mpd/mpd.conf",
            "bind_to_address \"192.168.1.5\"\nport \"6601\"\n",
        );
        home.write(
            ".config/mpdfm/config.toml",
            "mpd_address = \"10.0.0.2:6700\"\n",
        );

        let overrides = Overrides {
            no_mpd: true,
            ..Overrides::default()
        };
        let (config, _) = resolve(&overrides, &home.env());

        assert_eq!(config.mpd_address.to_string(), "10.0.0.2:6700");
        assert!(!config.mpd_enabled);
        assert_eq!(config.sources.mpd_enabled, Source::Flag("--no-mpd"));
    }

    // -- `mpdfm config show` -----------------------------------------------

    #[test]
    fn settings_lists_every_value_with_where_it_came_from() {
        let home = Home::new();
        home.with_real_dirs();
        home.write(".config/mpd/mpd.conf", REAL_USER_CONF);
        home.write(".config/mpdfm/config.toml", "delete_enabled = false\n");

        let (config, _) = resolve_in(&home);
        let settings = config.settings();

        // Every key the example file documents is shown, in that order.
        assert_eq!(
            settings.iter().map(|s| s.name).collect::<Vec<_>>(),
            [
                "music_dir",
                "playlist_dir",
                "data_dir",
                "state_file",
                "mpd_address",
                "mpd_enabled",
                "rewrite_saved_queue",
                "trigger_update_after_commit",
                "delete_enabled",
                "backup_keep",
                "organize_template",
                "organize_portable_names",
                "id3_version"
            ]
        );

        let find = |name| {
            settings
                .iter()
                .find(|s| s.name == name)
                .expect(name)
                .clone()
        };
        assert_eq!(find("music_dir").value, home.root.join("Music"));
        assert_eq!(
            find("music_dir").source.to_string(),
            format!(
                "{} (music_directory)",
                home.root.join(".config/mpd/mpd.conf")
            )
        );
        assert_eq!(find("delete_enabled").value, "false");
        assert_eq!(
            find("delete_enabled").source.to_string(),
            format!(
                "{} (delete_enabled)",
                home.root.join(".config/mpdfm/config.toml")
            )
        );
        assert_eq!(find("backup_keep").source.to_string(), "built-in default");
        assert_eq!(find("organize_template").value, DEFAULT_ORGANIZE_TEMPLATE);
    }

    #[test]
    fn a_state_file_that_is_not_configured_shows_as_none() {
        let home = Home::new();

        let (config, _) = resolve_in(&home);
        let state = config
            .settings()
            .into_iter()
            .find(|s| s.name == "state_file")
            .unwrap();

        assert_eq!(state.value, "<none>");
        assert_eq!(state.source, Source::Default);
    }

    #[test]
    fn every_documented_key_is_accepted_by_the_parser() {
        let home = Home::new();
        home.dir("Music");
        home.dir("Playlists");
        home.dir("Data");
        home.write(".config/mpdfm/config.toml", &uncommented_example());

        let (_, warnings) = resolve_in(&home);

        assert!(
            !warnings.iter().any(|w| matches!(
                w,
                ConfigWarning::UnknownKey { .. }
                    | ConfigWarning::WrongType { .. }
                    | ConfigWarning::MalformedToml { .. }
            )),
            "docs/config.example.toml is not accepted as written: {warnings:#?}"
        );
    }
}
