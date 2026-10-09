//! What some sites put in their articles that isn't the article: credits
//! for photos, "Listen to this story", the links and references at the end.
//! A reader mode keeps them, being part of the page's text.

use dom_query::{Document, NodeRef};

struct Site {
    /// The site and its subdomains.
    domains: &'static [&'static str],
    /// Elements to take out, as CSS selectors.
    remove: &'static [&'static str],
    /// Headings where the article ends: everything from them on goes.
    stop_at: &'static [&'static str],
    /// Paragraphs to take out, by what's in them.
    drop: &'static [&'static str],
    /// Paragraphs where the article ends, by what's in them.
    end: &'static [&'static str],
}

const NONE: Site = Site {
    domains: &[],
    remove: &[],
    stop_at: &[],
    drop: &[],
    end: &[],
};

const SITES: &[Site] = &[
    Site {
        domains: &["wikipedia.org"],
        remove: &[
            "#siteSub",
            ".mw-editsection",
            "sup.reference",
            ".noprint",
            ".mw-empty-elt",
            ".navbox",
            ".ambox",
            ".metadata",
        ],
        stop_at: &[
            "See also",
            "Notes",
            "References",
            "Footnotes",
            "Citations",
            "Sources",
            "Further reading",
            "External links",
        ],
        ..NONE
    },
    Site {
        // The abstract's page, for papers without their whole text.
        domains: &["arxiv.org"],
        stop_at: &["Submission history"],
        drop: &["View PDF"],
        ..NONE
    },
    Site {
        domains: &["nytimes.com"],
        drop: &[
            "Credit…",
            "This is a developing story. Check back for updates.",
        ],
        ..NONE
    },
    Site {
        domains: &["economist.com"],
        drop: &[
            "Listen to this story",
            "Your browser does not support the ",
            "Listen on the go",
            "Get The Economist app and play articles",
            "Play in app",
            "Enjoy more audio and podcasts on iOS or Android",
        ],
        end: &["This article appeared in the", "For more coverage of "],
        ..NONE
    },
    Site {
        domains: &["bbc.com", "bbc.co.uk"],
        stop_at: &["Related topics", "More on this story"],
        drop: &["(Image credit: ", "Tech Decoded"],
        end: &["You may also be interested in:"],
        ..NONE
    },
    Site {
        domains: &["tomshardware.com"],
        drop: &["(Image credit: "],
        ..NONE
    },
    Site {
        domains: &["cnn.com"],
        drop: &["Credit: "],
        ..NONE
    },
    Site {
        domains: &["arstechnica.com"],
        drop: &[
            "This story originally appeared on ",
            "Credit: ",
            "Listing image for first story",
        ],
        ..NONE
    },
    Site {
        domains: &["macrumors.com"],
        stop_at: &["Top Stories", "Related Stories"],
        ..NONE
    },
    Site {
        domains: &["wired.com", "wired.co.uk"],
        drop: &[
            "Read more: ",
            "Do you use social media regularly? Take our short survey.",
        ],
        stop_at: &["More Great WIRED Stories"],
        ..NONE
    },
    Site {
        domains: &["theguardian.com"],
        drop: &["Photograph:"],
        ..NONE
    },
    Site {
        domains: &["9to5mac.com"],
        drop: &[
            "We use income earning auto affiliate links.",
            "Check out 9to5Mac on YouTube for more Apple news:",
        ],
        stop_at: &["About the Author"],
        ..NONE
    },
    Site {
        domains: &["smithsonianmag.com"],
        stop_at: &["Like this article?"],
        ..NONE
    },
    Site {
        domains: &["cnet.com"],
        drop: &["Read more:", "Stay up-to-date on the latest news"],
        ..NONE
    },
];

/// Paragraphs that are only one of these go, wherever they are.
const ANYWHERE: &[&str] = &[
    "advertisement",
    "skip advertisement",
    "scroll to continue",
    "continue reading below",
    "story continues below advertisement",
];

fn sites(domain: &str) -> impl Iterator<Item = &'static Site> + '_ {
    SITES.iter().filter(move |site| {
        site.domains
            .iter()
            .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
    })
}

/// The page, before it's read: what's known not to be the article, out.
pub fn tidy_page(doc: &Document, domain: &str) {
    for site in sites(domain) {
        for selector in site.remove {
            doc.select(selector).remove();
        }
        if !site.stop_at.is_empty() {
            let headings = doc.select("h1, h2, h3, h4, h5, h6");
            let end = headings.nodes().iter().find(|h| {
                let text = h.text();
                let text = text.trim().trim_end_matches(':');
                site.stop_at.iter().any(|s| s.eq_ignore_ascii_case(text))
            });
            if let Some(end) = end {
                cut_from(end);
            }
        }
    }
}

/// Takes out `heading` and everything after it in the page.
fn cut_from(heading: &NodeRef) {
    // Headings on Wikipedia are in a box of their own, which goes too.
    let start = match heading.parent() {
        Some(p)
            if p.first_element_child().is_some_and(|c| c.id == heading.id)
                && p.element_children().len() <= 2
                && !p.is("body, article, main, section") =>
        {
            p
        }
        _ => *heading,
    };
    // What's after it, then after each box it's in, up to the page's body.
    let mut node = start;
    loop {
        let mut next = node.next_sibling();
        while let Some(sibling) = next {
            next = sibling.next_sibling();
            // Not the page's own details, its title among them, which
            // are often at the end.
            if !sibling.is("script, meta, link") {
                sibling.remove_from_parent();
            }
        }
        match node.parent() {
            Some(parent) if parent.is_element() && !parent.is("body, html") => node = parent,
            _ => break,
        }
    }
    start.remove_from_parent();
}

/// The article's Markdown, without the paragraphs that aren't it.
pub fn tidy_md(md: &str, domain: &str) -> String {
    let sites: Vec<&Site> = sites(domain).collect();
    let mut out = Vec::new();
    for block in md.split("\n\n") {
        let text: String = block.chars().filter(|&c| c != '\\').collect();
        let text = text.trim();
        if ANYWHERE.iter().any(|a| a.eq_ignore_ascii_case(text)) {
            continue;
        }
        if sites.iter().any(|s| s.end.iter().any(|e| text.contains(e))) {
            break;
        }
        if sites
            .iter()
            .any(|s| s.drop.iter().any(|d| text.contains(d)))
        {
            continue;
        }
        out.push(block);
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ends_wikipedia_articles_at_their_references() {
        let doc = Document::from(
            r#"<html><body><div id="siteSub">From Wikipedia, the free encyclopedia</div>
            <div class="mw-content"><p>Text.<sup class="reference">[1]</sup></p>
            <div class="mw-heading"><h2 id="History">History</h2><span class="mw-editsection">[edit]</span></div>
            <p>More.</p>
            <div class="mw-heading"><h2 id="See_also">See also</h2></div>
            <ul><li>Other</li></ul>
            <div class="mw-heading"><h2 id="References">References</h2></div>
            <ol><li>A book</li></ol></div>
            <div id="footer">Privacy policy</div></body></html>"#,
        );
        tidy_page(&doc, "en.wikipedia.org");
        let text = doc.select("body").text();
        let text: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(text.join(" "), "Text. History More.");
    }

    #[test]
    fn drops_what_isnt_the_article() {
        let md = "Listen to this story\\.\n\nThe article\\.\n\nAdvertisement\n\nMore of it\\.\n\nThis article appeared in the Finance section\\.\n\nAfter";
        assert_eq!(
            tidy_md(md, "www.economist.com"),
            "The article\\.\n\nMore of it\\."
        );
        // Only on its own site.
        assert_eq!(
            tidy_md("Listen to this story", "example.com"),
            "Listen to this story"
        );
    }
}
