//! The error panel: something went wrong, here is all of it, and here is what
//! to do next.
//!
//! # Not a toast
//!
//! A toast goes away on a timer, and an error that scrolls away unread is an
//! error that was swallowed — which the task forbids in as many words. So an
//! error opens this, on the view stack, and it stays until it is dismissed.
//! Afterwards `:messages` still has it (`widgets::toast`), because a panel that
//! was dismissed in a hurry is a panel somebody will want to read again.
//!
//! # Three parts, each optional but the first
//!
//! ```text
//! ┌ error ────────────────────────────────────────────┐
//! │these file(s) could not be read, so the editor was │
//! │not opened:                                        │
//! │                                                   │
//! │  - 01 Beef Rap.mp3: unexpected end of file        │
//! │                                                   │
//! │path  /home/user/Music/hiphop/MF DOOM - Mm..Food   │
//! │next  fix or unmark the file(s), then e again      │
//! └ esc to dismiss ───────────────────────────────────┘
//! ```
//!
//! - **the message**, whole: core's `Display`, which already names the step and
//!   the cause;
//! - **the path** it is about, when there is one, on a line of its own so it can
//!   be read (and selected with the mouse) without parsing it out of a
//!   sentence;
//! - **the next step**, when the code raising it knows one. "Run `mpdfm recover
//!   <txid>`" is worth more than the paragraph that explains why.
//!
//! # Wrapped, and scrolled when even that is not enough
//!
//! Everything is wrapped with `widgets::wrap`, which breaks a path with no
//! spaces in it rather than cutting it off. A list of two hundred unreadable
//! files does not fit in any panel, so the panel scrolls, and its footer says
//! how much more there is — the same contract as the help overlay.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::widgets::wrap;

/// The label column: `path  `, `next  `.
const LABEL: usize = 6;

/// An error, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorReport {
    /// What went wrong, whole.
    message: String,
    /// The file or directory it is about.
    path: Option<String>,
    /// What the user can do about it.
    next: Option<String>,
    /// How far down the panel has been scrolled.
    scroll: u16,
}

impl ErrorReport {
    /// An error with nothing known about it but what it says.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            path: None,
            next: None,
            scroll: 0,
        }
    }

    /// The path it is about.
    #[must_use]
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// What to do about it.
    #[must_use]
    pub fn next(mut self, next: impl Into<String>) -> Self {
        self.next = Some(next.into());
        self
    }

    /// The whole thing as one string, for the log and for `:messages`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = self.message.clone();
        if let Some(path) = &self.path {
            out.push_str(&format!("\npath  {path}"));
        }
        if let Some(next) = &self.next {
            out.push_str(&format!("\nnext  {next}"));
        }
        out
    }

    /// How far down it has been scrolled.
    #[must_use]
    pub fn scroll(&self) -> u16 {
        self.scroll
    }

    /// Scroll to `to`, which the caller has already clamped. Returns whether
    /// anything moved.
    pub fn scroll_to(&mut self, to: u16) -> bool {
        let moved = self.scroll != to;
        self.scroll = to;
        moved
    }

    /// The panel's text, `cells` wide.
    #[must_use]
    pub fn lines(&self, cells: usize) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = wrap(&self.message, cells)
            .into_iter()
            .map(Line::raw)
            .collect();

        let labelled = |label: &str, text: &str, style: Style, out: &mut Vec<Line<'static>>| {
            // Hung under itself, so a long path reads as one path.
            let rows = wrap(text, cells.saturating_sub(LABEL).max(1));
            for (index, row) in rows.into_iter().enumerate() {
                let head = if index == 0 {
                    Span::styled(
                        format!("{label:<LABEL$}"),
                        Style::new()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw(" ".repeat(LABEL))
                };
                out.push(Line::from(vec![head, Span::styled(row, style)]));
            }
        };

        if self.path.is_some() || self.next.is_some() {
            out.push(Line::default());
        }
        if let Some(path) = &self.path {
            labelled("path", path, Style::new().fg(Color::Cyan), &mut out);
        }
        if let Some(next) = &self.next {
            labelled("next", next, Style::new().fg(Color::Yellow), &mut out);
        }
        out
    }
}

/// A message with nothing else known about it — which is most of what core's
/// errors are, since their `Display` already names the path in the sentence.
impl From<String> for ErrorReport {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for ErrorReport {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::widgets::width;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_message_the_path_and_the_next_step_each_get_their_own_lines() {
        let report = ErrorReport::new("the scan could not read the music directory")
            .path("/home/user/Music")
            .next("check its permissions, then R to rescan");
        let rows = text(&report.lines(80));
        assert_eq!(
            rows,
            vec![
                "the scan could not read the music directory",
                "",
                "path  /home/user/Music",
                "next  check its permissions, then R to rescan",
            ]
        );
    }

    #[test]
    fn a_long_path_wraps_under_itself_and_nothing_is_cut() {
        let path = "/home/user/Music/hiphop/MF DOOM - Mm..Food (2004) [320]/01 Beef Rap.mp3";
        let report = ErrorReport::new("x").path(path);
        let rows = text(&report.lines(30));
        let path_rows: Vec<&String> = rows.iter().skip(2).collect();
        assert!(path_rows.len() > 1, "{rows:?}");
        assert!(
            path_rows[1..].iter().all(|row| row.starts_with("      ")),
            "{rows:?}"
        );
        assert!(rows.iter().all(|row| width(row) <= 30), "{rows:?}");
        let rejoined: String = path_rows
            .iter()
            .map(|row| row[LABEL..].to_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            rejoined.split_whitespace().collect::<String>(),
            path.split_whitespace().collect::<String>(),
            "every character of the path is on screen"
        );
    }

    #[test]
    fn the_summary_has_everything_the_panel_has() {
        let report = ErrorReport::new("it broke").path("/x").next("try again");
        assert_eq!(report.summary(), "it broke\npath  /x\nnext  try again");
        assert_eq!(ErrorReport::new("it broke").summary(), "it broke");
    }
}
