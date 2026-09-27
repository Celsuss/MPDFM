//! The seventeen committed playlist files a fixture's playlist directory is
//! filled from.
//!
//! Same count as the real `~/.config/mpd/playlists`, same shapes: plain relative
//! paths, a CUE virtual track, radio URLs with `#EXTINF` and comments, names with
//! spaces and accents, a `.m3u8`, CRLF, a BOM, a missing trailing newline, an
//! absolute path, a duplicate entry, and the one reference that never resolved.
//! `crates/core/tests/data/playlists/README.md` says what each file is for.
//!
//! The bytes are embedded with [`include_bytes!`] like the audio templates, so a
//! fixture works whatever the test process's working directory is. They are
//! reproductions rather than copies — the real playlists are the user's and are
//! not in this repository — and they use the same album paths as
//! [`names`][super::names], so a test can assert about both a playlist line and
//! the file it points at.
//!
//! Task 06's property test is the reason they are byte-exact:
//! `write(parse(bytes)) == bytes`, for all seventeen.

/// One committed playlist: the file name MPD would see, and its exact bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaylistTemplate {
    /// File name inside the playlist directory, spaces and accents included.
    pub name: &'static str,
    /// The file's exact bytes — BOM, line endings and final newline as committed.
    pub bytes: &'static [u8],
}

impl PlaylistTemplate {
    /// MPD's name for this playlist: [`PlaylistTemplate::name`] without its
    /// `.m3u`/`.m3u8` suffix.
    #[must_use]
    pub fn playlist_name(&self) -> &'static str {
        crate::playlist::playlist_name(self.name)
    }
}

/// Every committed playlist, in the order a `ls` of the directory gives them.
///
/// [`FixtureBuilder::real_playlists`][super::FixtureBuilder::real_playlists]
/// writes all of these into a fixture; a test that wants one on its own can read
/// its bytes straight from here.
pub const PLAYLIST_TEMPLATES: &[PlaylistTemplate] = &[
    PlaylistTemplate {
        name: "Absolute paths.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Absolute paths.m3u"),
    },
    PlaylistTemplate {
        name: "Bangers.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Bangers.m3u"),
    },
    PlaylistTemplate {
        name: "Chill.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Chill.m3u"),
    },
    PlaylistTemplate {
        name: "Coding flow.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Coding flow.m3u"),
    },
    PlaylistTemplate {
        name: "Cue sheets.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Cue sheets.m3u"),
    },
    PlaylistTemplate {
        name: "Duplicates.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Duplicates.m3u"),
    },
    PlaylistTemplate {
        name: "Empty.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Empty.m3u"),
    },
    PlaylistTemplate {
        name: "En kall Stockholms natt.m3u",
        bytes: include_bytes!("../../tests/data/playlists/En kall Stockholms natt.m3u"),
    },
    PlaylistTemplate {
        name: "Hip hop.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Hip hop.m3u"),
    },
    PlaylistTemplate {
        name: "Jazz.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Jazz.m3u"),
    },
    PlaylistTemplate {
        name: "Mixed bag.m3u8",
        bytes: include_bytes!("../../tests/data/playlists/Mixed bag.m3u8"),
    },
    PlaylistTemplate {
        name: "Only comments.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Only comments.m3u"),
    },
    PlaylistTemplate {
        name: "Pop.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Pop.m3u"),
    },
    PlaylistTemplate {
        name: "Radios.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Radios.m3u"),
    },
    PlaylistTemplate {
        name: "Saved queue.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Saved queue.m3u"),
    },
    PlaylistTemplate {
        name: "Whitespace.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Whitespace.m3u"),
    },
    PlaylistTemplate {
        name: "Windows.m3u",
        bytes: include_bytes!("../../tests/data/playlists/Windows.m3u"),
    },
];

/// The template with this file name.
///
/// # Panics
///
/// If there is no such fixture playlist — which in a test means a typo in the
/// test.
#[must_use]
pub fn playlist_template(name: &str) -> &'static PlaylistTemplate {
    PLAYLIST_TEMPLATES
        .iter()
        .find(|template| template.name == name)
        .unwrap_or_else(|| panic!("there is no committed playlist fixture called {name:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_seventeen_of_them_and_the_names_are_unique() {
        // The real library has seventeen. If this set grows, say why in
        // `tests/data/playlists/README.md`.
        assert_eq!(PLAYLIST_TEMPLATES.len(), 17);

        let mut names: Vec<&str> = PLAYLIST_TEMPLATES.iter().map(|it| it.name).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "two fixtures share a name");

        for template in PLAYLIST_TEMPLATES {
            assert!(
                crate::playlist::is_playlist_name(template.name),
                "{} would not be loaded by MPD",
                template.name
            );
        }
    }

    /// The four properties an editor would silently "fix", each of which a
    /// committed fixture depends on. If this fails, a `.m3u` was saved by
    /// something that tidied it — not a reason to change the assertion.
    #[test]
    fn shapes_that_cannot_be_typed_survive_in_the_repository() {
        let bytes_of = |name: &str| playlist_template(name).bytes;

        assert!(bytes_of("Jazz.m3u").ends_with(b"\r\n"), "Jazz.m3u is CRLF");
        assert!(
            !bytes_of("Jazz.m3u").windows(2).any(|pair| pair == b"\n\n"),
            "Jazz.m3u has no bare LF"
        );
        assert!(
            bytes_of("Bangers.m3u").starts_with(b"\xef\xbb\xbf"),
            "Bangers.m3u starts with a BOM"
        );
        assert!(
            !bytes_of("Chill.m3u").ends_with(b"\n"),
            "Chill.m3u has no trailing newline"
        );
        assert!(
            bytes_of("Windows.m3u").starts_with(b"\xef\xbb\xbf")
                && bytes_of("Windows.m3u").contains(&b'\r')
                && !bytes_of("Windows.m3u").ends_with(b"\n"),
            "Windows.m3u is a BOM, CRLF and no trailing newline at once"
        );
        assert!(
            bytes_of("Whitespace.m3u").windows(3).any(|w| w == b" \n "),
            "Whitespace.m3u keeps its trailing space and its spaces-only line"
        );
        assert!(bytes_of("Empty.m3u").is_empty(), "Empty.m3u is empty");
    }

    #[test]
    fn a_playlist_name_is_the_file_name_without_its_suffix() {
        assert_eq!(
            playlist_template("En kall Stockholms natt.m3u").playlist_name(),
            "En kall Stockholms natt"
        );
        assert_eq!(
            playlist_template("Mixed bag.m3u8").playlist_name(),
            "Mixed bag"
        );
    }

    #[test]
    fn every_fixture_but_the_empty_one_has_content() {
        for template in PLAYLIST_TEMPLATES {
            if template.name == "Empty.m3u" {
                continue;
            }
            assert!(!template.bytes.is_empty(), "{} is empty", template.name);
            // Small enough to read in a diff; a regeneration that blew one up
            // should be noticed here.
            assert!(
                template.bytes.len() < 4 * 1024,
                "{} is {} bytes, too big to commit",
                template.name,
                template.bytes.len()
            );
        }
    }
}
