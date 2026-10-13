mod api;
mod auth;
mod favorites;
mod tui;
mod view;

use anyhow::{Result, bail};
use api::{ApiError, Client, Page, UserPage};
use auth::{Config, Session};
use clap::{Parser, Subcommand};
use jiff::Zoned;
use std::io::{IsTerminal, Read};
use view::{PostView, post_url, reply_intent_url};

#[derive(Parser)]
#[command(
    name = "kvidre",
    version,
    about = "A tiny X/Twitter CLI. Run without a command to open the TUI."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the interactive terminal UI (the default)
    Tui,
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
    /// Read original posts from people you follow, newest first
    #[command(visible_alias = "t")]
    Timeline {
        #[arg(short = 'n', long, default_value_t = 10)]
        count: u32,
        /// Also include replies and reposts
        #[arg(short, long)]
        all: bool,
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
    /// List accounts you follow, most recently followed first
    Follows {
        /// Each account is a paid read (about $0.01), up to 1000
        #[arg(short = 'n', long, default_value_t = 100)]
        count: u32,
    },
    /// Stop following an account
    Unfollow { username: String },
    /// Read your favorites' posts from the last 7 days, newest first
    #[command(visible_alias = "fav")]
    Favorites {
        #[arg(short = 'n', long, default_value_t = 10)]
        count: u32,
        /// Only list who your favorites are (free, no API call)
        #[arg(short, long)]
        list: bool,
    },
    /// Add an account to your favorites
    Favorite { username: String },
    /// Remove an account from your favorites
    Unfavorite { username: String },
}

fn main() -> Result<()> {
    match Cli::parse().command.unwrap_or(Command::Tui) {
        Command::Tui => tui::run()?,
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
        Command::Timeline { count, all } => {
            print_page(&Client::load()?.following(count, None, !all)?)
        }
        Command::Mentions { count } => print_page(&Client::load()?.mentions(count, None)?),
        Command::User { username, count } => {
            let mut client = Client::load()?;
            let user = client.lookup_user(username.trim_start_matches('@'))?;
            print_page(&client.user_posts(&user.id, count, None)?)
        }
        Command::Post { text } => {
            let mut client = Client::load()?;
            let created = client.post(&text_or_stdin(text)?, None)?;
            println!("{}", post_url(&client.session.username, &created.id));
        }
        Command::Reply { post, text } => {
            let mut client = Client::load()?;
            let (text, id) = (text_or_stdin(text)?, post_id(&post)?);
            match client.post(&text, Some(&id)) {
                Ok(created) => println!("{}", post_url(&client.session.username, &created.id)),
                Err(e)
                    if e.downcast_ref::<ApiError>()
                        .is_some_and(|e| e.reply_restricted()) =>
                {
                    // X takes this reply on x.com but not from apps, so hand it over there.
                    let url = reply_intent_url(&id, &text);
                    eprintln!("{e}\nOpening it on x.com, press Reply to post:\n\n  {url}");
                    let _ = open::that(&url);
                }
                Err(e) => return Err(e),
            }
        }
        Command::Delete { post } => {
            Client::load()?.delete(&post_id(&post)?)?;
            println!("Deleted");
        }
        Command::Follows { count } => print_users(&Client::load()?.followed_accounts(count, None)?),
        Command::Unfollow { username } => {
            let mut client = Client::load()?;
            let user = client.lookup_user(username.trim_start_matches('@'))?;
            client.unfollow(&user.id)?;
            println!("Unfollowed @{}", user.username);
        }
        Command::Favorites { count, list } => {
            let saved = favorites::load()?;
            if saved.is_empty() {
                println!("No favorites yet. Add one with `kvidre favorite <username>`.");
            } else if list {
                for favorite in &saved {
                    println!("@{}", favorite.username);
                }
            } else {
                let ids: Vec<&str> = saved.iter().map(|f| f.id.as_str()).collect();
                print_page(&Client::load()?.favorites(&ids, count, None)?)
            }
        }
        Command::Favorite { username } => {
            let mut saved = favorites::load()?;
            let username = username.trim_start_matches('@');
            if saved
                .iter()
                .any(|f| f.username.eq_ignore_ascii_case(username))
            {
                println!("@{username} is already a favorite");
            } else {
                let user = Client::load()?.lookup_user(username)?;
                favorites::toggle(&mut saved, &user.id, &user.username);
                favorites::save(&saved)?;
                println!("Added @{} to favorites", user.username);
            }
        }
        Command::Unfavorite { username } => {
            let mut saved = favorites::load()?;
            let username = username.trim_start_matches('@');
            let Some(i) = saved
                .iter()
                .position(|f| f.username.eq_ignore_ascii_case(username))
            else {
                bail!("@{username} is not a favorite");
            };
            let removed = saved.remove(i);
            favorites::save(&saved)?;
            println!("Removed @{} from favorites", removed.username);
        }
    }
    Ok(())
}

fn print_users(page: &UserPage) {
    let style = if std::io::stdout().is_terminal() {
        Style::COLOR
    } else {
        Style::PLAIN
    };
    let Style { bold, dim, reset } = style;
    if page.data.is_empty() {
        println!("You don't follow anyone.");
    }
    for user in &page.data {
        let bio = user.description.as_deref().unwrap_or_default();
        let bio = bio.lines().next().unwrap_or_default();
        println!(
            "{bold}@{}{reset} {}  {dim}{bio}{reset}",
            user.username, user.name
        );
    }
    let more = if page.meta.next_token.is_some() {
        " (more available, raise -n)"
    } else {
        ""
    };
    eprintln!(
        "{dim}{} accounts{more}, about ${:.3}{reset}",
        page.data.len(),
        page.read_cost()
    );
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
    for view in PostView::all(page) {
        println!("{}", format_post(&view, &now, &style));
    }

    eprintln!(
        "{}{} posts read, about ${:.3}{}",
        style.dim,
        page.data.len(),
        page.read_cost(),
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

fn format_post(view: &PostView, now: &Zoned, style: &Style) -> String {
    let Style { bold, dim, reset } = style;
    let by = view
        .reposted_by
        .as_ref()
        .map(|u| format!(" · reposted by @{u}"))
        .unwrap_or_default();
    format!(
        "{bold}@{}{reset} {dim}{} · {}{by}{reset}\n{}\n{dim}{}{reset}\n",
        view.username,
        view.name,
        view.when(now),
        view.text,
        view.url()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use view::tests::{noon, repost_page};

    #[test]
    fn prints_reposts_in_full_credited_to_reposter() {
        let page = repost_page();
        let view = PostView::new(&page, &page.data[0]);
        assert_eq!(
            format_post(&view, &noon(), &Style::PLAIN),
            "@yunta Yunta · today 07:00 · reposted by @yoda\n\
             For now the app will delegate the task to the bot, but it can do everything.\n\
             https://x.com/yunta/status/1\n"
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
