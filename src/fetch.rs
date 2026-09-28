//! Fetching in the background: workers take jobs from a queue, and send
//! back what they get. Urgent jobs (the story on screen) go to the front,
//! prefetching the ones around it to the back.
//!
//! Articles have workers of their own: other sites can be slow to answer,
//! and HN's quick answers mustn't wait behind them.

use crate::article::{self, Article};
use crate::hn::{self, Comment, Feed, Story};
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// Workers for HN's APIs, and for articles.
const HN_WORKERS: usize = 8;
const ARTICLE_WORKERS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    Feed(Feed),
    Story(u64),
    /// A story's comments, with its top-level ones in HN's order.
    Thread(u64, Vec<u64>),
    Article(u64, String),
}

pub enum Done {
    Feed(Feed, Result<Vec<u64>, String>),
    Story(u64, Result<Story, String>),
    Thread(u64, Result<Vec<Comment>, String>),
    Article(u64, Article),
}

type Queue = Arc<(Mutex<VecDeque<Job>>, Condvar)>;

pub struct Fetcher {
    hn: Queue,
    articles: Queue,
    pub done: Receiver<Done>,
}

impl Fetcher {
    pub fn start() -> Fetcher {
        let (tx, done) = mpsc::channel();
        let pool = |workers| {
            let queue: Queue = Arc::default();
            for _ in 0..workers {
                let queue = Arc::clone(&queue);
                let tx = tx.clone();
                std::thread::spawn(move || work(&queue, &tx));
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

fn work(queue: &Queue, tx: &Sender<Done>) {
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
        let done = match job {
            Job::Feed(feed) => Done::Feed(feed, hn::feed(feed)),
            Job::Story(id) => Done::Story(id, hn::story(id)),
            Job::Thread(id, order) => Done::Thread(id, hn::thread(id, &order)),
            Job::Article(id, url) => Done::Article(id, article::fetch(&url)),
        };
        if tx.send(done).is_err() {
            return;
        }
    }
}
