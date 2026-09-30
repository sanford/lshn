//! An article's pictures: found in its text, fetched and shrunk in the
//! background, and drawn in whatever way the terminal can: Kitty's and
//! iTerm2's images here, Sixel or else coloured half blocks by
//! ratatui-image. The first is shown at the top of the article, the rest
//! after the paragraph they're in.
//!
//! The story's document leaves room for each with `<!-- image: W H ROWS N -->`
//! (its size in pixels, the most rows it may take, and which picture it is),
//! and the renderer works out how big it can be at the document's width.

use comrak::nodes::NodeValue;
use comrak::{Arena, parse_document};
use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use ratatui::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use std::num::NonZeroU16;
use std::ops::Range;
use std::sync::OnceLock;

/// Pictures smaller than this, either way, are icons and the like.
const SMALLEST: u32 = 120;
/// Pictures are shrunk to fit this on the fetching thread, so drawing
/// them doesn't wait on scaling a photo from a camera.
const LARGEST: u32 = 1600;
/// No more than this is downloaded.
const MAX_BYTES: u64 = 10 << 20;

/// The most rows a picture may take: in the preview, and read in full.
pub const PREVIEW_ROWS: usize = 12;
pub const FULL_ROWS: usize = 24;

/// The terminal's character cell, in pixels, once it's been asked.
static CELL: OnceLock<(u16, u16)> = OnceLock::new();

pub fn set_cell(width: u16, height: u16) {
    if width > 0 && height > 0 {
        let _ = CELL.set((width, height));
    }
}

fn cell() -> (u16, u16) {
    CELL.get().copied().unwrap_or((10, 20))
}

/// Most pictures shown from one article.
const MAX_PICTURES: usize = 20;

/// A picture in an article's Markdown.
#[derive(Debug, PartialEq, Eq)]
pub struct Found {
    pub url: String,
    /// Its `![…](…)`, to take out when it's shown.
    pub at: Range<usize>,
    /// Just after the top-level block it's in: where it's shown, since it
    /// may be inside a link or a sentence.
    pub after: usize,
}

/// The pictures in `md` worth showing, in order. SVGs are passed over:
/// there's no drawing them.
pub fn all(md: &str) -> Vec<Found> {
    let arena = Arena::new();
    let root = parse_document(&arena, md, &crate::render::options());
    let starts: Vec<usize> = std::iter::once(0)
        .chain(md.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let offset = |line: usize, col: usize| Some(starts.get(line.checked_sub(1)?)? + col.checked_sub(1)?);
    let line_end = |line: usize| starts.get(line).copied().unwrap_or(md.len());
    let mut found = Vec::new();
    for node in root.descendants() {
        let NodeValue::Image(link) = &node.data().value else {
            continue;
        };
        let url = link.url.clone();
        let path = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        if !url.starts_with("http") || path.ends_with(".svg") {
            continue;
        }
        let pos = node.data().sourcepos;
        let (Some(start), Some(end)) = (
            offset(pos.start.line, pos.start.column),
            offset(pos.end.line, pos.end.column).map(|e| e + 1),
        ) else {
            continue;
        };
        if end > md.len() || !md.get(start..end).is_some_and(|s| s.starts_with("![")) {
            continue;
        }
        let block = node
            .ancestors()
            .find(|n| n.parent().is_some_and(|p| p.parent().is_none()))
            .unwrap_or(node);
        let after = line_end(block.data().sourcepos.end.line).max(end);
        found.push(Found { url, at: start..end, after });
        if found.len() == MAX_PICTURES {
            break;
        }
    }
    found
}

/// Downloads a picture, to be decoded.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut response = crate::hn::agent().get(url).call().map_err(|e| e.to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// Decodes a picture, shrunk to a size worth drawing.
pub fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    let picture = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    if picture.width() < SMALLEST || picture.height() < SMALLEST {
        return Err("too small".into());
    }
    Ok(if picture.width() > LARGEST || picture.height() > LARGEST {
        picture.thumbnail(LARGEST, LARGEST)
    } else {
        picture
    })
}

/// A picture made ready to draw at one size.
pub enum Drawn {
    /// Kitty's (newer iTerm2s draw them too): sent once, then shown by
    /// a character in each cell it covers, so it moves with the text like
    /// any other. Only a row's first cell says which row it is, the rest
    /// follow on from it, so a step of a scroll rewrites a cell a row.
    Placed {
        id: u32,
        cols: usize,
        rows: usize,
        /// The picture, till it's been sent.
        send: Option<String>,
    },
    /// iTerm2: a picture per row, so it can scroll partly off screen. As
    /// JPEG, when it's opaque: the rows are sent again at each step of a
    /// scroll, and PNG is ten times the size for a photo. `soft` is the
    /// same at a sixteenth of the resolution, for while it's moving.
    Rows {
        cols: usize,
        rows: Vec<String>,
        soft: Vec<String>,
    },
    /// Anything else, as ratatui-image does it.
    Sliced(SlicedProtocol),
}

impl Drawn {
    pub fn new(picker: &Picker, picture: &DynamicImage, cols: usize, rows: usize) -> Option<Drawn> {
        let cell = picker.font_size();
        let (cw, ch) = (cell.width.into(), cell.height.into());
        if picker.protocol_type() == ProtocolType::Kitty && rows <= DIACRITICS.len() {
            let id = kitty_id();
            return Some(Drawn::Placed {
                id,
                cols,
                rows,
                send: Some(kitty_send(picture, cols, rows, cw, ch, id)?),
            });
        }
        if picker.protocol_type() == ProtocolType::Iterm2 {
            return Some(Drawn::Rows {
                cols,
                rows: iterm_rows(picture, cols, rows, cw, ch, 1)?,
                soft: iterm_rows(picture, cols, rows, cw, ch, SOFTER)?,
            });
        }
        let size = Size::new(cols as u16, rows as u16);
        SlicedProtocol::new(picker, picture.clone(), Some(size)).ok().map(Drawn::Sliced)
    }

    /// What's to be written to the terminal before it can be shown, once.
    pub fn take_send(&mut self) -> Option<String> {
        match self {
            Drawn::Placed { send, .. } => send.take(),
            _ => None,
        }
    }

    /// What frees it in the terminal, once it's no longer wanted.
    pub fn forget(&self) -> Option<String> {
        match self {
            Drawn::Placed { id, .. } => Some(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")),
            _ => None,
        }
    }

    /// Draws it `x` columns into `area` and `y` rows down, which is above
    /// the top once it's scrolled partly off; `soft` while it's moving.
    pub fn draw(&self, buf: &mut Buffer, area: Rect, x: u16, y: i32, soft: bool) {
        match self {
            Drawn::Placed { id, cols, rows, .. } => {
                let [_, r, g, b] = id.to_be_bytes();
                let left = area.x + x;
                let cols = (*cols as u16).min(area.right().saturating_sub(left));
                for (row, &mark) in DIACRITICS[..*rows].iter().enumerate() {
                    let at = y + row as i32;
                    if at < 0 || at >= i32::from(area.height) {
                        continue;
                    }
                    let top = area.y + at as u16;
                    for c in 0..cols {
                        let cell = &mut buf[(left + c, top)];
                        if c == 0 {
                            let mut first = String::from(PLACEHOLDER);
                            first.extend([mark, DIACRITICS[0]]);
                            cell.set_symbol(&first);
                        } else {
                            cell.set_char(PLACEHOLDER);
                        }
                        cell.set_fg(Color::Rgb(r, g, b))
                            .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
                    }
                }
            }
            Drawn::Rows { cols, rows, soft: light } => {
                let rows = if soft { light } else { rows };
                let left = area.x + x;
                let cols = (*cols as u16).min(area.right().saturating_sub(left));
                for (i, row) in rows.iter().enumerate() {
                    let at = y + i as i32;
                    if at < 0 || at >= i32::from(area.height) || cols == 0 {
                        continue;
                    }
                    let top = area.y + at as u16;
                    buf[(left, top)]
                        .set_symbol(row)
                        .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
                    for c in 1..cols {
                        buf[(left + c, top)].set_diff_option(CellDiffOption::Skip);
                    }
                }
            }
            Drawn::Sliced(protocol) => {
                let area = Rect {
                    x: area.x + x,
                    width: area.width.saturating_sub(x),
                    ..area
                };
                let y = y.clamp(i16::MIN.into(), i16::MAX.into()) as i16;
                SlicedImage::new(protocol, SignedPosition::from((0, y))).render(area, buf);
            }
        }
    }
}

/// Stands for a cell of a Kitty picture.
const PLACEHOLDER: char = '\u{10EEEE}';

/// Marks that follow it to say which row and column of the picture it is:
/// the first of Kitty's, enough for the most rows a picture takes.
const DIACRITICS: [char; 32] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}',
    '\u{346}', '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35B}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}',
];
const _: () = assert!(FULL_ROWS <= DIACRITICS.len());

/// A new number for a Kitty picture, which its cells carry as their
/// colour: never 0, and 24 bits, so that's all it takes. Starting from the
/// process's, so pictures left from another run aren't taken for these.
fn kitty_id() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    (std::process::id().wrapping_mul(7919).wrapping_add(n) & 0xFF_FFFF).max(1)
}

/// Kitty's escapes sending the picture at `cols`×`rows` cells, as `id`,
/// to be shown wherever its cells are. That size is given, not left to be
/// worked out from the picture's as Kitty does, since iTerm2 doesn't.
fn kitty_send(picture: &DynamicImage, cols: usize, rows: usize, cw: u32, ch: u32, id: u32) -> Option<String> {
    use base64::Engine;
    let scaled = picture.resize(cols as u32 * cw, rows as u32 * ch, FilterType::Triangle);
    let mut bytes = Vec::new();
    scaled
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .ok()?;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    // In pieces of at most 4096 characters, each saying if more follow.
    let pieces: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
    let mut out = String::with_capacity(data.len() + pieces.len() * 16 + 64);
    for (i, piece) in pieces.iter().enumerate() {
        let more = u8::from(i + 1 < pieces.len());
        out.push_str("\x1b_G");
        if i == 0 {
            out.push_str(&format!("a=T,U=1,f=100,i={id},c={cols},r={rows},"));
        }
        out.push_str(&format!("q=2,m={more};"));
        out.push_str(std::str::from_utf8(piece).ok()?);
        out.push_str("\x1b\\");
    }
    Some(out)
}

/// How much less detail the picture has while it moves, each way.
const SOFTER: u32 = 16;

/// iTerm2's escape for each row of the picture at `cols`×`rows` cells,
/// each clearing its row first. With `less` over 1, each row has that
/// much less detail each way, and iTerm2 stretches it to fit.
fn iterm_rows(
    picture: &DynamicImage,
    cols: usize,
    rows: usize,
    cw: u32,
    ch: u32,
    less: u32,
) -> Option<Vec<String>> {
    use base64::Engine;
    let scaled = picture.resize(cols as u32 * cw, rows as u32 * ch, FilterType::Triangle);
    let opaque = !scaled.color().has_alpha();
    let mut out = Vec::new();
    let mut y = 0;
    while y < scaled.height() {
        let h = ch.min(scaled.height() - y);
        let (w, row) = (scaled.width(), scaled.crop_imm(0, y, scaled.width(), h));
        let row = if less > 1 {
            row.resize_exact((w / less).max(1), (h / less).max(1), FilterType::Triangle)
        } else {
            row
        };
        let mut bytes = Vec::new();
        if opaque {
            row.to_rgb8()
                .write_with_encoder(JpegEncoder::new_with_quality(&mut bytes, 85))
                .ok()?;
        } else {
            row.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
                .ok()?;
        }
        out.push(format!(
            "\x1b[{cols}X\x1b]1337;File=inline=1;size={};width={w}px;height={h}px;preserveAspectRatio=0;doNotMoveCursor=1:{}\x07",
            bytes.len(),
            base64::engine::general_purpose::STANDARD.encode(&bytes),
        ));
        y += h;
    }
    Some(out)
}

/// Where a picture goes in a document: `<!-- image: W H ROWS N -->`, for
/// the article's `N`th picture.
pub fn marker(picture: &DynamicImage, rows: usize, index: usize) -> String {
    format!("<!-- image: {} {} {rows} {index} -->", picture.width(), picture.height())
}

/// The size in pixels, most rows and which picture, from a marker.
pub fn parse_marker(html: &str) -> Option<(u32, u32, usize, usize)> {
    let rest = html.trim().strip_prefix("<!-- image:")?.strip_suffix("-->")?;
    let mut numbers = rest.split_whitespace();
    let w = numbers.next()?.parse().ok()?;
    let h = numbers.next()?.parse().ok()?;
    let rows = numbers.next()?.parse().ok()?;
    let index = numbers.next()?.parse().ok()?;
    (w > 0 && h > 0).then_some((w, h, rows, index))
}

/// How many columns and rows a `w`×`h` picture takes, at most `cols` wide
/// and `rows` high: its own size if that fits, never bigger.
pub fn cells(w: u32, h: u32, cols: usize, rows: usize) -> (usize, usize) {
    let (cw, ch) = cell();
    let (cw, ch) = (f64::from(cw), f64::from(ch));
    let (w, h) = (f64::from(w), f64::from(h));
    let mut c = (w / cw).ceil().min(cols as f64);
    let mut r = (c * cw * h / w / ch).ceil();
    if r > rows as f64 {
        r = rows as f64;
        c = (r * ch * w / h / cw).floor();
    }
    (c.max(1.0) as usize, r.max(1.0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_pictures_that_can_be_drawn() {
        let md = "Intro.\n\n![a logo](https://x.com/logo.svg)\n\nText ![a photo](https://x.com/p.jpg?w=640) more.\nStill it.\n\n[![linked](https://x.com/l.png)](https://x.com/)\n";
        let found = all(md);
        let urls: Vec<&str> = found.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(urls, ["https://x.com/p.jpg?w=640", "https://x.com/l.png"]);
        assert_eq!(&md[found[0].at.clone()], "![a photo](https://x.com/p.jpg?w=640)");
        // Shown after its paragraph, not in the middle of it.
        assert!(md[..found[0].after].ends_with("Still it.\n"));
        assert_eq!(&md[found[1].at.clone()], "![linked](https://x.com/l.png)");
        assert_eq!(found[1].after, md.len());
        assert!(all("No pictures.").is_empty());
        // Parentheses in the address, as some image servers use.
        let md = "![x](https://a.com/fit(1x2)/b.png)";
        assert_eq!(&md[all(md)[0].at.clone()], md);
    }

    #[test]
    fn markers_say_the_size() {
        assert_eq!(parse_marker("<!-- image: 800 600 12 3 -->"), Some((800, 600, 12, 3)));
        assert_eq!(parse_marker("<!-- image: 0 600 12 0 -->"), None);
        assert_eq!(parse_marker("<!-- rule: x -->"), None);
    }

    #[test]
    fn pictures_fit_the_width_and_rows() {
        // 10×20 cells: a 1600×900 photo at 80 columns is 23 rows, over 12.
        assert_eq!(cells(1600, 900, 80, 12), (42, 12));
        assert_eq!(cells(1600, 900, 80, 30), (80, 23));
        // Small ones stay their size.
        assert_eq!(cells(200, 100, 80, 30), (20, 5));
    }
}



