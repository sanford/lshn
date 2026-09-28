//! The interactive reader: stories on the left, the selected one on the
//! right (title, article, then comments), and the story full screen.

mod mouse;
mod nav;
mod outline;
mod picker;
mod themes;

use crate::article::Article;
use crate::doc::Doc;
use crate::fetch::{Done, Fetcher, Job};
use crate::hn::{Comment, Feed, Story};
use crate::omarchy::Follow;
use crate::story::{self, Comments};
use crate::theme::{Choice, Mode, Theme};
use crate::{clipboard, safe, wrap};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{HashMap, HashSet};
use std::io;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Stories fetched ahead of the selection, so the list fills in before
/// it's scrolled to.
const LIST_AHEAD: usize = 40;
/// Stories either side of the selection whose article and comments are
/// fetched ahead, so they're there when selected.
const PREFETCH_BEHIND: usize = 2;
const PREFETCH_AHEAD: usize = 5;
/// Terminals at least this wide keep the list on screen when `Tab` goes to
/// the story: the story beside it still gets about 75 columns of text, a
/// better line length for reading than the whole width.
const WIDE: u16 = 130;

/// How stories are shown.
pub struct Settings {
    /// The widest to wrap text.
    pub max_width: Option<usize>,
    /// Scroll and click with the mouse. (Selecting text then needs a
    /// modifier key in most terminals.)
    pub mouse: bool,
    /// Open stories with the outline pane beside them.
    pub outline: bool,
    /// The theme asked for, by flag or config.
    pub choice: Choice,
    /// Colors come from Omarchy's theme, and follow it.
    pub omarchy: bool,
    /// The list to start with.
    pub feed: Feed,
}

pub fn run(theme: Theme, settings: Settings) -> io::Result<()> {
    // Previewing `auto` in the theme picker needs the terminal's background,
    // and it can't be asked once the screen is ours.
    let terminal_dark = match settings.choice {
        Choice::Mode(Mode::Auto) if !settings.omarchy => theme.dark,
        _ if theme.color && !settings.omarchy => crate::theme::detect_dark(),
        _ => true,
    };
    let mut app = App::new(theme, settings.max_width, Fetcher::start());
    app.choice = settings.choice;
    app.terminal_dark = terminal_dark;
    if settings.omarchy {
        app.follow = Follow::start();
    }
    app.omarchy = settings.omarchy;
    app.mouse_on = settings.mouse;
    app.outline_pane = settings.outline;
    app.open_feed(settings.feed);
    let mut terminal = ratatui::init();
    set_mouse(app.mouse_on, true);
    if app.mouse_on {
        // ratatui's panic hook restores the terminal, but doesn't know
        // about the mouse: turn it off first, or the shell gets mouse codes.
        let restore = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            set_mouse(true, false);
            restore(info);
        }));
    }
    let result = app.run(&mut terminal);
    set_mouse(app.mouse_on, false);
    ratatui::restore();
    result
}

/// Turns mouse reporting on or off, if lshn uses the mouse.
fn set_mouse(used: bool, on: bool) {
    use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use ratatui::crossterm::execute;
    if used {
        let _ = if on {
            execute!(io::stdout(), EnableMouseCapture)
        } else {
            execute!(io::stdout(), DisableMouseCapture)
        };
    }
}

#[derive(PartialEq, Eq)]
enum Focus {
    List,
    Reader,
}

/// What's been asked of the fetcher, so nothing's asked twice.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Asked {
    Feed(Feed),
    Story(u64),
    Thread(u64),
    Article(u64),
}

/// A story that passes the filter, with the positions of the matched
/// characters in its title.
struct Shown {
    id: u64,
    hits: Vec<u32>,
}

/// A story's document: the list's preview, or the full one.
type DocKey = (u64, bool);

struct App {
    theme: Rc<Theme>,
    /// The theme shown: what was asked for, or what's being previewed.
    choice: Choice,
    /// The theme and choice from before the theme picker opened, while it's
    /// open.
    theme_before: Option<(Rc<Theme>, Choice)>,
    terminal_dark: bool,
    omarchy: bool,
    follow: Option<Follow>,
    max_width: Option<usize>,

    fetcher: Fetcher,
    asked: HashSet<Asked>,
    /// Jobs asked for and not yet back.
    waiting: usize,
    feed: Feed,
    /// The feed's stories, in order, once it's loaded.
    ids: Option<Vec<u64>>,
    feed_error: Option<String>,
    stories: HashMap<u64, Story>,
    /// Stories that couldn't be loaded, or are dead or deleted.
    gone: HashSet<u64>,
    threads: HashMap<u64, Result<Vec<Comment>, String>>,
    articles: HashMap<u64, Article>,
    docs: HashMap<DocKey, Doc>,

    filter: String,
    typing: bool,
    matcher: Matcher,
    shown: Vec<Shown>,
    list: ListState,
    /// Rows the list has room for (a story can take more than one).
    list_rows: usize,
    /// How many rows each story in the list took, at the last draw.
    list_heights: Vec<usize>,
    /// Where the list's rows were drawn (empty when it isn't shown).
    list_area: Rect,

    focus: Focus,
    /// The story open full screen.
    reading: Option<u64>,
    /// Keep the list on screen while reading.
    list_in_reader: bool,
    /// The width stories and the list share, at the last draw.
    body_width: u16,
    /// Where `c` came to the comments from, to go back to.
    before_comments: HashMap<DocKey, usize>,
    /// Show the outline pane beside the story while reading.
    outline_pane: bool,
    /// Where the outline pane's rows were drawn (empty when it isn't shown).
    outline_area: Rect,
    outline_list: ListState,
    /// The outline pane has the keyboard: the heading selected in it, and
    /// where the document was when it took the keyboard.
    outline_focus: Option<(usize, usize)>,
    /// Text typed after `/` in the outline, narrowing its headings.
    outline_filter: Option<String>,
    /// `o` opened the pane just while it has the keyboard.
    outline_temporary: bool,
    help: bool,
    /// A prompt or popup that has the keyboard.
    prompt: Option<nav::Prompt>,
    /// The last search of a document, for `C-s` on an empty prompt.
    last_search: String,
    /// A message for the footer, until the next key.
    flash: Option<String>,
    /// The mouse is in use.
    mouse_on: bool,
}

impl App {
    fn new(theme: Theme, max_width: Option<usize>, fetcher: Fetcher) -> App {
        App {
            theme: Rc::new(theme),
            choice: Choice::Mode(Mode::Auto),
            theme_before: None,
            terminal_dark: true,
            omarchy: false,
            follow: None,
            max_width,
            fetcher,
            asked: HashSet::new(),
            waiting: 0,
            feed: Feed::Top,
            ids: None,
            feed_error: None,
            stories: HashMap::new(),
            gone: HashSet::new(),
            threads: HashMap::new(),
            articles: HashMap::new(),
            docs: HashMap::new(),
            filter: String::new(),
            typing: false,
            matcher: Matcher::new(Config::DEFAULT),
            shown: Vec::new(),
            list: ListState::default(),
            list_rows: 0,
            list_heights: Vec::new(),
            list_area: Rect::default(),
            focus: Focus::List,
            reading: None,
            list_in_reader: false,
            body_width: 0,
            before_comments: HashMap::new(),
            outline_pane: false,
            outline_area: Rect::default(),
            outline_list: ListState::default(),
            outline_focus: None,
            outline_filter: None,
            outline_temporary: false,
            help: false,
            prompt: None,
            last_search: String::new(),
            flash: None,
            mouse_on: false,
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            self.receive();
            self.restyle();
            self.preview_theme();
            self.prefetch();
            terminal.draw(|f| self.draw(f))?;
            // While fetching, wake up often to show what's come; otherwise
            // now and then, to keep the ages current.
            let wait = if self.waiting > 0 {
                Duration::from_millis(30)
            } else {
                Duration::from_secs(1)
            };
            if !event::poll(wait)? {
                continue;
            }
            let key = match event::read()? {
                Event::Key(key) => key,
                Event::Mouse(m) => {
                    self.flash = None;
                    self.mouse(m);
                    continue;
                }
                _ => continue, // Resizes redraw at the top of the loop.
            };
            if key.kind == KeyEventKind::Press && self.key(key) {
                return Ok(());
            }
        }
    }

    /// Asks the fetcher for something, unless it's been asked already.
    fn ask(&mut self, what: Asked, job: Job, urgent: bool) {
        if self.asked.insert(what) {
            self.waiting += 1;
            self.fetcher.push(job, urgent);
        } else if urgent {
            // Maybe still waiting behind prefetches: move it up.
            self.fetcher.hurry(&job);
        }
    }

    fn open_feed(&mut self, feed: Feed) {
        self.feed = feed;
        self.ids = None;
        self.feed_error = None;
        self.filter.clear();
        self.typing = false;
        self.asked.remove(&Asked::Feed(feed));
        self.ask(Asked::Feed(feed), Job::Feed(feed), true);
        self.refresh();
        self.list.select(None);
        *self.list.offset_mut() = 0;
    }

    /// Fetches everything again, keeping what's shown until it's replaced.
    /// Articles don't change, so they stay.
    fn reload(&mut self) {
        self.asked.retain(|a| matches!(a, Asked::Article(_)));
        self.gone.clear();
        let feed = self.feed;
        self.ask(Asked::Feed(feed), Job::Feed(feed), true);
        self.flash = Some(format!("Reloading {}…", feed.name()));
    }

    /// Takes in whatever the fetcher has sent back.
    fn receive(&mut self) {
        let mut list_changed = false;
        while let Ok(done) = self.fetcher.done.try_recv() {
            self.waiting = self.waiting.saturating_sub(1);
            match done {
                Done::Feed(feed, result) if feed == self.feed => {
                    match result {
                        Ok(ids) => {
                            self.ids = Some(ids);
                            self.feed_error = None;
                        }
                        Err(e) => self.feed_error = Some(e),
                    }
                    list_changed = true;
                }
                Done::Feed(..) => {}
                Done::Story(id, Ok(story)) if !story.dead && !story.deleted => {
                    self.stories.insert(id, story);
                    self.rebuild(id);
                    list_changed = true;
                }
                Done::Story(id, _) => {
                    self.gone.insert(id);
                    list_changed = true;
                }
                Done::Thread(id, result) => {
                    self.threads.insert(id, result);
                    self.rebuild(id);
                }
                Done::Article(id, article) => {
                    self.articles.insert(id, article);
                    self.rebuild(id);
                }
            }
        }
        if list_changed {
            self.refresh();
        }
    }

    /// Asks for the stories around the selection: the list's rows, and the
    /// comments and articles of the stories nearest it, the selected one
    /// first.
    fn prefetch(&mut self) {
        let Some(ids) = self.ids.clone() else { return };
        let sel = self.list.selected().unwrap_or(0);
        let selected_id = self.selected_id();
        // The rows on screen, and those a little way below; all of them
        // while filtering, so the filter sees every title.
        let first = self.list.offset().min(ids.len());
        let end = (sel + LIST_AHEAD).max(first + self.list_rows).min(ids.len());
        let (first, end) = if self.filter.is_empty() {
            (first, end)
        } else {
            (0, ids.len())
        };
        for &id in &ids[first..end] {
            if !self.stories.contains_key(&id) {
                self.ask(Asked::Story(id), Job::Story(id), false);
            }
        }
        // The selected story's, then its neighbours' (in the list as shown).
        let near: Vec<u64> = {
            let lo = sel.saturating_sub(PREFETCH_BEHIND);
            let hi = (sel + PREFETCH_AHEAD + 1).min(self.shown.len());
            self.shown[lo.min(hi)..hi].iter().map(|s| s.id).collect()
        };
        for id in selected_id.into_iter().chain(self.reading).chain(near) {
            let urgent = Some(id) == selected_id || Some(id) == self.reading;
            let Some(story) = self.stories.get(&id) else {
                self.ask(Asked::Story(id), Job::Story(id), urgent);
                continue;
            };
            let url = story.url.clone();
            let kids = story.kids.clone();
            // Comments come quickly and articles slowly: asking for the
            // article first puts the comments in front of it.
            if let Some(url) = url {
                self.ask(Asked::Article(id), Job::Article(id, url), urgent);
            }
            self.ask(Asked::Thread(id), Job::Thread(id, kids), urgent);
        }
    }

    fn selected_id(&self) -> Option<u64> {
        Some(self.shown.get(self.list.selected()?)?.id)
    }

    /// The story's document as it stands.
    fn markdown(&self, id: u64, preview: bool) -> String {
        let Some(story) = self.stories.get(&id) else {
            return "*Loading…*".into();
        };
        let comments = match self.threads.get(&id) {
            None => Comments::Loading,
            Some(Ok(c)) => Comments::Loaded(c),
            Some(Err(e)) => Comments::Failed(e),
        };
        story::markdown(story, self.articles.get(&id), comments, preview, now())
    }

    /// Brings a story's documents up to date with what's been fetched.
    fn rebuild(&mut self, id: u64) {
        for preview in [true, false] {
            if self.docs.contains_key(&(id, preview)) {
                let md = self.markdown(id, preview);
                self.docs.get_mut(&(id, preview)).unwrap().replace(md);
            }
        }
    }

    fn doc(&mut self, key: DocKey) -> &mut Doc {
        if !self.docs.contains_key(&key) {
            let doc = Doc::new(self.markdown(key.0, key.1));
            self.docs.insert(key, doc);
        }
        self.docs.get_mut(&key).unwrap()
    }

    /// The document on screen: the story being read, or else the selected
    /// one's preview.
    fn current_key(&self) -> Option<DocKey> {
        match self.focus {
            Focus::Reader => self.reading.map(|id| (id, false)),
            // The story being read stays whole, and where it was, while
            // it's selected; the others show their previews.
            Focus::List => self
                .selected_id()
                .map(|id| (id, Some(id) != self.reading)),
        }
    }

    fn current(&mut self) -> Option<&mut Doc> {
        let key = self.current_key()?;
        Some(self.doc(key))
    }

    /// Re-filters the list, keeping the same story selected.
    fn refresh(&mut self) {
        let keep = self.selected_id();
        let ids = self.ids.as_deref().unwrap_or_default();
        let listed = ids.iter().filter(|id| !self.gone.contains(id));
        if self.filter.is_empty() {
            self.shown = listed
                .map(|&id| Shown {
                    id,
                    hits: Vec::new(),
                })
                .collect();
        } else {
            // Each word, anywhere in the title: fuzzier finds too much
            // in titles as long as these.
            let pattern = Pattern::new(
                &self.filter,
                CaseMatching::Smart,
                Normalization::Smart,
                AtomKind::Substring,
            );
            let mut buf = Vec::new();
            let mut scored = Vec::new();
            for &id in listed {
                let Some(story) = self.stories.get(&id) else {
                    continue;
                };
                let mut hits = Vec::new();
                let hay = Utf32Str::new(&story.title, &mut buf);
                if let Some(score) = pattern.indices(hay, &mut self.matcher, &mut hits) {
                    hits.sort_unstable();
                    hits.dedup();
                    scored.push((score, Shown { id, hits }));
                }
            }
            // Best first; HN's order among equals.
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            self.shown = scored.into_iter().map(|(_, s)| s).collect();
        }
        let index = keep.and_then(|id| self.shown.iter().position(|s| s.id == id));
        self.list
            .select(index.or((!self.shown.is_empty()).then_some(0)));
    }

    /// Switches to the Omarchy theme's new colors, if it has changed.
    fn restyle(&mut self) {
        let Some(palette) = self.follow.as_ref().and_then(Follow::changed) else {
            return;
        };
        // Following is only on with color.
        self.set_theme(Rc::new(Theme::new(Mode::Auto, true, Some(&palette))));
    }

    fn set_theme(&mut self, theme: Rc<Theme>) {
        self.theme = theme;
        for doc in self.docs.values_mut() {
            doc.restyle();
        }
    }

    fn select_by(&mut self, delta: isize) {
        if self.shown.is_empty() {
            return;
        }
        let i = self
            .list
            .selected()
            .unwrap_or(0)
            .saturating_add_signed(delta);
        self.list.select(Some(i.min(self.shown.len() - 1)));
    }

    /// Opens the selected story full screen, where the preview was: at the
    /// same comment, or the same part of the article.
    fn read_selected(&mut self) {
        let Some(id) = self.selected_id() else { return };
        if self.reading == Some(id) {
            // Already on screen whole: carry on from there.
            self.focus = Focus::Reader;
            return;
        }
        let preview = self.doc((id, true));
        let in_comments = comments_line(preview).is_some_and(|line| preview.top() >= line);
        let heading = preview
            .current_heading()
            .map(|i| preview.headings()[i].slug.clone());
        let place = preview.place().filter(|_| preview.top() > 0);
        self.reading = Some(id);
        self.focus = Focus::Reader;
        let full = self.doc((id, false));
        match (in_comments, heading, place) {
            (true, Some(slug), _) => full.go_to_anchor(&slug),
            (false, _, Some(place)) => full.keep_place(place),
            _ => {}
        }
    }

    /// `>` `<`: the next or previous story. Reading, it opens there and
    /// then; in the list, it's the same as moving.
    fn next_story(&mut self, delta: isize) {
        let before = self.list.selected();
        self.select_by(delta);
        if self.list.selected() == before {
            self.flash = Some(if delta > 0 { "Last story" } else { "First story" }.into());
            return;
        }
        if self.focus == Focus::Reader {
            self.read_selected();
        }
    }

    /// `Tab`: from the list to the story and back. On a wide terminal
    /// both stay on screen, and `Tab` just moves between them; otherwise
    /// the story has the screen to itself.
    fn switch_view(&mut self) {
        match self.focus {
            Focus::List => {
                self.read_selected();
                self.list_in_reader = self.body_width >= WIDE;
            }
            Focus::Reader => self.focus = Focus::List,
        }
    }

    /// `c`: to the comments, and back to where that came from.
    fn toggle_comments(&mut self) {
        let Some(key) = self.current_key() else { return };
        let back = self.before_comments.get(&key).copied();
        let doc = self.doc(key);
        let Some(line) = comments_line(doc) else {
            self.flash = Some("Still loading".into());
            return;
        };
        let top = doc.top();
        if top < line {
            doc.jump_to(line);
            self.before_comments.insert(key, top);
        } else {
            doc.jump_to(back.unwrap_or(0));
            self.before_comments.remove(&key);
        }
    }

    /// `w`: the story's link in the browser (or its HN page, with `W` or
    /// when it has no link).
    fn open_in_browser(&mut self, hn_page: bool) {
        let id = match self.focus {
            Focus::Reader => self.reading,
            Focus::List => self.selected_id(),
        };
        let Some(story) = id.and_then(|id| self.stories.get(&id)) else {
            return;
        };
        let url = match &story.url {
            Some(url) if !hn_page => url.clone(),
            _ => story.hn_url(),
        };
        match crate::open::web(&url) {
            Ok(target) => {
                self.flash = Some(match crate::open::open(&target) {
                    Ok(()) => format!("Opened {}", target.target),
                    Err(e) => format!("Couldn't open {}: {e}", target.target),
                });
            }
            Err(why) => self.flash = Some(why),
        }
    }

    fn copy_link(&mut self, hn_page: bool) {
        let id = match self.focus {
            Focus::Reader => self.reading,
            Focus::List => self.selected_id(),
        };
        let Some(story) = id.and_then(|id| self.stories.get(&id)) else {
            return;
        };
        let url = match &story.url {
            Some(url) if !hn_page => url.clone(),
            _ => story.hn_url(),
        };
        self.flash = Some(match clipboard::copy(&url) {
            Ok(how) => format!("Copied {url} {how}"),
            Err(e) => format!("Couldn't copy: {e}"),
        });
    }

    /// Keys that work the same in the list and the reader.
    fn view_key(&mut self, key: KeyEvent, ctrl: bool) -> bool {
        match key.code {
            KeyCode::Tab => self.switch_view(),
            KeyCode::Char('>') => self.next_story(1),
            KeyCode::Char('<') => self.next_story(-1),
            KeyCode::Char('c') if !ctrl => self.toggle_comments(),
            KeyCode::Char('w') if !ctrl => self.open_in_browser(false),
            KeyCode::Char('W') => self.open_in_browser(true),
            KeyCode::Char('y') if !ctrl => self.copy_link(false),
            KeyCode::Char('Y') => self.copy_link(true),
            KeyCode::Char('r') if !ctrl => self.reload(),
            KeyCode::Char('O') if self.focus == Focus::Reader => self.focus_outline(false),
            KeyCode::Char('O') => self.outline_pane = !self.outline_pane,
            KeyCode::Char('t') if !ctrl => self.open_themes(),
            KeyCode::Char(c @ '1'..='6') => {
                let feed = Feed::ALL[c as usize - '1' as usize];
                self.focus = Focus::List;
                if feed != self.feed {
                    self.open_feed(feed);
                }
            }
            _ => return false,
        }
        true
    }

    /// Handles a key. Returns true to quit.
    fn key(&mut self, key: KeyEvent) -> bool {
        let key = emacs(key);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return true;
        }
        self.flash = None;
        if self.help {
            self.help = false;
            return false;
        }
        if let Some(prompt) = self.prompt.take() {
            return self.prompt_key(prompt, key, ctrl);
        }
        if self.typing {
            self.filter_key(key, ctrl);
            return false;
        }
        if self.outline_focus.is_some() {
            if self.focus == Focus::Reader && self.outline_pane {
                return self.outline_key(key);
            }
            self.leave_outline();
        }
        match key.code {
            KeyCode::Char('Q') => return true,
            KeyCode::Char('?') => self.help = true,
            _ if self.view_key(key, ctrl) => {}
            _ if self.focus == Focus::List => return self.list_key(key, ctrl),
            _ => return self.reader_key(key, ctrl),
        }
        false
    }

    fn filter_key(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Down => return self.select_by(1),
            KeyCode::Up => return self.select_by(-1),
            KeyCode::Enter => return self.typing = false,
            KeyCode::Esc => {
                self.typing = false;
                self.filter.clear();
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    self.typing = false;
                }
            }
            KeyCode::Char(c) if !ctrl => self.filter.push(c),
            _ => return,
        }
        self.refresh();
        // The best match.
        self.list
            .select((!self.shown.is_empty()).then_some(0));
    }

    fn list_key(&mut self, key: KeyEvent, ctrl: bool) -> bool {
        let page = self.list_rows.max(1) as isize;
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('d') if ctrl => self.select_by(page / 2),
            KeyCode::Char('u') if ctrl => self.select_by(-page / 2),
            KeyCode::Char('J') => self.select_by(page),
            KeyCode::Char('K') => self.select_by(-page),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => self.select_by(page),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.select_by(-page),
            KeyCode::Char('j') | KeyCode::Down => self.select_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.select_by(-1),
            KeyCode::PageDown => self.select_by(page),
            KeyCode::PageUp => self.select_by(-page),
            KeyCode::Char('g') | KeyCode::Home => self.select_by(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.select_by(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.read_selected(),
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.refresh();
            }
            KeyCode::Esc => return true,
            // Page through the preview without opening it.
            KeyCode::Char(' ') => {
                if let Some(doc) = self.current() {
                    doc.scroll_by(doc.page());
                }
            }
            KeyCode::Char('b') => {
                if let Some(doc) = self.current() {
                    doc.scroll_by(-doc.page());
                }
            }
            // The rest of the story keys work on the preview too.
            _ => {
                let search = matches!(key.code, KeyCode::Char('s' | 'r'));
                if !ctrl || search {
                    self.nav_key(key);
                }
            }
        }
        false
    }

    fn reader_key(&mut self, key: KeyEvent, ctrl: bool) -> bool {
        // Of the Ctrl keys, only Emacs's searches are for getting around.
        let search = matches!(key.code, KeyCode::Char('s' | 'r'));
        if (!ctrl || search) && self.nav_key(key) {
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            // Back to the list. Esc never quits from here: that's for the
            // list, so a Left too many never loses your place.
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                self.focus = Focus::List;
                return false;
            }
            KeyCode::Char('\\') => {
                self.list_in_reader = !self.list_in_reader;
                return false;
            }
            // What ↑ and ↓ do in the list: the next and previous story.
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.next_story(1);
                return false;
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.next_story(-1);
                return false;
            }
            _ => {}
        }
        let Some(doc) = self.current() else {
            return false;
        };
        let page = doc.page();
        let half = (page / 2).max(1);
        match key.code {
            KeyCode::Char('J') => doc.scroll_by(page),
            KeyCode::Char('K') => doc.scroll_by(-page),
            KeyCode::Char('d') if ctrl => doc.scroll_by(half),
            KeyCode::Char('u') if ctrl => doc.scroll_by(-half),
            KeyCode::Char('f') if ctrl => doc.scroll_by(page),
            KeyCode::Char('b') if ctrl => doc.scroll_by(-page),
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => doc.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => doc.scroll_by(-1),
            KeyCode::Char('d') => doc.scroll_by(half),
            KeyCode::Char('u') => doc.scroll_by(-half),
            KeyCode::Char(' ') | KeyCode::PageDown => doc.scroll_by(page),
            KeyCode::Char('b') | KeyCode::PageUp => doc.scroll_by(-page),
            KeyCode::Char('g') | KeyCode::Home => doc.scroll_to_top(),
            KeyCode::Char('G') | KeyCode::End => doc.scroll_to_bottom(),
            _ => {}
        }
        false
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(f.area());

        self.list_area = Rect::default();
        let reader_only = self.focus == Focus::Reader && !self.list_in_reader;
        self.body_width = body.width;
        let narrow = body.width < 80;
        if reader_only || (narrow && self.focus == Focus::Reader) {
            // A column of margin on each side.
            let area = Rect {
                x: body.x + 1,
                width: body.width.saturating_sub(2),
                ..body
            };
            self.draw_doc(f, area);
        } else if narrow {
            self.draw_list(f, body);
        } else {
            // Wide enough for the titles, up to 2/5 of the screen: the
            // story beside it is what's read, so it keeps the most room.
            // Longer titles wrap rather than lose their ends.
            let widest = self
                .shown
                .iter()
                .filter_map(|s| self.stories.get(&s.id))
                .map(|story| wrap::width(&story.title))
                .max()
                .unwrap_or(0);
            let want = widest + 5; // borders, the dot and margins
            let cap = usize::from(body.width) * 2 / 5;
            let list_w = want.clamp(30, cap.max(30)) as u16;
            let [list_area, doc_area] =
                Layout::horizontal([Constraint::Length(list_w), Constraint::Min(0)]).areas(body);
            self.draw_list(f, list_area);
            self.draw_preview(f, doc_area);
        }

        // After the document, so it's laid out.
        self.draw_header(f, header);
        self.draw_footer(f, footer);
        self.draw_picker(f);
        if self.help {
            draw_help(f);
        }
        self.theme.paint(f.buffer_mut());
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect) {
        let mut title = vec![" lshn ".bold(), Span::raw(" ")];
        for (i, feed) in Feed::ALL.iter().enumerate() {
            let name = format!("{} {}", i + 1, feed.name());
            title.push(if *feed == self.feed {
                Span::raw(name).bold().underlined()
            } else {
                Span::raw(name).dim()
            });
            title.push(Span::raw("  "));
        }
        let used: usize = title.iter().map(|s| s.width()).sum();
        // Reading: which section the top of the screen is in.
        let section: Vec<String> = match self.focus {
            Focus::Reader => self
                .current()
                .map(|d| d.section().iter().map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            Focus::List => Vec::new(),
        };
        // The story's title is already on screen; the trail is what's under it.
        let section = section.get(1..).unwrap_or_default();
        let room = usize::from(area.width).saturating_sub(used + 3);
        if let Some(trail) = section_trail(section, room) {
            title.push(Span::raw("§ ").dim());
            title.push(Span::raw(trail));
        }
        f.render_widget(Paragraph::new(Line::from(title)), area);
    }

    fn pane(&self, title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
        let block = Block::bordered().title(title);
        if focused {
            block
        } else {
            block.border_style(Style::new().dim())
        }
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let total = self.ids.as_ref().map(|ids| ids.len() - self.gone_in_feed());
        let count = match total {
            None => format!(" {} ", self.feed.name()),
            Some(total) if self.filter.is_empty() => format!(" {} ({total}) ", self.feed.name()),
            Some(total) => format!(" {} ({} of {total}) ", self.feed.name(), self.shown.len()),
        };
        // The filter shows on the list it narrows, not down in the footer.
        let mut title = vec![Span::raw(count)];
        if self.typing || !self.filter.is_empty() {
            title.extend(["/".bold(), Span::raw(self.filter.clone()).yellow()]);
            title.push(if self.typing {
                "▏".slow_blink()
            } else {
                Span::raw("")
            });
            title.push(Span::raw(" "));
        }
        let block = self.pane(Line::from(title), self.focus == Focus::List);
        let inner = block.inner(area);
        self.list_rows = usize::from(inner.height);
        self.list_area = inner;
        let width = usize::from(inner.width);
        let items: Vec<Vec<Line>> = self
            .shown
            .iter()
            .map(|s| title_lines(self.stories.get(&s.id), &s.hits, width))
            .collect();
        self.list_heights = items.iter().map(Vec::len).collect();
        let items: Vec<ListItem> = items.into_iter().map(ListItem::new).collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(self.list_highlight());
        f.render_stateful_widget(list, area, &mut self.list);

        let msg = match (&self.ids, &self.feed_error) {
            (_, Some(e)) => Some(format!("Couldn't load {}: {e}", self.feed.name())),
            (None, None) => Some("Loading…".into()),
            (Some(_), None) if self.shown.is_empty() && !self.filter.is_empty() => {
                Some("Nothing matches".into())
            }
            (Some(_), None) if self.shown.is_empty() => Some("No stories".into()),
            _ => None,
        };
        if let Some(msg) = msg {
            f.render_widget(Paragraph::new(format!(" {msg}")).dim(), inner);
        }
    }

    /// The selected story's highlight: full while the list has the
    /// keyboard, and softer (the code blocks' tint) while the story beside
    /// it does, so the eye goes where the keys will.
    fn list_highlight(&self) -> Style {
        let reversed = Style::new().add_modifier(Modifier::REVERSED);
        if self.focus == Focus::List {
            return reversed;
        }
        match self.theme.code_bg {
            Some(_) => self.theme.code_block(),
            None => reversed.add_modifier(Modifier::DIM),
        }
    }

    /// Stories in this feed that won't be listed.
    fn gone_in_feed(&self) -> usize {
        let ids = self.ids.as_deref().unwrap_or_default();
        ids.iter().filter(|id| self.gone.contains(id)).count()
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let block = self
            .pane("", self.focus == Focus::Reader)
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.draw_doc(f, inner);
    }

    fn draw_doc(&mut self, f: &mut Frame, area: Rect) {
        self.outline_area = Rect::default();
        let mut area = area;
        let mut pane = None;
        if self.outline_pane && self.focus == Focus::Reader {
            let w = Self::outline_width(area.width);
            pane = Some(Rect {
                x: area.right() - w,
                width: w,
                ..area
            });
            // Leave a column for the scrollbar, and one more for air.
            area.width = area.width.saturating_sub(w + 2);
        }
        let mut width = usize::from(area.width);
        if let Some(max) = self.max_width {
            width = width.min(max);
        }
        let theme = Rc::clone(&self.theme);
        if let Some(doc) = self.current() {
            doc.draw(f, area, width, &theme);
        }
        // After the document, so it shows the section it's scrolled to.
        if let Some(pane) = pane {
            self.draw_outline_pane(f, pane);
        }
    }

    fn draw_footer(&mut self, f: &mut Frame, area: Rect) {
        if let Some(line) = self.prompt_footer() {
            f.render_widget(Paragraph::new(line), area);
            return;
        }
        if let Some(msg) = &self.flash {
            f.render_widget(Paragraph::new(Line::from(format!(" {msg}")).yellow()), area);
            return;
        }
        let keys: Vec<(&str, &str)> = if self.outline_focus.is_some() {
            if self.outline_filter.is_some() {
                vec![("↑↓", "move"), ("⏎", "read here"), ("esc", "clear")]
            } else {
                vec![
                    ("↑↓", "move"),
                    ("/", "filter"),
                    ("⏎", "read here"),
                    ("esc", "back"),
                    ("O", "close"),
                    ("q", "quit"),
                ]
            }
        } else if self.typing {
            // The filter itself shows in the list's border.
            vec![("↑↓", "move"), ("⏎", "done"), ("esc", "clear")]
        } else {
            // Esc clears a search before it goes back.
            let searching = self.current().is_some_and(|d| d.search_status().is_some());
            match self.focus {
                Focus::List => {
                    let mut keys = vec![
                        ("↑↓", "move"),
                        ("→ tab", "read"),
                        ("c", "comments"),
                        ("space", "page"),
                        ("w", "open"),
                        ("/", "filter"),
                        ("1-6", "lists"),
                    ];
                    if !self.filter.is_empty() {
                        keys.push(("esc", "clear filter"));
                    }
                    keys.extend([("?", "help"), ("q", "quit")]);
                    keys
                }
                Focus::Reader => vec![
                    ("↑↓", "scroll"),
                    ("c", "comments"),
                    ("]", "next comment"),
                    ("⇧↑↓", "next story"),
                    ("/", "search"),
                    ("f", "follow"),
                    ("w", "open"),
                    ("← tab", if searching { "clear search" } else { "list" }),
                    ("?", "help"),
                ],
            }
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, what) in keys {
            spans.push(k.bold());
            spans.push(Span::raw(format!(" {what}  ")).dim());
        }
        // The search's match count, while there is one, then the position.
        let position = self
            .current()
            .map(|d| match d.search_status() {
                Some(matches) => format!("{matches} matches  {}", d.position()),
                None => d.position(),
            })
            .unwrap_or_default();
        let fetching = if self.waiting > 0 { "⋯ " } else { "" };
        let position = format!("{fetching}{position}");
        let pos_width = wrap::width(&position) as u16 + 1;
        let [keys_area, pos_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(pos_width)]).areas(area);
        f.render_widget(Paragraph::new(Line::from(spans)), keys_area);
        f.render_widget(
            Paragraph::new(Line::from(format!("{position} ")).dim()).right_aligned(),
            pos_area,
        );
    }
}

/// `path` with the home directory shown as `~`.
pub fn display_path(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
        };
    }
    path.display().to_string()
}

/// The line the comments start on, once the document's laid out.
fn comments_line(doc: &Doc) -> Option<usize> {
    doc.headings()
        .iter()
        .find(|h| h.level == 2 && h.text.starts_with(story::COMMENTS_HEADING))
        .map(|h| h.line)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A story's title in the list, after a dot, with filter matches
/// highlighted, wrapped at spaces to fit `width`: titles are what the list is for, so they're
/// never cut short. (The rest of what's known about a story is at the top
/// of its page.)
fn title_lines(story: Option<&Story>, hits: &[u32], width: usize) -> Vec<Line<'static>> {
    let Some(story) = story else {
        return vec![Line::from(" • …").dim()];
    };
    const INDENT: &str = "   ";
    let hit = Style::new().yellow().bold();
    let title = safe::printable(&story.title);
    let chars: Vec<(char, Style)> = title
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let style = if hits.binary_search(&(i as u32)).is_ok() {
                hit
            } else {
                Style::new()
            };
            (c, style)
        })
        .collect();

    // Break at the last space that fits, or mid-word if a word alone
    // doesn't.
    let mut rows: Vec<&[(char, Style)]> = Vec::new();
    let mut rest = &chars[..];
    while !rest.is_empty() {
        let room = width.saturating_sub(INDENT.len());
        let room = room.max(1);
        let mut used = 0;
        let mut end = rest.len();
        let mut last_space = None;
        for (i, &(c, _)) in rest.iter().enumerate() {
            let w = char_width(c);
            if used + w > room {
                end = i.max(1);
                break;
            }
            if c == ' ' {
                last_space = Some(i);
            }
            used += w;
        }
        // A row that ends just before a space breaks at that space.
        if end < rest.len() && rest[end].0 == ' ' {
            last_space = Some(end);
        }
        if end < rest.len()
            && let Some(space) = last_space.filter(|&s| s > 0)
        {
            rows.push(&rest[..space]);
            rest = &rest[space + 1..];
        } else {
            rows.push(&rest[..end]);
            rest = &rest[end..];
        }
    }

    rows.into_iter()
        .enumerate()
        .map(|(n, row)| {
            // A dot marks where each title starts; the rest of a wrapped
            // title lines up with its text, not the dot.
            let lead = if n == 0 {
                Span::raw(" • ").dim()
            } else {
                Span::raw(INDENT)
            };
            let mut spans: Vec<Span> = vec![lead];
            for &(c, style) in row {
                match spans.last_mut() {
                    Some(span) if span.style == style => span.content.to_mut().push(c),
                    _ => spans.push(Span::styled(c.to_string(), style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn char_width(c: char) -> usize {
    wrap::width(c.encode_utf8(&mut [0; 4]))
}

fn draw_help(f: &mut Frame) {
    const KEYS: &[(&str, &str)] = &[
        ("↑↓ j k", "Move / scroll"),
        ("⏎ → l", "Read the selected story full screen"),
        ("tab", "Between the list and the story (both stay, if there's room)"),
        ("⇧↓ ⇧↑ > <", "Next / previous story, reading"),
        ("esc ← h", "Back to the list (esc quits there)"),
        ("q", "Quit"),
        ("c", "To the comments, and back"),
        ("] [", "Next / previous comment (or heading)"),
        ("o", "Outline: the text follows as you move (/ filters)"),
        ("O", "Keep the outline open beside the story"),
        ("w W", "Open the story's link / its HN page in the browser"),
        ("y Y", "Copy the story's link / its HN page"),
        ("1-6", "Top, New, Best, Ask, Show, Jobs"),
        ("r", "Reload"),
        ("t", "Pick a color theme"),
        ("/ n N", "Search; next / previous match"),
        ("^s ^r", "Search forward / back; typing: next / previous"),
        ("f", "Follow a link (type the letters shown on it)"),
        ("J K", "Page down / up (⇧↓ ⇧↑ too, in the list)"),
        ("space b", "Page down / up (the preview, in the list)"),
        ("d u", "Half page down / up"),
        ("g G", "Top / bottom"),
        ("^n ^p", "Emacs: down / up"),
        ("^v M-v", "Emacs: page down / up"),
        ("M-< M->", "Emacs: top / bottom (^g: esc)"),
        ("/", "Filter the list by title (fuzzy)"),
        ("\\", "Show or hide the list while reading"),
        ("Q", "Quit from anywhere"),
    ];
    let key_width = KEYS.iter().map(|(k, _)| wrap::width(k)).max().unwrap_or(0);
    let rows: Vec<Line> = KEYS
        .iter()
        .map(|(k, what)| {
            let pad = key_width - wrap::width(k);
            Line::from(vec![
                format!("{k}{}   ", " ".repeat(pad)).bold(),
                Span::raw(*what),
            ])
        })
        .collect();
    let area = f.area();
    let mut lines = help_columns(rows, area);
    lines.push(Line::default());
    lines.push(Line::from("Press any key to close").dim());
    let width = (lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" Keys ")
                .padding(Padding::horizontal(1)),
        ),
        rect,
    );
}

/// "Comments (187) › alice · 3h ago", dropping outer sections to fit
/// `room` columns.
fn section_trail(section: &[String], room: usize) -> Option<String> {
    for skip in 0..section.len() {
        let mut trail = section[skip..].join(" › ");
        if skip > 0 {
            trail.insert_str(0, "… › ");
        }
        if wrap::width(&trail) <= room {
            return Some(trail);
        }
    }
    // Even the innermost section alone is too long: cut it short, unless
    // there's too little room for that to say anything.
    let last = section.last()?;
    if room < 8 {
        return None;
    }
    let mut trail = String::from("… › ");
    for c in last.chars() {
        if wrap::width(&trail) + wrap::width(c.encode_utf8(&mut [0; 4])) + 1 > room {
            break;
        }
        trail.push(c);
    }
    trail.push('…');
    Some(trail)
}

/// The help's rows, in one column, or in two side by side when one is too
/// tall for the screen and two fit across it.
fn help_columns(rows: Vec<Line<'static>>, area: Rect) -> Vec<Line<'static>> {
    // The border and padding, and the blank line and "Press any key" below.
    const FRAME_HEIGHT: usize = 4;
    const FRAME_WIDTH: usize = 4;
    const GAP: usize = 4;
    let column = rows.iter().map(Line::width).max().unwrap_or(0);
    let tall = rows.len() + FRAME_HEIGHT > usize::from(area.height);
    if !tall || 2 * column + GAP + FRAME_WIDTH > usize::from(area.width) {
        return rows;
    }
    let half = rows.len().div_ceil(2);
    let mut rows = rows.into_iter();
    let left: Vec<Line> = rows.by_ref().take(half).collect();
    let right: Vec<Line> = rows.collect();
    let mut right = right.into_iter();
    left.into_iter()
        .map(|mut line| {
            if let Some(r) = right.next() {
                let pad = column - line.width() + GAP;
                line.spans.push(Span::raw(" ".repeat(pad)));
                line.spans.extend(r.spans);
            }
            line
        })
        .collect()
}

/// Emacs's movement keys, as the keys they stand for, so they work
/// wherever those do: C-n C-p for ↓ ↑, C-v M-v for a page, M-< M-> for
/// the ends, and C-g for Esc.
fn emacs(key: KeyEvent) -> KeyEvent {
    let ctrl = key.modifiers == KeyModifiers::CONTROL;
    let alt = key.modifiers.difference(KeyModifiers::SHIFT) == KeyModifiers::ALT;
    let code = match key.code {
        KeyCode::Char('n') if ctrl => KeyCode::Down,
        KeyCode::Char('p') if ctrl => KeyCode::Up,
        KeyCode::Char('v') if ctrl => KeyCode::PageDown,
        KeyCode::Char('v') if alt => KeyCode::PageUp,
        KeyCode::Char('<') if alt => KeyCode::Home,
        KeyCode::Char('>') if alt => KeyCode::End,
        KeyCode::Char('g') if ctrl => KeyCode::Esc,
        _ => return key,
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn section_trail_drops_outer_sections_to_fit() {
        let s: Vec<String> = ["Guide", "Install", "On Windows"].map(String::from).into();
        assert_eq!(
            section_trail(&s, 80).unwrap(),
            "Guide › Install › On Windows"
        );
        assert_eq!(section_trail(&s, 25).unwrap(), "… › Install › On Windows");
        assert_eq!(section_trail(&s, 15).unwrap(), "… › On Windows");
        assert_eq!(section_trail(&s, 10).unwrap(), "… › On Wi…");
        assert_eq!(section_trail(&s, 5), None);
        assert_eq!(section_trail(&[], 80), None);
    }

    #[test]
    fn emacs_keys_stand_for_the_usual_ones() {
        let k = |c, m| emacs(KeyEvent::new(KeyCode::Char(c), m)).code;
        assert_eq!(k('n', KeyModifiers::CONTROL), KeyCode::Down);
        assert_eq!(k('p', KeyModifiers::CONTROL), KeyCode::Up);
        assert_eq!(k('v', KeyModifiers::CONTROL), KeyCode::PageDown);
        assert_eq!(k('v', KeyModifiers::ALT), KeyCode::PageUp);
        assert_eq!(k('>', KeyModifiers::ALT), KeyCode::End);
        assert_eq!(k('g', KeyModifiers::CONTROL), KeyCode::Esc);
        assert_eq!(k('n', KeyModifiers::NONE), KeyCode::Char('n'));
        assert_eq!(k('<', KeyModifiers::SHIFT), KeyCode::Char('<'));
    }

    #[test]
    fn help_goes_to_two_columns_only_when_too_short() {
        let rows =
            || -> Vec<Line<'static>> { (0..10).map(|i| Line::from(format!("row {i}"))).collect() };
        let area = |width, height| Rect::new(0, 0, width, height);
        assert_eq!(help_columns(rows(), area(80, 14)).len(), 10);
        let two = help_columns(rows(), area(80, 13));
        assert_eq!(two.len(), 5);
        assert_eq!(text(&two[0]), "row 0    row 5");
        assert_eq!(help_columns(rows(), area(17, 13)).len(), 10);
    }

    #[test]
    fn titles_wrap_at_spaces_instead_of_being_cut() {
        let story = Story {
            title: "A rather long title for a story".into(),
            url: Some("https://www.example.com/x".into()),
            ..Story::default()
        };
        let texts =
            |width| -> Vec<String> { title_lines(Some(&story), &[], width).iter().map(text).collect() };
        assert_eq!(texts(40), [" • A rather long title for a story"]);
        assert_eq!(texts(22), [" • A rather long title", "   for a story"]);
        // A word too long for a row breaks mid-word.
        assert_eq!(texts(6)[..3], [" • A", "   rat", "   her"]);
        assert_eq!(texts(22).concat(), " • A rather long title   for a story");
    }

    /// Tab goes back and forth between the list and the story: on a wide
    /// terminal with both on screen, otherwise to the story alone.
    #[test]
    fn tab_switches_views() {
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        for (width, both) in [(160, true), (100, false)] {
            let mut app = App::new(Theme::plain(), None, Fetcher::start());
            app.body_width = width;
            app.ids = Some(vec![1, 2]);
            app.refresh();
            assert!(!app.key(tab));
            assert!(app.focus == Focus::Reader && app.reading == Some(1));
            assert_eq!(app.list_in_reader, both, "{width}");
            // Back in the list, the story being read stays whole.
            assert!(!app.key(tab));
            assert!(app.focus == Focus::List);
            assert_eq!(app.current_key(), Some((1, false)));
            app.select_by(1);
            assert_eq!(app.current_key(), Some((2, true)));
        }
    }

    /// `>` and `<` go to the next and previous story, opening it when
    /// reading, and stop at the ends.
    #[test]
    fn next_and_previous_story() {
        let mut app = App::new(Theme::plain(), None, Fetcher::start());
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.ids = Some(vec![1, 2, 3]);
        app.refresh();
        app.key(key('>'));
        assert_eq!(app.selected_id(), Some(2));
        assert!(app.focus == Focus::List);
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.key(key('>'));
        assert!(app.focus == Focus::Reader && app.reading == Some(3));
        app.key(key('>'));
        assert_eq!(app.reading, Some(3));
        assert_eq!(app.flash.as_deref(), Some("Last story"));
        app.key(key('<'));
        assert_eq!(app.reading, Some(2));
        // Shift and the arrows do the same while reading.
        app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.reading, Some(1));
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.reading, Some(2));
    }

    /// Esc and ← go from the story back to the list; only Esc in the list
    /// quits, and ← there does nothing.
    #[test]
    fn left_goes_back_but_never_quits() {
        let mut app = App::new(Theme::plain(), None, Fetcher::start());
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.focus = Focus::Reader;
        assert!(!app.key(key(KeyCode::Left)));
        assert!(app.focus == Focus::List);
        assert!(!app.key(key(KeyCode::Left)));
        assert!(!app.key(key(KeyCode::Left)));
        app.focus = Focus::Reader;
        assert!(!app.key(key(KeyCode::Esc)));
        assert!(app.focus == Focus::List);
        assert!(app.key(key(KeyCode::Esc)));
    }
}
