//! Fetching the page a story links to, and pulling out its text, as
//! Markdown, the way a browser's reader mode does.

use crate::hn;
use crate::plain::{self, Kind};
use crate::sites;
use dom_query::{Document, NodeRef};
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

/// Whose article it is: a story's, from its link, or a page's, followed to
/// from one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Story(u64),
    Page(String),
}

impl Source {
    /// Its name in the cache: the story's id, or a page's address, hashed.
    pub fn key(&self) -> String {
        match self {
            Source::Story(id) => id.to_string(),
            Source::Page(url) => format!("page-{:016x}", fnv(url)),
        }
    }
}

/// FNV-1a: a hash that's the same from run to run, for naming files.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Sites whose pages are players or apps, so there's no reading them here:
/// what they are, if `url` is on one.
pub fn not_article(url: &str) -> Option<&'static str> {
    let domain = hn::domain(url).unwrap_or_default();
    NOT_ARTICLES
        .iter()
        .find(|(site, _)| domain == *site || domain.ends_with(&format!(".{site}")))
        .map(|(_, what)| *what)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Article {
    /// The article's text as Markdown, how many words it has, and its title
    /// (empty if it has none, or was cached before titles were kept).
    Text {
        md: String,
        words: usize,
        #[serde(default)]
        title: String,
    },
    /// Why there's nothing to show: "a PDF", "a video", an error.
    Unreadable(String),
    /// A page with no text to pull out, and what it says about itself
    /// instead, as Markdown: its picture and description, from its tags.
    /// `scripted`: it draws its text with JavaScript, in a browser.
    About { scripted: bool, md: String },
}

impl Article {
    /// The Markdown to show, and look in for pictures.
    pub fn md(&self) -> Option<&str> {
        match self {
            Article::Text { md, .. } | Article::About { md, .. } => Some(md),
            Article::Unreadable(_) => None,
        }
    }
}

pub fn fetch(url: &str) -> Article {
    if let Some(what) = not_article(url) {
        return Article::Unreadable(what.into());
    }
    // A paper's whole text, rather than its abstract or a PDF; not every
    // paper has one.
    if let Some(full) = full_text(url)
        && let Ok((Kind::Html, html)) = fetch_page(&full)
    {
        return extract(&html, &full);
    }
    match fetch_page(url) {
        Ok((Kind::Html, html)) => extract(&html, url),
        Ok((Kind::Markdown, md)) => plain::markdown(&md, url),
        Ok((Kind::Text, text)) => plain::text(&text),
        Err(why) => Article::Unreadable(why),
    }
}

/// Where a paper's whole text is, as a page, for a link to its abstract or
/// its PDF: arXiv makes HTML of most papers from their LaTeX.
fn full_text(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest
        .strip_prefix("arxiv.org/")
        .or_else(|| rest.strip_prefix("export.arxiv.org/"))?;
    let path = path.split(['?', '#']).next()?;
    let id = path
        .strip_prefix("abs/")
        .or_else(|| path.strip_prefix("pdf/"))?
        .trim_end_matches('/');
    let id = id.strip_suffix(".pdf").unwrap_or(id);
    (!id.is_empty()).then(|| format!("https://arxiv.org/html/{id}"))
}

/// The page at `url`, and what it is: HTML, or Markdown or plain text,
/// which are read too.
fn fetch_page(url: &str) -> Result<(Kind, String), String> {
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
    let readable = kind.is_empty()
        || kind.contains("html")
        || ["text/plain", "text/markdown", "text/x-markdown"]
            .iter()
            .any(|k| kind.starts_with(k));
    if !readable {
        return Err(describe(&kind));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_PAGE)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let body = String::from_utf8_lossy(&bytes).into_owned();
    Ok((plain::kind(&kind, url, &body), body))
}

/// What a page that isn't HTML is, from its content type.
fn describe(kind: &str) -> String {
    let kind = kind.split(';').next().unwrap_or("").trim();
    match kind {
        "application/pdf" => "a PDF".into(),
        k if k.starts_with("image/") => "an image".into(),
        k if k.starts_with("video/") => "a video".into(),
        k if k.starts_with("audio/") => "audio".into(),
        k => format!("not a web page ({k})"),
    }
}

pub fn extract(html: &str, url: &str) -> Article {
    let config = Config {
        text_mode: TextMode::Markdown,
        ..Config::default()
    };
    let html = tidy_emphasis(&unhide_streamed(html));
    let doc = Document::from(&*html);
    even_tables(&doc);
    math(&doc, &html);
    let domain = hn::domain(url).unwrap_or_default();
    sites::tidy_page(&doc, &domain);
    let parsed =
        Readability::with_document(doc, Some(url), Some(config)).and_then(|mut r| r.parse());
    let (md, title) = match parsed {
        Ok(article) => (
            section_breaks(&sites::tidy_md(
                &drop_metadata(article.text_content.trim()),
                &domain,
            )),
            article
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        ),
        Err(_) => Default::default(),
    };
    if md.is_empty() {
        return about(&html, url);
    }
    let words = md.split_whitespace().count();
    Article::Text { md, words, title }
}

/// A page with no article on it: what it says about itself, or else that
/// there's nothing.
fn about(html: &str, url: &str) -> Article {
    let doc = Document::from(html);
    let meta = |names: &[&str]| {
        names.iter().find_map(|name| {
            let tags = doc.select(&format!(r#"meta[property="{name}"], meta[name="{name}"]"#));
            let content = tags.attr("content")?.trim().to_string();
            (!content.is_empty()).then_some(content)
        })
    };
    let description = meta(&["og:description", "twitter:description", "description"]);
    let picture = meta(&["og:image", "twitter:image"]).and_then(|src| absolute(url, &src));
    let alt = meta(&["og:image:alt", "twitter:image:alt"]).unwrap_or_default();
    // Drawn by scripts: it has some, and next to no text without them.
    let scripts = doc.select("script").exists();
    doc.select("script, style, noscript, template").remove();
    let words = doc.select("body").text().split_whitespace().count();
    let scripted = scripts && words < 50;
    if !scripted && description.is_none() && picture.is_none() {
        return Article::Unreadable("couldn't find the article on the page".into());
    }
    let mut md = String::new();
    if let Some(src) = picture {
        md.push_str(&format!("![{}](<{src}>)\n\n", crate::story::escape(&alt)));
    }
    if let Some(description) = description {
        md.push_str(&crate::story::escape(&description));
    }
    Article::About { scripted, md }
}

/// `src` as an absolute address, from the page at `page`.
pub(crate) fn absolute(page: &str, src: &str) -> Option<String> {
    if src.starts_with("https://") || src.starts_with("http://") {
        return Some(src.to_string());
    }
    let (scheme, rest) = page.split_once("://")?;
    if let Some(rest) = src.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    let origin = format!("{scheme}://{}", rest.split('/').next()?);
    if src.starts_with('/') {
        return Some(format!("{origin}{src}"));
    }
    // Relative to the page's folder.
    let folder = page.rsplit_once('/').map_or(page, |(folder, _)| folder);
    Some(format!("{folder}/{src}"))
}

/// Tables made plain enough to come out as tables: a row of headings (`th`)
/// at the top, then rows of cells (`td`), every row as wide as the widest,
/// with a cell that spans columns followed by empty ones. Otherwise the
/// Markdown runs each row's text together: all it takes is a totals row of
/// `th` and `td` at the bottom, as Backblaze's drive stats have.
fn even_tables(doc: &Document) {
    for table in doc.select("table").nodes() {
        // A table in a table is for layout: left as it is.
        if table.is("table:has(table)") || table.is("table table") {
            continue;
        }
        let rows = table.find(&["tr"]);
        let headings = rows.first().is_some_and(|row| {
            let cells = cells(row);
            !cells.is_empty() && cells.iter().all(|c| c.is("th"))
        });
        let mut widths = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let heading_row = headings && i == 0;
            let mut width = 0;
            for cell in cells(row) {
                if !heading_row && cell.is("th") {
                    cell.rename("td");
                }
                let span = cell
                    .attr("colspan")
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 100);
                if span > 1 {
                    cell.remove_attr("colspan");
                    let tag = if heading_row { "th" } else { "td" };
                    for _ in 1..span {
                        // Made as elements: `<td>` as HTML is dropped, out
                        // of a table's context.
                        cell.insert_after(&cell.tree.new_element(tag));
                    }
                }
                width += span;
            }
            widths.push(width);
        }
        let widest = widths.iter().copied().max().unwrap_or(0);
        for (i, (row, width)) in rows.iter().zip(widths).enumerate() {
            if width > 0 && width < widest {
                let tag = if headings && i == 0 { "th" } else { "td" };
                for _ in width..widest {
                    row.append_child(&row.tree.new_element(tag));
                }
            }
        }
    }
}

/// A row's cells, headings and all.
fn cells<'a>(row: &NodeRef<'a>) -> Vec<NodeRef<'a>> {
    row.element_children()
        .into_iter()
        .filter(|c| c.is("td, th"))
        .collect()
}

/// Formulas as text. Pages write them in MathML, with the TeX they came
/// from alongside (arXiv's, Wikipedia's, KaTeX's), or in TeX, between
/// dollars, for MathJax to draw in the browser. Either way a reader mode
/// would make a mess of them: MathML's layout read out as text, or TeX.
fn math(doc: &Document, html: &str) {
    let replace = |node: &NodeRef, tex: &str| {
        let text = crate::tex::to_unicode(tex);
        node.replace_with(&node.tree.new_text(text));
    };
    // Wikipedia hides its MathML, showing a picture of it instead: both go.
    for node in doc.select(".mwe-math-element").nodes() {
        let math = node.find(&["math"]);
        let tex = math
            .first()
            .and_then(|m| m.attr("alttext"))
            .or_else(|| node.find(&["img"]).first().and_then(|img| img.attr("alt")));
        if let Some(tex) = tex {
            replace(node, &tex);
        }
    }
    // KaTeX's: the MathML for readers, then its drawing, in HTML.
    for node in doc.select(".katex").nodes() {
        if node.is(".katex .katex") {
            continue;
        }
        let tex = node
            .find(&["annotation"])
            .into_iter()
            .find(|a| a.attr("encoding").is_some_and(|e| e.contains("tex")))
            .map(|a| a.text());
        if let Some(tex) = tex {
            replace(node, &tex);
        }
    }
    // MathJax's, once it's read the page: the TeX in a script.
    for node in doc.select(r#"script[type^="math/tex"]"#).nodes() {
        let tex = node.text();
        replace(node, &tex);
    }
    for node in doc.select("math").nodes() {
        let annotation = node
            .find(&["annotation"])
            .into_iter()
            .find(|a| a.attr("encoding").is_some_and(|e| e.contains("tex")))
            .map(|a| a.text());
        match node.attr("alttext").or(annotation) {
            Some(tex) => replace(node, &tex),
            None => {
                // MathML alone: its text, which reads well enough for
                // something short.
                for a in node.find(&["annotation"]) {
                    a.remove_from_parent();
                }
                let text = node.text().split_whitespace().collect::<Vec<_>>().join(" ");
                node.replace_with(&node.tree.new_text(text));
            }
        }
    }
    // TeX in the text, for MathJax or KaTeX to draw in the browser.
    let lower = html.to_ascii_lowercase();
    if !lower.contains("mathjax") && !lower.contains("katex") {
        return;
    }
    let Some(body) = doc.select("body").nodes().first().cloned() else {
        return;
    };
    for node in body.descendants() {
        if !node.is_text() {
            continue;
        }
        let code = node.ancestors_it(None).any(|a| {
            a.node_name()
                .is_some_and(|n| matches!(&*n, "pre" | "code" | "script" | "style" | "textarea"))
        });
        let text = node.text();
        if code || !text.contains(['$', '\\']) {
            continue;
        }
        let converted = crate::tex::in_text(&text);
        if converted != *text {
            node.set_text(converted);
        }
    }
}

/// React pages that stream (Next.js's, say) send what they render last in
/// a hidden `<div hidden id="S:0">`, for a script to move into place. A
/// reader mode leaves out anything hidden, so there'd be nothing to read.
fn unhide_streamed(html: &str) -> std::borrow::Cow<'_, str> {
    const HIDDEN: &str = "<div hidden id=\"S:";
    if html.contains(HIDDEN) {
        html.replace(HIDDEN, "<div id=\"S:").into()
    } else {
        html.into()
    }
}

/// Spaces just inside italics and bold moved outside: `<i>July. </i>Then`
/// would be `*July. *Then` in Markdown, which isn't italics at all.
fn tidy_emphasis(html: &str) -> String {
    let mut html = html.to_string();
    for tag in ["i", "em", "b", "strong"] {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        for space in [" ", "&nbsp;", "\u{a0}"] {
            let (inside, outside) = (format!("{space}{close}"), format!("{close}{space}"));
            while html.contains(&inside) {
                html = html.replace(&inside, &outside);
            }
            let (inside, outside) = (format!("{open}{space}"), format!("{space}{open}"));
            while html.contains(&inside) {
                html = html.replace(&inside, &outside);
            }
        }
    }
    html
}

/// Paragraphs that are only a section break, as writers type them (`***`,
/// `* * *`, `⁂`), marked as one, to be drawn as one.
fn section_breaks(md: &str) -> String {
    md.split("\n\n")
        .map(|block| {
            let marks: String = block
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '\\')
                .collect();
            let asterisks = marks.len() >= 3 && marks.chars().all(|c| c == '*');
            if asterisks || marks == "⁂" {
                crate::render::SECTION_BREAK
            } else {
                block
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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
        let metadata = !sentence && ((words <= 40 && signals >= 2) || (words <= 8 && signals >= 1));
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
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
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
    fn evens_out_tables_to_come_out_as_tables() {
        let doc = Document::from(
            "<table><thead><tr><th>Model</th><th>Count</th><th>AFR</th></tr></thead>\
             <tbody><tr><td>A1</td><td>12</td><td>1.5</td></tr>\
             <tr><td colspan=2>merged</td><td>2</td></tr>\
             <tr><td>short</td></tr></tbody>\
             <tfoot><tr><th>Totals</th><td></td><th>3.5</th></tr></tfoot></table>",
        );
        even_tables(&doc);
        let md = doc.md(None).to_string();
        assert_eq!(
            md.trim(),
            "| Model | Count | AFR |\n\
             | ----- | ----- | --- |\n\
             | A1 | 12 | 1\\.5 |\n\
             | merged |  | 2 |\n\
             | short |  |  |\n\
             | Totals |  | 3\\.5 |",
        );
    }

    #[test]
    fn extracts_the_article_as_markdown() {
        let para = "This is a sentence of reasonable length for an article. ".repeat(12);
        let html = format!(
            "<html><head><title>T</title></head><body><nav>Home | About</nav>\
             <article><h1>Title</h1><p>{para}</p><h2>Part two</h2><p>{para}</p></article>\
             <footer>© 2026</footer></body></html>"
        );
        let Article::Text { md, words, .. } = extract(&html, "https://example.com/post") else {
            panic!("no text");
        };
        assert!(md.contains("Part two"), "{md}");
        assert!(md.contains("reasonable length"));
        assert!(!md.contains("Home | About"), "{md}");
        assert!(words > 100);
    }

    #[test]
    fn spaces_inside_emphasis_move_out() {
        assert_eq!(
            tidy_emphasis("<p><i>July 2019.  </i>Then <b> bold</b> and <em>fine</em></p>"),
            "<p><i>July 2019.</i>  Then  <b>bold</b> and <em>fine</em></p>"
        );
    }

    #[test]
    fn section_breaks_are_marked() {
        let md = "One.\n\n\\*\\*\\*\n\nTwo.\n\n\\* \\* \\*\n\n⁂\n\nThree *and* four.\n\n\\*\\*";
        let brk = crate::render::SECTION_BREAK;
        assert_eq!(
            section_breaks(md),
            format!("One.\n\n{brk}\n\nTwo.\n\n{brk}\n\n{brk}\n\nThree *and* four.\n\n\\*\\*")
        );
    }

    /// As MUBI's Notebook sends it: the article streamed in hidden, and a
    /// script to move it where the placeholder is.
    #[test]
    fn reads_what_react_streamed_in_hidden() {
        let para = "This is a sentence of reasonable length for an article. ".repeat(12);
        let html = format!(
            "<html><head><title>T</title></head><body><div><template id=\"B:0\"></template></div>\
             <div hidden id=\"S:0\"><div class=\"post-body\"><p>{para}</p><p>{para}</p></div></div>\
             <script>$RC(\"B:0\",\"S:0\")</script></body></html>"
        );
        let Article::Text { md, .. } = extract(&html, "https://example.com/post") else {
            panic!("no text");
        };
        assert!(md.contains("reasonable length"), "{md}");
    }

    #[test]
    fn drops_what_pages_say_about_themselves_at_the_start() {
        let body =
            "If we think writing code is dead, the bigger problem is teams not knowing the system.";
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
    fn says_what_a_page_without_an_article_is_about() {
        let url = "https://example.com/blog/post";
        // An app's shell: its text comes from its scripts.
        let shell = r#"<html><head><title>Post</title>
            <meta property="og:description" content="A tale & more">
            <meta property="og:image" content="/blog/post/cover.jpg">
            <meta property="og:image:alt" content="The cover">
            <script type="module" src="/assets/app.js"></script></head>
            <body><div id="root"></div></body></html>"#;
        match extract(shell, url) {
            Article::About { scripted: true, md } => {
                assert!(
                    md.contains("(<https://example.com/blog/post/cover.jpg>)"),
                    "{md}"
                );
                assert!(md.contains("A tale \\& more"), "{md}");
            }
            other => panic!("{other:?}"),
        }
        // No scripts, just tags: said, but not blamed on scripts.
        let tags = r#"<html><head><meta name="description" content="About it"></head><body><p>Hi.</p></body></html>"#;
        assert!(matches!(
            extract(tags, url),
            Article::About {
                scripted: false,
                ..
            } | Article::Text { .. }
        ));
        // Nothing at all.
        let empty = "<html><body></body></html>";
        assert!(matches!(extract(empty, url), Article::Unreadable(_)));
        assert_eq!(
            absolute(url, "//cdn.x/a.png").as_deref(),
            Some("https://cdn.x/a.png")
        );
        assert_eq!(
            absolute(url, "c.jpg").as_deref(),
            Some("https://example.com/blog/c.jpg")
        );
    }

    #[test]
    fn reads_papers_whole() {
        for url in [
            "https://arxiv.org/abs/2401.12345",
            "https://arxiv.org/abs/2401.12345v2",
            "http://www.arxiv.org/pdf/2401.12345.pdf",
            "https://export.arxiv.org/pdf/2401.12345",
        ] {
            let id = if url.contains("v2") {
                "2401.12345v2"
            } else {
                "2401.12345"
            };
            assert_eq!(
                full_text(url),
                Some(format!("https://arxiv.org/html/{id}")),
                "{url}"
            );
        }
        assert_eq!(full_text("https://arxiv.org/list/cs.AI/recent"), None);
        assert_eq!(full_text("https://example.com/abs/1"), None);
    }

    #[test]
    fn writes_formulas_as_text() {
        let para = "This is a sentence of reasonable length for an article. ".repeat(12);
        let html = format!(
            r#"<html><head><script src="mathjax.js"></script></head><body><article>
            <p>{para}</p>
            <p>LaTeXML: <math alttext="x^{{2}}" display="inline"><msup><mi>x</mi><mn>2</mn></msup></math> ends.</p>
            <p>KaTeX: <span class="katex"><span class="katex-mathml"><math><semantics><mrow><mi>α</mi></mrow>
              <annotation encoding="application/x-tex">\alpha</annotation></semantics></math></span>
              <span class="katex-html" aria-hidden="true">α</span></span> ends.</p>
            <p>Plain: <math><mi>y</mi><mo>+</mo><mn>1</mn></math> ends.</p>
            <p>MathJax: \(\beta \leq 1\) and $5 and <code>$x^2$</code>.</p>
            <p>{para}</p></article></body></html>"#
        );
        let Article::Text { md, .. } = extract(&html, "https://example.com/paper") else {
            panic!("no text");
        };
        assert!(md.contains("LaTeXML: x² ends"), "{md}");
        assert!(md.contains("KaTeX: α ends"), "{md}");
        assert!(md.contains("Plain: y\\+1 ends"), "{md}");
        assert!(md.contains("MathJax: β ≤ 1 and $5"), "{md}");
        assert!(md.contains("`$x^2$`"), "{md}");
    }

    #[test]
    fn skips_video_sites_without_fetching() {
        assert!(matches!(
            fetch("https://www.youtube.com/watch?v=x"),
            Article::Unreadable(w) if w == "a video"
        ));
    }
}
