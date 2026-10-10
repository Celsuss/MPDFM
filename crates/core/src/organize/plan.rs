//! From a template and a selection of tagged tracks to a [`Mapping`]: where
//! every file goes, which ones cannot go anywhere, and why.
//!
//! This is a plan *generator*, not a plan: it writes nothing and validates
//! nothing against the playlists. Task 28 turns the [`Mapping`]'s moves into
//! [`Operation`][crate::ops::Operation]s and runs them through the same
//! `validate` → preview → `commit` pipeline as a manual move.
//!
//! # What travels, and what stays
//!
//! - A track the template has no place for — a tag it requires is missing, or
//!   renders to no name — **stays where it is**, reported as
//!   [`Warning::Unplaceable`].
//! - Two files bound for one path are a [`Conflict::Collision`] naming both, and
//!   neither moves. Nothing is ever suffixed ` (1)`.
//! - A destination already taken by a file that is not itself moving away is a
//!   [`Conflict::Occupied`].
//! - The aux files of an album directory — cover art, `.nfo`, `.cue`, `.log`,
//!   `.sfv`, and any subdirectory holding no audio, like `Scans/` — follow its
//!   tracks when **all** of them go to one directory. When they scatter, that is
//!   an [`Warning::AlbumSplit`], and the aux files stay unless
//!   [`SplitAux::FollowMajority`] was confirmed.
//! - A multi-disc set's root keeps its own aux files with the set when every
//!   disc moves whole and the discs land side by side.
//! - A destination that differs from another path only by case is a
//!   [`Warning::CaseDifference`]: legal on ext4, a collision on the phone.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::sanitize::NameRules;
use super::template::{RenderContext, Template, Unplaceable};
use crate::library::{AlbumDir, DirPath, Library};
use crate::ops::Operation;
use crate::paths::RelPath;
use crate::tags::TagSet;

/// What to do with an album's aux files when its tracks scatter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SplitAux {
    /// Leave them where they are. The default: the user has not said which half
    /// of the album the cover belongs to.
    #[default]
    Leave,
    /// Move them with the most tracks (ties: the destination that sorts first).
    /// Only after the user has seen the [`Warning::AlbumSplit`] and agreed.
    FollowMajority,
}

/// How a run is configured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// How names are sanitized.
    pub rules: NameRules,
    /// What happens to the aux files of a split album.
    pub split_aux: SplitAux,
}

/// One file's journey.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Move {
    /// Where it is.
    pub from: RelPath,
    /// Where the template puts it.
    pub to: RelPath,
    /// Whether it is an aux file travelling with its album rather than a track
    /// the template placed.
    pub aux: bool,
}

/// A reason the mapping cannot be committed as it stands.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Conflict {
    /// Several files render to one destination.
    #[error("{} all map to {to}", .sources.iter().map(ToString::to_string).collect::<Vec<_>>().join(" and "))]
    Collision {
        /// The shared destination.
        to: RelPath,
        /// Every file bound there, sorted. Any of them may already be there.
        sources: Vec<RelPath>,
    },
    /// The destination exists — a file that is not moving away, or a
    /// directory — and would be overwritten.
    #[error("{from} maps to {to}, but {existing} is already there")]
    Occupied {
        /// The file being placed.
        from: RelPath,
        /// Where it would go.
        to: RelPath,
        /// What is in the way: `to` itself, or a file where one of its
        /// directories would have to be.
        existing: String,
    },
}

/// Something the user should hear about before committing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Warning {
    /// A track the template cannot place. It stays where it is; the reason is
    /// the to-do item ("tag the album") that would make it placeable.
    #[error("{path} stays where it is: {reason}")]
    Unplaceable {
        /// The track.
        path: RelPath,
        /// Why.
        reason: Unplaceable,
    },
    /// The tracks of one album directory are going to different places (or
    /// some are staying behind), so the album is being split.
    #[error(
        "{} will be split across {} directories; its {} other file(s) {}",
        .dir, .destinations.len(), .aux.len(),
        if *.aux_moved { "follow the most tracks" } else { "stay where they are" }
    )]
    AlbumSplit {
        /// The album directory being split.
        dir: DirPath,
        /// Where its tracks end up, the directory itself included when some
        /// stay. Sorted.
        destinations: Vec<DirPath>,
        /// Its aux files.
        aux: Vec<RelPath>,
        /// Whether they move anyway ([`SplitAux::FollowMajority`]).
        aux_moved: bool,
    },
    /// After the moves, `at` and `other` differ only by (ASCII) case. Only
    /// the shallowest such difference is reported, not every file below it.
    #[error("{at} differs from {other} only by case")]
    CaseDifference {
        /// A path this mapping creates.
        at: String,
        /// The path it differs from: one that stays, or another destination.
        other: String,
    },
}

/// Everything [`map`] decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mapping {
    /// The moves to make, sorted by source.
    pub moves: Vec<Move>,
    /// Tracks already where the template puts them.
    pub in_place: Vec<RelPath>,
    /// What blocks a commit. Every file named here stays where it is.
    pub conflicts: Vec<Conflict>,
    /// What the user should know: unplaceable tracks, split albums, case
    /// differences.
    pub warnings: Vec<Warning>,
}

impl Mapping {
    /// Whether nothing blocks a commit.
    #[must_use]
    pub fn is_committable(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// The tracks that cannot be placed, with why — task 28's to-do list.
    pub fn unplaceable(&self) -> impl Iterator<Item = (&RelPath, &Unplaceable)> {
        self.warnings.iter().filter_map(|w| match w {
            Warning::Unplaceable { path, reason } => Some((path, reason)),
            _ => None,
        })
    }

    /// The moves as operations for a [`Plan`][crate::ops::Plan], one
    /// [`Operation::MoveFile`] per file, aux files included only if `aux`.
    ///
    /// Per file and never a [`Operation::MoveDir`], even for an album that
    /// moves whole: the mapping has already decided where every aux file goes
    /// (and that a `Scans/` subdirectory goes too), so a directory move would
    /// only be a second, coarser opinion — one that would also sweep up a file
    /// the scan could not model. Task 28 then validates and commits these like
    /// any other plan.
    #[must_use]
    pub fn operations(&self, aux: bool) -> Vec<Operation> {
        self.moves
            .iter()
            .filter(|m| aux || !m.aux)
            .map(|m| Operation::MoveFile {
                from: m.from.clone(),
                to: m.to.clone(),
            })
            .collect()
    }

    /// How many tracks (not aux files) move.
    #[must_use]
    pub fn tracks_moving(&self) -> usize {
        self.moves.iter().filter(|m| !m.aux).count()
    }

    /// The destination of `from`, if it moves.
    #[must_use]
    pub fn destination(&self, from: &RelPath) -> Option<&RelPath> {
        self.moves
            .binary_search_by(|m| m.from.cmp(from))
            .ok()
            .map(|i| &self.moves[i].to)
    }
}

/// Map `tracks` — each with the tags read for it — through `template`.
///
/// Pure: the tags are the caller's (task 28 reads them with
/// [`tags::read_many`][crate::tags::read_many]), the library is the scan, and
/// nothing touches the disk. A track whose tags could not be read is simply not
/// in `tracks`, and stays where it is like any other unselected file.
#[must_use]
pub fn map(
    template: &Template,
    library: &Library,
    tracks: &BTreeMap<RelPath, TagSet>,
    options: &Options,
) -> Mapping {
    let mut mapping = Mapping::default();
    let root_len = library.root().as_str().len() + 1;

    // 1. Render every track.
    let mut placed: BTreeMap<RelPath, RelPath> = BTreeMap::new();
    for (rel, tags) in tracks {
        let ctx = RenderContext {
            rules: options.rules,
            in_disc_dir: library
                .album_dir(&DirPath::of(rel))
                .is_some_and(AlbumDir::is_disc),
            root_len,
        };
        match template.render(rel, tags, &ctx) {
            Ok(to) => {
                placed.insert(rel.clone(), to);
            }
            Err(reason) => mapping.warnings.push(Warning::Unplaceable {
                path: rel.clone(),
                reason,
            }),
        }
    }

    // 2. Settle the tracks: collisions and occupied destinations stay put.
    let audio = settle(placed, &BTreeMap::new(), library, &mut mapping);

    // 3. Aux files follow albums that move whole.
    let aux = aux_moves(library, tracks, &audio, options, &mut mapping);
    let aux = settle(aux, &audio, library, &mut mapping);

    mapping.moves = audio
        .into_iter()
        .map(|(from, to)| Move {
            from,
            to,
            aux: false,
        })
        .chain(aux.into_iter().map(|(from, to)| Move {
            from,
            to,
            aux: true,
        }))
        .collect();
    mapping.moves.sort();

    // 4. Case-only differences in the world after the moves.
    case_differences(library, &mut mapping);

    mapping
        .conflicts
        .sort_by(|a, b| conflict_key(a).cmp(&conflict_key(b)));
    mapping
}

/// Drop every candidate that collides or lands on something, recording why,
/// and return the moves that remain. In-place candidates (`from == to`) are
/// recorded in [`Mapping::in_place`] — after taking part in collision
/// detection, because a file that is already somewhere still occupies it.
///
/// `settled` are moves decided earlier (the tracks, when settling aux files):
/// their destinations are taken and their sources count as vacated.
fn settle(
    candidates: BTreeMap<RelPath, RelPath>,
    settled: &BTreeMap<RelPath, RelPath>,
    library: &Library,
    mapping: &mut Mapping,
) -> BTreeMap<RelPath, RelPath> {
    let mut by_dest: BTreeMap<&RelPath, Vec<&RelPath>> = BTreeMap::new();
    for (from, to) in candidates.iter().chain(settled) {
        by_dest.entry(to).or_default().push(from);
    }
    let mut moving = BTreeMap::new();
    let mut in_place = Vec::new();
    for (from, to) in &candidates {
        let sources = &by_dest[to];
        if sources.len() > 1 {
            continue;
        }
        if from == to {
            in_place.push(from.clone());
        } else {
            moving.insert(from.clone(), to.clone());
        }
    }
    for (to, mut sources) in by_dest {
        let touches_candidate = sources.iter().any(|s| candidates.contains_key(*s));
        if sources.len() > 1 && touches_candidate {
            sources.sort();
            mapping.conflicts.push(Conflict::Collision {
                to: to.clone(),
                sources: sources.into_iter().cloned().collect(),
            });
        }
    }
    mapping.in_place.extend(in_place);
    mapping.in_place.sort();

    // A destination is free if nothing is there, or what is there is leaving.
    // Blocking one move can block another that was counting on it leaving, so
    // repeat until nothing changes.
    loop {
        let vacated = |rel: &RelPath| moving.contains_key(rel) || settled.contains_key(rel);
        let blocked: Vec<(RelPath, RelPath, String)> = moving
            .iter()
            .filter_map(|(from, to)| {
                occupant(library, to, &vacated).map(|existing| (from.clone(), to.clone(), existing))
            })
            .collect();
        if blocked.is_empty() {
            break;
        }
        for (from, to, existing) in blocked {
            moving.remove(&from);
            mapping
                .conflicts
                .push(Conflict::Occupied { from, to, existing });
        }
    }
    moving
}

/// What is already at `to`, or where one of its directories would go, that is
/// not moving away.
fn occupant(library: &Library, to: &RelPath, vacated: &dyn Fn(&RelPath) -> bool) -> Option<String> {
    if library.get(to).is_some() && !vacated(to) {
        return Some(to.to_string());
    }
    let as_dir = DirPath::from(to.clone());
    if library.dir(&as_dir).is_some() {
        return Some(format!("{to}/"));
    }
    let mut ancestor = to.parent();
    while let Some(dir) = ancestor {
        if library.get(&dir).is_some() && !vacated(&dir) {
            return Some(dir.to_string());
        }
        ancestor = dir.parent();
    }
    None
}

/// The aux moves for every album directory the selection touches.
fn aux_moves(
    library: &Library,
    tracks: &BTreeMap<RelPath, TagSet>,
    audio: &BTreeMap<RelPath, RelPath>,
    options: &Options,
    mapping: &mut Mapping,
) -> BTreeMap<RelPath, RelPath> {
    let mut out = BTreeMap::new();
    // Album directories whose tracks all go to one directory, and where.
    let mut whole: BTreeMap<DirPath, DirPath> = BTreeMap::new();

    let touched: BTreeSet<DirPath> = tracks.keys().map(DirPath::of).collect();
    for dir in touched.iter().filter(|d| !d.is_root()) {
        let mut targets: BTreeMap<DirPath, usize> = BTreeMap::new();
        for entry in library.files_in(dir).filter(|e| e.is_audio()) {
            let target = audio
                .get(&entry.rel)
                .map_or_else(|| dir.clone(), DirPath::of);
            *targets.entry(target).or_default() += 1;
        }
        let aux = aux_files(library, dir);

        if targets.len() == 1 {
            let (target, _) = targets.pop_first().expect("one target");
            if target != *dir {
                move_under(&aux, dir, &target, &mut out);
            }
            whole.insert(dir.clone(), target);
            continue;
        }
        if targets.len() < 2 {
            continue;
        }
        let follow = match options.split_aux {
            SplitAux::Leave => None,
            SplitAux::FollowMajority => targets
                .iter()
                // `max_by_key` keeps the last of equals; reverse for the first.
                .rev()
                .max_by_key(|(_, n)| **n)
                .map(|(target, _)| target.clone())
                .filter(|target| target != dir),
        };
        if let Some(target) = &follow {
            move_under(&aux, dir, target, &mut out);
        }
        mapping.warnings.push(Warning::AlbumSplit {
            dir: dir.clone(),
            destinations: targets.into_keys().collect(),
            aux,
            aux_moved: follow.is_some(),
        });
    }

    // Multi-disc set roots: their own aux files go with the set when every
    // disc moves whole and the discs land together.
    let set_roots: BTreeSet<DirPath> = touched
        .iter()
        .filter_map(|d| library.album_dir(d)?.set_root.clone())
        .filter(|root| library.album_dir(root).is_none())
        .collect();
    for root in set_roots {
        let discs: Vec<&DirPath> = library.discs_of(&root).map(|a| &a.dir).collect();
        let targets: Option<BTreeSet<&DirPath>> = discs.iter().map(|d| whole.get(*d)).collect();
        let aux = aux_files(library, &root);
        let common = targets.as_ref().and_then(|targets| {
            if targets.len() == 1 {
                return targets.first().map(|t| (*t).clone());
            }
            let parents: BTreeSet<Option<DirPath>> = targets.iter().map(|t| t.parent()).collect();
            match parents.into_iter().collect::<Vec<_>>().as_slice() {
                [Some(parent)] => Some(parent.clone()),
                _ => None,
            }
        });
        match common {
            Some(target) if target == root => {}
            Some(target) => move_under(&aux, &root, &target, &mut out),
            None => {
                let mut destinations: BTreeSet<DirPath> = BTreeSet::new();
                for disc in &discs {
                    destinations
                        .insert(whole.get(*disc).cloned().unwrap_or_else(|| (*disc).clone()));
                }
                // Only a real scattering is news; an untouched set is not.
                if destinations.iter().any(|d| !discs.contains(&d)) {
                    mapping.warnings.push(Warning::AlbumSplit {
                        dir: root.clone(),
                        destinations: destinations.into_iter().collect(),
                        aux,
                        aux_moved: false,
                    });
                }
            }
        }
    }
    out
}

/// Every non-audio file that belongs to `dir`: directly inside it, or inside a
/// subdirectory holding no audio anywhere below it (`Scans/`, `Artwork/`).
fn aux_files(library: &Library, dir: &DirPath) -> Vec<RelPath> {
    let mut out: Vec<RelPath> = library
        .files_in(dir)
        .filter(|e| !e.is_audio())
        .map(|e| e.rel.clone())
        .collect();
    let mut stack: Vec<&DirPath> = library
        .subdirs_in(dir)
        .iter()
        .filter(|sub| !holds_audio(library, sub))
        .collect();
    while let Some(sub) = stack.pop() {
        out.extend(library.files_in(sub).map(|e| e.rel.clone()));
        stack.extend(library.subdirs_in(sub));
    }
    out.sort();
    out
}

fn holds_audio(library: &Library, dir: &DirPath) -> bool {
    library
        .album_dirs()
        .iter()
        .any(|a| a.dir == *dir || a.dir.starts_with_dir(dir))
}

/// `files`, all under `from`, re-rooted under `to`.
fn move_under(
    files: &[RelPath],
    from: &DirPath,
    to: &DirPath,
    out: &mut BTreeMap<RelPath, RelPath>,
) {
    for file in files {
        let rest = match from.as_rel() {
            Some(from) => &file.as_str()[from.as_str().len() + 1..],
            None => file.as_str(),
        };
        // `rest` is a tail of a valid `RelPath`, and `to` came from one.
        if let Ok(dest) = to.join(rest) {
            out.insert(file.clone(), dest);
        }
    }
}

/// Paths that, after the moves, differ from another only by ASCII case.
///
/// Folded ASCII-only, like the executor's own check (task 08): Unicode case
/// depends on the locale. A pair is reported only where its parents are
/// byte-identical, so `Pop/` vs `pop/` is one warning rather than one per track
/// below it.
fn case_differences(library: &Library, mapping: &mut Mapping) {
    let leaving: BTreeSet<&RelPath> = mapping.moves.iter().map(|m| &m.from).collect();
    let mut created: BTreeSet<String> = BTreeSet::new();
    for m in &mapping.moves {
        with_ancestors(&m.to, |p| {
            created.insert(p.to_owned());
        });
    }
    let mut staying: BTreeSet<String> = BTreeSet::new();
    for entry in library.entries() {
        if !leaving.contains(&entry.rel) {
            with_ancestors(&entry.rel, |p| {
                staying.insert(p.to_owned());
            });
        }
    }

    let mut folded: HashMap<String, BTreeSet<&str>> = HashMap::new();
    for path in created.iter().chain(&staying) {
        folded
            .entry(path.to_ascii_lowercase())
            .or_default()
            .insert(path.as_str());
    }
    let parent = |p: &str| p.rsplit_once('/').map(|(dir, _)| dir.to_owned());

    let mut found = BTreeSet::new();
    for spellings in folded.values().filter(|s| s.len() > 1) {
        for at in spellings.iter().filter(|s| created.contains(**s)) {
            for other in spellings.iter().filter(|o| *o != at) {
                if parent(at) != parent(other) {
                    continue;
                }
                // Two new spellings are one difference, not two.
                let pair = if created.contains(*other) && other < at {
                    (*other, *at)
                } else {
                    (*at, *other)
                };
                found.insert((pair.0.to_owned(), pair.1.to_owned()));
            }
        }
    }
    mapping.warnings.extend(
        found
            .into_iter()
            .map(|(at, other)| Warning::CaseDifference { at, other }),
    );
}

fn with_ancestors(rel: &RelPath, mut f: impl FnMut(&str)) {
    let s = rel.as_str();
    f(s);
    for (i, _) in s.match_indices('/') {
        f(&s[..i]);
    }
}

fn conflict_key(conflict: &Conflict) -> (&RelPath, Option<&RelPath>) {
    match conflict {
        Conflict::Collision { to, .. } => (to, None),
        Conflict::Occupied { from, to, .. } => (to, Some(from)),
    }
}
