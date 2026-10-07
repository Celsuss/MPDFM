//! The widgets the views are drawn from, and the column arithmetic they share.
//!
//! | module | draws |
//! | --- | --- |
//! | `filelist` | the listing: a window of rows, marks, flags, duration, bitrate |
//! | `details` | the narrow third column: tags, audio properties, playlists |
//! | `input` | one line of text being typed, and the window of it that fits |
//!
//! # Why the width math is here and not inlined
//!
//! A column is a number of **cells**, and `str::len` counts bytes while
//! `str::chars().count()` counts code points. Neither is the answer: `ノスタルジア`
//! is 18 bytes, 6 characters and **12 cells**. Truncating it by either of the
//! first two spills the row past the pane's border and pushes every column after
//! it out of alignment — the task's first pitfall, and the one that only shows up
//! on the 1 023 real files whose names are not ASCII.
//!
//! So every string that goes into a fixed-width column goes through [`fit`] or
//! [`pad`] first, both of which measure with `unicode-width`, and the two of them
//! are tested against the real names in `japanese/` and `KREAM - So Hï` rather
//! than against invented ones.
//!
//! One deliberate limitation: a wide character that would land half-in and
//! half-out of the column is dropped and replaced by a space, so the column is
//! exactly as wide as it says it is. Half of a `ス` is not a character, and a row
//! that is one cell too long is a row that corrupts the border.

pub mod details;
pub mod filelist;
pub mod input;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The ellipsis a truncated value ends with. One cell wide.
const ELLIPSIS: char = '…';

/// How many terminal cells `text` occupies.
#[must_use]
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// `text`, shortened to at most `cells` cells, with an ellipsis if anything was
/// dropped.
///
/// Never wider than asked for, and never splits a wide character: if the last
/// character that would fit is two cells wide and only one is left, the cell is
/// left blank. A `cells` of 0 gives the empty string, and of 1 gives either a
/// one-cell string or the ellipsis.
///
/// ```ignore
/// assert_eq!(fit("01 Beef Rap.mp3", 8), "01 Beef…");
/// assert_eq!(width(&fit("03 ノスタルジア.mp3", 10)), 10);
/// ```
#[must_use]
pub fn fit(text: &str, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    if width(text) <= cells {
        return text.to_owned();
    }

    // One cell is kept back for the ellipsis, which is what makes a truncation
    // visible rather than a name that merely looks short.
    let budget = cells - 1;
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push(ELLIPSIS);
    // A dropped half-cell: the ellipsis went in at `used + 1`, which may be one
    // short of `cells` when a two-cell character was the one that did not fit.
    out
}

/// `text`, trimmed or space-padded to exactly `cells` cells.
///
/// The invariant every caller relies on: `width(&pad(s, n)) == n`, for any string
/// and any width.
#[must_use]
pub fn pad(text: &str, cells: usize) -> String {
    let mut out = fit(text, cells);
    let short = cells.saturating_sub(width(&out));
    out.extend(std::iter::repeat_n(' ', short));
    out
}

/// `text`, right-aligned in exactly `cells` cells.
///
/// For the duration and bitrate columns, where the digits line up and the
/// shortest value is the one that moves.
#[must_use]
pub fn pad_left(text: &str, cells: usize) -> String {
    let fitted = fit(text, cells);
    let short = cells.saturating_sub(width(&fitted));
    let mut out = " ".repeat(short);
    out.push_str(&fitted);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real names the task names, so the assertions are about this library
    /// and not about an invented worst case.
    const CJK: &str = "03 ノスタルジア.mp3";
    const LATIN1: &str = "01 So Hï.mp3";

    #[test]
    fn width_counts_cells_and_not_bytes_or_characters() {
        // The three numbers that are all different, which is the whole reason
        // this module exists.
        assert_eq!(CJK.len(), 25, "bytes");
        assert_eq!(CJK.chars().count(), 13, "characters");
        assert_eq!(width(CJK), 19, "cells: six of them are two cells wide");

        // A combining-free Latin-1 name is one cell per character.
        assert_eq!(width(LATIN1), LATIN1.chars().count());
    }

    #[test]
    fn fit_never_returns_more_cells_than_it_was_asked_for() {
        for text in [CJK, LATIN1, "01 Beef Rap.mp3", "", "…", "ス"] {
            for cells in 0..=24 {
                let fitted = fit(text, cells);
                assert!(
                    width(&fitted) <= cells,
                    "fit({text:?}, {cells}) = {fitted:?} is {} cells",
                    width(&fitted)
                );
            }
        }
    }

    #[test]
    fn a_wide_character_is_dropped_rather_than_split_in_half() {
        // `03 ` is three cells, then a two-cell `ノ`. Asked for five cells, the
        // budget for text is four: `03 ` fits, `ノ` does not, and the result is
        // four cells rather than a half-drawn glyph in the fifth.
        let fitted = fit(CJK, 5);
        assert_eq!(fitted, "03 …");
        assert_eq!(width(&fitted), 4, "one cell is left blank on purpose");

        // Padding puts the missing cell back, which is what keeps the column
        // aligned even in the half-cell case.
        assert_eq!(width(&pad(CJK, 5)), 5);
    }

    #[test]
    fn pad_is_exact_for_every_width_and_every_name() {
        for text in [CJK, LATIN1, "01 Beef Rap.mp3", "", "ス", "ノスタルジア"] {
            for cells in 0..=30 {
                assert_eq!(
                    width(&pad(text, cells)),
                    cells,
                    "pad({text:?}, {cells}) is the wrong width"
                );
                assert_eq!(
                    width(&pad_left(text, cells)),
                    cells,
                    "pad_left({text:?}, {cells}) is the wrong width"
                );
            }
        }
    }

    #[test]
    fn a_short_name_is_returned_whole_and_a_long_one_says_it_was_cut() {
        assert_eq!(fit("01 Beef Rap.mp3", 40), "01 Beef Rap.mp3");
        assert_eq!(fit("01 Beef Rap.mp3", 8), "01 Beef…");
        assert_eq!(fit("", 8), "");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn right_alignment_puts_the_padding_in_front() {
        assert_eq!(pad_left("3:24", 6), "  3:24");
        assert_eq!(pad_left("320k", 6), "  320k");
        assert_eq!(pad("3:24", 6), "3:24  ");
    }
}
