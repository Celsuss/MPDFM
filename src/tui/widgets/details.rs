//! The narrow third column: what the focused entry actually is.
//!
//! ```text
//! ┌ details ──────────────┐
//! │01 Beef Rap.mp3        │
//! │                       │
//! │Title   Beef Rap       │
//! │Artist  MF DOOM        │
//! │Album   Mm..Food       │
//! │Genre   —              │
//! │Length  3:24           │
//! │Bitrate 320 kbps       │
//! │                       │
//! │⚠ 2 lines in 2 playlis…│
//! │Hip hop                │
//! │MF Doom                │
//! └───────────────────────┘
//! ```
//!
//! # Why the playlist list is the important half
//!
//! The tags are a convenience; the playlist references are the thing the task
//! calls "genuinely useful before a move". Moving a track that three playlists
//! point at is a different decision from moving one that nothing points at, and
//! this is the only place in the UI that answers the question *before* the
//! operation is staged.
//!
//! So the references are never elided to a count: the headline says how many
//! *lines* point at the file and how many playlists they are in — two different
//! numbers, because one playlist can list a track twice and a rewrite has to fix
//! both — and the names follow it, one per row.
//!
//! # Width
//!
//! Everything goes through [`super::pad`] and [`super::fit`]. The pane is the
//! narrowest thing on screen and therefore where a width bug shows first.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::{fit, pad};

/// Cells given to the label column.
///
/// Eight, which is one more than the longest label: `Bitrate` butted straight up
/// against its value on the real library, which reads as one word.
const LABEL_W: usize = 8;

/// A value shown for a field with nothing in it.
const ABSENT: &str = "—";

/// One label and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    /// `Title`, `Artist`, `Bitrate`.
    pub label: &'static str,
    /// What the file says, or [`ABSENT`].
    pub value: String,
}

impl Detail {
    /// A row whose value may be missing.
    #[must_use]
    pub fn new(label: &'static str, value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label,
            value: if value.is_empty() {
                ABSENT.to_owned()
            } else {
                value
            },
        }
    }
}

/// Everything the details pane shows about one entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    /// The entry's own name, for the pane's first line.
    pub name: String,
    /// The label/value rows, in display order.
    pub rows: Vec<Detail>,
    /// The names of the playlists that reference this entry, deduplicated.
    pub playlists: Vec<String>,
    /// How many playlist *lines* point at it, which is not the same number: one
    /// playlist can list a track twice, and a rewrite has to fix both.
    pub references: usize,
    /// Something to say instead of the rows — "reading…", or why the file could
    /// not be read.
    pub note: Option<String>,
}

impl Details {
    /// The lines this pane would draw at `cells` wide.
    ///
    /// Separated from the rendering so a test can assert on the text without a
    /// terminal, and so the caller can tell whether there is anything to show.
    #[must_use]
    pub fn lines(&self, cells: usize) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::styled(
                pad(&self.name, cells),
                Style::new().add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
        ];

        if let Some(note) = &self.note {
            lines.push(Line::from(Span::styled(
                fit(note, cells),
                Style::new().fg(Color::Yellow),
            )));
            lines.push(Line::raw(""));
        }

        // On a pane too narrow for both, the label gives way: a value with no
        // label is still information, and a label with no value is not. Half
        // the pane is the floor, so neither column can swallow the other.
        let label_w = LABEL_W.min(cells / 2);
        let value_w = cells.saturating_sub(label_w);
        for row in &self.rows {
            lines.push(Line::from(vec![
                Span::styled(pad(row.label, label_w), Style::new().fg(Color::DarkGray)),
                Span::raw(fit(&row.value, value_w)),
            ]));
        }

        lines.push(Line::raw(""));
        lines.extend(self.playlist_lines(cells));
        lines
    }

    /// The reference section: the headline, then as many names as fit.
    fn playlist_lines(&self, cells: usize) -> Vec<Line<'static>> {
        if self.playlists.is_empty() {
            return vec![Line::from(Span::styled(
                fit("in no playlist", cells),
                Style::new().fg(Color::DarkGray),
            ))];
        }

        let lines = if self.references == 1 {
            "line"
        } else {
            "lines"
        };
        let mut out = vec![Line::from(Span::styled(
            fit(
                &format!(
                    "⚠ {} {lines} in {}",
                    self.references,
                    pluralize(self.playlists.len())
                ),
                cells,
            ),
            Style::new().fg(Color::Magenta),
        ))];
        out.extend(self.playlists.iter().map(|name| {
            Line::from(Span::styled(
                fit(name, cells),
                Style::new().fg(Color::Magenta),
            ))
        }));
        out
    }
}

/// `1 playlist` / `3 playlists`.
fn pluralize(count: usize) -> String {
    if count == 1 {
        "1 playlist".to_owned()
    } else {
        format!("{count} playlists")
    }
}

/// The details pane, drawn in a block.
pub struct DetailsPane<'a> {
    details: &'a Details,
    block: Block<'a>,
}

impl<'a> DetailsPane<'a> {
    /// A pane showing these details.
    pub fn new(details: &'a Details) -> Self {
        Self {
            details,
            block: Block::new(),
        }
    }

    /// The block to draw it in.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = block;
        self
    }
}

impl Widget for DetailsPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = self.block.inner(area);
        self.block.clone().render(area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let lines = self.details.lines(usize::from(inner.width));
        Paragraph::new(lines).render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::super::width;
    use super::*;

    fn sample() -> Details {
        Details {
            name: "01 Beef Rap.mp3".to_owned(),
            rows: vec![
                Detail::new("Title", "Beef Rap"),
                Detail::new("Artist", "MF DOOM"),
                Detail::new("Album", "Mm..Food"),
                Detail::new("Genre", ""),
                Detail::new("Length", "3:24"),
                Detail::new("Bitrate", "320 kbps"),
            ],
            playlists: vec!["Hip hop.m3u".to_owned(), "MF Doom.m3u".to_owned()],
            references: 2,
            note: None,
        }
    }

    fn text(details: &Details, cells: usize) -> String {
        details
            .lines(cells)
            .iter()
            .map(ratatui::text::Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn it_names_the_playlists_that_reference_the_track() {
        let drawn = text(&sample(), 30);
        assert!(drawn.contains("Hip hop.m3u"), "{drawn}");
        assert!(drawn.contains("MF Doom.m3u"), "{drawn}");
        assert!(drawn.contains("2 playlists"), "{drawn}");
        assert!(drawn.contains("2 lines"), "{drawn}");
    }

    #[test]
    fn a_track_nothing_points_at_says_so_rather_than_showing_a_blank() {
        let details = Details {
            playlists: Vec::new(),
            references: 0,
            ..sample()
        };
        let drawn = text(&details, 30);
        assert!(drawn.contains("in no playlist"), "{drawn}");
    }

    #[test]
    fn one_playlist_and_one_line_are_singular() {
        let details = Details {
            playlists: vec!["MF Doom.m3u".to_owned()],
            references: 1,
            ..sample()
        };
        let drawn = text(&details, 30);
        assert!(drawn.contains("1 line in 1 playlist"), "{drawn}");
    }

    #[test]
    fn an_empty_field_shows_a_dash_and_not_an_empty_row() {
        let drawn = text(&sample(), 30);
        assert!(
            drawn
                .lines()
                .any(|line| line.starts_with("Genre") && line.contains('—')),
            "{drawn}"
        );
    }

    #[test]
    fn nothing_overflows_the_pane_however_narrow_it_is() {
        let details = Details {
            name: "03 ノスタルジア.mp3".to_owned(),
            rows: vec![Detail::new("Artist", "中島みゆき")],
            ..sample()
        };
        for cells in 4..=40 {
            for line in details.lines(cells) {
                assert!(
                    width(&line.to_string()) <= cells,
                    "a {cells}-cell pane got a {}-cell line: {line:?}",
                    width(&line.to_string())
                );
            }
        }
    }

    #[test]
    fn a_note_replaces_nothing_and_is_shown_above_the_rows() {
        let details = Details {
            note: Some("reading…".to_owned()),
            ..sample()
        };
        let drawn = text(&details, 30);
        assert!(drawn.contains("reading…"), "{drawn}");
        assert!(drawn.contains("Title"), "the rows are still there: {drawn}");
    }
}
