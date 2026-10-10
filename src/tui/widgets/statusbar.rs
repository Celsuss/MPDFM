//! The status bar: where you are, what is selected, what is staged, what is
//! narrowing the listing, and whether MPD is alive.
//!
//! # What is on it
//!
//! ```text
//! hiphop/MF DOOM - Mm..Food · 3 marked · 2 pending · filter `mp3` · ⚠ 1 warning · sort name · focus files      ● playing 01 Beef Rap.mp3
//! ```
//!
//! Everything on the left is about the library and the listing; the MPD
//! indicator is on the right, on its own, because it is about something else
//! and because a glance for "is the daemon up" should land in the same place
//! every time rather than wherever the left half happened to end.
//!
//! # What goes first when it does not fit
//!
//! The task's pitfall: decide this, or a narrow terminal gets a jumbled bar. It
//! is decided in [`ELISION`], one step at a time, least important first, and the
//! bar stops at the first step after which it fits:
//!
//! | step | part | what happens | why it can go |
//! | --- | --- | --- | --- |
//! | 1 | sort | dropped | the listing's order shows it |
//! | 2 | focus | dropped | the highlighted cursor shows it |
//! | 3 | song | shortened, then dropped | a convenience; the indicator stays |
//! | 4 | path | shortened from the left, then dropped | the listing's title carries it too |
//! | 5 | warnings | `⚠ 3 warnings` → `⚠3` | the badge stays, only the word goes |
//! | 6 | marked, pending | `3 marked` → `3m`, `2 pending` → `2p` | the counts are what matter |
//! | 7 | filter, find | shortened, never dropped | a listing quietly missing rows is a listing that lies |
//! | 8 | MPD | `● connected` → `●` | the glyph still says which of the three |
//!
//! `VISUAL` and `WRITING` are never touched: each is a state a key behaves
//! differently in, and short enough that there is no saving to be had.
//!
//! At [`MIN_SIZE`][crate::tui::terminal::MIN_SIZE]'s 60 columns every part that
//! survives step 8 fits with room to spare, which is what the tests check across
//! every width from 60 to 200. Below that floor the shell does not draw the bar at
//! all, so the last-resort truncation at the end of [`Status::layout`] is for a
//! caller that ignored the floor, not for a user.

use mpdfm_core::mpd::PlayState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{fit, fit_end, width};
use crate::tui::msg::MpdSnapshot;

/// Between two parts of the bar.
const SEP: &str = " · ";

/// How many cells [`SEP`] takes — three, though it is four bytes.
const SEP_W: usize = 3;

/// The narrowest a shortened path is still worth showing. `…/01 Beef` says
/// something; `…ef` does not, and dropping it is the honest alternative.
const PATH_MIN: usize = 10;

/// The same, for the song.
const SONG_MIN: usize = 8;

/// The narrowest an active filter is shortened to. It is never dropped, so this
/// is a floor and not a threshold.
const FILTER_MIN: usize = 12;

/// The parts of the bar, by name, so that the elision order is a list of these
/// rather than an order implied by some code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The directory the listing is showing.
    Path,
    /// How many things are marked.
    Marked,
    /// How many operations are staged.
    Pending,
    /// The filter in force.
    Filter,
    /// The library-wide result set being listed.
    Find,
    /// A visual range is open.
    Visual,
    /// A transaction is running.
    Writing,
    /// How many things the scan could not model.
    Warnings,
    /// The listing's sort.
    Sort,
    /// Which pane has the keyboard.
    Focus,
    /// The MPD indicator.
    Mpd,
    /// What MPD is playing.
    Song,
}

/// The order the bar gives things up in, least important first. See the module
/// documentation for what each step does and why it is where it is.
pub const ELISION: &[Slot] = &[
    Slot::Sort,
    Slot::Focus,
    Slot::Song,
    Slot::Path,
    Slot::Warnings,
    Slot::Marked,
    Slot::Pending,
    Slot::Filter,
    Slot::Find,
    Slot::Mpd,
];

/// What MPD was last seen doing, reduced to what the bar draws.
///
/// Also what decides whether a poll is worth a frame: two snapshots whose
/// `Light`s are equal look identical on screen, whatever else differs between
/// them — an elapsed-time field that ticks on every poll is not news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Light {
    /// No poll has answered yet.
    Unknown,
    /// `--no-mpd`, or `mpd_enabled = false`: MPDFM was told not to ask.
    Disabled,
    /// Asked, and nobody answered — not running, refused, or too slow.
    Offline,
    /// Answering, and rescanning its database.
    Updating,
    /// Answering.
    Connected {
        /// Playing, paused or stopped.
        play: PlayState,
        /// The file name of the current song, when there is one. The name and
        /// not the path: the directory is usually the album already on screen.
        song: Option<String>,
    },
}

impl Light {
    /// The light for what a poll found.
    #[must_use]
    pub fn of(snapshot: Option<&MpdSnapshot>) -> Self {
        let Some(snapshot) = snapshot else {
            return Self::Unknown;
        };
        let Some(state) = &snapshot.state else {
            // "I was told not to ask" and "it did not answer" are different
            // things to put in front of somebody wondering why there is no
            // indicator: the first is their own setting.
            return if snapshot.enabled {
                Self::Offline
            } else {
                Self::Disabled
            };
        };
        if state.updating {
            return Self::Updating;
        }
        Self::Connected {
            play: state.play_state,
            // "When playing", and paused is playing for this purpose: there is
            // a current song and the user will want to know which.
            song: match state.play_state {
                PlayState::Stop => None,
                PlayState::Play | PlayState::Pause => state
                    .song
                    .as_deref()
                    .map(|song| song.rsplit('/').next().unwrap_or(song).to_owned()),
            },
        }
    }

    /// The indicator, in full: `● connected`, `○ offline`, `◐ updating`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "○ mpd ?",
            Self::Disabled => "· mpd off",
            Self::Offline => "○ offline",
            Self::Updating => "◐ updating",
            Self::Connected { play, .. } => match play {
                PlayState::Play => "● playing",
                PlayState::Pause => "● paused",
                PlayState::Stop => "● connected",
            },
        }
    }

    /// The indicator with the words gone: the glyph alone still says which.
    fn glyph(&self) -> &'static str {
        self.label().split(' ').next().unwrap_or("?")
    }

    /// Its colour: green when it is there, red when it should be and is not.
    fn color(&self) -> Color {
        match self {
            Self::Unknown | Self::Disabled => Color::DarkGray,
            Self::Offline => Color::Red,
            Self::Updating => Color::Yellow,
            Self::Connected { .. } => Color::Green,
        }
    }

    /// The song, if there is one to show.
    fn song(&self) -> Option<&str> {
        match self {
            Self::Connected { song, .. } => song.as_deref(),
            _ => None,
        }
    }
}

impl std::fmt::Display for Light {
    /// The indicator and the song: what the log records when it changes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.song() {
            Some(song) => write!(f, "{} {song}", self.label()),
            None => f.write_str(self.label()),
        }
    }
}

/// Everything the bar shows, gathered by the shell into owned values.
///
/// The bar reads a little from everything, which is what a status bar is; this
/// struct is where that reading stops, so that the layout below is a pure
/// function of it and can be tested without an app.
#[derive(Debug, Clone)]
pub struct Status {
    /// The listing's directory, or what it is listing instead.
    pub path: String,
    /// How many things are marked.
    pub marked: usize,
    /// How many operations are staged.
    pub pending: usize,
    /// The filter in force, as typed.
    pub filter: Option<String>,
    /// The result set being listed: the query, and what to say about it.
    pub find: Option<String>,
    /// Whether a visual range is open.
    pub visual: bool,
    /// Whether a transaction is running.
    pub writing: bool,
    /// How many things the scan could not model.
    pub warnings: usize,
    /// The listing's sort.
    pub sort: String,
    /// Which pane has the keyboard.
    pub focus: &'static str,
    /// What MPD is doing.
    pub mpd: Light,
}

/// One half of a laid-out bar: each part that survived, and its text.
pub type Half = Vec<(Slot, String)>;

/// One part of the bar as it will be drawn.
#[derive(Debug, Clone)]
struct Part {
    slot: Slot,
    text: String,
}

impl Status {
    /// The parts that fit in `cells`, after as many elision steps as it took,
    /// left half and right half.
    ///
    /// Public for the tests, which are about which parts survive rather than
    /// about the colours they are drawn in.
    #[must_use]
    pub fn layout(&self, cells: usize) -> (Half, Half) {
        let mut left = self.left_parts();
        let mut right = vec![Part {
            slot: Slot::Mpd,
            text: self.mpd.label().to_owned(),
        }];
        if let Some(song) = self.mpd.song() {
            right.push(Part {
                slot: Slot::Song,
                text: song.to_owned(),
            });
        }

        for &slot in ELISION {
            let over = total(&left, &right).saturating_sub(cells);
            if over == 0 {
                break;
            }
            for parts in [&mut left, &mut right] {
                if let Some(at) = parts.iter().position(|part| part.slot == slot) {
                    match self.shrink(&parts[at], over) {
                        Some(text) => parts[at].text = text,
                        None => {
                            parts.remove(at);
                        }
                    }
                }
            }
        }

        let pairs = |parts: Vec<Part>| -> Half {
            parts
                .into_iter()
                .map(|part| (part.slot, part.text))
                .collect()
        };
        // Below the floor nothing above is enough, and a bar that overflows
        // its row would be clipped mid-glyph; so the left half is cut to what
        // is left, and only then the right.
        let over = total(&left, &right).saturating_sub(cells);
        if over > 0 {
            let right_w = right_width(&right);
            let room = cells.saturating_sub(right_w + SEP_W);
            let joined = left
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>()
                .join(SEP);
            let left = if left.is_empty() || room == 0 {
                Vec::new()
            } else {
                vec![(Slot::Path, fit(&joined, room))]
            };
            let right = if right_w > cells {
                vec![(Slot::Mpd, fit(self.mpd.glyph(), cells))]
            } else {
                pairs(right)
            };
            return (left, right);
        }
        (pairs(left), pairs(right))
    }

    /// The bar, laid out for `cells` columns: the left half, a gap, the right.
    #[must_use]
    pub fn line(&self, cells: usize) -> Line<'static> {
        let (left, right) = self.layout(cells);
        let dim = Style::new().fg(Color::DarkGray);

        let mut spans: Vec<Span<'static>> = Vec::new();
        for (index, (slot, text)) in left.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(SEP, dim));
            }
            spans.push(Span::styled(text.clone(), self.style(*slot)));
        }

        let used = joined(left.iter().map(|(_, text)| text.as_str()), SEP_W)
            + joined(right.iter().map(|(_, text)| text.as_str()), 1);
        let gap = cells
            .saturating_sub(used)
            .max(usize::from(!left.is_empty()));
        spans.push(Span::raw(" ".repeat(gap)));
        for (index, (slot, text)) in right.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(text.clone(), self.style(*slot)));
        }
        Line::from(spans)
    }

    /// The left half, in the order it is drawn.
    fn left_parts(&self) -> Vec<Part> {
        let mut parts = vec![
            Part {
                slot: Slot::Path,
                text: self.path.clone(),
            },
            Part {
                slot: Slot::Marked,
                text: format!("{} marked", self.marked),
            },
            Part {
                slot: Slot::Pending,
                text: format!("{} pending", self.pending),
            },
        ];
        if let Some(filter) = &self.filter {
            parts.push(Part {
                slot: Slot::Filter,
                text: format!("filter `{filter}`"),
            });
        }
        if let Some(find) = &self.find {
            parts.push(Part {
                slot: Slot::Find,
                text: find.clone(),
            });
        }
        if self.visual {
            parts.push(Part {
                slot: Slot::Visual,
                text: "VISUAL".to_owned(),
            });
        }
        if self.writing {
            parts.push(Part {
                slot: Slot::Writing,
                text: "WRITING".to_owned(),
            });
        }
        if self.warnings > 0 {
            let plural = if self.warnings == 1 { "" } else { "s" };
            parts.push(Part {
                slot: Slot::Warnings,
                text: format!("⚠ {} warning{plural}", self.warnings),
            });
        }
        parts.push(Part {
            slot: Slot::Sort,
            text: format!("sort {}", self.sort),
        });
        parts.push(Part {
            slot: Slot::Focus,
            text: format!("focus {}", self.focus),
        });
        parts
    }

    /// What one elision step does to `part`, given the bar is `over` cells too
    /// wide: its shorter text, or `None` to drop it.
    fn shrink(&self, part: &Part, over: usize) -> Option<String> {
        let target = width(&part.text).saturating_sub(over);
        match part.slot {
            Slot::Sort | Slot::Focus => None,
            Slot::Song => (target >= SONG_MIN).then(|| fit(&part.text, target)),
            // From the left: the end of a path is the album, the start is the
            // genre everybody already knows.
            Slot::Path => (target >= PATH_MIN).then(|| fit_end(&part.text, target)),
            Slot::Warnings => Some(format!("⚠{}", self.warnings)),
            Slot::Marked => Some(format!("{}m", self.marked)),
            Slot::Pending => Some(format!("{}p", self.pending)),
            Slot::Filter | Slot::Find => Some(fit(&part.text, target.max(FILTER_MIN))),
            Slot::Mpd => Some(self.mpd.glyph().to_owned()),
            Slot::Visual | Slot::Writing => Some(part.text.clone()),
        }
    }

    /// How a part is drawn. The things that change what a key does are the
    /// things that are not grey.
    fn style(&self, slot: Slot) -> Style {
        match slot {
            Slot::Mpd => Style::new().fg(self.mpd.color()),
            Slot::Filter | Slot::Find => Style::new().fg(Color::Magenta),
            Slot::Warnings => Style::new().fg(Color::Yellow),
            Slot::Writing => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            Slot::Visual => Style::new().add_modifier(Modifier::BOLD),
            Slot::Pending if self.pending > 0 => Style::new().fg(Color::Yellow),
            _ => Style::new().fg(Color::DarkGray),
        }
    }
}

/// Cells taken by some parts drawn with `sep` cells between each two.
fn joined<'a>(texts: impl Iterator<Item = &'a str>, sep: usize) -> usize {
    let (cells, count) = texts.fold((0usize, 0usize), |(cells, count), text| {
        (cells + width(text), count + 1)
    });
    cells + sep * count.saturating_sub(1)
}

/// The left half's width: its parts, with `SEP` between them.
fn parts_width(parts: &[Part]) -> usize {
    joined(parts.iter().map(|part| part.text.as_str()), SEP_W)
}

/// The right half's width: the indicator and the song, a space apart.
fn right_width(parts: &[Part]) -> usize {
    joined(parts.iter().map(|part| part.text.as_str()), 1)
}

/// The whole bar's width: both halves, and at least a separator's worth of gap
/// between them so the two never run into each other.
fn total(left: &[Part], right: &[Part]) -> usize {
    let gap = if left.is_empty() { 0 } else { SEP_W };
    parts_width(left) + gap + right_width(right)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::tui::msg::MpdState;

    /// Everything on at once, with a long path and a long song: the worst case.
    fn busy() -> Status {
        Status {
            path: "hiphop/MF DOOM - Mm..Food (2004) [320]".to_owned(),
            marked: 14,
            pending: 3,
            filter: Some("mp3".to_owned()),
            find: None,
            visual: true,
            writing: true,
            warnings: 2,
            sort: "name".to_owned(),
            focus: "files",
            mpd: Light::Connected {
                play: PlayState::Play,
                song: Some("03 ノスタルジア.mp3".to_owned()),
            },
        }
    }

    fn slots(status: &Status, cells: usize) -> HashSet<Slot> {
        let (left, right) = status.layout(cells);
        left.iter().chain(&right).map(|(slot, _)| *slot).collect()
    }

    fn drawn(status: &Status, cells: usize) -> String {
        status
            .line(cells)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn at_full_width_everything_is_there() {
        let status = busy();
        let text = drawn(&status, 200);
        for part in [
            "hiphop/MF DOOM - Mm..Food (2004) [320]",
            "14 marked",
            "3 pending",
            "filter `mp3`",
            "VISUAL",
            "WRITING",
            "⚠ 2 warnings",
            "sort name",
            "focus files",
            "● playing 03 ノスタルジア.mp3",
        ] {
            assert!(text.contains(part), "{part:?} missing from {text:?}");
        }
        assert_eq!(width(&text), 200, "the gap pads the bar to the full row");
        assert!(text.trim_end().ends_with(".mp3"), "MPD is on the right");
    }

    #[test]
    fn the_bar_is_never_wider_than_the_row_at_any_width() {
        for status in [
            busy(),
            Status {
                find: Some("find `ext:mp3` (7)".to_owned()),
                ..busy()
            },
        ] {
            for cells in 1..=200 {
                let text = drawn(&status, cells);
                assert!(
                    width(&text) <= cells,
                    "{cells} cells: {text:?} is {} wide",
                    width(&text)
                );
            }
        }
    }

    #[test]
    fn at_sixty_columns_the_parts_that_matter_are_all_still_there() {
        // The floor the shell draws at, and the width the task names.
        let status = busy();
        let text = drawn(&status, 60);
        assert!(text.contains("14m"), "{text}");
        assert!(text.contains("3p"), "{text}");
        assert!(
            text.contains("mp3"),
            "the filter is never invisible: {text}"
        );
        assert!(text.contains("VISUAL"), "{text}");
        assert!(text.contains("WRITING"), "{text}");
        assert!(text.contains('⚠'), "{text}");
        assert!(text.contains('●'), "{text}");

        // And with nothing unusual going on, sixty columns has room for the
        // words as well as the numbers.
        let quiet = Status {
            path: "/".to_owned(),
            marked: 0,
            pending: 0,
            filter: None,
            find: None,
            visual: false,
            writing: false,
            warnings: 0,
            sort: "name".to_owned(),
            focus: "tree",
            mpd: Light::Offline,
        };
        let text = drawn(&quiet, 60);
        for part in ["/", "0 marked", "0 pending", "○ offline"] {
            assert!(text.contains(part), "{part:?} missing from {text:?}");
        }
    }

    #[test]
    fn parts_go_in_the_declared_order_and_do_not_come_back() {
        // Narrowing one column at a time, every slot that disappears must be
        // the next droppable one in `ELISION` — and a slot gone at one width is
        // gone at every narrower width, so a bar never flickers between two
        // layouts as a terminal is dragged narrower.
        let status = busy();
        let droppable = [Slot::Sort, Slot::Focus, Slot::Song, Slot::Path];
        let mut gone: Vec<Slot> = Vec::new();
        let mut was = slots(&status, 200);
        for cells in (60..200).rev() {
            let now = slots(&status, cells);
            assert!(
                now.is_subset(&was),
                "{cells}: {now:?} is not within {was:?}"
            );
            for slot in was.difference(&now) {
                gone.push(*slot);
            }
            was = now;
        }
        assert_eq!(gone, droppable, "dropped in this order, and nothing else");
    }

    #[test]
    fn a_shortened_path_keeps_its_end() {
        let status = busy();
        let found = (60..200).find_map(|cells| {
            let (left, _) = status.layout(cells);
            left.into_iter()
                .find(|(slot, text)| *slot == Slot::Path && text.starts_with('…'))
                .map(|(_, text)| text)
        });
        let path = found.expect("some width shortens the path rather than dropping it");
        assert!(path.ends_with("[320]"), "{path}");
    }

    #[test]
    fn the_separator_is_measured_in_cells() {
        assert_eq!(width(SEP), SEP_W);
    }

    #[test]
    fn the_light_says_which_of_the_three_states_mpd_is_in() {
        let snapshot = |state: Option<MpdState>, enabled: bool| MpdSnapshot {
            state,
            enabled,
            problem: None,
            queue: None,
        };
        let state = |play: PlayState, updating: bool| MpdState {
            play_state: play,
            song: Some("hiphop/MF DOOM/01 Doomsday.mp3".to_owned()),
            updating,
        };

        assert_eq!(Light::of(None), Light::Unknown);
        assert_eq!(Light::of(Some(&snapshot(None, true))).label(), "○ offline");
        assert_eq!(Light::of(Some(&snapshot(None, false))).label(), "· mpd off");
        assert_eq!(
            Light::of(Some(&snapshot(Some(state(PlayState::Stop, true)), true))).label(),
            "◐ updating"
        );
        let stopped = Light::of(Some(&snapshot(Some(state(PlayState::Stop, false)), true)));
        assert_eq!(stopped.to_string(), "● connected", "no song when stopped");
        let playing = Light::of(Some(&snapshot(Some(state(PlayState::Play, false)), true)));
        assert_eq!(playing.to_string(), "● playing 01 Doomsday.mp3");
    }
}
