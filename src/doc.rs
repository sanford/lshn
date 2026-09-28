//! An open document: its text, rendered, and where it's scrolled to.
//!
//! Re-wrapped at a new width, it keeps the same part on screen through
//! source line numbers: each rendered line knows which top-level block (and
//! so which source lines) it came from. A position is a fractional source
//! line, so a 1-line table that renders as 10 lines, or a paragraph that
//! wraps to 5, stays in place.

use crate::render::{Figure, Heading, RLine, render};
use crate::theme::Theme;
use crate::wrap;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use std::path::PathBuf;

/// A top-level block: its source lines and the rendered lines it became.
#[derive(Debug)]
struct Block {
    first: usize,
    last: usize,
    r0: usize,
    r1: usize,
}

/// A search through the rendered text.
struct Search {
    query: String,
    /// Every match: rendered line and columns.
    matches: Vec<(usize, usize, usize)>,
    /// The match last jumped to.
    current: Option<usize>,
}

/// A label typed to follow the link it's drawn on.
pub struct Hint {
    pub label: String,
    line: usize,
    col: usize,
    /// The link's target.
    pub url: String,
}

pub struct Doc {
    md: String,
    /// The directory relative links are relative to.
    pub base: Option<PathBuf>,
    /// Where links starting with `/` start from: the top of the repository.
    pub site: Option<PathBuf>,
    headings: Vec<Heading>,
    links: Vec<String>,
    /// Room left for pictures.
    figures: Vec<Figure>,
    search: Option<Search>,
    /// Link hints on screen, while choosing a link to follow.
    pub hints: Vec<Hint>,
    /// A heading to jump to once the document is laid out.
    pending_anchor: Option<String>,
    /// A search match to jump to once the document is laid out: its source
    /// line and the query.
    pending_match: Option<(usize, String)>,
    /// After a reload: the first line with text at or below the top of the
    /// screen, how far below the top it was, and its old index, to find
    /// the same place in the new text.
    keep: Option<(String, usize, usize)>,

    lines: Vec<RLine>,
    blocks: Vec<Block>,
    /// The width `lines` was wrapped to (0 before the first layout).
    width: usize,
    /// Index of the first visible rendered line.
    top: usize,

    /// Visible lines at the last draw.
    height: usize,
    /// Where the text was drawn last, for the mouse.
    rendered_area: Rect,
    /// Where the scrollbar was drawn (empty when there was none).
    scrollbar: Rect,
}

impl Doc {
    pub fn new(md: String) -> Doc {
        Doc {
            md,
            base: None,
            site: None,
            headings: Vec::new(),
            links: Vec::new(),
            figures: Vec::new(),
            search: None,
            hints: Vec::new(),
            pending_anchor: None,
            pending_match: None,
            keep: None,
            lines: Vec::new(),
            blocks: Vec::new(),
            width: 0,
            top: 0,
            height: 0,
            rendered_area: Rect::default(),
            scrollbar: Rect::default(),
        }
    }

    /// Replaces the text, keeping the same part of it on screen: the next
    /// draw re-renders it, and finds the line that was at the top.
    pub fn replace(&mut self, md: String) {
        if md == self.md {
            return;
        }
        self.keep = self.place();
        self.md = md;
        self.width = 0;
    }

    /// Where the reader is: the first line with text at or below the top of
    /// the screen, how far below the top it is, and its index.
    pub fn place(&self) -> Option<(String, usize, usize)> {
        self.lines[self.top.min(self.lines.len())..]
            .iter()
            .enumerate()
            .map(|(i, l)| {
                (
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>(),
                    i,
                )
            })
            .find(|(text, _)| !text.trim().is_empty())
            .map(|(text, offset)| (text, offset, self.top + offset))
    }

    /// Goes to a [`Doc::place`] (from this or another document with the
    /// same text) once laid out.
    pub fn keep_place(&mut self, place: (String, usize, usize)) {
        self.keep = Some(place);
        self.width = 0;
    }

    /// Renders it again when next drawn, for a new theme.
    pub fn restyle(&mut self) {
        self.width = 0;
    }

    /// Wraps the rendered view for `width`, keeping the same part of the
    /// document at the top.
    fn layout(&mut self, width: usize, theme: &Theme) {
        if width == self.width {
            return;
        }
        let pos = (!self.lines.is_empty()).then(|| self.rendered_pos(self.top));
        let rendered = render(
            &self.md,
            width,
            theme,
            self.base.as_deref(),
            self.site.as_deref(),
        );
        self.lines = rendered.lines;
        self.headings = rendered.headings;
        self.links = rendered.links;
        self.figures = rendered.figures;
        self.width = width;
        if let Some(query) = self.search.as_ref().map(|s| s.query.clone()) {
            self.find(&query);
        }
        self.blocks = blocks(&self.lines);
        self.top = pos.map_or(0, |p| self.rendered_top_for(p));
        if let Some((text, offset, old)) = self.keep.take() {
            // The same text nearest where it was, if it's still there.
            let same = self.lines.iter().enumerate().filter(|(_, l)| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    == text
            });
            if let Some((i, _)) = same.min_by_key(|(i, _)| i.abs_diff(old)) {
                self.top = i.saturating_sub(offset);
            }
        }
        if let Some(slug) = self.pending_anchor.take() {
            self.go_to_anchor(&slug);
        }
        if let Some((line, query)) = self.pending_match.take() {
            self.go_to_match(line, &query);
        }
    }

    /// The source position at rendered line `i`.
    fn rendered_pos(&self, i: usize) -> f64 {
        if let Some(b) = self.blocks.iter().find(|b| b.r0 <= i && i <= b.r1) {
            let into = (i - b.r0) as f64 / (b.r1 - b.r0 + 1) as f64;
            return b.first as f64 + into * (b.last - b.first + 1) as f64;
        }
        // A blank line between blocks: the line after the previous block.
        self.blocks
            .iter()
            .rev()
            .find(|b| b.r1 < i)
            .map_or(1.0, |b| (b.last + 1) as f64)
    }

    /// The rendered line to put at the top to show source position `pos`.
    fn rendered_top_for(&self, pos: f64) -> usize {
        for b in &self.blocks {
            if pos < b.first as f64 {
                return b.r0;
            }
            if pos < (b.last + 1) as f64 {
                let into = (pos - b.first as f64) / (b.last - b.first + 1) as f64;
                return b.r0 + floor(into * (b.r1 - b.r0 + 1) as f64);
            }
        }
        self.lines.len()
    }

    fn max_top(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
    }

    /// Draws the rendered view into `area`, wrapped to `width` (which may be
    /// less than the area's).
    pub fn draw(&mut self, f: &mut Frame, area: Rect, width: usize, theme: &Theme) {
        self.layout(width.max(1), theme);
        self.height = area.height.into();
        self.top = self.top.min(self.max_top());
        self.draw_rendered(f, area);
    }

    fn draw_rendered(&mut self, f: &mut Frame, area: Rect) {
        self.rendered_area = area;
        let width = usize::from(area.width);
        let visible: Vec<Line> = self.lines[self.top..]
            .iter()
            .take(self.height)
            .enumerate()
            .map(|(i, l)| {
                let line = self.highlighted(self.top + i, l);
                Line::from(line.spans)
            })
            .collect();
        f.render_widget(Paragraph::new(visible), area);
        self.draw_scrollbar(f, area);

        let style = Style::new().black().on_yellow().bold();
        for hint in &self.hints {
            let Some(row) = hint.line.checked_sub(self.top).filter(|&r| r < self.height) else {
                continue;
            };
            if hint.col < width {
                let x = area.x + hint.col as u16;
                f.buffer_mut().set_stringn(
                    x,
                    area.y + row as u16,
                    &hint.label,
                    width - hint.col,
                    style,
                );
            }
        }
    }

    /// A scrollbar in the column right of `area`, when the document is
    /// longer than the screen and there's a free column: where you are, how
    /// much there is, and a tick at each top-level heading.
    fn draw_scrollbar(&mut self, f: &mut Frame, area: Rect) {
        self.scrollbar = Rect::default();
        let total = self.lines.len();
        let h = usize::from(area.height);
        if total <= h || h == 0 || area.right() >= f.area().right() {
            return;
        }
        let bar = Rect {
            x: area.right(),
            width: 1,
            ..area
        };
        self.scrollbar = bar;
        let row_of = |line: usize| (line * h / total).min(h - 1);
        let thumb = row_of(self.top)..=row_of(self.top + h - 1);
        let mut ticks: Vec<usize> = self
            .headings
            .iter()
            .filter(|hd| hd.level <= 2)
            .map(|hd| row_of(hd.line))
            .collect();
        ticks.dedup();
        // Ticks on most rows would say nothing: only show them sparse.
        if ticks.len() > h / 3 {
            ticks.clear();
        }
        let buf = f.buffer_mut();
        for r in 0..h {
            let (symbol, style) = if thumb.contains(&r) {
                ("┃", Style::new())
            } else if ticks.contains(&r) {
                ("├", Style::new().dim())
            } else {
                ("│", Style::new().dim())
            };
            buf[(bar.x, bar.y + r as u16)]
                .set_symbol(symbol)
                .set_style(style);
        }
    }

    /// If (x, y) is on the scrollbar, scrolls to put that point of the
    /// document in the middle of the screen, and returns true.
    pub fn scrollbar_jump(&mut self, x: u16, y: u16) -> bool {
        let bar = self.scrollbar;
        if bar.width == 0 || x != bar.x || y < bar.y || y >= bar.bottom() {
            return false;
        }
        let line = usize::from(y - bar.y) * self.lines.len() / usize::from(bar.height);
        self.top = line.saturating_sub(self.height / 2).min(self.max_top());
        true
    }

    /// The heading of the section at the top of the screen, as an index
    /// into [`Doc::headings`].
    /// The room left for pictures on screen, as drawn last: each one's
    /// size and the row it starts on, which is above the area once it's
    /// scrolled partly off the top; and the area the document was drawn in.
    pub fn figures(&self) -> (Rect, Vec<(Figure, i32)>) {
        let on_screen = self
            .figures
            .iter()
            .map(|f| (*f, f.line as i32 - self.top as i32))
            .filter(|(f, y)| *y < self.height as i32 && y + f.rows as i32 > 0)
            .collect();
        (self.rendered_area, on_screen)
    }

    pub fn current_heading(&self) -> Option<usize> {
        self.headings.iter().rposition(|h| h.line <= self.top)
    }

    /// The headings leading to the section at the top of the screen, from
    /// the outermost: ["Install", "Installing Rust on Windows"].
    pub fn section(&self) -> Vec<&str> {
        let Some(current) = self.current_heading() else {
            return Vec::new();
        };
        let mut trail: Vec<&Heading> = Vec::new();
        for h in &self.headings[..=current] {
            while trail.last().is_some_and(|t| t.level >= h.level) {
                trail.pop();
            }
            trail.push(h);
        }
        trail.iter().map(|h| h.text.as_str()).collect()
    }

    /// Line `i` with any search matches on it highlighted.
    fn highlighted(&self, i: usize, line: &RLine) -> RLine {
        let Some(search) = &self.search else {
            return line.clone();
        };
        let first = search.matches.partition_point(|m| m.0 < i);
        let mut line = line.clone();
        for (n, &(_, start, end)) in search.matches[first..]
            .iter()
            .take_while(|m| m.0 == i)
            .enumerate()
        {
            let style = if search.current == Some(first + n) {
                Style::new().black().on_yellow()
            } else {
                Style::new().reversed()
            };
            line.spans = wrap::restyle(line.spans, start, end, style);
        }
        line
    }

    /// Whether screen position (x, y) is on the text.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.rendered_area
            .contains(ratatui::layout::Position { x, y })
    }

    /// The target of the link at screen position (x, y), if there's one.
    pub fn link_at(&self, x: u16, y: u16) -> Option<String> {
        if !self.contains(x, y) {
            return None;
        }
        let area = self.rendered_area;
        let line = self.lines.get(self.top + usize::from(y - area.y))?;
        let col = usize::from(x - area.x);
        let link = line.links.iter().find(|l| l.start <= col && col < l.end)?;
        self.links.get(link.id as usize).cloned()
    }

    pub fn headings(&self) -> &[Heading] {
        &self.headings
    }

    /// Scrolls the rendered side so line `i` is at the top.
    pub fn jump_to(&mut self, i: usize) {
        self.top = i.min(self.max_top());
    }

    /// Jumps to the next heading below the top of the screen, or the
    /// previous one above it. Returns false if there isn't one.
    pub fn jump_heading(&mut self, forward: bool) -> bool {
        let top = self.top;
        let line = if forward {
            self.headings.iter().map(|h| h.line).find(|&l| l > top)
        } else {
            self.headings
                .iter()
                .map(|h| h.line)
                .rev()
                .find(|&l| l < top)
        };
        line.map(|l| self.jump_to(l)).is_some()
    }

    /// The line of the heading with anchor `slug`.
    pub fn anchor(&self, slug: &str) -> Option<usize> {
        let slug = slug.to_lowercase();
        self.headings
            .iter()
            .find(|h| h.slug == slug)
            .map(|h| h.line)
    }

    /// Jumps to the heading with anchor `slug`, now or, if the document
    /// hasn't been laid out yet, as soon as it is.
    pub fn go_to_anchor(&mut self, slug: &str) {
        if self.width == 0 {
            self.pending_anchor = Some(slug.to_string());
        } else if let Some(line) = self.anchor(slug) {
            self.jump_to(line);
        }
    }

    /// Searches for `query` from source line `line`, highlighting its
    /// matches and jumping to the first there, now or once laid out.
    pub fn go_to_match(&mut self, line: usize, query: &str) {
        if self.width == 0 {
            self.pending_match = Some((line, query.to_string()));
            return;
        }
        let from = self.rendered_top_for(line as f64);
        self.search(query, from, true);
    }

    pub fn top(&self) -> usize {
        self.top
    }

    /// Searches the rendered text for `query` and jumps to the first match
    /// at or after line `from`. Smart case: case matters only if the query
    /// has capitals. Returns false if nothing matches.
    pub fn search(&mut self, query: &str, from: usize, forward: bool) -> bool {
        if query.is_empty() {
            self.search = None;
            return true;
        }
        self.find(query);
        let search = self.search.as_mut().unwrap();
        let matches = &search.matches;
        // The first match from `from` on, or the last one above it,
        // wrapping around if there's none that way.
        let next = if forward {
            matches
                .iter()
                .position(|m| m.0 >= from)
                .or((!matches.is_empty()).then_some(0))
        } else {
            matches
                .iter()
                .rposition(|m| m.0 < from)
                .or(matches.len().checked_sub(1))
        };
        search.current = next;
        if let Some(i) = next {
            let line = search.matches[i].0;
            self.reveal(line);
        }
        next.is_some()
    }

    /// Moves to the next (or previous) match, wrapping around.
    pub fn search_next(&mut self, forward: bool) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };
        let n = search.matches.len();
        if n == 0 {
            return false;
        }
        let i = match search.current {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        };
        search.current = Some(i);
        let line = search.matches[i].0;
        self.reveal(line);
        true
    }

    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// "3/17" for the current match, or "0/0".
    pub fn search_status(&self) -> Option<String> {
        let s = self.search.as_ref()?;
        let current = s.current.map_or(0, |i| i + 1);
        Some(format!("{current}/{}", s.matches.len()))
    }

    /// Scrolls so line `i` is visible, a third of the way down if it wasn't.
    fn reveal(&mut self, i: usize) {
        if i < self.top || i >= self.top + self.height {
            self.top = i.saturating_sub(self.height / 3).min(self.max_top());
        }
    }

    fn find(&mut self, query: &str) {
        let case = query.chars().any(char::is_uppercase);
        let fold = |c: char| {
            if case {
                c
            } else {
                c.to_lowercase().next().unwrap_or(c)
            }
        };
        let needle: Vec<char> = query.chars().map(fold).collect();
        let mut matches = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let chars: Vec<char> = text.chars().collect();
            let mut col = 0;
            let mut cols = Vec::with_capacity(chars.len() + 1);
            for &c in &chars {
                cols.push(col);
                col += wrap::width(c.encode_utf8(&mut [0; 4]));
            }
            cols.push(col);
            let mut at = 0;
            while at + needle.len() <= chars.len() {
                if chars[at..at + needle.len()]
                    .iter()
                    .map(|&c| fold(c))
                    .eq(needle.iter().copied())
                {
                    matches.push((i, cols[at], cols[at + needle.len()]));
                    at += needle.len().max(1);
                } else {
                    at += 1;
                }
            }
        }
        let current = self.search.as_ref().and_then(|s| s.current);
        self.search = Some(Search {
            query: query.to_string(),
            current: current.filter(|&c| c < matches.len()),
            matches,
        });
    }

    /// The links on screen in the rendered view, top to bottom.
    pub fn visible_links(&self) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for (i, line) in self
            .lines
            .iter()
            .enumerate()
            .skip(self.top)
            .take(self.height)
        {
            for l in &line.links {
                out.push((i, l.start, self.links[l.id as usize].clone()));
            }
        }
        out
    }

    pub fn set_hints(&mut self, hints: Vec<(String, usize, usize, String)>) {
        self.hints = hints
            .into_iter()
            .map(|(label, line, col, url)| Hint {
                label,
                line,
                col,
                url,
            })
            .collect();
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.top = self.top.saturating_add_signed(delta).min(self.max_top());
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_by(isize::MIN / 2);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_by(isize::MAX / 2);
    }

    pub fn page(&self) -> isize {
        self.height.max(1) as isize
    }

    /// "Top", "Bot", "All" or a percentage, like less and vim.
    pub fn position(&self) -> String {
        let (top, len) = (self.top, self.lines.len());
        if len <= self.height {
            "All".into()
        } else if top == 0 {
            "Top".into()
        } else if top >= len - self.height {
            "Bot".into()
        } else {
            format!("{}%", (top + self.height) * 100 / len)
        }
    }
}

/// Groups rendered lines into the top-level blocks they came from.
fn blocks(lines: &[RLine]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((first, last)) = line.src else {
            continue;
        };
        match out.last_mut() {
            Some(b) if b.first == first && b.r1 + 1 == i => b.r1 = i,
            _ => out.push(Block {
                first,
                last,
                r0: i,
                r1: i,
            }),
        }
    }
    out
}

/// Rounds down, but treats 2.9999999 as the 3 it's meant to be.
fn floor(x: f64) -> usize {
    (x + 1e-9) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nOne paragraph that is long enough to wrap across several rendered lines at a narrow width.\n\nlast\n";

    fn laid_out(width: usize) -> Doc {
        let theme = Theme::plain();
        let mut doc = Doc::new(MD.into());
        doc.layout(width, &theme);
        doc.height = 1;
        doc
    }

    /// The article arriving above the comments mustn't move the comment
    /// being read.
    #[test]
    fn replacing_the_text_keeps_the_same_line_on_screen() {
        let theme = Theme::plain();
        let comments = "## Comments\n\nfirst comment\n\nsecond comment\n\nthird comment\n";
        let mut doc = Doc::new(format!("# Story\n\n*Loading…*\n\n{comments}"));
        doc.layout(40, &theme);
        doc.height = 3;
        let second = doc.lines.iter().position(|l| l.text() == "second comment").unwrap();
        doc.jump_to(second);

        let article = "A paragraph of the article.\n\n".repeat(30);
        doc.replace(format!("# Story\n\n{article}{comments}"));
        doc.layout(40, &theme);
        assert_eq!(doc.lines[doc.top].text(), "second comment");
        assert!(doc.top > second + 30);
    }

    #[test]
    fn searches_forward_or_back_from_a_line() {
        let theme = Theme::plain();
        let mut doc = Doc::new("x\n\nx\n\nx\n\nx\n".into());
        doc.layout(40, &theme);
        doc.height = 1;
        // Matches on lines 0, 2, 4 and 6: from line 3, the next is on 4 and
        // the one before on 2.
        assert!(doc.search("x", 3, true));
        assert_eq!(doc.search_status().unwrap(), "3/4");
        assert!(doc.search("x", 3, false));
        assert_eq!(doc.search_status().unwrap(), "2/4");
        // Nothing that way: wrap around.
        assert!(doc.search("x", 100, true));
        assert_eq!(doc.search_status().unwrap(), "1/4");
        assert!(doc.search("x", 0, false));
        assert_eq!(doc.search_status().unwrap(), "4/4");
    }

    #[test]
    fn maps_rendered_lines_to_source_and_back() {
        let doc = laid_out(20);
        for i in 0..doc.lines.len() {
            let pos = doc.rendered_pos(i);
            let back = doc.rendered_top_for(pos);
            if doc.lines[i].src.is_some() {
                assert_eq!(back, i, "line {i} at {pos}");
            }
        }
        // The table starts on source line 3.
        let table = doc
            .lines
            .iter()
            .position(|l| l.src == Some((3, 5)))
            .unwrap();
        assert_eq!(doc.rendered_pos(table), 3.0);
    }

    #[test]
    fn knows_the_section_at_the_top() {
        let md =
            "# Guide\n\nintro\n\n## Install\n\ntext\n\n### On Windows\n\nmore\n\n## Usage\n\nend\n";
        let mut doc = Doc::new(md.into());
        doc.layout(80, &Theme::plain());
        doc.height = 1;
        assert_eq!(doc.section(), ["Guide"]);
        let windows = doc.headings[2].line;
        doc.top = windows + 2;
        assert_eq!(doc.section(), ["Guide", "Install", "On Windows"]);
        doc.top = doc.headings[3].line;
        assert_eq!(doc.section(), ["Guide", "Usage"]);
    }
}
