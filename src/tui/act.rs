//! Acting as you on HN: logging in (`L`), upvoting (`v`) and replying
//! (`r`). The login is looked for only when it's first needed, so a
//! keyring isn't asked for it until then; without one, it's asked for, and
//! what was being done carries on once it's there.

use super::compose::TextBox;
use super::nav::Page;
use super::nav::{Prompt, Purpose};
use super::{App, Asked, Focus};
use crate::auth::{self, Form, Session};
use crate::fetch::Job;
use crate::session::{self, Saved};
use crate::{editor, hn, store, story};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding};
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

/// A reply being written, in the box: what it's to, HN's form to post it
/// with once that's here, and where the draft's kept till it's posted, so
/// nothing's lost if posting fails.
pub struct Compose {
    pub target: u64,
    /// The story it'll show up on.
    pub story: u64,
    /// Who it's replying to, and what they said.
    pub who: String,
    pub quoted: String,
    pub form: Option<Result<Form, String>>,
    /// Ctrl-S before the form's here: post once it is.
    pub waiting: bool,
    /// What's happening, or went wrong, in place of the keys' hints.
    pub status: Option<String>,
    pub text: TextBox,
    /// Where the keys go: the text, or a button.
    pub on: On,
    pub path: PathBuf,
    /// Where the text and the buttons were drawn, for the mouse.
    pub text_at: Rect,
    pub cancel_at: Rect,
    pub post_at: Rect,
}

/// What has the keyboard in the reply box; Tab goes round them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum On {
    Text,
    Cancel,
    Post,
}

impl On {
    fn next(self, forward: bool) -> On {
        match (self, forward) {
            (On::Text, true) | (On::Post, false) => On::Cancel,
            (On::Cancel, true) | (On::Text, false) => On::Post,
            (On::Post, true) | (On::Cancel, false) => On::Text,
        }
    }
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
        if reader && self.page == Some(Page::Replies) {
            return Some(self.current()?.focused()?.id);
        }
        if reader && self.page.is_some() {
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
                // The box opens at once; HN's form comes meanwhile.
                let is_story = self.stories.contains_key(&id);
                let (who, quoted) = self.said(id);
                let dir = store::dir().unwrap_or_else(std::env::temp_dir);
                let path = dir.join("drafts").join(format!("reply-{id}.txt"));
                let kept = std::fs::read_to_string(&path)
                    .map(|t| auth::reply_text(&t))
                    .unwrap_or_default();
                let story = self.story_on_screen().unwrap_or(id);
                // What's being answered stays in view above the box.
                if let Some(doc) = self.current()
                    && doc.focused().is_some_and(|c| c.id == id)
                {
                    doc.go_to_comment(id);
                }
                self.flash = None;
                self.prompt = Some(Prompt::Compose(Box::new(Compose {
                    target: id,
                    story,
                    who,
                    quoted,
                    form: None,
                    waiting: false,
                    status: None,
                    text: TextBox::new(&kept),
                    on: On::Text,
                    path,
                    text_at: Rect::default(),
                    cancel_at: Rect::default(),
                    post_at: Rect::default(),
                })));
                self.send(Job::ReplyForm(session, id, is_story), true);
            }
        }
    }

    /// The story being read or previewed.
    fn story_on_screen(&mut self) -> Option<u64> {
        if self.page == Some(Page::Replies) {
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
                // Only y: Enter, as if to go ahead, has logged people out
                // who meant to log in.
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    session::forget(store::dir().as_deref());
                    store::set_user(store::dir().as_deref(), None);
                    self.auth = Auth::Out;
                    self.flash = Some(format!("Logged out {user}"));
                }
            }
            Prompt::Compose(c) => self.compose_key(c, key, ctrl),
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

    /// Keys in the reply box: Tab goes round the text and the Cancel and
    /// Post buttons; Esc and Ctrl-S are Cancel and Post from anywhere, and
    /// Ctrl-O goes to the editor. In the text, the rest write.
    fn compose_key(&mut self, mut c: Box<Compose>, key: KeyEvent, ctrl: bool) {
        let press = matches!(key.code, KeyCode::Enter | KeyCode::Char(' '));
        match key.code {
            KeyCode::Esc => return self.keep_reply(&c),
            KeyCode::Char('s') if ctrl => return self.post_reply(c),
            KeyCode::Char('o') if ctrl => {
                self.editing = Some(c);
                return;
            }
            KeyCode::Tab => c.on = c.on.next(true),
            KeyCode::BackTab => c.on = c.on.next(false),
            _ if c.on == On::Text => {
                c.text.key(key);
            }
            _ if press && c.on == On::Cancel => return self.keep_reply(&c),
            _ if press && c.on == On::Post => return self.post_reply(c),
            KeyCode::Left | KeyCode::Right => {
                c.on = if c.on == On::Cancel {
                    On::Post
                } else {
                    On::Cancel
                };
            }
            _ => {}
        }
        self.prompt = Some(Prompt::Compose(c));
    }

    /// A click in the reply box: a button's pressed, or the text gets the
    /// keyboard back. Returns whether the box took it: while it's open,
    /// clicks elsewhere do nothing, so none follows a link by mistake.
    pub(super) fn compose_click(&mut self, x: u16, y: u16) -> bool {
        let Some(Prompt::Compose(c)) = &mut self.prompt else {
            return false;
        };
        let at = ratatui::layout::Position { x, y };
        if c.text_at.contains(at) {
            c.on = On::Text;
        } else if c.cancel_at.contains(at) {
            if let Some(Prompt::Compose(c)) = self.prompt.take() {
                self.keep_reply(&c);
            }
        } else if c.post_at.contains(at)
            && let Some(Prompt::Compose(c)) = self.prompt.take()
        {
            self.post_reply(c);
        }
        true
    }

    /// Closes the box, keeping what's written (if anything) for `r` to
    /// carry on with.
    pub(super) fn keep_reply(&mut self, c: &Compose) {
        if c.text.is_empty() {
            let _ = std::fs::remove_file(&c.path);
            return;
        }
        self.flash = Some(match save_draft(&c.path, &c.text.text()) {
            Ok(()) => format!("Kept for later: r on {}'s again carries on", c.who),
            Err(e) => format!("Couldn't keep the draft: {e}"),
        });
    }

    /// Posts what's written, once HN's form is here; it's kept meanwhile,
    /// and if posting fails.
    fn post_reply(&mut self, mut c: Box<Compose>) {
        if c.text.is_empty() {
            c.status = Some("Nothing to post yet".into());
            self.prompt = Some(Prompt::Compose(c));
            return;
        }
        let _ = save_draft(&c.path, &c.text.text());
        let (Some(Ok(form)), Auth::In(session)) = (&c.form, &self.auth) else {
            if let (Some(Err(_)), Auth::In(session)) = (&c.form, &self.auth) {
                // It failed before: ask again.
                let is_story = self.stories.contains_key(&c.target);
                self.send(Job::ReplyForm(session.clone(), c.target, is_story), true);
                c.form = None;
            }
            c.waiting = true;
            c.status = Some("Posting as soon as HN's reply form is here…".into());
            self.prompt = Some(Prompt::Compose(c));
            return;
        };
        let job = Job::Post(
            session.clone(),
            form.clone(),
            c.text.text().trim().to_string(),
            c.story,
        );
        self.posting = Some(c.path.clone());
        self.flash = Some("Posting…".into());
        self.send(job, true);
    }

    /// HN's form for a reply: kept with the box it's for, open or in the
    /// editor, and used at once if Ctrl-S was waiting for it.
    pub(super) fn got_reply_form(&mut self, id: u64, result: Result<Form, String>) {
        let c = match (&mut self.prompt, &mut self.editing) {
            (Some(Prompt::Compose(c)), _) | (_, Some(c)) if c.target == id => c,
            _ => return,
        };
        let failed = result.as_ref().err().map(|e| format!("Can't reply: {e}"));
        c.form = Some(result);
        if !c.waiting {
            // Said now, but not in the way of writing.
            c.status = failed;
            return;
        }
        c.waiting = false;
        c.status = failed;
        if c.status.is_none()
            && let Some(Prompt::Compose(c)) = self.prompt.take()
        {
            self.post_reply(c);
        }
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
                self.flash = Some(
                    "Posted. It shows here once HN's search has it, usually within a minute".into(),
                );
                // Fetch the thread again, for it.
                self.asked.remove(&Asked::Thread(story));
            }
            Err(e) => {
                self.flash = Some(format!(
                    "Not posted: {e}. Your reply's kept: r to try again"
                ));
            }
        }
    }

    /// Hands the reply to the editor, with what it's replying to below a
    /// line, and back to the box with what was written.
    pub(super) fn edit_reply(
        &mut self,
        terminal: &mut DefaultTerminal,
        mut c: Box<Compose>,
    ) -> std::io::Result<()> {
        let text = format!("{}{}", c.text.text(), auth::draft(&c.who, &c.quoted));
        let written = save_draft(&c.path, &text);
        let result = match written {
            Ok(()) => self.hand_over(terminal, || editor::edit(&c.path, 1))?,
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => {
                let text = auth::reply_text(&std::fs::read_to_string(&c.path).unwrap_or_default());
                c.text = TextBox::new(&text);
            }
            Err(e) => c.status = Some(format!("Couldn't edit: {e}")),
        }
        self.prompt = Some(Prompt::Compose(c));
        Ok(())
    }

    /// The reply box, over the bottom of the screen, so what it answers
    /// stays in view above it.
    pub(super) fn draw_compose(&mut self, f: &mut Frame) {
        let frame = self.theme.frame;
        let Some(Prompt::Compose(c)) = &mut self.prompt else {
            return;
        };
        let area = f.area();
        let height = (area.height * 2 / 5)
            .clamp(7, 18)
            .min(area.height.saturating_sub(2));
        let rect = Rect {
            x: area.x + 1,
            // Above the footer.
            y: area.bottom().saturating_sub(height + 1),
            width: area.width.saturating_sub(2),
            height,
        };
        // The keys are in the footer; here, just what's happening.
        let hints = match &c.status {
            Some(status) => Line::from(format!(" {status} ").yellow()),
            None if c.form.is_none() => Line::from(" getting HN's reply form… ".dim()),
            None => Line::default(),
        };
        let border = frame.map_or(Style::new(), |c| Style::new().fg(c));
        let block = Block::bordered()
            .border_style(border)
            .title(format!(" Reply to {} ", crate::safe::printable(&c.who)))
            .title_bottom(hints)
            .padding(Padding::horizontal(1));
        let inner = block.inner(rect);
        f.render_widget(Clear, rect);
        f.render_widget(block, rect);
        // The text, then a blank line and the buttons, on the right.
        let text_area = Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        };
        c.text.draw(f, text_area, c.on == On::Text);
        c.text_at = text_area;
        let accent = frame.map_or(Style::new().reversed(), |c| {
            Style::new().fg(Color::Black).bg(c)
        });
        let button = |label: &str, on: bool| {
            let span = Span::raw(format!(" {label} "));
            if on {
                span.style(accent.bold())
            } else {
                span.style(Style::new().bold().reversed())
            }
        };
        let buttons = Line::from(vec![
            button("Cancel", c.on == On::Cancel),
            Span::raw("  "),
            button("Post", c.on == On::Post),
        ])
        .right_aligned();
        let row = Rect {
            y: inner.bottom().saturating_sub(1),
            height: 1,
            ..inner
        };
        // Where each landed, on the right: " Cancel ", two spaces, " Post ".
        let (cancel_w, post_w) = (8, 6);
        let post_x = row.right().saturating_sub(post_w);
        c.post_at = Rect::new(post_x, row.y, post_w, 1);
        c.cancel_at = Rect::new(post_x.saturating_sub(2 + cancel_w), row.y, cancel_w, 1);
        f.render_widget(buttons, row);
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
                format!(" You're logged in as {user}. ").bold(),
                "y".bold(),
                " logs out; any other key keeps you logged in".dim(),
            ]),
            Prompt::Compose(c) => Line::from(vec![
                format!(" Replying to {}: ", crate::safe::printable(&c.who)).bold(),
                "tab".bold(),
                " to the buttons  ".dim(),
                "ctrl-s".bold(),
                " post  ".dim(),
                "esc".bold(),
                " cancel (it's kept for later)  ".dim(),
                "ctrl-o".bold(),
                " editor".dim(),
            ]),
            _ => return None,
        };
        Some(line)
    }
}

/// Writes `text` to the draft at `path`.
fn save_draft(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
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
    comments.iter().find_map(|c| {
        if c.id == id {
            Some(c)
        } else {
            find(&c.replies, id)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::Fetcher;
    use crate::store::{Cache, SeenStore};
    use crate::theme::Theme;
    use ratatui::crossterm::event::KeyModifiers;

    fn app() -> App {
        let mut app = App::new(
            Theme::plain(),
            None,
            Fetcher::start(Cache::none()),
            SeenStore::load(None, 0),
        );
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
        let footer: String = app
            .act_footer()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(
            footer.contains("••••••") && !footer.contains("secret"),
            "{footer}"
        );
        // Esc gives up on the login and what was waiting for it.
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.prompt.is_none() && app.pending.is_none());
    }

    #[test]
    fn the_reply_box_writes_goes_round_its_buttons_and_keeps_drafts() {
        let mut app = app();
        let path = std::env::temp_dir().join(format!("lshn-test-draft-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let open = |app: &mut App, text: &str| {
            app.prompt = Some(Prompt::Compose(Box::new(Compose {
                target: 7,
                story: 1,
                who: "bob".into(),
                quoted: "hi".into(),
                form: None,
                waiting: false,
                status: None,
                text: TextBox::new(text),
                on: On::Text,
                path: path.clone(),
                text_at: Rect::default(),
                cancel_at: Rect::default(),
                post_at: Rect::default(),
            })));
        };
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let compose = |app: &App| match &app.prompt {
            Some(Prompt::Compose(c)) => (c.text.text(), c.on, c.waiting),
            _ => panic!("the box closed"),
        };
        open(&mut app, "");
        for c in "Hi q".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert_eq!(compose(&app).0, "Hi q\n", "q and Enter write, in the box");
        // Tab: Cancel, Post, back to the text.
        app.key(key(KeyCode::Tab));
        assert_eq!(compose(&app).1, On::Cancel);
        app.key(key(KeyCode::Right));
        assert_eq!(compose(&app).1, On::Post);
        app.key(key(KeyCode::Char('x')));
        assert_eq!(
            compose(&app).0,
            "Hi q\n",
            "on a button, typing doesn't write"
        );
        app.key(key(KeyCode::Tab));
        assert_eq!(compose(&app).1, On::Text);
        // Cancel keeps it for later.
        app.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        app.key(key(KeyCode::BackTab));
        assert_eq!(compose(&app).1, On::Cancel);
        app.key(key(KeyCode::Enter));
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Hi q\n");
        // Posting before HN's form is here: it waits for it.
        open(&mut app, "Hi");
        app.auth = Auth::In(Session {
            cookie: "bob&x".into(),
        });
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(compose(&app).2, "waiting for the form");
        // An empty box leaves no draft behind.
        open(&mut app, "  ");
        app.key(key(KeyCode::Esc));
        assert!(!path.exists());
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
