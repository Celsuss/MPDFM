//! Bytes → [`Entry`] list, losing nothing on the way.
//!
//! # The shape of the file
//!
//! Three properties are of the file rather than of any line, and all three have
//! to come back out unchanged:
//!
//! - **The BOM.** Stripped here and recorded, re-emitted by the writer. It is not
//!   part of the first line's text, so a `#EXTM3U` behind a BOM is still
//!   recognized as `#EXTM3U`.
//! - **The line ending.** `Crlf` only when the file has at least one terminator
//!   and *every* one of them is `\r\n`; otherwise `Lf`. A file that mixes the two
//!   is therefore read as LF, and the `\r` of a CRLF line stays inside that
//!   line's text — where it makes the line [`Entry::Unparsed`], so the stray byte
//!   is preserved instead of being stripped from a path MPDFM would then fail to
//!   find. `Whitespace.m3u` in the fixture set is exactly this file.
//! - **The trailing newline.** A file whose last line is unterminated keeps it
//!   that way; that last line's bytes are taken verbatim, `\r` and all.
//!
//! # Classifying a line
//!
//! In order, first match wins:
//!
//! | The line | becomes |
//! | --- | --- |
//! | empty | [`Entry::Blank`] |
//! | only whitespace | [`Entry::Unparsed`] |
//! | contains `\r` or a NUL | [`Entry::Unparsed`] |
//! | exactly `#EXTM3U` | [`Entry::ExtM3u`] |
//! | `#EXTINF:<integer>,<title>` | [`Entry::ExtInf`] |
//! | starts with `#` | [`Entry::Comment`] |
//! | `<scheme>://…` | [`Entry::Url`] |
//! | a [`RelPath`], with or without a CUE suffix | [`Entry::Track`] |
//! | anything else | [`Entry::Unparsed`] |
//!
//! The last two rows are where the care is. [`RelPath::parse`] is what decides
//! whether a line is a track, and it rejects absolute paths, `./`, `..`, doubled
//! separators, backslashes and NULs — so every one of those lands in
//! [`Entry::Unparsed`] and survives untouched. It also guarantees that what it
//! accepts renders back byte-identically, which is why a track line can be
//! reconstructed from its parts at all (task 09 relies on this).
//!
//! An `#EXTINF` whose duration is not an integer, or which has no comma, is a
//! [`Entry::Comment`]: it starts with `#`, MPDFM has no reading of it, and a
//! comment round-trips perfectly well.

use camino::Utf8Path;

use super::{EXTM3U, Entry, LineEnding};
use crate::paths::RelPath;

/// Why a playlist's bytes could not be parsed.
///
/// There is one way, and it is the one `docs/PLAN.md` safety invariant 8 names:
/// bytes that are not UTF-8 are reported, never guessed at. Everything else in a
/// playlist is preserved rather than rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The file is not valid UTF-8.
    ///
    /// MPD's own playlists are UTF-8, and an m3u whose encoding MPDFM guessed at
    /// would be rewritten into a file the user did not write.
    #[error("{path}: not valid UTF-8 (first bad byte at offset {offset})")]
    NotUtf8 {
        /// The playlist that could not be read.
        path: String,
        /// Byte offset of the first invalid sequence, for a diagnostic that can
        /// actually be chased.
        offset: usize,
    },
}

/// What a parse yields: the lines, and the three file-level properties that have
/// to be reproduced.
#[derive(Debug)]
pub(super) struct Parsed {
    pub(super) entries: Vec<Entry>,
    pub(super) line_ending: LineEnding,
    pub(super) trailing_newline: bool,
    pub(super) bom: bool,
}

/// The UTF-8 byte-order mark, as it appears in a file some editor has saved.
const BOM: &str = "\u{feff}";

/// Parse `bytes` as the playlist at `path`, which is used for the error message
/// only.
pub(super) fn parse(path: &Utf8Path, bytes: &[u8]) -> Result<Parsed, ParseError> {
    let text = std::str::from_utf8(bytes).map_err(|err| ParseError::NotUtf8 {
        path: path.to_string(),
        offset: err.valid_up_to(),
    })?;

    let (text, bom) = match text.strip_prefix(BOM) {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    let line_ending = line_ending_of(text);
    let trailing_newline = text.ends_with('\n');
    let entries = split_lines(text, line_ending)
        .map(classify)
        .collect::<Vec<_>>();

    Ok(Parsed {
        entries,
        line_ending,
        trailing_newline,
        bom,
    })
}

/// The file's line ending: `Crlf` only if it has terminators and they are all
/// `\r\n`.
///
/// Deciding this by counting, rather than from the first terminator, is what
/// keeps a mixed file byte-exact: the odd `\r` then belongs to its line's text
/// instead of to a terminator the writer would re-emit in the wrong places.
fn line_ending_of(text: &str) -> LineEnding {
    let newlines = text.matches('\n').count();
    if newlines > 0 && newlines == text.matches("\r\n").count() {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

/// The file's lines, each without its terminator.
///
/// A terminated line in a CRLF file gives up its `\r`, because the writer puts
/// one back. An **unterminated** final line is taken verbatim, `\r` and all,
/// because the writer will not — `Windows.m3u` ends this way.
fn split_lines(text: &str, line_ending: LineEnding) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match rest.find('\n') {
            Some(index) => {
                let line = &rest[..index];
                rest = &rest[index + 1..];
                Some(match line_ending {
                    LineEnding::Crlf => line.strip_suffix('\r').unwrap_or(line),
                    LineEnding::Lf => line,
                })
            }
            None => {
                let line = rest;
                rest = "";
                Some(line)
            }
        }
    })
}

/// Read one line. See the table in the [module docs][self].
///
/// `pub(super)` for `rewrite`, which turns a [`LineEdit`][super::rewrite::LineEdit]'s
/// replacement text back into a typed [`Entry`] — so a rewritten line is read by
/// exactly the code that read the line it replaces.
pub(super) fn classify(line: &str) -> Entry {
    if line.is_empty() {
        return Entry::Blank;
    }
    // Spaces and tabs are a legal file name, but a line made of nothing else is
    // an accident, not a track: reading it as one would put a path of spaces in
    // the index and have `doctor` report it as a missing file for ever. It is
    // preserved rather than trimmed away, like every other line MPDFM does not
    // claim to understand.
    if line.trim().is_empty() {
        return Entry::Unparsed(line.to_owned());
    }
    // A control byte is never interpreted: an interior `\r` makes a line look
    // like a path whose name has a byte in it that MPD's own file never had, and
    // a NUL is not a thing a filesystem accepts. Both are preserved verbatim.
    if line.contains(['\r', '\0']) {
        return Entry::Unparsed(line.to_owned());
    }
    if line == EXTM3U {
        return Entry::ExtM3u;
    }
    if let Some(ext_inf) = ext_inf(line) {
        return ext_inf;
    }
    if line.starts_with('#') {
        return Entry::Comment(line.to_owned());
    }
    if is_url(line) {
        return Entry::Url(line.to_owned());
    }
    track(line).unwrap_or_else(|| Entry::Unparsed(line.to_owned()))
}

/// `#EXTINF:<integer>,<title>`, or `None` if it is not that — in which case the
/// caller makes it a comment.
fn ext_inf(line: &str) -> Option<Entry> {
    let (duration, title) = line.strip_prefix("#EXTINF:")?.split_once(',')?;
    // `trim` is for reading the number only; `raw` is what gets written, so the
    // whitespace is not lost.
    let duration = duration.trim().parse::<i64>().ok()?;
    Some(Entry::ExtInf {
        duration,
        title: title.to_owned(),
        raw: line.to_owned(),
    })
}

/// Whether the line begins with a URL scheme.
///
/// `<alpha><alphanumeric|+-.>*://` — RFC 3986's scheme, plus the `//` that every
/// stream URL in a playlist has. MPDFM cares only that this is *not a path*; the
/// rest of the line is never looked at, and never rewritten.
fn is_url(line: &str) -> bool {
    let Some(end) = line.find("://") else {
        return false;
    };
    let scheme = &line[..end];
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// A track line, plain or CUE, or `None` if the line is not a path MPDFM can
/// name.
fn track(line: &str) -> Option<Entry> {
    if let Some((sheet, cue)) = split_cue(line)
        && let Ok(rel) = RelPath::parse(sheet)
    {
        return Some(Entry::Track {
            rel,
            cue: Some(cue.to_owned()),
            raw: line.to_owned(),
        });
    }
    // Falling through rather than giving up matters for a line like
    // `/abs/a.flac.cue/track1`: the CUE split is right about its shape and wrong
    // about it being a track, and the answer is `Unparsed`, not a bad `RelPath`.
    let rel = RelPath::parse(line).ok()?;
    Some(Entry::Track {
        rel,
        cue: None,
        raw: line.to_owned(),
    })
}

/// Split `album.flac.cue/track0017` into the sheet and the virtual track id.
///
/// The rule (task 06): a component ending in `.cue`, case-insensitively,
/// followed by exactly one more component. So `a.cue/track1/extra` is not one —
/// it is an ordinary path that happens to have a `.cue` directory in it — and
/// neither is the sheet on its own.
fn split_cue(line: &str) -> Option<(&str, &str)> {
    let (sheet, cue) = line.rsplit_once('/')?;
    if cue.is_empty() {
        return None;
    }
    let name = sheet.rsplit('/').next().unwrap_or(sheet);
    // `get` rather than a slice: the last four *bytes* of a name ending in
    // `ノスタルジア` are not a character boundary, and indexing there would panic.
    let has_cue_suffix = name.len() > 4
        && name
            .get(name.len() - 4..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(".cue"));
    has_cue_suffix.then_some((sheet, cue))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a playlist body, with the path a test never cares about.
    fn parse_str(text: &str) -> Parsed {
        parse(Utf8Path::new("Test.m3u"), text.as_bytes()).expect("valid UTF-8")
    }

    fn entries(text: &str) -> Vec<Entry> {
        parse_str(text).entries
    }

    /// Classify a single line. Terminated, so that `one("")` is the blank line
    /// rather than an empty file.
    fn one(line: &str) -> Entry {
        let mut entries = entries(&format!("{line}\n"));
        assert_eq!(entries.len(), 1, "{line:?} should be one line");
        entries.remove(0)
    }

    fn track_of(line: &str) -> (String, Option<String>) {
        match one(line) {
            Entry::Track { rel, cue, raw } => {
                assert_eq!(raw, line, "a track's raw line is the line");
                (rel.as_str().to_owned(), cue)
            }
            other => panic!("{line:?} should be a track, was {other:?}"),
        }
    }

    #[test]
    fn every_kind_of_line_is_recognized() {
        assert_eq!(one(""), Entry::Blank);
        assert_eq!(one("#EXTM3U"), Entry::ExtM3u);
        assert_eq!(
            one("# Liquid Drum & Bass"),
            Entry::Comment("# Liquid Drum & Bass".to_owned())
        );
        assert_eq!(one("#"), Entry::Comment("#".to_owned()));
        assert_eq!(
            one("#EXTINF:-1,SomaFM - Groove Salad"),
            Entry::ExtInf {
                duration: -1,
                title: "SomaFM - Groove Salad".to_owned(),
                raw: "#EXTINF:-1,SomaFM - Groove Salad".to_owned(),
            }
        );
        assert_eq!(
            one("http://ice1.somafm.com/groovesalad-256-mp3"),
            Entry::Url("http://ice1.somafm.com/groovesalad-256-mp3".to_owned())
        );
        assert_eq!(
            track_of("coding-music/SwitchAngel/Coding_Trance.mp3"),
            (
                "coding-music/SwitchAngel/Coding_Trance.mp3".to_owned(),
                None
            )
        );
    }

    #[test]
    fn a_header_that_is_not_exactly_extm3u_is_a_comment() {
        // `ExtM3u` is the one variant the writer spells from nothing, so a line
        // that is only nearly `#EXTM3U` must not become it — it would be
        // rewritten into the canonical spelling.
        for line in ["#EXTM3U ", "#extm3u", "#EXTM3Ux"] {
            assert_eq!(
                one(line),
                Entry::Comment(line.to_owned()),
                "{line:?} should be a comment"
            );
        }
        // A leading space makes it a relative path — which is what it is.
        assert_ne!(one(" #EXTM3U"), Entry::ExtM3u);
    }

    #[test]
    fn an_extinf_that_is_not_one_stays_a_comment() {
        for line in [
            "#EXTINF:-1",            // no comma
            "#EXTINF:12.5,Half Way", // not an integer
            "#EXTINF:,No Duration",  // no number at all
            "#EXTINF:1x,Nearly",     // trailing junk
            "#EXTINF 240,Space Not Colon",
        ] {
            assert_eq!(
                one(line),
                Entry::Comment(line.to_owned()),
                "{line:?} should be a comment"
            );
        }
        // Whitespace around the number is read, and kept in `raw`.
        assert_eq!(
            one("#EXTINF: 240 ,Title"),
            Entry::ExtInf {
                duration: 240,
                title: "Title".to_owned(),
                raw: "#EXTINF: 240 ,Title".to_owned(),
            }
        );
        // A comma in the title is part of the title.
        assert_eq!(
            one("#EXTINF:240,Snoop Dogg - Young, Wild & Free"),
            Entry::ExtInf {
                duration: 240,
                title: "Snoop Dogg - Young, Wild & Free".to_owned(),
                raw: "#EXTINF:240,Snoop Dogg - Young, Wild & Free".to_owned(),
            }
        );
    }

    #[test]
    fn a_cue_virtual_track_splits_into_sheet_and_track_id() {
        assert_eq!(
            track_of("pop/Mercury/Acts 1.flac.cue/track0017"),
            (
                "pop/Mercury/Acts 1.flac.cue".to_owned(),
                Some("track0017".to_owned())
            )
        );
        // Case-insensitive, as MPD's own matching is.
        assert_eq!(
            track_of("pop/Mercury/Acts 1.flac.CUE/track0002"),
            (
                "pop/Mercury/Acts 1.flac.CUE".to_owned(),
                Some("track0002".to_owned())
            )
        );
        // The sheet on its own is an ordinary file.
        assert_eq!(
            track_of("pop/Mercury/Acts 1.flac.cue"),
            ("pop/Mercury/Acts 1.flac.cue".to_owned(), None)
        );
        // Two components past the `.cue`: not the CUE shape, so it is a path
        // that happens to have a `.cue` directory in it.
        assert_eq!(
            track_of("pop/Mercury/Acts 1.flac.cue/track0017/extra.mp3"),
            (
                "pop/Mercury/Acts 1.flac.cue/track0017/extra.mp3".to_owned(),
                None
            )
        );
        // A component that is *only* `.cue` is a hidden file, not a sheet with
        // an empty name — but either reading round-trips, so this only pins the
        // behaviour down.
        assert_eq!(
            track_of("pop/.cue/track1"),
            ("pop/.cue/track1".to_owned(), None)
        );
    }

    #[test]
    fn a_line_that_is_not_a_track_identity_is_preserved_untouched() {
        for line in [
            "/home/celsuss/Music/a.mp3",    // absolute
            "./a.mp3",                      // a leading `./` is rejected, not normalized
            "../a.mp3",                     // escapes the music directory
            "a//b.mp3",                     // empty component
            "hiphop/",                      // trailing separator
            "pop\\Imagine Dragons\\01.mp3", // backslashes
            "   ",                          // whitespace only
            "\t",                           // a tab
            "/abs/a.flac.cue/track1",       // CUE-shaped, but absolute
        ] {
            assert_eq!(
                one(line),
                Entry::Unparsed(line.to_owned()),
                "{line:?} should be preserved as-is"
            );
        }
    }

    #[test]
    fn a_relative_path_that_will_never_resolve_is_still_a_track() {
        // MPDFM's job is to say what the line *is*, not whether it works. A
        // `~` is a directory name to MPD, and the broken reference has been in
        // the real library for years. Task 07 reports both; neither is rewritten.
        assert_eq!(
            track_of("~/Music/a.mp3"),
            ("~/Music/a.mp3".to_owned(), None)
        );
        assert_eq!(
            track_of("pop/gone/missing.mp3"),
            ("pop/gone/missing.mp3".to_owned(), None)
        );
        // A trailing space is a legal file name on ext4, and dropping it would
        // point the line at a different file.
        assert_eq!(track_of("pop/a.mp3 "), ("pop/a.mp3 ".to_owned(), None));
    }

    #[test]
    fn any_scheme_is_a_url_and_nothing_else_is() {
        for line in [
            "http://ice.bassdrive.net/stream",
            "https://play.streamafrica.net/lofiradio",
            "file:///home/me/Music/a.mp3",
            "qobuz+v2://track/1234",
            "rtsp://example.org/live.sdp",
        ] {
            assert_eq!(one(line), Entry::Url(line.to_owned()), "{line:?} is a URL");
        }
        // No scheme: these are paths, or nothing.
        assert!(!is_url("pop/a.mp3"));
        assert!(!is_url("://no-scheme"));
        assert!(!is_url("1http://digit-first"));
        assert!(!is_url("C:\\Music\\a.mp3"));
    }

    #[test]
    fn the_line_ending_is_the_file_s_and_a_mixed_file_keeps_its_stray_bytes() {
        let lf = parse_str("a.mp3\nb.mp3\n");
        assert_eq!(lf.line_ending, LineEnding::Lf);
        assert!(lf.trailing_newline);

        let crlf = parse_str("a.mp3\r\nb.mp3\r\n");
        assert_eq!(crlf.line_ending, LineEnding::Crlf);
        assert!(crlf.trailing_newline);
        // The `\r`s belong to the terminators, so the lines are clean tracks.
        assert_eq!(crlf.entries.iter().filter_map(Entry::rel).count(), 2);

        // One CRLF line among LF ones: the file is LF, and that line keeps its
        // `\r` — as an `Unparsed`, so nothing invents a path with a control
        // byte in it.
        let mixed = parse_str("a.mp3\nb.mp3\r\nc.mp3\n");
        assert_eq!(mixed.line_ending, LineEnding::Lf);
        assert_eq!(
            mixed.entries[1],
            Entry::Unparsed("b.mp3\r".to_owned()),
            "the stray carriage return is kept"
        );
    }

    #[test]
    fn a_file_with_no_trailing_newline_says_so() {
        let parsed = parse_str("a.mp3\nb.mp3");
        assert!(!parsed.trailing_newline);
        assert_eq!(parsed.entries.len(), 2);

        // A CRLF file's unterminated last line is taken verbatim, `\r` and all,
        // because the writer adds no terminator after it.
        let parsed = parse_str("a.mp3\r\nb.mp3\r");
        assert_eq!(parsed.line_ending, LineEnding::Crlf);
        assert!(!parsed.trailing_newline);
        assert_eq!(parsed.entries[1], Entry::Unparsed("b.mp3\r".to_owned()));
    }

    #[test]
    fn blank_lines_are_counted_and_not_merged() {
        let parsed = parse_str("a.mp3\n\n\nb.mp3\n");
        assert_eq!(parsed.entries.len(), 4);
        assert_eq!(parsed.entries[1], Entry::Blank);
        assert_eq!(parsed.entries[2], Entry::Blank);
    }

    #[test]
    fn an_empty_file_has_no_lines_at_all() {
        let parsed = parse_str("");
        assert!(parsed.entries.is_empty());
        assert!(!parsed.trailing_newline);
        assert!(!parsed.bom);
        assert_eq!(parsed.line_ending, LineEnding::Lf);

        // One newline is one blank line, not an empty file.
        let parsed = parse_str("\n");
        assert_eq!(parsed.entries, vec![Entry::Blank]);
        assert!(parsed.trailing_newline);
    }

    #[test]
    fn a_bom_is_recorded_and_not_part_of_the_first_line() {
        let parsed = parse_str("\u{feff}#EXTM3U\na.mp3\n");
        assert!(parsed.bom);
        assert_eq!(parsed.entries[0], Entry::ExtM3u);

        // A BOM is only a BOM at the start of the file; anywhere else it is a
        // character in a line, and stays there.
        let parsed = parse_str("a.mp3\n\u{feff}b.mp3\n");
        assert!(!parsed.bom);
        assert_eq!(parsed.entries[1].line(), "\u{feff}b.mp3");
    }

    #[test]
    fn bytes_that_are_not_utf8_are_reported_with_an_offset() {
        let bytes = b"pop/ok.mp3\npop/\xff\xfe.mp3\n";
        let err = parse(Utf8Path::new("Bad.m3u"), bytes).expect_err("invalid UTF-8");
        assert_eq!(
            err,
            ParseError::NotUtf8 {
                path: "Bad.m3u".to_owned(),
                offset: 15,
            }
        );
        assert_eq!(
            err.to_string(),
            "Bad.m3u: not valid UTF-8 (first bad byte at offset 15)"
        );
    }

    #[test]
    fn a_nul_byte_in_a_line_is_preserved_and_never_a_path() {
        let parsed = parse(Utf8Path::new("Nul.m3u"), b"pop/a\0b.mp3\n").expect("valid UTF-8");
        assert_eq!(
            parsed.entries,
            vec![Entry::Unparsed("pop/a\0b.mp3".to_owned())]
        );
    }

    /// The invariant task 09 leans on: a track's bytes can be rebuilt from its
    /// parts, because `RelPath` renders back exactly what it parsed.
    #[test]
    fn a_parsed_track_s_raw_line_is_its_parts() {
        for line in [
            "coding-music/SwitchAngel/Coding_Trance.mp3",
            "electronic/KREAM - So Hï [c0D2h71bFFI]/03 ノスタルジア.mp3",
            "hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag/01 Beef Rap.mp3",
            "pop/Mercury/Acts 1.flac.cue/track0017",
            "pop/a.mp3 ",
            "single.mp3",
        ] {
            let entry = one(line);
            let Entry::Track { rel, cue, raw } = &entry else {
                panic!("{line:?} should be a track, was {entry:?}");
            };
            assert_eq!(raw, line);
            assert_eq!(&Entry::track(rel.clone(), cue.clone()), &entry);
        }
    }
}
