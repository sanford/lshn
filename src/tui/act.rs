//! Acting as you on HN: logging in (`L`), upvoting (`v`) and replying
//! (`r`). The login is looked for only when it's first needed, so a
//! keyring isn't asked for it until then; without one, it's asked for, and
//! what was being done carries on once it's there.

use super::nav::{Prompt, Purpose};
use super::{App, Asked, Focus};
use crate::auth::{self, Form, Session};
use crate::fetch::Job;
use crate::session::{self, Saved};
use crate::{editor, hn, story, store};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::path::PathBuf;

/// Whether you're logged in, as far as lshn knows.
pub enum Auth {
    /// Not looked for yet.
    Unknown,
    Out,
    /// Saved in the encrypted file: needs its passphrase.
    Locked,
    In(Session),
}

/// Something to do as you, waiting for the login.
#[derive(Clone, Copy)]
pub enum Action {
    Upvote(u64),
    Reply(u64),
}

/// A reply being written: what it's to, and where the draft is kept (until
/// it's posted, so nothing's lost if posting fails).
#[derive(Clone)]
pub struct Draft {
    pub form: Form,
    /// The story it'll show up on.
    pub story: u64,
    /// Who it's replying to, and what they said.
    pub who: String,
    pub quoted: String,
    pub path: PathBuf,
}

/// What a passphrase is for.
#[derive(Clone)]
pub enum Passphrase {
    /// Opening the saved session.
    Unlock,
    /// Saving this session, where there's no keyring.
    Save(Session),
}

impl App {
    /// `v`: in the list, upvotes the selected story; reading, the comment
    /// being read, or above the comments, the story.
    pub(super) fn upvote_key(&mut self) {
        match self.selected_story_in_list().or_else(|| self.acting_on()) {
            Some(id) => self.upvote(id),
            None => self.show_hints(Purpose::Upvote),
        }
    }

    /// `r`: in the list, replies to the selected story; reading, to the
    /// comment being read, or above the comments, the story.
    pub(super) fn reply_key(&mut self) {
        match self.selected_story_in_list().or_else(|| self.acting_on()) {
            Some(id) => self.reply(id),
            None => self.show_hints(Purpose::Reply),
        }
    }

    /// What `r` and `v` act on while reading: the focused comment, or else
    /// the story. `None` on someone's page, where there's no telling.
    pub(super) fn acting_on(&mut self) -> Option<u64> {
        let reader = self.focus == Focus::Reader || self.kept();
        if reader && self.user_page.as_deref() == Some(super::REPLIES) {
            return Some(self.current()?.focused()?.id);
        }
        if reader && self.user_page.is_some() {
            return None;
        }
        let (story, _) = self.current_key()?;
        Some(self.current()?.focused().map_or(story, |c| c.id))
    }

    /// The selected story, when the list has the keyboard and it's what's
    /// beside it.
    fn selected_story_in_list(&self) -> Option<u64> {
        (self.focus == Focus::List && !self.kept())
            .then(|| self.selected_id())
            .flatten()
    }

    /// `L`: logs in, or out.
    pub(super) fn login_key(&mut self) {
        self.find_login();
        match &self.auth {
            Auth::In(s) => {
                self.prompt = Some(Prompt::Logout {
                    user: s.user().to_string(),
                });
            }
            _ => self.ask_login(None),
        }
    }

    pub(super) fn upvote(&mut self, id: u64) {
        self.as_you(Action::Upvote(id));
    }

    pub(super) fn reply(&mut self, id: u64) {
        self.as_you(Action::Reply(id));
    }

    /// Looks for a saved login, the first time it's needed.
    pub(super) fn find_login(&mut self) {
        if !matches!(self.auth, Auth::Unknown) {
            return;
        }
        self.auth = match session::load(store::dir().as_deref()) {
            Saved::Keyring(cookie) => Auth::In(Session { cookie }),
            Saved::File => Auth::Locked,
            Saved::Nothing => Auth::Out,
        };
    }

    /// Does `action` as you, once logged in.
    fn as_you(&mut self, action: Action) {
        self.find_login();
        match &self.auth {
            Auth::In(s) => {
                let s = s.clone();
                self.act(s, action);
            }
            Auth::Locked => {
                self.pending = Some(action);
                self.prompt = Some(Prompt::Passphrase {
                    text: String::new(),
                    purpose: Passphrase::Unlock,
                });
            }
            Auth::Out | Auth::Unknown => self.ask_login(Some(action)),
        }
    }

    fn ask_login(&mut self, then: Option<Action>) {
        self.pending = then;
        self.prompt = Some(Prompt::LoginUser {
            user: String::new(),
        });
    }

    fn act(&mut self, session: Session, action: Action) {
        match action {
            Action::Upvote(id) => {
                self.flash = Some("Upvoting…".into());
                self.send(Job::Upvote(session, id), true);
            }
            Action::Reply(id) => {
                self.flash = Some("Getting the reply form…".into());
                let is_story = self.stories.contains_key(&id);
                self.replying = Some((id, self.story_on_screen()));
                self.send(Job::ReplyForm(session, id, is_story), true);
            }
        }
    }

    /// The story being read or previewed.
    fn story_on_screen(&mut self) -> Option<u64> {
        if self.user_page.as_deref() == Some(super::REPLIES) {
            let id = self.current()?.focused()?.id;
            return self.reply_to_you(id)?.story_id;
        }
        self.current_key().map(|(id, _)| id)
    }

    /// Keys for the login, passphrase, post and logout prompts. The prompt
    /// stays up only if this puts it back.
    pub(super) fn act_prompt_key(&mut self, prompt: Prompt, key: KeyEvent, ctrl: bool) {
        match prompt {
            Prompt::LoginUser { mut user } => match key.code {
                KeyCode::Esc => self.pending = None,
                KeyCode::Enter if hn::is_username(user.trim()) => {
                    self.prompt = Some(Prompt::LoginPassword {
                        user: user.trim().to_string(),
                        password: String::new(),
                    });
                }
                _ => {
                    edit(&mut user, key, ctrl);
                    self.prompt = Some(Prompt::LoginUser { user });
                }
            },
            Prompt::LoginPassword { user, mut password } => match key.code {
                KeyCode::Esc => self.pending = None,
                KeyCode::Enter if !password.is_empty() => {
                    self.flash = Some("Logging in…".into());
                    self.send(Job::Login(user, password), true);
                }
                _ => {
                    edit(&mut password, key, ctrl);
                    self.prompt = Some(Prompt::LoginPassword { user, password });
                }
            },
            Prompt::Passphrase { mut text, purpose } => match key.code {
                KeyCode::Esc => {
                    if let Passphrase::Save(s) = purpose {
                        // Logged in for now, but not kept.
                        self.auth = Auth::In(s);
                        self.flash = Some("Logged in until lshn quits".into());
                        self.run_pending();
                    } else {
                        self.pending = None;
                    }
                }
                KeyCode::Enter if !text.is_empty() => self.passphrase(text, purpose),
                _ => {
                    edit(&mut text, key, ctrl);
                    self.prompt = Some(Prompt::Passphrase { text, purpose });
                }
            },
            Prompt::Logout { user } => {
                if matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) {
                    session::forget(store::dir().as_deref());
                    store::set_user(store::dir().as_deref(), None);
                    self.auth = Auth::Out;
                    self.flash = Some(format!("Logged out {user}"));
                }
            }
            Prompt::Post { draft, text } => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    let Auth::In(session) = &self.auth else {
                        return;
                    };
                    self.flash = Some("Posting…".into());
                    self.posting = Some(draft.path.clone());
                    let job = Job::Post(session.clone(), draft.form, text, draft.story);
                    self.send(job, true);
                }
                KeyCode::Char('e' | 'E') => self.editing = Some(draft),
                KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q') => {
                    self.flash = Some(format!(
                        "Not posted. The draft's kept in {}",
                        super::display_path(&draft.path)
                    ));
                }
                _ => self.prompt = Some(Prompt::Post { draft, text }),
            },
            _ => {}
        }
    }

    fn passphrase(&mut self, text: String, purpose: Passphrase) {
        let Some(dir) = store::dir() else { return };
        match purpose {
            Passphrase::Unlock => match session::open_file(&dir, &text) {
                Ok(cookie) => {
                    self.auth = Auth::In(Session { cookie });
                    self.run_pending();
                }
                Err(e) => {
                    self.flash = Some(format!("{e}: try again, or esc"));
                    self.prompt = Some(Prompt::Passphrase {
                        text: String::new(),
                        purpose: Passphrase::Unlock,
                    });
                }
            },
            Passphrase::Save(s) => {
                self.flash = Some(match session::save_file(&dir, &s.cookie, &text) {
                    Ok(()) => format!("Logged in as {}, kept in ~/.lshn/session", s.user()),
                    Err(e) => format!("Logged in until lshn quits (couldn't save it: {e})"),
                });
                self.auth = Auth::In(s);
                self.run_pending();
            }
        }
    }

    /// Does what was waiting for the login.
    fn run_pending(&mut self) {
        if let (Some(action), Auth::In(s)) = (self.pending.take(), &self.auth) {
            let s = s.clone();
            self.act(s, action);
        }
    }

    pub(super) fn logged_in(&mut self, result: Result<Session, String>) {
        let s = match result {
            Ok(s) => s,
            Err(e) => {
                self.pending = None;
                self.flash = Some(format!("Couldn't log in: {e}"));
                return;
            }
        };
        // Who you are, for the replies to you: not a secret.
        store::set_user(store::dir().as_deref(), Some(s.user()));
        match session::save_keyring(&s.cookie) {
            Ok(()) => {
                self.flash = Some(format!("Logged in as {}", s.user()));
                self.auth = Auth::In(s);
                self.run_pending();
            }
            Err(_) => {
                self.flash = Some(
                    "No keyring here: a passphrase keeps the login in ~/.lshn/session (esc: just for now)"
                        .into(),
                );
                self.prompt = Some(Prompt::Passphrase {
                    text: String::new(),
                    purpose: Passphrase::Save(s),
                });
            }
        }
    }

    pub(super) fn upvoted(&mut self, result: Result<auth::Voted, String>) {
        self.flash = Some(match result {
            Ok(auth::Voted::Up) => "Upvoted: v again takes it back".into(),
            Ok(auth::Voted::Un) => "Unvoted".into(),
            Err(e) => format!("Not voted: {e}"),
        });
    }

    /// With the form to reply with: to the editor.
    pub(super) fn got_reply_form(&mut self, id: u64, result: Result<Form, String>) {
        let Some((target, story)) = self.replying.take().filter(|(t, _)| *t == id) else {
            return;
        };
        let form = match result {
            Ok(form) => form,
            Err(e) => {
                self.flash = Some(format!("Can't reply: {e}"));
                return;
            }
        };
        let (who, quoted) = self.said(target);
        let dir = store::dir().unwrap_or_else(std::env::temp_dir);
        self.flash = None;
        self.editing = Some(Draft {
            form,
            story: story.unwrap_or(target),
            who,
            quoted,
            path: dir.join("drafts").join(format!("reply-{target}.txt")),
        });
    }

    /// Who wrote item `id`, and what they said: a story's title, or a
    /// comment's text.
    pub(super) fn said(&self, id: u64) -> (String, String) {
        if let Some(s) = self.stories.get(&id) {
            return (s.by.clone(), s.title.clone());
        }
        if let Some(r) = self.reply_to_you(id) {
            return (r.by.clone(), story::html_to_text(&r.text));
        }
        self.threads
            .values()
            .filter_map(|t| t.as_ref().ok())
            .find_map(|comments| find(comments, id))
            .map(|c| (c.by.clone(), story::html_to_text(&c.text)))
            .unwrap_or_default()
    }

    pub(super) fn posted(&mut self, story: u64, result: Result<(), String>) {
        let draft = self.posting.take();
        match result {
            Ok(()) => {
                if let Some(path) = draft {
                    let _ = std::fs::remove_file(path);
                }
                self.flash =
                    Some("Posted. It shows here once HN's search has it, usually within a minute".into());
                // Fetch the thread again, for it.
                self.asked.remove(&Asked::Thread(story));
            }
            Err(e) => {
                self.flash = Some(format!("Not posted: {e}. Your reply's kept: r to try again"));
            }
        }
    }

    /// Writes the reply in the editor, then asks before posting it. A draft
    /// kept from before carries on where it was.
    pub(super) fn edit_reply(&mut self, terminal: &mut DefaultTerminal, draft: Draft) -> std::io::Result<()> {
        let text = std::fs::read_to_string(&draft.path)
            .ok()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| auth::draft(&draft.who, &draft.quoted));
        if let Some(dir) = draft.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&draft.path, text)?;
        // Hand the terminal to the editor until it's done.
        let result = self.hand_over(terminal, || editor::edit(&draft.path, 1))?;
        if let Err(e) = result {
            self.flash = Some(format!("Couldn't edit: {e}"));
            return Ok(());
        }
        let reply = auth::reply_text(&std::fs::read_to_string(&draft.path).unwrap_or_default());
        if reply.is_empty() {
            let _ = std::fs::remove_file(&draft.path);
            self.flash = Some("Not posted: the reply was empty".into());
        } else {
            self.prompt = Some(Prompt::Post { draft, text: reply });
        }
        Ok(())
    }

    /// The reply, before it's posted, and what it's replying to.
    pub(super) fn draw_post(&self, f: &mut Frame) {
        let Some(Prompt::Post { draft, text }) = &self.prompt else {
            return;
        };
        let area = f.area();
        let width = area.width.saturating_sub(8).min(90);
        let mut lines = vec![
            Line::from(format!("Replying to {}:", draft.who)).dim(),
            Line::from(format!("> {}", first_line(&draft.quoted))).dim(),
            Line::default(),
        ];
        lines.extend(text.lines().map(|l| Line::from(crate::safe::printable(l).into_owned())));
        let inner_w = usize::from(width.saturating_sub(4)).max(1);
        let rows: usize = lines
            .iter()
            .map(|l| l.width().div_ceil(inner_w).max(1))
            .sum();
        let height = (rows as u16 + 2).min(area.height.saturating_sub(2));
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::bordered()
                    .title(" Post this reply? ")
                    .padding(Padding::horizontal(1)),
            ),
            rect,
        );
    }

    /// The footer for these prompts.
    pub(super) fn act_footer(&self) -> Option<Line<'static>> {
        let masked = |s: &str| Span::raw("•".repeat(s.chars().count()));
        let line = match self.prompt.as_ref()? {
            Prompt::LoginUser { user } => Line::from(vec![
                " Log in to HN as: ".bold(),
                Span::raw(user.clone()),
                "▏".slow_blink(),
                "  ⏎ next  esc cancel".dim(),
            ]),
            Prompt::LoginPassword { user, password } => Line::from(vec![
                format!(" {user}'s password: ").bold(),
                masked(password),
                "▏".slow_blink(),
                "  ⏎ log in  esc cancel  (only HN's session is kept)".dim(),
            ]),
            Prompt::Passphrase { text, purpose } => Line::from(vec![
                match purpose {
                    Passphrase::Unlock => " Passphrase for your HN login: ".bold(),
                    Passphrase::Save(_) => " Choose a passphrase to keep the login: ".bold(),
                },
                masked(text),
                "▏".slow_blink(),
                "  ⏎ ok  esc cancel".dim(),
            ]),
            Prompt::Logout { user } => Line::from(vec![
                format!(" Log out {user}? ").bold(),
                "y/n".dim(),
            ]),
            Prompt::Post { .. } => Line::from(vec![
                " y".bold(),
                " post  ".dim(),
                "e".bold(),
                " edit  ".dim(),
                "n".bold(),
                " not now (the draft's kept)".dim(),
            ]),
            _ => return None,
        };
        Some(line)
    }
}

/// Typing into a prompt's text.
fn edit(text: &mut String, key: KeyEvent, ctrl: bool) {
    match key.code {
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char('u') if ctrl => text.clear(),
        KeyCode::Char(c) if !ctrl => text.push(c),
        _ => {}
    }
}

/// Comment `id`, among these and their replies.
pub(super) fn find(comments: &[hn::Comment], id: u64) -> Option<&hn::Comment> {
    comments
        .iter()
        .find_map(|c| if c.id == id { Some(c) } else { find(&c.replies, id) })
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::Fetcher;
    use crate::store::{Cache, SeenStore};
    use crate::theme::Theme;
    use ratatui::crossterm::event::KeyModifiers;

    fn app() -> App {
        let mut app = App::new(Theme::plain(), None, Fetcher::start(Cache::none()), SeenStore::load(None, 0));
        // Never the real keyring, in tests.
        app.auth = Auth::Out;
        app
    }

    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    #[test]
    fn logged_out_asks_to_log_in_and_keeps_what_was_asked() {
        let mut app = app();
        app.ids = Some(vec![7]);
        app.refresh();
        app.key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(matches!(app.prompt, Some(Prompt::LoginUser { .. })));
        assert!(matches!(app.pending, Some(Action::Upvote(7))));
        type_in(&mut app, "pg");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Prompt::LoginPassword { user, .. }) = &app.prompt else {
            panic!("no password prompt");
        };
        assert_eq!(user, "pg");
        type_in(&mut app, "secret");
        // The password never shows.
        let footer: String = app.act_footer().unwrap().spans.iter().map(|s| s.content.to_string()).collect();
        assert!(footer.contains("••••••") && !footer.contains("secret"), "{footer}");
        // Esc gives up on the login and what was waiting for it.
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.prompt.is_none() && app.pending.is_none());
    }

    #[test]
    fn a_written_reply_is_posted_edited_or_kept() {
        let mut app = app();
        let draft = Draft {
            form: Form {
                parent: "1".into(),
                goto: "item?id=1".into(),
                hmac: "x".into(),
            },
            story: 1,
            who: "bob".into(),
            quoted: "hi".into(),
            path: std::env::temp_dir().join("lshn-test-draft.txt"),
        };
        let post = |draft: &Draft| Prompt::Post {
            draft: draft.clone(),
            text: "My reply".into(),
        };
        app.prompt = Some(post(&draft));
        app.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert!(app.editing.is_some() && app.prompt.is_none());
        app.editing = None;
        app.prompt = Some(post(&draft));
        app.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(app.flash.as_deref().unwrap().contains("draft's kept"));
        // Other keys leave it up.
        app.prompt = Some(post(&draft));
        app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(matches!(app.prompt, Some(Prompt::Post { .. })));
    }

    #[test]
    fn finds_comments_anywhere_in_a_thread() {
        let c = |id, replies| hn::Comment {
            id,
            replies,
            ..hn::Comment::default()
        };
        let thread = vec![c(1, vec![c(2, vec![c(3, vec![])])]), c(4, vec![])];
        assert_eq!(find(&thread, 3).map(|c| c.id), Some(3));
        assert_eq!(find(&thread, 4).map(|c| c.id), Some(4));
        assert!(find(&thread, 5).is_none());
    }
}
