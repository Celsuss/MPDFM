//! [`Plan::validate`][super::op::Plan::validate]: work out everything a plan
//! would change, and change none of it.
//!
//! Four passes, in this order, because each needs the one before it:
//!
//! 1. **order** — sort the operations so that a destination another operation is
//!    about to vacate is vacated first. A ring with no such order is
//!    [`Conflict::Cycle`];
//! 2. **expand** — turn each operation into [`FsStep`]s, walking a directory move
//!    through [`exec_fs::expand_dir_move`] so that the merge and collision rules
//!    exist in exactly one place;
//! 3. **check** — hand every step to [`exec_fs::check`], which is the same code
//!    commit will run, and turn what it says into [`Conflict`]s and [`Warning`]s;
//! 4. **derive** — the playlist edits, the library-level warnings and the
//!    [`Summary`], all from the steps produced in pass 2 rather than from the
//!    operations, so the preview counts what commit does.
//!
//! # Nothing is written
//!
//! Pass 3 calls [`exec_fs::check`] and never `execute`. `check` is the half of a
//! step's preconditions that can be answered by reading: containment, existence,
//! kind, and the *advisory* write check on the mode bits. The authoritative write
//! check creates a file in the directory and removes it again, which is a write,
//! so it stays in commit. The preview therefore says a directory looks writable;
//! only commit knows.
//!
//! # Chained moves
//!
//! The roadmap left this open: given `a → b` and `b → c` in one plan, order them
//! or reject them? **Ordered**, with one rule that removes the ambiguity: every
//! operation's source must exist in the library *as it is now*. So the two ops
//! mean "these two distinct things swap places in the tree", they are sorted to
//! run `b → c` first, and they commit. The other reading — "move `a` to `b`, then
//! move that same file on to `c`" — requires `b` not to exist, which is
//! [`Conflict::SourceMissing`]; someone who wants that stages `a → c`.
//!
//! The reason to prefer ordering over rejecting is that a reorganization produces
//! chains constantly (`hiphop/X → hiphop/A/X`, `hiphop/Y → hiphop/X`), and a
//! planner that refused them would push the user into committing twice — two
//! journals, two undos, a window in between where the library is half-organized.

use std::collections::{BTreeMap, BTreeSet};

use camino::Utf8Path;

use crate::config::Config;
use crate::library::{DirPath, Library, ScanWarning};
use crate::paths::RelPath;
use crate::playlist::PlaylistIndex;
use crate::playlist::rewrite::{self, PathMove};

use super::effects::{Conflict, Effects, OpEffect, Summary, Warning};
use super::exec_fs::{self, FsError, FsStep, FsWarning, Merge};
use super::op::{Operation, Plan};

/// The directory a delete's backup goes in, under
/// [`Config::data_dir`][crate::config::Config::data_dir].
///
/// The real name is the transaction id, which does not exist until commit opens
/// the journal (task 11). The preview needs *a* path so that
/// [`exec_fs::check`] can confirm it lands inside the backup root, and commit
/// substitutes the id for this segment when it takes the steps over.
pub const PENDING_TX: &str = "pending";

/// See [`Plan::validate`][super::op::Plan::validate].
pub(super) fn validate(plan: &Plan, lib: &Library, idx: &PlaylistIndex, cfg: &Config) -> Effects {
    let root = cfg.music_dir.as_path();
    let options = exec_fs::Options::from_config(cfg);

    let mut effects = Effects::default();
    let (order, cycles) = execution_order(plan.ops());
    effects.conflicts.extend(cycles);
    effects.conflicts.extend(duplicate_destinations(plan.ops()));

    // Everything the operations placed so far take out of the way. A destination
    // inside this set is free by the time its own step runs, however occupied it
    // looks from here.
    let mut vacated: Vec<RelPath> = Vec::new();
    let mut moves: Vec<PathMove> = Vec::new();
    let mut bytes = 0u64;

    for index in order {
        let op = &plan.ops()[index];
        let steps = expand(op, index, lib, root, &vacated, cfg, &mut effects);

        for step in &steps {
            match exec_fs::check(step, root, &options) {
                Ok(warnings) => effects
                    .warnings
                    .extend(warnings.into_iter().filter_map(from_fs_warning)),
                Err(err) => {
                    if let Some(conflict) = from_fs_error(err, index, root, &vacated, cfg) {
                        effects.conflicts.push(conflict);
                    }
                }
            }
        }

        // The playlist half is derived from the steps, not from the operation, so
        // a file the expansion left behind is a file the playlists keep pointing
        // at — the preview and the rewrite cannot disagree about which.
        let mut mine = OpEffect {
            index,
            op: op.clone(),
            files: 0,
            audio: 0,
            bytes: 0,
            playlists: 0,
            refused: false,
        };
        let mut touched: Vec<RelPath> = Vec::new();
        for step in &steps {
            let source = match step {
                FsStep::RenameFile { from, to } | FsStep::CopyDelete { from, to } => {
                    moves.push(PathMove::moved(from.clone(), to.clone()));
                    from
                }
                FsStep::RemoveFile { target, .. } => {
                    moves.push(PathMove::deleted(target.clone()));
                    target
                }
                FsStep::MkDir { .. } | FsStep::RmDirIfEmpty { .. } => continue,
            };
            mine.files += 1;
            if let Some(entry) = lib.get(source) {
                mine.bytes += entry.size;
                if entry.is_audio() {
                    mine.audio += 1;
                }
            }
            touched.push(source.clone());
        }
        mine.playlists = idx.playlists_touching(&touched).len();
        bytes += mine.bytes;
        effects.ops.push(mine);

        vacated.push(op.source().clone());
        effects.fs_steps.extend(steps);
    }

    // An operation left out of the execution order — a cycle member — still gets
    // a row, so the pending view has something to put the cursor on. It expands
    // to nothing, because there is no order in which it could run.
    let placed: BTreeSet<usize> = effects.ops.iter().map(|effect| effect.index).collect();
    for (index, op) in plan.ops().iter().enumerate() {
        if !placed.contains(&index) {
            effects.ops.push(OpEffect {
                index,
                op: op.clone(),
                files: 0,
                audio: 0,
                bytes: 0,
                playlists: 0,
                refused: true,
            });
        }
    }

    let refused: BTreeSet<usize> = effects.conflicts.iter().flat_map(Conflict::ops).collect();
    for effect in &mut effects.ops {
        effect.refused = refused.contains(&effect.index);
    }

    effects.playlist_edits = rewrite::plan_playlist_edits(idx, &moves);
    for edit in &effects.playlist_edits {
        let removals = edit.removals();
        if removals > 0 {
            effects.warnings.push(Warning::PlaylistLinesRemoved {
                playlist: edit.file_name.clone(),
                lines: removals,
            });
        }
    }

    let sources: Vec<RelPath> = moves.iter().map(|m| m.from.clone()).collect();
    effects.warnings.extend(split_albums(lib, &sources));
    effects.warnings.extend(broken_nearby(lib, idx, &sources));
    effects.warnings.extend(unnamable_nearby(lib, &sources));
    dedup_warnings(&mut effects.warnings);

    effects.summary = Summary::of(&effects.fs_steps, &effects.playlist_edits, bytes);
    effects
}

// ---------------------------------------------------------------------------
// Pass 1 — order.

/// Sort the operations so that nothing lands on a path another operation has not
/// moved out of the way yet.
///
/// Kahn's algorithm over the edges "X vacates Y's destination, so X runs first",
/// with ties broken by the order the user staged them in — a plan with no
/// dependencies at all therefore executes exactly as it reads. Whatever is left
/// when no operation has an outstanding dependency is a ring, and comes back as
/// [`Conflict::Cycle`] with those operations dropped from the order: they cannot
/// be executed, and pretending otherwise would put half a swap on disk.
fn execution_order(ops: &[Operation]) -> (Vec<usize>, Vec<Conflict>) {
    // `blockers[y]` — operations that must run before `y`.
    let mut blockers: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); ops.len()];
    for (y, op) in ops.iter().enumerate() {
        let Some(dest) = op.destination() else {
            continue;
        };
        for (x, other) in ops.iter().enumerate() {
            if x != y && other.vacates(dest) {
                blockers[y].insert(x);
            }
        }
    }

    let mut ready: BTreeSet<usize> = (0..ops.len()).filter(|&i| blockers[i].is_empty()).collect();
    let mut order = Vec::with_capacity(ops.len());
    let mut done = vec![false; ops.len()];

    while let Some(&next) = ready.iter().next() {
        ready.remove(&next);
        done[next] = true;
        order.push(next);
        for (y, blocking) in blockers.iter_mut().enumerate() {
            if blocking.remove(&next) && blocking.is_empty() && !done[y] {
                ready.insert(y);
            }
        }
    }

    if order.len() == ops.len() {
        return (order, Vec::new());
    }

    let stuck: Vec<usize> = (0..ops.len()).filter(|&i| !done[i]).collect();
    let paths = stuck.iter().map(|&i| ops[i].source().clone()).collect();
    (order, vec![Conflict::Cycle { ops: stuck, paths }])
}

/// Two operations that want to create the same path. Reported once per pair,
/// naming the earlier one first.
fn duplicate_destinations(ops: &[Operation]) -> Vec<Conflict> {
    let mut seen: BTreeMap<&RelPath, usize> = BTreeMap::new();
    let mut conflicts = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let Some(dest) = op.destination() else {
            continue;
        };
        match seen.get(dest) {
            Some(&first) => conflicts.push(Conflict::DuplicateDestination {
                first,
                second: index,
                at: dest.clone(),
            }),
            None => {
                seen.insert(dest, index);
            }
        }
    }
    conflicts
}

// ---------------------------------------------------------------------------
// Pass 2 — expand.

/// Turn one operation into the steps that perform it, reporting what stopped it.
fn expand(
    op: &Operation,
    index: usize,
    lib: &Library,
    root: &Utf8Path,
    vacated: &[RelPath],
    cfg: &Config,
    effects: &mut Effects,
) -> Vec<FsStep> {
    match op {
        Operation::MoveFile { from, to } => {
            if lib.get(from).is_none() {
                effects.conflicts.push(Conflict::SourceMissing {
                    op: index,
                    at: from.clone(),
                });
                return Vec::new();
            }
            let mut steps = Vec::new();
            if let Some(parent) = to.parent() {
                steps.push(FsStep::MkDir { at: parent });
            }
            steps.push(FsStep::RenameFile {
                from: from.clone(),
                to: to.clone(),
            });
            if let Some(parent) = from.parent() {
                steps.push(FsStep::RmDirIfEmpty { at: parent });
            }
            steps
        }

        Operation::MoveDir { from, to } => {
            if lib.dir(&DirPath::from(from.clone())).is_none() {
                effects.conflicts.push(Conflict::SourceMissing {
                    op: index,
                    at: from.clone(),
                });
                return Vec::new();
            }
            // An earlier operation is taking the destination away, so the copy of
            // it still on disk is not in the way. `Merge::Allow` walks past it;
            // the collisions it reports are then filtered against the same set.
            let merge = if is_vacated(to, vacated) {
                Merge::Allow
            } else {
                Merge::Refuse
            };
            match exec_fs::expand_dir_move(from, to, root, merge) {
                Ok(expansion) => {
                    for collision in expansion.collisions {
                        if !is_vacated(&collision.to, vacated) {
                            effects.conflicts.push(Conflict::DestinationExists {
                                op: index,
                                at: collision.to,
                            });
                        }
                    }
                    effects
                        .warnings
                        .extend(expansion.warnings.into_iter().filter_map(from_fs_warning));
                    expansion.steps
                }
                Err(err) => {
                    if let Some(conflict) = from_fs_error(err, index, root, vacated, cfg) {
                        effects.conflicts.push(conflict);
                    }
                    Vec::new()
                }
            }
        }

        Operation::Delete { target } => {
            if lib.get(target).is_none() {
                effects.conflicts.push(Conflict::SourceMissing {
                    op: index,
                    at: target.clone(),
                });
                return Vec::new();
            }
            if !cfg.delete_enabled {
                effects.conflicts.push(Conflict::DeleteDisabled {
                    op: index,
                    target: target.clone(),
                });
                return Vec::new();
            }
            let mut steps = vec![FsStep::RemoveFile {
                target: target.clone(),
                // Mirroring the library path under the backup root, so two
                // deletes of `cover.jpg` from two albums keep both files.
                backup: Some(
                    cfg.data_dir
                        .join("backups")
                        .join(PENDING_TX)
                        .join("files")
                        .join(target.as_str()),
                ),
            }];
            if let Some(parent) = target.parent() {
                steps.push(FsStep::RmDirIfEmpty { at: parent });
            }
            steps
        }
    }
}

/// Whether `path` is inside something an earlier operation takes away.
fn is_vacated(path: &RelPath, vacated: &[RelPath]) -> bool {
    vacated
        .iter()
        .any(|gone| path == gone || path.starts_with_dir(gone))
}

// ---------------------------------------------------------------------------
// Pass 3 — what `exec_fs` said.

/// Turn a refusal from [`exec_fs::check`] into a conflict, or drop it when this
/// plan has already accounted for it.
fn from_fs_error(
    err: FsError,
    op: usize,
    root: &Utf8Path,
    vacated: &[RelPath],
    cfg: &Config,
) -> Option<Conflict> {
    match err {
        // An occupied destination that an earlier operation empties is not
        // occupied by the time this step runs.
        FsError::Exists { path } => {
            let at = RelPath::from_abs(&path, root).ok()?;
            (!is_vacated(&at, vacated)).then_some(Conflict::DestinationExists { op, at })
        }
        // The transaction's backup directory does not exist yet: commit creates
        // it before the first step runs (task 11). `exec_fs::check` is right to
        // insist on it, and the preview is right to ignore that it is not there.
        FsError::Missing { ref path } if crate::paths::contains(&cfg.data_dir, path) => None,
        FsError::Missing { path }
        | FsError::NotAFile { path }
        | FsError::NotADirectory { path } => Some(match RelPath::from_abs(&path, root) {
            Ok(at) => Conflict::SourceMissing { op, at },
            Err(reason) => Conflict::OutsideRoot {
                op,
                path: path.to_string(),
                reason: reason.to_string(),
            },
        }),
        FsError::Outside { path, root } => Some(Conflict::OutsideRoot {
            op,
            path: path.to_string(),
            reason: format!("it is not under {root}"),
        }),
        FsError::NotWritable { dir } => Some(Conflict::NotWritable { op, dir }),
        FsError::DeleteDisabled { path } => Some(Conflict::DeleteDisabled {
            op,
            target: RelPath::from_abs(&path, root).ok()?,
        }),
        other => Some(Conflict::Unreadable {
            op,
            message: other.to_string(),
        }),
    }
}

/// Turn a note from [`exec_fs`] into one of ours. `None` for the ones that are
/// about commit rather than about the plan.
fn from_fs_warning(warning: FsWarning) -> Option<Warning> {
    match warning {
        FsWarning::CaseCollision { at, existing } => Some(Warning::CaseDifference { at, existing }),
        FsWarning::Symlink { at } => Some(Warning::SkippedSymlink { at }),
        FsWarning::Unnamable { path, reason } => Some(Warning::SkippedUnnamable {
            path,
            reason: reason.to_string(),
        }),
        // Both describe what commit *did*, not what it would do: a directory
        // left in place is not a change, and a delete with no backup cannot be
        // planned here (every `Delete` gets one).
        FsWarning::OnlyClutter { .. } | FsWarning::NoBackup { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Pass 4 — what the library says about the neighbourhood.

/// Album directories losing some of their tracks but not all of them.
fn split_albums(lib: &Library, sources: &[RelPath]) -> Vec<Warning> {
    let mut leaving: BTreeMap<DirPath, usize> = BTreeMap::new();
    for source in sources {
        if lib.get(source).is_some_and(crate::library::Entry::is_audio) {
            *leaving.entry(DirPath::of(source)).or_default() += 1;
        }
    }

    leaving
        .into_iter()
        .filter_map(|(dir, moved)| {
            let album = lib.album_dir(&dir)?;
            (moved < album.audio).then_some(Warning::AlbumSplit {
                album: dir,
                moved,
                total: album.audio,
            })
        })
        .collect()
}

/// Playlist entries that already do not resolve, in a directory this plan
/// touches. Not caused by the commit, and easy to blame on it afterwards.
fn broken_nearby(lib: &Library, idx: &PlaylistIndex, sources: &[RelPath]) -> Vec<Warning> {
    let dirs: BTreeSet<DirPath> = sources.iter().map(DirPath::of).collect();

    idx.broken(lib)
        .into_iter()
        .filter(|(_, path)| dirs.contains(&DirPath::of(path)))
        .filter_map(|(reference, path)| {
            let playlist = idx.playlist(reference.playlist)?;
            Some(Warning::BrokenReferenceNearby {
                playlist: playlist
                    .path()
                    .file_name()
                    .unwrap_or(playlist.name())
                    .to_owned(),
                path: path.as_str().to_owned(),
            })
        })
        .collect()
}

/// Entries the scan could not name, in a directory this plan touches. They stay
/// where they are while everything around them moves.
fn unnamable_nearby(lib: &Library, sources: &[RelPath]) -> Vec<Warning> {
    let dirs: Vec<camino::Utf8PathBuf> = sources
        .iter()
        .filter_map(RelPath::parent)
        .map(|dir| dir.to_abs(lib.root()))
        .collect();

    lib.warnings()
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                ScanWarning::NotUtf8 { .. } | ScanWarning::Unnamable { .. }
            )
        })
        .filter(|warning| {
            let path = warning.path();
            dirs.iter()
                .any(|dir| path.starts_with(dir.as_str()) && path.len() > dir.as_str().len())
        })
        .map(|warning| Warning::SkippedUnnamable {
            path: warning.path().to_owned(),
            reason: warning.to_string(),
        })
        .collect()
}

/// Keep the first of each identical warning. A directory move reports the same
/// unnamable entry once from the expansion and once from the scan; the user
/// wants to be told once.
fn dedup_warnings(warnings: &mut Vec<Warning>) {
    let mut seen = BTreeSet::new();
    warnings.retain(|warning| seen.insert(warning.to_string()));
}
