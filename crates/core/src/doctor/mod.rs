//! Library health checks: what is wrong with the library, as a work queue.
//!
//! `mpdfm doctor` (task 29) is a thin renderer over [`run`]. Every check here
//! answers one question from machinery that already exists — the scan (task 05),
//! the playlist index (task 07), MPD's state file (task 14) and the tag reader
//! (task 16) — and hands back a [`Check`]: its findings, and a fix where one is
//! obvious.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::doctor::{self, Inputs, Options, Queue};
//! use mpdfm_core::library::Library;
//! use mpdfm_core::playlist::PlaylistIndex;
//!
//! let library = Library::scan(Utf8Path::new("/home/me/Music"))?;
//! let (index, index_warnings) = PlaylistIndex::load(Utf8Path::new("/home/me/.config/mpd/playlists"));
//! let inputs = Inputs { library: &library, index: &index, index_warnings: &index_warnings, queue: Queue::NotConfigured };
//!
//! let report = doctor::run(&inputs, &Options::default(), &mut |_| {});
//! println!("{} problem(s)", report.count(doctor::Severity::Problem));
//! # Ok::<(), mpdfm_core::Error>(())
//! ```
//!
//! # A doctor that cries wolf gets ignored
//!
//! So every check carries a [`Severity`], fixed per check rather than per
//! finding, and the bar for each is written down:
//!
//! | | means | e.g. |
//! |---|---|---|
//! | [`Problem`](Severity::Problem) | something is **broken**: a reference that does not resolve, a name MPDFM cannot handle | `broken-references` |
//! | [`Warning`](Severity::Warning) | the library works, and this is still worth fixing | `missing-tags` |
//! | [`Note`](Severity::Note) | worth knowing, and normal | `unreferenced-audio` |
//!
//! Missing genres are a warning and not a problem, because a library with
//! 1 000 of them still plays; calling each one a problem would bury the one
//! broken reference that matters. "Referenced by no playlist" is a note, because
//! it describes nearly every track in a 2 800-file library.
//!
//! The heuristic checks — an inconsistent album, a gap in the track numbers —
//! only speak about a directory that *is* an album by a clear majority of its
//! own tags. A directory of loose singles has a different album on every track
//! and is not inconsistent; it is a directory of singles.
//!
//! # Groups, and what each costs
//!
//! Checks belong to a [`Group`], and `--check` selects by group or by name.
//! The [`Tags`](Group::Tags) group and `same-song` read every audio file's tags
//! (about a second for the real library); nothing else opens an audio file.
//! [`identical-files`](checks::duplicates) reads **every byte** of every file
//! whose size another file shares, so it runs only with [`Options::deep`] and
//! reports [`Progress`] as it goes.

pub mod checks;

use std::collections::BTreeSet;

use serde::Serialize;

use crate::library::Library;
use crate::mpd::state::MpdState;
use crate::playlist::{IndexWarning, PlaylistIndex};

/// Whether a finding is something to fix. See the module docs for the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Something is broken.
    Problem,
    /// Worth fixing; nothing is broken.
    Warning,
    /// Worth knowing. Never a reason to act.
    Note,
}

impl Severity {
    /// The stable name `--json` uses.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Problem => "problem",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }
}

/// Which family a check belongs to — what `--check <group>` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Playlists and MPD's saved queue, against the library.
    References,
    /// What the files' tags say, and whether it agrees with itself.
    Tags,
    /// Names and directories.
    Filesystem,
    /// The same song, or the same bytes, in more than one place.
    Duplicates,
}

impl Group {
    /// Every group, in report order.
    pub const ALL: [Self; 4] = [
        Self::References,
        Self::Tags,
        Self::Filesystem,
        Self::Duplicates,
    ];

    /// The stable name `--check` and `--json` use.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::References => "references",
            Self::Tags => "tags",
            Self::Filesystem => "filesystem",
            Self::Duplicates => "duplicates",
        }
    }
}

/// What a check is, independent of what it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    /// Its stable machine-readable name, which `--check` selects on and `--json`
    /// keys by.
    pub name: &'static str,
    /// Its family.
    pub group: Group,
    /// How seriously to take what it finds.
    pub severity: Severity,
    /// What a finding means, in one line.
    pub about: &'static str,
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    /// What it is about, as the user would name it: a path, or `playlist:line`.
    pub what: String,
    /// Why it is being mentioned. May be empty.
    pub detail: String,
    /// A shell command that would fix it, when there is an obvious one. Always
    /// copy-pasteable: every path in it is quoted with [`shell_quote`].
    pub fix: Option<String>,
}

impl Item {
    /// A finding with no fix.
    #[must_use]
    pub fn new(what: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            detail: detail.into(),
            fix: None,
        }
    }

    /// The same finding with a fix.
    #[must_use]
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// One check, and everything it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What the check is.
    pub info: Info,
    /// What it found, in a stable order: by path, or by playlist and line.
    pub items: Vec<Item>,
    /// Why the check did not run, when it did not — `identical-files` without
    /// `--deep`, `broken-queue-entries` with no state file. A skipped check has
    /// no items, and saying so is the difference between "clean" and "not
    /// looked at".
    pub skipped: Option<String>,
}

impl Check {
    /// A check that ran.
    #[must_use]
    pub fn ran(info: Info, items: Vec<Item>) -> Self {
        Self {
            info,
            items,
            skipped: None,
        }
    }

    /// A check that did not run, and why.
    #[must_use]
    pub fn skipped(info: Info, why: impl Into<String>) -> Self {
        Self {
            info,
            items: Vec::new(),
            skipped: Some(why.into()),
        }
    }
}

/// MPD's saved queue, as far as it could be had.
#[derive(Debug)]
pub enum Queue {
    /// No state file is configured.
    NotConfigured,
    /// One is configured and could not be read; the message says why.
    Unreadable(String),
    /// The state file, parsed.
    Loaded(MpdState),
}

/// Everything the checks look at, already loaded.
///
/// Loaded by the caller rather than here, so the CLI and a test both decide
/// where things come from, and so a TUI that already holds a [`Library`] does
/// not walk the disk a second time.
#[derive(Debug)]
pub struct Inputs<'a> {
    /// The scanned library.
    pub library: &'a Library,
    /// MPD's playlists.
    pub index: &'a PlaylistIndex,
    /// The playlists that could not be indexed.
    pub index_warnings: &'a [IndexWarning],
    /// MPD's saved queue.
    pub queue: Queue,
}

/// Which checks to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection(BTreeSet<&'static str>);

impl Default for Selection {
    fn default() -> Self {
        Self::all()
    }
}

impl Selection {
    /// Every check.
    #[must_use]
    pub fn all() -> Self {
        Self(checks::ALL.iter().map(|info| info.name).collect())
    }

    /// The checks `names` select, where each name is a check's or a group's.
    ///
    /// An empty list selects everything, which is what no `--check` means.
    ///
    /// # Errors
    ///
    /// [`UnknownCheck`] for a name that is neither, which lists the names that
    /// are.
    pub fn parse<S: AsRef<str>>(names: &[S]) -> Result<Self, UnknownCheck> {
        if names.is_empty() {
            return Ok(Self::all());
        }
        let mut selected = BTreeSet::new();
        for name in names {
            let name = name.as_ref().trim();
            if let Some(group) = Group::ALL.iter().find(|group| group.as_str() == name) {
                selected.extend(
                    checks::ALL
                        .iter()
                        .filter(|info| info.group == *group)
                        .map(|info| info.name),
                );
            } else if let Some(info) = checks::ALL.iter().find(|info| info.name == name) {
                selected.insert(info.name);
            } else {
                return Err(UnknownCheck(name.to_owned()));
            }
        }
        Ok(Self(selected))
    }

    /// Whether `name` is selected.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    /// Whether any check in `group` is selected.
    #[must_use]
    pub fn any_in(&self, group: Group) -> bool {
        checks::ALL
            .iter()
            .any(|info| info.group == group && self.contains(info.name))
    }
}

/// A `--check` name that names nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "no check or group is called `{0}`; the groups are {groups}, and the checks are {checks}",
    groups = Group::ALL.map(Group::as_str).join(", "),
    checks = checks::ALL.iter().map(|info| info.name).collect::<Vec<_>>().join(", ")
)]
pub struct UnknownCheck(pub String);

/// How to run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Which checks.
    pub selection: Selection,
    /// Read every byte of every same-sized file to find true duplicates.
    pub deep: bool,
}

/// How far a slow pass has got, for a progress line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Reading tags: `done` of `total` audio files.
    ReadingTags {
        /// Files read so far.
        done: usize,
        /// Files to read.
        total: usize,
    },
    /// `--deep`: hashing `done` of `total` files whose size another file
    /// shares.
    Hashing {
        /// Files hashed so far.
        done: usize,
        /// Files to hash.
        total: usize,
    },
}

/// Every check that was selected, in report order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// One per selected check: grouped, and in [`checks::ALL`] order within a
    /// group.
    pub checks: Vec<Check>,
}

impl Report {
    /// How many findings of `severity` there are.
    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.checks
            .iter()
            .filter(|check| check.info.severity == severity)
            .map(|check| check.items.len())
            .sum()
    }

    /// One check by name, if it was selected.
    #[must_use]
    pub fn check(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|check| check.info.name == name)
    }
}

/// Run the selected checks.
///
/// Never fails: anything that cannot be read is a finding, and a check that
/// cannot run at all comes back [`Check::skipped`]. `progress` hears about the
/// two passes that open audio files.
pub fn run(inputs: &Inputs<'_>, options: &Options, progress: &mut dyn FnMut(Progress)) -> Report {
    let selection = &options.selection;
    let wants = |name: &str| selection.contains(name);

    // Tags are read once, and only when something needs them.
    let tags = (selection.any_in(Group::Tags) || wants("same-song"))
        .then(|| checks::tags::Table::read(inputs.library, progress));

    let mut found = Vec::new();
    for &info in checks::ALL {
        if !wants(info.name) {
            continue;
        }
        let check = match info.group {
            Group::References => checks::references::run(info, inputs),
            Group::Filesystem => checks::filesystem::run(info, inputs.library),
            Group::Tags => checks::tags::run(
                info,
                inputs.library,
                tags.as_ref().expect("tags are read when a tag check runs"),
            ),
            Group::Duplicates => {
                checks::duplicates::run(info, inputs.library, tags.as_ref(), options.deep, progress)
            }
        };
        found.push(check);
    }
    found.sort_by_key(|check| check.info.group);
    Report { checks: found }
}

/// `text` quoted for a POSIX shell, so a suggested command can be pasted as is.
///
/// Always single quotes, with `'` spelled `'\''`: nothing inside single quotes
/// is special, so this is right for every name in the real library — the
/// apostrophes, `$`, `!`, `&`, and the 1 023 non-ASCII names, which a Rust
/// `{:?}` would have turned into `\u{308}` escapes no shell understands.
///
/// ```
/// use mpdfm_core::doctor::shell_quote;
///
/// assert_eq!(shell_quote("jazz/nothing here"), "'jazz/nothing here'");
/// assert_eq!(shell_quote("01.Smokin' On.mp3"), r"'01.Smokin'\'' On.mp3'");
/// ```
#[must_use]
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_check_has_a_unique_name_that_is_not_a_group_name() {
        let mut seen = BTreeSet::new();
        for info in checks::ALL {
            assert!(seen.insert(info.name), "{} twice", info.name);
            assert!(
                Group::ALL.iter().all(|group| group.as_str() != info.name),
                "{} is also a group",
                info.name
            );
        }
    }

    #[test]
    fn a_group_selects_exactly_its_checks() {
        let tags = Selection::parse(&["tags"]).unwrap();
        for info in checks::ALL {
            assert_eq!(
                tags.contains(info.name),
                info.group == Group::Tags,
                "{}",
                info.name
            );
        }
    }

    #[test]
    fn names_and_groups_combine_and_nothing_means_everything() {
        let some = Selection::parse(&["broken-references", "duplicates"]).unwrap();
        assert!(some.contains("broken-references"));
        assert!(some.contains("same-song"));
        assert!(!some.contains("missing-tags"));
        assert_eq!(Selection::parse::<&str>(&[]).unwrap(), Selection::all());
    }

    #[test]
    fn an_unknown_name_is_refused_with_the_list_of_known_ones() {
        let err = Selection::parse(&["tag"]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("`tag`"), "{message}");
        assert!(message.contains("tags"), "{message}");
        assert!(message.contains("broken-references"), "{message}");
    }

    #[test]
    fn a_quoted_name_survives_a_shell() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("So Hi\u{308}"), "'So Hi\u{308}'");
    }
}
