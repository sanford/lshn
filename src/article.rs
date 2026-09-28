//! Fetching the page a story links to, and pulling out its text, as
//! Markdown, the way a browser's reader mode does.

use crate::hn;
use dom_smoothie::{Config, Readability, TextMode};

/// Pages bigger than this aren't articles.
const MAX_PAGE: u64 = 8 << 20;

/// Sites whose pages are players or apps, with no text to pull out.
const NOT_ARTICLES: &[(&str, &str)] = &[
    ("youtube.com", "a video"),
    ("youtu.be", "a video"),
    ("vimeo.com", "a video"),
    ("x.com", "a post on X"),
    ("twitter.com", "a post on X"),
];

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Article {
    /// The article's text as Markdown, and how many words it has.
    Text { md: String, words: usize },
    /// Why there's nothing to show: "a PDF", "a video", an error.
    Unreadable(String),
}

pub fn fetch(url: &str) -> Article {
    let domain = hn::domain(url).unwrap_or_default();
    if let Some((_, what)) = NOT_ARTICLES
        .iter()
        .find(|(site, _)| domain == *site || domain.ends_with(&format!(".{site}")))
    {
        return Article::Unreadable((*what).into());
    }
    match fetch_html(url) {
        Ok(html) => extract(&html, url),
        Err(why) => Article::Unreadable(why),
    }
}

fn fetch_html(url: &str) -> Result<String, String> {
    let mut response = hn::agent().get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(404 | 410) => "the page is gone".to_string(),
        ureq::Error::StatusCode(401 | 403) => "the site wouldn't let lshn in".to_string(),
        ureq::Error::StatusCode(code) => format!("the site answered {code}"),
        e => e.to_string(),
    })?;
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !kind.is_empty() && !kind.contains("html") {
        return Err(describe(&kind));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_PAGE)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// What a page that isn't HTML is, from its content type.
fn describe(kind: &str) -> String {
    let kind = kind.split(';').next().unwrap_or("").trim();
    match kind {
        "application/pdf" => "a PDF".into(),
        k if k.starts_with("image/") => "an image".into(),
        k if k.starts_with("video/") => "a video".into(),
        k if k.starts_with("audio/") => "audio".into(),
        "text/plain" => "plain text".into(),
        k => format!("not a web page ({k})"),
    }
}

pub fn extract(html: &str, url: &str) -> Article {
    let config = Config {
        text_mode: TextMode::Markdown,
        ..Config::default()
    };
    let parsed = Readability::new(html, Some(url), Some(config)).and_then(|mut r| r.parse());
    match parsed {
        Ok(article) => {
            let md = drop_metadata(article.text_content.trim());
            if md.is_empty() {
                return Article::Unreadable("no article text found".into());
            }
            let words = md.split_whitespace().count();
            Article::Text { md, words }
        }
        Err(_) => Article::Unreadable("couldn't find the article on the page".into()),
    }
}

/// The article without the lines some pages open with about the page
/// itself: "Updated Sep 28 · 4 min read · 882 words". Only short
/// paragraphs at the start go, and only while they look like that.
fn drop_metadata(md: &str) -> String {
    let mut blocks = md.split("\n\n").peekable();
    while let Some(block) = blocks.peek() {
        let words = block.split_whitespace().count();
        let signals = metadata_signals(block);
        // Sentences end like sentences; lines of metadata don't.
        let sentence = block.trim_end().ends_with(['.', '!', '?', ':']);
        let metadata =
            !sentence && ((words <= 40 && signals >= 2) || (words <= 8 && signals >= 1));
        if !metadata || block.trim_start().starts_with('#') {
            break;
        }
        blocks.next();
    }
    blocks.collect::<Vec<_>>().join("\n\n").trim().to_string()
}

/// How many kinds of page metadata `text` mentions.
fn metadata_signals(text: &str) -> usize {
    let lower = text.to_lowercase();
    let words = [
        &["min read", "minute read", "minutes read", "reading time"][..],
        &["updated"],
        &["published", "posted on", "created"],
        &[" words"],
    ];
    let mut signals = words
        .iter()
        .filter(|any| any.iter().any(|w| lower.contains(w)))
        .count();
    if has_date(&lower) {
        signals += 1;
    }
    signals
}

/// Whether `text` (lowercase) has a date in it, like "sep 28" or
/// "2026-09-28".
fn has_date(text: &str) -> bool {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .collect();
    let number = |w: &str| w.chars().next().is_some_and(|c| c.is_ascii_digit());
    let iso = |w: &str| {
        let parts: Vec<&str> = w.split('-').collect();
        parts.len() == 3
            && parts[0].len() == 4
            && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    };
    words.iter().any(|w| iso(w))
        || words.windows(2).any(|pair| {
            let month = |w: &str| MONTHS.iter().any(|m| w.starts_with(m) && w.len() <= 9);
            (month(pair[0]) && number(pair[1])) || (number(pair[0]) && month(pair[1]))
        })
}

/// Minutes to read `words`, at a comfortable pace.
pub fn minutes(words: usize) -> usize {
    words.div_ceil(230).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_article_as_markdown() {
        let para = "This is a sentence of reasonable length for an article. ".repeat(12);
        let html = format!(
            "<html><head><title>T</title></head><body><nav>Home | About</nav>\
             <article><h1>Title</h1><p>{para}</p><h2>Part two</h2><p>{para}</p></article>\
             <footer>© 2026</footer></body></html>"
        );
        let Article::Text { md, words } = extract(&html, "https://example.com/post") else {
            panic!("no text");
        };
        assert!(md.contains("Part two"), "{md}");
        assert!(md.contains("reasonable length"));
        assert!(!md.contains("Home | About"), "{md}");
        assert!(words > 100);
    }

    #[test]
    fn drops_what_pages_say_about_themselves_at_the_start() {
        let body = "If we think writing code is dead, the bigger problem is teams not knowing the system.";
        let md = format!(
            "Last updatedUpdated: Sep 28, 2026 · CreatedCreated: Sep 26, 2026 · 4 min read recently updated Recent changes Sep 28today published · 882 words\n\n5 min read\n\n{body}\n\nUpdated 2026-09-28 later on stays."
        );
        assert_eq!(
            drop_metadata(&md),
            format!("{body}\n\nUpdated 2026-09-28 later on stays.")
        );
        // Ordinary openings stay, even with a date or an "updated" in them.
        for opening in [
            "On May 4 we shipped the new compiler, and here is what we learned about it.",
            "The updated guidance, published last week, says three things.",
            "## Published work",
        ] {
            let md = format!("{opening}\n\n{body}");
            assert_eq!(drop_metadata(&md), md, "{opening}");
        }
    }

    #[test]
    fn names_what_isnt_a_page() {
        assert_eq!(describe("application/pdf"), "a PDF");
        assert_eq!(describe("image/png; x=y"), "an image");
    }

    #[test]
    fn skips_video_sites_without_fetching() {
        assert!(matches!(
            fetch("https://www.youtube.com/watch?v=x"),
            Article::Unreadable(w) if w == "a video"
        ));
    }
}
