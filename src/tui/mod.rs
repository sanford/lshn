//! The interactive reader: stories on the left, the selected one on the
//! right (title, article, then comments), and the story full screen.

mod act;
mod mouse;
mod nav;
mod outline;
mod picker;
mod themes;

use crate::article::Article;
use crate::doc::Doc;
use crate::fetch::{self, Done, Fetcher, Got, Job, Waiting};
use crate::hn::{Comment, Feed, Story, User};
use crate::omarchy::Follow;
use crate::store::{self, Cache, Seen, SeenStore};
use crate::story::{self, Comments};
use crate::theme::{Choice, Mode, Theme};
use crate::{clipboard, safe, wrap};
use image::DynamicImage;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use ratatui_image::picker::Picker;
use std::collections::{HashMap, HashSet};
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// How long the picture waits for scrolling to stop before it's drawn.
const FIGURE_SETTLE: Duration = Duration::from_millis(150);

/// How stories are shown.
pub struct Settings {
    /// The widest to wrap text.
    pub max_width: Option<usize>,
    /// Scroll and click with the mouse. (Selecting text then needs a
    /// modifier key in most terminals.)
    pub mouse: bool,
    /// Open stories with the outline pane beside them.
    pub outline: bool,
    /// Show articles' first pictures.
    pub images: bool,
    /// The theme asked for, by flag or config.
    pub choice: Choice,
    /// Colors come from Omarchy's theme, and follow it.
    pub omarchy: bool,
    /// The list to start with.
    pub feed: Feed,
    /// Sites and words whose stories aren't listed.
    pub mute: Vec<String>,
}

pub fn run(theme: Theme, settings: Settings) -> io::Result<()> {
    // Previewing `auto` in the theme picker needs the terminal's background,
    // and it can't be asked once the screen is ours.
    let terminal_dark = match settings.choice {
        Choice::Mode(Mode::Auto) if !settings.omarchy => theme.dark,
        _ if theme.color && !settings.omarchy => crate::theme::detect_dark(),
        _ => true,
    };
    let dir = store::dir();
    let cache = Cache::new(dir.as_deref());
    cache.prune();
    let seen = SeenStore::load(dir.as_deref(), now());
    let mut app = App::new(theme, settings.max_width, Fetcher::start(cache), seen);
    app.choice = settings.choice;
    app.terminal_dark = terminal_dark;
    if settings.omarchy {
        app.follow = Follow::start();
    }
    app.omarchy = settings.omarchy;
    app.mouse_on = settings.mouse;
    app.outline_pane = settings.outline;
    app.mute = settings.mute;
    app.open_feed(settings.feed);
    let mut terminal = ratatui::init();
    if settings.images {
        app.picker = picker();
        if let Some(picker) = &app.picker {
            let cell = picker.font_size();
            crate::figure::set_cell(cell.width, cell.height);
        }
    }
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

/// How the terminal can draw pictures, and its cell size in pixels. Asked
/// after the screen's ours and before reading keys, and briefly: a terminal
/// that doesn't answer holds up the start, and whatever's typed meanwhile is
/// lost. tmux passes on neither the question nor pictures, unless set up
/// to, so there it's half blocks without asking.
fn picker() -> Option<Picker> {
    use ratatui_image::picker::cap_parser::QueryStdioOptions;
    if std::env::var_os("TMUX").is_some() {
        return Some(Picker::halfblocks());
    }
    let mut picker = Picker::from_query_stdio_with_options(QueryStdioOptions {
        timeout: Duration::from_millis(250),
        ..QueryStdioOptions::default()
    })
    .ok()?;
    // iTerm2 says it can do Sixel too, and that's what gets picked, but
    // its own pictures are what it draws best.
    let iterm = |var| std::env::var(var).is_ok_and(|v| v.contains("iTerm"));
    if iterm("TERM_PROGRAM") || iterm("LC_TERMINAL") {
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Iterm2);
    }
    Some(picker)
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
    Figure(u64),
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
    waiting: Waiting,
    feed: Feed,
    /// A search of HN, whose results are the list instead of the feed's.
    search: Option<String>,
    /// The list's stories, in order, once it's loaded.
    ids: Option<Vec<u64>>,
    feed_error: Option<String>,
    stories: HashMap<u64, Story>,
    /// Stories that couldn't be loaded, or are dead or deleted.
    gone: HashSet<u64>,
    /// Sites and words whose stories aren't listed.
    mute: Vec<String>,
    threads: HashMap<u64, Result<Vec<Comment>, String>>,
    articles: HashMap<u64, Article>,
    /// Articles' first pictures, by story.
    figures: HashMap<u64, DynamicImage>,
    /// How the terminal draws pictures, once asked; `None` if it isn't to.
    picker: Option<Picker>,
    /// The picture last drawn, ready at its size: story, columns, rows.
    drawn: Option<((u64, usize, usize), crate::figure::Drawn)>,
    /// Where the picture was last put (story, row), and when it moved.
    figure_at: Option<(u64, i32)>,
    figure_moved: Option<Instant>,
    /// Moving fast: the picture waits till it stops.
    figure_hidden: bool,
    docs: HashMap<DocKey, Doc>,
    users: HashMap<String, Result<User, String>>,
    user_docs: HashMap<String, Doc>,
    /// Someone's page, shown in the reader in place of the story.
    user_page: Option<String>,
    /// Where following links came from, to go back to.
    history: Vec<nav::Back>,
    /// A story an HN link was followed to, to open once it's loaded.
    opening: Option<u64>,
    /// Whether you're logged in to HN.
    auth: act::Auth,
    /// What's waiting for you to log in.
    pending: Option<act::Action>,
    /// A reply form asked for: what's being replied to, and the story.
    replying: Option<(u64, Option<u64>)>,
    /// A reply to write in the editor, once the key's handled.
    editing: Option<act::Draft>,
    /// The draft of a reply being posted, to delete once it is.
    posting: Option<std::path::PathBuf>,
    /// The stories you've read, and how much of each thread you'd seen.
    seen: SeenStore,
    /// For the story being read: the newest comment seen before it was
    /// opened, so what's new stays marked while it's read, though `seen`
    /// has moved on.
    marks: HashMap<u64, Option<u64>>,

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
    /// The story selected when the reader was left for the list.
    left_on: Option<u64>,
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
    fn new(theme: Theme, max_width: Option<usize>, fetcher: Fetcher, seen: SeenStore) -> App {
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
            waiting: Waiting::default(),
            feed: Feed::Top,
            search: None,
            ids: None,
            feed_error: None,
            stories: HashMap::new(),
            gone: HashSet::new(),
            mute: Vec::new(),
            threads: HashMap::new(),
            articles: HashMap::new(),
            figures: HashMap::new(),
            picker: None,
            drawn: None,
            figure_at: None,
            figure_moved: None,
            figure_hidden: false,
            docs: HashMap::new(),
            users: HashMap::new(),
            user_docs: HashMap::new(),
            user_page: None,
            history: Vec::new(),
            opening: None,
            auth: act::Auth::Unknown,
            pending: None,
            replying: None,
            editing: None,
            posting: None,
            seen,
            marks: HashMap::new(),
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
            left_on: None,
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
            if let Some(draft) = self.editing.take() {
                self.edit_reply(terminal, draft)?;
            }
            self.restyle();
            self.preview_theme();
            self.prefetch();
            // All at once: without it, a terminal can show a frame half
            // drawn, which shows most while pictures scroll.
            use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
            ratatui::crossterm::queue!(io::stdout(), BeginSynchronizedUpdate)?;
            terminal.draw(|f| self.draw(f))?;
            ratatui::crossterm::execute!(io::stdout(), EndSynchronizedUpdate)?;
            // While fetching, wake up often to show what's come; otherwise
            // now and then, to keep the ages current.
            let wait = if self.waiting.any() || self.figure_hidden {
                Duration::from_millis(30)
            } else {
                Duration::from_secs(1)
            };
            if !event::poll(wait)? {
                continue;
            }
            // Everything that's waiting, then one frame: a key held down
            // doesn't queue up frames to catch up on.
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press && self.key(key) => {
                        return Ok(());
                    }
                    Event::Mouse(m) => {
                        self.flash = None;
                        self.mouse(m);
                    }
                    _ => {} // Resizes redraw at the top of the loop.
                }
                if self.editing.is_some() || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }

    /// Hands the fetcher a job, counting it until it's done.
    fn send(&mut self, job: Job, urgent: bool) {
        self.waiting.start(job.kind());
        self.fetcher.push(job, urgent);
    }

    /// Asks the fetcher for something, unless it's been asked already.
    fn ask(&mut self, what: Asked, job: Job, urgent: bool) {
        if self.asked.insert(what) {
            self.send(job, urgent);
        } else if urgent {
            // Maybe still waiting behind prefetches: move it up.
            self.fetcher.hurry(&job);
        }
    }

    fn open_feed(&mut self, feed: Feed) {
        self.feed = feed;
        self.search = None;
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

    /// Lists the stories matching `query`, best first.
    fn search_hn(&mut self, query: String) {
        self.search = Some(query.clone());
        self.ids = None;
        self.feed_error = None;
        self.filter.clear();
        self.typing = false;
        self.focus = Focus::List;
        self.send(Job::Search(query), true);
        self.refresh();
        self.list.select(None);
        *self.list.offset_mut() = 0;
    }

    /// What the list is: a feed's name, or "Search".
    fn list_name(&self) -> &'static str {
        match self.search {
            Some(_) => "Search",
            None => self.feed.name(),
        }
    }

    /// Fetches everything again, keeping what's shown until it's replaced.
    /// Articles don't change, so they stay.
    fn reload(&mut self) {
        self.asked.retain(|a| matches!(a, Asked::Article(_)));
        self.gone.clear();
        match self.search.clone() {
            Some(query) => self.send(Job::Search(query), true),
            None => {
                let feed = self.feed;
                self.ask(Asked::Feed(feed), Job::Feed(feed), true);
            }
        }
        self.flash = Some(format!("Reloading {}…", self.list_name()));
    }

    /// Takes in whatever the fetcher has sent back. A cached copy shows
    /// until the fetched one replaces it; if fetching fails, the cached
    /// copy stays.
    fn receive(&mut self) {
        let mut list_changed = false;
        while let Ok(Done { got, last }) = self.fetcher.done.try_recv() {
            if last {
                self.waiting.finish(got.kind());
            }
            match got {
                Got::Feed(feed, result) if feed == self.feed && self.search.is_none() => {
                    match result {
                        Ok(ids) => {
                            self.ids = Some(ids);
                            self.feed_error = None;
                        }
                        Err(e) if self.ids.is_some() => {
                            self.flash = Some(format!("Showing saved stories: {e}"));
                        }
                        Err(e) => self.feed_error = Some(e),
                    }
                    list_changed = true;
                }
                Got::Feed(..) => {}
                Got::Search(query, result) if self.search.as_ref() == Some(&query) => {
                    match result {
                        Ok(ids) => self.ids = Some(ids),
                        Err(e) => self.feed_error = Some(e),
                    }
                    list_changed = true;
                }
                Got::Search(..) => {}
                // A link to an HN item that isn't a story: a comment, say.
                Got::Story(id, Ok(story)) if story.title.is_empty() => {
                    if self.opening == Some(id) {
                        self.opening = None;
                        self.open_outside(&crate::hn::item_url(id));
                    }
                }
                Got::Story(id, Ok(story)) if !story.dead && !story.deleted => {
                    self.stories.insert(id, story);
                    if self.opening == Some(id) {
                        self.opening = None;
                        self.open_story_page(id);
                    }
                    self.rebuild(id);
                    if self.reading == Some(id) {
                        self.note_seen(id);
                    }
                    list_changed = true;
                }
                Got::Story(id, Err(e)) if self.opening == Some(id) && last => {
                    self.opening = None;
                    self.flash = Some(format!("Couldn't open it: {e}"));
                }
                Got::Story(id, Err(_)) if self.stories.contains_key(&id) => {}
                Got::Story(id, _) => {
                    self.gone.insert(id);
                    list_changed = true;
                }
                Got::Thread(id, Err(_)) if matches!(self.threads.get(&id), Some(Ok(_))) => {}
                Got::Thread(id, result) => {
                    self.threads.insert(id, result);
                    self.rebuild(id);
                    if self.reading == Some(id) {
                        self.note_seen(id);
                    }
                }
                Got::User(name, result) => {
                    let failed = result.is_err();
                    if !(failed && matches!(self.users.get(&name), Some(Ok(_)))) {
                        self.users.insert(name.clone(), result);
                    }
                    let md = story::user_markdown(&name, self.users.get(&name), now());
                    if let Some(doc) = self.user_docs.get_mut(&name) {
                        doc.replace(md);
                    }
                }
                Got::LoggedIn(result) => self.logged_in(result),
                Got::Upvoted(result) => self.upvoted(result),
                Got::ReplyForm(id, result) => self.got_reply_form(id, result),
                Got::Posted(story, result) => self.posted(story, result),
                Got::Article(id, article) => {
                    if let (Some(_), Article::Text { md, .. }) = (&self.picker, &article)
                        && let Some((url, _)) = crate::figure::first(md)
                    {
                        let urgent = self.current_key().is_some_and(|(on, _)| on == id);
                        self.ask(Asked::Figure(id), Job::Figure(id, url), urgent);
                    }
                    self.articles.insert(id, article);
                    self.rebuild(id);
                }
                Got::Figure(id, Ok(picture)) => {
                    self.figures.insert(id, picture);
                    self.rebuild(id);
                }
                Got::Figure(_, Err(_)) => {}
            }
        }
        if list_changed {
            self.refresh();
        }
    }

    /// Remembers that story `id` has been read, as far as its comments go
    /// now.
    fn note_seen(&mut self, id: u64) {
        let Some(story) = self.stories.get(&id) else {
            return;
        };
        let before = self.seen.stories.get(&id).copied().unwrap_or_default();
        let newest = match self.threads.get(&id) {
            Some(Ok(comments)) => comments.iter().map(|c| c.newest()).max().unwrap_or(0),
            _ => 0,
        };
        let seen = Seen {
            at: now(),
            newest: newest.max(before.newest),
            count: story.descendants,
        };
        if seen != before {
            self.seen.stories.insert(id, seen);
            self.seen.save();
        }
    }

    /// Opens story `id` in the reader, and leaves the one that was there:
    /// its comments stop being marked new.
    fn start_reading(&mut self, id: u64) {
        if let Some(before) = self.reading.filter(|&r| r != id) {
            self.marks.remove(&before);
            self.rebuild(before);
        }
        let seen = self.seen.stories.get(&id).map(|s| s.newest);
        self.marks.entry(id).or_insert(seen);
        self.reading = Some(id);
        self.note_seen(id);
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
        let seen = match self.marks.get(&id) {
            Some(&marked) => marked,
            None => self.seen.stories.get(&id).map(|s| s.newest),
        };
        let picture = self.figures.get(&id);
        story::markdown(story, self.articles.get(&id), comments, preview, now(), seen, picture)
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
            _ if self.kept() => self.reading.map(|id| (id, false)),
            // The story being read stays whole, and where it was, while
            // it's selected; the others show their previews.
            Focus::List => self
                .selected_id()
                .map(|id| (id, Some(id) != self.reading)),
        }
    }

    fn current(&mut self) -> Option<&mut Doc> {
        if (self.focus == Focus::Reader || self.kept())
            && let Some(name) = self.user_page.clone()
        {
            return Some(self.user_doc(&name));
        }
        let key = self.current_key()?;
        Some(self.doc(key))
    }

    /// Re-filters the list, keeping the same story selected.
    fn refresh(&mut self) {
        let keep = self.selected_id();
        let ids = self.ids.as_deref().unwrap_or_default();
        let listed: Vec<u64> = ids.iter().copied().filter(|&id| !self.hidden(id)).collect();
        if self.filter.is_empty() {
            self.shown = listed
                .into_iter()
                .map(|id| Shown {
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
            for id in listed {
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
        if self.kept() || self.reading == Some(id) {
            // Back to what was being read, links followed and all.
            self.focus = Focus::Reader;
            return;
        }
        // Another story: a fresh start, with nothing to go back to.
        self.history.clear();
        self.user_page = None;
        let preview = self.doc((id, true));
        let in_comments = comments_line(preview).is_some_and(|line| preview.top() >= line);
        let heading = preview
            .current_heading()
            .map(|i| preview.headings()[i].slug.clone());
        let place = preview.place().filter(|_| preview.top() > 0);
        self.start_reading(id);
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
            Focus::Reader => self.leave_reader(),
        }
    }

    /// From the reader to the list. What was being read stays beside it
    /// until another story's selected.
    fn leave_reader(&mut self) {
        self.focus = Focus::List;
        self.left_on = self.selected_id();
    }

    /// In the list, with the reader's page still beside it: nothing else
    /// has been selected since leaving it.
    fn kept(&self) -> bool {
        self.focus == Focus::List && self.left_on.is_some() && self.left_on == self.selected_id()
    }

    /// `c`: to the comments, and back to where that came from.
    fn toggle_comments(&mut self) {
        if self.focus == Focus::Reader && self.user_page.is_some() {
            self.flash = Some("No comments on someone's page".into());
            return;
        }
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
        let reader_shown = self.focus == Focus::Reader || self.kept();
        if let (true, Some(name)) = (reader_shown, &self.user_page) {
            let url = crate::hn::user_url(name);
            return self.open_outside_now(&url);
        }
        let id = match self.current_key() {
            Some((id, _)) => Some(id),
            None => self.selected_id(),
        };
        let Some(story) = id.and_then(|id| self.stories.get(&id)) else {
            return;
        };
        let url = match &story.url {
            Some(url) if !hn_page => url.clone(),
            _ => story.hn_url(),
        };
        self.open_outside_now(&url);
    }

    /// Opens a web page in the browser, without asking: for `w`, where
    /// it's the story's own link.
    fn open_outside_now(&mut self, url: &str) {
        match crate::open::web(url) {
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
        let reader_shown = self.focus == Focus::Reader || self.kept();
        if let (true, Some(name)) = (reader_shown, &self.user_page) {
            let url = crate::hn::user_url(name);
            self.flash = Some(match clipboard::copy(&url) {
                Ok(how) => format!("Copied {url} {how}"),
                Err(e) => format!("Couldn't copy: {e}"),
            });
            return;
        }
        let id = match self.current_key() {
            Some((id, _)) => Some(id),
            None => self.selected_id(),
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
            KeyCode::Char('s') if !ctrl => {
                self.prompt = Some(nav::Prompt::SearchHn {
                    query: self.search.clone().unwrap_or_default(),
                });
            }
            KeyCode::Char('>') => self.next_story(1),
            KeyCode::Char('<') => self.next_story(-1),
            KeyCode::Char('c') if !ctrl => self.toggle_comments(),
            KeyCode::Char('w') if !ctrl => self.open_in_browser(false),
            KeyCode::Char('W') => self.open_in_browser(true),
            KeyCode::Char('y') if !ctrl => self.copy_link(false),
            KeyCode::Char('Y') => self.copy_link(true),
            KeyCode::Char('R') => self.reload(),
            KeyCode::Char('r') if !ctrl => self.reply_key(),
            KeyCode::Char('v') if !ctrl => self.upvote_key(),
            KeyCode::Char('L') => self.login_key(),
            KeyCode::Char('O') if self.focus == Focus::Reader => self.focus_outline(false),
            KeyCode::Char('O') => self.outline_pane = !self.outline_pane,
            KeyCode::Char('t') if !ctrl => self.open_themes(),
            KeyCode::Char(c @ '1'..='6') => {
                let feed = Feed::ALL[c as usize - '1' as usize];
                self.focus = Focus::List;
                if feed != self.feed || self.search.is_some() {
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
            // Back where a link was followed from, then to the list. Esc
            // never quits from here: that's for the list, so a Left too
            // many never loses your place.
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                if !self.go_back() {
                    self.leave_reader();
                }
                return false;
            }
            KeyCode::Char('\\') => {
                self.list_in_reader = !self.list_in_reader;
                return false;
            }
            // What ↑ and ↓ do in the list: the next and previous story.
            // ⌃J ⌃K keep your hands on the home row.
            KeyCode::Down if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) => {
                self.next_story(1);
                return false;
            }
            KeyCode::Up if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) => {
                self.next_story(-1);
                return false;
            }
            KeyCode::Char('j') if ctrl => {
                self.next_story(1);
                return false;
            }
            KeyCode::Char('k') if ctrl => {
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
        self.draw_post(f);
        if self.help {
            draw_help(f);
        }
        self.theme.paint(f.buffer_mut());
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect) {
        let mut title = vec![" lshn ".bold(), Span::raw(" ")];
        for (i, feed) in Feed::ALL.iter().enumerate() {
            let name = format!("{} {}", i + 1, feed.name());
            title.push(if *feed == self.feed && self.search.is_none() {
                Span::raw(name).bold().underlined()
            } else {
                Span::raw(name).dim()
            });
            title.push(Span::raw("  "));
        }
        title.push(match &self.search {
            Some(query) => Span::raw(format!("s “{}”", safe::printable(query))).bold().underlined(),
            None => Span::raw("s Search").dim(),
        });
        title.push(Span::raw("  "));
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
        // Full screen, what's still coming for the story goes at the right.
        let reader_only = self.focus == Focus::Reader && !self.list_in_reader;
        let coming = self.coming().filter(|_| reader_only).unwrap_or_default();
        let coming_w = wrap::width(&coming);
        let room = room.saturating_sub(coming_w + usize::from(coming_w > 0) * 2);
        if let Some(trail) = section_trail(section, room) {
            title.push(Span::raw("§ ").dim());
            title.push(Span::raw(trail));
        }
        let [title_area, coming_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(coming_w as u16 + 1)])
                .areas(area);
        f.render_widget(Paragraph::new(Line::from(title)), title_area);
        f.render_widget(Paragraph::new(Line::from(coming).dim()), coming_area);
    }

    /// What's still coming for the story (or page) on screen: "⠹ comments
    /// · article". `None` once it's all there.
    fn coming(&self) -> Option<String> {
        let reader = self.focus == Focus::Reader || self.kept();
        let mut parts = Vec::new();
        if let (true, Some(name)) = (reader, &self.user_page) {
            if !self.users.contains_key(name) {
                parts.push("their page");
            }
        } else {
            let (id, _) = self.current_key()?;
            match self.stories.get(&id) {
                None => parts.push("story"),
                Some(story) => {
                    if !self.threads.contains_key(&id) {
                        parts.push("comments");
                    }
                    if story.url.is_some() && !self.articles.contains_key(&id) {
                        parts.push("article");
                    }
                }
            }
        }
        (!parts.is_empty()).then(|| format!("{} {}", fetch::spinner(), parts.join(" · ")))
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
            None => format!(" {} ", self.list_name()),
            Some(total) if self.filter.is_empty() => format!(" {} ({total}) ", self.list_name()),
            Some(total) => format!(" {} ({} of {total}) ", self.list_name(), self.shown.len()),
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
            .map(|s| {
                let story = self.stories.get(&s.id);
                let seen = self.seen.stories.get(&s.id);
                title_lines(story, &s.hits, seen, width)
            })
            .collect();
        self.list_heights = items.iter().map(Vec::len).collect();
        let items: Vec<ListItem> = items.into_iter().map(ListItem::new).collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(self.list_highlight());
        f.render_stateful_widget(list, area, &mut self.list);

        let msg = match (&self.ids, &self.feed_error) {
            (_, Some(e)) => Some(format!("Couldn't load {}: {e}", self.list_name())),
            (None, None) => Some("Loading…".into()),
            (Some(_), None) if self.shown.is_empty() && !self.filter.is_empty() => {
                Some("Nothing matches".into())
            }
            (Some(_), None) if self.shown.is_empty() && self.search.is_some() => {
                Some("No stories match".into())
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

    /// Stories in this list that won't be shown.
    fn gone_in_feed(&self) -> usize {
        let ids = self.ids.as_deref().unwrap_or_default();
        ids.iter().filter(|&&id| self.hidden(id)).count()
    }

    /// Whether a story is left out of the list: it's gone, or muted.
    fn hidden(&self, id: u64) -> bool {
        self.gone.contains(&id)
            || self
                .stories
                .get(&id)
                .is_some_and(|story| muted(story, &self.mute))
    }

    /// The story's picture, over the room its document left for it.
    fn draw_figure(&mut self, f: &mut Frame) {
        let user_page = (self.focus == Focus::Reader || self.kept()) && self.user_page.is_some();
        let on_screen = self
            .current_key()
            .filter(|_| !user_page && self.picker.is_some())
            .filter(|(id, _)| self.figures.contains_key(id))
            .and_then(|(id, _)| Some((id, self.current()?.figure()?)));
        let Some((id, (area, figure, y))) = on_screen else {
            // Nothing to wait for.
            self.figure_hidden = false;
            return;
        };
        let (Some(picker), Some(picture)) = (&self.picker, self.figures.get(&id)) else {
            return;
        };
        // A picture is sent again whenever it moves, which is a lot for a
        // terminal to keep up with at a key's repeat rate: moving again
        // soon after the last move, it's hidden till things settle.
        let now = Instant::now();
        let settled = self.figure_moved.is_none_or(|t| now - t >= FIGURE_SETTLE);
        if self.figure_at != Some((id, y)) {
            self.figure_hidden = !settled;
            self.figure_at = Some((id, y));
            self.figure_moved = Some(now);
        } else if settled {
            self.figure_hidden = false;
        }
        if self.figure_hidden {
            return;
        }
        let key = (id, figure.cols, figure.rows);
        if self.drawn.as_ref().is_none_or(|(k, _)| *k != key) {
            self.drawn = crate::figure::Drawn::new(picker, picture, figure.cols, figure.rows)
                .map(|d| (key, d));
        }
        if let Some((_, drawn)) = &self.drawn {
            // Centred over the text.
            let x = figure.width.saturating_sub(figure.cols) / 2;
            drawn.draw(f.buffer_mut(), area, x as u16, y);
        }
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let mut block = self
            .pane("", self.focus == Focus::Reader)
            .padding(Padding::horizontal(1));
        if let Some(coming) = self.coming() {
            block = block.title_top(Line::from(format!(" {coming} ")).dim().right_aligned());
        }
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
        self.draw_figure(f);
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
                        ("s", "search"),
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
                    ("^j ^k", "next story"),
                    ("/", "search"),
                    ("f", "follow"),
                    ("w", "open"),
                    // Tab always goes to the list; ← and Esc go back first.
                    match (searching, self.history.is_empty()) {
                        (true, _) => ("esc", "clear search"),
                        (false, true) => ("← tab", "list"),
                        (false, false) => ("←", "back"),
                    },
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

/// Whether `story` is from a muted site ("example.com", or anything under
/// it), or has a muted word in its title. Words match whole, ignoring case:
/// "ai" doesn't mute "Hawaii".
fn muted(story: &Story, mute: &[String]) -> bool {
    let domain = story.domain().unwrap_or_default();
    let title = story.title.to_lowercase();
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    mute.iter().any(|m| {
        let m = m.trim().to_lowercase();
        if m.is_empty() {
            return false;
        }
        let site = !m.contains(' ') && m.contains('.');
        if site {
            return domain == m || domain.ends_with(&format!(".{m}"));
        }
        // A phrase: its words, in order, somewhere in the title.
        let phrase: Vec<&str> = m.split_whitespace().collect();
        words.windows(phrase.len()).any(|w| w == phrase.as_slice())
    })
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
fn title_lines(
    story: Option<&Story>,
    hits: &[u32],
    seen: Option<&Seen>,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(story) = story else {
        return vec![Line::from(format!(" {} ", crate::fetch::spinner())).dim()];
    };
    const INDENT: &str = "   ";
    let hit = Style::new().yellow().bold();
    // Stories you've read fade, and say how many comments they've had
    // since.
    let plain = if seen.is_some() {
        Style::new().dim()
    } else {
        Style::new()
    };
    let title = safe::printable(&story.title);
    let mut chars: Vec<(char, Style)> = title
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let style = if hits.binary_search(&(i as u32)).is_ok() {
                hit
            } else {
                plain
            };
            (c, style)
        })
        .collect();
    let new = seen.map_or(0, |s| story.descendants.saturating_sub(s.count));
    if new > 0 {
        chars.push((' ', plain));
        chars.extend(format!("+{new}").chars().map(|c| (c, Style::new().yellow())));
    }

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
        ("^j ^k ^↓ ^↑", "Next / previous story, reading (⇧↓ ⇧↑ > < too)"),
        ("esc ← h", "Back to the list (esc quits there)"),
        ("q", "Quit"),
        ("c", "To the comments, and back"),
        ("] [", "Next / previous comment (or heading)"),
        ("o", "Outline: the text follows as you move (/ filters)"),
        ("O", "Keep the outline open beside the story"),
        ("w W", "Open the story's link / its HN page in the browser"),
        ("y Y", "Copy the story's link / its HN page"),
        ("1-6", "Top, New, Best, Ask, Show, Jobs"),
        ("s", "Search all of HN's stories"),
        ("v", "Upvote the story, or choose a comment on screen"),
        ("r", "Reply to the story, or choose a comment on screen"),
        ("L", "Log in to HN, or out"),
        ("R", "Reload"),
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
            |width| -> Vec<String> { title_lines(Some(&story), &[], None, width).iter().map(text).collect() };
        assert_eq!(texts(40), [" • A rather long title for a story"]);
        assert_eq!(texts(22), [" • A rather long title", "   for a story"]);
        // A word too long for a row breaks mid-word.
        assert_eq!(texts(6)[..3], [" • A", "   rat", "   her"]);
        assert_eq!(texts(22).concat(), " • A rather long title   for a story");
    }

    #[test]
    fn mutes_sites_and_words() {
        let story = |title: &str, url: &str| Story {
            title: title.into(),
            url: Some(url.into()),
            ..Story::default()
        };
        let mute = ["medium.com".to_string(), "AI".into(), "web3 wallet".into()];
        assert!(muted(&story("x", "https://medium.com/a"), &mute));
        assert!(muted(&story("x", "https://blog.medium.com/a"), &mute));
        assert!(!muted(&story("x", "https://notmedium.com/a"), &mute));
        assert!(muted(&story("The AI boom", "https://a.b"), &mute));
        assert!(muted(&story("Why AI's hype", "https://a.b"), &mute));
        assert!(!muted(&story("Hawaii", "https://a.b"), &mute));
        assert!(muted(&story("A Web3 wallet for cats", "https://a.b"), &mute));
        assert!(!muted(&story("A wallet for web3", "https://a.b"), &mute));
    }

    #[test]
    fn read_stories_say_how_many_comments_theyve_had_since() {
        let story = Story {
            title: "Title".into(),
            descendants: 12,
            ..Story::default()
        };
        let seen = Seen {
            at: 1,
            newest: 5,
            count: 9,
        };
        let lines = title_lines(Some(&story), &[], Some(&seen), 40);
        assert_eq!(text(&lines[0]), " • Title +3");
        let title = lines[0].spans.iter().find(|s| s.content.contains("Title")).unwrap();
        assert!(title.style.add_modifier.contains(Modifier::DIM));
        let caught_up = Seen { count: 12, ..seen };
        assert_eq!(text(&title_lines(Some(&story), &[], Some(&caught_up), 40)[0]), " • Title");
    }

    /// Reading a story remembers it, and its comments stay marked new
    /// until another story is read.
    #[test]
    fn reading_remembers_and_marks_new_comments_until_you_move_on() {
        let mut app = App::new(Theme::plain(), None, Fetcher::start(Cache::none()), SeenStore::load(None, 0));
        let comment = |id| crate::hn::Comment {
            id,
            by: "x".into(),
            text: "hi".into(),
            ..crate::hn::Comment::default()
        };
        for id in [1, 2] {
            app.stories.insert(id, Story { id, title: "t".into(), descendants: 2, ..Story::default() });
        }
        app.seen.stories.insert(1, Seen { at: 1, newest: 100, count: 1 });
        app.threads.insert(1, Ok(vec![comment(100), comment(200)]));
        app.ids = Some(vec![1, 2]);
        app.refresh();

        app.read_selected();
        assert!(app.markdown(1, false).contains("(2, 1 new)"));
        // Seen now goes up to the newest, but it's still marked while read.
        assert_eq!(app.seen.stories[&1].newest, 200);
        assert_eq!(app.seen.stories[&1].count, 2);
        app.next_story(1);
        assert!(app.seen.stories.contains_key(&2));
        let md = app.markdown(1, false);
        assert!(!md.contains("`new`") && !md.contains("new)"), "{md}");
    }

    /// Following HN links opens stories and people's pages here, and Esc
    /// comes back through them, each where it was, then to the list.
    #[test]
    fn follows_hn_links_and_comes_back() {
        let mut app = App::new(Theme::plain(), None, Fetcher::start(Cache::none()), SeenStore::load(None, 0));
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        for id in [1, 2] {
            app.stories.insert(id, Story { id, title: format!("Story {id}"), ..Story::default() });
        }
        app.ids = Some(vec![1]);
        app.refresh();
        app.read_selected();
        app.follow("https://news.ycombinator.com/item?id=2");
        assert_eq!(app.reading, Some(2));
        app.follow("https://news.ycombinator.com/user?id=pg");
        assert_eq!(app.user_page.as_deref(), Some("pg"));
        assert!(app.current().is_some());

        // Tab to the list and back keeps the page and the way back.
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.focus == Focus::Reader);
        assert_eq!(app.user_page.as_deref(), Some("pg"));

        app.key(esc);
        assert!(app.user_page.is_none() && app.reading == Some(2));
        app.key(esc);
        assert!(app.focus == Focus::Reader && app.reading == Some(1));
        app.key(esc);
        assert!(app.focus == Focus::List);
        // A link to the story on screen goes to its comments, not a new page.
        app.focus = Focus::Reader;
        app.follow("https://news.ycombinator.com/item?id=1");
        assert!(app.history.is_empty());
    }

    /// Tab goes back and forth between the list and the story: on a wide
    /// terminal with both on screen, otherwise to the story alone.
    #[test]
    fn tab_switches_views() {
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        for (width, both) in [(160, true), (100, false)] {
            let mut app = App::new(Theme::plain(), None, Fetcher::start(Cache::none()), SeenStore::load(None, 0));
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
        let mut app = App::new(Theme::plain(), None, Fetcher::start(Cache::none()), SeenStore::load(None, 0));
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
        // And Ctrl, with the arrows or on the home row.
        app.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(app.reading, Some(1));
        app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(app.reading, Some(2));
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(app.reading, Some(3));
        app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(app.reading, Some(2));
    }

    /// Esc and ← go from the story back to the list; only Esc in the list
    /// quits, and ← there does nothing.
    #[test]
    fn left_goes_back_but_never_quits() {
        let mut app = App::new(Theme::plain(), None, Fetcher::start(Cache::none()), SeenStore::load(None, 0));
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
