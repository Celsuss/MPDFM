//! [`Effects::render`]: the preview, as one string, at a given terminal width.
//!
//! One renderer, two callers — `mpdfm move --dry-run` and the TUI's pending view
//! (task 24). The pitfall this exists to avoid is the TUI growing its own
//! formatting and quietly disagreeing with the CLI about what a commit will do,
//! on the one screen whose entire job is to be believed.
//!
//! # Layout
//!
//! ```text
//! PENDING (2 ops)
//! MOVE    hiphop/MF DOOM - Mm..Food (2004) → hiphop/MF DOOM/Mm..Food (2004)
//!         14 audio + 3 aux files, 61.2 MB, 2 playlists
//! DELETE  lofi/mix/old take.mp3
//!         1 audio file, 4.1 MB, 1 playlist
//!
//! Playlists
//!   Coding flow.m3u    3 lines rewritten
//!   Hip hop.m3u        1 line rewritten
//!
//! ! Coding flow.m3u: 1 line(s) will be removed, not rewritten
//! ```
//!
//! A refused plan leads with its conflicts instead, each on an `x` line, and says
//! so in the header — the user should not have to read to the bottom to find out
//! that nothing is going to happen.
//!
//! # Tag edits are collapsed by field
//!
//! A bulk edit is one [`Operation::WriteTags`] per file, because the unit of
//! reversal is the file. Fourteen rows that all say the same thing is not a
//! preview, so they are rendered as one row per changed **field**, with the count
//! of files it is written to and the paths left out:
//!
//! ```text
//! PENDING (14 ops)
//! TAG     genre = "Hip Hop"          14 files
//! TAG     albumartist = "MF DOOM"    14 files
//! TAG     comment = <cleared>        14 files
//! ```
//!
//! Which field and which value is what the user is checking; which fourteen files
//! they already know, because they selected them. A tag edit that is *not*
//! uniform — different values for the same field, as `renumber tracks` produces —
//! says so rather than picking one of them to show.
//!
//! # Width
//!
//! `width` is a hard limit: no line comes back longer, at any width down to the
//! 40 columns the renderer clamps at. Paths are the only thing long enough to
//! need it and are shortened from the *left*, because the end of a music path is
//! the track and the start is the genre — `…OOM - Mm..Food (2004)/01 Beef Rap.mp3`
//! is still recognizable, and `hiphop/MF DOOM - Mm..Foo…` is not.

use super::effects::{Conflict, Effects, OpEffect, Summary, Warning};

/// Below this the layout stops making sense, so it stops shrinking.
const MIN_WIDTH: usize = 40;

/// How far the detail line under an operation is indented.
const INDENT: &str = "        ";

/// How many distinct values of one field a `TAG` row will list before it gives
/// up and says `<per file>`.
const MAX_TAG_VALUES: usize = 3;

/// How wide the indent in front of a playlist's name is.
///
/// Two cells, and named because the pending view draws its `▸` / `▾` expansion
/// marker *in* them rather than in front of them — so a playlist row is the same
/// width and the same string in both front-ends
/// (`docs/tasks/24-pending-view.md`).
pub const PLAYLIST_INDENT: usize = 2;

/// One line of the preview, and what it is about.
///
/// [`Effects::render`] is these lines joined with newlines, and that is what the
/// CLI prints. The TUI needs the `kind` as well: which line is which operation,
/// so a cursor can sit on one and `dd` can drop it, and which line is which
/// playlist, so the per-line diff can be unfolded underneath it. Both front-ends
/// therefore draw the same strings, which is what makes the anti-divergence
/// check in `docs/tasks/24-pending-view.md` something that can be asserted
/// rather than hoped for.
///
/// How many lines there are, and what kind each is, does **not** depend on
/// `width` — only the text does. That is what lets a caller work out what is
/// selectable without knowing how wide the pane is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewLine {
    /// The line, already fitted to the width it was rendered at.
    pub text: String,
    /// What it is about.
    pub kind: LineKind,
}

/// What a [`PreviewLine`] is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// The first line: how much is staged, and whether it can be committed.
    Header,

    /// A section heading, or the blank line in front of one.
    Section,

    /// One staged operation.
    Op {
        /// Its index in [`Plan::ops`][super::op::Plan::ops].
        op: usize,
        /// Whether a [`Conflict`] names it.
        refused: bool,
    },

    /// The line under an operation that says what it costs.
    Detail {
        /// The operation it belongs to.
        op: usize,
    },

    /// One changed tag field, collapsed across every operation that writes it.
    ///
    /// Not one operation but several: a bulk edit is one
    /// [`Operation::WriteTags`][super::op::Operation::WriteTags] per file and the
    /// preview shows one row per *field*, so this carries every staged operation
    /// the row stands for.
    Tag {
        /// The staged operations this row stands for, ascending.
        ops: Vec<usize>,
        /// Whether a [`Conflict`] names any of them.
        refused: bool,
    },

    /// One conflict.
    Conflict {
        /// Its index into [`Effects::conflicts`].
        at: usize,
    },

    /// One affected playlist.
    Playlist {
        /// Its index into [`Effects::playlist_edits`], or `None` for MPD's saved
        /// queue — whose edits are [`Effects::state_edits`].
        at: Option<usize>,
    },

    /// One warning.
    Warning {
        /// Its index into [`Effects::warnings`].
        at: usize,
    },

    /// A line that only qualifies the one above it.
    Note,
}

impl LineKind {
    /// The staged operations this line is about, if any.
    ///
    /// One for an [`LineKind::Op`] row, every file's edit for a
    /// [`LineKind::Tag`] row, and none for everything else. What the pending
    /// view's `dd` takes off the plan.
    #[must_use]
    pub fn ops(&self) -> Vec<usize> {
        match self {
            Self::Op { op, .. } | Self::Detail { op } => vec![*op],
            Self::Tag { ops, .. } => ops.clone(),
            _ => Vec::new(),
        }
    }
}

impl Effects {
    /// Render the preview at `width` columns.
    ///
    /// No trailing newline: the caller decides whether this is a paragraph in a
    /// larger message or the whole of one.
    #[must_use]
    pub fn render(&self, width: usize) -> String {
        self.lines(width)
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The preview as lines that know what they are about.
    ///
    /// [`Effects::render`] is this, joined. See [`PreviewLine`] for why the TUI
    /// takes this shape and the CLI takes the string.
    #[must_use]
    pub fn lines(&self, width: usize) -> Vec<PreviewLine> {
        let width = width.max(MIN_WIDTH);
        let mut out: Vec<PreviewLine> = Vec::new();

        out.push(line(self.header(), LineKind::Header));
        for op in &self.ops {
            // Tag edits get their own section, by field rather than by file.
            if matches!(op.op, super::op::Operation::WriteTags { .. }) {
                continue;
            }
            out.push(line(
                op_line(op, width),
                LineKind::Op {
                    op: op.index,
                    refused: op.refused,
                },
            ));
            out.push(line(
                format!("{INDENT}{}", fit(&detail(op), width - INDENT.len())),
                LineKind::Detail { op: op.index },
            ));
        }
        out.extend(self.tag_rows(width));

        if !self.conflicts.is_empty() {
            out.push(line(String::new(), LineKind::Section));
            out.push(line(
                format!("Conflicts ({})", self.conflicts.len()),
                LineKind::Section,
            ));
            out.extend(self.conflicts.iter().enumerate().map(|(at, conflict)| {
                line(marked('x', conflict, width), LineKind::Conflict { at })
            }));
        }

        if !self.playlist_edits.is_empty() || !self.state_edits.is_empty() {
            out.push(line(String::new(), LineKind::Section));
            out.push(line("Playlists".to_owned(), LineKind::Section));
            out.extend(self.playlist_rows(width));
        }

        if !self.warnings.is_empty() {
            out.push(line(String::new(), LineKind::Section));
            out.extend(
                self.warnings.iter().enumerate().map(|(at, warning)| {
                    line(marked('!', warning, width), LineKind::Warning { at })
                }),
            );
        }

        out
    }

    /// The first line: how much is staged, and whether it can be committed.
    fn header(&self) -> String {
        if self.ops.is_empty() && self.conflicts.is_empty() {
            return "NOTHING PENDING".to_owned();
        }
        let ops = plural(self.ops.len(), "op", "ops");
        if self.conflicts.is_empty() {
            format!("PENDING ({ops})")
        } else {
            format!(
                "REFUSED ({ops}, {})",
                plural(self.conflicts.len(), "conflict", "conflicts")
            )
        }
    }

    /// One row per changed field across every staged tag edit, with how many
    /// files it is written to.
    ///
    /// Collapsed by `(field, edit)`, in the order fields are displayed, so an
    /// album-wide `--genre` is one line however many tracks it touches. A field
    /// whose value differs between files — `renumber tracks`, `title from
    /// filename` — collapses to one row per distinct value, and once there are
    /// more than [`MAX_TAG_VALUES`] of them it says `<per file>` instead of
    /// listing four hundred numbers.
    ///
    /// Each row carries the staged operations it stands for, because one row on
    /// screen has to be one thing the pending view can drop.
    fn tag_rows(&self, width: usize) -> Vec<PreviewLine> {
        use super::op::Operation;

        // `(field, rendered edit)` → the operations that write it, keeping field
        // order and then first-seen order within a field.
        let mut counts: Vec<((crate::tags::Field, String), Vec<usize>)> = Vec::new();
        let mut refused = false;
        for effect in &self.ops {
            let Operation::WriteTags { changes, .. } = &effect.op else {
                continue;
            };
            refused |= effect.refused;
            for (field, edit) in changes.edits() {
                let key = (*field, edit.rendered());
                match counts.iter_mut().find(|(seen, _)| *seen == key) {
                    Some((_, ops)) => ops.push(effect.index),
                    None => counts.push((key, vec![effect.index])),
                }
            }
        }
        if counts.is_empty() {
            return Vec::new();
        }
        counts.sort_by_key(|((field, _), _)| *field);

        let mut rows = Vec::new();
        let mut field = None;
        let mut group: Vec<(String, Vec<usize>)> = Vec::new();
        let mut flush = |field: crate::tags::Field, group: &mut Vec<(String, Vec<usize>)>| {
            let files: usize = group.iter().map(|(_, ops)| ops.len()).sum();
            let mut ops: Vec<usize> = group
                .iter()
                .flat_map(|(_, ops)| ops.iter().copied())
                .collect();
            ops.sort_unstable();
            ops.dedup();
            let body = if group.len() > MAX_TAG_VALUES {
                format!("{field} <per file>")
            } else {
                group
                    .iter()
                    .map(|(value, _)| format!("{field} = {value}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let label = format!("{:<7}", "TAG");
            let note = plural(files, "file", "files");
            let room = width.saturating_sub(label.chars().count() + note.len() + 4);
            rows.push(line(
                format!("{label}{}  {note}", fit(&body, room.max(8))),
                LineKind::Tag { ops, refused },
            ));
            group.clear();
        };
        for ((this, value), ops) in counts {
            if field.is_some_and(|seen| seen != this) {
                flush(field.expect("just checked"), &mut group);
            }
            field = Some(this);
            group.push((value, ops));
        }
        if let Some(field) = field {
            flush(field, &mut group);
        }
        if refused {
            rows.push(line(format!("{INDENT}REFUSED"), LineKind::Note));
        }
        rows
    }

    /// One row per affected playlist, plus MPD's saved queue when task 14 has
    /// filled it in. The names are padded to a common column so the counts line
    /// up; a name too long for the width is shortened like any other path.
    fn playlist_rows(&self, width: usize) -> Vec<PreviewLine> {
        let mut rows: Vec<(Option<usize>, String, String)> = self
            .playlist_edits
            .iter()
            .enumerate()
            .map(|(at, edit)| {
                (
                    Some(at),
                    edit.file_name.clone(),
                    lines_note(edit.rewrites(), edit.removals()),
                )
            })
            .collect();

        if !self.state_edits.is_empty() {
            let removed = self.state_edits.iter().filter(|e| e.is_removal()).count();
            rows.push((
                None,
                "MPD saved queue".to_owned(),
                lines_note(self.state_edits.len() - removed, removed),
            ));
        }

        // Two columns, each capped so that name + gap + note always fits.
        let widest_note = rows
            .iter()
            .map(|(_, _, note)| note.len())
            .max()
            .unwrap_or(0);
        let name_room = width
            .saturating_sub(PLAYLIST_INDENT + 1 + widest_note)
            .max(8);
        let name_column = rows
            .iter()
            .map(|(_, name, _)| fit(name, name_room).chars().count())
            .max()
            .unwrap_or(0);

        rows.into_iter()
            .map(|(at, name, note)| {
                let name = fit(&name, name_room);
                let pad = name_column.saturating_sub(name.chars().count());
                line(
                    format!(
                        "{:indent$}{name}{:pad$} {note}",
                        "",
                        "",
                        indent = PLAYLIST_INDENT,
                        pad = pad
                    ),
                    LineKind::Playlist { at },
                )
            })
            .collect()
    }
}

/// A [`PreviewLine`], at the call sites that build one of each.
fn line(text: String, kind: LineKind) -> PreviewLine {
    PreviewLine { text, kind }
}

/// `MOVE    from → to`, or `DELETE  target`.
fn op_line(effect: &OpEffect, width: usize) -> String {
    use super::op::Operation;

    let label = format!("{:<7}", effect.op.verb());
    let room = width.saturating_sub(label.chars().count());

    let body = match &effect.op {
        Operation::MoveFile { from, to } | Operation::MoveDir { from, to } => {
            // The arrow and its spaces are not negotiable; the paths share what
            // is left, and a short one lends its slack to a long one.
            let budget = room.saturating_sub(3);
            let (left, right) = share(from.as_str(), to.as_str(), budget);
            format!("{left} → {right}")
        }
        Operation::Delete { target } => tail(target.as_str(), room),
        // Rendered by `tag_rows`, which collapses them by field; this arm is
        // unreachable from `render` and is here so that a caller formatting one
        // operation on its own still gets something true.
        Operation::WriteTags { target, .. } => tail(target.as_str(), room),
    };

    format!("{label}{body}")
}

/// The line under an operation: what it costs.
fn detail(effect: &OpEffect) -> String {
    let mut parts = Vec::new();
    let aux = effect.files.saturating_sub(effect.audio);
    match (effect.audio, aux) {
        (0, 0) => parts.push("no files".to_owned()),
        (audio, 0) => parts.push(plural(audio, "audio file", "audio files")),
        (0, aux) => parts.push(plural(aux, "aux file", "aux files")),
        (audio, aux) => parts.push(format!("{audio} audio + {aux} aux files")),
    }
    if effect.bytes > 0 {
        parts.push(bytes(effect.bytes));
    }
    if effect.playlists > 0 {
        parts.push(plural(effect.playlists, "playlist", "playlists"));
    }
    if effect.refused {
        parts.push("REFUSED".to_owned());
    }
    parts.join(", ")
}

/// `x ` or `! ` and a message, shortened rather than folded: a preview that
/// reflows is a preview whose lines cannot be grepped.
fn marked(mark: char, message: &impl std::fmt::Display, width: usize) -> String {
    format!("{mark} {}", elide(&message.to_string(), width - 2))
}

// ---------------------------------------------------------------------------

/// `3 lines rewritten`, `1 line removed`, or both.
fn lines_note(rewritten: usize, removed: usize) -> String {
    let mut parts = Vec::new();
    if rewritten > 0 {
        parts.push(format!("{} rewritten", plural(rewritten, "line", "lines")));
    }
    if removed > 0 {
        parts.push(format!("{} removed", plural(removed, "line", "lines")));
    }
    if parts.is_empty() {
        parts.push("no change".to_owned());
    }
    parts.join(", ")
}

/// `1 op` / `2 ops`.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// A size a human reads at a glance. Decimal units, because that is what a
/// filesystem and a music player both report.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Give two paths a shared budget, letting the shorter one hand over its slack.
fn share(left: &str, right: &str, budget: usize) -> (String, String) {
    let (left_len, right_len) = (left.chars().count(), right.chars().count());
    if left_len + right_len <= budget {
        return (left.to_owned(), right.to_owned());
    }
    let half = budget / 2;
    // Whichever already fits in its half keeps what it has; the other takes the
    // rest. When both overflow they split it down the middle.
    let (left_room, right_room) = if left_len <= half {
        (left_len, budget - left_len)
    } else if right_len <= half {
        (budget - right_len, right_len)
    } else {
        (half, budget - half)
    };
    (tail(left, left_room), tail(right, right_room))
}

/// Keep the end of a path, marking what was dropped — the track and the album
/// carry more than the genre does.
fn tail(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    let kept: String = text.chars().skip(len - (width - 1)).collect();
    format!("…{kept}")
}

/// Keep both ends of a message and drop the middle.
///
/// A conflict reads `<a very long path> already exists`: the path identifies it
/// and the last few words are the verdict, so dropping either end leaves a line
/// that says nothing. What the middle of a scene-release directory name holds is
/// the release group and the bitrate, which is the part nobody needs here.
fn elide(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    // Two thirds to the head, which carries the path; the rest to the verdict.
    let head = (width - 1) * 2 / 3;
    let tail = width - 1 - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(len - tail).collect();
    format!("{start}…{end}")
}

/// Keep the start of a message, marking what was dropped. Used where the start
/// is the whole of the meaning — a two-column row's left-hand name.
fn fit(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

// ---------------------------------------------------------------------------

impl std::fmt::Display for Summary {
    /// The one-line version, for a log or a status bar.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}, {}, {}, {} across {}",
            plural(self.files_moved, "file moved", "files moved"),
            plural(self.files_deleted, "file deleted", "files deleted"),
            plural(self.tags_written, "file retagged", "files retagged"),
            plural(self.lines_rewritten, "line rewritten", "lines rewritten"),
            plural(self.playlists_affected, "playlist", "playlists"),
        )
    }
}

impl Conflict {
    /// The one-line rendering the preview uses. Same as [`Display`][std::fmt::Display];
    /// named so a caller reads at the call site that this is user-facing text.
    #[must_use]
    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl Warning {
    /// The one-line rendering the preview uses.
    #[must_use]
    pub fn message(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_its_end_when_it_has_to_be_shortened() {
        let path = "hiphop/MF DOOM - Mm..Food (2004)/01 Beef Rap.mp3";
        assert_eq!(tail(path, 20), "…04)/01 Beef Rap.mp3");
        assert_eq!(tail(path, 20).chars().count(), 20);
        assert_eq!(tail(path, 200), path);
    }

    #[test]
    fn a_message_keeps_its_start_when_it_has_to_be_shortened() {
        assert_eq!(fit("destination exists", 10), "destinati…");
        assert_eq!(fit("short", 10), "short");
    }

    #[test]
    fn a_conflict_keeps_both_the_path_and_the_verdict() {
        let message = "operation 0: hiphop/A Very Long Scene Release Name [320]                        tag/01 Track.mp3 already exists";
        let short = elide(message, 60);

        assert_eq!(short.chars().count(), 60);
        assert!(
            short.starts_with("operation 0: hiphop/A Very Long"),
            "{short}"
        );
        assert!(short.ends_with("already exists"), "{short}");
        assert_eq!(elide(message, 500), message);
    }

    #[test]
    fn shortening_is_by_character_not_by_byte() {
        // Eight characters, sixteen bytes.
        let text = "ααααααββ";
        assert_eq!(tail(text, 4).chars().count(), 4);
        assert_eq!(fit(text, 4).chars().count(), 4);
    }

    #[test]
    fn the_shorter_path_lends_its_slack_to_the_longer_one() {
        let (left, right) = share("a/b.mp3", "very/long/destination/path/b.mp3", 20);
        assert_eq!(left, "a/b.mp3");
        assert_eq!(left.chars().count() + right.chars().count(), 20);
    }

    #[test]
    fn sizes_read_in_decimal_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_000), "1.0 kB");
        assert_eq!(bytes(61_200_000), "61.2 MB");
    }

    // -- the two shapes ----------------------------------------------------

    /// A plan with one of everything in it: two operations, a tag edit, a
    /// conflict, two playlists and a warning.
    fn everything() -> Effects {
        use crate::paths::RelPath;
        use crate::playlist::rewrite::{LineEdit, PlaylistEdit};
        use crate::tags::{Field, TagDelta};

        use super::super::op::Operation;

        let rel = |s: &str| RelPath::parse(s).expect("a relative path");
        let effect = |index: usize, op: Operation, refused: bool| OpEffect {
            index,
            op,
            files: 3,
            audio: 2,
            bytes: 4_200_000,
            playlists: 1,
            refused,
        };

        Effects {
            ops: vec![
                effect(
                    0,
                    Operation::MoveDir {
                        from: rel("hiphop/MF DOOM - Mm Food"),
                        to: rel("hiphop/MF DOOM/2004 - Mm..Food"),
                    },
                    false,
                ),
                effect(
                    1,
                    Operation::Delete {
                        target: rel("hiphop/MF DOOM - Mm Food/folder.nfo"),
                    },
                    true,
                ),
                effect(
                    2,
                    Operation::WriteTags {
                        target: rel("hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3"),
                        changes: TagDelta::new().set(Field::Genre, "Hip Hop"),
                    },
                    false,
                ),
            ],
            conflicts: vec![Conflict::DeleteDisabled {
                op: 1,
                target: rel("hiphop/MF DOOM - Mm Food/folder.nfo"),
            }],
            playlist_edits: vec![PlaylistEdit {
                playlist: 0,
                file_name: "Coding flow.m3u".to_owned(),
                real_path: "/playlists/Coding flow.m3u".into(),
                line_edits: vec![LineEdit {
                    entry: 2,
                    old: "hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3".to_owned(),
                    new: Some("hiphop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3".to_owned()),
                }],
            }],
            state_edits: vec![LineEdit {
                entry: 7,
                old: "hiphop/MF DOOM - Mm Food/02 Hoe Cakes.mp3".to_owned(),
                new: None,
            }],
            warnings: vec![Warning::InMpdQueue {
                path: rel("hiphop/MF DOOM - Mm Food/01 Beef Rap.mp3"),
            }],
            ..Effects::default()
        }
    }

    #[test]
    fn the_string_the_cli_prints_is_the_lines_the_tui_draws() {
        let effects = everything();
        for width in [40, 72, 80, 200] {
            let joined = effects
                .lines(width)
                .into_iter()
                .map(|line| line.text)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(joined, effects.render(width), "at {width} columns");
        }
    }

    #[test]
    fn how_many_lines_there_are_and_what_they_are_about_does_not_depend_on_the_width() {
        // What `docs/tasks/24-pending-view.md` needs from this: the pending view
        // works out what the cursor can land on without knowing how wide its
        // pane is, and unfolds a playlist's diff under a row it identified at
        // one width and drew at another.
        let effects = everything();
        let kinds = |width| {
            effects
                .lines(width)
                .into_iter()
                .map(|line| line.kind)
                .collect::<Vec<_>>()
        };
        let at_eighty = kinds(80);
        for width in [1, 40, 41, 79, 120, 1000] {
            assert_eq!(kinds(width), at_eighty, "at {width} columns");
        }
    }

    #[test]
    fn every_row_knows_which_staged_operations_it_stands_for() {
        let lines = everything().lines(80);
        let ops_of = |kind: fn(&LineKind) -> bool| {
            lines
                .iter()
                .filter(|line| kind(&line.kind))
                .map(|line| line.kind.ops())
                .collect::<Vec<_>>()
        };

        // The move and the delete, in execution order, one operation each.
        assert_eq!(
            ops_of(|kind| matches!(kind, LineKind::Op { .. })),
            vec![vec![0], vec![1]]
        );
        // The tag edit is rendered by field, and the field's row stands for the
        // one operation that writes it — fourteen of them for an album.
        assert_eq!(
            ops_of(|kind| matches!(kind, LineKind::Tag { .. })),
            vec![vec![2]]
        );
        // The conflict named operation 1, so its row says so.
        assert!(lines.iter().any(|line| matches!(
            line.kind,
            LineKind::Op {
                op: 1,
                refused: true
            }
        )));
        // A section heading is about no operation at all, so nothing can be
        // dropped from one.
        assert!(
            lines
                .iter()
                .filter(|line| matches!(line.kind, LineKind::Section | LineKind::Header))
                .all(|line| line.kind.ops().is_empty())
        );
    }

    #[test]
    fn the_playlist_rows_name_their_edits_and_the_saved_queue_names_none() {
        let lines = everything().lines(80);
        let playlists: Vec<(Option<usize>, String)> = lines
            .iter()
            .filter_map(|line| match line.kind {
                LineKind::Playlist { at } => Some((at, line.text.clone())),
                _ => None,
            })
            .collect();

        assert_eq!(playlists.len(), 2, "{playlists:?}");
        assert_eq!(playlists[0].0, Some(0));
        assert!(
            playlists[0].1.starts_with("  Coding flow.m3u"),
            "{playlists:?}"
        );
        // MPD's saved queue is not in `playlist_edits`, and the pending view has
        // to be able to tell: its diff comes from `state_edits` instead.
        assert_eq!(playlists[1].0, None);
        assert!(playlists[1].1.contains("MPD saved queue"), "{playlists:?}");

        // The indent the pending view puts its expansion marker in.
        for (_, text) in &playlists {
            assert!(text.starts_with(&" ".repeat(PLAYLIST_INDENT)), "{text:?}");
        }
    }
}
