//! Display-ready posts shared by the CLI output and the TUI.

use crate::api::{Page, Post};
use jiff::{Timestamp, Zoned};

/// A post resolved for display: a repost shows its original, credited to the reposter.
#[derive(Clone, Debug, PartialEq)]
pub struct PostView {
    /// ID of the post shown (the original, for reposts). Replies and links target this.
    pub id: String,
    pub author_id: Option<String>,
    pub username: String,
    pub name: String,
    pub created_at: Option<String>,
    pub reposted_by: Option<String>,
    /// True for any repost entry, even when X didn't include the original.
    pub is_repost: bool,
    pub text: String,
}

impl PostView {
    pub fn all(page: &Page) -> Vec<PostView> {
        page.data.iter().map(|post| Self::new(page, post)).collect()
    }

    pub fn new(page: &Page, post: &Post) -> Self {
        let (shown, reposted_by) = match page.reposted(post) {
            Some(original) => (original, page.user(post.author_id.as_deref())),
            None => (post, None),
        };
        let author = page.user(shown.author_id.as_deref());
        Self {
            id: shown.id.clone(),
            author_id: shown.author_id.clone(),
            username: author.map_or("unknown".into(), |u| u.username.clone()),
            name: author.map_or(String::new(), |u| u.name.clone()),
            created_at: shown.created_at.clone(),
            reposted_by: reposted_by.map(|u| u.username.clone()),
            is_repost: post.referenced_tweets.iter().any(|r| r.kind == "retweeted"),
            text: unescape(shown.full_text()),
        }
    }

    pub fn url(&self) -> String {
        post_url(&self.username, &self.id)
    }

    pub fn when(&self, now: &Zoned) -> String {
        self.created_at
            .as_deref()
            .map(|t| format_time(t, now))
            .unwrap_or_default()
    }
}

pub fn post_url(username: &str, id: &str) -> String {
    format!("https://x.com/{username}/status/{id}")
}

/// X returns post text with `&`, `<` and `>` HTML-escaped.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Formats an API timestamp in `now`'s time zone: "today 08:31",
/// "yesterday 21:10", "Oct 9 08:31", or "2025-06-01 08:31" for older years.
pub fn format_time(created_at: &str, now: &Zoned) -> String {
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
pub mod tests {
    use super::*;
    use jiff::tz::{TimeZone, offset};

    /// 12:00 local on 2026-10-11 in a fixed UTC+2 zone.
    pub fn noon() -> Zoned {
        "2026-10-11T10:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(TimeZone::fixed(offset(2)))
    }

    pub const REPOST_PAGE: &str = r#"{
        "data": [{
            "id": "2", "author_id": "10", "created_at": "2026-10-11T06:31:00.000Z",
            "text": "RT @yunta: For now the app will delegate the task to…",
            "referenced_tweets": [{"type": "retweeted", "id": "1"}]
        }, {
            "id": "3", "author_id": "10", "created_at": "2026-10-11T06:00:00.000Z",
            "text": "RT @gone: deleted original &amp; more…",
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
        },
        "meta": {"next_token": "abc"}
    }"#;

    pub fn repost_page() -> Page {
        serde_json::from_str(REPOST_PAGE).unwrap()
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

    #[test]
    fn resolves_reposts_to_full_original() {
        let page = repost_page();
        let view = PostView::new(&page, &page.data[0]);
        assert_eq!(view.username, "yunta");
        assert_eq!(view.reposted_by.as_deref(), Some("yoda"));
        assert_eq!(view.url(), "https://x.com/yunta/status/1");
        assert!(view.text.ends_with("it can do everything."));
        assert_eq!(view.when(&noon()), "today 07:00");
    }

    #[test]
    fn falls_back_to_repost_text_when_original_is_missing() {
        let page = repost_page();
        let view = PostView::new(&page, &page.data[1]);
        assert_eq!(view.username, "yoda");
        assert_eq!(view.reposted_by, None);
        assert!(view.is_repost);
        assert_eq!(view.text, "RT @gone: deleted original & more…");
    }

    #[test]
    fn page_metadata() {
        let page = repost_page();
        assert_eq!(page.meta.next_token.as_deref(), Some("abc"));
        assert!((page.read_cost() - 0.015).abs() < 1e-9);
    }
}
