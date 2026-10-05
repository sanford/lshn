//! Writing a reply in lshn itself (`r`): a box over the bottom of the
//! screen, with what's being answered still in view above it, where the
//! text wraps as it's typed. Tab goes to its Cancel and Post buttons (Esc
//! and Ctrl-S from anywhere), and Ctrl-O hands it to `$EDITOR` for a long
//! one.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthChar;

/// Text being written: its lines, and where the cursor is in them.
pub struct TextBox {
    lines: Vec<String>,
    /// The cursor: a line, and a character in it.
    row: usize,
    col: usize,
    /// The first row on screen, of the rows the lines wrap to.
    top: usize,
    /// How wide it was drawn last, for moving up and down its rows.
    width: usize,
}

/// A row of the wrapped text: its line, and its characters from `start`
/// to just before `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    line: usize,
    start: usize,
    end: usize,
}

fn width_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

impl TextBox {
    /// A box with `text` in it, the cursor at its end.
    pub fn new(text: &str) -> TextBox {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        TextBox {
            lines,
            row,
            col,
            top: 0,
            width: 60,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.text().trim().is_empty()
    }

    fn len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    /// The byte where character `col` of line `row` starts.
    fn byte(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }

    fn insert(&mut self, c: char) {
        let at = self.byte(self.row, self.col);
        self.lines[self.row].insert(at, c);
        self.col += 1;
    }

    fn newline(&mut self) {
        let at = self.byte(self.row, self.col);
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let at = self.byte(self.row, self.col);
            self.lines[self.row].remove(at);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.len(self.row);
            self.lines[self.row].push_str(&line);
        }
    }

    fn delete(&mut self) {
        if self.col < self.len(self.row) {
            let at = self.byte(self.row, self.col);
            self.lines[self.row].remove(at);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    /// Deletes back to the start of the word before the cursor.
    fn delete_word(&mut self) {
        if self.col == 0 {
            self.backspace();
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut start = self.col;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let (from, to) = (self.byte(self.row, start), self.byte(self.row, self.col));
        self.lines[self.row].replace_range(from..to, "");
        self.col = start;
    }

    fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.len(self.row);
        }
    }

    fn right(&mut self) {
        if self.col < self.len(self.row) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    /// Pasted text, as it was: its lines kept, control characters left out.
    pub fn paste(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => self.newline(),
                '\t' => self.insert(' '),
                c if c.is_control() => {}
                c => self.insert(c),
            }
        }
    }

    /// Handles a key that edits or moves. Returns whether it was one.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = self.len(self.row),
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char('d') if ctrl => self.delete(),
            KeyCode::Char('k') if ctrl => {
                // To the end of the line, or the line break at its end.
                let at = self.byte(self.row, self.col);
                if at == self.lines[self.row].len() {
                    self.delete();
                } else {
                    self.lines[self.row].truncate(at);
                }
            }
            KeyCode::Backspace if alt || ctrl => self.delete_word(),
            KeyCode::Char(c) if !ctrl && !alt => self.insert(c),
            KeyCode::Enter => self.newline(),
            KeyCode::Tab => self.insert(' '),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.vertical(-1),
            KeyCode::Down => self.vertical(1),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.len(self.row),
            _ => return false,
        }
        true
    }

    /// The rows the text wraps to at `width`: at the last space that fits,
    /// or where it must, if a word's too long for a row.
    fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(1);
        let mut rows = Vec::new();
        for (line, text) in self.lines.iter().enumerate() {
            let chars: Vec<char> = text.chars().collect();
            let mut start = 0;
            loop {
                let (mut used, mut end, mut space) = (0, start, None);
                while end < chars.len() && used + width_of(chars[end]) <= width {
                    if chars[end] == ' ' {
                        space = Some(end);
                    }
                    used += width_of(chars[end]);
                    end += 1;
                }
                if end < chars.len() {
                    if let Some(space) = space.filter(|&s| s > start) {
                        end = space + 1;
                    } else if end == start {
                        end += 1;
                    }
                }
                rows.push(Row { line, start, end });
                if end >= chars.len() {
                    break;
                }
                start = end;
            }
        }
        rows
    }

    /// The cursor's row, of `rows`, and its column on screen.
    fn cursor(&self, rows: &[Row]) -> (usize, usize) {
        let at = rows
            .iter()
            .rposition(|r| r.line == self.row && r.start <= self.col)
            .unwrap_or(0);
        let r = rows[at];
        let x = self.lines[r.line]
            .chars()
            .skip(r.start)
            .take(self.col - r.start)
            .map(width_of)
            .sum();
        (at, x)
    }

    /// Up or down a row of the wrapped text, as near the same column as
    /// there is.
    fn vertical(&mut self, by: isize) {
        let rows = self.rows(self.width);
        let (at, x) = self.cursor(&rows);
        let Some(to) = at.checked_add_signed(by).filter(|&t| t < rows.len()) else {
            // Past the first or last row: to the start or end.
            if by < 0 {
                (self.row, self.col) = (0, 0);
            } else {
                self.row = self.lines.len() - 1;
                self.col = self.len(self.row);
            }
            return;
        };
        let r = rows[to];
        let chars: Vec<char> = self.lines[r.line].chars().collect();
        // Not the row's end, where it wraps: that's the next row's start.
        let last = if r.end < chars.len() {
            r.end - 1
        } else {
            r.end
        };
        let (mut col, mut used) = (r.start, 0);
        while col < last && used + width_of(chars[col]) <= x {
            used += width_of(chars[col]);
            col += 1;
        }
        (self.row, self.col) = (r.line, col);
    }

    /// Draws the text in `area`, scrolled to show the cursor, and puts the
    /// terminal's cursor there if it has the keyboard (`focused`).
    pub fn draw(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        self.width = usize::from(area.width).max(1);
        let rows = self.rows(self.width);
        let (at, x) = self.cursor(&rows);
        let height = usize::from(area.height).max(1);
        if at < self.top {
            self.top = at;
        } else if at >= self.top + height {
            self.top = at + 1 - height;
        }
        let lines: Vec<Line> = rows
            .iter()
            .skip(self.top)
            .take(height)
            .map(|r| {
                let text: String = self.lines[r.line]
                    .chars()
                    .skip(r.start)
                    .take(r.end - r.start)
                    .collect();
                Line::from(text)
            })
            .collect();
        f.render_widget(Paragraph::new(lines), area);
        if !focused || area.height == 0 {
            return;
        }
        f.set_cursor_position(Position {
            x: area.x + (x as u16).min(area.width.saturating_sub(1)),
            y: area.y + (at - self.top) as u16,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(text: &str) -> TextBox {
        let mut b = TextBox::new("");
        for c in text.chars() {
            b.key(key(if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            }));
        }
        b
    }

    #[test]
    fn types_and_breaks_lines_and_joins_them_again() {
        let mut b = typed("Hello\nworld");
        assert_eq!(b.text(), "Hello\nworld");
        b.key(key(KeyCode::Home));
        b.key(key(KeyCode::Backspace));
        assert_eq!(b.text(), "Helloworld");
        b.key(key(KeyCode::Char(' ')));
        assert_eq!(b.text(), "Hello world");
        b.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(b.text(), "world", "the word before the cursor");
        b.paste("ü, 日本\r\nnext\tline\u{1b}");
        assert_eq!(b.text(), "ü, 日本\nnext lineworld");
    }

    #[test]
    fn wraps_at_spaces_and_moves_by_rows() {
        let mut b = TextBox::new("one two three four");
        let rows = b.rows(9);
        let text = |r: &Row| {
            b.lines[0]
                .chars()
                .skip(r.start)
                .take(r.end - r.start)
                .collect::<String>()
        };
        assert_eq!(
            rows.iter().map(text).collect::<Vec<_>>(),
            ["one two ", "three ", "four"]
        );
        // A word too long for a row is broken.
        assert_eq!(TextBox::new("abcdefghij").rows(4).len(), 3);
        b.width = 9;
        // From the end, up a row: as near the same column as there is.
        b.key(key(KeyCode::Up));
        assert_eq!((b.row, b.col), (0, 12), "the end of \"three\"");
        b.key(key(KeyCode::Up));
        b.key(key(KeyCode::Up));
        assert_eq!((b.row, b.col), (0, 0), "past the top: the start");
        b.key(key(KeyCode::Down));
        assert_eq!(b.col, 8, "the start of \"three\", not the end of \"two \"");
    }
}
