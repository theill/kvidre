# tw

A tiny X/Twitter CLI for reading and posting a little, using the official X API v2.

```
tw timeline -n 10        # home timeline, newest first   (alias: t)
tw mentions              # posts mentioning you          (alias: m)
tw user jack -n 5        # someone's recent posts        (alias: u)
tw post "hello world"    # publish                       (alias: p)
echo "from stdin" | tw post
tw reply https://x.com/jack/status/20 "nice"            (alias: r)
tw delete <id-or-url>
tw whoami / tw logout
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
tw login --client-id YOUR_CLIENT_ID
```

`tw` keeps the client ID in `~/.config/tw/config.json`. It keeps tokens in `~/.config/tw/session.json` with mode 0600, and refreshes them automatically.

## Cost

X charges per request. Reported rates are about $0.005 per post read and $0.015 per post created, and a post with a URL costs much more (around $0.20). Check the developer console for current prices.

After each read, `tw` prints an estimate to stderr. Commands that read posts default to small counts. Mentions and user posts can't go below 5, because that's X's minimum.
