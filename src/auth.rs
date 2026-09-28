//! Acting on HN as you: logging in, voting and commenting. HN has no API
//! for these, so this does what a browser does: each page with a vote
//! arrow or a comment box carries a token for it (an `auth` in the vote
//! link, an `hmac` in the form), which is read from the page and sent back.
//!
//! Nothing here retries. If HN says no (you're posting too fast, say),
//! that's what's reported, and it's for you to try again.

use std::sync::OnceLock;
use std::time::Duration;

const HN: &str = "https://news.ycombinator.com";
/// HN's pages are small; more than this isn't one.
const MAX_PAGE: u64 = 8 << 20;

/// A logged-in session: HN's `user` cookie, "name&token".
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Session {
    pub cookie: String,
}

/// Without the cookie: it's as good as a password.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session({})", self.user())
    }
}

impl Session {
    /// Whose session it is.
    pub fn user(&self) -> &str {
        self.cookie.split('&').next().unwrap_or("")
    }
}

/// An agent that doesn't follow redirects, since where HN redirects to
/// says how things went, and that takes any status as an answer.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(concat!("lshn/", env!("CARGO_PKG_VERSION")))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// What came back: the status, where it redirects to, and the page.
struct Reply {
    status: u16,
    location: Option<String>,
    set_cookie: Vec<String>,
    body: String,
}

fn reply(response: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Reply, String> {
    let mut response = response.map_err(|e| e.to_string())?;
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let location = header("location");
    let set_cookie = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect();
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_PAGE)
        .read_to_string()
        .unwrap_or_default();
    Ok(Reply {
        status,
        location,
        set_cookie,
        body,
    })
}

fn get(session: &Session, path: &str) -> Result<Reply, String> {
    reply(
        agent()
            .get(format!("{HN}/{path}"))
            .header("Cookie", format!("user={}", session.cookie))
            .call(),
    )
}

/// Logs in, for a session to keep.
pub fn login(user: &str, password: &str) -> Result<Session, String> {
    let r = reply(
        agent()
            .post(format!("{HN}/login"))
            .send_form([("acct", user), ("pw", password), ("goto", "news")]),
    )?;
    if let Some(cookie) = r.set_cookie.iter().find_map(|c| user_cookie(c)) {
        return Ok(Session { cookie });
    }
    Err(refusal(&r.body).unwrap_or_else(|| {
        if r.body.contains("recaptcha") || r.body.contains("Validation required") {
            "HN wants a captcha: log in once on the website, then try again here".into()
        } else if r.body.contains("Bad login") {
            "Bad login".into()
        } else {
            format!("HN said {}", r.status)
        }
    }))
}

/// The `user` cookie's value from a `Set-Cookie` header.
fn user_cookie(header: &str) -> Option<String> {
    let value = header.strip_prefix("user=")?.split(';').next()?.trim();
    (value.contains('&') && !value.is_empty()).then(|| value.to_string())
}

/// Upvotes item `id`, a story or a comment.
pub fn upvote(session: &Session, id: u64) -> Result<(), String> {
    let page = get(session, &format!("item?id={id}"))?;
    if !page.body.contains("logout") {
        return Err("Not logged in any more: L to log in again".into());
    }
    let Some(link) = vote_link(&page.body, id) else {
        return Err(if page.body.contains(&format!("id='un_{id}'")) {
            "Already upvoted".into()
        } else {
            "No way to vote on that".into()
        });
    };
    let r = get(session, &link)?;
    match r.status {
        200..=399 if refusal(&r.body).is_none() => Ok(()),
        _ => Err(refusal(&r.body).unwrap_or_else(|| format!("HN said {}", r.status))),
    }
}

/// The link that upvotes item `id`, from a page it's on: its `href`,
/// "vote?id=…&how=up&auth=…". Not there when you've voted on it already.
fn vote_link(page: &str, id: u64) -> Option<String> {
    let at = page.find(&format!("id='up_{id}'"))?;
    let tag = &page[at..at + page[at..].find('>')?];
    if tag.contains("nosee") {
        return None;
    }
    let href = attr(tag, "href")?;
    let href = href.replace("&amp;", "&");
    href.starts_with("vote?").then_some(href)
}

/// What's needed to post a comment: where it goes, and the form's token.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Form {
    pub parent: String,
    pub goto: String,
    pub hmac: String,
}

/// The form for replying to item `id`: a comment's reply page, or a
/// story's own page for a comment at the top of its thread.
pub fn reply_form(session: &Session, id: u64, is_story: bool) -> Result<Form, String> {
    let path = if is_story {
        format!("item?id={id}")
    } else {
        format!("reply?id={id}")
    };
    let page = get(session, &path)?;
    if !page.body.contains("logout") {
        return Err("Not logged in any more: L to log in again".into());
    }
    comment_form(&page.body).ok_or_else(|| {
        refusal(&page.body).unwrap_or_else(|| "No way to reply to that (too old, or locked?)".into())
    })
}

fn comment_form(page: &str) -> Option<Form> {
    let start = page.find("action=\"comment\"").or_else(|| page.find("action='comment'"))?;
    let form = &page[start..start + page[start..].find("</form>")?];
    let input = |name: &str| {
        form.split('<')
            .filter(|t| t.starts_with("input"))
            .find(|t| attr(t, "name").as_deref() == Some(name))
            .and_then(|t| attr(t, "value"))
            .map(|v| v.replace("&amp;", "&"))
    };
    Some(Form {
        parent: input("parent")?,
        goto: input("goto").unwrap_or_else(|| "news".into()),
        hmac: input("hmac")?,
    })
}

/// Posts a comment. Its id isn't known until HN lists it.
pub fn post(session: &Session, form: &Form, text: &str) -> Result<(), String> {
    let r = reply(
        agent()
            .post(format!("{HN}/comment"))
            .header("Cookie", format!("user={}", session.cookie))
            .send_form([
                ("parent", form.parent.as_str()),
                ("goto", form.goto.as_str()),
                ("hmac", form.hmac.as_str()),
                ("text", text),
            ]),
    )?;
    // Posted: HN sends you on to the thread.
    if (300..400).contains(&r.status) && r.location.as_deref().is_some_and(|l| !l.contains("login")) {
        return Ok(());
    }
    Err(refusal(&r.body).unwrap_or_else(|| format!("HN said {}", r.status)))
}

/// What HN says when it turns something down, if it's one of the messages
/// it gives: its text, without the page around it.
fn refusal(page: &str) -> Option<String> {
    const MESSAGES: &[&str] = &[
        "posting too fast",
        "Please slow down",
        "Please try again",
        "can't reply",
        "too old",
        "flagged",
        "Unknown or expired link",
        "Bad login",
    ];
    let text = plain(page);
    let at = MESSAGES.iter().find_map(|m| text.find(m))?;
    // The sentence it's in.
    let start = text[..at].rfind(['.', '!', '?']).map_or(0, |i| i + 1);
    let end = text[at..].find(['.', '!', '?']).map_or(text.len(), |i| at + i + 1);
    Some(text[start..end].trim().to_string())
}

/// A page's text, without its tags, in one line.
fn plain(page: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in page.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An attribute's value in a tag, quoted either way or not at all.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    loop {
        let at = rest.find(name)?;
        let before = rest[..at].chars().last();
        let after = &rest[at + name.len()..];
        rest = after;
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let Some(value) = after.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        return match value.chars().next()? {
            q @ ('"' | '\'') => {
                let value = &value[1..];
                Some(value[..value.find(q)?].to_string())
            }
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .map(str::to_string),
        };
    }
}

/// Where the editor's text is written: a reply, over the comment it
/// answers, which is below the line and left out.
pub const CUT: &str = "# ------------------------ >8 ------------------------";

/// The file to write a reply in: space for it, then the line, then what
/// it's replying to, quoted.
pub fn draft(replying_to: &str, quoted: &str) -> String {
    let mut out = format!(
        "\n\n{CUT}\n# Write your reply above the line; everything from the line down is left out.\n# Save and quit to see it before it's posted. Leave it empty not to post.\n#\n# HN formats: a blank line between paragraphs, *italics*, and lines\n# starting with two spaces as code. Links are made from URLs.\n#\n# Replying to {replying_to}:\n#\n"
    );
    for line in quoted.lines() {
        out.push_str(&format!("# > {line}\n"));
    }
    out
}

/// The reply in a draft: what's above the line, trimmed.
pub fn reply_text(draft: &str) -> String {
    let text = match draft.find(CUT) {
        Some(at) => &draft[..at],
        None => draft,
    };
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: &str = r#"<html><a id='up_42' class='clicky' href='vote?id=42&amp;how=up&amp;auth=abc123&amp;goto=item%3Fid%3D40#42'><div class='votearrow' title='upvote'></div></a>
    <a id='up_43' class='clicky nosee' href='vote?id=43&amp;how=up&amp;auth=def&amp;goto=item%3Fid%3D40#43'></a>
    <a id='un_43' class='clicky' href='vote?id=43&amp;how=un&amp;auth=def'>unvote</a>
    <form action="comment" method="post"><input type="hidden" name="parent" value="40"><input type="hidden" name="goto" value="item?id=40"><input type="hidden" name="hmac" value="f00d"><textarea name="text"></textarea><br><input type="submit" value="add comment"></form>
    <a id='logout' href="logout?auth=x&amp;goto=news">logout</a></html>"#;

    #[test]
    fn finds_vote_links_but_not_for_what_youve_voted_on() {
        assert_eq!(
            vote_link(ITEM, 42).as_deref(),
            Some("vote?id=42&how=up&auth=abc123&goto=item%3Fid%3D40#42")
        );
        assert_eq!(vote_link(ITEM, 43), None);
        assert_eq!(vote_link(ITEM, 44), None);
    }

    #[test]
    fn reads_the_comment_form() {
        assert_eq!(
            comment_form(ITEM),
            Some(Form {
                parent: "40".into(),
                goto: "item?id=40".into(),
                hmac: "f00d".into(),
            })
        );
        assert_eq!(comment_form("<html>no form</html>"), None);
    }

    #[test]
    fn takes_the_session_from_the_cookie() {
        assert_eq!(
            user_cookie("user=pg&Xy12; expires=Sat, 01 Jan 2050 00:00:00 GMT; Secure; HttpOnly").as_deref(),
            Some("pg&Xy12")
        );
        assert_eq!(user_cookie("user=; Max-Age=0"), None);
        assert_eq!(user_cookie("other=pg&x"), None);
        assert_eq!(Session { cookie: "pg&Xy12".into() }.user(), "pg");
    }

    #[test]
    fn reports_what_hn_says_no_with() {
        let page = "<html><body><td>You're posting too fast. Please slow down. Thanks.</td></body></html>";
        assert_eq!(refusal(page).as_deref(), Some("You're posting too fast."));
        assert_eq!(refusal("<p>All fine</p>"), None);
    }

    #[test]
    fn drafts_keep_only_whats_above_the_line() {
        let draft = draft("bob", "first line\nsecond");
        assert!(draft.contains("# > first line\n# > second\n"));
        assert_eq!(reply_text(&draft), "");
        assert_eq!(reply_text(&format!("My reply.\n\n  code\n{draft}")), "My reply.\n\n  code");
    }

    #[test]
    fn attributes_quoted_any_way() {
        assert_eq!(attr("a href='x' id=y", "href").as_deref(), Some("x"));
        assert_eq!(attr(r#"input name="hmac" value="v""#, "value").as_deref(), Some("v"));
        assert_eq!(attr("a id=y>", "id").as_deref(), Some("y"));
        // Not a longer attribute that ends the same way.
        assert_eq!(attr("a data-href='no' href='yes'", "href").as_deref(), Some("yes"));
    }
}
