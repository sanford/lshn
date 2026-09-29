//! Getting around: search, headings and the outline, and following links,
//! to the web or, for HN's own stories and people, to their pages here,
//! with a history to go back through.

use super::menu;
use super::picker::{Outcome, Picker, Target};
use super::{App, Asked, Focus};
use crate::doc::Doc;
use crate::fetch::Job;
use crate::hn::{self, Link};
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
    /// Choosing a link by its hint letters, to follow it, or to upvote or
    /// reply to the story or comment it's to.
    Hints { typed: String, purpose: Purpose },
    /// The outline or the themes.
    Pick(Picker),
    /// Typing a search of all of HN's stories.
    SearchHn { query: String },
    /// Logging in: the username, then the password.
    LoginUser { user: String },
    LoginPassword { user: String, password: String },
    /// The passphrase for the login's file, where there's no keyring.
    Passphrase {
        text: String,
        purpose: super::act::Passphrase,
    },
    Logout { user: String },
    /// A reply, written, to post or not.
    Post {
        draft: super::act::Draft,
        text: String,
    },
    /// Confirming opening something outside lshn.
    Open(crate::open::Target),
    /// What to copy.
    Menu(super::menu::Menu<super::copy::Copy>),
    /// Selecting lines to copy, from `anchor` to `cursor`.
    Select { anchor: usize, cursor: usize },
}

/// What choosing a hinted link does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Follow,
    Upvote,
    Reply,
}

/// A page to go back to, and the line that was at the top of the screen.
pub enum Back {
    Story { id: u64, top: usize },
    User { name: String, top: usize },
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
            // To the next comment, or heading; with a brace, skipping
            // replies.
            KeyCode::Char(c @ (']' | '[' | '}' | '{')) => {
                let forward = matches!(c, ']' | '}');
                if !doc.jump(forward, matches!(c, ']' | '[')) {
                    let which = if forward { "below" } else { "above" };
                    self.flash = Some(format!("Nothing {which}"));
                }
            }
            // The outline pane, just while choosing: the document follows
            // the selection, and the pane goes away after.
            KeyCode::Char('o') => self.focus_outline(true),
            KeyCode::Char('f') => self.show_hints(Purpose::Follow),
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
            Prompt::Hints { mut typed, purpose } => {
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
                    match (purpose, hn::link(&url)) {
                        (Purpose::Follow, _) => self.follow(&url),
                        (Purpose::Upvote, Some(Link::Item(id))) => self.upvote(id),
                        (Purpose::Reply, Some(Link::Item(id))) => self.reply(id),
                        _ => {}
                    }
                } else if partial {
                    self.prompt = Some(Prompt::Hints { typed, purpose });
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
            Prompt::SearchHn { mut query } => match key.code {
                KeyCode::Esc => {}
                // An HN link or an item's id (a long number: not a year, say)
                // opens it; anything else is searched for.
                KeyCode::Enter => match hn::parse(&query) {
                    Some(link) if query.trim().len() >= 6 => self.open_hn(link),
                    _ if query.trim().is_empty() => {}
                    _ => self.search_hn(query.trim().to_string()),
                },
                KeyCode::Backspace => {
                    query.pop();
                    self.prompt = Some(Prompt::SearchHn { query });
                }
                KeyCode::Char(c) if !ctrl => {
                    query.push(c);
                    self.prompt = Some(Prompt::SearchHn { query });
                }
                _ => self.prompt = Some(Prompt::SearchHn { query }),
            },
            p @ (Prompt::LoginUser { .. }
            | Prompt::LoginPassword { .. }
            | Prompt::Passphrase { .. }
            | Prompt::Logout { .. }
            | Prompt::Post { .. }) => self.act_prompt_key(p, key, ctrl),
            Prompt::Open(target) => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    self.flash = Some(match crate::open::open(&target) {
                        Ok(()) => format!("Opened {}", target.what),
                        Err(e) => format!("Couldn't open {}: {e}", target.what),
                    });
                }
                KeyCode::Char('c') => self.put(&target.target, &target.target),
                _ => {}
            },
            Prompt::Menu(mut menu) => match menu.key(key) {
                menu::Outcome::Stay => self.prompt = Some(Prompt::Menu(menu)),
                menu::Outcome::Close => {}
                menu::Outcome::Choose(choice) => self.choose(choice),
            },
            Prompt::Select { anchor, cursor } => {
                self.select_key(key, anchor, cursor);
            },
        }
        false
    }

    /// Labels the links on screen so one can be chosen by typing: any
    /// link to follow, or to upvote or reply, the links to the story and
    /// its comments (their ages).
    pub(super) fn show_hints(&mut self, purpose: Purpose) {
        let Some(doc) = self.current() else { return };
        let mut links = doc.visible_links();
        if purpose != Purpose::Follow {
            links.retain(|(_, _, url)| matches!(hn::link(url), Some(Link::Item(_))));
        }
        if links.is_empty() {
            self.flash = Some(match purpose {
                Purpose::Follow => "No links on screen".into(),
                _ => "No story or comments on screen".into(),
            });
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
            purpose,
        });
    }

    fn clear_hints(&mut self) {
        if let Some(doc) = self.current() {
            doc.set_hints(Vec::new());
        }
    }

    /// Follows a link: to a heading in the story, to a story or someone's
    /// page on HN (shown here), or (after asking) to the browser.
    pub(super) fn follow(&mut self, url: &str) {
        if let Some(link) = hn::link(url) {
            return self.open_hn(link);
        }
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
            None => self.open_outside(url),
        }
    }

    /// Asks to open `url` in the browser.
    pub(super) fn open_outside(&mut self, url: &str) {
        match crate::open::web(url) {
            Ok(target) => self.prompt = Some(Prompt::Open(target)),
            Err(why) => self.flash = Some(why),
        }
    }

    /// Opens an HN link here: a story (once it's loaded, if it isn't), or
    /// someone's page. A link to the story on screen goes to its comments.
    pub(super) fn open_hn(&mut self, link: Link) {
        match link {
            Link::User(name) => self.open_user(name),
            // A reply to you: its story's known.
            Link::Item(id) if self.reply_to_you(id).and_then(|r| r.story_id).is_some() => {
                let story = self.reply_to_you(id).and_then(|r| r.story_id).unwrap_or(id);
                self.open_comment(story, id);
            }
            Link::Item(id) => {
                let on_screen = self.user_page.is_none()
                    && self.current_key().is_some_and(|(shown, _)| shown == id);
                if on_screen {
                    if let Some(doc) = self.current()
                        && let Some(line) = super::comments_line(doc)
                    {
                        doc.jump_to(line);
                    }
                } else if self.stories.contains_key(&id) {
                    self.open_story_page(id);
                } else {
                    self.opening = Some(id);
                    self.opening_at = None;
                    self.flash = Some("Opening…".into());
                    self.ask(Asked::Story(id), Job::Story(id), true);
                }
            }
        }
    }

    /// Opens story `story` at comment `comment`: in place, if it's the
    /// one being read.
    pub(super) fn open_comment(&mut self, story: u64, comment: u64) {
        let reading = self.focus == Focus::Reader
            && self.user_page.is_none()
            && self.reading == Some(story);
        if reading {
            self.flash = None;
            self.doc((story, false)).go_to_comment(comment);
        } else if self.stories.contains_key(&story) {
            self.flash = None;
            self.open_story_page(story);
            self.doc((story, false)).go_to_comment(comment);
        } else {
            self.opening = Some(story);
            self.opening_at = Some(comment);
            self.flash = Some("Opening…".into());
            self.ask(Asked::Story(story), Job::Story(story), true);
        }
    }

    /// Where the reader is now, to come back to: `None` from the list.
    pub(super) fn here(&mut self) -> Option<Back> {
        if self.focus != Focus::Reader {
            return None;
        }
        let top = self.current()?.top();
        Some(match (&self.user_page, self.reading) {
            (Some(name), _) => Back::User {
                name: name.clone(),
                top,
            },
            (None, Some(id)) => Back::Story { id, top },
            (None, None) => return None,
        })
    }

    /// Reads story `id`, with Esc coming back here.
    pub(super) fn open_story_page(&mut self, id: u64) {
        match self.here() {
            Some(here) => self.history.push(here),
            None => self.history.clear(),
        }
        self.user_page = None;
        self.start_reading(id);
        self.focus = Focus::Reader;
        self.doc((id, false)).jump_to(0);
    }

    /// Shows someone's page, with Esc coming back here.
    fn open_user(&mut self, name: String) {
        match self.here() {
            Some(here) => self.history.push(here),
            None => self.history.clear(),
        }
        self.focus = Focus::Reader;
        self.user_page = Some(name.clone());
        self.user_doc(&name).jump_to(0);
        // Fetched afresh each time: it's who they are now that's wanted.
        self.send(Job::User(name), true);
    }

    /// Someone's page as a document, made the first time it's needed.
    pub(super) fn user_doc(&mut self, name: &str) -> &mut Doc {
        if !self.user_docs.contains_key(name) {
            let md = if name == super::REPLIES {
                self.replies_markdown()
            } else {
                crate::story::user_markdown(name, self.users.get(name), super::now())
            };
            self.user_docs.insert(name.to_string(), Doc::new(md));
        }
        self.user_docs.get_mut(name).unwrap()
    }

    /// Goes back to where the last link was followed from. Returns false
    /// if there's nowhere to go back to.
    pub(super) fn go_back(&mut self) -> bool {
        let Some(back) = self.history.pop() else {
            return false;
        };
        match back {
            Back::Story { id, top } => {
                self.user_page = None;
                self.start_reading(id);
                self.doc((id, false)).jump_to(top);
            }
            Back::User { name, top } => {
                self.user_page = Some(name.clone());
                self.user_doc(&name).jump_to(top);
            }
        }
        true
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
        if let Some(line) = self.act_footer() {
            return Some(line);
        }
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
            Prompt::Hints { typed, purpose } => Line::from(vec![
                match purpose {
                    Purpose::Follow => " Follow link: ".bold(),
                    Purpose::Upvote => " Upvote (a comment's age, or the story's comments link): ".bold(),
                    Purpose::Reply => " Reply to (a comment's age, or the story's comments link): ".bold(),
                },
                Span::raw(format!("type its letters {typed}")),
                "  esc cancels".dim(),
            ]),
            Prompt::Pick(_) => Line::from(" ↑↓ move  / filter  ⏎ go  esc close  q quit".dim()),
            Prompt::Menu(_) => Line::from(" its letter, or ↑↓ and ⏎, copies  esc close".dim()),
            Prompt::Select { .. } => self.select_footer(),
            // Their footers are act_footer's.
            Prompt::LoginUser { .. }
            | Prompt::LoginPassword { .. }
            | Prompt::Passphrase { .. }
            | Prompt::Logout { .. }
            | Prompt::Post { .. } => return None,
            Prompt::SearchHn { query } => Line::from(vec![
                " Search HN: ".bold(),
                Span::raw(query.clone()),
                "▏".slow_blink(),
                "  ⏎ search (an HN link or id: open it)  esc cancel".dim(),
            ]),
            Prompt::Open(target) => Line::from(vec![
                " Open ".bold(),
                Span::raw(target.what.clone()),
                "? ".bold(),
                "y/n, c copy  ".dim(),
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
