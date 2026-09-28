//! Hacker News: the story lists and stories from the official Firebase API,
//! and whole comment threads, in one request each, from Algolia's.

use serde::{Deserialize, Serialize};
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
    /// How many of this comment and its replies are newer than comment
    /// `seen`.
    pub fn newer_than(&self, seen: u64) -> usize {
        usize::from(self.id > seen) + self.replies.iter().map(|r| r.newer_than(seen)).sum::<usize>()
    }

    /// The newest comment's id, of this one and its replies.
    pub fn newest(&self) -> u64 {
        self.replies.iter().map(Comment::newest).fold(self.id, u64::max)
    }

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

/// Someone on HN, and what they've posted lately.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: String,
    /// Seconds since the epoch.
    pub created: u64,
    pub karma: u64,
    /// As HTML.
    pub about: Option<String>,
    /// Newest first.
    pub recent: Vec<Post>,
}

/// A story or comment someone posted.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Post {
    pub id: u64,
    pub time: u64,
    /// A story's title; `None` for a comment.
    pub title: Option<String>,
    pub url: Option<String>,
    pub points: Option<u64>,
    pub comments: Option<u64>,
    /// A comment's text, as HTML.
    pub text: Option<String>,
    /// The story a comment is on.
    pub story_id: Option<u64>,
    pub story_title: Option<String>,
}

#[derive(Deserialize)]
struct PostHit {
    #[serde(rename = "objectID")]
    id: String,
    #[serde(default)]
    created_at_i: u64,
    title: Option<String>,
    url: Option<String>,
    points: Option<u64>,
    num_comments: Option<u64>,
    comment_text: Option<String>,
    story_id: Option<u64>,
    story_title: Option<String>,
}

#[derive(Deserialize)]
struct PostHits {
    hits: Vec<PostHit>,
}

/// Whether `name` could be an HN username: letters, digits, `-` and `_`.
pub fn is_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn user_url(name: &str) -> String {
    format!("https://news.ycombinator.com/user?id={name}")
}

/// Someone's profile, from HN, and their latest posts, from Algolia.
pub fn user(name: &str) -> Result<User, String> {
    if !is_username(name) {
        return Err(format!("{name} isn't a username"));
    }
    let user: Option<User> = get_json(&format!("{FIREBASE}/user/{name}.json"))?;
    let mut user = user.ok_or_else(|| format!("No user {name}"))?;
    let hits: PostHits = agent()
        .get(format!("{ALGOLIA}/search_by_date"))
        .query("tags", format!("author_{name}"))
        .query("hitsPerPage", "30")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_JSON)
        .read_json()
        .map_err(|e| e.to_string())?;
    user.recent = hits
        .hits
        .into_iter()
        .filter_map(|h| {
            Some(Post {
                id: h.id.parse().ok()?,
                time: h.created_at_i,
                title: h.title.filter(|_| h.comment_text.is_none()),
                url: h.url,
                points: h.points,
                comments: h.num_comments,
                text: h.comment_text,
                story_id: h.story_id,
                story_title: h.story_title,
            })
        })
        .collect();
    Ok(user)
}

/// Where a link on HN goes, when it's somewhere lshn can show.
#[derive(Debug, PartialEq, Eq)]
pub enum Link {
    Item(u64),
    User(String),
}

/// The story or user an HN link is to, like
/// `https://news.ycombinator.com/item?id=123`.
pub fn link(url: &str) -> Option<Link> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.strip_prefix("news.ycombinator.com/")?;
    let (page, query) = rest.split_once('?')?;
    let query = query.split('#').next().unwrap_or("");
    let id = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("id="))?;
    match page {
        "item" => id.parse().ok().map(Link::Item),
        "user" if is_username(id) => Some(Link::User(id.to_string())),
        _ => None,
    }
}

#[derive(Deserialize)]
struct SearchResults {
    hits: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    #[serde(rename = "objectID")]
    id: String,
}

/// Stories matching `query`, best first, from Algolia's search.
pub fn search(query: &str) -> Result<Vec<u64>, String> {
    let results: SearchResults = agent()
        .get(format!("{ALGOLIA}/search"))
        .query("query", query)
        .query("tags", "story")
        .query("hitsPerPage", "60")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_JSON)
        .read_json()
        .map_err(|e| e.to_string())?;
    Ok(results.hits.iter().filter_map(|h| h.id.parse().ok()).collect())
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
    fn knows_hn_links() {
        assert_eq!(link("https://news.ycombinator.com/item?id=123"), Some(Link::Item(123)));
        assert_eq!(link("http://news.ycombinator.com/item?id=9&p=2#x"), Some(Link::Item(9)));
        assert_eq!(link("https://news.ycombinator.com/user?id=pg"), Some(Link::User("pg".into())));
        assert_eq!(link("https://news.ycombinator.com/user?id=a%20b"), None);
        assert_eq!(link("https://news.ycombinator.com/newest"), None);
        assert_eq!(link("https://example.com/item?id=1"), None);
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
