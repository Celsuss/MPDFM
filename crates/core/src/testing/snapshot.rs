//! A recursive digest of a directory tree, and the comparison that makes undo
//! testable.
//!
//! The undo invariant (`docs/PLAN.md` §8) is "commit → undo ⇒ the filesystem and
//! every playlist are byte-identical to the starting state". [`Snapshot`] is how
//! a test says that in one line: capture before, capture after, compare.
//!
//! What is recorded, and why each thing is:
//!
//! - **every path** in the tree, sorted — a move that leaves an empty directory
//!   behind, or forgets to create one, has changed the tree
//! - **file contents** — verbatim for text (playlists, the state file, `.nfo`,
//!   `.cue`), as size plus a digest for binaries (the audio files). Text is kept
//!   whole so that a failure prints the offending playlist line rather than a
//!   pair of hashes
//! - **symlinks, as symlinks**, with their target — rewriting `Radios.m3u` must
//!   edit the link's target and leave the link in place, and a snapshot that
//!   followed links could not tell the difference
//! - **the unix mode** — task 06 must preserve a playlist's permissions
//!
//! What is deliberately *not* recorded: mtimes and inode numbers. Restoring a
//! file byte-for-byte legitimately gives it a new mtime, so including either
//! would make every undo test fail for the wrong reason. Tests that care about
//! inode identity (a move must not silently copy) assert on that directly.

use std::collections::BTreeMap;

use camino::Utf8Path;

/// Text files up to this size are kept verbatim. Above it — no such file exists
/// in a fixture today — the digest is used instead, so one enormous file cannot
/// make a failure message unreadable.
const MAX_VERBATIM: u64 = 64 * 1024;

/// A recursive digest of one directory tree.
///
/// Two snapshots are equal when the trees are indistinguishable in everything
/// listed in the [module docs][self]. Compare with [`Snapshot::assert_same`]
/// rather than `assert_eq!`, which would print both whole trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Keyed and therefore ordered by path, so two captures of the same tree
    /// compare regardless of the order `readdir` happened to return.
    entries: BTreeMap<String, Entry>,
}

/// One filesystem entry, as a snapshot sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What it is, and its contents.
    pub kind: EntryKind,
    /// Permission bits, on unix. `None` elsewhere.
    pub mode: Option<u32>,
}

/// The contents of a snapshotted entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A directory. Its children appear as their own entries.
    Dir,
    /// A symlink, recorded without following it. `target` is rendered with the
    /// snapshot root replaced by `<root>` when it points inside the tree, so the
    /// rendering does not change with the temp directory's name.
    Symlink {
        /// Where the link points, as stored in the link itself.
        target: String,
    },
    /// A file whose bytes are valid UTF-8 and small enough to keep whole.
    Text {
        /// The exact contents.
        contents: String,
    },
    /// Any other file: size plus a digest of the contents.
    Binary {
        /// Length in bytes.
        size: u64,
        /// [`digest`] of the contents.
        digest: u64,
    },
    /// The entry exists but could not be read — an unreadable directory, say,
    /// which task 05 creates on purpose. Only the error kind is kept; the
    /// message text varies between platforms.
    Unreadable {
        /// `std::io::ErrorKind`, debug-formatted.
        error: String,
    },
}

impl Snapshot {
    /// Walk `root` and record everything under it.
    ///
    /// Symlinks are never followed, so a link into a directory that is itself
    /// inside `root` records the link once and the target's contents once —
    /// not twice.
    ///
    /// # Panics
    ///
    /// If `root` itself cannot be walked. A test whose fixture has vanished
    /// should fail loudly rather than compare two empty snapshots.
    #[must_use]
    pub fn capture(root: &Utf8Path) -> Self {
        let mut entries = BTreeMap::new();
        for result in walkdir::WalkDir::new(root).min_depth(1).follow_links(false) {
            let (path, entry) = match result {
                Ok(dir_entry) => {
                    let path = dir_entry.path().to_path_buf();
                    let entry = Entry::read(&path, root);
                    (path, entry)
                }
                Err(err) => {
                    // A path is attached to every walkdir error raised below the
                    // root; without one there is nothing to record it against.
                    let Some(path) = err.path().map(std::path::Path::to_path_buf) else {
                        panic!("cannot walk snapshot root {root}: {err}");
                    };
                    let entry = Entry {
                        kind: EntryKind::Unreadable {
                            error: format!("{:?}", err.io_error().map(std::io::Error::kind)),
                        },
                        mode: mode_of(&path),
                    };
                    (path, entry)
                }
            };
            entries.insert(relative_to(&path, root), entry);
        }
        Self { entries }
    }

    /// The entries, by path relative to the captured root, in path order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &Entry)> {
        self.entries
            .iter()
            .map(|(path, entry)| (path.as_str(), entry))
    }

    /// One entry by its path relative to the captured root.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.get(path)
    }

    /// How many entries were recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tree was empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One line per difference, empty when the two snapshots are equal.
    ///
    /// Only the paths that differ appear, which is the whole point: an undo bug
    /// shows up as two or three lines instead of two 40-entry trees.
    #[must_use]
    pub fn diff(&self, other: &Snapshot) -> Vec<String> {
        let mut lines = Vec::new();
        for (path, entry) in &self.entries {
            match other.entries.get(path) {
                None => lines.push(format!("- {path} ({})", entry.summary())),
                Some(theirs) if theirs != entry => lines.push(format!(
                    "~ {path}\n    before: {}\n    after:  {}",
                    entry.summary(),
                    theirs.summary()
                )),
                Some(_) => {}
            }
        }
        for (path, entry) in &other.entries {
            if !self.entries.contains_key(path) {
                lines.push(format!("+ {path} ({})", entry.summary()));
            }
        }
        lines.sort();
        lines
    }

    /// Panic unless the two snapshots are identical, listing what changed.
    ///
    /// # Panics
    ///
    /// When any entry differs, is missing, or is new.
    pub fn assert_same(&self, other: &Snapshot) {
        let diff = self.diff(other);
        assert!(
            diff.is_empty(),
            "the tree changed ({} difference(s)); `-` was there before, `+` is there now:\n{}",
            diff.len(),
            diff.join("\n")
        );
    }
}

impl std::fmt::Display for Snapshot {
    /// The whole tree as text, one entry per line, text files indented under
    /// theirs. Stable across runs — the temp directory's name never appears —
    /// so it is safe to hand to `insta`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (path, entry) in &self.entries {
            writeln!(f, "{} {path} ({})", entry.mode_string(), entry.summary())?;
            if let EntryKind::Text { contents } = &entry.kind {
                for line in contents.lines() {
                    writeln!(f, "    | {line}")?;
                }
            }
        }
        Ok(())
    }
}

impl Entry {
    /// Classify and read one path, without following it if it is a link.
    fn read(path: &std::path::Path, root: &Utf8Path) -> Self {
        let mode = mode_of(path);
        let kind = match std::fs::symlink_metadata(path) {
            Err(err) => EntryKind::Unreadable {
                error: format!("{:?}", Some(err.kind())),
            },
            Ok(meta) if meta.is_dir() => EntryKind::Dir,
            Ok(meta) if meta.is_symlink() => match std::fs::read_link(path) {
                Ok(target) => EntryKind::Symlink {
                    target: relative_to_root_marker(&target, root),
                },
                Err(err) => EntryKind::Unreadable {
                    error: format!("{:?}", Some(err.kind())),
                },
            },
            Ok(_) => match std::fs::read(path) {
                Err(err) => EntryKind::Unreadable {
                    error: format!("{:?}", Some(err.kind())),
                },
                Ok(bytes) => Self::classify_contents(bytes),
            },
        };
        Self { kind, mode }
    }

    /// Text short enough to print is kept whole; everything else is digested.
    ///
    /// The length comes from the bytes actually read rather than from the
    /// directory entry's metadata, so the two can never disagree.
    fn classify_contents(bytes: Vec<u8>) -> EntryKind {
        let size = bytes.len() as u64;
        match String::from_utf8(bytes) {
            Ok(contents) if size <= MAX_VERBATIM => EntryKind::Text { contents },
            Ok(contents) => EntryKind::Binary {
                size,
                digest: digest(contents.as_bytes()),
            },
            Err(err) => EntryKind::Binary {
                size,
                digest: digest(err.as_bytes()),
            },
        }
    }

    /// A one-line rendering, used by both [`Snapshot::diff`] and [`Display`].
    fn summary(&self) -> String {
        match &self.kind {
            EntryKind::Dir => "dir".to_owned(),
            EntryKind::Symlink { target } => format!("link -> {target}"),
            EntryKind::Text { contents } => {
                format!("text, {} bytes", contents.len())
            }
            EntryKind::Binary { size, digest } => format!("file, {size} bytes, {digest:#018x}"),
            EntryKind::Unreadable { error } => format!("unreadable, {error}"),
        }
    }

    fn mode_string(&self) -> String {
        match self.mode {
            Some(mode) => format!("{mode:04o}"),
            None => "----".to_owned(),
        }
    }
}

/// FNV-1a, 64-bit. Not cryptographic and not meant to be: it exists so a test
/// can say "these bytes did not change" without keeping a megabyte of audio in
/// memory, and it is specified precisely enough that the same bytes always give
/// the same number, on any machine and any compiler version — which `DefaultHasher`
/// does not promise.
#[must_use]
pub fn digest(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Permission bits, on platforms that have them.
fn mode_of(path: &std::path::Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // `symlink_metadata`: the mode of the link, not of what it points at.
        std::fs::symlink_metadata(path)
            .ok()
            .map(|meta| meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// `path` relative to `root`, as a `/`-separated string. Non-UTF-8 components
/// are rendered lossily: a fixture can contain a deliberately non-UTF-8 name
/// (task 05 needs one) and the snapshot must still record that it is there.
fn relative_to(path: &std::path::Path, root: &Utf8Path) -> String {
    path.strip_prefix(root.as_std_path())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Render a symlink target with the snapshot root replaced by `<root>`, so the
/// rendering does not carry the temp directory's random name.
fn relative_to_root_marker(target: &std::path::Path, root: &Utf8Path) -> String {
    match target.strip_prefix(root.as_std_path()) {
        Ok(rest) => format!("<root>/{}", rest.to_string_lossy()),
        Err(_) => target.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_fnv1a_and_changes_with_one_byte() {
        // The published FNV-1a 64 test vector, so a "tidy-up" of the loop cannot
        // silently change what we are computing.
        assert_eq!(digest(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(digest(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(digest(b"hello world"), digest(b"hello worle"));
    }

    #[test]
    fn diff_names_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(root.join("a.m3u"), "pop/a.mp3\n").unwrap();
        std::fs::write(root.join("b.m3u"), "pop/b.mp3\n").unwrap();

        let before = Snapshot::capture(root);
        before.assert_same(&Snapshot::capture(root));

        std::fs::write(root.join("b.m3u"), "pop/B.mp3\n").unwrap();
        std::fs::write(root.join("c.m3u"), "").unwrap();
        std::fs::remove_file(root.join("a.m3u")).unwrap();

        let diff = before.diff(&Snapshot::capture(root));
        assert_eq!(diff.len(), 3, "{diff:#?}");
        assert!(
            diff.iter().any(|line| line.starts_with("- a.m3u")),
            "{diff:#?}"
        );
        assert!(
            diff.iter().any(|line| line.starts_with("+ c.m3u")),
            "{diff:#?}"
        );
        assert!(
            diff.iter().any(|line| line.starts_with("~ b.m3u")),
            "{diff:#?}"
        );
    }
}
