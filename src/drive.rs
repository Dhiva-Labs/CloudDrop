//! Minimal Google Drive v3 client: just the calls CloudDrop needs.

use crate::auth::Auth;
use crate::config::Endpoints;
use futures_util::StreamExt;
use md5::{Digest, Md5};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const FIELDS: &str = "id,name,mimeType,parents,md5Checksum,size,trashed,headRevisionId";

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum DriveError {
    /// Sign-in is missing, expired or revoked; nothing will work until `clouddrop login`.
    #[error("Google sign-in needed: {0}")]
    Auth(String),
    /// Worth retrying later (network, rate limit, server error).
    #[error("{0}")]
    Transient(String),
    #[error("not found in Google Drive")]
    NotFound,
    #[error("{0}")]
    Permanent(String),
}

pub type DResult<T> = Result<T, DriveError>;

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub parents: Vec<String>,
    pub md5_checksum: Option<String>,
    pub size: Option<String>,
    #[serde(default)]
    pub trashed: bool,
    pub head_revision_id: Option<String>,
}

impl FileMeta {
    pub fn size_bytes(&self) -> Option<u64> {
        self.size.as_deref().and_then(|s| s.parse().ok())
    }
}

#[derive(Debug, PartialEq)]
pub enum UploadStatus {
    Done(FileMeta),
    /// Upload continues from this byte offset.
    Next(u64),
}

pub struct Drive {
    http: reqwest::Client,
    ep: Endpoints,
    auth: Arc<Auth>,
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        // Resumable uploads answer 308 without Location; never treat it as a redirect.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(120))
        .user_agent(concat!("clouddrop/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

/// Quotes a value for a Drive search query.
fn q_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

impl Drive {
    pub fn new(http: reqwest::Client, ep: Endpoints, auth: Arc<Auth>) -> Self {
        Self { http, ep, auth }
    }

    /// Sends an authorised request, refreshing the token once on 401.
    /// Returns the response for 2xx and 308; other statuses become errors.
    async fn call(&self, build: impl Fn(&reqwest::Client) -> RequestBuilder) -> DResult<Response> {
        let mut refreshed = false;
        loop {
            let token = if refreshed {
                self.auth.refresh().await?
            } else {
                self.auth.token().await?
            };
            let resp = build(&self.http)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| DriveError::Transient(format!("network error: {e}")))?;
            let st = resp.status();
            if st == StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                continue;
            }
            if st.is_success() || st == StatusCode::PERMANENT_REDIRECT {
                return Ok(resp);
            }
            return Err(classify(resp).await);
        }
    }

    async fn json<T: for<'de> Deserialize<'de>>(resp: Response) -> DResult<T> {
        resp.json()
            .await
            .map_err(|e| DriveError::Transient(format!("bad response from Drive: {e}")))
    }

    /// Storage used and available in the account: (limit, usage, usage in Drive).
    /// A missing limit means unlimited storage.
    pub async fn storage_quota(&self) -> DResult<(Option<u64>, u64, u64)> {
        let url = format!("{}/about", self.ep.api_base);
        let resp = self
            .call(|c| c.get(&url).query(&[("fields", "storageQuota")]))
            .await?;
        let v: serde_json::Value = Self::json(resp).await?;
        let q = &v["storageQuota"];
        let num = |k: &str| q[k].as_str().and_then(|s| s.parse::<u64>().ok());
        Ok((num("limit"), num("usage").unwrap_or(0), num("usageInDrive").unwrap_or(0)))
    }

    pub async fn get(&self, id: &str) -> DResult<FileMeta> {
        let url = format!("{}/files/{id}", self.ep.api_base);
        let resp = self.call(|c| c.get(&url).query(&[("fields", FIELDS)])).await?;
        Self::json(resp).await
    }

    /// Non-trashed items named `name` directly inside `parent` that this app can see.
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> DResult<Vec<FileMeta>> {
        let mut q = format!(
            "name = '{}' and '{}' in parents and trashed = false",
            q_escape(name),
            q_escape(parent)
        );
        if folder {
            q.push_str(&format!(" and mimeType = '{FOLDER_MIME}'"));
        }
        let url = format!("{}/files", self.ep.api_base);
        let fields = format!("files({FIELDS})");
        let resp = self
            .call(|c| {
                c.get(&url).query(&[
                    ("q", q.as_str()),
                    ("fields", fields.as_str()),
                    ("spaces", "drive"),
                    ("pageSize", "100"),
                ])
            })
            .await?;
        #[derive(Deserialize)]
        struct List {
            #[serde(default)]
            files: Vec<FileMeta>,
        }
        let list: List = Self::json(resp).await?;
        Ok(list.files)
    }

    pub async fn create_folder(&self, name: &str, parent: &str) -> DResult<FileMeta> {
        let url = format!("{}/files", self.ep.api_base);
        let body = json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent] });
        let resp = self
            .call(|c| c.post(&url).query(&[("fields", FIELDS)]).json(&body))
            .await?;
        Self::json(resp).await
    }

    /// Renames and/or moves an item. `parents` = (new, old) when moving.
    pub async fn update_meta(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> DResult<FileMeta> {
        let url = format!("{}/files/{id}", self.ep.api_base);
        let mut query = vec![("fields", FIELDS.to_string())];
        if let Some((add, remove)) = parents {
            query.push(("addParents", add.to_string()));
            query.push(("removeParents", remove.to_string()));
        }
        let body = match name {
            Some(n) => json!({ "name": n }),
            None => json!({}),
        };
        let resp = self
            .call(|c| c.patch(&url).query(&query).json(&body))
            .await?;
        Self::json(resp).await
    }

    /// Marks the current head revision "keep forever" so replacing the content never purges it.
    pub async fn pin_head_revision(&self, id: &str) -> DResult<()> {
        let meta = self.get(id).await?;
        let Some(rev) = meta.head_revision_id else {
            return Ok(());
        };
        let url = format!("{}/files/{id}/revisions/{rev}", self.ep.api_base);
        let body = json!({ "keepForever": true });
        self.call(|c| c.patch(&url).query(&[("fields", "id")]).json(&body))
            .await?;
        Ok(())
    }

    /// Opens a resumable upload session; returns the session URL.
    /// `existing` = upload a new revision of that file instead of creating one.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        mime: &str,
    ) -> DResult<String> {
        let (parent, old_parent) = parents;
        let mut query = vec![
            ("uploadType", "resumable".to_string()),
            ("fields", FIELDS.to_string()),
        ];
        let (method, url, body) = match existing {
            None => (
                Method::POST,
                format!("{}/files", self.ep.upload_base),
                json!({ "name": name, "parents": [parent] }),
            ),
            Some(id) => {
                if let Some(old) = old_parent.filter(|o| *o != parent) {
                    query.push(("addParents", parent.to_string()));
                    query.push(("removeParents", old.to_string()));
                }
                (
                    Method::PATCH,
                    format!("{}/files/{id}", self.ep.upload_base),
                    json!({ "name": name }),
                )
            }
        };
        let resp = self
            .call(|c| {
                c.request(method.clone(), &url)
                    .query(&query)
                    .header("X-Upload-Content-Type", mime)
                    .header("X-Upload-Content-Length", size.to_string())
                    .json(&body)
            })
            .await?;
        resp.headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| DriveError::Transient("Drive did not return an upload session".into()))
    }

    /// Asks a session how much it has received. NotFound = the session expired.
    pub async fn query_upload(&self, session: &str, total: u64) -> DResult<UploadStatus> {
        let resp = self
            .call(|c| {
                c.put(session)
                    .header(reqwest::header::CONTENT_RANGE, format!("bytes */{total}"))
                    .header(reqwest::header::CONTENT_LENGTH, "0")
            })
            .await
            .map_err(gone_is_not_found)?;
        upload_status(resp).await
    }

    pub async fn put_chunk(
        &self,
        session: &str,
        data: bytes::Bytes,
        start: u64,
        total: u64,
    ) -> DResult<UploadStatus> {
        let range = if data.is_empty() {
            format!("bytes */{total}")
        } else {
            format!("bytes {start}-{}/{total}", start + data.len() as u64 - 1)
        };
        let resp = self
            .call(|c| {
                c.put(session)
                    .timeout(Duration::from_secs(600))
                    .header(reqwest::header::CONTENT_RANGE, range.clone())
                    .body(data.clone())
            })
            .await
            .map_err(gone_is_not_found)?;
        upload_status(resp).await
    }

    /// Streams a file's content into `out`; returns (md5 hex, bytes written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> DResult<(String, u64)> {
        let url = format!("{}/files/{id}", self.ep.api_base);
        let resp = self
            .call(|c| c.get(&url).query(&[("alt", "media")]).timeout(Duration::from_secs(24 * 3600)))
            .await?;
        let mut stream = resp.bytes_stream();
        let mut hasher = Md5::new();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| DriveError::Transient(format!("download interrupted: {e}")))?;
            hasher.update(&chunk);
            total += chunk.len() as u64;
            out.write_all(&chunk)
                .await
                .map_err(|e| DriveError::Permanent(format!("cannot write download: {e}")))?;
        }
        out.flush()
            .await
            .map_err(|e| DriveError::Permanent(format!("cannot write download: {e}")))?;
        Ok((hex::encode(hasher.finalize()), total))
    }
}

/// Upload session URLs answer 404/410 once expired.
fn gone_is_not_found(e: DriveError) -> DriveError {
    match e {
        DriveError::Permanent(m) if m.starts_with("410") => DriveError::NotFound,
        other => other,
    }
}

async fn upload_status(resp: Response) -> DResult<UploadStatus> {
    if resp.status() == StatusCode::PERMANENT_REDIRECT {
        // "Range: bytes=0-N" lists what Drive has; absent means nothing yet.
        let next = resp
            .headers()
            .get(reqwest::header::RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|r| r.rsplit('-').next())
            .and_then(|n| n.parse::<u64>().ok())
            .map(|end| end + 1)
            .unwrap_or(0);
        return Ok(UploadStatus::Next(next));
    }
    Ok(UploadStatus::Done(Drive::json(resp).await?))
}

async fn classify(resp: Response) -> DriveError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let (reason, message) = google_error(&body);
    let text = format!("{} {}", status.as_u16(), message.as_deref().unwrap_or(status.canonical_reason().unwrap_or("")));
    match status.as_u16() {
        401 => DriveError::Auth(text),
        404 => DriveError::NotFound,
        408 | 429 | 500..=599 => DriveError::Transient(text),
        403 if matches!(
            reason.as_deref(),
            Some("rateLimitExceeded" | "userRateLimitExceeded" | "backendError")
        ) =>
        {
            DriveError::Transient(text)
        }
        _ => DriveError::Permanent(text),
    }
}

/// Extracts (reason, message) from a Google JSON error body.
fn google_error(body: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return (None, None);
    };
    let err = &v["error"];
    let reason = err["errors"][0]["reason"].as_str().map(str::to_string);
    let message = err["message"].as_str().map(str::to_string);
    (reason, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_escaping() {
        assert_eq!(q_escape(r"O'Brien \ co"), r"O\'Brien \\ co");
    }

    #[test]
    fn parses_google_error() {
        let body = r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}],"code":403,"message":"slow down"}}"#;
        assert_eq!(
            google_error(body),
            (Some("userRateLimitExceeded".into()), Some("slow down".into()))
        );
        assert_eq!(google_error("<html>"), (None, None));
    }
}
