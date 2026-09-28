//! Hacker News: the story lists and stories from the official Firebase API,
//! and whole comment threads, in one request each, from Algolia's.

use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

const FIREBASE: &str = "https://hacker-news.firebaseio.com/v0";
const ALGOLIA: &str = "https://hn.algolia.com/api/v1";
/// The biggest threads run to several megabytes.
const MAX_JSON: u64 = 64 << 20;

/// The lists on HN's front page, and the keys that pick them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Feed {
    Top,
    New,
    Best,
    Ask,
    Show,
    Jobs,
}

impl Feed {
    pub const ALL: [Feed; 6] = [
        Feed::Top,
        Feed::New,
        Feed::Best,
        Feed::Ask,
        Feed::Show,
        Feed::Jobs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Feed::Top => "Top",
            Feed::New => "New",
            Feed::Best => "Best",
            Feed::Ask => "Ask",
            Feed::Show => "Show",
            Feed::Jobs => "Jobs",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Feed::Top => "topstories",
            Feed::New => "newstories",
            Feed::Best => "beststories",
            Feed::Ask => "askstories",
            Feed::Show => "showstories",
            Feed::Jobs => "jobstories",
        }
    }
}

/// A story (or job, or poll) as the list shows it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Story {
    pub id: u64,
    pub title: String,
    pub url: Option<String>,
    pub by: String,
    pub score: u64,
    /// Seconds since the epoch.
    pub time: u64,
    /// How many comments, all told.
    pub descendants: u64,
    /// Its top-level comments, in HN's order.
    pub kids: Vec<u64>,
    /// An Ask HN's or Show HN's own text, as HTML.
    pub text: Option<String>,
    pub dead: bool,
    pub deleted: bool,
}

impl Story {
    /// The site the story links to, like "example.com", without "www.".
    pub fn domain(&self) -> Option<String> {
        domain(self.url.as_deref()?)
    }

    /// The story's page on HN.
    pub fn hn_url(&self) -> String {
        item_url(self.id)
    }
}

pub fn item_url(id: u64) -> String {
    format!("https://news.ycombinator.com/item?id={id}")
}

pub fn domain(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// A comment, with its replies.
#[derive(Clone, Debug, Default)]
pub struct Comment {
    pub id: u64,
    /// Empty for a deleted comment.
    pub by: String,
    /// As HTML. Empty for a deleted comment.
    pub text: String,
    pub time: u64,
    pub replies: Vec<Comment>,
}

impl Comment {
    /// How many comments this is, with all its replies.
    pub fn count(&self) -> usize {
        1 + self.replies.iter().map(Comment::count).sum::<usize>()
    }
}

/// One agent for everything, so connections are kept and reused.
pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .user_agent(concat!("lshn/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_JSON)
        .read_json()
        .map_err(|e| e.to_string())
}

/// The ids of a feed's stories, in order.
pub fn feed(feed: Feed) -> Result<Vec<u64>, String> {
    get_json(&format!("{FIREBASE}/{}.json", feed.endpoint()))
}

pub fn story(id: u64) -> Result<Story, String> {
    // A missing item comes back as `null`.
    let story: Option<Story> = get_json(&format!("{FIREBASE}/item/{id}.json"))?;
    story.ok_or_else(|| format!("No item {id}"))
}

#[derive(Deserialize)]
struct AlgoliaItem {
    id: u64,
    author: Option<String>,
    text: Option<String>,
    #[serde(default)]
    created_at_i: u64,
    #[serde(default)]
    children: Vec<AlgoliaItem>,
}

impl From<AlgoliaItem> for Comment {
    fn from(item: AlgoliaItem) -> Comment {
        Comment {
            id: item.id,
            by: item.author.unwrap_or_default(),
            text: item.text.unwrap_or_default(),
            time: item.created_at_i,
            replies: tidy(item.children.into_iter().map(Comment::from).collect()),
        }
    }
}

/// Drops deleted comments with nothing under them.
fn tidy(comments: Vec<Comment>) -> Vec<Comment> {
    comments
        .into_iter()
        .filter(|c| !c.text.is_empty() || !c.replies.is_empty())
        .collect()
}

/// A story's comments, all of them. Top-level ones come in `order` (HN's
/// ranking, from the story's `kids`); Algolia's order is by age.
pub fn thread(id: u64, order: &[u64]) -> Result<Vec<Comment>, String> {
    let item: AlgoliaItem = get_json(&format!("{ALGOLIA}/items/{id}"))?;
    let mut comments = tidy(item.children.into_iter().map(Comment::from).collect());
    let rank = |c: &Comment| order.iter().position(|&k| k == c.id).unwrap_or(usize::MAX);
    comments.sort_by_key(rank);
    Ok(comments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_drops_www_and_the_path() {
        assert_eq!(
            domain("https://www.Example.com/a/b?c").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            domain("http://user@blog.x.dev:8080/").as_deref(),
            Some("blog.x.dev:8080")
        );
        assert_eq!(domain("not a url"), None);
    }

    #[test]
    fn threads_follow_hns_order_and_lose_empty_deletions() {
        let json = r#"{"id":1,"author":"op","text":null,"children":[
            {"id":2,"author":"a","text":"first","created_at_i":10,"children":[]},
            {"id":3,"author":null,"text":null,"created_at_i":11,"children":[]},
            {"id":4,"author":"b","text":"second","created_at_i":12,"children":[
                {"id":5,"author":null,"text":null,"children":[
                    {"id":6,"author":"c","text":"kept","children":[]}]}]}]}"#;
        let item: AlgoliaItem = serde_json::from_str(json).unwrap();
        let mut comments = tidy(item.children.into_iter().map(Comment::from).collect());
        let order = [4, 2];
        comments.sort_by_key(|c| order.iter().position(|&k| k == c.id).unwrap_or(usize::MAX));
        let ids: Vec<u64> = comments.iter().map(|c| c.id).collect();
        assert_eq!(ids, [4, 2]);
        // The deleted reply stays: it has a reply of its own.
        assert_eq!(comments[0].replies[0].replies[0].text, "kept");
        assert_eq!(comments[0].count(), 3);
    }
}
