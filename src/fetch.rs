//! Fetching in the background: workers take jobs from a queue, and send
//! back what they get. Urgent jobs (the story on screen) go to the front,
//! prefetching the ones around it to the back.
//!
//! Articles have workers of their own: other sites can be slow to answer,
//! and HN's quick answers mustn't wait behind them.
//!
//! What's in the cache comes back first, straight away, and what's fetched
//! follows, and goes in the cache. Articles don't change, so a cached one
//! isn't fetched again.

use crate::article::{self, Article};
use crate::auth::{self, Form, Session};
use crate::hn::{self, Comment, Feed, Replies, Story, User};
use crate::figure;
use crate::store::Cache;
use image::DynamicImage;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// Where articles are cached: numbered, so an article extracted the old way
/// is extracted again when the way changes.
const ARTICLES: &str = "article-2";

/// Workers for HN's APIs, and for articles.
const HN_WORKERS: usize = 8;
const ARTICLE_WORKERS: usize = 6;

/// Not `Debug`: some carry a password or a session.
#[derive(Clone, PartialEq, Eq)]
pub enum Job {
    Feed(Feed),
    /// Stories matching a search.
    Search(String),
    Story(u64),
    /// The story a comment is on.
    StoryOf(u64),
    /// A story's comments, with its top-level ones in HN's order.
    Thread(u64, Vec<u64>),
    Article(u64, String),
    /// A picture in a story's article: which one, and its address.
    Figure(u64, usize, String),
    /// Someone's profile and latest posts.
    User(String),
    /// The replies to someone's latest posts.
    Replies(String),
    /// Logging in: a username and password.
    Login(String, String),
    Upvote(Session, u64),
    /// The form for replying to an item, and whether it's a story.
    ReplyForm(Session, u64, bool),
    /// Posting a reply: the form, the text, and the story it's on.
    Post(Session, Form, String, u64),
}

pub enum Got {
    Feed(Feed, Result<Vec<u64>, String>),
    Search(String, Result<Vec<u64>, String>),
    Story(u64, Result<Story, String>),
    /// The story comment `.0` is on.
    StoryOf(u64, Result<u64, String>),
    Thread(u64, Result<Vec<Comment>, String>),
    Article(u64, Article),
    Figure(u64, usize, Result<DynamicImage, String>),
    User(String, Result<User, String>),
    Replies(String, Result<Replies, String>),
    LoggedIn(Result<Session, String>),
    Upvoted(Result<(), String>),
    ReplyForm(u64, Result<Form, String>),
    /// A reply posted, or not, to the story with this id.
    Posted(u64, Result<(), String>),
}

/// Something back from the fetcher.
pub struct Done {
    pub got: Got,
    /// This finishes the job. Otherwise it's from the cache, and the
    /// fetched copy is on its way.
    pub last: bool,
}

/// What a job fetches, for saying what's still coming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    List,
    Story,
    Comments,
    Article,
    /// Someone's page, or something done as you.
    Other,
}

impl Job {
    pub fn kind(&self) -> Kind {
        match self {
            Job::Feed(_) | Job::Search(_) => Kind::List,
            Job::Story(_) => Kind::Story,
            Job::Thread(..) => Kind::Comments,
            Job::Article(..) => Kind::Article,
            _ => Kind::Other,
        }
    }
}

impl Got {
    pub fn kind(&self) -> Kind {
        match self {
            Got::Feed(..) | Got::Search(..) => Kind::List,
            Got::Story(..) => Kind::Story,
            Got::Thread(..) => Kind::Comments,
            Got::Article(..) => Kind::Article,
            _ => Kind::Other,
        }
    }
}

/// How many jobs of each kind are under way.
#[derive(Default)]
pub struct Waiting([usize; 5]);

impl Waiting {
    fn slot(kind: Kind) -> usize {
        kind as usize
    }

    pub fn start(&mut self, kind: Kind) {
        self.0[Self::slot(kind)] += 1;
    }

    pub fn finish(&mut self, kind: Kind) {
        let n = &mut self.0[Self::slot(kind)];
        *n = n.saturating_sub(1);
    }

    pub fn any(&self) -> bool {
        self.0.iter().any(|&n| n > 0)
    }
}

/// A spinner's frame, turning every 80ms.
pub fn spinner() -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    FRAMES[(ms / 80 % FRAMES.len() as u128) as usize]
}

type Queue = Arc<(Mutex<VecDeque<Job>>, Condvar)>;

pub struct Fetcher {
    hn: Queue,
    articles: Queue,
    pub done: Receiver<Done>,
}

impl Fetcher {
    pub fn start(cache: Cache) -> Fetcher {
        let (tx, done) = mpsc::channel();
        let pool = |workers| {
            let queue: Queue = Arc::default();
            for _ in 0..workers {
                let queue = Arc::clone(&queue);
                let tx = tx.clone();
                let cache = cache.clone();
                std::thread::spawn(move || work(&queue, &tx, &cache));
            }
            queue
        };
        Fetcher {
            hn: pool(HN_WORKERS),
            articles: pool(ARTICLE_WORKERS),
            done,
        }
    }

    fn queue(&self, job: &Job) -> &Queue {
        match job {
            Job::Article(..) | Job::Figure(..) => &self.articles,
            _ => &self.hn,
        }
    }

    /// Queues `job`, at the front if it's `urgent`.
    pub fn push(&self, job: Job, urgent: bool) {
        let (lock, ready) = &**self.queue(&job);
        let mut queue = lock.lock().unwrap();
        if urgent {
            queue.push_front(job);
        } else {
            queue.push_back(job);
        }
        ready.notify_one();
    }

    /// Moves `job` to the front, if it's still waiting to start.
    pub fn hurry(&self, job: &Job) {
        let (lock, _) = &**self.queue(job);
        let mut queue = lock.lock().unwrap();
        if let Some(i) = queue.iter().position(|j| j == job)
            && let Some(job) = queue.remove(i)
        {
            queue.push_front(job);
        }
    }
}

fn work(queue: &Queue, tx: &Sender<Done>, cache: &Cache) {
    let (lock, ready) = &**queue;
    loop {
        let job = {
            let mut queue = lock.lock().unwrap();
            loop {
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = ready.wait(queue).unwrap();
            }
        };
        if run(job, tx, cache).is_none() {
            return;
        }
    }
}

/// Does a job, sending back the cached copy (if there is one) and then
/// the fetched one. `None` when there's no one left to send to.
fn run(job: Job, tx: &Sender<Done>, cache: &Cache) -> Option<()> {
    let send = |got, last| tx.send(Done { got, last }).ok();
    match job {
        Job::Feed(feed) => {
            let key = feed.name().to_lowercase();
            if let Some(ids) = cache.get("feed", &key) {
                send(Got::Feed(feed, Ok(ids)), false)?;
            }
            let fresh = hn::feed(feed);
            if let Ok(ids) = &fresh {
                cache.put("feed", &key, ids);
            }
            send(Got::Feed(feed, fresh), true)
        }
        Job::Search(query) => {
            let found = hn::search(&query);
            send(Got::Search(query, found), true)
        }
        Job::StoryOf(id) => send(Got::StoryOf(id, hn::story_of(id)), true),
        Job::Story(id) => {
            if let Some(story) = cache.get("story", &id.to_string()) {
                send(Got::Story(id, Ok(story)), false)?;
            }
            let fresh = hn::story(id);
            if let Ok(story) = &fresh {
                cache.put("story", &id.to_string(), story);
            }
            send(Got::Story(id, fresh), true)
        }
        Job::Thread(id, order) => {
            if let Some(comments) = cache.get("thread", &id.to_string()) {
                send(Got::Thread(id, Ok(comments)), false)?;
            }
            let fresh = hn::thread(id, &order);
            if let Ok(comments) = &fresh {
                cache.put("thread", &id.to_string(), comments);
            }
            send(Got::Thread(id, fresh), true)
        }
        Job::Login(user, password) => send(Got::LoggedIn(auth::login(&user, &password)), true),
        Job::Upvote(session, id) => send(Got::Upvoted(auth::upvote(&session, id)), true),
        Job::ReplyForm(session, id, is_story) => {
            send(Got::ReplyForm(id, auth::reply_form(&session, id, is_story)), true)
        }
        Job::Post(session, form, text, story) => {
            send(Got::Posted(story, auth::post(&session, &form, &text)), true)
        }
        Job::User(name) => {
            if let Some(user) = cache.get("user", &name) {
                send(Got::User(name.clone(), Ok(user)), false)?;
            }
            let fresh = hn::user(&name);
            if let Ok(user) = &fresh {
                cache.put("user", &name, user);
            }
            send(Got::User(name, fresh), true)
        }
        Job::Replies(name) => {
            if let Some(replies) = cache.get("replies", &name) {
                send(Got::Replies(name.clone(), Ok(replies)), false)?;
            }
            let fresh = hn::replies(&name);
            if let Ok(replies) = &fresh {
                cache.put("replies", &name, replies);
            }
            send(Got::Replies(name, fresh), true)
        }
        Job::Figure(id, index, url) => {
            let key = format!("{id}-{index}");
            let picture = match cache.get_bytes("figure", &key) {
                Some(bytes) => figure::decode(&bytes),
                None => figure::download(&url).and_then(|bytes| {
                    let picture = figure::decode(&bytes)?;
                    cache.put_bytes("figure", &key, &bytes);
                    Ok(picture)
                }),
            };
            send(Got::Figure(id, index, picture), true)
        }
        Job::Article(id, url) => {
            if let Some(article) = cache.get(ARTICLES, &id.to_string()) {
                return send(Got::Article(id, article), true);
            }
            let article = article::fetch(&url);
            // What couldn't be read may be readable next time.
            if matches!(article, Article::Text { .. }) {
                cache.put(ARTICLES, &id.to_string(), &article);
            }
            send(Got::Article(id, article), true)
        }
    }
}
