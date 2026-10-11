# kvidre

*Kvidre* is Danish for "to chirp". It's a tiny X/Twitter client for reading and posting a little, using the official X API v2.

## TUI

Run `kvidre` with no arguments (or `kvidre tui`) to open the terminal UI. It has Home and Mentions tabs, and you can open tabs for other users.

| Key | Action |
|---|---|
| `j`/`k`, `↓`/`↑` | move between posts |
| `tab`, `1`–`9` | switch feed |
| `c` / `r` | write a post / reply to the selected post |
| `o`, `Enter` | open the post in your browser |
| `d` | delete your own post (asks first) |
| `u` / `x` | open a user's posts / close their tab |
| `R` / `m` | reload / load older posts |
| `?` / `q` | help / quit |

Every read is paid, so a feed only loads when you first open it and when you press `R` or `m`. Nothing refreshes in the background. The status bar shows an estimate of what the session has cost so far.

## CLI

```
kvidre timeline -n 10      # home timeline, newest first   (alias: t)
kvidre mentions            # posts mentioning you          (alias: m)
kvidre user jack -n 5      # someone's recent posts        (alias: u)
kvidre post "hello world"  # publish                       (alias: p)
echo "from stdin" | kvidre post
kvidre reply https://x.com/jack/status/20 "nice"           (alias: r)
kvidre delete <id-or-url>
kvidre whoami / kvidre logout
```

## Setup

1. In the [X developer console](https://console.x.com), create an app.
2. Under **User authentication settings**, choose:
   - **OAuth 2.0**, app type **Native App** (public client, no secret needed)
   - App permissions: **Read and write**
   - Callback URI: `http://127.0.0.1:8723/callback`
   - Website URL, if asked: anything (e.g. your homepage)
3. Copy the **OAuth 2.0 Client ID** (not the API key).
4. Buy a few dollars of credits and set a **spending limit**. The API is pay-per-use.
5. Install and log in:

```sh
cargo install --path .
kvidre login --client-id YOUR_CLIENT_ID
```

`kvidre` keeps the client ID in `~/.config/kvidre/config.json`. It keeps tokens in `~/.config/kvidre/session.json` with mode 0600, and refreshes them automatically.

## Cost

X charges per request. Reported rates are about $0.005 per post read and $0.015 per post created, and a post with a URL costs much more (around $0.20). Check the developer console for current prices.

After each read, `kvidre` prints an estimate to stderr. Commands that read posts default to small counts. Mentions and user posts can't go below 5, because that's X's minimum.
