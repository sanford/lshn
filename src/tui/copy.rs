//! Copying (`c`): a menu of the story's link, its HN page, a Markdown link
//! to it, the comment being read or a link to it, and the article, each
//! saying just what it'll copy. Dragging over the text with the mouse
//! copies what it covers (see `mouse`).

use super::menu::{Item, Menu};
use super::nav::Prompt;
use super::{App, Focus};
use crate::article::Article;
use crate::clipboard;
use crate::hn::{self, Comment};

/// What choosing an item copies, and what the flash calls it.
pub(super) type Copy = (String, String);

/// "1 line", "42 lines".
fn lines(n: usize) -> String {
    format!("{n} line{}", if n == 1 { "" } else { "s" })
}

/// How much `text` is: its words, or if it's more than one line, its
/// lines.
pub(super) fn amount(text: &str) -> String {
    match text.lines().count() {
        0 | 1 => {
            let n = text.split_whitespace().count();
            format!("{n} word{}", if n == 1 { "" } else { "s" })
        }
        n => lines(n),
    }
}

/// `[text](url)`, with the brackets in `text` escaped.
fn markdown_link(text: &str, url: &str) -> String {
    let text = text.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
    format!("[{text}](<{url}>)")
}

/// The comment `id`, among `comments` and their replies.
fn find(comments: &[Comment], id: u64) -> Option<&Comment> {
    comments
        .iter()
        .find_map(|c| if c.id == id { Some(c) } else { find(&c.replies, id) })
}

/// An item that copies `text`: its detail is `text` itself.
fn copies(key: char, label: &str, text: String) -> Item<Copy> {
    Item::new(key, label, text.clone(), (text.clone(), text))
}

impl App {
    /// `c`: what to copy, and what each choice will copy.
    pub(super) fn open_copy(&mut self) {
        let reader_shown = self.focus == Focus::Reader || self.kept();
        let items = match (reader_shown, self.user_page.clone()) {
            (true, Some(name)) => {
                let url = hn::user_url(&name);
                vec![
                    copies('h', "Their HN page", url.clone()),
                    copies('m', "Markdown link", markdown_link(&name, &url)),
                ]
            }
            _ => match self.story_items() {
                Some(items) => items,
                None => return,
            },
        };
        self.prompt = Some(Prompt::Menu(Menu::new("Copy", items)));
    }

    /// The story's links, the comment being read, and the article.
    fn story_items(&mut self) -> Option<Vec<Item<Copy>>> {
        let id = match self.current_key() {
            Some((id, _)) => id,
            None => self.selected_id()?,
        };
        let story = self.stories.get(&id)?;
        let (url, hn_url, title) = (story.url.clone(), story.hn_url(), story.title.clone());
        let mut items = vec![
            match &url {
                Some(url) => copies('l', "Link", url.clone()),
                None => Item::off('l', "Link", "none: it's on HN"),
            },
            copies('h', "HN page", hn_url.clone()),
            copies(
                'm',
                "Markdown link",
                markdown_link(&title, url.as_deref().unwrap_or(&hn_url)),
            ),
        ];

        // The comment being read, while reading.
        let focused = if self.focus == Focus::Reader {
            self.current().and_then(|d| Some((d.focused()?.id, d.focused_text())))
        } else {
            None
        };
        let author = |cid| {
            let thread = self.threads.get(&id)?.as_ref().ok()?;
            let by = &find(thread, cid)?.by;
            Some(if by.is_empty() { "[deleted]".to_string() } else { by.clone() })
        };
        match focused {
            Some((cid, text)) => {
                let by = author(cid).unwrap_or_default();
                items.push(match text {
                    Some(text) => {
                        let what = format!("{by}'s, {}", amount(&text));
                        Item::new('c', "Comment", what.clone(), (text, format!("{by}'s comment")))
                    }
                    None => Item::off('c', "Comment", "it has no text"),
                });
                items.push(copies('k', "Comment's link", hn::item_url(cid)));
            }
            None => {
                let why = "none being read: in the comments, j and k pick one";
                items.push(Item::off('c', "Comment", why));
                items.push(Item::off('k', "Comment's link", why));
            }
        }

        items.push(match self.articles.get(&id) {
            Some(Article::Text { md, words }) => {
                let site = url.as_deref().and_then(hn::domain).unwrap_or_default();
                let what = format!("from {site}, {words} words, as Markdown");
                Item::new('a', "Article", what, (md.clone(), "the article".into()))
            }
            Some(Article::Unreadable(why)) => {
                Item::off('a', "Article", format!("couldn't be read: {why}"))
            }
            None if url.is_none() => Item::off('a', "Article", "none: it's on HN"),
            None => Item::off('a', "Article", "not read yet"),
        });
        Some(items)
    }

    /// Copies `text`, and says so: "Copied `what`".
    pub(super) fn put(&mut self, text: &str, what: &str) {
        self.flash = Some(match clipboard::copy(text) {
            Ok(how) => format!("Copied {what} {how}"),
            Err(e) => format!("Couldn't copy: {e}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_much() {
        assert_eq!(amount("one two three\n"), "3 words");
        assert_eq!(amount("a\n\nb\n"), "3 lines");
        assert_eq!(markdown_link("Ask [HN]", "https://x"), "[Ask \\[HN\\]](<https://x>)");
    }
}
