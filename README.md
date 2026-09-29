# lshn

**A terminal Hacker News reader built for speed: scan the stories, and read each one — the article and its comments — without leaving the list.**

`lshn` shows HN's front page on the left and the selected story on the right: its title, the article it links to (pulled out of the page the way a browser's reader mode does), and then its comments, threaded. Holding `↓` shows each story as fast as your keyboard repeats, because the stories around the one you're on are fetched before you get to them.

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
| `s` | Search all of HN's stories: the matches, best first, become the list |
| `q` `Esc` | Quit |

Reading:

| Key | |
|---|---|
| `↑` `↓` `j` `k` | Scroll two lines (`scroll` in the settings); in the comments, go comment to comment. `J` `K`, a page |
| `Space` `b`, `d` `u`, `g` `G` | Page, half page; top, bottom. On a comment, `Space` folds it |
| `C` `E` | Fold every thread to a line; unfold everything |
| `c` | To the comments, and back to where you were |
| `]` `[` | Next and previous comment, replies included (or heading, in the article) |
| `}` `{` | Next and previous thread: top-level comments only |
| `o` `O` | The outline: the article's headings and each top-level comment |
| `/` `n` `N` | Search the story; next and previous match |
| `f` | Follow a link: type the letters drawn on it |
| `Esc` `←` `h` | Back to where you followed a link from, then to the list. `←` never quits, however many times you press it |
| `Ctrl-J` `Ctrl-K` (or `Ctrl-↓` `Ctrl-↑`, `⇧↓` `⇧↑`, `>` `<`) | Next and previous story: what `↓` and `↑` do in the list |
| `\` | Keep the list on screen while reading |

Anywhere:

| Key | |
|---|---|
| `w` `W` | Open the story's link, or its HN page, in the browser |
| `y` `Y` | Copy the story's link, or its HN page |
| `v` | Upvote: in the list, the selected story; reading, the comment being read, or above the comments, the story |
| `r` | Reply to the same: write it in your editor, see it, then post it |
| `L` | Log in to HN, or out |
| `R` | Reload |
| `t` | Pick a color theme |
| `?` | All of the above |

Emacs keys work too: `Ctrl-N` `Ctrl-P`, `Ctrl-V` `Alt-V`, `Alt-<` `Alt->`, `Ctrl-G`, and `Ctrl-S` `Ctrl-R` to search.

### Following links

Every author's name is a link: follow it (`f`, or click) for their page — karma, when they joined, what they say about themselves, and what they've posted lately, with `]` `[` going from post to post. Links to stories on HN open here too, rather than in the browser, and a story's own "comments" link goes to its comments. `Esc` goes back the way you came, each page where you left it. Other links open in the browser, after you say yes.

### Voting and replying

`v` and `r` act as you on HN, so the first time, `L` (or `v` or `r` themselves) asks you to log in. lshn keeps HN's session, never your password: in the system's keyring (Keychain on macOS, Credential Manager on Windows, the Secret Service on Linux) or, where there isn't one, in `~/.lshn/session`, encrypted with a passphrase you choose, and asked for once a run.

In the comments, `j` and `k` (or `↓` `↑`) go from comment to comment, and the page only scrolls as far as it takes to show the next one whole; one taller than the screen is read down a line at a time first. The selected comment has a band behind it and its bars in the accent color, with `r reply · v upvote · space fold` beside its author, and the footer says who `r` would answer. `Space` folds it and its replies to one line (`▸ 12 more`) and back; `C` folds every thread, `E` unfolds everything. `]` and `[` also go comment to comment, and `}` and `{` thread to thread. `v` and `r` act on the selected comment, or above the comments, on the story. A reply is written in `$VISUAL` or `$EDITOR`, with what you're replying to quoted below a line; save and quit, and it's shown to you to post (`y`), edit again (`e`), or keep for later (`n`). Drafts stay in `~/.lshn/drafts/` until they're posted, so nothing is lost if HN says no. When it does — you're posting too fast, say — lshn tells you what it said, and never tries again by itself.

### What it remembers

Stories you've opened fade in the list, and when they've had comments since, say how many: `+12`. Open one again and its new comments are marked `new`, and the comments' heading counts them. They stay marked while you read, and aren't new any more once you move on.

What it fetches is kept in `~/.lshn/cache/` for a week, so the last lists, stories and comments show the moment it starts, and are replaced as fresh ones arrive, without losing your place. Without a connection, what's saved is still there to read. Articles are fetched once. What you've read is in `~/.lshn/seen.json`, for 90 days.

### Where it comes from

Story lists and stories come from HN's [official API](https://github.com/HackerNews/API). Each story's comments come from [Algolia's HN API](https://hn.algolia.com/api) in one request, however many there are, with the top-level comments put in HN's order. Articles are fetched from their sites and reduced to their text with [dom_smoothie](https://github.com/niklak/dom_smoothie), a port of Firefox's Readability. Pages that are really apps, videos or PDFs say so, and `w` opens them in the browser.

Comments follow HN's convention for quoting: a paragraph starting with `>` is shown in italics, so a reply reads as what it answers and then the answer.

An article's first picture is shown at the top of it, in the preview and read in full. Terminals that can draw pictures (iTerm2, Kitty, WezTerm, Ghostty, and those with Sixel) show the picture itself; others, and tmux, get a rougher version drawn in colored half blocks. `images = false` turns them off.

Everything a story or comment says is treated as text: none of it can become formatting, or reach the terminal as a control sequence.

### Settings

`~/.lshn/config.toml`, all optional:

```toml
theme = "tokyo-night"   # hn (the default: HN's orange), auto, dark, light, or one of Omarchy's themes
width = 100             # wrap text at 100 columns
feed = "best"           # the list to start with
mouse = false           # leave the mouse to the terminal
outline = true          # show the outline beside stories
scroll = 1              # lines j and k scroll (default 2)
images = false          # don't show articles' first pictures
mute = ["example.com", "crypto"]  # hide stories from these sites (and their subdomains),
                                  # or with these words or phrases in their titles
```

## Building

```sh
cargo build --release
./target/release/lshn
```

While hacking on it, `./run.sh [ARGS]` (or `.\run.ps1` on Windows) builds, installs to `~/.local/bin`, and runs in one step.
