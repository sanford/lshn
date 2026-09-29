//! The mouse: the wheel scrolls what's under the pointer, clicks choose
//! stories and follow links, and dragging over the text copies what it
//! covers.

use super::nav::Prompt;
use super::{App, Focus};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;

/// Lines per wheel notch.
const WHEEL: isize = 3;

impl App {
    pub(super) fn mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let down = match m.kind {
            MouseEventKind::ScrollDown => Some(true),
            MouseEventKind::ScrollUp => Some(false),
            _ => None,
        };
        // A menu takes clicks: on an item, it does it; outside, it closes.
        if let Some(Prompt::Menu(menu)) = &mut self.prompt {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                match menu.click(x, y) {
                    super::menu::Outcome::Stay => {}
                    super::menu::Outcome::Close => self.prompt = None,
                    super::menu::Outcome::Choose((text, what)) => {
                        self.prompt = None;
                        self.put(&text, &what);
                    }
                }
            }
            return;
        }
        // A popup takes the wheel for its list; clicks outside close it.
        if let Some(Prompt::Pick(picker)) = &mut self.prompt {
            if let Some(down) = down {
                let code = if down { KeyCode::Down } else { KeyCode::Up };
                picker.key(KeyEvent::new(code, KeyModifiers::NONE), false);
            }
            return;
        }
        // The scrollbar and the outline pane take clicks and drags, but not
        // a drag over the text.
        if self.press.is_none()
            && matches!(
                m.kind,
                MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
            )
        {
            if let Some(doc) = self.current()
                && doc.scrollbar_jump(x, y)
            {
                return;
            }
            if m.kind == MouseEventKind::Down(MouseButton::Left) && self.outline_click(x, y) {
                return;
            }
        }
        if self.drag(m) {
            return;
        }
        let over_list = self.list_area.contains(Position { x, y });
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if over_list => {
                self.select_by(if down == Some(true) { 1 } else { -1 });
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let delta = if down == Some(true) { WHEEL } else { -WHEEL };
                if let Some(doc) = self.current()
                    && doc.contains(x, y)
                {
                    doc.scroll_by(delta);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if over_list => self.click_list(y),
            _ => {}
        }
    }

    /// Pressing on the text and dragging selects what's dragged over, and
    /// letting go copies it; letting go without dragging follows the link
    /// clicked on. Returns whether it took the event.
    fn drag(&mut self, m: MouseEvent) -> bool {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.press = None;
                let Some(doc) = self.current() else { return false };
                doc.selection = None;
                if !doc.contains(x, y) {
                    return false;
                }
                let spot = doc.spot_at(x, y);
                self.press = Some((spot, false));
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some((from, _)) = self.press else { return false };
                self.press = Some((from, true));
                if let Some(doc) = self.current() {
                    doc.select_to(from, x, y);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some((_, dragged)) = self.press.take() else { return false };
                if dragged {
                    let text = self.current().and_then(|d| d.selected_text());
                    if let Some(text) = text {
                        self.put(&text, &super::copy::amount(&text));
                    }
                } else if let Some(url) = self.current().and_then(|d| d.link_at(x, y)) {
                    self.follow(&url);
                }
                true
            }
            _ => false,
        }
    }

    /// A click on a story in the list selects it, or opens it if it was
    /// already selected.
    fn click_list(&mut self, y: u16) {
        // Stories take a row or more each: count down from the top one.
        let mut row = usize::from(y.saturating_sub(self.list_area.y));
        let mut i = self.list.offset();
        while let Some(&h) = self.list_heights.get(i)
            && row >= h
        {
            row -= h;
            i += 1;
        }
        if i >= self.shown.len() {
            return;
        }
        if self.list.selected() == Some(i) {
            self.read_selected();
        } else {
            self.list.select(Some(i));
            self.focus = Focus::List;
        }
    }
}
