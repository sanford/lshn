mod ansi;
mod article;
mod clipboard;
mod config;
mod doc;
mod fetch;
mod highlight;
mod hn;
mod html;
mod omarchy;
mod open;
mod palettes;
mod render;
mod safe;
mod story;
mod theme;
mod tui;
mod wrap;

use clap::Parser;
use hn::Feed;
use std::io::{self, ErrorKind, IsTerminal, Write};
use clap::ValueEnum;
use std::process::ExitCode;
use theme::{Choice, Mode, Theme};

/// Read Hacker News in the terminal.
///
/// Stories on the left; on the right, the selected one's article and its
/// comments. When output isn't a terminal, prints the list.
///
/// Defaults for the options can go in ~/.lshn/config.toml, e.g. `theme =
/// "dark"`, `width = 100`, `feed = "best"`, `mouse = false`.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The list to start with (top, new, best, ask, show or jobs), or a
    /// story's id to print it
    #[arg(value_name = "LIST|ID")]
    what: Option<String>,

    /// Wrap text at N columns instead of the terminal's width
    #[arg(short, long, value_name = "N")]
    width: Option<usize>,

    /// Don't use the mouse, so the terminal's own text selection works
    #[arg(long)]
    no_mouse: bool,

    /// Print without colors or styles
    #[arg(short, long)]
    plain: bool,

    /// Color theme: auto, dark, light, or one of Omarchy's themes, like
    /// tokyo-night, which colors everything
    #[arg(long)]
    theme: Option<Choice>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lshn: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> io::Result<()> {
    // Flags win over the config file.
    let config = config::load();
    let (feed, id) = match args.what.as_deref() {
        None => (None, None),
        Some(what) => match what.parse::<u64>() {
            Ok(id) => (None, Some(id)),
            Err(_) => (
                Some(Feed::from_str(what, true).map_err(|_| {
                    io::Error::other(format!(
                        "{what}: not a list (top, new, best, ask, show, jobs) or a story's id"
                    ))
                })?),
                None,
            ),
        },
    };
    let feed = feed.or(config.feed).unwrap_or(Feed::Top);
    let interactive = io::stdout().is_terminal() && !args.plain;
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let choice = args
        .theme
        .or(config.theme)
        .unwrap_or(Choice::Mode(Mode::Auto));
    let color = !no_color && !args.plain;
    // On Omarchy the desktop's theme wins, unless code is asked to be
    // plain dark or light.
    let palette = (color && !matches!(choice, Choice::Mode(Mode::Dark | Mode::Light)))
        .then(omarchy::palette)
        .flatten();
    let theme = match &palette {
        Some(p) => Theme::new(Mode::Auto, color, Some(p)),
        None => Theme::chosen(choice, color),
    };
    let width = args.width.or(config.width).filter(|&w| w > 0);
    if let Some(id) = id {
        if interactive {
            return Err(io::Error::other(
                "opening a story by id is for printing, for now: lshn ID | less -R",
            ));
        }
        return print_story(id, &theme, width.unwrap_or_else(terminal_width));
    }
    if !interactive {
        return list(feed);
    }
    let settings = tui::Settings {
        max_width: width,
        mouse: !args.no_mouse && config.mouse.unwrap_or(true),
        outline: config.outline.unwrap_or(false),
        choice,
        omarchy: palette.is_some(),
        feed,
    };
    tui::run(theme, settings)
}

/// Prints a feed's first 30 stories, one per line: points, comments,
/// title and link, separated by tabs.
fn list(feed: Feed) -> io::Result<()> {
    let ids = hn::feed(feed).map_err(io::Error::other)?;
    let ids: Vec<u64> = ids.into_iter().take(30).collect();
    // All at once, then in order.
    let stories: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = ids.iter().map(|&id| s.spawn(move || hn::story(id))).collect();
        handles.into_iter().map(|h| h.join().ok()).collect()
    });
    let mut out = io::stdout().lock();
    for story in stories.into_iter().flatten().flatten() {
        let url = story.url.clone().unwrap_or_else(|| story.hn_url());
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            story.score,
            story.descendants,
            safe::printable(&story.title),
            safe::printable(&url)
        )?;
    }
    out.flush()
}

/// Prints a story, its article and its comments, rendered.
fn print_story(id: u64, theme: &Theme, width: usize) -> io::Result<()> {
    let story = hn::story(id).map_err(io::Error::other)?;
    let (article, thread) = std::thread::scope(|s| {
        let article = story.url.as_deref().map(|url| s.spawn(|| article::fetch(url)));
        let thread = hn::thread(id, &story.kids);
        (article.and_then(|a| a.join().ok()), thread)
    });
    let comments = match &thread {
        Ok(c) => story::Comments::Loaded(c),
        Err(e) => story::Comments::Failed(e),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let md = story::markdown(&story, article.as_ref(), comments, false, now);
    let mut lines = render::render(&md, width, theme, None, None).lines;
    for span in lines.iter_mut().flat_map(|l| &mut l.spans) {
        span.style = theme.recolor(span.style);
    }
    ansi::print(&lines, &mut io::stdout().lock())
}

/// Width for printed output: $COLUMNS, else the terminal's, else 80.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&w| w > 0)
        .or_else(|| {
            ratatui::crossterm::terminal::size()
                .ok()
                .map(|(w, _)| w.into())
        })
        .filter(|&w| w > 0)
        .unwrap_or(80)
}
