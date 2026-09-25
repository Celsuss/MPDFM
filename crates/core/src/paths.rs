//! [`RelPath`] — the canonical identity of a track — and [`contains`], the guard
//! that keeps every operation inside the configured roots.
//!
//! A track is identified by its path relative to `music_directory`: UTF-8,
//! `/`-separated, no `.` or `..`, never absolute. That is exactly what
//! playlists, MPD's state file and MPD's database all store, so it is the only
//! identity that survives a round trip through them (`docs/PLAN.md` §5). The
//! newtype exists so a raw `PathBuf` can never reach a playlist line: if you
//! hold a `RelPath`, it is already in the form an m3u wants, and [`Display`][std::fmt::Display]
//! writes exactly those bytes.
//!
//! # Two things this module deliberately does not do
//!
//! **No case folding.** ext4 is case-sensitive, so `Artist/` and `artist/` are
//! two different directories and comparing them folded would make MPDFM rewrite
//! the wrong playlist line. Case folding belongs only in the collision
//! *warnings* of tasks 08 and 27, never in identity.
//!
//! **No Unicode normalization.** NFC and NFD spellings of a visually identical
//! name are different byte strings, and the filesystem treats them as different
//! files. Comparison is therefore byte-wise; reporting suspected NFC/NFD
//! near-duplicates is `doctor`'s job (task 29).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};

/// Why a string or path could not become a [`RelPath`].
///
/// Callers match on the variant — the scanner turns [`PathError::NotUtf8`] into
/// a warning and skips the file, while the playlist parser treats any rejection
/// as "this line is not a track" and preserves it byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The empty string, which names nothing.
    #[error("empty path")]
    Empty,

    /// Absolute, e.g. `/home/me/Music/a.mp3`. A track identity is relative to
    /// the music directory.
    #[error("path is absolute: {path}")]
    Absolute {
        /// The offending input.
        path: String,
    },

    /// Contains a backslash. MPDFM is `/`-separated everywhere; a backslash is
    /// a literal character in a Unix filename and almost always a Windows path
    /// that wandered in.
    #[error("path contains a backslash: {path}")]
    Backslash {
        /// The offending input.
        path: String,
    },

    /// Ends with `/`, e.g. `hiphop/`. A track is a file, not a directory.
    #[error("path has a trailing separator: {path}")]
    TrailingSlash {
        /// The offending input.
        path: String,
    },

    /// Has a repeated separator, e.g. `a//b`.
    #[error("path has an empty component: {path}")]
    EmptyComponent {
        /// The offending input.
        path: String,
    },

    /// Has a `.` component, e.g. `./a.mp3` or `a/./b.mp3`. MPDFM rejects these
    /// rather than normalizing them, so that every accepted string renders back
    /// byte-identically — see [`RelPath::parse`].
    #[error("path contains a `.` component: {path}")]
    CurDir {
        /// The offending input.
        path: String,
    },

    /// Has a `..` component, e.g. `../a.mp3`. Note this is component-wise: the
    /// real album name `MF DOOM - Mm..Food (2004)` is fine.
    #[error("path contains a `..` component: {path}")]
    ParentDir {
        /// The offending input.
        path: String,
    },

    /// Contains a NUL byte, which no filesystem accepts.
    #[error("path contains a NUL byte: {path:?}")]
    InteriorNul {
        /// The offending input.
        path: String,
    },

    /// Not valid UTF-8. MPDFM reports and skips these; it never guesses an
    /// encoding (`docs/PLAN.md` safety invariant 8).
    #[error("path is not valid UTF-8: {lossy}")]
    NotUtf8 {
        /// The path with invalid sequences replaced, for the warning message
        /// only. Never write this back to disk.
        lossy: String,
    },

    /// `path` does not live under `prefix`, so the requested relative path or
    /// reparenting is not defined.
    #[error("{path} is not under {prefix}")]
    NotUnder {
        /// The path that was expected to be inside `prefix`.
        path: String,
        /// The root or moved directory it was measured against.
        prefix: String,
    },
}

/// A track's identity: a path relative to `music_directory`, guaranteed
/// relative, `/`-separated, free of `.` and `..` components, and free of empty,
/// repeated or trailing separators.
///
/// Ordering and hashing are byte-wise over that canonical form, which is what
/// makes it usable as the key of the playlist index (task 07).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelPath(Utf8PathBuf);

impl RelPath {
    /// Parse a playlist line (or any string) into a `RelPath`.
    ///
    /// Validation runs over the string itself rather than over
    /// [`Utf8Path::components`], because `components` silently swallows `.` and
    /// repeated separators — exactly the malformations that must be rejected.
    ///
    /// A leading `./` is **rejected**, not normalized. That keeps the round-trip
    /// property exact: for every `s` this accepts, `parse(s).to_string() == s`,
    /// with no exception. A playlist line spelled `./pop/a.mp3` is therefore not
    /// a track line as far as the parser is concerned, and survives byte-for-byte
    /// instead of being respelled behind the user's back.
    ///
    /// ```
    /// use mpdfm_core::paths::{PathError, RelPath};
    ///
    /// let rel = RelPath::parse("electronic/kream/KREAM - So Hï.mp3")?;
    /// assert_eq!(rel.to_string(), "electronic/kream/KREAM - So Hï.mp3");
    ///
    /// // `..` is rejected component-wise, so this real album name is fine:
    /// assert!(RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)/01.mp3").is_ok());
    /// assert!(matches!(
    ///     RelPath::parse("./a.mp3"),
    ///     Err(PathError::CurDir { .. })
    /// ));
    /// # Ok::<(), PathError>(())
    /// ```
    pub fn parse(s: &str) -> Result<Self, PathError> {
        if s.is_empty() {
            return Err(PathError::Empty);
        }
        if s.contains('\\') {
            return Err(PathError::Backslash { path: s.to_owned() });
        }
        if s.contains('\0') {
            return Err(PathError::InteriorNul { path: s.to_owned() });
        }
        // Checked before the component loop: for `/abs` the first component is
        // empty, and `Absolute` is the more useful diagnostic.
        if s.starts_with('/') {
            return Err(PathError::Absolute { path: s.to_owned() });
        }
        if s.ends_with('/') {
            return Err(PathError::TrailingSlash { path: s.to_owned() });
        }
        for component in s.split('/') {
            match component {
                "" => return Err(PathError::EmptyComponent { path: s.to_owned() }),
                "." => return Err(PathError::CurDir { path: s.to_owned() }),
                ".." => return Err(PathError::ParentDir { path: s.to_owned() }),
                _ => {}
            }
        }
        Ok(Self(Utf8PathBuf::from(s)))
    }

    /// Measure an absolute path against `root`.
    ///
    /// This is purely lexical — it resolves no symlinks and touches no disk, so
    /// it is safe to call on paths that do not exist yet (a move destination).
    /// Callers that need the safety guarantee of invariant 5 must also call
    /// [`contains`]; the scanner (task 05) relies on the fact that it walks
    /// downwards from `root` itself.
    ///
    /// `abs == root` yields [`PathError::Empty`]: the root is not a track.
    pub fn from_abs(abs: &Utf8Path, root: &Utf8Path) -> Result<Self, PathError> {
        let rel = abs.strip_prefix(root).map_err(|_| PathError::NotUnder {
            path: abs.to_string(),
            prefix: root.to_string(),
        })?;
        Self::parse(rel.as_str())
    }

    /// [`RelPath::from_abs`] for a path straight from the operating system, such
    /// as a `walkdir` entry.
    ///
    /// This is where non-UTF-8 names are caught: they return
    /// [`PathError::NotUtf8`] carrying a lossy rendering for the warning
    /// message. It never panics, whatever bytes the filesystem hands over.
    pub fn from_abs_os(abs: &Path, root: &Utf8Path) -> Result<Self, PathError> {
        let utf8 = Utf8Path::from_path(abs).ok_or_else(|| PathError::NotUtf8 {
            lossy: abs.to_string_lossy().into_owned(),
        })?;
        Self::from_abs(utf8, root)
    }

    /// The canonical form — exactly the bytes to write into an m3u.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Borrow as a [`Utf8Path`], for joining and the camino API.
    #[must_use]
    pub fn as_path(&self) -> &Utf8Path {
        &self.0
    }

    /// The `/`-separated components, none of which can be empty, `.` or `..`.
    ///
    /// This is the one place the separator is split on, so every component-wise
    /// comparison in MPDFM agrees.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.as_str().split('/')
    }

    /// The containing directory, or `None` for a single-component path.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.as_str()
            .rsplit_once('/')
            .map(|(dir, _)| Self(Utf8PathBuf::from(dir)))
    }

    /// The final component.
    ///
    /// Infallible by construction: a `RelPath` is never empty and never ends in
    /// a separator, so there is always a last component.
    #[must_use]
    pub fn file_name(&self) -> &str {
        match self.as_str().rsplit_once('/') {
            Some((_, name)) => name,
            None => self.as_str(),
        }
    }

    /// The extension of [`RelPath::file_name`], without the dot.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        self.0.extension()
    }

    /// Whether this path lies strictly inside the directory `dir`.
    ///
    /// The comparison is component-wise, so `hiphop/MF DOOM` does **not** match
    /// `hiphop/MF DOOM Extra/01.mp3` — string prefixing would, and would move
    /// the wrong album. A path does not lie inside itself; that case belongs to
    /// a file move and is handled by [`RelPath::reparent`].
    #[must_use]
    pub fn starts_with_dir(&self, dir: &RelPath) -> bool {
        self.strip_dir_prefix(dir).is_some()
    }

    /// Where this path lands when `from` is moved to `to`.
    ///
    /// Handles both shapes of move: `from` may be this very file, or a directory
    /// containing it. Anything else is [`PathError::NotUnder`] — a caller asking
    /// to reparent an unrelated path has a bug, and guessing would corrupt a
    /// playlist.
    ///
    /// ```
    /// use mpdfm_core::paths::{PathError, RelPath};
    ///
    /// let track = RelPath::parse("a/b/c.mp3")?;
    /// let moved = track.reparent(&RelPath::parse("a/b")?, &RelPath::parse("x/y")?)?;
    /// assert_eq!(moved.as_str(), "x/y/c.mp3");
    /// # Ok::<(), PathError>(())
    /// ```
    pub fn reparent(&self, from: &RelPath, to: &RelPath) -> Result<Self, PathError> {
        if self == from {
            return Ok(to.clone());
        }
        let rest = self
            .strip_dir_prefix(from)
            .ok_or_else(|| PathError::NotUnder {
                path: self.to_string(),
                prefix: from.to_string(),
            })?;
        // Both halves are already canonical, so this re-parse cannot fail; it
        // runs anyway to keep validation in exactly one place.
        Self::parse(&format!("{to}/{rest}"))
    }

    /// The absolute path this identity refers to under `root`.
    #[must_use]
    pub fn to_abs(&self, root: &Utf8Path) -> Utf8PathBuf {
        root.join(self.as_str())
    }

    /// The part of this path below `dir`, or `None` if it is not strictly
    /// inside `dir`. The `/` test is what makes the prefix component-wise.
    fn strip_dir_prefix(&self, dir: &RelPath) -> Option<&str> {
        self.as_str()
            .strip_prefix(dir.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rest| !rest.is_empty())
    }
}

impl std::fmt::Display for RelPath {
    /// Writes the canonical form — the exact bytes of a playlist line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<Utf8Path> for RelPath {
    fn as_ref(&self) -> &Utf8Path {
        self.as_path()
    }
}

/// Whether `candidate` really lives under `root`, with symlinks and `..`
/// resolved first.
///
/// This is the guard for safety invariant 5: no operation may read or write
/// outside `music_directory`, `playlist_directory` and MPDFM's own data
/// directory. It **fails closed** — anything it cannot resolve or reason about
/// is reported as *not* contained, because the cost of a false `true` is writing
/// outside the library.
///
/// - `root` is canonicalized; if it cannot be, the answer is `false`.
/// - `candidate` must be absolute (pass [`RelPath::to_abs`] output). A relative
///   path would silently be resolved against the process's working directory,
///   so it is rejected instead.
/// - `candidate` need not exist — a move destination does not yet. The longest
///   existing ancestor is canonicalized and the remaining components appended,
///   so a symlink anywhere in the existing part is resolved. A `.` or `..` in
///   the part that does not exist cannot be resolved safely and yields `false`.
/// - `root` itself counts as contained.
///
/// The comparison is done on [`Path`], not [`Utf8Path`], so a non-UTF-8
/// component somewhere above the root (a mount point we do not control) cannot
/// make the guard give the wrong answer.
#[must_use]
pub fn contains(root: &Utf8Path, candidate: &Utf8Path) -> bool {
    let Ok(root) = root.as_std_path().canonicalize() else {
        return false;
    };
    let candidate = candidate.as_std_path();
    if !candidate.is_absolute() {
        return false;
    }
    match resolve_longest_existing(candidate) {
        // `Path::starts_with` is component-wise, so `/music` does not contain
        // `/musicbox`.
        Some(resolved) => resolved.starts_with(&root),
        None => false,
    }
}

/// Canonicalize the longest existing ancestor of `path` and re-append the
/// components below it.
///
/// `None` when nothing can be canonicalized, or when the non-existent tail
/// contains a `.` or `..` component — [`Path::file_name`] returns `None` for
/// those, and a relative component that cannot be resolved must not be
/// silently dropped.
fn resolve_longest_existing(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<&OsStr> = Vec::new();
    let mut probe = path;
    loop {
        if let Ok(mut resolved) = probe.canonicalize() {
            resolved.extend(tail.iter().rev());
            return Some(resolved);
        }
        tail.push(probe.file_name()?);
        probe = probe.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real names from the library this tool is written for: non-ASCII, spaces,
    /// brackets, `&`, `+`, an apostrophe, and `..` inside a component.
    const REAL_PATHS: &[&str] = &[
        "electronic/kream/KREAM - So Hï [c0D2h71bFFI].mp3",
        "hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3",
        "pop/Imagine Dragons - Mercury (2 CD)/CD 1 - Acts 1 & 2/03 Wrecked.flac",
        "jazz/Miles Davis - Kind of Blue/02 Freddie Freeloader + alt.m4a",
        "rock/Guns N' Roses - Appetite for Destruction/01 Welcome.mp3",
        "single.mp3",
    ];

    fn parse(s: &str) -> RelPath {
        RelPath::parse(s).expect("should be a valid RelPath")
    }

    #[test]
    fn parse_rejects_malformed_paths() {
        let cases: &[(&str, PathError)] = &[
            ("", PathError::Empty),
            (
                "/abs/a.mp3",
                PathError::Absolute {
                    path: "/abs/a.mp3".into(),
                },
            ),
            (
                "../x.mp3",
                PathError::ParentDir {
                    path: "../x.mp3".into(),
                },
            ),
            (
                "a/../b.mp3",
                PathError::ParentDir {
                    path: "a/../b.mp3".into(),
                },
            ),
            (
                "./x.mp3",
                PathError::CurDir {
                    path: "./x.mp3".into(),
                },
            ),
            (
                "a/./b.mp3",
                PathError::CurDir {
                    path: "a/./b.mp3".into(),
                },
            ),
            (
                "a//b",
                PathError::EmptyComponent {
                    path: "a//b".into(),
                },
            ),
            ("a/", PathError::TrailingSlash { path: "a/".into() }),
            (
                "a\\b",
                PathError::Backslash {
                    path: "a\\b".into(),
                },
            ),
            (
                "C:\\music\\a.mp3",
                PathError::Backslash {
                    path: "C:\\music\\a.mp3".into(),
                },
            ),
            (
                "a\0b",
                PathError::InteriorNul {
                    path: "a\0b".into(),
                },
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                RelPath::parse(input).unwrap_err(),
                *expected,
                "wrong rejection for {input:?}"
            );
        }
    }

    #[test]
    fn parse_round_trips_byte_for_byte() {
        for input in REAL_PATHS {
            assert_eq!(parse(input).to_string(), *input);
        }
    }

    #[test]
    fn dotdot_rejection_is_component_wise() {
        // `Mm..Food` is a real album; only a whole `..` component is illegal.
        assert!(RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)/01.mp3").is_ok());
        assert!(RelPath::parse("hiphop/..Food/01.mp3").is_ok());
        assert!(RelPath::parse("hiphop/../01.mp3").is_err());
    }

    #[test]
    fn starts_with_dir_is_component_wise() {
        let track = parse("hiphop/MF DOOM Extra/01 Beef Rap.mp3");
        assert!(!track.starts_with_dir(&parse("hiphop/MF DOOM")));
        assert!(track.starts_with_dir(&parse("hiphop/MF DOOM Extra")));
        assert!(track.starts_with_dir(&parse("hiphop")));

        // A path does not lie inside itself.
        let dir = parse("hiphop/MF DOOM");
        assert!(!dir.starts_with_dir(&dir));
    }

    #[test]
    fn reparent_maps_a_directory_move() {
        let moved = parse("a/b/c.mp3")
            .reparent(&parse("a/b"), &parse("x/y"))
            .unwrap();
        assert_eq!(moved.as_str(), "x/y/c.mp3");

        let deep = parse("hiphop/MF DOOM - Mm..Food (2004)/CD 1/01 Beef Rap.mp3")
            .reparent(
                &parse("hiphop/MF DOOM - Mm..Food (2004)"),
                &parse("hiphop/MF DOOM/Mm..Food (2004)"),
            )
            .unwrap();
        assert_eq!(
            deep.as_str(),
            "hiphop/MF DOOM/Mm..Food (2004)/CD 1/01 Beef Rap.mp3"
        );
    }

    #[test]
    fn reparent_handles_a_file_move_and_rejects_strangers() {
        let renamed = parse("a/b.mp3")
            .reparent(&parse("a/b.mp3"), &parse("x/y.mp3"))
            .unwrap();
        assert_eq!(renamed.as_str(), "x/y.mp3");

        let err = parse("other/b.mp3")
            .reparent(&parse("a/b"), &parse("x/y"))
            .unwrap_err();
        assert_eq!(
            err,
            PathError::NotUnder {
                path: "other/b.mp3".into(),
                prefix: "a/b".into(),
            }
        );

        // Component-wise here too: a sibling with a shared string prefix is not
        // affected by the move.
        assert!(
            parse("a/bb/c.mp3")
                .reparent(&parse("a/b"), &parse("x/y"))
                .is_err()
        );
    }

    #[test]
    fn helpers_split_the_canonical_form() {
        let rel = parse("electronic/kream/KREAM - So Hï [c0D2h71bFFI].mp3");
        assert_eq!(rel.parent().unwrap().as_str(), "electronic/kream");
        assert_eq!(rel.file_name(), "KREAM - So Hï [c0D2h71bFFI].mp3");
        assert_eq!(rel.extension(), Some("mp3"));
        assert_eq!(
            rel.components().collect::<Vec<_>>(),
            ["electronic", "kream", "KREAM - So Hï [c0D2h71bFFI].mp3"]
        );
        assert_eq!(rel.to_abs(Utf8Path::new("/home/me/Music")).as_str(), {
            "/home/me/Music/electronic/kream/KREAM - So Hï [c0D2h71bFFI].mp3"
        });

        let single = parse("single.mp3");
        assert_eq!(single.parent(), None);
        assert_eq!(single.file_name(), "single.mp3");

        assert_eq!(parse("no-extension").extension(), None);
    }

    #[test]
    fn from_abs_measures_against_the_root() {
        let root = Utf8Path::new("/home/me/Music");
        for input in REAL_PATHS {
            let abs = Utf8PathBuf::from(format!("{root}/{input}"));
            assert_eq!(RelPath::from_abs(&abs, root).unwrap().as_str(), *input);
        }

        // Component-wise: a sibling directory sharing a string prefix is out.
        let err = RelPath::from_abs(Utf8Path::new("/home/me/Musicbox/a.mp3"), root).unwrap_err();
        assert_eq!(
            err,
            PathError::NotUnder {
                path: "/home/me/Musicbox/a.mp3".into(),
                prefix: "/home/me/Music".into(),
            }
        );

        // The root itself is not a track.
        assert_eq!(RelPath::from_abs(root, root).unwrap_err(), PathError::Empty);
    }

    #[cfg(unix)]
    #[test]
    fn from_abs_os_reports_non_utf8_and_never_panics() {
        use std::os::unix::ffi::OsStrExt;

        let root = Utf8Path::new("/home/me/Music");
        let raw = Path::new(OsStr::from_bytes(b"/home/me/Music/electronic/So H\xEF.mp3"));
        let err = RelPath::from_abs_os(raw, root).unwrap_err();
        let PathError::NotUtf8 { lossy } = err else {
            panic!("expected NotUtf8, got {err:?}");
        };
        assert!(lossy.contains("So H"), "lossy rendering was {lossy:?}");

        // Valid UTF-8 still goes through.
        let ok = Path::new("/home/me/Music/electronic/So Hï.mp3");
        assert_eq!(
            RelPath::from_abs_os(ok, root).unwrap().as_str(),
            "electronic/So Hï.mp3"
        );
    }

    #[test]
    fn identity_does_not_fold_case() {
        // ext4 is case-sensitive: these are two different directories.
        assert_ne!(parse("Artist/01.mp3"), parse("artist/01.mp3"));
        assert!(!parse("artist/01.mp3").starts_with_dir(&parse("Artist")));
    }

    #[test]
    fn identity_does_not_normalize_unicode() {
        let nfc = "electronic/So H\u{ef}.mp3"; // ï as one code point
        let nfd = "electronic/So Hi\u{308}.mp3"; // i + combining diaeresis
        assert_ne!(parse(nfc), parse(nfd));
        // Both still round-trip exactly as given.
        assert_eq!(parse(nfc).to_string(), nfc);
        assert_eq!(parse(nfd).to_string(), nfd);
    }

    /// `contains` is tested against a real filesystem — a symlink that is not
    /// actually followed proves nothing.
    mod containment {
        use super::*;

        fn utf8(path: &Path) -> &Utf8Path {
            Utf8Path::from_path(path).expect("temp dir path should be UTF-8")
        }

        #[test]
        fn accepts_paths_inside_the_root() {
            let root_dir = tempfile::tempdir().unwrap();
            let root = utf8(root_dir.path());
            std::fs::create_dir_all(root.join("hiphop/MF DOOM")).unwrap();
            std::fs::write(root.join("hiphop/MF DOOM/01.mp3"), b"x").unwrap();

            assert!(contains(root, &root.join("hiphop/MF DOOM/01.mp3")));
            assert!(contains(root, &root.join("hiphop")));
            // The root itself counts as contained.
            assert!(contains(root, root));
        }

        #[test]
        fn accepts_a_destination_that_does_not_exist_yet() {
            let root_dir = tempfile::tempdir().unwrap();
            let root = utf8(root_dir.path());

            // Move destinations are checked before they are created.
            assert!(contains(root, &root.join("new/album/01 Track.mp3")));
        }

        #[test]
        fn rejects_paths_outside_the_root() {
            let root_dir = tempfile::tempdir().unwrap();
            let outside_dir = tempfile::tempdir().unwrap();
            let root = utf8(root_dir.path());

            assert!(!contains(root, utf8(outside_dir.path())));
            // `..` is resolved before the check, not compared literally.
            assert!(!contains(root, &root.join("../escape.mp3")));
            // A relative candidate would be resolved against the working
            // directory, so it is refused outright.
            assert!(!contains(root, Utf8Path::new("hiphop/01.mp3")));
        }

        #[cfg(unix)]
        #[test]
        fn rejects_a_symlink_inside_the_root_pointing_outside_it() {
            let root_dir = tempfile::tempdir().unwrap();
            let outside_dir = tempfile::tempdir().unwrap();
            let root = utf8(root_dir.path());
            let outside = utf8(outside_dir.path());
            std::fs::write(outside.join("secret.mp3"), b"x").unwrap();

            // root/escape -> <outside>
            std::os::unix::fs::symlink(outside, root.join("escape")).unwrap();

            assert!(!contains(root, &root.join("escape")));
            assert!(!contains(root, &root.join("escape/secret.mp3")));
            // Including a destination under the link that does not exist yet.
            assert!(!contains(root, &root.join("escape/new.mp3")));
        }

        #[cfg(unix)]
        #[test]
        fn accepts_a_symlink_that_stays_inside_the_root() {
            let root_dir = tempfile::tempdir().unwrap();
            let root = utf8(root_dir.path());
            std::fs::create_dir_all(root.join("hiphop/MF DOOM")).unwrap();
            std::os::unix::fs::symlink(root.join("hiphop"), root.join("rap")).unwrap();

            assert!(contains(root, &root.join("rap/MF DOOM")));
        }

        #[test]
        fn rejects_a_root_that_cannot_be_resolved() {
            let root_dir = tempfile::tempdir().unwrap();
            let missing = utf8(root_dir.path()).join("gone");

            assert!(!contains(&missing, &missing.join("01.mp3")));
        }
    }
}
