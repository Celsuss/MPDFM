//! The pending view: what is about to happen to the library, and the `c` that
//! says yes.
//!
//! ```text
//! ┌ pending ──────────────────────────────────────────────────────────────┐
//! │ PENDING (3 ops)                                                       │
//! │ MOVE   hiphop/MF DOOM - Mm Food → hiphop/MF DOOM/2004 - Mm..Food      │
//! │        14 audio + 3 aux files, 61.2 MB, 2 playlists                   │
//! │ DELETE hiphop/MF DOOM - Mm Food/folder.nfo                            │
//! │        1 aux file, 2.1 kB                                             │
//! │ TAG    genre = "Hip Hop"                                    14 files  │
//! │                                                                       │
//! │ Playlists                                                             │
//! │ ▾ Coding flow.m3u        3 lines rewritten                            │
//! │     - hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3                        │
//! │     + hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3                  │
//! │ ▸ Hip hop.m3u            1 line rewritten                             │
//! │ ▾ MPD saved queue        2 lines rewritten                            │
//! │                                                                       │
//! │ ! 1 file is in MPD's current queue and will need a requeue             │
//! └ c commit · dd drop · x discard · enter expand · esc back ─────────────┘
//! ```
//!
//! # One renderer, two front-ends
//!
//! Every line above that is not a diff comes from
//! [`Effects::lines`][mpdfm_core::ops::Effects::lines], which is
//! [`Effects::render`][mpdfm_core::ops::Effects::render] — the string
//! `mpdfm move --dry-run` prints — in the shape a cursor can be put on. So the
//! anti-divergence check the task asks for is a real assertion and not a wish:
//! fold everything away and this view's body *is* the CLI's output, character
//! for character, which `the_body_is_what_the_cli_prints` proves.
//!
//! The one deliberate difference is the two cells in front of a playlist's name:
//! the preview indents them and this draws its `▸` / `▾` there, in the same
//! columns rather than in front of them.
//!
//! # Folding, not truncating
//!
//! Two things unfold, and both are something the user has to be able to see in
//! full:
//!
//! - **a playlist**, into every line that changes — `-` what it says, `+` what it
//!   will say (`widgets::diff`). Not a count: a count is a claim about a file the
//!   user cannot see, and this is the screen that has to be believed;
//! - **a refused operation**, into the reasons it is refused.
//!
//! Nothing is ever dropped to make it fit. The cursor can be on a diff line as
//! easily as on an operation, so a two-thousand-line plan is *scrolled*, which is
//! the task's own pitfall: a hidden change is what this program exists to
//! prevent.
//!
//! Refused operations unfold **by default**, because a plan that cannot be
//! committed is only useful to somebody who can see why.
//!
//! # What this view does not own
//!
//! The plan. [`App`][crate::tui::app::App] holds that, which is what makes
//! staged operations survive `esc` and a resize, and it re-validates it after
//! every change — dropping an operation can make a conflict appear as easily as
//! disappear, so this view never patches [`Effects`]; it is handed a new one
//! ([`Pending::revalidated`]).
//!
//! It does not own the commit either. That is a worker (`tui::work`), because a
//! two-thousand-file organize is not instant, and what comes back is a
//! [`Report`] this view shows until it is dismissed.

use std::collections::BTreeSet;

use mpdfm_core::journal::record::Record;
use mpdfm_core::ops::{Effects, LineKind, PLAYLIST_INDENT, PreviewLine};
use mpdfm_core::playlist::rewrite::LineEdit;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::widgets::diff;
use crate::tui::widgets::{fit, width};

/// The width the renderings that only the *structure* is read from are made at.
///
/// Any width would do: [`Effects::lines`] produces the same number of lines, in
/// the same order, with the same kinds, whatever width it is given — only the
/// text changes (see [`PreviewLine`], and the core test that pins it). 80 is the
/// width the CLI lays out for.
const NOMINAL: usize = 80;

/// The marker on a row whose detail is unfolded.
const OPEN: &str = "▾ ";

/// The marker on a row that has something to unfold.
const SHUT: &str = "▸ ";

/// What MPD's saved queue is called in the preview, and therefore the key its
/// fold is remembered under.
const QUEUE: &str = "MPD saved queue";

// ---------------------------------------------------------------------------

/// The pending view's whole state.
///
/// Everything else about a staged plan — the operations, whether a transaction is
/// running — belongs to the shell. What is here is what a *view* owns: where the
/// cursor is, what is unfolded, and what the last commit said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// What the staged plan would do. Replaced wholesale on every change; never
    /// patched.
    effects: Effects,
    /// Which selectable row the cursor is on.
    cursor: usize,
    /// What is unfolded.
    open: BTreeSet<Fold>,
    /// The first row on screen, as the last frame left it.
    scroll: usize,
    /// What a finished transaction left behind, until it is dismissed.
    report: Option<Report>,
}

/// Something that can be unfolded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Fold {
    /// A playlist's line diff, by the name the preview gives it — which is
    /// stable across a re-validation, where an index into
    /// [`Effects::playlist_edits`] is not.
    Playlist(String),
    /// Why an operation is refused, by its index in the plan.
    Reasons(usize),
}

/// Something the cursor can be on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spot {
    /// An operation's row: one operation, or — for a tag edit, which the preview
    /// collapses by field — every file's edit of one field.
    Op {
        /// What `dd` would take off the plan.
        ops: Vec<usize>,
        /// Whether a conflict names any of them.
        refused: bool,
    },
    /// A playlist's row, or MPD's saved queue.
    Playlist {
        /// The name the preview gives it.
        label: String,
    },
    /// A line to read: a conflict, a warning, a diff.
    Text,
}

impl Spot {
    /// What this row can unfold, if anything.
    fn fold(&self) -> Option<Fold> {
        match self {
            Self::Playlist { label } => Some(Fold::Playlist(label.clone())),
            Self::Op { ops, refused: true } => ops.first().copied().map(Fold::Reasons),
            Self::Op { .. } | Self::Text => None,
        }
    }
}

/// One drawn row: the line, what it is about, and which selectable row it is —
/// if the cursor can be on it at all.
struct Row {
    line: Line<'static>,
    what: Spot,
    spot: Option<usize>,
}

// ---------------------------------------------------------------------------

/// What a finished transaction left behind.
///
/// Held by the view rather than flashed as a message, because every part of it
/// is something the user may want to act on: the id `undo` takes, what did not
/// happen, and the command that puts a half-finished transaction back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// It committed.
    Done {
        /// The transaction id — what `mpdfm undo <txid>` takes.
        txid: String,
        /// Its one-line summary, as the journal records it.
        headline: String,
        /// What MPDFM asked MPD to do about it.
        mpd: Mpd,
        /// Everything the commit mentioned that did not stop it.
        warnings: Vec<String>,
    },

    /// It did not commit, or it stopped partway through.
    ///
    /// The message is core's, whole and never truncated: it says which step
    /// stopped it and, when there is something on disk to put back, the
    /// `mpdfm recover <txid>` that does it.
    Failed {
        /// What went wrong.
        message: String,
    },

    /// It was called off before anything was changed.
    Cancelled,
}

/// What the commit asked MPD to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mpd {
    /// The daemon was asked to rescan this many directories.
    Queued {
        /// How many.
        dirs: usize,
    },
    /// It was asked and could not be told. The library and the playlists are
    /// consistent either way; MPD's own index is what is behind.
    Failed {
        /// Why not.
        why: String,
    },
    /// Nobody asked: no daemon, or the configuration says not to.
    NotAsked,
}

impl Mpd {
    /// What a committed transaction's record says about MPD.
    #[must_use]
    pub fn of(record: &Record) -> Self {
        match (&record.mpd_update_failed, record.mpd_update_requested) {
            (Some(why), _) => Self::Failed { why: why.clone() },
            (None, true) => Self::Queued {
                dirs: record.mpd_update_dirs.len(),
            },
            (None, false) => Self::NotAsked,
        }
    }

    /// The one line the report shows.
    fn line(&self) -> String {
        match self {
            Self::Queued { dirs } => {
                let plural = if *dirs == 1 { "y" } else { "ies" };
                format!("MPD was asked to rescan {dirs} director{plural}")
            }
            Self::Failed { why } => format!("MPD was not told to rescan: {why}"),
            Self::NotAsked => "MPD was not asked to rescan".to_owned(),
        }
    }
}

/// The keys this view's own text names, read off the live keymap.
///
/// The same arrangement the tag editor uses, and for the same reason: a user who
/// has rebound `c` is told the key they chose, and one who has unbound it is not
/// told about a key that does nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    /// Commit everything staged.
    pub commit: Option<String>,
    /// Take one operation off the plan.
    pub drop: Option<String>,
    /// Throw the whole plan away.
    pub discard: Option<String>,
    /// Reverse the last transaction.
    pub undo: Option<String>,
    /// Unfold the row under the cursor.
    pub expand: Option<String>,
    /// Leave the view.
    pub back: Option<String>,
}

impl Hints {
    /// `key label`, or nothing when the action is not bound to anything.
    fn hint(key: Option<&String>, label: &str) -> Option<String> {
        key.map(|key| format!("{key} {label}"))
    }
}

// ---------------------------------------------------------------------------

impl Pending {
    /// The view over what a plan would do.
    ///
    /// Opens with every refused operation unfolded and the cursor on the first
    /// of them: a plan that cannot be committed is only useful to somebody who
    /// can see why not.
    #[must_use]
    pub fn new(effects: Effects) -> Self {
        let mut view = Self {
            effects,
            cursor: 0,
            open: BTreeSet::new(),
            scroll: 0,
            report: None,
        };
        view.open_the_refusals();
        view.cursor = view.first_refusal().unwrap_or(0);
        view
    }

    /// What this view is about.
    #[must_use]
    pub fn effects(&self) -> &Effects {
        &self.effects
    }

    /// Whether `c` may do anything.
    #[must_use]
    pub fn is_committable(&self) -> bool {
        self.report.is_none() && self.effects.is_committable()
    }

    /// The plan changed, so here is what it would do now.
    ///
    /// The cursor is kept on the same *operation* where that operation is still
    /// there, rather than on the same row: dropping the third of five leaves the
    /// cursor where the user was looking. Folds are remembered by name, so a
    /// playlist that is still affected is still unfolded.
    ///
    /// The report is left alone: a commit's own rescan comes straight back
    /// through here, and the txid it produced must not vanish out from under the
    /// `u` that was offered with it. What clears a report is leaving, or staging
    /// something new ([`Pending::dismiss`]).
    pub fn revalidated(&mut self, effects: Effects) {
        let was = self.spot();
        self.effects = effects;
        self.open_the_refusals();

        let spots = self.spots();
        self.cursor = was
            .and_then(|was| spots.iter().position(|spot| *spot == was))
            .or_else(|| self.first_refusal())
            .unwrap_or(self.cursor)
            .min(spots.len().saturating_sub(1));
    }

    /// Forget the last transaction's report: this view is about the plan again.
    pub fn dismiss(&mut self) {
        self.report = None;
    }

    /// Unfold every refused operation, which is what [`Pending::new`] and
    /// [`Pending::revalidated`] both want.
    fn open_the_refusals(&mut self) {
        for spot in self.spots() {
            if matches!(spot, Spot::Op { refused: true, .. })
                && let Some(fold) = spot.fold()
            {
                self.open.insert(fold);
            }
        }
    }

    /// The first row a conflict refuses, which is where the cursor belongs when
    /// there is one.
    fn first_refusal(&self) -> Option<usize> {
        self.spots()
            .iter()
            .position(|spot| matches!(spot, Spot::Op { refused: true, .. }))
    }

    /// What a finished transaction left behind. Shown until it is dismissed.
    pub fn finished(&mut self, report: Report) {
        self.report = Some(report);
        self.cursor = 0;
        self.scroll = 0;
    }

    /// The report, if a transaction has finished and nobody has left yet.
    #[must_use]
    pub fn report(&self) -> Option<&Report> {
        self.report.as_ref()
    }

    // -- the cursor --------------------------------------------------------

    /// What the cursor is on.
    fn spot(&self) -> Option<Spot> {
        self.spots().get(self.cursor).cloned()
    }

    /// Every row the cursor can be on, in the order they are drawn.
    ///
    /// Derived from the shared renderer's line kinds, which do not depend on the
    /// width — so navigating does not need to know how wide the pane is.
    fn spots(&self) -> Vec<Spot> {
        Self::spots_of(&self.layout())
    }

    /// Every drawn row, at the width only the structure is read from.
    ///
    /// The one call the keys make, and they make it once each: laying out a
    /// four-hundred-operation plan is cheap but not free, and the cursor needs
    /// both what it can land on and where those rows are.
    fn layout(&self) -> Vec<Row> {
        self.rows_from(&self.effects.lines(NOMINAL), NOMINAL)
    }

    /// The selectable rows of a layout, in the order they are drawn.
    fn spots_of(rows: &[Row]) -> Vec<Spot> {
        rows.iter()
            .filter(|row| row.spot.is_some())
            .map(|row| row.what.clone())
            .collect()
    }

    /// The staged operations `dd` would take off the plan.
    ///
    /// Empty when the cursor is not on an operation — a diff line is something
    /// to read, not something to drop.
    #[must_use]
    pub fn selected_ops(&self) -> Vec<usize> {
        match self.spot() {
            Some(Spot::Op { ops, .. }) => ops,
            _ => Vec::new(),
        }
    }

    /// Move the cursor, stopping at both ends.
    ///
    /// `rows` is how many rows the pane has, for the half-page jumps and to keep
    /// the cursor on screen.
    pub fn move_cursor(&mut self, delta: isize, rows: usize) -> bool {
        self.go(self.cursor.saturating_add_signed(delta), rows)
    }

    /// Put the cursor on a row, by index, clamped to what there is.
    pub fn set_cursor(&mut self, row: usize, rows: usize) -> bool {
        self.go(row, rows)
    }

    /// Put the cursor on `target`, clamped, and scroll to keep it on screen.
    ///
    /// One layout for both halves of that: which row is the last one, and where
    /// the row the cursor lands on is.
    fn go(&mut self, target: usize, rows: usize) -> bool {
        let all = self.layout();
        let spots = all.iter().filter(|row| row.spot.is_some()).count();
        let target = target.min(spots.saturating_sub(1));
        let moved = target != self.cursor;
        self.cursor = target;
        self.follow(&all, rows);
        moved
    }

    /// Where the cursor is.
    ///
    /// Only the tests ask: on screen it is the reversed row, and every key that
    /// moves it says whether it moved.
    #[cfg(test)]
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Keep the cursor's row on screen, given the layout and how many rows the
    /// pane has.
    fn follow(&mut self, drawn: &[Row], rows: usize) {
        let rows = rows.max(1);
        let Some(at) = drawn.iter().position(|row| row.spot == Some(self.cursor)) else {
            return;
        };
        if at < self.scroll {
            self.scroll = at;
        } else if at >= self.scroll + rows {
            self.scroll = at + 1 - rows;
        }
        self.scroll = self.scroll.min(drawn.len().saturating_sub(rows));
    }

    // -- unfolding ---------------------------------------------------------

    /// Unfold or fold the row under the cursor. Returns whether anything moved.
    pub fn toggle(&mut self) -> bool {
        let Some(fold) = self.spot().and_then(|spot| spot.fold()) else {
            return false;
        };
        if !self.open.remove(&fold) {
            self.open.insert(fold);
        }
        true
    }

    /// Unfold the row under the cursor, if it has anything folded.
    pub fn expand(&mut self) -> bool {
        let Some(fold) = self.spot().and_then(|spot| spot.fold()) else {
            return false;
        };
        self.open.insert(fold)
    }

    /// Fold the row under the cursor back up.
    pub fn collapse(&mut self) -> bool {
        let Some(fold) = self.spot().and_then(|spot| spot.fold()) else {
            return false;
        };
        self.open.remove(&fold)
    }

    /// Whether the cursor is on something with a fold at all, for the footer.
    #[must_use]
    pub fn can_expand(&self) -> bool {
        self.spot().is_some_and(|spot| spot.fold().is_some())
    }

    // -- drawing -----------------------------------------------------------

    /// The pane's title: what state the plan is in.
    #[must_use]
    pub fn title(&self) -> String {
        match &self.report {
            Some(Report::Done { txid, .. }) => format!(" committed — {txid} "),
            Some(Report::Failed { .. }) => " commit failed ".to_owned(),
            Some(Report::Cancelled) => " commit cancelled ".to_owned(),
            None if !self.effects.conflicts.is_empty() => " pending — refused ".to_owned(),
            None => " pending ".to_owned(),
        }
    }

    /// The bottom line: what can be done from here, in the keys that do it.
    #[must_use]
    pub fn footer(&self, hints: &Hints) -> String {
        let parts: Vec<String> = match &self.report {
            // The one the task asks for by name: after a commit, `u` is offered
            // right there rather than remembered from the browser.
            Some(Report::Done { .. }) => [
                Hints::hint(hints.undo.as_ref(), "undo this transaction"),
                Hints::hint(hints.back.as_ref(), "back"),
            ]
            .into_iter()
            .flatten()
            .collect(),
            Some(Report::Failed { .. } | Report::Cancelled) => {
                [Hints::hint(hints.back.as_ref(), "back")]
                    .into_iter()
                    .flatten()
                    .collect()
            }
            None => {
                let commit = if self.effects.conflicts.is_empty() {
                    Hints::hint(hints.commit.as_ref(), "commit")
                } else {
                    // Not a key that does nothing: the reason is unfolded under
                    // the operation it is about.
                    Some(format!(
                        "refused — {} conflict(s)",
                        self.effects.conflicts.len()
                    ))
                };
                [
                    commit,
                    Hints::hint(hints.drop.as_ref(), "drop op"),
                    Hints::hint(hints.discard.as_ref(), "discard"),
                    self.can_expand()
                        .then(|| Hints::hint(hints.expand.as_ref(), "expand"))
                        .flatten(),
                    Hints::hint(hints.back.as_ref(), "back"),
                ]
                .into_iter()
                .flatten()
                .collect()
            }
        };
        format!(" {} ", parts.join(" · "))
    }

    /// The body, `rows` rows of it from wherever it is scrolled to.
    ///
    /// A pure function of the state: everything that moves the cursor or the
    /// scroll happens in the methods above.
    #[must_use]
    pub fn lines(&self, cells: usize, rows: usize) -> Vec<Line<'static>> {
        if rows == 0 {
            return Vec::new();
        }
        let all = self.rows_from(&self.effects.lines(cells), cells);
        let start = self.window(&all, rows);
        all.into_iter()
            .skip(start)
            .take(rows)
            .map(|row| row.line)
            .collect()
    }

    /// The body as plain text, every fold shut.
    ///
    /// What the anti-divergence test compares with `mpdfm move --dry-run`: with
    /// nothing unfolded this is [`Effects::render`], give or take the two cells a
    /// playlist's `▸` is drawn in. A caller that wants what is on screen wants
    /// [`Pending::lines`].
    ///
    /// Test-only, because that comparison is the only thing it is for: what a
    /// user sees comes from [`Pending::lines`], folds, styles and all.
    #[cfg(test)]
    #[must_use]
    pub fn body_text(&self, cells: usize) -> String {
        let shut = Self {
            effects: self.effects.clone(),
            cursor: self.cursor,
            open: BTreeSet::new(),
            scroll: 0,
            report: None,
        };
        shut.rows_from(&shut.effects.lines(cells), cells)
            .iter()
            .map(|row| row.line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Which row the window starts at: where it was scrolled to, moved as little
    /// as it takes to keep the cursor's row on screen.
    fn window(&self, all: &[Row], rows: usize) -> usize {
        let rows = rows.max(1);
        let last = all.len().saturating_sub(rows);
        let mut start = self.scroll.min(last);
        if let Some(at) = all.iter().position(|row| row.spot == Some(self.cursor)) {
            if at < start {
                start = at;
            } else if at >= start + rows {
                start = at + 1 - rows;
            }
        }
        start
    }

    /// Every drawn row: the preview's own lines, with what is unfolded between
    /// them.
    ///
    /// The one place the layout is decided, so that what the cursor counts and
    /// what the frame draws cannot disagree.
    fn rows_from(&self, lines: &[PreviewLine], cells: usize) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::with_capacity(lines.len());
        let mut spot = 0;

        if let Some(report) = &self.report {
            rows.extend(report_rows(report, cells).into_iter().map(|line| Row {
                line,
                what: Spot::Text,
                spot: None,
            }));
        }

        // An operation's reasons go under its *cost* line and not between the
        // two, so the row the user is reading stays the shape it is everywhere
        // else. Which means they are held for one line.
        let mut held: Option<Vec<usize>> = None;

        for line in lines {
            match &line.kind {
                LineKind::Op { op, refused } => {
                    rows.push(self.row(
                        line,
                        cells,
                        Some(spot),
                        &Spot::Op {
                            ops: vec![*op],
                            refused: *refused,
                        },
                    ));
                    spot += 1;
                    if *refused && self.open.contains(&Fold::Reasons(*op)) {
                        held = Some(vec![*op]);
                    }
                }
                LineKind::Tag { ops, refused } => {
                    rows.push(self.row(
                        line,
                        cells,
                        Some(spot),
                        &Spot::Op {
                            ops: ops.clone(),
                            refused: *refused,
                        },
                    ));
                    spot += 1;
                    // A tag row has no cost line under it, so its reasons go
                    // straight after it.
                    if *refused
                        && ops
                            .first()
                            .is_some_and(|op| self.open.contains(&Fold::Reasons(*op)))
                    {
                        rows.extend(self.reasons(ops, cells));
                    }
                }
                LineKind::Playlist { at } => {
                    let label = self.label(*at);
                    let open = self.open.contains(&Fold::Playlist(label.clone()));
                    rows.push(self.row(line, cells, Some(spot), &Spot::Playlist { label }));
                    spot += 1;
                    if open {
                        for row in diff::lines(self.edits(*at), cells) {
                            // Selectable, which is what makes a long diff
                            // scrollable: the cursor walks into it rather than
                            // the view holding a second scroll of its own.
                            let on = spot == self.cursor && self.report.is_none();
                            let line = if on {
                                row.style(Style::new().add_modifier(Modifier::REVERSED))
                            } else {
                                row
                            };
                            rows.push(Row {
                                line,
                                what: Spot::Text,
                                spot: Some(spot),
                            });
                            spot += 1;
                        }
                    }
                }
                LineKind::Conflict { .. } | LineKind::Warning { .. } => {
                    rows.push(self.row(line, cells, Some(spot), &Spot::Text));
                    spot += 1;
                }
                LineKind::Detail { .. } => {
                    rows.push(self.row(line, cells, None, &Spot::Text));
                    if let Some(ops) = held.take() {
                        rows.extend(self.reasons(&ops, cells));
                    }
                }
                LineKind::Header | LineKind::Section | LineKind::Note => {
                    rows.push(self.row(line, cells, None, &Spot::Text));
                }
            }
        }
        rows
    }

    /// One row, styled for what it is and marked if the cursor is on it.
    fn row(&self, line: &PreviewLine, cells: usize, spot: Option<usize>, what: &Spot) -> Row {
        let text = match (&line.kind, what) {
            // The expansion marker goes *in* the preview's own indent, so the
            // row is the same width and the same string as the CLI's.
            (LineKind::Playlist { .. }, Spot::Playlist { label }) => {
                let marker = if self.open.contains(&Fold::Playlist(label.clone())) {
                    OPEN
                } else {
                    SHUT
                };
                let rest: String = line.text.chars().skip(PLAYLIST_INDENT).collect();
                format!("{marker}{rest}")
            }
            _ => line.text.clone(),
        };

        let mut styled = Line::from(Span::styled(fit(&text, cells), style_of(&line.kind)));
        if spot.is_some() && spot == Some(self.cursor) && self.report.is_none() {
            styled = styled.style(Style::new().add_modifier(Modifier::REVERSED));
        }
        Row {
            line: styled,
            what: what.clone(),
            spot,
        }
    }

    /// Why these operations are refused, unfolded under the row they are about.
    fn reasons(&self, ops: &[usize], cells: usize) -> Vec<Row> {
        self.effects
            .conflicts
            .iter()
            .filter(|conflict| conflict.ops().iter().any(|op| ops.contains(op)))
            .map(|conflict| Row {
                what: Spot::Text,
                line: Line::from(vec![
                    Span::raw(" ".repeat(diff::INDENT.min(cells))),
                    Span::styled(
                        fit(
                            &format!("x {}", conflict.message()),
                            cells.saturating_sub(diff::INDENT),
                        ),
                        Style::new().fg(Color::Red),
                    ),
                ]),
                spot: None,
            })
            .collect()
    }

    /// What the preview calls this playlist. `None` is MPD's saved queue.
    fn label(&self, at: Option<usize>) -> String {
        at.and_then(|at| self.effects.playlist_edits.get(at))
            .map_or_else(|| QUEUE.to_owned(), |edit| edit.file_name.clone())
    }

    /// The lines one playlist row unfolds into.
    fn edits(&self, at: Option<usize>) -> &[LineEdit] {
        at.and_then(|at| self.effects.playlist_edits.get(at))
            .map_or(self.effects.state_edits.as_slice(), |edit| {
                edit.line_edits.as_slice()
            })
    }
}

// ---------------------------------------------------------------------------

/// What a line of the preview is drawn in.
fn style_of(kind: &LineKind) -> Style {
    match kind {
        LineKind::Header => Style::new().add_modifier(Modifier::BOLD),
        LineKind::Section => Style::new().fg(Color::DarkGray),
        LineKind::Op { refused: true, .. } | LineKind::Tag { refused: true, .. } => {
            Style::new().fg(Color::Red)
        }
        LineKind::Op { .. } | LineKind::Tag { .. } => Style::new(),
        LineKind::Detail { .. } | LineKind::Note => Style::new().fg(Color::DarkGray),
        // A conflict is the one thing on this screen that means nothing will
        // happen, and it is red for the same reason commit is disabled.
        LineKind::Conflict { .. } => Style::new().fg(Color::Red),
        LineKind::Warning { .. } => Style::new().fg(Color::Yellow),
        LineKind::Playlist { .. } => Style::new().fg(Color::Cyan),
    }
}

/// The report at the top of the view, above whatever is left of the plan.
fn report_rows(report: &Report, cells: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut say = |text: String, style: Style| {
        lines.push(Line::from(Span::styled(fit(&text, cells), style)));
    };

    match report {
        Report::Done {
            txid,
            headline,
            mpd,
            warnings,
        } => {
            say(
                format!("COMMITTED {txid}"),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            );
            say(headline.clone(), Style::new());
            say(mpd.line(), Style::new().fg(Color::DarkGray));
            for warning in warnings {
                say(format!("! {warning}"), Style::new().fg(Color::Yellow));
            }
        }
        Report::Failed { message } => {
            say(
                "COMMIT FAILED".to_owned(),
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            );
            // Core's message, whole: it says which step stopped the transaction
            // and the `mpdfm recover` that puts it back. Wrapped by hand rather
            // than truncated, because the recovery command is on the last line
            // of it and that is the half a user needs.
            for line in wrap(message, cells) {
                say(line, Style::new().fg(Color::Red));
            }
        }
        Report::Cancelled => {
            say(
                "COMMIT CANCELLED".to_owned(),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            );
            say(
                "nothing was changed, and the plan is still staged".to_owned(),
                Style::new().fg(Color::DarkGray),
            );
        }
    }
    lines.push(Line::raw(""));
    lines
}

/// Break text into lines that fit, on spaces where there are any.
///
/// Only the failure report needs this: everything else on this screen is a row
/// whose columns line up, and reflowing one of those would be worse than
/// shortening it. A recovery command, on the other hand, has to be readable in
/// full.
fn wrap(text: &str, cells: usize) -> Vec<String> {
    let cells = cells.max(8);
    let mut out = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if width(&candidate) > cells && !line.is_empty() {
                out.push(std::mem::take(&mut line));
                line = word.to_owned();
            } else {
                line = candidate;
            }
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use mpdfm_core::library::Library;
    use mpdfm_core::ops::{Operation, Plan};
    use mpdfm_core::paths::RelPath;
    use mpdfm_core::playlist::PlaylistIndex;
    use mpdfm_core::testing::{Fixture, names};

    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::parse(path).unwrap_or_else(|err| panic!("{path:?}: {err}"))
    }

    /// What a plan would do, worked out against a real fixture.
    ///
    /// A fixture and not a hand-built `Effects`: the whole point of this view is
    /// that it shows what commit will do, and an `Effects` written out by hand
    /// is one nothing will ever commit.
    fn effects_of(fx: &Fixture, plan: &Plan) -> Effects {
        let library = Library::scan(fx.music_dir()).expect("the fixture scans");
        let (index, _) = PlaylistIndex::load(fx.playlist_dir());
        plan.validate(&library, &index, &fx.config())
    }

    /// The album move two playlists and MPD's saved queue all name.
    fn album_move() -> Plan {
        Plan::of(vec![Operation::MoveDir {
            from: rel(names::MF_DOOM_ALBUM),
            to: rel("hiphop/MF DOOM/Mm..Food (2004)"),
        }])
    }

    /// A plan nothing will commit: the destination is occupied, and MPDFM never
    /// overwrites.
    fn refused_move() -> Plan {
        Plan::of(vec![Operation::MoveDir {
            from: rel(names::MF_DOOM_ALBUM),
            to: rel(names::SNOOP_ALBUM),
        }])
    }

    fn view(plan: &Plan) -> (Fixture, Pending) {
        let fx = Fixture::realistic();
        let effects = effects_of(&fx, plan);
        (fx, Pending::new(effects))
    }

    /// What is on screen, as one string per row.
    fn drawn(view: &Pending, cells: usize, rows: usize) -> Vec<String> {
        view.lines(cells, rows)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Open every fold there is, as a user pressing `enter` down the list would.
    fn open_everything(view: &mut Pending) {
        let mut at = 0;
        // The list grows as folds open, so its length is read again each time
        // rather than once.
        while at < view.spots().len() {
            view.set_cursor(at, 40);
            view.expand();
            at += 1;
        }
    }

    // -- the anti-divergence check -----------------------------------------

    #[test]
    fn the_body_is_what_the_cli_prints() {
        // `docs/tasks/24-pending-view.md`'s anti-divergence criterion, as a
        // string comparison. `mpdfm move --dry-run` prints `Effects::render`
        // (`cli/move.rs`), this view draws `Effects::lines`, and the two are one
        // function — so the only way this fails is if the view starts formatting
        // something itself.
        let (fx, view) = view(&album_move());
        let effects = effects_of(&fx, &album_move());

        for cells in [40, 72, 80, 132] {
            let body = view.body_text(cells);
            // The one permitted difference: the `▸` is drawn in the two columns
            // the preview indents a playlist's name by, so it is put back.
            let folded: String = body
                .lines()
                .map(
                    |line| match line.strip_prefix(SHUT).or_else(|| line.strip_prefix(OPEN)) {
                        Some(rest) => format!("{:PLAYLIST_INDENT$}{rest}", ""),
                        None => line.to_owned(),
                    },
                )
                .collect::<Vec<_>>()
                .join("\n");

            assert_eq!(
                folded,
                effects.render(cells),
                "at {cells} columns the pending view and `--dry-run` disagree"
            );
        }
    }

    #[test]
    fn only_a_playlist_row_is_ever_touched_by_that_normalization() {
        // Keeps the test above honest: if some other row grew a marker, the
        // comparison would be hiding it.
        let (_fx, view) = view(&album_move());
        let marked = view
            .body_text(80)
            .lines()
            .filter(|line| line.starts_with(SHUT) || line.starts_with(OPEN))
            .count();
        assert_eq!(
            marked,
            view.effects().playlist_edits.len() + 1,
            "two playlists and MPD's saved queue, and nothing else"
        );
    }

    // -- the diff ----------------------------------------------------------

    #[test]
    fn a_playlist_unfolds_into_every_line_that_changes() {
        let (_fx, mut view) = view(&album_move());
        let shut = drawn(&view, 80, 60);
        assert!(
            shut.iter().any(|row| row.contains(names::HIP_HOP_PLAYLIST)),
            "{shut:?}"
        );
        assert!(
            !shut.iter().any(|row| row.trim_start().starts_with("- ")),
            "nothing is unfolded to begin with: {shut:?}"
        );

        // Put the cursor on `Hip hop.m3u` and open it.
        let at = view
            .spots()
            .iter()
            .position(
                |spot| matches!(spot, Spot::Playlist { label } if label == names::HIP_HOP_PLAYLIST),
            )
            .expect("the playlist has a row");
        view.set_cursor(at, 60);
        assert!(view.toggle(), "enter unfolds it");

        let open = drawn(&view, 80, 60);
        let olds: Vec<&String> = open
            .iter()
            .filter(|row| row.trim_start().starts_with("- "))
            .collect();
        let news: Vec<&String> = open
            .iter()
            .filter(|row| row.trim_start().starts_with("+ "))
            .collect();

        // Exactly the lines commit will rewrite, old and new, and not a count.
        let edit = view
            .effects()
            .playlist_edits
            .iter()
            .find(|edit| edit.file_name == names::HIP_HOP_PLAYLIST)
            .expect("it is in the plan");
        assert_eq!(olds.len(), edit.line_edits.len(), "{open:?}");
        assert_eq!(news.len(), edit.rewrites(), "{open:?}");
        assert!(
            olds[0].contains("01 Beef Rap.mp3") && news[0].contains("01 Beef Rap.mp3"),
            "{olds:?} {news:?}"
        );
        assert!(news[0].contains("Mm..Food (2004)"), "{news:?}");

        // And folding it again puts it back.
        assert!(view.toggle());
        assert_eq!(drawn(&view, 80, 60), shut);
    }

    #[test]
    fn mpd_s_saved_queue_unfolds_the_same_way_a_playlist_does() {
        let (_fx, mut view) = view(&album_move());
        let at = view
            .spots()
            .iter()
            .position(|spot| matches!(spot, Spot::Playlist { label } if label == QUEUE))
            .expect("the fixture's state file names the album");
        view.set_cursor(at, 60);
        view.expand();

        let rows = drawn(&view, 80, 60);
        let queue = rows
            .iter()
            .position(|row| row.contains(QUEUE))
            .expect("its row is on screen");
        assert!(
            rows[queue + 1].trim_start().starts_with("- "),
            "the line it holds now: {:?}",
            &rows[queue..]
        );
        assert!(
            rows[queue + 2].trim_start().starts_with("+ "),
            "and what it will hold: {:?}",
            &rows[queue..]
        );
    }

    #[test]
    fn a_long_diff_scrolls_rather_than_being_cut_short() {
        // The task's pitfall. The whole album's playlist lines, in a pane with
        // room for six rows: every one of them can be reached, and none of them
        // is dropped.
        let (_fx, mut view) = view(&album_move());
        open_everything(&mut view);

        // Nothing is dropped: one `-` row per changed line, in a body that is
        // taller than any pane.
        let total = view
            .effects()
            .playlist_edits
            .iter()
            .map(|edit| edit.line_edits.len())
            .sum::<usize>()
            + view.effects().state_edits.len();
        let whole = drawn(&view, 80, 1_000);
        assert_eq!(
            whole
                .iter()
                .filter(|row| row.trim_start().starts_with("- "))
                .count(),
            total,
            "{whole:?}"
        );

        // And a pane with room for six of them reaches every one: the cursor
        // walks into the diff, and the window follows it a row at a time.
        let rows = 6;
        view.set_cursor(0, rows);
        assert_eq!(
            drawn(&view, 80, rows),
            whole[..rows],
            "it starts at the top"
        );
        let mut steps = 0;
        while view.move_cursor(1, rows) {
            steps += 1;
            assert!(steps < 1_000, "the cursor should reach the end");
        }
        assert_eq!(
            drawn(&view, 80, rows),
            whole[whole.len() - rows..],
            "and ends at the bottom"
        );
        assert!(
            steps + 1 >= total * 2,
            "every diff row is a row the cursor can be on: {steps} steps"
        );
    }

    // -- conflicts ---------------------------------------------------------

    #[test]
    fn a_refused_operation_opens_with_the_reason_under_it_and_nothing_can_commit() {
        // MPDFM never overwrites, so a move onto something that is already
        // there is refused — with a reason that names what is in the way.
        let (_fx, view) = view(&refused_move());

        assert!(!view.is_committable(), "a refused plan cannot be committed");
        assert!(view.title().contains("refused"), "{}", view.title());

        let rows = drawn(&view, 200, 40);
        let at = rows
            .iter()
            .position(|row| row.starts_with("MOVE"))
            .expect("the operation has a row");
        // Unfolded without being asked, because a plan that will not commit is
        // only useful to somebody who can see why.
        assert!(
            rows[at + 2].contains("already exists"),
            "the reason is under the operation: {rows:?}"
        );
        // And the cursor is on it, so the first thing `dd` would drop is the
        // thing that is wrong.
        assert_eq!(view.cursor(), 0);
        assert_eq!(view.selected_ops(), vec![0]);
    }

    #[test]
    fn the_footer_says_what_can_be_done_and_a_refused_plan_offers_no_commit() {
        let hints = Hints {
            commit: Some("c".to_owned()),
            drop: Some("dd".to_owned()),
            discard: Some("x".to_owned()),
            undo: Some("u".to_owned()),
            expand: Some("enter".to_owned()),
            back: Some("esc".to_owned()),
        };

        let (_fx, ok) = view(&album_move());
        let footer = ok.footer(&hints);
        assert!(footer.contains("c commit"), "{footer}");
        assert!(footer.contains("dd drop op"), "{footer}");

        let refused = view(&refused_move()).1;
        let footer = refused.footer(&hints);
        assert!(!footer.contains("c commit"), "{footer}");
        assert!(footer.contains("refused"), "{footer}");
    }

    // -- what a row stands for ---------------------------------------------

    #[test]
    fn a_tag_row_stands_for_every_file_it_writes() {
        use mpdfm_core::tags::{Field, TagDelta};

        // What a bulk edit stages: one operation per file, shown as one row per
        // field. Dropping that row has to drop all four operations, or the row
        // would be a lie about what is left.
        let fx = Fixture::realistic();
        let library = Library::scan(fx.music_dir()).expect("the fixture scans");
        let ops: Vec<Operation> = library
            .files_in(&mpdfm_core::library::DirPath::parse(names::MF_DOOM_ALBUM).expect("a dir"))
            .filter(|entry| entry.is_audio())
            .map(|entry| Operation::WriteTags {
                target: entry.rel.clone(),
                changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
            })
            .collect();
        let plan = Plan::of(ops);
        let files = plan.len();
        assert!(files >= 3, "the fixture's album has tracks in it");

        let effects = effects_of(&fx, &plan);
        let view = Pending::new(effects);
        let rows = drawn(&view, 80, 40);
        assert_eq!(
            rows.iter().filter(|row| row.starts_with("TAG")).count(),
            1,
            "one row for the one field: {rows:?}"
        );
        assert_eq!(
            view.selected_ops(),
            (0..files).collect::<Vec<_>>(),
            "and it stands for every file's edit"
        );
    }

    #[test]
    fn a_diff_line_is_something_to_read_and_not_something_to_drop() {
        let (_fx, mut view) = view(&album_move());
        open_everything(&mut view);
        let at = view
            .spots()
            .iter()
            .position(|spot| matches!(spot, Spot::Text))
            .expect("there is something to read");
        view.set_cursor(at, 40);
        assert!(
            view.selected_ops().is_empty(),
            "`dd` on a playlist line takes nothing off the plan"
        );
    }

    // -- re-validation -----------------------------------------------------

    #[test]
    fn a_new_preview_keeps_the_cursor_on_the_same_operation_and_the_folds_open() {
        let plan = Plan::of(vec![
            Operation::MoveDir {
                from: rel(names::MF_DOOM_ALBUM),
                to: rel("hiphop/MF DOOM/Mm..Food (2004)"),
            },
            Operation::MoveDir {
                from: rel(names::SNOOP_ALBUM),
                to: rel("hiphop/Snoop Dogg/Mac + Devin (2011)"),
            },
        ]);
        let (fx, mut view) = view(&plan);

        // On the second operation, with a playlist unfolded.
        view.set_cursor(1, 40);
        let was = view.spot();
        let at = view
            .spots()
            .iter()
            .position(|spot| matches!(spot, Spot::Playlist { .. }))
            .expect("a playlist is affected");
        view.set_cursor(at, 40);
        view.expand();
        view.set_cursor(1, 40);

        // The same plan validated again — which is what a rescan produces.
        view.revalidated(effects_of(&fx, &plan));
        assert_eq!(view.spot(), was, "the cursor is on the same operation");
        assert!(
            drawn(&view, 80, 60)
                .iter()
                .any(|row| row.trim_start().starts_with("+ ")),
            "and the playlist is still unfolded"
        );
    }

    // -- the report --------------------------------------------------------

    #[test]
    fn a_committed_transaction_shows_its_id_what_it_did_and_what_mpd_was_told() {
        let (_fx, mut view) = view(&album_move());
        view.finished(Report::Done {
            txid: "20260101T101010Z-abcd".to_owned(),
            headline: "moved 8 files, rewrote 4 lines across 2 playlists".to_owned(),
            mpd: Mpd::Queued { dirs: 3 },
            warnings: vec!["an old backup was kept".to_owned()],
        });

        assert!(
            view.title().contains("20260101T101010Z-abcd"),
            "{}",
            view.title()
        );
        let rows = drawn(&view, 100, 40);
        let text = rows.join("\n");
        assert!(text.contains("COMMITTED 20260101T101010Z-abcd"), "{text}");
        assert!(text.contains("moved 8 files"), "{text}");
        assert!(
            text.contains("MPD was asked to rescan 3 directories"),
            "{text}"
        );
        assert!(text.contains("an old backup was kept"), "{text}");
        assert!(!view.is_committable(), "there is nothing left to commit");

        let hints = Hints {
            undo: Some("u".to_owned()),
            back: Some("esc".to_owned()),
            ..Hints::default()
        };
        assert!(
            view.footer(&hints).contains("u undo this transaction"),
            "undo is offered right there: {}",
            view.footer(&hints)
        );
    }

    #[test]
    fn a_failed_commit_shows_what_stopped_it_and_the_command_that_puts_it_back() {
        let (_fx, mut view) = view(&album_move());
        view.finished(Report::Failed {
            message: "transaction 20260101T101010Z-abcd stopped at step 3 \
                      (rename hiphop/a.mp3): no space left on device\n\
                      Run `mpdfm recover 20260101T101010Z-abcd` to put it back."
                .to_owned(),
        });

        let text = drawn(&view, 80, 40).join("\n");
        assert!(text.contains("COMMIT FAILED"), "{text}");
        assert!(text.contains("stopped at step 3"), "{text}");
        // Wrapped and not cut: the recovery command is the half that matters.
        assert!(
            text.contains("mpdfm recover 20260101T101010Z-abcd"),
            "{text}"
        );
    }

    #[test]
    fn a_cancelled_commit_says_the_plan_is_still_there() {
        let (_fx, mut view) = view(&album_move());
        view.finished(Report::Cancelled);
        let text = drawn(&view, 80, 40).join("\n");
        assert!(text.contains("COMMIT CANCELLED"), "{text}");
        assert!(text.contains("nothing was changed"), "{text}");
        assert!(text.contains("still staged"), "{text}");

        // Dismissing it leaves the plan on screen again.
        view.dismiss();
        assert!(view.report().is_none());
        assert!(view.is_committable(), "the plan is untouched");
    }

    #[test]
    fn what_mpd_was_told_is_read_off_the_record_and_not_assumed() {
        assert_eq!(
            Mpd::Queued { dirs: 1 }.line(),
            "MPD was asked to rescan 1 directory"
        );
        assert!(
            Mpd::Failed {
                why: "connection refused".to_owned()
            }
            .line()
            .contains("connection refused")
        );
        assert_eq!(Mpd::NotAsked.line(), "MPD was not asked to rescan");
    }

    // -- the pane ----------------------------------------------------------

    #[test]
    fn no_row_is_ever_wider_than_the_pane() {
        let (_fx, mut view) = view(&album_move());
        open_everything(&mut view);
        for cells in [20, 40, 41, 79, 80, 120] {
            for row in drawn(&view, cells, 40) {
                assert!(
                    width(&row) <= cells,
                    "at {cells} cells, {row:?} is {} wide",
                    width(&row)
                );
            }
        }
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let (_fx, mut view) = view(&album_move());
        let last = view.spots().len() - 1;

        assert!(!view.move_cursor(-1, 10), "already at the top");
        assert_eq!(view.cursor(), 0);
        assert!(view.set_cursor(usize::MAX, 10));
        assert_eq!(view.cursor(), last);
        assert!(!view.move_cursor(1, 10), "already at the bottom");
    }

    #[test]
    fn a_pane_with_no_room_draws_nothing_rather_than_panicking() {
        let (_fx, mut view) = view(&album_move());
        open_everything(&mut view);
        assert!(drawn(&view, 0, 0).is_empty());
        assert!(drawn(&view, 80, 0).is_empty());
        assert_eq!(drawn(&view, 1, 1).len(), 1);
        assert_eq!(drawn(&view, 0, 3).len(), 3);
    }
}
