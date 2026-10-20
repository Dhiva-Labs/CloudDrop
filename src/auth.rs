//! Google OAuth for an installed app: loopback redirect + PKCE, refresh-token storage.

use crate::config::Config;
use crate::remote::{RResult, RemoteError};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
}

pub fn token_path() -> PathBuf {
    crate::config::data_dir().join("token.json")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

struct Loaded {
    token: Token,
    file_mtime: Option<SystemTime>,
}

pub struct Auth {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    client_secret: String,
    path: PathBuf,
    state: Mutex<Option<Loaded>>,
}

impl Auth {
    pub fn new(http: reqwest::Client, cfg: &Config, path: PathBuf) -> Self {
        Self {
            http,
            token_url: cfg.endpoints.token_url.clone(),
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
            path,
            state: Mutex::new(None),
        }
    }

    fn file_mtime(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path).and_then(|m| m.modified()).ok()
    }

    /// Loads the token file when first needed or when `clouddrop login` rewrote it.
    fn reload_if_needed(&self, slot: &mut Option<Loaded>) -> RResult<()> {
        let mtime = self.file_mtime();
        if slot.as_ref().is_some_and(|l| l.file_mtime == mtime) {
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.path).map_err(|_| {
            RemoteError::Auth("not signed in — run `clouddrop login`".into())
        })?;
        let token: Token = serde_json::from_str(&text)
            .map_err(|_| RemoteError::Auth("token file is corrupt — run `clouddrop login`".into()))?;
        *slot = Some(Loaded {
            token,
            file_mtime: mtime,
        });
        Ok(())
    }

    /// A valid access token, refreshed when it is about to expire.
    pub async fn token(&self) -> RResult<String> {
        let mut slot = self.state.lock().await;
        self.reload_if_needed(&mut slot)?;
        let loaded = slot.as_ref().expect("loaded");
        if loaded.token.expires_at - 60 > unix_now() {
            return Ok(loaded.token.access_token.clone());
        }
        self.refresh_locked(&mut slot).await
    }

    pub async fn refresh(&self) -> RResult<String> {
        let mut slot = self.state.lock().await;
        self.reload_if_needed(&mut slot)?;
        self.refresh_locked(&mut slot).await
    }

    async fn refresh_locked(&self, slot: &mut Option<Loaded>) -> RResult<String> {
        let refresh_token = slot.as_ref().expect("loaded").token.refresh_token.clone();
        let resp = self
            .http
            .post(&self.token_url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|e| RemoteError::Transient(format!("cannot reach Google sign-in: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status.is_client_error() {
                return Err(RemoteError::Auth(format!(
                    "Google rejected the saved sign-in ({}) — run `clouddrop login`",
                    body.trim()
                )));
            }
            return Err(RemoteError::Transient(format!("token refresh failed: {status}")));
        }
        let tr: TokenResponse = resp
            .json()
            .await
            .map_err(|e| RemoteError::Transient(format!("bad token response: {e}")))?;
        let token = Token {
            access_token: tr.access_token,
            refresh_token: tr.refresh_token.unwrap_or(refresh_token),
            expires_at: unix_now() + tr.expires_in.unwrap_or(3600),
        };
        save_token(&self.path, &token)
            .map_err(|e| RemoteError::Permanent(format!("cannot save token: {e}")))?;
        let access = token.access_token.clone();
        *slot = Some(Loaded {
            token,
            file_mtime: self.file_mtime(),
        });
        Ok(access)
    }
}

pub fn save_token(path: &Path, token: &Token) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::util::write_private(path, serde_json::to_string_pretty(token)?.as_bytes())
}

fn random_b64(n: usize) -> String {
    let bytes: Vec<u8> = (0..n).map(|_| rand::random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Interactive sign-in. `show_url` receives the consent URL (print it / open a browser).
pub async fn login(cfg: &Config, token_file: &Path, show_url: impl FnOnce(&str)) -> Result<()> {
    if cfg.client_id.is_empty() || cfg.client_secret.is_empty() {
        bail!("client_id/client_secret missing from the config — run `clouddrop setup`");
    }
    let verifier = random_b64(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(16);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let mut url = url::Url::parse(&cfg.endpoints.auth_url)?;
    url.query_pairs_mut()
        .append_pair("client_id", &cfg.client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    show_url(url.as_str());

    let code = tokio::time::timeout(Duration::from_secs(300), wait_for_code(&listener, &state))
        .await
        .map_err(|_| anyhow!("timed out waiting for the browser sign-in"))??;

    let http = crate::remote::http_client();
    let resp = http
        .post(&cfg.endpoints.token_url)
        .form(&[
            ("code", code.as_str()),
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .context("cannot reach Google sign-in")?;
    if !resp.status().is_success() {
        let status = resp.status();
        bail!("sign-in failed ({status}): {}", resp.text().await.unwrap_or_default());
    }
    let tr: TokenResponse = resp.json().await.context("bad token response")?;
    let refresh_token = tr
        .refresh_token
        .ok_or_else(|| anyhow!("Google did not return a refresh token; remove CloudDrop's access at myaccount.google.com/permissions and sign in again"))?;
    save_token(
        token_file,
        &Token {
            access_token: tr.access_token,
            refresh_token,
            expires_at: unix_now() + tr.expires_in.unwrap_or(3600),
        },
    )
}

/// Serves the loopback redirect until a request carries `code` (or an error) for our `state`.
async fn wait_for_code(listener: &tokio::net::TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
        let parsed = url::Url::parse(&format!("http://localhost{target}"))?;
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();

        let (status, page, outcome) = if parsed.path() != "/" {
            ("404 Not Found", "Not found", None)
        } else if params.get("state").map(String::as_str) != Some(state) {
            ("400 Bad Request", "Sign-in state mismatch. Start again from the terminal.", None)
        } else if let Some(err) = params.get("error") {
            (
                "200 OK",
                "Sign-in was cancelled. You can close this tab.",
                Some(Err(anyhow!("sign-in refused: {err}"))),
            )
        } else if let Some(code) = params.get("code") {
            (
                "200 OK",
                "CloudDrop is signed in. You can close this tab.",
                Some(Ok(code.clone())),
            )
        } else {
            ("400 Bad Request", "Missing code.", None)
        };
        let html = format!(
            "<!doctype html><meta charset=utf-8><title>CloudDrop</title>\
             <body style=\"font:16px system-ui;margin:4em auto;max-width:30em\"><p>{page}</p>"
        );
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
            html.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
        if let Some(outcome) = outcome {
            return outcome;
        }
    }
}
