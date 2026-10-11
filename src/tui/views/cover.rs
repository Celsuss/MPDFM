//! The cover view: one directory's loose images and its tracks' embedded covers,
//! and what each track's cover is about to become.
//!
//! ```text
//! ┌ covers · Singles/Artist ─────────────────────────────┬ preview ──────────┐
//! │ images                                               │                   │
//! │ ▸ cover.jpg        image/jpeg 1200×1200, 312.0 kB  2 │      (picture)    │
//! │   Song A.jpg       image/jpeg 600×600, 84.2 kB       │                   │
//! │ tracks · 1 marked                                    │                   │
//! │   ● 01 Song A.mp3  = cover.jpg      → Song A.jpg     │                   │
//! │     02 Song B.mp3  no cover                          │                   │
//! └ s assign · n pair by name · w stage · esc back ──────┴───────────────────┘
//! ```
//!
//! # Drafts, then a stage
//!
//! What a track's front cover should become is a **draft** until `w` stages
//! it, the same way the tag editor's fields are: `s` assigns the image under
//! the cursor to the marked tracks — or to every track in the directory when
//! none is marked — `x` drafts a removal, `n` pairs tracks with images by name,
//! and `h` / `l` on a track cycle its draft by hand. That last one is how a
//! pairing that guessed nothing, or two equally good images, is fixed: nothing
//! is written until the drafts read right.
//!
//! The file operations — writing the embedded cover out (`e`), renaming an
//! image to `cover.<ext>` (`C`), deleting one (`d`) or every other one (`D`) —
//! are staged at once, like the browser's verbs, because there is nothing about
//! them to draft.
//!
//! # The survey arrives later
//!
//! Reading every track's pictures and hashing every image is a few hundred
//! kilobytes per album, so a worker does it (`work::read_covers`) and the view
//! says what it is waiting for in the meantime.

use std::collections::BTreeSet;

use mpdfm_core::art::plan::{Paired, ToPair};
use mpdfm_core::art::{AlbumArt, Loose, human_bytes};
use mpdfm_core::library::DirPath;
use mpdfm_core::paths::RelPath;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::widgets::{fit, pad};

/// What the worker read: the art, and each track's title for pairing by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survey {
    /// The directory's tracks, their pictures, and its loose images.
    pub art: AlbumArt,
    /// Each track's title tag, in `art.tracks` order.
    pub titles: Vec<Option<String>>,
}

/// What one track's front cover is to become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Draft {
    /// What it is now.
    Keep,
    /// This loose image, by its index in the survey.
    Embed(usize),
    /// No front cover.
    Remove,
}

/// What the cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// A loose image, by index.
    Image(usize),
    /// A track, by index.
    Track(usize),
}

/// What `n` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairReport {
    /// Tracks given a draft.
    pub paired: usize,
    /// Tracks no image matched, by file name.
    pub unmatched: Vec<String>,
    /// Tracks more than one image matched, by file name.
    pub ambiguous: Vec<String>,
}

/// Keys for the footer, already resolved from the live keymap. `None` for an
/// action nothing is bound to, which is then not offered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    /// Assign the image under the cursor.
    pub assign: Option<String>,
    /// Pair by name.
    pub pair: Option<String>,
    /// Stage the drafts.
    pub stage: Option<String>,
    /// Leave.
    pub back: Option<String>,
    /// The help, which lists the rest.
    pub help: Option<String>,
}

/// The view's whole state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Covers {
    dir: DirPath,
    survey: Result<Option<Survey>, String>,
    /// Tracks to mark once the survey lands: what was marked in the browser.
    premarked: Vec<RelPath>,
    cursor: usize,
    scroll: usize,
    marks: BTreeSet<usize>,
    drafts: Vec<Draft>,
    downscale: bool,
}

impl Covers {
    /// A view on `dir`, waiting for its survey. Tracks in `premarked` are
    /// marked when it lands.
    #[must_use]
    pub fn opening(dir: DirPath, premarked: Vec<RelPath>) -> Self {
        Self {
            dir,
            survey: Ok(None),
            premarked,
            cursor: 0,
            scroll: 0,
            marks: BTreeSet::new(),
            drafts: Vec::new(),
            downscale: false,
        }
    }

    /// The directory it is about.
    #[must_use]
    pub fn dir(&self) -> &DirPath {
        &self.dir
    }

    /// Start again on a fresh survey — after a commit or a rescan, when what
    /// was read is out of date. The cursor stays where it was, as near as the
    /// new survey allows; marks and drafts go, because they were about the old
    /// one.
    pub fn reloading(&mut self) {
        self.survey = Ok(None);
        self.marks.clear();
        self.drafts.clear();
    }

    /// The directory is gone from the library.
    pub fn gone(&mut self) {
        self.survey = Err(format!("{} is no longer in the library", self.dir));
        self.marks.clear();
        self.drafts.clear();
    }

    /// The worker answered.
    pub fn arrived(&mut self, result: Result<Survey, String>) {
        match result {
            Ok(survey) => {
                self.drafts = vec![Draft::Keep; survey.art.tracks.len()];
                for (at, track) in survey.art.tracks.iter().enumerate() {
                    if self.premarked.contains(&track.rel) {
                        self.marks.insert(at);
                    }
                }
                self.premarked.clear();
                let rows = survey.art.loose.len() + survey.art.tracks.len();
                self.cursor = self.cursor.min(rows.saturating_sub(1));
                self.survey = Ok(Some(survey));
            }
            Err(err) => self.survey = Err(err),
        }
    }

    /// The survey, once it has been read.
    #[must_use]
    pub fn survey(&self) -> Option<&Survey> {
        self.survey.as_ref().ok().and_then(Option::as_ref)
    }

    fn loose(&self) -> &[Loose] {
        self.survey().map_or(&[], |survey| &survey.art.loose)
    }

    fn track_count(&self) -> usize {
        self.survey().map_or(0, |survey| survey.art.tracks.len())
    }

    fn rows(&self) -> usize {
        self.loose().len() + self.track_count()
    }

    // -- the cursor --------------------------------------------------------

    /// What the cursor is on.
    #[must_use]
    pub fn focused(&self) -> Option<Focus> {
        let images = self.loose().len();
        if self.cursor < images {
            Some(Focus::Image(self.cursor))
        } else if self.cursor < self.rows() {
            Some(Focus::Track(self.cursor - images))
        } else {
            None
        }
    }

    /// The library path whose picture the preview shows: the image, or the
    /// track whose embedded cover it is.
    #[must_use]
    pub fn focused_rel(&self) -> Option<RelPath> {
        let survey = self.survey()?;
        match self.focused()? {
            Focus::Image(at) => self.dir.join(&survey.art.loose[at].name).ok(),
            Focus::Track(at) => Some(survey.art.tracks[at].rel.clone()),
        }
    }

    /// Move by `delta` rows, in a list `rows` rows tall.
    pub fn move_cursor(&mut self, delta: isize, rows: usize) -> bool {
        let last = self.rows().saturating_sub(1);
        let to = self.cursor.saturating_add_signed(delta).min(last);
        self.set_cursor(to, rows)
    }

    /// Put the cursor on row `to`, clamped.
    pub fn set_cursor(&mut self, to: usize, rows: usize) -> bool {
        let to = to.min(self.rows().saturating_sub(1));
        let moved = to != self.cursor;
        self.cursor = to;
        self.follow(rows);
        moved
    }

    /// Jump between the images and the tracks.
    pub fn switch_section(&mut self, rows: usize) -> bool {
        let images = self.loose().len();
        let to = match self.focused() {
            Some(Focus::Image(_)) if self.track_count() > 0 => images,
            Some(Focus::Track(_)) if images > 0 => 0,
            _ => return false,
        };
        self.set_cursor(to, rows)
    }

    /// Keep the cursor's line inside the `rows` the list has. A heading sits
    /// above each section, so a list line is a row plus one or two.
    fn follow(&mut self, rows: usize) {
        let line = self.line_of_cursor();
        let rows = rows.max(1);
        if line < self.scroll {
            self.scroll = line.saturating_sub(1);
        } else if line >= self.scroll + rows {
            self.scroll = line + 1 - rows;
        }
    }

    fn line_of_cursor(&self) -> usize {
        // `images` heading, the images, `tracks` heading, the tracks.
        match self.focused() {
            Some(Focus::Image(at)) => 1 + at,
            Some(Focus::Track(at)) => 2 + self.loose().len().max(1) + at,
            None => 0,
        }
    }

    // -- marks -------------------------------------------------------------

    /// Mark or unmark the track under the cursor. Images are not marked: the
    /// one under the cursor is the one the verbs use.
    pub fn toggle_mark(&mut self) -> bool {
        let Some(Focus::Track(at)) = self.focused() else {
            return false;
        };
        if !self.marks.remove(&at) {
            self.marks.insert(at);
        }
        true
    }

    /// Mark every track.
    pub fn mark_all(&mut self) -> bool {
        let before = self.marks.len();
        self.marks.extend(0..self.track_count());
        self.marks.len() != before
    }

    /// Unmark every track.
    pub fn unmark_all(&mut self) -> bool {
        let had = !self.marks.is_empty();
        self.marks.clear();
        had
    }

    /// The tracks a verb is about: the marked ones, or every track in the
    /// directory when none is marked.
    #[must_use]
    pub fn targets(&self) -> Vec<usize> {
        if self.marks.is_empty() {
            (0..self.track_count()).collect()
        } else {
            self.marks.iter().copied().collect()
        }
    }

    // -- drafts ------------------------------------------------------------

    /// Each track's draft, in survey order.
    #[must_use]
    pub fn drafts(&self) -> &[Draft] {
        &self.drafts
    }

    /// Whether any track has a draft that is not [`Draft::Keep`].
    #[must_use]
    pub fn has_drafts(&self) -> bool {
        self.drafts.iter().any(|draft| *draft != Draft::Keep)
    }

    /// Forget every draft — they have been staged.
    pub fn clear_drafts(&mut self) {
        self.drafts
            .iter_mut()
            .for_each(|draft| *draft = Draft::Keep);
        self.marks.clear();
    }

    /// `s`: draft the image under the cursor onto the target tracks.
    ///
    /// # Errors
    ///
    /// The cursor is not on an image, or the image cannot be embedded — the
    /// message says which.
    pub fn assign(&mut self) -> Result<usize, String> {
        let Some(Focus::Image(image)) = self.focused() else {
            return Err("put the cursor on an image to assign it".to_owned());
        };
        let file = &self.loose()[image];
        if !file.image.format.embeddable() {
            return Err(format!(
                "{} is {}, and only JPEG and PNG are embedded",
                file.name,
                if file.is_image() {
                    file.image.format.to_string()
                } else {
                    "not an image".to_owned()
                }
            ));
        }
        let targets = self.targets();
        for &at in &targets {
            self.drafts[at] = Draft::Embed(image);
        }
        Ok(targets.len())
    }

    /// `x`: draft the removal of the target tracks' front covers.
    pub fn draft_removal(&mut self) -> usize {
        let targets = self.targets();
        for &at in &targets {
            self.drafts[at] = Draft::Remove;
        }
        targets.len()
    }

    /// `h` / `l` on a track: step its draft through keep, every embeddable
    /// image, and remove — the hand fix for a pairing that guessed wrong or
    /// not at all.
    pub fn cycle(&mut self, forward: bool) -> bool {
        let Some(Focus::Track(at)) = self.focused() else {
            return false;
        };
        let mut choices = vec![Draft::Keep];
        choices.extend(
            self.loose()
                .iter()
                .enumerate()
                .filter(|(_, file)| file.image.format.embeddable())
                .map(|(image, _)| Draft::Embed(image)),
        );
        choices.push(Draft::Remove);
        let now = choices
            .iter()
            .position(|draft| *draft == self.drafts[at])
            .unwrap_or(0);
        let next = if forward {
            (now + 1) % choices.len()
        } else {
            (now + choices.len() - 1) % choices.len()
        };
        self.drafts[at] = choices[next];
        true
    }

    /// `n`: draft, for each target track, the image whose name goes with it.
    /// Tracks nothing matches, or more than one image matches, keep whatever
    /// draft they had — and are named in the report so they can be fixed by
    /// hand.
    pub fn pair(&mut self) -> PairReport {
        let Some(survey) = self.survey() else {
            return PairReport::default();
        };
        let targets = self.targets();
        let to_pair: Vec<ToPair<'_>> = targets
            .iter()
            .map(|&at| ToPair {
                rel: &survey.art.tracks[at].rel,
                title: survey.titles.get(at).and_then(Option::as_deref),
            })
            .collect();
        let paired = mpdfm_core::art::plan::pair(&to_pair, &survey.art.loose);
        let mut report = PairReport::default();
        let mut drafts = Vec::new();
        for (&at, paired) in targets.iter().zip(paired) {
            let name = survey.art.tracks[at].rel.file_name().to_owned();
            match paired {
                Paired::Image(image) => {
                    drafts.push((at, Draft::Embed(image)));
                    report.paired += 1;
                }
                Paired::NoMatch => report.unmatched.push(name),
                Paired::Ambiguous(_) => report.ambiguous.push(name),
            }
        }
        for (at, draft) in drafts {
            self.drafts[at] = draft;
        }
        report
    }

    /// `z`: whether large images are downscaled before they are embedded.
    pub fn toggle_downscale(&mut self) -> bool {
        self.downscale = !self.downscale;
        self.downscale
    }

    /// Whether `z` is on.
    #[must_use]
    pub fn downscale(&self) -> bool {
        self.downscale
    }

    // -- drawing -----------------------------------------------------------

    /// The pane's title.
    #[must_use]
    pub fn title(&self) -> String {
        let shown = if self.dir.is_root() {
            "/".to_owned()
        } else {
            self.dir.to_string()
        };
        format!(" covers \u{b7} {shown} ")
    }

    /// The bottom line, in the keys the user has.
    #[must_use]
    pub fn footer(&self, hints: &Hints) -> String {
        let hint = |key: &Option<String>, label: &str| key.as_ref().map(|k| format!("{k} {label}"));
        let parts: Vec<String> = [
            hint(&hints.assign, "assign"),
            hint(&hints.pair, "pair by name"),
            self.has_drafts()
                .then(|| hint(&hints.stage, "stage"))
                .flatten(),
            hint(&hints.help, "more"),
            hint(&hints.back, "back"),
        ]
        .into_iter()
        .flatten()
        .collect();
        format!(" {} ", parts.join(" \u{b7} "))
    }

    /// The list, `rows` rows of it from wherever it is scrolled to.
    #[must_use]
    pub fn lines(&self, cells: usize, rows: usize) -> Vec<Line<'static>> {
        let survey = match &self.survey {
            Ok(Some(survey)) => survey,
            Ok(None) => return vec![Line::from(dim("reading the pictures…"))],
            Err(err) => return vec![Line::styled(err.clone(), Style::new().fg(Color::Red))],
        };
        let mut all: Vec<Line<'static>> = Vec::new();
        let loose = &survey.art.loose;
        let tracks = &survey.art.tracks;
        let name_cells = loose
            .iter()
            .map(|file| crate::tui::widgets::width(&file.name))
            .chain(
                tracks
                    .iter()
                    .map(|track| crate::tui::widgets::width(track.rel.file_name()) + 2),
            )
            .max()
            .unwrap_or(0)
            .clamp(8, cells.saturating_sub(4) / 2);

        all.push(heading("images", ""));
        if loose.is_empty() {
            all.push(Line::from(dim("  no image files here")));
        }
        for (at, file) in loose.iter().enumerate() {
            let used = tracks
                .iter()
                .filter(|track| {
                    track
                        .cover()
                        .is_some_and(|c| c.image.hash == file.image.hash)
                })
                .count();
            let what = if file.is_image() {
                file.image.to_string()
            } else {
                format!("not an image ({})", human_bytes(file.image.size))
            };
            let used = match used {
                0 => String::new(),
                n => format!("  in {n} track{}", if n == 1 { "" } else { "s" }),
            };
            let cursor = self.cursor == at;
            let style = if file.image.format.embeddable() {
                Style::new()
            } else {
                Style::new().fg(Color::DarkGray)
            };
            all.push(row(
                cursor,
                vec![
                    Span::styled(pad(&file.name, name_cells), style),
                    Span::raw("  "),
                    Span::styled(what, Style::new().fg(Color::DarkGray)),
                    Span::styled(used, Style::new().fg(Color::Cyan)),
                ],
                cells,
            ));
        }

        let marked = if self.marks.is_empty() {
            String::new()
        } else {
            format!(" \u{b7} {} marked", self.marks.len())
        };
        let downscale = if self.downscale {
            " \u{b7} downscaling large images"
        } else {
            ""
        };
        all.push(heading("tracks", &format!("{marked}{downscale}")));
        if tracks.is_empty() {
            all.push(Line::from(dim("  no audio files directly here")));
        }
        for (at, track) in tracks.iter().enumerate() {
            let now = match (&track.error, track.cover()) {
                (Some(_), _) => Span::styled("unreadable", Style::new().fg(Color::Red)),
                (None, None) => dim("no cover"),
                (None, Some(cover)) => match loose
                    .iter()
                    .find(|file| file.image.hash == cover.image.hash)
                {
                    Some(file) => {
                        Span::styled(format!("= {}", file.name), Style::new().fg(Color::Green))
                    }
                    None => Span::raw(format!("own {}", cover.image.dimensions())),
                },
            };
            let draft = match self.drafts.get(at) {
                Some(Draft::Embed(image)) => Span::styled(
                    format!("  \u{2192} {}", loose[*image].name),
                    Style::new().fg(Color::Yellow),
                ),
                Some(Draft::Remove) => {
                    Span::styled("  \u{2192} remove", Style::new().fg(Color::Red))
                }
                _ => Span::raw(""),
            };
            let mark = if self.marks.contains(&at) {
                Span::styled("\u{25cf} ", Style::new().fg(Color::Magenta))
            } else {
                Span::raw("  ")
            };
            all.push(row(
                self.cursor == loose.len() + at,
                vec![
                    mark,
                    Span::raw(pad(track.rel.file_name(), name_cells.saturating_sub(2))),
                    Span::raw("  "),
                    now,
                    draft,
                ],
                cells,
            ));
        }

        let rows = rows.max(1);
        let start = self.scroll.min(all.len().saturating_sub(rows));
        all.into_iter().skip(start).take(rows).collect()
    }
}

fn dim(text: &str) -> Span<'static> {
    Span::styled(text.to_owned(), Style::new().fg(Color::DarkGray))
}

fn heading(text: &str, rest: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(text.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
        dim(rest),
    ])
}

/// One list row, with the cursor's `▸` and highlight when it is on it.
fn row(cursor: bool, spans: Vec<Span<'static>>, cells: usize) -> Line<'static> {
    let mut all = vec![Span::raw(if cursor { "\u{25b8} " } else { "  " })];
    all.extend(spans);
    let text: String = all.iter().map(|span| span.content.as_ref()).collect();
    if crate::tui::widgets::width(&text) > cells {
        // Too wide to keep the styling cell-accurate: cut the whole row.
        let line = Line::from(fit(&text, cells));
        return if cursor {
            line.style(Style::new().add_modifier(Modifier::REVERSED))
        } else {
            line
        };
    }
    let line = Line::from(all);
    if cursor {
        line.style(Style::new().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use mpdfm_core::art::read::TrackArt;
    use mpdfm_core::art::{Embedded, ImageFacts, ImageFormat, PictureKind};

    use super::*;

    fn facts(format: ImageFormat, hash: u64) -> ImageFacts {
        ImageFacts {
            format,
            width: Some(600),
            height: Some(600),
            size: 1000,
            hash,
        }
    }

    fn loose(name: &str, format: ImageFormat, hash: u64) -> Loose {
        Loose {
            name: name.to_owned(),
            rank: 3,
            image: facts(format, hash),
        }
    }

    fn track(name: &str, cover: Option<u64>) -> TrackArt {
        TrackArt {
            rel: RelPath::parse(&format!("s/{name}")).unwrap(),
            pictures: cover
                .map(|hash| Embedded {
                    kind: PictureKind::FRONT,
                    description: String::new(),
                    declared: None,
                    image: facts(ImageFormat::Jpeg, hash),
                })
                .into_iter()
                .collect(),
            error: None,
        }
    }

    /// Three singles, three images, and a WebP nothing can embed.
    fn singles() -> Covers {
        let mut view = Covers::opening(DirPath::parse("s").unwrap(), Vec::new());
        view.arrived(Ok(Survey {
            art: AlbumArt {
                dir: DirPath::parse("s").unwrap(),
                tracks: vec![
                    track("01 Song A.mp3", Some(1)),
                    track("02 Song B.mp3", None),
                    track("03 Other.mp3", None),
                ],
                loose: vec![
                    loose("Song A.jpg", ImageFormat::Jpeg, 1),
                    loose("Song B.png", ImageFormat::Png, 2),
                    loose("Song C.webp", ImageFormat::Webp, 3),
                ],
                mismatches: Vec::new(),
            },
            titles: vec![None, None, Some("Song C".to_owned())],
        }));
        view
    }

    #[test]
    fn assigning_with_nothing_marked_drafts_every_track() {
        let mut view = singles();
        assert_eq!(view.assign(), Ok(3));
        assert_eq!(view.drafts(), [Draft::Embed(0); 3]);
    }

    #[test]
    fn assigning_with_marks_drafts_only_the_marks() {
        let mut view = singles();
        view.set_cursor(4, 20);
        view.toggle_mark();
        view.set_cursor(1, 20);
        assert_eq!(view.assign(), Ok(1));
        assert_eq!(view.drafts(), [Draft::Keep, Draft::Embed(1), Draft::Keep]);
    }

    #[test]
    fn an_image_that_cannot_be_embedded_is_refused_by_name() {
        let mut view = singles();
        view.set_cursor(2, 20);
        let err = view.assign().unwrap_err();
        assert!(err.contains("Song C.webp"), "{err}");
        assert!(!view.has_drafts());
    }

    #[test]
    fn pairing_leaves_what_it_cannot_match_for_a_hand_fix() {
        let mut view = singles();
        let report = view.pair();
        assert_eq!(report.paired, 2);
        // "Song C" is only a WebP, which is never a candidate.
        assert_eq!(report.unmatched, ["03 Other.mp3"]);
        assert_eq!(
            view.drafts(),
            [Draft::Embed(0), Draft::Embed(1), Draft::Keep]
        );

        // The hand fix: on the third track, step through the choices.
        view.set_cursor(5, 20);
        assert!(view.cycle(true));
        assert_eq!(view.drafts()[2], Draft::Embed(0));
        view.cycle(true);
        assert_eq!(view.drafts()[2], Draft::Embed(1), "the WebP is skipped");
        view.cycle(true);
        assert_eq!(view.drafts()[2], Draft::Remove);
        view.cycle(true);
        assert_eq!(view.drafts()[2], Draft::Keep);
        view.cycle(false);
        assert_eq!(view.drafts()[2], Draft::Remove);
    }

    #[test]
    fn the_list_shows_what_each_track_has_and_will_have() {
        let mut view = singles();
        view.pair();
        let text: Vec<String> = view
            .lines(80, 20)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let text = text.join("\n");
        assert!(text.contains("= Song A.jpg"), "{text}");
        assert!(text.contains("no cover  \u{2192} Song B.png"), "{text}");
        assert!(text.contains("in 1 track"), "{text}");
    }

    #[test]
    fn tab_jumps_between_the_images_and_the_tracks() {
        let mut view = singles();
        assert_eq!(view.focused(), Some(Focus::Image(0)));
        view.switch_section(20);
        assert_eq!(view.focused(), Some(Focus::Track(0)));
        view.switch_section(20);
        assert_eq!(view.focused(), Some(Focus::Image(0)));
    }

    #[test]
    fn browser_marks_in_this_directory_carry_over() {
        let rel = RelPath::parse("s/02 Song B.mp3").unwrap();
        let mut view = Covers::opening(DirPath::parse("s").unwrap(), vec![rel]);
        let survey = singles().survey().cloned().unwrap();
        view.arrived(Ok(survey));
        assert_eq!(view.targets(), [1]);
    }
}
