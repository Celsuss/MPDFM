//! The template language: parsing `{genre}/{albumartist}/{year} - {album}/{track:02} {title}`
//! once, and rendering it against one file's tags.
//!
//! # Syntax
//!
//! A template is `/`-separated segments of literal text and `{fields}`. The
//! source file's extension is always appended to the last segment, so a
//! template names a file without one; a trailing `.{ext}` is accepted and means
//! the same thing.
//!
//! | Field | Value |
//! |---|---|
//! | `artist` | the track artist |
//! | `albumartist` | the album artist, falling back to `artist` |
//! | `album`, `title`, `genre` | as tagged |
//! | `year` | the leading four digits of the date |
//! | `track`, `disc` | the number, without its total |
//! | `ext` | the source file's extension, without the dot |
//! | `original_dir` | the directory the file is in now — a whole segment on its own |
//! | `filename` | the file's current name, without its extension |
//!
//! A multi-valued tag (three `ARTIST` comments in a FLAC) renders joined with
//! `; `, as the editor shows it.
//!
//! | Form | Meaning |
//! |---|---|
//! | `{track:02}` | zero-pad a number to two digits (`year`, `track`, `disc`) |
//! | `{artist:upper}`, `:lower`, `:title` | change case (text fields) |
//! | `{album?}` | when `album` is absent, leave out the **whole segment** |
//! | `{genre\|Unsorted}` | when `genre` is absent, use `Unsorted` |
//! | `{{`, `}}` | a literal brace |
//!
//! A field written plainly is **required**: a file without it is unplaceable,
//! and stays where it is. `?` and `|default` are the explicit opt-ins to place
//! it anyway — MPDFM never invents `Unknown Artist` on its own.
//!
//! # Multi-disc sets
//!
//! A template that never mentions `{disc}` would merge the discs of a set into
//! one directory, and two `01` tracks into one listing. So when a file is one
//! disc of several — its disc tag says so, or it sits in a `CD 1`-style
//! directory — and the template does not place the disc itself, a `Disc N`
//! directory is added in front of the file name. A file that is clearly one
//! disc of several but carries no disc number is unplaceable: which disc it is
//! is exactly what is missing.

use std::fmt;

use super::sanitize::{self, NameError, NameRules, PATH_MAX};
use crate::paths::RelPath;
use crate::tags::TagSet;

/// A value a template can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Token {
    /// `artist`.
    Artist,
    /// `albumartist`, which falls back to `artist`.
    AlbumArtist,
    /// `album`.
    Album,
    /// `title`.
    Title,
    /// `genre`.
    Genre,
    /// `year`.
    Year,
    /// `track`.
    Track,
    /// `disc`.
    Disc,
    /// `ext`.
    Ext,
    /// `original_dir`.
    OriginalDir,
    /// `filename`.
    Filename,
}

impl Token {
    /// Every token, in the order the module documentation lists them.
    pub const ALL: [Token; 11] = [
        Self::Artist,
        Self::AlbumArtist,
        Self::Album,
        Self::Title,
        Self::Genre,
        Self::Year,
        Self::Track,
        Self::Disc,
        Self::Ext,
        Self::OriginalDir,
        Self::Filename,
    ];

    /// The name a template spells it with.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::AlbumArtist => "albumartist",
            Self::Album => "album",
            Self::Title => "title",
            Self::Genre => "genre",
            Self::Year => "year",
            Self::Track => "track",
            Self::Disc => "disc",
            Self::Ext => "ext",
            Self::OriginalDir => "original_dir",
            Self::Filename => "filename",
        }
    }

    /// The token a template name stands for.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|token| token.as_str() == name)
    }

    /// Whether the value is a number, which is what `:02` pads.
    #[must_use]
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Year | Self::Track | Self::Disc)
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a `:modifier` does to a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    /// `:02` — zero-pad to this many digits.
    Pad(usize),
    /// `:upper`.
    Upper,
    /// `:lower`.
    Lower,
    /// `:title` — capitalize each word, lower-case the rest of it.
    Title,
}

/// What happens when a field's tag is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Absent {
    /// `{album}` — the file is unplaceable.
    Required,
    /// `{album?}` — the segment is left out.
    OmitSegment,
    /// `{genre|Unsorted}` — this literal is used instead.
    Default(String),
}

/// One `{…}` in a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// What it asks for.
    pub token: Token,
    /// How the value is reshaped, if at all. Not applied to a default.
    pub modifier: Option<Modifier>,
    /// What happens without a value.
    pub absent: Absent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Field(Field),
}

/// A parsed template, ready to render against any number of files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    source: String,
    /// The last one names the file; the rest name directories.
    segments: Vec<Vec<Part>>,
}

/// Where a template is wrong, and how.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} (at byte {at} of {template:?})")]
pub struct TemplateError {
    /// The template as given.
    pub template: String,
    /// The byte offset the problem starts at, for an inline marker.
    pub at: usize,
    /// What the problem is.
    pub kind: TemplateErrorKind,
}

/// The ways a template can be malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateErrorKind {
    /// Nothing at all.
    #[error("the template is empty")]
    Empty,
    /// Starts with `/`, so it would name something outside the music directory.
    #[error("a template is relative to the music directory and cannot start with `/`")]
    Absolute,
    /// `a//b`, or a trailing `/`.
    #[error("empty path segment")]
    EmptySegment,
    /// A literal `.` or `..` segment, which would climb out of where it is.
    #[error("a `{0}` segment would leave the music directory")]
    DotSegment(String),
    /// A `{` with no `}`.
    #[error("unclosed `{{`")]
    Unclosed,
    /// A `}` with no `{`. Write `}}` for a literal one.
    #[error("unmatched `}}` (write `}}}}` for a literal brace)")]
    StrayBrace,
    /// `{}` or `{|x}`.
    #[error("a field needs a name")]
    NoName,
    /// A name that is not a [`Token`].
    #[error("unknown field `{0}`")]
    UnknownToken(String),
    /// A `:modifier` that is not one of `02`, `upper`, `lower`, `title`.
    #[error("unknown modifier `:{0}`")]
    UnknownModifier(String),
    /// `{title:02}`.
    #[error("`{0}` is not a number and cannot be zero-padded")]
    PadOnText(Token),
    /// `{year:upper}`.
    #[error("`{0}` is a number and has no case")]
    CaseOnNumber(Token),
    /// `{title?}` in the file name, which cannot be left out.
    #[error("`?` would leave out the file name; use `{{{0}|…}}` to give a default")]
    OptionalFileName(Token),
    /// `{original_dir}` with something else in its segment.
    #[error("`original_dir` is a path and must be a whole segment on its own")]
    OriginalDirNotAlone,
    /// `{original_dir}` with a modifier or default.
    #[error("`original_dir` takes no modifier or default")]
    OriginalDirDecorated,
}

impl Template {
    /// Parse a template, rejecting anything that could name a path outside the
    /// music directory.
    ///
    /// ```
    /// use mpdfm_core::organize::Template;
    ///
    /// assert!(Template::parse("{genre}/{albumartist}/{year} - {album}/{track:02} {title}").is_ok());
    /// assert!(Template::parse("../{title}").is_err());
    /// assert!(Template::parse("{title:02}").is_err());
    /// ```
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        let err = |at, kind| TemplateError {
            template: source.to_owned(),
            at,
            kind,
        };
        if source.trim().is_empty() {
            return Err(err(0, TemplateErrorKind::Empty));
        }
        if source.starts_with('/') {
            return Err(err(0, TemplateErrorKind::Absolute));
        }

        let mut segments = Vec::new();
        let mut parts: Vec<Part> = Vec::new();
        let mut literal = String::new();
        let mut segment_start = 0;
        let mut chars = source.char_indices().peekable();

        while let Some((at, c)) = chars.next() {
            match c {
                '{' if chars.peek().is_some_and(|&(_, next)| next == '{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek().is_some_and(|&(_, next)| next == '}') => {
                    chars.next();
                    literal.push('}');
                }
                '}' => return Err(err(at, TemplateErrorKind::StrayBrace)),
                '{' => {
                    let body_start = at + 1;
                    let close = source[body_start..]
                        .find('}')
                        .ok_or_else(|| err(at, TemplateErrorKind::Unclosed))?;
                    let body = &source[body_start..body_start + close];
                    if body.contains('{') {
                        return Err(err(at, TemplateErrorKind::Unclosed));
                    }
                    let field = parse_field(body).map_err(|kind| err(at, kind))?;
                    flush(&mut literal, &mut parts);
                    parts.push(Part::Field(field));
                    while chars.peek().is_some_and(|&(i, _)| i <= body_start + close) {
                        chars.next();
                    }
                }
                '/' => {
                    flush(&mut literal, &mut parts);
                    segments.push(
                        check_segment(std::mem::take(&mut parts), segment_start)
                            .map_err(|(at, kind)| err(at, kind))?,
                    );
                    segment_start = at + 1;
                }
                _ => literal.push(c),
            }
        }
        flush(&mut literal, &mut parts);
        strip_trailing_ext(&mut parts);
        let last = check_segment(parts, segment_start).map_err(|(at, kind)| err(at, kind))?;
        if let Some(token) = last.iter().find_map(|part| match part {
            Part::Field(f) if f.absent == Absent::OmitSegment => Some(f.token),
            _ => None,
        }) {
            return Err(err(
                segment_start,
                TemplateErrorKind::OptionalFileName(token),
            ));
        }
        if last
            .iter()
            .any(|part| matches!(part, Part::Field(f) if f.token == Token::OriginalDir))
        {
            return Err(err(segment_start, TemplateErrorKind::OriginalDirNotAlone));
        }
        segments.push(last);

        Ok(Self {
            source: source.to_owned(),
            segments,
        })
    }

    /// The template as it was written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether the template mentions `token` anywhere.
    #[must_use]
    pub fn uses(&self, token: Token) -> bool {
        self.fields().any(|field| field.token == token)
    }

    /// Every field, in template order.
    pub fn fields(&self) -> impl Iterator<Item = &Field> {
        self.segments
            .iter()
            .flatten()
            .filter_map(|part| match part {
                Part::Field(field) => Some(field),
                Part::Literal(_) => None,
            })
    }

    /// Where `rel`, tagged `tags`, belongs under this template.
    ///
    /// Pure: no tag or filesystem I/O. `ctx` carries what the file's tags
    /// cannot say — whether it sits in a disc directory, and how long the music
    /// directory's own path is.
    ///
    /// ```
    /// use mpdfm_core::organize::{RenderContext, Template};
    /// use mpdfm_core::paths::RelPath;
    /// use mpdfm_core::tags::{TagSet, Values};
    ///
    /// let template = Template::parse("{genre|Unsorted}/{albumartist}/{album}/{track:02} {title}")?;
    /// let tags = TagSet {
    ///     artist: Values::one("MF DOOM"),
    ///     album: Values::one("Mm..Food"),
    ///     title: Values::one("Beef Rap"),
    ///     track: Some((1, Some(15))),
    ///     ..TagSet::default()
    /// };
    /// let from = RelPath::parse("hiphop/MF DOOM - Mm..Food (2004)/01 Beef Rap.mp3")?;
    /// let to = template.render(&from, &tags, &RenderContext::default()).unwrap();
    /// assert_eq!(to.as_str(), "Unsorted/MF DOOM/Mm..Food/01 Beef Rap.mp3");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn render(
        &self,
        rel: &RelPath,
        tags: &TagSet,
        ctx: &RenderContext,
    ) -> Result<RelPath, Unplaceable> {
        let values = Values {
            rel,
            tags,
            rules: &ctx.rules,
        };

        // Render every segment to raw text first, collecting every missing field
        // so the user is told all of what to fix at once.
        let mut missing = Vec::new();
        let mut raw: Vec<String> = Vec::new();
        'segments: for parts in &self.segments {
            if let [Part::Field(f)] = parts.as_slice()
                && f.token == Token::OriginalDir
            {
                if let Some(dir) = rel.parent() {
                    raw.extend(dir.components().map(str::to_owned));
                }
                continue;
            }
            let mut text = String::new();
            let mut this_missing = Vec::new();
            for part in parts {
                match part {
                    Part::Literal(s) => text.push_str(s),
                    Part::Field(field) => match values.get(field.token) {
                        Some(value) => text.push_str(&apply(&value, field.modifier)),
                        None => match &field.absent {
                            Absent::Required => this_missing.push(field.token),
                            Absent::OmitSegment => continue 'segments,
                            Absent::Default(default) => text.push_str(default),
                        },
                    },
                }
            }
            missing.extend(this_missing);
            raw.push(text);
        }

        // The last segment rendered is the file name. It cannot have been
        // omitted (`parse` refuses `?` there) or expanded (nor `original_dir`).
        let Some(stem) = raw.pop() else {
            unreachable!("the file-name segment always renders");
        };

        if let Some(disc) = self.disc_segment(tags, ctx) {
            match disc {
                Some(n) => raw.push(format!("Disc {n}")),
                None => missing.push(Token::Disc),
            }
        }

        if !missing.is_empty() {
            missing.sort();
            missing.dedup();
            return Err(Unplaceable::Missing(missing));
        }

        let mut dirs = Vec::with_capacity(raw.len());
        for text in raw {
            dirs.push(sanitize::segment(&text, &ctx.rules).map_err(Unplaceable::Unnamable)?);
        }
        let ext = rel
            .extension()
            .map(|ext| sanitize::extension(ext, &ctx.rules));
        let stem = sanitize::clean(&stem, &ctx.rules).map_err(Unplaceable::Unnamable)?;
        // `file_name` re-cleans, which is idempotent, and applies the byte limit
        // around the extension.
        let name = sanitize::file_name(&stem, ext.as_deref(), &ctx.rules)
            .map_err(Unplaceable::Unnamable)?;

        let path = fit_path(dirs, name, ext.as_deref(), ctx)?;
        // Every segment is already a clean name, so this cannot fail; it runs
        // anyway so `RelPath` stays the one place path validity is decided.
        RelPath::parse(&path).map_err(Unplaceable::Path)
    }

    /// `None` when no disc directory is needed; `Some(Some(n))` for `Disc n`;
    /// `Some(None)` when one is needed but the number is not tagged.
    fn disc_segment(&self, tags: &TagSet, ctx: &RenderContext) -> Option<Option<u32>> {
        if self.uses(Token::Disc) {
            return None;
        }
        let tagged_multi = tags
            .disc
            .is_some_and(|(n, total)| n > 1 || total.is_some_and(|t| t > 1));
        (tagged_multi || ctx.in_disc_dir).then(|| tags.disc.map(|(n, _)| n))
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl std::str::FromStr for Template {
    type Err = TemplateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// What a file's tags cannot say about it, for [`Template::render`].
#[derive(Debug, Clone, Default)]
pub struct RenderContext {
    /// How names are sanitized.
    pub rules: NameRules,
    /// The file is in a directory named like one disc of a set (`CD 1 - …`),
    /// per [`AlbumDir::set_root`][crate::library::AlbumDir::set_root].
    pub in_disc_dir: bool,
    /// The byte length of the music directory's absolute path, so the whole
    /// path can be kept under [`PATH_MAX`]. Zero measures the relative path
    /// alone.
    pub root_len: usize,
}

/// Why a file has no place under a template. It stays where it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unplaceable {
    /// The template needs tags the file does not have. Sorted, deduplicated.
    #[error("missing {}", list(.0))]
    Missing(Vec<Token>),
    /// The tags render to something that is not a name.
    #[error(transparent)]
    Unnamable(NameError),
    /// The rendered path is not a valid [`RelPath`]. Every segment has been
    /// sanitized before this is checked, so this is a bug guard, not a case.
    #[error(transparent)]
    Path(crate::paths::PathError),
    /// The path cannot be brought under [`PATH_MAX`] bytes even with every
    /// segment cut.
    #[error("the path would be {bytes} bytes, over the {PATH_MAX}-byte limit")]
    TooLong {
        /// How long it came out, with the music directory.
        bytes: usize,
    },
}

fn list(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------

/// The values one file offers the template.
struct Values<'a> {
    rel: &'a RelPath,
    tags: &'a TagSet,
    rules: &'a NameRules,
}

/// A rendered value: text, or a number still waiting for its padding.
enum Value {
    Text(String),
    Number(u32),
}

impl Values<'_> {
    fn get(&self, token: Token) -> Option<Value> {
        let text = |values: &crate::tags::Values| {
            let joined = values.joined();
            (!joined.trim().is_empty()).then_some(Value::Text(joined))
        };
        match token {
            Token::Artist => text(&self.tags.artist),
            Token::AlbumArtist => text(&self.tags.album_artist).or_else(|| text(&self.tags.artist)),
            Token::Album => text(&self.tags.album),
            Token::Title => text(&self.tags.title),
            Token::Genre => text(&self.tags.genre),
            Token::Year => self.tags.year().map(Value::Number),
            Token::Track => self.tags.track.map(|(n, _)| Value::Number(n)),
            Token::Disc => self.tags.disc.map(|(n, _)| Value::Number(n)),
            Token::Ext => self
                .rel
                .extension()
                .map(|ext| Value::Text(sanitize::extension(ext, self.rules))),
            // Expanded by `render` before it gets here.
            Token::OriginalDir => None,
            Token::Filename => {
                let name = self.rel.file_name();
                let stem = match self.rel.extension() {
                    Some(ext) => &name[..name.len() - ext.len() - 1],
                    None => name,
                };
                Some(Value::Text(stem.to_owned()))
            }
        }
    }
}

fn apply(value: &Value, modifier: Option<Modifier>) -> String {
    match (value, modifier) {
        (Value::Number(n), Some(Modifier::Pad(width))) => format!("{n:0width$}"),
        (Value::Number(n), _) => n.to_string(),
        (Value::Text(s), Some(Modifier::Upper)) => s.to_uppercase(),
        (Value::Text(s), Some(Modifier::Lower)) => s.to_lowercase(),
        (Value::Text(s), Some(Modifier::Title)) => title_case(s),
        (Value::Text(s), _) => s.clone(),
    }
}

/// Capitalize the first letter of each word and lower-case the rest of it. A
/// word starts at a letter not preceded by a letter, digit or apostrophe, so
/// `smokin' on` is `Smokin' On` and `hip-hop (soundtrack)` is
/// `Hip-Hop (Soundtrack)`.
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_word = false;
    for c in s.chars() {
        if in_word {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        in_word = c.is_alphanumeric() || c == '\'' || c == '’';
    }
    out
}

fn flush(literal: &mut String, parts: &mut Vec<Part>) {
    if !literal.is_empty() {
        parts.push(Part::Literal(std::mem::take(literal)));
    }
}

/// `…{title}.{ext}` → `…{title}`: the extension is appended anyway.
fn strip_trailing_ext(parts: &mut Vec<Part>) {
    let [.., Part::Literal(dot), Part::Field(f)] = parts.as_mut_slice() else {
        return;
    };
    if f.token != Token::Ext || f.modifier.is_some() || f.absent != Absent::Required {
        return;
    }
    if dot.ends_with('.') {
        dot.pop();
        let empty = dot.is_empty();
        parts.pop();
        if empty {
            parts.pop();
        }
    }
}

fn check_segment(parts: Vec<Part>, at: usize) -> Result<Vec<Part>, (usize, TemplateErrorKind)> {
    match parts.as_slice() {
        [] => return Err((at, TemplateErrorKind::EmptySegment)),
        [Part::Literal(s)] if s.trim() == "." || s.trim() == ".." => {
            return Err((at, TemplateErrorKind::DotSegment(s.trim().to_owned())));
        }
        [Part::Literal(s)] if s.trim().is_empty() => {
            return Err((at, TemplateErrorKind::EmptySegment));
        }
        _ => {}
    }
    for part in &parts {
        if let Part::Field(f) = part
            && f.token == Token::OriginalDir
        {
            if parts.len() > 1 {
                return Err((at, TemplateErrorKind::OriginalDirNotAlone));
            }
            if f.modifier.is_some() || f.absent != Absent::Required {
                return Err((at, TemplateErrorKind::OriginalDirDecorated));
            }
        }
    }
    Ok(parts)
}

/// `name[:modifier][?|default]`.
fn parse_field(body: &str) -> Result<Field, TemplateErrorKind> {
    let (head, absent) = match body.split_once('|') {
        Some((head, default)) => (head, Absent::Default(default.to_owned())),
        None => match body.strip_suffix('?') {
            Some(head) => (head, Absent::OmitSegment),
            None => (body, Absent::Required),
        },
    };
    let (name, modifier) = match head.split_once(':') {
        Some((name, modifier)) => (name, Some(modifier)),
        None => (head, None),
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(TemplateErrorKind::NoName);
    }
    let token =
        Token::parse(name).ok_or_else(|| TemplateErrorKind::UnknownToken(name.to_owned()))?;
    let modifier = modifier
        .map(|m| parse_modifier(token, m.trim()))
        .transpose()?;
    Ok(Field {
        token,
        modifier,
        absent,
    })
}

fn parse_modifier(token: Token, text: &str) -> Result<Modifier, TemplateErrorKind> {
    let modifier = match text {
        "upper" => Modifier::Upper,
        "lower" => Modifier::Lower,
        "title" => Modifier::Title,
        digits if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            // `02` and `2` both mean two digits wide.
            let width = digits
                .parse()
                .ok()
                .filter(|w| *w <= 9)
                .ok_or_else(|| TemplateErrorKind::UnknownModifier(text.to_owned()))?;
            Modifier::Pad(width)
        }
        _ => return Err(TemplateErrorKind::UnknownModifier(text.to_owned())),
    };
    match modifier {
        Modifier::Pad(_) if !token.is_numeric() => Err(TemplateErrorKind::PadOnText(token)),
        Modifier::Upper | Modifier::Lower | Modifier::Title if token.is_numeric() => {
            Err(TemplateErrorKind::CaseOnNumber(token))
        }
        _ => Ok(modifier),
    }
}

/// Join `dirs` and `name`, cutting segments until the absolute path fits
/// [`PATH_MAX`]: the file name first, because only it is per-file — cutting an
/// album directory differently for two of its tracks would split the album —
/// then the longest directory, repeatedly.
fn fit_path(
    mut dirs: Vec<String>,
    mut name: String,
    ext: Option<&str>,
    ctx: &RenderContext,
) -> Result<String, Unplaceable> {
    /// No segment is cut shorter than this; past it, the path is refused.
    const FLOOR: usize = 32;
    // The path proper, without its NUL.
    let limit = PATH_MAX - 1;
    let length = |dirs: &[String], name: &str| {
        ctx.root_len + dirs.iter().map(|d| d.len() + 1).sum::<usize>() + name.len()
    };

    let mut excess = length(&dirs, &name).saturating_sub(limit);
    if excess > 0 && name.len() > FLOOR {
        let ext_len = ext.map_or(0, |e| e.len() + 1);
        let stem = &name[..name.len() - ext_len];
        let target = name.len().saturating_sub(excess).max(FLOOR);
        let cut = sanitize::truncate_middle(stem, target.saturating_sub(ext_len)).into_owned();
        name = format!("{cut}{}", &name[name.len() - ext_len..]);
        excess = length(&dirs, &name).saturating_sub(limit);
    }
    while excess > 0 {
        let Some(longest) = dirs
            .iter_mut()
            .filter(|d| d.len() > FLOOR)
            .max_by_key(|d| d.len())
        else {
            return Err(Unplaceable::TooLong {
                bytes: length(&dirs, &name),
            });
        };
        let target = longest.len().saturating_sub(excess).max(FLOOR);
        *longest = sanitize::truncate_middle(longest, target).into_owned();
        excess = length(&dirs, &name).saturating_sub(limit);
    }

    dirs.push(name);
    Ok(dirs.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags::Values as V;
    use crate::testing::names;

    const DEFAULT: &str = crate::config::DEFAULT_ORGANIZE_TEMPLATE;

    fn snoop_tags() -> TagSet {
        TagSet {
            title: V::one("Smokin' On"),
            artist: V::of(["Snoop Dogg", "Wiz Khalifa"]),
            album_artist: V::one("Snoop Dogg & Wiz Khalifa"),
            album: V::one("Mac + Devin Go To High School (Soundtrack)"),
            date: V::one("2011-12-13"),
            track: Some((1, Some(18))),
            genre: V::one("Hip Hop/Rap"),
            ..TagSet::default()
        }
    }

    fn snoop() -> RelPath {
        RelPath::parse(names::SNOOP_TRACK).unwrap()
    }

    fn render(template: &str, tags: &TagSet) -> Result<String, Unplaceable> {
        render_at(template, &snoop(), tags)
    }

    fn render_at(template: &str, rel: &RelPath, tags: &TagSet) -> Result<String, Unplaceable> {
        Template::parse(template)
            .unwrap()
            .render(rel, tags, &RenderContext::default())
            .map(|p| p.to_string())
    }

    fn kind(template: &str) -> TemplateErrorKind {
        Template::parse(template).unwrap_err().kind
    }

    // -- the default template, on the real album that motivated it ----------

    #[test]
    fn the_snoop_soundtrack_lands_somewhere_predictable() {
        assert_eq!(
            render(DEFAULT, &snoop_tags()).unwrap(),
            "Hip Hop_Rap/Snoop Dogg & Wiz Khalifa/2011 - Mac + Devin Go To High School \
             (Soundtrack)/01 Smokin' On.mp3"
        );
    }

    // -- every token ---------------------------------------------------------

    #[test]
    fn artist_joins_every_value() {
        assert_eq!(
            render("{artist}", &snoop_tags()).unwrap(),
            "Snoop Dogg; Wiz Khalifa.mp3"
        );
    }

    #[test]
    fn albumartist_falls_back_to_artist() {
        let mut tags = snoop_tags();
        assert_eq!(
            render("{albumartist}", &tags).unwrap(),
            "Snoop Dogg & Wiz Khalifa.mp3"
        );
        tags.album_artist = V::none();
        assert_eq!(
            render("{albumartist}", &tags).unwrap(),
            "Snoop Dogg; Wiz Khalifa.mp3"
        );
        tags.artist = V::none();
        assert_eq!(
            render("{albumartist}", &tags),
            Err(Unplaceable::Missing(vec![Token::AlbumArtist]))
        );
    }

    #[test]
    fn album_title_and_genre_render_as_tagged() {
        assert_eq!(
            render("{genre}/{album}/{title}", &snoop_tags()).unwrap(),
            "Hip Hop_Rap/Mac + Devin Go To High School (Soundtrack)/Smokin' On.mp3"
        );
    }

    #[test]
    fn year_is_narrowed_from_a_full_date() {
        assert_eq!(render("{year}", &snoop_tags()).unwrap(), "2011.mp3");
    }

    #[test]
    fn track_and_disc_drop_their_totals() {
        let tags = TagSet {
            disc: Some((2, Some(2))),
            ..snoop_tags()
        };
        assert_eq!(render("{disc}-{track}", &tags).unwrap(), "2-1.mp3");
    }

    #[test]
    fn ext_is_appended_automatically_and_usable_as_a_field() {
        assert_eq!(render("{title}", &snoop_tags()).unwrap(), "Smokin' On.mp3");
        // A trailing `.{ext}` is the same template.
        assert_eq!(
            render("{title}.{ext}", &snoop_tags()).unwrap(),
            "Smokin' On.mp3"
        );
        // Anywhere else it is a value like any other.
        assert_eq!(
            render("{ext}/{title}", &snoop_tags()).unwrap(),
            "mp3/Smokin' On.mp3"
        );
    }

    #[test]
    fn original_dir_keeps_the_current_directory_whole() {
        assert_eq!(
            render("{original_dir}/{track:02} {title}", &snoop_tags()).unwrap(),
            format!("{}/01 Smokin' On.mp3", names::SNOOP_ALBUM)
        );
        // A track at the library root has no directory to keep.
        let stray = RelPath::parse("stray.mp3").unwrap();
        assert_eq!(
            render_at("{original_dir}/{title}", &stray, &snoop_tags()).unwrap(),
            "Smokin' On.mp3"
        );
    }

    #[test]
    fn filename_is_the_current_name_without_its_extension() {
        assert_eq!(
            render("{genre}/{filename}", &snoop_tags()).unwrap(),
            "Hip Hop_Rap/01.Smokin' On.mp3"
        );
    }

    // -- every modifier ------------------------------------------------------

    #[test]
    fn zero_padding() {
        assert_eq!(render("{track:02}", &snoop_tags()).unwrap(), "01.mp3");
        assert_eq!(render("{track:3}", &snoop_tags()).unwrap(), "001.mp3");
        // Padding never truncates.
        let tags = TagSet {
            track: Some((117, None)),
            ..snoop_tags()
        };
        assert_eq!(render("{track:02}", &tags).unwrap(), "117.mp3");
    }

    #[test]
    fn upper_lower_and_title_case() {
        let tags = TagSet {
            artist: V::one("snoop dogg & WIZ khalifa"),
            title: V::one("smokin' on (hip-hop mix)"),
            ..snoop_tags()
        };
        assert_eq!(
            render("{artist:upper}", &tags).unwrap(),
            "SNOOP DOGG & WIZ KHALIFA.mp3"
        );
        assert_eq!(
            render("{artist:lower}", &tags).unwrap(),
            "snoop dogg & wiz khalifa.mp3"
        );
        assert_eq!(
            render("{artist:title} - {title:title}", &tags).unwrap(),
            "Snoop Dogg & Wiz Khalifa - Smokin' On (Hip-Hop Mix).mp3"
        );
        // Unicode-aware, not ASCII-only.
        let kream = TagSet {
            title: V::one("so hï"),
            ..TagSet::default()
        };
        assert_eq!(render("{title:upper}", &kream).unwrap(), "SO HÏ.mp3");
    }

    #[test]
    fn optional_field_omits_its_whole_segment() {
        let mut tags = snoop_tags();
        tags.genre = V::none();
        assert_eq!(
            render("{genre?}/{year} - {album?}/{title}", &tags).unwrap(),
            "2011 - Mac + Devin Go To High School (Soundtrack)/Smokin' On.mp3"
        );
        tags.album = V::none();
        assert_eq!(
            render("{genre?}/{year} - {album?}/{title}", &tags).unwrap(),
            "Smokin' On.mp3"
        );
        // Present, it is an ordinary field.
        assert_eq!(
            render("{album?}/{title}", &snoop_tags()).unwrap(),
            "Mac + Devin Go To High School (Soundtrack)/Smokin' On.mp3"
        );
    }

    #[test]
    fn literal_default_is_the_explicit_opt_in() {
        let mut tags = snoop_tags();
        tags.genre = V::none();
        assert_eq!(
            render("{genre|Unsorted}/{title}", &tags).unwrap(),
            "Unsorted/Smokin' On.mp3"
        );
        // Not used when the tag is there.
        assert_eq!(
            render("{genre|Unsorted}/{title}", &snoop_tags()).unwrap(),
            "Hip Hop_Rap/Smokin' On.mp3"
        );
        // A default is a literal: no modifier, but still sanitized.
        assert_eq!(
            render("{genre:upper|Not/Sorted: yet}/{title}", &tags).unwrap(),
            "Not_Sorted_ yet/Smokin' On.mp3"
        );
    }

    #[test]
    fn a_blank_tag_counts_as_absent() {
        let tags = TagSet {
            genre: V::one("   "),
            ..snoop_tags()
        };
        assert_eq!(
            render("{genre|Unsorted}/{title}", &tags).unwrap(),
            "Unsorted/Smokin' On.mp3"
        );
    }

    #[test]
    fn braces_can_be_written_literally() {
        assert_eq!(
            render("{{{year}}} {title}", &snoop_tags()).unwrap(),
            "{2011} Smokin' On.mp3"
        );
    }

    // -- missing tags --------------------------------------------------------

    #[test]
    fn a_missing_album_is_unplaceable_and_says_so() {
        let mut tags = snoop_tags();
        tags.album = V::none();
        assert_eq!(
            render(DEFAULT, &tags),
            Err(Unplaceable::Missing(vec![Token::Album]))
        );
    }

    #[test]
    fn every_missing_field_is_reported_at_once() {
        let tags = TagSet {
            title: V::one("Smokin' On"),
            ..TagSet::default()
        };
        assert_eq!(
            render(DEFAULT, &tags).unwrap_err().to_string(),
            "missing albumartist, album, genre, year, track"
        );
    }

    // -- sanitization through the template -----------------------------------

    #[test]
    fn a_slash_in_a_tag_never_creates_a_directory_level() {
        let rendered = render("{genre}/{title}", &snoop_tags()).unwrap();
        assert_eq!(rendered.matches('/').count(), 1, "{rendered}");
    }

    #[test]
    fn portable_replacement_can_be_switched_off() {
        let tags = TagSet {
            title: V::one(r#"What's "Good"? Part 1: *Intro*"#),
            ..snoop_tags()
        };
        let template = Template::parse("{title}").unwrap();
        let portable = template
            .render(&snoop(), &tags, &RenderContext::default())
            .unwrap();
        assert_eq!(portable.as_str(), "What's _Good__ Part 1_ _Intro_.mp3");

        let ext4 = RenderContext {
            rules: NameRules {
                portable: false,
                ..NameRules::default()
            },
            ..RenderContext::default()
        };
        let kept = template.render(&snoop(), &tags, &ext4).unwrap();
        assert_eq!(kept.as_str(), r#"What's "Good"? Part 1: *Intro*.mp3"#);
    }

    #[test]
    fn a_300_byte_album_name_is_cut_and_the_extension_kept() {
        let tags = TagSet {
            album: V::one("夜明けのスキャット".repeat(12)),
            title: V::one("Smokin' On ".repeat(30)),
            ..snoop_tags()
        };
        assert!(tags.album.joined().len() >= 300);
        let path = render("{album}/{title}", &tags).unwrap();
        let (dir, name) = path.split_once('/').unwrap();
        assert!(dir.len() <= 255, "{}", dir.len());
        assert!(name.len() <= 255, "{}", name.len());
        assert!(name.ends_with(".mp3"));
    }

    #[test]
    fn a_title_that_sanitizes_to_nothing_is_unplaceable() {
        let tags = TagSet {
            title: V::one(" ... "),
            ..snoop_tags()
        };
        assert!(matches!(
            render("{title}", &tags),
            Err(Unplaceable::Unnamable(NameError::Empty { .. }))
        ));
    }

    #[test]
    fn the_whole_path_is_kept_under_path_max() {
        // Twenty 200-byte directory levels: 4 000 bytes before the root.
        let template = std::iter::repeat_n("{album}", 20)
            .chain(["{title}"])
            .collect::<Vec<_>>()
            .join("/");
        let tags = TagSet {
            album: V::one("a".repeat(200)),
            title: V::one("t".repeat(200)),
            ..TagSet::default()
        };
        let ctx = RenderContext {
            root_len: "/home/celsuss/Music/".len(),
            ..RenderContext::default()
        };
        let path = Template::parse(&template)
            .unwrap()
            .render(&snoop(), &tags, &ctx)
            .unwrap();
        assert!(ctx.root_len + path.as_str().len() < PATH_MAX);
        assert!(path.as_str().ends_with(".mp3"));
        assert!(path.components().all(|c| c.len() <= 255));
    }

    #[test]
    fn a_path_that_cannot_fit_is_refused() {
        let template = std::iter::repeat_n("{album}", 200)
            .chain(["{title}"])
            .collect::<Vec<_>>()
            .join("/");
        let tags = TagSet {
            album: V::one("a".repeat(40)),
            title: V::one("t"),
            ..TagSet::default()
        };
        assert!(matches!(
            render(&template, &tags),
            Err(Unplaceable::TooLong { .. })
        ));
    }

    // -- multi-disc ----------------------------------------------------------

    #[test]
    fn a_disc_of_a_set_gets_a_disc_directory() {
        let rel = RelPath::parse(names::MERCURY_TRACK).unwrap();
        let tags = TagSet {
            title: V::one("Wrecked"),
            album: V::one("Mercury - Acts 1 & 2"),
            artist: V::one("Imagine Dragons"),
            track: Some((1, Some(13))),
            disc: Some((1, Some(2))),
            ..TagSet::default()
        };
        assert_eq!(
            render_at("{albumartist}/{album}/{track:02} {title}", &rel, &tags).unwrap(),
            "Imagine Dragons/Mercury - Acts 1 & 2/Disc 1/01 Wrecked.mp3"
        );
        // A template that places the disc itself is left alone.
        assert_eq!(
            render_at(
                "{albumartist}/{album}/{disc}-{track:02} {title}",
                &rel,
                &tags
            )
            .unwrap(),
            "Imagine Dragons/Mercury - Acts 1 & 2/1-01 Wrecked.mp3"
        );
        // A single-disc album is not given one.
        let single = TagSet {
            disc: Some((1, Some(1))),
            ..tags
        };
        assert_eq!(
            render_at("{album}/{title}", &rel, &single).unwrap(),
            "Mercury - Acts 1 & 2/Wrecked.mp3"
        );
    }

    #[test]
    fn a_disc_directory_without_a_disc_tag_is_unplaceable() {
        let rel = RelPath::parse(names::MERCURY_TRACK).unwrap();
        let tags = TagSet {
            title: V::one("Wrecked"),
            album: V::one("Mercury - Acts 1 & 2"),
            ..TagSet::default()
        };
        let ctx = RenderContext {
            in_disc_dir: true,
            ..RenderContext::default()
        };
        let template = Template::parse("{album}/{title}").unwrap();
        assert_eq!(
            template.render(&rel, &tags, &ctx),
            Err(Unplaceable::Missing(vec![Token::Disc]))
        );
    }

    // -- malformed templates -------------------------------------------------

    #[test]
    fn a_template_that_would_leave_the_music_directory_is_rejected() {
        assert_eq!(kind("/{genre}/{title}"), TemplateErrorKind::Absolute);
        assert_eq!(
            kind("../{title}"),
            TemplateErrorKind::DotSegment("..".to_owned())
        );
        assert_eq!(
            kind("{genre}/../../{title}"),
            TemplateErrorKind::DotSegment("..".to_owned())
        );
        assert_eq!(
            kind("./{title}"),
            TemplateErrorKind::DotSegment(".".to_owned())
        );
        // A value can never climb either: `..` as a tag sanitizes to nothing.
        let tags = TagSet {
            genre: V::one(".."),
            ..snoop_tags()
        };
        assert!(matches!(
            render("{genre}/{title}", &tags),
            Err(Unplaceable::Unnamable(_))
        ));
    }

    #[test]
    fn malformed_templates_name_the_problem() {
        assert_eq!(kind(""), TemplateErrorKind::Empty);
        assert_eq!(kind("{genre}//{title}"), TemplateErrorKind::EmptySegment);
        assert_eq!(kind("{genre}/"), TemplateErrorKind::EmptySegment);
        assert_eq!(kind("{genre/{title}"), TemplateErrorKind::Unclosed);
        assert_eq!(kind("{genre"), TemplateErrorKind::Unclosed);
        assert_eq!(kind("genre}"), TemplateErrorKind::StrayBrace);
        assert_eq!(kind("{}"), TemplateErrorKind::NoName);
        assert_eq!(
            kind("{composer}"),
            TemplateErrorKind::UnknownToken("composer".to_owned())
        );
        assert_eq!(
            kind("{title:shout}"),
            TemplateErrorKind::UnknownModifier("shout".to_owned())
        );
        assert_eq!(
            kind("{title:02}"),
            TemplateErrorKind::PadOnText(Token::Title)
        );
        assert_eq!(
            kind("{year:upper}"),
            TemplateErrorKind::CaseOnNumber(Token::Year)
        );
        assert_eq!(
            kind("{album}/{title?}"),
            TemplateErrorKind::OptionalFileName(Token::Title)
        );
        assert_eq!(
            kind("x {original_dir}/{title}"),
            TemplateErrorKind::OriginalDirNotAlone
        );
        assert_eq!(
            kind("{original_dir?}/{title}"),
            TemplateErrorKind::OriginalDirDecorated
        );
        assert_eq!(
            kind("{original_dir}"),
            TemplateErrorKind::OriginalDirNotAlone
        );
    }

    #[test]
    fn an_error_points_at_its_field() {
        let err = Template::parse("{genre}/{bogus}").unwrap_err();
        assert_eq!(err.at, 8);
        assert_eq!(
            err.to_string(),
            "unknown field `bogus` (at byte 8 of \"{genre}/{bogus}\")"
        );
    }

    #[test]
    fn the_default_template_parses() {
        let template = Template::parse(DEFAULT).unwrap();
        assert_eq!(template.as_str(), DEFAULT);
        assert!(!template.uses(Token::Disc));
        assert!(template.uses(Token::AlbumArtist));
    }
}
