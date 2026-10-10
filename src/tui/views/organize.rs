//! The organize view: a template line, and where the first files would go.
//!
//! ```text
//! ┌ organize · 42 files ──────────────────────────────────────────────────┐
//! │ template {genre}/{albumartist}/{year} - {album}/{track:02} {title}    │
//! │                                                                       │
//! │ first 20 of 42                                                        │
//! │ → Hip-Hop/MF DOOM/2004 - Mm..Food/01 Beef Rap.mp3                     │
//! │ → Hip-Hop/MF DOOM/2004 - Mm..Food/02 Hoe Cakes.mp3                    │
//! │ ✗ 01.Smokin' On.mp3 stays: missing genre                              │
//! └ enter stage · esc cancel ─────────────────────────────────────────────┘
//! ```
//!
//! # Live, and cheap enough to be
//!
//! Every keystroke re-parses the template and re-renders the first
//! [`PREVIEW`] files against it. That is [`Template::render`] twenty times —
//! pure string work on tags already in memory — so it can sit behind a
//! keypress without a worker. A template that does not parse shows the parser's
//! message under the line with a `^` at the byte it is about, and the last good
//! preview is cleared rather than left up looking current.
//!
//! The **whole** mapping — collisions, aux files, split albums — is only run
//! on `enter`, because it is a pass over every selected file and the library,
//! and its answer is a plan for the pending view rather than something to
//! watch change letter by letter.
//!
//! # The tags arrive later
//!
//! The view opens before they are read. A whole-library organize is 2 800 tag
//! reads, which is seconds, so a worker reads them (`tui::work`) and the view
//! says what it is waiting for. The template can be typed in the meantime; the
//! preview fills in when the answer lands.
//!
//! A file whose tags cannot be read is not a reason to refuse the rest, as it
//! is for the tag editor: nothing is being written to it, it simply stays where
//! it is, which is what an unplaceable file does anyway.

use std::collections::BTreeMap;

use mpdfm_core::library::{AlbumDir, DirPath, Library};
use mpdfm_core::organize::{NameRules, RenderContext, Template, TemplateError};
use mpdfm_core::paths::RelPath;
use mpdfm_core::tags::TagSet;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::msg::Reads;
use crate::tui::widgets::input::Input;
use crate::tui::widgets::{fit, fit_end, width};

/// How many files the live preview renders.
pub const PREVIEW: usize = 20;

/// What `template ` takes up in front of the line.
const LABEL: &str = "template ";

/// Where one previewed file would go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Where it is.
    pub from: RelPath,
    /// Where the template puts it, or why it cannot.
    pub to: Result<RelPath, String>,
}

/// The organize view's whole state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Organize {
    /// The audio files it is about, sorted.
    files: Vec<RelPath>,
    /// Their tags, once read. `None` while the worker is out.
    tags: Option<BTreeMap<RelPath, TagSet>>,
    /// The ones that would not read, with why.
    unreadable: Vec<(RelPath, String)>,
    /// What is typed.
    line: Input,
    /// The parser's answer for it.
    parsed: Result<Template, TemplateError>,
    /// The first [`PREVIEW`] files, rendered.
    preview: Vec<Row>,
}

impl Organize {
    /// A view on `files`, waiting for their tags, with `template` on the line.
    #[must_use]
    pub fn opening(files: Vec<RelPath>, template: &str) -> Self {
        Self {
            files,
            tags: None,
            unreadable: Vec::new(),
            line: Input::of(template),
            parsed: Template::parse(template),
            preview: Vec::new(),
        }
    }

    /// The tags have been read.
    pub fn arrived(&mut self, reads: Reads, library: &Library, rules: NameRules) {
        let mut tags = BTreeMap::new();
        for (rel, read) in reads {
            match read {
                Ok(info) => {
                    tags.insert(rel, info.tags);
                }
                Err(err) => self.unreadable.push((rel, err)),
            }
        }
        self.tags = Some(tags);
        self.refresh(library, rules);
    }

    /// Whether the tags are still being read.
    #[cfg(test)]
    #[must_use]
    pub fn is_reading(&self) -> bool {
        self.tags.is_none()
    }

    /// The selection, each with its tags — what `enter` maps. `None` until
    /// they have been read.
    #[must_use]
    pub fn tracks(&self) -> Option<&BTreeMap<RelPath, TagSet>> {
        self.tags.as_ref()
    }

    /// The files whose tags would not read.
    #[must_use]
    pub fn unreadable(&self) -> &[(RelPath, String)] {
        &self.unreadable
    }

    /// The template, if what is typed is one.
    pub fn template(&self) -> Result<&Template, &TemplateError> {
        self.parsed.as_ref()
    }

    /// The rendered preview.
    #[cfg(test)]
    #[must_use]
    pub fn preview(&self) -> &[Row] {
        &self.preview
    }

    // -- the line ----------------------------------------------------------

    /// Type a character, and re-render.
    pub fn insert(&mut self, c: char, library: &Library, rules: NameRules) -> bool {
        self.line.insert(c) && self.refresh(library, rules)
    }

    /// Delete the character before the cursor, and re-render.
    pub fn backspace(&mut self, library: &Library, rules: NameRules) -> bool {
        self.line.backspace() && self.refresh(library, rules)
    }

    /// Empty the line, and re-render.
    pub fn clear(&mut self, library: &Library, rules: NameRules) -> bool {
        self.line.clear() && self.refresh(library, rules)
    }

    /// Cursor left.
    pub fn left(&mut self) -> bool {
        self.line.left()
    }

    /// Cursor right.
    pub fn right(&mut self) -> bool {
        self.line.right()
    }

    /// Re-parse what is typed and re-render the preview. Always `true`: the
    /// line itself changed, whatever the preview did.
    fn refresh(&mut self, library: &Library, rules: NameRules) -> bool {
        self.parsed = Template::parse(self.line.text());
        self.preview.clear();
        let (Ok(template), Some(tags)) = (&self.parsed, &self.tags) else {
            return true;
        };
        let root_len = library.root().as_str().len() + 1;
        for (rel, tags) in tags.iter().take(PREVIEW) {
            let ctx = RenderContext {
                rules,
                in_disc_dir: library
                    .album_dir(&DirPath::of(rel))
                    .is_some_and(AlbumDir::is_disc),
                root_len,
            };
            self.preview.push(Row {
                from: rel.clone(),
                to: template
                    .render(rel, tags, &ctx)
                    .map_err(|reason| reason.to_string()),
            });
        }
        true
    }

    // -- drawing -----------------------------------------------------------

    /// The panel's title.
    #[must_use]
    pub fn title(&self) -> String {
        let plural = if self.files.len() == 1 { "" } else { "s" };
        format!(" organize · {} file{plural} ", self.files.len())
    }

    /// The body, `cells` wide.
    #[must_use]
    pub fn lines(&self, cells: usize) -> Vec<Line<'static>> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let mut lines = Vec::new();

        let field = cells.saturating_sub(LABEL.len());
        let window = self.line.window(field);
        lines.push(Line::from(vec![
            Span::styled(LABEL, dim),
            Span::raw(window.text),
        ]));

        // The error, with a `^` under the byte it is about — or a blank line,
        // so the preview below does not jump as the template becomes valid.
        match &self.parsed {
            Err(err) => {
                let column = self.caret_under(err.at, field);
                let marker = format!("{}^ {}", " ".repeat(LABEL.len() + column), err.kind);
                lines.push(Line::styled(
                    fit(&marker, cells),
                    Style::new().fg(Color::Red),
                ));
            }
            Ok(_) => lines.push(Line::raw("")),
        }

        let Some(tags) = &self.tags else {
            lines.push(Line::styled(
                format!("reading the tags of {} file(s)…", self.files.len()),
                dim,
            ));
            return lines;
        };

        let shown = PREVIEW.min(tags.len());
        let mut heading = format!("first {shown} of {}", tags.len());
        if !self.unreadable.is_empty() {
            heading.push_str(&format!(
                " · {} unreadable, staying where they are",
                self.unreadable.len()
            ));
        }
        lines.push(Line::styled(heading, dim));

        if self.parsed.is_err() {
            return lines;
        }
        for row in &self.preview {
            lines.push(match &row.to {
                Ok(to) if to == &row.from => Line::styled(fit_end(&format!("= {to}"), cells), dim),
                Ok(to) => Line::from(vec![
                    Span::styled("→ ", Style::new().fg(Color::Green)),
                    Span::raw(fit_end(to.as_str(), cells.saturating_sub(2))),
                ]),
                Err(reason) => Line::styled(
                    fit(
                        &format!("✗ {} stays: {reason}", row.from.file_name()),
                        cells,
                    ),
                    Style::new().fg(Color::Yellow),
                ),
            });
        }
        lines
    }

    /// Where the terminal's cursor goes: column and row inside the panel.
    #[must_use]
    pub fn caret(&self, cells: usize) -> (u16, u16) {
        let window = self.line.window(cells.saturating_sub(LABEL.len()));
        let column = LABEL.len() + window.cursor;
        (u16::try_from(column).unwrap_or(u16::MAX), 0)
    }

    /// The column, inside the line's window, that byte `at` of the text sits
    /// at — clamped to the window when it has scrolled out of it.
    fn caret_under(&self, at: usize, field: usize) -> usize {
        let text = self.line.text();
        let at = (0..=at.min(text.len()))
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        let before = width(&text[..at]);
        let cursor_cells = width(&text[..self.line.cursor()]);
        let offset = cursor_cells.saturating_sub(field.saturating_sub(1));
        before.saturating_sub(offset).min(field.saturating_sub(1))
    }

    /// The panel's bottom line.
    #[must_use]
    pub fn footer(&self, stage: Option<String>, cancel: Option<String>) -> String {
        let stage = stage.unwrap_or_else(|| "enter".to_owned());
        let cancel = cancel.unwrap_or_else(|| "esc".to_owned());
        format!(" {stage} stage · {cancel} cancel ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpdfm_core::library::Format;
    use mpdfm_core::tags::{AudioInfo, Values};
    use mpdfm_core::testing::Fixture;

    use crate::tui::msg::TrackInfo;

    fn info() -> AudioInfo {
        AudioInfo {
            duration: std::time::Duration::from_secs(204),
            bitrate: 320,
            sample_rate: 44_100,
            channels: 2,
            format: Format::Mp3,
        }
    }

    fn tagged(album: &str, n: u32) -> TagSet {
        TagSet {
            artist: Values::one("MF DOOM"),
            album: Values::one(album),
            title: Values::one(format!("Track {n}")),
            genre: Values::one("Hip-Hop"),
            date: Values::one("2004"),
            track: Some((n, None)),
            ..TagSet::default()
        }
    }

    fn view(library: &Library) -> Organize {
        let files: Vec<RelPath> = library
            .entries()
            .iter()
            .filter(|e| e.is_audio())
            .map(|e| e.rel.clone())
            .collect();
        let mut view = Organize::opening(files.clone(), "{genre}/{album}/{track:02} {title}");
        let reads: Reads = files
            .into_iter()
            .zip(1..)
            .map(|(rel, n)| {
                let info = TrackInfo {
                    tags: tagged("Mm..Food", n),
                    info: info(),
                };
                (rel, Ok(info))
            })
            .collect();
        view.arrived(reads, library, NameRules::default());
        view
    }

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_preview_follows_the_template_as_it_is_typed() {
        let fx = Fixture::builder()
            .album("hiphop/MF DOOM", &["01 a.mp3", "02 b.mp3"])
            .build();
        let library = Library::scan(fx.music_dir()).unwrap();
        let mut view = view(&library);
        assert_eq!(
            view.preview()[0]
                .to
                .as_ref()
                .map(RelPath::as_str)
                .map_err(String::as_str),
            Ok("Hip-Hop/Mm..Food/01 Track 1.mp3")
        );

        for c in " (x)".chars() {
            view.insert(c, &library, NameRules::default());
        }
        assert_eq!(
            view.preview()[1]
                .to
                .as_ref()
                .map(RelPath::as_str)
                .map_err(String::as_str),
            Ok("Hip-Hop/Mm..Food/02 Track 2 (x).mp3")
        );
    }

    #[test]
    fn an_invalid_template_shows_where_and_clears_the_preview() {
        let fx = Fixture::builder()
            .album("hiphop/MF DOOM", &["01 a.mp3"])
            .build();
        let library = Library::scan(fx.music_dir()).unwrap();
        let mut view = view(&library);
        view.clear(&library, NameRules::default());
        for c in "{genre/x".chars() {
            view.insert(c, &library, NameRules::default());
        }
        assert!(view.template().is_err());
        assert!(view.preview().is_empty(), "no stale preview");

        let shown = text(&view.lines(80));
        assert!(shown.contains("^ unclosed `{`"), "{shown}");
        let caret = shown.lines().nth(1).unwrap();
        assert_eq!(
            caret.find('^'),
            Some(LABEL.len()),
            "under the `{{`: {shown}"
        );
    }

    #[test]
    fn a_file_without_a_needed_tag_says_why_it_stays() {
        let fx = Fixture::builder()
            .album("hiphop/MF DOOM", &["01 a.mp3"])
            .build();
        let library = Library::scan(fx.music_dir()).unwrap();
        let rel = library.entries()[0].rel.clone();
        let mut view = Organize::opening(vec![rel.clone()], "{genre}/{title}");
        view.arrived(
            vec![(
                rel,
                Ok(TrackInfo {
                    tags: TagSet::default(),
                    info: info(),
                }),
            )],
            &library,
            NameRules::default(),
        );
        let shown = text(&view.lines(80));
        assert!(
            shown.contains("✗ 01 a.mp3 stays: missing title, genre"),
            "{shown}"
        );
    }

    #[test]
    fn while_reading_it_says_so() {
        let view = Organize::opening(vec![RelPath::parse("a/01.mp3").unwrap()], "{genre}/{title}");
        assert!(view.is_reading());
        assert!(text(&view.lines(80)).contains("reading the tags of 1 file(s)"));
    }
}
