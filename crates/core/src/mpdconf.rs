//! A reader for MPD's own configuration file format.
//!
//! MPDFM has to agree with MPD about where the library is, and the only way to
//! be sure is to read the same file MPD reads, the way MPD reads it. The format
//! is small but has three shapes that a naive `key = value` split gets wrong:
//!
//! ```text
//! music_directory       "~/Music"     # quoted value, then a comment
//! port                  6600          # unquoted value
//! audio_output {                      # a block …
//!         type    "pipewire"          # … whose keys are NOT top-level keys
//!         name    "PipeWire Sound Server"
//! }
//! ```
//!
//! That last one is the trap. `audio_output` blocks have `name` and `type` keys
//! of their own, and a parser that flattened them would happily report a
//! `path "/tmp/mpd.fifo"` from a `fifo` output as though it were a top-level
//! setting. [`MpdConf::parse`] tracks brace depth and keeps block contents out
//! of the top level entirely.
//!
//! # Fidelity over forgiveness
//!
//! Where this parser could be more permissive than MPD, it is not. An unquoted
//! value stops at the first whitespace, exactly as MPD's tokenizer stops — so
//! `music_directory /home/me/My Music` yields `/home/me/My` here and in MPD,
//! with a [`MpdConfWarning::TrailingGarbage`] to say so. Being cleverer than MPD
//! would mean MPDFM and MPD disagreeing about which directory the library is in,
//! which is the one disagreement this tool cannot afford.
//!
//! Nothing here expands `~`: MPD does that itself, and so does
//! [`crate::config`], which knows what `~` means. This module only tokenizes.

/// The top-level `key value` pairs of an mpd.conf, in file order.
///
/// Values inside `block { … }` sections are dropped rather than recorded —
/// MPDFM has no use for them, and keeping them would invite exactly the
/// confusion the block handling exists to prevent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MpdConf {
    /// In file order, so [`MpdConf::get`] can return the first of a repeated
    /// key the way MPD's own "first wins" lookups do.
    entries: Vec<(String, String)>,
}

/// Something surprising in an mpd.conf. None of these stop the parse: a
/// malformed line is skipped and the rest of the file is still read, because a
/// typo in an `audio_output` block is no reason for MPDFM to forget where the
/// music is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MpdConfWarning {
    /// A `"` was opened and never closed before the end of the line.
    #[error("line {line}: unterminated quote")]
    UnterminatedQuote {
        /// 1-based line number.
        line: usize,
    },

    /// A key with nothing after it.
    #[error("line {line}: `{key}` has no value")]
    MissingValue {
        /// 1-based line number.
        line: usize,
        /// The key that was left dangling.
        key: String,
    },

    /// Text after a complete value. MPD ignores it; it usually means a value
    /// with spaces was left unquoted, in which case the value being used is a
    /// truncation of what the user meant.
    #[error("line {line}: ignoring `{rest}` after the value of `{key}`")]
    TrailingGarbage {
        /// 1-based line number.
        line: usize,
        /// The key whose value was cut short.
        key: String,
        /// What was dropped.
        rest: String,
    },

    /// A `}` with no open block.
    #[error("line {line}: `}}` without a matching `{{`")]
    UnmatchedBrace {
        /// 1-based line number.
        line: usize,
    },

    /// End of file inside a block.
    #[error("block `{name}` opened on line {line} is never closed")]
    UnclosedBlock {
        /// 1-based line number of the opening brace.
        line: usize,
        /// The block's name, e.g. `audio_output`.
        name: String,
    },

    /// An `include` or `include_optional` directive. MPDFM does not follow
    /// these, so any setting they would have provided is invisible to it.
    #[error("line {line}: `{directive} {target}` is not followed; settings in it are not seen")]
    Include {
        /// 1-based line number.
        line: usize,
        /// `include` or `include_optional`.
        directive: String,
        /// The file that would have been pulled in.
        target: String,
    },
}

impl MpdConf {
    /// Read an mpd.conf, collecting warnings rather than failing.
    ///
    /// ```
    /// use mpdfm_core::mpdconf::MpdConf;
    ///
    /// let (conf, warnings) = MpdConf::parse(
    ///     r#"
    ///     music_directory "~/Music"    # where the music is
    ///     audio_output {
    ///             type "fifo"
    ///             path "/tmp/mpd.fifo"
    ///     }
    ///     port 6600
    ///     "#,
    /// );
    ///
    /// assert_eq!(conf.get("music_directory"), Some("~/Music"));
    /// assert_eq!(conf.get("port"), Some("6600"));
    /// // The block's keys did not leak into the top level:
    /// assert_eq!(conf.get("path"), None);
    /// assert_eq!(conf.get("type"), None);
    /// assert!(warnings.is_empty());
    /// ```
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<MpdConfWarning>) {
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
        // The stack, not a counter: reporting *which* block was left open at EOF
        // needs its name and line, and nested blocks (`audio_output` holding a
        // `filter` sub-block) need each level remembered.
        let mut blocks: Vec<(usize, String)> = Vec::new();

        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let (code, unterminated) = strip_comment(raw);
            if unterminated {
                warnings.push(MpdConfWarning::UnterminatedQuote { line });
                continue;
            }
            let code = code.trim();
            if code.is_empty() {
                continue;
            }

            if code == "}" {
                if blocks.pop().is_none() {
                    warnings.push(MpdConfWarning::UnmatchedBrace { line });
                }
                continue;
            }

            let (key, rest) = split_key(code);

            // A block opens with `name {`. Inside one, that is all we look for:
            // the contents belong to an audio output or a decoder plugin, never
            // to MPDFM.
            if rest == "{" {
                blocks.push((line, key.to_owned()));
                continue;
            }
            if !blocks.is_empty() {
                continue;
            }

            if rest.is_empty() {
                warnings.push(MpdConfWarning::MissingValue {
                    line,
                    key: key.to_owned(),
                });
                continue;
            }

            let Some((value, after)) = take_value(rest) else {
                warnings.push(MpdConfWarning::UnterminatedQuote { line });
                continue;
            };
            if !after.is_empty() {
                warnings.push(MpdConfWarning::TrailingGarbage {
                    line,
                    key: key.to_owned(),
                    rest: after.to_owned(),
                });
            }

            if key == "include" || key == "include_optional" {
                warnings.push(MpdConfWarning::Include {
                    line,
                    directive: key.to_owned(),
                    target: value,
                });
                continue;
            }

            entries.push((key.to_owned(), value));
        }

        for (line, name) in blocks {
            warnings.push(MpdConfWarning::UnclosedBlock { line, name });
        }

        (Self { entries }, warnings)
    }

    /// The first value for `key`, or `None`.
    ///
    /// First rather than last: MPD's settings are read into slots that reject a
    /// second assignment, so the earliest occurrence is the one in force.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Every value for `key`, in file order. `bind_to_address` is legitimately
    /// repeated — MPD listens on all of them.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.entries
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Split a line at the `#` that starts its comment, ignoring `#` inside a
/// quoted value. The flag is set when the line ended mid-quote.
///
/// Scanning bytes is safe for arbitrary UTF-8: every byte of a multi-byte
/// sequence is `>= 0x80`, so none of them can be mistaken for `"`, `\` or `#`,
/// and the only index ever sliced at is the position of an ASCII `#`.
fn strip_comment(line: &str) -> (&str, bool) {
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => quoted = !quoted,
            // Skip whatever follows a backslash so that `\"` does not close the
            // quote — the same escape `take_value` honours.
            b'\\' if quoted => i += 1,
            b'#' if !quoted => return (&line[..i], false),
            _ => {}
        }
        i += 1;
    }
    (line, quoted)
}

/// Split a trimmed line into its key and the remainder.
fn split_key(code: &str) -> (&str, &str) {
    match code.find(char::is_whitespace) {
        Some(at) => (&code[..at], code[at..].trim_start()),
        None => (code, ""),
    }
}

/// Take one value off the front of `rest`, returning it and what follows.
///
/// `None` when a quoted value is never closed. A quoted value honours `\\` and
/// `\"`; an unquoted one runs to the first whitespace, as MPD's tokenizer does.
fn take_value(rest: &str) -> Option<(String, &str)> {
    let Some(body) = rest.strip_prefix('"') else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        return Some((rest[..end].to_owned(), rest[end..].trim()));
    };

    let mut value = String::new();
    let mut chars = body.char_indices();
    while let Some((at, ch)) = chars.next() {
        match ch {
            '"' => return Some((value, body[at + 1..].trim())),
            '\\' => match chars.next() {
                Some((_, escaped)) => value.push(escaped),
                // A trailing backslash: the quote can no longer be closed.
                None => return None,
            },
            _ => value.push(ch),
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// This machine's real `~/.config/mpd/mpd.conf`, verbatim. Copied in rather
    /// than read from `$HOME`, so the test is hermetic and still proves the
    /// parser handles the file it was written for (`docs/PLAN.md` §3).
    pub(crate) const REAL_USER_CONF: &str = r#"# ~/.config/mpd/mpd.conf

# Hardware / System
db_file            "~/.config/mpd/database"
log_file           "syslog"
# Storing the PID file in /tmp avoids permission errors
pid_file           "/tmp/mpd.pid"
state_file         "~/.config/mpd/state"

# Music Directory
# Ensure this matches where your music actually lives
music_directory       "~/Music"
playlist_directory    "~/.config/mpd/playlists"

# Audio Output (PipeWire Native)
audio_output {
        type            "pipewire"
        name            "PipeWire Sound Server"
}

# Used for cava in rmpc
audio_output {
    type                    "fifo"
    name                    "visualizer"
    path                    "/tmp/mpd.fifo"
    format                  "44100:16:2"
}

# Networking
# Binding to 127.0.0.1 prevents other computers from accessing your music
bind_to_address "127.0.0.1"
port "6600"
"#;

    /// This machine's real `/etc/mpd.conf`, verbatim — the file that must *not*
    /// win, since it names a different library.
    pub(crate) const REAL_SYSTEM_CONF: &str = r#"# See: /usr/share/doc/mpd/mpdconf.example

pid_file "/run/mpd/mpd.pid"
db_file "/var/lib/mpd/mpd.db"
state_file "/var/lib/mpd/mpdstate"
playlist_directory "/var/lib/mpd/playlists"
"#;

    fn parse(text: &str) -> MpdConf {
        let (conf, warnings) = MpdConf::parse(text);
        assert_eq!(warnings, [], "unexpected warnings");
        conf
    }

    #[test]
    fn reads_the_real_user_config() {
        let conf = parse(REAL_USER_CONF);
        assert_eq!(conf.get("music_directory"), Some("~/Music"));
        assert_eq!(
            conf.get("playlist_directory"),
            Some("~/.config/mpd/playlists")
        );
        assert_eq!(conf.get("state_file"), Some("~/.config/mpd/state"));
        assert_eq!(conf.get("bind_to_address"), Some("127.0.0.1"));
        assert_eq!(conf.get("port"), Some("6600"));
    }

    #[test]
    fn audio_output_blocks_do_not_leak_keys() {
        let conf = parse(REAL_USER_CONF);
        // Both blocks define `type` and `name`; the fifo one also defines
        // `path` and `format`. None of them is a top-level setting.
        for leaked in ["type", "name", "path", "format"] {
            assert_eq!(conf.get(leaked), None, "`{leaked}` leaked out of a block");
        }
        // `db_file` sits above the blocks, `port` below: the parser resumed.
        assert_eq!(conf.get("db_file"), Some("~/.config/mpd/database"));
        assert_eq!(conf.get("port"), Some("6600"));
    }

    #[test]
    fn reads_the_real_system_config() {
        let conf = parse(REAL_SYSTEM_CONF);
        // No `music_directory` at all — the system file is partial, which is
        // why the search must not merge files together.
        assert_eq!(conf.get("music_directory"), None);
        assert_eq!(
            conf.get("playlist_directory"),
            Some("/var/lib/mpd/playlists")
        );
        assert_eq!(conf.get("state_file"), Some("/var/lib/mpd/mpdstate"));
    }

    #[test]
    fn nested_blocks_are_skipped_to_the_right_depth() {
        let conf = parse(
            r#"
            music_directory "/music"
            audio_output {
                type "alsa"
                filter {
                    plugin "normalize"
                    music_directory "/wrong"
                }
                mixer_type "software"
            }
            port 6601
            "#,
        );
        assert_eq!(conf.get("music_directory"), Some("/music"));
        assert_eq!(conf.get("plugin"), None);
        assert_eq!(conf.get("mixer_type"), None);
        assert_eq!(conf.get("port"), Some("6601"));
    }

    #[test]
    fn handles_unquoted_values_and_comments() {
        let conf = parse(
            r#"
            # leading comment
            port 6600
            music_directory /srv/music   # trailing comment
            log_level    verbose
            "#,
        );
        assert_eq!(conf.get("port"), Some("6600"));
        assert_eq!(conf.get("music_directory"), Some("/srv/music"));
        assert_eq!(conf.get("log_level"), Some("verbose"));
    }

    #[test]
    fn a_hash_inside_quotes_is_not_a_comment() {
        let conf = parse(r#"music_directory "/srv/music #1 (the good one)""#);
        assert_eq!(
            conf.get("music_directory"),
            Some("/srv/music #1 (the good one)")
        );
    }

    #[test]
    fn honours_escapes_inside_a_quoted_value() {
        let conf = parse(r#"music_directory "/srv/say \"hi\"\\here""#);
        assert_eq!(conf.get("music_directory"), Some(r#"/srv/say "hi"\here"#));
    }

    #[test]
    fn repeated_keys_keep_file_order_and_get_returns_the_first() {
        let conf = parse(
            r#"
            bind_to_address "/run/mpd/socket"
            bind_to_address "127.0.0.1"
            "#,
        );
        assert_eq!(conf.get("bind_to_address"), Some("/run/mpd/socket"));
        assert_eq!(
            conf.get_all("bind_to_address").collect::<Vec<_>>(),
            ["/run/mpd/socket", "127.0.0.1"]
        );
    }

    #[test]
    fn an_unquoted_value_stops_where_mpds_own_tokenizer_stops() {
        let (conf, warnings) = MpdConf::parse("music_directory /srv/My Music");
        assert_eq!(conf.get("music_directory"), Some("/srv/My"));
        assert_eq!(
            warnings,
            [MpdConfWarning::TrailingGarbage {
                line: 1,
                key: "music_directory".into(),
                rest: "Music".into(),
            }]
        );
    }

    #[test]
    fn malformed_lines_warn_without_losing_the_rest_of_the_file() {
        let (conf, warnings) = MpdConf::parse(concat!(
            "music_directory \"/unterminated\n",
            "playlist_directory\n",
            "}\n",
            "port 6600\n",
        ));
        assert_eq!(conf.get("port"), Some("6600"));
        assert_eq!(
            warnings,
            [
                MpdConfWarning::UnterminatedQuote { line: 1 },
                MpdConfWarning::MissingValue {
                    line: 2,
                    key: "playlist_directory".into()
                },
                MpdConfWarning::UnmatchedBrace { line: 3 },
            ]
        );
    }

    #[test]
    fn an_unclosed_block_is_reported_with_its_name() {
        let (conf, warnings) = MpdConf::parse("audio_output {\n    type \"alsa\"\n");
        assert_eq!(conf.get("type"), None);
        assert_eq!(
            warnings,
            [MpdConfWarning::UnclosedBlock {
                line: 1,
                name: "audio_output".into()
            }]
        );
    }

    #[test]
    fn include_is_reported_rather_than_silently_ignored() {
        let (conf, warnings) = MpdConf::parse("include \"outputs.conf\"\nport 6600\n");
        assert_eq!(conf.get("include"), None);
        assert_eq!(conf.get("port"), Some("6600"));
        assert_eq!(
            warnings,
            [MpdConfWarning::Include {
                line: 1,
                directive: "include".into(),
                target: "outputs.conf".into(),
            }]
        );
    }

    #[test]
    fn an_empty_file_is_not_an_error() {
        let (conf, warnings) = MpdConf::parse("");
        assert_eq!(conf, MpdConf::default());
        assert_eq!(warnings, []);
    }

    #[test]
    fn non_ascii_values_survive_the_byte_scan() {
        let conf = parse("music_directory \"/srv/Musik/Tänd Ljusen ノスタルジア\"");
        assert_eq!(
            conf.get("music_directory"),
            Some("/srv/Musik/Tänd Ljusen ノスタルジア")
        );
    }
}
