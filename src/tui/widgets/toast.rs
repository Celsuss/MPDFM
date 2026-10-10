//! The message line: what just happened, one message at a time, and the
//! history `:messages` reads back.
//!
//! # Two lists, because two questions
//!
//! - **the queue** answers "what is on the bottom line now". A message gets its
//!   turn and its own clock, and goes. A queue and not a slot: two things worth
//!   saying in the same second — a scan's counts and a warning about what it
//!   found — would otherwise mean the second silently overwriting the first;
//! - **the history** answers "what did it say a minute ago". Everything that
//!   was ever queued goes into it, *and every error*, which is never queued —
//!   an error opens a panel (`views::error`), and the panel is dismissed, and
//!   after that `:messages` is the only place it is still written down.
//!
//! Both are bounded. A flood is a bug somewhere, and dropping the oldest keeps
//! the news a user wants without growing a list forever.
//!
//! # Wrapped, never truncated
//!
//! A toast longer than the row wraps onto as many as [`MAX_ROWS`] rows, and the
//! shell gives the message line that many — so "committed 20260924T224500Z-a3f1
//! — 14 files, 2 playlists · u to undo" on a 60-column terminal is two rows and
//! not a txid with its end cut off. Past [`MAX_ROWS`] the last row says where the
//! rest is, which is `:messages`, which shows every message whole.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{fit, pad, wrap};

/// How many messages may be waiting for the bottom line.
///
/// Generous: the point of the cap is that nothing grows without bound, not that
/// anything is ever expected to reach it.
pub const QUEUE: usize = 16;

/// How many messages `:messages` remembers.
pub const KEPT: usize = 200;

/// The most rows one toast may take on the bottom line.
///
/// Three is a txid, a count and a hint on a 60-column terminal, with room to
/// spare; more than that and the message line would be eating the listing a
/// toast is usually about.
pub const MAX_ROWS: usize = 3;

/// How serious a message is, and therefore whether it expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something worked.
    Info,
    /// Something is worth a look.
    Warn,
    /// Something went wrong. Never queued — see the module documentation.
    Error,
}

impl Level {
    /// The word `:messages` prints in front of it.
    fn word(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// The colour it is drawn in.
    #[must_use]
    pub fn color(self) -> Color {
        match self {
            Self::Info => Color::Green,
            Self::Warn => Color::Yellow,
            Self::Error => Color::Red,
        }
    }
}

/// A transient message on the bottom line.
#[derive(Debug, Clone)]
pub struct Toast {
    /// What it says. Whole: the line wraps it rather than cutting it.
    pub text: String,
    /// How serious it is.
    pub level: Level,
    /// How long it gets once it is the one on screen.
    lifetime: Duration,
    /// When it will have had its turn. `None` until it reaches the front of the
    /// queue, because a message that waited behind two others has not been read
    /// yet and its clock should not have been running.
    expires: Option<Instant>,
}

impl Toast {
    /// When it will have had its turn, once it is on screen.
    #[cfg(test)]
    #[must_use]
    pub fn expires(&self) -> Option<Instant> {
        self.expires
    }

    /// The rows it takes at `cells` wide: wrapped, and at most [`MAX_ROWS`] of
    /// them, the last saying where the rest is if it did not all fit.
    #[must_use]
    pub fn rows(&self, cells: usize) -> Vec<String> {
        let mut rows = wrap(&self.text, cells);
        if rows.len() > MAX_ROWS {
            rows.truncate(MAX_ROWS);
            let more = " … :messages for the rest";
            let last = &mut rows[MAX_ROWS - 1];
            let keep = cells.saturating_sub(super::width(more));
            *last = format!("{}{more}", fit(last, keep).trim_end_matches('…'));
            *last = fit(last, cells);
        }
        rows
    }
}

/// One line of history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its position in the session, from 1: what `:messages` numbers it with,
    /// so "the third thing it said" is something a user can refer to.
    pub number: usize,
    /// How serious it was.
    pub level: Level,
    /// What it said, whole.
    pub text: String,
}

/// The queue and the history.
#[derive(Debug, Default)]
pub struct Toasts {
    /// Waiting for the bottom line, oldest first.
    queue: VecDeque<Toast>,
    /// Everything said this session, oldest first, up to [`KEPT`].
    history: VecDeque<Entry>,
    /// How many have ever been said, which numbers the next one.
    said: usize,
}

impl Toasts {
    /// Nothing said yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a message for the bottom line, and remember it.
    pub fn push(&mut self, level: Level, text: String, lifetime: Duration, now: Instant) {
        self.remember(level, text.clone());
        if self.queue.len() >= QUEUE {
            self.queue.pop_front();
        }
        self.queue.push_back(Toast {
            text,
            level,
            lifetime,
            expires: None,
        });
        self.start_clock(now);
    }

    /// Remember a message without queueing it: an error, which has a panel.
    pub fn remember(&mut self, level: Level, text: String) {
        self.said += 1;
        if self.history.len() >= KEPT {
            self.history.pop_front();
        }
        self.history.push_back(Entry {
            number: self.said,
            level,
            text,
        });
    }

    /// The message on screen.
    #[must_use]
    pub fn front(&self) -> Option<&Toast> {
        self.queue.front()
    }

    /// The last message queued.
    // The tests' view of the queue; the shell only ever needs the front.
    #[cfg(test)]
    #[must_use]
    pub fn back(&self) -> Option<&Toast> {
        self.queue.back()
    }

    /// How many are waiting, the one on screen included.
    // The tests' view of the queue; the shell only ever needs the front.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Everything waiting, the one on screen first.
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.queue.iter()
    }

    /// Whether the bottom line is free.
    // The tests' view of the queue; the shell only ever needs the front.
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Empty the queue. The history keeps them: clearing the line is not
    /// forgetting what it said.
    // The tests' view of the queue; the shell only ever needs the front.
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.queue.clear();
    }

    /// `esc`: the message on screen has been read. Returns whether there was one.
    pub fn dismiss(&mut self, now: Instant) -> bool {
        let dismissed = self.queue.pop_front().is_some();
        self.start_clock(now);
        dismissed
    }

    /// Drop the front message if it has had its turn. Returns whether the
    /// bottom line now says something different.
    pub fn retire(&mut self, now: Instant) -> bool {
        let done = self
            .queue
            .front()
            .and_then(|front| front.expires)
            .is_some_and(|expires| now >= expires);
        if !done {
            return false;
        }
        self.queue.pop_front();
        self.start_clock(now);
        true
    }

    /// Everything said this session that is still remembered, oldest first.
    /// `:messages` draws it with [`Toasts::history_lines`]; this is for tests.
    #[cfg(test)]
    pub fn history(&self) -> impl DoubleEndedIterator<Item = &Entry> + ExactSizeIterator {
        self.history.iter()
    }

    /// `:messages`, laid out `cells` wide: newest first, each one whole and
    /// wrapped under its number and level.
    #[must_use]
    pub fn history_lines(&self, cells: usize) -> Vec<Line<'static>> {
        if self.history.is_empty() {
            return vec![Line::styled(
                "nothing has been said yet",
                Style::new().fg(Color::DarkGray),
            )];
        }
        // `  12 warn  ` — the number right-aligned so the levels line up, and
        // the text hung under itself rather than under the number.
        let number_w = self.said.to_string().len();
        let gutter = number_w + 1 + 5 + 1;
        let text_w = cells.saturating_sub(gutter).max(1);
        let mut lines = Vec::new();
        for entry in self.history.iter().rev() {
            let style = Style::new().fg(entry.level.color());
            for (index, row) in wrap(&entry.text, text_w).into_iter().enumerate() {
                let head = if index == 0 {
                    vec![
                        Span::styled(
                            format!("{:>number_w$} ", entry.number),
                            Style::new().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            pad(entry.level.word(), 5),
                            style.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                    ]
                } else {
                    vec![Span::raw(" ".repeat(gutter))]
                };
                let mut spans = head;
                spans.push(Span::styled(row, style));
                lines.push(Line::from(spans));
            }
        }
        lines
    }

    /// Give whatever is at the front of the queue its clock, if it has not got
    /// one: the lifetime a toast gets is time spent *on screen*, not time spent
    /// waiting behind another one.
    fn start_clock(&mut self, now: Instant) {
        if let Some(front) = self.queue.front_mut()
            && front.expires.is_none()
        {
            front.expires = Some(now + front.lifetime);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::widgets::width;

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_long_toast_wraps_and_keeps_every_word() {
        let mut toasts = Toasts::new();
        let text = "committed 20260924T224500Z-a3f1 — 14 files, 2 playlists · u to undo";
        toasts.push(
            Level::Info,
            text.to_owned(),
            Duration::from_secs(4),
            Instant::now(),
        );
        let rows = toasts.front().expect("queued").rows(40);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows.join(" "), text, "nothing was cut");
        assert!(rows.iter().all(|row| width(row) <= 40));
    }

    #[test]
    fn a_toast_too_long_for_the_line_says_where_the_rest_is() {
        let mut toasts = Toasts::new();
        let text = "word ".repeat(100);
        toasts.push(
            Level::Warn,
            text.clone(),
            Duration::from_secs(4),
            Instant::now(),
        );
        let rows = toasts.front().expect("queued").rows(60);
        assert_eq!(rows.len(), MAX_ROWS);
        assert!(
            rows[MAX_ROWS - 1].ends_with(":messages for the rest"),
            "{rows:?}"
        );
        assert!(rows.iter().all(|row| width(row) <= 60));

        // And `:messages` does have the rest.
        let all: String = toasts.history_lines(60).iter().map(text_of).collect();
        assert_eq!(all.matches("word").count(), 100);
    }

    #[test]
    fn an_error_is_remembered_but_never_queued() {
        let mut toasts = Toasts::new();
        toasts.remember(Level::Error, "music_dir does not exist".to_owned());
        assert!(toasts.is_empty(), "an error has a panel, not a toast");
        assert_eq!(toasts.history().len(), 1);
        assert_eq!(
            toasts.history().next().map(|entry| entry.level),
            Some(Level::Error)
        );
    }

    #[test]
    fn clearing_the_line_does_not_forget_what_it_said() {
        let mut toasts = Toasts::new();
        let now = Instant::now();
        toasts.push(Level::Info, "one".to_owned(), Duration::ZERO, now);
        toasts.push(Level::Info, "two".to_owned(), Duration::ZERO, now);
        assert!(toasts.retire(now));
        toasts.clear();
        assert!(toasts.is_empty());

        let numbers: Vec<usize> = toasts.history().map(|entry| entry.number).collect();
        assert_eq!(numbers, vec![1, 2]);
        let lines: Vec<String> = toasts.history_lines(80).iter().map(text_of).collect();
        assert_eq!(lines, vec!["2 info  two", "1 info  one"], "newest first");
    }

    #[test]
    fn the_history_is_bounded_and_keeps_the_newest() {
        let mut toasts = Toasts::new();
        for n in 0..KEPT + 10 {
            toasts.remember(Level::Info, format!("message {n}"));
        }
        assert_eq!(toasts.history().len(), KEPT);
        assert_eq!(
            toasts.history().next().map(|entry| entry.number),
            Some(11),
            "the first ten went, and the numbers did not restart"
        );
    }

    #[test]
    fn a_wrapped_history_entry_hangs_under_its_text() {
        let mut toasts = Toasts::new();
        toasts.remember(
            Level::Warn,
            "a fairly long warning that has to wrap".to_owned(),
        );
        let lines: Vec<String> = toasts.history_lines(24).iter().map(text_of).collect();
        assert!(lines.len() > 1, "{lines:?}");
        assert!(lines[0].starts_with("1 warn  "), "{lines:?}");
        assert!(lines[1].starts_with("        "), "{lines:?}");
        assert!(lines.iter().all(|line| width(line) <= 24), "{lines:?}");
    }
}
