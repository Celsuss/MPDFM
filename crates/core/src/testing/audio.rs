//! The committed audio templates a fixture stamps its tracks out of.
//!
//! The bytes are embedded with [`include_bytes!`] rather than read from disk, so
//! a fixture works whatever the test process's working directory is, and so that
//! `cargo test` never needs `ffmpeg` (task 03). `crates/core/tests/data/` holds
//! the files themselves, a description of each, and the script that regenerates
//! them.
//!
//! Every track of a given format is therefore **byte-identical** to every other.
//! That is what makes a snapshot comparison meaningful — a move must not change
//! any bytes — but it means content alone cannot tell two tracks apart. A test
//! that needs distinguishable files should change one with
//! [`Fixture::flip_byte`][super::Fixture::flip_byte].

/// One of the committed audio files, chosen by a track's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioTemplate {
    /// mp3 with ID3v2.4 tags — the default for a `.mp3` track.
    Mp3v24,
    /// mp3 with ID3v2.3 tags, for the frame-version cases in task 16.
    Mp3v23,
    /// flac with Vorbis comments.
    Flac,
    /// m4a with MP4 atoms.
    M4a,
    /// mp3 with no tags at all, which must read as an empty `TagSet`.
    Untagged,
}

impl AudioTemplate {
    /// The file's bytes, ready to write.
    #[must_use]
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Self::Mp3v24 => include_bytes!("../../tests/data/sine-id3v24.mp3"),
            Self::Mp3v23 => include_bytes!("../../tests/data/sine-id3v23.mp3"),
            Self::Flac => include_bytes!("../../tests/data/sine.flac"),
            Self::M4a => include_bytes!("../../tests/data/sine.m4a"),
            Self::Untagged => include_bytes!("../../tests/data/untagged.mp3"),
        }
    }

    /// The extension the template's container wants, without the dot.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3v24 | Self::Mp3v23 | Self::Untagged => "mp3",
            Self::Flac => "flac",
            Self::M4a => "m4a",
        }
    }

    /// The template implied by a file name's extension, or `None` if the name is
    /// not an audio file MPDFM handles.
    ///
    /// The match is case-insensitive because scene releases ship `.MP3` and
    /// `.Flac` and MPD accepts both.
    #[must_use]
    pub fn for_file_name(name: &str) -> Option<Self> {
        let (_, ext) = name.rsplit_once('.')?;
        match ext.to_ascii_lowercase().as_str() {
            "mp3" => Some(Self::Mp3v24),
            "flac" => Some(Self::Flac),
            "m4a" => Some(Self::M4a),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_is_small_and_non_empty() {
        for template in [
            AudioTemplate::Mp3v24,
            AudioTemplate::Mp3v23,
            AudioTemplate::Flac,
            AudioTemplate::M4a,
            AudioTemplate::Untagged,
        ] {
            let len = template.bytes().len();
            assert!(len > 0, "{template:?} is empty");
            // Committed fixtures stay tiny; a regeneration that blows this up
            // (flac padding, a Xing header) should be noticed here.
            assert!(
                len < 8 * 1024,
                "{template:?} is {len} bytes, too big to commit"
            );
        }
    }

    #[test]
    fn extension_picks_the_container() {
        assert_eq!(
            AudioTemplate::for_file_name("01 Beef Rap.mp3"),
            Some(AudioTemplate::Mp3v24)
        );
        assert_eq!(
            AudioTemplate::for_file_name("02 Wrecked.FLAC"),
            Some(AudioTemplate::Flac)
        );
        assert_eq!(
            AudioTemplate::for_file_name("03 Freddie.m4a"),
            Some(AudioTemplate::M4a)
        );
        assert_eq!(AudioTemplate::for_file_name("folder.jpg"), None);
        assert_eq!(AudioTemplate::for_file_name("no-extension"), None);
    }

    #[test]
    fn mp3_variants_are_different_files() {
        assert_ne!(AudioTemplate::Mp3v24.bytes(), AudioTemplate::Mp3v23.bytes());
        assert_eq!(AudioTemplate::Mp3v23.extension(), "mp3");
    }
}
