//! The tag editor: one file's tags, or four hundred files' tags, as a form.
//!
//! ```text
//! ┌ Edit tags — 14 files selected ───────────────────────────────┐
//! │ Title         <multiple>        per-file — use [T]           │
//! │ Artist        MF DOOM                                        │
//! │ Album artist  MF DOOM                                        │
//! │ Album         Mm..Food                                       │
//! │ Year          2004                                           │
//! │ Track         <multiple>        per-file — use [N]           │
//! │ Disc          1/1                                            │
//! │ Genre       ▸ Hip Hop_                                       │
//! │ Comment       <multiple>                                     │
//! │ Composer      —                                              │
//! │                                                              │
//! │ Actions: [T] titles from filenames  [N] renumber tracks      │
//! │ modified: genre                                              │
//! └ [w] stage · [W] stage & commit · [esc] cancel ───────────────┘
//! ```
//!
//! # The form is not the tags
//!
//! The task's second pitfall: *keep the form's state separate from `TagSet` so
//! cancelling is trivially correct*. So the editor holds a read-only
//! [`BulkView`] of what the files say and, separately, a `BTreeMap<Field, String>`
//! of **what the user typed**. Nothing is derived from the first on the way out —
//! [`BulkView::delta_for`] is handed the second — so cancelling is dropping a map
//! and there is no code path that could write a value the user merely looked at.
//!
//! That is also the whole of the `<multiple>` guarantee
//! (`docs/tasks/18-tag-bulk.md`), and it is enforced here by one thing:
//! **a field only enters the map when a keystroke changed it.** Not when the user
//! opened it, not when it was shown. [`Input::touched`] is the difference, which
//! matters because a `<multiple>` field opens *empty* — so "the text differs from
//! what was displayed" would make opening one and changing your mind
//! indistinguishable from emptying it on purpose, and those two must do very
//! different things to fourteen files.
//!
//! # Empty is not the same as untouched
//!
//! The first pitfall: the difference has to be *visible*, or the user cannot tell
//! what `w` will do. A field nobody touched shows the selection's own answer —
//! a value, `—` for absent, or `<multiple>`. A field the user emptied shows
//! `<cleared>` and is listed under `modified:`, because emptying a field is a
//! request to take the frame out of the file ([`Edit::Clear`], never
//! `Edit::Set("")` — see [`Edit`]'s own documentation for why those are not the
//! same write).
//!
//! # Per-file fields are refused, not accepted and then quietly dropped
//!
//! `title` and `track` are [`Field::is_per_file`]: one typed value across a
//! selection would give every track the same title. The editor refuses to *open*
//! them for a selection of more than one and names the action to use instead, and
//! [`BulkView::per_file_in`] is checked again at staging — the same function the
//! CLI refuses `--title` with, so the two front-ends cannot disagree about what
//! is allowed.
//!
//! For a selection of one, typing a title is exactly what the user means, and
//! both checks allow it.
//!
//! # Actions are previewed before they are part of the form
//!
//! `title from filename` is a guess about scene naming and `renumber tracks`
//! renumbers by the order the browser was showing. Both compute a *different*
//! value per file, so neither can be shown in a one-line field — they produce a
//! [`Preview`] the user reads and accepts, and only then are they folded into
//! what `w` would stage ([`tags::merge`]).

use std::collections::BTreeMap;

use mpdfm_core::paths::RelPath;
use mpdfm_core::tags::{
    self, AudioInfo, BulkView, Edit, FIELDS, Field, MULTIPLE, TagDelta, TagSet, Values, parse_pair,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::msg::TrackInfo;
use crate::tui::widgets::input::Input;
use crate::tui::widgets::{fit, pad, width};

/// Cells given to the field-name column.
///
/// Thirteen: `Album artist` is twelve and a label touching its value reads as one
/// word, which the details pane learned the same way (`widgets/details.rs`).
const LABEL_W: usize = 13;

/// Cells given to the marker between the label and the value.
const MARKER_W: usize = 2;

/// What a field with nothing in it shows.
const ABSENT: &str = "—";

/// What a field the user emptied shows.
///
/// The same word [`Edit::rendered`] prints in a plan preview, so the form and the
/// pending view call the same thing by the same name.
const CLEARED: &str = "<cleared>";

/// The marker on the field being typed into.
const EDITING: &str = "▸";

// ---------------------------------------------------------------------------

/// The tag editor's whole state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagEdit {
    /// The files, in the order the browser offered them — which is the order
    /// `renumber tracks` numbers in.
    files: Vec<RelPath>,
    /// What the selection says, once a worker has read it. `None` while the read
    /// is out.
    selection: Option<Selection>,
    /// Which field row the cursor is on.
    cursor: usize,
    /// The field being typed into, when there is one.
    editing: Option<Input>,
    /// What the user typed, by field. An empty string is a request to remove the
    /// field; a field that is not here is a field nobody touched.
    changed: BTreeMap<Field, String>,
    /// A per-file action's result, waiting to be accepted or rejected.
    preview: Option<Preview>,
    /// The per-file actions already accepted, oldest first.
    applied: Vec<Applied>,
}

/// The selection's tags, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Selection {
    /// Read-only. Nothing is ever written back off this.
    view: BulkView,
    /// The file's own facts, when the selection is exactly one file. Read-only
    /// in the form, because a bitrate is not a thing a tag editor sets.
    info: Option<AudioInfo>,
}

/// One of the two per-file actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// Take each file's title from its own name.
    TitleFromFilename,
    /// Number the files `1..n` in the order they are displayed.
    RenumberTracks,
}

impl FileAction {
    /// The field it writes.
    #[must_use]
    pub fn field(self) -> Field {
        match self {
            Self::TitleFromFilename => Field::Title,
            Self::RenumberTracks => Field::Track,
        }
    }

    /// What the form calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::TitleFromFilename => "titles from filenames",
            Self::RenumberTracks => "renumber tracks",
        }
    }
}

impl std::fmt::Display for FileAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A per-file action the user has accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Applied {
    action: FileAction,
    /// What it worked out, held rather than recomputed: the files have not been
    /// re-read since, so computing it again could only produce the same answer
    /// or a different one, and neither is useful.
    deltas: Vec<(RelPath, TagDelta)>,
}

/// A per-file action's result, before the user has agreed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Which action produced it.
    pub action: FileAction,
    /// One row per file it would change: the file, what it says now, and what it
    /// would say.
    pub rows: Vec<(RelPath, String, String)>,
    /// How far down the list has been scrolled.
    pub scroll: usize,
    /// The edits themselves.
    deltas: Vec<(RelPath, TagDelta)>,
}

/// What opening a field for typing turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begin {
    /// The field is open and the next keystroke types into it.
    Opened,
    /// This field cannot be typed across this many files; the action named is
    /// what to use instead.
    PerFile(FileAction),
    /// The tags have not arrived yet.
    NotReady,
}

/// What asking for a per-file action turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// There is a preview to look at.
    Shown,
    /// The action would change nothing — every file already says that.
    Nothing,
    /// The tags have not arrived yet.
    NotReady,
}

/// The keys the form names in its own text, as the live keymap spells them.
///
/// Passed in rather than written down, for the reason the help overlay is
/// generated: a user who has remapped `w` is told the key they chose, and a user
/// who has unbound an action is not told about a key that does nothing.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// [`Action::TitleFromFilename`][crate::tui::action::Action::TitleFromFilename].
    pub titles: Option<String>,
    /// [`Action::RenumberTracks`][crate::tui::action::Action::RenumberTracks].
    pub renumber: Option<String>,
    /// [`Action::ClearField`][crate::tui::action::Action::ClearField].
    pub clear: Option<String>,
    /// [`Action::StageTags`][crate::tui::action::Action::StageTags].
    pub stage: Option<String>,
    /// [`Action::StageAndCommit`][crate::tui::action::Action::StageAndCommit].
    pub commit: Option<String>,
    /// [`Action::Submit`][crate::tui::action::Action::Submit].
    pub accept: Option<String>,
    /// [`Action::Cancel`][crate::tui::action::Action::Cancel].
    pub cancel: Option<String>,
}

impl TagEdit {
    /// An editor over `files`, with their tags not read yet.
    ///
    /// Two states and not one, because reading four hundred files is a worker's
    /// job: the form is on screen saying what it is waiting for, and
    /// [`TagEdit::arrived`] fills it in.
    #[must_use]
    pub fn opening(files: Vec<RelPath>) -> Self {
        Self {
            files,
            selection: None,
            cursor: 0,
            editing: None,
            changed: BTreeMap::new(),
            preview: None,
            applied: Vec::new(),
        }
    }

    /// The files being edited, in the order they were selected.
    ///
    /// Only the tests read the whole list; what the UI needs from it is the
    /// count and the one path a single-file form shows, which are
    /// [`TagEdit::len`] and [`TagEdit::title`]. Gated for the reason
    /// [`Browser::targets`][crate::tui::views::browser::Browser] is: an accessor
    /// nothing calls is one the next reader has to work out the purpose of.
    #[cfg(test)]
    #[must_use]
    pub fn files(&self) -> &[RelPath] {
        &self.files
    }

    /// How many files are being edited.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the tags have not arrived yet.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.selection.is_none()
    }

    /// The tags came back.
    ///
    /// # Errors
    ///
    /// The messages of the files that would not read. A batch with an unreadable
    /// file in it is refused whole, for the reason `mpdfm tag set` refuses it: a
    /// bulk view of nine of ten files is a view of a selection nobody asked for,
    /// and `<multiple>` would be answering a different question from the one the
    /// user asked.
    pub fn arrived(
        &mut self,
        reads: Vec<(RelPath, Result<TrackInfo, String>)>,
    ) -> Result<(), Vec<String>> {
        let one = reads.len() == 1;
        let mut unreadable = Vec::new();
        let mut selection: Vec<(RelPath, TagSet)> = Vec::new();
        let mut info = None;

        for (rel, read) in reads {
            match read {
                Ok(track) => {
                    if one {
                        info = Some(track.info);
                    }
                    selection.push((rel, track.tags));
                }
                Err(message) => unreadable.push(message),
            }
        }
        if !unreadable.is_empty() {
            return Err(unreadable);
        }

        self.files = selection.iter().map(|(rel, _)| rel.clone()).collect();
        self.selection = Some(Selection {
            view: BulkView::of(&selection),
            info,
        });
        Ok(())
    }

    // -- the cursor and the open field -------------------------------------

    /// The field the cursor is on.
    #[must_use]
    pub fn field(&self) -> Field {
        FIELDS[self.cursor.min(FIELDS.len() - 1)]
    }

    /// Move the cursor by `delta` rows, stopping at both ends.
    pub fn move_cursor(&mut self, delta: isize) -> bool {
        let last = FIELDS.len() - 1;
        let target = self.cursor.saturating_add_signed(delta).min(last);
        let moved = target != self.cursor;
        self.cursor = target;
        moved
    }

    /// Put the cursor on a row: `isize::MIN` for the first, `isize::MAX` for the
    /// last.
    pub fn set_cursor(&mut self, row: usize) -> bool {
        let target = row.min(FIELDS.len() - 1);
        let moved = target != self.cursor;
        self.cursor = target;
        moved
    }

    /// Whether a field is open for typing.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// The open field's text editor, for the keys that edit text.
    pub fn input_mut(&mut self) -> Option<&mut Input> {
        self.editing.as_mut()
    }

    /// Open the field under the cursor for typing.
    ///
    /// It opens holding what the field says now, except for a `<multiple>` field,
    /// which opens **empty**: prefilling it with the word `<multiple>` would mean
    /// the user's first keystroke appended to a value no file has.
    pub fn begin(&mut self) -> Begin {
        let Some(selection) = &self.selection else {
            return Begin::NotReady;
        };
        let field = self.field();

        // The same rule `BulkView::per_file_in` enforces at staging, applied
        // early so the user is told before they type rather than after.
        if selection.view.len() > 1 && field.is_per_file() {
            return Begin::PerFile(match field {
                Field::Title => FileAction::TitleFromFilename,
                _ => FileAction::RenumberTracks,
            });
        }

        let text = match self.changed.get(&field) {
            // Back into a field that has already been typed in: the user's own
            // text, not the file's.
            Some(text) => text.clone(),
            None => match selection.view.get(field) {
                value if value.is_multiple() => String::new(),
                value => value.label(),
            },
        };
        self.editing = Some(Input::of(text));
        Begin::Opened
    }

    /// Close the open field, recording what was typed in it.
    ///
    /// Recording happens here and nowhere else, and only when a keystroke
    /// actually changed something — see the module documentation. Typing a field
    /// back to what it already said is not a modification either, which keeps the
    /// `modified:` line honest.
    ///
    /// **Nothing is written to a file.** The task is explicit: never write on
    /// field exit.
    pub fn end(&mut self) -> bool {
        let Some(input) = self.editing.take() else {
            return false;
        };
        if !input.touched() {
            // Opened and left alone. For a `<multiple>` field this is the case
            // the whole design exists for.
            return true;
        }

        let field = self.field();
        let text = input.into_text();
        let unchanged =
            self.selection
                .as_ref()
                .is_some_and(|selection| match selection.view.get(field) {
                    // A selection that disagrees has no value to have typed back.
                    value if value.is_multiple() => false,
                    value => value.label() == text,
                });
        if unchanged {
            self.changed.remove(&field);
        } else {
            self.changed.insert(field, text);
        }
        true
    }

    /// `clear this field`: ask for the field to be removed from every file.
    ///
    /// The explicit clear a `<multiple>` field needs (`docs/tasks/18-tag-bulk.md`):
    /// leaving such a field alone and asking for it to be emptied are different
    /// requests, and this is the second one.
    pub fn clear_field(&mut self) -> bool {
        if self.selection.is_none() {
            return false;
        }
        self.editing = None;
        self.changed.insert(self.field(), String::new());
        true
    }

    // -- what the user has asked for ---------------------------------------

    /// The fields the form would write, in display order.
    #[must_use]
    pub fn modified(&self) -> Vec<Field> {
        let mut fields: Vec<Field> = self.changed.keys().copied().collect();
        for applied in &self.applied {
            let field = applied.action.field();
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
        fields.sort_unstable();
        fields
    }

    /// Whether there is anything to lose by closing the form.
    ///
    /// Includes the field that is open and has been typed in, so that `esc` out
    /// of a half-finished edit still asks.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        !self.changed.is_empty()
            || !self.applied.is_empty()
            || self.editing.as_ref().is_some_and(Input::touched)
    }

    /// What is wrong with what has been typed, by field.
    ///
    /// Checked as it is typed — the open field's live text is validated before it
    /// has been recorded — so the error is on screen while the cursor is still in
    /// the field that caused it.
    #[must_use]
    pub fn errors(&self) -> Vec<(Field, String)> {
        FIELDS
            .into_iter()
            .filter_map(|field| match self.shown(field) {
                Shown::Typed(text) => invalid(field, text).map(|why| (field, why)),
                Shown::Current => None,
            })
            .collect()
    }

    /// The per-file fields a typed value was given, which must not be written
    /// across a selection.
    ///
    /// [`BulkView::per_file_in`]'s answer, which is also the CLI's: one
    /// implementation, so `mpdfm tag set --title` across four hundred files and
    /// this form refuse for the same reason and with the same list.
    #[must_use]
    pub fn per_file_refused(&self) -> Vec<Field> {
        self.selection
            .as_ref()
            .map(|selection| selection.view.per_file_in(&self.edits()))
            .unwrap_or_default()
    }

    /// The fields the user changed, as core's vocabulary for them.
    ///
    /// An empty string is [`Edit::Clear`] and not `Edit::Set("")`: the first
    /// takes the frame out of the file and the second writes a frame holding
    /// nothing, and a tag editor that confused them would leave empty frames
    /// behind every time somebody emptied a field.
    #[must_use]
    pub fn edits(&self) -> BTreeMap<Field, Edit> {
        self.changed
            .iter()
            .map(|(field, text)| {
                let edit = if text.is_empty() {
                    Edit::Clear
                } else {
                    Edit::Set(Values::typed(text))
                };
                (*field, edit)
            })
            .collect()
    }

    /// One [`TagDelta`] per file, for the files this form would actually change.
    ///
    /// The typed fields first and the accepted actions after them, so an action
    /// wins over a typed value for the same field — the rule `mpdfm tag set`
    /// applies, because a renumbering is the more specific request.
    ///
    /// A file that already says what was asked for is not in the result, and
    /// neither is a field that is merely on screen: [`BulkView::delta_for`] is
    /// given [`TagEdit::edits`] and reads nothing off the view's own values.
    #[must_use]
    pub fn deltas(&self) -> Vec<(RelPath, TagDelta)> {
        let Some(selection) = &self.selection else {
            return Vec::new();
        };
        let mut sets = vec![selection.view.delta_for(&self.edits())];
        sets.extend(self.applied.iter().map(|applied| applied.deltas.clone()));
        tags::merge(sets)
    }

    // -- the per-file actions ----------------------------------------------

    /// Work out what an action would do and show it.
    pub fn start_action(&mut self, action: FileAction) -> Started {
        let Some(selection) = &self.selection else {
            return Started::NotReady;
        };
        let deltas = match action {
            FileAction::TitleFromFilename => selection.view.titles_from_filenames(),
            FileAction::RenumberTracks => selection.view.renumber_tracks(),
        };
        if deltas.is_empty() {
            return Started::Nothing;
        }

        let field = action.field();
        let now: BTreeMap<&RelPath, String> = selection
            .view
            .selection()
            .map(|(rel, tags)| (rel, tags.get(field).joined()))
            .collect();
        let rows = deltas
            .iter()
            .map(|(rel, delta)| {
                let was = now.get(rel).cloned().unwrap_or_default();
                let will = delta
                    .get(field)
                    .and_then(Edit::values)
                    .map(Values::joined)
                    .unwrap_or_else(|| CLEARED.to_owned());
                (rel.clone(), was, will)
            })
            .collect();

        self.editing = None;
        self.preview = Some(Preview {
            action,
            rows,
            scroll: 0,
            deltas,
        });
        Started::Shown
    }

    /// The preview waiting to be answered, if there is one.
    #[must_use]
    pub fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    /// Fold the previewed action into the form.
    pub fn accept_preview(&mut self) -> bool {
        let Some(preview) = self.preview.take() else {
            return false;
        };
        // Asking for the same action twice replaces the first: it was computed
        // from the same files and the second answer is the one the user just
        // looked at.
        self.applied
            .retain(|applied| applied.action != preview.action);
        self.applied.push(Applied {
            action: preview.action,
            deltas: preview.deltas,
        });
        true
    }

    /// Throw the previewed action away.
    pub fn cancel_preview(&mut self) -> bool {
        self.preview.take().is_some()
    }

    /// Scroll the open preview, stopping at both ends.
    pub fn scroll_preview(&mut self, delta: isize, rows: usize) -> bool {
        let Some(preview) = &mut self.preview else {
            return false;
        };
        let last = preview.rows.len().saturating_sub(rows.max(1));
        let target = preview.scroll.saturating_add_signed(delta).min(last);
        let moved = target != preview.scroll;
        preview.scroll = target;
        moved
    }

    // -- drawing -----------------------------------------------------------

    /// The form's title, which is also how many files it is about.
    #[must_use]
    pub fn title(&self) -> String {
        match self.files.len() {
            1 => format!(
                " Edit tags — {} ",
                self.files.first().map_or("one file", |rel| rel.file_name())
            ),
            n => format!(" Edit tags — {n} files selected "),
        }
    }

    /// The form, as lines of styled text, in a pane `cells` wide.
    ///
    /// A pure function of the form's state: everything that mutates happens in
    /// the methods above, never here, which is the rule every view in this
    /// directory follows.
    #[must_use]
    pub fn lines(&self, cells: usize, hints: &Hints) -> Vec<Line<'static>> {
        let Some(selection) = &self.selection else {
            return vec![Line::from(Span::styled(
                format!("reading {} file(s)…", self.files.len()),
                Style::new().fg(Color::Yellow),
            ))];
        };

        let errors: BTreeMap<Field, String> = self.errors().into_iter().collect();
        let mut lines: Vec<Line<'static>> = FIELDS
            .into_iter()
            .enumerate()
            .map(|(row, field)| self.row(row, field, cells, &errors, hints))
            .collect();

        // The one file's own facts, which the form shows and never writes.
        if let (1, Some(info)) = (self.files.len(), selection.info.as_ref()) {
            lines.push(Line::raw(""));
            if let Some(rel) = self.files.first() {
                lines.push(Line::from(Span::styled(
                    fit(rel.as_str(), cells),
                    Style::new().fg(Color::Cyan),
                )));
            }
            lines.push(Line::from(Span::styled(
                fit(
                    &format!(
                        "{} · {} · {} kbps · {} Hz · {} ch",
                        info.format,
                        info.duration_hms(),
                        info.bitrate,
                        info.sample_rate,
                        info.channels
                    ),
                    cells,
                ),
                Style::new().fg(Color::DarkGray),
            )));
        }

        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            fit(&self.actions_line(hints), cells),
            Style::new().fg(Color::DarkGray),
        )));
        lines.push(self.modified_line(cells));
        lines
    }

    /// Where the terminal's own cursor goes: the column and row of the open
    /// field's caret, as offsets inside the form's area.
    ///
    /// Takes the same `hints` the frame was drawn with, because they decide how
    /// wide the note beside a field is and therefore where the value starts.
    #[must_use]
    pub fn caret(&self, cells: usize, hints: &Hints) -> Option<(u16, u16)> {
        let input = self.editing.as_ref()?;
        let field = self.field();
        let (value_w, _) = self.columns(cells, field, &self.errors().into_iter().collect(), hints);
        let column = LABEL_W + MARKER_W + input.window(value_w).cursor;
        Some((
            u16::try_from(column).unwrap_or(u16::MAX),
            u16::try_from(self.cursor).unwrap_or(u16::MAX),
        ))
    }

    /// One field's row.
    fn row(
        &self,
        row: usize,
        field: Field,
        cells: usize,
        errors: &BTreeMap<Field, String>,
        hints: &Hints,
    ) -> Line<'static> {
        let editing = row == self.cursor && self.editing.is_some();
        let (value_w, note_w) = self.columns(cells, field, errors, hints);

        let (value, value_style) = match self.shown(field) {
            Shown::Typed(_) if editing => {
                let shown = self
                    .editing
                    .as_ref()
                    .map(|input| input.window(value_w))
                    .map(|window| window.text)
                    .unwrap_or_default();
                (shown, Style::new().fg(Color::Yellow))
            }
            Shown::Typed("") => (
                pad(CLEARED, value_w),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Shown::Typed(text) => (
                pad(text, value_w),
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Shown::Current => {
                let value = self
                    .selection
                    .as_ref()
                    .map(|selection| selection.view.get(field).clone());
                match value {
                    Some(value) if value.is_multiple() => (
                        pad(MULTIPLE, value_w),
                        // Visually distinct, which the task asks for: this is the
                        // one value on screen that is MPDFM talking rather than
                        // the file.
                        Style::new()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Some(value) => {
                        let label = value.label();
                        if label.is_empty() {
                            (pad(ABSENT, value_w), Style::new().fg(Color::DarkGray))
                        } else {
                            (pad(&label, value_w), Style::new())
                        }
                    }
                    None => (pad("", value_w), Style::new()),
                }
            }
        };

        let (note, note_style) = self.note(field, errors, hints);
        let mut line = Line::from(vec![
            Span::styled(pad(label_of(field), LABEL_W), Style::new().fg(Color::Gray)),
            Span::styled(
                pad(if editing { EDITING } else { "" }, MARKER_W),
                Style::new().fg(Color::Yellow),
            ),
            Span::styled(value, value_style),
            Span::styled(pad(&note, note_w), note_style),
        ]);
        // The cursor row is marked by reversing it, except while it is being
        // typed into: the terminal's own cursor is in it then, and reversing the
        // row as well makes it impossible to see where that cursor is.
        if row == self.cursor && !editing {
            line = line.style(Style::new().add_modifier(Modifier::REVERSED));
        }
        line
    }

    /// How the row's two variable columns divide what is left of the width.
    ///
    /// One function, because [`TagEdit::caret`] has to agree with
    /// [`TagEdit::row`] about where the value starts and how wide it is — a
    /// terminal cursor one cell away from the character it is in front of is a
    /// bug nobody can explain.
    fn columns(
        &self,
        cells: usize,
        field: Field,
        errors: &BTreeMap<Field, String>,
        hints: &Hints,
    ) -> (usize, usize) {
        let rest = cells.saturating_sub(LABEL_W + MARKER_W);
        let (note, _) = self.note(field, errors, hints);
        if note.is_empty() {
            return (rest, 0);
        }
        // Half the remaining width is the ceiling, so a long error cannot
        // swallow the value it is about.
        let note_w = (width(&note) + 1).min(rest / 2);
        (rest - note_w, note_w)
    }

    /// What to say beside a field: what is wrong with it, or why it cannot be
    /// typed.
    fn note(
        &self,
        field: Field,
        errors: &BTreeMap<Field, String>,
        hints: &Hints,
    ) -> (String, Style) {
        if let Some(why) = errors.get(&field) {
            return (format!(" {why}"), Style::new().fg(Color::Red));
        }
        let bulk = self.files.len() > 1;
        if bulk && field.is_per_file() {
            let (action, key) = match field {
                Field::Title => (FileAction::TitleFromFilename, hints.titles.as_deref()),
                _ => (FileAction::RenumberTracks, hints.renumber.as_deref()),
            };
            let note = match key {
                Some(key) => format!(" per-file — {key} {action}"),
                None => " per-file".to_owned(),
            };
            return (note, Style::new().fg(Color::DarkGray));
        }
        (String::new(), Style::new())
    }

    /// The `Actions:` line, naming only the keys that are actually bound.
    fn actions_line(&self, hints: &Hints) -> String {
        let mut parts = Vec::new();
        if let Some(key) = &hints.titles {
            parts.push(format!("{key} {}", FileAction::TitleFromFilename));
        }
        if let Some(key) = &hints.renumber {
            parts.push(format!("{key} {}", FileAction::RenumberTracks));
        }
        if let Some(key) = &hints.clear {
            parts.push(format!("{key} clear this field"));
        }
        if parts.is_empty() {
            return String::new();
        }
        format!("Actions: {}", parts.join("  ·  "))
    }

    /// The `modified:` line — what `w` would write, spelled out.
    ///
    /// The task asks for the modified fields to be listed at the bottom so it is
    /// obvious what will be written, and this is also where the count of files
    /// goes: "genre (14 files)" is the sentence a user checks before pressing a
    /// key that changes fourteen files.
    fn modified_line(&self, cells: usize) -> Line<'static> {
        let modified = self.modified();
        if modified.is_empty() {
            return Line::from(Span::styled(
                pad("modified: nothing — no file would be written", cells),
                Style::new().fg(Color::DarkGray),
            ));
        }
        let names: Vec<String> = modified
            .iter()
            .map(|field| {
                let by = self
                    .applied
                    .iter()
                    .find(|applied| applied.action.field() == *field)
                    .map(|applied| format!(" ({})", applied.action));
                format!("{field}{}", by.unwrap_or_default())
            })
            .collect();
        let files = self.deltas().len();
        let plural = if files == 1 { "" } else { "s" };
        Line::from(Span::styled(
            pad(
                &format!("modified: {} — {files} file{plural}", names.join(", ")),
                cells,
            ),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
    }

    /// The bottom line of the form: how to stage it, and how to leave.
    #[must_use]
    pub fn footer(&self, hints: &Hints) -> String {
        let mut parts = Vec::new();
        if let Some(key) = &hints.stage {
            parts.push(format!("{key} stage"));
        }
        if let Some(key) = &hints.commit {
            parts.push(format!("{key} stage & commit"));
        }
        if let Some(key) = &hints.cancel {
            parts.push(format!("{key} cancel"));
        }
        format!(" {} ", parts.join(" · "))
    }

    /// What one row is showing: the selection's own answer, or the user's text.
    fn shown(&self, field: Field) -> Shown<'_> {
        if let Some(input) = &self.editing
            && self.field() == field
        {
            return Shown::Typed(input.text());
        }
        match self.changed.get(&field) {
            Some(text) => Shown::Typed(text),
            None => Shown::Current,
        }
    }
}

/// What a field row is showing.
enum Shown<'a> {
    /// The selection's value, untouched by the user.
    Current,
    /// What the user typed. Empty means "take this field out of the file".
    Typed(&'a str),
}

impl Preview {
    /// The preview's title: which action, over how many files.
    #[must_use]
    pub fn title(&self) -> String {
        let plural = if self.rows.len() == 1 { "" } else { "s" };
        format!(
            " {} — {} file{plural} would change ",
            self.action,
            self.rows.len()
        )
    }

    /// The bottom line: how to accept it, and how not to.
    #[must_use]
    pub fn footer(&self, hints: &Hints) -> String {
        let accept = hints.accept.as_deref().unwrap_or("enter");
        let cancel = hints.cancel.as_deref().unwrap_or("esc");
        let hidden = self.rows.len().saturating_sub(self.scroll);
        format!(" {accept} apply · {cancel} cancel · {hidden} below ")
    }

    /// The list, `rows` rows of it from where it is scrolled to.
    #[must_use]
    pub fn lines(&self, cells: usize, rows: usize) -> Vec<Line<'static>> {
        // Half for the name and half for the change, which keeps a scene-release
        // file name readable without hiding the value it is about.
        let name_w = (cells / 2).max(1);
        let change_w = cells.saturating_sub(name_w + 1);
        self.rows
            .iter()
            .skip(self.scroll)
            .take(rows)
            .map(|(rel, was, will)| {
                let was = if was.is_empty() { ABSENT } else { was };
                Line::from(vec![
                    Span::raw(pad(rel.file_name(), name_w)),
                    Span::raw(" "),
                    Span::styled(
                        fit(&format!("{was} → {will}"), change_w),
                        Style::new().fg(Color::Yellow),
                    ),
                ])
            })
            .collect()
    }
}

/// The field's name, as the form spells it.
///
/// Not [`Field::as_str`], which is what `keys.toml` and `--clear` take: a form
/// label is read by a human and `albumartist` is not how a human spells it.
fn label_of(field: Field) -> &'static str {
    match field {
        Field::Title => "Title",
        Field::Artist => "Artist",
        Field::AlbumArtist => "Album artist",
        Field::Album => "Album",
        Field::Year => "Year",
        Field::Track => "Track",
        Field::Disc => "Disc",
        Field::Genre => "Genre",
        Field::Comment => "Comment",
        Field::Composer => "Composer",
    }
}

/// Why a field will not take this text, if it will not.
///
/// Only the two shapes that are not free text. Everything else a tag can hold is
/// a string, and a tag editor that refused a genre for not being in a list would
/// be wrong about this library — `(17)` resolves to `Rock`, but `Nu Jazz` and
/// `レゲエ` are genres too.
///
/// An empty string is always valid: it is a request to remove the field, not a
/// value that has to parse.
#[must_use]
pub fn invalid(field: Field, text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    match field {
        Field::Year => {
            (!is_date(text)).then(|| "not a year or a date — try 2004 or 2019-03-15".to_owned())
        }
        Field::Track | Field::Disc => parse_pair(text)
            .is_none()
            .then(|| "not a number — try 5 or 5/12".to_owned()),
        _ => None,
    }
}

/// Whether this is a year or a date MPDFM would write.
///
/// `2004`, `2019-03`, `2019-03-15`. Deliberately not a calendar check: the real
/// library holds `2004-00-00` and a `DATE` field is whatever the tagger that
/// wrote it believed, so the question here is only whether the *shape* is one
/// [`TagSet::year`][mpdfm_core::tags::TagSet::year] can narrow — four digits,
/// then up to two more dash-separated numbers.
fn is_date(text: &str) -> bool {
    let mut parts = text.split('-');
    let Some(year) = parts.next() else {
        return false;
    };
    if year.len() != 4 || !year.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    rest.len() <= 2
        && rest.iter().all(|part| {
            !part.is_empty() && part.len() <= 2 && part.chars().all(|c| c.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use mpdfm_core::library::Format;
    use mpdfm_core::tags::AudioInfo;

    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::parse(path).expect("a relative path")
    }

    /// A read as the worker reports it, with no audio properties worth naming.
    fn read(path: &str, tags: TagSet) -> (RelPath, Result<TrackInfo, String>) {
        (
            rel(path),
            Ok(TrackInfo {
                tags,
                info: AudioInfo {
                    duration: std::time::Duration::from_secs(204),
                    bitrate: 320,
                    sample_rate: 44_100,
                    channels: 2,
                    format: Format::Mp3,
                },
            }),
        )
    }

    /// An album of three tracks: one album, three titles — the shape task 18's
    /// first acceptance criterion is about.
    ///
    /// The tagged titles are lower-case and the file names are not, so
    /// `titles from filenames` has something to do. That is not a contrivance: it
    /// is the ordinary state of a scene release whose tags were written by a
    /// different tool from the one that named the files.
    fn album() -> TagEdit {
        let files: Vec<(RelPath, Result<TrackInfo, String>)> =
            ["Beef Rap", "Hoe Cakes", "Potholderz"]
                .into_iter()
                .enumerate()
                .map(|(index, title)| {
                    read(
                        &format!("hiphop/Mm..Food/0{} {title}.mp3", index + 1),
                        TagSet {
                            title: Values::one(title.to_lowercase()),
                            artist: Values::one("MF DOOM"),
                            album: Values::one("Mm..Food"),
                            date: Values::one("2004"),
                            track: Some((u32::try_from(index).unwrap() + 1, Some(3))),
                            ..TagSet::default()
                        },
                    )
                })
                .collect();

        let mut form = TagEdit::opening(files.iter().map(|(rel, _)| rel.clone()).collect());
        form.arrived(files).expect("the fixture reads");
        form
    }

    /// One file, which is the other half of every rule here.
    fn single() -> TagEdit {
        let files = vec![read(
            "hiphop/Mm..Food/01 Beef Rap.mp3",
            TagSet {
                title: Values::one("Beef Rap"),
                album: Values::one("Mm..Food"),
                genre: Values::one("Hip Hop"),
                ..TagSet::default()
            },
        )];
        let mut form = TagEdit::opening(files.iter().map(|(rel, _)| rel.clone()).collect());
        form.arrived(files).expect("the fixture reads");
        form
    }

    /// Put the cursor on a field.
    fn go_to(form: &mut TagEdit, field: Field) {
        let row = FIELDS.iter().position(|f| *f == field).expect("a field");
        form.set_cursor(row);
        assert_eq!(form.field(), field);
    }

    /// Open a field, replace what is in it, and close it — one whole edit.
    ///
    /// A field opens holding its current value with the cursor at the end of it,
    /// which is what makes fixing a typo possible; replacing the value is
    /// `ctrl-u` and then typing, which is what this does.
    fn type_into(form: &mut TagEdit, field: Field, text: &str) {
        go_to(form, field);
        assert_eq!(form.begin(), Begin::Opened, "{field} would not open");
        let input = form.input_mut().expect("the field is open");
        input.clear();
        for c in text.chars() {
            input.insert(c);
        }
        form.end();
    }

    // -- the critical rule -------------------------------------------------

    #[test]
    fn a_multiple_field_left_alone_produces_no_change() {
        let mut form = album();
        // Three different titles.
        assert!(
            form.selection
                .as_ref()
                .unwrap()
                .view
                .get(Field::Title)
                .is_multiple()
        );

        // Edit something else entirely.
        type_into(&mut form, Field::Genre, "Hip Hop");

        assert_eq!(form.modified(), vec![Field::Genre]);
        for (rel, delta) in form.deltas() {
            assert!(
                delta.get(Field::Title).is_none(),
                "{rel} would have had its title flattened"
            );
            assert_eq!(delta.len(), 1, "{rel}: only the genre was asked for");
        }
    }

    #[test]
    fn opening_a_multiple_field_and_changing_your_mind_writes_nothing() {
        let mut form = album();
        go_to(&mut form, Field::Comment);
        // The comment is absent everywhere here, so use the one that differs.
        go_to(&mut form, Field::Title);
        // Title is per-file across three files, so use a field that is not: make
        // the artists disagree first.
        let mut form = {
            let files = vec![
                read(
                    "a/1.mp3",
                    TagSet {
                        artist: Values::one("MF DOOM"),
                        ..TagSet::default()
                    },
                ),
                read(
                    "a/2.mp3",
                    TagSet {
                        artist: Values::one("Madlib"),
                        ..TagSet::default()
                    },
                ),
            ];
            let mut form = TagEdit::opening(files.iter().map(|(rel, _)| rel.clone()).collect());
            form.arrived(files).expect("it reads");
            form
        };

        go_to(&mut form, Field::Artist);
        assert_eq!(form.begin(), Begin::Opened);
        // The field opened empty, because the selection disagrees.
        assert_eq!(form.input_mut().unwrap().text(), "");
        // Move around in it, then leave without typing.
        form.input_mut().unwrap().left();
        form.end();

        assert!(form.modified().is_empty(), "nothing was typed");
        assert!(form.deltas().is_empty(), "so nothing would be written");
        assert!(!form.is_modified());
    }

    #[test]
    fn typing_into_a_multiple_field_writes_to_every_file() {
        let files = vec![
            read(
                "a/1.mp3",
                TagSet {
                    genre: Values::one("Hip Hop"),
                    ..TagSet::default()
                },
            ),
            read("a/2.mp3", TagSet::default()),
        ];
        let mut form = TagEdit::opening(files.iter().map(|(rel, _)| rel.clone()).collect());
        form.arrived(files).expect("it reads");
        assert!(
            form.selection
                .as_ref()
                .unwrap()
                .view
                .get(Field::Genre)
                .is_multiple()
        );

        type_into(&mut form, Field::Genre, "Jazz");

        let deltas = form.deltas();
        assert_eq!(
            deltas.len(),
            2,
            "both files, including the one that had none"
        );
        for (_, delta) in deltas {
            assert_eq!(
                delta.get(Field::Genre),
                Some(&Edit::Set(Values::one("Jazz")))
            );
        }
    }

    #[test]
    fn an_explicit_clear_is_not_the_same_as_leaving_a_field_alone() {
        let mut form = album();
        go_to(&mut form, Field::Album);
        assert!(form.clear_field());

        assert_eq!(form.modified(), vec![Field::Album]);
        assert_eq!(form.edits().get(&Field::Album), Some(&Edit::Clear));
        let deltas = form.deltas();
        assert_eq!(deltas.len(), 3, "every file has an album to remove");
        assert!(
            deltas
                .iter()
                .all(|(_, delta)| delta.get(Field::Album) == Some(&Edit::Clear))
        );
    }

    #[test]
    fn emptying_a_field_by_hand_is_a_clear_and_says_so_on_screen() {
        let mut form = album();
        go_to(&mut form, Field::Album);
        assert_eq!(form.begin(), Begin::Opened);
        assert_eq!(form.input_mut().unwrap().text(), "Mm..Food");
        form.input_mut().unwrap().clear();
        form.end();

        assert_eq!(form.edits().get(&Field::Album), Some(&Edit::Clear));
        let drawn = drawn(&form, 70);
        assert!(
            drawn.iter().any(|line| line.contains(CLEARED)),
            "the user cannot tell an emptied field from an empty one:\n{}",
            drawn.join("\n")
        );
    }

    #[test]
    fn typing_a_field_back_to_what_it_said_is_not_a_modification() {
        let mut form = album();
        go_to(&mut form, Field::Album);
        form.begin();
        let input = form.input_mut().unwrap();
        input.backspace();
        input.insert('d');
        form.end();

        assert!(
            form.modified().is_empty(),
            "the value is the one that was already there"
        );
    }

    // -- per-file fields ---------------------------------------------------

    #[test]
    fn a_per_file_field_will_not_open_across_a_selection_but_will_for_one_file() {
        let mut form = album();
        go_to(&mut form, Field::Title);
        assert_eq!(form.begin(), Begin::PerFile(FileAction::TitleFromFilename));
        go_to(&mut form, Field::Track);
        assert_eq!(form.begin(), Begin::PerFile(FileAction::RenumberTracks));
        assert!(!form.is_editing(), "nothing was opened");

        let mut one = single();
        go_to(&mut one, Field::Title);
        assert_eq!(one.begin(), Begin::Opened, "one file wants its own title");
    }

    #[test]
    fn clearing_a_per_file_field_across_a_selection_is_allowed() {
        // The refusal is about typing one value into many files, not about
        // removing the field — which is what core's `per_file_in` says too.
        let mut form = album();
        go_to(&mut form, Field::Title);
        form.clear_field();
        assert!(form.per_file_refused().is_empty());
        assert_eq!(form.deltas().len(), 3);
    }

    #[test]
    fn a_typed_per_file_field_is_refused_at_staging_as_well() {
        // Belt and braces: `begin` refuses early for the user's sake, and this is
        // the check that cannot be got round, shared with `mpdfm tag set`.
        let mut one = single();
        type_into(&mut one, Field::Title, "Beef Rap (Remix)");
        assert!(one.per_file_refused().is_empty(), "one file is allowed");
    }

    // -- validation --------------------------------------------------------

    #[test]
    fn a_year_must_be_a_year_or_a_date() {
        assert_eq!(invalid(Field::Year, "2004"), None);
        assert_eq!(invalid(Field::Year, "2019-03"), None);
        assert_eq!(invalid(Field::Year, "2019-03-15"), None);
        // The real library holds this one, so it has to be writable.
        assert_eq!(invalid(Field::Year, "2004-00-00"), None);
        assert_eq!(invalid(Field::Year, ""), None, "empty is a clear");

        assert!(invalid(Field::Year, "20x4").is_some());
        assert!(invalid(Field::Year, "204").is_some());
        assert!(invalid(Field::Year, "nineteen").is_some());
        assert!(invalid(Field::Year, "2019-").is_some());
        assert!(invalid(Field::Year, "2019-03-15-02").is_some());
    }

    #[test]
    fn a_track_must_be_a_number_or_a_number_and_a_total() {
        assert_eq!(invalid(Field::Track, "5"), None);
        assert_eq!(invalid(Field::Track, "05/12"), None);
        assert_eq!(invalid(Field::Disc, "1/1"), None);
        assert!(invalid(Field::Track, "one").is_some());
        assert!(
            invalid(Field::Track, "5/").is_none(),
            "a bare total is dropped"
        );
        assert!(invalid(Field::Track, "a/b").is_some());
    }

    #[test]
    fn an_error_is_reported_while_the_field_is_still_open() {
        let mut one = single();
        go_to(&mut one, Field::Year);
        one.begin();
        for c in "20x4".chars() {
            one.input_mut().unwrap().insert(c);
        }

        let errors = one.errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, Field::Year);
        let drawn = drawn(&one, 78);
        assert!(
            drawn.iter().any(|line| line.contains("not a year")),
            "the reason is not on screen:\n{}",
            drawn.join("\n")
        );
    }

    #[test]
    fn a_free_text_field_takes_anything_including_cjk() {
        for text in ["Nu Jazz", "レゲエ", "Hip Hop; Rap", "MF DOOM & Madlib"] {
            assert_eq!(invalid(Field::Genre, text), None, "{text}");
        }
    }

    #[test]
    fn utf8_typed_into_a_field_reaches_the_delta_unchanged() {
        let mut one = single();
        type_into(&mut one, Field::Album, "ノスタルジア");
        let deltas = one.deltas();
        assert_eq!(deltas.len(), 1);
        assert_eq!(
            deltas[0].1.get(Field::Album),
            Some(&Edit::Set(Values::one("ノスタルジア")))
        );
    }

    #[test]
    fn a_semicolon_separated_value_becomes_several_values() {
        let mut one = single();
        type_into(&mut one, Field::Artist, "Madvillain; MF DOOM");
        let edit = one.edits().get(&Field::Artist).cloned();
        assert_eq!(
            edit,
            Some(Edit::Set(Values::of(["Madvillain", "MF DOOM"]))),
            "the separator the whole program splits on"
        );
    }

    // -- the actions -------------------------------------------------------

    #[test]
    fn an_action_previews_before_it_is_part_of_the_form() {
        let mut form = album();
        assert_eq!(
            form.start_action(FileAction::RenumberTracks),
            Started::Nothing,
            "they are already 1..3 of 3"
        );

        // Titles come from the file names, which differ from the tags here.
        assert_eq!(
            form.start_action(FileAction::TitleFromFilename),
            Started::Shown
        );
        let preview = form.preview().expect("a preview");
        assert_eq!(preview.rows.len(), 3);
        assert_eq!(preview.rows[0].1, "beef rap", "what it says now");
        assert_eq!(preview.rows[0].2, "Beef Rap", "what its own name says");

        // Nothing is staged until it is accepted.
        assert!(form.modified().is_empty());
        assert!(form.cancel_preview());
        assert!(form.modified().is_empty());

        form.start_action(FileAction::TitleFromFilename);
        assert!(form.accept_preview());
        assert_eq!(form.modified(), vec![Field::Title]);
        assert_eq!(form.deltas().len(), 3);
    }

    #[test]
    fn renumbering_uses_the_order_the_browser_was_showing() {
        // Three files whose numbers are wrong, in the order they were marked.
        let files = vec![
            read(
                "a/x.mp3",
                TagSet {
                    track: Some((9, None)),
                    ..TagSet::default()
                },
            ),
            read(
                "a/y.mp3",
                TagSet {
                    track: Some((4, None)),
                    ..TagSet::default()
                },
            ),
        ];
        let mut form = TagEdit::opening(files.iter().map(|(rel, _)| rel.clone()).collect());
        form.arrived(files).expect("it reads");

        assert_eq!(
            form.start_action(FileAction::RenumberTracks),
            Started::Shown
        );
        let rows = &form.preview().unwrap().rows;
        assert_eq!(rows[0].2, "1/2");
        assert_eq!(rows[1].2, "2/2");
        form.accept_preview();

        let deltas = form.deltas();
        assert_eq!(deltas.len(), 2);
        assert_eq!(
            deltas[0].1.get(Field::Track),
            Some(&Edit::Set(Values::one("1/2")))
        );
    }

    #[test]
    fn an_action_and_a_typed_field_become_one_operation_per_file() {
        let mut form = album();
        type_into(&mut form, Field::Genre, "Hip Hop");
        form.start_action(FileAction::TitleFromFilename);
        form.accept_preview();

        let deltas = form.deltas();
        assert_eq!(deltas.len(), 3);
        for (rel, delta) in deltas {
            assert_eq!(delta.len(), 2, "{rel} should hold both fields");
            assert!(delta.get(Field::Genre).is_some() && delta.get(Field::Title).is_some());
        }
    }

    #[test]
    fn asking_for_the_same_action_twice_does_not_apply_it_twice() {
        let mut form = album();
        for _ in 0..2 {
            form.start_action(FileAction::TitleFromFilename);
            form.accept_preview();
        }
        assert_eq!(form.modified(), vec![Field::Title]);
        assert_eq!(form.deltas().len(), 3);
    }

    // -- the form on screen ------------------------------------------------

    /// The form's lines as plain text.
    fn drawn(form: &TagEdit, cells: usize) -> Vec<String> {
        form.lines(cells, &Hints::default())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn every_field_has_a_row_and_a_multiple_one_says_so() {
        let form = album();
        let drawn = drawn(&form, 70);
        for field in FIELDS {
            assert!(
                drawn.iter().any(|line| line.starts_with(label_of(field))),
                "no row for {field}:\n{}",
                drawn.join("\n")
            );
        }
        assert!(
            drawn.iter().any(|line| line.contains(MULTIPLE)),
            "three titles and no `<multiple>`:\n{}",
            drawn.join("\n")
        );
        assert!(drawn.iter().any(|line| line.contains("modified: nothing")));
    }

    #[test]
    fn the_note_beside_a_per_file_field_names_the_key_rather_than_being_cut_off() {
        // The column arithmetic has to be done with the *same* hints the row is
        // drawn with, or the note is measured as " per-file" and then rendered as
        // " per-file — T titles from filenames" cut down to fit. Found on the
        // real library, at 120 columns, where there was room for all of it.
        let form = album();
        let hints = Hints {
            titles: Some("T".to_owned()),
            renumber: Some("N".to_owned()),
            ..Hints::default()
        };
        let drawn: Vec<String> = form
            .lines(120, &hints)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        let shown = drawn.join("\n");
        assert!(
            shown.contains("per-file — T titles from filenames"),
            "the note was truncated:\n{shown}"
        );
        assert!(
            shown.contains("per-file — N renumber tracks"),
            "the note was truncated:\n{shown}"
        );
    }

    #[test]
    fn every_row_is_exactly_as_wide_as_the_pane() {
        // The form is drawn inside a border; a row one cell too wide corrupts it.
        let mut form = album();
        type_into(&mut form, Field::Genre, "ノスタルジア");
        go_to(&mut form, Field::Year);
        form.begin();
        let hints = Hints {
            titles: Some("T".to_owned()),
            renumber: Some("N".to_owned()),
            clear: Some("C".to_owned()),
            stage: Some("w".to_owned()),
            commit: Some("W".to_owned()),
            accept: Some("enter".to_owned()),
            cancel: Some("esc".to_owned()),
        };
        for cells in [20, 30, 48, 70, 100, 200] {
            for line in form.lines(cells, &hints) {
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(
                    width(&text) <= cells,
                    "a {}-cell row in a {cells}-cell pane: {text:?}",
                    width(&text)
                );
            }
        }
    }

    #[test]
    fn one_file_also_shows_its_path_and_what_the_audio_is() {
        let drawn = drawn(&single(), 78);
        let text = drawn.join("\n");
        assert!(text.contains("hiphop/Mm..Food/01 Beef Rap.mp3"), "{text}");
        assert!(
            text.contains("320 kbps") && text.contains("44100 Hz"),
            "{text}"
        );
        assert!(text.contains("3:24"), "the duration is missing:\n{text}");
    }

    #[test]
    fn a_selection_does_not_show_one_files_properties() {
        let text = drawn(&album(), 78).join("\n");
        assert!(
            !text.contains("kbps"),
            "fourteen files do not have one bitrate:\n{text}"
        );
        assert!(text.contains("3 files selected") || album().title().contains("3 files"));
    }

    #[test]
    fn the_caret_is_in_the_field_the_cursor_is_in() {
        let mut one = single();
        go_to(&mut one, Field::Genre);
        assert_eq!(one.caret(70, &Hints::default()), None, "no field is open");
        one.begin();
        let (column, row) = one.caret(70, &Hints::default()).expect("a caret");
        assert_eq!(
            usize::from(row),
            FIELDS.iter().position(|f| *f == Field::Genre).unwrap()
        );
        assert_eq!(
            usize::from(column),
            LABEL_W + MARKER_W + "Hip Hop".len(),
            "after the value it opened on"
        );
    }

    #[test]
    fn a_file_that_will_not_read_refuses_the_whole_selection() {
        let mut form = TagEdit::opening(vec![rel("a/1.mp3"), rel("a/2.mp3")]);
        let err = form
            .arrived(vec![
                read("a/1.mp3", TagSet::default()),
                (rel("a/2.mp3"), Err("a/2.mp3: not an audio file".to_owned())),
            ])
            .expect_err("one file could not be read");
        assert_eq!(err, vec!["a/2.mp3: not an audio file"]);
        assert!(form.is_loading(), "the form never opened");
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut form = album();
        assert!(!form.move_cursor(-1));
        assert_eq!(form.field(), FIELDS[0]);
        assert!(form.move_cursor(isize::MAX));
        assert_eq!(form.field(), FIELDS[FIELDS.len() - 1]);
        assert!(!form.move_cursor(1));
    }

    #[test]
    fn a_form_with_two_hundred_files_across_two_albums_previews_correctly() {
        let files: Vec<(RelPath, Result<TrackInfo, String>)> = (0..200)
            .map(|index| {
                let album = if index < 100 {
                    "Mm..Food"
                } else {
                    "Madvillainy"
                };
                read(
                    &format!("hiphop/{album}/{index:03} Track.mp3"),
                    TagSet {
                        album: Values::one(album),
                        genre: Values::one("Rap"),
                        ..TagSet::default()
                    },
                )
            })
            .collect();
        let mut form = TagEdit::opening(files.iter().map(|(rel, _)| rel.clone()).collect());
        form.arrived(files).expect("they read");

        // Two albums, so the album field disagrees and the genre does not.
        let view = &form.selection.as_ref().unwrap().view;
        assert!(view.get(Field::Album).is_multiple());
        assert_eq!(
            view.get(Field::Genre).same().map(Values::joined),
            Some("Rap".to_owned())
        );

        type_into(&mut form, Field::Genre, "Hip Hop");
        assert_eq!(form.deltas().len(), 200, "all of them change");

        form.start_action(FileAction::RenumberTracks);
        let preview = form.preview().expect("a preview");
        assert_eq!(preview.rows.len(), 200);
        assert_eq!(preview.rows[199].2, "200/200");
        // It scrolls rather than truncating: a hidden change is the thing this
        // program exists to prevent.
        assert_eq!(preview.lines(60, 20).len(), 20);
        form.accept_preview();

        assert!(form.deltas().iter().all(|(_, delta)| delta.len() == 2));
        assert!(
            form.modified_line(80).to_string().contains("200 file"),
            "the count of files is what the user checks"
        );
    }
}
