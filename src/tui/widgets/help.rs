//! The help overlay's text, generated from the live keymap.
//!
//! Generated and never written down, which is the acceptance criterion: a
//! binding the user has remapped away cannot be documented here, because this
//! reads the same table the keypress did ([`KeyMap::help`]), and the words next
//! to each key are [`Action::help`][crate::tui::action::Action::help] — the same
//! strings `keys.toml`'s documentation lists.
//!
//! # Layout
//!
//! Every mode has a section, and **the mode the help was opened from comes
//! first**: `?` in the tag editor is a question about the tag editor, and the
//! answer should not be below the browser's twenty bindings. The others follow
//! in [`Mode::ALL`] order, so a user reading the whole thing reads it in the
//! same order every time. Then the `:` commands, from [`command::USAGE`] — the
//! same table the parser is tested against — and last the one key that is not
//! in the keymap at all.
//!
//! The key column is as wide as the widest key in it (up to a cap), measured in
//! cells, so a `ctrl-d` and a `j / down` line their descriptions up.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{pad, width};
use crate::tui::command;
use crate::tui::keys::{KeyMap, Mode};

/// The key column never grows past this; a key list longer than it pushes its
/// own description along rather than everybody's.
const KEYS_MAX: usize = 16;

/// What each mode is, for its section header.
fn about(mode: Mode) -> &'static str {
    match mode {
        Mode::Browser => "the library",
        Mode::Pending => "what is staged, and committing it",
        Mode::TagEdit => "the tag editor",
        Mode::Search => "the / f F line",
        Mode::Command => "the : line",
    }
}

/// The whole help, with `first`'s section first.
#[must_use]
pub fn lines(map: &KeyMap, first: Mode) -> Vec<Line<'static>> {
    let sections = map.help();
    let keys_w = sections
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| width(&row.keys))
        .chain(command::USAGE.iter().map(|(usage, _)| width(usage) + 1))
        .max()
        .unwrap_or(0)
        .min(KEYS_MAX);

    let header = |title: String, about: &str| {
        Line::from(vec![
            Span::styled(
                title,
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" — {about}"), Style::new().fg(Color::DarkGray)),
        ])
    };
    let row = |keys: &str, help: &str| {
        let keys = if width(keys) > keys_w {
            format!("{keys} ")
        } else {
            pad(keys, keys_w + 1)
        };
        Line::from(vec![
            Span::styled(keys, Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(help.to_owned()),
        ])
    };

    let order = std::iter::once(first).chain(Mode::ALL.iter().copied().filter(|&m| m != first));
    let mut out = Vec::new();
    for mode in order {
        let Some(section) = sections.iter().find(|section| section.mode == mode) else {
            continue;
        };
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.push(header(mode.name().to_owned(), about(mode)));
        if section.rows.is_empty() {
            out.push(Line::styled(
                "(nothing bound)",
                Style::new().fg(Color::DarkGray),
            ));
        }
        for binding in &section.rows {
            out.push(row(&binding.keys, binding.action.help()));
        }
    }

    out.push(Line::default());
    out.push(header("commands".to_owned(), "type : then one of these"));
    for (usage, help) in command::USAGE {
        out.push(row(&format!(":{usage}"), help));
    }

    // Not generated, because it is not in the table: see `App::on_key`.
    out.push(Line::default());
    out.push(header(
        "always".to_owned(),
        "not in keys.toml, and cannot be",
    ));
    out.push(row("ctrl-c", "quit (asks once, then leaves)"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn every_mode_has_a_section_and_the_one_it_was_opened_from_is_first() {
        let map = KeyMap::defaults();
        for &first in Mode::ALL {
            let rows = text(&lines(&map, first));
            assert!(
                rows[0].starts_with(first.name()),
                "{first}: the first section is {:?}",
                rows[0]
            );
            for &mode in Mode::ALL {
                assert!(
                    rows.iter()
                        .any(|row| row.starts_with(&format!("{} — ", mode.name()))),
                    "{mode} has no section when opened from {first}"
                );
            }
        }
    }

    #[test]
    fn the_commands_and_ctrl_c_come_last() {
        let rows = text(&lines(&KeyMap::defaults(), Mode::Browser));
        let commands = rows
            .iter()
            .position(|row| row.starts_with("commands"))
            .expect("a commands section");
        for (usage, _) in command::USAGE {
            assert!(
                rows[commands..]
                    .iter()
                    .any(|row| row.starts_with(&format!(":{usage}"))),
                ":{usage} is not listed"
            );
        }
        assert!(rows[commands..].iter().any(|row| row.contains(":messages")));
        assert!(rows.last().is_some_and(|row| row.starts_with("ctrl-c")));
    }

    #[test]
    fn descriptions_line_up_in_one_column() {
        // A binding row is two spans, the padded keys and the description; the
        // first is the same width on every row whose keys fit under the cap.
        let all = lines(&KeyMap::defaults(), Mode::Browser);
        let widths: std::collections::BTreeSet<usize> = all
            .iter()
            .filter(|line| line.spans.len() == 2 && !line.spans[1].content.starts_with(" — "))
            .map(|line| width(&line.spans[0].content))
            .filter(|&w| w <= KEYS_MAX + 1)
            .collect();
        assert_eq!(widths.len(), 1, "more than one key column: {widths:?}");
    }
}
