//! Thin client over the handful of X API v2 endpoints kvidre uses.

use crate::auth::{Config, Session};
use anyhow::Result;
use reqwest::blocking::{Client as Http, RequestBuilder, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::fmt;

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

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct User {
    pub id: String,
    pub username: String,
    pub name: String,
    /// Profile bio; only requested when listing accounts.
    #[serde(default)]
    pub description: Option<String>,
}

/// A page of accounts, e.g. the ones you follow.
#[derive(Deserialize)]
pub struct UserPage {
    #[serde(default)]
    pub data: Vec<User>,
    #[serde(default)]
    pub meta: Meta,
}

impl UserPage {
    /// Estimated cost, assuming each returned account is billed as a user read.
    pub fn read_cost(&self) -> f64 {
        self.data.len() as f64 * COST_READ_USER
    }
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
    Err(ApiError::new(status.as_u16(), &body).into())
}

/// A non-2xx response from X. Displays X's own reason first, since the TUI
/// shows errors on one line and a long generic hint would push it out of view.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    reason: String,
}

impl ApiError {
    fn new(status: u16, body: &str) -> Self {
        let mut reason = reason(body).unwrap_or_else(|| body.trim().to_string());
        if !reason.is_empty() && !reason.ends_with(['.', '!', '?']) {
            reason.push('.');
        }
        Self { status, reason }
    }

    fn mentions(&self, text: &str) -> bool {
        self.reason.to_lowercase().contains(text)
    }

    /// X refuses replies from self-serve apps unless the post mentions you or
    /// is your own, though the same reply works on x.com.
    pub fn reply_restricted(&self) -> bool {
        self.status == 403 && self.mentions("repl") && !self.mentions("duplicate")
    }

    fn hint(&self) -> &'static str {
        match self.status {
            402 => "Top up credits in the developer console.",
            403 if self.mentions("duplicate") => {
                "You already posted this text. Change it and try again."
            }
            403 if self.reply_restricted() => {
                "This is X's rule for apps, not your account. Replying on x.com still works."
            }
            403 if self.mentions("client-not-enrolled") || self.mentions("project") => {
                "Attach the app to a Project in the developer console, then run `kvidre login` again."
            }
            403 => {
                "Check the app has credits and Read and write permission in the developer console."
            }
            429 => "Rate limited. Try again later.",
            _ => "",
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = [self.reason.as_str(), self.hint()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            write!(f, "X API returned {}.", self.status)
        } else {
            write!(f, "X API {}: {text}", self.status)
        }
    }
}

impl std::error::Error for ApiError {}

/// X's explanation from an error body like
/// `{"detail": "...", "reason": "...", "errors": [{"message": "..."}]}`.
fn reason(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for key in ["detail", "reason"] {
        if let Some(s) = value[key].as_str() {
            parts.push(s);
        }
    }
    if let Some(errors) = value["errors"].as_array() {
        parts.extend(errors.iter().filter_map(|e| e["message"].as_str()));
    }
    let parts: Vec<&str> = parts
        .into_iter()
        .map(|s| s.trim().trim_end_matches('.'))
        .filter(|s| !s.is_empty())
        .collect();
    (!parts.is_empty()).then(|| format!("{}.", parts.join(". ")))
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

    fn timeline(
        &mut self,
        path: &str,
        count: u32,
        page: Option<&str>,
        exclude: Option<&str>,
    ) -> Result<Page> {
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
        if let Some(exclude) = exclude {
            query.push(("exclude", exclude.into()));
        }
        self.send(|http| http.get(&url).query(&query))
    }

    /// Posts from you and the accounts you follow, newest first. X accepts 1–100.
    /// With `originals_only`, X leaves out replies and reposts, so they aren't billed.
    pub fn following(
        &mut self,
        count: u32,
        page: Option<&str>,
        originals_only: bool,
    ) -> Result<Page> {
        let path = format!(
            "/users/{}/timelines/reverse_chronological",
            self.session.user_id
        );
        let exclude = originals_only.then_some("replies,retweets");
        self.timeline(&path, count.clamp(1, 100), page, exclude)
    }

    /// Posts mentioning you. X accepts 5–100.
    pub fn mentions(&mut self, count: u32, page: Option<&str>) -> Result<Page> {
        let path = format!("/users/{}/mentions", self.session.user_id);
        self.timeline(&path, count.clamp(5, 100), page, None)
    }

    pub fn lookup_user(&mut self, username: &str) -> Result<User> {
        let url = format!("{BASE}/users/by/username/{username}");
        let user: Data<User> = self.send(|http| http.get(&url))?;
        Ok(user.data)
    }

    /// A user's recent posts. X accepts 5–100.
    pub fn user_posts(&mut self, user_id: &str, count: u32, page: Option<&str>) -> Result<Page> {
        let path = format!("/users/{user_id}/tweets");
        self.timeline(&path, count.clamp(5, 100), page, None)
    }

    /// Accounts you follow, most recently followed first. X accepts 1–1000.
    pub fn followed_accounts(&mut self, count: u32, page: Option<&str>) -> Result<UserPage> {
        self.session.require_scope("follows.read")?;
        let url = format!("{BASE}/users/{}/following", self.session.user_id);
        let mut query = vec![
            ("max_results", count.clamp(1, 1000).to_string()),
            ("user.fields", "username,name,description".into()),
        ];
        if let Some(token) = page {
            query.push(("pagination_token", token.into()));
        }
        self.send(|http| http.get(&url).query(&query))
    }

    pub fn unfollow(&mut self, user_id: &str) -> Result<()> {
        self.session.require_scope("follows.write")?;
        let url = format!("{BASE}/users/{}/following/{user_id}", self.session.user_id);
        let _: serde_json::Value = self.send(|http| http.delete(&url))?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn explain(status: u16, body: &str) -> String {
        ApiError::new(status, body).to_string()
    }

    #[test]
    fn explain_leads_with_x_reason() {
        let duplicate = r#"{"detail":"You are not allowed to create a Tweet with duplicate content.","type":"about:blank","title":"Forbidden","status":403}"#;
        assert_eq!(
            explain(403, duplicate),
            "X API 403: You are not allowed to create a Tweet with duplicate content. You already posted this text. Change it and try again."
        );
        assert!(!ApiError::new(403, duplicate).reply_restricted());

        let reply = r#"{"detail":"You can only reply to or quote posts where you are mentioned or are the author.","status":403}"#;
        assert_eq!(
            explain(403, reply),
            "X API 403: You can only reply to or quote posts where you are mentioned or are the author. This is X's rule for apps, not your account. Replying on x.com still works."
        );
        assert!(ApiError::new(403, reply).reply_restricted());

        let enrolled = r#"{"detail":"When authenticating requests to the Twitter API v2 endpoints, you must use keys and tokens from a Twitter developer App that is attached to a Project.","reason":"client-not-enrolled","title":"Client Forbidden"}"#;
        assert!(explain(403, enrolled).contains("Attach the app to a Project"));

        let invalid = r#"{"detail":"One or more parameters to your request was invalid.","errors":[{"message":"Please include either text or media in your Tweet."}]}"#;
        assert_eq!(
            explain(400, invalid),
            "X API 400: One or more parameters to your request was invalid. Please include either text or media in your Tweet."
        );
    }

    #[test]
    fn explain_falls_back_to_raw_body() {
        assert_eq!(
            explain(403, "Forbidden"),
            "X API 403: Forbidden. Check the app has credits and Read and write permission in the developer console."
        );
        assert_eq!(explain(500, ""), "X API returned 500.");
    }
}
