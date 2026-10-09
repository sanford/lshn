mod ansi;
mod article;
mod auth;
mod clipboard;
mod config;
mod doc;
mod editor;
mod fetch;
mod figure;
mod guess;
mod highlight;
mod hn;
mod html;
mod omarchy;
mod open;
mod palettes;
mod plain;
mod render;
mod safe;
mod session;
mod sites;
mod sizing;
mod store;
mod story;
mod tex;
mod theme;
mod tui;
mod wrap;

use clap::{CommandFactory, Parser, ValueEnum};
use hn::Feed;
use std::io::{self, ErrorKind, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use theme::{Choice, Mode, Theme};

/// Read Hacker News in the terminal.
///
/// Stories on the left; on the right, the selected one's article and its
/// comments. When output isn't a terminal, prints the list.
///
/// Defaults for the options can go in ~/.lshn/config.toml, e.g. `theme =
/// "dark"`, `width = 100`, `feed = "best"`, `mouse = false`: `lshn
/// --edit-config` opens it with every setting listed.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The list to start with (top, new, best, ask, show or jobs), or a
    /// story or comment to open: its id, or its link on HN (someone's page
    /// too). Or any other page, to read as an article. With output that
    /// isn't a terminal, a story or page is printed
    #[arg(value_name = "LIST|ID|LINK")]
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

    /// Print the story (its article and comments) or the page as
    /// Markdown, to keep: `lshn 12345 --markdown > story.md`
    #[arg(long, requires = "what")]
    markdown: bool,

    /// Read settings from FILE instead of ~/.lshn/config.toml
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Open the config file in $VISUAL or $EDITOR, starting one with every
    /// setting in it if there isn't one
    #[arg(long, conflicts_with_all = ["what", "completions", "man"])]
    edit_config: bool,

    /// Print the script that completes lshn's options in SHELL
    #[arg(long, value_name = "SHELL", exclusive = true)]
    completions: Option<clap_complete::Shell>,

    /// Print the man page
    #[arg(long, hide = true, exclusive = true)]
    man: bool,
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
    if let Some(path) = args.config {
        // Named on purpose, so unlike the usual one, it has to be there.
        if !args.edit_config && !path.is_file() {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                format!("{}: no such config file", path.display()),
            ));
        }
        config::choose(path);
    }
    if args.edit_config {
        return edit_config();
    }
    if let Some(shell) = args.completions {
        clap_complete::generate(shell, &mut Args::command(), "lshn", &mut io::stdout());
        return Ok(());
    }
    if args.man {
        return clap_mangen::Man::new(Args::command()).render(&mut io::stdout());
    }
    for e in palettes::errors() {
        eprintln!("lshn: ignoring theme {e}");
    }
    // Flags win over the config file.
    let config = config::load();
    let (feed, open) = match args.what.as_deref() {
        None => (None, None),
        Some(what) => match (Feed::from_str(what, true), hn::parse(what)) {
            (Ok(feed), _) => (Some(feed), None),
            (_, Some(link)) => (None, Some(link)),
            // A link without its `https://`: `example.com/post`.
            _ if what.contains('.') && !what.contains(char::is_whitespace) => {
                match hn::parse(&format!("https://{what}")) {
                    Some(link) => (None, Some(link)),
                    None => return Err(not_a_list(what)),
                }
            }
            _ => return Err(not_a_list(what)),
        },
    };
    let feed = feed.or(config.feed).unwrap_or(Feed::Top);
    let interactive = io::stdout().is_terminal() && !args.plain;
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let choice = args
        .theme
        .or(config.theme)
        // Hacker News's own orange, unless asked otherwise.
        .unwrap_or(Choice::Named(palettes::DEFAULT));
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
    if args.markdown {
        let md = match &open {
            Some(hn::Link::Item(id)) => story_markdown(*id, true)?,
            Some(hn::Link::Web(url)) => page_markdown(url, true),
            _ => {
                return Err(io::Error::other(
                    "--markdown is for a story, a comment's story, or a page",
                ));
            }
        };
        let mut out = io::stdout().lock();
        out.write_all(md.as_bytes())?;
        return out.flush();
    }
    match &open {
        Some(hn::Link::Item(id)) if !interactive => {
            return print_story(*id, &theme, width.unwrap_or_else(terminal_width));
        }
        Some(hn::Link::Web(url)) if !interactive => {
            return print_page(url, &theme, width.unwrap_or_else(terminal_width));
        }
        Some(hn::Link::User(_)) if !interactive => {
            return Err(io::Error::other(
                "someone's page is only for reading here, in a terminal",
            ));
        }
        _ => {}
    }
    if !interactive {
        return list(feed);
    }
    let settings = tui::Settings {
        max_width: width,
        mouse: !args.no_mouse && config.mouse.unwrap_or(true),
        outline: config.outline.unwrap_or(false),
        scroll: config.scroll.unwrap_or(2).max(1),
        images: config.images.unwrap_or(true),
        big_titles: config.big_titles.unwrap_or(true),
        choice,
        omarchy: palette.is_some(),
        feed,
        mute: config.mute,
        open,
        user: config.user.filter(|u| hn::is_username(u)),
    };
    tui::run(theme, settings)
}

/// Opens the config file in the editor, writing the template first if
/// there's no file, then says whether lshn can read what was saved.
fn edit_config() -> io::Result<()> {
    let path = config::path().ok_or_else(|| io::Error::other("no home directory"))?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, config::TEMPLATE)?;
    }
    editor::edit(&path, 1)?;
    // load() says what's wrong, if anything.
    config::load();
    Ok(())
}

/// Prints a feed's first 30 stories, one per line: points, comments,
/// title and link, separated by tabs.
fn list(feed: Feed) -> io::Result<()> {
    let ids = match feed {
        Feed::Saved => {
            store::Marked::load(store::dir().as_deref(), "saved", None, 0).newest_first()
        }
        _ => hn::feed(feed).map_err(io::Error::other)?,
    };
    let ids: Vec<u64> = ids.into_iter().take(30).collect();
    // All at once, then in order.
    let stories: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = ids
            .iter()
            .map(|&id| s.spawn(move || hn::story(id)))
            .collect();
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

/// Prints a story, its article and its comments, rendered: for a
/// comment, the story it's on.
fn print_story(id: u64, theme: &Theme, width: usize) -> io::Result<()> {
    let md = story_markdown(id, false)?;
    print_md(&md, theme, width)
}

/// A story's document, its article and its comments: for a comment, the
/// story it's on. `export`: as Markdown to keep, outside lshn.
fn story_markdown(id: u64, export: bool) -> io::Result<String> {
    let mut story = hn::story(id).map_err(io::Error::other)?;
    if story.title.is_empty() && story.parent.is_some() {
        let id = hn::story_of(id).map_err(io::Error::other)?;
        story = hn::story(id).map_err(io::Error::other)?;
    }
    let id = story.id;
    let (article, thread) = std::thread::scope(|s| {
        let article = story
            .url
            .as_deref()
            .map(|url| s.spawn(|| article::fetch(url)));
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
    let md = story::markdown(
        &story,
        article.as_ref(),
        comments,
        false,
        now,
        story::Marks {
            seen: None,
            folded: &Default::default(),
        },
        &|_| None,
    );
    if !export {
        return Ok(md);
    }
    let mut times = std::collections::HashMap::from([(story.id, story.time)]);
    if let Ok(comments) = &thread {
        story::times(comments, &mut times);
    }
    Ok(story::export(&md, &times))
}

/// Prints a page that isn't on HN, read as its article is.
fn print_page(url: &str, theme: &Theme, width: usize) -> io::Result<()> {
    print_md(&page_markdown(url, false), theme, width)
}

fn page_markdown(url: &str, export: bool) -> String {
    let article = article::fetch(url);
    let md = story::page_markdown(url, Some(&article), &|_| None);
    if export {
        story::export(&md, &Default::default())
    } else {
        md
    }
}

fn print_md(md: &str, theme: &Theme, width: usize) -> io::Result<()> {
    let mut lines = render::render(md, width, theme, None, None).lines;
    for span in lines.iter_mut().flat_map(|l| &mut l.spans) {
        span.style = theme.recolor(span.style);
    }
    ansi::print(&lines, &mut io::stdout().lock())
}

fn not_a_list(what: &str) -> io::Error {
    io::Error::other(format!(
        "{what}: not a list (top, new, best, ask, show, jobs), an id or a link"
    ))
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
