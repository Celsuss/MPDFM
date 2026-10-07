//! The listing: one line per entry, in fixed columns, for the rows that are on
//! screen and no others.
//!
//! ```text
//! ●   01 Beef Rap.mp3             3:24  320k
//! ● ⚠ 02 Hoe Cakes.mp3            4:02  320k
//!     03 Potholderz.mp3           2:58  320k
//!     folder.jpg
//! ```
//!
//! # Virtualized, and why that is the widget's business
//!
//! [`FileList`] is handed **only the rows it will draw**. The caller windows the
//! listing with `views::browser::window` and passes a slice; a 400-file
//! directory therefore costs the same per frame as a 4-file
//! one, because nothing here iterates over what is scrolled off. That is the
//! task's first requirement, and it is enforced by shape rather than by
//! discipline: the widget cannot touch a row it was not given.
//!
//! The same slice decides which tags are read, so "only the visible rows are laid
//! out" and "tags are read only for visible rows" are one fact rather than two.
//!
//! # The columns
//!
//! | column | width | holds |
//! | --- | --- | --- |
//! | mark | 2 | `●` when the row is marked |
//! | flag | 2 | `⚠` when a playlist references this entry, or something under it |
//! | name | the rest | the file or directory name, ellipsized |
//! | time | 6 | `3:24`, right-aligned |
//! | rate | 6 | `320k`, right-aligned |
//!
//! Every one of them is measured in cells by [`super::pad`], never in bytes: see
//! that module for the `ノスタルジア` case.
//!
//! The last two columns are dropped on a narrow pane rather than squeezed — a
//! three-character name is not worth a bitrate — and the thresholds are
//! [`TIME_FROM`] and [`RATE_FROM`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::{pad, pad_left};

/// Cells for the mark column, the trailing space included.
const MARK_W: usize = 2;
/// Cells for the playlist-reference flag.
const FLAG_W: usize = 2;
/// Cells for the duration.
const TIME_W: usize = 6;
/// Cells for the bitrate.
const RATE_W: usize = 6;
/// The narrowest name that is still worth reading.
const NAME_MIN: usize = 8;

/// How wide the pane has to be before the duration column is drawn.
const TIME_FROM: usize = MARK_W + FLAG_W + NAME_MIN + TIME_W;
/// How wide the pane has to be before the bitrate column is drawn.
const RATE_FROM: usize = TIME_FROM + RATE_W;

/// What a row is, which is all the styling depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A subdirectory. Listed first and drawn with a trailing `/`.
    Dir,
    /// A track MPDFM can edit.
    Audio,
    /// Cover art, a `.nfo`, a `.cue` — everything else.
    ///
    /// Dimmed rather than hidden: a move takes the whole directory, and the user
    /// has to see that `folder.jpg` travels with the album. The task is explicit
    /// about this and it is an acceptance criterion.
    Other,
}

/// What is known about a row's metadata, which is a state and not an `Option`.
///
/// "Not read yet" and "read, and the file has no bitrate to report" look the same
/// in an `Option<String>` and are different things to put in front of a user who
/// is waiting for a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meta {
    /// Nothing to show: a directory, or a file that is not audio.
    None,
    /// A read has been asked for and has not come back.
    Reading,
    /// The file was read.
    Known {
        /// `3:24`.
        duration: String,
        /// `320k`.
        bitrate: String,
    },
    /// The file could not be read. The reason is in the details pane; here there
    /// is only room to say that there is one.
    Failed,
}

/// One line of the listing, resolved to exactly what will be drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The file or directory name, without its parent.
    pub name: String,
    /// What it is.
    pub kind: RowKind,
    /// Whether it is in the mark set.
    pub marked: bool,
    /// Whether a playlist references it, or — for a directory — anything inside
    /// it.
    pub referenced: bool,
    /// Its duration and bitrate, if that is a thing it has.
    pub meta: Meta,
}

/// A window of the listing, drawn inside a block.
pub struct FileList<'a> {
    rows: &'a [Row],
    /// Index **within `rows`** of the cursor, if the cursor is in this window.
    cursor: Option<usize>,
    /// Indices within `rows` covered by an in-progress visual selection.
    range: Option<(usize, usize)>,
    /// Whether this pane has the keyboard, which decides how loud the cursor is.
    focused: bool,
    block: Block<'a>,
}

impl<'a> FileList<'a> {
    /// A list over exactly these rows.
    pub fn new(rows: &'a [Row]) -> Self {
        Self {
            rows,
            cursor: None,
            range: None,
            focused: false,
            block: Block::new(),
        }
    }

    /// Put the cursor on a row of this window.
    #[must_use]
    pub fn cursor(mut self, cursor: Option<usize>) -> Self {
        self.cursor = cursor;
        self
    }

    /// Highlight an inclusive range of this window as a pending visual selection.
    #[must_use]
    pub fn range(mut self, range: Option<(usize, usize)>) -> Self {
        self.range = range;
        self
    }

    /// Whether this pane has the keyboard.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The block to draw it in.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = block;
        self
    }

    /// One row, as spans that add up to exactly `cells` cells.
    fn line(&self, index: usize, row: &Row, cells: usize) -> Line<'static> {
        let in_range = self
            .range
            .is_some_and(|(from, to)| index >= from && index <= to);
        let marked = row.marked || in_range;

        let time = cells >= TIME_FROM;
        let rate = cells >= RATE_FROM;
        let name_w = cells
            .saturating_sub(MARK_W + FLAG_W)
            .saturating_sub(if time { TIME_W } else { 0 })
            .saturating_sub(if rate { RATE_W } else { 0 });

        let name = match row.kind {
            RowKind::Dir => format!("{}/", row.name),
            _ => row.name.clone(),
        };

        let mut spans = vec![
            Span::styled(
                pad(if marked { "●" } else { "" }, MARK_W),
                Style::new().fg(if in_range {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
            Span::styled(
                pad(if row.referenced { "⚠" } else { "" }, FLAG_W),
                Style::new().fg(Color::Magenta),
            ),
            Span::styled(pad(&name, name_w), name_style(row.kind)),
        ];
        if time {
            spans.push(Span::styled(
                pad_left(meta_text(&row.meta, true), TIME_W),
                Style::new().fg(Color::DarkGray),
            ));
        }
        if rate {
            spans.push(Span::styled(
                pad_left(meta_text(&row.meta, false), RATE_W),
                Style::new().fg(Color::DarkGray),
            ));
        }

        let mut line = Line::from(spans);
        if self.cursor == Some(index) {
            line = line.style(if self.focused {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                // A pane without the keyboard still shows where it was left, or
                // `tab` would look like it lost the position.
                Style::new().add_modifier(Modifier::BOLD)
            });
        }
        line
    }
}

impl Widget for FileList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let inner = self.block.inner(area);
        self.block.clone().render(area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let cells = usize::from(inner.width);
        let lines: Vec<Line<'static>> = self
            .rows
            .iter()
            .take(usize::from(inner.height))
            .enumerate()
            .map(|(index, row)| self.line(index, row, cells))
            .collect();
        Paragraph::new(lines).render(inner, buf);
    }
}

/// How a name is drawn, which is the whole of "visually distinguished".
fn name_style(kind: RowKind) -> Style {
    match kind {
        RowKind::Dir => Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        RowKind::Audio => Style::new(),
        RowKind::Other => Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
    }
}

/// The duration or the bitrate column's text for a row's metadata state.
fn meta_text(meta: &Meta, duration: bool) -> &str {
    match meta {
        Meta::None => "",
        // Not a spinner: the column is six cells and a frame costs a redraw.
        Meta::Reading => "·",
        Meta::Failed => "?",
        Meta::Known { duration: d, .. } if duration => d,
        Meta::Known { bitrate, .. } => bitrate,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Borders;

    use super::*;

    /// Every drawn row, cell by cell.
    ///
    /// Cells and not a string, because that is the only honest way to ask where
    /// a column landed: a two-cell `ノ` occupies `buffer[(x, y)]` and leaves
    /// `buffer[(x + 1, y)]` as a filler, so a reconstructed string measures
    /// wider than the row really is. Everything below counts cells.
    fn cells(terminal: &Terminal<TestBackend>, y: u16) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area().width)
            .map(|x| buffer[(x, y)].symbol().to_owned())
            .collect()
    }

    /// Every drawn row as text, for a `contains` assertion.
    fn drawn(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let height = terminal.backend().buffer().area().height;
        (0..height).map(|y| cells(terminal, y).concat()).collect()
    }

    /// Which cell `needle` starts in, on row `y`.
    fn column_of(terminal: &Terminal<TestBackend>, y: u16, needle: &str) -> usize {
        let cells = cells(terminal, y);
        let joined = cells.concat();
        let byte = joined
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not on row {y}: {joined:?}"));
        let mut at = 0;
        for (column, cell) in cells.iter().enumerate() {
            if at + cell.len() > byte {
                return column;
            }
            at += cell.len();
        }
        cells.len()
    }

    fn row(name: &str, kind: RowKind) -> Row {
        Row {
            name: name.to_owned(),
            kind,
            marked: false,
            referenced: false,
            meta: Meta::None,
        }
    }

    fn track(name: &str, duration: &str, bitrate: &str) -> Row {
        Row {
            meta: Meta::Known {
                duration: duration.to_owned(),
                bitrate: bitrate.to_owned(),
            },
            ..row(name, RowKind::Audio)
        }
    }

    fn render(rows: &[Row], width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("a backend has a size");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    FileList::new(rows)
                        .cursor(Some(0))
                        .focused(true)
                        .block(Block::new().borders(Borders::ALL)),
                    frame.area(),
                );
            })
            .expect("drawing should work");
        terminal
    }

    #[test]
    fn the_columns_line_up_whatever_the_name_is() {
        // The three widths that break naive maths: ASCII, Latin-1 with a
        // combining-free `ï`, and CJK where every character is two cells.
        let rows = [
            track("01 Beef Rap.mp3", "3:24", "320k"),
            track("01 So Hï.mp3", "4:02", "192k"),
            track("03 ノスタルジア.mp3", "2:58", "128k"),
        ];
        let terminal = render(&rows, 50, 5);

        for y in 1..=3 {
            let cells = cells(&terminal, y);
            // The border is intact, which is the thing a width bug destroys:
            // one cell too many and the right-hand `│` is pushed off the row.
            assert_eq!(cells[0], "│", "row {y}: {}", cells.concat());
            assert_eq!(cells[49], "│", "row {y}: {}", cells.concat());
        }

        // And the bitrate column starts at the same cell on all three, which is
        // the alignment a naive `str::len` gets wrong by six on the CJK row.
        assert_eq!(column_of(&terminal, 1, "320k"), 45);
        assert_eq!(column_of(&terminal, 2, "192k"), 45);
        assert_eq!(column_of(&terminal, 3, "128k"), 45);
    }

    #[test]
    fn only_the_rows_it_is_given_are_drawn() {
        // The widget cannot scroll past its window, because it does not have one:
        // a listing of three in a pane with room for two shows two.
        let rows = [
            track("a.mp3", "1:00", "320k"),
            track("b.mp3", "2:00", "320k"),
            track("c.mp3", "3:00", "320k"),
        ];
        let terminal = render(&rows, 40, 4);
        let text = drawn(&terminal).join("\n");
        assert!(text.contains("a.mp3"), "{text}");
        assert!(text.contains("b.mp3"), "{text}");
        assert!(!text.contains("c.mp3"), "a third row had nowhere to go");
    }

    #[test]
    fn a_directory_gets_a_slash_and_a_non_audio_file_is_still_listed() {
        let rows = [
            row("CD 1 - Mercury", RowKind::Dir),
            track("01 Wrecked.mp3", "3:24", "320k"),
            row("folder.jpg", RowKind::Other),
            row("info.nfo", RowKind::Other),
        ];
        let terminal = render(&rows, 50, 6);
        let text = drawn(&terminal).join("\n");
        assert!(text.contains("CD 1 - Mercury/"), "{text}");
        assert!(text.contains("folder.jpg"), "{text}");
        assert!(text.contains("info.nfo"), "{text}");
    }

    #[test]
    fn a_non_audio_row_is_dimmed_and_a_track_is_not() {
        let rows = [
            track("01 Wrecked.mp3", "3:24", "320k"),
            row("folder.jpg", RowKind::Other),
        ];
        let mut terminal = Terminal::new(TestBackend::new(50, 4)).expect("a backend has a size");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    FileList::new(&rows).block(Block::new().borders(Borders::ALL)),
                    frame.area(),
                );
            })
            .expect("drawing should work");

        let buffer = terminal.backend().buffer();
        // Column 5 is inside the name, past the mark and flag columns.
        let audio = buffer[(5, 1)].style();
        let other = buffer[(5, 2)].style();
        assert!(
            !audio.add_modifier.contains(Modifier::DIM),
            "a track should be drawn plainly"
        );
        assert!(
            other.add_modifier.contains(Modifier::DIM),
            "a non-audio file has to be visibly different, not merely present"
        );
    }

    #[test]
    fn a_mark_and_a_playlist_flag_are_both_visible() {
        let rows = [Row {
            marked: true,
            referenced: true,
            ..track("01 Beef Rap.mp3", "3:24", "320k")
        }];
        let terminal = render(&rows, 50, 3);
        let line = drawn(&terminal)[1].clone();
        assert!(line.contains('●'), "{line}");
        assert!(line.contains('⚠'), "{line}");
        // The glyphs must not push the name out of its column.
        assert_eq!(column_of(&terminal, 1, "01 Beef Rap.mp3"), 5);
    }

    #[test]
    fn a_narrow_pane_drops_the_columns_rather_than_the_name() {
        let rows = [track("01 Beef Rap.mp3", "3:24", "320k")];
        // 20 cells of pane: no room for either number.
        let terminal = render(&rows, 22, 3);
        let line = drawn(&terminal)[1].clone();
        assert!(line.contains("Beef"), "the name survives: {line}");
        assert!(!line.contains("320k"), "{line}");
        assert_eq!(cells(&terminal, 1)[21], "│", "and the border holds: {line}");
    }

    #[test]
    fn a_row_whose_tags_have_not_arrived_says_so_rather_than_showing_nothing() {
        let rows = [Row {
            meta: Meta::Reading,
            ..row("01 Beef Rap.mp3", RowKind::Audio)
        }];
        let terminal = render(&rows, 50, 3);
        let line = drawn(&terminal)[1].clone();
        assert!(line.contains('·'), "{line}");
    }

    #[test]
    fn a_visual_range_shows_as_marked_before_it_is_committed() {
        let rows = [
            track("a.mp3", "1:00", "320k"),
            track("b.mp3", "2:00", "320k"),
            track("c.mp3", "3:00", "320k"),
        ];
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).expect("a backend has a size");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    FileList::new(&rows)
                        .range(Some((0, 1)))
                        .block(Block::new().borders(Borders::ALL)),
                    frame.area(),
                );
            })
            .expect("drawing should work");
        let lines = drawn(&terminal);
        assert!(lines[1].contains('●'), "{:?}", lines[1]);
        assert!(lines[2].contains('●'), "{:?}", lines[2]);
        assert!(!lines[3].contains('●'), "{:?}", lines[3]);
    }
}
