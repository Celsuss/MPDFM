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

impl Effects {
    /// Render the preview at `width` columns.
    ///
    /// No trailing newline: the caller decides whether this is a paragraph in a
    /// larger message or the whole of one.
    #[must_use]
    pub fn render(&self, width: usize) -> String {
        let width = width.max(MIN_WIDTH);
        let mut out: Vec<String> = Vec::new();

        out.push(self.header());
        for op in &self.ops {
            out.push(op_line(op, width));
            out.push(format!(
                "{INDENT}{}",
                fit(&detail(op), width - INDENT.len())
            ));
        }

        if !self.conflicts.is_empty() {
            out.push(String::new());
            out.push(format!("Conflicts ({})", self.conflicts.len()));
            out.extend(
                self.conflicts
                    .iter()
                    .map(|conflict| marked('x', conflict, width)),
            );
        }

        if !self.playlist_edits.is_empty() || !self.state_edits.is_empty() {
            out.push(String::new());
            out.push("Playlists".to_owned());
            out.extend(self.playlist_rows(width));
        }

        if !self.warnings.is_empty() {
            out.push(String::new());
            out.extend(
                self.warnings
                    .iter()
                    .map(|warning| marked('!', warning, width)),
            );
        }

        out.join("\n")
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

    /// One row per affected playlist, plus MPD's saved queue when task 14 has
    /// filled it in. The names are padded to a common column so the counts line
    /// up; a name too long for the width is shortened like any other path.
    fn playlist_rows(&self, width: usize) -> Vec<String> {
        let mut rows: Vec<(String, String)> = self
            .playlist_edits
            .iter()
            .map(|edit| {
                (
                    edit.file_name.clone(),
                    lines_note(edit.rewrites(), edit.removals()),
                )
            })
            .collect();

        if !self.state_edits.is_empty() {
            let removed = self.state_edits.iter().filter(|e| e.is_removal()).count();
            rows.push((
                "MPD saved queue".to_owned(),
                lines_note(self.state_edits.len() - removed, removed),
            ));
        }

        // Two columns, each capped so that name + gap + note always fits.
        let widest_note = rows.iter().map(|(_, note)| note.len()).max().unwrap_or(0);
        let name_room = width.saturating_sub(2 + 1 + widest_note).max(8);
        let name_column = rows
            .iter()
            .map(|(name, _)| fit(name, name_room).chars().count())
            .max()
            .unwrap_or(0);

        rows.into_iter()
            .map(|(name, note)| {
                let name = fit(&name, name_room);
                let pad = name_column.saturating_sub(name.chars().count());
                format!("  {name}{:pad$} {note}", "", pad = pad)
            })
            .collect()
    }
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
            "{}, {}, {} across {}",
            plural(self.files_moved, "file moved", "files moved"),
            plural(self.files_deleted, "file deleted", "files deleted"),
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
}
