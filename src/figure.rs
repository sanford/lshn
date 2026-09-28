//! An article's first picture, shown at the top of it: found in the
//! article's text, fetched and shrunk in the background, and drawn by
//! ratatui-image in whatever way the terminal can (iTerm2's images, Kitty's,
//! Sixel, or else coloured half blocks).
//!
//! The story's document leaves room for it with `<!-- image: W H ROWS -->`
//! (its size in pixels, and the most rows it may take), and the renderer
//! works out how big it can be at the document's width.

use comrak::nodes::NodeValue;
use comrak::{Arena, parse_document};
use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Rect, Size};
use ratatui::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use std::num::NonZeroU16;
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

/// The first picture in `md` worth showing: its address, and where it is in
/// `md` so it can be taken out when it's shown at the top instead. SVGs are
/// passed over: there's no drawing them.
pub fn first(md: &str) -> Option<(String, std::ops::Range<usize>)> {
    let arena = Arena::new();
    let root = parse_document(&arena, md, &crate::render::options());
    let starts: Vec<usize> = std::iter::once(0)
        .chain(md.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let offset = |line: usize, col: usize| Some(starts.get(line.checked_sub(1)?)? + col.checked_sub(1)?);
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
        let start = offset(pos.start.line, pos.start.column)?;
        let end = offset(pos.end.line, pos.end.column)? + 1;
        let range = (end <= md.len() && md.get(start..end)?.starts_with("![")).then_some(start..end)?;
        return Some((url, range));
    }
    None
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
        if picker.protocol_type() == ProtocolType::Iterm2 {
            let cell = picker.font_size();
            let (cw, ch) = (cell.width.into(), cell.height.into());
            return Some(Drawn::Rows {
                cols,
                rows: iterm_rows(picture, cols, rows, cw, ch, 1)?,
                soft: iterm_rows(picture, cols, rows, cw, ch, SOFTER)?,
            });
        }
        let size = Size::new(cols as u16, rows as u16);
        SlicedProtocol::new(picker, picture.clone(), Some(size)).ok().map(Drawn::Sliced)
    }

    /// Whether there's a lighter version to show while it moves.
    pub fn has_soft(&self) -> bool {
        matches!(self, Drawn::Rows { .. })
    }

    /// Draws it `x` columns into `area` and `y` rows down, which is above
    /// the top once it's scrolled partly off; `soft` while it's moving.
    pub fn draw(&self, buf: &mut Buffer, area: Rect, x: u16, y: i32, soft: bool) {
        match self {
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

/// Where a picture goes in a document: `<!-- image: W H ROWS -->`.
pub fn marker(picture: &DynamicImage, rows: usize) -> String {
    format!("<!-- image: {} {} {rows} -->", picture.width(), picture.height())
}

/// The size in pixels and most rows, from a marker.
pub fn parse_marker(html: &str) -> Option<(u32, u32, usize)> {
    let rest = html.trim().strip_prefix("<!-- image:")?.strip_suffix("-->")?;
    let mut numbers = rest.split_whitespace();
    let w = numbers.next()?.parse().ok()?;
    let h = numbers.next()?.parse().ok()?;
    let rows = numbers.next()?.parse().ok()?;
    (w > 0 && h > 0).then_some((w, h, rows))
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
    fn finds_the_first_picture_that_can_be_drawn() {
        let md = "Intro.\n\n![a logo](https://x.com/logo.svg)\n\nText ![a photo](https://x.com/p.jpg?w=640) more.\n";
        let (url, range) = first(md).unwrap();
        assert_eq!(url, "https://x.com/p.jpg?w=640");
        assert_eq!(&md[range], "![a photo](https://x.com/p.jpg?w=640)");
        assert_eq!(first("No pictures."), None);
        // Parentheses in the address, as some image servers use.
        let md = "![x](https://a.com/fit(1x2)/b.png)";
        assert_eq!(&md[first(md).unwrap().1], md);
    }

    #[test]
    fn markers_say_the_size() {
        assert_eq!(parse_marker("<!-- image: 800 600 12 -->"), Some((800, 600, 12)));
        assert_eq!(parse_marker("<!-- image: 0 600 12 -->"), None);
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



