//! The library browser: where you are in the tree, what is marked, and what is
//! known about the rows on screen.
//!
//! ```text
//! ┌ Music ───────────┐┌ hiphop/MF DOOM - Mm..Food (2004) ───────┐┌ details ───────┐
//! │⚠ ▾ /             ││●   01 Beef Rap.mp3              3:24 320k││01 Beef Rap.mp3 │
//! │    ▸ electronic  ││● ⚠ 02 Hoe Cakes.mp3             4:02 320k││                │
//! │⚠   ▾ hiphop      ││    03 Potholderz.mp3            2:58 320k││Title   Beef Rap│
//! │⚠       MF DOOM -…││    folder.jpg                            ││Artist  MF DOOM │
//! │⚠       Snoop Dog…││    info.nfo                              ││Genre   —       │
//! │    ▸ japanese    ││                                          ││⚠ 2 lines in 2 …│
//! └──────────────────┘└ 1/5 ─────────────────────────────────────┘└────────────────┘
//!  2 marked · 0 pending · sort name · focus files · ● playing 01 Beef Rap.mp3
//! ```
//!
//! # What this module is and is not
//!
//! It is **state plus pure queries**. Nothing here opens a file, spawns a thread
//! or draws anything: [`Browser::rows`] turns the model into the rows a frame
//! needs, [`Browser::wanted`] says which tags somebody else should go and read,
//! and `app.rs` does both of those things to it. The reason is the same one task
//! 20 gives for `work.rs`: the only way to be sure the UI never blocks is for the
//! code that decides what to show to have no way of blocking.
//!
//! # Three ideas hold the whole thing up
//!
//! **One cursor is derived and one is stored.** The tree pane's cursor *is*
//! [`Browser::dir`] — the row it sits on is the directory whose contents the
//! middle pane lists, so the two cannot disagree. The files pane's cursor is a
//! stored index, because the thing it points at has no other name.
//!
//! **A window is computed, never remembered.** [`window`] takes the stored scroll
//! offset as a hint and returns a range that is guaranteed to contain the cursor.
//! A remembered window would go stale the moment the terminal was resized, the
//! sort changed or a rescan shortened the listing; a computed one cannot. This is
//! also where virtualization lives: everything downstream — the widget, the tag
//! reads — is given the range and nothing else.
//!
//! **Tags are a cache keyed by path, and a request set.** [`Browser::wanted`]
//! returns the visible audio rows that are neither cached nor already out for
//! reading, and marks them as asked-for; when the answers come back
//! [`Browser::tags_arrived`] fills the cache. Scrolling through a 400-file
//! directory therefore reads each file at most once, and only the ones that were
//! actually looked at. The counter in
//! [`library::audio_reads`][mpdfm_core::library::audio_reads] is what keeps that
//! honest in the tests.
//!
//! # Sorting
//!
//! Four orders, remembered for the session (`:set sort=track`). Directories
//! always come first — they are a different kind of thing, and a listing that
//! interleaved them by size would be unusable — and inside each group the order
//! is:
//!
//! | sort | key |
//! | --- | --- |
//! | `name` | [`natural_cmp`], so `02` comes before `10` |
//! | `track` | the track-number tag, then the name |
//! | `mtime` | newest first |
//! | `size` | largest first |
//!
//! `track` is the one that needs data the browser does not have, so choosing it
//! asks for the tags of the **whole directory** rather than of the visible window
//! — an album is a few dozen files, and a sort that only ordered what happened to
//! be on screen would be a lie. Until they arrive the listing is in name order,
//! which is what a half-read album looks like anyway.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use mpdfm_core::library::{DirPath, Kind, Library, ScanWarning};
use mpdfm_core::paths::RelPath;
use mpdfm_core::playlist::PlaylistIndex;
use mpdfm_core::tags::Field;

use crate::tui::msg::TrackInfo;
use crate::tui::widgets::details::{Detail, Details};
use crate::tui::widgets::filelist::{Meta, Row, RowKind};

/// How a listing is ordered. Remembered for the session; see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// By name, naturally: `02` before `10`.
    #[default]
    Name,
    /// By the track-number tag, then by name.
    Track,
    /// Most recently modified first.
    Mtime,
    /// Largest first.
    Size,
}

impl Sort {
    /// Every sort, in the order `:set sort=` lists them when asked for one it
    /// does not have.
    pub const ALL: &'static [Self] = &[Self::Name, Self::Track, Self::Mtime, Self::Size];

    /// The name `:set sort=<name>` takes, and the one the status bar shows.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Track => "track",
            Self::Mtime => "mtime",
            Self::Size => "size",
        }
    }

    /// The sort with this name, if there is one.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|sort| sort.as_str() == name)
    }

    /// Every name, for the message that lists them.
    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|sort| sort.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which of the browser's two panes a call is about.
///
/// Re-stated here rather than imported from `app.rs` so the view does not depend
/// on the shell; `app.rs` maps its own [`Focus`][crate::tui::app::Focus] onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The directory tree on the left.
    Tree,
    /// The listing in the middle.
    Files,
}

/// What a listing row points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A subdirectory of the current directory.
    Dir(DirPath),
    /// A file, as an index into [`Library::entries`].
    File(usize),
}

/// One row of the tree pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// The directory the row stands for.
    pub dir: DirPath,
    /// How deep it is, for the indent.
    pub depth: usize,
    /// Whether it has subdirectories at all.
    pub has_children: bool,
    /// Whether those subdirectories are being shown.
    pub expanded: bool,
    /// Whether a playlist references anything inside it.
    pub referenced: bool,
}

/// What is known about one track, once a worker has read it.
#[derive(Debug, Clone)]
pub enum Cached {
    /// It was read.
    Read(Box<TrackInfo>),
    /// It was not, and this is why.
    Failed(String),
}

/// The browser's whole state.
pub struct Browser {
    /// The directory the listing shows, and the tree pane's cursor.
    dir: DirPath,
    /// Which tree nodes are open. The root is always open and is not in here.
    expanded: BTreeSet<DirPath>,
    /// Scroll hints; [`window`] treats them as hints and not as truth.
    tree_offset: usize,
    files_offset: usize,
    /// The listing's cursor, as an index into [`Browser::rows`].
    cursor: usize,
    /// Everything marked, anywhere in the library.
    ///
    /// A `HashSet<RelPath>` and not a per-directory flag, which is the task's
    /// requirement and the reason marking across directories works at all: it
    /// survives navigation because it was never attached to a directory.
    marks: HashSet<RelPath>,
    /// Where a visual selection started, as an index into the listing.
    visual: Option<usize>,
    /// The sort, remembered for the session.
    sort: Sort,
    /// Tags and audio properties, keyed by path.
    tags: HashMap<RelPath, Cached>,
    /// Paths a worker has been asked about and has not answered.
    asked: HashSet<RelPath>,
    /// Directories with a playlist-referenced file somewhere inside them.
    flagged: HashSet<DirPath>,
    /// The current directory's rows, already sorted.
    ///
    /// Held rather than derived per call, and the reason is measured: a frame
    /// asks for the listing five times — the widget, the scroll offset, the
    /// details pane, the tag window — and re-sorting 400 entries each time cost
    /// 2 ms a frame, which is most of the budget for a key that is held down.
    /// Every mutator that can change it ends in [`Browser::refresh`], so the one
    /// risk a cache carries is answered by there being a single place that
    /// fills it.
    listing: Vec<Target>,
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}

impl Browser {
    /// A browser at the root of a library that has not been scanned yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dir: DirPath::root(),
            expanded: BTreeSet::new(),
            tree_offset: 0,
            files_offset: 0,
            cursor: 0,
            marks: HashSet::new(),
            visual: None,
            sort: Sort::default(),
            tags: HashMap::new(),
            asked: HashSet::new(),
            flagged: HashSet::new(),
            listing: Vec::new(),
        }
    }

    // -- what the shell asks it --------------------------------------------

    /// The directory the listing is showing.
    #[must_use]
    pub fn dir(&self) -> &DirPath {
        &self.dir
    }

    /// The current directory, spelled for a title: the root is `/` and not the
    /// empty string a [`DirPath`] displays as.
    #[must_use]
    pub fn dir_label(&self) -> String {
        if self.dir.is_root() {
            "/".to_owned()
        } else {
            self.dir.to_string()
        }
    }

    /// How many things are marked.
    #[must_use]
    pub fn marked(&self) -> usize {
        self.marks.len()
    }

    /// Everything marked, in path order — what a staged operation would take.
    ///
    /// The seam to the tasks that change things: the tag editor opens on this
    /// (task 23) and the plan builder will stage it (task 24). Path order and not
    /// mark order, because a selection of two hundred files across two albums has
    /// to be *readable* in the preview, and the order somebody happened to press
    /// `space` in is not.
    ///
    /// Directories can be marked too ([`Browser::mark_all`] marks them), so what
    /// comes back is paths and not tracks; the caller decides what a directory
    /// means to it.
    #[must_use]
    pub fn marks(&self) -> Vec<RelPath> {
        let mut marks: Vec<RelPath> = self.marks.iter().cloned().collect();
        marks.sort_unstable();
        marks
    }

    /// The sort in force.
    #[must_use]
    pub fn sort(&self) -> Sort {
        self.sort
    }

    /// Change the sort, and say whether that changed anything.
    pub fn set_sort(&mut self, sort: Sort, library: Option<&Library>) -> bool {
        let changed = self.sort != sort;
        self.sort = sort;
        if let Some(library) = library {
            self.refresh(library);
        }
        changed
    }

    /// Whether a visual selection is in progress, for the status bar.
    #[must_use]
    pub fn in_visual(&self) -> bool {
        self.visual.is_some()
    }

    /// The listing's cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// How many rows the given pane has.
    #[must_use]
    pub fn row_count(&self, pane: Pane, library: &Library) -> usize {
        match pane {
            Pane::Tree => self.tree_rows(library).len(),
            Pane::Files => self.listing.len(),
        }
    }

    /// The cursor of the given pane, as a row index.
    #[must_use]
    pub fn pane_cursor(&self, pane: Pane, library: &Library) -> usize {
        match pane {
            Pane::Tree => self.tree_cursor(library),
            Pane::Files => self.cursor,
        }
    }

    // -- the model, after a scan -------------------------------------------

    /// A new library landed: re-derive everything that was about the old one.
    ///
    /// The tag cache is **dropped**, not kept. A rescan is the user saying the
    /// library may have changed under them, and a cached bitrate for a file that
    /// has been re-encoded is worse than no bitrate. Re-reading forty rows costs
    /// a few milliseconds on a worker; showing a stale number costs trust.
    ///
    /// Marks are kept, because they are the user's own work and nothing about a
    /// rescan says they changed their mind — except for marks on paths that have
    /// since gone, which are dropped since nothing can be done with them.
    pub fn library_changed(&mut self, library: &Library, index: Option<&PlaylistIndex>) {
        self.tags.clear();
        self.asked.clear();
        self.visual = None;

        self.flagged = index.map(flagged_dirs).unwrap_or_default();
        self.marks.retain(|rel| {
            library.get(rel).is_some() || library.dir(&DirPath::from(rel.clone())).is_some()
        });

        // Walk up to the nearest directory that still exists, so a browser that
        // was inside an album somebody deleted lands somewhere real rather than
        // on an empty listing with a path that is not there.
        while library.dir(&self.dir).is_none() {
            let Some(parent) = self.dir.parent() else {
                self.dir = DirPath::root();
                break;
            };
            self.dir = parent;
        }
        self.refresh(library);
    }

    // -- navigation ---------------------------------------------------------

    /// Move the focused pane's cursor by `delta` rows, stopping at either end.
    pub fn move_cursor(&mut self, pane: Pane, delta: isize, library: &Library) -> bool {
        let count = self.row_count(pane, library);
        if count == 0 {
            return false;
        }
        let from = self.pane_cursor(pane, library);
        let to = from.saturating_add_signed(delta).min(count - 1);
        self.set_cursor(pane, to, library)
    }

    /// Put the focused pane's cursor on a row. Returns whether it moved.
    pub fn set_cursor(&mut self, pane: Pane, row: usize, library: &Library) -> bool {
        match pane {
            Pane::Tree => {
                let rows = self.tree_rows(library);
                let Some(target) = rows.get(row.min(rows.len().saturating_sub(1))) else {
                    return false;
                };
                let moved = target.dir != self.dir;
                if moved {
                    self.select(target.dir.clone());
                    self.refresh(library);
                }
                moved
            }
            Pane::Files => {
                let count = self.listing.len();
                let row = row.min(count.saturating_sub(1));
                let moved = self.cursor != row;
                self.cursor = row;
                moved
            }
        }
    }

    /// Put the cursor on the last row of the focused pane.
    pub fn go_last(&mut self, pane: Pane, library: &Library) -> bool {
        let last = self.row_count(pane, library).saturating_sub(1);
        self.set_cursor(pane, last, library)
    }

    /// `l` / `enter`: into the thing under the cursor.
    ///
    /// In the tree that means opening a closed node, and moving the keyboard to
    /// the listing when it is already open — a second `l` on a directory you can
    /// already see the inside of should take you there, not do nothing.
    ///
    /// In the listing it means entering a subdirectory. On a file it returns
    /// [`Enter::File`] and lets the caller decide; there is no "open" for a track
    /// in this task, and task 23's editor is what will answer it.
    pub fn enter(&mut self, pane: Pane, library: &Library) -> Enter {
        match pane {
            Pane::Tree => {
                let dir = self.dir.clone();
                if !library.subdirs_in(&dir).is_empty() && self.expand(dir) {
                    return Enter::Opened;
                }
                Enter::ToFiles
            }
            Pane::Files => match self.focused_target().cloned() {
                Some(Target::Dir(dir)) => {
                    self.open(dir, library);
                    Enter::Opened
                }
                Some(Target::File(index)) => library
                    .entry(index)
                    .map_or(Enter::Nothing, |entry| Enter::File(entry.rel.clone())),
                None => Enter::Nothing,
            },
        }
    }

    /// `h`: out of the thing under the cursor.
    ///
    /// Closes an open tree node, and otherwise goes to the parent — the two
    /// halves of what `h` means in a tree, in the order a user expects them.
    pub fn leave(&mut self, pane: Pane, library: &Library) -> bool {
        if pane == Pane::Tree && self.expanded.remove(&self.dir) {
            return true;
        }
        self.go_up(library)
    }

    /// `-` / `backspace`: the parent directory, wherever the keyboard is.
    pub fn go_up(&mut self, library: &Library) -> bool {
        let Some(parent) = self.dir.parent() else {
            return false;
        };
        let was = std::mem::replace(&mut self.dir, parent);
        // Leave the child open, so `h` then `l` puts you back where you were.
        self.expand(was);
        self.cursor = 0;
        self.files_offset = 0;
        self.visual = None;
        self.refresh(library);
        true
    }

    /// Make `dir` the current directory and show it in the tree.
    pub fn open(&mut self, dir: DirPath, library: &Library) {
        let mut ancestor = dir.parent();
        while let Some(parent) = ancestor {
            ancestor = parent.parent();
            self.expand(parent);
        }
        if !library.subdirs_in(&dir).is_empty() {
            self.expand(dir.clone());
        }
        self.select(dir);
        self.refresh(library);
    }

    /// Open a tree node. The root is never in the set — it is the pane itself,
    /// always drawn open — so expanding it is not a change and collapsing it is
    /// not a thing `h` can do.
    fn expand(&mut self, dir: DirPath) -> bool {
        !dir.is_root() && self.expanded.insert(dir)
    }

    /// Select a directory without touching what is expanded.
    fn select(&mut self, dir: DirPath) {
        if self.dir == dir {
            return;
        }
        self.dir = dir;
        self.cursor = 0;
        self.files_offset = 0;
        self.visual = None;
    }

    // -- marking ------------------------------------------------------------

    /// `space`: mark or unmark the row under the cursor, then step down.
    ///
    /// Stepping down is what makes marking a run of files three keypresses
    /// instead of six, and it is what every file manager with a mark key does.
    pub fn toggle_mark(&mut self, library: &Library) -> bool {
        let Some(rel) = self.focused_path(library) else {
            return false;
        };
        if !self.marks.remove(&rel) {
            self.marks.insert(rel);
        }
        self.move_cursor(Pane::Files, 1, library);
        true
    }

    /// `v`: start a visual range, or finish one by marking everything in it.
    pub fn visual(&mut self, library: &Library) -> bool {
        let Some(anchor) = self.visual.take() else {
            if self.listing.is_empty() {
                return false;
            }
            self.visual = Some(self.cursor);
            return true;
        };
        let (from, to) = (anchor.min(self.cursor), anchor.max(self.cursor));
        let marked: Vec<RelPath> = self
            .listing
            .get(from..=to)
            .unwrap_or_default()
            .iter()
            .filter_map(|target| path_of(target, library))
            .collect();
        self.marks.extend(marked);
        true
    }

    /// `esc` while a range is open: throw it away without marking anything.
    pub fn cancel_visual(&mut self) -> bool {
        self.visual.take().is_some()
    }

    /// `a`: mark every row of the current listing, directories included.
    pub fn mark_all(&mut self, library: &Library) -> bool {
        let before = self.marks.len();
        let all: Vec<RelPath> = self
            .listing
            .iter()
            .filter_map(|target| path_of(target, library))
            .collect();
        self.marks.extend(all);
        self.marks.len() != before
    }

    /// `A`: unmark everything, everywhere.
    pub fn unmark_all(&mut self) -> bool {
        let had = !self.marks.is_empty() || self.visual.is_some();
        self.marks.clear();
        self.visual = None;
        had
    }

    // -- rows ---------------------------------------------------------------

    /// What the listing points at, sorted: subdirectories first, then files.
    ///
    /// Only the tests read the whole listing — everything the UI does goes
    /// through [`Browser::rows`], which hands out a window of it, because that
    /// is what virtualization means.
    #[cfg(test)]
    #[must_use]
    pub fn targets(&self) -> &[Target] {
        &self.listing
    }

    /// Rebuild the listing. Every mutator that can change it ends here.
    fn refresh(&mut self, library: &Library) {
        let mut dirs: Vec<&DirPath> = library.subdirs_in(&self.dir).iter().collect();
        dirs.sort_by(|a, b| {
            natural_cmp(
                a.file_name().unwrap_or_default(),
                b.file_name().unwrap_or_default(),
            )
        });

        let mut files: Vec<usize> = library.indices_in(&self.dir).to_vec();
        files.sort_by(|&a, &b| self.file_cmp(a, b, library));

        self.listing = dirs
            .into_iter()
            .map(|dir| Target::Dir(dir.clone()))
            .chain(files.into_iter().map(Target::File))
            .collect();
        self.cursor = self.cursor.min(self.listing.len().saturating_sub(1));
    }

    /// Two files, in the order the current sort puts them.
    fn file_cmp(&self, a: usize, b: usize, library: &Library) -> Ordering {
        let (Some(left), Some(right)) = (library.entry(a), library.entry(b)) else {
            return Ordering::Equal;
        };
        let by_name = || natural_cmp(left.file_name(), right.file_name());
        match self.sort {
            Sort::Name => by_name(),
            Sort::Track => self
                .track_of(&left.rel)
                .cmp(&self.track_of(&right.rel))
                .then_with(by_name),
            // Newest and largest first: the two questions these sorts are asked
            // are "what did I just add?" and "what is eating the disk?".
            Sort::Mtime => right.mtime.cmp(&left.mtime).then_with(by_name),
            Sort::Size => right.size.cmp(&left.size).then_with(by_name),
        }
    }

    /// A file's track number, or [`u32::MAX`] when it has none yet — which puts
    /// unread and untagged files after the ones that have a number, rather than
    /// shuffling them to the top as a zero would.
    fn track_of(&self, rel: &RelPath) -> u32 {
        match self.tags.get(rel) {
            Some(Cached::Read(info)) => info.tags.track.map_or(u32::MAX, |pair| pair.0),
            _ => u32::MAX,
        }
    }

    /// The tree pane's rows: the root, then every expanded node's children.
    #[must_use]
    pub fn tree_rows(&self, library: &Library) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_tree(&DirPath::root(), 0, library, &mut rows);
        rows
    }

    /// One tree node and, if it is open, everything under it.
    fn push_tree(&self, dir: &DirPath, depth: usize, library: &Library, rows: &mut Vec<TreeRow>) {
        let children = library.subdirs_in(dir);
        // The root is always open: it is the pane, not a node in it.
        let expanded = dir.is_root() || self.expanded.contains(dir);
        rows.push(TreeRow {
            dir: dir.clone(),
            depth,
            has_children: !children.is_empty(),
            expanded,
            referenced: self.flagged.contains(dir),
        });
        if !expanded {
            return;
        }
        let mut sorted: Vec<&DirPath> = children.iter().collect();
        sorted.sort_by(|a, b| {
            natural_cmp(
                a.file_name().unwrap_or_default(),
                b.file_name().unwrap_or_default(),
            )
        });
        for child in sorted {
            self.push_tree(child, depth + 1, library, rows);
        }
    }

    /// Which tree row the current directory is on.
    fn tree_cursor(&self, library: &Library) -> usize {
        self.tree_rows(library)
            .iter()
            .position(|row| row.dir == self.dir)
            .unwrap_or(0)
    }

    /// What the listing's cursor is on.
    #[must_use]
    pub fn focused_target(&self) -> Option<&Target> {
        self.listing.get(self.cursor)
    }

    /// The path the listing's cursor is on.
    #[must_use]
    pub fn focused_path(&self, library: &Library) -> Option<RelPath> {
        path_of(self.focused_target()?, library)
    }

    // -- tags ---------------------------------------------------------------

    /// The paths a worker should read next, given how many rows are on screen.
    ///
    /// Everything it returns is marked as asked-for, so calling it twice without
    /// an answer in between returns nothing the second time and a held-down `j`
    /// does not start a thread per row.
    ///
    /// The window is the visible rows — except under [`Sort::Track`], where it is
    /// the whole directory, because the sort cannot be computed from a window of
    /// itself.
    pub fn wanted(&mut self, library: &Library, rows: usize) -> Vec<RelPath> {
        let targets = std::mem::take(&mut self.listing);
        let visible = if self.sort == Sort::Track {
            0..targets.len()
        } else {
            window(self.files_offset, self.cursor, rows, targets.len())
        };

        let mut wanted = Vec::new();
        for target in targets.get(visible).unwrap_or_default() {
            let Target::File(index) = target else {
                continue;
            };
            let Some(entry) = library.entry(*index) else {
                continue;
            };
            if !entry.is_audio() || self.tags.contains_key(&entry.rel) {
                continue;
            }
            if self.asked.insert(entry.rel.clone()) {
                wanted.push(entry.rel.clone());
            }
        }
        self.listing = targets;
        wanted
    }

    /// A worker answered. Returns whether anything on screen would change.
    pub fn tags_arrived(&mut self, reads: Vec<(RelPath, Result<TrackInfo, String>)>) -> bool {
        let mut changed = false;
        for (rel, result) in reads {
            self.asked.remove(&rel);
            let cached = match result {
                Ok(info) => Cached::Read(Box::new(info)),
                Err(message) => Cached::Failed(message),
            };
            self.tags.insert(rel, cached);
            changed = true;
        }
        changed
    }

    /// How many files have been read, for the tests that count them.
    #[cfg(test)]
    #[must_use]
    pub fn cached(&self) -> usize {
        self.tags.len()
    }

    // -- drawing ------------------------------------------------------------

    /// The listing's visible rows, and where the cursor is inside them.
    ///
    /// This is the virtualization boundary: `rows` is as long as the pane is
    /// tall, whatever the directory holds.
    #[must_use]
    pub fn rows(&self, library: &Library, height: usize) -> Window<Row> {
        let range = window(self.files_offset, self.cursor, height, self.listing.len());
        let rows = self
            .listing
            .get(range.clone())
            .unwrap_or_default()
            .iter()
            .map(|target| self.row(target, library))
            .collect();
        Window {
            rows,
            cursor: self.cursor.checked_sub(range.start),
            total: self.listing.len(),
            range,
        }
    }

    /// One listing row, resolved for the widget.
    fn row(&self, target: &Target, library: &Library) -> Row {
        match target {
            Target::Dir(dir) => Row {
                name: dir.file_name().unwrap_or("/").to_owned(),
                kind: RowKind::Dir,
                marked: dir.as_rel().is_some_and(|rel| self.marks.contains(rel)),
                referenced: self.flagged.contains(dir),
                meta: Meta::None,
            },
            Target::File(index) => {
                let Some(entry) = library.entry(*index) else {
                    return Row {
                        name: String::new(),
                        kind: RowKind::Other,
                        marked: false,
                        referenced: false,
                        meta: Meta::None,
                    };
                };
                Row {
                    name: entry.file_name().to_owned(),
                    kind: if entry.is_audio() {
                        RowKind::Audio
                    } else {
                        RowKind::Other
                    },
                    marked: self.marks.contains(&entry.rel),
                    referenced: self.flagged.contains(&DirPath::from(entry.rel.clone())),
                    meta: self.meta_of(&entry.rel, entry.kind),
                }
            }
        }
    }

    /// The duration and bitrate columns' state for one file.
    fn meta_of(&self, rel: &RelPath, kind: Kind) -> Meta {
        if !kind.is_audio() {
            return Meta::None;
        }
        match self.tags.get(rel) {
            Some(Cached::Read(info)) => Meta::Known {
                duration: info.info.duration_hms(),
                bitrate: format!("{}k", info.info.bitrate),
            },
            Some(Cached::Failed(_)) => Meta::Failed,
            None => Meta::Reading,
        }
    }

    /// The tree pane's visible rows.
    #[must_use]
    pub fn tree(&self, library: &Library, height: usize) -> Window<TreeRow> {
        let rows = self.tree_rows(library);
        let cursor = self.tree_cursor(library);
        let range = window(self.tree_offset, cursor, height, rows.len());
        Window {
            cursor: cursor.checked_sub(range.start),
            rows: rows.get(range.clone()).unwrap_or_default().to_vec(),
            total: rows.len(),
            range,
        }
    }

    /// Remember where each pane was scrolled to, so the next frame starts there.
    ///
    /// Called after drawing with the ranges that were actually used. The offsets
    /// are hints — [`window`] re-derives a range that contains the cursor
    /// whatever they say — so a stale one is a cosmetic jump and never a cursor
    /// nobody can see.
    pub fn scrolled(&mut self, tree: usize, files: usize) {
        self.tree_offset = tree;
        self.files_offset = files;
    }

    /// The inclusive range of listing rows an open visual selection covers.
    #[must_use]
    pub fn visual_range(&self) -> Option<(usize, usize)> {
        let anchor = self.visual?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// What the details pane shows for the focused row.
    #[must_use]
    pub fn details(&self, library: &Library, index: Option<&PlaylistIndex>) -> Details {
        let Some(target) = self.focused_target().cloned() else {
            return Details {
                name: "nothing selected".to_owned(),
                ..Details::default()
            };
        };

        match target {
            Target::Dir(dir) => {
                let files = library.indices_in(&dir).len();
                let subdirs = library.subdirs_in(&dir).len();
                Details {
                    name: dir.file_name().unwrap_or("/").to_owned(),
                    rows: vec![
                        Detail::new("Kind", "directory"),
                        Detail::new("Files", files.to_string()),
                        Detail::new("Dirs", subdirs.to_string()),
                    ],
                    note: self
                        .flagged
                        .contains(&dir)
                        .then(|| "a playlist points inside".to_owned()),
                    ..Details::default()
                }
            }
            Target::File(entry_index) => {
                let Some(entry) = library.entry(entry_index) else {
                    return Details::default();
                };
                let refs = index.map_or(0, |index| index.refs_to(&entry.rel).len());
                let playlists = index.map_or_else(Vec::new, |index| {
                    let mut names: Vec<String> = index
                        .refs_to(&entry.rel)
                        .iter()
                        .filter_map(|reference| index.playlist(reference.playlist))
                        .map(|playlist| playlist.name().to_owned())
                        .collect();
                    names.sort_unstable();
                    names.dedup();
                    names
                });

                let mut rows = Vec::new();
                let mut note = None;
                match self.tags.get(&entry.rel) {
                    Some(Cached::Read(info)) => {
                        // The tags first: they are what the user is reading the
                        // pane for, and the file's own facts are below them.
                        for field in [
                            Field::Title,
                            Field::Artist,
                            Field::Album,
                            Field::Genre,
                            Field::Track,
                        ] {
                            rows.push(Detail::new(label_of(field), info.tags.get(field).joined()));
                        }
                        rows.push(Detail::new("Length", info.info.duration_hms()));
                        rows.push(Detail::new(
                            "Bitrate",
                            format!("{} kbps", info.info.bitrate),
                        ));
                        rows.push(Detail::new("Rate", format!("{} Hz", info.info.sample_rate)));
                    }
                    Some(Cached::Failed(message)) => note = Some(message.clone()),
                    None if entry.is_audio() => note = Some("reading…".to_owned()),
                    None => {}
                }
                rows.push(Detail::new("Kind", entry.kind.to_string()));
                rows.push(Detail::new("Size", human_size(entry.size)));

                Details {
                    name: entry.file_name().to_owned(),
                    rows,
                    playlists,
                    references: refs,
                    note,
                }
            }
        }
    }

    /// Why the listing is empty, in the words the user needs.
    ///
    /// A directory the scan could not read looks exactly like an empty one in the
    /// model — the walk recorded the directory and then failed to list it — so
    /// the difference is in the warnings, and saying which of the two it is, is
    /// the whole of "a permission-denied directory renders sensibly".
    #[must_use]
    pub fn empty_reason(&self, library: &Library) -> String {
        let abs = self.dir.to_abs(library.root());
        let unreadable = library.warnings().iter().find_map(|warning| match warning {
            ScanWarning::Unreadable { path, message } if path == abs.as_str() => Some(message),
            _ => None,
        });
        match unreadable {
            Some(message) => format!("cannot be read: {message}"),
            None => "empty directory".to_owned(),
        }
    }
}

/// What `l` or `enter` turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enter {
    /// A directory was opened; the listing has changed.
    Opened,
    /// The tree node was already open, so the keyboard should move right.
    ToFiles,
    /// A file, which this task has nothing to open with.
    File(RelPath),
    /// There was nothing under the cursor.
    Nothing,
}

/// A slice of a listing, plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window<T> {
    /// The rows that are on screen, and no others.
    pub rows: Vec<T>,
    /// Where the cursor is inside `rows`, if it is inside them at all.
    pub cursor: Option<usize>,
    /// Which rows of the whole listing these are.
    pub range: Range<usize>,
    /// How long the whole listing is.
    pub total: usize,
}

/// The rows a pane `height` tall shows, given where it was scrolled to.
///
/// `offset` is a **hint**: the range always contains `cursor`, so a stale offset
/// — from a resize, a new sort, a rescan — is corrected rather than believed. The
/// hint is what makes scrolling sticky, and the correction is what makes it
/// impossible to lose the cursor off the top or the bottom of a pane.
///
/// Returns an empty range for a pane with no height and for a listing with no
/// rows, both of which are states the browser is really in: the first before the
/// first resize, the second in an empty directory.
#[must_use]
pub fn window(offset: usize, cursor: usize, height: usize, total: usize) -> Range<usize> {
    if height == 0 || total == 0 {
        return 0..0;
    }
    let last_start = total.saturating_sub(height);
    let start = offset
        .min(last_start)
        .min(cursor)
        .max(cursor.saturating_sub(height - 1));
    start..(start + height).min(total)
}

/// Compare two names the way a person reads them: digits as numbers.
///
/// `02 Hoe Cakes` before `10 Deep Fried Frenz`, which a byte comparison gets
/// backwards. Case is ignored for the ordering and used only to break a tie, so
/// `a.mp3` and `A.mp3` sort next to each other and still have a stable order.
///
/// Digit runs are compared by value and not by length, so `2` and `02` are equal
/// here and separated by the tie-break — which keeps the whole thing a total
/// order, as a sort comparator has to be.
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a, b);
    loop {
        let (Some(x), Some(y)) = (left.chars().next(), right.chars().next()) else {
            break;
        };
        if x.is_ascii_digit() && y.is_ascii_digit() {
            let (nx, ny) = (digits(left), digits(right));
            let ord = compare_numbers(nx, ny);
            if ord != Ordering::Equal {
                return ord;
            }
            left = &left[nx.len()..];
            right = &right[ny.len()..];
        } else {
            let ord = lower(x).cmp(&lower(y));
            if ord != Ordering::Equal {
                return ord;
            }
            left = &left[x.len_utf8()..];
            right = &right[y.len_utf8()..];
        }
    }
    // One ran out, or every chunk compared equal: fall back to a comparison
    // that is total, so two names never compare equal unless they are equal.
    left.len().cmp(&right.len()).then_with(|| a.cmp(b))
}

/// The run of ASCII digits at the front of `s`, which may be empty.
///
/// A slice and not a `String`: this is the comparator's hot path, and sorting a
/// 400-entry directory is a few thousand calls of it.
fn digits(s: &str) -> &str {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    &s[..end]
}

/// Two runs of digits, by value — without parsing, so a 40-digit run in a scene
/// release's name cannot overflow anything.
fn compare_numbers(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// The lower-case form of one character, for comparison only.
fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// The path a listing row stands for.
fn path_of(target: &Target, library: &Library) -> Option<RelPath> {
    match target {
        Target::Dir(dir) => dir.as_rel().cloned(),
        Target::File(index) => library.entry(*index).map(|entry| entry.rel.clone()),
    }
}

/// Every directory with a playlist-referenced file somewhere inside it.
///
/// Computed once per scan, from the index's own key set — 231 references in the
/// real setup, so this is a few hundred `HashSet` inserts and not something worth
/// being clever about. It is what makes `⚠` appear on `hiphop/` and not only on
/// the one track inside it, which is the task's "flag directories containing such
/// entries".
fn flagged_dirs(index: &PlaylistIndex) -> HashSet<DirPath> {
    let mut flagged = HashSet::new();
    for rel in index.paths() {
        // The file itself, so a listing row can be flagged, and then every
        // directory above it.
        let mut dir = Some(DirPath::from(rel.clone()));
        while let Some(current) = dir {
            dir = current.parent();
            if !flagged.insert(current) {
                // Already walked up from another file in the same directory.
                break;
            }
        }
    }
    flagged
}

/// The details pane's label for a tag field — capitalized, because it is a
/// heading and not the `--clear <field>` name the CLI takes.
fn label_of(field: Field) -> &'static str {
    match field {
        Field::Title => "Title",
        Field::Artist => "Artist",
        Field::AlbumArtist => "Alb.art",
        Field::Album => "Album",
        Field::Year => "Year",
        Field::Track => "Track",
        Field::Disc => "Disc",
        Field::Genre => "Genre",
        Field::Comment => "Comment",
        Field::Composer => "Composer",
    }
}

/// A size a person can read, in the two units a music library needs.
fn human_size(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display string; the error is below the one decimal shown"
    )]
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{} kB", bytes / 1024)
    }
}

#[cfg(test)]
mod tests {
    use mpdfm_core::testing::{Fixture, names};

    use super::*;

    /// A browser over a freshly scanned fixture.
    fn browser(fx: &Fixture) -> (Browser, Library, PlaylistIndex) {
        let library = Library::scan(fx.music_dir()).expect("the fixture scans");
        let (index, _) = PlaylistIndex::load(fx.playlist_dir());
        let mut browser = Browser::new();
        browser.library_changed(&library, Some(&index));
        (browser, library, index)
    }

    /// The listing's rows as names, for an assertion that reads like the screen.
    fn names_in(browser: &Browser, library: &Library) -> Vec<String> {
        browser
            .targets()
            .iter()
            .map(|target| match target {
                Target::Dir(dir) => format!("{}/", dir.file_name().unwrap_or("/")),
                Target::File(index) => library
                    .entry(*index)
                    .map_or_else(String::new, |entry| entry.file_name().to_owned()),
            })
            .collect()
    }

    fn dir(path: &str) -> DirPath {
        DirPath::parse(path).expect("a fixture path")
    }

    // -- the window --------------------------------------------------------

    #[test]
    fn a_window_always_contains_the_cursor_however_stale_the_offset_is() {
        // 400 rows, a 20-row pane, and an offset left over from somewhere else.
        for cursor in [0, 1, 19, 20, 200, 398, 399] {
            for offset in [0, 7, 380, 399, 10_000] {
                let range = window(offset, cursor, 20, 400);
                assert!(
                    range.contains(&cursor),
                    "offset {offset}, cursor {cursor}: {range:?}"
                );
                assert_eq!(range.len(), 20, "a full pane is always full");
                assert!(range.end <= 400);
            }
        }
    }

    #[test]
    fn a_window_degenerates_sensibly() {
        assert_eq!(window(0, 0, 0, 400), 0..0, "a pane with no height");
        assert_eq!(window(0, 0, 20, 0), 0..0, "a listing with no rows");
        assert_eq!(window(0, 2, 20, 3), 0..3, "a listing shorter than the pane");
        assert_eq!(window(99, 2, 20, 3), 0..3, "and a nonsense offset for it");
    }

    #[test]
    fn scrolling_is_sticky_when_the_offset_still_holds_the_cursor() {
        // The point of keeping an offset at all: the view does not re-centre on
        // every keypress.
        assert_eq!(window(100, 105, 20, 400), 100..120);
        assert_eq!(window(100, 119, 20, 400), 100..120);
        // One past the bottom and it scrolls by exactly one row.
        assert_eq!(window(100, 120, 20, 400), 101..121);
    }

    // -- natural sort ------------------------------------------------------

    #[test]
    fn natural_sort_puts_02_before_10() {
        let mut names = [
            "10 Deep Fried Frenz.mp3",
            "2 Hoe Cakes.mp3",
            "02 Hoe Cakes.mp3",
            "1 Beef Rap.mp3",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names[0], "1 Beef Rap.mp3");
        assert_eq!(names[3], "10 Deep Fried Frenz.mp3");
        // A byte sort would have put `10` second, which is the bug this exists
        // to avoid.
        assert_ne!(names[1], "10 Deep Fried Frenz.mp3");
    }

    #[test]
    fn natural_sort_is_a_total_order() {
        // A comparator that reports two different strings equal makes
        // `sort_by`'s output depend on the input order, which is how a listing
        // starts shuffling between frames.
        let names = ["2 a", "02 a", "2 A", "a", "A", "", "10", "9", "ノ", "ï"];
        for a in names {
            for b in names {
                assert_eq!(
                    natural_cmp(a, b) == Ordering::Equal,
                    a == b,
                    "{a:?} vs {b:?}"
                );
                assert_eq!(
                    natural_cmp(a, b).reverse(),
                    natural_cmp(b, a),
                    "{a:?} vs {b:?} is not antisymmetric"
                );
            }
        }
    }

    #[test]
    fn natural_sort_ignores_case_except_to_break_a_tie() {
        assert_eq!(natural_cmp("album", "Beef"), Ordering::Less);
        assert_eq!(natural_cmp("Beef", "album"), Ordering::Greater);
        // Same letters, different case: adjacent, and in a fixed order.
        assert_eq!(natural_cmp("a", "A"), Ordering::Greater);
    }

    #[test]
    fn a_very_long_digit_run_does_not_overflow() {
        let long = format!("x{}1.mp3", "9".repeat(40));
        let longer = format!("x{}2.mp3", "9".repeat(40));
        assert_eq!(natural_cmp(&long, &longer), Ordering::Less);
    }

    // -- navigation --------------------------------------------------------

    #[test]
    fn it_starts_at_the_root_and_lists_the_genres() {
        let fx = Fixture::realistic();
        let (browser, library, _) = browser(&fx);
        assert!(browser.dir().is_root());
        assert_eq!(browser.dir_label(), "/");

        let rows = names_in(&browser, &library);
        assert!(rows.contains(&"hiphop/".to_owned()), "{rows:?}");
        assert!(rows.contains(&"electronic/".to_owned()), "{rows:?}");
    }

    #[test]
    fn l_and_h_go_in_and_back_out_again() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);

        // Into `coding-music`, which is the first row at the root.
        let rows = names_in(&browser, &library);
        assert_eq!(rows[0], "coding-music/");
        assert_eq!(browser.enter(Pane::Files, &library), Enter::Opened);
        assert_eq!(browser.dir().as_str(), "coding-music");

        assert!(browser.leave(Pane::Files, &library));
        assert!(browser.dir().is_root());
        // And the child it came out of is left open in the tree.
        let tree: Vec<String> = browser
            .tree_rows(&library)
            .iter()
            .map(|row| row.dir.to_string())
            .collect();
        assert!(
            tree.contains(&"coding-music/SwitchAngel".to_owned()),
            "coming back out should leave the branch open: {tree:?}"
        );
    }

    #[test]
    fn entering_a_file_hands_it_back_rather_than_pretending_to_open_it() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        // The first row of an album directory is a file; the directory has none.
        let what = browser.enter(Pane::Files, &library);
        assert!(matches!(what, Enter::File(_)), "{what:?}");
    }

    #[test]
    fn the_tree_expands_the_path_to_wherever_the_listing_went() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MERCURY_CD1), &library);

        let rows = browser.tree_rows(&library);
        let shown: Vec<&str> = rows.iter().map(|row| row.dir.as_str()).collect();
        assert!(shown.contains(&names::MERCURY_CD1), "{shown:?}");
        assert!(shown.contains(&names::MERCURY_ALBUM), "{shown:?}");
        assert!(shown.contains(&"pop"), "{shown:?}");
        // And the branch that was not entered is still closed.
        assert!(
            !shown.contains(&names::MF_DOOM_ALBUM),
            "an unvisited branch should stay shut: {shown:?}"
        );
    }

    #[test]
    fn moving_the_tree_cursor_changes_what_the_listing_shows() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        assert!(browser.dir().is_root());

        // Row 0 of the tree is the root itself; row 1 is its first child.
        assert!(browser.move_cursor(Pane::Tree, 1, &library));
        assert_eq!(browser.dir().as_str(), "coding-music");
        assert_eq!(browser.pane_cursor(Pane::Tree, &library), 1);
    }

    #[test]
    fn a_rescan_that_lost_the_current_directory_walks_up_to_one_that_exists() {
        let fx = Fixture::realistic();
        let (mut browser, library, index) = browser(&fx);
        browser.open(dir(names::MERCURY_CD1), &library);

        // A library that no longer has the disc directory in it.
        let smaller = Fixture::builder().album("pop", &["01 Wrecked.mp3"]).build();
        let smaller = Library::scan(smaller.music_dir()).expect("it scans");
        browser.library_changed(&smaller, Some(&index));

        assert!(
            browser.dir().is_root() || smaller.dir(browser.dir()).is_some(),
            "it should land somewhere real, not on {}",
            browser.dir()
        );
    }

    // -- marking -----------------------------------------------------------

    #[test]
    fn marks_survive_a_change_of_directory() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);

        browser.open(dir(names::MF_DOOM_ALBUM), &library);
        assert!(browser.toggle_mark(&library));
        assert_eq!(browser.marked(), 1);

        browser.open(dir(names::KIND_OF_BLUE_ALBUM), &library);
        assert_eq!(browser.marked(), 1, "navigating must not clear a mark");
        assert!(browser.toggle_mark(&library));
        assert_eq!(
            browser.marked(),
            2,
            "and marks accumulate across directories"
        );

        let marks = browser.marks();
        assert!(marks.iter().any(|rel| rel.as_str().contains("MF DOOM")));
        assert!(
            marks
                .iter()
                .any(|rel| rel.as_str().contains("Kind of Blue"))
        );
    }

    #[test]
    fn marking_steps_down_so_a_run_of_files_is_one_key_each() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        browser.toggle_mark(&library);
        browser.toggle_mark(&library);
        assert_eq!(browser.marked(), 2);
        assert_eq!(browser.cursor(), 2);
    }

    #[test]
    fn v_marks_a_contiguous_range() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        assert!(browser.visual(&library), "`v` opens a range");
        assert!(browser.in_visual());
        browser.move_cursor(Pane::Files, 2, &library);
        assert_eq!(browser.visual_range(), Some((0, 2)));
        assert_eq!(browser.marked(), 0, "nothing is marked until it is closed");

        assert!(browser.visual(&library), "`v` closes it");
        assert!(!browser.in_visual());
        assert_eq!(browser.marked(), 3);
    }

    #[test]
    fn a_range_opened_upwards_marks_the_same_rows() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        browser.move_cursor(Pane::Files, 2, &library);
        browser.visual(&library);
        browser.move_cursor(Pane::Files, -2, &library);
        assert_eq!(browser.visual_range(), Some((0, 2)));
        browser.visual(&library);
        assert_eq!(browser.marked(), 3);
    }

    #[test]
    fn esc_abandons_a_range_without_marking_anything() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        browser.visual(&library);
        browser.move_cursor(Pane::Files, 2, &library);
        assert!(browser.cancel_visual());
        assert_eq!(browser.marked(), 0);
        assert!(
            !browser.cancel_visual(),
            "and there is nothing left to cancel"
        );
    }

    #[test]
    fn a_marks_everything_in_view_and_capital_a_clears_the_lot() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        assert!(browser.mark_all(&library));
        // Three tracks and five aux files: the non-audio ones are marked too,
        // because they are what travels with the album.
        assert_eq!(browser.marked(), 8);

        browser.open(dir(names::KIND_OF_BLUE_ALBUM), &library);
        assert_eq!(browser.marked(), 8, "`a` in another directory keeps these");
        assert!(browser.unmark_all());
        assert_eq!(browser.marked(), 0);
    }

    #[test]
    fn a_directory_can_be_marked_because_moving_an_album_is_the_point() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir("hiphop"), &library);

        assert!(browser.toggle_mark(&library));
        let marks = browser.marks();
        assert_eq!(marks.len(), 1);
        assert!(marks[0].as_str().starts_with("hiphop/"), "{marks:?}");
    }

    // -- sorting -----------------------------------------------------------

    #[test]
    fn the_listing_sorts_directories_first_whatever_the_order_is() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MERCURY_ALBUM), &library);

        for sort in Sort::ALL {
            browser.set_sort(*sort, Some(&library));
            let rows = names_in(&browser, &library);
            assert!(rows[0].ends_with('/'), "{sort}: {rows:?}");
        }
    }

    #[test]
    fn size_and_mtime_put_the_biggest_and_newest_first() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        browser.set_sort(Sort::Size, Some(&library));
        let sizes: Vec<u64> = browser
            .targets()
            .iter()
            .filter_map(|target| match target {
                Target::File(index) => library.entry(*index).map(|entry| entry.size),
                Target::Dir(_) => None,
            })
            .collect();
        assert!(
            sizes.windows(2).all(|pair| pair[0] >= pair[1]),
            "largest first: {sizes:?}"
        );
    }

    #[test]
    fn the_sort_is_remembered_and_only_takes_names_it_has() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);

        assert_eq!(browser.sort(), Sort::Name);
        assert!(browser.set_sort(Sort::Mtime, Some(&library)));
        assert!(
            !browser.set_sort(Sort::Mtime, Some(&library)),
            "setting it twice is not a change"
        );
        browser.open(dir(names::MF_DOOM_ALBUM), &library);
        assert_eq!(browser.sort(), Sort::Mtime, "it survives navigation");

        assert_eq!(Sort::parse("track"), Some(Sort::Track));
        assert_eq!(Sort::parse("Track"), None);
        assert_eq!(Sort::parse("nonsense"), None);
    }

    // -- tags --------------------------------------------------------------

    #[test]
    fn only_the_visible_audio_rows_are_asked_for() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        // Two rows of pane for eight rows of directory.
        let wanted = browser.wanted(&library, 2);
        assert_eq!(wanted.len(), 2, "{wanted:?}");
        assert!(
            wanted.iter().all(|rel| rel.as_str().ends_with(".mp3")),
            "only audio: {wanted:?}"
        );

        // Asking again with nothing having come back asks for nothing: a held
        // `j` must not start a thread per row.
        assert!(browser.wanted(&library, 2).is_empty());
    }

    #[test]
    fn a_non_audio_row_is_never_asked_for() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        // The whole directory: three tracks and five aux files.
        let wanted = browser.wanted(&library, 40);
        assert_eq!(wanted.len(), 3, "{wanted:?}");
    }

    #[test]
    fn track_order_asks_for_the_whole_directory_because_a_window_cannot_sort_itself() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);
        browser.set_sort(Sort::Track, Some(&library));

        let wanted = browser.wanted(&library, 1);
        assert_eq!(wanted.len(), 3, "one row of pane, three tracks: {wanted:?}");
    }

    #[test]
    fn an_answer_fills_the_cache_and_stops_the_asking() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        let wanted = browser.wanted(&library, 40);
        let reads = wanted
            .iter()
            .map(|rel| {
                let (tags, info) =
                    mpdfm_core::tags::read(&rel.to_abs(library.root())).expect("the fixture reads");
                (rel.clone(), Ok(TrackInfo { tags, info }))
            })
            .collect();
        assert!(browser.tags_arrived(reads));
        assert_eq!(browser.cached(), 3);
        assert!(
            browser.wanted(&library, 40).is_empty(),
            "a cached row is never read twice"
        );
    }

    #[test]
    fn a_file_that_will_not_read_is_cached_as_a_failure_and_not_retried_forever() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        let wanted = browser.wanted(&library, 1);
        let rel = wanted[0].clone();
        browser.tags_arrived(vec![(rel.clone(), Err("not an mp3".to_owned()))]);
        assert!(browser.wanted(&library, 1).is_empty());

        let rows = browser.rows(&library, 1);
        assert_eq!(rows.rows[0].meta, Meta::Failed);
    }

    #[test]
    fn a_rescan_drops_the_cache_because_a_stale_bitrate_is_worse_than_none() {
        let fx = Fixture::realistic();
        let (mut browser, library, index) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);
        let wanted = browser.wanted(&library, 40);
        browser.tags_arrived(
            wanted
                .iter()
                .map(|rel| (rel.clone(), Err("whatever".to_owned())))
                .collect(),
        );
        assert_eq!(browser.cached(), 3);

        browser.library_changed(&library, Some(&index));
        assert_eq!(browser.cached(), 0);
    }

    // -- rows and details ---------------------------------------------------

    #[test]
    fn the_listing_hands_out_only_as_many_rows_as_the_pane_is_tall() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        let shown = browser.rows(&library, 3);
        assert_eq!(shown.rows.len(), 3);
        assert_eq!(shown.total, 8, "the directory is longer than the pane");
        assert_eq!(shown.cursor, Some(0));
    }

    #[test]
    fn non_audio_rows_are_listed_and_marked_as_different() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        let shown = browser.rows(&library, 40);
        let jpg = shown
            .rows
            .iter()
            .find(|row| row.name == "folder.jpg")
            .expect("the cover art is in the listing");
        assert_eq!(jpg.kind, RowKind::Other);
        assert_eq!(jpg.meta, Meta::None, "a jpg has no bitrate to wait for");

        let mp3 = shown
            .rows
            .iter()
            .find(|row| row.name == "01 Beef Rap.mp3")
            .expect("and so is the track");
        assert_eq!(mp3.kind, RowKind::Audio);
    }

    #[test]
    fn the_details_pane_names_the_playlists_that_reference_the_track() {
        let fx = Fixture::realistic();
        let (mut browser, library, index) = browser(&fx);
        browser.open(dir(names::MF_DOOM_ALBUM), &library);

        // `01 Beef Rap.mp3` is the one two playlists point at.
        let details = browser.details(&library, Some(&index));
        assert_eq!(details.name, "01 Beef Rap.mp3");
        assert_eq!(details.references, 2, "{details:?}");
        // `Playlist::name` is what MPD calls a playlist: the file name without
        // its `.m3u`, which is also what the details pane has room for.
        assert_eq!(
            details.playlists,
            vec!["Hip hop".to_owned(), "MF Doom".to_owned()],
            "{details:?}"
        );
    }

    #[test]
    fn a_track_nothing_references_says_nothing_rather_than_guessing() {
        let fx = Fixture::realistic();
        let (mut browser, library, index) = browser(&fx);
        browser.open(dir(names::KIND_OF_BLUE_ALBUM), &library);

        let details = browser.details(&library, Some(&index));
        assert_eq!(details.references, 0);
        assert!(details.playlists.is_empty());
    }

    #[test]
    fn a_directory_with_a_referenced_file_inside_it_is_flagged_too() {
        let fx = Fixture::realistic();
        let (browser, library, _) = browser(&fx);

        // The root listing: `hiphop/` holds the two referenced tracks.
        let shown = browser.rows(&library, 40);
        let hiphop = shown
            .rows
            .iter()
            .find(|row| row.name == "hiphop")
            .expect("the genre directory is there");
        assert!(
            hiphop.referenced,
            "a move of hiphop/ would rewrite playlists, and the browser has to say so"
        );

        let coding = shown
            .rows
            .iter()
            .find(|row| row.name == "coding-music")
            .expect("and so is this one");
        assert!(!coding.referenced, "nothing points into coding-music");
    }

    // -- the awkward directories -------------------------------------------

    #[test]
    fn an_empty_directory_says_it_is_empty() {
        let fx = Fixture::builder()
            .album("pop/empty-ish", &["a.mp3"])
            .build();
        std::fs::create_dir(fx.music_dir().join("pop/nothing")).expect("mkdir");
        let library = Library::scan(fx.music_dir()).expect("it scans");
        let mut browser = Browser::new();
        browser.library_changed(&library, None);
        browser.open(dir("pop/nothing"), &library);

        assert!(browser.targets().is_empty());
        assert_eq!(browser.empty_reason(&library), "empty directory");
        assert_eq!(browser.details(&library, None).name, "nothing selected");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_read_says_why_rather_than_looking_empty() {
        use std::os::unix::fs::PermissionsExt as _;

        let fx = Fixture::builder().album("pop/fine", &["a.mp3"]).build();
        let locked = fx.music_dir().join("pop/locked");
        std::fs::create_dir(&locked).expect("mkdir");
        std::fs::write(locked.join("secret.mp3"), b"x").expect("write");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");

        let library = Library::scan(fx.music_dir()).expect("the rest of it still scans");
        let mut browser = Browser::new();
        browser.library_changed(&library, None);
        browser.open(dir("pop/locked"), &library);

        let reason = browser.empty_reason(&library);
        // Running as root reads it anyway, which is a fine outcome too — what
        // must not happen is a silent empty listing on a directory that failed.
        if library.warnings().is_empty() {
            assert_eq!(reason, "empty directory");
        } else {
            assert!(reason.starts_with("cannot be read:"), "{reason}");
        }

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("put it back so the fixture can clean up");
    }

    #[test]
    fn a_name_that_is_not_ascii_comes_through_whole() {
        let fx = Fixture::realistic();
        let (mut browser, library, _) = browser(&fx);
        browser.open(dir(names::KREAM_ALBUM), &library);

        let rows = names_in(&browser, &library);
        assert!(rows.contains(&"01 So Hï.mp3".to_owned()), "{rows:?}");
        assert!(rows.contains(&"03 ノスタルジア.mp3".to_owned()), "{rows:?}");
        assert!(
            browser.dir_label().contains("So Hï"),
            "{}",
            browser.dir_label()
        );
    }
}
