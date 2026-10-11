//! Thin client over the handful of X API v2 endpoints tw uses.

use crate::auth::{Config, Session};
use anyhow::{Result, bail};
use reqwest::blocking::{Client as Http, RequestBuilder, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

const BASE: &str = "https://api.x.com/2";
const TWEET_FIELDS: &str = "created_at,author_id,note_tweet";

#[derive(Deserialize)]
pub struct Post {
    pub id: String,
    pub text: String,
    pub author_id: Option<String>,
    pub created_at: Option<String>,
    /// Full text of posts longer than 280 characters.
    pub note_tweet: Option<NoteTweet>,
}

#[derive(Deserialize)]
pub struct NoteTweet {
    pub text: String,
}

#[derive(Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub name: String,
}

#[derive(Deserialize, Default)]
pub struct Includes {
    #[serde(default)]
    pub users: Vec<User>,
}

#[derive(Deserialize)]
pub struct Page {
    #[serde(default)]
    pub data: Vec<Post>,
    #[serde(default)]
    pub includes: Includes,
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

    fn timeline(&mut self, path: &str, count: u32) -> Result<Page> {
        let url = format!("{BASE}{path}");
        let query = [
            ("max_results", count.to_string()),
            ("tweet.fields", TWEET_FIELDS.into()),
            ("expansions", "author_id".into()),
            ("user.fields", "username,name".into()),
        ];
        self.send(|http| http.get(&url).query(&query))
    }

    /// Home timeline, newest first. X accepts 1–100.
    pub fn home(&mut self, count: u32) -> Result<Page> {
        let path = format!(
            "/users/{}/timelines/reverse_chronological",
            self.session.user_id
        );
        self.timeline(&path, count.clamp(1, 100))
    }

    /// Posts mentioning you. X accepts 5–100.
    pub fn mentions(&mut self, count: u32) -> Result<Page> {
        let path = format!("/users/{}/mentions", self.session.user_id);
        self.timeline(&path, count.clamp(5, 100))
    }

    /// A user's recent posts. X accepts 5–100.
    pub fn user_posts(&mut self, username: &str, count: u32) -> Result<Page> {
        let url = format!("{BASE}/users/by/username/{username}");
        let user: Data<User> = self.send(|http| http.get(&url))?;
        let mut page = self.timeline(
            &format!("/users/{}/tweets", user.data.id),
            count.clamp(5, 100),
        )?;
        page.includes.users.push(user.data);
        Ok(page)
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
