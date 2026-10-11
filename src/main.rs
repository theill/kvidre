mod api;
mod auth;

use anyhow::{Result, bail};
use api::{Client, Page, Post};
use auth::{Config, Session};
use clap::{Parser, Subcommand};
use jiff::{Timestamp, Zoned};
use std::io::{IsTerminal, Read};

/// Rough pay-per-use price of reading one post, for the cost hint after reads.
/// Check the X developer console for current rates.
const COST_PER_POST_READ: f64 = 0.005;

#[derive(Parser)]
#[command(name = "tw", version, about = "A tiny X/Twitter CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Log in through the browser (OAuth 2.0 PKCE)
    Login {
        /// OAuth 2.0 client ID from the X developer console (saved for next time)
        #[arg(long)]
        client_id: Option<String>,
        /// Client secret, only for apps registered as confidential clients
        #[arg(long)]
        client_secret: Option<String>,
    },
    /// Forget the stored session
    Logout,
    /// Show the logged-in account
    Whoami,
    /// Read your home timeline, newest first
    #[command(visible_alias = "t")]
    Timeline {
        #[arg(short = 'n', long, default_value_t = 10)]
        count: u32,
    },
    /// Read posts that mention you
    #[command(visible_alias = "m")]
    Mentions {
        #[arg(short = 'n', long, default_value_t = 5)]
        count: u32,
    },
    /// Read a user's recent posts
    #[command(visible_alias = "u")]
    User {
        username: String,
        #[arg(short = 'n', long, default_value_t = 5)]
        count: u32,
    },
    /// Publish a post (reads stdin if no text is given)
    #[command(visible_alias = "p")]
    Post { text: Vec<String> },
    /// Reply to a post by ID or URL (reads stdin if no text is given)
    #[command(visible_alias = "r")]
    Reply { post: String, text: Vec<String> },
    /// Delete one of your posts by ID or URL
    Delete { post: String },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Login {
            client_id,
            client_secret,
        } => {
            let config = match client_id {
                Some(client_id) => {
                    let config = Config {
                        client_id,
                        client_secret,
                    };
                    config.save()?;
                    config
                }
                None => Config::load()?,
            };
            let session = auth::login(&reqwest::blocking::Client::new(), &config)?;
            println!("Logged in as @{}", session.username);
        }
        Command::Logout => {
            Session::delete()?;
            println!("Logged out");
        }
        Command::Whoami => {
            let session = Session::load()?;
            println!("@{} (id {})", session.username, session.user_id);
        }
        Command::Timeline { count } => print_page(&Client::load()?.home(count)?),
        Command::Mentions { count } => print_page(&Client::load()?.mentions(count)?),
        Command::User { username, count } => {
            let username = username.trim_start_matches('@');
            print_page(&Client::load()?.user_posts(username, count)?)
        }
        Command::Post { text } => {
            let mut client = Client::load()?;
            let created = client.post(&text_or_stdin(text)?, None)?;
            println!("{}", post_url(&client.session.username, &created.id));
        }
        Command::Reply { post, text } => {
            let mut client = Client::load()?;
            let created = client.post(&text_or_stdin(text)?, Some(&post_id(&post)?))?;
            println!("{}", post_url(&client.session.username, &created.id));
        }
        Command::Delete { post } => {
            Client::load()?.delete(&post_id(&post)?)?;
            println!("Deleted");
        }
    }
    Ok(())
}

fn text_or_stdin(words: Vec<String>) -> Result<String> {
    let text = if words.is_empty() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        words.join(" ")
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("nothing to post");
    }
    Ok(text)
}

/// Accepts a bare ID or a post URL like https://x.com/user/status/123.
fn post_id(arg: &str) -> Result<String> {
    let id = arg
        .split("/status/")
        .nth(1)
        .unwrap_or(arg)
        .split(['?', '/'])
        .next()
        .unwrap_or_default();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("not a post ID or URL: {arg}");
    }
    Ok(id.to_string())
}

fn post_url(username: &str, id: &str) -> String {
    format!("https://x.com/{username}/status/{id}")
}

fn print_page(page: &Page) {
    let style = if std::io::stdout().is_terminal() {
        Style::COLOR
    } else {
        Style::PLAIN
    };

    if page.data.is_empty() {
        println!("No posts.");
    }
    let now = Zoned::now();
    for post in &page.data {
        println!("{}", format_post(page, post, &now, &style));
    }

    let n = page.data.len();
    eprintln!(
        "{}{n} posts read, about ${:.3}{}",
        style.dim,
        n as f64 * COST_PER_POST_READ,
        style.reset
    );
}

struct Style {
    bold: &'static str,
    dim: &'static str,
    reset: &'static str,
}

impl Style {
    const COLOR: Style = Style {
        bold: "\x1b[1m",
        dim: "\x1b[2m",
        reset: "\x1b[0m",
    };
    const PLAIN: Style = Style {
        bold: "",
        dim: "",
        reset: "",
    };
}

/// Renders one post; a repost shows the original in full, credited to the reposter.
fn format_post(page: &Page, post: &Post, now: &Zoned, style: &Style) -> String {
    let Style { bold, dim, reset } = style;
    let (shown, reposted_by) = match page.reposted(post) {
        Some(original) => (original, page.user(post.author_id.as_deref())),
        None => (post, None),
    };
    let (username, name) = page
        .user(shown.author_id.as_deref())
        .map_or(("unknown", ""), |u| (u.username.as_str(), u.name.as_str()));
    let when = shown
        .created_at
        .as_deref()
        .map(|t| format_time(t, now))
        .unwrap_or_default();
    let by = reposted_by
        .map(|u| format!(" · reposted by @{}", u.username))
        .unwrap_or_default();

    format!(
        "{bold}@{username}{reset} {dim}{name} · {when}{by}{reset}\n{}\n{dim}{}{reset}\n",
        shown.full_text(),
        post_url(username, &shown.id)
    )
}

/// Formats an API timestamp in `now`'s time zone: "today 08:31",
/// "yesterday 21:10", "Oct 9 08:31", or "2025-06-01 08:31" for older years.
fn format_time(created_at: &str, now: &Zoned) -> String {
    let Ok(timestamp) = created_at.parse::<Timestamp>() else {
        return created_at.to_string();
    };
    let local = timestamp.to_zoned(now.time_zone().clone());
    let format = if local.date() == now.date() {
        "today %H:%M"
    } else if now.date().yesterday().ok() == Some(local.date()) {
        "yesterday %H:%M"
    } else if local.year() == now.year() {
        "%b %-d %H:%M"
    } else {
        "%Y-%m-%d %H:%M"
    };
    local.strftime(format).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::{TimeZone, offset};

    /// 12:00 local on 2026-10-11 in a fixed UTC+2 zone.
    fn noon() -> Zoned {
        "2026-10-11T10:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(TimeZone::fixed(offset(2)))
    }

    #[test]
    fn formats_times_in_local_zone() {
        let now = noon();
        let f = |t| format_time(t, &now);
        assert_eq!(f("2026-10-11T06:31:00.000Z"), "today 08:31");
        // 22:30 UTC is already the next day at UTC+2.
        assert_eq!(f("2026-10-10T22:30:00.000Z"), "today 00:30");
        assert_eq!(f("2026-10-10T21:59:00.000Z"), "yesterday 23:59");
        assert_eq!(f("2026-10-09T12:00:00.000Z"), "Oct 9 14:00");
        assert_eq!(f("2025-12-31T23:30:00.000Z"), "Jan 1 01:30");
        assert_eq!(f("2025-06-01T00:00:00.000Z"), "2025-06-01 02:00");
        assert_eq!(f("garbage"), "garbage");
    }

    const REPOST_PAGE: &str = r#"{
        "data": [{
            "id": "2", "author_id": "10", "created_at": "2026-10-11T06:31:00.000Z",
            "text": "RT @yunta: For now the app will delegate the task to…",
            "referenced_tweets": [{"type": "retweeted", "id": "1"}]
        }, {
            "id": "3", "author_id": "10", "created_at": "2026-10-11T06:00:00.000Z",
            "text": "RT @gone: deleted original…",
            "referenced_tweets": [{"type": "retweeted", "id": "404"}]
        }],
        "includes": {
            "users": [
                {"id": "10", "username": "yoda", "name": "Yoda"},
                {"id": "20", "username": "yunta", "name": "Yunta"}
            ],
            "tweets": [{
                "id": "1", "author_id": "20", "created_at": "2026-10-11T05:00:00.000Z",
                "text": "For now the app will delegate the task to…",
                "note_tweet": {"text": "For now the app will delegate the task to the bot, but it can do everything."}
            }]
        }
    }"#;

    #[test]
    fn shows_reposts_in_full_credited_to_reposter() {
        let page: Page = serde_json::from_str(REPOST_PAGE).unwrap();
        assert_eq!(
            format_post(&page, &page.data[0], &noon(), &Style::PLAIN),
            "@yunta Yunta · today 07:00 · reposted by @yoda\n\
             For now the app will delegate the task to the bot, but it can do everything.\n\
             https://x.com/yunta/status/1\n"
        );
    }

    #[test]
    fn falls_back_to_repost_text_when_original_is_missing() {
        let page: Page = serde_json::from_str(REPOST_PAGE).unwrap();
        assert_eq!(
            format_post(&page, &page.data[1], &noon(), &Style::PLAIN),
            "@yoda Yoda · today 08:00\nRT @gone: deleted original…\nhttps://x.com/yoda/status/3\n"
        );
    }

    #[test]
    fn parses_post_ids() {
        assert_eq!(post_id("1234").unwrap(), "1234");
        assert_eq!(post_id("https://x.com/jack/status/20").unwrap(), "20");
        assert_eq!(
            post_id("https://twitter.com/a/status/99?s=20").unwrap(),
            "99"
        );
        assert_eq!(post_id("https://x.com/a/status/99/photo/1").unwrap(), "99");
        assert!(post_id("https://x.com/jack").is_err());
        assert!(post_id("").is_err());
    }
}
