//! The slim line a scan, a commit or a library search shows while it runs.
//!
//! One shape for all three, because they are one thing to the user: a worker
//! is busy, here is how far it has got, and here is the key that stops it — or
//! no key, because it cannot be stopped.
//!
//! ```text
//! scanning ·  1 280 files · 112 dirs · hiphop/MF DOOM - Mm..Food · esc to stop
//! committing 3 operations ━━━━━━──── 61% · 11/18 files (61%) · esc to stop
//! ```
//!
//! # A bar only when the total is known
//!
//! A commit knows how many steps it has, and a library search how many files
//! it will look at, so both get a bar. A scan does not know how many files there
//! are until it has found them (`ScanProgress`'s own documentation), and a bar
//! that guesses is worse than a count that does not — so a scan gets the count.
//!
//! # Cancellable where the operation is
//!
//! The key is passed in by whoever knows whether the worker can still stop,
//! which is the worker's owner: a scan always can, a search always can, and a
//! commit only before its first change (`commit::Options::cancel`). This widget
//! draws what it is told and has no opinion.
//!
//! # Narrow terminals
//!
//! The detail goes first (from its end, since its start is the count), then
//! the bar. The label and the stop key are never cut: one says what is running
//! and the other is how to make it not be.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::{fit, width};

/// Cells the bar takes, when there is one.
const BAR: usize = 10;

/// What is running, and how far it has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// What is running: `scanning`, `committing 3 operations`.
    label: String,
    /// How far, out of how many, when that is known.
    fraction: Option<(usize, usize)>,
    /// Everything else worth saying: counts, the directory being read.
    detail: String,
    /// The key that stops it, when it can be stopped.
    stop: Option<String>,
}

impl Progress {
    /// A running thing with nothing yet known about it.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            fraction: None,
            detail: String::new(),
            stop: None,
        }
    }

    /// It is `done` of `total` of the way through. A total of zero draws no
    /// bar, since there is no fraction to draw.
    #[must_use]
    pub fn fraction(mut self, done: usize, total: usize) -> Self {
        self.fraction = (total > 0).then_some((done.min(total), total));
        self
    }

    /// Say something more.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// `key` stops it.
    #[must_use]
    pub fn stop(mut self, key: Option<String>) -> Self {
        self.stop = key;
        self
    }

    /// The line, at most `cells` wide.
    #[must_use]
    pub fn line(&self, cells: usize) -> Line<'static> {
        let accent = Style::new().fg(Color::Cyan);
        let dim = Style::new().fg(Color::DarkGray);

        let stop = self
            .stop
            .as_ref()
            .map(|key| format!(" \u{b7} {key} to stop"))
            .unwrap_or_default();
        let fixed = width(&self.label) + width(&stop);

        // The bar, only if it leaves the detail some room — a bar with nothing
        // after it is less use than the numbers it replaced.
        let bar = self.fraction.filter(|_| cells >= fixed + BAR + 6 + 20);
        let bar_w = if bar.is_some() { 1 + BAR + 5 } else { 0 };

        let mut spans = vec![Span::styled(fit(&self.label, cells), accent)];
        if let Some((done, total)) = bar {
            let filled = done * BAR / total;
            let percent = done * 100 / total;
            spans.push(Span::raw(" "));
            spans.push(Span::styled("━".repeat(filled), accent));
            spans.push(Span::styled("─".repeat(BAR - filled), dim));
            spans.push(Span::styled(format!(" {percent:>3}%"), accent));
        }
        if !self.detail.is_empty() {
            let room = cells.saturating_sub(fixed + bar_w + 3);
            if room >= 4 {
                spans.push(Span::styled(" \u{b7} ", dim));
                spans.push(Span::styled(fit(&self.detail, room), accent));
            }
        }
        if !stop.is_empty() && fixed <= cells {
            spans.push(Span::styled(stop, dim));
        }
        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_scan_shows_counts_and_the_key_that_stops_it() {
        let progress = Progress::new("scanning")
            .detail("1 280 files \u{b7} 112 dirs \u{b7} hiphop/MF DOOM - Mm..Food")
            .stop(Some("esc".to_owned()));
        assert_eq!(
            text(&progress.line(100)),
            "scanning \u{b7} 1 280 files \u{b7} 112 dirs \u{b7} hiphop/MF DOOM - Mm..Food \u{b7} esc to stop"
        );
    }

    #[test]
    fn a_known_total_draws_a_bar_and_an_unknown_one_does_not() {
        let line = text(&Progress::new("committing").fraction(5, 10).line(80));
        assert!(line.contains("━━━━━─────  50%"), "{line}");
        let line = text(&Progress::new("scanning").line(80));
        assert!(!line.contains('━') && !line.contains('%'), "{line}");
        let line = text(&Progress::new("committing").fraction(0, 0).line(80));
        assert!(!line.contains('%'), "no total, no bar: {line}");
    }

    #[test]
    fn a_commit_past_its_first_change_offers_no_key() {
        let line = text(&Progress::new("committing").fraction(9, 10).line(80));
        assert!(!line.contains("to stop"), "{line}");
    }

    #[test]
    fn narrowing_cuts_the_detail_then_the_bar_and_never_the_key() {
        let progress = Progress::new("committing 3 operations")
            .fraction(11, 18)
            .detail("11/18 files (61%)")
            .stop(Some("esc".to_owned()));
        for cells in 40..=120 {
            let line = text(&progress.line(cells));
            assert!(width(&line) <= cells, "{cells}: {line:?}");
            assert!(line.starts_with("committing 3 operations"), "{line}");
            assert!(line.ends_with("esc to stop"), "{cells}: {line}");
        }
        assert!(text(&progress.line(120)).contains('━'));
        assert!(
            !text(&progress.line(60)).contains('━'),
            "too narrow for a bar"
        );
    }
}
