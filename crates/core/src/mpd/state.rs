//! MPD's state file, and the saved queue buried in the middle of it.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::mpd::state::{self, MpdState};
//! use mpdfm_core::paths::RelPath;
//! use mpdfm_core::playlist::rewrite::PathMove;
//!
//! let mut saved = MpdState::load(Utf8Path::new("/home/me/.config/mpd/state"))?;
//! println!("{} songs in the saved queue", saved.queue_paths().len());
//!
//! let moves = [PathMove::moved(
//!     RelPath::parse("electronic/kream/So Hï.mp3")?,
//!     RelPath::parse("electronic/KREAM/So Hï.mp3")?,
//! )];
//!
//! // Pure: these are the lines the preview shows and the journal records.
//! let edits = state::rewrite(&mut saved, &moves);
//! if !edits.is_empty() {
//!     saved.write()?;   // atomic temp file + rename, like a playlist
//! }
//! # Ok::<(), mpdfm_core::Error>(())
//! ```
//!
//! # The store everybody forgets
//!
//! `~/.config/mpd/state` holds the queue MPD had loaded when it last shut down,
//! as `N:relative/path` lines. 61 of them on this machine (`docs/PLAN.md` §1).
//! They are relative to `music_directory` exactly as a playlist's are, they break
//! exactly as silently when a `mv` goes past them, and unlike the playlists
//! nobody looks at them until the queue comes back empty-handed.
//!
//! # The file, as MPD actually writes it
//!
//! ```text
//! sw_volume: 45
//! audio_device_state:1:PipeWire Sound Server
//! state: pause
//! current: 1
//! lastloadedplaylist:
//! playlist_begin
//! 0:electronic/KREAM - So Hï [c0D2h71bFFI]/01 So Hï.mp3
//! 1:hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3
//! playlist_end
//! ```
//!
//! Three shapes matter beyond the obvious one:
//!
//! - `audio_device_state:1:PipeWire Sound Server` has **no space** after the
//!   first colon and two colons in it. It is not a queue entry and it is outside
//!   the queue section, so nothing here looks at it twice;
//! - a queue entry that is not a plain database song — a stream URL, or a song
//!   with a start or end time — is written in MPD's *long* format: the `N:` line
//!   carries `song_begin: <uri>` and several `Tag: value` lines and a `song_end`
//!   follow it. Any entry may also be followed by `Prio: N`. So a line inside the
//!   section that does not begin `<digits>:` belongs to the entry above it, and
//!   travels with it;
//! - `lastloadedplaylist:` is written with a trailing space when nothing is
//!   loaded. That space is the user's file, not MPDFM's to tidy.
//!
//! # Fidelity, same contract as task 06
//!
//! Everything that is not a rewritten queue line comes back **byte-for-byte**:
//! unknown keys, `audio_device_state`, the ordering, the trailing space, the
//! line ending, the presence or absence of a final newline. [`StateLine::Other`]
//! is what makes that true by construction — a line MPDFM has no reading of is
//! held as its own bytes and written back unchanged.
//!
//! # `current:` is a position, not a song id
//!
//! MPD writes `current: <queue position>` (`PlaylistState.cxx`, via
//! `OrderToPosition`), 0-based, and reads it back as a position too. So removing
//! an entry has to renumber it, and this module does
//! ([`rewrite`]): the new value is how many surviving entries used to sit before
//! it, which lands on the entry that took the removed one's place. If the whole
//! queue goes, the `current:` line goes with it — MPD writes none when there is
//! no current song.
//!
//! # Writing it while MPD runs achieves nothing
//!
//! **MPD overwrites this file from memory when it shuts down.** A rewrite behind
//! a running daemon is erased the moment someone stops it, so MPDFM does not
//! pretend otherwise: the preview warns, naming each moved file that is in the
//! live queue, and leaves the file alone. The on-disk rewrite happens when the
//! daemon is *not* reachable, which is the case where it is the queue. See
//! [`Live`][crate::ops::Live] for the seam that decides which, and
//! `docs/tasks/14-mpd-state-queue.md` for why that is the default.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use camino::{Utf8Path, Utf8PathBuf};

use crate::paths::RelPath;
use crate::playlist::rewrite::{LineEdit, PathMove};
use crate::playlist::{LineEnding, parse as playlist_parse, write as playlist_write};
use crate::{Error, Result};

/// The line that opens the saved queue.
const QUEUE_BEGIN: &str = "playlist_begin";

/// The line that closes it.
const QUEUE_END: &str = "playlist_end";

/// The key holding the current queue position.
const CURRENT: &str = "current:";

/// Why MPD's state file could not be parsed, or an edit could not be applied to
/// it.
///
/// The shapes mirror [`RewriteError`][crate::playlist::rewrite::RewriteError],
/// for the same reason: a state edit is planned during the preview, written into
/// the journal, and applied some time later, so "line 7" on its own is not enough
/// to be sure it still names what it named.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// The file is not valid UTF-8. MPD writes it as UTF-8; MPDFM never guesses
    /// an encoding (`docs/PLAN.md` safety invariant 8).
    #[error("{path}: not valid UTF-8 (first bad byte at offset {offset})")]
    NotUtf8 {
        /// The state file.
        path: String,
        /// Byte offset of the first invalid sequence.
        offset: usize,
    },

    /// The line is not what the plan saw: MPD has saved its state since, or the
    /// edit came from a journal written against a different version of the file.
    /// Nothing is written.
    #[error(
        "{path} line {line}: the plan expected {expected:?} but the file says {found:?}",
        line = line + 1
    )]
    Stale {
        /// The state file.
        path: Utf8PathBuf,
        /// The 0-based line index; the message adds one.
        line: usize,
        /// What [`LineEdit::old`] said.
        expected: String,
        /// What the file says now.
        found: String,
    },

    /// The edit names a line past the end of the file — the same staleness, in
    /// the shape a shorter queue produces.
    #[error("{path} has {lines} lines; the plan names line {line}", line = line + 1)]
    PastTheEnd {
        /// The state file.
        path: Utf8PathBuf,
        /// The 0-based line index; the message adds one.
        line: usize,
        /// How many lines the file actually has.
        lines: usize,
    },

    /// Two [`LineEdit`]s name the same line. Applying either and dropping the
    /// other would be a silent choice between two intentions.
    #[error("{path} line {line}: two edits name the same line", line = line + 1)]
    Conflict {
        /// The state file.
        path: Utf8PathBuf,
        /// The 0-based line index; the message adds one.
        line: usize,
    },
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// One line of MPD's state file.
///
/// Every variant renders back to the exact bytes it was parsed from —
/// [`StateLine::line`] is that rendering — which is what makes the round-trip
/// property hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateLine {
    /// Anything that is not a queue entry: a `key: value` line, an
    /// `audio_device_state`, the `playlist_begin`/`playlist_end` markers
    /// themselves, a tag line belonging to a long-format entry, an unknown key
    /// from a newer MPD. Held as its own bytes and never interpreted.
    Other(String),

    /// `N:relative/path` inside the queue section, naming a file under
    /// `music_directory`. The one line shape this module rewrites.
    QueueEntry {
        /// Its position in the queue, as the file spells it.
        index: u32,
        /// The file it names.
        rel: RelPath,
        /// The whole line, which is what gets written back.
        raw: String,
    },

    /// `N:…` inside the queue section whose target is not a library path — a
    /// stream URL, or the `song_begin:` of a long-format entry.
    ///
    /// Its *number* is maintained, because MPD's queue indices have to stay
    /// consecutive and this entry occupies one of them; its text is never
    /// otherwise touched.
    QueueOther {
        /// Its position in the queue, as the file spells it.
        index: u32,
        /// The whole line, which is what gets written back.
        raw: String,
    },
}

impl StateLine {
    /// A queue entry built from its parts, with `raw` derived from them.
    ///
    /// This is how [`rewrite`] produces a changed line — from the number and the
    /// path, rather than by editing the old string and hoping the fields still
    /// agree with it.
    ///
    /// ```
    /// use mpdfm_core::mpd::state::StateLine;
    /// use mpdfm_core::paths::RelPath;
    ///
    /// let rel = RelPath::parse("pop/new/a.mp3")?;
    /// assert_eq!(StateLine::queue_entry(7, rel).line(), "7:pop/new/a.mp3");
    /// # Ok::<(), mpdfm_core::paths::PathError>(())
    /// ```
    #[must_use]
    pub fn queue_entry(index: u32, rel: RelPath) -> Self {
        let raw = format!("{index}:{rel}");
        Self::QueueEntry { index, rel, raw }
    }

    /// The exact bytes of this line, without its line ending.
    #[must_use]
    pub fn line(&self) -> &str {
        match self {
            Self::Other(text)
            | Self::QueueEntry { raw: text, .. }
            | Self::QueueOther { raw: text, .. } => text,
        }
    }

    /// The file this line names, for a queue entry that names one.
    #[must_use]
    pub fn rel(&self) -> Option<&RelPath> {
        match self {
            Self::QueueEntry { rel, .. } => Some(rel),
            _ => None,
        }
    }

    /// Its queue position, for a line that occupies one.
    #[must_use]
    pub fn index(&self) -> Option<u32> {
        match self {
            Self::QueueEntry { index, .. } | Self::QueueOther { index, .. } => Some(*index),
            Self::Other(_) => None,
        }
    }

    /// This line renumbered to `index`, or `None` if it is not a queue line or
    /// already has that number.
    fn renumbered(&self, index: u32) -> Option<String> {
        match self {
            Self::Other(_) => None,
            Self::QueueEntry {
                index: was, rel, ..
            } => (*was != index).then(|| format!("{index}:{rel}")),
            Self::QueueOther { index: was, raw } => (*was != index).then(|| {
                // Only the number changes; whatever followed the colon — a
                // `song_begin:`, a stream URL with its own colons — is copied.
                let rest = raw.split_once(':').map_or("", |(_, rest)| rest);
                format!("{index}:{rest}")
            }),
        }
    }
}

/// MPD's state file, parsed so that it can be written back byte-identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpdState {
    /// The file as found, which may be a symlink.
    path: Utf8PathBuf,
    /// Symlink resolved — this is what the writer replaces.
    real_path: Utf8PathBuf,
    lines: Vec<StateLine>,
    line_ending: LineEnding,
    trailing_newline: bool,
}

impl MpdState {
    /// Read and parse the state file at `path`, resolving it if it is a symlink.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be read or a symlink cannot be resolved,
    /// and [`Error::State`] if its bytes are not UTF-8.
    pub fn load(path: &Utf8Path) -> Result<Self> {
        let real_path = crate::playlist::real_path_of(path)?;
        let bytes = std::fs::read(&real_path).map_err(|source| Error::Io {
            path: real_path.to_string(),
            source,
        })?;
        let mut state = Self::from_bytes(path, &bytes)?;
        state.real_path = real_path;
        Ok(state)
    }

    /// Parse bytes that are already in hand, with no disk access at all.
    ///
    /// `real_path` is set to `path` verbatim: **no symlink is resolved**, which
    /// makes this the right constructor for bytes from a backup or a test and the
    /// wrong one for the file on disk.
    ///
    /// # Errors
    ///
    /// [`StateError::NotUtf8`] if the bytes are not UTF-8.
    pub fn from_bytes(path: &Utf8Path, bytes: &[u8]) -> std::result::Result<Self, StateError> {
        let text = std::str::from_utf8(bytes).map_err(|err| StateError::NotUtf8 {
            path: path.to_string(),
            offset: err.valid_up_to(),
        })?;

        // Exactly task 06's rules, from task 06's code: one line ending for the
        // whole file, decided by counting, and a final line that keeps its lack
        // of a terminator.
        let line_ending = playlist_parse::line_ending_of(text);
        let trailing_newline = text.ends_with('\n');
        let raw: Vec<&str> = playlist_parse::split_lines(text, line_ending).collect();
        let section = section_of(&raw);
        let lines = raw
            .iter()
            .enumerate()
            .map(|(at, line)| classify(line, section.as_ref().is_some_and(|it| it.contains(&at))))
            .collect();

        Ok(Self {
            path: path.to_owned(),
            real_path: path.to_owned(),
            lines,
            line_ending,
            trailing_newline,
        })
    }

    /// The state file as found — the symlink, when it is one.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// The file the writer replaces: [`MpdState::path`] with symlinks resolved.
    #[must_use]
    pub fn real_path(&self) -> &Utf8Path {
        &self.real_path
    }

    /// Every line, in file order.
    #[must_use]
    pub fn lines(&self) -> &[StateLine] {
        &self.lines
    }

    /// Whether the file has a `playlist_begin` … `playlist_end` section at all.
    ///
    /// A state file MPD wrote with nothing queued has an empty one; a state file
    /// from a daemon that has never played has none, and neither is an error —
    /// there is simply no saved queue to rewrite.
    #[must_use]
    pub fn has_queue(&self) -> bool {
        section_of_lines(&self.lines).is_some()
    }

    /// The library paths in the saved queue, in queue order.
    ///
    /// Entries MPDFM cannot name as library paths — a stream URL — are not here;
    /// they are still in [`MpdState::lines`], and still counted when the queue is
    /// renumbered.
    #[must_use]
    pub fn queue_paths(&self) -> Vec<&RelPath> {
        self.lines.iter().filter_map(StateLine::rel).collect()
    }

    /// The queue position `current:` names, if the file has one.
    #[must_use]
    pub fn current(&self) -> Option<u32> {
        current_line(&self.lines, self.section().as_ref()).map(|(_, _, position, _)| position)
    }

    /// The file's exact bytes, as [`MpdState::write`] would write them.
    ///
    /// For an unmodified state file this is byte-identical to what was parsed.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let ending = self.line_ending.as_str();
        let mut out = String::new();
        for (at, line) in self.lines.iter().enumerate() {
            if at > 0 {
                out.push_str(ending);
            }
            out.push_str(line.line());
        }
        // Guarded on there being a line to terminate, so an empty file stays
        // empty rather than becoming a lone newline.
        if self.trailing_newline && !self.lines.is_empty() {
            out.push_str(ending);
        }
        out.into_bytes()
    }

    /// Replace [`MpdState::real_path`] with [`MpdState::to_bytes`], atomically.
    ///
    /// Temp file in the same directory, `fsync`, `rename` — the same writer the
    /// playlists go through (`docs/PLAN.md` safety invariant 6), so a crash
    /// leaves either the old state file or the new one and MPD never reads a
    /// half-written queue.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the directory cannot be written to, the write or the
    /// rename fails, or `real_path` is still a symlink — which means this value
    /// came from [`MpdState::from_bytes`] rather than from [`MpdState::load`].
    pub fn write(&self) -> Result<()> {
        playlist_write::replace_file(
            &self.real_path,
            &self.to_bytes(),
            playlist_write::Stop::Never,
        )
    }

    /// The interior of the queue section: the line indices between the markers.
    fn section(&self) -> Option<Range<usize>> {
        section_of_lines(&self.lines)
    }
}

// ---------------------------------------------------------------------------
// Parsing one line
// ---------------------------------------------------------------------------

/// Read one line, knowing whether it is inside the queue section.
///
/// Outside it, nothing is a queue entry: `audio_device_state:1:visualizer` is
/// not an entry and neither is any future key that happens to look like one.
fn classify(line: &str, in_queue: bool) -> StateLine {
    if !in_queue {
        return StateLine::Other(line.to_owned());
    }
    let Some((index, rest)) = split_index(line) else {
        // A tag line, a `song_end`, a `Prio: 3` — part of the entry above it.
        return StateLine::Other(line.to_owned());
    };
    match RelPath::parse(rest) {
        Ok(rel) => StateLine::QueueEntry {
            index,
            rel,
            raw: line.to_owned(),
        },
        // A stream URL, an absolute path, a `song_begin:`. Preserved verbatim,
        // and never rewritten — the same promise `Entry::Unparsed` makes.
        Err(_) => StateLine::QueueOther {
            index,
            raw: line.to_owned(),
        },
    }
}

/// `N:rest` split into its number and its remainder, or `None` if the line does
/// not start that way.
fn split_index(line: &str) -> Option<(u32, &str)> {
    let digits = line.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 {
        return None;
    }
    let rest = line[digits..].strip_prefix(':')?;
    let index = line[..digits].parse::<u32>().ok()?;
    Some((index, rest))
}

/// The file's `current:` line: its index, and the text before its number, the
/// number, and the text after it.
///
/// Looked for **outside** the queue section. MPD writes it in the key head, and a
/// continuation line inside the queue that happened to start `current:` would be
/// a song's tag rather than the player's position — reading it as the latter
/// would renumber a line a removal is also entitled to delete.
fn current_line<'a>(
    lines: &'a [StateLine],
    section: Option<&Range<usize>>,
) -> Option<(usize, &'a str, u32, &'a str)> {
    lines
        .iter()
        .enumerate()
        .filter(|(at, _)| !section.is_some_and(|queue| queue.contains(at)))
        .find_map(|(at, line)| {
            current_of(line.line()).map(|(head, position, tail)| (at, head, position, tail))
        })
}

/// A `current:` line split into the text before its number, the number, and the
/// text after it — so a rewrite changes the number and nothing else.
fn current_of(line: &str) -> Option<(&str, u32, &str)> {
    let rest = line.strip_prefix(CURRENT)?;
    let value = rest.trim_start();
    let head = line.len() - value.len();
    let trimmed = value.trim_end();
    let position = trimmed.parse::<u32>().ok()?;
    Some((&line[..head], position, &value[trimmed.len()..]))
}

/// The interior of the queue section in a list of raw lines.
///
/// The first `playlist_begin` and the first `playlist_end` after it. A
/// `playlist_begin` with no `playlist_end` is **no section at all**: a truncated
/// file is not something to renumber, and reading to the end of the file instead
/// would treat whatever MPD writes after the queue as queue entries.
fn section_of(lines: &[&str]) -> Option<Range<usize>> {
    bounds(lines.iter().copied())
}

/// The same, over parsed lines. The markers are [`StateLine::Other`], so their
/// text is still exactly what was read.
fn section_of_lines(lines: &[StateLine]) -> Option<Range<usize>> {
    bounds(lines.iter().map(StateLine::line))
}

fn bounds<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Range<usize>> {
    let mut begin = None;
    for (at, line) in lines.enumerate() {
        match (begin, line) {
            (None, QUEUE_BEGIN) => begin = Some(at),
            (Some(begin), QUEUE_END) => return Some(begin + 1..at),
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Planning the edits
// ---------------------------------------------------------------------------

/// Which lines of the saved queue `moves` touches, and what they become —
/// applying them to `state` as it goes.
///
/// The returned edits are in ascending line order, each line named once, indexed
/// into the [`MpdState::lines`] of the state **as it was passed in**. That is
/// what the preview renders, what the journal records, and what [`applied`] can
/// replay against the backup copy later.
///
/// The matching rule is the playlists' rule, unchanged (`docs/PLAN.md` safety
/// invariant 3): a line is rewritten when its parsed [`RelPath`] *equals* a
/// moved path. No prefix test, no case folding, no string replacement.
///
/// Three things happen, in this order, because each needs the one before it:
///
/// 1. **deletions** take their `N:` line and the continuation lines that belong
///    to it — a `Prio:` or a long-format entry's tags would otherwise be left
///    attached to the entry above;
/// 2. **renumbering** gives every surviving queue line its new consecutive
///    position, whether or not its path changed;
/// 3. **`current:`** is remapped onto the renumbered queue, or dropped when
///    nothing is left to be current.
///
/// A state file with no queue section, or one no move touches, yields no edits
/// and leaves `state` untouched.
///
/// Not `#[must_use]`: the mutation of `state` is the other half of what this
/// does, and a caller that only wants the file put right — a test, a future
/// `doctor --fix` — has no use for the list.
pub fn rewrite(state: &mut MpdState, moves: &[PathMove]) -> Vec<LineEdit> {
    let Some(section) = state.section() else {
        return Vec::new();
    };

    // Pass 1 — what goes, and what is renamed.
    let mut removed: BTreeSet<usize> = BTreeSet::new();
    let mut renamed: BTreeMap<usize, RelPath> = BTreeMap::new();
    for at in section.clone() {
        let Some(rel) = state.lines[at].rel() else {
            continue;
        };
        let Some(path_move) = moves.iter().find(|candidate| &candidate.from == rel) else {
            continue;
        };
        match &path_move.to {
            Some(to) => {
                renamed.insert(at, to.clone());
            }
            None => {
                removed.insert(at);
                removed.extend(continuations(&state.lines, at, section.end));
            }
        }
    }
    if removed.is_empty() && renamed.is_empty() {
        return Vec::new();
    }

    // Pass 2 — the queue that is left, as (line, old position) pairs.
    //
    // Renumbering happens only when something was removed, because a removal is
    // the only thing that can break the sequence. A file whose indices were not
    // consecutive to begin with is not a file MPD wrote and is not this module's
    // to tidy: a move rewrites the lines it matches and leaves the rest alone.
    let survivors: Vec<(usize, u32)> = section
        .clone()
        .filter_map(|at| state.lines[at].index().map(|index| (at, index)))
        .filter(|(at, _)| !removed.contains(at))
        .collect();
    let renumber = !removed.is_empty();

    let mut edits: BTreeMap<usize, LineEdit> = BTreeMap::new();
    let edit = |at: usize, old: &StateLine, new: Option<String>| LineEdit {
        entry: at,
        old: old.line().to_owned(),
        new,
    };
    for at in &removed {
        edits.insert(*at, edit(*at, &state.lines[*at], None));
    }
    for (position, (at, was)) in survivors.iter().enumerate() {
        let line = &state.lines[*at];
        let index = if renumber {
            u32::try_from(position).unwrap_or(u32::MAX)
        } else {
            *was
        };
        // A renamed entry is rebuilt from its parts; an untouched one only needs
        // its number put right, and needs no edit at all when the number is
        // already right.
        let new = match renamed.get(at) {
            Some(to) => Some(StateLine::queue_entry(index, to.clone()).line().to_owned()),
            None => line.renumbered(index),
        };
        if let Some(new) = new.filter(|new| new != line.line()) {
            edits.insert(*at, edit(*at, line, Some(new)));
        }
    }

    // Pass 3 — `current:`, which is a position into the queue just renumbered,
    // and so only moves when the queue got shorter.
    if renumber && let Some((at, new)) = current_edit(&state.lines, Some(&section), &survivors) {
        edits.insert(at, edit(at, &state.lines[at], new));
    }

    let edits: Vec<LineEdit> = edits.into_values().collect();
    // Applying our own edits cannot fail: they were built from these very lines.
    // Should that ever stop being true, leaving `state` as it was and reporting
    // no edits is the safe half of the bargain.
    match applied(state, &edits) {
        Ok(updated) => {
            *state = updated;
            edits
        }
        Err(_) => Vec::new(),
    }
}

/// The lines that belong to the entry starting at `at`: everything up to the next
/// queue line or the end of the section.
///
/// A short-format entry followed by another has none. One followed by `Prio: 3`,
/// or a long-format entry with its `Time:`/`Artist:`/`song_end` lines, has those.
fn continuations(lines: &[StateLine], at: usize, end: usize) -> Vec<usize> {
    (at + 1..end)
        .take_while(|&next| matches!(lines[next], StateLine::Other(_)))
        .collect()
}

/// What the `current:` line becomes once `survivors` are renumbered from 0.
///
/// `None` for a file with no such line, or one whose position does not move. The
/// inner `None` removes the line, which is what MPD itself writes when there is
/// no current song.
fn current_edit(
    lines: &[StateLine],
    section: Option<&Range<usize>>,
    survivors: &[(usize, u32)],
) -> Option<(usize, Option<String>)> {
    let (at, head, position, tail) = current_line(lines, section)?;

    if survivors.is_empty() {
        return Some((at, None));
    }
    // How many surviving entries used to sit before it. For a position that was
    // itself removed that is the entry which has taken its place; for one after
    // a removal it is the same song, shifted down.
    let moved = survivors
        .iter()
        .filter(|(_, was)| *was < position)
        .count()
        // A position past the end of the shortened queue — every entry from it
        // onwards was removed — clamps to the last surviving entry rather than
        // naming a song that is not there.
        .min(survivors.len() - 1);
    let moved = u32::try_from(moved).unwrap_or(u32::MAX);
    (moved != position).then(|| (at, Some(format!("{head}{moved}{tail}"))))
}

// ---------------------------------------------------------------------------
// Applying them
// ---------------------------------------------------------------------------

/// `state` with `edits` applied, or the reason they cannot be.
///
/// Every line is checked against [`LineEdit::old`] before any is changed, so a
/// rejected edit leaves the caller with the state it passed in. Replacement lines
/// are re-classified from their text rather than trusted as strings, by the same
/// `classify` that read the lines they replace.
///
/// # Errors
///
/// [`StateError::Stale`] for a line that no longer reads as the plan saw it —
/// MPD has saved its state since — [`StateError::PastTheEnd`] for one past the
/// end of the file, and [`StateError::Conflict`] for two edits naming one line.
pub fn applied(state: &MpdState, edits: &[LineEdit]) -> std::result::Result<MpdState, StateError> {
    let mut by_line: BTreeMap<usize, &LineEdit> = BTreeMap::new();
    for edit in edits {
        let Some(line) = state.lines.get(edit.entry) else {
            return Err(StateError::PastTheEnd {
                path: state.real_path.clone(),
                line: edit.entry,
                lines: state.lines.len(),
            });
        };
        if line.line() != edit.old {
            return Err(StateError::Stale {
                path: state.real_path.clone(),
                line: edit.entry,
                expected: edit.old.clone(),
                found: line.line().to_owned(),
            });
        }
        if by_line.insert(edit.entry, edit).is_some() {
            return Err(StateError::Conflict {
                path: state.real_path.clone(),
                line: edit.entry,
            });
        }
    }

    // The section is taken from the file as it stands, before any removal shifts
    // an index: a replacement line is inside the queue exactly when the line it
    // replaces was.
    let section = state.section();
    let lines = state
        .lines
        .iter()
        .enumerate()
        .filter_map(|(at, line)| match by_line.get(&at) {
            None => Some(line.clone()),
            Some(edit) => edit
                .new
                .as_deref()
                .map(|new| classify(new, section.as_ref().is_some_and(|it| it.contains(&at)))),
        })
        .collect();

    let mut out = state.clone();
    out.lines = lines;
    Ok(out)
}

/// The bytes a commit leaves in the state file, given the bytes it had before.
///
/// Undo's and recover's "is this still what we left here?" question (task 12):
/// the copy in the transaction's backup directory is the *before*, this is the
/// *after*, and a file that matches neither was written by MPD in between.
///
/// # Errors
///
/// [`Error::State`] if `original` is not UTF-8 or the edits do not apply to it —
/// which, for the backup of the very file they were applied to, means the backup
/// is not what it claims to be.
pub fn after(path: &Utf8Path, original: &[u8], edits: &[LineEdit]) -> Result<Vec<u8>> {
    let state = MpdState::from_bytes(path, original)?;
    Ok(applied(&state, edits)?.to_bytes())
}

/// Read the state file, check `edits` against it and work out the bytes to
/// write — **without writing any of them**.
///
/// The commit needs this separately from the write for the same reason the
/// playlists do ([`rewrite::prepare`][crate::playlist::rewrite::prepare]): a
/// state file MPD has saved since the preview must stop the transaction before
/// the first file moves, not after.
///
/// # Errors
///
/// [`Error::Io`] if the file cannot be read, and [`Error::State`] if it is not
/// UTF-8 or an edit no longer matches it.
pub fn prepare(path: &Utf8Path, edits: &[LineEdit]) -> Result<Prepared> {
    let state = MpdState::load(path)?;
    let updated = applied(&state, edits)?;
    Ok(Prepared {
        real_path: state.real_path.clone(),
        bytes: updated.to_bytes(),
    })
}

/// A verified state-file edit, with the bytes to write already computed.
///
/// Holding the bytes rather than re-reading them at write time is deliberate:
/// re-reading would open a window where a state file MPD saved in between is
/// written back from the newer bytes without ever having been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    real_path: Utf8PathBuf,
    bytes: Vec<u8>,
}

impl Prepared {
    /// The file that will be replaced — the state file with symlinks resolved.
    #[must_use]
    pub fn real_path(&self) -> &Utf8Path {
        &self.real_path
    }

    /// The bytes it will hold.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Replace the state file, atomically.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if it cannot be replaced.
    pub fn write(&self) -> Result<()> {
        replace(&self.real_path, &self.bytes)
    }
}

/// Replace the state file's bytes, atomically and through its resolved path.
///
/// The primitive for the callers that compute the bytes somewhere else: `recover`
/// finishing a transaction, and `undo` putting the backup back.
///
/// # Errors
///
/// [`Error::Io`] if the file cannot be replaced.
pub fn replace(path: &Utf8Path, bytes: &[u8]) -> Result<()> {
    let real_path = crate::playlist::real_path_of(path)?;
    playlist_write::replace_file(&real_path, bytes, playlist_write::Stop::Never)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state file from its lines, with no disk anywhere near it.
    fn state(lines: &[&str]) -> MpdState {
        let body = lines.join("\n") + "\n";
        MpdState::from_bytes(Utf8Path::new("/c/mpd/state"), body.as_bytes())
            .expect("the test's own bytes are UTF-8")
    }

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).expect("test path")
    }

    fn lines(state: &MpdState) -> Vec<&str> {
        state.lines().iter().map(StateLine::line).collect()
    }

    /// Where the first queue entry lands in a [`with_queue`] file built with no
    /// `current:` line: two head lines, then `playlist_begin`.
    const QUEUE_LINE: usize = 3;

    /// The head of a real state file, trimmed, with a queue of `tracks`.
    fn with_queue(current: Option<u32>, tracks: &[&str]) -> MpdState {
        let mut lines = vec!["sw_volume: 45", "audio_device_state:1:visualizer"];
        let current = current.map(|position| format!("current: {position}"));
        if let Some(current) = &current {
            lines.push(current);
        }
        lines.push("playlist_begin");
        let queued: Vec<String> = tracks
            .iter()
            .enumerate()
            .map(|(at, track)| format!("{at}:{track}"))
            .collect();
        lines.extend(queued.iter().map(String::as_str));
        lines.push("playlist_end");
        state(&lines)
    }

    #[test]
    fn a_line_outside_the_queue_section_is_never_an_entry() {
        let parsed = state(&[
            "0:not/in/the/queue.mp3",
            "audio_device_state:1:PipeWire Sound Server",
            "playlist_begin",
            "0:pop/a.mp3",
            "playlist_end",
            "1:after/the/section.mp3",
        ]);

        assert_eq!(parsed.queue_paths(), vec![&rel("pop/a.mp3")]);
        assert!(matches!(parsed.lines()[0], StateLine::Other(_)));
        assert!(matches!(parsed.lines()[1], StateLine::Other(_)));
        assert!(matches!(parsed.lines()[5], StateLine::Other(_)));
    }

    #[test]
    fn a_queue_entry_that_is_not_a_library_path_keeps_its_position() {
        let parsed = state(&[
            "playlist_begin",
            "0:https://ice.somafm.com/groovesalad",
            "1:pop/a.mp3",
            "playlist_end",
        ]);

        assert_eq!(parsed.queue_paths(), vec![&rel("pop/a.mp3")]);
        assert_eq!(parsed.lines()[1].index(), Some(0));
        assert_eq!(parsed.lines()[1].rel(), None);
    }

    #[test]
    fn a_begin_with_no_end_is_not_a_section() {
        let parsed = state(&["playlist_begin", "0:pop/a.mp3"]);
        assert!(!parsed.has_queue());
        assert!(parsed.queue_paths().is_empty());
    }

    #[test]
    fn a_move_rewrites_only_its_own_line() {
        let mut parsed = with_queue(Some(1), &["pop/a.mp3", "pop/b.mp3", "pop/c.mp3"]);
        let before = parsed.lines().len();

        let edits = rewrite(
            &mut parsed,
            &[PathMove::moved(rel("pop/b.mp3"), rel("jazz/b.mp3"))],
        );

        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].old, "1:pop/b.mp3");
        assert_eq!(edits[0].new.as_deref(), Some("1:jazz/b.mp3"));
        assert_eq!(parsed.lines().len(), before);
        assert_eq!(parsed.current(), Some(1), "current: is untouched by a move");
        assert_eq!(
            parsed.queue_paths(),
            vec![&rel("pop/a.mp3"), &rel("jazz/b.mp3"), &rel("pop/c.mp3")]
        );
    }

    #[test]
    fn a_removal_renumbers_what_is_left_and_moves_current_with_it() {
        let mut parsed = with_queue(Some(2), &["pop/a.mp3", "pop/b.mp3", "pop/c.mp3"]);

        let edits = rewrite(&mut parsed, &[PathMove::deleted(rel("pop/a.mp3"))]);

        // The removed line, the two renumbered ones, and `current:`.
        assert_eq!(edits.len(), 4);
        assert!(edits.iter().any(|edit| edit.is_removal()));
        assert_eq!(parsed.current(), Some(1));
        assert_eq!(
            parsed
                .lines()
                .iter()
                .filter_map(StateLine::index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn current_lands_on_the_entry_that_replaced_the_one_it_named() {
        let mut parsed = with_queue(Some(1), &["pop/a.mp3", "pop/b.mp3", "pop/c.mp3"]);
        rewrite(&mut parsed, &[PathMove::deleted(rel("pop/b.mp3"))]);
        // Was `pop/b.mp3`; position 1 is now `pop/c.mp3`.
        assert_eq!(parsed.current(), Some(1));
        assert_eq!(
            parsed.queue_paths(),
            vec![&rel("pop/a.mp3"), &rel("pop/c.mp3")]
        );
    }

    #[test]
    fn current_past_the_shortened_queue_clamps_to_the_last_entry() {
        let mut parsed = with_queue(Some(2), &["pop/a.mp3", "pop/b.mp3", "pop/c.mp3"]);
        rewrite(
            &mut parsed,
            &[
                PathMove::deleted(rel("pop/b.mp3")),
                PathMove::deleted(rel("pop/c.mp3")),
            ],
        );
        assert_eq!(parsed.current(), Some(0));
    }

    #[test]
    fn emptying_the_queue_removes_the_current_line() {
        let mut parsed = with_queue(Some(0), &["pop/a.mp3"]);
        rewrite(&mut parsed, &[PathMove::deleted(rel("pop/a.mp3"))]);

        assert_eq!(parsed.current(), None);
        assert!(parsed.has_queue(), "the markers stay");
        assert!(parsed.queue_paths().is_empty());
    }

    #[test]
    fn a_removal_takes_the_lines_that_belong_to_its_entry() {
        let mut parsed = state(&[
            "playlist_begin",
            "0:pop/a.mp3",
            "Prio: 3",
            "1:pop/b.mp3",
            "playlist_end",
        ]);

        rewrite(&mut parsed, &[PathMove::deleted(rel("pop/a.mp3"))]);

        let left: Vec<&str> = parsed.lines().iter().map(StateLine::line).collect();
        assert_eq!(left, vec!["playlist_begin", "0:pop/b.mp3", "playlist_end"]);
    }

    /// A file MPD never wrote, so not one to tidy: a move touches the line it
    /// matches and leaves the odd numbering alone.
    #[test]
    fn a_move_does_not_renumber_a_queue_that_was_already_crooked() {
        let mut parsed = state(&[
            "playlist_begin",
            "0:pop/a.mp3",
            "7:pop/b.mp3",
            "9:pop/c.mp3",
            "playlist_end",
        ]);

        let edits = rewrite(
            &mut parsed,
            &[PathMove::moved(rel("pop/b.mp3"), rel("jazz/b.mp3"))],
        );

        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new.as_deref(), Some("7:jazz/b.mp3"));
        assert_eq!(
            parsed
                .lines()
                .iter()
                .filter_map(StateLine::index)
                .collect::<Vec<_>>(),
            vec![0, 7, 9]
        );
    }

    /// A removal, on the other hand, has to put the sequence back — MPD reads
    /// these as positions.
    #[test]
    fn a_removal_does_renumber_a_crooked_queue() {
        let mut parsed = state(&[
            "playlist_begin",
            "0:pop/a.mp3",
            "7:pop/b.mp3",
            "9:pop/c.mp3",
            "playlist_end",
        ]);

        rewrite(&mut parsed, &[PathMove::deleted(rel("pop/b.mp3"))]);

        assert_eq!(
            parsed
                .lines()
                .iter()
                .filter_map(StateLine::index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    /// `current:` belongs to the key head. A tag line inside the queue that
    /// happens to start the same way is a song's, and a removal is free to take
    /// it with the entry it belongs to.
    #[test]
    fn a_current_line_inside_the_queue_is_not_the_players_position() {
        let mut parsed = state(&[
            "current: 1",
            "playlist_begin",
            "0:pop/a.mp3",
            "current: not-the-player",
            "1:pop/b.mp3",
            "playlist_end",
        ]);
        assert_eq!(parsed.current(), Some(1));

        rewrite(&mut parsed, &[PathMove::deleted(rel("pop/a.mp3"))]);

        assert_eq!(
            lines(&parsed),
            vec![
                "current: 0",
                "playlist_begin",
                "0:pop/b.mp3",
                "playlist_end"
            ],
            "the head's current: moved and the queue's namesake went with its entry"
        );
    }

    #[test]
    fn a_move_nothing_matches_changes_nothing() {
        let mut parsed = with_queue(Some(0), &["pop/a.mp3"]);
        let before = parsed.clone();

        let edits = rewrite(
            &mut parsed,
            &[PathMove::moved(rel("pop/z.mp3"), rel("pop/y.mp3"))],
        );

        assert!(edits.is_empty());
        assert_eq!(parsed, before);
    }

    #[test]
    fn a_file_with_no_queue_section_yields_no_edits() {
        let mut parsed = state(&["state: stop", "current: 0"]);
        let edits = rewrite(&mut parsed, &[PathMove::deleted(rel("pop/a.mp3"))]);
        assert!(edits.is_empty());
    }

    #[test]
    fn an_edit_whose_line_has_changed_is_refused() {
        let parsed = with_queue(None, &["pop/a.mp3"]);
        let stale = LineEdit {
            entry: QUEUE_LINE,
            old: "0:pop/somewhere-else.mp3".to_owned(),
            new: Some("0:pop/b.mp3".to_owned()),
        };

        let err = applied(&parsed, &[stale]).expect_err("the line does not read that way");
        assert!(
            matches!(
                err,
                StateError::Stale {
                    line: QUEUE_LINE,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn an_edit_past_the_end_is_refused() {
        let parsed = with_queue(None, &["pop/a.mp3"]);
        let err = applied(
            &parsed,
            &[LineEdit {
                entry: 99,
                old: String::new(),
                new: None,
            }],
        )
        .expect_err("there is no line 99");
        assert!(matches!(err, StateError::PastTheEnd { .. }), "{err:?}");
    }

    #[test]
    fn two_edits_on_one_line_are_refused() {
        let parsed = with_queue(None, &["pop/a.mp3"]);
        let twice = vec![
            LineEdit {
                entry: QUEUE_LINE,
                old: "0:pop/a.mp3".to_owned(),
                new: None,
            },
            LineEdit {
                entry: QUEUE_LINE,
                old: "0:pop/a.mp3".to_owned(),
                new: Some("0:pop/b.mp3".to_owned()),
            },
        ];

        let err = applied(&parsed, &twice).expect_err("one line, two intentions");
        assert!(
            matches!(
                err,
                StateError::Conflict {
                    line: QUEUE_LINE,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn the_file_level_properties_come_back_out() {
        for body in [
            "state: stop\n",
            "state: stop",
            "state: stop\r\ncurrent: 0\r\n",
            "",
            "playlist_begin\nplaylist_end\n",
        ] {
            let parsed =
                MpdState::from_bytes(Utf8Path::new("/c/state"), body.as_bytes()).expect("UTF-8");
            assert_eq!(
                String::from_utf8(parsed.to_bytes()).expect("UTF-8 back"),
                body,
                "{body:?} did not round-trip"
            );
        }
    }

    #[test]
    fn bytes_that_are_not_utf8_are_reported_rather_than_guessed_at() {
        let err = MpdState::from_bytes(Utf8Path::new("/c/state"), b"state: \xff\n")
            .expect_err("not UTF-8");
        assert!(
            matches!(err, StateError::NotUtf8 { offset: 7, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn current_keeps_the_spacing_the_file_had() {
        assert_eq!(current_of("current: 12"), Some(("current: ", 12, "")));
        assert_eq!(current_of("current:7"), Some(("current:", 7, "")));
        assert_eq!(current_of("current: 3 "), Some(("current: ", 3, " ")));
        assert_eq!(current_of("currently: 3"), None);
        assert_eq!(current_of("current: none"), None);
    }

    #[test]
    fn an_index_prefix_is_digits_then_a_colon_and_nothing_else() {
        assert_eq!(split_index("12:pop/a.mp3"), Some((12, "pop/a.mp3")));
        assert_eq!(split_index("0:"), Some((0, "")));
        assert_eq!(split_index("audio_device_state:1:x"), None);
        assert_eq!(split_index(":pop/a.mp3"), None);
        assert_eq!(split_index("12"), None);
        assert_eq!(split_index("playlist_end"), None);
    }
}
