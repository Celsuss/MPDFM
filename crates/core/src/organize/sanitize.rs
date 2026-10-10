//! Turning a rendered template segment into a name a filesystem will take.
//!
//! Everything here works on **one path segment at a time**. A template is split
//! on `/` when it is parsed, before any tag value is substituted, so a `/` that
//! arrives inside a value — the genre `Hip Hop/Rap` — is just another character
//! of one segment and is replaced like `:` is. A tag can never create a
//! directory level.
//!
//! The rules, in the order they run:
//!
//! 1. `/` and NUL become [`REPLACEMENT`]: no Linux filesystem accepts them.
//! 2. With [`NameRules::portable`] on (the default), so do `: * ? " < > | \`
//!    and the C0 control characters, which FAT and NTFS refuse. These files end
//!    up on phones and USB sticks.
//! 3. Runs of whitespace — tabs and newlines included — collapse to one space.
//!    A newline in a name would also split a playlist line in two.
//! 4. Leading and trailing whitespace and dots are trimmed: a leading dot hides
//!    the file, and a trailing one is silently dropped by Windows.
//! 5. A segment that is empty after all that, or is `.`/`..`, is refused rather
//!    than filled in. Inventing a name is inventing metadata.
//! 6. The segment is cut to [`NAME_MAX`] **bytes** — ext4's limit is in bytes,
//!    so a CJK album name reaches it at about 85 characters — in the middle, on
//!    a character boundary, keeping a file's extension intact.

use std::borrow::Cow;

/// What every refused character is replaced with.
///
/// One character for all of them, so a name is predictable from its tags: there
/// is no table to remember of which character became which.
pub const REPLACEMENT: char = '_';

/// The longest name, in bytes, ext4 (and most Linux filesystems) accept for one
/// path component.
pub const NAME_MAX: usize = 255;

/// The longest absolute path, in bytes, including its terminating NUL — so the
/// path itself is at most one byte less.
pub const PATH_MAX: usize = 4096;

/// What a name is shortened with when it is cut in the middle.
pub const ELLIPSIS: &str = "…";

/// Characters FAT and NTFS refuse in a name, on top of `/` and NUL.
const NOT_PORTABLE: &[char] = &[':', '*', '?', '"', '<', '>', '|', '\\'];

/// How names are made. Built once per organize run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRules {
    /// Replace the characters FAT and NTFS refuse, too. From
    /// [`Config::organize_portable_names`][crate::config::Config::organize_portable_names].
    pub portable: bool,
    /// Lower-case the extension (`.MP3` → `.mp3`). Off by default: the
    /// extension is kept exactly as it is.
    pub lowercase_ext: bool,
}

impl Default for NameRules {
    fn default() -> Self {
        Self {
            portable: true,
            lowercase_ext: false,
        }
    }
}

impl NameRules {
    /// The rules a configuration asks for.
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            portable: config.organize_portable_names,
            ..Self::default()
        }
    }
}

/// Why a rendered segment could not become a name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// Nothing was left after sanitizing — a title of `...`, a genre of spaces.
    #[error("{raw:?} leaves nothing to name a file or directory with")]
    Empty {
        /// The segment as rendered, before sanitizing.
        raw: String,
    },

    /// The segment is `.` or `..`. Unreachable through [`segment`], whose
    /// trimming of dots empties both first, but checked separately so the
    /// guarantee does not rest on an ordering detail.
    #[error("{raw:?} would name the current or parent directory")]
    Dots {
        /// The segment as rendered.
        raw: String,
    },

    /// An extension too long to leave any room for a name in front of it.
    #[error("the extension {ext:?} is too long for a {NAME_MAX}-byte name")]
    ExtensionTooLong {
        /// The extension, without its dot.
        ext: String,
    },
}

/// One directory name: sanitized, checked and cut to [`NAME_MAX`] bytes.
///
/// ```
/// use mpdfm_core::organize::sanitize::{segment, NameRules};
///
/// let rules = NameRules::default();
/// assert_eq!(segment("Hip Hop/Rap", &rules)?, "Hip Hop_Rap");
/// assert_eq!(segment("  ..Mm..Food.  ", &rules)?, "Mm..Food");
/// assert!(segment(" . ", &rules).is_err());
/// # Ok::<(), mpdfm_core::organize::sanitize::NameError>(())
/// ```
pub fn segment(raw: &str, rules: &NameRules) -> Result<String, NameError> {
    let clean = clean(raw, rules)?;
    Ok(truncate_middle(&clean, NAME_MAX).into_owned())
}

/// A file name: `stem` sanitized like a [`segment`], then `.ext` appended and
/// kept whole while the stem is cut to fit [`NAME_MAX`].
///
/// The extension is the source file's, exactly — lower-cased only when
/// [`NameRules::lowercase_ext`] says so — so a container the user named `.MP3`
/// does not quietly change.
pub fn file_name(stem: &str, ext: Option<&str>, rules: &NameRules) -> Result<String, NameError> {
    let stem = clean(stem, rules)?;
    let Some(ext) = ext else {
        return Ok(truncate_middle(&stem, NAME_MAX).into_owned());
    };
    let ext = extension(ext, rules);
    // A stem needs at least one character plus the ellipsis to be cut at all.
    let room = NAME_MAX
        .checked_sub(ext.len() + 1)
        .filter(|room| *room > ELLIPSIS.len())
        .ok_or_else(|| NameError::ExtensionTooLong { ext: ext.clone() })?;
    Ok(format!("{}.{ext}", truncate_middle(&stem, room)))
}

/// The extension as it will be written: unchanged, or ASCII-lower-cased.
///
/// ASCII only, like every other case fold in MPDFM: Unicode case depends on the
/// locale, and an extension is ASCII in practice anyway.
#[must_use]
pub fn extension(ext: &str, rules: &NameRules) -> String {
    if rules.lowercase_ext {
        ext.to_ascii_lowercase()
    } else {
        ext.to_owned()
    }
}

/// Steps 1–5 of the module rules: replace, collapse, trim, refuse. No length
/// limit is applied — see [`truncate_middle`].
pub fn clean(raw: &str, rules: &NameRules) -> Result<String, NameError> {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(if refused(c, rules) { REPLACEMENT } else { c });
    }
    let trimmed = out.trim_matches(|c: char| c == '.' || c == ' ');
    match trimmed {
        "" => Err(NameError::Empty {
            raw: raw.to_owned(),
        }),
        "." | ".." => Err(NameError::Dots {
            raw: raw.to_owned(),
        }),
        _ => Ok(trimmed.to_owned()),
    }
}

/// Whether `c` cannot stay in a name under `rules`. Whitespace never reaches
/// this — it is collapsed instead.
fn refused(c: char, rules: &NameRules) -> bool {
    c == '/' || c == '\0' || (rules.portable && (NOT_PORTABLE.contains(&c) || c.is_control()))
}

/// `name` shortened to at most `max` bytes by cutting out its middle and
/// putting [`ELLIPSIS`] there, on character boundaries so the result is still
/// valid UTF-8. A name that already fits is returned as it is.
///
/// The middle goes because both ends carry the information: the start is the
/// artist and title, the end is the year, the bitrate, the release group.
///
/// ```
/// use mpdfm_core::organize::sanitize::truncate_middle;
///
/// assert_eq!(truncate_middle("short", 255), "short");
/// let cut = truncate_middle("abcdefghij", 7);
/// assert_eq!(cut, "ab…ij");
/// assert!(cut.len() <= 7);
/// ```
#[must_use]
pub fn truncate_middle(name: &str, max: usize) -> Cow<'_, str> {
    if name.len() <= max {
        return Cow::Borrowed(name);
    }
    let Some(budget) = max.checked_sub(ELLIPSIS.len()) else {
        // No room for the ellipsis: keep what prefix fits.
        return Cow::Owned(prefix_within(name, max).to_owned());
    };
    // The head gets the extra byte of an odd budget; the start of a name is the
    // part a reader scans first.
    let head = prefix_within(name, budget - budget / 2);
    let tail = suffix_within(name, budget - head.len());
    // Re-trim at the cut: a space beside the ellipsis is noise.
    let head = head.trim_end_matches(' ');
    let tail = tail.trim_start_matches(' ');
    Cow::Owned(format!("{head}{ELLIPSIS}{tail}"))
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a
/// character boundary.
fn prefix_within(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The longest suffix of `s` that is at most `max` bytes and starts on a
/// character boundary.
fn suffix_within(s: &str, max: usize) -> &str {
    let mut start = s.len() - max.min(s.len());
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORTABLE: NameRules = NameRules {
        portable: true,
        lowercase_ext: false,
    };
    const EXT4: NameRules = NameRules {
        portable: false,
        lowercase_ext: false,
    };

    #[test]
    fn a_slash_in_a_value_is_replaced_never_kept() {
        assert_eq!(segment("Hip Hop/Rap", &PORTABLE).unwrap(), "Hip Hop_Rap");
        assert_eq!(segment("Hip Hop/Rap", &EXT4).unwrap(), "Hip Hop_Rap");
        assert_eq!(segment("AC/DC", &EXT4).unwrap(), "AC_DC");
    }

    #[test]
    fn nul_is_always_replaced() {
        assert_eq!(segment("a\0b", &EXT4).unwrap(), "a_b");
    }

    #[test]
    fn fat_characters_are_replaced_by_default() {
        assert_eq!(
            segment(r#"What? "Why": *now* <a|b> c\d"#, &NameRules::default()).unwrap(),
            "What_ _Why__ _now_ _a_b_ c_d"
        );
    }

    #[test]
    fn fat_replacement_can_be_switched_off() {
        assert_eq!(
            segment(r#"What? "Why": *now* <a|b> c\d"#, &EXT4).unwrap(),
            r#"What? "Why": *now* <a|b> c\d"#
        );
    }

    #[test]
    fn control_characters_are_not_portable() {
        assert_eq!(segment("a\u{7}b", &PORTABLE).unwrap(), "a_b");
        assert_eq!(segment("a\u{7}b", &EXT4).unwrap(), "a\u{7}b");
    }

    #[test]
    fn whitespace_collapses_and_newlines_cannot_survive() {
        assert_eq!(
            segment("  Young,\t Wild \n\n& Free  ", &EXT4).unwrap(),
            "Young, Wild & Free"
        );
    }

    #[test]
    fn leading_and_trailing_dots_are_trimmed_but_inner_ones_kept() {
        assert_eq!(segment(".hidden", &EXT4).unwrap(), "hidden");
        assert_eq!(segment("Vol. 2.", &EXT4).unwrap(), "Vol. 2");
        assert_eq!(
            segment("MF DOOM - Mm..Food (2004)", &EXT4).unwrap(),
            "MF DOOM - Mm..Food (2004)"
        );
        // Dots and spaces interleaved at the edges all go.
        assert_eq!(segment(" . .x. . ", &EXT4).unwrap(), "x");
    }

    #[test]
    fn an_empty_or_dot_segment_is_refused() {
        for raw in ["", "   ", ".", "..", "...", " . . "] {
            assert!(
                matches!(segment(raw, &EXT4), Err(NameError::Empty { .. })),
                "{raw:?}"
            );
        }
        // With portability on, `?` is replaced, so a title of `???` is a name.
        assert_eq!(segment("???", &PORTABLE).unwrap(), "___");
    }

    #[test]
    fn a_300_byte_cjk_album_is_cut_to_255_bytes_of_valid_utf8() {
        // 100 three-byte characters.
        let album: String = "夜明けのスキャット".chars().cycle().take(100).collect();
        assert_eq!(album.len(), 300);

        let cut = segment(&album, &EXT4).unwrap();
        assert!(cut.len() <= NAME_MAX, "{}", cut.len());
        assert!(cut.contains(ELLIPSIS));
        // Both ends survive.
        assert!(cut.starts_with("夜明け"));
        assert!(cut.ends_with(album.chars().last().unwrap()));
        // `String` cannot hold invalid UTF-8, but round-trip through bytes to
        // make the claim explicit.
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
    }

    #[test]
    fn a_300_byte_title_is_cut_in_the_middle_keeping_the_extension() {
        let title: String = "Snoop Dogg & Wiz Khalifa - Mac + Devin ".repeat(8);
        assert!(title.len() > 300);

        let name = file_name(&title, Some("mp3"), &EXT4).unwrap();
        assert!(name.len() <= NAME_MAX, "{}", name.len());
        assert!(name.ends_with(".mp3"), "{name}");
        assert!(name.starts_with("Snoop Dogg & Wiz Khalifa"));
        assert!(name.contains(ELLIPSIS));
    }

    #[test]
    fn a_cut_lands_on_character_boundaries_for_every_length() {
        let name = "KREAM - So Hï ".repeat(30);
        for max in 0..80 {
            let cut = truncate_middle(&name, max);
            assert!(cut.len() <= max, "{max}: {cut:?}");
        }
    }

    #[test]
    fn the_extension_is_kept_exactly_or_lowercased_on_request() {
        assert_eq!(
            file_name("01 So What", Some("FLAC"), &EXT4).unwrap(),
            "01 So What.FLAC"
        );
        let lower = NameRules {
            lowercase_ext: true,
            ..EXT4
        };
        assert_eq!(
            file_name("01 So What", Some("FLAC"), &lower).unwrap(),
            "01 So What.flac"
        );
    }

    #[test]
    fn a_stem_that_sanitizes_to_nothing_refuses_the_file_name() {
        assert!(matches!(
            file_name(" ... ", Some("mp3"), &EXT4),
            Err(NameError::Empty { .. })
        ));
    }

    #[test]
    fn config_switches_portability() {
        let mut config = crate::testing::Fixture::builder().build().config();
        assert!(NameRules::from_config(&config).portable);
        config.organize_portable_names = false;
        assert!(!NameRules::from_config(&config).portable);
    }
}
