# Privacy

lshn has no telemetry, analytics or crash reporting, and sends nothing to its author. It has no server of its own. It only makes requests to the sites below, and only to show you Hacker News or to act on it when you ask.

## What it connects to

- **Hacker News** (`news.ycombinator.com` and HN's API at `hacker-news.firebaseio.com`), for the story lists, stories, comments and profiles. If you log in, your username and password go to `news.ycombinator.com/login` and nowhere else, along with your votes and comments when you vote or reply.
- **Algolia's HN search** (`hn.algolia.com`), which HN uses for its own search. lshn sends it what you type into search, and the story and user IDs it needs to fetch comment threads and people's latest posts.
- **The sites stories link to**, to show the article and its pictures. lshn fetches the articles for the stories around the one you're on before you reach them, so a site can get a request for a story you scroll past without opening.
- **Your browser**, when you open a link in it. From then on the browser's own privacy settings apply.

Every request identifies itself as `lshn/<version>`. As with any web request, each site sees your IP address and handles it under its own privacy policy: [Y Combinator's](https://www.ycombinator.com/legal/#privacy), [Algolia's](https://www.algolia.com/policies/privacy/) and those of the article sites.

## What it keeps on your computer

All of it is in `~/.lshn/`, on your computer only:

- `config.toml`: your settings.
- `seen.json`, `saved.json`, `hidden.json`, `replies-seen.json`: which stories you've read (forgotten after 90 days), saved and hidden, and the newest reply to you you've seen.
- `user`: your HN username, if you've logged in. It's public on HN anyway.
- `cache/`: what lshn fetched, so it starts instantly and doesn't fetch articles twice. Anything older than 7 days is deleted at startup.

Your password is never stored. Once you log in, lshn keeps HN's session cookie so you stay logged in. It goes in your system's keyring (Keychain on macOS, Credential Manager on Windows, Secret Service on Linux), or, if you don't have one, in `~/.lshn/session`, encrypted with a passphrase you choose. Logging out deletes it.

To remove everything, delete `~/.lshn/`, and the `lshn` entry from your keyring if you logged in.

## Questions

Open an [issue](https://github.com/sanford/lshn/issues).
