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
use crate::hn::{self, Comment, Feed, Story};
use crate::store::Cache;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// Workers for HN's APIs, and for articles.
const HN_WORKERS: usize = 8;
const ARTICLE_WORKERS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    Feed(Feed),
    /// Stories matching a search.
    Search(String),
    Story(u64),
    /// A story's comments, with its top-level ones in HN's order.
    Thread(u64, Vec<u64>),
    Article(u64, String),
}

pub enum Got {
    Feed(Feed, Result<Vec<u64>, String>),
    Search(String, Result<Vec<u64>, String>),
    Story(u64, Result<Story, String>),
    Thread(u64, Result<Vec<Comment>, String>),
    Article(u64, Article),
}

/// Something back from the fetcher.
pub struct Done {
    pub got: Got,
    /// This finishes the job. Otherwise it's from the cache, and the
    /// fetched copy is on its way.
    pub last: bool,
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
            Job::Article(..) => &self.articles,
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
        Job::Article(id, url) => {
            if let Some(article) = cache.get("article", &id.to_string()) {
                return send(Got::Article(id, article), true);
            }
            let article = article::fetch(&url);
            // What couldn't be read may be readable next time.
            if matches!(article, Article::Text { .. }) {
                cache.put("article", &id.to_string(), &article);
            }
            send(Got::Article(id, article), true)
        }
    }
}
