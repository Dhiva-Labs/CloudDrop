//! Dropbox API v2 client for an "App folder" app: CloudDrop only sees `Apps/<app name>`, whose
//! root is [`ROOT`]. The engine reaches it through [`crate::remote::Remote`].
//!
//! Ids are Dropbox's `id:…` file ids, which survive renames and moves, and every call that takes
//! an id gets one. The calls that only take paths (making a folder, where a move or copy goes,
//! where a new file goes) look the parent folder's path up first.

use crate::db::Kind;
use crate::hash::Hasher;
use crate::remote::{
    Capabilities, ContentHash, HashKind, OAuthApp, ProviderInfo, RemoteError, RemoteMeta, RemoteResult, SignIn,
    StorageQuota, TokenSource, UploadStatus,
};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use reqwest::{RequestBuilder, Response, StatusCode, header};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::AsyncWriteExt;

/// The loopback port of the browser sign-in; the app's settings on dropbox.com must list
/// `http://localhost:53691/` as a redirect URI.
pub const REDIRECT_PORT: u16 = 53691;

pub static INFO: ProviderInfo = ProviderInfo {
    experimental: false,
    id: "dropbox",
    name: "Dropbox",
    caps: Capabilities {
        hash: Some(HashKind::DropboxContent),
        // File history lasts 30 to 180 days, so a replaced version is kept as a copy.
        pin_versions: false,
        stable_ids: true,
        resumable: true,
        case_insensitive: true,
        // The API refuses backslashes; DEL doesn't survive either.
        forbidden_chars: "\\\u{7f}",
        // Dropbox hashes content in 4 MiB blocks.
        chunk_unit: 4 * 1024 * 1024,
    },
    sign_in: SignIn::OAuth(OAuthApp {
        auth_url: "https://www.dropbox.com/oauth2/authorize",
        token_url: "https://api.dropboxapi.com/oauth2/token",
        scopes: &["files.metadata.read", "files.content.read", "files.content.write", "account_info.read"],
        // A refresh token, not just a four-hour access token.
        extra_auth_params: &[("token_access_type", "offline")],
        // Dropbox matches redirect addresses exactly, port included.
        redirect_port: Some(REDIRECT_PORT),
    }),
    web_url: no_web_url,
};

/// Dropbox's web pages go by path, and an id doesn't say where the item is.
fn no_web_url(_: Kind, _: &str) -> Option<String> {
    None
}

/// The app folder: where the top CloudDrop folder goes.
pub const ROOT: &str = "root";
pub const API: &str = "https://api.dropboxapi.com";
pub const CONTENT: &str = "https://content.dropboxapi.com";

/// `[dropbox]` in config.toml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DropboxConfig {
    /// The Dropbox app's key (its OAuth client id); empty = the one CloudDrop is built with.
    #[serde(alias = "client_id", skip_serializing_if = "String::is_empty")]
    pub app_key: String,
    /// The API and content hosts; overridable so tests can point at a local fake.
    #[serde(skip_serializing_if = "is_default_api")]
    pub api_base: String,
    #[serde(skip_serializing_if = "is_default_content")]
    pub content_base: String,
}

impl Default for DropboxConfig {
    fn default() -> Self {
        DropboxConfig { app_key: String::new(), api_base: API.into(), content_base: CONTENT.into() }
    }
}

impl DropboxConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn is_default_api(s: &str) -> bool {
    s == API
}

fn is_default_content(s: &str) -> bool {
    s == CONTENT
}

/// Stands in for the browser sign-in until it is built: every call asks for a sign-in, so the
/// engine says so instead of failing in other ways.
pub struct NotSignedIn;

impl TokenSource for NotSignedIn {
    fn token(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(async { Err(RemoteError::Auth("Dropbox sign-in isn't available in this build yet".into())) })
    }

    fn refresh(&self) -> BoxFuture<'_, RemoteResult<String>> {
        self.token()
    }
}

/// An item's metadata as Dropbox sends it.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
struct Metadata {
    /// "file", "folder" or "deleted"; absent where the API returns a file's or folder's own type.
    #[serde(rename = ".tag")]
    tag: Option<String>,
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    path_lower: Option<String>,
    path_display: Option<String>,
    size: Option<u64>,
    content_hash: Option<String>,
    rev: Option<String>,
}

impl Metadata {
    fn is_folder(&self) -> bool {
        self.tag.as_deref() == Some("folder")
    }

    /// Where it is, as people see it ("/CloudDrop/Report.pdf").
    fn display(&self) -> RemoteResult<&str> {
        self.path_display
            .as_deref()
            .or(self.path_lower.as_deref())
            .ok_or_else(|| RemoteError::Permanent(format!("Dropbox did not say where \"{}\" is", self.name)))
    }

    /// Where it is, in Dropbox's lowercase (what case-insensitive comparisons use).
    fn lower(&self) -> RemoteResult<String> {
        match &self.path_lower {
            Some(p) => Ok(p.clone()),
            None => Ok(self.display()?.to_lowercase()),
        }
    }

    fn into_meta(self, parent: String) -> RemoteMeta {
        RemoteMeta {
            folder: self.is_folder(),
            size: if self.is_folder() { None } else { self.size },
            hash: self
                .content_hash
                .map(|h| ContentHash { kind: HashKind::DropboxContent, value: h.to_lowercase() }),
            id: self.id,
            name: self.name,
            parents: vec![parent],
            trashed: false,
        }
    }
}

/// A folder's path: (as shown, lowercase); both "" for the app folder.
struct FolderPath {
    display: String,
    lower: String,
}

/// What `start_upload` gives the engine to keep (`entries.upload_url`): enough to finish the
/// upload after a restart.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Session {
    session_id: String,
    /// The folder the file goes into, and its name there.
    parent: String,
    name: String,
    /// New content for this file, replacing revision `rev`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    existing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rev: Option<String>,
}

impl Session {
    /// A session this client can't read is as good as one Dropbox forgot.
    fn parse(s: &str) -> RemoteResult<Self> {
        serde_json::from_str(s).map_err(|_| RemoteError::NotFound)
    }
}

pub struct Dropbox {
    http: reqwest::Client,
    api: String,
    content: String,
    tokens: Arc<dyn TokenSource>,
}

impl Dropbox {
    /// `api` and `content` are the two hosts' base URLs ([`API`] and [`CONTENT`]).
    pub fn new(
        http: reqwest::Client,
        api: impl Into<String>,
        content: impl Into<String>,
        tokens: Arc<dyn TokenSource>,
    ) -> Self {
        Self { http, api: api.into(), content: content.into(), tokens }
    }

    /// Sends an authorised request, refreshing the token once on 401. Returns the response for
    /// 2xx and 409 (the endpoint's own error, for the caller to read); other statuses become errors.
    async fn send(&self, build: impl Fn(&reqwest::Client) -> RequestBuilder) -> RemoteResult<Response> {
        let mut refreshed = false;
        loop {
            let token = if refreshed {
                self.tokens.refresh().await?
            } else {
                self.tokens.token().await?
            };
            let resp = build(&self.http)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| RemoteError::Transient(format!("network error: {e}")))?;
            let st = resp.status();
            if st == StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                continue;
            }
            if st.is_success() || st == StatusCode::CONFLICT {
                return Ok(resp);
            }
            return Err(classify(resp).await);
        }
    }

    /// [`Self::send`], with the endpoint's own errors as errors too.
    async fn call(&self, build: impl Fn(&reqwest::Client) -> RequestBuilder) -> RemoteResult<Response> {
        let resp = self.send(build).await?;
        if resp.status() == StatusCode::CONFLICT {
            return Err(endpoint_error(&json_of(resp).await?));
        }
        Ok(resp)
    }

    /// An RPC call on the API host: JSON in, JSON out.
    async fn rpc<T: DeserializeOwned>(&self, route: &str, arg: &Value) -> RemoteResult<T> {
        let url = format!("{}/2/{route}", self.api);
        json_of(self.call(|c| c.post(&url).json(arg)).await?).await
    }

    /// A request to the content host, with its argument in the `Dropbox-API-Arg` header.
    fn content_call(&self, c: &reqwest::Client, route: &str, arg: &Value) -> RequestBuilder {
        c.post(format!("{}/2/{route}", self.content))
            .header("Dropbox-API-Arg", api_arg(arg))
    }

    /// An upload call on the content host, with `data` as its body.
    fn upload_call(&self, c: &reqwest::Client, route: &str, arg: &Value, data: &bytes::Bytes) -> RequestBuilder {
        self.content_call(c, route, arg)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .timeout(Duration::from_secs(600))
            .body(data.clone())
    }

    /// Metadata of a path or an id; NotFound for deleted items.
    async fn metadata(&self, path: &str) -> RemoteResult<Metadata> {
        let m: Metadata = self
            .rpc("files/get_metadata", &json!({ "path": path, "include_deleted": false }))
            .await?;
        if m.tag.as_deref() == Some("deleted") {
            return Err(RemoteError::NotFound);
        }
        Ok(m)
    }

    /// Where folder `id` is; NotFound when it's gone.
    async fn folder_path(&self, id: &str) -> RemoteResult<FolderPath> {
        if id == ROOT {
            return Ok(FolderPath { display: String::new(), lower: String::new() });
        }
        let m = self.metadata(id).await?;
        Ok(FolderPath { display: m.display()?.to_string(), lower: m.lower()? })
    }

    /// The id of the folder `m` is in.
    async fn parent_id(&self, m: &Metadata) -> RemoteResult<String> {
        let lower = m.lower()?;
        let dir = parent_dir(&lower);
        if dir.is_empty() {
            return Ok(ROOT.into());
        }
        Ok(self.metadata(dir).await?.id)
    }

    /// Space in the account: (allocated, used, used). Team accounts report the member's own
    /// allowance where there is one, else the team's.
    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        let v: Value = self.rpc("users/get_space_usage", &Value::Null).await?;
        let used = v["used"].as_u64().unwrap_or(0);
        let a = &v["allocation"];
        let limit = match a[".tag"].as_str() {
            Some("individual") => a["allocated"].as_u64(),
            Some("team") => a["user_within_team_space_allocated"]
                .as_u64()
                .filter(|n| *n > 0)
                .or(a["allocated"].as_u64()),
            _ => None,
        };
        Ok((limit, used, used))
    }

    /// The app folder.
    pub async fn top_parent(&self) -> RemoteResult<String> {
        Ok(ROOT.into())
    }

    /// Dropbox has no trash flag in metadata: a deleted item is NotFound.
    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        if id == ROOT {
            return Ok(RemoteMeta { id: ROOT.into(), folder: true, ..Default::default() });
        }
        let m = self.metadata(id).await?;
        let parent = self.parent_id(&m).await?;
        Ok(m.into_meta(parent))
    }

    /// The item called `name` (in any case) inside `parent`, if there is one.
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        let dir = self.folder_path(parent).await?;
        match self.metadata(&child(&dir.display, name)).await {
            Ok(m) if !folder || m.is_folder() => Ok(vec![m.into_meta(parent.to_string())]),
            Ok(_) | Err(RemoteError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Makes a folder; where something already has the name, Dropbox picks "name (1)".
    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        let dir = self.folder_path(parent).await?;
        #[derive(Deserialize)]
        struct Created {
            metadata: Metadata,
        }
        let arg = json!({ "path": child(&dir.display, name), "autorename": true });
        let mut m = self.rpc::<Created>("files/create_folder_v2", &arg).await?.metadata;
        m.tag = Some("folder".into());
        Ok(m.into_meta(parent.to_string()))
    }

    /// Renames and/or moves an item. `parents` = (new, old) when moving; only the new one matters.
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        let new_parent = parents.map(|(new, _)| new);
        let m = self.relocate(id, name, new_parent).await?;
        let parent = match new_parent {
            Some(p) => p.to_string(),
            None => self.parent_id(&m).await?,
        };
        Ok(m.into_meta(parent))
    }

    /// Puts `id` at `name` (or its own name) inside `parent` (or where it is); returns its
    /// metadata afterwards. Where the name is taken, Dropbox picks "name (1)".
    async fn relocate(&self, id: &str, name: Option<&str>, parent: Option<&str>) -> RemoteResult<Metadata> {
        let cur = self.metadata(id).await?;
        let cur_lower = cur.lower()?;
        let dir = match parent {
            Some(p) => self.folder_path(p).await?,
            None => FolderPath {
                display: parent_dir(cur.display()?).to_string(),
                lower: parent_dir(&cur_lower).to_string(),
            },
        };
        let name = name.unwrap_or(&cur.name);
        let same_folder = dir.lower == parent_dir(&cur_lower);
        if same_folder && name == cur.name {
            return Ok(cur);
        }
        if same_folder && name.to_lowercase() == cur.name.to_lowercase() {
            // Dropbox doesn't rename by case alone: go through another name first.
            self.move_to(id, &child(&dir.display, &format!(".clouddrop-rename-{name}"))).await?;
        }
        self.move_to(id, &child(&dir.display, name)).await
    }

    async fn move_to(&self, id: &str, to: &str) -> RemoteResult<Metadata> {
        #[derive(Deserialize)]
        struct Moved {
            metadata: Metadata,
        }
        let arg = json!({ "from_path": id, "to_path": to, "autorename": true, "allow_ownership_transfer": false });
        Ok(self.rpc::<Moved>("files/move_v2", &arg).await?.metadata)
    }

    /// Dropbox's file history doesn't last forever, so the current content is kept as a copy
    /// next to the file: "report (replaced 2026-10-21).pdf" (Dropbox adds " (1)" if that's taken).
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        let m = self.metadata(id).await?;
        if m.is_folder() {
            return Ok(());
        }
        let to = child(parent_dir(m.display()?), &replaced_name(&m.name, &today()));
        let arg = json!({ "from_path": id, "to_path": to, "autorename": true, "allow_ownership_transfer": false });
        self.rpc::<Value>("files/copy_v2", &arg).await?;
        Ok(())
    }

    /// Opens an upload session; returns it as JSON (see [`Session`]). `existing` = new content
    /// for that file, which is first renamed/moved to `name` in `parents.0`.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        _size: u64,
        _mime: &str,
    ) -> RemoteResult<String> {
        let parent = parents.0;
        let rev = match existing {
            Some(id) => self.relocate(id, Some(name), Some(parent)).await?.rev,
            None => {
                self.folder_path(parent).await?;
                None
            }
        };
        #[derive(Deserialize)]
        struct Started {
            session_id: String,
        }
        let arg = json!({ "close": false });
        let resp = self
            .call(|c| self.upload_call(c, "files/upload_session/start", &arg, &bytes::Bytes::new()))
            .await?;
        let started: Started = json_of(resp).await?;
        let session = Session {
            session_id: started.session_id,
            parent: parent.to_string(),
            name: name.to_string(),
            existing: existing.map(str::to_string),
            rev,
        };
        Ok(serde_json::to_string(&session).expect("session as JSON"))
    }

    /// How much the session holds: an empty append at offset 0 either works (nothing yet) or
    /// says the right offset. NotFound = Dropbox forgot the session.
    pub async fn query_upload(&self, session: &str, _total: u64) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        self.append(&s, 0, bytes::Bytes::new()).await
    }

    /// Appends a chunk; the last one (or an empty one at the end) finishes the upload.
    pub async fn put_chunk(
        &self,
        session: &str,
        data: bytes::Bytes,
        start: u64,
        total: u64,
    ) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        if start + data.len() as u64 >= total {
            return self.finish(&s, start, data).await;
        }
        self.append(&s, start, data).await
    }

    async fn append(&self, s: &Session, offset: u64, data: bytes::Bytes) -> RemoteResult<UploadStatus> {
        let arg = json!({ "cursor": { "session_id": s.session_id, "offset": offset }, "close": false });
        let end = offset + data.len() as u64;
        let resp = self
            .send(|c| self.upload_call(c, "files/upload_session/append_v2", &arg, &data))
            .await?;
        if resp.status() == StatusCode::CONFLICT {
            return session_error(&json_of(resp).await?);
        }
        Ok(UploadStatus::Next(end))
    }

    /// Commits the session with its last bytes: a new file "name" in the parent, or the new
    /// revision of the existing file (only if it is still the revision `start_upload` saw;
    /// otherwise Dropbox keeps both, naming the new one "name (1)").
    async fn finish(&self, s: &Session, offset: u64, data: bytes::Bytes) -> RemoteResult<UploadStatus> {
        let (path, mode) = match (&s.existing, &s.rev) {
            (Some(id), Some(rev)) => (id.clone(), json!({ ".tag": "update", "update": rev })),
            (Some(id), None) => (id.clone(), json!("overwrite")),
            (None, _) => (child(&self.folder_path(&s.parent).await?.display, &s.name), json!("add")),
        };
        let arg = json!({
            "cursor": { "session_id": s.session_id, "offset": offset },
            "commit": { "path": path, "mode": mode, "autorename": true, "mute": true },
        });
        let resp = self
            .send(|c| self.upload_call(c, "files/upload_session/finish", &arg, &data))
            .await?;
        if resp.status() == StatusCode::CONFLICT {
            let err = session_error(&json_of(resp).await?);
            if let (Some(id), Err(RemoteError::Permanent(_))) = (&s.existing, &err) {
                // Committing to a file deleted meanwhile: NotFound, so the engine starts afresh.
                self.metadata(id).await?;
            }
            return err;
        }
        let m: Metadata = json_of(resp).await?;
        Ok(UploadStatus::Done(m.into_meta(s.parent.clone())))
    }

    /// Bytes `start..end` of a file's content (for downloading placeholders on demand).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes> {
        if end <= start {
            return Ok(bytes::Bytes::new());
        }
        let arg = json!({ "path": id });
        let range = format!("bytes={start}-{}", end - 1);
        let resp = self
            .call(|c| {
                self.content_call(c, "files/download", &arg)
                    .header(header::RANGE, range.clone())
                    .timeout(Duration::from_secs(600))
            })
            .await?;
        let partial = resp.status() == StatusCode::PARTIAL_CONTENT;
        let body = resp
            .bytes()
            .await
            .map_err(|e| RemoteError::Transient(format!("download interrupted: {e}")))?;
        if partial {
            return Ok(body);
        }
        // The whole file came back.
        let len = body.len() as u64;
        Ok(body.slice(start.min(len) as usize..end.min(len) as usize))
    }

    /// Streams a file's content into `out`; returns (its Dropbox content hash, bytes written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        let arg = json!({ "path": id });
        let resp = self
            .call(|c| self.content_call(c, "files/download", &arg).timeout(Duration::from_secs(24 * 3600)))
            .await?;
        let mut stream = resp.bytes_stream();
        let mut hasher = Hasher::new(HashKind::DropboxContent);
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| RemoteError::Transient(format!("download interrupted: {e}")))?;
            hasher.update(&chunk);
            total += chunk.len() as u64;
            out.write_all(&chunk)
                .await
                .map_err(|e| RemoteError::Permanent(format!("cannot write download: {e}")))?;
        }
        out.flush()
            .await
            .map_err(|e| RemoteError::Permanent(format!("cannot write download: {e}")))?;
        Ok((hasher.finish(), total))
    }
}

async fn json_of<T: DeserializeOwned>(resp: Response) -> RemoteResult<T> {
    resp.json()
        .await
        .map_err(|e| RemoteError::Transient(format!("bad response from Dropbox: {e}")))
}

/// "/a/b.txt" → "/a"; "/b.txt" → "" (the app folder).
fn parent_dir(path: &str) -> &str {
    &path[..path.rfind('/').unwrap_or(0)]
}

fn child(dir: &str, name: &str) -> String {
    format!("{dir}/{name}")
}

/// "report.pdf" → "report (replaced 2026-10-21).pdf"; ".bashrc" → ".bashrc (replaced 2026-10-21)".
fn replaced_name(name: &str, date: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} (replaced {date}){}", &name[..i], &name[i..]),
        _ => format!("{name} (replaced {date})"),
    }
}

/// Today's date (UTC) as YYYY-MM-DD.
fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_date(secs / 86_400);
    format!("{y:04}-{m:02}-{d:02}")
}

/// (year, month, day) of a day counted from 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

/// JSON for the `Dropbox-API-Arg` header, which must be ASCII: everything else (and DEL) goes
/// as `\u` escapes, which only ever occur inside JSON strings.
fn api_arg(v: &Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        if c.is_ascii() && c != '\u{7f}' {
            out.push(c);
        } else {
            for u in c.encode_utf16(&mut [0u16; 2]) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

/// An upload session's error: where Dropbox says the upload really is, or the error.
fn session_error(v: &Value) -> RemoteResult<UploadStatus> {
    let e = &v["error"];
    match e["correct_offset"].as_u64().or(e["lookup_failed"]["correct_offset"].as_u64()) {
        Some(offset) => Ok(UploadStatus::Next(offset)),
        None => Err(endpoint_error(v)),
    }
}

/// The parts of an `error_summary` ("path/not_found/..." → ["path", "not_found"]).
fn summary_tags(v: &Value) -> Vec<&str> {
    v["error_summary"]
        .as_str()
        .unwrap_or("")
        .split('/')
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.chars().all(|c| c == '.'))
        .collect()
}

/// An endpoint's own error (HTTP 409).
fn endpoint_error(v: &Value) -> RemoteError {
    let tags = summary_tags(v);
    let has = |t: &str| tags.contains(&t);
    if has("insufficient_space") || has("insufficient_quota") {
        RemoteError::Quota("your Dropbox is full".into())
    } else if has("not_found") {
        RemoteError::NotFound
    } else if has("too_many_write_operations") {
        RemoteError::RateLimited {
            message: "Dropbox is busy with other changes to this account".into(),
            retry_after: None,
        }
    } else if has("internal_error") {
        RemoteError::Transient("Dropbox had an internal error".into())
    } else if has("disallowed_name") || has("malformed_path") {
        RemoteError::Permanent("Dropbox does not accept this name".into())
    } else if has("too_large") {
        RemoteError::Permanent("the file is larger than Dropbox accepts".into())
    } else {
        RemoteError::Permanent(format!("Dropbox refused the change ({})", tags.join("/")))
    }
}

async fn classify(resp: Response) -> RemoteError {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let body = resp.text().await.unwrap_or_default();
    classify_parts(status, retry_after, &body)
}

/// Maps an HTTP failure (not 2xx; 409 included) to an error.
fn classify_parts(status: u16, retry_after: Option<u64>, body: &str) -> RemoteError {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let detail = match v["error_summary"].as_str() {
        Some(_) => summary_tags(&v).join("/"),
        None => body.trim().chars().take(200).collect(),
    };
    let slow_down = |retry_after: Option<u64>| RemoteError::RateLimited {
        message: "Dropbox asked CloudDrop to slow down".into(),
        retry_after: retry_after.or(v["error"]["retry_after"].as_u64()),
    };
    match status {
        401 => RemoteError::Auth(format!("Dropbox did not accept the sign-in ({detail})")),
        409 => endpoint_error(&v),
        429 => slow_down(retry_after),
        // Dropbox's documented way to say "rate limited" on older apps.
        503 if retry_after.is_some() => slow_down(retry_after),
        408 | 500..=599 => RemoteError::Transient(format!("Dropbox is having trouble ({status})")),
        _ => RemoteError::Permanent(format!("Dropbox rejected a request ({status}: {detail})")),
    }
}

#[cfg(test)]
#[path = "../tests/common/dropbox_mock.rs"]
mod mock;

#[cfg(test)]
mod tests {
    use super::*;
    use mock::{Fault, Shared};
    use std::sync::Mutex;

    /// Access tokens from the mock's token endpoint, like the real sign-in.
    struct Tokens {
        http: reqwest::Client,
        token_url: String,
        current: Mutex<String>,
    }

    impl TokenSource for Tokens {
        fn token(&self) -> BoxFuture<'_, RemoteResult<String>> {
            let t = self.current.lock().unwrap().clone();
            Box::pin(async move { Ok(t) })
        }

        fn refresh(&self) -> BoxFuture<'_, RemoteResult<String>> {
            Box::pin(async move {
                let form = [("grant_type", "refresh_token"), ("refresh_token", "good-refresh"), ("client_id", "app")];
                let resp = self
                    .http
                    .post(&self.token_url)
                    .form(&form)
                    .send()
                    .await
                    .map_err(|e| RemoteError::Transient(e.to_string()))?;
                if !resp.status().is_success() {
                    return Err(RemoteError::Auth("the refresh token was revoked".into()));
                }
                let v: Value = resp.json().await.map_err(|e| RemoteError::Transient(e.to_string()))?;
                let t = v["access_token"].as_str().unwrap().to_string();
                *self.current.lock().unwrap() = t.clone();
                Ok(t)
            })
        }
    }

    async fn setup() -> (Dropbox, Shared) {
        let mock = mock::start().await;
        let (api, content, token) = {
            let mut st = mock.lock().unwrap();
            (st.api.clone(), st.content.clone(), st.issue_token())
        };
        let http = crate::drive::http_client();
        let tokens =
            Tokens { http: http.clone(), token_url: format!("{api}/oauth2/token"), current: Mutex::new(token) };
        (Dropbox::new(http, api, content, Arc::new(tokens)), mock)
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /// Bytes that differ from block to block.
    fn data(len: usize) -> Vec<u8> {
        (0..len as u32).map(|i| (i * 31 % 251) as u8).collect()
    }

    const MIB: usize = 1024 * 1024;

    /// Uploads `content` in `chunk`-sized pieces, the way the engine does.
    async fn upload(
        db: &Dropbox,
        existing: Option<&str>,
        name: &str,
        parent: &str,
        content: &[u8],
        chunk: usize,
    ) -> RemoteMeta {
        let total = content.len() as u64;
        let session = db.start_upload(existing, name, (parent, None), total, "text/plain").await.unwrap();
        let mut offset = 0u64;
        loop {
            let end = (offset as usize + chunk).min(content.len());
            let piece = bytes::Bytes::copy_from_slice(&content[offset as usize..end]);
            match db.put_chunk(&session, piece, offset, total).await.unwrap() {
                UploadStatus::Done(m) => return m,
                UploadStatus::Next(n) => offset = n,
            }
        }
    }

    fn dropbox_hash(content: &[u8]) -> ContentHash {
        let mut h = Hasher::new(HashKind::DropboxContent);
        h.update(content);
        h.finish()
    }

    #[test]
    fn provider_info() {
        assert_eq!((INFO.id, INFO.name), ("dropbox", "Dropbox"));
        let caps = &INFO.caps;
        assert_eq!(caps.hash, Some(HashKind::DropboxContent));
        assert!(!caps.pin_versions && caps.stable_ids && caps.resumable && caps.case_insensitive);
        assert_eq!(caps.chunk_unit, 4 * MIB);
        let SignIn::OAuth(app) = &INFO.sign_in else { panic!("Dropbox signs in with OAuth") };
        assert_eq!(app.auth_url, "https://www.dropbox.com/oauth2/authorize");
        assert_eq!(app.token_url, "https://api.dropboxapi.com/oauth2/token");
        assert_eq!(app.extra_auth_params, &[("token_access_type", "offline")]);
        assert!(app.scopes.contains(&"files.content.write"));
        assert_eq!((INFO.web_url)(Kind::File, "id:abc"), None);
    }

    #[test]
    fn config_section_stays_out_of_the_file_until_set() {
        #[derive(Serialize, Deserialize)]
        struct C {
            #[serde(default, skip_serializing_if = "DropboxConfig::is_default")]
            dropbox: DropboxConfig,
        }
        let empty: C = toml::from_str("").unwrap();
        assert!(empty.dropbox.is_default());
        assert_eq!(toml::to_string(&empty).unwrap().trim(), "");
        let set: C = toml::from_str("[dropbox]\napp_key = \"k1\"").unwrap();
        assert_eq!((set.dropbox.app_key.as_str(), set.dropbox.api_base.as_str()), ("k1", API));
        assert_eq!(toml::to_string(&set).unwrap().trim(), "[dropbox]\napp_key = \"k1\"");
        let alias: C = toml::from_str("[dropbox]\nclient_id = \"k2\"").unwrap();
        assert_eq!(alias.dropbox.app_key, "k2");
    }

    #[test]
    fn header_argument_is_ascii() {
        let v = json!({ "path": "/café ☕ 𝄞\u{7f}.txt" });
        let arg = api_arg(&v);
        assert!(arg.is_ascii());
        for escaped in [r"\u00e9", r"\u2615", r"\ud834\udd1e", r"\u007f"] {
            assert!(arg.contains(escaped), "{arg} has {escaped}");
        }
        assert_eq!(serde_json::from_str::<Value>(&arg).unwrap(), v);
    }

    #[test]
    fn names_and_dates() {
        assert_eq!(replaced_name("report.pdf", "2026-10-21"), "report (replaced 2026-10-21).pdf");
        assert_eq!(replaced_name("a.tar.gz", "2026-10-21"), "a.tar (replaced 2026-10-21).gz");
        assert_eq!(replaced_name(".bashrc", "2026-10-21"), ".bashrc (replaced 2026-10-21)");
        assert_eq!(replaced_name("README", "2026-10-21"), "README (replaced 2026-10-21)");
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(19_782), (2024, 2, 29));
        assert_eq!(civil_date(20_747), (2026, 10, 21));
        assert_eq!(civil_date(10_956), (1999, 12, 31));
        assert_eq!((parent_dir("/a/b.txt"), parent_dir("/b.txt")), ("/a", ""));
    }

    #[test]
    fn error_mapping() {
        let e = classify_parts;
        let expired = r#"{"error_summary": "expired_access_token/..", "error": {".tag": "expired_access_token"}}"#;
        assert!(matches!(e(401, None, expired), RemoteError::Auth(m) if m.contains("expired_access_token")));
        let slow = r#"{"error_summary": "too_many_requests/...", "error": {"reason": {".tag": "too_many_requests"}, "retry_after": 9}}"#;
        assert_eq!(
            e(429, Some(5), slow),
            RemoteError::RateLimited { message: "Dropbox asked CloudDrop to slow down".into(), retry_after: Some(5) }
        );
        assert!(matches!(e(429, None, slow), RemoteError::RateLimited { retry_after: Some(9), .. }));
        assert!(matches!(e(503, Some(2), ""), RemoteError::RateLimited { retry_after: Some(2), .. }));
        assert!(matches!(e(503, None, ""), RemoteError::Transient(_)));
        assert!(matches!(e(500, None, "oops"), RemoteError::Transient(_)));
        let full = r#"{"error_summary": "path/insufficient_space/..", "error": {".tag": "path", "path": {".tag": "insufficient_space"}}}"#;
        assert!(matches!(e(409, None, full), RemoteError::Quota(_)));
        let quota = r#"{"error_summary": "insufficient_quota/.", "error": {".tag": "insufficient_quota"}}"#;
        assert!(matches!(e(409, None, quota), RemoteError::Quota(_)));
        let gone = r#"{"error_summary": "path/not_found/...", "error": {".tag": "path", "path": {".tag": "not_found"}}}"#;
        assert_eq!(e(409, None, gone), RemoteError::NotFound);
        let lookup = r#"{"error_summary": "path_lookup/not_found/..", "error": {}}"#;
        assert_eq!(e(409, None, lookup), RemoteError::NotFound);
        let busy = r#"{"error_summary": "path/too_many_write_operations/..", "error": {}}"#;
        assert!(e(409, None, busy).is_transient());
        let clash = r#"{"error_summary": "path/conflict/file/..", "error": {}}"#;
        assert_eq!(e(409, None, clash), RemoteError::Permanent("Dropbox refused the change (path/conflict/file)".into()));
        assert!(matches!(e(400, None, "Error in call to API function"), RemoteError::Permanent(m) if m.contains("400")));
        // A wrong URL is not a missing file.
        assert!(matches!(e(404, None, "Not Found"), RemoteError::Permanent(_)));
    }

    #[test]
    fn upload_session_errors() {
        let offset = json!({"error_summary": "incorrect_offset/..", "error": {".tag": "incorrect_offset", "correct_offset": 7}});
        assert_eq!(session_error(&offset), Ok(UploadStatus::Next(7)));
        let finish = json!({"error_summary": "lookup_failed/incorrect_offset/..",
            "error": {".tag": "lookup_failed", "lookup_failed": {".tag": "incorrect_offset", "correct_offset": 9}}});
        assert_eq!(session_error(&finish), Ok(UploadStatus::Next(9)));
        let gone = json!({"error_summary": "lookup_failed/not_found/..", "error": {".tag": "lookup_failed"}});
        assert_eq!(session_error(&gone), Err(RemoteError::NotFound));
        assert_eq!(Session::parse("https://www.googleapis.com/upload/x"), Err(RemoteError::NotFound));
    }

    #[tokio::test]
    async fn folders_and_lookups() {
        let (db, mock) = setup().await;
        let top = db.top_parent().await.unwrap();
        assert_eq!(top, ROOT);
        assert!(db.find_child("CloudDrop", &top, true).await.unwrap().is_empty());
        let root = db.create_folder("CloudDrop", &top).await.unwrap();
        assert!(root.folder && root.id.starts_with("id:"));
        assert_eq!((root.name.as_str(), root.parents.as_slice()), ("CloudDrop", &[ROOT.to_string()][..]));
        let sub = db.create_folder("Photos", &root.id).await.unwrap();
        assert_eq!(mock.lock().unwrap().tree(), pairs(&[("/CloudDrop/", ""), ("/CloudDrop/Photos/", "")]));

        // Names are case-insensitive; the result carries Dropbox's own spelling.
        let found = db.find_child("cloudDROP", &top, true).await.unwrap();
        assert_eq!(found, vec![root.clone()]);
        assert_eq!(db.get(&sub.id).await.unwrap(), sub);
        assert_eq!(db.get(&root.id).await.unwrap().parents, vec![ROOT.to_string()]);

        // A file isn't a folder; without `folder` it is found too.
        mock.lock().unwrap().put_file("/CloudDrop/notes.txt", b"hi");
        assert!(db.find_child("notes.txt", &root.id, true).await.unwrap().is_empty());
        let notes = db.find_child("Notes.TXT", &root.id, false).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].hash, Some(dropbox_hash(b"hi")));
        assert_eq!((notes[0].size, notes[0].folder), (Some(2), false));

        // A name already taken gets Dropbox's " (1)".
        let again = db.create_folder("photos", &root.id).await.unwrap();
        assert_eq!(again.name, "photos (1)");

        // Deleted on dropbox.com: NotFound, never "trashed".
        mock.lock().unwrap().delete("/CloudDrop/Photos");
        assert_eq!(db.get(&sub.id).await, Err(RemoteError::NotFound));
        assert_eq!(db.find_child("x", &sub.id, false).await, Err(RemoteError::NotFound));
        assert_eq!(db.create_folder("x", &sub.id).await, Err(RemoteError::NotFound));
        assert!(db.find_child("Photos", &root.id, true).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn uploads_multi_block_files_and_hashes_agree() {
        let (db, mock) = setup().await;
        let root = db.create_folder("CloudDrop", ROOT).await.unwrap();
        let content = data(9 * MIB + 12_345);
        let m = upload(&db, None, "big.bin", &root.id, &content, 4 * MIB).await;
        let want = dropbox_hash(&content);
        assert_eq!(want.value, mock::content_hash(&content), "the client and the mock agree");
        assert_eq!(m.hash, Some(want.clone()));
        assert!(m.same_content(&want, content.len() as u64));
        assert_eq!((m.name.as_str(), m.parents.as_slice()), ("big.bin", &[root.id.clone()][..]));
        {
            let st = mock.lock().unwrap();
            assert_eq!(st.lookup("/CloudDrop/big.bin").unwrap().content, content);
            assert_eq!(st.count("files/upload_session/append_v2"), 2);
            assert_eq!(st.count("files/upload_session/finish"), 1);
        }
        assert_eq!(db.get(&m.id).await.unwrap(), m);

        // Downloads hash what they wrote, the same way.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        let mut out = tokio::fs::File::create(&path).await.unwrap();
        let (hash, n) = db.download(&m.id, &mut out).await.unwrap();
        assert_eq!((hash, n), (want, content.len() as u64));
        assert_eq!(std::fs::read(&path).unwrap(), content);
        let range = db.download_range(&m.id, 4 * MIB as u64 - 3, 4 * MIB as u64 + 5).await.unwrap();
        assert_eq!(range.as_ref(), &content[4 * MIB - 3..4 * MIB + 5]);
        assert!(db.download_range(&m.id, 7, 7).await.unwrap().is_empty());
        assert_eq!(db.download_range("id:gone", 0, 4).await, Err(RemoteError::NotFound));

        // Small, empty and non-ASCII files take the same path.
        let small = upload(&db, None, "café ☕.txt", &root.id, b"hello", 4 * MIB).await;
        assert_eq!(small.hash.unwrap().value, "9595c9df90075148eb06860365df33584b75bff782a510c6cd4883a419833d50");
        let empty = upload(&db, None, "empty", &root.id, b"", 4 * MIB).await;
        assert_eq!((empty.size, empty.hash), (Some(0), Some(dropbox_hash(b""))));
        assert_eq!(mock.lock().unwrap().lookup("/cloudDrop/CAFÉ ☕.TXT").unwrap().content, b"hello");
    }

    #[tokio::test]
    async fn resumes_after_a_cut() {
        let (db, mock) = setup().await;
        let content = data(10 * MIB);
        let total = content.len() as u64;
        let chunk = |a: usize, b: usize| bytes::Bytes::copy_from_slice(&content[a..b]);
        let session = db.start_upload(None, "video.mp4", (ROOT, None), total, "video/mp4").await.unwrap();
        assert_eq!(db.query_upload(&session, total).await, Ok(UploadStatus::Next(0)));
        assert_eq!(db.put_chunk(&session, chunk(0, 4 * MIB), 0, total).await, Ok(UploadStatus::Next(4 * MIB as u64)));

        // Dropbox takes the second chunk, but the reply never arrives.
        mock.lock().unwrap().fail("files/upload_session/append_v2", Fault::LostReply);
        let cut = db.put_chunk(&session, chunk(4 * MIB, 8 * MIB), 4 * MIB as u64, total).await;
        assert!(matches!(cut, Err(RemoteError::Transient(_))), "{cut:?}");

        // After a restart the session says where it is (incorrect_offset), and so does a
        // chunk sent again from the old offset.
        assert_eq!(db.query_upload(&session, total).await, Ok(UploadStatus::Next(8 * MIB as u64)));
        let again = db.put_chunk(&session, chunk(4 * MIB, 8 * MIB), 4 * MIB as u64, total).await;
        assert_eq!(again, Ok(UploadStatus::Next(8 * MIB as u64)));

        let m = match db.put_chunk(&session, chunk(8 * MIB, 10 * MIB), 8 * MIB as u64, total).await.unwrap() {
            UploadStatus::Done(m) => m,
            other => panic!("{other:?}"),
        };
        assert_eq!(m.hash, Some(dropbox_hash(&content)));
        assert_eq!(mock.lock().unwrap().lookup("/video.mp4").unwrap().content, content);

        // A finished (or forgotten) session is NotFound, as is one this client can't read.
        assert_eq!(db.query_upload(&session, total).await, Err(RemoteError::NotFound));
        let other = db.start_upload(None, "x", (ROOT, None), 3, "").await.unwrap();
        mock.lock().unwrap().expire_sessions();
        assert_eq!(db.put_chunk(&other, bytes::Bytes::from_static(b"abc"), 0, 3).await, Err(RemoteError::NotFound));
        assert_eq!(db.query_upload("not json", 3).await, Err(RemoteError::NotFound));
        assert_eq!(db.start_upload(None, "x", ("id:gone", None), 3, "").await, Err(RemoteError::NotFound));
    }

    #[tokio::test]
    async fn a_lost_finishing_reply_does_not_duplicate_the_file() {
        let (db, mock) = setup().await;
        let session = db.start_upload(None, "a.txt", (ROOT, None), 3, "").await.unwrap();
        mock.lock().unwrap().fail("files/upload_session/finish", Fault::LostReply);
        assert!(db.put_chunk(&session, bytes::Bytes::from_static(b"abc"), 0, 3).await.is_err());
        // Dropbox committed the file and forgot the session, so the engine uploads again; the
        // same content at the same name is no conflict.
        assert_eq!(db.query_upload(&session, 3).await, Err(RemoteError::NotFound));
        let m = upload(&db, None, "a.txt", ROOT, b"abc", 4 * MIB).await;
        assert_eq!(m.name, "a.txt");
        assert_eq!(mock.lock().unwrap().tree(), pairs(&[("/a.txt", "abc")]));
    }

    #[tokio::test]
    async fn replacing_content_keeps_the_old_version() {
        let (db, mock) = setup().await;
        let root = db.create_folder("CloudDrop", ROOT).await.unwrap();
        let old = upload(&db, None, "report.pdf", &root.id, b"version 1", 4 * MIB).await;
        db.keep_version(&old.id).await.unwrap();
        let new = upload(&db, Some(&old.id), "report.pdf", &root.id, b"version 2", 4 * MIB).await;
        assert_eq!(new.id, old.id, "the same file, with new content");
        assert_eq!(new.hash, Some(dropbox_hash(b"version 2")));
        let kept = format!("/CloudDrop/{}", replaced_name("report.pdf", &today()));
        assert_eq!(
            mock.lock().unwrap().tree(),
            pairs(&[("/CloudDrop/", ""), (&kept, "version 1"), ("/CloudDrop/report.pdf", "version 2")])
        );

        // Kept twice on one day: both copies stay.
        db.keep_version(&old.id).await.unwrap();
        let second = kept.replace(").pdf", ") (1).pdf");
        assert_eq!(mock.lock().unwrap().lookup(&second).unwrap().content, b"version 2");
        assert_eq!(db.keep_version(&root.id).await, Ok(()), "folders have nothing to keep");

        // New content for a renamed and moved file: it moves first, then takes the content.
        let sub = db.create_folder("Archive", &root.id).await.unwrap();
        let moved = upload(&db, Some(&old.id), "final.pdf", &sub.id, b"version 3", 4 * MIB).await;
        assert_eq!((moved.id.as_str(), moved.parents.as_slice()), (old.id.as_str(), &[sub.id.clone()][..]));
        assert_eq!(mock.lock().unwrap().lookup("/CloudDrop/Archive/final.pdf").unwrap().content, b"version 3");

        // Changed on dropbox.com during the upload: Dropbox keeps both instead of overwriting.
        let session = db.start_upload(Some(&old.id), "final.pdf", (&sub.id, None), 9, "").await.unwrap();
        {
            let mut st = mock.lock().unwrap();
            let i = st.items.get_mut(&old.id).unwrap();
            i.content = b"edited on the web".to_vec();
            i.rev = "0000000000ff".into();
        }
        let done = db.put_chunk(&session, bytes::Bytes::from_static(b"version 4"), 0, 9).await.unwrap();
        let UploadStatus::Done(m) = done else { panic!("{done:?}") };
        assert_ne!(m.id, old.id);
        let st = mock.lock().unwrap();
        assert_eq!(st.lookup("/CloudDrop/Archive/final.pdf").unwrap().content, b"edited on the web");
        assert_eq!(st.lookup("/CloudDrop/Archive/final (1).pdf").unwrap().content, b"version 4");
    }

    #[tokio::test]
    async fn new_file_never_overwrites_another() {
        let (db, mock) = setup().await;
        mock.lock().unwrap().put_file("/notes.txt", b"theirs");
        let m = upload(&db, None, "notes.txt", ROOT, b"mine", 4 * MIB).await;
        assert_eq!(m.name, "notes (1).txt");
        let st = mock.lock().unwrap();
        assert_eq!(st.lookup("/notes.txt").unwrap().content, b"theirs");
        assert_eq!(st.lookup("/notes (1).txt").unwrap().content, b"mine");
    }

    #[tokio::test]
    async fn uploading_to_a_file_deleted_meanwhile_is_not_found() {
        let (db, mock) = setup().await;
        let old = upload(&db, None, "a.txt", ROOT, b"one", 4 * MIB).await;
        let session = db.start_upload(Some(&old.id), "a.txt", (ROOT, None), 3, "").await.unwrap();
        mock.lock().unwrap().delete("/a.txt");
        assert_eq!(db.put_chunk(&session, bytes::Bytes::from_static(b"two"), 0, 3).await, Err(RemoteError::NotFound));
        assert_eq!(db.start_upload(Some(&old.id), "a.txt", (ROOT, None), 3, "").await, Err(RemoteError::NotFound));
        assert_eq!(db.keep_version(&old.id).await, Err(RemoteError::NotFound));
    }

    #[tokio::test]
    async fn moves_and_renames_keep_ids() {
        let (db, mock) = setup().await;
        let root = db.create_folder("CloudDrop", ROOT).await.unwrap();
        let a = db.create_folder("A", &root.id).await.unwrap();
        let f = upload(&db, None, "Report.txt", &root.id, b"r", 4 * MIB).await;

        let renamed = db.move_rename(&f.id, Some("Summary.txt"), None).await.unwrap();
        assert_eq!((renamed.id.as_str(), renamed.name.as_str()), (f.id.as_str(), "Summary.txt"));
        assert_eq!(renamed.parents, vec![root.id.clone()]);
        let moved = db.move_rename(&f.id, None, Some((&a.id, &root.id))).await.unwrap();
        assert_eq!((moved.name.as_str(), moved.parents.as_slice()), ("Summary.txt", &[a.id.clone()][..]));
        let both = db.move_rename(&f.id, Some("x.txt"), Some((&root.id, &a.id))).await.unwrap();
        assert_eq!((both.name.as_str(), both.parents.as_slice()), ("x.txt", &[root.id.clone()][..]));

        // By case alone, which Dropbox's move can't do in one step.
        let cased = db.move_rename(&f.id, Some("X.txt"), None).await.unwrap();
        assert_eq!(cased.name, "X.txt");
        let calls = mock.lock().unwrap().count("files/move_v2");
        assert_eq!(db.move_rename(&f.id, Some("X.txt"), None).await.unwrap().name, "X.txt");
        assert_eq!(mock.lock().unwrap().count("files/move_v2"), calls, "already there: nothing to do");

        // Folders move with what's in them; the name taken gets " (1)".
        db.create_folder("B", &root.id).await.unwrap();
        let folder = db.move_rename(&a.id, Some("B"), None).await.unwrap();
        assert_eq!((folder.id.as_str(), folder.name.as_str(), folder.folder), (a.id.as_str(), "B (1)", true));
        assert_eq!(
            mock.lock().unwrap().tree(),
            pairs(&[("/CloudDrop/", ""), ("/CloudDrop/B (1)/", ""), ("/CloudDrop/B/", ""), ("/CloudDrop/X.txt", "r")])
        );
        assert_eq!(db.move_rename("id:gone", Some("y"), None).await, Err(RemoteError::NotFound));
    }

    #[tokio::test]
    async fn quota() {
        let (db, mock) = setup().await;
        upload(&db, None, "a", ROOT, &data(1000), 4 * MIB).await;
        assert_eq!(db.storage_quota().await, Ok((Some(2 << 30), 1000, 1000)));

        // Full: a new file, a replacement and a kept copy are refused as Quota.
        mock.lock().unwrap().allocated = 1500;
        let session = db.start_upload(None, "b", (ROOT, None), 600, "").await.unwrap();
        let full = db.put_chunk(&session, bytes::Bytes::from(data(600)), 0, 600).await;
        assert!(matches!(full, Err(RemoteError::Quota(_))), "{full:?}");
        let a = db.find_child("a", ROOT, false).await.unwrap().remove(0);
        assert!(matches!(db.keep_version(&a.id).await, Err(RemoteError::Quota(_))));
    }

    #[tokio::test]
    async fn expired_tokens_refresh_once() {
        let (db, mock) = setup().await;
        mock.lock().unwrap().expire_tokens();
        assert!(db.storage_quota().await.is_ok());
        assert_eq!(mock.lock().unwrap().refreshes, 1);
        // Uploads (content host) refresh the same way.
        mock.lock().unwrap().expire_tokens();
        upload(&db, None, "a", ROOT, b"abc", 4 * MIB).await;
        assert_eq!(mock.lock().unwrap().refreshes, 2);

        // The sign-in itself was revoked: Auth.
        {
            let mut st = mock.lock().unwrap();
            st.expire_tokens();
            st.refresh_token = "revoked".into();
        }
        assert!(matches!(db.storage_quota().await, Err(RemoteError::Auth(_))));
    }

    #[tokio::test]
    async fn without_a_sign_in_everything_asks_for_one() {
        let db = Dropbox::new(crate::drive::http_client(), API, CONTENT, Arc::new(NotSignedIn));
        assert!(matches!(db.get("id:x").await, Err(RemoteError::Auth(_))));
        assert!(matches!(db.start_upload(None, "a", (ROOT, None), 1, "").await, Err(RemoteError::Auth(_))));
    }

    #[tokio::test]
    async fn rate_limits_and_server_errors() {
        let (db, mock) = setup().await;
        mock.lock().unwrap().fail("files/get_metadata", Fault::RateLimited(7));
        let err = db.get("id:x").await.unwrap_err();
        assert_eq!(err, RemoteError::RateLimited { message: "Dropbox asked CloudDrop to slow down".into(), retry_after: Some(7) });
        mock.lock().unwrap().fail("files/upload_session/start", Fault::Status(503));
        assert!(matches!(db.start_upload(None, "a", (ROOT, None), 1, "").await, Err(RemoteError::Transient(_))));
        mock.lock().unwrap().fail("files/download", Fault::Status(500));
        assert!(db.download_range("id:x", 0, 1).await.unwrap_err().is_transient());
        assert_eq!(db.get("id:x").await, Err(RemoteError::NotFound));
    }
}
