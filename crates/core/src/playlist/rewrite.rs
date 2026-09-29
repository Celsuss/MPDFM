//! Turning a set of path moves into the exact playlist lines that have to
//! change — and then changing those and nothing else.
//!
//! ```no_run
//! use camino::Utf8Path;
//! use mpdfm_core::paths::RelPath;
//! use mpdfm_core::playlist::PlaylistIndex;
//! use mpdfm_core::playlist::rewrite::{self, PathMove};
//!
//! let (index, _warnings) = PlaylistIndex::load(Utf8Path::new("/home/me/.config/mpd/playlists"));
//! let moves = [PathMove::moved(
//!     RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)/01 Beef Rap.mp3")?,
//!     RelPath::parse("hiphop/MF DOOM/Mm..Food (2004)/01 Beef Rap.mp3")?,
//! )];
//!
//! // Pure: this is what the preview renders (task 10).
//! let edits = rewrite::plan_playlist_edits(&index, &moves);
//!
//! // Backs every affected playlist up, then writes them.
//! rewrite::apply(&edits, Utf8Path::new("/home/me/.local/share/mpdfm/backups/tx"))?;
//! # Ok::<(), mpdfm_core::Error>(())
//! ```
//!
//! # The one matching rule
//!
//! A line is rewritten when its parsed [`RelPath`] **equals** a moved path.
//! That is the whole rule (`docs/PLAN.md` safety invariant 3), and everything
//! else here exists to keep it that way:
//!
//! - the lookup is [`PlaylistIndex::refs_to`], which is a hash lookup on the
//!   parsed identity. There is no prefix test, no `str::replace`, no
//!   case folding and no Unicode normalization anywhere in this module;
//! - a directory move never reaches [`plan_playlist_edits`] as a directory. It
//!   is expanded into one [`PathMove`] per referenced file first — by
//!   [`expand_dir_move`] here, or by the planner in task 10 — and the expansion
//!   itself goes through [`RelPath::starts_with_dir`], which is component-wise.
//!   That is what makes `hiphop/MF DOOM` leave
//!   `hiphop/MF DOOM Instrumentals` alone, where a string prefix would have
//!   moved half of someone's library;
//! - a CUE virtual track is keyed under its `.cue` sheet, so
//!   `pop/old/a.flac.cue/track0017` is rewritten when the *sheet* moves, and the
//!   `/track0017` component travels with it untouched. The rebuilt line comes
//!   from [`Entry::track`], never from editing the old string.
//!
//! Every other line — a radio URL, an `#EXTINF`, a comment, a blank, a Windows
//! path MPDFM cannot read — is not in the index at all, is therefore never named
//! by an edit, and is written back byte-for-byte by task 06's writer.
//!
//! # Deletions are different, and are treated as different
//!
//! A move rewrites a line; a delete *removes* one, along with the `#EXTINF`
//! immediately above it when there is one (an orphaned `#EXTINF` would title the
//! next track). That loses something the user wrote, which a move does not,
//! so [`PlaylistEdit::removals`] is counted separately from
//! [`PlaylistEdit::rewrites`] and the preview reports it prominently.
//!
//! Only an `#EXTINF` that *immediately* precedes the removed line goes with it.
//! Playlists in the wild put the directive somewhere else, or nowhere, and
//! searching upwards for one would eventually eat the previous track's.
//!
//! # Backup first, then write
//!
//! [`apply`] runs in three passes, and the order is the point:
//!
//! 1. read and verify every affected playlist, and compute its new bytes.
//!    Nothing is written, so an edit that no longer matches the file fails with
//!    the library and the playlists exactly as they were;
//! 2. copy every one of them into the transaction's backup directory, keeping
//!    its name, and `fsync`. **Now** there is something to undo with;
//! 3. replace them one at a time, each through task 06's atomic temp-file +
//!    `rename` (and therefore through a resolved symlink, which is what makes
//!    the dotfiles-repo `Radios.m3u` work).
//!
//! A failure in pass 3 leaves the playlists before it written and the ones after
//! it untouched, and every one of the five has a backup — so [`restore`] puts
//! the whole set back regardless of where it stopped. Task 11 owns the journal
//! that records which pass was reached; this module owns being safe to resume
//! from either side of it.

use std::collections::BTreeMap;
use std::io::Write as _;

use camino::{Utf8Path, Utf8PathBuf};

use super::{Entry, Playlist, PlaylistIndex, Ref, parse, write};
use crate::paths::{PathError, RelPath};
use crate::{Error, Result};

/// What happens to one path, as the playlists spell it.
///
/// `to` is `None` for a deletion. A deletion is a different kind of edit — it
/// removes a line instead of changing one — but it selects its lines by the same
/// exact match, so it belongs in the same list rather than in a parallel one
/// that could fall out of step with it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PathMove {
    /// The path the playlists name today.
    pub from: RelPath,
    /// Where it lands, or `None` when the file is being deleted.
    pub to: Option<RelPath>,
}

impl PathMove {
    /// `from` moves to `to`.
    #[must_use]
    pub fn moved(from: RelPath, to: RelPath) -> Self {
        Self { from, to: Some(to) }
    }

    /// `target` is being deleted, so its lines go away.
    #[must_use]
    pub fn deleted(target: RelPath) -> Self {
        Self {
            from: target,
            to: None,
        }
    }

    /// Whether this removes lines rather than rewriting them.
    #[must_use]
    pub fn is_delete(&self) -> bool {
        self.to.is_none()
    }
}

/// One line to change, named by its position and guarded by its current text.
///
/// `old` is not decoration: [`apply`] refuses an edit whose line no longer reads
/// that way. A `PlaylistEdit` is planned during the preview, written into the
/// journal, and applied some time later — possibly after a crash and a restart —
/// and "line 7 of Pop.m3u" on its own is not enough to be sure it still names
/// what it named when the plan was made.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LineEdit {
    /// 0-based index into the playlist's [`Playlist::entries`]. Add one before
    /// showing it to a user.
    pub entry: usize,
    /// The line's exact bytes as the plan saw them.
    pub old: String,
    /// The line's bytes afterwards, or `None` when the line is removed.
    pub new: Option<String>,
}

impl LineEdit {
    /// Whether this removes the line instead of rewriting it.
    #[must_use]
    pub fn is_removal(&self) -> bool {
        self.new.is_none()
    }
}

/// Every line to change in one playlist.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlaylistEdit {
    /// Index into [`PlaylistIndex::playlists`] of the index this was planned
    /// against. Only meaningful to that index — it is rebuilt after every
    /// operation — so the fields below are what [`apply`] actually uses.
    pub playlist: usize,

    /// The playlist's file name as the playlist directory spells it, e.g.
    /// `Hip hop.m3u`. This is what the backup copy is called, and what a report
    /// names — for a symlinked playlist it is the link's name, which is the one
    /// the user knows.
    pub file_name: String,

    /// The file to replace: the playlist's path with symlinks resolved. Writing
    /// here rather than to the link is what keeps `Radios.m3u` a symlink into the
    /// dotfiles repository instead of turning it into a regular file.
    pub real_path: Utf8PathBuf,

    /// The lines to change, in ascending line order, each line named once.
    pub line_edits: Vec<LineEdit>,
}

impl PlaylistEdit {
    /// How many lines are rewritten in place.
    #[must_use]
    pub fn rewrites(&self) -> usize {
        self.line_edits
            .iter()
            .filter(|edit| !edit.is_removal())
            .count()
    }

    /// How many lines are removed — the destructive half, reported separately
    /// because it is the half that loses something.
    #[must_use]
    pub fn removals(&self) -> usize {
        self.line_edits
            .iter()
            .filter(|edit| edit.is_removal())
            .count()
    }
}

// ---------------------------------------------------------------------------

/// Which lines each move touches, and what they become.
///
/// Pure: no disk access, no ordering assumptions, and calling it twice with the
/// same arguments gives the same answer. This is what task 10's preview renders
/// and what task 11 writes into the journal before anything is mutated.
///
/// Playlists come back in index order and lines in file order, so a preview reads
/// top to bottom. A playlist no move touches is not in the result at all.
///
/// Two moves naming the same `from` is a planner bug — task 10 reports it as a
/// conflict — and is resolved here by keeping the first, so that the result stays
/// deterministic instead of depending on iteration order.
#[must_use]
pub fn plan_playlist_edits(index: &PlaylistIndex, moves: &[PathMove]) -> Vec<PlaylistEdit> {
    // Keyed both ways so the result is ordered and each line is named once, no
    // matter what order the moves arrive in.
    let mut by_playlist: BTreeMap<usize, BTreeMap<usize, LineEdit>> = BTreeMap::new();

    for path_move in moves {
        for &reference in index.refs_to(&path_move.from) {
            let Some(entry) = index.entry(reference) else {
                continue;
            };
            // The index only ever keys track lines, and only under their own
            // parsed path. Re-checking costs nothing and means a stale or
            // hand-built index cannot make this module rewrite the wrong line.
            if entry.rel() != Some(&path_move.from) {
                continue;
            }

            let lines = by_playlist.entry(reference.playlist).or_default();
            let new = path_move.to.as_ref().map(|to| {
                // Rebuilt from the parts, so the CUE suffix survives and the
                // written bytes cannot disagree with the parsed path.
                Entry::track(to.clone(), entry.cue().map(str::to_owned))
                    .line()
                    .to_owned()
            });
            lines.entry(reference.entry).or_insert(LineEdit {
                entry: reference.entry,
                old: entry.line().to_owned(),
                new,
            });

            if path_move.is_delete() {
                remove_leading_ext_inf(index, reference, lines);
            }
        }
    }

    by_playlist
        .into_iter()
        .filter_map(|(playlist, lines)| {
            let source = index.playlist(playlist)?;
            Some(PlaylistEdit {
                playlist,
                file_name: file_name_of(source.path()),
                real_path: source.real_path().to_owned(),
                line_edits: lines.into_values().collect(),
            })
        })
        .collect()
}

/// A directory move, as the *playlists* see it: one [`PathMove`] per referenced
/// file inside it.
///
/// This is the playlist half of the expansion task 10 does — it walks the
/// index's referenced paths, not the filesystem, so it says nothing about the
/// cover art and `.nfo` files that also have to move. Both halves exist because
/// they answer different questions, and both go through
/// [`RelPath::starts_with_dir`], so neither can drift into prefix matching.
///
/// A line naming `from` itself is not included: a playlist entry that is the
/// directory is not a track, and a *file* move is [`PathMove::moved`].
///
/// # Errors
///
/// [`PathError`] if a referenced path cannot be reparented onto `to`, which
/// means `from` was not a prefix of it after all.
pub fn expand_dir_move(
    index: &PlaylistIndex,
    from: &RelPath,
    to: &RelPath,
) -> std::result::Result<Vec<PathMove>, PathError> {
    index
        .refs_under_dir(from)
        .into_iter()
        .map(|(path, _refs)| {
            let landing = path.reparent(from, to)?;
            Ok(PathMove::moved(path, landing))
        })
        .collect()
}

/// The `#EXTINF` directly above a removed line, which goes with it.
///
/// Only the line immediately above, and only if it really is an `#EXTINF`: it
/// cannot already be claimed by another edit, because the only other line a
/// removal claims is a track, and the only line a move claims is a track.
fn remove_leading_ext_inf(
    index: &PlaylistIndex,
    reference: Ref,
    lines: &mut BTreeMap<usize, LineEdit>,
) {
    let Some(above) = reference.entry.checked_sub(1) else {
        return;
    };
    let above = Ref {
        playlist: reference.playlist,
        entry: above,
    };
    let Some(entry @ Entry::ExtInf { .. }) = index.entry(above) else {
        return;
    };
    lines.entry(above.entry).or_insert(LineEdit {
        entry: above.entry,
        old: entry.line().to_owned(),
        new: None,
    });
}

/// The file name of a playlist path, for the backup copy and for reports.
///
/// A playlist always has one — it was found by listing a directory. The fallback
/// only exists so that a hand-built [`Playlist`] cannot panic here.
fn file_name_of(path: &Utf8Path) -> String {
    path.file_name().unwrap_or("playlist.m3u").to_owned()
}

// ---------------------------------------------------------------------------

/// A failure only a test needs, because it cannot be produced from outside the
/// process.
///
/// Making a single playlist's write fail — the third of five, with the first two
/// already replaced — needs a directory that is writable for two files and not
/// for the third, which is not a thing a filesystem offers. The precedent is
/// [`crate::ops::exec_fs::Inject`] and task 06's `Stop`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Inject {
    /// Behave normally.
    #[default]
    Nothing,

    /// Fail just before writing the playlist at this position in `edits`, with
    /// the backups of all of them already on disk. Out of range means "never",
    /// so a test can assert the uninjected run too.
    FailBeforeWriting(usize),
}

/// Why a rewrite was refused.
///
/// Every variant names the playlist and, where there is one, the line — an error
/// from the middle of a five-playlist rewrite that does not say which file and
/// which line is not worth raising.
#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    /// The line is not what the plan saw. The file was edited between the
    /// preview and the commit, or the edit came from a journal written against a
    /// different version of it. Nothing is written.
    #[error(
        "{playlist} line {line}: the plan expected {expected:?} but the file says {found:?}",
        line = line + 1
    )]
    Stale {
        /// The playlist.
        playlist: Utf8PathBuf,
        /// The 0-based line index; the message adds one.
        line: usize,
        /// What [`LineEdit::old`] said.
        expected: String,
        /// What the file says now.
        found: String,
    },

    /// The edit names a line past the end of the file — the same staleness, in
    /// the shape a shortened playlist produces.
    #[error(
        "{playlist} has {lines} lines; the plan names line {line}",
        line = line + 1
    )]
    PastTheEnd {
        /// The playlist.
        playlist: Utf8PathBuf,
        /// The 0-based line index; the message adds one.
        line: usize,
        /// How many lines the file actually has.
        lines: usize,
    },

    /// Two [`LineEdit`]s name the same line. Applying either and dropping the
    /// other would be a silent choice between two intentions, so neither is
    /// applied.
    #[error("{playlist} line {line}: two edits name the same line", line = line + 1)]
    Conflict {
        /// The playlist.
        playlist: Utf8PathBuf,
        /// The 0-based line index; the message adds one.
        line: usize,
    },

    /// The backup directory already holds a file of that name. MPDFM never
    /// overwrites a backup — it is the only copy of what is about to change.
    #[error("{path} already exists; refusing to overwrite a backup")]
    BackupExists {
        /// The backup that is already there.
        path: Utf8PathBuf,
    },

    /// [`Inject::FailBeforeWriting`] fired. Never raised in production.
    #[error("simulated failure before writing {playlist}")]
    Injected {
        /// The playlist that was about to be written.
        playlist: Utf8PathBuf,
    },
}

/// Back up every affected playlist, then apply the edits.
///
/// `backup_dir` is the transaction's own directory under MPDFM's data directory;
/// it is created if it is not there. Each playlist is copied into it under
/// [`PlaylistEdit::file_name`], which is what [`restore`] looks for.
///
/// See the [module docs][self] for the three passes and what a failure in each
/// one leaves behind.
///
/// # Errors
///
/// [`Error::Rewrite`] for an edit that no longer matches its file or a backup
/// name that is already taken — in both cases before anything is written —
/// [`Error::Playlist`] for a playlist whose bytes are not UTF-8, and
/// [`Error::Io`] for a read, a backup or a write that fails.
pub fn apply(edits: &[PlaylistEdit], backup_dir: &Utf8Path) -> Result<()> {
    apply_with(edits, backup_dir, Inject::Nothing)
}

/// [`apply`], with the failure injection the atomicity tests need.
///
/// # Errors
///
/// As [`apply`], plus [`RewriteError::Injected`] when `inject` fires.
pub fn apply_with(edits: &[PlaylistEdit], backup_dir: &Utf8Path, inject: Inject) -> Result<()> {
    // Pass 1: read, verify, compute. Nothing is written, so a stale edit — or a
    // playlist that has stopped being UTF-8 — costs nothing.
    let mut pending = Vec::with_capacity(edits.len());
    for edit in edits {
        let original = read(&edit.real_path)?;
        let playlist = Playlist::from_bytes(&edit.real_path, &original)?;
        let updated = rewritten(&playlist, edit)?.to_bytes();
        pending.push((edit, original, updated));
    }

    // Pass 2: every backup, before the first modification.
    std::fs::create_dir_all(backup_dir).map_err(|source| Error::Io {
        path: backup_dir.to_string(),
        source,
    })?;
    for (edit, original, _) in &pending {
        back_up(backup_dir, &edit.file_name, original)?;
    }
    sync_dir(backup_dir);

    // Pass 3: write. Each one is atomic on its own; the set is not, which is
    // what the backups and the journal are for.
    for (position, (edit, _, updated)) in pending.iter().enumerate() {
        if inject == Inject::FailBeforeWriting(position) {
            return Err(RewriteError::Injected {
                playlist: edit.real_path.clone(),
            }
            .into());
        }
        write::replace_file(&edit.real_path, updated, write::Stop::Never)?;
    }
    Ok(())
}

/// Put every playlist in `edits` back from its backup.
///
/// This is the playlist half of undo (task 12), and it is deliberately
/// unconditional: a playlist pass 3 never reached is restored to the bytes it
/// already has, which is a no-op worth doing rather than a state worth
/// reasoning about. Writing goes through the resolved
/// [`PlaylistEdit::real_path`], so a restored symlinked playlist is still a
/// symlink.
///
/// # Errors
///
/// [`Error::Io`] if a backup cannot be read or a playlist cannot be replaced.
pub fn restore(edits: &[PlaylistEdit], backup_dir: &Utf8Path) -> Result<()> {
    for edit in edits {
        let bytes = read(&backup_dir.join(&edit.file_name))?;
        write::replace_file(&edit.real_path, &bytes, write::Stop::Never)?;
    }
    Ok(())
}

/// The playlist with `edit` applied, or the reason it cannot be.
///
/// Every line is checked before any is changed, so a rejected edit leaves the
/// caller with the playlist it passed in. The new entries are re-classified from
/// their text rather than trusted as strings: a replacement line that did not
/// parse back to the track it claims to be is a bug this would surface.
fn rewritten(
    playlist: &Playlist,
    edit: &PlaylistEdit,
) -> std::result::Result<Playlist, RewriteError> {
    let entries = playlist.entries();
    let mut by_line: BTreeMap<usize, &LineEdit> = BTreeMap::new();

    for line_edit in &edit.line_edits {
        let Some(entry) = entries.get(line_edit.entry) else {
            return Err(RewriteError::PastTheEnd {
                playlist: edit.real_path.clone(),
                line: line_edit.entry,
                lines: entries.len(),
            });
        };
        if entry.line() != line_edit.old {
            return Err(RewriteError::Stale {
                playlist: edit.real_path.clone(),
                line: line_edit.entry,
                expected: line_edit.old.clone(),
                found: entry.line().to_owned(),
            });
        }
        if by_line.insert(line_edit.entry, line_edit).is_some() {
            return Err(RewriteError::Conflict {
                playlist: edit.real_path.clone(),
                line: line_edit.entry,
            });
        }
    }

    // Rebuilt in one pass rather than mutated in place: a removal shifts every
    // index after it, and an off-by-one there would rewrite the wrong line.
    let rebuilt: Vec<Entry> = entries
        .iter()
        .enumerate()
        .filter_map(|(line, entry)| match by_line.get(&line) {
            None => Some(entry.clone()),
            Some(line_edit) => line_edit.new.as_deref().map(parse::classify),
        })
        .collect();

    let mut out = playlist.clone();
    *out.entries_mut() = rebuilt;
    Ok(out)
}

/// Copy one playlist's bytes into the backup directory, never over something
/// already there.
fn back_up(backup_dir: &Utf8Path, file_name: &str, bytes: &[u8]) -> Result<()> {
    let path = backup_dir.join(file_name);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(RewriteError::BackupExists { path }.into());
        }
        Err(source) => {
            return Err(Error::Io {
                path: path.to_string(),
                source,
            });
        }
    };
    let write = file.write_all(bytes).and_then(|()| file.sync_all());
    write.map_err(|source| Error::Io {
        path: path.to_string(),
        source,
    })
}

/// Read a file, naming it in the error.
fn read(path: &Utf8Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_string(),
        source,
    })
}

/// Make the backup directory's new names durable.
///
/// Best effort: the bytes are already `fsync`ed, and failing to sync the
/// directory is not a reason to refuse to make the edits.
fn sync_dir(dir: &Utf8Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A playlist parsed from lines, with no disk anywhere near it.
    fn playlist(lines: &[&str]) -> Playlist {
        let body = lines.join("\n") + "\n";
        Playlist::from_bytes(Utf8Path::new("/p/Test.m3u"), body.as_bytes())
            .expect("the test's own bytes are UTF-8")
    }

    /// An edit against `/p/Test.m3u`, from its lines.
    fn edit(line_edits: Vec<LineEdit>) -> PlaylistEdit {
        PlaylistEdit {
            playlist: 0,
            file_name: "Test.m3u".to_owned(),
            real_path: Utf8PathBuf::from("/p/Test.m3u"),
            line_edits,
        }
    }

    fn rewrite(entry: usize, old: &str, new: &str) -> LineEdit {
        LineEdit {
            entry,
            old: old.to_owned(),
            new: Some(new.to_owned()),
        }
    }

    fn remove(entry: usize, old: &str) -> LineEdit {
        LineEdit {
            entry,
            old: old.to_owned(),
            new: None,
        }
    }

    #[test]
    fn a_rewritten_line_is_reparsed_rather_than_stored_as_text() {
        let before = playlist(&["a/b.mp3"]);
        let after = rewritten(&before, &edit(vec![rewrite(0, "a/b.mp3", "x/y.mp3")]))
            .expect("the line matches");

        let entry = &after.entries()[0];
        assert_eq!(entry.line(), "x/y.mp3");
        assert_eq!(
            entry.rel().map(RelPath::as_str),
            Some("x/y.mp3"),
            "a rewritten line has to come back out as a track, not as text"
        );
    }

    #[test]
    fn removals_do_not_shift_the_lines_that_follow_them() {
        let before = playlist(&["#EXTM3U", "a/1.mp3", "a/2.mp3", "a/3.mp3"]);
        let after = rewritten(
            &before,
            &edit(vec![remove(1, "a/1.mp3"), rewrite(3, "a/3.mp3", "b/3.mp3")]),
        )
        .expect("both lines match");

        let lines: Vec<&str> = after.entries().iter().map(Entry::line).collect();
        assert_eq!(lines, ["#EXTM3U", "a/2.mp3", "b/3.mp3"]);
    }

    #[test]
    fn a_line_that_has_changed_since_the_plan_is_refused() {
        let before = playlist(&["a/b.mp3"]);
        let err = rewritten(&before, &edit(vec![rewrite(0, "a/OTHER.mp3", "x/y.mp3")]))
            .expect_err("the file no longer says what the plan saw");

        assert!(
            matches!(err, RewriteError::Stale { line: 0, .. }),
            "expected a stale line 0, got {err}"
        );
        // Line numbers are 1-based in the message, 0-based in the data.
        assert!(err.to_string().contains("line 1"), "{err}");
    }

    #[test]
    fn a_line_past_the_end_is_refused() {
        let before = playlist(&["a/b.mp3"]);
        let err = rewritten(&before, &edit(vec![rewrite(9, "a/b.mp3", "x/y.mp3")]))
            .expect_err("there is no line 10");

        assert!(
            matches!(err, RewriteError::PastTheEnd { lines: 1, .. }),
            "expected a past-the-end error, got {err}"
        );
    }

    #[test]
    fn two_edits_naming_one_line_are_refused_rather_than_resolved() {
        let before = playlist(&["a/b.mp3"]);
        let err = rewritten(
            &before,
            &edit(vec![
                rewrite(0, "a/b.mp3", "x/y.mp3"),
                rewrite(0, "a/b.mp3", "z/w.mp3"),
            ]),
        )
        .expect_err("two intentions for one line is not something to guess at");

        assert!(
            matches!(err, RewriteError::Conflict { line: 0, .. }),
            "expected a conflict, got {err}"
        );
    }

    #[test]
    fn a_refused_edit_changes_nothing_even_when_an_earlier_line_matched() {
        let before = playlist(&["a/1.mp3", "a/2.mp3"]);
        let _ = rewritten(
            &before,
            &edit(vec![
                rewrite(0, "a/1.mp3", "b/1.mp3"),
                rewrite(1, "a/NOPE.mp3", "b/2.mp3"),
            ]),
        )
        .expect_err("the second line does not match");

        let lines: Vec<&str> = before.entries().iter().map(Entry::line).collect();
        assert_eq!(lines, ["a/1.mp3", "a/2.mp3"]);
    }

    #[test]
    fn the_file_s_own_properties_survive_a_rewrite() {
        let before = Playlist::from_bytes(
            Utf8Path::new("/p/Test.m3u"),
            "\u{feff}#EXTM3U\r\na/b.mp3".as_bytes(),
        )
        .expect("UTF-8");
        assert!(before.has_bom() && !before.trailing_newline());

        let after = rewritten(&before, &edit(vec![rewrite(1, "a/b.mp3", "x/y.mp3")]))
            .expect("the line matches");

        assert_eq!(after.to_bytes(), "\u{feff}#EXTM3U\r\nx/y.mp3".as_bytes());
    }
}
