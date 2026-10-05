//! The mouse: the wheel scrolls what's under the pointer, clicks choose
//! stories and follow links, and dragging over the text copies what it
//! covers.

use super::nav::Prompt;
use super::{App, Focus, HeaderHit};
use crate::hn::Feed;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;

/// Lines per wheel notch: one, since a wheel or trackpad already goes as
/// fast as it's turned.
const WHEEL: isize = 1;

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
                    super::menu::Outcome::Choose(choice) => {
                        self.prompt = None;
                        self.choose(choice);
                    }
                }
            }
            return;
        }
        // The reply box takes clicks; the wheel still scrolls what's behind.
        if m.kind == MouseEventKind::Down(MouseButton::Left) && self.compose_click(x, y) {
            return;
        }
        if matches!(self.prompt, Some(Prompt::Compose(_)))
            && matches!(m.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
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
            MouseEventKind::Down(MouseButton::Left) => self.click_header(x, y),
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
                // A click ends a selection made with the keys.
                if matches!(self.prompt, Some(Prompt::Select { .. })) {
                    self.prompt = None;
                }
                let list = self.focus == Focus::List;
                let Some(doc) = self.current() else {
                    return false;
                };
                doc.selection = None;
                if !doc.contains(x, y) {
                    return false;
                }
                // From the list, a click on the story beside it reads it,
                // as `Tab` would. The story's laid out afresh, so the
                // click goes no further.
                if list {
                    self.switch_view();
                    return true;
                }
                let spot = doc.spot_at(x, y);
                self.press = Some((spot, false));
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some((from, _)) = self.press else {
                    return false;
                };
                self.press = Some((from, true));
                if let Some(doc) = self.current() {
                    doc.select_to(from, x, y);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some((_, dragged)) = self.press.take() else {
                    return false;
                };
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

    /// `lshn` in the header goes back to the list, and to the first tab
    /// (Top) from another; a tab, to what its key does; `esc Back`, back
    /// where the last link was followed from.
    pub(super) fn click_header(&mut self, x: u16, y: u16) {
        let (row, hits) = &self.header_hits;
        let hit = hits
            .iter()
            .find(|(from, to, _)| y == *row && (*from..*to).contains(&x));
        let home = self.feed == Feed::ALL[0] && self.search.is_none();
        match hit.map(|h| h.2) {
            Some(HeaderHit::Home) if !home => {
                self.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
            }
            Some(HeaderHit::Home) if self.focus == Focus::Reader => self.leave_reader(),
            Some(HeaderHit::Key(c)) => {
                self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            Some(HeaderHit::Back) => {
                self.go_back();
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
            self.chose = true;
            self.focus = Focus::List;
        }
    }
}
