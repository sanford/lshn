//! Opening links outside lshn: web pages in the browser, and email.
//!
//! Links come from documents, which may be someone else's, so this is
//! careful about what it hands to the system: the system's opener will run
//! programs and trigger any app's URL handler if asked to.

use std::process::{Command, Stdio};

/// URL schemes that are opened. Others (`file:`, `smb:`, `ssh:`, apps'
/// own schemes) can do more than show a page, so they're refused.
const SCHEMES: &[&str] = &["http", "https", "mailto"];

/// Something that may be opened, and how to describe it when asking.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub target: String,
    /// "example.com in your browser", "an email to me@x.io"
    pub what: String,
}

/// Checks a web link. Returns what to ask about, or why it won't be opened.
pub fn web(url: &str) -> Result<Target, String> {
    if url
        .chars()
        .any(|c| crate::safe::is_unsafe(c) || c.is_whitespace())
    {
        return Err("Not opening a link with hidden characters in it".into());
    }
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    let scheme = scheme.filter(|s| SCHEMES.contains(&s.as_str()));
    let Some(scheme) = scheme else {
        return Err(format!(
            "Not opening {}: lshn only opens web pages and email links",
            shorten(url)
        ));
    };
    let what = if scheme == "mailto" {
        let to = url["mailto:".len()..].split('?').next().unwrap_or("");
        format!("an email to {to}")
    } else {
        format!("{} in your browser", host(url).unwrap_or("this page"))
    };
    Ok(Target {
        target: url.to_string(),
        what,
    })
}

/// The host of a URL, without any `user@` in front of it (which is how
/// `https://yourbank.com@evil.example` hides where it goes).
fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host.split(':').next()?,
    };
    (!host.is_empty()).then_some(host)
}

fn shorten(s: &str) -> String {
    let mut out: String = s.chars().take(60).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// Opens a checked target with the system's default app. On Windows this
/// avoids `cmd /c start`, which would run anything after an `&` in a URL.
pub fn open(t: &Target) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        Command::new("xdg-open")
    };
    cmd.arg(&t.target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_web_pages_and_names_the_host() {
        assert_eq!(
            web("https://example.com/a?b").unwrap().what,
            "example.com in your browser"
        );
        assert_eq!(
            web("https://yourbank.com@evil.example/x").unwrap().what,
            "evil.example in your browser"
        );
        assert_eq!(
            web("http://[::1]:8080/").unwrap().what,
            "::1 in your browser"
        );
        assert_eq!(
            web("mailto:me@x.io?subject=hi").unwrap().what,
            "an email to me@x.io"
        );
    }

    #[test]
    fn refuses_other_schemes_and_hidden_characters() {
        for url in [
            "file:///Applications/Calculator.app",
            "smb://server/share",
            "ssh://host",
            "vscode://file/x",
            "ms-msdt:/id",
            "javascript:alert(1)",
            "https://x.io/\u{1b}]0;x",
            "https://x.io/a\u{202e}b",
            "https://x.io/a b",
        ] {
            assert!(web(url).is_err(), "{url}");
        }
    }
}
