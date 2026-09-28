# lshn

**A terminal Hacker News reader built for speed: scan the stories, and read each one — the article and its comments — without leaving the list.**

`lshn` shows HN's front page on the left and the selected story on the right: its title, the article it links to (pulled out of the page the way a browser's reader mode does), and then its comments, threaded. Holding `↓` shows each story as fast as your keyboard repeats, because the stories around the one you're on are fetched before you get to them.

It's [`lsmd`](https://github.com/sanford/lsmd)'s reader, pointed at Hacker News: the same keys, search, outline, link hints and color themes.

## Usage

```sh
lshn                 # the front page
lshn best            # ...or new, best, ask, show, jobs
lshn 12345 | less -R # not a terminal: print a story, its article and comments
lshn | head          # not a terminal: list the front page (points, comments, title, link)
```

`-w 100` caps the text width. `-p` prints without colors, as does setting `NO_COLOR`.

### Keys

In the list:

| Key | |
|---|---|
| `↑` `↓` `j` `k` | Move; with `Shift` (`⇧↑` `⇧↓` `K` `J`), a page at a time |
| `Enter` `→` `l` | Read the story full screen |
| `Tab` | Go to the story. On a terminal 130 columns or wider the list stays beside it, and `Tab` goes back and forth between them |
| `c` | Jump the preview to the comments, and back |
| `Space` `b` | Page through the preview |
| `/` | Filter the list by title |
| `1`–`6` | Top, New, Best, Ask, Show, Jobs |
| `q` `Esc` | Quit |

Reading:

| Key | |
|---|---|
| `↑` `↓` `j` `k` | Scroll a line; `J` `K`, a page |
| `Space` `b`, `d` `u`, `g` `G` | Page, half page; top, bottom |
| `c` | To the comments, and back to where you were |
| `]` `[` | Next and previous top-level comment (or heading, in the article) |
| `o` `O` | The outline: the article's headings and each top-level comment |
| `/` `n` `N` | Search the story; next and previous match |
| `f` | Follow a link: type the letters drawn on it |
| `Esc` `←` `h` `Tab` | Back to the list. `←` never quits, however many times you press it |
| `⇧↓` `⇧↑` (or `>` `<`) | Next and previous story: what `↓` and `↑` do in the list |
| `\` | Keep the list on screen while reading |

Anywhere:

| Key | |
|---|---|
| `w` `W` | Open the story's link, or its HN page, in the browser |
| `y` `Y` | Copy the story's link, or its HN page |
| `r` | Reload |
| `t` | Pick a color theme |
| `?` | All of the above |

Emacs keys work too: `Ctrl-N` `Ctrl-P`, `Ctrl-V` `Alt-V`, `Alt-<` `Alt->`, `Ctrl-G`, and `Ctrl-S` `Ctrl-R` to search.

### Where it comes from

Story lists and stories come from HN's [official API](https://github.com/HackerNews/API). Each story's comments come from [Algolia's HN API](https://hn.algolia.com/api) in one request, however many there are, with the top-level comments put in HN's order. Articles are fetched from their sites and reduced to their text with [dom_smoothie](https://github.com/niklak/dom_smoothie), a port of Firefox's Readability. Pages that are really apps, videos or PDFs say so, and `w` opens them in the browser.

Everything a story or comment says is treated as text: none of it can become formatting, or reach the terminal as a control sequence.

### Settings

`~/.lshn/config.toml`, all optional:

```toml
theme = "tokyo-night"   # auto, dark, light, or one of Omarchy's themes
width = 100             # wrap text at 100 columns
feed = "best"           # the list to start with
mouse = false           # leave the mouse to the terminal
outline = true          # show the outline beside stories
```

## Building

```sh
cargo build --release
./target/release/lshn
```

While hacking on it, `./run.sh [ARGS]` (or `.\run.ps1` on Windows) builds, installs to `~/.local/bin`, and runs in one step.
