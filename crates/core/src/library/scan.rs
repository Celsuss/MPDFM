//! The walk: one pass over `music_directory` producing every [`Entry`] the
//! model holds.
//!
//! # What it does
//!
//! `stat` and nothing else. Every entry costs one directory read and one
//! `lstat` — the metadata `walkdir` already has to fetch — and no file is ever
//! opened. That is what keeps a 3 100-file library scannable at startup, and it
//! is a property worth defending: see the benchmark and the no-tag-I/O test in
//! `tests/library.rs`.
//!
//! # What it refuses to do
//!
//! **Follow symlinks.** A link pointing at an ancestor makes the walk infinite,
//! and a link pointing out of the library puts paths in the model that safety
//! invariant 5 forbids MPDFM from writing to. Links are reported as
//! [`ScanWarning::Symlink`] and left alone. The one exception is `root` itself,
//! which `walkdir` follows by default: a `music_directory` that is a symlink is
//! an ordinary setup, and the link is resolved before the walk rather than during
//! it.
//!
//! **Guess at a name it cannot represent.** A non-UTF-8 name, or one a
//! [`RelPath`] rejects, is reported and skipped (`docs/PLAN.md` safety invariant
//! 8). For a *directory* the whole subtree is skipped with it: MPDFM could not
//! name anything inside it either, and one warning is more useful than one per
//! file.
//!
//! **Stop.** Everything except a missing or unreadable root is a warning. A
//! library with one unreadable album in it is still a library.

use camino::Utf8Path;
use walkdir::WalkDir;

use super::model::{DirPath, Entry, Kind, Library, ScanWarning};
use crate::paths::{PathError, RelPath};
use crate::{Error, Result};

/// Walk `root` and build the [`Library`].
///
/// # Errors
///
/// [`Error::Io`] if `root` is missing, is not a directory, or cannot be `stat`ed
/// — the one failure that is not worth continuing past, since every later
/// question is about the tree underneath it. Everything else is a
/// [`ScanWarning`] on the returned library.
pub(super) fn scan(root: &Utf8Path) -> Result<Library> {
    // Checked before the walk so that "there is no library there" is an error
    // with the root's name in it, rather than an empty model and a warning that
    // reads like one file went missing. `metadata` follows a symlinked root, as
    // the walk below does.
    let metadata = std::fs::metadata(root).map_err(|source| Error::Io {
        path: root.to_string(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(Error::Io {
            path: root.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "music directory is not a directory",
            ),
        });
    }

    let mut entries = Vec::new();
    // The root is a directory of the library even when it holds no files, so the
    // browser has somewhere to start.
    let mut dirs = vec![DirPath::root()];
    let mut warnings = Vec::new();

    // Driven by hand rather than with a `for` loop: `skip_current_dir` is how a
    // directory whose name MPDFM cannot represent is pruned, and it needs the
    // iterator itself.
    let mut walk = WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter();
    loop {
        let Some(result) = walk.next() else {
            break;
        };
        let found = match result {
            Ok(found) => found,
            Err(err) => {
                warnings.push(unreadable(root, &err));
                continue;
            }
        };

        // Name it first, whatever it turns out to be: a warning about a symlink
        // or an unreadable file is only useful if it can say which path.
        let rel = match RelPath::from_abs_os(found.path(), root) {
            Ok(rel) => rel,
            Err(err) => {
                warnings.push(unnamable(found.path(), err));
                if found.file_type().is_dir() {
                    walk.skip_current_dir();
                }
                continue;
            }
        };

        let file_type = found.file_type();
        if file_type.is_symlink() {
            // `walkdir` does not descend into it, so reporting it is all there is
            // to do. It is left out of the model on purpose: MPDFM does not move
            // what it has not resolved.
            warnings.push(ScanWarning::Symlink {
                path: rel,
                target: std::fs::read_link(found.path())
                    .ok()
                    .map(|target| target.to_string_lossy().into_owned()),
            });
            continue;
        }
        if file_type.is_dir() {
            dirs.push(DirPath::from(rel));
            continue;
        }

        // `metadata` here is `lstat` — `follow_links` is off — and is the only
        // syscall spent per file. Nothing opens it.
        let metadata = match found.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                warnings.push(unreadable(root, &err));
                continue;
            }
        };
        let mtime = match metadata.modified() {
            Ok(mtime) => mtime,
            // Unreachable on Linux. An entry with no mtime could not be verified
            // before an undo (task 12), so it is reported rather than modelled
            // with a timestamp that is not true.
            Err(err) => {
                warnings.push(ScanWarning::Unreadable {
                    path: rel.to_string(),
                    message: err.to_string(),
                });
                continue;
            }
        };
        entries.push(Entry {
            kind: Kind::of(&rel),
            rel,
            size: metadata.len(),
            mtime,
        });
    }

    Ok(Library::assemble(root, entries, dirs, warnings))
}

/// A [`ScanWarning::Unreadable`] from a `walkdir` error.
///
/// `walkdir` attaches the path to every error it raises below the root; without
/// one — which the up-front check on the root makes unlikely — the root is the
/// only honest thing to name.
fn unreadable(root: &Utf8Path, err: &walkdir::Error) -> ScanWarning {
    let path = err.path().map_or_else(
        || root.to_string(),
        |path| path.to_string_lossy().into_owned(),
    );
    ScanWarning::Unreadable {
        path,
        message: message_of(err),
    }
}

/// The operating system's complaint, without `walkdir`'s own prose around it.
fn message_of(err: &walkdir::Error) -> String {
    err.io_error()
        .map_or_else(|| err.to_string(), std::io::Error::to_string)
}

/// A [`ScanWarning`] for a path MPDFM cannot name. Non-UTF-8 gets its own
/// variant: it is the case `docs/PLAN.md` calls out, and the one a user is most
/// likely to have to fix by renaming.
fn unnamable(path: &std::path::Path, err: PathError) -> ScanWarning {
    match err {
        PathError::NotUtf8 { lossy } => ScanWarning::NotUtf8 { lossy },
        reason => ScanWarning::Unnamable {
            path: path.to_string_lossy().into_owned(),
            reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failures that are *not* warnings. Everything else is exercised
    /// against the fixture library in `tests/library.rs`.
    #[test]
    fn a_root_that_is_not_a_readable_directory_is_an_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = camino::Utf8Path::from_path(temp.path()).expect("temp dir path is UTF-8");

        let missing = root.join("gone");
        let err = scan(&missing).expect_err("a missing root should not scan");
        assert!(
            err.to_string().contains(missing.as_str()),
            "the error should name the root: {err}"
        );

        let file = root.join("not-a-dir");
        std::fs::write(&file, b"x").expect("write");
        let err = scan(&file).expect_err("a file is not a library");
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn an_empty_root_scans_to_an_empty_library() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = camino::Utf8Path::from_path(temp.path()).expect("temp dir path is UTF-8");

        let library = scan(root).expect("an empty directory is a valid library");
        assert!(library.is_empty());
        assert!(library.warnings().is_empty());
        // The root itself is always a directory of the library, so a browser has
        // somewhere to stand.
        assert_eq!(library.dir_count(), 1);
        assert!(library.dir(&DirPath::root()).is_some());
    }
}
