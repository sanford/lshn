//! Settings from `~/.lshn/config.toml`. Everything is optional, and
//! command-line flags win. For example:
//!
//! ```toml
//! theme = "dark"          # auto, dark, light or a theme's name
//! width = 100             # wrap text at 100 columns (0: the terminal's width)
//! mouse = false           # leave the mouse to the terminal
//! outline = true          # show the outline pane beside stories
//! scroll = 1              # lines j and k scroll (default 2)
//! images = false          # don't show articles' first pictures
//! feed = "best"           # the list to start with: top, new, best, ask, show or jobs
//! mute = ["example.com", "crypto"]  # hide stories from these sites, or with these words
//! big-titles = false      # titles at the text's size, even where they can be bigger
//! user = "you"            # whose replies i shows, without logging in
//! ```
//!
//! `lshn --edit-config` opens it in the editor, starting it from
//! [`TEMPLATE`] if there isn't one.

use crate::hn::Feed;
use crate::theme::Choice;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Default, Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    pub theme: Option<Choice>,
    pub width: Option<usize>,
    pub mouse: Option<bool>,
    pub outline: Option<bool>,
    pub scroll: Option<usize>,
    pub images: Option<bool>,
    pub feed: Option<Feed>,
    pub mute: Vec<String>,
    pub big_titles: Option<bool>,
    pub user: Option<String>,
}

/// A new config file: every setting, commented out, at its default.
pub const TEMPLATE: &str = r#"# lshn's settings. Everything is optional, and command-line flags win.
# Take the # off a line to change it.

# auto, dark, light, or a theme's name, like "tokyo-night" (t shows them all).
# Themes of your own go in ~/.lshn/themes/, as NAME.toml in Omarchy's
# colors.toml format.
# theme = "hn"

# Wrap text at this many columns (0: the terminal's width).
# width = 0

# Scroll and click with the mouse. Off, the terminal's own text selection
# works without holding a modifier key.
# mouse = true

# Show the outline beside stories.
# outline = false

# Lines j and k scroll.
# scroll = 2

# Show articles' pictures, where the terminal can.
# images = true

# The list to start with: top, new, best, ask, show or jobs.
# feed = "top"

# Hide stories from these sites (and their subdomains), or with these words
# in their titles.
# mute = ["example.com", "crypto"]

# Draw stories' titles bigger, in terminals that can (Kitty).
# big-titles = true

# Your username on HN, for i to show the replies to you without logging in.
# user = "you"
"#;

/// `~/.lshn`, where the config file and the user's themes go.
pub fn dir() -> Option<PathBuf> {
    crate::store::dir()
}

/// The config file `--config` named, if it did.
static CHOSEN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Uses `path` instead of `~/.lshn/config.toml`, for reading, saving the
/// theme and editing.
pub fn choose(path: PathBuf) {
    let _ = CHOSEN.set(path);
}

pub fn path() -> Option<PathBuf> {
    if let Some(path) = CHOSEN.get() {
        return Some(path.clone());
    }
    Some(dir()?.join("config.toml"))
}

/// Reads the config file. A missing file is fine; a broken one is reported
/// and otherwise ignored, so lshn still starts.
pub fn load() -> Config {
    let Some(path) = path() else {
        return Config::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    parse(&text).unwrap_or_else(|e| {
        eprintln!("lshn: ignoring {}: {e}", path.display());
        Config::default()
    })
}

fn parse(text: &str) -> Result<Config, String> {
    toml::from_str(text).map_err(|e| e.message().to_string())
}

/// Saves the theme to the config file, keeping everything else in it as it
/// was. Returns the file's path.
pub fn save_theme(theme: &str) -> std::io::Result<PathBuf> {
    let path = path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    // Through a symlink (a dotfiles repo, say) to the file itself, so the
    // rename below replaces the file rather than the link.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    write_theme(&path, theme)?;
    Ok(path)
}

fn write_theme(path: &Path, theme: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written beside it and renamed over it, so it's never half-written.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, with_theme(&text, theme))?;
    // With the old file's permissions, not the new one's defaults.
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, path)
}

/// `text` with its `theme =` line set to `theme`, or one added.
fn with_theme(text: &str, theme: &str) -> String {
    let line = format!("theme = \"{theme}\"");
    let is_theme = |l: &str| {
        l.trim_start()
            .strip_prefix("theme")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    let mut out: Vec<String> = Vec::new();
    let mut found = false;
    for l in text.lines() {
        if is_theme(l) {
            if !found {
                out.push(line.clone());
            }
            found = true;
        } else {
            out.push(l.to_string());
        }
    }
    if !found {
        // At the top: the config has no tables, but keys must come before
        // any that it someday has.
        out.insert(0, line);
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_the_theme_keeping_the_rest() {
        assert_eq!(with_theme("", "nord"), "theme = \"nord\"\n");
        assert_eq!(
            with_theme("width = 80 # narrow\n", "nord"),
            "theme = \"nord\"\nwidth = 80 # narrow\n"
        );
        assert_eq!(
            with_theme("# mine\ntheme = \"dark\"   # was\nwidth = 80", "gruvbox"),
            "# mine\ntheme = \"gruvbox\"\nwidth = 80\n"
        );
        assert_eq!(
            with_theme("themes = 1", "nord"),
            "theme = \"nord\"\nthemes = 1\n",
            "only the theme line"
        );
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lshn-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        std::fs::write(&file, "width = 90\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_theme(&file, "nord").unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "theme = \"nord\"\nwidth = 90\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_every_setting() {
        let c = parse(
            "theme = \"light\"\nwidth = 100\nmouse = false\noutline = true\nfeed = \"best\"\nmute = [\"x.com\"]\n",
        )
        .unwrap();
        assert_eq!(c.theme, Some(Choice::Mode(crate::theme::Mode::Light)));
        assert_eq!(c.width, Some(100));
        assert_eq!(c.mouse, Some(false));
        assert_eq!(c.outline, Some(true));
        assert_eq!(c.feed, Some(Feed::Best));
        assert_eq!(c.mute, ["x.com"]);
    }

    #[test]
    fn the_template_is_all_comments_and_every_line_parses() {
        assert!(parse(TEMPLATE).unwrap().theme.is_none());
        let settings: String = TEMPLATE
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| l.contains(" = "))
            .map(|l| format!("{l}\n"))
            .collect();
        let c = parse(&settings).unwrap();
        assert_eq!(c.theme, Some(Choice::Named("hn")));
        assert_eq!(c.big_titles, Some(true));
        assert_eq!(settings.lines().count(), 10, "{settings}");
    }

    #[test]
    fn empty_is_all_defaults() {
        let c = parse("").unwrap();
        assert!(c.theme.is_none() && c.mouse.is_none());
    }

    #[test]
    fn rejects_typos() {
        let e = parse("thme = \"dark\"").unwrap_err();
        assert!(e.contains("thme"), "{e}");
        assert!(parse("theme = \"blue\"").is_err());
    }
}
