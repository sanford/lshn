//! An open document: its text, rendered, and where it's scrolled to.
//!
//! Re-wrapped at a new width, it keeps the same part on screen through
//! source line numbers: each rendered line knows which top-level block (and
//! so which source lines) it came from. A position is a fractional source
//! line, so a 1-line table that renders as 10 lines, or a paragraph that
//! wraps to 5, stays in place.

use crate::render::{CommentMark, Figure, Heading, Join, RLine, render};
use crate::theme::Theme;
use crate::wrap;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::HashSet;
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
    /// Each comment's own lines, not its replies'.
    comments: Vec<CommentSpan>,
    /// Lines of the title, to draw twice the size (see `sizing`).
    big: Vec<usize>,
    /// The comment the cursor's on, by id, so it stays put when the thread
    /// changes; `None` in the article above.
    cursor: Option<u64>,
    /// The link in the cursor's comment `j` and `k` are on: the comment's
    /// id, and which of its [`LinkStop`]s. One past the last is after
    /// them all, come up into it from below with the last off screen.
    link: Option<(u64, usize)>,
    /// `Esc` let go of the link, but `j` and `k` go on from it.
    link_hidden: bool,
    /// In the article, the line `j` and `k` have taken the band to, and
    /// which of the links starting on it.
    band_at: Option<(usize, usize)>,
    /// Across a re-render, the row on screen the cursor's comment was on.
    cursor_row: Option<usize>,
    /// How the cursor's comment is shown, when it is.
    focus_style: Option<FocusStyle>,
    search: Option<Search>,
    /// Link hints on screen, while choosing a link to follow.
    pub hints: Vec<Hint>,
    /// A heading to jump to once the document is laid out.
    pending_anchor: Option<String>,
    /// A comment to put the cursor on once it's there: its thread may
    /// still be coming.
    pending_comment: Option<u64>,
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
    /// Text selected with the mouse, to copy: where the drag started and
    /// where it is.
    pub selection: Option<(Spot, Spot)>,
}

/// A place in the text: a rendered line, and a column on it.
pub type Spot = (usize, usize);

impl Doc {
    pub fn new(md: String) -> Doc {
        Doc {
            md,
            base: None,
            site: None,
            headings: Vec::new(),
            links: Vec::new(),
            figures: Vec::new(),
            big: Vec::new(),
            comments: Vec::new(),
            cursor: None,
            link: None,
            link_hidden: false,
            band_at: None,
            cursor_row: None,
            focus_style: None,
            search: None,
            hints: Vec::new(),
            pending_anchor: None,
            pending_comment: None,
            pending_match: None,
            keep: None,
            lines: Vec::new(),
            blocks: Vec::new(),
            width: 0,
            top: 0,
            height: 0,
            rendered_area: Rect::default(),
            scrollbar: Rect::default(),
            selection: None,
        }
    }

    /// Replaces the text, keeping the same part of it on screen: the next
    /// draw re-renders it, and finds the line that was at the top.
    pub fn replace(&mut self, md: String) {
        if md == self.md {
            return;
        }
        self.keep = self.place();
        // The comment the cursor's on, where it was on screen: a fold, or
        // new comments above, mustn't move it.
        self.cursor_row = self
            .cursor_index()
            .and_then(|i| self.comments[i].start.checked_sub(self.top))
            .filter(|&row| row < self.height);
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
        self.big = rendered.big;
        self.selection = None;
        self.comments = comment_spans(&rendered.comments, &self.lines);
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
        if let (Some(row), Some(i)) = (self.cursor_row.take(), self.cursor_index()) {
            self.top = self.comments[i]
                .start
                .saturating_sub(row)
                .min(self.max_top());
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
    /// Links to the pages in `visited` are dimmed.
    pub fn draw(
        &mut self,
        f: &mut Frame,
        area: Rect,
        width: usize,
        theme: &Theme,
        visited: &HashSet<String>,
    ) {
        self.layout(width.max(1), theme);
        self.height = area.height.into();
        self.top = self.top.min(self.max_top());
        self.place_pending_comment();
        self.follow_view();
        self.draw_rendered(f, area, visited);
    }

    fn draw_rendered(&mut self, f: &mut Frame, area: Rect, visited: &HashSet<String>) {
        self.rendered_area = area;
        let width = usize::from(area.width);
        let link = self.focused_link().map(|s| s.id);
        let visible: Vec<Line> = self.lines[self.top..]
            .iter()
            .take(self.height)
            .enumerate()
            .map(|(i, l)| {
                let line = self.highlighted(self.top + i, l, link, visited);
                Line::from(line.spans)
            })
            .collect();
        f.render_widget(Paragraph::new(visible), area);
        self.draw_scrollbar(f, area);
        if let Some((from, to)) = self.selected() {
            let bottom = self.top + self.height;
            for line in from.0.max(self.top)..=to.0.min(bottom.saturating_sub(1)) {
                // From the text, not the bars beside it.
                let start = if line == from.0 {
                    from.1
                } else {
                    self.lines[line].front
                };
                let end = if line == to.0 {
                    to.1 + 1
                } else {
                    wrap::spans_width(&self.lines[line].spans)
                };
                let end = end.min(width);
                if start < end {
                    let row = Rect::new(
                        area.x + start as u16,
                        area.y + (line - self.top) as u16,
                        (end - start) as u16,
                        1,
                    );
                    f.buffer_mut()
                        .set_style(row, Style::new().add_modifier(Modifier::REVERSED));
                }
            }
        }
        // Before the link hints, which a big title would hide.
        for &line in &self.big {
            let Some(row) = line.checked_sub(self.top) else {
                continue;
            };
            if row + 1 < self.height && line + 1 < self.lines.len() {
                let cols = wrap::spans_width(&self.lines[line].spans);
                let y = area.y + row as u16;
                crate::sizing::place(f.buffer_mut(), area.x, y, cols as u16);
            }
        }

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

    /// Line `i` with any search matches on it highlighted, the link `j`
    /// and `k` are on, or the band's on in the article (by id), reversed, and links already opened dimmed.
    fn highlighted(
        &self,
        i: usize,
        line: &RLine,
        link: Option<u32>,
        visited: &HashSet<String>,
    ) -> RLine {
        let mut line = line.clone();
        if let (Some(style), Some(c)) = (self.focus_style, self.focused())
            && (c.start..c.end).contains(&i)
        {
            line.spans = focus_bars(line.spans, c.depth * 2, style.accent);
            if i == c.start {
                line.spans.push(Span::styled(
                    "   r reply · v upvote · space fold",
                    Style::new().dim(),
                ));
            }
            if let Some(band) = style.band {
                line.spans = banded(line.spans, c.depth * 2, self.width, band);
            }
        }
        for l in &line.links {
            let style = if Some(l.id) == link && self.focus_style.is_some() {
                Style::new().add_modifier(Modifier::REVERSED)
            } else if visited.contains(&self.links[l.id as usize]) {
                Style::new().add_modifier(Modifier::DIM)
            } else {
                continue;
            };
            line.spans = wrap::restyle(line.spans, l.start, l.end, style);
        }
        let Some(search) = &self.search else {
            return line;
        };
        let first = search.matches.partition_point(|m| m.0 < i);
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

    /// The spot at screen position (x, y), or the nearest on the text:
    /// above it, the top line; below it, the bottom one.
    pub fn spot_at(&self, x: u16, y: u16) -> Spot {
        let area = self.rendered_area;
        let row = y.clamp(area.y, area.bottom().saturating_sub(1)) - area.y;
        let last = self.lines.len().saturating_sub(1);
        let line = (self.top + usize::from(row)).min(last);
        (line, usize::from(x.saturating_sub(area.x)))
    }

    /// Selects from where a drag started to (x, y), scrolling a line
    /// when it's gone past the top or bottom.
    pub fn select_to(&mut self, from: Spot, x: u16, y: u16) {
        let area = self.rendered_area;
        if y < area.y {
            self.scroll_by(-1);
        } else if y >= area.bottom() {
            self.scroll_by(1);
        }
        self.selection = Some((from, self.spot_at(x, y)));
    }

    /// Whether line `i` has any text of its own, not just bars.
    fn has_text(&self, i: usize) -> bool {
        let line = &self.lines[i];
        !self
            .column_text(i, line.body.0, line.body.1)
            .trim()
            .is_empty()
    }

    /// The line with text `by` lines with text on from line `i`: up with a
    /// negative `by`; with 0, `i` or the nearest after it (or before, at
    /// the end). As far as there is.
    pub fn text_line(&self, i: usize, by: isize) -> usize {
        let last = self.lines.len().saturating_sub(1);
        let mut at = i.min(last);
        if by == 0 {
            return (at..=last)
                .find(|&l| self.has_text(l))
                .or_else(|| (0..at).rev().find(|&l| self.has_text(l)))
                .unwrap_or(at);
        }
        for _ in 0..by.unsigned_abs() {
            let next = if by > 0 {
                (at + 1..=last).find(|&l| self.has_text(l))
            } else {
                (0..at).rev().find(|&l| self.has_text(l))
            };
            match next {
                Some(l) => at = l,
                None => break,
            }
        }
        at
    }

    /// Where selecting with the keys starts: the first line of the comment
    /// the cursor's on, after its header, or the first on screen.
    pub fn select_start(&self) -> Option<usize> {
        let from = self.focused().map_or(self.top, |c| c.start + 1);
        let line = self.text_line(from, 0);
        (!self.lines.is_empty() && self.has_text(line)).then_some(line)
    }

    /// Selects lines `a` to `b`, either way round, whole, and shows `b`.
    pub fn select_lines(&mut self, a: usize, b: usize) {
        const END: usize = usize::MAX / 2;
        self.selection = Some(((a.min(b), 0), (a.max(b), END)));
        let height = self.height.max(1);
        if b < self.top {
            self.jump_to(b);
        } else if b >= self.top + height {
            self.jump_to((b + 1).saturating_sub(height));
        }
    }

    /// The selection, its first spot first.
    fn selected(&self) -> Option<(Spot, Spot)> {
        let (a, b) = self.selection?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected text, to copy.
    pub fn selected_text(&self) -> Option<String> {
        let (from, to) = self.selected()?;
        let text = self.text_between(from, (to.0, to.1 + 1));
        (!text.trim().is_empty()).then_some(text)
    }

    /// The text from spot `from` to just before `to`, as plain text: no
    /// quote bars, paragraphs whole again, and links that show an address
    /// (cut short, often) as their whole address.
    pub fn text_between(&self, from: Spot, to: Spot) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut link = None;
        let last = to.0.min(self.lines.len().saturating_sub(1));
        for i in from.0..=last {
            let line = &self.lines[i];
            let start = if i == from.0 { from.1 } else { 0 };
            let end = if i == to.0 { to.1 } else { usize::MAX };
            let (body, body_end) = line.body;
            let text = self.columns(i, start.max(body), end.min(body_end), &mut link);
            let text = text.trim_end();
            match (line.join, out.last_mut()) {
                (Join::Space, Some(prev)) if i > from.0 => {
                    if !prev.is_empty() && !text.is_empty() {
                        prev.push(' ');
                    }
                    prev.push_str(text.trim_start());
                }
                (Join::Direct, Some(prev)) if i > from.0 => prev.push_str(text),
                _ => {
                    // What's in front, less the quote bars: a list's marker.
                    // Started partway in, as indented as the line's text,
                    // to be only as indented as the rest in the end.
                    let lead = if start > line.front {
                        " ".repeat(line.front)
                    } else {
                        self.columns(i, start, line.front.min(end), &mut None)
                    };
                    out.push(lead.replace('│', " ") + text);
                }
            }
        }
        tidy(out)
    }

    /// Line `i`'s text from column `start` to just before `end`, with a
    /// link that shows an address given as the address it goes to: once,
    /// though it's wrapped over lines, which `link`, the last one given,
    /// keeps track of.
    fn columns(&self, i: usize, start: usize, end: usize, link: &mut Option<u32>) -> String {
        use unicode_width::UnicodeWidthChar;
        let line = &self.lines[i];
        let shown = |l: &crate::wrap::LinkSpan| {
            let text = self.column_text(i, l.start, l.end);
            let target = self.links.get(l.id as usize)?;
            let address = text.contains("://") || text.starts_with("www.");
            (address && target.starts_with("http")).then_some(target)
        };
        let mut out = String::new();
        let mut col = 0;
        for c in line.spans.iter().flat_map(|s| s.content.chars()) {
            let here = col;
            col += c.width().unwrap_or(0);
            if here < start || here >= end {
                continue;
            }
            match line.links.iter().find(|l| (l.start..l.end).contains(&here)) {
                Some(l) if shown(l).is_some() || *link == Some(l.id) => {
                    if *link != Some(l.id)
                        && let Some(target) = shown(l)
                    {
                        out.push_str(target);
                    }
                    *link = Some(l.id);
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// Line `i`'s text from column `start` to just before `end`, as shown.
    fn column_text(&self, i: usize, start: usize, end: usize) -> String {
        use unicode_width::UnicodeWidthChar;
        let mut col = 0;
        let mut out = String::new();
        for c in self.lines[i].spans.iter().flat_map(|s| s.content.chars()) {
            if (start..end).contains(&col) {
                out.push(c);
            }
            col += c.width().unwrap_or(0);
        }
        out
    }

    /// The text of the comment the cursor's on, without its header.
    pub fn focused_text(&self) -> Option<String> {
        let c = self.focused()?;
        let text = self.text_between((c.start + 1, 0), (c.end.checked_sub(1)?, usize::MAX));
        (!text.trim().is_empty()).then_some(text)
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

    fn cursor_index(&self) -> Option<usize> {
        let id = self.cursor?;
        self.comments.iter().position(|c| c.id == id)
    }

    /// The comment the cursor's on.
    pub fn focused(&self) -> Option<CommentSpan> {
        self.cursor_index().map(|i| self.comments[i])
    }

    /// How to show the cursor's comment, or not to.
    pub fn show_focus(&mut self, style: Option<FocusStyle>) {
        self.focus_style = style;
    }

    /// The links in comment `c`'s text, in order, one for each page: not
    /// the ones in its header (the author, the age).
    fn link_stops(&self, c: &CommentSpan) -> Vec<LinkStop> {
        let Some(body) = (c.start..c.end).find(|&i| blank(&self.lines[i])) else {
            return Vec::new();
        };
        let mut stops: Vec<LinkStop> = Vec::new();
        for i in body..c.end {
            for l in &self.lines[i].links {
                let url = &self.links[l.id as usize];
                if let Some(s) = stops.iter_mut().find(|s| s.id == l.id) {
                    s.last = i;
                } else if !stops.iter().any(|s| &s.url == url) {
                    stops.push(LinkStop {
                        id: l.id,
                        first: i,
                        last: i,
                        url: url.clone(),
                    });
                }
            }
        }
        stops
    }

    /// The link `j` and `k` are on, in the comment the cursor's on; in the
    /// article, the one the reading band's on.
    pub fn focused_link(&self) -> Option<LinkStop> {
        if self.cursor.is_none() {
            return self.band_link();
        }
        if self.link_hidden {
            return None;
        }
        let c = self.focused()?;
        let (id, n) = self.link?;
        (id == c.id).then(|| self.link_stops(&c).into_iter().nth(n))?
    }

    /// Back from a link to the comment it's in, `j` and `k` going on from
    /// where it was. Returns false if no link was chosen: the article's
    /// band isn't let go of, it moves on as the article scrolls.
    pub fn clear_link(&mut self) -> bool {
        let had = self.cursor.is_some() && self.focused_link().is_some();
        self.link_hidden = true;
        had
    }

    /// In the article, the link the reading band's on.
    fn band_link(&self) -> Option<LinkStop> {
        let (line, n) = self.band_place()?;
        let mut stops = self.band_stops(line);
        Some(stops.swap_remove(n.min(stops.len() - 1)))
    }

    /// Where the band's on in the article: a line with links starting on
    /// it, and which of them. The one `j` and `k` came to, while it's near
    /// the band; or else the line nearest it, at its first link.
    fn band_place(&self) -> Option<(usize, usize)> {
        let (band, near) = self.band_window()?;
        if let Some((line, n)) = self.band_at
            && near.contains(&line)
            && self.starts_links(line)
        {
            return Some((line, n));
        }
        let line = near
            .filter(|&i| self.starts_links(i))
            .min_by_key(|&i| i.abs_diff(band))?;
        Some((line, 0))
    }

    /// In the article, the line the band's on and the lines near enough
    /// it to count, on screen and above the comments: all of them when the
    /// article fits on the screen, where the band doesn't move, for `j`
    /// and `k` to go to each link in turn.
    fn band_window(&self) -> Option<(usize, std::ops::Range<usize>)> {
        /// How far from the band a link can be: far enough that one
        /// scroll never carries a link past it unseen.
        const REACH: usize = 2;
        let end = self.comments.first().map_or(self.lines.len(), |c| c.start);
        let band = self.band_line(end)?;
        let bottom = (self.top + self.height).min(end);
        if end <= self.height {
            return Some((band, self.top..bottom));
        }
        let from = self.top.max(band.saturating_sub(REACH));
        Some((band, from..bottom.min(band + REACH + 1)))
    }

    /// Whether line `i` has links on it, links wrapped onto it from the
    /// line above aside: those start there, or at the top of the screen.
    fn starts_links(&self, i: usize) -> bool {
        self.lines[i].links.iter().any(|l| self.starts_on(i, l.id))
    }

    fn starts_on(&self, i: usize, id: u32) -> bool {
        i == self.top || !self.lines[i - 1].links.iter().any(|l| l.id == id)
    }

    /// The links starting on line `i`, in order, each page once: a
    /// story's points, age and comments all go to its HN page.
    fn band_stops(&self, i: usize) -> Vec<LinkStop> {
        let bottom = (self.top + self.height).min(self.lines.len());
        let mut stops: Vec<LinkStop> = Vec::new();
        for l in &self.lines[i].links {
            let url = &self.links[l.id as usize];
            if !self.starts_on(i, l.id) || stops.iter().any(|s| s.id == l.id || &s.url == url) {
                continue;
            }
            let mut last = i;
            while last + 1 < bottom && self.lines[last + 1].links.iter().any(|m| m.id == l.id) {
                last += 1;
            }
            stops.push(LinkStop {
                id: l.id,
                first: i,
                last,
                url: url.clone(),
            });
        }
        stops
    }

    /// The next line of links after `from` (or before, going back) that's
    /// near the band; from nowhere, the first (or last) of them.
    fn next_band_line(&self, forward: bool, from: Option<usize>) -> Option<usize> {
        let (_, near) = self.band_window()?;
        if forward {
            let start = from.map_or(near.start, |l| (l + 1).max(near.start));
            (start..near.end).find(|&i| self.starts_links(i))
        } else {
            let end = from.map_or(near.end, |l| l.min(near.end));
            (near.start..end).rev().find(|&i| self.starts_links(i))
        }
    }

    /// Puts the band on line `i`: at its first link, or coming up to it,
    /// its last.
    fn band_onto(&mut self, i: usize, forward: bool) {
        let n = if forward {
            0
        } else {
            self.band_stops(i).len() - 1
        };
        self.band_at = Some((i, n));
    }

    /// `j` or `k` in the article: onto the next or previous link near the
    /// band, along its line and then on to the next line of them, so none
    /// is skipped. Returns false when there's none, to scroll on.
    fn band_step(&mut self, forward: bool) -> bool {
        let place = self.band_place();
        if let Some((line, n)) = place {
            let next = if forward { n + 1 } else { n.wrapping_sub(1) };
            if next < self.band_stops(line).len() {
                self.band_at = Some((line, next));
                return true;
            }
        }
        match self.next_band_line(forward, place.map(|p| p.0)) {
            Some(i) => {
                self.band_onto(i, forward);
                true
            }
            None => false,
        }
    }

    /// The line the reading band's on, in an article `end` lines long. It
    /// reads a third of the way down the screen, but starts at the top and
    /// ends at the bottom, sliding to and from there over the first and
    /// last screenful of scrolling, so every line passes through it.
    fn band_line(&self, end: usize) -> Option<usize> {
        let last = end.checked_sub(1)?;
        let height = self.height.max(1);
        // How far the article scrolls before its last line's at the bottom.
        let scroll = end.saturating_sub(height);
        let top = self.top;
        if top >= scroll {
            return Some(if scroll == 0 { top } else { last });
        }
        let row = height / 3;
        let slide = height - 1 - row;
        Some(if scroll < row + slide {
            // Too short to settle: it slides the whole way.
            top * last / scroll
        } else if top < row {
            top * 2
        } else if top + slide > scroll {
            top + row + (top + slide - scroll)
        } else {
            top + row
        })
    }

    /// `j` or `k`: in the article, a line, or along a line of links in the
    /// reading band; among the comments, the next or
    /// previous one, or in one with links, each of its links in turn
    /// instead, scrolling only as far as it takes to show it whole. One taller than
    /// the screen is scrolled through first. Scrolling goes `lines` at a
    /// time.
    pub fn step(&mut self, forward: bool, lines: usize) {
        let by = lines.max(1) as isize;
        let bottom = self.top + self.height;
        self.link_hidden = false;
        let Some(i) = self.cursor_index() else {
            // Along the line of links in the band first.
            if self.band_step(forward) {
                return;
            }
            // The first comment on screen takes the cursor.
            let first = self
                .comments
                .iter()
                .position(|c| c.start >= self.top && c.start < bottom);
            match first {
                Some(i) if forward => self.enter(i, false),
                _ => {
                    // Scrolled, the band goes on from the line it was on.
                    let from = self.band_place().map(|p| p.0);
                    self.scroll_by(if forward { by } else { -by });
                    if let Some(i) = self.next_band_line(forward, from) {
                        self.band_onto(i, forward);
                    }
                }
            }
            return;
        };
        let c = self.comments[i];
        let stops = self.link_stops(&c);
        let at = self
            .link
            .filter(|l| l.0 == c.id)
            .map(|l| l.1.min(stops.len()));
        if forward {
            // The next link not scrolled past, once it's all on screen.
            let next = (at.map_or(0, |n| n + 1)..stops.len()).find(|&n| stops[n].first >= self.top);
            if let Some(n) = next {
                if stops[n].last < bottom {
                    self.link = Some((c.id, n));
                } else {
                    self.scroll_by(by);
                }
                return;
            }
            if c.end > bottom || i + 1 == self.comments.len() {
                self.scroll_by(by);
            } else {
                self.enter(i + 1, false);
            }
            return;
        }
        if let Some(at) = at {
            // The link before, once it's on screen; before the first, on
            // as from the comment.
            match (0..at).rev().find(|&n| stops[n].last < bottom) {
                Some(n) if stops[n].first >= self.top => return self.link = Some((c.id, n)),
                Some(_) => return self.scroll_by(-by),
                None => self.link = None,
            }
        }
        if c.start < self.top {
            self.scroll_by(-by);
        } else if i == 0 {
            // Back up into the article.
            self.cursor = None;
            self.scroll_by(-by);
        } else {
            self.enter(i - 1, true);
        }
    }

    /// Puts the cursor on comment `i` and onto its first link, or coming up
    /// from below, its last, if it shows. Coming up, if the last is above
    /// the screen, after it, for `k` to scroll up to it.
    fn enter(&mut self, i: usize, from_below: bool) {
        self.move_cursor(i, !from_below);
        let c = self.comments[i];
        let stops = self.link_stops(&c);
        let bottom = self.top + self.height;
        let shows = |s: &LinkStop| s.first >= self.top && s.last < bottom;
        self.link = match (from_below, stops.first(), stops.last()) {
            (false, Some(first), _) if shows(first) => Some((c.id, 0)),
            (true, _, Some(last)) => {
                Some((c.id, stops.len() - usize::from(last.first >= self.top)))
            }
            _ => None,
        };
    }

    /// Puts the cursor on comment `i`, scrolling as little as shows it: all
    /// of it if it fits, or else its top coming down, its end going up.
    fn move_cursor(&mut self, i: usize, down: bool) {
        const ABOVE: usize = 1;
        const BELOW: usize = 2;
        self.cursor = Some(self.comments[i].id);
        self.link = None;
        self.link_hidden = false;
        let (start, end) = (self.comments[i].start, self.comments[i].end);
        let room = self.height.saturating_sub(ABOVE + BELOW).max(1);
        let top = if end - start > room {
            if down {
                start.saturating_sub(ABOVE)
            } else {
                (end + BELOW).saturating_sub(self.height).min(self.top)
            }
        } else if start < self.top + ABOVE {
            start.saturating_sub(ABOVE)
        } else if end + BELOW > self.top + self.height {
            (end + BELOW).saturating_sub(self.height)
        } else {
            self.top
        };
        self.top = top.min(self.max_top());
    }

    /// After scrolling some other way (a page, the mouse, a search): the
    /// cursor onto the comments on screen, if it's left them.
    fn follow_view(&mut self) {
        let (top, bottom) = (self.top, self.top + self.height);
        let on_screen = |c: &CommentSpan| c.start < bottom && c.end > top;
        match self.cursor_index() {
            Some(i) if on_screen(&self.comments[i]) => {}
            None if self.comments.first().is_none_or(|c| c.start > top) => {}
            _ => {
                let seen = self
                    .comments
                    .iter()
                    .find(|c| c.start >= top && c.start < bottom);
                let here = seen.or_else(|| self.comments.iter().find(|c| on_screen(c)));
                self.cursor = here.map(|c| c.id);
            }
        }
        // A link scrolled off screen is let go.
        if self
            .focused_link()
            .is_some_and(|s| s.first < top || s.last >= bottom)
        {
            self.link = None;
        }
    }

    /// Moves to the next or previous comment (only top-level ones, unless
    /// `replies`), or in the article, heading. Returns false if there's
    /// nothing that way.
    pub fn jump(&mut self, forward: bool, replies: bool) -> bool {
        let at = self.cursor_index();
        let from = at.map_or(self.top, |i| self.comments[i].start);
        let comments = self
            .comments
            .iter()
            .enumerate()
            .filter(|(_, c)| replies || c.depth == 0)
            .map(|(i, c)| (c.start, Some(i)));
        // Top-level comments are headings too: one target each.
        let headings = self
            .headings
            .iter()
            .filter(|h| !self.comments.iter().any(|c| c.start == h.line))
            .map(|h| (h.line, None));
        let targets = comments.chain(headings);
        let target = if forward {
            targets.filter(|t| t.0 > from).min()
        } else {
            targets.filter(|t| t.0 < from).max()
        };
        match target {
            Some((_, Some(i))) => {
                self.enter(i, false);
                true
            }
            Some((line, None)) => {
                self.cursor = None;
                self.jump_to(line);
                true
            }
            None => false,
        }
    }

    /// Scrolls the rendered side so line `i` is at the top.
    pub fn jump_to(&mut self, i: usize) {
        self.top = i.min(self.max_top());
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
    /// Puts the cursor on comment `id`, a little way down the screen with
    /// what it answers above, now or once it's there.
    pub fn go_to_comment(&mut self, id: u64) {
        self.pending_comment = Some(id);
    }

    /// The comment the cursor's to go to, once it's there.
    pub fn pending_comment(&self) -> Option<u64> {
        self.pending_comment
    }

    /// Stops waiting for comment it was to go to: it isn't coming. To the
    /// comments instead.
    pub fn give_up_comment(&mut self, comments: Option<usize>) {
        if self.pending_comment.take().is_some()
            && let Some(line) = comments
        {
            self.jump_to(line);
        }
    }

    fn place_pending_comment(&mut self) {
        let Some(id) = self.pending_comment else {
            return;
        };
        let Some(c) = self.comments.iter().find(|c| c.id == id).copied() else {
            return;
        };
        self.pending_comment = None;
        self.cursor = Some(id);
        self.top = c.start.saturating_sub(self.height / 4).min(self.max_top());
        let bottom = self.top + self.height;
        let first = self.link_stops(&c).into_iter().next();
        self.link = first
            .filter(|s| s.first >= self.top && s.last < bottom)
            .map(|_| (id, 0));
        self.link_hidden = false;
    }

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

/// How the cursor's comment stands out: its bars in `accent`, and behind
/// it, if there's one, a `band` of color.
#[derive(Clone, Copy, Debug)]
pub struct FocusStyle {
    pub accent: Style,
    pub band: Option<ratatui::style::Color>,
}

/// `spans` with `color` behind them from column `from`, out to `width`.
fn banded(
    spans: Vec<Span<'static>>,
    from: usize,
    width: usize,
    color: ratatui::style::Color,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    let mut col = 0;
    for span in spans {
        let w = wrap::width(&span.content);
        if col + w <= from {
            out.push(span);
        } else if col >= from {
            out.push(Span::styled(span.content, span.style.bg(color)));
        } else {
            // Split where the band starts.
            let mut left = String::new();
            let mut right = String::new();
            let mut at = col;
            for ch in span.content.chars() {
                if at < from {
                    left.push(ch)
                } else {
                    right.push(ch)
                }
                at += wrap::width(ch.encode_utf8(&mut [0; 4]));
            }
            out.push(Span::styled(left, span.style));
            out.push(Span::styled(right, span.style.bg(color)));
        }
        col += w;
    }
    if col < width {
        let pad = " ".repeat(width - col.max(from));
        if col < from {
            out.push(Span::raw(" ".repeat(from - col)));
        }
        out.push(Span::styled(pad, Style::new().bg(color)));
    }
    out
}

/// A comment's own lines: from its first to its last with text, before
/// its replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommentSpan {
    pub id: u64,
    pub depth: usize,
    pub start: usize,
    pub end: usize,
}

/// A link in a comment's text, for `j` and `k` to stop on, or in the
/// article, in the reading band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkStop {
    /// Its id in the document's links.
    id: u32,
    /// The rendered lines it's on: more than one if it wraps.
    first: usize,
    last: usize,
    pub url: String,
}

/// Whether a line in the comments is empty, but for quote bars.
fn blank(l: &RLine) -> bool {
    l.spans
        .iter()
        .flat_map(|s| s.content.chars())
        .all(|c| c == '│' || c.is_whitespace())
}

fn comment_spans(marks: &[CommentMark], lines: &[RLine]) -> Vec<CommentSpan> {
    marks
        .iter()
        .enumerate()
        .map(|(n, m)| {
            let mut end = marks.get(n + 1).map_or(lines.len(), |next| next.line);
            while end > m.line + 1 && lines.get(end - 1).is_some_and(blank) {
                end -= 1;
            }
            CommentSpan {
                id: m.id,
                depth: m.depth,
                start: m.line,
                end,
            }
        })
        .collect()
}

/// Copied lines as text: indented only as much as they are more than the
/// least, with no blank lines at either end or more than one together.
fn tidy(lines: Vec<String>) -> String {
    let lines: Vec<String> = lines
        .into_iter()
        .map(|l| l.trim_end().to_string())
        .collect();
    let indent = lines
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let mut out = String::new();
    let mut blank = false;
    for line in &lines {
        if line.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        out.push_str(&line[indent..]);
        out.push('\n');
    }
    out
}

/// `spans` with the quote bars up to column `last` in `style`, and the one
/// there, the comment's own, drawn heavy.
fn focus_bars(spans: Vec<Span<'static>>, last: usize, style: Style) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 4);
    let mut col = 0;
    for span in spans {
        let mut plain = String::new();
        for c in span.content.chars() {
            if c == '│' && col <= last && col % 2 == 0 {
                if !plain.is_empty() {
                    out.push(Span::styled(std::mem::take(&mut plain), span.style));
                }
                out.push(Span::styled(if col == last { "┃" } else { "│" }, style));
            } else {
                plain.push(c);
            }
            col += wrap::width(c.encode_utf8(&mut [0; 4]));
        }
        if !plain.is_empty() {
            out.push(Span::styled(plain, span.style));
        }
    }
    out
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

    #[test]
    fn copies_plain_text_without_bars_and_with_whole_paragraphs() {
        let md = "# Title\n\n> ### ann · 1h ago\n>\n> A first paragraph long enough to wrap onto more lines.\n>\n> > ### bob · 2h ago\n> >\n> > - a list item\n> > - see [https://example.com/a/lon...](https://example.com/a/long/path) and [this](https://x.org)\n\n```\nlet code = \"a line of code that is too long to fit\";\n```\n";
        let mut doc = Doc::new(md.into());
        doc.layout(30, &Theme::new(crate::theme::Mode::Dark, true, None));
        let last = doc.lines.len() - 1;
        let all = doc.text_between((0, 0), (last, usize::MAX));
        assert_eq!(
            all,
            "Title\n═════\n\n  \
             ann · 1h ago\n\n  \
             A first paragraph long enough to wrap onto more lines.\n\n    \
             bob · 2h ago\n\n    \
             • a list item\n    \
             • see https://example.com/a/long/path and this\n\n\
             let code = \"a line of code that is too long to fit\";\n"
        );
        // Just the reply: indented no more than it has to be.
        let bob = doc
            .lines
            .iter()
            .position(|l| l.text().contains("bob"))
            .unwrap();
        assert!(
            doc.text_between((bob, 0), (bob + 3, usize::MAX))
                .starts_with("bob · 2h ago\n\n• a list item\n")
        );
        // From the middle of a line to the middle of the next.
        let first = doc
            .lines
            .iter()
            .position(|l| l.text().contains("A first"))
            .unwrap();
        doc.selection = Some(((first + 1, 7), (first, 4)));
        let text = doc.selected_text().unwrap();
        assert!(
            text.starts_with("first paragraph") && text.lines().count() == 1,
            "{text:?}"
        );
        // From partway into a comment to the reply below: the reply's no
        // more indented than it was.
        let bob = doc
            .lines
            .iter()
            .position(|l| l.text().contains("bob"))
            .unwrap();
        let text = doc.text_between((first, 4), (bob, usize::MAX));
        assert!(text.ends_with("lines.\n\n  bob · 2h ago\n"), "{text:?}");
    }

    #[test]
    fn selects_whole_lines_with_text_from_the_keys() {
        let md = "# T\n\n> ### ann · 1h ago\n>\n> one\n>\n> two\n";
        let mut doc = Doc::new(md.into());
        doc.layout(30, &Theme::new(crate::theme::Mode::Dark, true, None));
        doc.height = 20;
        let one = doc
            .lines
            .iter()
            .position(|l| l.text().contains("one"))
            .unwrap();
        let two = doc
            .lines
            .iter()
            .position(|l| l.text().contains("two"))
            .unwrap();
        assert_eq!(doc.text_line(one, 1), two, "over the blank line between");
        assert_eq!(doc.text_line(two, 1), two, "no further than the end");
        doc.select_lines(two, one);
        assert_eq!(doc.selected_text().unwrap(), "one\n\ntwo\n");
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
        let second = doc
            .lines
            .iter()
            .position(|l| l.text() == "second comment")
            .unwrap();
        doc.jump_to(second);

        let article = "A paragraph of the article.\n\n".repeat(30);
        doc.replace(format!("# Story\n\n{article}{comments}"));
        doc.layout(40, &theme);
        assert_eq!(doc.lines[doc.top].text(), "second comment");
        assert!(doc.top > second + 30);
    }

    /// A thread as a story's document has it: a top-level comment with a
    /// reply, then another top-level one.
    fn thread() -> Doc {
        use crate::render::comment_marker;
        let theme = Theme::plain();
        let md = format!(
            "# Story\n\n## Comments\n\n{}\n\n> ### bob\n>\n> first\n> line\n\n{}\n\n> > **alice**\n> >\n> > reply\n\n{}\n\n> ### carol\n>\n> second\n",
            comment_marker(1, 0),
            comment_marker(2, 1),
            comment_marker(3, 0),
        );
        let mut doc = Doc::new(md);
        doc.layout(40, &theme);
        doc.height = 4;
        doc
    }

    #[test]
    fn knows_each_comments_own_lines() {
        let doc = thread();
        let text = |c: &CommentSpan| -> Vec<String> {
            doc.lines[c.start..c.end].iter().map(|l| l.text()).collect()
        };
        let spans = &doc.comments;
        assert_eq!(
            spans.iter().map(|c| (c.id, c.depth)).collect::<Vec<_>>(),
            [(1, 0), (2, 1), (3, 0)]
        );
        assert_eq!(text(&spans[0]), ["│ bob", "│", "│ first line"]);
        assert_eq!(text(&spans[1]), ["│ │ alice", "│ │", "│ │ reply"]);
    }

    #[test]
    fn moves_between_comments_and_focuses_them() {
        let mut doc = thread();
        // Above the comments: nothing's focused.
        assert_eq!(doc.focused(), None);
        // ] goes to the Comments heading, then each comment.
        assert!(doc.jump(true, true));
        assert_eq!(doc.focused(), None);
        assert!(doc.jump(true, true));
        assert_eq!(doc.focused().map(|c| c.id), Some(1));
        assert!(doc.jump(true, true));
        assert_eq!(doc.focused().map(|c| c.id), Some(2));
        // } skips replies; { comes back to the top-level one before.
        assert!(doc.jump(true, false));
        assert_eq!(doc.focused().map(|c| c.id), Some(3));
        assert!(doc.jump(false, false));
        assert_eq!(doc.focused().map(|c| c.id), Some(1));
        // Scrolling past it, the cursor comes along.
        doc.jump_to(doc.comments[2].start);
        doc.follow_view();
        assert_eq!(doc.focused().map(|c| c.id), Some(3));
    }

    /// Article lines, then three comments, the second taller than the
    /// screen.
    fn long_thread() -> Doc {
        use crate::render::comment_marker;
        let theme = Theme::plain();
        let article = (1..=6)
            .map(|n| format!("article {n}\n\n"))
            .collect::<String>();
        let long = (1..=8)
            .map(|n| format!("> long {n}\n>\n"))
            .collect::<String>();
        let md = format!(
            "{article}{}\n\n> ### a\n>\n> short\n\n{}\n\n> ### b\n>\n{long}\n{}\n\n> ### c\n>\n> last\n",
            comment_marker(1, 0),
            comment_marker(2, 0),
            comment_marker(3, 0),
        );
        let mut doc = Doc::new(md);
        doc.layout(40, &theme);
        doc.height = 6;
        doc
    }

    #[test]
    fn j_and_k_move_by_comment_once_theyre_on_screen() {
        let mut doc = long_thread();
        let id = |doc: &Doc| doc.focused().map(|c| c.id);
        // The article scrolls, as many lines at a time as asked, till a
        // comment shows.
        doc.step(true, 2);
        assert_eq!((doc.top, id(&doc)), (2, None));
        doc.step(false, 1);
        assert_eq!((doc.top, id(&doc)), (1, None));
        while id(&doc).is_none() {
            doc.step(true, 1);
        }
        assert_eq!(id(&doc), Some(1));
        let a = doc.comments[0];
        assert!(
            a.start >= doc.top && a.end <= doc.top + doc.height,
            "all of it shows"
        );
        // To the long one: its top shows, and j reads down through it
        // before moving on.
        doc.step(true, 1);
        assert_eq!(id(&doc), Some(2));
        let b = doc.comments[1];
        assert_eq!(doc.top, b.start - 1);
        let mut steps = 0;
        while id(&doc) == Some(2) {
            doc.step(true, 1);
            steps += 1;
        }
        assert!(steps > 5, "scrolled through it first, in {steps}");
        assert_eq!(id(&doc), Some(3));
        // And k back: up through the long one from its end.
        doc.step(false, 1);
        assert_eq!(id(&doc), Some(2));
        assert!(b.end <= doc.top + doc.height, "its end shows");
        while id(&doc) == Some(2) {
            doc.step(false, 1);
        }
        assert_eq!(id(&doc), Some(1));
        // Above the first, back to the article.
        doc.step(false, 1);
        assert_eq!(id(&doc), None);
    }

    /// Comments with links: the first has two in its text (one twice),
    /// the second none, the third one.
    fn linked_thread(height: usize) -> Doc {
        use crate::render::comment_marker;
        let theme = Theme::plain();
        let md = format!(
            "# Story\n\n{}\n\n> ### [bob](<https://news.ycombinator.com/user?id=bob>) · [1h](<https://news.ycombinator.com/item?id=1>)\n>\n> see [a talk](<https://youtube.com/watch?v=a>) and\n> [another one that wraps over the line](<https://youtube.com/watch?v=b>),\n> and [a talk](<https://youtube.com/watch?v=a>) again\n\n{}\n\n> ### carol\n>\n> no links\n\n{}\n\n> ### dave\n>\n> [last](<https://example.com/>)\n",
            comment_marker(1, 0),
            comment_marker(2, 0),
            comment_marker(3, 0),
        );
        let mut doc = Doc::new(md);
        doc.layout(30, &theme);
        doc.height = height;
        doc
    }

    /// Where `j` and `k` are: the comment, and the link if on one.
    fn stop(doc: &Doc) -> (Option<u64>, Option<String>) {
        (
            doc.focused().map(|c| c.id),
            doc.focused_link().map(|l| l.url),
        )
    }

    #[test]
    fn j_and_k_stop_on_each_link_in_a_comment() {
        let mut doc = linked_thread(40);
        let stops = doc.link_stops(&doc.comments[0]);
        let urls: Vec<&str> = stops.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://youtube.com/watch?v=a",
                "https://youtube.com/watch?v=b"
            ],
            "not the header's, nor one twice"
        );
        assert!(stops[1].last > stops[1].first, "the second wraps");
        let some = |id: u64, url: Option<&str>| (Some(id), url.map(String::from));
        // A comment with links is each of them in turn; one without, itself.
        let expected = [
            some(1, Some("https://youtube.com/watch?v=a")),
            some(1, Some("https://youtube.com/watch?v=b")),
            some(2, None),
            some(3, Some("https://example.com/")),
        ];
        let mut seen = Vec::new();
        while stop(&doc) != expected[3] {
            doc.step(true, 1);
            if doc.focused().is_some() {
                seen.push(stop(&doc));
            }
        }
        assert_eq!(seen, expected);
        // And back, each k undoing a j.
        for want in expected.iter().rev().skip(1) {
            doc.step(false, 1);
            assert_eq!(&stop(&doc), want);
        }
        // Above the first link, the article.
        doc.step(false, 1);
        assert_eq!(stop(&doc), (None, None));
        // Esc lets go of a link, and j goes on from it.
        doc.step(true, 1);
        assert!(doc.clear_link());
        assert_eq!(stop(&doc), some(1, None));
        assert!(!doc.clear_link());
        doc.step(true, 1);
        assert_eq!(stop(&doc), some(1, Some("https://youtube.com/watch?v=b")));
        // ] and [ go comment to comment, onto each one's first link.
        doc.jump(true, true);
        assert_eq!(stop(&doc), some(2, None));
        doc.jump(true, true);
        assert_eq!(stop(&doc), some(3, Some("https://example.com/")));
        doc.jump(false, true);
        assert_eq!(stop(&doc), some(2, None));
    }

    /// An article with a link every few lines, the first in its first
    /// line and the last in its last, and no comments.
    fn linked_article(height: usize) -> (Doc, Vec<String>) {
        let theme = Theme::plain();
        let urls: Vec<String> = (0..12)
            .map(|n| format!("https://example.com/{n}"))
            .collect();
        let md: Vec<String> = urls
            .iter()
            .enumerate()
            .map(|(n, url)| format!("[link {n}](<{url}>)\n\nfiller\n\nmore filler"))
            .collect();
        let mut md = md.join("\n\n");
        md.push_str("\n\n[end](<https://example.com/end>)\n");
        let mut urls = urls;
        urls.push("https://example.com/end".into());
        let mut doc = Doc::new(md);
        doc.layout(30, &theme);
        doc.height = height;
        (doc, urls)
    }

    /// The links the band's on, in turn, scrolling `by` at a time to the
    /// end, or with `by` negative, the top.
    fn band_links(doc: &mut Doc, by: isize) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        loop {
            if let Some(l) = doc.focused_link() {
                assert!(
                    l.first >= doc.top && l.last < doc.top + doc.height,
                    "{l:?} on screen"
                );
                if seen.last() != Some(&l.url) {
                    seen.push(l.url);
                }
            }
            if doc.top == if by > 0 { doc.max_top() } else { 0 } {
                return seen;
            }
            doc.scroll_by(by);
        }
    }

    #[test]
    fn the_band_comes_to_every_link_in_the_article_as_it_scrolls() {
        for by in [1, 2] {
            let (mut doc, urls) = linked_article(10);
            assert!(doc.max_top() > 20, "scrolls a good way");
            assert_eq!(
                doc.focused_link().map(|l| l.url).as_ref(),
                Some(&urls[0]),
                "the first at the top"
            );
            assert_eq!(band_links(&mut doc, by), urls, "scrolling {by} at a time");
            // And back up: each again, the other way.
            let mut up = band_links(&mut doc, -by);
            up.reverse();
            assert_eq!(up, urls, "scrolling back {by} at a time");
        }
        // Esc doesn't let go of it: it's just where you're reading.
        let (mut doc, urls) = linked_article(10);
        assert!(!doc.clear_link());
        assert_eq!(doc.focused_link().map(|l| l.url).as_ref(), Some(&urls[0]));
    }

    #[test]
    fn j_and_k_go_to_every_link_in_an_article_that_fits() {
        let (mut doc, urls) = linked_article(200);
        let url = |doc: &Doc| doc.focused_link().map(|l| l.url);
        let mut seen = vec![url(&doc).unwrap()];
        for _ in 1..urls.len() {
            doc.step(true, 2);
            seen.push(url(&doc).unwrap());
        }
        assert_eq!(seen, urls);
        doc.step(false, 2);
        assert_eq!(url(&doc).as_ref(), urls.iter().rev().nth(1));
    }

    #[test]
    fn j_and_k_go_along_a_line_of_links_in_the_band() {
        // Side by side, and wrapped to a line each.
        for width in [100, 30] {
            band_steps_to_every_link(width);
        }
    }

    fn band_steps_to_every_link(width: usize) {
        let theme = Theme::plain();
        // Lines of one link and of three, between filler.
        let mut md = String::new();
        let mut urls = Vec::new();
        for n in 0..8 {
            let line: Vec<String> = (0..if n % 2 == 0 { 3 } else { 1 })
                .map(|k| {
                    let url = format!("https://example.com/{n}/{k}");
                    urls.push(url.clone());
                    format!("[l{n}{k}](<{url}>)")
                })
                .collect();
            md.push_str(&format!(
                "{}\n\nfiller\n\nmore\n\nand more\n\n",
                line.join(" ")
            ));
        }
        let mut doc = Doc::new(md);
        doc.layout(width, &theme);
        doc.height = 10;
        assert_eq!(doc.lines[0].links.len(), if width > 90 { 3 } else { 1 });
        let url = |doc: &Doc| doc.focused_link().map(|l| l.url);
        // Down: every link, each once, in order.
        let mut seen = Vec::new();
        for _ in 0..200 {
            if let Some(u) = url(&doc)
                && seen.last() != Some(&u)
            {
                seen.push(u);
            }
            doc.step(true, 2);
        }
        assert_eq!(doc.top, doc.max_top(), "came to the end");
        assert_eq!(seen, urls);
        // And up: each again, the other way.
        let mut seen = Vec::new();
        for _ in 0..200 {
            if let Some(u) = url(&doc)
                && seen.last() != Some(&u)
            {
                seen.push(u);
            }
            doc.step(false, 2);
        }
        assert_eq!(doc.top, 0, "came to the top");
        seen.reverse();
        assert_eq!(seen, urls);
    }

    #[test]
    fn the_band_stays_in_the_article_above_the_comments() {
        let mut doc = linked_thread(6);
        // Down to just above the comments: never a link in them.
        while doc.comments[0].start > doc.top {
            if let Some(l) = doc.focused_link() {
                assert!(l.last < doc.comments[0].start, "{l:?} in the comments");
            }
            doc.scroll_by(1);
        }
    }

    #[test]
    fn links_below_the_screen_are_scrolled_to_and_off_it_let_go() {
        let mut doc = linked_thread(3);
        doc.jump(true, true);
        assert_eq!(doc.focused().map(|c| c.id), Some(1));
        // Each link shows whole before it's chosen.
        let mut picked = Vec::new();
        while doc.focused().map(|c| c.id) == Some(1) {
            doc.step(true, 1);
            // As drawing does.
            doc.follow_view();
            assert!(doc.top < 40, "never moves on");
            if let Some(l) = doc.focused_link() {
                assert!(
                    l.first >= doc.top && l.last < doc.top + doc.height,
                    "{l:?} on screen"
                );
                if picked.last() != Some(&l.url) {
                    picked.push(l.url);
                }
            }
        }
        assert_eq!(picked.len(), 2);
        // Up from below: onto the last link once it's back on screen.
        doc.step(false, 1);
        while doc.focused_link().is_none() {
            doc.step(false, 1);
            assert!(doc.top > 0, "never comes to it");
        }
        assert_eq!(
            doc.focused_link().map(|l| l.url).as_deref(),
            Some("https://youtube.com/watch?v=b")
        );
        // Scrolled away, the link isn't chosen any more.
        doc.scroll_by(-3);
        doc.follow_view();
        assert_eq!(doc.focused_link(), None);
    }

    #[test]
    fn paging_brings_the_cursor_along() {
        let mut doc = long_thread();
        doc.jump(true, true);
        assert_eq!(doc.focused().map(|c| c.id), Some(1));
        doc.jump_to(doc.comments[2].start);
        doc.follow_view();
        assert_eq!(doc.focused().map(|c| c.id), Some(3));
    }

    #[test]
    fn the_focused_comments_bars_stand_out() {
        let accent = Style::new().bold();
        let spans = vec![
            Span::raw("│ "),
            Span::raw("│ "),
            Span::raw("reply │ not a bar"),
        ];
        let focused = focus_bars(spans, 2, accent);
        let text: String = focused.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "│ ┃ reply │ not a bar");
        // Both bars in the accent; nothing in the text.
        let styled: Vec<&str> = focused
            .iter()
            .filter(|s| s.style == accent)
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(styled, ["│", "┃"]);
        // A top-level comment's: only its own, even with more drawn.
        let text: String = focus_bars(vec![Span::raw("│ │ x")], 0, accent)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, "┃ │ x");
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
