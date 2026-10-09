//! Finding things: the line you type a pattern into, and the walk that answers
//! `F`.
//!
//! ```text
//!  ┌ hiphop/MF DOOM - Mm..Food ──────────────┐
//!  │     01 Beef Rap.mp3             3:24 320k│
//!  │ ▸   02 Hoe Cakes.mp3            4:02 320k│   `/hoe` put the cursor here
//!  │     03 Potholderz.mp3           2:58 320k│
//!  └──────────────────────────────────────────┘
//!  0 marked · 0 pending · sort name · focus files
//!  /hoe · 1 match
//! ```
//!
//! # Three keys, one line
//!
//! `/`, `f` and `F` differ in **what happens to the query**, not in how it is
//! typed, so they are one [`Prompt`] with a [`Kind`] on it rather than three
//! line editors:
//!
//! | key | while typing | `enter` | `esc` |
//! | --- | --- | --- | --- |
//! | `/` | the cursor follows the first match | keeps the position | puts the cursor back |
//! | `f` | the listing narrows as you type | leaves the filter on | puts the old filter back |
//! | `F` | nothing — the library is not walked per keystroke | starts the walk | nothing was started |
//!
//! The editing itself is [`Input`]'s, the same one the `:` line and every tag
//! field use, so backspacing over a `ï` cannot work in one of them and not the
//! others.
//!
//! # What `esc` has to put back
//!
//! Both of the things the line changed while it was open: the cursor
//! ([`Prompt::origin`]) and the filter that was in force before it
//! ([`Prompt::restore`]). The second is why `restore` is an `Option<Query>` and
//! not a flag — `f` typed while a filter is already on must put *that* filter
//! back and not simply clear it, and "it was off" is one of the values.
//!
//! # Where the matching lives
//!
//! Nowhere near here. The grammar is [`mpdfm_core::query`] and the matching
//! against a listing is `views::browser` — this module parses nothing and matches nothing. All it
//! holds is the text, the error the parser last gave for it, and the two things
//! `esc` restores.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mpdfm_core::query::{FindProgress, Query, QueryError};

use crate::tui::widgets::input::Input;

/// What the line being typed is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `/` — move the cursor to the matches in the current listing.
    Search,
    /// `f` — narrow the current listing, and stay narrowed.
    Filter,
    /// `F` — walk the whole library and show the hits as a flat listing.
    Find,
}

impl Kind {
    /// The character the line is drawn behind, which is also the key that opened
    /// it.
    ///
    /// Three different ones because the three do different things, and a line
    /// that looked the same whichever key opened it would be a line whose
    /// `enter` the user could not predict.
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Search => "/",
            Self::Filter => "f ",
            Self::Find => "F ",
        }
    }

    /// What this is called in a message and in the log.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Filter => "filter",
            Self::Find => "find",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The pattern being typed, and what `esc` would put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    kind: Kind,
    line: Input,
    /// The parser's last complaint about this text, shown under it.
    error: Option<String>,
    /// Where the listing's cursor was when the line opened.
    origin: usize,
    /// The filter that was in force when the line opened.
    restore: Option<Query>,
    /// How many rows matched, as of the last keystroke. `None` before the first
    /// one, and for `F`, which has not walked anything yet.
    matches: Option<usize>,
}

impl Prompt {
    /// A fresh line.
    ///
    /// `origin` is the listing cursor and `restore` the filter in force — the
    /// two things `esc` undoes.
    #[must_use]
    pub fn new(kind: Kind, origin: usize, restore: Option<Query>) -> Self {
        Self {
            kind,
            line: Input::default(),
            error: None,
            origin,
            restore,
            matches: None,
        }
    }

    /// What this line is for.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// What has been typed.
    #[must_use]
    pub fn text(&self) -> &str {
        self.line.text()
    }

    /// Where the cursor is, as a byte offset into [`Prompt::text`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.line.cursor()
    }

    /// The parser's complaint, if the text does not parse.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The listing cursor the line opened on, which `esc` restores.
    #[must_use]
    pub fn origin(&self) -> usize {
        self.origin
    }

    /// The filter the line opened with, which `esc` restores.
    #[must_use]
    pub fn restore(&self) -> Option<&Query> {
        self.restore.as_ref()
    }

    /// How many rows matched, last time anybody counted.
    #[must_use]
    pub fn matches(&self) -> Option<usize> {
        self.matches
    }

    /// Record how many rows match, for the line to say so.
    ///
    /// Set by the shell, which is the only thing that has a listing to count
    /// against; `None` means nobody has counted, which is not the same as zero
    /// and must not read as "no matches".
    pub fn note(&mut self, matches: Option<usize>) {
        self.matches = matches;
    }

    /// Record why the text will not parse, and leave it open to be edited.
    pub fn fail(&mut self, message: impl std::fmt::Display) {
        self.error = Some(message.to_string());
        self.matches = None;
    }

    /// Type a character. Editing clears the last complaint, which was about
    /// different text.
    pub fn insert(&mut self, c: char) -> bool {
        self.error = None;
        self.line.insert(c)
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) -> bool {
        if !self.line.backspace() {
            return false;
        }
        self.error = None;
        true
    }

    /// Throw the whole pattern away, leaving the line open.
    pub fn clear(&mut self) -> bool {
        let had = self.line.clear() || self.error.is_some();
        self.error = None;
        had
    }

    /// Cursor one character left.
    pub fn left(&mut self) -> bool {
        self.line.left()
    }

    /// Cursor one character right.
    pub fn right(&mut self) -> bool {
        self.line.right()
    }

    /// The query this line holds.
    ///
    /// # Errors
    ///
    /// Whatever [`mpdfm_core::query::parse`] says, which the caller puts back on
    /// the line with [`Prompt::fail`]. An empty line is an empty query and not an
    /// error — see core's `parse`.
    pub fn query(&self) -> Result<Query, QueryError> {
        mpdfm_core::query::parse(self.text())
    }

    /// What goes after the pattern on the bottom line: the match count, or
    /// nothing when nobody has counted.
    ///
    /// `no matches` is a state and has to be visible — a `/` that moved no
    /// cursor and said nothing is indistinguishable from a key that did not
    /// arrive.
    #[must_use]
    pub fn note_text(&self) -> String {
        match self.matches() {
            None => String::new(),
            Some(0) => " \u{b7} no matches".to_owned(),
            Some(1) => " \u{b7} 1 match".to_owned(),
            Some(n) => format!(" \u{b7} {n} matches"),
        }
    }
}

// ---------------------------------------------------------------------------
// The library-wide walk
// ---------------------------------------------------------------------------

/// A library-wide search on a worker, and how far it has got.
///
/// Held by the shell for the same reason a commit is: the walk reads up to 2 800
/// files, so the thing that must not block is the thread that draws
/// (`docs/tasks/25-search-and-filter.md`'s acceptance criterion). What is here
/// is what the message line needs and the one bit the `esc` key sets.
#[derive(Debug)]
pub struct Finding {
    /// What is being looked for, as the user typed it.
    query: String,
    /// The last thing the worker said. `None` until the first report.
    progress: Option<FindProgress>,
    /// Set from the UI thread to call the walk off.
    ///
    /// Shared rather than sent, exactly as a commit's is: a message would have
    /// to be received by a thread that is in the middle of a synchronous walk.
    cancel: Arc<AtomicBool>,
    /// Whether the user has asked for that and the worker has not answered yet.
    cancelling: bool,
}

impl Finding {
    /// A walk that has just been started.
    #[must_use]
    pub fn new(query: String, cancel: Arc<AtomicBool>) -> Self {
        Self {
            query,
            progress: None,
            cancel,
            cancelling: false,
        }
    }

    /// What is being looked for.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Record what the worker last said. Returns whether that is news.
    pub fn advanced(&mut self, progress: FindProgress) -> bool {
        let changed = self.progress != Some(progress);
        self.progress = Some(progress);
        changed
    }

    /// Ask the walk to stop. Returns whether this is the first such ask.
    pub fn cancel(&mut self) -> bool {
        if self.cancelling {
            return false;
        }
        self.cancelling = true;
        self.cancel.store(true, Ordering::Relaxed);
        true
    }

    /// Whether a stop has been asked for.
    #[must_use]
    pub fn is_cancelling(&self) -> bool {
        self.cancelling
    }

    /// The line the message bar shows while the walk is running.
    ///
    /// A percentage is honest here, unlike a scan's: the library is already in
    /// memory, so the denominator is known before the first file is opened.
    #[must_use]
    pub fn line(&self) -> String {
        if self.cancelling {
            return format!("find `{}`: stopping…", self.query);
        }
        match self.progress {
            None => format!("find `{}`: starting…", self.query()),
            Some(progress) => format!(
                "find `{}`: {}% \u{b7} {} of {} files \u{b7} {} read \u{b7} {} hit(s)",
                self.query(),
                progress.percent(),
                progress.scanned,
                progress.total,
                progress.read,
                progress.hits,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_keys_are_three_prefixes() {
        assert_eq!(Kind::Search.prefix(), "/");
        assert_eq!(Kind::Filter.prefix(), "f ");
        assert_eq!(Kind::Find.prefix(), "F ");
        for kind in [Kind::Search, Kind::Filter, Kind::Find] {
            assert!(!kind.label().is_empty());
            assert_eq!(kind.to_string(), kind.label());
        }
    }

    #[test]
    fn the_line_edits_by_characters_and_not_by_bytes() {
        let mut prompt = Prompt::new(Kind::Search, 7, None);
        for c in "So Hï".chars() {
            prompt.insert(c);
        }
        assert_eq!(prompt.text(), "So Hï");
        // `ï` is two bytes; a byte-wise cursor would split it and panic.
        assert!(prompt.left());
        assert_eq!(prompt.cursor(), "So H".len());
        assert!(prompt.backspace());
        assert_eq!(prompt.text(), "So ï");
        assert!(prompt.right());
        assert!(!prompt.right());
    }

    #[test]
    fn it_remembers_what_esc_has_to_put_back() {
        let was = mpdfm_core::query::parse("ext:flac").expect("parses");
        let prompt = Prompt::new(Kind::Filter, 12, Some(was.clone()));
        assert_eq!(prompt.origin(), 12);
        assert_eq!(prompt.restore(), Some(&was));

        // "there was no filter" is a value and not an absence of one.
        let fresh = Prompt::new(Kind::Filter, 0, None);
        assert_eq!(fresh.restore(), None);
    }

    #[test]
    fn a_complaint_lasts_until_the_text_changes() {
        let mut prompt = Prompt::new(Kind::Search, 0, None);
        for c in "artist:".chars() {
            prompt.insert(c);
        }
        let err = prompt.query().expect_err("a key with no value");
        prompt.fail(err);
        assert!(prompt.error().is_some());
        assert_eq!(prompt.matches(), None, "a failed parse counted nothing");

        prompt.insert('d');
        assert_eq!(prompt.error(), None);
        assert!(prompt.query().is_ok());
    }

    #[test]
    fn an_empty_line_is_an_empty_query_and_not_a_failure() {
        let prompt = Prompt::new(Kind::Filter, 0, None);
        let query = prompt.query().expect("nothing typed is not an error");
        assert!(query.is_empty());
    }

    #[test]
    fn no_matches_says_so_and_nobody_counted_says_nothing() {
        let mut prompt = Prompt::new(Kind::Search, 0, None);
        assert_eq!(prompt.note_text(), "", "nothing has been counted yet");

        prompt.note(Some(0));
        assert_eq!(prompt.note_text(), " \u{b7} no matches");
        prompt.note(Some(1));
        assert_eq!(prompt.note_text(), " \u{b7} 1 match");
        prompt.note(Some(9));
        assert_eq!(prompt.note_text(), " \u{b7} 9 matches");
    }

    #[test]
    fn clearing_reports_whether_there_was_anything_to_clear() {
        let mut prompt = Prompt::new(Kind::Search, 0, None);
        assert!(!prompt.clear(), "an empty line is already clear");
        prompt.insert('x');
        assert!(prompt.clear());
        assert_eq!(prompt.text(), "");
        assert!(!prompt.backspace());
    }

    #[test]
    fn a_walk_reports_and_can_be_called_off_once() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut finding = Finding::new("missing:genre".to_owned(), Arc::clone(&cancel));
        assert_eq!(finding.query(), "missing:genre");
        assert!(finding.line().contains("starting"));

        let progress = FindProgress {
            scanned: 1_400,
            total: 2_800,
            hits: 12,
            read: 1_400,
        };
        assert!(finding.advanced(progress));
        assert!(!finding.advanced(progress), "the same news is not news");
        let line = finding.line();
        assert!(line.contains("50%"), "{line}");
        assert!(line.contains("2800"), "{line}");
        assert!(line.contains("12 hit(s)"), "{line}");

        assert!(finding.cancel(), "the first ask is the one that counts");
        assert!(cancel.load(Ordering::Relaxed), "the worker can see it");
        assert!(!finding.cancel(), "asking twice is not a second event");
        assert!(finding.is_cancelling());
        assert!(finding.line().contains("stopping"));
    }
}
