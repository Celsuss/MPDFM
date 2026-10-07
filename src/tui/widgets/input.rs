//! One line of text being typed, and the window of it that fits on screen.
//!
//! ```text
//! │ Genre      ▸ Hip Ho_                │   the field is 7 cells; the value is
//! │ Comment      … ノスタルジアの夏_    │   longer, so it scrolls with the cursor
//! ```
//!
//! Two callers, one editor: the `:` line ([`CommandLine`][crate::tui::command::CommandLine])
//! and every field of the tag editor (task 23). They had no business growing two
//! implementations of "backspace over a `ï`", which is the kind of thing that works
//! in one of them and not the other for a year before anybody notices.
//!
//! # What it is and is not
//!
//! Deliberately small: insert, backspace, move, clear, and the one piece of
//! arithmetic a fixed-width field needs. No history, no completion, no selection,
//! no word motions. Task 26 can add what the command line wants; a tag field wants
//! none of it.
//!
//! # Two things it does that a `String` and an index do not
//!
//! **It scrolls.** A tag field is twenty cells wide and an album title is not, so
//! [`Input::window`] returns the slice of the value around the cursor and the
//! column the terminal's own cursor goes in — measured in **cells**, so a value
//! with a `ノ` in it does not put the cursor adrift (see [the module
//! above][super]).
//!
//! **It remembers whether it was touched**, which is load-bearing rather than a
//! convenience. A tag editor field showing `<multiple>` must become "modified"
//! only when the user *actually types in it* (`docs/tasks/18-tag-bulk.md`), and
//! "the text is different from what was there" cannot answer that: a
//! `<multiple>` field opens empty, so a user who opens one and changes their mind
//! leaves it looking exactly like a user who emptied it on purpose. One of those
//! must write nothing to fourteen files and the other must clear a field on all
//! of them. [`Input::touched`] is the difference, and it is set by the keystroke
//! rather than inferred from the result.

use unicode_width::UnicodeWidthChar;

use super::{pad, width};

/// A line of text with a cursor in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    text: String,
    /// Where the next character goes, as a byte offset on a character boundary.
    cursor: usize,
    /// Whether any keystroke has changed the text since this input was made.
    touched: bool,
}

/// What [`Input::window`] worked out: the text to draw, and where the cursor is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Exactly as many cells wide as was asked for, space-padded.
    pub text: String,
    /// The terminal cursor's column, as an offset in cells from the field's
    /// first cell. Never past the last one.
    pub cursor: usize,
}

impl Input {
    /// An untouched input holding `text`, with the cursor at the end of it.
    ///
    /// The shape a form field opens in: what is there now, ready to be edited,
    /// and not yet counted as a change.
    #[must_use]
    pub fn of(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            touched: false,
        }
    }

    /// What has been typed.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text, consuming the input.
    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }

    /// Where the cursor is, as a byte offset into [`Input::text`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether a keystroke has changed the text.
    ///
    /// See the module documentation: this, and not a comparison against the old
    /// value, is what decides whether a `<multiple>` field was edited.
    #[must_use]
    pub fn touched(&self) -> bool {
        self.touched
    }

    /// Type a character at the cursor.
    pub fn insert(&mut self, c: char) -> bool {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.touched = true;
        true
    }

    /// Delete the character before the cursor. Returns whether there was one.
    pub fn backspace(&mut self) -> bool {
        let Some(previous) = self.text[..self.cursor].chars().next_back() else {
            return false;
        };
        self.cursor -= previous.len_utf8();
        self.text.remove(self.cursor);
        self.touched = true;
        true
    }

    /// Throw the whole line away. Returns whether there was anything to throw.
    ///
    /// Counts as a touch even when the line was already empty: `ctrl-u` on a
    /// field showing `<multiple>` is the user saying "nothing", which is a
    /// request to clear the field and not the absence of one.
    pub fn clear(&mut self) -> bool {
        let had = !self.text.is_empty();
        self.text.clear();
        self.cursor = 0;
        self.touched = true;
        had
    }

    /// Cursor one character left. Returns whether it moved.
    pub fn left(&mut self) -> bool {
        match self.text[..self.cursor].chars().next_back() {
            Some(previous) => {
                self.cursor -= previous.len_utf8();
                true
            }
            None => false,
        }
    }

    /// Cursor one character right. Returns whether it moved.
    pub fn right(&mut self) -> bool {
        match self.text[self.cursor..].chars().next() {
            Some(next) => {
                self.cursor += next.len_utf8();
                true
            }
            None => false,
        }
    }

    /// The `cells`-wide slice of the text the cursor is in, and the cursor's
    /// column within it.
    ///
    /// The window is pinned to the right of the cursor rather than centred on
    /// it, which is what a line editor does: typing never makes the text jump,
    /// and the character just typed is always the one before the cursor.
    ///
    /// A wide character that would be cut in half by either edge is replaced by
    /// a space, for the reason [the module above][super] gives: half a `ス` is
    /// not a character, and a field one cell too wide corrupts the border.
    #[must_use]
    pub fn window(&self, cells: usize) -> Window {
        if cells == 0 {
            return Window {
                text: String::new(),
                cursor: 0,
            };
        }

        // One cell is kept back for the cursor itself, so that a cursor at the
        // end of a full field has somewhere to sit.
        let before = width(&self.text[..self.cursor]);
        let offset = before.saturating_sub(cells - 1);

        let mut shown = String::new();
        let mut column = 0;
        for c in self.text.chars() {
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if column + w <= offset {
                // Entirely left of the window.
                column += w;
                continue;
            }
            if column < offset {
                // Straddling the left edge.
                shown.push(' ');
                column += w;
                continue;
            }
            if column + w > offset + cells {
                break;
            }
            shown.push(c);
            column += w;
        }

        Window {
            text: pad(&shown, cells),
            cursor: before - offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name from this library whose cells, characters and bytes are three
    /// different numbers.
    const CJK: &str = "ノスタルジア";

    #[test]
    fn typing_and_deleting_walk_whole_characters() {
        let mut input = Input::default();
        for c in "Hïp".chars() {
            input.insert(c);
        }
        assert_eq!(input.text(), "Hïp");
        assert_eq!(input.cursor(), 4, "`ï` is two bytes");

        assert!(input.backspace());
        assert!(input.backspace());
        assert_eq!(input.text(), "H", "the two-byte character went whole");
        assert!(input.backspace());
        assert!(!input.backspace(), "nothing left to delete");
    }

    #[test]
    fn a_character_goes_in_where_the_cursor_is() {
        let mut input = Input::of("Mm.Food");
        input.left();
        input.left();
        input.left();
        input.left();
        input.insert('.');
        assert_eq!(input.text(), "Mm..Food");
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut input = Input::of("ab");
        assert!(!input.right(), "already at the end");
        assert!(input.left() && input.left());
        assert!(!input.left(), "already at the start");
    }

    #[test]
    fn an_input_opened_on_a_value_is_not_yet_a_change() {
        // The whole of the `<multiple>` rule: opening a field is not editing it.
        let mut input = Input::of("MF DOOM");
        assert!(!input.touched());
        assert_eq!(input.cursor(), "MF DOOM".len(), "ready to be edited");

        input.right();
        input.left();
        assert!(!input.touched(), "moving around is not typing");

        input.insert('!');
        assert!(input.touched());
    }

    #[test]
    fn clearing_an_already_empty_field_is_still_a_change() {
        // `ctrl-u` on a `<multiple>` field means "clear it on every file", which
        // is a different request from never having touched it.
        let mut input = Input::default();
        assert!(!input.clear(), "there was nothing to throw away");
        assert!(
            input.touched(),
            "but the user asked for nothing, deliberately"
        );
    }

    #[test]
    fn a_value_that_fits_is_shown_whole_with_the_cursor_after_it() {
        let shown = Input::of("Hip Hop").window(20);
        assert_eq!(shown.text, "Hip Hop             ");
        assert_eq!(width(&shown.text), 20);
        assert_eq!(shown.cursor, 7);
    }

    #[test]
    fn a_value_longer_than_the_field_scrolls_to_keep_the_cursor_in_view() {
        let input = Input::of("Madvillainy; Operation Doomsday");
        let shown = input.window(10);
        assert_eq!(width(&shown.text), 10);
        assert_eq!(shown.cursor, 9, "one cell short of the edge, where it sits");
        assert_eq!(shown.text, " Doomsday ");

        // At the start of the same value, the window is the other end of it.
        let mut input = input;
        while input.left() {}
        let shown = input.window(10);
        assert_eq!(shown.cursor, 0);
        assert_eq!(shown.text, "Madvillain");
    }

    #[test]
    fn the_window_is_exactly_as_wide_as_asked_for_whatever_is_in_it() {
        for text in ["", "a", CJK, "01 So Hï", "Madvillainy; Operation Doomsday"] {
            let mut input = Input::of(text);
            loop {
                for cells in 0..=12 {
                    let shown = input.window(cells);
                    assert_eq!(
                        width(&shown.text),
                        cells,
                        "{text:?} at cursor {} in {cells} cells is {:?}",
                        input.cursor(),
                        shown.text
                    );
                    assert!(
                        cells == 0 || shown.cursor < cells,
                        "the cursor left the field: {} of {cells}",
                        shown.cursor
                    );
                }
                if !input.left() {
                    break;
                }
            }
        }
    }

    #[test]
    fn a_wide_character_cut_by_the_left_edge_becomes_a_space() {
        // Six two-cell characters — twelve cells — with the cursor at the end of
        // them in a four-cell field. The window starts at column 9, which is the
        // second half of `ジ`, so that cell is blank rather than half a glyph and
        // the field is still exactly four cells wide.
        let shown = Input::of(CJK).window(4);
        assert_eq!(width(&shown.text), 4);
        assert_eq!(shown.text, " ア ");
        assert_eq!(shown.cursor, 3);
    }
}
