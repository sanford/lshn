//! Pages that aren't HTML but are still for reading: Markdown, as a README
//! or a raw `.md` file is, and plain text, as an RFC is.

use crate::article::{Article, absolute};
use crate::render;
use crate::story::escape;
use comrak::nodes::NodeValue;
use comrak::{Arena, format_commonmark, parse_document};

/// The fence info string of a block of plain text: kept as it's laid out,
/// but not drawn as code.
pub const VERBATIM: &str = "verbatim";

/// What a page's content type and address say it is.
#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Html,
    Markdown,
    Text,
}

/// `kind` is the content type, lowercase; servers label raw `.md` files
/// `text/plain`, and now and then HTML too.
pub fn kind(kind: &str, url: &str, body: &str) -> Kind {
    let kind = kind.split(';').next().unwrap_or("").trim();
    let html = looks_like_html(body);
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    let md_path = path.ends_with(".md") || path.ends_with(".markdown");
    match kind {
        "text/markdown" | "text/x-markdown" => Kind::Markdown,
        "text/plain" if html => Kind::Html,
        "text/plain" if md_path => Kind::Markdown,
        "text/plain" => Kind::Text,
        _ => Kind::Html,
    }
}

fn looks_like_html(body: &str) -> bool {
    let head: String = body.chars().take(256).collect::<String>().to_lowercase();
    head.contains("<!doctype html") || head.contains("<html")
}

/// A Markdown page, ready to go in a story's document: its links made
/// whole, anything that would pass for lshn's own markers taken out, and a
/// heading it opens with taken as its title.
pub fn markdown(md: &str, url: &str) -> Article {
    let arena = Arena::new();
    let options = render::options();
    let root = parse_document(&arena, md, &options);
    let mut title = String::new();
    let mut gone = Vec::new();
    for node in root.descendants() {
        let mut ast = node.data_mut();
        match &mut ast.value {
            NodeValue::Link(link) | NodeValue::Image(link) => {
                link.url = resolve(url, &link.url);
            }
            // lshn marks where comments, pictures and rules go with HTML
            // comments: a page's own can't be taken for them.
            NodeValue::HtmlBlock(html) if html.literal.trim_start().starts_with("<!--") => {
                gone.push(node)
            }
            NodeValue::HtmlInline(html) if html.trim_start().starts_with("<!--") => gone.push(node),
            _ => {}
        }
    }
    for node in gone {
        node.detach();
    }
    let first = root
        .children()
        .find(|n| !matches!(n.data().value, NodeValue::FrontMatter(_)));
    if let Some(first) = first
        && matches!(first.data().value, NodeValue::Heading(h) if h.level == 1)
    {
        title = render::plain_text(first).trim().to_string();
        first.detach();
    }
    let mut out = String::new();
    if format_commonmark(root, &options, &mut out).is_err() {
        return Article::Unreadable("couldn't read the Markdown".into());
    }
    let md = out.trim().to_string();
    if md.is_empty() {
        return Article::Unreadable("the page is empty".into());
    }
    let words = md.split_whitespace().count();
    Article::Text { md, words, title }
}

/// A link on the page at `page`, as a whole address. Anchors and links
/// with a scheme (`mailto:`) are left as they are.
fn resolve(page: &str, url: &str) -> String {
    let scheme = url.find(':').is_some_and(|i| {
        i > 1
            && url[..i]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    });
    if url.is_empty() || url.starts_with('#') || scheme {
        return url.to_string();
    }
    absolute(page, url).unwrap_or_else(|| url.to_string())
}

/// A plain-text page: paragraphs of prose flow, as they would in a book;
/// anything laid out (a table, a diagram, an indented list) keeps its
/// lines.
pub fn text(text: &str) -> Article {
    let text = text.replace("\r\n", "\n").replace(['\r', '\u{c}'], "\n");
    let mut md = Vec::new();
    for chunk in text.split("\n\n") {
        let lines: Vec<&str> = chunk
            .split('\n')
            .map(str::trim_end)
            .skip_while(|l| l.is_empty())
            .collect();
        let end = lines
            .iter()
            .rposition(|l| !l.is_empty())
            .map_or(0, |i| i + 1);
        let lines = &lines[..end];
        if lines.is_empty() {
            continue;
        }
        if prose(lines) {
            let joined: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
            md.push(escape(&joined.join(" ")));
        } else {
            md.push(fenced(&lines.join("\n")));
        }
    }
    if md.is_empty() {
        return Article::Unreadable("the page is empty".into());
    }
    let md = md.join("\n\n");
    let words = text.split_whitespace().count();
    Article::Text {
        md,
        words,
        title: String::new(),
    }
}

/// Whether `lines` are a paragraph of prose, wrapped to a width, rather
/// than laid out: they start together, nothing in them is lined up in
/// columns, and only the last falls well short of the longest.
fn prose(lines: &[&str]) -> bool {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let first = indent(lines[0]);
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    // A heading or a line of its own isn't a paragraph to join.
    if lines.len() == 1 {
        return first < 4 && !lines[0].trim_start().contains("   ");
    }
    lines.iter().enumerate().all(|(i, line)| {
        let body = line.trim_start();
        // The first line may be indented more, as a paragraph's often is.
        let aligned = indent(line) == first || (i == 0 && indent(line) <= first + 4);
        let columns = body.contains("   ") || body.contains('|') || body.contains("--+");
        let last = i + 1 == lines.len();
        let full = last || line.chars().count() * 3 >= widest * 2;
        aligned && !columns && full && first < 8
    }) && indent(lines[1]) < 8
}

/// `text` as a fenced block of plain text, with a fence longer than any run
/// of backticks in it.
fn fenced(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{VERBATIM}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_markdown_and_text_from_their_type_and_address() {
        assert_eq!(
            kind("text/markdown; charset=utf-8", "https://x/a", ""),
            Kind::Markdown
        );
        assert_eq!(
            kind("text/plain", "https://raw.x/README.md?raw=1", "# Hi"),
            Kind::Markdown
        );
        assert_eq!(
            kind("text/plain", "https://x/rfc1149.txt", "Hi"),
            Kind::Text
        );
        assert_eq!(
            kind("text/plain", "https://x/a.md", "<!DOCTYPE html><html>"),
            Kind::Html
        );
        assert_eq!(kind("text/html", "https://x/a.md", ""), Kind::Html);
        assert_eq!(kind("", "https://x/", ""), Kind::Html);
    }

    #[test]
    fn markdown_pages_have_whole_links_and_no_markers() {
        let md = "# The Title\n\nSee [docs](docs/a.md), [top](#top), [mail](mailto:a@b.c) \
                  and ![logo](/logo.png).\n\n<!-- comment: 1 0 -->\n\nText <!-- image: 1 1 1 0 --> here.\n";
        let Article::Text { md, title, .. } =
            markdown(md, "https://github.com/a/b/blob/main/README.md")
        else {
            panic!("not text");
        };
        assert_eq!(title, "The Title");
        assert!(!md.contains("The Title"), "{md}");
        assert!(
            md.contains("(https://github.com/a/b/blob/main/docs/a.md)"),
            "{md}"
        );
        assert!(md.contains("(#top)"), "{md}");
        assert!(md.contains("(mailto:a@b.c)"), "{md}");
        assert!(md.contains("(https://github.com/logo.png)"), "{md}");
        assert!(!md.contains("<!--"), "{md}");
    }

    #[test]
    fn prose_flows_and_layout_stays() {
        let text = "Network Working Group                                        D. Waitzman\r\n\
                    Request for Comments: 1149                                       BBN STC\r\n\
                    \r\n\
                    \x20  Avian carriers can provide high delay, low throughput, and low\r\n\
                    \x20  altitude service.  The connection topology is limited to a single\r\n\
                    \x20  point-to-point path for each carrier.\r\n\
                    \r\n\
                    \x20    +------+   +------+\r\n\
                    \x20    | host |---| bird |\r\n\
                    \x20    +------+   +------+\r\n";
        let Article::Text { md, .. } = super::text(text) else {
            panic!("not text");
        };
        let blocks: Vec<&str> = md.split("\n\n").collect();
        assert!(
            blocks[0].starts_with("```verbatim\nNetwork Working Group"),
            "{md}"
        );
        assert!(
            blocks[1].starts_with(
                "Avian carriers can provide high delay, low throughput, and low altitude"
            ),
            "{md}"
        );
        assert!(blocks[2].starts_with("```verbatim\n     +------+"), "{md}");
    }

    #[test]
    fn fences_outlast_the_backticks_inside() {
        assert_eq!(fenced("a ```` b"), "`````verbatim\na ```` b\n`````");
    }
}
