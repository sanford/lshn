mod ansi;
mod article;
mod auth;
mod clipboard;
mod config;
mod doc;
mod editor;
mod fetch;
mod figure;
mod highlight;
mod hn;
mod html;
mod omarchy;
mod open;
mod palettes;
mod render;
mod safe;
mod session;
mod sizing;
mod store;
mod story;
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
        scroll: config.scroll.unwrap_or(2).max(1),
        images: config.images.unwrap_or(true),
        big_titles: config.big_titles.unwrap_or(true),
        choice,
        omarchy: palette.is_some(),
        feed,
        mute: config.mute,
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
    let md = story::markdown(&story, article.as_ref(), comments, false, now, story::Marks { seen: None, folded: &Default::default() }, &|_| None);
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
