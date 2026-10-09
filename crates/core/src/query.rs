//! The one query language `/`, `f`, `F`, `:find` and a future `mpdfm find` all
//! speak.
//!
//! ```
//! use mpdfm_core::query::{self, Subject};
//! use mpdfm_core::tags::{TagSet, Values};
//!
//! # fn main() -> Result<(), query::QueryError> {
//! let query = query::parse("artist:doom genre:\"hip hop\"")?;
//!
//! let tags = TagSet {
//!     artist: Values::one("MF DOOM"),
//!     genre: Values::one("Hip Hop"),
//!     ..TagSet::default()
//! };
//! assert!(query.matches(&Subject::row("01 Beef Rap.mp3").with_tags(&tags)));
//!
//! // Smart case, like vim: a lowercase pattern ignores case and one with an
//! // uppercase letter in it does not.
//! assert!(query::parse("doom")?.matches(&Subject::row("MF DOOM - Beef Rap.mp3")));
//! assert!(!query::parse("DOOM")?.matches(&Subject::row("mf doom - beef rap.mp3")));
//! # Ok(())
//! # }
//! ```
//!
//! # Why this is in core
//!
//! Because four front-ends have to agree about it. `/` in the browser, `f`'s
//! filter, `F`'s library-wide walk and `:find` are the same grammar today, and
//! `mpdfm find` will be the same grammar tomorrow (`docs/tasks/25-search-and-filter.md`
//! names this as the pitfall to avoid). A parser in the TUI would be a second
//! grammar the moment the CLI wanted one.
//!
//! # The grammar, in full
//!
//! Whitespace-separated terms, **all of which must match** — there is no `or`
//! and no negation, because the thing this exists for is narrowing. A term is
//! either bare text or `key:value`:
//!
//! | term | matches |
//! | --- | --- |
//! | `doom` | the name, the path, or `title` / `artist` / `album` / `genre` |
//! | `artist:doom` | that one field, and only it |
//! | `name:beef` | the file name alone |
//! | `path:hiphop` | the whole relative path |
//! | `ext:flac` | the extension, whole and case-insensitively |
//! | `missing:genre` | a file whose `genre` is absent or empty |
//!
//! `key` for a field is anything [`Field::parse`] takes, so `album-artist:`,
//! `albumartist:` and `date:` all work. Values quote with `"` when they hold a
//! space: `album:"mm..food"`, `artist:"Count Bass D"`.
//!
//! A term whose prefix is not a key MPDFM knows is **bare text, colon and all**
//! — `AC:DC` searches for `AC:DC` rather than failing — because a file name is
//! allowed to contain a colon and a search box that refused one would be wrong
//! about the library.
//!
//! # Tags, and the honest answer when there are none
//!
//! A [`Subject`] carries tags only when somebody has read them. Without them a
//! tag term **does not match**: `missing:genre` on a file nobody has opened is
//! not "yes" and not "no", and of the two lies the quieter one is to leave the
//! file out of a result set the user is about to act on.
//!
//! That is why there are two questions a caller can ask:
//!
//! - [`Query::needs_tags`] — would reading tags change any answer? A browser
//!   whose filter needs tags asks for the whole directory's rather than the
//!   screenful it would otherwise read (`src/tui/views/browser.rs`);
//! - [`Query::may_match`] — could this file match *whatever* its tags say? A
//!   `false` is a file [`find`] never opens, which is what makes `ext:flac` over
//!   2 800 files cost no tag reads at all.

use unicode_normalization::UnicodeNormalization as _;

use crate::library::Library;
use crate::paths::RelPath;
use crate::tags::{self, AudioInfo, Field, TagSet};

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

/// A piece of text to look for, and the case rule it was read under.
///
/// Substring, never anchored and never a glob: `food` finds `Mm..Food` and
/// nobody has to think about what `.` means. The task leaves a fuzzy matcher
/// optional and this is the fallback it asks to keep.
///
/// # Normalization, and why it is here
///
/// `ï` has two spellings in Unicode — one code point, or an `i` followed by a
/// combining diaeresis — and a substring match over bytes cannot see through
/// the difference. That is not a theoretical problem: the real library this was
/// written for holds `KREAM - So Hï [c0D2h71bFFI].mp3` in the *decomposed*
/// spelling, and a keyboard produces the composed one, so a search for `So Hï`
/// would find nothing at all.
///
/// So a **non-ASCII** pattern, and the text it is matched against, are both
/// folded to NFC before comparing. An ASCII-only pattern skips every bit of
/// that, which is what keeps the common case — a filter re-evaluated for every
/// row of every frame — a plain `contains`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// What the user typed.
    needle: String,
    /// The needle, prepared for comparison: lowercased when the pattern is
    /// case-insensitive, and NFC when it is not ASCII.
    ///
    /// Held rather than derived per call: a filter is re-evaluated for every row
    /// of every frame.
    prepared: String,
    /// Whether case is ignored, which is what smart case decided.
    fold: bool,
    /// Whether either side has to be normalized, which only a non-ASCII pattern
    /// needs.
    normalize: bool,
}

impl Pattern {
    /// Read a pattern under **smart case**: case-insensitive unless the pattern
    /// contains an uppercase letter, which is vim's rule and the one the task
    /// names.
    ///
    /// Uppercase is [`char::is_uppercase`] and not an ASCII test, so `Ä` makes a
    /// pattern case-sensitive exactly as `A` does.
    ///
    /// ```
    /// use mpdfm_core::query::Pattern;
    ///
    /// assert!(Pattern::new("doom").matches("MF DOOM"));
    /// assert!(!Pattern::new("DOOM").matches("mf doom"));
    /// assert!(Pattern::new("DOOM").matches("MF DOOM"));
    /// ```
    #[must_use]
    pub fn new(needle: &str) -> Self {
        let fold = !needle.chars().any(char::is_uppercase);
        let normalize = !needle.is_ascii();
        let prepared = prepare(needle, fold, normalize);
        Self {
            needle: needle.to_owned(),
            prepared,
            fold,
            normalize,
        }
    }

    /// What the user typed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.needle
    }

    /// Whether case matters, which is what smart case decided.
    #[must_use]
    pub fn is_case_sensitive(&self) -> bool {
        !self.fold
    }

    /// Whether `haystack` contains this pattern.
    ///
    /// Lowercasing is Unicode's and not ASCII's, so a `So Hï` typed in lower
    /// case finds `So HÏ` — which matters here, because 1 023 files in the
    /// library this was written for are not ASCII. See the type's documentation
    /// for the normalization half of the same problem.
    #[must_use]
    pub fn matches(&self, haystack: &str) -> bool {
        if !self.fold && !self.normalize {
            // The hot path: an ASCII pattern with a capital in it is a plain
            // substring search, with nothing allocated.
            return haystack.contains(&self.needle);
        }
        prepare(haystack, self.fold, self.normalize).contains(&self.prepared)
    }
}

/// Put text in the form [`Pattern::matches`] compares in.
///
/// Lowercase first and normalize second: `to_lowercase` can change which
/// composition a string is in, and the point of this is that both sides come
/// out the same.
fn prepare(text: &str, fold: bool, normalize: bool) -> String {
    let folded = if fold {
        text.to_lowercase()
    } else {
        text.to_owned()
    };
    if normalize {
        folded.nfc().collect()
    } else {
        folded
    }
}

// ---------------------------------------------------------------------------
// Terms
// ---------------------------------------------------------------------------

/// One thing a query asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// Bare text: the name, the path, or any of the four searchable tag fields.
    Text(Pattern),
    /// `name:beef` — the file name, without its directory.
    Name(Pattern),
    /// `path:hiphop/doom` — the whole path, relative to the music directory.
    Path(Pattern),
    /// `artist:doom` — one modelled field.
    Field(Field, Pattern),
    /// `ext:flac` — the extension, compared whole and case-insensitively.
    ///
    /// Held lowercased, since there is no case in which `ext:FLAC` means
    /// something different from `ext:flac`.
    Ext(String),
    /// `missing:genre` — a file that has nothing in that field.
    Missing(Field),
}

/// The tag fields bare text searches, in the order they are tried.
///
/// The four the task names. `albumartist`, `comment` and `composer` are
/// reachable with `albumartist:` and friends but are not swept by a bare word:
/// a bare word is what somebody types when they are looking for a track, and a
/// comment field full of `vtwin88cube` would make it match things nobody meant.
const SEARCHED: [Field; 4] = [Field::Title, Field::Artist, Field::Album, Field::Genre];

impl Term {
    /// Whether this term can be answered without opening the file.
    ///
    /// The three that can are the ones [`Query::may_match`] rejects on, and
    /// therefore the reason a search can be cheap.
    #[must_use]
    pub fn is_tag_free(&self) -> bool {
        matches!(self, Self::Name(_) | Self::Path(_) | Self::Ext(_))
    }

    /// Whether this term matches `subject`.
    #[must_use]
    pub fn matches(&self, subject: &Subject<'_>) -> bool {
        match self {
            Self::Text(pattern) => {
                pattern.matches(subject.name)
                    || subject.path.is_some_and(|path| pattern.matches(path))
                    || SEARCHED
                        .into_iter()
                        .any(|field| subject.field_matches(field, pattern))
            }
            Self::Name(pattern) => pattern.matches(subject.name),
            Self::Path(pattern) => pattern.matches(subject.path.unwrap_or(subject.name)),
            Self::Field(field, pattern) => subject.field_matches(*field, pattern),
            Self::Ext(ext) => subject
                .extension()
                .is_some_and(|found| found.to_lowercase() == *ext),
            // No tags is not an empty field: see the module documentation.
            Self::Missing(field) => subject.tags.is_some_and(|tags| tags.get(*field).is_empty()),
        }
    }
}

// ---------------------------------------------------------------------------
// Subjects
// ---------------------------------------------------------------------------

/// The thing a query is asked about: a name, maybe a path, maybe tags.
///
/// Borrowed throughout, because one of these is made per row per frame.
///
/// The path is optional and the distinction is deliberate. A listing row is
/// matched on its **name** — a filter typed inside `hiphop/MF DOOM` that matched
/// the directory it is in would narrow a directory to all of itself — while a
/// library-wide hit is matched on its whole path, because `doom` is where an
/// untagged library keeps its artist. [`Subject::row`] is the first,
/// [`Subject::file`] the second.
#[derive(Debug, Clone, Copy)]
pub struct Subject<'a> {
    name: &'a str,
    path: Option<&'a str>,
    tags: Option<&'a TagSet>,
}

impl<'a> Subject<'a> {
    /// A row of a listing, matched on its name alone.
    #[must_use]
    pub fn row(name: &'a str) -> Self {
        Self {
            name,
            path: None,
            tags: None,
        }
    }

    /// A file in the library, matched on its name and on its whole path.
    #[must_use]
    pub fn file(rel: &'a RelPath) -> Self {
        Self {
            name: rel.file_name(),
            path: Some(rel.as_str()),
            tags: None,
        }
    }

    /// The same subject, with the tags somebody has read for it.
    #[must_use]
    pub fn with_tags(mut self, tags: &'a TagSet) -> Self {
        self.tags = Some(tags);
        self
    }

    /// The name being matched.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// The extension of the name, without the dot.
    fn extension(&self) -> Option<&str> {
        let (_, ext) = self.name.rsplit_once('.')?;
        (!ext.is_empty()).then_some(ext)
    }

    /// Whether any of one field's values matches, with no tags meaning no.
    fn field_matches(&self, field: Field, pattern: &Pattern) -> bool {
        let Some(tags) = self.tags else {
            return false;
        };
        tags.get(field)
            .all()
            .iter()
            .any(|value| pattern.matches(value))
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// A parsed query: every term must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    terms: Vec<Term>,
    /// What the user typed, for the status bar and the log.
    raw: String,
}

impl Query {
    /// What the user typed.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The terms, in the order they were typed.
    #[must_use]
    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    /// Whether this query asks for nothing, and so matches everything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether reading a file's tags could change the answer.
    ///
    /// `true` for anything but a query made only of `name:`, `path:` and `ext:`
    /// terms — a bare word included, since a bare word sweeps the tag fields as
    /// well as the name.
    #[must_use]
    pub fn needs_tags(&self) -> bool {
        self.terms.iter().any(|term| !term.is_tag_free())
    }

    /// Whether every term matches.
    #[must_use]
    pub fn matches(&self, subject: &Subject<'_>) -> bool {
        self.terms.iter().all(|term| term.matches(subject))
    }

    /// Whether this subject could match once its tags are known.
    ///
    /// Only the terms that need no tags are consulted, so a `false` is final:
    /// nothing a file's tags might say can make it a hit. This is what lets a
    /// search skip opening it ([`find`]), and what lets a filter keep a row
    /// whose tags have not arrived instead of hiding it and then never asking.
    #[must_use]
    pub fn may_match(&self, subject: &Subject<'_>) -> bool {
        self.terms
            .iter()
            .filter(|term| term.is_tag_free())
            .all(|term| term.matches(subject))
    }
}

impl std::fmt::Display for Query {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Why a query could not be read.
///
/// Written to be shown under the line being typed, the way a bad `:` command is
/// (`src/tui/command.rs`): short, and naming the fix. An incremental search
/// passes through several of these on the way to a valid query — `artist:` is
/// one keystroke from `artist:d` — so none of them is an event, and the UI
/// shows the reason and matches nothing until the next keystroke.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    /// `artist:` with nothing after it.
    #[error("{key}: needs something to look for, as in `{key}:doom`")]
    MissingValue {
        /// The key that was given no value.
        key: String,
    },

    /// `missing:bpm` — a field MPDFM does not model.
    #[error("there is no `{name}` field; try {}", field_names())]
    UnknownField {
        /// What was typed.
        name: String,
    },

    /// A `"` that is never closed.
    #[error("a quote is left open")]
    UnterminatedQuote,
}

/// Every field name, for the message that lists them.
fn field_names() -> String {
    tags::FIELDS
        .iter()
        .map(|field| field.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read a query.
///
/// An empty line — or one of nothing but spaces — is an empty [`Query`], which
/// matches everything. That is the right answer for the UI: backspacing the last
/// character of a filter puts the whole listing back rather than raising an
/// error about it.
///
/// # Errors
///
/// A [`QueryError`] for a key with no value, a field that does not exist, or an
/// unclosed quote. Every other shape parses — see the module documentation on
/// why `AC:DC` is text and not a failure.
pub fn parse(text: &str) -> Result<Query, QueryError> {
    let mut terms = Vec::new();
    for token in tokenize(text)? {
        terms.push(term(&token)?);
    }
    Ok(Query {
        terms,
        raw: text.trim().to_owned(),
    })
}

/// One token, already unquoted, as a term.
fn term(token: &Token) -> Result<Term, QueryError> {
    // A token that was quoted is text, whatever is inside it: `":"` is a colon
    // to look for, and quoting is how a user says so.
    let Some((key, value)) = token.split_key() else {
        return Ok(Term::Text(Pattern::new(&token.text)));
    };

    let missing_value = || QueryError::MissingValue {
        key: key.to_lowercase(),
    };
    match key.to_lowercase().as_str() {
        "name" | "file" => {
            if value.is_empty() {
                return Err(missing_value());
            }
            Ok(Term::Name(Pattern::new(value)))
        }
        "path" | "dir" => {
            if value.is_empty() {
                return Err(missing_value());
            }
            Ok(Term::Path(Pattern::new(value)))
        }
        "ext" => {
            if value.is_empty() {
                return Err(missing_value());
            }
            // A typed `.flac` means the same thing as `flac`; nobody should have
            // to remember which.
            let ext = value.trim_start_matches('.').to_lowercase();
            Ok(Term::Ext(ext))
        }
        "missing" | "no" => {
            if value.is_empty() {
                return Err(missing_value());
            }
            Field::parse(value)
                .map(Term::Missing)
                .ok_or_else(|| QueryError::UnknownField {
                    name: value.to_owned(),
                })
        }
        other => match Field::parse(other) {
            Some(field) if value.is_empty() => Err(QueryError::MissingValue {
                key: field.as_str().to_owned(),
            }),
            Some(field) => Ok(Term::Field(field, Pattern::new(value))),
            // Not a key at all: the whole token is text, colon included.
            None => Ok(Term::Text(Pattern::new(&token.text))),
        },
    }
}

/// One whitespace-separated word, with its quotes taken off.
struct Token {
    /// The text, unquoted.
    text: String,
    /// Where the first `:` outside of quotes was, as a byte offset into `text`.
    ///
    /// Recorded by the tokenizer rather than searched for afterwards, which is
    /// what makes `album:"a:b"` one field term whose value holds a colon, and
    /// `"a:b"` a piece of text that holds one.
    colon: Option<usize>,
}

impl Token {
    /// The key and value, for a token that has a bare colon in it.
    fn split_key(&self) -> Option<(&str, &str)> {
        let at = self.colon?;
        Some((&self.text[..at], &self.text[at + 1..]))
    }
}

/// Split a line into tokens, honouring double quotes.
///
/// Quotes group and then vanish: `album:"mm..food"` is one token whose text is
/// `album:mm..food`. A quote inside a word is allowed to start a group —
/// `album:"a b"` is the shape the task asks for — and a `"` on its own closes
/// nothing, which is the error.
fn tokenize(text: &str) -> Result<Vec<Token>, QueryError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut colon = None;
    let mut quoted = false;
    let mut started = false;

    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    tokens.push(Token {
                        text: std::mem::take(&mut current),
                        colon: colon.take(),
                    });
                    started = false;
                }
            }
            c => {
                if c == ':' && !quoted && colon.is_none() {
                    colon = Some(current.len());
                }
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err(QueryError::UnterminatedQuote);
    }
    if started {
        tokens.push(Token {
            text: current,
            colon,
        });
    }
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// The library-wide walk
// ---------------------------------------------------------------------------

/// How often [`find`] reports, in entries.
///
/// Small enough that a progress line moves on a library of any size, large
/// enough that the channel send is not most of the work for a query that reads
/// no tags. 2 800 files is 44 reports.
const REPORT_EVERY: usize = 64;

/// How far a [`find`] has got.
///
/// A percentage is honest here, unlike a scan's — the library is already in
/// memory, so the denominator is known before the first file is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindProgress {
    /// Entries considered so far.
    pub scanned: usize,
    /// Entries there are, which is [`Library::len`].
    pub total: usize,
    /// Hits so far.
    pub hits: usize,
    /// Files opened so far, which is the part that costs anything.
    pub read: usize,
}

impl FindProgress {
    /// How far along, in whole percent.
    #[must_use]
    pub fn percent(&self) -> usize {
        self.scanned.saturating_mul(100) / self.total.max(1)
    }
}

/// Whether [`find`] should carry on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep going.
    Go,
    /// Stop where you are; what has been found so far is still returned.
    Stop,
}

/// One file a query matched.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Its index in [`Library::entries`], so a caller can put it in a listing
    /// without looking it up again.
    pub index: usize,
    /// Its path.
    pub rel: RelPath,
    /// What its tags said, when the query made it necessary to open it.
    ///
    /// `None` and `info` `None` together: either the file was opened or it was
    /// not. A query of nothing but `ext:` and `name:` terms opens nothing, which
    /// is the point of [`Query::may_match`].
    pub tags: Option<TagSet>,
    /// What the audio is, from the same open as `tags`.
    ///
    /// Read alongside the tags rather than skipped, so that the result listing
    /// can show a duration and a bitrate and the rows the user is about to act
    /// on are never read twice (`docs/tasks/25-search-and-filter.md` asks for the
    /// results to be cached for the session).
    pub info: Option<AudioInfo>,
}

/// Everything a [`find`] produced.
#[derive(Debug, Clone, Default)]
pub struct Found {
    /// The matches, in [`Library::entries`] order — which is path order.
    pub hits: Vec<Hit>,
    /// Files whose tags could not be read, and why.
    ///
    /// Reported rather than silently skipped: a search that could not open nine
    /// files found a result set that may be missing nine, and the user is the
    /// only one who can decide whether that matters.
    pub failed: Vec<(RelPath, String)>,
    /// How many entries were considered.
    pub scanned: usize,
    /// How many files were opened.
    pub read: usize,
    /// Whether the caller stopped it early.
    pub cancelled: bool,
}

/// Walk the whole library and collect what a query matches, reading tags only
/// where the query makes it necessary.
///
/// `report` is called on this thread every 64 entries and once at the end, and
/// returns [`Flow::Stop`] to call the search off — which is how the
/// TUI's `esc` reaches a walk that is already running. Whatever was found before
/// the stop comes back, with [`Found::cancelled`] set.
///
/// # Cost
///
/// One `stat`-free pass over an in-memory model, plus one open per file the
/// query cannot decide without. `ext:flac` over 2 800 files opens none;
/// `missing:genre` opens every audio file, which is the pitfall the task names
/// and the reason this is a worker and not a keypress.
pub fn find(
    query: &Query,
    library: &Library,
    report: &mut dyn FnMut(&FindProgress) -> Flow,
) -> Found {
    let root = library.root();
    let total = library.len();
    let mut found = Found::default();

    for (index, entry) in library.entries().iter().enumerate() {
        if index % REPORT_EVERY == 0 {
            let progress = FindProgress {
                scanned: index,
                total,
                hits: found.hits.len(),
                read: found.read,
            };
            if report(&progress) == Flow::Stop {
                found.cancelled = true;
                break;
            }
        }
        found.scanned = index + 1;

        let subject = Subject::file(&entry.rel);
        // Already decided, with nothing opened: every term matched on the name
        // and the path alone. `missing:` cannot pass this way, since it answers
        // `false` without tags.
        if query.matches(&subject) {
            found.hits.push(Hit {
                index,
                rel: entry.rel.clone(),
                tags: None,
                info: None,
            });
            continue;
        }
        // Rejected, with nothing opened.
        if !query.may_match(&subject) {
            continue;
        }
        // Undecided — but only an audio file has tags to decide it.
        if !entry.is_audio() {
            continue;
        }

        found.read += 1;
        match tags::read(&entry.rel.to_abs(root)) {
            Ok((tags, info)) => {
                if query.matches(&subject.with_tags(&tags)) {
                    found.hits.push(Hit {
                        index,
                        rel: entry.rel.clone(),
                        tags: Some(tags),
                        info: Some(info),
                    });
                }
            }
            Err(err) => found.failed.push((entry.rel.clone(), err.to_string())),
        }
    }

    let progress = FindProgress {
        scanned: found.scanned,
        total,
        hits: found.hits.len(),
        read: found.read,
    };
    report(&progress);
    found
}

/// [`find`] with no progress and no way to stop it, for a caller that is already
/// on a thread of its own and has nothing to draw.
///
/// What a future `mpdfm find` would call.
#[must_use]
pub fn find_all(query: &Query, library: &Library) -> Found {
    find(query, library, &mut |_| Flow::Go)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags::Values;

    fn tagged(artist: &str, album: &str, genre: &str) -> TagSet {
        TagSet {
            artist: Values::one(artist),
            album: Values::one(album),
            genre: Values::one(genre),
            ..TagSet::default()
        }
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let query = parse("   ").expect("whitespace is not an error");
        assert!(query.is_empty());
        assert!(query.matches(&Subject::row("anything at all")));
        assert!(!query.needs_tags(), "there is nothing to read tags for");
    }

    #[test]
    fn bare_text_is_a_substring_of_the_name() {
        let query = parse("beef").expect("a word parses");
        assert!(query.matches(&Subject::row("01 Beef Rap.mp3")));
        assert!(!query.matches(&Subject::row("02 Hoe Cakes.mp3")));
    }

    #[test]
    fn smart_case_is_vims_rule() {
        assert!(parse("doom").unwrap().matches(&Subject::row("MF DOOM")));
        assert!(!parse("DOOM").unwrap().matches(&Subject::row("mf doom")));
        assert!(parse("DOOM").unwrap().matches(&Subject::row("MF DOOM")));

        // The rule is about the *pattern*, per term, so one query can hold both.
        let query = parse("doom Beef").expect("two terms");
        assert!(query.matches(&Subject::row("MF DOOM - Beef Rap")));
        assert!(!query.matches(&Subject::row("MF DOOM - beef rap")));
    }

    #[test]
    fn a_non_ascii_pattern_matches_a_non_ascii_name_in_either_case() {
        // 1 023 files in the real library are like this, which is why the
        // lowercasing is Unicode's and not ASCII's.
        assert!(
            parse("so hï")
                .unwrap()
                .matches(&Subject::row("01 So Hï.mp3"))
        );
        assert!(parse("hï").unwrap().matches(&Subject::row("So HÏ")));
        assert!(
            parse("ノスタルジア")
                .unwrap()
                .matches(&Subject::row("03 ノスタルジア.mp3"))
        );

        // The real library holds this name in the *decomposed* spelling — an
        // `i` followed by a combining diaeresis — and a keyboard produces the
        // composed one. Both have to match, in either direction.
        let decomposed = "KREAM - So Hi\u{308} [c0D2h71bFFI].mp3";
        assert_ne!(decomposed, "KREAM - So Hï [c0D2h71bFFI].mp3");
        assert!(parse("So Hï").unwrap().matches(&Subject::row(decomposed)));
        assert!(
            parse("so hï").unwrap().matches(&Subject::row(decomposed)),
            "and case-insensitively, which lowercases before it normalizes"
        );
        assert!(
            parse("So Hi\u{308}")
                .unwrap()
                .matches(&Subject::row("KREAM - So Hï [c0D2h71bFFI].mp3")),
            "the other direction too"
        );
        // An uppercase non-ASCII letter makes the pattern case-sensitive, the
        // same way `A` does.
        assert!(Pattern::new("Tänd").is_case_sensitive());
        assert!(
            parse("tänd")
                .unwrap()
                .matches(&Subject::row("02 TÄND Ljusen.mp3"))
        );
    }

    #[test]
    fn every_term_has_to_match() {
        let query = parse("beef rap").expect("two words");
        assert!(query.matches(&Subject::row("01 Beef Rap.mp3")));
        assert!(!query.matches(&Subject::row("01 Beef Stew.mp3")));
    }

    #[test]
    fn a_field_term_looks_only_at_that_field() {
        let tags = tagged("MF DOOM", "Mm..Food", "Hip Hop");
        let subject = Subject::row("01 Beef Rap.mp3").with_tags(&tags);

        assert!(parse("artist:doom").unwrap().matches(&subject));
        assert!(!parse("genre:jazz").unwrap().matches(&subject));
        assert!(
            !parse("album:doom").unwrap().matches(&subject),
            "`doom` is the artist, not the album"
        );
        // Every spelling `Field::parse` takes is a key here.
        assert!(parse("album-artist:x").unwrap().terms().len() == 1);
    }

    #[test]
    fn a_quoted_value_may_hold_spaces() {
        let tags = tagged("MF DOOM", "Mm..Food", "Hip Hop");
        let subject = Subject::row("01 Beef Rap.mp3").with_tags(&tags);

        assert!(parse("album:\"mm..food\"").unwrap().matches(&subject));
        assert!(parse("genre:\"hip hop\"").unwrap().matches(&subject));

        // Unquoted, the space splits it into a field term and a bare word —
        // which is a different question, and the reason quoting exists.
        let split = parse("genre:hip hop").expect("two terms");
        assert_eq!(split.terms().len(), 2);
        assert_eq!(
            split.terms()[1],
            Term::Text(Pattern::new("hop")),
            "the second word is text, not part of the genre"
        );
        assert!(
            !parse("genre:hip beats").unwrap().matches(&subject),
            "a bare word that matches nothing fails the whole query"
        );
    }

    #[test]
    fn ext_compares_the_whole_extension_and_ignores_case() {
        let query = parse("ext:flac").expect("an extension");
        assert!(query.matches(&Subject::row("01 So What.flac")));
        assert!(query.matches(&Subject::row("01 So What.FLAC")));
        assert!(
            !query.matches(&Subject::row("flac notes.txt")),
            "`ext` is not a substring of the name"
        );
        assert!(!query.matches(&Subject::row("no extension")));
        // A typed dot is allowed.
        assert!(parse("ext:.mp3").unwrap().matches(&Subject::row("a.mp3")));
        assert!(!query.needs_tags(), "an extension is in the name");
    }

    #[test]
    fn missing_finds_an_empty_field_and_says_nothing_without_tags() {
        let query = parse("missing:genre").expect("a field");
        assert!(query.needs_tags());

        let none = TagSet::default();
        assert!(query.matches(&Subject::row("x.mp3").with_tags(&none)));

        let some = tagged("MF DOOM", "Mm..Food", "Hip Hop");
        assert!(!query.matches(&Subject::row("x.mp3").with_tags(&some)));

        // No tags at all is not an empty field.
        assert!(!query.matches(&Subject::row("x.mp3")));
        // ...and it is not a rejection either, so a search still opens the file.
        assert!(query.may_match(&Subject::row("x.mp3")));
    }

    #[test]
    fn a_path_term_matches_the_path_and_a_row_matches_only_its_name() {
        let rel = RelPath::parse("hiphop/MF DOOM/01 Beef Rap.mp3").expect("a path");
        assert!(parse("path:hiphop").unwrap().matches(&Subject::file(&rel)));
        assert!(!parse("name:hiphop").unwrap().matches(&Subject::file(&rel)));

        // A bare word sweeps the path too, which is what makes `F doom` find an
        // untagged library's artists.
        assert!(parse("doom").unwrap().matches(&Subject::file(&rel)));
        // ...and a listing row has no path, so a filter inside that directory
        // does not match every row in it.
        assert!(
            !parse("doom")
                .unwrap()
                .matches(&Subject::row("01 Beef Rap.mp3"))
        );
    }

    #[test]
    fn a_colon_that_is_not_a_key_is_text() {
        // A file name is allowed to hold a colon.
        let query = parse("AC:DC").expect("not a key, so text");
        assert_eq!(query.terms(), &[Term::Text(Pattern::new("AC:DC"))]);
        assert!(query.matches(&Subject::row("AC:DC - Back in Black.mp3")));

        // And a quoted one is text even when it looks like a key.
        let quoted = parse("\"artist:doom\"").expect("quoted");
        assert_eq!(quoted.terms(), &[Term::Text(Pattern::new("artist:doom"))]);
    }

    #[test]
    fn a_query_that_cannot_be_read_says_what_is_wrong_with_it() {
        assert_eq!(
            parse("artist:"),
            Err(QueryError::MissingValue {
                key: "artist".to_owned()
            })
        );
        assert_eq!(
            parse("artist:").unwrap_err().to_string(),
            "artist: needs something to look for, as in `artist:doom`"
        );
        assert_eq!(
            parse("missing:bpm"),
            Err(QueryError::UnknownField {
                name: "bpm".to_owned()
            })
        );
        assert!(
            parse("missing:bpm")
                .unwrap_err()
                .to_string()
                .contains("genre"),
            "the message lists the fields there are"
        );
        assert_eq!(parse("album:\"mm"), Err(QueryError::UnterminatedQuote));
        for key in ["ext:", "missing:", "name:", "path:"] {
            assert!(parse(key).is_err(), "{key} needs a value");
        }
    }

    #[test]
    fn may_match_rejects_only_on_what_it_can_see() {
        let query = parse("ext:flac missing:genre").expect("both");
        // The extension decides it, whatever the tags say.
        assert!(!query.may_match(&Subject::row("01 Beef Rap.mp3")));
        assert!(query.may_match(&Subject::row("01 So What.flac")));
        assert!(query.needs_tags());
    }

    #[test]
    fn the_raw_text_survives_parsing_for_the_status_bar() {
        let query = parse("  artist:doom  ext:flac  ").expect("spacing");
        assert_eq!(query.raw(), "artist:doom  ext:flac");
        assert_eq!(query.to_string(), "artist:doom  ext:flac");
    }
}
