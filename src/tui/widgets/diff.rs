//! The per-playlist line diff: what a playlist says now, and what it will say.
//!
//! ```text
//!     - hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3
//!     + hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3
//!     - hiphop/MF DOOM - Mm Food/folder.nfo
//!       entry 9 is removed, not rewritten
//! ```
//!
//! # Why this is a widget and not a count
//!
//! The pending view's job is to be believed, and "3 lines rewritten" is a claim
//! about a file the user cannot see. This is the screen that earns their trust
//! (`docs/tasks/24-pending-view.md`), so every changed line gets its old text and
//! its new text, from the [`LineEdit`]s **that commit will apply** — not from a
//! second calculation that could disagree with them.
//!
//! The same function draws MPD's saved queue, which is a list of
//! [`LineEdit`]s with no playlist around it ([`Effects::state_edits`]).
//!
//! # Two rows per edit, always
//!
//! A rewrite is a `-` and a `+`. A removal is a `-` and a line that says so in
//! words, rather than a `+` with nothing after it — losing a line is the one
//! thing a commit does that destroys something the user wrote, and it should not
//! be signalled by the *absence* of a character.
//!
//! Two rows per edit whatever the edit is, so [`rows`] can say how tall a diff
//! is without rendering it — which is what lets the pending view scroll a
//! two-thousand-line plan instead of truncating it.
//!
//! # Nothing is dropped vertically
//!
//! Long paths are shortened from the left ([`fit_end`]), because the end of a
//! music path is the track. Lines are never dropped: the view scrolls. A hidden
//! change is exactly what this program exists to prevent, which is the task's
//! own pitfall.
//!
//! [`Effects::state_edits`]: mpdfm_core::ops::Effects::state_edits

use mpdfm_core::playlist::rewrite::LineEdit;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{fit, fit_end};

/// How far a diff row is indented under the playlist it belongs to.
///
/// Two past the playlist name, which is itself indented by
/// [`PLAYLIST_INDENT`][mpdfm_core::ops::PLAYLIST_INDENT] — so a diff reads as
/// belonging to the row above it rather than as a section of its own.
pub const INDENT: usize = 4;

/// The mark in front of the line as it is now.
const OLD: &str = "- ";

/// The mark in front of the line as it will be.
const NEW: &str = "+ ";

/// How many rows these edits unfold into.
///
/// Two per edit, and the count is exact: the pending view adds it to its row
/// total before it knows how wide its pane is.
#[must_use]
pub fn rows(edits: &[LineEdit]) -> usize {
    edits.len() * 2
}

/// The edits as styled rows, `cells` wide.
///
/// A pure function of the edits: every row this returns is one commit will
/// apply, in the order it will apply them.
#[must_use]
pub fn lines(edits: &[LineEdit], cells: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(rows(edits));
    for edit in edits {
        out.push(row(OLD, &edit.old, Color::Red, cells));
        match &edit.new {
            Some(new) => out.push(row(NEW, new, Color::Green, cells)),
            // Spelled out, because a removal is the destructive half and the
            // entry number is what makes it findable in the file afterwards.
            None => out.push(note(
                &format!("entry {} is removed, not rewritten", edit.entry + 1),
                cells,
            )),
        }
    }
    out
}

/// One `-` or `+` row: the mark, then as much of the path as there is room for.
///
/// At a width too narrow for even the mark, the mark is what is kept: a row that
/// says `-` and nothing else is still a row the user can count, and one cell of
/// overflow corrupts the pane's border.
fn row(mark: &str, path: &str, color: Color, cells: usize) -> Line<'static> {
    let gutter = INDENT + mark.len();
    if cells <= gutter {
        return Line::from(Span::styled(
            fit(&format!("{:INDENT$}{mark}", ""), cells),
            Style::new().fg(color),
        ));
    }
    Line::from(vec![
        Span::raw(format!("{:INDENT$}", "")),
        Span::styled(
            mark.to_owned(),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(fit_end(path, cells - gutter), Style::new().fg(color)),
    ])
}

/// The line under a removal, in words and in the same column as the paths.
fn note(text: &str, cells: usize) -> Line<'static> {
    let indent = INDENT + OLD.len();
    if cells <= indent {
        return Line::raw(" ".repeat(cells));
    }
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(
            fit(text, cells - indent),
            Style::new().fg(Color::Red).add_modifier(Modifier::DIM),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::super::width;
    use super::*;

    fn rewrite(entry: usize) -> LineEdit {
        LineEdit {
            entry,
            old: "hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3".to_owned(),
            new: Some("hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3".to_owned()),
        }
    }

    fn removal(entry: usize) -> LineEdit {
        LineEdit {
            entry,
            old: "hiphop/MF DOOM - Mm Food/folder.nfo".to_owned(),
            new: None,
        }
    }

    /// The rows as plain text, which is what a reader sees.
    fn drawn(edits: &[LineEdit], cells: usize) -> Vec<String> {
        lines(edits, cells)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_rewritten_line_shows_both_what_it_says_and_what_it_will_say() {
        let drawn = drawn(&[rewrite(2)], 80);
        assert_eq!(
            drawn,
            vec![
                "    - hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3",
                "    + hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3",
            ]
        );
    }

    #[test]
    fn a_removed_line_says_so_in_words_rather_than_by_showing_nothing() {
        // The one thing a commit does that loses something the user wrote, and
        // the entry number is how they find it in the backup afterwards.
        let drawn = drawn(&[removal(8)], 80);
        assert_eq!(drawn.len(), 2);
        assert!(drawn[0].starts_with("    - hiphop/MF DOOM"), "{drawn:?}");
        assert_eq!(drawn[1], "      entry 9 is removed, not rewritten");
    }

    #[test]
    fn two_thousand_changed_lines_are_two_thousand_rows_and_none_of_them_is_dropped() {
        // The task's pitfall: a long plan scrolls, it does not truncate.
        let edits: Vec<LineEdit> = (0..2_000).map(rewrite).collect();
        assert_eq!(rows(&edits), 4_000);
        assert_eq!(lines(&edits, 80).len(), 4_000);
    }

    #[test]
    fn a_row_is_never_wider_than_the_pane_at_any_width() {
        let edits = [rewrite(0), removal(1)];
        for cells in 0..=90 {
            for line in lines(&edits, cells) {
                let text = line.to_string();
                assert!(
                    width(&text) <= cells,
                    "at {cells} cells, {text:?} is {} wide",
                    width(&text)
                );
            }
        }
    }

    #[test]
    fn a_path_too_long_for_the_pane_keeps_its_end() {
        let drawn = drawn(&[rewrite(0)], 24);
        assert!(drawn[0].ends_with("Beef Rap.mp3"), "{drawn:?}");
        assert!(drawn[1].ends_with("Beef Rap.mp3"), "{drawn:?}");
        assert!(drawn[0].contains('…'), "{drawn:?}");
    }
}
