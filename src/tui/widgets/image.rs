//! Cover art in the details pane — as pixels when the terminal can show them,
//! as a bordered description when it cannot.
//!
//! ```text
//! ┌ cover ───────────────┐      ┌ cover ───────────────┐
//! │      ▒▒▒▒▒▒▒▒▒▒      │      │ front cover          │
//! │      ▒▒▒▒▒▒▒▒▒▒      │      │ image/jpeg 600×600   │
//! │      ▒▒▒▒▒▒▒▒▒▒      │      │ 84.2 kB              │
//! └──────────────────────┘      └──────────────────────┘
//!     kitty, sixel, iTerm2           anything else
//! ```
//!
//! # Detection is conservative
//!
//! A graphics escape sent to a terminal that does not understand it is printed,
//! or half-interpreted, and either way the screen is corrupted until the next full
//! redraw. So [`detect`] picks a protocol only when the terminal has identified
//! itself — by an environment variable only that terminal sets — and returns
//! [`Protocol::Placeholder`] for everything else, including every terminal
//! multiplexer, which would need the escapes wrapped to pass them through.
//! `image_protocol` in `config.toml` overrides it either way.
//!
//! It does not ask the terminal (a DA1 query for sixel, a kitty `a=q`): the
//! answer arrives on stdin, in the middle of the key stream the event thread is
//! already reading, at a time nobody controls. An environment variable cannot
//! be wrong in that way.
//!
//! # Three protocols, two ways of drawing
//!
//! **kitty** uses its *Unicode placeholders*: the image is sent to the terminal
//! once, outside the frame, and the cells it covers hold `U+10EEEE` plus two
//! combining marks saying which row and column of the image each one is, in a
//! foreground colour that encodes the image's id. The image is therefore part of
//! the cell grid: when anything else is drawn over those cells — another entry's
//! details, the help overlay — the image is gone from them, with nothing to clean
//! up. This is the protocol the user's own terminal speaks, and the only one of
//! the three that composes with a cell-diffing renderer for free.
//!
//! **sixel** and **iTerm2** draw at the cursor, so they are written straight to
//! the terminal after the frame, at the pane's position, and the cells under
//! them are marked `skip` so the renderer leaves the pixels alone. They are
//! written again only when what is on screen there changes — a different
//! picture, a different area, or the image having been hidden — never once per
//! frame.
//!
//! # Cost
//!
//! A 5 MB PNG takes a moment to decode, so it is decoded on a worker
//! (`work::read_art`), once, and shrunk there to [`MAX_DECODED`] pixels. What a
//! frame needs is then the encoded payload for one pane size, which is computed
//! the first time that size is drawn and cached by path and size
//! ([`ArtCache`]): moving the cursor back to an album already seen costs
//! nothing, and redrawing the same screen costs nothing at all.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use camino::Utf8Path;
use mpdfm_core::art::{self, ImageFacts, PictureKind, human_bytes};
use mpdfm_core::config::ImageProtocol;
use mpdfm_core::library::Kind;
use mpdfm_core::paths::RelPath;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::fit;

/// How art is drawn on this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// The kitty graphics protocol, through Unicode placeholders.
    Kitty,
    /// DEC sixel graphics.
    Sixel,
    /// iTerm2's inline image escape.
    Iterm,
    /// No graphics: a bordered description of the image.
    Placeholder,
}

impl Protocol {
    /// Whether this protocol sends graphics at all.
    #[must_use]
    pub fn draws(self) -> bool {
        self != Self::Placeholder
    }
}

/// Which protocol to use, given the setting and the environment.
///
/// `var` is the environment, as a function so a test can supply one. See the
/// [module docs][self] for why nothing is asked of the terminal itself.
pub fn detect(setting: ImageProtocol, var: impl Fn(&str) -> Option<String>) -> Protocol {
    match setting {
        ImageProtocol::Kitty => return Protocol::Kitty,
        ImageProtocol::Sixel => return Protocol::Sixel,
        ImageProtocol::Iterm => return Protocol::Iterm,
        ImageProtocol::None => return Protocol::Placeholder,
        ImageProtocol::Auto => {}
    }
    let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
    let term = var("TERM").unwrap_or_default();
    let program = var("TERM_PROGRAM").unwrap_or_default();

    // A multiplexer between us and the terminal swallows or mangles the escapes
    // unless they are wrapped for it, so whatever is outside it does not count.
    if set("TMUX") || set("STY") || term.starts_with("screen") || term.starts_with("tmux") {
        return Protocol::Placeholder;
    }
    if set("KITTY_WINDOW_ID") || term == "xterm-kitty" {
        return Protocol::Kitty;
    }
    if term == "xterm-ghostty" || program.eq_ignore_ascii_case("ghostty") {
        return Protocol::Kitty;
    }
    if program == "iTerm.app" || var("LC_TERMINAL").is_some_and(|v| v == "iTerm2") {
        return Protocol::Iterm;
    }
    // WezTerm speaks all three; iTerm2's is the one it implements completely.
    if program == "WezTerm" {
        return Protocol::Iterm;
    }
    if term.starts_with("foot") || term == "mlterm" || term.contains("sixel") {
        return Protocol::Sixel;
    }
    Protocol::Placeholder
}

// ---------------------------------------------------------------------------
// Loading — on a worker.

/// The longest side a picture is kept at once decoded. Comfortably more than the
/// details pane will ever show, and small enough that encoding a pane-sized copy
/// on the drawing thread is a millisecond rather than a hundred.
pub const MAX_DECODED: u32 = 512;

/// A picture loaded for display.
#[derive(Debug)]
pub struct Art {
    /// What it is of, for the placeholder: `front cover`, or the image file's
    /// own name for a loose image.
    pub label: String,
    /// What the bytes are.
    pub facts: ImageFacts,
    /// The decoded pixels, shrunk to [`MAX_DECODED`]. `None` when nothing will
    /// draw them, or when they could not be decoded.
    pub pixels: Option<image::RgbaImage>,
}

/// Load the art for the library entry at `abs`: a track's preferred embedded
/// picture, or an image file itself.
///
/// `Ok(None)` for something with no art — a track with no pictures, an `.nfo`.
/// `decode` is false when the protocol is the placeholder, which needs only the
/// image's facts.
///
/// # Errors
///
/// The entry cannot be read. A picture that will not decode is not an error: it
/// still has facts, and the placeholder shows them.
pub fn load(rel: &RelPath, abs: &Utf8Path, decode: bool) -> Result<Option<Art>, String> {
    let (label, bytes) = if Kind::of(rel).is_audio() {
        let pictures = art::embedded(abs).map_err(|err| err.to_string())?;
        let Some(preferred) = art::read::preferred(&pictures).cloned() else {
            return Ok(None);
        };
        let found = art::read::picture_data(abs, |p| p.image == preferred.image)
            .map_err(|err| err.to_string())?;
        let Some((picture, bytes)) = found else {
            return Ok(None);
        };
        (kind_label(picture.kind), bytes)
    } else if is_image_name(rel.file_name()) {
        let bytes = std::fs::read(abs).map_err(|err| format!("{abs}: {err}"))?;
        (rel.file_name().to_owned(), bytes)
    } else {
        return Ok(None);
    };

    let facts = ImageFacts::of(&bytes);
    if facts.format == art::ImageFormat::Unknown {
        return Ok(None);
    }
    let pixels = decode
        .then(|| image::load_from_memory(&bytes).ok())
        .flatten()
        .map(|image| {
            if image.width() > MAX_DECODED || image.height() > MAX_DECODED {
                image.thumbnail(MAX_DECODED, MAX_DECODED)
            } else {
                image
            }
            .to_rgba8()
        });
    Ok(Some(Art {
        label,
        facts,
        pixels,
    }))
}

/// Whether the browser should ask for art for this entry at all.
#[must_use]
pub fn has_art(rel: &RelPath) -> bool {
    Kind::of(rel).is_audio() || is_image_name(rel.file_name())
}

fn is_image_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp"
        )
    })
}

fn kind_label(kind: PictureKind) -> String {
    kind.name().to_owned()
}

// ---------------------------------------------------------------------------
// The cache — on the drawing thread.

/// What is known about one entry's art.
#[derive(Debug, Clone)]
pub enum Slot {
    /// A worker is reading it.
    Loading,
    /// It has none.
    Nothing,
    /// It is here.
    Ready(Arc<Art>),
    /// It could not be read. The worker has logged why; the pane shows the
    /// entry's details without art, as it would for one with none.
    Failed,
}

/// How many entries' art is kept. A screenful of albums looked at twice is the
/// working set; past that the oldest are dropped.
const MAX_SLOTS: usize = 48;

/// The art the browser has loaded, and the payloads it has encoded from it.
///
/// Interior mutability for the payloads, because they are worked out while
/// rendering, and rendering does not take the app mutably.
#[derive(Debug)]
pub struct ArtCache {
    /// How art is drawn here.
    pub protocol: Protocol,
    /// One cell's size in pixels, for fitting an image to a pane.
    pub cell: (u16, u16),
    slots: HashMap<RelPath, Slot>,
    order: Vec<RelPath>,
    in_flight: Option<RelPath>,
    encoded: RefCell<HashMap<(RelPath, u16, u16), Arc<Encoded>>>,
    encodes: Cell<usize>,
    /// What the last frame drew, so graphics are written only when it changes.
    pending: RefCell<Option<Placed>>,
    shown: Option<(RelPath, Rect)>,
    kitty_sent: Option<u32>,
}

/// One payload, for one picture at one pane size.
#[derive(Debug)]
pub struct Encoded {
    /// The cells it covers.
    pub cols: u16,
    /// The rows it covers.
    pub rows: u16,
    /// The bytes to write to the terminal.
    pub bytes: Vec<u8>,
    /// The kitty image id — the colour its placeholder cells are drawn in.
    pub id: u32,
}

/// Where a frame put a picture that still has to be written to the terminal.
#[derive(Debug, Clone)]
struct Placed {
    rel: RelPath,
    area: Rect,
    payload: Arc<Encoded>,
}

impl ArtCache {
    /// An empty cache for `protocol`, with cells of `cell` pixels.
    #[must_use]
    pub fn new(protocol: Protocol, cell: (u16, u16)) -> Self {
        Self {
            protocol,
            cell: (cell.0.max(1), cell.1.max(1)),
            slots: HashMap::new(),
            order: Vec::new(),
            in_flight: None,
            encoded: RefCell::new(HashMap::new()),
            encodes: Cell::new(0),
            pending: RefCell::new(None),
            shown: None,
            kitty_sent: None,
        }
    }

    /// What is known about `rel`'s art.
    #[cfg(test)]
    #[must_use]
    pub fn slot(&self, rel: &RelPath) -> Option<&Slot> {
        self.slots.get(rel)
    }

    /// The entry a worker should load next, if any: `rel`, unless it is already
    /// known or a load is already out. Marks it loading.
    pub fn want(&mut self, rel: &RelPath) -> Option<RelPath> {
        if self.in_flight.is_some() || self.slots.contains_key(rel) || !has_art(rel) {
            return None;
        }
        self.in_flight = Some(rel.clone());
        self.insert(rel.clone(), Slot::Loading);
        Some(rel.clone())
    }

    /// A worker answered.
    pub fn arrived(&mut self, rel: RelPath, result: Result<Option<Art>, String>) {
        if self.in_flight.as_ref() == Some(&rel) {
            self.in_flight = None;
        }
        let slot = match result {
            Ok(Some(art)) => Slot::Ready(Arc::new(art)),
            Ok(None) => Slot::Nothing,
            Err(_) => Slot::Failed,
        };
        self.insert(rel, slot);
    }

    /// Forget everything — the library was rescanned, so any file may have
    /// changed.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.order.clear();
        self.encoded.borrow_mut().clear();
        // A load that is out will still answer; it is simply stored again.
    }

    /// How many payloads have been encoded, ever. A test's window onto "not once
    /// per frame".
    #[cfg(test)]
    #[must_use]
    pub fn encodes(&self) -> usize {
        self.encodes.get()
    }

    fn insert(&mut self, rel: RelPath, slot: Slot) {
        if !self.slots.contains_key(&rel) {
            self.order.push(rel.clone());
        }
        self.slots.insert(rel, slot);
        while self.order.len() > MAX_SLOTS {
            let oldest = self.order.remove(0);
            self.slots.remove(&oldest);
            self.encoded
                .borrow_mut()
                .retain(|(rel, _, _), _| *rel != oldest);
        }
    }

    /// How many rows the art should take in a details pane `width` cells wide
    /// and `height` tall, or 0 for none.
    #[must_use]
    pub fn rows_for(&self, rel: &RelPath, width: u16, height: u16) -> u16 {
        let Some(Slot::Ready(art)) = self.slots.get(rel) else {
            return 0;
        };
        let inner = width.saturating_sub(2);
        let wanted = match (&art.pixels, self.protocol.draws()) {
            (Some(pixels), true) => {
                let (_, rows) = fit_cells(pixels.dimensions(), (inner, height), self.cell);
                rows + 2
            }
            _ => PLACEHOLDER_ROWS,
        };
        // Never more than half the pane: the playlists below it matter more.
        wanted.min(height / 2)
    }

    /// Draw `rel`'s art into `area` of `buf`: pixels, or the placeholder.
    ///
    /// `graphics` is false when something is on top of the browser — the help, a
    /// prompt — and the art must not be written over it.
    pub fn render(&self, rel: &RelPath, area: Rect, buf: &mut Buffer, graphics: bool) {
        let Some(Slot::Ready(art)) = self.slots.get(rel) else {
            return;
        };
        draw_border(area, buf);
        let inner = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let pixels = match &art.pixels {
            Some(pixels) if self.protocol.draws() && graphics => pixels,
            _ => {
                placeholder(art, inner, buf);
                return;
            }
        };

        let payload = self.payload(rel, pixels, inner);
        let at = Rect {
            x: inner.x + (inner.width - payload.cols) / 2,
            y: inner.y,
            width: payload.cols,
            height: payload.rows,
        };
        match self.protocol {
            Protocol::Kitty => kitty_cells(at, payload.id, buf),
            Protocol::Sixel | Protocol::Iterm => {
                for y in at.top()..at.bottom() {
                    for x in at.left()..at.right() {
                        // A symbol the area never holds otherwise, so that when the
                        // image goes the renderer sees every one of these cells
                        // change and paints over the pixels.
                        buf[(x, y)]
                            .set_symbol("\u{2800}")
                            .set_diff_option(CellDiffOption::Skip);
                    }
                }
            }
            Protocol::Placeholder => {}
        }
        *self.pending.borrow_mut() = Some(Placed {
            rel: rel.clone(),
            area: at,
            payload,
        });
    }

    /// The payload for `rel` at `inner`'s size, encoded once and kept.
    fn payload(&self, rel: &RelPath, pixels: &image::RgbaImage, inner: Rect) -> Arc<Encoded> {
        let key = (rel.clone(), inner.width, inner.height);
        if let Some(found) = self.encoded.borrow().get(&key) {
            return Arc::clone(found);
        }
        let (cols, rows) = fit_cells(pixels.dimensions(), (inner.width, inner.height), self.cell);
        let target = (
            u32::from(cols) * u32::from(self.cell.0),
            u32::from(rows) * u32::from(self.cell.1),
        );
        let id = kitty_id(rel, inner);
        let bytes = encode(self.protocol, pixels, target, (cols, rows), id);
        self.encodes.set(self.encodes.get() + 1);
        let encoded = Arc::new(Encoded {
            cols,
            rows,
            bytes,
            id,
        });
        self.encoded.borrow_mut().insert(key, Arc::clone(&encoded));
        encoded
    }

    /// After a frame: write whatever graphics it placed that the terminal does
    /// not have yet. `resized` says the terminal was cleared since the last
    /// frame, which loses sixel and iTerm2 pixels.
    ///
    /// # Errors
    ///
    /// Writing to the terminal failed.
    pub fn flush(&mut self, out: &mut dyn Write, resized: bool) -> std::io::Result<()> {
        let placed = self.pending.borrow_mut().take();
        let Some(placed) = placed else {
            self.shown = None;
            return Ok(());
        };
        match self.protocol {
            Protocol::Kitty => {
                // Position-free: the cells already say where. Only new image data
                // has to be sent, and the old image's data freed.
                if self.kitty_sent != Some(placed.payload.id) {
                    if let Some(old) = self.kitty_sent {
                        write!(out, "\x1b_Ga=d,d=I,i={old},q=2\x1b\\")?;
                    }
                    out.write_all(&placed.payload.bytes)?;
                    self.kitty_sent = Some(placed.payload.id);
                }
            }
            Protocol::Sixel | Protocol::Iterm => {
                let same = self
                    .shown
                    .as_ref()
                    .is_some_and(|(rel, area)| *rel == placed.rel && *area == placed.area);
                if !same || resized {
                    // Save the cursor, draw, put it back: the renderer believes it
                    // knows where the cursor is.
                    write!(
                        out,
                        "\x1b7\x1b[{};{}H",
                        placed.area.y + 1,
                        placed.area.x + 1
                    )?;
                    out.write_all(&placed.payload.bytes)?;
                    out.write_all(b"\x1b8")?;
                }
            }
            Protocol::Placeholder => return Ok(()),
        }
        out.flush()?;
        self.shown = Some((placed.rel, placed.area));
        Ok(())
    }
}

/// Rows the placeholder takes, border included.
const PLACEHOLDER_ROWS: u16 = 5;

/// The text fallback: what the image is, inside the border.
fn placeholder(art: &Art, inner: Rect, buf: &mut Buffer) {
    let lines = [
        art.label.clone(),
        format!("{} {}", art.facts.format.mime(), art.facts.dimensions()),
        human_bytes(art.facts.size),
    ];
    let style = Style::new().fg(Color::DarkGray);
    for (row, text) in lines.iter().enumerate().take(usize::from(inner.height)) {
        let y = inner.y + u16::try_from(row).unwrap_or(u16::MAX);
        buf.set_stringn(
            inner.x,
            y,
            fit(text, usize::from(inner.width)),
            usize::from(inner.width),
            style,
        );
    }
}

/// A plain box with ` cover ` in its top edge.
fn draw_border(area: Rect, buf: &mut Buffer) {
    use ratatui::widgets::{Block, Borders, Widget as _};
    Block::new()
        .borders(Borders::ALL)
        .title(" cover ")
        .render(area, buf);
}

/// How many cells an image of `pixels` takes inside `room`, keeping its aspect
/// ratio, given cells of `cell` pixels. At least one by one.
#[must_use]
pub fn fit_cells(pixels: (u32, u32), room: (u16, u16), cell: (u16, u16)) -> (u16, u16) {
    let (w, h) = (f64::from(pixels.0.max(1)), f64::from(pixels.1.max(1)));
    let (room_w, room_h) = (
        f64::from(room.0) * f64::from(cell.0),
        f64::from(room.1) * f64::from(cell.1),
    );
    let scale = (room_w / w).min(room_h / h);
    let cols = ((w * scale) / f64::from(cell.0)).floor();
    let rows = ((h * scale) / f64::from(cell.1)).floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let clamp = |value: f64, max: u16| (value as u16).clamp(1, max.max(1));
    (clamp(cols, room.0), clamp(rows, room.1))
}

/// A kitty image id for `rel` at this size: 24 bits, never 0, from the path and
/// the area — so a different picture or a different size is a different image,
/// and the terminal never shows the old one in the new one's cells.
fn kitty_id(rel: &RelPath, area: Rect) -> u32 {
    let mut hash = mpdfm_core::ops::exec_fs::hash_bytes(rel.as_str().as_bytes());
    hash ^= u64::from(area.width) << 32 | u64::from(area.height);
    #[allow(clippy::cast_possible_truncation)]
    let id = (hash ^ (hash >> 24) ^ (hash >> 48)) as u32 & 0x00ff_ffff;
    id.max(1)
}

// ---------------------------------------------------------------------------
// Encoders.

/// The bytes that put `pixels`, resized to `target` pixels, on the terminal in
/// `cells`.
fn encode(
    protocol: Protocol,
    pixels: &image::RgbaImage,
    target: (u32, u32),
    cells: (u16, u16),
    id: u32,
) -> Vec<u8> {
    let resized = image::imageops::resize(
        pixels,
        target.0.max(1),
        target.1.max(1),
        image::imageops::FilterType::Triangle,
    );
    match protocol {
        Protocol::Kitty => kitty(&png(&resized), cells, id),
        Protocol::Sixel => sixel(&resized),
        Protocol::Iterm => iterm(&png(&resized), cells),
        Protocol::Placeholder => Vec::new(),
    }
}

fn png(pixels: &image::RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    // Encoding an in-memory image into memory fails only for lack of memory.
    let _ =
        image::DynamicImage::ImageRgba8(pixels.clone()).write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

/// kitty: transmit a PNG as a virtual placement (`U=1`) `cells` big, in 4 KiB
/// base64 chunks, quietly (`q=2`: no reply on stdin).
pub fn kitty(png: &[u8], cells: (u16, u16), id: u32) -> Vec<u8> {
    let data = base64(png);
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
    let mut out = Vec::with_capacity(data.len() + chunks.len() * 32);
    for (at, chunk) in chunks.iter().enumerate() {
        let more = u8::from(at + 1 < chunks.len());
        if at == 0 {
            let _ = write!(
                out,
                "\x1b_Ga=T,U=1,f=100,t=d,i={id},c={},r={},q=2,m={more};",
                cells.0, cells.1
            );
        } else {
            let _ = write!(out, "\x1b_Gm={more};");
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// kitty: the placeholder cells for image `id` over `area`. Every cell names its
/// own row and column, so nothing depends on the cell to its left having been
/// drawn in the same write.
fn kitty_cells(area: Rect, id: u32, buf: &mut Buffer) {
    let color = Color::Rgb(
        ((id >> 16) & 0xff) as u8,
        ((id >> 8) & 0xff) as u8,
        (id & 0xff) as u8,
    );
    for row in 0..area.height {
        for col in 0..area.width {
            let (Some(r), Some(c)) = (
                DIACRITICS.get(usize::from(row)),
                DIACRITICS.get(usize::from(col)),
            ) else {
                continue;
            };
            let symbol: String = [PLACEHOLDER, *r, *c].iter().collect();
            buf[(area.x + col, area.y + row)]
                .set_symbol(&symbol)
                .set_fg(color);
        }
    }
}

/// kitty's placeholder character.
const PLACEHOLDER: char = '\u{10EEEE}';

/// kitty's row/column diacritics, in order: the first 64 of the 297 in
/// `rowcolumn-diacritics.txt`, read out of the `kitten` binary and checked
/// against kitty's published table. 64 rows and columns is more than any
/// details pane.
const DIACRITICS: [char; 64] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
];

/// iTerm2: the PNG inline, sized in cells.
pub fn iterm(png: &[u8], cells: (u16, u16)) -> Vec<u8> {
    let mut out = Vec::with_capacity(png.len() * 4 / 3 + 96);
    let _ = write!(
        out,
        "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=1:",
        png.len(),
        cells.0,
        cells.1
    );
    out.extend_from_slice(base64(png).as_bytes());
    out.push(0x07);
    out
}

/// Sixel, quantized to a 6×6×6 colour cube: no dithering and no palette
/// search, which is plenty for a thumbnail and costs one pass.
pub fn sixel(pixels: &image::RgbaImage) -> Vec<u8> {
    let (width, height) = pixels.dimensions();
    let index = |p: &image::Rgba<u8>| -> usize {
        let level = |v: u8| (usize::from(v) * 5 + 127) / 255;
        level(p[0]) * 36 + level(p[1]) * 6 + level(p[2])
    };
    let mut out = Vec::new();
    let _ = write!(out, "\x1bPq\"1;1;{width};{height}");
    for i in 0..216 {
        let pct = |level: usize| level * 100 / 5;
        let _ = write!(
            out,
            "#{i};2;{};{};{}",
            pct(i / 36),
            pct(i / 6 % 6),
            pct(i % 6)
        );
    }
    for band in (0..height).step_by(6) {
        let rows = (height - band).min(6);
        // Which colours appear in this band, and in which order they were met.
        let mut used: Vec<usize> = Vec::new();
        for y in band..band + rows {
            for x in 0..width {
                let colour = index(pixels.get_pixel(x, y));
                if !used.contains(&colour) {
                    used.push(colour);
                }
            }
        }
        for (n, colour) in used.iter().enumerate() {
            let _ = write!(out, "#{colour}");
            let mut run: Option<(u8, usize)> = None;
            let emit = |out: &mut Vec<u8>, run: (u8, usize)| {
                let (ch, count) = run;
                if count > 3 {
                    let _ = write!(out, "!{count}");
                    out.push(ch);
                } else {
                    out.extend(std::iter::repeat_n(ch, count));
                }
            };
            for x in 0..width {
                let mut bits = 0u8;
                for dy in 0..rows {
                    if index(pixels.get_pixel(x, band + dy)) == *colour {
                        bits |= 1 << dy;
                    }
                }
                let ch = 0x3f + bits;
                run = match run {
                    Some((seen, count)) if seen == ch => Some((seen, count + 1)),
                    Some(previous) => {
                        emit(&mut out, previous);
                        Some((ch, 1))
                    }
                    None => Some((ch, 1)),
                };
            }
            if let Some(last) = run {
                emit(&mut out, last);
            }
            out.push(if n + 1 < used.len() { b'$' } else { b'-' });
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Standard base64 with padding — the one encoding all three protocols want, and
/// not worth a dependency.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn detection_picks_a_protocol_only_when_the_terminal_says_so() {
        let auto = ImageProtocol::Auto;
        assert_eq!(
            detect(auto, env(&[("TERM", "xterm-kitty")])),
            Protocol::Kitty
        );
        assert_eq!(
            detect(
                auto,
                env(&[("TERM", "xterm-256color"), ("KITTY_WINDOW_ID", "1")])
            ),
            Protocol::Kitty
        );
        assert_eq!(
            detect(auto, env(&[("TERM_PROGRAM", "iTerm.app")])),
            Protocol::Iterm
        );
        assert_eq!(
            detect(auto, env(&[("TERM_PROGRAM", "WezTerm")])),
            Protocol::Iterm
        );
        assert_eq!(detect(auto, env(&[("TERM", "foot")])), Protocol::Sixel);

        // Everything else is the placeholder.
        for unknown in [
            &[("TERM", "xterm-256color")][..],
            &[("TERM", "alacritty")],
            &[("TERM", "linux")],
            &[("TERM", "dumb")],
            &[],
        ] {
            assert_eq!(
                detect(auto, env(unknown)),
                Protocol::Placeholder,
                "{unknown:?}"
            );
        }
    }

    #[test]
    fn a_multiplexer_hides_whatever_terminal_is_outside_it() {
        let auto = ImageProtocol::Auto;
        assert_eq!(
            detect(
                auto,
                env(&[
                    ("TERM", "xterm-kitty"),
                    ("TMUX", "/tmp/tmux-1000/default,1,0")
                ])
            ),
            Protocol::Placeholder
        );
        assert_eq!(
            detect(
                auto,
                env(&[("KITTY_WINDOW_ID", "3"), ("TERM", "screen-256color")])
            ),
            Protocol::Placeholder
        );
    }

    #[test]
    fn the_setting_overrides_detection_both_ways() {
        let kitty = env(&[("TERM", "xterm-kitty")]);
        assert_eq!(detect(ImageProtocol::None, &kitty), Protocol::Placeholder);
        let dumb = env(&[("TERM", "dumb")]);
        assert_eq!(detect(ImageProtocol::Sixel, &dumb), Protocol::Sixel);
        assert_eq!(detect(ImageProtocol::Kitty, &dumb), Protocol::Kitty);
        assert_eq!(detect(ImageProtocol::Iterm, &dumb), Protocol::Iterm);
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded, "{plain:?}");
        }
    }

    #[test]
    fn an_image_is_fitted_into_cells_keeping_its_aspect_ratio() {
        // A square cover in a 24×20 pane of 10×20 px cells: 24 cols is 240 px,
        // so 240 px tall is 12 rows.
        assert_eq!(fit_cells((600, 600), (24, 20), (10, 20)), (24, 12));
        // A wide one is limited by width; a tall one by height.
        assert_eq!(fit_cells((1200, 600), (24, 20), (10, 20)), (24, 6));
        assert_eq!(fit_cells((300, 1200), (24, 10), (10, 20)), (5, 10));
        // Never zero.
        assert_eq!(fit_cells((1, 10_000), (24, 10), (10, 20)), (1, 10));
    }

    #[test]
    fn a_placeholder_cell_is_one_cell_wide() {
        use unicode_width::UnicodeWidthStr as _;
        let symbol: String = [PLACEHOLDER, DIACRITICS[3], DIACRITICS[7]].iter().collect();
        assert_eq!(
            symbol.width(),
            1,
            "the renderer would skip the next cell otherwise"
        );
    }

    #[test]
    fn kitty_chunks_its_transmission_and_keeps_it_quiet() {
        let bytes = kitty(&vec![7u8; 10_000], (12, 6), 42);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("\x1b_Ga=T,U=1,f=100,t=d,i=42,c=12,r=6,q=2,m=1;"));
        assert_eq!(
            text.matches("\x1b_G").count(),
            4,
            "13 336 base64 bytes in 4 KiB chunks"
        );
        assert!(text.ends_with("\x1b\\"));
        assert!(
            text.contains("\x1b_Gm=0;"),
            "the last chunk says it is the last"
        );
    }

    #[test]
    fn sixel_is_framed_and_covers_every_band() {
        let pixels = image::RgbaImage::from_pixel(4, 13, image::Rgba([255, 0, 0, 255]));
        let text = String::from_utf8(sixel(&pixels)).unwrap();
        assert!(text.starts_with("\x1bPq\"1;1;4;13"));
        assert!(text.ends_with("\x1b\\"));
        // 13 rows are three bands of six, each ending in a newline.
        assert_eq!(text.matches('-').count(), 3);
    }
}
