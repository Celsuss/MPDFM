//! What a scan produces: [`Entry`] and its [`Kind`], the [`DirPath`] that keys
//! the by-directory index, the derived [`AlbumDir`] view, and the [`Library`]
//! that holds all of it.
//!
//! Nothing here touches the filesystem — [`Library::scan`] is the only door in,
//! and it delegates the walk to `scan.rs`. Keeping the structure separate from
//! the I/O is what lets the assembly rules (sort order, the by-directory index,
//! what counts as an album directory) be tested without a temp directory.
//!
//! # No tags
//!
//! An [`Entry`] carries a path, a kind, a size and an mtime — never a tag. The
//! real library is ~2 800 files and reading every tag at startup would cost
//! seconds for data most runs never look at, so tag reading is lazy and by index
//! (task 16): the browser resolves the rows it is about to draw with
//! [`Library::indices_in`] and asks for tags for those alone.

use std::collections::BTreeMap;
use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};

use crate::paths::{PathError, RelPath};

/// An audio container MPDFM reads and writes tags in.
///
/// The three MPD is fed on this machine: 2 440 mp3, 357 flac, 5 m4a
/// (`docs/PLAN.md` §3). A format MPDFM cannot write tags to has no business
/// being in this enum — it is [`Kind::Other`], scanned and moved but not edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Format {
    /// MPEG-1 Layer III, ID3v2.3 or v2.4.
    Mp3,
    /// FLAC with Vorbis comments.
    Flac,
    /// MP4/AAC with iTunes-style atoms.
    M4a,
}

impl Format {
    /// The format an extension names, case-insensitively, or `None` if it names
    /// no audio format MPDFM handles.
    ///
    /// ```
    /// use mpdfm_core::library::Format;
    ///
    /// assert_eq!(Format::from_extension("FLAC"), Some(Format::Flac));
    /// assert_eq!(Format::from_extension("wav"), None);
    /// ```
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "mp3" => Some(Self::Mp3),
            "flac" => Some(Self::Flac),
            "m4a" => Some(Self::M4a),
            _ => None,
        }
    }

    /// The canonical lower-case extension.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::M4a => "m4a",
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a file in the library is, decided by extension alone.
///
/// The classification is deliberately shallow: no content sniffing, no tag read,
/// just the extension lower-cased. It exists to answer two questions — "is this
/// a track the user can edit?" and "what else has to travel with the album?" —
/// and both are answered well enough by the name.
///
/// **Nothing is silently dropped.** A file MPDFM has no opinion about is
/// [`Kind::Other`] and still appears in the model, because a reorg moves the
/// whole album directory: the `.nfo`, the `.sfv`, the stray `Thumbs.db`. A kind
/// the scanner refused to model would be clutter left behind in a half-moved
/// album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A track: `mp3`, `flac`, `m4a`.
    Audio(Format),
    /// Cover art and screenshots: `jpg`, `jpeg`, `png`, `gif`.
    Image,
    /// A CUE sheet. Its own kind rather than a sidecar because playlists
    /// reference virtual tracks *through* it (`album.flac.cue/track0017`), so
    /// tasks 06 and 09 have to treat it as something that can be referenced.
    Cue,
    /// An `m3u`/`m3u8` **inside the music directory** — scene clutter that
    /// travels with its album, not one of MPD's own playlists. Those live in
    /// `playlist_directory` and are never scanned by this walk.
    Playlist,
    /// Release clutter: `nfo`, `sfv`, `txt`, `log`, `pdf`, `sfk`.
    Sidecar,
    /// Anything else. Scanned, counted and moved; never interpreted.
    Other,
}

impl Kind {
    /// Classify a path by its extension, case-insensitively.
    ///
    /// ```
    /// use mpdfm_core::library::{Format, Kind};
    /// use mpdfm_core::paths::RelPath;
    ///
    /// let track = RelPath::parse("jazz/Kind of Blue/01 So What.FLAC")?;
    /// assert_eq!(Kind::of(&track), Kind::Audio(Format::Flac));
    ///
    /// // A `.m3u` next to an album is clutter, not one of MPD's playlists.
    /// let stray = RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)/Mm..Food.m3u")?;
    /// assert_eq!(Kind::of(&stray), Kind::Playlist);
    ///
    /// // Unknown extensions are kept, so a move takes them along.
    /// let odd = RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)/Thumbs.db")?;
    /// assert_eq!(Kind::of(&odd), Kind::Other);
    /// # Ok::<(), mpdfm_core::paths::PathError>(())
    /// ```
    #[must_use]
    pub fn of(rel: &RelPath) -> Self {
        Self::from_extension(rel.extension())
    }

    /// [`Kind::of`] for an extension that has already been split off. `None` —
    /// a file with no extension, or a dotfile like `.nomedia` — is
    /// [`Kind::Other`].
    #[must_use]
    pub fn from_extension(ext: Option<&str>) -> Self {
        let Some(ext) = ext else {
            return Self::Other;
        };
        if let Some(format) = Format::from_extension(ext) {
            return Self::Audio(format);
        }
        match ext.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "gif" => Self::Image,
            "cue" => Self::Cue,
            "m3u" | "m3u8" => Self::Playlist,
            "nfo" | "sfv" | "txt" | "log" | "pdf" | "sfk" => Self::Sidecar,
            _ => Self::Other,
        }
    }

    /// Whether this is a track.
    #[must_use]
    pub fn is_audio(self) -> bool {
        matches!(self, Self::Audio(_))
    }

    /// The audio format, for a track.
    #[must_use]
    pub fn format(self) -> Option<Format> {
        match self {
            Self::Audio(format) => Some(format),
            _ => None,
        }
    }

    /// A short label, for `mpdfm scan` and its `--json` form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio(format) => format.as_str(),
            Self::Image => "image",
            Self::Cue => "cue",
            Self::Playlist => "playlist",
            Self::Sidecar => "sidecar",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------

/// A directory inside the library: a [`RelPath`], or the library root itself.
///
/// The root needs a name and cannot have a [`RelPath`] one — a `RelPath` is
/// never empty — but it is where the browser starts and where a stray top-level
/// track lives, so the by-directory index is keyed by this instead. [`Ord`] puts
/// the root first and is otherwise `RelPath`'s byte order.
///
/// ```
/// use mpdfm_core::library::DirPath;
/// use mpdfm_core::paths::RelPath;
///
/// let track = RelPath::parse("pop/Mercury (2 CD)/CD 1/01 Wrecked.mp3")?;
/// let dir = DirPath::of(&track);
/// assert_eq!(dir.as_str(), "pop/Mercury (2 CD)/CD 1");
/// assert_eq!(dir.file_name(), Some("CD 1"));
///
/// let stray = RelPath::parse("single.mp3")?;
/// assert!(DirPath::of(&stray).is_root());
/// # Ok::<(), mpdfm_core::paths::PathError>(())
/// ```
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct DirPath(Option<RelPath>);

impl DirPath {
    /// The library root — `music_directory` itself.
    #[must_use]
    pub fn root() -> Self {
        Self(None)
    }

    /// The directory a file lives in.
    #[must_use]
    pub fn of(rel: &RelPath) -> Self {
        Self(rel.parent())
    }

    /// Parse a directory path, where the empty string is the root.
    ///
    /// Everything else goes through [`RelPath::parse`], so a `DirPath` is as
    /// canonical as a track identity is.
    pub fn parse(s: &str) -> Result<Self, PathError> {
        if s.is_empty() {
            return Ok(Self::root());
        }
        RelPath::parse(s).map(Self::from)
    }

    /// Whether this is the library root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_none()
    }

    /// The path as a [`RelPath`], or `None` for the root.
    #[must_use]
    pub fn as_rel(&self) -> Option<&RelPath> {
        self.0.as_ref()
    }

    /// The canonical form; the empty string for the root.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref().map_or("", RelPath::as_str)
    }

    /// The containing directory, or `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let rel = self.0.as_ref()?;
        Some(Self(rel.parent()))
    }

    /// The final component — what the browser shows in a list — or `None` for
    /// the root, which has no name of its own.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.0.as_ref().map(RelPath::file_name)
    }

    /// The path of a child of this directory.
    ///
    /// Goes through [`RelPath::parse`], so a name with a `/` or a `..` in it —
    /// the shapes task 27's templates can produce — is rejected here rather than
    /// escaping the directory.
    pub fn join(&self, name: &str) -> Result<RelPath, PathError> {
        match self.0.as_ref() {
            Some(dir) => RelPath::parse(&format!("{dir}/{name}")),
            None => RelPath::parse(name),
        }
    }

    /// The absolute path this directory refers to under `root`.
    #[must_use]
    pub fn to_abs(&self, root: &Utf8Path) -> Utf8PathBuf {
        match self.0.as_ref() {
            Some(rel) => rel.to_abs(root),
            None => root.to_path_buf(),
        }
    }

    /// Whether this directory lies strictly inside `dir`.
    ///
    /// Component-wise, like [`RelPath::starts_with_dir`]: the root contains
    /// every other directory, and nothing lies inside itself.
    #[must_use]
    pub fn starts_with_dir(&self, dir: &DirPath) -> bool {
        match (self.0.as_ref(), dir.0.as_ref()) {
            (Some(_), None) => true,
            (Some(mine), Some(theirs)) => mine.starts_with_dir(theirs),
            (None, _) => false,
        }
    }
}

impl From<RelPath> for DirPath {
    fn from(rel: RelPath) -> Self {
        Self(Some(rel))
    }
}

impl std::fmt::Display for DirPath {
    /// The canonical form, which for the root is the empty string. Callers
    /// printing a list of directories are expected to label the root themselves.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------

/// One file found by the scan.
///
/// `size` and `mtime` are recorded now, cheaply, from the `stat` the walk had to
/// do anyway: task 12's undo verifies a file has not changed before reversing an
/// operation, and it cannot go back in time to find out what it used to look
/// like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Identity: the path relative to `music_directory`.
    pub rel: RelPath,
    /// What the extension says it is.
    pub kind: Kind,
    /// Size in bytes, from `stat`.
    pub size: u64,
    /// Last modification time, from `stat`.
    pub mtime: SystemTime,
}

impl Entry {
    /// The directory this file lives in — its key in the by-directory index.
    #[must_use]
    pub fn dir(&self) -> DirPath {
        DirPath::of(&self.rel)
    }

    /// The file name, without its directory.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.rel.file_name()
    }

    /// Whether this is a track.
    #[must_use]
    pub fn is_audio(&self) -> bool {
        self.kind.is_audio()
    }

    /// The audio format, for a track.
    #[must_use]
    pub fn format(&self) -> Option<Format> {
        self.kind.format()
    }
}

/// One directory's contents, as a browser needs them: the files directly inside
/// it, and the directories directly inside it.
///
/// Both are recorded even when empty, so navigating the tree never falls back to
/// the disk. A directory that holds nothing but other directories — a genre, or
/// a multi-disc set's root — is exactly the case a files-only index would lose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dir {
    files: Vec<usize>,
    subdirs: Vec<DirPath>,
}

impl Dir {
    /// Indices into [`Library::entries`] of the files directly inside, in entry
    /// order.
    ///
    /// Indices rather than references so the caller can hand a window of them to
    /// the lazy tag reader (task 16) without borrowing the whole library.
    #[must_use]
    pub fn files(&self) -> &[usize] {
        &self.files
    }

    /// The directories directly inside, in [`DirPath`] order.
    #[must_use]
    pub fn subdirs(&self) -> &[DirPath] {
        &self.subdirs
    }

    /// Whether the directory holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.subdirs.is_empty()
    }
}

/// A directory that directly contains audio files — the unit a move, a reorg and
/// a cover-art lookup all operate on.
///
/// Derived, not found: the scan records files and directories, and this view is
/// computed from them. A multi-disc set's root is therefore *not* an album
/// directory (it holds no audio itself); its discs are, each pointing back at it
/// through [`AlbumDir::set_root`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumDir {
    /// The directory itself.
    pub dir: DirPath,
    /// The set root, when this is one disc of a multi-disc release: the parent
    /// directory of a directory named like a disc (`CD 1 - …`, `Disc 2`). `None`
    /// for an ordinary album, whose parent is a genre directory and not part of
    /// the release.
    pub set_root: Option<DirPath>,
    /// How many audio files are directly inside.
    pub audio: usize,
}

impl AlbumDir {
    /// Whether this album directory is one disc of a multi-disc set.
    #[must_use]
    pub fn is_disc(&self) -> bool {
        self.set_root.is_some()
    }
}

/// The disc number a directory name announces, or `None` if it announces none.
///
/// This is the whole of MPDFM's multi-disc detection, and it is deliberately
/// narrow: a disc word (`cd`, `disc`, `disk`, case-insensitively) at the start of
/// the name, then separators, then digits. `CD 1 - Mercury - Acts 1` is a disc;
/// `Disc Jockey Mixes` and `cdrom rips` are not, because no number follows the
/// word.
///
/// ```
/// use mpdfm_core::library::disc_number;
///
/// assert_eq!(disc_number("CD 1 - Mercury - Acts 1"), Some(1));
/// assert_eq!(disc_number("disk_02"), Some(2));
/// assert_eq!(disc_number("Disc Jockey Mixes"), None);
/// assert_eq!(disc_number("Miles Davis - Kind of Blue (1959) [FLAC]"), None);
/// ```
#[must_use]
pub fn disc_number(dir_name: &str) -> Option<u32> {
    const WORDS: &[&str] = &["cd", "disc", "disk"];
    const SEPARATORS: &[char] = &[' ', '-', '_', '.', '#', '\t'];

    let lower = dir_name.to_ascii_lowercase();
    let rest = WORDS
        .iter()
        .find_map(|word| lower.strip_prefix(word))?
        .trim_start_matches(SEPARATORS);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

// ---------------------------------------------------------------------------

/// How many entries of each kind a library holds — the body of `mpdfm scan`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// `mp3` files.
    pub mp3: usize,
    /// `flac` files.
    pub flac: usize,
    /// `m4a` files.
    pub m4a: usize,
    /// Cover art and other images.
    pub images: usize,
    /// CUE sheets.
    pub cues: usize,
    /// `m3u`/`m3u8` files inside the music directory.
    pub playlists: usize,
    /// `nfo`, `sfv`, `txt`, `log`, `pdf`, `sfk`.
    pub sidecars: usize,
    /// Everything else.
    pub other: usize,
}

impl Counts {
    /// Tracks, of any format.
    #[must_use]
    pub fn audio(&self) -> usize {
        self.mp3 + self.flac + self.m4a
    }

    /// Every file counted.
    #[must_use]
    pub fn total(&self) -> usize {
        self.audio() + self.images + self.cues + self.playlists + self.sidecars + self.other
    }

    /// Count one more file of this kind.
    fn add(&mut self, kind: Kind) {
        let slot = match kind {
            Kind::Audio(Format::Mp3) => &mut self.mp3,
            Kind::Audio(Format::Flac) => &mut self.flac,
            Kind::Audio(Format::M4a) => &mut self.m4a,
            Kind::Image => &mut self.images,
            Kind::Cue => &mut self.cues,
            Kind::Playlist => &mut self.playlists,
            Kind::Sidecar => &mut self.sidecars,
            Kind::Other => &mut self.other,
        };
        *slot += 1;
    }
}

/// Something the scan could not model, reported rather than raised.
///
/// A library with one unreadable directory in it is still a library, and
/// refusing to show the other 3 000 files would be the wrong trade. Every
/// variant names the path it is about, and every one of them means "this is not
/// in the model" — so a caller that wants to be sure a move is complete checks
/// the warnings first (task 10's preview does).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanWarning {
    /// A name that is not valid UTF-8. Reported and skipped, never guessed at
    /// (`docs/PLAN.md` safety invariant 8). `lossy` is for the message only.
    #[error("skipped a name that is not valid UTF-8: {lossy}")]
    NotUtf8 {
        /// The path with invalid sequences replaced. Never write it to disk.
        lossy: String,
    },

    /// A name that is valid UTF-8 but cannot be a [`RelPath`] — a backslash, for
    /// instance, which is a legal character in an ext4 filename and an illegal
    /// one in a path MPDFM will hand to a playlist.
    #[error("skipped {path}: {reason}")]
    Unnamable {
        /// The path, as the filesystem spells it.
        path: String,
        /// Why it was rejected.
        reason: PathError,
    },

    /// A symlink, which the scan reports and does not follow — a link into a
    /// parent directory would otherwise walk forever, and a link out of the
    /// library would put paths in the model that safety invariant 5 forbids
    /// touching.
    #[error("not followed: {path} is a symlink to {}", target.as_deref().unwrap_or("?"))]
    Symlink {
        /// The link itself, relative to the music directory.
        path: RelPath,
        /// What it points at, if that could be read.
        target: Option<String>,
    },

    /// A directory that could not be listed, or a file that could not be
    /// `stat`ed. The scan continues with the rest of the tree.
    #[error("cannot read {path}: {message}")]
    Unreadable {
        /// The path that could not be read.
        path: String,
        /// The operating system's complaint.
        message: String,
    },
}

impl ScanWarning {
    /// The path the warning is about, which is what it is sorted and looked up
    /// by.
    ///
    /// For [`ScanWarning::NotUtf8`] this is the lossy rendering, the only
    /// spelling there is. It is a label, never an identity: it does not round-trip
    /// to the bytes on disk.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::NotUtf8 { lossy } => lossy,
            Self::Unnamable { path, .. } | Self::Unreadable { path, .. } => path,
            Self::Symlink { path, .. } => path.as_str(),
        }
    }
}

// ---------------------------------------------------------------------------

/// An in-memory model of the music library: every file, indexed by directory,
/// with the album directories derived and everything the walk could not model
/// reported.
///
/// Built by [`Library::scan`] and then read-only. The browser, the move planner
/// and `doctor` all work from one of these rather than from the disk, so a single
/// walk answers every question until the user asks for a rescan.
#[derive(Debug, Clone)]
pub struct Library {
    root: Utf8PathBuf,
    entries: Vec<Entry>,
    by_dir: BTreeMap<DirPath, Dir>,
    album_dirs: Vec<AlbumDir>,
    warnings: Vec<ScanWarning>,
}

impl Library {
    /// Walk `root` and build the model. See `scan.rs` for what the walk does and
    /// does not do.
    ///
    /// # Errors
    ///
    /// Only if `root` itself is missing or is not a directory. Every other
    /// problem is a [`ScanWarning`].
    pub fn scan(root: &Utf8Path) -> crate::Result<Self> {
        super::scan::scan(root)
    }

    /// Assemble a library from what the walk collected: sort the entries into a
    /// deterministic order, index them by directory, and derive the album
    /// directories.
    ///
    /// `dirs` is every directory the walk saw, including [`DirPath::root`] — the
    /// ones that hold no files are what makes the index navigable.
    pub(super) fn assemble(
        root: &Utf8Path,
        mut entries: Vec<Entry>,
        dirs: Vec<DirPath>,
        mut warnings: Vec<ScanWarning>,
    ) -> Self {
        // Warnings are output a user reads and a test asserts on, so they are
        // ordered by the path they name rather than by the order the walk
        // happened to meet them. A stable sort, so two warnings about one path
        // stay in the order they were raised.
        warnings.sort_by(|a, b| a.path().cmp(b.path()));

        // An explicit comparator, not readdir order, which is arbitrary on ext4
        // and would make two scans of an unchanged library disagree. Byte order
        // over the canonical path: case-sensitive, because the filesystem is
        // (`paths`), and total, because two entries never share a path.
        entries.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));

        let mut by_dir: BTreeMap<DirPath, Dir> = BTreeMap::new();
        for dir in dirs {
            by_dir.entry(dir).or_default();
        }
        for (index, entry) in entries.iter().enumerate() {
            by_dir.entry(entry.dir()).or_default().files.push(index);
        }

        // Keys come out of a `BTreeMap` in order, so each parent's `subdirs`
        // ends up sorted without a second sort.
        let children: Vec<(DirPath, DirPath)> = by_dir
            .keys()
            .filter_map(|dir| dir.parent().map(|parent| (parent, dir.clone())))
            .collect();
        for (parent, child) in children {
            by_dir.entry(parent).or_default().subdirs.push(child);
        }

        let album_dirs = derive_album_dirs(&entries, &by_dir);

        Self {
            root: root.to_path_buf(),
            entries,
            by_dir,
            album_dirs,
            warnings,
        }
    }

    /// The root this library was scanned from — `music_directory`.
    #[must_use]
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Every file, in [`RelPath`] byte order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// One entry by index.
    #[must_use]
    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// The index of a path, for a caller that has an identity and wants a row.
    ///
    /// A binary search over the sorted entries, so it costs no extra index.
    #[must_use]
    pub fn index_of(&self, rel: &RelPath) -> Option<usize> {
        self.entries
            .binary_search_by(|entry| entry.rel.cmp(rel))
            .ok()
    }

    /// One entry by path.
    #[must_use]
    pub fn get(&self, rel: &RelPath) -> Option<&Entry> {
        self.index_of(rel).map(|index| &self.entries[index])
    }

    /// How many files the library holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the library holds no files at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One directory's contents, or `None` if the scan saw no such directory.
    #[must_use]
    pub fn dir(&self, dir: &DirPath) -> Option<&Dir> {
        self.by_dir.get(dir)
    }

    /// Every directory the scan saw, in [`DirPath`] order, root first.
    pub fn dirs(&self) -> impl Iterator<Item = (&DirPath, &Dir)> {
        self.by_dir.iter()
    }

    /// How many directories the scan saw, the root included.
    #[must_use]
    pub fn dir_count(&self) -> usize {
        self.by_dir.len()
    }

    /// Indices of the files directly inside a directory, in entry order; empty
    /// for a directory the scan never saw.
    ///
    /// This is the browser's lookup, and the reason the index exists: drawing a
    /// directory, or paging through it, never touches the disk again. Indices
    /// rather than entries so a visible window of them can be handed to the lazy
    /// tag reader (task 16).
    #[must_use]
    pub fn indices_in(&self, dir: &DirPath) -> &[usize] {
        self.by_dir.get(dir).map_or(&[], Dir::files)
    }

    /// The files directly inside a directory, in entry order.
    pub fn files_in(&self, dir: &DirPath) -> impl Iterator<Item = &Entry> {
        self.indices_in(dir)
            .iter()
            .map(|&index| &self.entries[index])
    }

    /// The directories directly inside a directory, in [`DirPath`] order.
    #[must_use]
    pub fn subdirs_in(&self, dir: &DirPath) -> &[DirPath] {
        self.by_dir.get(dir).map_or(&[], Dir::subdirs)
    }

    /// Every album directory — every directory that directly contains audio —
    /// in [`DirPath`] order.
    #[must_use]
    pub fn album_dirs(&self) -> &[AlbumDir] {
        &self.album_dirs
    }

    /// The album directory at this path, if it is one.
    #[must_use]
    pub fn album_dir(&self, dir: &DirPath) -> Option<&AlbumDir> {
        self.album_dirs
            .binary_search_by(|album| album.dir.cmp(dir))
            .ok()
            .map(|index| &self.album_dirs[index])
    }

    /// The disc directories of a multi-disc set, in order.
    ///
    /// Empty for anything that is not a set root — which includes an ordinary
    /// album directory, whose own path is not any disc's [`AlbumDir::set_root`].
    /// Task 27 uses this to keep a set together when it re-files it.
    pub fn discs_of(&self, set_root: &DirPath) -> impl Iterator<Item = &AlbumDir> {
        self.album_dirs
            .iter()
            .filter(move |album| album.set_root.as_ref() == Some(set_root))
    }

    /// Everything the walk could not model.
    #[must_use]
    pub fn warnings(&self) -> &[ScanWarning] {
        &self.warnings
    }

    /// How many files of each kind the library holds.
    #[must_use]
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for entry in &self.entries {
            counts.add(entry.kind);
        }
        counts
    }
}

/// Every directory that directly contains audio, with a multi-disc set's discs
/// linked to the set root.
///
/// The set root is the disc directory's parent, and it has to be a directory the
/// scan actually saw and not the library root: a `CD 1` sitting at the top level
/// belongs to no release, and calling the whole library its set would mislead
/// task 27 into treating every top-level directory as one album.
fn derive_album_dirs(entries: &[Entry], by_dir: &BTreeMap<DirPath, Dir>) -> Vec<AlbumDir> {
    by_dir
        .iter()
        .filter_map(|(dir, contents)| {
            let audio = contents
                .files
                .iter()
                .filter(|&&index| entries[index].is_audio())
                .count();
            if audio == 0 {
                return None;
            }
            let set_root = dir
                .file_name()
                .and_then(disc_number)
                .and_then(|_| dir.parent())
                .filter(|parent| !parent.is_root() && by_dir.contains_key(parent));
            Some(AlbumDir {
                dir: dir.clone(),
                set_root,
                audio,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).expect("test path should be a valid RelPath")
    }

    fn entry(path: &str) -> Entry {
        Entry {
            kind: Kind::of(&rel(path)),
            rel: rel(path),
            size: 1,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    /// Assemble a library from paths alone, as if a walk had found exactly these
    /// files and the directories above them.
    fn library(paths: &[&str]) -> Library {
        let entries: Vec<Entry> = paths.iter().map(|path| entry(path)).collect();
        let mut dirs = vec![DirPath::root()];
        for entry in &entries {
            let mut dir = entry.dir();
            while !dir.is_root() {
                dirs.push(dir.clone());
                dir = dir.parent().expect("a non-root dir has a parent");
            }
        }
        Library::assemble(Utf8Path::new("/music"), entries, dirs, Vec::new())
    }

    #[test]
    fn classification_is_by_extension_and_case_insensitive() {
        let cases: &[(&str, Kind)] = &[
            ("a/01.mp3", Kind::Audio(Format::Mp3)),
            ("a/01.MP3", Kind::Audio(Format::Mp3)),
            ("a/01.flac", Kind::Audio(Format::Flac)),
            ("a/01.FLAC", Kind::Audio(Format::Flac)),
            ("a/01.m4a", Kind::Audio(Format::M4a)),
            ("a/folder.jpg", Kind::Image),
            ("a/cover.JPEG", Kind::Image),
            ("a/back.png", Kind::Image),
            ("a/anim.gif", Kind::Image),
            ("a/album.flac.cue", Kind::Cue),
            ("a/Mm..Food.m3u", Kind::Playlist),
            ("a/list.m3u8", Kind::Playlist),
            ("a/info.nfo", Kind::Sidecar),
            ("a/mm..food.sfv", Kind::Sidecar),
            ("a/notes.txt", Kind::Sidecar),
            ("a/eac.log", Kind::Sidecar),
            ("a/booklet.pdf", Kind::Sidecar),
            ("a/wave.sfk", Kind::Sidecar),
            // Nothing is silently dropped: an extension MPDFM has no opinion
            // about still produces an entry.
            ("a/Thumbs.db", Kind::Other),
            ("a/README", Kind::Other),
            ("a/.nomedia", Kind::Other),
            ("a/track.wav", Kind::Other),
        ];
        for (path, expected) in cases {
            assert_eq!(Kind::of(&rel(path)), *expected, "wrong kind for {path}");
        }
    }

    #[test]
    fn disc_numbers_need_a_number_after_the_word() {
        for (name, expected) in [
            ("CD 1 - Mercury - Acts 1", Some(1)),
            ("CD 2 - Mercury - Acts 2", Some(2)),
            ("CD1", Some(1)),
            ("cd-01", Some(1)),
            ("disk_02", Some(2)),
            ("Disc 3 of 3", Some(3)),
            ("DISC.10", Some(10)),
            ("Disc Jockey Mixes", None),
            ("cdrom rips", None),
            ("Miles Davis - Kind of Blue (1959) [FLAC]", None),
            ("", None),
        ] {
            assert_eq!(
                disc_number(name),
                expected,
                "wrong disc number for {name:?}"
            );
        }
    }

    #[test]
    fn dir_paths_name_the_root_as_well() {
        let root = DirPath::root();
        assert!(root.is_root());
        assert_eq!(root.as_str(), "");
        assert_eq!(root.file_name(), None);
        assert_eq!(root.parent(), None);
        assert_eq!(root.as_rel(), None);
        assert_eq!(root.to_abs(Utf8Path::new("/music")), "/music");
        assert_eq!(root.join("single.mp3").unwrap(), rel("single.mp3"));
        assert_eq!(DirPath::parse("").unwrap(), root);

        let dir = DirPath::parse("pop/Mercury (2 CD)/CD 1").unwrap();
        assert!(!dir.is_root());
        assert_eq!(dir.file_name(), Some("CD 1"));
        assert_eq!(dir.parent().unwrap().as_str(), "pop/Mercury (2 CD)");
        assert_eq!(
            dir.to_abs(Utf8Path::new("/music")),
            "/music/pop/Mercury (2 CD)/CD 1"
        );
        assert_eq!(
            dir.join("01.mp3").unwrap().as_str(),
            "pop/Mercury (2 CD)/CD 1/01.mp3"
        );
        // A child name that is a path, or escapes, is refused rather than joined.
        assert!(dir.join("../x.mp3").is_err());
        assert!(dir.join("a/b.mp3").is_ok());

        // The root sorts first, and contains everything.
        assert!(root < dir);
        assert!(dir.starts_with_dir(&root));
        assert!(!root.starts_with_dir(&dir));
        assert!(!dir.starts_with_dir(&dir));
    }

    #[test]
    fn entries_are_sorted_by_path_not_insertion_order() {
        let library = library(&["b/02.mp3", "a/02.mp3", "b/01.mp3", "a/01.mp3"]);
        let paths: Vec<&str> = library.entries().iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(paths, ["a/01.mp3", "a/02.mp3", "b/01.mp3", "b/02.mp3"]);

        // Sorted means searchable.
        assert_eq!(library.index_of(&rel("b/01.mp3")), Some(2));
        assert_eq!(library.index_of(&rel("nope.mp3")), None);
        assert_eq!(library.get(&rel("a/02.mp3")).unwrap().rel, rel("a/02.mp3"));
    }

    #[test]
    fn the_index_holds_files_and_subdirs_of_every_directory() {
        let library = library(&[
            "single.mp3",
            "pop/Mercury (2 CD)/CD 1/01.mp3",
            "pop/Mercury (2 CD)/CD 2/01.mp3",
            "jazz/Kind of Blue/01.flac",
            "jazz/Kind of Blue/folder.jpg",
        ]);

        let root = DirPath::root();
        // A stray top-level track lives in the root, which a `RelPath` key could
        // not have named.
        assert_eq!(
            library
                .files_in(&root)
                .map(Entry::file_name)
                .collect::<Vec<_>>(),
            ["single.mp3"]
        );
        assert_eq!(
            library
                .subdirs_in(&root)
                .iter()
                .map(DirPath::as_str)
                .collect::<Vec<_>>(),
            ["jazz", "pop"]
        );

        // A directory that holds only directories is still in the index.
        let set = DirPath::parse("pop/Mercury (2 CD)").unwrap();
        assert!(library.files_in(&set).next().is_none());
        assert_eq!(
            library
                .subdirs_in(&set)
                .iter()
                .map(|d| d.file_name().unwrap())
                .collect::<Vec<_>>(),
            ["CD 1", "CD 2"]
        );

        let album = DirPath::parse("jazz/Kind of Blue").unwrap();
        assert_eq!(
            library
                .files_in(&album)
                .map(Entry::file_name)
                .collect::<Vec<_>>(),
            ["01.flac", "folder.jpg"]
        );
        assert!(library.subdirs_in(&album).is_empty());

        // A directory nobody scanned answers empty rather than panicking.
        let missing = DirPath::parse("nope").unwrap();
        assert!(library.dir(&missing).is_none());
        assert!(library.indices_in(&missing).is_empty());
        assert!(library.subdirs_in(&missing).is_empty());
    }

    #[test]
    fn album_dirs_are_directories_with_audio_directly_inside() {
        let library = library(&[
            "jazz/Kind of Blue/01.flac",
            "jazz/Kind of Blue/folder.jpg",
            // Art alone does not make an album directory.
            "pop/Covers/folder.jpg",
        ]);
        let albums: Vec<&str> = library
            .album_dirs()
            .iter()
            .map(|album| album.dir.as_str())
            .collect();
        assert_eq!(albums, ["jazz/Kind of Blue"]);
        assert_eq!(
            library
                .album_dir(&DirPath::parse("jazz/Kind of Blue").unwrap())
                .unwrap()
                .audio,
            1
        );
        assert!(
            library
                .album_dir(&DirPath::parse("pop/Covers").unwrap())
                .is_none()
        );
        assert!(
            library
                .album_dir(&DirPath::parse("jazz").unwrap())
                .is_none()
        );
    }

    #[test]
    fn a_discs_set_root_is_its_parent_and_a_genre_is_not() {
        let library = library(&[
            "pop/Mercury (2 CD)/CD 1 - Acts 1/01.mp3",
            "pop/Mercury (2 CD)/CD 2 - Acts 2/01.mp3",
            "jazz/Kind of Blue/01.flac",
        ]);
        let set = DirPath::parse("pop/Mercury (2 CD)").unwrap();

        let cd1 = library
            .album_dir(&DirPath::parse("pop/Mercury (2 CD)/CD 1 - Acts 1").unwrap())
            .unwrap();
        assert!(cd1.is_disc());
        assert_eq!(cd1.set_root.as_ref(), Some(&set));

        // The set root holds no audio, so it is not an album directory itself —
        // it is found through its discs.
        assert!(library.album_dir(&set).is_none());
        assert_eq!(
            library
                .discs_of(&set)
                .map(|a| a.dir.file_name().unwrap())
                .collect::<Vec<_>>(),
            ["CD 1 - Acts 1", "CD 2 - Acts 2"]
        );

        // An ordinary album's parent is a genre directory, not a set.
        let album = library
            .album_dir(&DirPath::parse("jazz/Kind of Blue").unwrap())
            .unwrap();
        assert!(!album.is_disc());
        assert!(
            library
                .discs_of(&DirPath::parse("jazz").unwrap())
                .next()
                .is_none()
        );
    }

    #[test]
    fn a_disc_directory_at_the_top_level_belongs_to_no_set() {
        // The library root is never a set root: that would make task 27 treat
        // the whole library as one release.
        let library = library(&["CD 1/01.mp3"]);
        let cd1 = library.album_dir(&DirPath::parse("CD 1").unwrap()).unwrap();
        assert!(!cd1.is_disc());
    }

    #[test]
    fn counts_add_up_to_every_entry() {
        let library = library(&[
            "a/01.mp3",
            "a/02.mp3",
            "a/03.flac",
            "a/04.m4a",
            "a/folder.jpg",
            "a/album.flac.cue",
            "a/stray.m3u",
            "a/info.nfo",
            "a/Thumbs.db",
        ]);
        let counts = library.counts();
        assert_eq!(counts.mp3, 2);
        assert_eq!(counts.flac, 1);
        assert_eq!(counts.m4a, 1);
        assert_eq!(counts.audio(), 4);
        assert_eq!(counts.images, 1);
        assert_eq!(counts.cues, 1);
        assert_eq!(counts.playlists, 1);
        assert_eq!(counts.sidecars, 1);
        assert_eq!(counts.other, 1);
        assert_eq!(counts.total(), library.len());
    }
}
