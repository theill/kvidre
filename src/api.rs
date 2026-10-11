//! Thin client over the handful of X API v2 endpoints kvidre uses.

use crate::auth::{Config, Session};
use anyhow::{Result, bail};
use reqwest::blocking::{Client as Http, RequestBuilder, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

const BASE: &str = "https://api.x.com/2";
const TWEET_FIELDS: &str = "created_at,author_id,note_tweet,referenced_tweets";
// Pull in reposted originals and their authors so reposts show in full.
const EXPANSIONS: &str = "author_id,referenced_tweets.id,referenced_tweets.id.author_id";

// Rough pay-per-use prices for the cost estimates kvidre shows. Check the
// X developer console for current rates.
pub const COST_READ_POST: f64 = 0.005;
pub const COST_READ_USER: f64 = 0.010;
const COST_CREATE_POST: f64 = 0.015;
const COST_CREATE_POST_WITH_URL: f64 = 0.20;

pub fn post_cost(text: &str) -> f64 {
    if text.contains("http://") || text.contains("https://") {
        COST_CREATE_POST_WITH_URL
    } else {
        COST_CREATE_POST
    }
}

#[derive(Deserialize)]
pub struct Post {
    pub id: String,
    pub text: String,
    pub author_id: Option<String>,
    pub created_at: Option<String>,
    /// Full text of posts longer than 280 characters.
    pub note_tweet: Option<NoteTweet>,
    #[serde(default)]
    pub referenced_tweets: Vec<Reference>,
}

impl Post {
    pub fn full_text(&self) -> &str {
        self.note_tweet.as_ref().map_or(&self.text, |n| &n.text)
    }
}

/// A post this one reposts, quotes or replies to.
#[derive(Deserialize)]
pub struct Reference {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

#[derive(Deserialize)]
pub struct NoteTweet {
    pub text: String,
}

#[derive(Deserialize, Clone)]
pub struct User {
    pub id: String,
    pub username: String,
    pub name: String,
}

#[derive(Deserialize, Default)]
pub struct Includes {
    #[serde(default)]
    pub users: Vec<User>,
    #[serde(default)]
    pub tweets: Vec<Post>,
}

#[derive(Deserialize, Default)]
pub struct Meta {
    /// Pass back as `pagination_token` to fetch the next, older page.
    pub next_token: Option<String>,
}

#[derive(Deserialize)]
pub struct Page {
    #[serde(default)]
    pub data: Vec<Post>,
    #[serde(default)]
    pub includes: Includes,
    #[serde(default)]
    pub meta: Meta,
}

impl Page {
    /// Estimated cost of reading this page, counting included originals.
    pub fn read_cost(&self) -> f64 {
        (self.data.len() + self.includes.tweets.len()) as f64 * COST_READ_POST
    }

    pub fn user(&self, id: Option<&str>) -> Option<&User> {
        self.includes
            .users
            .iter()
            .find(|u| Some(u.id.as_str()) == id)
    }

    /// The original post if `post` is a repost and X included it.
    pub fn reposted(&self, post: &Post) -> Option<&Post> {
        let reference = post
            .referenced_tweets
            .iter()
            .find(|r| r.kind == "retweeted")?;
        self.includes.tweets.iter().find(|t| t.id == reference.id)
    }
}

#[derive(Deserialize)]
struct Data<T> {
    data: T,
}

#[derive(Deserialize)]
pub struct Created {
    pub id: String,
}

/// Turns a non-2xx response into an error that includes X's explanation.
pub fn check(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().unwrap_or_default();
    match status.as_u16() {
        402 => bail!("X API: out of credits (402). Top up in the developer console.\n{body}"),
        403 => bail!(
            "X API: forbidden (403). Check the app has credits and Read and write permission in the developer console.\n{body}"
        ),
        429 => bail!("X API: rate limited (429). Try again later.\n{body}"),
        _ => bail!("X API returned {status}\n{body}"),
    }
}

pub struct Client {
    http: Http,
    config: Config,
    pub session: Session,
}

impl Client {
    pub fn load() -> Result<Self> {
        Ok(Self {
            http: Http::new(),
            config: Config::load()?,
            session: Session::load()?,
        })
    }

    fn send<T: DeserializeOwned>(&mut self, build: impl Fn(&Http) -> RequestBuilder) -> Result<T> {
        if self.session.is_expired() {
            self.session.refresh(&self.http, &self.config)?;
        }
        let resp = build(&self.http)
            .bearer_auth(&self.session.access_token)
            .send()?;
        Ok(check(resp)?.json()?)
    }

    fn timeline(&mut self, path: &str, count: u32, page: Option<&str>) -> Result<Page> {
        let url = format!("{BASE}{path}");
        let mut query = vec![
            ("max_results", count.to_string()),
            ("tweet.fields", TWEET_FIELDS.into()),
            ("expansions", EXPANSIONS.into()),
            ("user.fields", "username,name".into()),
        ];
        if let Some(token) = page {
            query.push(("pagination_token", token.into()));
        }
        self.send(|http| http.get(&url).query(&query))
    }

    /// Home timeline, newest first. X accepts 1–100.
    pub fn home(&mut self, count: u32, page: Option<&str>) -> Result<Page> {
        let path = format!(
            "/users/{}/timelines/reverse_chronological",
            self.session.user_id
        );
        self.timeline(&path, count.clamp(1, 100), page)
    }

    /// Posts mentioning you. X accepts 5–100.
    pub fn mentions(&mut self, count: u32, page: Option<&str>) -> Result<Page> {
        let path = format!("/users/{}/mentions", self.session.user_id);
        self.timeline(&path, count.clamp(5, 100), page)
    }

    pub fn lookup_user(&mut self, username: &str) -> Result<User> {
        let url = format!("{BASE}/users/by/username/{username}");
        let user: Data<User> = self.send(|http| http.get(&url))?;
        Ok(user.data)
    }

    /// A user's recent posts. X accepts 5–100.
    pub fn user_posts(&mut self, user_id: &str, count: u32, page: Option<&str>) -> Result<Page> {
        let path = format!("/users/{user_id}/tweets");
        self.timeline(&path, count.clamp(5, 100), page)
    }

    pub fn post(&mut self, text: &str, reply_to: Option<&str>) -> Result<Created> {
        let mut body = json!({ "text": text });
        if let Some(id) = reply_to {
            body["reply"] = json!({ "in_reply_to_tweet_id": id });
        }
        let url = format!("{BASE}/tweets");
        let created: Data<Created> = self.send(|http| http.post(&url).json(&body))?;
        Ok(created.data)
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        let url = format!("{BASE}/tweets/{id}");
        let _: serde_json::Value = self.send(|http| http.delete(&url))?;
        Ok(())
    }
}
