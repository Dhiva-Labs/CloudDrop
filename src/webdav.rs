//! WebDAV client for any WebDAV server, using Nextcloud/ownCloud extras where the server has
//! them: chunked uploads that survive an interruption, SHA-1 checksums, Login Flow v2. Two
//! providers share it: `webdav` (a form: address, user name, app password) and `nextcloud`
//! (Login Flow v2 in the browser). The engine reaches it through [`crate::remote::Remote`].
//!
//! Ids are paths below the files' top level: `/` itself, `/Backups/CloudDrop/a.txt`. A
//! child's id is its parent's id, `/` and its name, so when a folder moves, the ids beneath it
//! change by a prefix (`stable_ids: false`).
//!
//! Freeing space proves the cloud copy by downloading it (`hash: None`). Nextcloud's
//! `oc:checksums` is whatever the uploading client said, so it is only an extra check (after
//! uploads, and on downloads), never the proof.

use crate::config::Config;
use crate::db::Kind;
use crate::hash::Hasher;
use crate::remote::{
    Capabilities, ContentHash, Field, FieldKind, HashKind, Provider, ProviderInfo, RemoteError, RemoteMeta,
    RemoteResult, SignIn, StorageQuota, UploadStatus,
};
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION, RANGE, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const CAPS: Capabilities = Capabilities {
    hash: None,
    pin_versions: false,
    stable_ids: false,
    resumable: false,
    case_insensitive: false,
    // Nextcloud and ownCloud refuse names with a backslash, and Windows-backed servers read it
    // as a folder separator.
    forbidden_chars: "\\",
    chunk_unit: 1024 * 1024,
};

/// Any WebDAV server, signed in with a form.
pub static INFO: ProviderInfo = ProviderInfo {
    id: "webdav",
    name: "WebDAV",
    caps: CAPS,
    sign_in: SignIn::Form(&[
        Field {
            key: "url",
            label: "Server address",
            kind: FieldKind::Url,
            required: true,
            hint: "https://cloud.example.com for Nextcloud or ownCloud, else the WebDAV address of your files",
        },
        Field { key: "user", label: "User name", kind: FieldKind::Text, required: true, hint: "" },
        Field {
            key: "password",
            label: "App password",
            kind: FieldKind::Secret,
            required: true,
            hint: "Make one in your account's security settings",
        },
        Field {
            key: "folder",
            label: "Folder",
            kind: FieldKind::Text,
            required: false,
            hint: "Where the CloudDrop folder goes, e.g. Backups (empty: the top level)",
        },
    ]),
    web_url: no_web_url,
};

/// Nextcloud, signed in with Login Flow v2 ([`nextcloud_login_start`]). Its chunked uploads are
/// kept on the server, so an interrupted upload continues where it stopped.
pub static NEXTCLOUD_INFO: ProviderInfo = ProviderInfo {
    id: "nextcloud",
    name: "Nextcloud",
    caps: Capabilities { resumable: true, ..CAPS },
    sign_in: SignIn::NextcloudLoginFlow,
    web_url: no_web_url,
};

/// Web pages need the server's address, which a provider-wide link function doesn't have.
fn no_web_url(_: Kind, _: &str) -> Option<String> {
    None
}

/// `[webdav]` and `[nextcloud]` in config.toml. The app password is in credentials.json,
/// under the provider's id (`{"password": …}`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavConfig {
    /// The server: `https://cloud.example.com` for Nextcloud or ownCloud (its WebDAV address
    /// works too), or the WebDAV address of your files on any other server.
    pub url: String,
    /// The name you sign in with.
    pub user: String,
    /// The folder, below your files' top level, that holds the CloudDrop folder ("" = the top
    /// level). Made if it doesn't exist.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// What the server turned out to be ([`WebDav::probe`]); looked up once per start when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerInfo>,
}

impl WebDavConfig {
    /// Nothing set: the section stays out of config.toml.
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }
}

/// What [`WebDav::probe`] found out about a server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    /// "Nextcloud", "ownCloud" (or a rebranded name), from `status.php`; "" for another server.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub product: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// The account's id on Nextcloud/ownCloud (its files are `remote.php/dav/files/<id>`),
    /// which can differ from the name people sign in with.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    /// Chunked uploads (`remote.php/dav/uploads`, the `dav.chunking` capability).
    pub chunking: bool,
}

impl ServerInfo {
    pub fn is_nextcloud(&self) -> bool {
        !self.product.is_empty()
    }
}

/// Nextcloud assembles at most 10000 chunks (S3 multipart parts on S3 storage)…
const MAX_CHUNKS: u64 = 10_000;
/// …each at least 5 MiB but the last, when its storage is S3. Smaller chunks go as one PUT.
const MIN_CHUNK: u64 = 5 * 1024 * 1024;
/// For requests that carry no file content.
const META_TIMEOUT: Duration = Duration::from_secs(120);
/// For one chunk going up, one piece of a download, or the server finishing an upload.
const STALL: Duration = Duration::from_secs(600);
/// Uploads this process hasn't heard of for that long are dropped.
const STALE: Duration = Duration::from_secs(3600);

const PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:prop><d:resourcetype/><d:getcontentlength/><oc:checksums/></d:prop></d:propfind>"#;
const QUOTA_PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:quota-used-bytes/><d:quota-available-bytes/></d:prop></d:propfind>"#;
const PRINCIPAL_PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;

const DAV: &str = "DAV:";
const OC: &str = "http://owncloud.org/ns";

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        // A redirected PROPFIND or MOVE must not turn into a GET; `request` follows them itself.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        // No read timeout: one PUT can carry a whole large file. Requests set their own.
        // Nextcloud names the app password after this.
        .user_agent(concat!("CloudDrop/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

pub struct WebDav {
    http: reqwest::Client,
    provider: Provider,
    cfg: WebDavConfig,
    password: Option<String>,
    resolved: tokio::sync::OnceCell<Resolved>,
    uploads: Mutex<HashMap<String, (Upload, Instant)>>,
    min_chunk: u64,
}

/// Where the account's files are, once the server is known.
#[derive(Debug)]
struct Resolved {
    /// URL of the files' top level, without the trailing slash.
    root: String,
    /// The account's chunked-upload folder (Nextcloud/ownCloud), without the trailing slash.
    uploads: Option<String>,
    info: ServerInfo,
}

/// What `start_upload` hands the engine (kept in `entries.upload_url`).
#[derive(Debug, Serialize, Deserialize)]
struct Session {
    /// Unique per session; also the name of its chunk folder.
    id: String,
    /// Where the file goes.
    path: String,
    #[serde(default)]
    mime: String,
    /// The server takes chunked uploads (decided per session, when it starts).
    #[serde(default)]
    chunked: bool,
}

impl Session {
    fn parse(s: &str) -> RemoteResult<Session> {
        // Not ours (or from an older version): the engine starts a new one.
        serde_json::from_str(s).map_err(|_| RemoteError::NotFound)
    }
}

/// An upload this process is in the middle of.
enum Upload {
    /// Nextcloud chunks: the next offset and chunk number, and the SHA-1 so far (None when the
    /// upload started in another process).
    Chunks { next: u64, index: u32, sha1: Option<Hasher> },
    /// One PUT whose body is still being fed.
    Stream(Stream),
}

struct Stream {
    tx: Option<tokio::sync::mpsc::Sender<std::io::Result<Bytes>>>,
    reply: Option<tokio::task::JoinHandle<reqwest::Result<Response>>>,
    next: u64,
    sha1: Hasher,
}

impl Drop for Stream {
    fn drop(&mut self) {
        // An abandoned PUT ends without its last bytes, so the server keeps nothing of it.
        if let Some(reply) = &self.reply {
            reply.abort();
        }
    }
}

/// A file or folder as PROPFIND describes it.
#[derive(Debug, Default)]
struct Prop {
    /// Last segment of the href, decoded.
    name: String,
    folder: bool,
    size: Option<u64>,
    /// The SHA-1 in `oc:checksums` (lowercase hex), if the server keeps one.
    sha1: Option<String>,
    quota_used: Option<u64>,
    quota_available: Option<i64>,
    principal: Option<String>,
}

impl WebDav {
    /// `cfg` is the provider's section; `password` its app password (None: every call answers
    /// `Auth`).
    pub fn new(http: reqwest::Client, provider: Provider, cfg: WebDavConfig, password: Option<String>) -> Self {
        Self {
            http,
            provider,
            cfg,
            password,
            resolved: tokio::sync::OnceCell::new(),
            uploads: Mutex::new(HashMap::new()),
            min_chunk: MIN_CHUNK,
        }
    }

    /// The client for `cfg.provider` (`webdav` or `nextcloud`), with its section of the config
    /// and its password from `credentials` (a credentials.json).
    pub fn from_config(cfg: &Config, credentials: &Path) -> Self {
        let (provider, section) = match cfg.provider {
            Provider::Nextcloud => (Provider::Nextcloud, &cfg.nextcloud),
            _ => (Provider::WebDav, &cfg.webdav),
        };
        let password = crate::credentials::load_from(credentials, provider.as_str())
            .ok()
            .flatten()
            .and_then(|m| m.get("password").and_then(|v| v.as_str()).map(str::to_string));
        Self::new(http_client(), provider, section.clone(), password)
    }

    /// Chunks smaller than this go up as one PUT instead (Nextcloud on S3 storage needs 5 MiB;
    /// tests use small ones).
    pub fn set_min_chunk(&mut self, bytes: u64) {
        self.min_chunk = bytes;
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Looks at the server afresh: whether it's Nextcloud/ownCloud, the account's id, chunked
    /// uploads, and that the user name and password work. Setup and `clouddrop check` save the
    /// result as the section's `server`, so later starts skip the lookup.
    pub async fn probe(&self) -> RemoteResult<ServerInfo> {
        let info = self.detect().await?;
        let r = Resolved::new(&self.cfg.url, info)?;
        let top = self.propfind(&r.url("/", true), 0, PROPS).await;
        match top {
            Ok(_) => Ok(r.info),
            Err(RemoteError::NotFound) => Err(RemoteError::Permanent(format!(
                "There are no WebDAV files at {}; check the server address.",
                r.root
            ))),
            Err(e) => Err(e),
        }
    }

    async fn resolved(&self) -> RemoteResult<&Resolved> {
        self.resolved
            .get_or_try_init(|| async {
                let info = match &self.cfg.server {
                    Some(info) => info.clone(),
                    None => self.detect().await?,
                };
                Resolved::new(&self.cfg.url, info)
            })
            .await
    }

    /// Nextcloud and ownCloud answer `status.php`; anything else is plain WebDAV at `url`.
    async fn detect(&self) -> RemoteResult<ServerInfo> {
        let base = base_url(&self.cfg.url)?;
        let server = server_root(&base);
        #[derive(Deserialize)]
        struct Status {
            #[serde(default)]
            installed: bool,
            #[serde(default)]
            productname: String,
            #[serde(default)]
            versionstring: String,
            #[serde(default)]
            version: String,
        }
        let resp = self
            .http
            .get(format!("{server}/status.php"))
            .timeout(META_TIMEOUT)
            .send()
            .await
            .map_err(network)?;
        // A server error says nothing about what it is: try again later rather than guess.
        if resp.status().is_server_error() || resp.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(classify(resp).await);
        }
        let status = if resp.status().is_success() { resp.json::<Status>().await.ok() } else { None };
        let Some(st) = status.filter(|s| s.installed && !s.version.is_empty()) else {
            return Ok(ServerInfo::default());
        };
        let user_id = match files_user(&base) {
            Some(id) => id,
            None => self.current_user(server).await?.unwrap_or_else(|| self.cfg.user.clone()),
        };
        Ok(ServerInfo {
            product: if st.productname.is_empty() { "Nextcloud".into() } else { st.productname },
            version: if st.versionstring.is_empty() { st.version } else { st.versionstring },
            user_id,
            chunking: self.has_chunking(server).await?,
        })
    }

    /// The account's id, from its DAV principal (`/remote.php/dav/principals/users/<id>/`).
    async fn current_user(&self, server: &str) -> RemoteResult<Option<String>> {
        match self.propfind(&format!("{server}/remote.php/dav/"), 0, PRINCIPAL_PROPS).await {
            Ok(props) => Ok(props.into_iter().find_map(|p| p.principal).and_then(|href| {
                let path = href_path(&href);
                let rest = path.split("/principals/users/").nth(1)?;
                Some(rest.trim_matches('/').to_string()).filter(|s| !s.is_empty() && !s.contains('/'))
            })),
            Err(e @ RemoteError::Auth(_)) => Err(e),
            Err(_) => Ok(None),
        }
    }

    /// The `dav.chunking` capability (OCS), which Nextcloud and ownCloud 10 report.
    async fn has_chunking(&self, server: &str) -> RemoteResult<bool> {
        let url = format!("{server}/ocs/v1.php/cloud/capabilities?format=json");
        let resp = self
            .request(Method::GET, &url, |b| b.header("OCS-APIRequest", "true").timeout(META_TIMEOUT))
            .await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(classify(resp).await);
        }
        if !resp.status().is_success() {
            return Ok(false);
        }
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        let chunking = &v["ocs"]["data"]["capabilities"]["dav"]["chunking"];
        Ok(chunking.as_str().is_some_and(|s| !s.is_empty()))
    }

    /// Sends a request with the account's credentials and returns the response whatever its
    /// status, after following redirects within the same server.
    async fn request(
        &self,
        method: Method,
        url: &str,
        build: impl Fn(RequestBuilder) -> RequestBuilder,
    ) -> RemoteResult<Response> {
        let Some(password) = self.password.as_deref() else {
            return Err(RemoteError::Auth("no app password is saved for this server".into()));
        };
        let mut url = url.to_string();
        for _ in 0..4 {
            let req = self.http.request(method.clone(), &url).basic_auth(&self.cfg.user, Some(password));
            let resp = build(req).send().await.map_err(network)?;
            match redirect_target(&url, &resp) {
                Some(next) => url = next,
                None => return Ok(resp),
            }
        }
        Err(RemoteError::Permanent("The server keeps redirecting; check its address.".into()))
    }

    /// [`Self::request`], with statuses other than 2xx turned into errors.
    async fn call(
        &self,
        method: Method,
        url: &str,
        build: impl Fn(RequestBuilder) -> RequestBuilder,
    ) -> RemoteResult<Response> {
        let resp = self.request(method, url, build).await?;
        if resp.status().is_success() {
            Ok(resp)
        } else {
            Err(classify(resp).await)
        }
    }

    async fn propfind(&self, url: &str, depth: u8, body: &'static str) -> RemoteResult<Vec<Prop>> {
        let resp = self
            .call(Method::from_bytes(b"PROPFIND").expect("method"), url, |b| {
                b.header("Depth", depth.to_string())
                    .header(CONTENT_TYPE, "application/xml; charset=utf-8")
                    .timeout(META_TIMEOUT)
                    .body(body)
            })
            .await?;
        let text = resp
            .text()
            .await
            .map_err(|e| RemoteError::Transient(format!("bad answer from the server: {e}")))?;
        parse_multistatus(&text)
            .ok_or_else(|| RemoteError::Transient("the server sent a WebDAV answer CloudDrop can't read".into()))
    }

    /// The item at `id`, with the SHA-1 the server keeps for it.
    async fn stat(&self, id: &str) -> RemoteResult<(RemoteMeta, Option<String>)> {
        let r = self.resolved().await?;
        let props = self.propfind(&r.url(id, id == "/"), 0, PROPS).await?;
        let p = props.into_iter().next().ok_or(RemoteError::NotFound)?;
        let meta = RemoteMeta {
            id: id.to_string(),
            name: name_of(id).to_string(),
            parents: if id == "/" { vec![] } else { vec![parent_id(id)] },
            folder: p.folder,
            hash: None,
            size: if p.folder { None } else { p.size },
            trashed: false,
        };
        Ok((meta, p.sha1))
    }

    /// Storage of the account: (limit, used, used). No limit when the server says none or
    /// unlimited (Nextcloud's negative `quota-available-bytes`).
    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        let r = self.resolved().await?;
        let props = self.propfind(&r.url("/", true), 0, QUOTA_PROPS).await?;
        let p = props.into_iter().next().unwrap_or_default();
        let used = p.quota_used.unwrap_or(0);
        let limit = p.quota_available.and_then(|a| u64::try_from(a).ok()).map(|a| used + a);
        Ok((limit, used, used))
    }

    /// The configured folder, made if it isn't there yet.
    pub async fn top_parent(&self) -> RemoteResult<String> {
        let top = norm_id(&self.cfg.folder);
        if top == "/" {
            return Ok(top);
        }
        match self.stat(&top).await {
            Ok((m, _)) if m.folder => return Ok(top),
            Ok(_) => return Err(not_a_folder(&top)),
            Err(RemoteError::NotFound) => {}
            Err(e) => return Err(e),
        }
        let mut id = "/".to_string();
        let mut missing = false;
        for seg in top.split('/').filter(|s| !s.is_empty()) {
            id = child_id(&id, seg);
            if !missing {
                match self.stat(&id).await {
                    Ok((m, _)) if m.folder => continue,
                    Ok(_) => return Err(not_a_folder(&id)),
                    Err(RemoteError::NotFound) => missing = true,
                    Err(e) => return Err(e),
                }
            }
            self.mkcol(&id).await?;
        }
        Ok(top)
    }

    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        Ok(self.stat(&norm_id(id)).await?.0)
    }

    /// The item named `name` in `parent`, if there is one (and it's a folder, when `folder`).
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        match self.stat(&child_id(&norm_id(parent), name)).await {
            Ok((m, _)) if m.folder || !folder => Ok(vec![m]),
            Ok(_) | Err(RemoteError::NotFound) => Ok(vec![]),
            Err(e) => Err(e),
        }
    }

    /// MKCOL; a folder already there of that name is fine.
    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        let parent = norm_id(parent);
        let id = child_id(&parent, name);
        self.mkcol(&id).await?;
        Ok(RemoteMeta { name: name.to_string(), parents: vec![parent], folder: true, id, ..Default::default() })
    }

    async fn mkcol(&self, id: &str) -> RemoteResult<()> {
        let r = self.resolved().await?;
        let mkcol = Method::from_bytes(b"MKCOL").expect("method");
        let resp = self.request(mkcol, &r.url(id, true), |b| b.timeout(META_TIMEOUT)).await?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            // Something is there already.
            405 => match self.stat(id).await? {
                (m, _) if m.folder => Ok(()),
                _ => Err(RemoteError::Permanent(format!("A file named “{}” is in the way.", name_of(id)))),
            },
            _ => Err(classify(resp).await),
        }
    }

    /// Renames and/or moves an item (MOVE, never overwriting); returns it under its new id.
    /// A file already at the new name is first kept aside as "<stem> (replaced …).<ext>".
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        let from = norm_id(id);
        let parent = parents.map_or_else(|| parent_id(&from), |(new, _)| norm_id(new));
        let to = child_id(&parent, name.unwrap_or(name_of(&from)));
        let (meta, _) = self.stat(&from).await?;
        if to != from {
            self.relocate(&from, &to, meta.folder).await?;
        }
        Ok(RemoteMeta { name: name_of(&to).to_string(), parents: vec![parent], id: to, ..meta })
    }

    async fn relocate(&self, from: &str, to: &str, folder: bool) -> RemoteResult<()> {
        let r = self.resolved().await?;
        for attempt in 0..2 {
            let resp = self
                .request(Method::from_bytes(b"MOVE").expect("method"), &r.url(from, folder), |b| {
                    b.header("Destination", r.url(to, folder)).header("Overwrite", "F").timeout(META_TIMEOUT)
                })
                .await?;
            match resp.status().as_u16() {
                200..=299 => return Ok(()),
                412 if attempt == 0 => self.clear_way(to).await?,
                _ => return Err(classify(resp).await),
            }
        }
        Err(RemoteError::Permanent(format!("“{}” is already there.", name_of(to))))
    }

    /// Something CloudDrop doesn't know is at `id`, where a file of its own has to go: a file is
    /// kept aside (moved to "<stem> (replaced …).<ext>"), never overwritten; a folder stays.
    async fn clear_way(&self, id: &str) -> RemoteResult<()> {
        match self.stat(id).await? {
            (m, _) if m.folder => {
                Err(RemoteError::Permanent(format!("A folder named “{}” is already there.", name_of(id))))
            }
            _ => self.set_aside(id, "MOVE").await.map(drop),
        }
    }

    /// Copies (or moves) `id` to "<stem> (replaced YYYY-MM-DD).<ext>" in its folder, numbered
    /// when that name is taken; returns the new id.
    async fn set_aside(&self, id: &str, method: &str) -> RemoteResult<String> {
        let r = self.resolved().await?;
        let aside = replaced_name(name_of(id), &today());
        let parent = parent_id(id);
        for n in 1..=100 {
            let name = if n == 1 { aside.clone() } else { crate::util::numbered_name(&aside, n) };
            let to = child_id(&parent, &name);
            let resp = self
                .request(Method::from_bytes(method.as_bytes()).expect("method"), &r.url(id, false), |b| {
                    b.header("Destination", r.url(&to, false))
                        .header("Overwrite", "F")
                        .header("Depth", "0")
                        .timeout(STALL)
                })
                .await?;
            match resp.status().as_u16() {
                200..=299 => return Ok(to),
                412 => continue,
                _ => return Err(classify(resp).await),
            }
        }
        Err(RemoteError::Permanent(format!("Too many replaced copies of “{}” today.", name_of(id))))
    }

    /// Keeps the current content as "<stem> (replaced YYYY-MM-DD).<ext>" next to the file
    /// (a server-side COPY), so replacing it never loses it.
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        self.set_aside(&norm_id(id), "COPY").await.map(drop)
    }

    /// Prepares an upload of `name` into `parents.0`; returns the session (JSON). `existing`
    /// = new content for that file, which moves there first if it was elsewhere. A file
    /// CloudDrop doesn't know at the target is kept aside rather than overwritten.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        _size: u64,
        mime: &str,
    ) -> RemoteResult<String> {
        let r = self.resolved().await?;
        let parent = norm_id(parents.0);
        let path = child_id(&parent, name);
        match existing.map(norm_id) {
            Some(from) if from != path => self.relocate(&from, &path, false).await?,
            Some(_) => {}
            None => match self.stat(&path).await {
                Ok(_) => self.clear_way(&path).await?,
                // Its folder has to exist, or the PUT fails at the end.
                Err(RemoteError::NotFound) => match self.stat(&parent).await? {
                    (m, _) if m.folder => {}
                    _ => return Err(RemoteError::NotFound),
                },
                Err(e) => return Err(e),
            },
        }
        let session = Session {
            id: format!("clouddrop-{:032x}", rand::random::<u128>()),
            path,
            mime: mime.to_string(),
            chunked: r.uploads.is_some(),
        };
        Ok(serde_json::to_string(&session).expect("session json"))
    }

    /// How much of a session the server holds. Chunked uploads (Nextcloud) say what the server
    /// has; a plain PUT can't continue, so it starts over (`Next(0)`).
    pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        let r = self.resolved().await?;
        let mine = self.take_upload(&s.id);
        let Some(folder) = r.uploads.as_deref().filter(|_| s.chunked) else {
            return Ok(UploadStatus::Next(0));
        };
        let (have, count) = self.chunks_on_server(folder, &s).await?;
        if have > total {
            return Err(RemoteError::NotFound);
        }
        if count == 0 {
            // Nothing there yet: `put_chunk` starts it from the beginning.
            return Ok(UploadStatus::Next(0));
        }
        let sha1 = match mine {
            Some(Upload::Chunks { next, sha1, .. }) if next == have => sha1,
            _ => None,
        };
        if have == total {
            return self.finish_chunks(r, &s, total, sha1).await;
        }
        self.keep_upload(&s.id, Upload::Chunks { next: have, index: count + 1, sha1 });
        Ok(UploadStatus::Next(have))
    }

    /// Sends bytes `start..start + data.len()` of the file. The whole file at once is one PUT.
    /// Otherwise Nextcloud gets numbered chunks it assembles at the end; other servers one PUT
    /// whose body is fed chunk by chunk (so `Next` there means "sent", not "stored").
    pub async fn put_chunk(&self, session: &str, data: Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        let r = self.resolved().await?;
        let end = start + data.len() as u64;
        if end > total {
            return Err(RemoteError::Permanent("upload chunk goes past the end of the file".into()));
        }
        let mine = self.take_upload(&s.id);
        if start == 0 && end == total {
            drop(mine);
            return self.put_whole(r, &s, data, total).await;
        }
        let upload = match mine {
            Some(Upload::Chunks { next, index, sha1 }) if next == start => Upload::Chunks { next, index, sha1 },
            Some(Upload::Stream(st)) if st.next == start => Upload::Stream(st),
            other => {
                drop(other);
                let folder = r.uploads.as_deref().filter(|_| s.chunked);
                match folder {
                    Some(folder) if start == 0 => {
                        let n = data.len() as u64;
                        if n >= self.min_chunk && total.div_ceil(n) <= MAX_CHUNKS {
                            self.begin_chunks(r, folder, &s, total).await?;
                            Upload::Chunks { next: 0, index: 1, sha1: Some(Hasher::new(HashKind::Sha1)) }
                        } else {
                            Upload::Stream(self.begin_stream(r, &s, total)?)
                        }
                    }
                    Some(folder) => {
                        // Started in another process, or a chunk got lost: ask the server.
                        let (have, count) = self.chunks_on_server(folder, &s).await?;
                        if have != start {
                            return Ok(UploadStatus::Next(have));
                        }
                        Upload::Chunks { next: have, index: count + 1, sha1: None }
                    }
                    None if start == 0 => Upload::Stream(self.begin_stream(r, &s, total)?),
                    // A PUT can't continue where an earlier one stopped.
                    None => return Ok(UploadStatus::Next(0)),
                }
            }
        };
        match upload {
            Upload::Chunks { next, index, mut sha1 } => {
                let folder = r.uploads.as_deref().expect("chunked uploads");
                self.put_part(r, folder, &s, index, data.clone(), total).await?;
                if let Some(h) = &mut sha1 {
                    h.update(&data);
                }
                if end < total {
                    self.keep_upload(&s.id, Upload::Chunks { next: next + data.len() as u64, index: index + 1, sha1 });
                    return Ok(UploadStatus::Next(end));
                }
                self.finish_chunks(r, &s, total, sha1).await
            }
            Upload::Stream(mut st) => {
                st.sha1.update(&data);
                feed(&mut st, data).await?;
                st.next = end;
                if end < total {
                    self.keep_upload(&s.id, Upload::Stream(st));
                    return Ok(UploadStatus::Next(end));
                }
                self.finish_stream(&s, st, total).await
            }
        }
    }

    fn take_upload(&self, id: &str) -> Option<Upload> {
        let mut map = self.uploads.lock().unwrap();
        map.retain(|_, (_, at)| at.elapsed() < STALE);
        map.remove(id).map(|(u, _)| u)
    }

    fn keep_upload(&self, id: &str, upload: Upload) {
        self.uploads.lock().unwrap().insert(id.to_string(), (upload, Instant::now()));
    }

    /// The whole file in one PUT, with its SHA-1 for Nextcloud to keep.
    async fn put_whole(&self, r: &Resolved, s: &Session, data: Bytes, total: u64) -> RemoteResult<UploadStatus> {
        let mut h = Hasher::new(HashKind::Sha1);
        h.update(&data);
        let sha1 = h.finish().value;
        let nextcloud = r.info.is_nextcloud();
        let resp = self
            .call(Method::PUT, &r.url(&s.path, false), |b| {
                let b = b.header(CONTENT_TYPE, mime_or_default(&s.mime)).timeout(STALL).body(data.clone());
                if nextcloud {
                    b.header("OC-Checksum", format!("SHA1:{sha1}")).header("X-Hash", "sha1")
                } else {
                    b
                }
            })
            .await?;
        check_received(&resp, &sha1)?;
        self.uploaded(&s.path, total, Some(&sha1)).await
    }

    /// Starts a PUT of the whole file whose body comes from [`feed`].
    fn begin_stream(&self, r: &Resolved, s: &Session, total: u64) -> RemoteResult<Stream> {
        let Some(password) = self.password.as_deref() else {
            return Err(RemoteError::Auth("no app password is saved for this server".into()));
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(1);
        let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)));
        let mut req = self
            .http
            .put(r.url(&s.path, false))
            .basic_auth(&self.cfg.user, Some(password))
            // A known length, not chunked transfer encoding, which some servers refuse.
            .header(CONTENT_LENGTH, total)
            .header(CONTENT_TYPE, mime_or_default(&s.mime))
            .body(body);
        if r.info.is_nextcloud() {
            req = req.header("X-Hash", "sha1");
        }
        Ok(Stream {
            tx: Some(tx),
            reply: Some(tokio::spawn(req.send())),
            next: 0,
            sha1: Hasher::new(HashKind::Sha1),
        })
    }

    async fn finish_stream(&self, s: &Session, mut st: Stream, total: u64) -> RemoteResult<UploadStatus> {
        drop(st.tx.take());
        let reply = st.reply.take().expect("upload reply");
        let resp = match tokio::time::timeout(STALL, reply).await {
            Ok(Ok(Ok(resp))) => resp,
            Ok(Ok(Err(e))) => return Err(network(e)),
            Ok(Err(e)) => return Err(RemoteError::Transient(format!("upload task failed: {e}"))),
            Err(_) => return Err(RemoteError::Transient("the server took too long to take the upload".into())),
        };
        if !resp.status().is_success() {
            return Err(classify(resp).await);
        }
        let sha1 = std::mem::replace(&mut st.sha1, Hasher::new(HashKind::Sha1)).finish().value;
        check_received(&resp, &sha1)?;
        self.uploaded(&s.path, total, Some(&sha1)).await
    }

    /// MKCOL of the session's chunk folder, naming where the file will go (chunked upload v2).
    async fn begin_chunks(&self, r: &Resolved, folder: &str, s: &Session, total: u64) -> RemoteResult<()> {
        let url = format!("{folder}/{}/", encode_segment(&s.id));
        let mkcol = Method::from_bytes(b"MKCOL").expect("method");
        let resp = self
            .request(mkcol, &url, |b| {
                b.header("Destination", r.url(&s.path, false))
                    .header("OC-Total-Length", total)
                    .timeout(META_TIMEOUT)
            })
            .await?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            // Already there: fine while empty; with old chunks in it, start a new session.
            405 => match self.chunks_on_server(folder, s).await? {
                (0, 0) => Ok(()),
                _ => Err(RemoteError::NotFound),
            },
            _ => Err(classify(resp).await),
        }
    }

    async fn put_part(
        &self,
        r: &Resolved,
        folder: &str,
        s: &Session,
        index: u32,
        data: Bytes,
        total: u64,
    ) -> RemoteResult<()> {
        if u64::from(index) > MAX_CHUNKS {
            return Err(RemoteError::Permanent("too many upload chunks for this file".into()));
        }
        let url = format!("{folder}/{}/{index:05}", encode_segment(&s.id));
        self.call(Method::PUT, &url, |b| {
            b.header("Destination", r.url(&s.path, false))
                .header("OC-Total-Length", total)
                .timeout(STALL)
                .body(data.clone())
        })
        .await?;
        Ok(())
    }

    /// Contiguous chunks the server holds for a session: (bytes, how many).
    async fn chunks_on_server(&self, folder: &str, s: &Session) -> RemoteResult<(u64, u32)> {
        let url = format!("{folder}/{}/", encode_segment(&s.id));
        let props = match self.propfind(&url, 1, PROPS).await {
            Ok(p) => p,
            Err(RemoteError::NotFound) => return Ok((0, 0)),
            Err(e) => return Err(e),
        };
        let mut chunks: Vec<(u32, u64)> = props
            .iter()
            .filter(|p| !p.folder && !p.name.is_empty() && p.name.bytes().all(|b| b.is_ascii_digit()))
            .filter_map(|p| Some((p.name.parse().ok()?, p.size?)))
            .collect();
        chunks.sort_unstable();
        let (mut bytes, mut count) = (0, 0);
        for (index, size) in chunks {
            if index != count + 1 {
                break;
            }
            bytes += size;
            count = index;
        }
        Ok((bytes, count))
    }

    /// MOVE of the chunk folder's `.file` to where the file goes: the server assembles it.
    async fn finish_chunks(
        &self,
        r: &Resolved,
        s: &Session,
        total: u64,
        sha1: Option<Hasher>,
    ) -> RemoteResult<UploadStatus> {
        let folder = r.uploads.as_deref().expect("chunked uploads");
        let sha1 = sha1.map(|h| h.finish().value);
        let url = format!("{folder}/{}/.file", encode_segment(&s.id));
        self.call(Method::from_bytes(b"MOVE").expect("method"), &url, |b| {
            let b = b
                .header("Destination", r.url(&s.path, false))
                .header("OC-Total-Length", total)
                // Assembling a large file takes the server a while.
                .timeout(STALL * 3);
            match &sha1 {
                Some(h) => b.header("OC-Checksum", format!("SHA1:{h}")),
                None => b,
            }
        })
        .await?;
        self.uploaded(&s.path, total, sha1.as_deref()).await
    }

    /// The finished upload, checked: the size, and the SHA-1 where the server keeps one.
    async fn uploaded(&self, path: &str, total: u64, sha1: Option<&str>) -> RemoteResult<UploadStatus> {
        let (meta, kept) = self.stat(path).await?;
        if meta.folder || meta.size != Some(total) {
            return Err(RemoteError::Transient(format!(
                "the server holds {} bytes of “{}”, not {total}; uploading again",
                meta.size.unwrap_or(0),
                meta.name
            )));
        }
        if let (Some(kept), Some(sent)) = (kept, sha1)
            && kept != sent
        {
            return Err(RemoteError::Transient(format!(
                "the server's checksum of “{}” doesn't match what was sent; uploading again",
                meta.name
            )));
        }
        Ok(UploadStatus::Done(meta))
    }

    /// Bytes `start..end` of a file (GET with Range).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<Bytes> {
        if end <= start {
            return Ok(Bytes::new());
        }
        let r = self.resolved().await?;
        let range = format!("bytes={start}-{}", end - 1);
        let resp = self
            .call(Method::GET, &r.url(&norm_id(id), false), |b| b.header(RANGE, range.clone()).timeout(STALL))
            .await?;
        let partial = resp.status() == StatusCode::PARTIAL_CONTENT;
        let body = resp
            .bytes()
            .await
            .map_err(|e| RemoteError::Transient(format!("download interrupted: {e}")))?;
        if partial {
            return Ok(body);
        }
        // The server ignored Range and sent all of it.
        let len = body.len() as u64;
        Ok(body.slice(start.min(len) as usize..end.min(len) as usize))
    }

    /// Streams a file into `out`; returns (its SHA-256, bytes written). Where the server keeps
    /// a SHA-1 for it (Nextcloud's `OC-Checksum`), the content has to match that too.
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        let r = self.resolved().await?;
        let id = norm_id(id);
        // Only the wait for the answer is bounded: a large download takes as long as it takes.
        let resp = tokio::time::timeout(META_TIMEOUT, self.call(Method::GET, &r.url(&id, false), |b| b))
            .await
            .map_err(|_| RemoteError::Transient("the server took too long to answer".into()))??;
        let kept = resp.headers().get("OC-Checksum").and_then(|v| v.to_str().ok()).and_then(sha1_of);
        let mut sha256 = Hasher::new(HashKind::Sha256);
        let mut sha1 = kept.as_ref().map(|_| Hasher::new(HashKind::Sha1));
        let mut stream = resp.bytes_stream();
        let mut total = 0u64;
        loop {
            let next = tokio::time::timeout(STALL, stream.next())
                .await
                .map_err(|_| RemoteError::Transient("download stalled".into()))?;
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|e| RemoteError::Transient(format!("download interrupted: {e}")))?;
            sha256.update(&chunk);
            if let Some(h) = &mut sha1 {
                h.update(&chunk);
            }
            total += chunk.len() as u64;
            out.write_all(&chunk)
                .await
                .map_err(|e| RemoteError::Permanent(format!("cannot write download: {e}")))?;
        }
        out.flush()
            .await
            .map_err(|e| RemoteError::Permanent(format!("cannot write download: {e}")))?;
        if let (Some(kept), Some(h)) = (kept, sha1)
            && h.finish().value != kept
        {
            return Err(RemoteError::Permanent(format!(
                "“{}” on the server doesn't match the checksum the server keeps for it.",
                name_of(&id)
            )));
        }
        Ok((sha256.finish(), total))
    }
}

/// Hands one chunk to a streaming PUT; when the request already ended, says why.
async fn feed(st: &mut Stream, data: Bytes) -> RemoteResult<()> {
    let tx = st.tx.as_ref().expect("upload body");
    match tokio::time::timeout(STALL, tx.send(Ok(data))).await {
        Ok(Ok(())) => Ok(()),
        Err(_) => Err(RemoteError::Transient("upload stalled".into())),
        Ok(Err(_)) => {
            let reply = st.reply.take().expect("upload reply");
            Err(match reply.await {
                Ok(Ok(resp)) if !resp.status().is_success() => classify(resp).await,
                Ok(Ok(_)) => RemoteError::Transient("the server ended the upload early".into()),
                Ok(Err(e)) => network(e),
                Err(e) => RemoteError::Transient(format!("upload task failed: {e}")),
            })
        }
    }
}

/// Nextcloud's `X-Hash-SHA1`: what it computed over the bytes it received.
fn check_received(resp: &Response, sha1: &str) -> RemoteResult<()> {
    match resp.headers().get("X-Hash-SHA1").and_then(|v| v.to_str().ok()) {
        Some(got) if !got.trim().eq_ignore_ascii_case(sha1) => Err(RemoteError::Transient(
            "the server received different content than was sent; uploading again".into(),
        )),
        _ => Ok(()),
    }
}

fn mime_or_default(mime: &str) -> &str {
    if mime.is_empty() { "application/octet-stream" } else { mime }
}

impl Resolved {
    fn new(url: &str, info: ServerInfo) -> RemoteResult<Resolved> {
        let base = base_url(url)?;
        let server = server_root(&base);
        let (root, uploads) = if info.is_nextcloud() {
            let id = encode_segment(&info.user_id);
            let uploads = info.chunking.then(|| format!("{server}/remote.php/dav/uploads/{id}"));
            (format!("{server}/remote.php/dav/files/{id}"), uploads)
        } else {
            (base.clone(), None)
        };
        Ok(Resolved { root, uploads, info })
    }

    /// URL of an id (with the trailing slash of a folder, when known to be one).
    fn url(&self, id: &str, folder: bool) -> String {
        let mut u = format!("{}{}", self.root, encode_path(id));
        if folder || id == "/" {
            u.push('/');
        }
        u
    }
}

/// The address as configured: checked, without a trailing slash, query or fragment.
fn base_url(url: &str) -> RemoteResult<String> {
    let bad = || RemoteError::Permanent(format!("“{url}” is not a web address the server can be reached at."));
    let mut u = url::Url::parse(url.trim()).map_err(|_| bad())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(bad());
    }
    u.set_query(None);
    u.set_fragment(None);
    Ok(u.as_str().trim_end_matches('/').to_string())
}

/// Nextcloud's address from any of its WebDAV addresses (`…/remote.php/dav/files/me`).
fn server_root(base: &str) -> &str {
    match base.find("/remote.php/") {
        Some(i) => &base[..i],
        None => base.strip_suffix("/remote.php").unwrap_or(base),
    }
}

/// The user id in a `…/remote.php/dav/files/<id>` address.
fn files_user(base: &str) -> Option<String> {
    let path = url::Url::parse(base).ok()?.path().to_string();
    let rest = path.split("/remote.php/dav/files/").nth(1)?;
    let id = rest.split('/').next().filter(|s| !s.is_empty())?;
    Some(decode(id))
}

/// Follows a redirect only within the same server (credentials go nowhere else), or from
/// http to https on the same host.
fn redirect_target(url: &str, resp: &Response) -> Option<String> {
    if !matches!(resp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let from = url::Url::parse(url).ok()?;
    let to = from.join(resp.headers().get(LOCATION)?.to_str().ok()?).ok()?;
    let same_origin = from.scheme() == to.scheme() && from.port_or_known_default() == to.port_or_known_default();
    let upgrade = from.scheme() == "http" && to.scheme() == "https";
    let same = from.host_str() == to.host_str() && (same_origin || upgrade);
    same.then(|| to.to_string())
}

fn network(e: reqwest::Error) -> RemoteError {
    if e.is_timeout() {
        RemoteError::Transient("the server took too long to answer".into())
    } else {
        RemoteError::Transient(format!("network error: {e}"))
    }
}

async fn classify(resp: Response) -> RemoteError {
    let code = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let body = resp.text().await.unwrap_or_default();
    error_for(code, retry_after, sabre_message(&body))
}

fn error_for(code: u16, retry_after: Option<u64>, detail: Option<String>) -> RemoteError {
    let reason = StatusCode::from_u16(code).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
    let what = match detail {
        Some(d) => format!("{code} {d}"),
        None => format!("{code} {reason}"),
    };
    match code {
        401 => RemoteError::Auth(format!("the server didn't accept the user name or app password ({what})")),
        // 409: the folder it goes in is missing.
        404 | 409 => RemoteError::NotFound,
        429 => RemoteError::RateLimited { message: format!("The server asks CloudDrop to slow down ({what})."), retry_after },
        503 if retry_after.is_some() => {
            RemoteError::RateLimited { message: format!("The server is busy ({what})."), retry_after }
        }
        507 => RemoteError::Quota(format!("The server's storage is full ({what}).")),
        408 | 423 | 500..=599 => RemoteError::Transient(format!("server error {what}")),
        300..=399 => RemoteError::Permanent(format!("The server sends CloudDrop elsewhere ({what}); check its address.")),
        _ => RemoteError::Permanent(format!("The server refused ({what}).")),
    }
}

/// The message in a SabreDAV (Nextcloud, ownCloud) error body.
fn sabre_message(body: &str) -> Option<String> {
    let root = xml::parse(body)?;
    let msg = root.find("http://sabredav.org/ns", "message")?.text.trim().to_string();
    Some(msg).filter(|m| !m.is_empty()).map(|m| m.chars().take(200).collect())
}

fn parse_multistatus(text: &str) -> Option<Vec<Prop>> {
    let root = xml::parse(text)?;
    if !root.is(DAV, "multistatus") {
        return None;
    }
    let mut out = Vec::new();
    for resp in root.children(DAV, "response") {
        let Some(href) = resp.child(DAV, "href") else { continue };
        let path = href_path(href.text.trim());
        let mut p = Prop { name: name_of(&path).to_string(), ..Default::default() };
        let mut found = false;
        for ps in resp.children(DAV, "propstat") {
            let ok = ps.child(DAV, "status").is_some_and(|s| s.text.split_whitespace().nth(1) == Some("200"));
            let Some(prop) = ps.child(DAV, "prop").filter(|_| ok) else { continue };
            found = true;
            for el in &prop.children {
                let text = el.text.trim();
                match (el.ns.as_str(), el.name.as_str()) {
                    (DAV, "resourcetype") => p.folder = el.child(DAV, "collection").is_some(),
                    (DAV, "getcontentlength") => p.size = text.parse().ok(),
                    (DAV, "quota-used-bytes") => p.quota_used = text.parse().ok(),
                    (DAV, "quota-available-bytes") => p.quota_available = text.parse().ok(),
                    (DAV, "current-user-principal") => {
                        p.principal = el.child(DAV, "href").map(|h| h.text.trim().to_string())
                    }
                    (OC, "checksums") => {
                        p.sha1 = el.children(OC, "checksum").find_map(|c| sha1_of(&c.text));
                    }
                    _ => {}
                }
            }
        }
        if found {
            out.push(p);
        }
    }
    Some(out)
}

/// The SHA-1 in "SHA1:ab… MD5:cd…" (`oc:checksum`, `OC-Checksum`), lowercase.
fn sha1_of(list: &str) -> Option<String> {
    list.split_whitespace().find_map(|c| {
        let (kind, value) = c.split_once(':')?;
        (kind.eq_ignore_ascii_case("sha1") && value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| value.to_ascii_lowercase())
    })
}

/// The decoded path of an href, which servers send as a path or a whole URL.
fn href_path(href: &str) -> String {
    let path = match href.find("://") {
        Some(i) => href[i + 3..].find('/').map_or("/", |j| &href[i + 3 + j..]),
        None => href,
    };
    decode(path)
}

// ---- Paths and names ----

/// "/a/b" from any spelling ("a/b/", "//a//b").
fn norm_id(id: &str) -> String {
    let segs: Vec<&str> = id.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}", segs.join("/"))
}

fn child_id(parent: &str, name: &str) -> String {
    if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") }
}

fn parent_id(id: &str) -> String {
    match id.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => id[..i].into(),
    }
}

fn name_of(id: &str) -> &str {
    id.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

fn not_a_folder(id: &str) -> RemoteError {
    RemoteError::Permanent(format!("“{id}” on the server is a file, not a folder."))
}

/// Percent-encodes everything but RFC 3986's unreserved characters, so spaces, `#`, `%`, `?`
/// and non-ASCII names all survive.
fn encode_segment(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn encode_path(id: &str) -> String {
    let mut out = String::new();
    for seg in id.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        out.push_str(&encode_segment(seg));
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "report.pdf" → "report (replaced 2026-10-21).pdf"; ".bashrc" → ".bashrc (replaced …)".
fn replaced_name(name: &str, date: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} (replaced {date}){}", &name[..i], &name[i..]),
        _ => format!("{name} (replaced {date})"),
    }
}

/// Today's date (UTC) as YYYY-MM-DD.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to (year, month, day), from Howard Hinnant's date algorithms.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

// ---- Nextcloud Login Flow v2 ----

/// A Nextcloud Login Flow v2 in progress ([`nextcloud_login_start`]).
#[derive(Debug, Clone)]
pub struct LoginFlow {
    /// The page to open in the browser, where the person signs in and grants access.
    pub login_url: String,
    /// Where [`nextcloud_login_poll`] asks whether that has happened.
    pub poll_endpoint: String,
    pub token: String,
    /// How often to ask, and when to give up (the server keeps a flow for 20 minutes).
    pub interval: Duration,
    pub timeout: Duration,
}

/// What a finished Login Flow hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextcloudLogin {
    pub server: String,
    pub login_name: String,
    pub app_password: String,
}

impl NextcloudLogin {
    /// The `[nextcloud]` section for this account, keeping the folder of `previous`.
    pub fn settings(&self, previous: &WebDavConfig) -> WebDavConfig {
        WebDavConfig {
            url: self.server.clone(),
            user: self.login_name.clone(),
            folder: previous.folder.clone(),
            server: None,
        }
    }

    /// What goes into credentials.json for the provider.
    pub fn secrets(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("password".into(), self.app_password.clone().into());
        m
    }
}

/// Starts Login Flow v2 on `server` (its address; a WebDAV address of it works too). Open
/// `login_url` in the browser, then wait with [`nextcloud_login_poll`].
pub async fn nextcloud_login_start(server: &str) -> RemoteResult<LoginFlow> {
    let base = base_url(server)?;
    let resp = http_client()
        .post(format!("{}/index.php/login/v2", server_root(&base)))
        .timeout(META_TIMEOUT)
        .send()
        .await
        .map_err(network)?;
    let unsupported = || RemoteError::Permanent("This server doesn't offer Nextcloud's browser sign-in.".into());
    if resp.status() == StatusCode::NOT_FOUND {
        return Err(unsupported());
    }
    if !resp.status().is_success() {
        return Err(classify(resp).await);
    }
    #[derive(Deserialize)]
    struct Start {
        poll: Poll,
        login: String,
    }
    #[derive(Deserialize)]
    struct Poll {
        token: String,
        endpoint: String,
    }
    let start: Start = resp.json().await.map_err(|_| unsupported())?;
    Ok(LoginFlow {
        login_url: start.login,
        poll_endpoint: start.poll.endpoint,
        token: start.poll.token,
        interval: Duration::from_secs(2),
        timeout: Duration::from_secs(20 * 60),
    })
}

/// Waits until the person has granted access in the browser; returns the server, the login
/// name and a new app password. Network trouble on the way is retried until the flow expires.
pub async fn nextcloud_login_poll(flow: &LoginFlow) -> RemoteResult<NextcloudLogin> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Done {
        server: String,
        login_name: String,
        app_password: String,
    }
    let http = http_client();
    let deadline = Instant::now() + flow.timeout;
    loop {
        let resp = http
            .post(&flow.poll_endpoint)
            .form(&[("token", flow.token.as_str())])
            .timeout(META_TIMEOUT)
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let done: Done = r
                    .json()
                    .await
                    .map_err(|e| RemoteError::Permanent(format!("The server's sign-in answer is unreadable: {e}")))?;
                return Ok(NextcloudLogin {
                    server: done.server,
                    login_name: done.login_name,
                    app_password: done.app_password,
                });
            }
            // 404: not granted yet.
            Ok(r) if r.status() == StatusCode::NOT_FOUND => {}
            Ok(r) => match classify(r).await {
                e if e.is_transient() => {}
                e => return Err(e),
            },
            Err(_) => {}
        }
        if Instant::now() + flow.interval > deadline {
            return Err(RemoteError::Auth("sign-in wasn't finished in the browser in time".into()));
        }
        tokio::time::sleep(flow.interval).await;
    }
}

/// Just enough XML for WebDAV answers: elements with resolved namespaces and their text.
/// No DTDs, so no entity expansion; nesting is capped.
mod xml {
    #[derive(Debug, Default)]
    pub struct Element {
        pub ns: String,
        pub name: String,
        pub text: String,
        pub children: Vec<Element>,
    }

    impl Element {
        pub fn is(&self, ns: &str, name: &str) -> bool {
            self.ns == ns && self.name == name
        }

        pub fn child(&self, ns: &str, name: &str) -> Option<&Element> {
            self.children.iter().find(|c| c.is(ns, name))
        }

        pub fn children<'a>(&'a self, ns: &'a str, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
            self.children.iter().filter(move |c| c.is(ns, name))
        }

        /// The first element of that name here or anywhere below.
        pub fn find(&self, ns: &str, name: &str) -> Option<&Element> {
            if self.is(ns, name) {
                return Some(self);
            }
            self.children.iter().find_map(|c| c.find(ns, name))
        }
    }

    const MAX_DEPTH: usize = 64;

    struct Open {
        el: Element,
        tag: String,
        /// Namespace declarations on this element: (prefix, uri), "" for the default.
        scope: Vec<(String, String)>,
    }

    pub fn parse(src: &str) -> Option<Element> {
        let mut stack: Vec<Open> = Vec::new();
        let mut rest = src;
        while !rest.is_empty() {
            let Some(after) = rest.strip_prefix('<') else {
                let end = rest.find('<').unwrap_or(rest.len());
                if let Some(top) = stack.last_mut() {
                    top.el.text.push_str(&unescape(&rest[..end]));
                }
                rest = &rest[end..];
                continue;
            };
            if let Some(r) = after.strip_prefix("!--") {
                rest = &r[r.find("-->")? + 3..];
                continue;
            }
            if let Some(r) = after.strip_prefix("![CDATA[") {
                let end = r.find("]]>")?;
                if let Some(top) = stack.last_mut() {
                    top.el.text.push_str(&r[..end]);
                }
                rest = &r[end + 3..];
                continue;
            }
            if after.starts_with('?') || after.starts_with('!') {
                rest = &after[after.find('>')? + 1..];
                continue;
            }
            let end = tag_end(after)?;
            let tag = &after[..end];
            rest = &after[end + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                let open = stack.pop()?;
                if open.tag != name.trim() {
                    return None;
                }
                match stack.last_mut() {
                    Some(parent) => parent.el.children.push(open.el),
                    None => return Some(open.el),
                }
                continue;
            }
            let (tag, closed) = match tag.strip_suffix('/') {
                Some(t) => (t, true),
                None => (tag, false),
            };
            let split = tag.find(char::is_whitespace).unwrap_or(tag.len());
            let (qname, attrs) = (&tag[..split], &tag[split..]);
            let scope: Vec<(String, String)> = attributes(attrs)
                .into_iter()
                .filter_map(|(k, v)| match k {
                    "xmlns" => Some((String::new(), v)),
                    _ => k.strip_prefix("xmlns:").map(|p| (p.to_string(), v)),
                })
                .collect();
            let (prefix, local) = qname.split_once(':').unwrap_or(("", qname));
            let ns = scope
                .iter()
                .chain(stack.iter().rev().flat_map(|o| o.scope.iter()))
                .find(|(p, _)| p == prefix)
                .map(|(_, uri)| uri.clone())
                .unwrap_or_default();
            let el = Element { ns, name: local.to_string(), ..Default::default() };
            if closed {
                match stack.last_mut() {
                    Some(parent) => parent.el.children.push(el),
                    None => return Some(el),
                }
            } else {
                if stack.len() >= MAX_DEPTH {
                    return None;
                }
                stack.push(Open { el, tag: qname.to_string(), scope });
            }
        }
        None
    }

    /// Where a tag's `>` is, outside quoted attribute values.
    fn tag_end(s: &str) -> Option<usize> {
        let mut quote = None;
        for (i, c) in s.char_indices() {
            match (quote, c) {
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, '>') => return Some(i),
                _ => {}
            }
        }
        None
    }

    fn attributes(mut s: &str) -> Vec<(&str, String)> {
        let mut out = Vec::new();
        loop {
            s = s.trim_start();
            let Some(eq) = s.find('=') else { break };
            let key = s[..eq].trim();
            let value = s[eq + 1..].trim_start();
            let Some(q) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else { break };
            let Some(close) = value[1..].find(q) else { break };
            out.push((key, unescape(&value[1..1 + close])));
            s = &value[close + 2..];
        }
        out
    }

    pub fn unescape(s: &str) -> String {
        if !s.contains('&') {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find('&') {
            out.push_str(&rest[..i]);
            rest = &rest[i..];
            let entity = rest[1..].find(';').filter(|&n| n <= 10).map(|n| &rest[1..1 + n]);
            let ch = entity.and_then(|e| match e {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let n = match e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => e.strip_prefix('#').and_then(|d| d.parse().ok()),
                    };
                    n.and_then(char::from_u32)
                }
            });
            match (ch, entity) {
                (Some(c), Some(e)) => {
                    out.push(c);
                    rest = &rest[e.len() + 2..];
                }
                _ => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        }
        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_say_what_they_offer() {
        for info in [&INFO, &NEXTCLOUD_INFO] {
            assert_eq!(info.caps.hash, None, "{}: freeing verifies by download", info.id);
            assert!(!info.caps.pin_versions && !info.caps.stable_ids && !info.caps.case_insensitive);
            assert_eq!(info.caps.chunk_unit, 1 << 20);
            assert_eq!((info.web_url)(Kind::File, "/a"), None);
        }
        assert!(!INFO.caps.resumable);
        assert!(NEXTCLOUD_INFO.caps.resumable);
        let SignIn::Form(fields) = INFO.sign_in else { panic!("webdav signs in with a form") };
        let keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
        assert_eq!(keys, ["url", "user", "password", "folder"]);
        assert!(fields.iter().all(|f| (f.kind == FieldKind::Secret) == (f.key == "password")));
        assert!(matches!(NEXTCLOUD_INFO.sign_in, SignIn::NextcloudLoginFlow));
    }

    #[test]
    fn ids_are_paths() {
        assert_eq!(norm_id(""), "/");
        assert_eq!(norm_id("Backups/Laptop/"), "/Backups/Laptop");
        assert_eq!(norm_id("//a//b"), "/a/b");
        assert_eq!(child_id("/", "a b"), "/a b");
        assert_eq!(child_id("/a", "b"), "/a/b");
        assert_eq!(parent_id("/a"), "/");
        assert_eq!(parent_id("/a/b c"), "/a");
        assert_eq!(name_of("/a/b c"), "b c");
        assert_eq!(name_of("/"), "");
    }

    #[test]
    fn encodes_and_decodes_odd_names() {
        for name in ["a b", "100% #1?", "a%20b", "über naïve 日本", "x&y=z+w;", "q\"uote'(1)[2]{3}", "tab\tnew\nline"] {
            let enc = encode_segment(name);
            assert!(enc.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)), "{enc}");
            assert_eq!(decode(&enc), name);
        }
        assert_eq!(encode_segment("a b#%"), "a%20b%23%25");
        assert_eq!(encode_path("/a b/c"), "/a%20b/c");
        assert_eq!(encode_path("/"), "");
        assert_eq!(decode("%E2%82%ac%zz%4"), "€%zz%4");
        assert_eq!(href_path("http://h:8080/dav/a%20b/"), "/dav/a b/");
        assert_eq!(href_path("/remote.php/dav/files/me/%23x"), "/remote.php/dav/files/me/#x");
    }

    #[test]
    fn server_addresses() {
        assert_eq!(base_url(" https://cloud.example.com/ ").unwrap(), "https://cloud.example.com");
        assert_eq!(base_url("https://h/remote.php/dav/files/me/?x=1#f").unwrap(), "https://h/remote.php/dav/files/me");
        assert!(base_url("cloud.example.com").is_err());
        assert!(base_url("ftp://h/").is_err());
        assert_eq!(server_root("https://h/nc/remote.php/dav/files/me"), "https://h/nc");
        assert_eq!(server_root("https://h/remote.php/webdav"), "https://h");
        assert_eq!(server_root("https://h/dav"), "https://h/dav");
        assert_eq!(files_user("https://h/remote.php/dav/files/a%40b.c/x").as_deref(), Some("a@b.c"));
        assert_eq!(files_user("https://h/remote.php/webdav"), None);
        let nc = ServerInfo { product: "Nextcloud".into(), user_id: "a b".into(), chunking: true, ..Default::default() };
        let r = Resolved::new("https://h/nc/remote.php/webdav/", nc).unwrap();
        assert_eq!(r.url("/x y", false), "https://h/nc/remote.php/dav/files/a%20b/x%20y");
        assert_eq!(r.url("/", false), "https://h/nc/remote.php/dav/files/a%20b/");
        assert_eq!(r.uploads.as_deref(), Some("https://h/nc/remote.php/dav/uploads/a%20b"));
        let plain = Resolved::new("https://h/dav/", ServerInfo::default()).unwrap();
        assert_eq!(plain.url("/d", true), "https://h/dav/d/");
        assert_eq!(plain.uploads, None);
    }

    #[test]
    fn replaced_names_and_dates() {
        assert_eq!(replaced_name("report.pdf", "2026-10-21"), "report (replaced 2026-10-21).pdf");
        assert_eq!(replaced_name(".bashrc", "2026-10-21"), ".bashrc (replaced 2026-10-21)");
        assert_eq!(replaced_name("Makefile", "2026-10-21"), "Makefile (replaced 2026-10-21)");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_747), (2026, 10, 21));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(today().len(), 10);
    }

    #[test]
    fn errors_map_to_remote_errors() {
        assert!(matches!(error_for(401, None, None), RemoteError::Auth(_)));
        assert_eq!(error_for(404, None, None), RemoteError::NotFound);
        assert_eq!(error_for(409, None, None), RemoteError::NotFound);
        assert!(matches!(error_for(429, Some(7), None), RemoteError::RateLimited { retry_after: Some(7), .. }));
        assert!(matches!(error_for(503, Some(2), None), RemoteError::RateLimited { retry_after: Some(2), .. }));
        assert!(matches!(error_for(503, None, None), RemoteError::Transient(_)));
        assert!(matches!(error_for(507, None, None), RemoteError::Quota(_)));
        assert!(matches!(error_for(423, None, None), RemoteError::Transient(_)));
        let RemoteError::Permanent(m) = error_for(403, None, Some("Forbidden name".into())) else { panic!() };
        assert_eq!(m, "The server refused (403 Forbidden name).");
        let body = r#"<?xml version="1.0"?><d:error xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns">
            <s:exception>Sabre\DAV\Exception\Forbidden</s:exception><s:message>No &amp; never</s:message></d:error>"#;
        assert_eq!(sabre_message(body).as_deref(), Some("No & never"));
        assert_eq!(sabre_message("<html><body>oops"), None);
    }

    #[test]
    fn checksums() {
        let sha = "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D";
        assert_eq!(sha1_of(&format!("MD5:5d41 SHA1:{sha} ADLER32:062c")).as_deref(), Some(&sha.to_lowercase()[..]));
        assert_eq!(sha1_of("MD5:5d41402abc4b2a76b9719d911017c592"), None);
        assert_eq!(sha1_of("SHA1:short"), None);
    }

    #[test]
    fn reads_multistatus_with_any_prefixes() {
        // Apache-style: D: prefix, lp1 for live properties, full URL hrefs, a 404 propstat.
        let apache = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:ns0="DAV:">
<D:response xmlns:lp1="DAV:">
<D:href>http://h/dav/a%20b/</D:href>
<D:propstat><D:prop><lp1:resourcetype><D:collection/></lp1:resourcetype></D:prop>
<D:status>HTTP/1.1 200 OK</D:status></D:propstat>
<D:propstat><D:prop><lp1:getcontentlength/><ns1:checksums xmlns:ns1="http://owncloud.org/ns"/></D:prop>
<D:status>HTTP/1.1 404 Not Found</D:status></D:propstat>
</D:response>
<D:response><D:href>/dav/a%20b/R&amp;D%20&#x2603;.txt</D:href>
<D:propstat><D:prop><D:resourcetype/><D:getcontentlength>12</D:getcontentlength></D:prop>
<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
<D:response><D:href>/dav/gone</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>
</D:multistatus>"#;
        let props = parse_multistatus(apache).unwrap();
        assert_eq!(props.len(), 2);
        assert_eq!((props[0].name.as_str(), props[0].folder, props[0].size), ("a b", true, None));
        assert_eq!((props[1].name.as_str(), props[1].folder, props[1].size), ("R&D ☃.txt", false, Some(12)));
        // Nextcloud-style, with a default namespace, checksums, quota and CDATA.
        let nc = r#"<?xml version="1.0"?>
<multistatus xmlns="DAV:" xmlns:oc="http://owncloud.org/ns"><!-- hi -->
<response><href><![CDATA[/remote.php/dav/files/me/x.txt]]></href><propstat><prop>
<resourcetype/><getcontentlength>5</getcontentlength>
<oc:checksums><oc:checksum>SHA1:aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d MD5:5d41402abc4b2a76b9719d911017c592</oc:checksum></oc:checksums>
<quota-used-bytes>100</quota-used-bytes><quota-available-bytes>-3</quota-available-bytes>
</prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        let p = &parse_multistatus(nc).unwrap()[0];
        assert_eq!(p.name, "x.txt");
        assert_eq!(p.sha1.as_deref(), Some("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"));
        assert_eq!((p.quota_used, p.quota_available), (Some(100), Some(-3)));
        assert!(parse_multistatus("<html>nope</html>").is_none());
        assert!(parse_multistatus("<d:multistatus xmlns:d=\"DAV:\"><d:response>").is_none());
    }

    #[test]
    fn xml_is_bounded() {
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert!(xml::parse(&deep).is_none());
        assert!(xml::parse("<a><b></a></b>").is_none());
        assert_eq!(xml::unescape("&lt;&#65;&#x42;&bogus;&amp"), "<AB&bogus;&amp");
        let el = xml::parse(r#"<x:a xmlns:x="u" k='v>w'>t</x:a>"#).unwrap();
        assert_eq!((el.ns.as_str(), el.name.as_str(), el.text.as_str()), ("u", "a", "t"));
    }

    #[test]
    fn sessions_are_json() {
        let s = Session { id: "clouddrop-1".into(), path: "/a b".into(), mime: "text/plain".into(), chunked: true };
        let back = Session::parse(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!((back.id.as_str(), back.path.as_str(), back.chunked), ("clouddrop-1", "/a b", true));
        assert_eq!(Session::parse("https://www.googleapis.com/upload/x").unwrap_err(), RemoteError::NotFound);
    }

    #[test]
    fn config_section_round_trips_and_stays_out_when_unset() {
        assert!(WebDavConfig::default().is_unset());
        let cfg = WebDavConfig {
            url: "https://cloud.example.com".into(),
            user: "me".into(),
            folder: "Backups".into(),
            server: Some(ServerInfo {
                product: "Nextcloud".into(),
                version: "29.0.4".into(),
                user_id: "me".into(),
                chunking: true,
            }),
        };
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(toml::from_str::<WebDavConfig>(&text).unwrap(), cfg);
        let plain: WebDavConfig = toml::from_str("url = \"https://h/dav\"\nuser = \"me\"").unwrap();
        assert_eq!((plain.folder.as_str(), plain.server), ("", None));
    }

    #[test]
    fn login_results_become_settings_and_secrets() {
        let login = NextcloudLogin { server: "https://c".into(), login_name: "me".into(), app_password: "pw".into() };
        let prev = WebDavConfig { folder: "Backups".into(), url: "https://old".into(), ..Default::default() };
        let s = login.settings(&prev);
        assert_eq!((s.url.as_str(), s.user.as_str(), s.folder.as_str()), ("https://c", "me", "Backups"));
        assert_eq!(login.secrets()["password"], "pw");
    }
}
