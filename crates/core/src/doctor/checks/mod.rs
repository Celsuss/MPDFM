//! The checks themselves, one module per [`Group`].
//!
//! [`ALL`] is the catalogue: every check's name, group, severity and one-line
//! meaning, in report order. A check's module looks its own name up in it by
//! matching on `info.name`, so the catalogue and the code cannot drift apart
//! without a test noticing — `every_catalogued_check_is_implemented` in
//! `tests/doctor.rs` runs each one.

pub mod duplicates;
pub mod filesystem;
pub mod references;
pub mod tags;

use super::{Group, Info, Severity};

/// Shorthand for a catalogue row.
const fn info(name: &'static str, group: Group, severity: Severity, about: &'static str) -> Info {
    Info {
        name,
        group,
        severity,
        about,
    }
}

/// Every check, in report order.
pub const ALL: &[Info] = &[
    // --- references ---
    info(
        "broken-references",
        Group::References,
        Severity::Problem,
        "playlist lines whose file, or CUE track, is missing",
    ),
    info(
        "unrewritable-entries",
        Group::References,
        Severity::Problem,
        "playlist lines that are not paths relative to the music directory",
    ),
    info(
        "unreadable-playlists",
        Group::References,
        Severity::Problem,
        "playlists that could not be read, so a move cannot fix them",
    ),
    info(
        "broken-queue-entries",
        Group::References,
        Severity::Warning,
        "entries in MPD's saved queue whose file is missing",
    ),
    info(
        "duplicate-entries",
        Group::References,
        Severity::Note,
        "the same track more than once in one playlist",
    ),
    info(
        "unreferenced-audio",
        Group::References,
        Severity::Note,
        "tracks that appear in no playlist (normal, not a defect)",
    ),
    // --- tags ---
    info(
        "unreadable-tags",
        Group::Tags,
        Severity::Warning,
        "audio files whose tags could not be read",
    ),
    info(
        "untagged",
        Group::Tags,
        Severity::Warning,
        "audio files with no tags at all",
    ),
    info(
        "missing-tags",
        Group::Tags,
        Severity::Warning,
        "tracks missing a title, artist, album or genre",
    ),
    info(
        "inconsistent-albums",
        Group::Tags,
        Severity::Warning,
        "album directories where a few tracks disagree on album, album artist or year",
    ),
    info(
        "track-numbers",
        Group::Tags,
        Severity::Warning,
        "album directories with a duplicated or missing track number",
    ),
    info(
        "id3v1-only",
        Group::Tags,
        Severity::Warning,
        "mp3s with only an ID3v1 tag, which MPDFM cannot edit",
    ),
    info(
        "implausible-years",
        Group::Tags,
        Severity::Warning,
        "dates that are not a plausible year",
    ),
    // --- filesystem ---
    info(
        "bad-names",
        Group::Filesystem,
        Severity::Problem,
        "names MPDFM cannot handle: not UTF-8, or not a usable path",
    ),
    info(
        "unreadable-paths",
        Group::Filesystem,
        Severity::Problem,
        "files or directories that could not be read",
    ),
    info(
        "normalization-twins",
        Group::Filesystem,
        Severity::Problem,
        "sibling names that differ only in Unicode normalization (NFC/NFD)",
    ),
    info(
        "case-collisions",
        Group::Filesystem,
        Severity::Warning,
        "sibling names that differ only in case",
    ),
    info(
        "unfiled-audio",
        Group::Filesystem,
        Severity::Warning,
        "tracks sitting directly in the music directory",
    ),
    info(
        "empty-dirs",
        Group::Filesystem,
        Severity::Warning,
        "directories with nothing in them",
    ),
    info(
        "orphan-aux",
        Group::Filesystem,
        Severity::Warning,
        "cover art, .nfo, .sfv and the like with no album around them",
    ),
    info(
        "no-audio-dirs",
        Group::Filesystem,
        Severity::Note,
        "album subdirectories with files but no tracks (scans, covers)",
    ),
    info(
        "partial-downloads",
        Group::Filesystem,
        Severity::Note,
        "incomplete downloads that may still be being written",
    ),
    // --- duplicates ---
    info(
        "same-song",
        Group::Duplicates,
        Severity::Note,
        "the same artist and title in more than one place",
    ),
    info(
        "identical-files",
        Group::Duplicates,
        Severity::Note,
        "byte-identical audio files (--deep)",
    ),
];

/// A message for a catalogue row a group module does not implement — a bug,
/// caught by the test that runs every check.
fn unknown(info: Info) -> ! {
    unreachable!(
        "{} is catalogued under {:?} and not implemented there",
        info.name, info.group
    )
}
