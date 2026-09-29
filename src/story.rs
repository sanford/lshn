//! A story as one Markdown document: its title and details, the article,
//! then the comments. The reader shows it like any other document, so
//! search, the outline and link hints all work on it.
//!
//! Everything HN sends is someone else's text: it's escaped here, so it
//! reads as written and never as Markdown.

use crate::article::{self, Article};
use crate::figure;
use std::collections::HashSet;
use std::ops::Range;
use crate::hn::{self, Comment, Story, User};

/// Replies nest this deep, then stay there, so deep threads keep room.
const MAX_DEPTH: usize = 10;
/// The preview shows about this much of a long article.
const PREVIEW_WORDS: usize = 180;

/// What's known of a story's comments so far.
pub enum Comments<'a> {
    Loading,
    Failed(&'a str),
    Loaded(&'a [Comment]),
}

/// The heading that starts the comments, which `C` jumps to.
pub const COMMENTS_HEADING: &str = "Comments";

/// What the reader has done to a thread: the newest comment they'd seen
/// before (newer ones are marked new), and what they've folded.
#[derive(Clone, Copy)]
pub struct Marks<'a> {
    pub seen: Option<u64>,
    pub folded: &'a HashSet<u64>,
}

/// The story's document. `article` is `None` while it's still coming;
/// `preview` shortens a long article, for the list's preview pane.
/// `marks` say which comments are new, and which folded.
pub fn markdown(
    story: &Story,
    article: Option<&Article>,
    comments: Comments,
    preview: bool,
    now: u64,
    marks: Marks,
    pictures: Pictures,
) -> String {
    let Marks { seen, folded } = marks;
    let mut md = String::new();
    md.push_str(&format!("# {}\n\n", escape(&story.title)));
    md.push_str(&details(story, now));
    md.push_str("\n\n");

    if let Some(text) = story.text.as_deref().filter(|t| !t.is_empty()) {
        md.push_str(&html_to_md(text));
        md.push_str("\n\n");
    }
    if story.url.is_some() {
        // Thin rules around what came from the site rather than from HN.
        md.push_str(&article_rule(story, article));
        md.push_str(&article_md(story, article, preview, pictures));
        md.push_str("\n\n---\n\n");
    }

    let count = match &comments {
        Comments::Loaded(c) => c.iter().map(Comment::count).sum(),
        _ => story.descendants as usize,
    };
    let new = match (&comments, seen) {
        (Comments::Loaded(c), Some(seen)) => c.iter().map(|c| c.newer_than(seen)).sum(),
        _ => 0,
    };
    if new > 0 {
        md.push_str(&format!("## {COMMENTS_HEADING} ({count}, {new} new)\n\n"));
    } else {
        md.push_str(&format!("## {COMMENTS_HEADING} ({count})\n\n"));
    }
    match comments {
        Comments::Loading => md.push_str("*Loading comments…*\n"),
        Comments::Failed(why) => {
            md.push_str(&format!("*Couldn't load the comments: {}*\n", escape(why)))
        }
        Comments::Loaded([]) => md.push_str("*No comments yet.*\n"),
        Comments::Loaded(comments) => {
            let context = Context {
                op: &story.by,
                now,
                seen,
                folded,
            };
            for c in comments {
                comment(&mut md, c, 0, &context);
            }
        }
    }
    md
}

/// "example.com · 412 points · alice · 3h ago · 187 comments": where it's
/// from stands out, the rest is muted.
fn details(story: &Story, now: u64) -> String {
    let hn_page = story.hn_url();
    let mut parts = Vec::new();
    if let (Some(url), Some(domain)) = (&story.url, story.domain()) {
        parts.push(format!("[{}](<{}>)", escape(&domain), link_target(url)));
    }
    parts.push(format!("[{} points](<{hn_page}> \"muted\")", story.score));
    if hn::is_username(&story.by) {
        parts.push(format!("[{}](<{}> \"muted\")", escape(&story.by), hn::user_url(&story.by)));
    } else if !story.by.is_empty() {
        parts.push(escape(&story.by));
    }
    parts.push(format!("[{}](<{hn_page}> \"muted\")", ago(story.time, now)));
    let s = if story.descendants == 1 { "" } else { "s" };
    parts.push(format!("[{} comment{s}](<{hn_page}> \"muted\")", story.descendants));
    parts.join(" · ")
}

/// "article from example.com · 6 min read", on the rule above it.
fn article_rule(story: &Story, article: Option<&Article>) -> String {
    let mut label = String::from("article");
    if let Some(domain) = story.domain() {
        // Nothing in it can end the comment early.
        let domain: String = domain.chars().filter(|c| !c.is_control()).collect();
        label.push_str(&format!(" from {}", domain.replace("--", "-")));
    }
    if let Some(Article::Text { words, .. }) = article {
        label.push_str(&format!(" · {} min read", article::minutes(*words)));
    }
    format!("<!-- rule: {label} -->\n\n")
}

/// The article's pictures that have been fetched, by their place in it.
pub type Pictures<'a> = &'a dyn Fn(usize) -> Option<&'a image::DynamicImage>;

/// With its pictures, once they're fetched: the first at the top, the rest
/// after the paragraphs they're in. The preview has only the first.
fn article_md(story: &Story, article: Option<&Article>, preview: bool, pictures: Pictures) -> String {
    match article {
        None => "*Loading the article…*".into(),
        // A note with a bar down its left, so it isn't read as the article.
        Some(Article::Unreadable(why)) => {
            let mut note = format!(
                "> [!NOTE] Couldn't read the article\n> {}.",
                escape(&capitalized(why))
            );
            if let Some(url) = &story.url {
                // The address, and right under it how to get there.
                note.push_str(&format!(
                    "\n>\n> [{}](<{}>)\\\n> `w` opens it in your browser, `c` copies it.",
                    escape(url),
                    link_target(url)
                ));
            }
            note
        }
        Some(Article::Text { md, words }) => {
            let md = with_pictures(md, preview, pictures);
            let md = demote_headings(&md, &story.title);
            if preview && *words > PREVIEW_WORDS * 3 / 2 {
                format!(
                    "{}\n\n*… {} min read, `⏎` for the rest*",
                    first_words(&md, PREVIEW_WORDS),
                    article::minutes(*words)
                )
            } else {
                md
            }
        }
    }
}

fn capitalized(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn with_pictures(md: &str, preview: bool, pictures: Pictures) -> String {
    let rows = if preview { figure::PREVIEW_ROWS } else { figure::FULL_ROWS };
    let found = figure::all(md);
    let shown = if preview { &found[..found.len().min(1)] } else { &found[..] };
    // From the end back, so what's left to do stays where it was found.
    let mut edits: Vec<(usize, Range<usize>, String)> = Vec::new();
    for (i, f) in shown.iter().enumerate() {
        let Some(picture) = pictures(i) else { continue };
        let at = if i == 0 { 0 } else { f.after };
        edits.push((i, at..at, format!("\n\n{}\n\n", figure::marker(picture, rows, i))));
        edits.push((i, f.at.clone(), String::new()));
    }
    // Where a picture's taken out at the very place one goes in (an article
    // that starts with its first), out before in, or the taking out would
    // cut into what went in.
    edits.sort_by_key(|(i, range, _)| (std::cmp::Reverse(range.start), std::cmp::Reverse(*i), range.is_empty()));
    let mut md = md.to_string();
    for (_, range, text) in edits {
        md.replace_range(range, &text);
    }
    md.trim_start().to_string()
}

/// The article's headings one level down, under the story's title, and
/// without a first heading that only repeats the title.
fn demote_headings(md: &str, title: &str) -> String {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    let mut seen_heading = false;
    for line in md.lines() {
        let trimmed = line.trim_start();
        if let Some(f) = fence {
            if trimmed.starts_with(f) {
                fence = None;
            }
            out.push(line.to_string());
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            out.push(line.to_string());
            continue;
        }
        let hashes = line.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            let text = line[hashes..].trim();
            let first = !seen_heading;
            seen_heading = true;
            if first && text.eq_ignore_ascii_case(title.trim()) {
                continue;
            }
            out.push(format!("{} {text}", "#".repeat((hashes + 1).min(6))));
            continue;
        }
        out.push(line.to_string());
    }
    out.join("\n").trim().to_string()
}

/// Whole blocks from the start of `md` until they've `words` words.
fn first_words(md: &str, words: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut fenced = false;
    for line in md.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
        }
        if line.trim().is_empty() && !fenced && count >= words {
            break;
        }
        count += line.split_whitespace().count();
        out.push_str(line);
        out.push('\n');
    }
    if fenced {
        out.push_str("```\n");
    }
    out.trim_end().to_string()
}

/// What every comment's header needs to know.
struct Context<'a> {
    /// The story's author, whose comments say so.
    op: &'a str,
    now: u64,
    /// The newest comment seen before: newer ones are marked new.
    seen: Option<u64>,
    /// Comments shown as just their header, their text and replies hidden.
    folded: &'a HashSet<u64>,
}

/// A comment and its replies. Top-level comments are headings, so `]`,
/// `[` and the outline go between them; replies nest in quote bars.
fn comment(md: &mut String, c: &Comment, depth: usize, context: &Context) {
    let who = if c.by.is_empty() {
        "*\\[deleted\\]*".to_string()
    } else if c.by == context.op {
        format!("{} (OP)", author(&c.by))
    } else {
        author(&c.by)
    };
    // The age links to the comment, as on HN: it's what `v` and `r` choose.
    let when = format!("[{}](<{}> \"muted\")", ago(c.time, context.now), hn::item_url(c.id));
    let new = if context.seen.is_some_and(|seen| c.id > seen) {
        " · `new`"
    } else {
        ""
    };
    let folded = context.folded.contains(&c.id);
    let hidden = match c.count() - 1 {
        _ if !folded => String::new(),
        0 => " · ▸ folded".to_string(),
        1 => " · ▸ 1 more".to_string(),
        n => format!(" · ▸ {n} more"),
    };
    let mut body = if depth == 0 {
        format!("### {who} · {when}{new}{hidden}\n\n")
    } else {
        format!("**{who}** · {when}{new}{hidden}\n\n")
    };
    if !folded {
        body.push_str(&html_to_md(&c.text));
    }
    // Every comment in a bar, top-level ones too, so its header and text
    // read as one block.
    let bars = "> ".repeat(depth.min(MAX_DEPTH) + 1);
    // So the reader knows which lines are which comment's.
    md.push_str(&crate::render::comment_marker(c.id, depth.min(MAX_DEPTH)));
    md.push_str("\n\n");
    for line in body.trim_end().lines() {
        md.push_str(bars.trim_end());
        if !line.is_empty() {
            if !bars.is_empty() {
                md.push(' ');
            }
            md.push_str(line);
        }
        md.push('\n');
    }
    md.push('\n');
    if folded {
        return;
    }
    for reply in &c.replies {
        comment(md, reply, depth + 1, context);
    }
}

/// A username, linked to their page (which lshn shows itself).
fn author(name: &str) -> String {
    if hn::is_username(name) {
        format!("[{}](<{}>)", escape(name), hn::user_url(name))
    } else {
        escape(name)
    }
}

/// Someone's page: who they are, then what they've posted lately, each a
/// heading so `]` and `[` go between them.
pub fn user_markdown(name: &str, user: Option<&Result<User, String>>, now: u64) -> String {
    let mut md = format!("# {}\n\n", escape(name));
    let user = match user {
        None => return md + "*Loading…*\n",
        Some(Err(e)) => return md + &format!("*Couldn't load: {}*\n", escape(e)),
        Some(Ok(user)) => user,
    };
    md.push_str(&format!(
        "{} karma · joined {} · [on HN](<{}>)\n\n",
        user.karma,
        ago(user.created, now),
        hn::user_url(name)
    ));
    if let Some(about) = user.about.as_deref().filter(|a| !a.trim().is_empty()) {
        md.push_str(&html_to_md(about));
        md.push_str("\n\n");
    }
    md.push_str("## Recent\n\n");
    if user.recent.is_empty() {
        md.push_str("*Nothing yet.*\n");
    }
    for post in &user.recent {
        let when = ago(post.time, now);
        match (&post.title, &post.text) {
            (Some(title), _) => {
                md.push_str(&format!(
                    "### [{}](<{}>)\n\n{} points · {} comments · {when}\n\n",
                    escape(title),
                    hn::item_url(post.id),
                    post.points.unwrap_or(0),
                    post.comments.unwrap_or(0),
                ));
            }
            (None, Some(text)) => {
                let on = match (post.story_id, &post.story_title) {
                    (Some(id), Some(title)) => {
                        format!("On [{}](<{}>)", escape(title), hn::item_url(id))
                    }
                    _ => "A comment".into(),
                };
                // In a bar, like comments in a thread.
                let body = format!("### {on} · {when}\n\n{}", html_to_md(text));
                for line in body.trim_end().lines() {
                    md.push_str(if line.is_empty() { ">" } else { "> " });
                    md.push_str(line);
                    md.push('\n');
                }
                md.push('\n');
            }
            (None, None) => {}
        }
    }
    md
}

/// "3h ago", from seconds since the epoch.
pub fn ago(then: u64, now: u64) -> String {
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    match now.saturating_sub(then) {
        s if s < MIN => "now".into(),
        s if s < HOUR => format!("{}m ago", s / MIN),
        s if s < DAY => format!("{}h ago", s / HOUR),
        s if s < 14 * DAY => format!("{}d ago", s / DAY),
        s if s < 60 * DAY => format!("{}w ago", s / (7 * DAY)),
        s if s < 365 * DAY => format!("{}mo ago", s / (30 * DAY)),
        s => format!("{}y ago", s / (365 * DAY)),
    }
}

/// `text` with everything Markdown might act on escaped.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.push(' '),
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '!' | '|' | '~' | '&' | '-'
            | '+' | '.' | '{' | '}' | '(' | ')' | '=' | '^' | '$' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// A URL to go between `<` and `>` as a link's target.
fn link_target(url: &str) -> String {
    url.chars()
        .flat_map(|c| match c {
            ' ' => "%20".chars().collect::<Vec<_>>(),
            '<' => "%3C".chars().collect(),
            '>' => "%3E".chars().collect(),
            c if c.is_control() => Vec::new(),
            c => vec![c],
        })
        .collect()
}

/// HN's comment HTML as plain text, paragraphs apart: for quoting in a
/// reply's draft.
pub fn html_to_text(html: &str) -> String {
    let with_breaks = html.replace("<p>", "\n\n");
    let mut out = String::new();
    let mut in_tag = false;
    for c in with_breaks.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode(&out).trim().to_string()
}

/// HN's comment HTML as Markdown. HN uses only a few tags: `<p>` between
/// paragraphs, `<i>`, `<a href>`, and `<pre><code>` for code.
pub fn html_to_md(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    // Inside a link: its target and text so far.
    let mut link: Option<(String, String)> = None;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(&mut out, &mut link, &decode(rest));
            break;
        };
        push_text(&mut out, &mut link, &decode(&rest[..lt]));
        rest = &rest[lt..];
        let Some(gt) = rest.find('>') else {
            push_text(&mut out, &mut link, &decode(rest));
            break;
        };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_ascii_lowercase();
        let closing = tag.starts_with('/');
        match (name.as_str(), closing) {
            ("p", false) => {
                finish_link(&mut out, &mut link);
                out.push_str("\n\n");
            }
            ("i" | "em", _) => push_raw(&mut out, &mut link, "*"),
            ("b" | "strong", _) => push_raw(&mut out, &mut link, "**"),
            ("a", false) => {
                finish_link(&mut out, &mut link);
                link = Some((attr(tag, "href").map(|h| decode(&h)).unwrap_or_default(), String::new()));
            }
            ("a", true) => finish_link(&mut out, &mut link),
            ("pre", false) => {
                finish_link(&mut out, &mut link);
                let end = rest.find("</pre>").unwrap_or(rest.len());
                let inner = &rest[..end];
                rest = rest.get(end + "</pre>".len()..).unwrap_or("");
                let inner = inner.trim_start_matches("<code>");
                let inner = inner.strip_suffix("</code>").unwrap_or(inner);
                out.push_str(&code_block(&decode(inner)));
            }
            _ => {}
        }
    }
    finish_link(&mut out, &mut link);
    quotes(out.trim())
}

/// HN has no quotes: people start a paragraph with `>` instead. Those go in
/// italics, so a reply reads as what it answers, then the answer.
fn quotes(md: &str) -> String {
    md.split("\n\n")
        .map(|para| {
            let quoted = para.starts_with("\\>") && !para.contains('\n');
            if quoted {
                format!("_{}_", para.trim_end())
            } else {
                para.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn push_text(out: &mut String, link: &mut Option<(String, String)>, text: &str) {
    match link {
        Some((_, t)) => t.push_str(text),
        None => out.push_str(&escape(text)),
    }
}

fn push_raw(out: &mut String, link: &mut Option<(String, String)>, md: &str) {
    if link.is_none() {
        out.push_str(md);
    }
}

fn finish_link(out: &mut String, link: &mut Option<(String, String)>) {
    let Some((href, text)) = link.take() else { return };
    let text = if text.trim().is_empty() { href.clone() } else { text };
    if href.is_empty() {
        out.push_str(&escape(&text));
    } else {
        out.push_str(&format!("[{}](<{}>)", escape(&text), link_target(&href)));
    }
}

fn code_block(code: &str) -> String {
    let longest = code
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    let code = code.trim_matches('\n');
    format!("\n\n{fence}\n{code}\n{fence}\n\n")
}

/// The value of attribute `name` in a tag's text.
fn attr(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let rest = &rest[1..];
        Some(rest[..rest.find(quote)?].to_string())
    } else {
        Some(rest.split_whitespace().next()?.to_string())
    }
}

/// HTML's character references, the ones HN uses and numeric ones.
fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // References are short: look for the `;` only nearby.
        let semi = rest
            .char_indices()
            .take_while(|&(i, _)| i < 12)
            .find(|&(_, c)| c == ';')
            .map(|(i, _)| i);
        let Some(semi) = semi else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story() -> Story {
        Story {
            id: 7,
            title: "Show HN: A *thing*".into(),
            url: Some("https://www.example.com/post".into()),
            by: "alice".into(),
            score: 42,
            time: 1000,
            descendants: 2,
            ..Story::default()
        }
    }

    #[test]
    fn comment_html_becomes_markdown() {
        let html = "Look at <i>this</i>: <a href=\"https:&#x2F;&#x2F;x.com&#x2F;a_b\" rel=\"nofollow\">https:&#x2F;&#x2F;x.com&#x2F;a_b</a><p>2 * 3 &gt; 5<pre><code>  let x = `a`;\n</code></pre>after";
        assert_eq!(
            html_to_md(html),
            "Look at *this*: [https://x\\.com/a\\_b](<https://x.com/a_b>)\n\n2 \\* 3 \\> 5\n\n```\n  let x = `a`;\n```\n\nafter"
        );
    }

    #[test]
    fn quoted_paragraphs_go_in_italics() {
        assert_eq!(
            html_to_md("&gt; they said <i>this</i><p>I disagree.<p>&gt;&gt; nested"),
            "_\\> they said *this*_\n\nI disagree\\.\n\n_\\>\\> nested_"
        );
        // Code isn't touched.
        assert!(html_to_md("<pre><code>&gt; prompt\n</code></pre>").contains("\n> prompt\n"));
    }

    #[test]
    fn plain_text_for_quoting() {
        assert_eq!(
            html_to_text("It&#x27;s <i>fine</i>.<p>See <a href=\"x\">x.com</a>"),
            "It's fine.\n\nSee x.com"
        );
    }

    #[test]
    fn decodes_references() {
        assert_eq!(decode("a &amp; b &#x27;c&#39; &bogus; &"), "a & b 'c' &bogus; &");
        // Not a reference, just before text that isn't ASCII.
        assert_eq!(decode("Q&A’s ’’’’ &amp;"), "Q&A’s ’’’’ &");
    }

    #[test]
    fn folded_comments_are_just_their_header() {
        let reply = Comment {
            id: 2,
            by: "alice".into(),
            text: "the reply".into(),
            time: 0,
            replies: vec![],
        };
        let comments = vec![Comment {
            id: 1,
            by: "bob".into(),
            text: "the comment".into(),
            time: 0,
            replies: vec![reply],
        }];
        let folded = HashSet::from([1]);
        let md = markdown(&story(), None, Comments::Loaded(&comments), false, 0, Marks { seen: None, folded: &folded }, &|_| None);
        assert!(md.contains("· ▸ 1 more"), "{md}");
        assert!(!md.contains("the comment") && !md.contains("the reply"), "{md}");
        let md = markdown(&story(), None, Comments::Loaded(&comments), false, 0, Marks { seen: None, folded: &HashSet::new() }, &|_| None);
        assert!(md.contains("the comment") && md.contains("the reply") && !md.contains("▸"));
    }

    #[test]
    fn replies_nest_in_quotes_under_top_level_headings() {
        let comments = vec![Comment {
            id: 1,
            by: "bob".into(),
            text: "Top<p>second".into(),
            time: 10_000 - 3600,
            replies: vec![Comment {
                id: 2,
                by: "alice".into(),
                text: "reply".into(),
                time: 10_000 - 60,
                replies: vec![],
            }],
        }];
        let md = markdown(&story(), None, Comments::Loaded(&comments), false, 10_000 + 3600, Marks { seen: None, folded: &HashSet::new() }, &|_| None);
        assert!(md.starts_with("# Show HN: A \\*thing\\*\n"), "{md}");
        assert!(md.contains("[example\\.com](<https://www.example.com/post>) · [42 points](<https://news.ycombinator.com/item?id=7> \"muted\")"), "{md}");
        assert!(md.contains("*Loading the article…*"));
        assert!(md.contains("## Comments (2)\n"));
        assert!(md.contains("> ### [bob](<https://news.ycombinator.com/user?id=bob>) · [2h ago](<https://news.ycombinator.com/item?id=1> \"muted\")\n>\n> Top\n>\n> second\n"), "{md}");
        assert!(md.contains("> > **[alice](<https://news.ycombinator.com/user?id=alice>) (OP)** · [1h ago](<https://news.ycombinator.com/item?id=2> \"muted\")\n> >\n> > reply\n"), "{md}");
        // Still headings, in their bars, for `]` and `[`.
        let doc = crate::render::render(&md, 80, &crate::theme::Theme::plain(), None, None);
        assert!(doc.headings.iter().any(|h| h.level == 3 && h.text.starts_with("bob")));
    }

    #[test]
    fn marks_comments_newer_than_the_last_seen() {
        let reply = Comment {
            id: 30,
            by: "carol".into(),
            text: "later".into(),
            ..Comment::default()
        };
        let comments = vec![Comment {
            id: 10,
            by: "bob".into(),
            text: "early".into(),
            replies: vec![reply],
            ..Comment::default()
        }];
        let md = markdown(&story(), None, Comments::Loaded(&comments), false, 0, Marks { seen: Some(20), folded: &HashSet::new() }, &|_| None);
        assert!(md.contains("## Comments (2, 1 new)"), "{md}");
        assert!(md.contains("id=bob>) · [now](<https://news.ycombinator.com/item?id=10> \"muted\")\n"), "{md}");
        assert!(md.contains("id=carol>)** · [now](<https://news.ycombinator.com/item?id=30> \"muted\") · `new`"), "{md}");
        // Never read: nothing's new.
        let md = markdown(&story(), None, Comments::Loaded(&comments), false, 0, Marks { seen: None, folded: &HashSet::new() }, &|_| None);
        assert!(!md.contains("`new`") && !md.contains("new)"), "{md}");
    }

    #[test]
    fn user_pages_list_what_theyve_posted() {
        let user = User {
            id: "pg".into(),
            karma: 10,
            about: Some("Bug fixer.".into()),
            recent: vec![
                hn::Post {
                    id: 2,
                    text: Some("A reply".into()),
                    story_id: Some(1),
                    story_title: Some("An essay".into()),
                    ..hn::Post::default()
                },
                hn::Post {
                    id: 1,
                    title: Some("An essay".into()),
                    points: Some(5),
                    comments: Some(3),
                    ..hn::Post::default()
                },
            ],
            ..User::default()
        };
        let md = user_markdown("pg", Some(&Ok(user)), 0);
        assert!(md.starts_with("# pg\n\n10 karma · joined now"), "{md}");
        assert!(md.contains("Bug fixer\\."));
        assert!(md.contains("> ### On [An essay](<https://news.ycombinator.com/item?id=1>) · now\n>\n> A reply"), "{md}");
        assert!(md.contains("### [An essay](<https://news.ycombinator.com/item?id=1>)\n\n5 points · 3 comments"), "{md}");
        assert!(user_markdown("pg", None, 0).contains("Loading"));
    }

    #[test]
    fn unreadable_articles_say_why_and_where_they_are() {
        let article = Article::Unreadable("the page is gone".into());
        let md = markdown(&story(), Some(&article), Comments::Loading, false, 0, Marks { seen: None, folded: &HashSet::new() }, &|_| None);
        assert!(md.contains("> [!NOTE] Couldn't read the article\n> The page is gone."), "{md}");
        assert!(
            md.contains("> [https://www\\.example\\.com/post](<https://www.example.com/post>)\\\n> `w` opens it"),
            "{md}"
        );
    }

    #[test]
    fn pictures_go_first_and_after_their_paragraphs() {
        let md = "Intro.\n\n![one](https://x.com/1.jpg)\n\nText ![two](https://x.com/2.jpg) more.\n\nEnd.";
        let picture = image::DynamicImage::new_rgb8(400, 200);
        let both = |_| Some(&picture);
        assert_eq!(
            with_pictures(md, false, &both),
            "<!-- image: 400 200 24 0 -->\n\nIntro.\n\n\n\nText  more.\n\n\n<!-- image: 400 200 24 1 -->\n\n\nEnd."
        );
        // Only what's been fetched; the rest stay links for now.
        let first = |i| (i == 0).then_some(&picture);
        let shown = with_pictures(md, false, &first);
        assert!(shown.contains("![two]") && !shown.contains("![one]"), "{shown}");
        // Previews have only the first.
        assert!(!with_pictures(md, true, &both).contains("image: 400 200 12 1"));
        // An article that starts with its picture.
        let md = "![one](https://x.com/1.jpg)\n\nIntro.";
        assert_eq!(with_pictures(md, false, &both), "<!-- image: 400 200 24 0 -->\n\n\n\nIntro.");
    }

    #[test]
    fn previews_cut_long_articles_short() {
        let md = (0..40)
            .map(|i| format!("Paragraph {i} has exactly seven words here."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let article = Article::Text {
            md: format!("# Show HN: A *thing*\n\n{md}"),
            words: 280,
        };
        let full = markdown(&story(), Some(&article), Comments::Loading, false, 0, Marks { seen: None, folded: &HashSet::new() }, &|_| None);
        let preview = markdown(&story(), Some(&article), Comments::Loading, true, 0, Marks { seen: None, folded: &HashSet::new() }, &|_| None);
        assert!(full.contains("Paragraph 39"));
        assert!(!preview.contains("Paragraph 39"));
        assert!(preview.contains("min read"));
        // The article's own copy of the title is dropped.
        assert_eq!(full.matches("A *thing*").count() + full.matches("A \\*thing\\*").count(), 1);
    }

    #[test]
    fn article_headings_go_under_the_title() {
        assert_eq!(
            demote_headings("# Other\n\n```\n# not a heading\n```\n## Sub", "Title"),
            "## Other\n\n```\n# not a heading\n```\n### Sub"
        );
    }
}
