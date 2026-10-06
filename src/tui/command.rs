//! Command mode: the `:` line, and the commands it understands.
//!
//! # Why it mirrors the CLI
//!
//! `:move <dst>` does what `mpdfm move` does, `:undo [txid]` what `mpdfm undo`
//! does, and so on down the list. That is the whole design: a user who has learned
//! one has learned the other, and there is no second vocabulary to document. The
//! shapes are deliberately the same as the clap definitions in
//! [`crate::cli`] — same names, same optional argument on `undo`, same
//! `k=v` for a setting.
//!
//! # What this module owns
//!
//! The parser and the line editor, and nothing about what a command *does*.
//! [`parse`] turns text into a [`Command`]; [`App::run_command`][run] decides what
//! happens next, which for most of these is the task that implements them (22 for
//! staging a move, 28 for organize, 29 for the doctor report). A command that
//! parses and then says which task owns it is a command whose grammar is already
//! settled and tested, which is what those tasks need from this one.
//!
//! # Errors are shown, never swallowed
//!
//! A command that does not parse leaves the line open with the reason under it, so
//! the user can edit what they typed rather than retype it. That is why
//! [`CommandLine`] holds an `error` rather than the app posting a toast: the
//! message belongs next to the text that caused it.
//!
//! [run]: super::app::App::run_command

/// A command typed at `:`.
///
/// One variant per entry in the task's list. The arguments are kept as the user
/// typed them: resolving a path against the library is the business of the code
/// that executes it, which already has to do that for the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `:move <dst>` — stage a move of the marked files to `dst`.
    Move {
        /// The destination, relative to the music directory.
        dst: String,
    },
    /// `:organize <template>` — re-file by a path template.
    Organize {
        /// The template, e.g. `{genre}/{albumartist}/{year} - {album}`.
        template: String,
    },
    /// `:undo [txid]` — reverse a committed transaction, the latest by default.
    Undo {
        /// Which transaction, or `None` for the most recent undoable one.
        txid: Option<String>,
    },
    /// `:doctor` — the library and playlist health report.
    Doctor,
    /// `:set <k>=<v>` — change a setting for this session.
    Set {
        /// The setting's name, as `config.toml` spells it.
        key: String,
        /// The value, as it would be written in `config.toml`.
        value: String,
    },
    /// `:q`, and `:q!` which does not ask about staged operations.
    Quit {
        /// Whether to leave without asking.
        force: bool,
    },
}

/// Why a command could not be run.
///
/// Each one is written to be read in the two-thirds of a line under the command
/// being edited, so they are short and they name the fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// Nothing was typed. `:` then `enter` just closes the line.
    #[error("no command")]
    Empty,

    /// A command MPDFM does not have.
    #[error("no command `{name}`; try {}", usage_names())]
    Unknown {
        /// What was typed.
        name: String,
    },

    /// A command that needs an argument and was not given one.
    #[error("{command} needs {what}")]
    MissingArgument {
        /// The command, as the user typed it.
        command: &'static str,
        /// What it wanted, e.g. `a destination`.
        what: &'static str,
    },

    /// A command that takes nothing, or one thing, and was given more.
    #[error("{command} takes {takes}")]
    TooManyArguments {
        /// The command.
        command: &'static str,
        /// What it does take, e.g. `no arguments`.
        takes: &'static str,
    },

    /// `:set` without a `key=value`.
    #[error("set takes key=value, as in `set backup_keep=20`")]
    BadSetting,
}

/// Every command, with the arguments it takes, for the help overlay.
///
/// Written here next to the parser, so that a command cannot be added to one
/// without appearing in the other — the help overlay lists exactly this.
pub const USAGE: &[(&str, &str)] = &[
    ("move <dst>", "stage a move of the marks"),
    ("organize <template>", "re-file by a template"),
    ("undo [txid]", "reverse a committed transaction"),
    ("doctor", "library and playlist health"),
    ("set <k>=<v>", "change a setting for this session"),
    ("q", "quit, asking if ops are pending"),
    ("q!", "quit, discarding pending ops"),
];

/// The command names, for the "no such command" message.
fn usage_names() -> String {
    USAGE
        .iter()
        .map(|(usage, _)| usage.split(['<', ' ']).next().unwrap_or(usage).trim())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read a command line.
///
/// A leading `:` is accepted but not required: the line editor holds what was typed
/// *after* the `:`, and a pasted `:q` should not be a different thing.
///
/// Arguments are taken as the rest of the line rather than split into words,
/// because the one argument `move` and `organize` take is a path or a template and
/// both routinely contain spaces. `undo` is the exception and rejects a second word,
/// since a transaction id has none.
///
/// # Errors
///
/// A [`CommandError`] for anything that cannot be run, which the command line shows
/// under the text that caused it.
pub fn parse(line: &str) -> Result<Command, CommandError> {
    let line = line.trim().strip_prefix(':').unwrap_or(line.trim()).trim();
    let (name, rest) = match line.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (line, ""),
    };

    let argument = |what: &'static str| {
        if rest.is_empty() {
            Err(CommandError::MissingArgument {
                command: name_of(name),
                what,
            })
        } else {
            Ok(rest.to_owned())
        }
    };
    let nothing = |takes: &'static str| {
        if rest.is_empty() {
            Ok(())
        } else {
            Err(CommandError::TooManyArguments {
                command: name_of(name),
                takes,
            })
        }
    };

    match name {
        "" => Err(CommandError::Empty),
        "move" | "mv" => Ok(Command::Move {
            dst: argument("a destination")?,
        }),
        "organize" => Ok(Command::Organize {
            template: argument("a template")?,
        }),
        "undo" => {
            // One word or none: a txid is `20260924T224500Z-a3f1`.
            if rest.split_whitespace().count() > 1 {
                return Err(CommandError::TooManyArguments {
                    command: "undo",
                    takes: "one transaction id",
                });
            }
            Ok(Command::Undo {
                txid: (!rest.is_empty()).then(|| rest.to_owned()),
            })
        }
        "doctor" => {
            nothing("no arguments")?;
            Ok(Command::Doctor)
        }
        "set" => {
            let (key, value) = rest.split_once('=').ok_or(CommandError::BadSetting)?;
            let (key, value) = (key.trim(), value.trim());
            if key.is_empty() || value.is_empty() {
                return Err(CommandError::BadSetting);
            }
            Ok(Command::Set {
                key: key.to_owned(),
                value: value.to_owned(),
            })
        }
        "q" | "quit" => {
            nothing("no arguments")?;
            Ok(Command::Quit { force: false })
        }
        "q!" | "quit!" => {
            nothing("no arguments")?;
            Ok(Command::Quit { force: true })
        }
        other => Err(CommandError::Unknown {
            name: other.to_owned(),
        }),
    }
}

/// The canonical spelling of a command the user may have abbreviated, for a
/// message that names the command rather than the typo.
fn name_of(typed: &str) -> &'static str {
    match typed {
        "move" | "mv" => "move",
        "organize" => "organize",
        "undo" => "undo",
        "doctor" => "doctor",
        "set" => "set",
        _ => "quit",
    }
}

// ---------------------------------------------------------------------------
// The line
// ---------------------------------------------------------------------------

/// The `:` line being typed.
///
/// A deliberately small editor: insert, backspace, move, clear. There is no
/// history and no completion, which task 26 can add — what is here is what
/// `:move hiphop/MF DOOM` needs, including fixing a typo in the middle of it
/// without retyping the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLine {
    text: String,
    /// Where the next character goes, as a byte offset on a character boundary.
    cursor: usize,
    /// The last thing the parser said about this line, shown beneath it.
    error: Option<String>,
}

impl CommandLine {
    /// An empty line.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been typed, without the leading `:`.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The complaint to show under the line, if the last attempt failed.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Where the cursor is, as a byte offset into [`CommandLine::text`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Record why the line could not be run, and leave it open to be edited.
    pub fn fail(&mut self, message: impl std::fmt::Display) {
        self.error = Some(message.to_string());
    }

    /// Type a character.
    ///
    /// Editing clears the error: a message about the text as it was is misleading
    /// once the text has changed.
    pub fn insert(&mut self, c: char) -> bool {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.error = None;
        true
    }

    /// Delete the character before the cursor. Returns whether there was one.
    pub fn backspace(&mut self) -> bool {
        let Some(previous) = self.text[..self.cursor].chars().next_back() else {
            return false;
        };
        self.cursor -= previous.len_utf8();
        self.text.remove(self.cursor);
        self.error = None;
        true
    }

    /// Throw the whole line away. Returns whether there was anything to throw.
    pub fn clear(&mut self) -> bool {
        let had = !self.text.is_empty() || self.error.is_some();
        self.text.clear();
        self.cursor = 0;
        self.error = None;
        had
    }

    /// Cursor one character left. Returns whether it moved.
    pub fn left(&mut self) -> bool {
        match self.text[..self.cursor].chars().next_back() {
            Some(previous) => {
                self.cursor -= previous.len_utf8();
                true
            }
            None => false,
        }
    }

    /// Cursor one character right. Returns whether it moved.
    pub fn right(&mut self) -> bool {
        match self.text[self.cursor..].chars().next() {
            Some(next) => {
                self.cursor += next.len_utf8();
                true
            }
            None => false,
        }
    }

    /// The command this line holds.
    ///
    /// # Errors
    ///
    /// Whatever [`parse`] says, which the caller puts back on the line with
    /// [`CommandLine::fail`].
    pub fn parse(&self) -> Result<Command, CommandError> {
        parse(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parsing -----------------------------------------------------------

    #[test]
    fn every_command_in_the_task_parses() {
        assert_eq!(
            parse("move hiphop/MF DOOM"),
            Ok(Command::Move {
                dst: "hiphop/MF DOOM".to_owned()
            }),
            "the argument is the rest of the line, spaces and all"
        );
        assert_eq!(
            parse("organize {genre}/{albumartist}/{year} - {album}"),
            Ok(Command::Organize {
                template: "{genre}/{albumartist}/{year} - {album}".to_owned()
            })
        );
        assert_eq!(parse("undo"), Ok(Command::Undo { txid: None }));
        assert_eq!(
            parse("undo 20260924T224500Z-a3f1"),
            Ok(Command::Undo {
                txid: Some("20260924T224500Z-a3f1".to_owned())
            })
        );
        assert_eq!(parse("doctor"), Ok(Command::Doctor));
        assert_eq!(
            parse("set backup_keep=20"),
            Ok(Command::Set {
                key: "backup_keep".to_owned(),
                value: "20".to_owned()
            })
        );
        assert_eq!(parse("q"), Ok(Command::Quit { force: false }));
        assert_eq!(parse("q!"), Ok(Command::Quit { force: true }));
    }

    #[test]
    fn a_leading_colon_and_surrounding_space_are_both_accepted() {
        // The line editor holds what comes after the `:`, but a pasted command
        // should not be a different command.
        assert_eq!(parse(":q"), Ok(Command::Quit { force: false }));
        assert_eq!(parse("  :doctor  "), Ok(Command::Doctor));
        assert_eq!(parse("  undo   "), Ok(Command::Undo { txid: None }));
    }

    #[test]
    fn the_long_spellings_of_quit_work_too() {
        assert_eq!(parse("quit"), Ok(Command::Quit { force: false }));
        assert_eq!(parse("quit!"), Ok(Command::Quit { force: true }));
        assert_eq!(
            parse("mv x"),
            Ok(Command::Move {
                dst: "x".to_owned()
            })
        );
    }

    #[test]
    fn a_command_that_cannot_be_run_says_what_is_wrong_with_it() {
        assert_eq!(parse(""), Err(CommandError::Empty));
        assert_eq!(parse("   "), Err(CommandError::Empty));

        let unknown = parse("wibble x").expect_err("there is no such command");
        let message = unknown.to_string();
        assert!(message.contains("no command `wibble`"), "{message}");
        // The message lists the commands there are, so the next attempt can work.
        assert!(message.contains("move"), "{message}");
        assert!(message.contains("doctor"), "{message}");

        assert_eq!(
            parse("move"),
            Err(CommandError::MissingArgument {
                command: "move",
                what: "a destination"
            })
        );
        assert_eq!(
            parse("move").unwrap_err().to_string(),
            "move needs a destination"
        );
        assert_eq!(
            parse("organize"),
            Err(CommandError::MissingArgument {
                command: "organize",
                what: "a template"
            })
        );
        assert_eq!(
            parse("doctor now"),
            Err(CommandError::TooManyArguments {
                command: "doctor",
                takes: "no arguments"
            })
        );
        assert_eq!(
            parse("undo a b"),
            Err(CommandError::TooManyArguments {
                command: "undo",
                takes: "one transaction id"
            })
        );
        assert_eq!(
            parse("q now").unwrap_err().to_string(),
            "quit takes no arguments"
        );

        for bad in ["set", "set backup_keep", "set =20", "set backup_keep="] {
            assert_eq!(parse(bad), Err(CommandError::BadSetting), "{bad}");
        }
    }

    #[test]
    fn a_setting_value_may_contain_an_equals_sign() {
        // `organize_template` has none, but a future setting could, and splitting
        // on the first `=` is the reading that never loses part of a value.
        assert_eq!(
            parse("set organize_template={album}={year}"),
            Ok(Command::Set {
                key: "organize_template".to_owned(),
                value: "{album}={year}".to_owned()
            })
        );
    }

    #[test]
    fn the_usage_table_covers_every_command_the_parser_takes() {
        // The help overlay lists `USAGE`; this is what stops the list and the
        // grammar drifting apart.
        for (usage, help) in USAGE {
            assert!(!help.is_empty(), "{usage} has no description");
            let line = usage
                .replace("<dst>", "x")
                .replace("<template>", "x")
                .replace("[txid]", "")
                .replace("<k>=<v>", "k=v");
            assert!(
                parse(&line).is_ok(),
                "`{usage}` is documented but `{line}` does not parse"
            );
        }
    }

    // -- the line ----------------------------------------------------------

    #[test]
    fn the_line_edits_in_the_middle_as_well_as_at_the_end() {
        let mut line = CommandLine::new();
        for c in "move x".chars() {
            line.insert(c);
        }
        assert_eq!(line.text(), "move x");
        assert_eq!(line.cursor(), 6);

        // Back over the `x` and fix it without retyping the command.
        assert!(line.left());
        line.insert('y');
        assert_eq!(line.text(), "move yx");
        assert!(line.right());
        assert!(!line.right(), "the end of the line is the end");

        assert!(line.backspace());
        assert_eq!(line.text(), "move y");
        assert_eq!(
            line.parse(),
            Ok(Command::Move {
                dst: "y".to_owned()
            })
        );

        assert!(line.clear());
        assert_eq!(line.text(), "");
        assert_eq!(line.cursor(), 0);
        assert!(!line.clear(), "an empty line is already clear");
        assert!(!line.backspace());
        assert!(!line.left());
    }

    #[test]
    fn the_cursor_moves_by_characters_and_not_by_bytes() {
        let mut line = CommandLine::new();
        for c in "move So Hï".chars() {
            line.insert(c);
        }
        // `ï` is two bytes; a byte-wise cursor would split it and panic.
        assert!(line.left());
        assert_eq!(line.cursor(), "move So H".len());
        assert!(line.backspace());
        assert_eq!(line.text(), "move So ï");
    }

    #[test]
    fn an_error_is_shown_until_the_line_changes() {
        let mut line = CommandLine::new();
        for c in "move".chars() {
            line.insert(c);
        }
        let err = line.parse().expect_err("move needs a destination");
        line.fail(err);
        assert_eq!(line.error(), Some("move needs a destination"));

        // Editing invalidates the complaint, because it was about different text.
        line.insert(' ');
        assert_eq!(line.error(), None);
    }
}
