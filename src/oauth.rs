//! Browser sign-in for providers that use OAuth 2 with PKCE, a loopback redirect and no client
//! secret (`SignIn::OAuth`), and [`TokenFile`], the token source behind every OAuth provider
//! (Google's `auth::Auth` included): tokens in a file readable only by you, refreshed with the
//! `refresh_token` grant.
//!
//! A provider's tokens live in `token-<id>.json` in the data directory, in the same format as
//! Google's token.json ([`Token`]). Its client id comes from `[<id>] client_id` in the config or
//! from the build (`CLOUDDROP_OAUTH_CLIENTS="onedrive=…,dropbox=…"`). Google keeps its own
//! flow, with a client secret, in `auth.rs`.
//!
//! A provider's client is built like this (in `remote_for`):
//!
//! ```ignore
//! let app = oauth::app(Provider::OneDrive).expect("OAuth");
//! let id = oauth::client_id(cfg, Provider::OneDrive).unwrap_or_default();
//! let tokens = Arc::new(oauth::TokenFile::new(http.clone(), &OAuthClient::new(app, id), oauth::token_path("onedrive")));
//! ```

use crate::auth::{Token, TokenResponse, pkce, random_b64, save_token, unix_now, wait_for_code};
use crate::config::Config;
use crate::remote::{OAuthApp, Provider, RemoteError, RemoteResult, SignIn, TokenSource};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::future::BoxFuture;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

/// `token-<id>.json` in the data directory.
pub fn token_path(provider: &str) -> PathBuf {
    token_path_in(&crate::config::data_dir(), provider)
}

pub fn token_path_in(data_dir: &Path, provider: &str) -> PathBuf {
    data_dir.join(format!("token-{provider}.json"))
}

/// The OAuth app of a provider that signs in this way.
pub fn app(provider: Provider) -> Option<&'static OAuthApp> {
    match &provider.info().sign_in {
        SignIn::OAuth(app) => Some(app),
        _ => None,
    }
}

/// A client id compiled in for `provider`, from "id=client,id=client" (the build's
/// `CLOUDDROP_OAUTH_CLIENTS`).
fn client_from_list(list: &str, provider: &str) -> Option<String> {
    list.split(',')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.trim() == provider)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The client id to sign in with: `[<id>] client_id` (Dropbox calls it `app_key`) in the
/// config, or the one built in.
pub fn client_id(cfg: &Config, provider: Provider) -> Option<String> {
    let section = cfg.provider_section(provider);
    section
        .get("client_id")
        .or_else(|| section.get("app_key"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| client_from_list(option_env!("CLOUDDROP_OAUTH_CLIENTS")?, provider.as_str()))
}

/// How `provider` signs in with this config: its [`OAuthApp`], the client id, and OneDrive's
/// tenant. None: it doesn't use OAuth, or this build has no client id for it.
pub fn client_for(cfg: &Config, provider: Provider) -> Option<OAuthClient> {
    let mut client = OAuthClient::new(app(provider)?, client_id(cfg, provider)?);
    if provider == Provider::OneDrive {
        (client.auth_url, client.token_url) = crate::onedrive::oauth_endpoints(&cfg.onedrive.tenant);
    }
    Some(client)
}

/// Where and as whom to sign in: a provider's [`OAuthApp`] with a client id. The URLs can
/// be pointed elsewhere (tests use a mock).
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthClient {
    pub client_id: String,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub extra_auth_params: Vec<(String, String)>,
    pub redirect_port: Option<u16>,
}

impl OAuthClient {
    pub fn new(app: &OAuthApp, client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            auth_url: app.auth_url.into(),
            token_url: app.token_url.into(),
            scopes: app.scopes.iter().map(|s| s.to_string()).collect(),
            extra_auth_params: app
                .extra_auth_params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            redirect_port: app.redirect_port,
        }
    }

    /// The consent page, sending the browser back to `redirect`.
    pub fn consent_url(&self, redirect: &str, challenge: &str, state: &str) -> Result<url::Url> {
        let mut url = url::Url::parse(&self.auth_url).context("bad sign-in address")?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", redirect)
                .append_pair("response_type", "code")
                .append_pair("code_challenge", challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", state);
            if !self.scopes.is_empty() {
                q.append_pair("scope", &self.scopes.join(" "));
            }
            for (k, v) in &self.extra_auth_params {
                q.append_pair(k, v);
            }
        }
        Ok(url)
    }
}

/// Interactive sign-in: `show_url` gets the consent page (print it, open a browser); once the
/// browser comes back to the loopback address, the code is exchanged (PKCE, no client secret)
/// and the tokens saved in `token_file`.
pub async fn login(client: &OAuthClient, token_file: &Path, show_url: impl FnOnce(&str)) -> Result<()> {
    if client.client_id.trim().is_empty() {
        bail!("no client id for this provider: set one with `clouddrop setup --provider … --set client_id=…`");
    }
    let (verifier, challenge) = pkce();
    let state = random_b64(16);
    let (listener, redirect) = match client.redirect_port {
        Some(port) => {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("port {port} is busy; close what uses it and sign in again"))?;
            (listener, format!("http://localhost:{port}/"))
        }
        None => {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
            (listener, redirect)
        }
    };
    show_url(client.consent_url(&redirect, &challenge, &state)?.as_str());

    let code = tokio::time::timeout(Duration::from_secs(300), wait_for_code(&listener, &state))
        .await
        .map_err(|_| anyhow!("timed out waiting for the browser sign-in"))??;

    let resp = crate::drive::http_client()
        .post(&client.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .context("cannot reach the sign-in server")?;
    if !resp.status().is_success() {
        let status = resp.status();
        bail!("sign-in failed ({status}): {}", resp.text().await.unwrap_or_default().trim());
    }
    let tr: TokenResponse = resp.json().await.context("bad token response")?;
    let refresh_token = tr
        .refresh_token
        .ok_or_else(|| anyhow!("the provider did not return a refresh token, so the sign-in would not last; try again"))?;
    save_token(
        token_file,
        &Token {
            access_token: tr.access_token,
            refresh_token,
            expires_at: unix_now() + tr.expires_in.unwrap_or(3600),
        },
    )
}

struct Loaded {
    token: Token,
    file_mtime: Option<SystemTime>,
}

/// OAuth tokens in a file: a valid access token on request, refreshed before it expires and
/// after a 401, and reloaded when `clouddrop login` rewrites the file.
pub struct TokenFile {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    /// Only Google's installed-app flow has one.
    client_secret: Option<String>,
    /// Who signs in, for messages ("Google rejected the saved sign-in").
    who: String,
    path: PathBuf,
    state: Mutex<Option<Loaded>>,
}

impl TokenFile {
    /// Tokens for `client` (no client secret) in `path`.
    pub fn new(http: reqwest::Client, client: &OAuthClient, path: PathBuf) -> Self {
        Self::with_secret(http, &client.token_url, &client.client_id, None, "the provider", path)
    }

    pub fn with_secret(
        http: reqwest::Client,
        token_url: &str,
        client_id: &str,
        client_secret: Option<&str>,
        who: &str,
        path: PathBuf,
    ) -> Self {
        Self {
            http,
            token_url: token_url.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.map(str::to_string),
            who: who.to_string(),
            path,
            state: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn file_mtime(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path).and_then(|m| m.modified()).ok()
    }

    /// Loads the token file when first needed or when `clouddrop login` rewrote it.
    fn reload_if_needed(&self, slot: &mut Option<Loaded>) -> RemoteResult<()> {
        let mtime = self.file_mtime();
        if slot.as_ref().is_some_and(|l| l.file_mtime == mtime) {
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.path)
            .map_err(|_| RemoteError::Auth("not signed in — run `clouddrop login`".into()))?;
        let token: Token = serde_json::from_str(&text)
            .map_err(|_| RemoteError::Auth("token file is corrupt — run `clouddrop login`".into()))?;
        *slot = Some(Loaded { token, file_mtime: mtime });
        Ok(())
    }

    /// A valid access token, refreshed when it is about to expire.
    pub async fn token(&self) -> RemoteResult<String> {
        let mut slot = self.state.lock().await;
        self.reload_if_needed(&mut slot)?;
        let loaded = slot.as_ref().expect("loaded");
        if loaded.token.expires_at - 60 > unix_now() {
            return Ok(loaded.token.access_token.clone());
        }
        self.refresh_locked(&mut slot).await
    }

    /// A fresh access token (after a 401), or `Auth` when the sign-in is no longer valid.
    pub async fn refresh(&self) -> RemoteResult<String> {
        let mut slot = self.state.lock().await;
        self.reload_if_needed(&mut slot)?;
        self.refresh_locked(&mut slot).await
    }

    async fn refresh_locked(&self, slot: &mut Option<Loaded>) -> RemoteResult<String> {
        let refresh_token = slot.as_ref().expect("loaded").token.refresh_token.clone();
        let mut form = vec![
            ("client_id", self.client_id.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.as_str()));
        }
        let who = &self.who;
        let resp = self
            .http
            .post(&self.token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| RemoteError::Transient(format!("cannot reach {who} sign-in: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status.is_client_error() {
                return Err(RemoteError::Auth(format!(
                    "{who} rejected the saved sign-in ({}) — run `clouddrop login`",
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
        *slot = Some(Loaded { token, file_mtime: self.file_mtime() });
        Ok(access)
    }
}

impl TokenSource for TokenFile {
    fn token(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(TokenFile::token(self))
    }

    fn refresh(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(TokenFile::refresh(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static APP: OAuthApp = OAuthApp {
        auth_url: "https://login.example.com/authorize",
        token_url: "https://login.example.com/token",
        scopes: &["files.readwrite.appfolder", "offline_access"],
        extra_auth_params: &[("token_access_type", "offline")],
        redirect_port: None,
    };

    #[test]
    fn each_provider_signs_in_its_own_way() {
        let mut cfg = Config { provider: Provider::OneDrive, ..Config::default() };
        cfg.onedrive.client_id = "od-client".into();
        cfg.onedrive.tenant = "contoso.onmicrosoft.com".into();
        let od = client_for(&cfg, Provider::OneDrive).unwrap();
        assert_eq!(od.client_id, "od-client");
        assert!(od.auth_url.contains("/contoso.onmicrosoft.com/oauth2/v2.0/authorize"), "{}", od.auth_url);
        assert!(od.token_url.ends_with("/contoso.onmicrosoft.com/oauth2/v2.0/token"));
        assert_eq!(od.redirect_port, None);

        let mut cfg = Config { provider: Provider::Dropbox, ..Config::default() };
        assert_eq!(client_for(&cfg, Provider::Dropbox).map(|c| c.client_id), client_from_list(option_env!("CLOUDDROP_OAUTH_CLIENTS").unwrap_or(""), "dropbox"));
        cfg.dropbox.app_key = "db-key".into();
        let db = client_for(&cfg, Provider::Dropbox).unwrap();
        assert_eq!((db.client_id.as_str(), db.redirect_port), ("db-key", Some(crate::dropbox::REDIRECT_PORT)));
        assert!(client_for(&cfg, Provider::GoogleDrive).is_none(), "Google signs in its own way");
    }

    #[test]
    fn consent_url_has_pkce_scopes_and_extras_but_no_secret() {
        let c = OAuthClient::new(&APP, "abc");
        let url = c.consent_url("http://127.0.0.1:5000", "chal", "st").unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "abc");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5000");
        assert_eq!(q["response_type"], "code");
        assert_eq!((q["code_challenge"].as_str(), q["code_challenge_method"].as_str()), ("chal", "S256"));
        assert_eq!(q["state"], "st");
        assert_eq!(q["scope"], "files.readwrite.appfolder offline_access");
        assert_eq!(q["token_access_type"], "offline");
        assert!(!q.contains_key("client_secret"));
    }

    #[test]
    fn built_in_client_ids() {
        let list = "onedrive=one-id, dropbox = box-id ,s3=";
        assert_eq!(client_from_list(list, "onedrive").as_deref(), Some("one-id"));
        assert_eq!(client_from_list(list, "dropbox").as_deref(), Some("box-id"));
        assert_eq!(client_from_list(list, "s3"), None);
        assert_eq!(client_from_list(list, "mega"), None);
    }

    #[test]
    fn token_files_per_provider() {
        assert_eq!(token_path_in(Path::new("/d"), "dropbox"), Path::new("/d/token-dropbox.json"));
        assert!(app(Provider::GoogleDrive).is_none(), "Google signs in with its own flow");
    }

    #[tokio::test]
    async fn missing_or_corrupt_token_file_needs_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token-x.json");
        let c = OAuthClient::new(&APP, "abc");
        let tokens = TokenFile::new(reqwest::Client::new(), &c, path.clone());
        assert!(matches!(tokens.token().await, Err(RemoteError::Auth(_))));
        std::fs::write(&path, "not json").unwrap();
        assert!(matches!(tokens.token().await, Err(RemoteError::Auth(_))));
        let fresh = Token { access_token: "a".into(), refresh_token: "r".into(), expires_at: unix_now() + 3600 };
        save_token(&path, &fresh).unwrap();
        assert_eq!(tokens.token().await.unwrap(), "a", "a valid token is used without a refresh");
    }
}
