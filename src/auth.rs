//! OAuth 2.0 (Authorization Code + PKCE) login and token storage.

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const AUTHORIZE_URL: &str = "https://x.com/i/oauth2/authorize";
const TOKEN_URL: &str = "https://api.x.com/2/oauth2/token";
const ME_URL: &str = "https://api.x.com/2/users/me";
// Must exactly match a callback URL registered in the X developer console.
// X requires 127.0.0.1 rather than localhost for local callbacks.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8723/callback";
const LISTEN_ADDR: &str = "127.0.0.1:8723";
const SCOPES: &str = "tweet.read tweet.write users.read offline.access";

/// App credentials from the X developer console (~/.config/tw/config.json).
#[derive(Serialize, Deserialize)]
pub struct Config {
    pub client_id: String,
    /// Only needed if the app is registered as a confidential client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// Logged-in user's tokens (~/.config/tw/session.json, mode 0600).
#[derive(Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
    pub user_id: String,
    pub username: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("no config directory")?
        .join("tw");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write_private(path: PathBuf, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
    Ok(())
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("config.json");
        let raw = fs::read_to_string(&path).map_err(|_| {
            anyhow!("no client id configured; run `tw login --client-id <ID>` first")
        })?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self) -> Result<()> {
        write_private(config_dir()?.join("config.json"), self)
    }
}

impl Session {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("session.json");
        let raw =
            fs::read_to_string(&path).map_err(|_| anyhow!("not logged in; run `tw login`"))?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self) -> Result<()> {
        write_private(config_dir()?.join("session.json"), self)
    }

    pub fn delete() -> Result<()> {
        let path = config_dir()?.join("session.json");
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn is_expired(&self) -> bool {
        now() + 60 >= self.expires_at
    }

    /// Exchanges the refresh token for a new access token and persists it.
    pub fn refresh(&mut self, http: &reqwest::blocking::Client, config: &Config) -> Result<()> {
        let refresh_token = self
            .refresh_token
            .clone()
            .context("session expired and has no refresh token; run `tw login`")?;
        let tokens = request_token(
            http,
            config,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_token),
            ],
        )
        .context("refreshing token failed; run `tw login`")?;
        self.access_token = tokens.access_token;
        // X rotates refresh tokens: the old one stops working after use.
        self.refresh_token = tokens.refresh_token.or(self.refresh_token.take());
        self.expires_at = now() + tokens.expires_in;
        self.save()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn random_string() -> Result<String> {
    let mut buf = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn request_token(
    http: &reqwest::blocking::Client,
    config: &Config,
    params: &[(&str, &str)],
) -> Result<TokenResponse> {
    let mut form: Vec<(&str, &str)> = params.to_vec();
    form.push(("client_id", &config.client_id));
    let mut req = http.post(TOKEN_URL).form(&form);
    if let Some(secret) = &config.client_secret {
        req = req.basic_auth(&config.client_id, Some(secret));
    }
    let resp = req.send()?;
    let status = resp.status();
    if !status.is_success() {
        bail!(
            "token endpoint returned {status}: {}",
            resp.text().unwrap_or_default()
        );
    }
    Ok(resp.json()?)
}

/// Runs the browser login flow and stores the resulting session.
pub fn login(http: &reqwest::blocking::Client, config: &Config) -> Result<Session> {
    let verifier = random_string()?;
    let challenge = pkce_challenge(&verifier);
    let state = random_string()?;

    let url = Url::parse_with_params(
        AUTHORIZE_URL,
        &[
            ("response_type", "code"),
            ("client_id", config.client_id.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ],
    )?;

    let listener = TcpListener::bind(LISTEN_ADDR)
        .with_context(|| format!("could not listen on {LISTEN_ADDR}"))?;
    eprintln!("Opening browser to authorize tw. If it doesn't open, visit:\n\n  {url}\n");
    let _ = open::that(url.as_str());

    let code = wait_for_code(&listener, &state)?;
    let tokens = request_token(
        http,
        config,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", &verifier),
        ],
    )?;

    #[derive(Deserialize)]
    struct Me {
        data: MeData,
    }
    #[derive(Deserialize)]
    struct MeData {
        id: String,
        username: String,
    }
    let me: Me = http
        .get(ME_URL)
        .bearer_auth(&tokens.access_token)
        .send()?
        .error_for_status()?
        .json()?;

    let session = Session {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: now() + tokens.expires_in,
        user_id: me.data.id,
        username: me.data.username,
    };
    session.save()?;
    Ok(session)
}

/// Accepts connections on the callback port until the redirect with `code` arrives.
fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut request_line = String::new();
        BufReader::new(&stream).read_line(&mut request_line)?;
        // e.g. "GET /callback?code=...&state=... HTTP/1.1"
        let path = request_line.split_whitespace().nth(1).unwrap_or("/");
        if !path.starts_with("/callback") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        }

        let url = Url::parse(&format!("http://localhost{path}"))?;
        let param = |key: &str| {
            url.query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned())
        };

        let result = if let Some(error) = param("error") {
            Err(anyhow!("authorization denied: {error}"))
        } else if param("state").as_deref() != Some(expected_state) {
            Err(anyhow!("state mismatch in callback"))
        } else {
            param("code").context("callback had no code")
        };

        let body = if result.is_ok() {
            "tw is authorized. You can close this tab."
        } else {
            "tw authorization failed. Check the terminal."
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        return result;
    }
    bail!("callback listener closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    fn callback(request_path: &str, state: &str) -> Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let request_path = request_path.to_string();
        let client = std::thread::spawn(move || {
            let mut favicon = TcpStream::connect(addr).unwrap();
            write!(favicon, "GET /favicon.ico HTTP/1.1\r\n\r\n").unwrap();
            let mut stream = TcpStream::connect(addr).unwrap();
            write!(
                stream,
                "GET {request_path} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let result = wait_for_code(&listener, state);
        assert!(client.join().unwrap().starts_with("HTTP/1.1 200"));
        result
    }

    #[test]
    fn callback_returns_code() {
        assert_eq!(
            callback("/callback?state=abc&code=x%2By", "abc").unwrap(),
            "x+y"
        );
    }

    #[test]
    fn callback_rejects_wrong_state_and_denial() {
        assert!(callback("/callback?state=evil&code=x", "abc").is_err());
        assert!(callback("/callback?error=access_denied&state=abc", "abc").is_err());
    }
}
