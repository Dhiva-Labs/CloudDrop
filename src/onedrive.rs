//! Minimal Microsoft Graph client for OneDrive (personal, work and school accounts): just the
//! calls CloudDrop needs, inside its app folder. With `Files.ReadWrite.AppFolder` CloudDrop sees
//! Apps/CloudDrop and nothing else. The engine reaches it through [`crate::remote::Remote`];
//! docs/PROVIDERS.md › "OneDrive" says what a release needs.

use crate::db::Kind;
use crate::hash::Hasher;
use crate::remote::{
    Capabilities, ContentHash, HashKind, OAuthApp, ProviderInfo, RemoteError, RemoteMeta, RemoteResult, SignIn,
    StorageQuota, TokenSource, UploadStatus,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE, RETRY_AFTER};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;

/// Microsoft Graph, for personal and work or school accounts alike.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

pub static INFO: ProviderInfo = ProviderInfo {
    experimental: false,
    id: "onedrive",
    name: "OneDrive",
    caps: Capabilities {
        hash: Some(HashKind::QuickXor),
        // OneDrive keeps versions by itself but can't pin one, so keep_version makes a copy.
        pin_versions: false,
        stable_ids: true,
        resumable: true,
        case_insensitive: true,
        forbidden_chars: "\"*:<>?/\\|",
        chunk_unit: 320 * 1024,
    },
    sign_in: SignIn::OAuth(OAuthApp {
        auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        scopes: &["Files.ReadWrite.AppFolder", "offline_access"],
        extra_auth_params: &[],
        // Entra accepts any port for a loopback redirect.
        redirect_port: None,
    }),
    web_url,
};

/// The sign-in endpoints for `[onedrive] tenant`: "common" (any account, as in [`INFO`]),
/// "consumers", "organizations", or one organisation's tenant id or domain.
pub fn oauth_endpoints(tenant: &str) -> (String, String) {
    let tenant = match tenant.trim() {
        "" => "common",
        t => t,
    };
    let base = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0");
    (format!("{base}/authorize"), format!("{base}/token"))
}

/// A link to the item on onedrive.live.com, for personal accounts only: their ids start with
/// the drive's cid ("a1b2c3d4e5f60789!123"), which is all the link needs. Work and school ids
/// don't say which SharePoint site holds them, so those items get no link.
fn web_url(_kind: Kind, id: &str) -> Option<String> {
    let (cid, _) = id.split_once('!')?;
    if cid.is_empty() || !cid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("https://onedrive.live.com/?cid={}&id={id}", cid.to_ascii_lowercase()))
}

/// Stands in for the OAuth sign-in until it is wired up: every call asks for sign-in, so a
/// config saying `provider = "onedrive"` shows "sign in needed" instead of failing harder.
pub struct NotSignedIn;

impl TokenSource for NotSignedIn {
    fn token(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(async { Err(RemoteError::Auth("OneDrive sign-in isn't available in this build yet".into())) })
    }

    fn refresh(&self) -> BoxFuture<'_, RemoteResult<String>> {
        self.token()
    }
}

/// Largest fragment sent in one request: Graph takes less than 60 MiB, in multiples of 320 KiB.
const MAX_FRAGMENT: usize = 191 * 320 * 1024;
const _: () = assert!(MAX_FRAGMENT < 60 * 1024 * 1024 && MAX_FRAGMENT.is_multiple_of(320 * 1024));
/// How long `keep_version` waits for OneDrive to finish copying the old content.
const COPY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A driveItem as Graph sends it (the parts CloudDrop reads).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    #[serde(default)]
    name: String,
    size: Option<u64>,
    folder: Option<Value>,
    file: Option<FileFacet>,
    parent_reference: Option<ParentRef>,
    deleted: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct FileFacet {
    #[serde(default)]
    hashes: Hashes,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Hashes {
    quick_xor_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ParentRef {
    id: Option<String>,
    drive_id: Option<String>,
}

impl From<Item> for RemoteMeta {
    fn from(i: Item) -> Self {
        let folder = i.folder.is_some();
        let size = if folder { None } else { i.size };
        let mut hash = i
            .file
            .and_then(|f| f.hashes.quick_xor_hash)
            .and_then(|h| quickxor_hex(&h))
            .map(|value| ContentHash { kind: HashKind::QuickXor, value });
        // Empty files may come without a hash, but their content (nothing) is known.
        if hash.is_none() && size == Some(0) {
            hash = Some(Hasher::new(HashKind::QuickXor).finish());
        }
        RemoteMeta {
            id: i.id,
            name: i.name,
            parents: i.parent_reference.and_then(|p| p.id).into_iter().collect(),
            folder,
            hash,
            size,
            trashed: i.deleted.is_some(),
        }
    }
}

/// Graph sends quickXorHash in base64; CloudDrop keeps hashes as lowercase hex.
fn quickxor_hex(b64: &str) -> Option<String> {
    let raw = B64.decode(b64.trim()).ok()?;
    (raw.len() == 20).then(|| hex::encode(raw))
}

/// What `start_upload` hands the engine to keep (`entries.upload_url`), as JSON: the upload URL
/// and where the file goes.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Session {
    /// Graph's uploadUrl; None for an empty file, which goes up in one request instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    /// The file whose content is replaced; None = a new file `name` in `parent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<String>,
    parent: String,
    name: String,
}

impl Session {
    /// A session this client can't read counts as expired, so the upload starts again.
    fn parse(s: &str) -> RemoteResult<Session> {
        serde_json::from_str(s).map_err(|_| RemoteError::NotFound)
    }
}

pub struct OneDrive {
    http: reqwest::Client,
    base: String,
    auth: Arc<dyn TokenSource>,
    approot: OnceCell<String>,
}

impl OneDrive {
    /// `base` is Graph's address ([`GRAPH_BASE`]; tests point it at a fake). `http` must not
    /// follow redirects ([`crate::drive::http_client`]): downloads follow them here, without
    /// the token.
    pub fn new(http: reqwest::Client, base: impl Into<String>, auth: Arc<dyn TokenSource>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        Self { http, base, auth, approot: OnceCell::new() }
    }

    /// Sends an authorised Graph request, refreshing the token once on 401; returns the last
    /// response whatever its status.
    async fn send(&self, build: impl Fn(&reqwest::Client) -> RequestBuilder) -> RemoteResult<Response> {
        let mut refreshed = false;
        loop {
            let token = if refreshed {
                self.auth.refresh().await?
            } else {
                self.auth.token().await?
            };
            let resp = build(&self.http).bearer_auth(token).send().await.map_err(network)?;
            if resp.status() == StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                continue;
            }
            return Ok(resp);
        }
    }

    /// [`Self::send`], with statuses other than 2xx and 3xx turned into errors.
    async fn call(&self, build: impl Fn(&reqwest::Client) -> RequestBuilder) -> RemoteResult<Response> {
        let resp = self.send(build).await?;
        if resp.status().is_success() || resp.status().is_redirection() {
            Ok(resp)
        } else {
            Err(classify(resp).await)
        }
    }

    fn item_url(&self, id: &str) -> String {
        format!("{}/me/drive/items/{id}", self.base)
    }

    /// The item called `name` in folder `parent`, by path (`…/items/{parent}:/{name}`),
    /// followed by `action` (`/content`, `/createUploadSession`) when given.
    fn child_url(&self, parent: &str, name: &str, action: Option<&str>) -> String {
        let mut url = format!("{}/me/drive/items/{parent}:/{}", self.base, encode_segment(name));
        if let Some(a) = action {
            url.push(':');
            url.push_str(a);
        }
        url
    }

    /// Storage of the account: (total, used, used). The whole OneDrive is CloudDrop's cloud
    /// drive, so both usages are the same. A missing or zero total means unknown.
    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        let url = format!("{}/me/drive", self.base);
        let v: Value = json(self.call(|c| c.get(&url)).await?).await?;
        let q = &v["quota"];
        let used = q["used"].as_u64().unwrap_or(0);
        Ok((q["total"].as_u64().filter(|t| *t > 0), used, used))
    }

    /// The app folder (Apps/CloudDrop), which Graph creates on first use.
    pub async fn top_parent(&self) -> RemoteResult<String> {
        let id = self
            .approot
            .get_or_try_init(|| async {
                let url = format!("{}/me/drive/special/approot", self.base);
                Ok::<_, RemoteError>(json::<Item>(self.call(|c| c.get(&url)).await?).await?.id)
            })
            .await?;
        Ok(id.clone())
    }

    /// A recycled item answers 404 by id, so it is NotFound here rather than trashed.
    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        Ok(self.item(id).await?.into())
    }

    async fn item(&self, id: &str) -> RemoteResult<Item> {
        let url = self.item_url(id);
        json(self.call(|c| c.get(&url)).await?).await
    }

    /// The item named `name` in `parent`, if any. Names are unique in a folder, ignoring case,
    /// so there is at most one, and the recycle bin isn't reachable by path.
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        let url = self.child_url(parent, name, None);
        match self.call(|c| c.get(&url)).await {
            Ok(resp) => {
                let m: RemoteMeta = json::<Item>(resp).await?.into();
                Ok(if m.trashed || (folder && !m.folder) { vec![] } else { vec![m] })
            }
            Err(RemoteError::NotFound) => Ok(vec![]),
            Err(e) => Err(e),
        }
    }

    /// Creates a folder; one that already has the name is returned instead (names are unique).
    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        let url = format!("{}/children", self.item_url(parent));
        let body = json!({ "name": name, "folder": {}, "@microsoft.graph.conflictBehavior": "fail" });
        let resp = self.send(|c| c.post(&url).json(&body)).await?;
        if resp.status() == StatusCode::CONFLICT
            && let Some(existing) = self.find_child(name, parent, true).await?.into_iter().next()
        {
            return Ok(existing);
        }
        Ok(json::<Item>(checked(resp).await?).await?.into())
    }

    /// Renames and/or moves an item. `parents` = (new, old) when moving; Graph only needs the
    /// new one.
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        let mut body = serde_json::Map::new();
        if let Some(n) = name {
            body.insert("name".into(), json!(n));
        }
        if let Some((to, _)) = parents {
            body.insert("parentReference".into(), json!({ "id": to }));
        }
        if body.is_empty() {
            return self.get(id).await;
        }
        let url = self.item_url(id);
        let body = Value::Object(body);
        Ok(json::<Item>(self.call(|c| c.patch(&url).json(&body)).await?).await?.into())
    }

    /// OneDrive can't pin a version, so this keeps the current content as a copy next to the
    /// file, "<stem> (replaced YYYY-MM-DD).<ext>" (UTC date; " (2)" and up when taken), and
    /// waits until OneDrive has finished making it.
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        let item = self.item(id).await?;
        let parent_ref = item.parent_reference.unwrap_or_default();
        let Some(parent) = parent_ref.id else {
            return Err(RemoteError::Permanent("OneDrive did not say which folder the file is in.".into()));
        };
        let base = replaced_name(&item.name, &utc_date(SystemTime::now()));
        let mut name = base.clone();
        let mut n = 1;
        while !self.find_child(&name, &parent, false).await?.is_empty() {
            n += 1;
            name = crate::util::numbered_name(&base, n);
        }
        let mut target = json!({ "id": parent });
        if let Some(drive) = parent_ref.drive_id {
            target["driveId"] = json!(drive);
        }
        let body = json!({ "parentReference": target, "name": name });
        let url = format!("{}/copy", self.item_url(id));
        let resp = self.call(|c| c.post(&url).json(&body)).await?;
        let monitor = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
        match (resp.status(), monitor) {
            (_, Some(m)) => self.wait_for_copy(&m).await,
            (StatusCode::OK | StatusCode::CREATED, None) => Ok(()),
            _ => Err(RemoteError::Transient("OneDrive did not say how its copy is going".into())),
        }
    }

    /// Follows a copy's monitor URL until OneDrive says it's done. The URL needs no token, and
    /// gets none: it isn't always on Graph's host.
    async fn wait_for_copy(&self, monitor: &str) -> RemoteResult<()> {
        let deadline = Instant::now() + COPY_TIMEOUT;
        let mut pause = Duration::from_millis(200);
        loop {
            let resp = self.http.get(monitor).send().await.map_err(network)?;
            let st = resp.status();
            // Some services answer a finished copy with a redirect to the new item.
            if st.is_redirection() {
                return Ok(());
            }
            if !st.is_success() {
                return Err(classify(resp).await);
            }
            let v: Value = json(resp).await?;
            match v["status"].as_str().unwrap_or_default() {
                "completed" => return Ok(()),
                "failed" | "cancelled" | "deleteFailed" => {
                    let why = v["error"]["message"].as_str().unwrap_or("the copy failed");
                    return Err(RemoteError::Permanent(format!("OneDrive could not keep the old version: {why}")));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(RemoteError::Transient("OneDrive is taking too long to copy the old version".into()));
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(5));
        }
    }

    /// Opens an upload session; returns it as JSON (the upload URL and where the file goes).
    /// `existing` = replace that file's content, after giving it `name` in `parents.0`. A new
    /// file fails if the folder already has one of that name.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        _mime: &str,
    ) -> RemoteResult<String> {
        let (parent, old_parent) = parents;
        if let Some(id) = existing {
            // The session only replaces content, so name and place the file first.
            let moving = old_parent.filter(|o| *o != parent).map(|o| (parent, o));
            self.move_rename(id, Some(name), moving).await?;
        }
        let mut session = Session {
            url: None,
            item: existing.map(str::to_string),
            parent: parent.to_string(),
            name: name.to_string(),
        };
        if size > 0 {
            let (url, behavior) = match existing {
                Some(id) => (format!("{}/createUploadSession", self.item_url(id)), "replace"),
                None => (self.child_url(parent, name, Some("/createUploadSession")), "fail"),
            };
            // fileSize lets OneDrive refuse a file that won't fit before any of it is sent.
            let body = json!({ "item": { "@microsoft.graph.conflictBehavior": behavior, "fileSize": size } });
            let v: Value = json(self.call(|c| c.post(&url).json(&body)).await?).await?;
            let upload = v["uploadUrl"]
                .as_str()
                .ok_or_else(|| RemoteError::Transient("OneDrive did not return an upload session".into()))?;
            session.url = Some(upload.to_string());
        }
        Ok(serde_json::to_string(&session).expect("session is JSON"))
    }

    /// Asks a session how much it has received. NotFound = the session expired (or finished:
    /// OneDrive forgets a session once the file is complete).
    pub async fn query_upload(&self, session: &str, _total: u64) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        let Some(url) = &s.url else {
            return Ok(UploadStatus::Next(0));
        };
        let resp = self.http.get(url).send().await.map_err(network)?;
        self.finished(upload_reply(resp).await?).await
    }

    /// Sends `data` at `start`, in fragments under Graph's 60 MiB limit, to the upload URL
    /// (which carries its own authorisation, so no token goes with it).
    pub async fn put_chunk(
        &self,
        session: &str,
        mut data: bytes::Bytes,
        start: u64,
        total: u64,
    ) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        let Some(url) = &s.url else {
            return self.put_whole(&s, data).await;
        };
        if data.is_empty() {
            return self.query_upload(session, total).await;
        }
        let mut offset = start;
        loop {
            let part = data.split_to(data.len().min(MAX_FRAGMENT));
            let last = offset + part.len() as u64 - 1;
            let resp = self
                .http
                .put(url)
                .timeout(Duration::from_secs(600))
                .header(CONTENT_RANGE, format!("bytes {offset}-{last}/{total}"))
                .body(part)
                .send()
                .await
                .map_err(network)?;
            // OneDrive already has some of these bytes: ask where to go on from.
            if resp.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                return self.query_upload(session, total).await;
            }
            match upload_reply(resp).await? {
                UploadStatus::Next(n) if n == last + 1 && !data.is_empty() => offset = n,
                status => return self.finished(status).await,
            }
        }
    }

    /// Uploads a whole file in one request: for empty files, which upload sessions can't take.
    async fn put_whole(&self, s: &Session, data: bytes::Bytes) -> RemoteResult<UploadStatus> {
        let url = match &s.item {
            Some(id) => format!("{}/content", self.item_url(id)),
            None => format!(
                "{}?@microsoft.graph.conflictBehavior=fail",
                self.child_url(&s.parent, &s.name, Some("/content"))
            ),
        };
        let resp = self
            .call(|c| c.put(&url).header(CONTENT_TYPE, "application/octet-stream").body(data.clone()))
            .await?;
        self.finished(UploadStatus::Done(json::<Item>(resp).await?.into())).await
    }

    /// A finished upload's metadata. OneDrive sometimes adds the hash a moment later, so ask
    /// once more when it's missing (and go without if that fails: the engine asks again).
    async fn finished(&self, status: UploadStatus) -> RemoteResult<UploadStatus> {
        match status {
            UploadStatus::Done(m) if m.hash.is_none() && !m.folder => {
                Ok(UploadStatus::Done(self.get(&m.id).await.unwrap_or(m)))
            }
            other => Ok(other),
        }
    }

    /// GETs a file's content. Graph answers with a redirect to a short-lived pre-authenticated
    /// URL, followed here without the token.
    async fn content(&self, id: &str, range: Option<&str>, timeout: Duration) -> RemoteResult<Response> {
        let url = format!("{}/content", self.item_url(id));
        let ranged = |r: RequestBuilder| match range {
            Some(v) => r.header(RANGE, v),
            None => r,
        };
        let resp = self.call(|c| ranged(c.get(&url).timeout(timeout))).await?;
        if !resp.status().is_redirection() {
            return Ok(resp);
        }
        let to = resp
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|loc| url::Url::parse(&url).ok()?.join(loc).ok())
            .ok_or_else(|| RemoteError::Transient("OneDrive did not say where to download from".into()))?;
        checked(ranged(self.http.get(to).timeout(timeout)).send().await.map_err(network)?).await
    }

    /// Bytes `start..end` of a file's content (for downloading placeholders on demand).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes> {
        if end <= start {
            return Ok(bytes::Bytes::new());
        }
        let range = format!("bytes={start}-{}", end - 1);
        let resp = self.content(id, Some(&range), Duration::from_secs(600)).await?;
        let partial = resp.status() == StatusCode::PARTIAL_CONTENT;
        let body = resp.bytes().await.map_err(interrupted)?;
        if partial {
            return Ok(body);
        }
        // The whole file came back: cut the range out of it.
        let len = body.len() as u64;
        Ok(body.slice(start.min(len) as usize..end.min(len) as usize))
    }

    /// Streams a file's content into `out`; returns (its QuickXorHash, bytes written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        let resp = self.content(id, None, Duration::from_secs(24 * 3600)).await?;
        let mut stream = resp.bytes_stream();
        let mut hasher = Hasher::new(HashKind::QuickXor);
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(interrupted)?;
            hasher.update(&chunk);
            total += chunk.len() as u64;
            out.write_all(&chunk).await.map_err(cannot_write)?;
        }
        out.flush().await.map_err(cannot_write)?;
        Ok((hasher.finish(), total))
    }
}

async fn json<T: for<'de> Deserialize<'de>>(resp: Response) -> RemoteResult<T> {
    resp.json()
        .await
        .map_err(|e| RemoteError::Transient(format!("bad response from OneDrive: {e}")))
}

/// The response if it succeeded, else its error.
async fn checked(resp: Response) -> RemoteResult<Response> {
    if resp.status().is_success() {
        Ok(resp)
    } else {
        Err(classify(resp).await)
    }
}

fn network(e: reqwest::Error) -> RemoteError {
    RemoteError::Transient(format!("network error: {e}"))
}

fn interrupted(e: reqwest::Error) -> RemoteError {
    RemoteError::Transient(format!("download interrupted: {e}"))
}

fn cannot_write(e: std::io::Error) -> RemoteError {
    RemoteError::Permanent(format!("cannot write download: {e}"))
}

/// What an upload URL answered: the finished item, or where to go on from.
async fn upload_reply(resp: Response) -> RemoteResult<UploadStatus> {
    let st = resp.status();
    // The URL carries its own authorisation: once refused or unknown, the session is gone.
    if matches!(st, StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND | StatusCode::GONE) {
        return Err(RemoteError::NotFound);
    }
    let v: Value = json(checked(resp).await?).await?;
    if v.get("id").is_some() {
        let item: Item = serde_json::from_value(v)
            .map_err(|e| RemoteError::Transient(format!("bad response from OneDrive: {e}")))?;
        return Ok(UploadStatus::Done(item.into()));
    }
    next_expected(&v)
        .map(UploadStatus::Next)
        .ok_or_else(|| RemoteError::Transient("OneDrive did not say how much of the upload it has".into()))
}

/// The start of the first range in `nextExpectedRanges` ("327680-" or "327680-655359").
fn next_expected(v: &Value) -> Option<u64> {
    v["nextExpectedRanges"].as_array()?.first()?.as_str()?.split('-').next()?.parse().ok()
}

async fn classify(resp: Response) -> RemoteError {
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let body = resp.text().await.unwrap_or_default();
    let (codes, message) = graph_error(&body);
    let message = message.unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
    error_for(status.as_u16(), &codes, &message, retry_after)
}

/// Maps a Graph error to CloudDrop's kinds. `codes` are its code and inner codes.
fn error_for(status: u16, codes: &[String], message: &str, retry_after: Option<u64>) -> RemoteError {
    let has = |c: &str| codes.iter().any(|x| x == c);
    if status == 507 || has("quotaLimitReached") || has("insufficientStorage") {
        return RemoteError::Quota("Your OneDrive is full.".into());
    }
    let text = format!("OneDrive: {message} ({status})");
    match status {
        401 => RemoteError::Auth(text),
        404 | 410 => RemoteError::NotFound,
        429 | 503 => RemoteError::RateLimited { message: text, retry_after },
        // 423: the file is locked (open in an Office app), for a while.
        408 | 423 | 500..=599 => RemoteError::Transient(text),
        _ => RemoteError::Permanent(text),
    }
}

/// The codes (outermost first) and message of a Graph error body:
/// `{"error": {"code": …, "message": …, "innerError": {"code": …, "innerError": …}}}`.
fn graph_error(body: &str) -> (Vec<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return (vec![], None);
    };
    let message = v["error"]["message"].as_str().filter(|m| !m.is_empty()).map(str::to_string);
    let mut codes = Vec::new();
    let mut e = &v["error"];
    while let Some(code) = e["code"].as_str() {
        codes.push(code.to_string());
        e = if e["innerError"].is_object() { &e["innerError"] } else { &e["innererror"] };
    }
    (codes, message)
}

/// Percent-encodes a name for a URL path: everything but letters, digits and `-._~`.
fn encode_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// "report.pdf" → "report (replaced 2026-10-21).pdf"; ".bashrc" → ".bashrc (replaced 2026-10-21)".
pub(crate) fn replaced_name(name: &str, date: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} (replaced {date}){}", &name[..i], &name[i..]),
        _ => format!("{name} (replaced {date})"),
    }
}

/// The UTC date of `t` as YYYY-MM-DD (Howard Hinnant's days-to-civil algorithm).
pub(crate) fn utc_date(t: SystemTime) -> String {
    let days = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn item(v: Value) -> RemoteMeta {
        serde_json::from_value::<Item>(v).unwrap().into()
    }

    #[test]
    fn capabilities_and_sign_in() {
        assert_eq!((INFO.id, INFO.name), ("onedrive", "OneDrive"));
        assert_eq!(INFO.caps.hash, Some(HashKind::QuickXor));
        assert!(!INFO.caps.pin_versions && INFO.caps.stable_ids && INFO.caps.resumable && INFO.caps.case_insensitive);
        assert_eq!(INFO.caps.chunk_unit, 320 * 1024);
        assert_eq!(MAX_FRAGMENT % INFO.caps.chunk_unit, 0);
        for c in ['"', '*', ':', '<', '>', '?', '/', '\\', '|'] {
            assert!(INFO.caps.forbidden_chars.contains(c), "{c}");
        }
        let SignIn::OAuth(app) = &INFO.sign_in else { panic!("OneDrive signs in with OAuth") };
        assert_eq!(app.scopes, ["Files.ReadWrite.AppFolder", "offline_access"]);
        assert_eq!((app.auth_url.to_string(), app.token_url.to_string()), oauth_endpoints("common"));
        assert_eq!(oauth_endpoints(" "), oauth_endpoints("common"));
        assert_eq!(
            oauth_endpoints("contoso.onmicrosoft.com").1,
            "https://login.microsoftonline.com/contoso.onmicrosoft.com/oauth2/v2.0/token"
        );
    }

    #[test]
    fn web_links_only_for_personal_ids() {
        assert_eq!(
            web_url(Kind::File, "A1B2C3D4E5F60789!123").as_deref(),
            Some("https://onedrive.live.com/?cid=a1b2c3d4e5f60789&id=A1B2C3D4E5F60789!123")
        );
        assert!(web_url(Kind::Dir, "a1b2c3d4e5f60789!s0b1c2d3e4f5").is_some());
        // Work and school ids say nothing about the SharePoint site that holds them.
        assert_eq!(web_url(Kind::File, "01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K"), None);
        assert_eq!(web_url(Kind::File, "not-hex!1"), None);
    }

    #[test]
    fn neutral_metadata() {
        let m = item(json!({
            "id": "C!5", "name": "a.txt", "size": 1,
            "parentReference": {"id": "C!3", "driveId": "c"},
            "file": {"mimeType": "text/plain", "hashes": {"quickXorHash": "SgAAAAAAAAAAAAAAAQAAAAAAAAA="}}
        }));
        let hash = ContentHash { kind: HashKind::QuickXor, value: "4a00000000000000000000000100000000000000".into() };
        assert!(m.same_content(&hash, 1) && !m.folder && !m.trashed);
        assert_eq!((m.id.as_str(), m.name.as_str(), m.parents.as_slice()), ("C!5", "a.txt", &["C!3".to_string()][..]));
        assert_eq!(m.md5(), None);

        let folder = item(json!({"id": "C!3", "name": "Docs", "size": 99, "folder": {"childCount": 2}}));
        assert!(folder.folder);
        assert_eq!((folder.size, folder.hash, folder.parents.len()), (None, None, 0));

        // No hash yet (OneDrive adds it later), and a recycled item.
        let bare = item(json!({"id": "C!6", "size": 5, "file": {}, "deleted": {"state": "deleted"}}));
        assert_eq!(bare.hash, None);
        assert!(bare.trashed);
        // An empty file's hash is known without asking.
        let empty = item(json!({"id": "C!7", "size": 0, "file": {"mimeType": "text/plain"}}));
        assert_eq!(empty.hash, Some(Hasher::new(HashKind::QuickXor).finish()));
    }

    #[test]
    fn quickxor_base64_to_hex() {
        assert_eq!(quickxor_hex("zBTHrspn3mEcohlJdIUAbjGNaNg=").as_deref(), Some("cc14c7aeca67de611ca219497485006e318d68d8"));
        assert_eq!(quickxor_hex("not base64!"), None);
        assert_eq!(quickxor_hex("AAAA"), None, "too short for a QuickXorHash");
    }

    #[test]
    fn graph_errors() {
        let body = r#"{"error":{"code":"accessDenied","message":"Nope","innerError":{"code":"quotaLimitReached"}}}"#;
        assert_eq!(graph_error(body), (vec!["accessDenied".to_string(), "quotaLimitReached".to_string()], Some("Nope".into())));
        assert_eq!(graph_error("<html>"), (vec![], None));
        let (codes, _) = graph_error(body);
        assert!(matches!(error_for(403, &codes, "Nope", None), RemoteError::Quota(_)));
        assert!(matches!(error_for(507, &[], "Insufficient Storage", None), RemoteError::Quota(_)));
        assert!(matches!(error_for(401, &[], "x", None), RemoteError::Auth(_)));
        assert_eq!(error_for(404, &[], "x", None), RemoteError::NotFound);
        assert_eq!(
            error_for(429, &[], "Too Many Requests", Some(7)),
            RemoteError::RateLimited { message: "OneDrive: Too Many Requests (429)".into(), retry_after: Some(7) }
        );
        assert!(matches!(error_for(503, &[], "x", None), RemoteError::RateLimited { retry_after: None, .. }));
        assert!(matches!(error_for(502, &[], "x", None), RemoteError::Transient(_)));
        assert!(matches!(error_for(423, &[], "x", None), RemoteError::Transient(_)));
        assert_eq!(
            error_for(409, &["nameAlreadyExists".into()], "Name already exists", None),
            RemoteError::Permanent("OneDrive: Name already exists (409)".into())
        );
    }

    #[test]
    fn upload_ranges() {
        assert_eq!(next_expected(&json!({"nextExpectedRanges": ["327680-"]})), Some(327_680));
        assert_eq!(next_expected(&json!({"nextExpectedRanges": ["12-99", "200-"]})), Some(12));
        assert_eq!(next_expected(&json!({"nextExpectedRanges": []})), None);
        let s = Session { url: Some("https://u/1".into()), item: None, parent: "P".into(), name: "a b.txt".into() };
        assert_eq!(Session::parse(&serde_json::to_string(&s).unwrap()).unwrap(), s);
        // A Drive session URL (or anything else) isn't ours: start again.
        assert_eq!(Session::parse("https://www.googleapis.com/upload/x"), Err(RemoteError::NotFound));
    }

    #[test]
    fn names_in_urls_and_copies() {
        assert_eq!(encode_segment("a b#%+é.txt"), "a%20b%23%25%2B%C3%A9.txt");
        assert_eq!(replaced_name("report.pdf", "2026-10-21"), "report (replaced 2026-10-21).pdf");
        assert_eq!(replaced_name("a.tar.gz", "2026-10-21"), "a.tar (replaced 2026-10-21).gz");
        assert_eq!(replaced_name(".bashrc", "2026-10-21"), ".bashrc (replaced 2026-10-21)");
        assert_eq!(replaced_name("README", "2026-10-21"), "README (replaced 2026-10-21)");
        let day = |n: u64| utc_date(UNIX_EPOCH + Duration::from_secs(n * 86_400 + 86_399));
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(10_651), "1999-03-01");
        assert_eq!(day(11_016), "2000-02-29");
        assert_eq!(day(20_088), "2024-12-31");
        assert_eq!(day(20_747), "2026-10-21");
    }

    #[tokio::test]
    async fn configured_without_sign_in_asks_for_it() {
        let cfg = Config { provider: crate::remote::Provider::OneDrive, ..Config::default() };
        let remote = crate::remote_for(&cfg, "unused-token.json".into());
        assert_eq!(remote.provider(), crate::remote::Provider::OneDrive);
        assert!(matches!(remote.top_parent().await, Err(RemoteError::Auth(_))));
        assert!(matches!(remote.storage_quota().await, Err(RemoteError::Auth(_))));
    }

    #[test]
    fn config_section() {
        let old: Config = toml::from_str("sync_dir = \"/x\"").unwrap();
        assert_eq!((old.onedrive.client_id.as_str(), old.onedrive.tenant.as_str()), ("", "common"));
        assert!(!toml::to_string_pretty(&old).unwrap().contains("onedrive"), "untouched files stay the same");
        let c: Config = toml::from_str("provider = \"onedrive\"\n[onedrive]\nclient_id = \"abc\"").unwrap();
        assert_eq!((c.provider.as_str(), c.onedrive.client_id.as_str(), c.onedrive.tenant.as_str()), ("onedrive", "abc", "common"));
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.onedrive, c.onedrive);
    }
}
