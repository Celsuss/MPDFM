//! [`Entry`][super::Entry] list → bytes, and bytes → the file, atomically.
//!
//! # Serializing
//!
//! Join every line's exact bytes with the file's line ending, put the BOM back if
//! there was one, and terminate the last line only if it was terminated. Nothing
//! is sorted, nothing is deduplicated, no whitespace is trimmed, and
//! `#EXTM3U` is the only text this module produces from nothing — which is why
//! the parser recognizes it only in its exact spelling.
//!
//! # Replacing the file
//!
//! Temp file in the *same directory*, `fsync`, `rename` (`docs/PLAN.md` safety
//! invariant 6). A crash at any point leaves either the old playlist or the new
//! one, never a truncated file, and because the temp file is a sibling the rename
//! is atomic — across a filesystem boundary it would not be.
//!
//! Three details that are each there for a reason:
//!
//! - **The target is never opened for writing.** Truncating the file in place
//!   would destroy a playlist if the write then failed, and truncating a *symlink*
//!   would follow it and do the same to the link's target. So the write refuses a
//!   `real_path` that is still a symlink; [`Playlist::load`][super::Playlist::load]
//!   resolves it first, which is what makes the dotfiles-repo `Radios.m3u` work.
//! - **The temp name starts with a dot and ends in `.tmp`**, so MPD — which reads
//!   this very directory — does not pick it up as a playlist called
//!   `.Radios.m3u.mpdfm-1234.0`.
//! - **The original's permission bits are copied onto the temp file** before the
//!   rename. A playlist that was `0600` stays `0600`; without this, the new file
//!   would carry whatever the process umask happened to say.
//!
//! The directory is `fsync`ed after the rename so the new name survives a power
//! cut too. That failing does not fail the write — the rename has already
//! happened, and reporting an error would tell the caller a lie.

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use camino::Utf8Path;

use super::Playlist;
use crate::{Error, Result};

/// The file's exact bytes: BOM, lines joined by the line ending, trailing
/// newline if it had one.
pub(super) fn to_bytes(playlist: &Playlist) -> Vec<u8> {
    let ending = playlist.line_ending().as_str();
    let mut out = String::new();
    if playlist.has_bom() {
        out.push_str(BOM);
    }
    for (index, entry) in playlist.entries().iter().enumerate() {
        if index > 0 {
            out.push_str(ending);
        }
        out.push_str(entry.line());
    }
    // Guarded on there being a line to terminate: a playlist every entry of which
    // has been removed is an empty file, not a lone newline.
    if playlist.trailing_newline() && !playlist.entries().is_empty() {
        out.push_str(ending);
    }
    out.into_bytes()
}

/// The UTF-8 byte-order mark.
const BOM: &str = "\u{feff}";

/// Where a write should stop.
///
/// [`Stop::BeforeRename`] exists for the atomicity test: everything written and
/// `fsync`ed, and then the failure the test cannot cause from outside. The
/// production path passes [`Stop::Never`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// Complete the write.
    Never,
    /// Fail after the temp file is written and synced, before the rename.
    BeforeRename,
}

/// Distinguishes one process's temp files from another's, and one write from the
/// next. `create_new` is what actually guarantees exclusivity; this only keeps
/// the retry loop short.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many names to try before giving up. Reachable only if a previous run died
/// leaving temp files with this process's pid, which is why it is not 1.
const TEMP_ATTEMPTS: u32 = 16;

/// Replace `target` with `bytes`, atomically, preserving `target`'s mode.
///
/// `target` must be a real file, not a symlink: see the [module docs][self].
pub(super) fn replace_file(target: &Utf8Path, bytes: &[u8], stop: Stop) -> Result<()> {
    let io = |source: std::io::Error| Error::Io {
        path: target.to_string(),
        source,
    };

    let existing = match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_symlink() => {
            return Err(io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to write through a symlink; load the playlist with \
                 Playlist::load, which resolves it",
            )));
        }
        Ok(metadata) => Some(metadata),
        // A playlist that does not exist yet is being created, which is allowed.
        // Any other error is the caller's to see.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(io(err)),
    };

    let dir = target.parent().filter(|dir| !dir.as_str().is_empty());
    let dir = dir.ok_or_else(|| {
        io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "playlist path has no directory to write a temp file in",
        ))
    })?;

    let (temp, mut file) = create_temp(dir, target).map_err(io)?;
    // From here on the temp file exists, so every failure removes it before
    // returning: a directory littered with `.Radios.m3u.mpdfm-*.tmp` after a
    // failed write is its own bug report.
    let write = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        if let Some(metadata) = &existing {
            file.set_permissions(metadata.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        if stop == Stop::BeforeRename {
            return Err(std::io::Error::other("simulated failure before rename"));
        }
        std::fs::rename(&temp, target)
    })();
    if let Err(err) = write {
        // Best effort: the write has already failed, and a failure to clean up
        // must not replace the error that says why.
        let _ = std::fs::remove_file(&temp);
        return Err(io(err));
    }

    // Durability of the *name*, not of the contents — those were synced above.
    // The rename has happened either way, so a failure here is not the caller's
    // problem.
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    Ok(())
}

/// Create a sibling temp file, and hand back its path and the open handle.
///
/// `create_new` means the file did not exist a moment ago, so this can never
/// truncate something that matters.
fn create_temp(
    dir: &Utf8Path,
    target: &Utf8Path,
) -> std::io::Result<(camino::Utf8PathBuf, std::fs::File)> {
    let name = target.file_name().unwrap_or("playlist");
    let pid = std::process::id();
    let mut last = None;
    for _ in 0..TEMP_ATTEMPTS {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".{name}.mpdfm-{pid}.{counter}.tmp"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => last = Some(err),
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no unused temp file name in the playlist directory",
        )
    }))
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use camino::{Utf8Path, Utf8PathBuf};

    use super::*;
    use crate::playlist::{Entry, LineEnding, Playlist};

    /// A directory that deletes itself, and the playlist path inside it.
    struct Dir {
        temp: tempfile::TempDir,
    }

    impl Dir {
        fn new() -> Self {
            Self {
                temp: tempfile::tempdir().expect("a temp directory"),
            }
        }

        fn path(&self) -> Utf8PathBuf {
            Utf8Path::from_path(self.temp.path())
                .expect("a temp path is UTF-8")
                .to_owned()
        }

        /// Write `bytes` to `name` and load it back as a playlist.
        fn playlist(&self, name: &str, bytes: &[u8]) -> Playlist {
            let path = self.path().join(name);
            std::fs::write(&path, bytes).expect("can write the fixture playlist");
            Playlist::load(&path).expect("the fixture playlist parses")
        }

        /// Every file name in the directory, sorted — so a leftover temp file is
        /// visible.
        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.temp.path())
                .expect("can read the temp directory")
                .map(|entry| {
                    entry
                        .expect("a readable directory entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        }
    }

    fn read(path: &Utf8Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
    }

    fn mode_of(path: &Utf8Path) -> u32 {
        std::fs::metadata(path)
            .unwrap_or_else(|err| panic!("cannot stat {path}: {err}"))
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn serializing_reproduces_the_bytes_it_parsed() {
        let cases: &[&[u8]] = &[
            b"",
            b"\n",
            b"a.mp3\n",
            b"a.mp3",
            b"#EXTM3U\r\na.mp3\r\n",
            b"#EXTM3U\r\na.mp3",
            b"\xef\xbb\xbf#EXTM3U\na.mp3\n",
            b"\xef\xbb\xbf",
            b"a.mp3\n\n\n# trailing comment\n",
            b"a.mp3\nb.mp3\r\nc.mp3\n",
            b"   \n\t\n",
        ];
        for bytes in cases {
            let playlist =
                Playlist::from_bytes(Utf8Path::new("Round trip.m3u"), bytes).expect("UTF-8");
            assert_eq!(
                playlist.to_bytes(),
                *bytes,
                "{:?} did not round-trip",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn an_emptied_playlist_is_an_empty_file_not_a_newline() {
        let mut playlist =
            Playlist::from_bytes(Utf8Path::new("Gone.m3u"), b"a.mp3\nb.mp3\n").expect("UTF-8");
        assert!(playlist.trailing_newline());
        playlist.entries_mut().clear();
        assert!(playlist.to_bytes().is_empty());
    }

    #[test]
    fn a_rewritten_line_uses_the_file_s_line_ending() {
        let mut playlist =
            Playlist::from_bytes(Utf8Path::new("Jazz.m3u"), b"#EXTM3U\r\nold.flac\r\n")
                .expect("UTF-8");
        assert_eq!(playlist.line_ending(), LineEnding::Crlf);
        playlist.entries_mut()[1] = Entry::track(
            crate::paths::RelPath::parse("new.flac").expect("valid"),
            None,
        );
        assert_eq!(playlist.to_bytes(), b"#EXTM3U\r\nnew.flac\r\n");
    }

    #[test]
    fn writing_replaces_the_file_and_keeps_its_mode() {
        let dir = Dir::new();
        let playlist = dir.playlist("Pop.m3u", b"#EXTM3U\nold.mp3\n");
        std::fs::set_permissions(playlist.real_path(), std::fs::Permissions::from_mode(0o640))
            .expect("can chmod the fixture");

        let mut playlist = Playlist::load(playlist.path()).expect("reloads");
        playlist
            .entries_mut()
            .push(Entry::Comment("# added".to_owned()));
        playlist.write().expect("the write succeeds");

        assert_eq!(read(playlist.real_path()), b"#EXTM3U\nold.mp3\n# added\n");
        assert_eq!(mode_of(playlist.real_path()), 0o640);
        assert_eq!(dir.names(), vec!["Pop.m3u".to_owned()], "no temp file left");
    }

    #[test]
    fn a_failure_before_the_rename_leaves_the_original_untouched() {
        let dir = Dir::new();
        let original = b"#EXTM3U\nkeep-me.mp3\n";
        let mut playlist = dir.playlist("Pop.m3u", original);
        playlist.entries_mut().clear();

        let err = replace_file(
            playlist.real_path(),
            &playlist.to_bytes(),
            Stop::BeforeRename,
        )
        .expect_err("the simulated failure should be reported");
        assert!(
            err.to_string().contains("simulated failure"),
            "unexpected error: {err}"
        );

        assert_eq!(
            read(playlist.real_path()),
            original,
            "the original playlist must be byte-identical after a failed write"
        );
        assert_eq!(
            dir.names(),
            vec!["Pop.m3u".to_owned()],
            "the temp file must be cleaned up"
        );
    }

    #[test]
    fn a_directory_that_cannot_be_written_to_fails_without_touching_the_playlist() {
        let dir = Dir::new();
        let original = b"#EXTM3U\nkeep-me.mp3\n";
        let playlist = dir.playlist("Pop.m3u", original);

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))
            .expect("can chmod the temp directory");
        let refused = playlist.write();
        // Root ignores the mode bits, so say so rather than pass vacuously.
        let read_only = refused.is_err();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("can chmod the temp directory back");

        if read_only {
            assert_eq!(read(playlist.real_path()), original);
            assert_eq!(dir.names(), vec!["Pop.m3u".to_owned()]);
        } else {
            eprintln!("skipped: running as a user the directory mode does not stop");
        }
    }

    #[test]
    fn writing_through_a_symlink_is_refused() {
        let dir = Dir::new();
        let target = dir.path().join("Target.m3u");
        std::fs::write(&target, b"#EXTM3U\n").expect("can write the link target");
        let link = dir.path().join("Link.m3u");
        std::os::unix::fs::symlink(&target, &link).expect("can create the link");

        // `from_bytes` does not resolve the link, which is exactly the mistake
        // this refusal is here to catch.
        let playlist = Playlist::from_bytes(&link, b"#EXTM3U\n").expect("UTF-8");
        let err = playlist.write().expect_err("a symlink must be refused");
        assert!(
            err.to_string().contains("symlink"),
            "unexpected error: {err}"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("the link is still there")
                .is_symlink()
        );

        // Loaded properly, the same write lands on the target.
        let playlist = Playlist::load(&link).expect("loads through the link");
        assert_eq!(playlist.real_path(), target);
        playlist.write().expect("writes the target");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("the link is still there")
                .is_symlink()
        );
    }

    #[test]
    fn a_playlist_that_does_not_exist_yet_is_created() {
        let dir = Dir::new();
        let path = dir.path().join("New list.m3u");
        let playlist = Playlist::from_bytes(&path, b"#EXTM3U\na.mp3\n").expect("UTF-8");
        playlist.write().expect("creates the file");
        assert_eq!(read(&path), b"#EXTM3U\na.mp3\n");
        assert_eq!(playlist.name(), "New list");
    }

    #[test]
    fn temp_names_are_unique_and_hidden_from_mpd() {
        let dir = Dir::new();
        let target = dir.path().join("Pop.m3u");
        let (first, _keep) = create_temp(&dir.path(), &target).expect("a temp file");
        let (second, _keep) = create_temp(&dir.path(), &target).expect("another temp file");
        assert_ne!(first, second);
        for temp in [&first, &second] {
            let name = temp.file_name().expect("a name");
            assert!(name.starts_with(".Pop.m3u.mpdfm-"), "{name} is not hidden");
            assert!(name.ends_with(".tmp"), "{name} does not end in .tmp");
            assert!(
                !crate::playlist::is_playlist_name(name),
                "{name} would be loaded by MPD as a playlist"
            );
        }
    }
}
