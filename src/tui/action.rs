//! Everything the user can ask for, named and decoupled from the keys that ask.
//!
//! An [`Action`] is a verb with no arguments and no UI state in it. That is the
//! whole point of the type, and it buys three things:
//!
//! - **a key is data.** [`KeyMap`][super::keys::KeyMap] is a table from a key
//!   sequence to an `Action`, so remapping is editing a table and not editing a
//!   `match`;
//! - **`:` can produce one too.** Command mode parses text into an `Action` (or
//!   into a [`Command`][super::command::Command] when it carries an argument),
//!   which is why `:q` and `q` cannot drift apart;
//! - **a test can produce one.** `app.dispatch(Action::Bottom)` needs no terminal,
//!   no key event and no knowledge of what `G` is bound to.
//!
//! # The names
//!
//! Every action has a `snake_case` name, and that name is the whole of the
//! configuration vocabulary: `keys.toml` says `"ctrl-r" = "rescan"` and nothing
//! else about it. A name that is not in this list is a warning at startup
//! listing the ones that are — see [`KeyWarning`][super::keys::KeyWarning].
//!
//! The name, the enum variant and the one-line help are declared together by the
//! `actions!` macro below, so they cannot drift: adding a variant without a name
//! does not compile, and the help overlay (task 26) reads the same strings.
//!
//! # What is here and what is not
//!
//! This task owns the vocabulary and the dispatch. Most of the verbs below are
//! implemented by tasks 22–25 — marking files, staging a move, the tag editor —
//! and until then [`App::dispatch`][super::app::App::dispatch] answers them with a
//! message naming the task that owns them. That is deliberate: the binding table
//! is complete and tested now, which is what later tasks need from it, and a
//! binding that silently did nothing would be indistinguishable from one that is
//! broken.

/// Declare the action vocabulary once: the variant, the configuration name and
/// the line the help overlay shows.
///
/// Three parallel mappings over three dozen variants is exactly where hand-written
/// code drifts, so none of the three is hand-written. The `match` arms are
/// exhaustive by construction, which means a new action cannot be added without a
/// name and a description.
macro_rules! actions {
    ($( $(#[$meta:meta])* $variant:ident => $name:literal, $help:literal; )*) => {
        /// One thing the user asked for.
        ///
        /// See the [module documentation][self] for why this carries no arguments
        /// and no UI state.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action {
            $( $(#[$meta])* $variant, )*
        }

        impl Action {
            /// Every action, in the order the help overlay lists them.
            ///
            /// The order is the declaration order below, which is grouped the way
            /// a user thinks — move, mark, change, find, chrome — rather than
            /// alphabetically.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )* ];

            /// The name `keys.toml` uses for this action.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self { $( Self::$variant => $name, )* }
            }

            /// The one line the help overlay shows next to the keys.
            #[must_use]
            pub fn help(self) -> &'static str {
                match self { $( Self::$variant => $help, )* }
            }

            /// The action with this name, if there is one.
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                match name { $( $name => Some(Self::$variant), )* _ => None }
            }
        }
    };
}

actions! {
    // -- moving around -----------------------------------------------------

    /// Down one row.
    Down => "down", "move down";
    /// Up one row.
    Up => "up", "move up";
    /// Left: out of a directory, or to the pane on the left.
    ///
    /// In command mode and in the tag editor this is the text cursor, which is
    /// why it is `left` and not `parent`: the direction is the action, and each
    /// view decides what lies that way.
    Left => "left", "left / out";
    /// Right: into a directory, or to the pane on the right.
    Right => "right", "right / in";
    /// The first row.
    Top => "top", "first row";
    /// The last row.
    Bottom => "bottom", "last row";
    /// Down half a screen.
    HalfPageDown => "half_page_down", "half page down";
    /// Up half a screen.
    HalfPageUp => "half_page_up", "half page up";
    /// Enter the selected directory, or open the selected file.
    Open => "open", "enter dir / open";
    /// The parent directory.
    Parent => "parent", "parent dir";
    /// The next pane.
    SwitchPane => "switch_pane", "switch pane";

    // -- marking -----------------------------------------------------------

    /// Mark or unmark the row under the cursor.
    ToggleMark => "toggle_mark", "mark / unmark";
    /// Start a visual range selection.
    VisualSelect => "visual_select", "visual-select range";
    /// Mark every row in the listing.
    MarkAll => "mark_all", "mark all in view";
    /// Unmark everything.
    UnmarkAll => "unmark_all", "unmark all";

    // -- changing things ---------------------------------------------------

    /// Open the tag editor on the marks, or on the row under the cursor.
    EditTags => "edit_tags", "edit tags";
    /// Stage a move of the marks.
    StageMove => "stage_move", "stage a move of the marks";
    /// Rename the row under the cursor.
    Rename => "rename", "rename";
    /// Stage a delete of the marks.
    StageDelete => "stage_delete", "stage a delete";
    /// Take one operation back off the plan.
    ///
    /// The pending view's `d`, and the one action that is not a browser verb: it
    /// needs a staged operation to point at.
    Unstage => "unstage", "unstage this operation";
    /// Re-file the marks by the organize template.
    Organize => "organize", "organize by template";
    /// Show what is staged.
    ShowPending => "show_pending", "show pending ops";
    /// Commit the staged operations.
    Commit => "commit", "commit pending";
    /// Throw the staged operations away.
    DiscardPending => "discard_pending", "discard pending";
    /// Reverse the most recent committed transaction.
    Undo => "undo", "undo last transaction";

    // -- finding things ----------------------------------------------------

    /// Search within the listing.
    Search => "search", "search";
    /// The next match.
    SearchNext => "search_next", "next match";
    /// The previous match.
    SearchPrev => "search_prev", "prev match";
    /// Narrow the listing to what matches.
    Filter => "filter", "filter";

    // -- text entry --------------------------------------------------------

    /// Accept what has been typed: run the command, keep the search position.
    Submit => "submit", "accept";
    /// Delete the character before the cursor.
    DeleteChar => "delete_char", "delete a character";
    /// Clear the whole line.
    ClearLine => "clear_line", "clear the line";

    // -- chrome ------------------------------------------------------------

    /// Close the overlay, clear the filter, dismiss the message.
    Cancel => "cancel", "clear filter / close overlay";
    /// Open the command line.
    CommandMode => "command_mode", "command mode";
    /// Open — or close — the help overlay.
    Help => "help", "help overlay";
    /// Walk the library again.
    Rescan => "rescan", "rescan library";
    /// Leave, asking first if anything is staged.
    Quit => "quit", "quit (warns if ops pending)";
    /// Leave without asking, discarding whatever is staged.
    ///
    /// Not bound by default — `:q!` is how a user reaches it, and the confirmation
    /// prompt is the other — but bindable like anything else.
    ForceQuit => "force_quit", "quit, discarding pending ops";
}

impl Action {
    /// Every action name, for the warning that lists them.
    pub fn names() -> impl Iterator<Item = &'static str> {
        Self::ALL.iter().copied().map(Self::name)
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_action_round_trips_through_its_name() {
        for &action in Action::ALL {
            assert_eq!(
                Action::from_name(action.name()),
                Some(action),
                "{action:?} does not come back from its own name"
            );
        }
    }

    #[test]
    fn the_names_are_unique_and_spelled_the_way_a_config_file_spells_them() {
        let mut seen = HashSet::new();
        for &action in Action::ALL {
            let name = action.name();
            assert!(seen.insert(name), "two actions are called `{name}`");
            assert!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "`{name}` is not snake_case, which is what keys.toml takes"
            );
            assert!(!action.help().is_empty(), "`{name}` has no help line");
        }
    }

    #[test]
    fn nothing_is_called_none_because_that_is_how_a_binding_is_removed() {
        // `"R" = "none"` in keys.toml unbinds `R`. An action by that name would
        // make the sentinel unreachable and the config ambiguous.
        assert_eq!(Action::from_name("none"), None);
    }

    #[test]
    fn an_unknown_name_is_not_an_action() {
        assert_eq!(Action::from_name("rescann"), None);
        assert_eq!(Action::from_name(""), None);
        assert_eq!(Action::from_name("Rescan"), None);
    }
}
