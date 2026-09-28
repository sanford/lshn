//! Getting around a story: search, headings and the outline, and
//! following links.

use super::picker::{Outcome, Picker, Target};
use super::App;
use crate::render;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

/// Something in the reader that takes the keyboard until it's done.
pub enum Prompt {
    /// Typing a search. `from` is where the search started.
    /// Searching the document as you type, from line `from`, forward
    /// unless it was started with `C-r`.
    Search {
        query: String,
        from: usize,
        forward: bool,
    },
    /// Choosing a link by its hint letters.
    Hints { typed: String },
    /// The outline or the themes.
    Pick(Picker),
    /// Confirming opening something outside lshn.
    Open(crate::open::Target),
}

/// Letters for link hints, easiest to reach first.
const HINT_KEYS: &str = "asdfjklghqwertyuiopzxcvbnm";

impl App {
    /// Reader keys for getting around. Returns true if the key was one.
    pub(super) fn nav_key(&mut self, key: KeyEvent) -> bool {
        let Some(doc) = self.current() else {
            return false;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // `/`, or Emacs's C-s and C-r.
            KeyCode::Char(c) if c == '/' || (ctrl && matches!(c, 's' | 'r')) => {
                let from = doc.top();
                doc.clear_search();
                self.prompt = Some(Prompt::Search {
                    query: String::new(),
                    from,
                    forward: c != 'r',
                });
            }
            KeyCode::Char(c @ ('n' | 'N')) => {
                if !doc.search_next(c == 'n') {
                    self.flash = Some("No search (/ to search)".into());
                }
            }
            KeyCode::Char(c @ (']' | '[')) => {
                if !doc.jump_heading(c == ']') {
                    let which = if c == ']' { "below" } else { "above" };
                    self.flash = Some(format!("No heading {which}"));
                }
            }
            // The outline pane, just while choosing: the document follows
            // the selection, and the pane goes away after.
            KeyCode::Char('o') => self.focus_outline(true),
            KeyCode::Char('f') => self.show_hints(),
            KeyCode::Esc if doc.search_status().is_some() => doc.clear_search(),
            _ => return false,
        }
        true
    }

    /// Handles a key while `prompt` is up. The prompt stays up only if this
    /// puts it back. Returns true to quit.
    pub(super) fn prompt_key(&mut self, prompt: Prompt, key: KeyEvent, ctrl: bool) -> bool {
        match prompt {
            Prompt::Search {
                mut query,
                from,
                forward,
            } => {
                match key.code {
                    // Emacs: the next or previous match, still typing. On an
                    // empty prompt, C-s brings back the last search.
                    KeyCode::Char('s') if ctrl && query.is_empty() => {
                        query = self.last_search.clone();
                    }
                    KeyCode::Char(c @ ('s' | 'r')) if ctrl => {
                        if let Some(doc) = self.current() {
                            doc.search_next(c == 's');
                        }
                        self.prompt = Some(Prompt::Search {
                            query,
                            from,
                            forward,
                        });
                        return false;
                    }
                    KeyCode::Enter => {
                        if !query.is_empty() {
                            self.last_search = query.clone();
                        }
                        if let Some(doc) = self.current()
                            && doc.search_status().is_some_and(|s| s.starts_with("0/"))
                        {
                            doc.clear_search();
                            self.flash = Some(format!("Not found: {query}"));
                        }
                        return false;
                    }
                    KeyCode::Esc => {
                        if let Some(doc) = self.current() {
                            doc.clear_search();
                            doc.jump_to(from);
                        }
                        return false;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                    }
                    KeyCode::Char(c) if !ctrl => query.push(c),
                    // Anything else leaves the search as it is.
                    _ => {
                        self.prompt = Some(Prompt::Search {
                            query,
                            from,
                            forward,
                        });
                        return false;
                    }
                }
                if let Some(doc) = self.current() {
                    doc.search(&query, from, forward);
                }
                self.prompt = Some(Prompt::Search {
                    query,
                    from,
                    forward,
                });
            }
            Prompt::Hints { mut typed } => {
                let KeyCode::Char(c) = key.code else {
                    {
                        self.clear_hints();
                        return false;
                    }
                };
                typed.push(c);
                let Some(doc) = self.current() else {
                    return false;
                };
                let url = doc
                    .hints
                    .iter()
                    .find(|h| h.label == typed)
                    .map(|h| h.url.clone());
                let partial = doc.hints.iter().any(|h| h.label.starts_with(&typed));
                if let Some(url) = url {
                    self.clear_hints();
                    self.follow(&url);
                } else if partial {
                    self.prompt = Some(Prompt::Hints { typed });
                } else {
                    self.clear_hints();
                }
            }
            Prompt::Pick(mut picker) => match picker.key(key, ctrl) {
                Outcome::Stay => self.prompt = Some(Prompt::Pick(picker)),
                Outcome::Close => {}
                Outcome::Choose(Target::Theme(choice)) => self.keep_theme(choice),
                Outcome::Choose(target) => self.go(target),
                Outcome::Quit => return true,
            },
            Prompt::Open(target) => {
                if matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) {
                    self.flash = Some(match crate::open::open(&target) {
                        Ok(()) => format!("Opened {}", target.what),
                        Err(e) => format!("Couldn't open {}: {e}", target.what),
                    });
                }
            }
        }
        false
    }

    /// Labels the links on screen so one can be followed by typing.
    fn show_hints(&mut self) {
        let Some(doc) = self.current() else { return };
        let links = doc.visible_links();
        if links.is_empty() {
            self.flash = Some("No links on screen".into());
            return;
        }
        let labels = hint_labels(links.len());
        let hints = links
            .into_iter()
            .zip(labels)
            .map(|((line, col, url), label)| (label, line, col, url))
            .collect();
        doc.set_hints(hints);
        self.prompt = Some(Prompt::Hints {
            typed: String::new(),
        });
    }

    fn clear_hints(&mut self) {
        if let Some(doc) = self.current() {
            doc.set_hints(Vec::new());
        }
    }

    /// Follows a link: to a heading in the story, or (after asking) to
    /// the browser.
    pub(super) fn follow(&mut self, url: &str) {
        let Some(doc) = self.current() else { return };
        match render::local_path(url) {
            Some(path) if path.is_empty() => {
                let Some((_, anchor)) = url.split_once('#') else {
                    return;
                };
                match doc.anchor(anchor) {
                    Some(line) => doc.jump_to(line),
                    None => self.flash = Some(format!("No heading #{anchor}")),
                }
            }
            Some(path) => self.flash = Some(format!("Not a web link: {path}")),
            None => match crate::open::web(url) {
                Ok(target) => self.prompt = Some(Prompt::Open(target)),
                Err(why) => self.flash = Some(why),
            },
        }
    }

    pub(super) fn draw_picker(&mut self, f: &mut Frame) {
        if let Some(Prompt::Pick(picker)) = &mut self.prompt {
            picker.draw(f);
        }
    }

    /// Goes where a picker row points.
    pub(super) fn go(&mut self, target: Target) {
        match target {
            Target::Line(line) => {
                if let Some(doc) = self.current() {
                    doc.jump_to(line);
                }
            }
            Target::Theme(_) => unreachable!("themes are kept in prompt_key"),
        }
    }

    /// The footer while a prompt is up.
    pub(super) fn prompt_footer(&mut self) -> Option<Line<'static>> {
        let line = match self.prompt.as_ref()? {
            Prompt::Search { query, .. } => {
                let query = query.clone();
                let status = self
                    .current()
                    .and_then(|d| d.search_status())
                    .unwrap_or_default();
                Line::from(vec![
                    " /".bold(),
                    Span::raw(query),
                    "▏".slow_blink(),
                    Span::raw(format!("  {status}")).dim(),
                ])
            }
            Prompt::Hints { typed } => Line::from(vec![
                " Follow link: ".bold(),
                Span::raw(format!("type its letters {typed}")),
                "  esc cancels".dim(),
            ]),
            Prompt::Pick(_) => Line::from(" ↑↓ move  / filter  ⏎ go  esc close  q quit".dim()),
            Prompt::Open(target) => Line::from(vec![
                " Open ".bold(),
                Span::raw(target.what.clone()),
                "? ".bold(),
                "y/n  ".dim(),
                Span::raw(target.target.clone()).dim(),
            ]),
        };
        Some(line)
    }
}

/// `n` distinct labels, all the same length, from [`HINT_KEYS`].
fn hint_labels(n: usize) -> Vec<String> {
    let keys: Vec<char> = HINT_KEYS.chars().collect();
    let mut len = 1;
    while keys.len().pow(len) < n {
        len += 1;
    }
    (0..n)
        .map(|mut i| {
            let mut label = Vec::new();
            for _ in 0..len {
                label.push(keys[i % keys.len()]);
                i /= keys.len();
            }
            label.iter().rev().collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_unique_and_even() {
        assert_eq!(hint_labels(3), ["a", "s", "d"]);
        let many = hint_labels(30);
        assert!(many.iter().all(|l| l.len() == 2));
        let mut sorted = many.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 30);
    }
}
