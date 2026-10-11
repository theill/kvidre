mod api;
mod auth;

use anyhow::{Result, bail};
use api::{Client, Page};
use auth::{Config, Session};
use clap::{Parser, Subcommand};
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
    let color = std::io::stdout().is_terminal();
    let (bold, dim, reset) = if color {
        ("\x1b[1m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "")
    };

    if page.data.is_empty() {
        println!("No posts.");
    }
    for post in &page.data {
        let author = post
            .author_id
            .as_ref()
            .and_then(|id| page.includes.users.iter().find(|u| &u.id == id));
        let (username, name) =
            author.map_or(("unknown", ""), |u| (u.username.as_str(), u.name.as_str()));
        // "2026-10-11T09:12:00.000Z" -> "2026-10-11 09:12"
        let when = post
            .created_at
            .as_deref()
            .and_then(|t| t.get(..16))
            .map(|t| t.replace('T', " "))
            .unwrap_or_default();
        let text = post.note_tweet.as_ref().map_or(&post.text, |n| &n.text);

        println!("{bold}@{username}{reset} {dim}{name} · {when}{reset}");
        println!("{text}");
        println!("{dim}{}{reset}\n", post_url(username, &post.id));
    }

    let n = page.data.len();
    eprintln!(
        "{dim}{n} posts read, about ${:.3}{reset}",
        n as f64 * COST_PER_POST_READ
    );
}

#[cfg(test)]
mod tests {
    use super::post_id;

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
