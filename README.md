# kvidre

*Kvidre* is Danish for "to chirp". It's a small terminal client for X (Twitter). Read your timeline and mentions, then post and reply, from a full-screen TUI or from plain commands you can script. It uses the official X API v2.

```
 1 Home │ 2 Mentions │ 3 @robin                                        @you
▌ @sparrow Sparrow · today 08:31 · ⟲ @robin
▌ Spring is here. The first chirp of the year is always the loudest one.
▌
  @wren Wren · today 08:12
  Shipped a new release this morning. Changelog in the thread below.

  @starling Starling · yesterday 21:40
  Anyone else reading the whole timeline from a terminal now?

c post · r reply · o open · R reload · m more · ? help     ≈$0.050 this session
```

## Features

- **TUI** with Home, Mentions and per-user tabs. Reposts show the full original post, long posts aren't cut off, and times are in your local time zone.
- **Compose, reply, open in browser and delete** your own posts without leaving the terminal.
- **Scriptable CLI.** For example `kvidre post "hello"`, `echo "text" | kvidre post`, or `kvidre timeline -n 5`.
- **Keeps API costs down.** Nothing loads in the background, and you see a running estimate of what the session has cost.
- **Uses your own credentials.** Tokens stay on your machine in a file only you can read. `kvidre` talks to X and nothing else.

## Before you start: you need your own X API app

X's API is **pay-per-use**: X bills the developer account behind the app for every post read and every post created. `kvidre` doesn't come with shared credentials, because then everyone's usage would land on one person's bill. You create your own (free) developer app and buy a few dollars of API credits from X.

For light personal use the cost is small. X's rates are roughly:

| What you do | Approximate cost |
|---|---|
| Load 10 posts from a feed | $0.05 |
| Publish a post or reply | $0.015 |
| Publish a post containing a link | $0.20 |
| Open a user's tab | $0.01 for the profile lookup, plus the posts it loads |

These rates come from third-party reports. X sets and changes its own prices, so **check the current rates in the [X developer console](https://console.x.com)** before relying on these numbers.

## Install

You need:
- **Linux or macOS.** Windows isn't supported yet.
- **Rust 1.89 or newer.** Install it with [rustup](https://rustup.rs) if you don't have it, and update with `rustup update`.

```sh
cargo install --git https://github.com/theill/kvidre
```

Or build from a clone:

```sh
git clone https://github.com/theill/kvidre
cd kvidre
cargo install --path .
```

Either way, `kvidre` is installed into `~/.cargo/bin`, which rustup adds to your `PATH`.

## Set up your X app (one time, about 10 minutes)

1. **Sign in** at [console.x.com](https://console.x.com) with the X account you want to use. Accept the Developer Agreement and fill in the short use-case profile.
2. **Add credits.** Buy a small amount of API credits ($5 lasts a long time for light use) and **set a spending limit**, so a mistake can never run up a large bill.
3. **Create a Project, then create an App inside it.** The app must belong to a project. If it doesn't, every call fails with `403 client-not-enrolled`.
4. **Configure user authentication** in the app's settings:

   | Setting | Value |
   |---|---|
   | OAuth 2.0 | On |
   | App type | **Native App** (no client secret needed) |
   | App permissions | **Read and write** |
   | Callback URI | `http://127.0.0.1:8723/callback`, exactly as written. X doesn't accept `localhost` here. |
   | Website URL | Anything, for example your homepage |

5. **Copy the OAuth 2.0 Client ID.** It's a different value from the "API Key". You won't need the other keys the console shows you.

## Log in

```sh
kvidre login --client-id YOUR_CLIENT_ID
```

Your browser opens X's authorization page. Click **Authorize**, and the terminal prints `Logged in as @you`. You only do this once: `kvidre` saves your client ID and renews your login automatically.

> **Logging in on a remote machine over SSH?** The browser has to reach port 8723 on the machine running `kvidre`. Connect with `ssh -L 8723:127.0.0.1:8723 yourserver`, run `kvidre login`, and open the printed URL in your local browser.

## Using the TUI

Run `kvidre` with no arguments (or `kvidre tui`). Home loads when the TUI starts. The other tabs load the first time you open them, and after that only when you ask.

| Key | Action |
|---|---|
| `j` / `k`, `↓` / `↑` | Move between posts |
| `g` / `G` | First / last post |
| `PgDn` / `PgUp` | Jump 5 posts |
| `tab`, `1`–`9` | Switch tab |
| `R` | Reload the current tab (paid read) |
| `m` | Load older posts (paid read) |
| `c` | Write a new post. `Enter` sends, `Alt+Enter` adds a new line, `Esc` cancels. |
| `r` | Reply to the selected post |
| `o`, `Enter` | Open the selected post in your browser |
| `d` | Delete one of your own posts (asks for confirmation) |
| `u` / `x` | Open a user's posts in a new tab / close that tab |
| `?` | Show all keys |
| `q`, `Ctrl+C` | Quit |

The compose box counts characters and warns you before you send a post containing a link, since those cost more. If a send fails, your draft stays open so you don't lose anything.

## Using the CLI

Every TUI action is also a command, which is handy for scripts and quick posts:

```sh
kvidre timeline -n 10        # your home timeline, newest first   (alias: t)
kvidre mentions              # posts mentioning you               (alias: m)
kvidre user robin -n 5       # someone's recent posts             (alias: u)
kvidre post "hello world"    # publish a post                     (alias: p)
echo "from a script" | kvidre post
kvidre reply https://x.com/robin/status/123 "nice!"               (alias: r)
kvidre delete https://x.com/you/status/456
kvidre whoami
kvidre logout
```

`reply` and `delete` take either a post ID or the post's full URL. After each read, the CLI prints how many posts it fetched and the estimated cost to stderr, so piping the output stays clean.

## Where your data lives

| File | Linux | macOS |
|---|---|---|
| Client ID | `~/.config/kvidre/config.json` | `~/Library/Application Support/kvidre/config.json` |
| Login tokens | `~/.config/kvidre/session.json` | `~/Library/Application Support/kvidre/session.json` |

Both files are readable only by you (mode `0600`). `kvidre logout` deletes your login tokens. To remove everything:

```sh
cargo uninstall kvidre
rm -rf ~/.config/kvidre                              # Linux
rm -rf ~/Library/Application\ Support/kvidre         # macOS
```

You can also revoke `kvidre`'s access to your account at any time in X under **Settings and privacy → Security and account access → Apps and sessions → Connected apps**.

## Troubleshooting

| You see | What it means and how to fix it |
|---|---|
| X's authorize page shows a redirect or callback error | The callback URI in the console doesn't exactly match `http://127.0.0.1:8723/callback`. Check for typos, `localhost`, or a trailing `/`. |
| `403` with `client-not-enrolled` | Your app isn't attached to a Project. Attach it in the console, then run `kvidre login` again. |
| `403` when posting | The app's permissions are Read only. Switch to **Read and write**, then run `kvidre login` again. |
| `401` / `unauthorized_client` at login | The app type is a confidential client (Web App or Bot). Switch it to **Native App**, or add `--client-secret YOUR_SECRET` to the login command. |
| `402` | You're out of API credits. Top up in the developer console. |
| `429` | Rate limited by X. Wait a few minutes. |
| `could not listen on 127.0.0.1:8723` | Another program is using port 8723. Close it and try again. |
| `not logged in` | Run `kvidre login`. |

## License

MIT. See [LICENSE](LICENSE).
