//! The mouse: the wheel scrolls what's under the pointer, and clicks
//! choose stories and follow links.

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
        // A popup takes the wheel for its list; clicks outside close it.
        if let Some(Prompt::Pick(picker)) = &mut self.prompt {
            if let Some(down) = down {
                let code = if down { KeyCode::Down } else { KeyCode::Up };
                picker.key(KeyEvent::new(code, KeyModifiers::NONE), false);
            }
            return;
        }
        // The scrollbar and the outline pane take clicks and drags.
        if matches!(
            m.kind,
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
        ) {
            if let Some(doc) = self.current()
                && doc.scrollbar_jump(x, y)
            {
                return;
            }
            if m.kind == MouseEventKind::Down(MouseButton::Left) && self.outline_click(x, y) {
                return;
            }
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
            MouseEventKind::Down(MouseButton::Left) => {
                let url = self.current().and_then(|d| d.link_at(x, y));
                if let Some(url) = url {
                    self.follow(&url);
                }
            }
            _ => {}
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
