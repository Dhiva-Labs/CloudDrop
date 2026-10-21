//! S3-compatible object storage (Amazon S3, Backblaze B2, Wasabi, Cloudflare R2, MinIO, Garage):
//! a small client on reqwest that signs its requests with AWS Signature Version 4 itself, since
//! the AWS SDK is large and every crate is vendored into the offline build. The engine reaches
//! it through [`crate::remote::Remote`].
//!
//! **Ids** are keys relative to the configured prefix. Files are `CloudDrop/photos/a.jpg`;
//! folders end in `/` (`CloudDrop/photos/`) and are recorded with a zero-byte marker object of
//! that name, so empty folders exist. A prefix with objects under it but no marker counts as a
//! folder too. The top parent is `""`, the prefix itself. Ids are paths, so `stable_ids` is false.
//!
//! **Hashes** are SHA-256. A file sent in one PUT carries `x-amz-checksum-sha256`, which the
//! service checks and reports back. Where a service doesn't keep that checksum, the object's
//! ETag (the MD5 of its content, computed by the service) is compared with the MD5 recorded next
//! to the SHA-256 in the object's metadata at upload: the same proof Google Drive's
//! `md5Checksum` gives. A multipart object only has a checksum of its parts' checksums, so `get`
//! reports no hash for it and the engine proves it by downloading.
//!
//! **Uploads** bigger than one part are multipart uploads, and `ListParts` says how far one got.
//! Parts are [`PART_UNIT`], or a multiple of it for files that would need over 10,000 parts.
//!
//! **Renames** copy every object to its new key and check the copy. The old key is deleted only
//! when the bucket has versioning (the delete then just hides it; the content stays as a
//! noncurrent version). Otherwise the old key stays where it was, and the log says so.

use crate::hash::Hasher;
use crate::remote::{
    Capabilities, ContentHash, Field, FieldKind, HashKind, ProviderInfo, RemoteError, RemoteMeta, RemoteResult,
    SignIn, StorageQuota, UploadStatus,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use bytes::Bytes;
use futures_util::StreamExt;
use md5::Md5;
use reqwest::{Method, Response, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncWriteExt;

/// Upload parts are this size or a multiple of it. S3 wants parts of at least 5 MiB, except the last.
pub const PART_UNIT: usize = 8 * 1024 * 1024;
/// Parts one multipart upload may have.
const MAX_PARTS: u64 = 10_000;
/// The biggest object one CopyObject copies; bigger ones are copied part by part.
const COPY_MAX: u64 = 5 * 1024 * 1024 * 1024;
/// Part size when copying part by part (5 TiB, the largest object, is 5,120 parts).
const COPY_PART: u64 = 1024 * 1024 * 1024;
/// `storage_quota` adds up at most this many listing pages (1,000 objects each).
const QUOTA_PAGES: usize = 20;
const QUOTA_EVERY: Duration = Duration::from_secs(3600);
const QUOTA_TOO_BIG_EVERY: Duration = Duration::from_secs(24 * 3600);
const VERSIONING_EVERY: Duration = Duration::from_secs(3600);
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Metadata CloudDrop records on the objects it uploads (`x-amz-meta-…`).
const META_SHA256: &str = "clouddrop-sha256";
const META_MD5: &str = "clouddrop-md5";
const META_UPLOAD: &str = "clouddrop-upload";

pub static INFO: ProviderInfo = ProviderInfo {
    experimental: false,
    id: "s3",
    name: "S3-compatible storage",
    caps: Capabilities {
        hash: Some(HashKind::Sha256),
        pin_versions: false,
        stable_ids: false,
        resumable: true,
        case_insensitive: false,
        forbidden_chars: "",
        chunk_unit: PART_UNIT,
    },
    sign_in: SignIn::Form(&[
        Field {
            key: "endpoint",
            label: "Endpoint",
            kind: FieldKind::Url,
            required: false,
            hint: "https://s3.us-west-004.backblazeb2.com — leave empty for Amazon S3",
        },
        Field {
            key: "region",
            label: "Region",
            kind: FieldKind::Text,
            required: false,
            hint: "us-east-1 when empty; \"auto\" for Cloudflare R2",
        },
        Field {
            key: "bucket",
            label: "Bucket",
            kind: FieldKind::Text,
            required: true,
            hint: "my-backups",
        },
        Field {
            key: "prefix",
            label: "Folder in the bucket",
            kind: FieldKind::Text,
            required: false,
            hint: "Where CloudDrop's folder goes, e.g. laptop; empty for the top of the bucket",
        },
        Field {
            key: "access_key_id",
            label: "Access key ID",
            kind: FieldKind::Text,
            required: true,
            hint: "A key that can only reach this bucket (and folder)",
        },
        Field {
            key: "secret_access_key",
            label: "Secret access key",
            kind: FieldKind::Secret,
            required: true,
            hint: "",
        },
        Field {
            key: "addressing",
            label: "Addressing",
            kind: FieldKind::Text,
            required: false,
            hint: "path or virtual; empty chooses virtual-hosted for Amazon S3 and path-style elsewhere",
        },
    ]),
    web_url: |_, _| None,
};

/// The `[s3]` section of config.toml. The secret key is in the credentials store
/// ([`crate::credentials`], `secret_access_key`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct S3Config {
    /// The service's address, e.g. `https://s3.us-west-004.backblazeb2.com`; empty = Amazon S3.
    pub endpoint: String,
    /// Empty = us-east-1.
    pub region: String,
    pub bucket: String,
    /// Folder inside the bucket that holds the CloudDrop folder; empty = the top of the bucket.
    pub prefix: String,
    pub access_key_id: String,
    /// `path` (`https://host/bucket/key`) or `virtual` (`https://bucket.host/key`); empty
    /// chooses virtual-hosted for Amazon S3 and path-style elsewhere.
    pub addressing: String,
}

impl S3Config {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The secret key `clouddrop setup` saved, if any.
pub fn saved_secret() -> Option<String> {
    match crate::credentials::load(INFO.id) {
        Ok(secrets) => secrets
            .and_then(|m| m.get("secret_access_key")?.as_str().map(str::to_string))
            .filter(|s| !s.is_empty()),
        Err(e) => {
            tracing::warn!("cannot read the S3 secret key: {e:#}");
            None
        }
    }
}

pub struct S3 {
    http: reqwest::Client,
    /// `https://host[:port]`.
    origin: String,
    /// What goes in the Host header (and is signed).
    host: String,
    /// The endpoint's own path ("" or e.g. "/s3"), the start of every canonical URI.
    base_path: String,
    path_style: bool,
    bucket: String,
    /// The configured prefix with a trailing "/", or "" for the top of the bucket.
    prefix: String,
    region: String,
    access_key: String,
    secret: Mutex<Option<String>>,
    /// Read the secret from the credentials store again while there is none.
    reload_secret: bool,
    /// Cleared once the service turns out not to take `x-amz-checksum-*` headers.
    checksums: AtomicBool,
    versioning: Mutex<Option<(Instant, bool)>>,
    /// Multipart uploads in progress in this process, by upload id.
    uploads: Mutex<HashMap<String, UploadState>>,
    quota: Mutex<Option<(Instant, Duration, StorageQuota)>>,
    part_unit: u64,
    copy_max: u64,
    copy_part: u64,
    quota_pages: usize,
}

/// What an upload session string holds (JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Session {
    /// The file's id, and its full key in the bucket.
    id: String,
    key: String,
    /// The multipart upload; None for a file small enough for one PUT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upload_id: Option<String>,
    part_size: u64,
    /// The parts carry SHA-256 checksums (the upload was created asking for them).
    #[serde(default)]
    checksum: bool,
    mime: String,
    /// Recorded on the object (`x-amz-meta-clouddrop-upload`), to recognise it after a crash.
    nonce: String,
    /// The file's earlier id, when the upload also renames or moves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replaces: Option<String>,
}

impl Session {
    /// A session this version can't read is as good as expired.
    fn parse(s: &str) -> RemoteResult<Session> {
        serde_json::from_str(s).map_err(|_| RemoteError::NotFound)
    }
}

/// An upload this process is sending.
#[derive(Default)]
struct UploadState {
    /// Bytes received but not sent yet (less than a part), starting at offset `held_from`.
    held: Vec<u8>,
    held_from: u64,
    /// Parts sent so far, by number.
    parts: BTreeMap<u32, Part>,
}

#[derive(Debug, Clone, PartialEq)]
struct Part {
    number: u32,
    /// Without quotes.
    etag: String,
    size: u64,
    /// Base64, as S3 has it.
    sha256: Option<String>,
}

/// An object's metadata from HEAD.
#[derive(Debug, Clone, Default)]
struct Head {
    size: u64,
    /// Without quotes, lowercase.
    etag: String,
    /// The service's SHA-256 of the whole content (hex), if it keeps one.
    sha256: Option<String>,
    content_type: Option<String>,
    /// `x-amz-meta-*`, without the prefix.
    meta: BTreeMap<String, String>,
}

impl Head {
    /// SHA-256 of the content, from the service, or else from the metadata CloudDrop recorded
    /// when the MD5 recorded with it is the object's ETag.
    fn hash(&self) -> Option<ContentHash> {
        if let Some(v) = &self.sha256 {
            return Some(ContentHash { kind: HashKind::Sha256, value: v.clone() });
        }
        let sha = self.meta.get(META_SHA256)?;
        let md5 = self.meta.get(META_MD5)?;
        (is_hex(sha, 64) && is_hex(md5, 32) && md5.eq_ignore_ascii_case(&self.etag))
            .then(|| ContentHash { kind: HashKind::Sha256, value: sha.to_ascii_lowercase() })
    }

    /// Whether `other` holds the same content as far as the service lets us tell (checking a
    /// copy): the same size, and the same SHA-256 or else the same MD5 ETag where both have one.
    fn same_content(&self, other: &Head) -> bool {
        self.size == other.size && self.compare(other).unwrap_or(true)
    }

    /// Whether `other` is known to hold the same content (before overwriting it): the same
    /// size and the same SHA-256 or MD5 ETag. Sizes alone don't tell.
    fn surely_same(&self, other: &Head) -> bool {
        self.size == other.size && self.compare(other).unwrap_or(false)
    }

    /// Same SHA-256, or else the same MD5 ETag; None where they can't be compared.
    fn compare(&self, other: &Head) -> Option<bool> {
        if let (Some(a), Some(b)) = (self.hash(), other.hash()) {
            return Some(a == b);
        }
        Some(plain_md5(&self.etag)? == plain_md5(&other.etag)?)
    }
}

/// An object in a listing.
#[derive(Debug, Clone, PartialEq)]
struct Listed {
    key: String,
    size: u64,
}

/// A request that failed: the service's own error, or no answer at all.
#[derive(Debug)]
enum Failure {
    Network(String),
    S3 { status: u16, code: String, message: String, retry_after: Option<u64> },
}

impl From<Failure> for RemoteError {
    fn from(f: Failure) -> Self {
        match f {
            Failure::Network(m) => RemoteError::Transient(format!("network error: {m}")),
            Failure::S3 { status, code, message, retry_after } => s3_error(status, &code, &message, retry_after),
        }
    }
}

impl Failure {
    /// The service doesn't take `x-amz-checksum-*` headers.
    fn refuses_checksums(&self) -> bool {
        matches!(self, Failure::S3 { code, message, .. }
            if code == "NotImplemented"
                || (matches!(code.as_str(), "InvalidArgument" | "InvalidRequest")
                    && message.to_ascii_lowercase().contains("checksum")))
    }
}

/// One request to the service.
struct Req {
    method: Method,
    /// Full key in the bucket; "" for the bucket itself.
    key: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Bytes,
    /// SHA-256 of `body`, when the caller has it already.
    body_sha256: Option<[u8; 32]>,
    timeout: Duration,
}

impl Req {
    fn new(method: Method, key: &str) -> Self {
        Req {
            method,
            key: key.to_string(),
            query: Vec::new(),
            headers: Vec::new(),
            body: Bytes::new(),
            body_sha256: None,
            timeout: Duration::from_secs(120),
        }
    }

    fn query(mut self, k: &str, v: impl ToString) -> Self {
        self.query.push((k.to_string(), v.to_string()));
        self
    }

    fn header(mut self, k: &str, v: impl ToString) -> Self {
        self.headers.push((k.to_ascii_lowercase(), v.to_string()));
        self
    }

    fn body(mut self, body: impl Into<Bytes>, sha256: Option<[u8; 32]>) -> Self {
        self.body = body.into();
        self.body_sha256 = sha256;
        self
    }

    fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

impl S3 {
    pub fn new(http: reqwest::Client, cfg: &S3Config, secret: Option<String>) -> Self {
        let region = match cfg.region.trim() {
            "" => "us-east-1".to_string(),
            r => r.to_string(),
        };
        let endpoint = match cfg.endpoint.trim().trim_end_matches('/') {
            "" => format!("https://s3.{region}.amazonaws.com"),
            e if e.contains("://") => e.to_string(),
            e => format!("https://{e}"),
        };
        let bucket = cfg.bucket.trim().to_string();
        let (scheme, host, base_path) = match url::Url::parse(&endpoint) {
            Ok(u) => {
                let host = match u.port() {
                    Some(p) => format!("{}:{p}", u.host_str().unwrap_or_default()),
                    None => u.host_str().unwrap_or_default().to_string(),
                };
                (u.scheme().to_string(), host, u.path().trim_end_matches('/').to_string())
            }
            Err(e) => {
                tracing::warn!("S3 endpoint {endpoint:?} is not an address: {e}");
                ("https".to_string(), endpoint.clone(), String::new())
            }
        };
        let path_style = match cfg.addressing.trim().to_ascii_lowercase() {
            a if a.starts_with("path") => true,
            a if a.starts_with("virtual") => false,
            _ => !(host.ends_with(".amazonaws.com") && !bucket.contains('.')),
        };
        let host = if path_style { host } else { format!("{bucket}.{host}") };
        let prefix: Vec<&str> = cfg
            .prefix
            .split('/')
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .collect();
        let prefix = if prefix.is_empty() { String::new() } else { format!("{}/", prefix.join("/")) };
        S3 {
            http,
            origin: format!("{scheme}://{host}"),
            host,
            base_path,
            path_style,
            bucket,
            prefix,
            region,
            access_key: cfg.access_key_id.trim().to_string(),
            secret: Mutex::new(secret.filter(|s| !s.is_empty())),
            reload_secret: false,
            checksums: AtomicBool::new(true),
            versioning: Mutex::new(None),
            uploads: Mutex::new(HashMap::new()),
            quota: Mutex::new(None),
            part_unit: PART_UNIT as u64,
            copy_max: COPY_MAX,
            copy_part: COPY_PART,
            quota_pages: QUOTA_PAGES,
        }
    }

    /// With the secret key from the credentials store, read again on use until there is one
    /// (so a running service picks up `clouddrop setup`).
    pub fn with_saved_secret(http: reqwest::Client, cfg: &S3Config) -> Self {
        let mut s = Self::new(http, cfg, saved_secret());
        s.reload_secret = true;
        s
    }

    fn secret(&self) -> Option<String> {
        let mut secret = self.secret.lock().unwrap();
        if secret.is_none() && self.reload_secret {
            *secret = saved_secret();
        }
        secret.clone()
    }

    fn key(&self, id: &str) -> String {
        format!("{}{id}", self.prefix)
    }

    /// (URL, canonical URI, canonical query string) for a key in the bucket.
    fn target(&self, key: &str, query: &[(String, String)]) -> (String, String, String) {
        let mut uri = self.base_path.clone();
        if self.path_style {
            uri.push('/');
            uri.push_str(&uri_encode(&self.bucket, true));
            if !key.is_empty() {
                uri.push('/');
            }
        } else {
            uri.push('/');
        }
        uri.push_str(&uri_encode(key, true));
        let query = canonical_query(query);
        let mut url = format!("{}{uri}", self.origin);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        (url, uri, query)
    }

    /// Signs and sends a request; any answer but 2xx is a failure.
    async fn send(&self, req: &Req) -> Result<Response, Failure> {
        let resp = self.send_raw(req).await?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        let body = resp.text().await.unwrap_or_default();
        if body.trim().is_empty() && req.method == Method::HEAD && matches!(status, 400 | 403) {
            return Err(self.explain(status, &req.key).await);
        }
        let (code, message) = error_body(&body);
        Err(Failure::S3 { status, code, message, retry_after })
    }

    async fn send_raw(&self, req: &Req) -> Result<Response, Failure> {
        let secret = self.secret().ok_or_else(|| Failure::S3 {
            status: 401,
            code: "NoSecret".into(),
            message: "no S3 secret key is saved; run `clouddrop setup --provider s3`".into(),
            retry_after: None,
        })?;
        if self.bucket.is_empty() {
            return Err(Failure::S3 {
                status: 400,
                code: "NoBucket".into(),
                message: "no S3 bucket is set; run `clouddrop setup --provider s3`".into(),
                retry_after: None,
            });
        }
        let (url, uri, query) = self.target(&req.key, &req.query);
        let payload = match req.body_sha256 {
            Some(d) => hex::encode(d),
            None if req.body.is_empty() => EMPTY_SHA256.to_string(),
            None => hex::encode(Sha256::digest(&req.body)),
        };
        let date = amz_date(SystemTime::now());
        let mut headers = req.headers.clone();
        headers.push(("host".into(), self.host.clone()));
        headers.push(("x-amz-content-sha256".into(), payload.clone()));
        headers.push(("x-amz-date".into(), date.clone()));
        let key = SigningKey { access_key: &self.access_key, secret: &secret, region: &self.region, service: "s3" };
        let auth = key.authorization(req.method.as_str(), &uri, &query, &headers, &payload, &date);
        let mut rb = self
            .http
            .request(req.method.clone(), &url)
            .timeout(req.timeout)
            .header(reqwest::header::AUTHORIZATION, auth);
        for (k, v) in headers.iter().filter(|(k, _)| k != "host") {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if matches!(req.method, Method::PUT | Method::POST) {
            rb = rb.body(req.body.clone());
        }
        rb.send().await.map_err(|e| Failure::Network(e.to_string()))
    }

    /// HEAD answers 400 and 403 without saying why: ask a listing, which does.
    async fn explain(&self, status: u16, key: &str) -> Failure {
        let probe = Req::new(Method::GET, "").query("list-type", 2).query("max-keys", 0).query("prefix", key);
        let plain = Failure::S3 {
            status,
            code: if status == 403 { "AccessDenied" } else { "BadRequest" }.into(),
            message: if status == 403 { "access to this object was denied" } else { "the service refused the request" }
                .into(),
            retry_after: None,
        };
        match self.send_raw(&probe).await {
            Ok(resp) if !resp.status().is_success() => {
                let probe_status = resp.status().as_u16();
                let (code, message) = error_body(&resp.text().await.unwrap_or_default());
                if code.is_empty() {
                    plain
                } else {
                    Failure::S3 { status: probe_status, code, message, retry_after: None }
                }
            }
            Err(f @ Failure::S3 { .. }) => f,
            Ok(_) | Err(Failure::Network(_)) => plain,
        }
    }

    /// Sends a request built with or without `x-amz-checksum-*` headers: with them, unless the
    /// service turned out not to take them (then once more without, remembering that).
    async fn send_checksummed(&self, build: impl Fn(bool) -> Req) -> Result<(Response, bool), Failure> {
        let with = self.checksums.load(Ordering::Relaxed);
        match self.send(&build(with)).await {
            Err(f) if with && f.refuses_checksums() => {
                tracing::info!("S3: the service doesn't take SHA-256 checksums; uploads are proven by their MD5 instead");
                self.checksums.store(false, Ordering::Relaxed);
                Ok((self.send(&build(false)).await?, false))
            }
            r => r.map(|resp| (resp, with)),
        }
    }

    async fn head(&self, key: &str) -> RemoteResult<Head> {
        let mut req = Req::new(Method::HEAD, key);
        if self.checksums.load(Ordering::Relaxed) {
            req = req.header("x-amz-checksum-mode", "ENABLED");
        }
        let resp = self.send(&req).await?;
        Ok(parse_head(resp.headers()))
    }

    /// One page of ListObjectsV2 under `prefix`: (objects, continuation token).
    async fn list_page(&self, prefix: &str, max: u32, token: Option<&str>) -> RemoteResult<(Vec<Listed>, Option<String>)> {
        let mut req = Req::new(Method::GET, "")
            .query("list-type", 2)
            .query("encoding-type", "url")
            .query("max-keys", max)
            .query("prefix", prefix);
        if let Some(t) = token {
            req = req.query("continuation-token", t);
        }
        let xml = text(self.send(&req).await?).await?;
        let encoded = xml_first(&xml, "EncodingType").is_some_and(|e| e.eq_ignore_ascii_case("url"));
        let objects = xml_all(&xml, "Contents")
            .into_iter()
            .map(|c| Listed {
                key: listed_key(xml_first(c, "Key").unwrap_or_default(), encoded),
                size: xml_first(c, "Size").and_then(|s| s.trim().parse().ok()).unwrap_or(0),
            })
            .collect();
        let truncated = xml_first(&xml, "IsTruncated").is_some_and(|t| t.trim() == "true");
        let next = xml_first(&xml, "NextContinuationToken").map(xml_text).filter(|_| truncated);
        Ok((objects, next))
    }

    /// Every object under `prefix`.
    async fn list_all(&self, prefix: &str) -> RemoteResult<Vec<Listed>> {
        let mut out = Vec::new();
        let mut token = None;
        loop {
            let (objects, next) = self.list_page(prefix, 1000, token.as_deref()).await?;
            out.extend(objects);
            match next {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
    }

    /// A folder exists when its marker or anything else is under its prefix.
    async fn folder_exists(&self, id: &str) -> RemoteResult<bool> {
        if id.is_empty() {
            return Ok(true);
        }
        Ok(!self.list_page(&self.key(id), 1, None).await?.0.is_empty())
    }

    /// Whether the bucket keeps versions ("Suspended" doesn't count: a delete there removes
    /// objects written since). Asked once an hour; a service that can't say counts as no.
    async fn versioning(&self) -> RemoteResult<bool> {
        if let Some((at, v)) = *self.versioning.lock().unwrap()
            && at.elapsed() < VERSIONING_EVERY
        {
            return Ok(v);
        }
        let enabled = match self.send(&Req::new(Method::GET, "").query("versioning", "")).await {
            Ok(resp) => xml_first(&text(resp).await?, "Status").is_some_and(|s| s.trim() == "Enabled"),
            Err(f) => match RemoteError::from(f) {
                e @ (RemoteError::Transient(_) | RemoteError::RateLimited { .. } | RemoteError::Auth(_)) => return Err(e),
                e => {
                    tracing::warn!("S3: cannot tell whether the bucket keeps versions ({e}); treating it as not");
                    false
                }
            },
        };
        *self.versioning.lock().unwrap() = Some((Instant::now(), enabled));
        Ok(enabled)
    }

    /// What CloudDrop holds: the bytes of everything under the prefix, as (no limit, used,
    /// used). Listing costs a request per 1,000 objects, so it is done at most once an hour,
    /// and above 20,000 objects not at all (then the answer is (None, 0, 0), once a day).
    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        if let Some((at, every, q)) = *self.quota.lock().unwrap()
            && at.elapsed() < every
        {
            return Ok(q);
        }
        let mut used = 0u64;
        let mut token = None;
        let mut result = ((None, 0, 0), QUOTA_TOO_BIG_EVERY);
        for _ in 0..self.quota_pages {
            let (objects, next) = self.list_page(&self.prefix, 1000, token.as_deref()).await?;
            used += objects.iter().map(|o| o.size).sum::<u64>();
            match next {
                Some(t) => token = Some(t),
                None => {
                    result = ((None, used, used), QUOTA_EVERY);
                    break;
                }
            }
        }
        *self.quota.lock().unwrap() = Some((Instant::now(), result.1, result.0));
        Ok(result.0)
    }

    /// The configured prefix (`""`).
    pub async fn top_parent(&self) -> RemoteResult<String> {
        Ok(String::new())
    }

    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        if id.is_empty() || id.ends_with('/') {
            return match self.folder_exists(id).await? {
                true => Ok(folder_meta(id)),
                false => Err(RemoteError::NotFound),
            };
        }
        Ok(file_meta(id, &self.head(&self.key(id)).await?))
    }

    /// The file or folder named `name` in `parent`. Files only when `folder` is false: in S3
    /// a file `x` and a folder `x/` are different keys, so a folder never stands in the way.
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        let id = child_id(parent, name, folder);
        if folder {
            return Ok(if self.folder_exists(&id).await? { vec![folder_meta(&id)] } else { vec![] });
        }
        match self.head(&self.key(&id)).await {
            Ok(h) => Ok(vec![file_meta(&id, &h)]),
            Err(RemoteError::NotFound) => Ok(vec![]),
            Err(e) => Err(e),
        }
    }

    /// Writes the folder's marker object. NotFound when `parent` is gone.
    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        if !self.folder_exists(&dir_id(parent)).await? {
            return Err(RemoteError::NotFound);
        }
        let id = child_id(parent, name, true);
        self.put_marker(&self.key(&id)).await?;
        Ok(folder_meta(&id))
    }

    async fn put_marker(&self, key: &str) -> RemoteResult<()> {
        let md5 = B64.encode(Md5::digest(b""));
        let req = Req::new(Method::PUT, key)
            .header("content-type", "application/x-directory")
            .header("content-md5", md5);
        self.send(&req).await?;
        Ok(())
    }

    /// Renames and/or moves a file or folder by copying it (every object under a folder),
    /// checking each copy, and deleting the old keys where the bucket keeps versions. Returns
    /// the new id.
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        let folder = id.ends_with('/');
        let (old_parent, old_name) = split_id(id);
        let parent = parents.map_or(old_parent, |(new, _)| new);
        let new_id = child_id(parent, name.unwrap_or(old_name), folder);
        if new_id == id {
            return self.get(id).await;
        }
        if folder {
            self.move_folder(id, &new_id).await
        } else {
            self.move_file(id, &new_id).await
        }
    }

    async fn move_file(&self, id: &str, new_id: &str) -> RemoteResult<RemoteMeta> {
        let (src, dst) = (self.key(id), self.key(new_id));
        let head = match self.head(&src).await {
            Ok(h) => h,
            // Moved before an interruption, with the old key deleted already?
            Err(RemoteError::NotFound) => return self.get(new_id).await,
            Err(e) => return Err(e),
        };
        let copied = self.copy_checked(&src, &head, &dst).await?;
        self.retire(&[src], id, new_id).await?;
        Ok(file_meta(new_id, &copied))
    }

    async fn move_folder(&self, id: &str, new_id: &str) -> RemoteResult<RemoteMeta> {
        if new_id.starts_with(id) {
            return Err(RemoteError::Permanent("a folder cannot be moved into itself".into()));
        }
        let (src, dst) = (self.key(id), self.key(new_id));
        let objects = self.list_all(&src).await?;
        if objects.is_empty() {
            // Moved before an interruption, with the old keys deleted already?
            return self.get(new_id).await;
        }
        for o in &objects {
            let head = match self.head(&o.key).await {
                Ok(h) => h,
                Err(RemoteError::NotFound) => continue,
                Err(e) => return Err(e),
            };
            self.copy_checked(&o.key, &head, &format!("{dst}{}", &o.key[src.len()..])).await?;
        }
        if !objects.iter().any(|o| o.key == src) {
            self.put_marker(&dst).await?;
        }
        let keys: Vec<String> = objects.into_iter().map(|o| o.key).collect();
        self.retire(&keys, id, new_id).await?;
        Ok(folder_meta(new_id))
    }

    /// Copies `src` to `dst` and checks the copy holds the same content. Different content
    /// already at `dst` (left by an earlier rename in a bucket without versioning) is kept
    /// aside first. Returns the copy's metadata.
    async fn copy_checked(&self, src: &str, head: &Head, dst: &str) -> RemoteResult<Head> {
        match self.head(dst).await {
            Ok(existing) if existing.surely_same(head) => {}
            Ok(existing) => {
                self.keep_aside(dst, &existing).await?;
            }
            Err(RemoteError::NotFound) => {}
            Err(e) => return Err(e),
        }
        self.copy(src, head, dst).await?;
        let copied = self.head(dst).await?;
        if !copied.same_content(head) {
            return Err(RemoteError::Permanent(format!(
                "the copy of {src} doesn't match the original, which was kept"
            )));
        }
        Ok(copied)
    }

    /// Deletes `keys` after their content was copied to `to` and checked, but only where the
    /// bucket keeps versions: the delete then hides them and the content stays as noncurrent
    /// versions. Otherwise they stay.
    async fn retire(&self, keys: &[String], from: &str, to: &str) -> RemoteResult<()> {
        if keys.is_empty() {
            return Ok(());
        }
        if !self.versioning().await? {
            tracing::info!(
                "S3: kept {from} after copying it to {to}: the bucket has no versioning, so CloudDrop leaves the old {}",
                if keys.len() == 1 { "object" } else { "objects" }
            );
            return Ok(());
        }
        for k in keys {
            match self.send(&Req::new(Method::DELETE, k)).await.map_err(RemoteError::from) {
                Ok(_) | Err(RemoteError::NotFound) => {}
                // A key without delete permission: the copies stand, the old keys stay.
                Err(RemoteError::Permanent(m)) => {
                    tracing::warn!("S3: kept {from} after copying it to {to}: cannot delete {k} ({m})");
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// A server-side copy, part by part above 5 GiB.
    async fn copy(&self, src: &str, head: &Head, dst: &str) -> RemoteResult<()> {
        let source = format!("{}/{}", self.bucket, uri_encode(src, true));
        if head.size > self.copy_max {
            return self.copy_parts(&source, head, dst).await;
        }
        let (resp, _) = self
            .send_checksummed(|ck| {
                let req = Req::new(Method::PUT, dst).header("x-amz-copy-source", &source).timeout(Duration::from_secs(3600));
                if ck { req.header("x-amz-checksum-algorithm", "SHA256") } else { req }
            })
            .await?;
        xml_ok(&text(resp).await?)?;
        Ok(())
    }

    async fn copy_parts(&self, source: &str, head: &Head, dst: &str) -> RemoteResult<()> {
        let mut meta: Vec<(String, String)> =
            head.meta.iter().map(|(k, v)| (format!("x-amz-meta-{k}"), v.clone())).collect();
        if let Some(ct) = &head.content_type {
            meta.push(("content-type".into(), ct.clone()));
        }
        let (upload_id, checksum) = self.create_multipart(dst, &meta).await?;
        let result = async {
            let mut parts = Vec::new();
            let mut at = 0;
            while at < head.size {
                let end = (at + self.copy_part).min(head.size);
                let number = parts.len() as u32 + 1;
                let req = Req::new(Method::PUT, dst)
                    .query("partNumber", number)
                    .query("uploadId", &upload_id)
                    .header("x-amz-copy-source", source)
                    .header("x-amz-copy-source-range", format!("bytes={at}-{}", end - 1))
                    .timeout(Duration::from_secs(3600));
                let xml = text(self.send(&req).await?).await?;
                xml_ok(&xml)?;
                parts.push(Part {
                    number,
                    etag: unquote(&xml_text(xml_first(&xml, "ETag").unwrap_or_default())),
                    size: end - at,
                    sha256: xml_first(&xml, "ChecksumSHA256").map(xml_text).filter(|_| checksum),
                });
                at = end;
            }
            self.complete(dst, &upload_id, &parts, checksum).await
        }
        .await;
        if result.is_err() {
            self.abort(dst, &upload_id).await;
        }
        result
    }

    /// Keeps the current content of a file as "<stem> (replaced YYYY-MM-DD).<ext>" next to it
    /// (a server-side copy), so replacing the content never loses it.
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        let key = self.key(id);
        let head = self.head(&key).await?;
        self.keep_aside(&key, &head).await?;
        Ok(())
    }

    /// Copies the object at `key` to the first free "(replaced …)" name next to it.
    async fn keep_aside(&self, key: &str, head: &Head) -> RemoteResult<String> {
        let (dir, name) = split_id(key);
        let date = utc_date(SystemTime::now());
        for n in 1..=100 {
            let aside = format!("{dir}{}", replaced_name(name, &date, n));
            match self.head(&aside).await {
                Ok(_) => continue,
                Err(RemoteError::NotFound) => {}
                Err(e) => return Err(e),
            }
            self.copy(key, head, &aside).await?;
            if !self.head(&aside).await?.same_content(head) {
                return Err(RemoteError::Permanent(format!("the copy kept of {name} doesn't match it")));
            }
            return Ok(aside);
        }
        Err(RemoteError::Permanent(format!("too many replaced copies of {name} today")))
    }

    /// Opens an upload; returns the session (JSON). Files of one part are sent with a single
    /// PUT later, bigger ones get a multipart upload now. NotFound when `parent` is gone.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        mime: &str,
    ) -> RemoteResult<String> {
        let parent = dir_id(parents.0);
        if !self.folder_exists(&parent).await? {
            return Err(RemoteError::NotFound);
        }
        let id = child_id(&parent, name, false);
        let key = self.key(&id);
        let mut session = Session {
            id: id.clone(),
            key: key.clone(),
            upload_id: None,
            part_size: part_size(size, self.part_unit),
            checksum: false,
            mime: mime.to_string(),
            nonce: format!("{:032x}", rand::random::<u128>()),
            replaces: existing.filter(|old| *old != id).map(str::to_string),
        };
        if size > self.part_unit {
            self.abort_stale(&key).await;
            let headers = vec![
                ("content-type".to_string(), mime.to_string()),
                (format!("x-amz-meta-{META_UPLOAD}"), session.nonce.clone()),
            ];
            let (upload_id, checksum) = self.create_multipart(&key, &headers).await?;
            session.upload_id = Some(upload_id);
            session.checksum = checksum;
        }
        Ok(serde_json::to_string(&session).expect("session json"))
    }

    /// Returns (upload id, whether its parts carry SHA-256 checksums).
    async fn create_multipart(&self, key: &str, headers: &[(String, String)]) -> RemoteResult<(String, bool)> {
        let (resp, checksum) = self
            .send_checksummed(|ck| {
                let mut req = Req::new(Method::POST, key).query("uploads", "");
                req.headers.extend(headers.iter().cloned());
                if ck { req.header("x-amz-checksum-algorithm", "SHA256") } else { req }
            })
            .await?;
        let xml = text(resp).await?;
        let id = xml_first(&xml, "UploadId")
            .map(xml_text)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| RemoteError::Transient("S3 did not return an upload id".into()))?;
        Ok((id, checksum))
    }

    /// Aborts multipart uploads of `key` left by earlier attempts (their parts cost storage
    /// until aborted). Best effort.
    async fn abort_stale(&self, key: &str) {
        let req = Req::new(Method::GET, "").query("uploads", "").query("encoding-type", "url").query("prefix", key);
        let xml = match self.send(&req).await {
            Ok(resp) => resp.text().await.unwrap_or_default(),
            Err(f) => {
                tracing::debug!("S3: cannot list unfinished uploads: {}", RemoteError::from(f));
                return;
            }
        };
        let encoded = xml_first(&xml, "EncodingType").is_some_and(|e| e.eq_ignore_ascii_case("url"));
        for u in xml_all(&xml, "Upload") {
            if listed_key(xml_first(u, "Key").unwrap_or_default(), encoded) == key
                && let Some(id) = xml_first(u, "UploadId").map(xml_text)
            {
                self.abort(key, &id).await;
            }
        }
    }

    async fn abort(&self, key: &str, upload_id: &str) {
        let req = Req::new(Method::DELETE, key).query("uploadId", upload_id);
        if let Err(f) = self.send(&req).await {
            tracing::debug!("S3: cannot abort an upload of {key}: {}", RemoteError::from(f));
        }
    }

    /// How far a session got. NotFound = it expired (or was aborted).
    pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        let Some(upload_id) = &s.upload_id else {
            self.uploads.lock().unwrap().remove(&s.nonce);
            return Ok(match self.made_by(&s).await? {
                Some(meta) => UploadStatus::Done(meta),
                None => UploadStatus::Next(0),
            });
        };
        let known = match self.uploads.lock().unwrap().get_mut(upload_id) {
            Some(st) => {
                st.held.clear();
                st.parts.clone()
            }
            None => BTreeMap::new(),
        };
        self.finish_multipart(&s, upload_id, total, &known).await
    }

    /// The object this session made, if it was completed (before an interruption).
    async fn made_by(&self, s: &Session) -> RemoteResult<Option<RemoteMeta>> {
        match self.head(&s.key).await {
            Ok(h) if h.meta.get(META_UPLOAD) == Some(&s.nonce) => Ok(Some(file_meta(&s.id, &h))),
            Ok(_) | Err(RemoteError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn put_chunk(&self, session: &str, data: Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus> {
        let s = Session::parse(session)?;
        let state_key = s.upload_id.clone().unwrap_or_else(|| s.nonce.clone());
        let mut st = self.uploads.lock().unwrap().remove(&state_key).unwrap_or_default();
        // Bytes from `from` on: what is held from earlier chunks, then this one.
        let (from, data) = if !st.held.is_empty() && st.held_from + st.held.len() as u64 == start {
            let mut held = std::mem::take(&mut st.held);
            held.extend_from_slice(&data);
            (st.held_from, Bytes::from(held))
        } else {
            st.held.clear();
            (start, data)
        };
        let result = if from == 0 && data.len() as u64 == total {
            self.put_whole(&s, data).await
        } else {
            match &s.upload_id {
                // A one-part file in pieces: hold them until it's all there.
                None if from == 0 => {
                    st.held = data.to_vec();
                    st.held_from = 0;
                    Ok(UploadStatus::Next(st.held.len() as u64))
                }
                None => Ok(UploadStatus::Next(0)),
                Some(upload_id) => self.put_parts(&s, upload_id, &mut st, from, data, total).await,
            }
        };
        match &result {
            Ok(UploadStatus::Done(_)) => {}
            Ok(UploadStatus::Next(_)) => {
                self.uploads.lock().unwrap().insert(state_key, st);
            }
            Err(_) => {
                st.held.clear();
                self.uploads.lock().unwrap().insert(state_key, st);
            }
        }
        result
    }

    /// The whole file in one PUT, which gives it a full SHA-256 (and an MD5 ETag).
    async fn put_whole(&self, s: &Session, data: Bytes) -> RemoteResult<UploadStatus> {
        let sha: [u8; 32] = Sha256::digest(&data).into();
        let md5 = Md5::digest(&data);
        let (sha_hex, md5_hex, md5_b64) = (hex::encode(sha), hex::encode(md5), B64.encode(md5));
        let (resp, checksum) = self
            .send_checksummed(|ck| {
                let req = Req::new(Method::PUT, &s.key)
                    .header("content-type", &s.mime)
                    .header("content-md5", &md5_b64)
                    .header(&format!("x-amz-meta-{META_SHA256}"), &sha_hex)
                    .header(&format!("x-amz-meta-{META_MD5}"), &md5_hex)
                    .header(&format!("x-amz-meta-{META_UPLOAD}"), &s.nonce)
                    .body(data.clone(), Some(sha))
                    .timeout(Duration::from_secs(600));
                if ck { req.header("x-amz-checksum-sha256", B64.encode(sha)) } else { req }
            })
            .await?;
        let headers = resp.headers();
        let etag = unquote(headers.get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).unwrap_or_default());
        let checked = checksum
            && headers.get("x-amz-checksum-sha256").and_then(|v| v.to_str().ok()) == Some(B64.encode(sha).as_str());
        let size = data.len() as u64;
        if let Some(upload_id) = &s.upload_id {
            self.abort(&s.key, upload_id).await;
        }
        self.after_upload(s).await;
        let mut meta = file_meta(&s.id, &Head { size, etag: etag.clone(), ..Default::default() });
        if checked || etag.eq_ignore_ascii_case(&md5_hex) {
            meta.hash = Some(ContentHash { kind: HashKind::Sha256, value: sha_hex });
        }
        Ok(UploadStatus::Done(meta))
    }

    /// Sends `data` (starting at offset `from`) as whole parts; holds a rest smaller than a
    /// part for the next chunk, unless it ends the file.
    async fn put_parts(
        &self,
        s: &Session,
        upload_id: &str,
        st: &mut UploadState,
        from: u64,
        mut data: Bytes,
        total: u64,
    ) -> RemoteResult<UploadStatus> {
        let p = s.part_size;
        if !from.is_multiple_of(p) {
            // Not where this process left off: continue from what S3 has.
            return self.finish_multipart(s, upload_id, total, &st.parts).await;
        }
        let mut at = from;
        loop {
            let ends = at + data.len() as u64 == total;
            if data.is_empty() || ((data.len() as u64) < p && !ends) {
                break;
            }
            let part = data.split_to((data.len() as u64).min(p) as usize);
            let number = (at / p + 1) as u32;
            at += part.len() as u64;
            let sent = self.upload_part(s, upload_id, number, part).await?;
            st.parts.insert(number, sent);
        }
        if at == total {
            return self.finish_multipart(s, upload_id, total, &st.parts).await;
        }
        st.held = data.to_vec();
        st.held_from = at;
        Ok(UploadStatus::Next(at + st.held.len() as u64))
    }

    async fn upload_part(&self, s: &Session, upload_id: &str, number: u32, data: Bytes) -> RemoteResult<Part> {
        let sha: [u8; 32] = Sha256::digest(&data).into();
        let size = data.len() as u64;
        let mut req = Req::new(Method::PUT, &s.key)
            .query("partNumber", number)
            .query("uploadId", upload_id)
            .header("content-md5", B64.encode(Md5::digest(&data)))
            .body(data, Some(sha))
            .timeout(Duration::from_secs(600));
        if s.checksum {
            req = req.header("x-amz-checksum-sha256", B64.encode(sha));
        }
        let resp = self.send(&req).await?;
        let etag = resp.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).unwrap_or_default();
        Ok(Part { number, etag: unquote(etag), size, sha256: s.checksum.then(|| B64.encode(sha)) })
    }

    /// Completes the upload when every part is there (Done), or says where to continue (Next).
    async fn finish_multipart(
        &self,
        s: &Session,
        upload_id: &str,
        total: u64,
        known: &BTreeMap<u32, Part>,
    ) -> RemoteResult<UploadStatus> {
        let mut parts = match self.list_parts(&s.key, upload_id).await {
            Ok(p) => p,
            // Completed already (before an interruption), or expired.
            Err(RemoteError::NotFound) => {
                return self.made_by(s).await?.map(UploadStatus::Done).ok_or(RemoteError::NotFound);
            }
            Err(e) => return Err(e),
        };
        for (n, p) in known {
            parts.insert(*n, p.clone());
        }
        let p = s.part_size;
        let count = total.div_ceil(p).max(1) as u32;
        let mut have = Vec::new();
        for n in 1..=count {
            let want = p.min(total - (n as u64 - 1) * p);
            match parts.remove(&n) {
                Some(part) if part.size == want => have.push(part),
                _ => return Ok(UploadStatus::Next((n as u64 - 1) * p)),
            }
        }
        match self.complete(&s.key, upload_id, &have, s.checksum).await {
            Ok(()) => {}
            Err(RemoteError::NotFound) => {
                return self.made_by(s).await?.map(UploadStatus::Done).ok_or(RemoteError::NotFound);
            }
            Err(e) => return Err(e),
        }
        self.uploads.lock().unwrap().remove(upload_id);
        self.after_upload(s).await;
        // A multipart object's checksum covers its parts, not the content: no hash (the
        // engine asks `get`, which says the same, and proves the file by downloading it).
        Ok(UploadStatus::Done(file_meta(&s.id, &Head { size: total, ..Default::default() })))
    }

    /// The parts S3 has of an upload, by number.
    async fn list_parts(&self, key: &str, upload_id: &str) -> RemoteResult<BTreeMap<u32, Part>> {
        let mut out = BTreeMap::new();
        let mut marker = 0u32;
        loop {
            let mut req = Req::new(Method::GET, key).query("uploadId", upload_id);
            if marker > 0 {
                req = req.query("part-number-marker", marker);
            }
            let xml = text(self.send(&req).await?).await?;
            for p in xml_all(&xml, "Part") {
                let Some(number) = xml_first(p, "PartNumber").and_then(|n| n.trim().parse().ok()) else {
                    continue;
                };
                out.insert(
                    number,
                    Part {
                        number,
                        etag: unquote(&xml_text(xml_first(p, "ETag").unwrap_or_default())),
                        size: xml_first(p, "Size").and_then(|n| n.trim().parse().ok()).unwrap_or(0),
                        sha256: xml_first(p, "ChecksumSHA256").map(xml_text).filter(|v| !v.is_empty()),
                    },
                );
            }
            let next = xml_first(&xml, "NextPartNumberMarker").and_then(|n| n.trim().parse::<u32>().ok());
            match next {
                Some(n) if xml_first(&xml, "IsTruncated").is_some_and(|t| t.trim() == "true") && n > marker => marker = n,
                _ => return Ok(out),
            }
        }
    }

    async fn complete(&self, key: &str, upload_id: &str, parts: &[Part], checksum: bool) -> RemoteResult<()> {
        let mut xml = String::from("<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
        for p in parts {
            xml.push_str(&format!("<Part><PartNumber>{}</PartNumber><ETag>\"{}\"</ETag>", p.number, xml_escape(&p.etag)));
            if let Some(sha) = p.sha256.as_ref().filter(|_| checksum) {
                xml.push_str(&format!("<ChecksumSHA256>{}</ChecksumSHA256>", xml_escape(sha)));
            }
            xml.push_str("</Part>");
        }
        xml.push_str("</CompleteMultipartUpload>");
        let req = Req::new(Method::POST, key)
            .query("uploadId", upload_id)
            .header("content-type", "application/xml")
            .body(xml, None)
            .timeout(Duration::from_secs(3600));
        // S3 can answer 200 and report the failure in the body.
        xml_ok(&text(self.send(&req).await?).await?)
    }

    /// After an upload that also renamed or moved the file, its old key goes the way a
    /// rename's would. The upload stands either way.
    async fn after_upload(&self, s: &Session) {
        if let Some(old) = &s.replaces
            && let Err(e) = self.retire(&[self.key(old)], old, &s.id).await
        {
            tracing::warn!("S3: kept {old} after uploading {}: {e}", s.id);
        }
    }

    /// Bytes `start..end` of a file's content (for downloading placeholders on demand).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<Bytes> {
        if end <= start {
            return Ok(Bytes::new());
        }
        let req = Req::new(Method::GET, &self.key(id))
            .header("range", format!("bytes={start}-{}", end - 1))
            .timeout(Duration::from_secs(600));
        let resp = self.send(&req).await?;
        let whole = resp.status() == StatusCode::OK;
        let body = resp
            .bytes()
            .await
            .map_err(|e| RemoteError::Transient(format!("download interrupted: {e}")))?;
        if whole {
            // The service ignored the range and sent everything.
            let s = (start as usize).min(body.len());
            let e = (end as usize).min(body.len());
            return Ok(body.slice(s..e));
        }
        Ok(body)
    }

    /// Streams a file's content into `out`; returns (its SHA-256, bytes written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        let req = Req::new(Method::GET, &self.key(id)).timeout(Duration::from_secs(24 * 3600));
        let resp = self.send(&req).await?;
        let mut stream = resp.bytes_stream();
        let mut hasher = Hasher::new(HashKind::Sha256);
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

/// A folder id, ending in "/" ("" for the top).
fn dir_id(id: &str) -> String {
    if id.is_empty() || id.ends_with('/') { id.to_string() } else { format!("{id}/") }
}

/// The id of `name` in folder `parent`; folders end in "/".
fn child_id(parent: &str, name: &str, folder: bool) -> String {
    format!("{}{name}{}", dir_id(parent), if folder { "/" } else { "" })
}

/// (parent id, name) of an id or key.
fn split_id(id: &str) -> (&str, &str) {
    let trimmed = id.strip_suffix('/').unwrap_or(id);
    match trimmed.rfind('/') {
        Some(i) => (&id[..=i], &trimmed[i + 1..]),
        None => ("", trimmed),
    }
}

fn folder_meta(id: &str) -> RemoteMeta {
    let (parent, name) = split_id(id);
    RemoteMeta {
        id: id.to_string(),
        name: name.to_string(),
        parents: if id.is_empty() { vec![] } else { vec![parent.to_string()] },
        folder: true,
        ..Default::default()
    }
}

fn file_meta(id: &str, h: &Head) -> RemoteMeta {
    let (parent, name) = split_id(id);
    RemoteMeta {
        id: id.to_string(),
        name: name.to_string(),
        parents: vec![parent.to_string()],
        folder: false,
        hash: h.hash(),
        size: Some(h.size),
        trashed: false,
    }
}

/// Part size for a file of `total` bytes: `unit`, or the smallest multiple of it that needs
/// at most 10,000 parts.
fn part_size(total: u64, unit: u64) -> u64 {
    if total.div_ceil(unit) <= MAX_PARTS {
        unit
    } else {
        total.div_ceil(MAX_PARTS).div_ceil(unit) * unit
    }
}

/// "photo.jpg" → "photo (replaced 2026-10-21).jpg"; the `n`th copy of the day gets " n" after
/// the date.
fn replaced_name(name: &str, date: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let count = if n > 1 { format!(" {n}") } else { String::new() };
    format!("{stem} (replaced {date}{count}){ext}")
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// An ETag that is an MD5 of the content (single-part uploads, without SSE-KMS or SSE-C).
fn plain_md5(etag: &str) -> Option<&str> {
    is_hex(etag, 32).then_some(etag)
}

fn unquote(etag: &str) -> String {
    etag.trim().trim_matches('"').to_ascii_lowercase()
}

fn parse_head(h: &reqwest::header::HeaderMap) -> Head {
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
    let composite = get("x-amz-checksum-type").is_some_and(|t| t.eq_ignore_ascii_case("COMPOSITE"));
    let sha256 = get("x-amz-checksum-sha256")
        .filter(|v| !composite && !v.contains('-'))
        .and_then(|v| B64.decode(v.trim()).ok())
        .filter(|d| d.len() == 32)
        .map(hex::encode);
    Head {
        size: get("content-length").and_then(|v| v.trim().parse().ok()).unwrap_or(0),
        etag: unquote(get("etag").unwrap_or_default()),
        sha256,
        content_type: get("content-type").map(str::to_string),
        meta: h
            .iter()
            .filter_map(|(k, v)| {
                let name = k.as_str().strip_prefix("x-amz-meta-")?;
                Some((name.to_string(), v.to_str().ok()?.to_string()))
            })
            .collect(),
    }
}

async fn text(resp: Response) -> RemoteResult<String> {
    resp.text()
        .await
        .map_err(|e| RemoteError::Transient(format!("bad response from S3: {e}")))
}

/// An `<Error>` in a 200 answer (CopyObject, UploadPartCopy, CompleteMultipartUpload).
fn xml_ok(xml: &str) -> RemoteResult<()> {
    if xml_first(xml, "Error").is_none() {
        return Ok(());
    }
    let (code, message) = error_body(xml);
    Err(s3_error(500, &code, &message, None))
}

/// (Code, Message) of an S3 error body.
fn error_body(body: &str) -> (String, String) {
    (
        xml_first(body, "Code").map(xml_text).unwrap_or_default(),
        xml_first(body, "Message").map(xml_text).unwrap_or_default(),
    )
}

/// Maps an S3 error to the neutral kinds.
fn s3_error(status: u16, code: &str, message: &str, retry_after: Option<u64>) -> RemoteError {
    let reason = StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
    let detail = [message, code, reason].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
    let text = format!("{status} {detail}");
    let lower = message.to_ascii_lowercase();
    match code {
        "NoSecret" => return RemoteError::Auth(message.to_string()),
        "NoBucket" => return RemoteError::Permanent(message.to_string()),
        "SignatureDoesNotMatch" | "InvalidAccessKeyId" | "ExpiredToken" | "InvalidToken" | "TokenRefreshRequired" => {
            return RemoteError::Auth(format!("S3 refused the access key or secret key ({code})"));
        }
        "SlowDown" | "TooManyRequests" | "RequestLimitExceeded" | "Throttling" | "ServiceUnavailable" => {
            return RemoteError::RateLimited { message: text, retry_after };
        }
        "NoSuchKey" | "NoSuchUpload" | "NoSuchVersion" => return RemoteError::NotFound,
        "NoSuchBucket" => return RemoteError::Permanent("the S3 bucket does not exist".into()),
        "RequestTimeTooSkewed" => {
            return RemoteError::Permanent("this computer's clock is too far off for S3; correct it and try again".into());
        }
        "AuthorizationHeaderMalformed" | "PermanentRedirect" | "IllegalLocationConstraintException" => {
            return RemoteError::Permanent(format!("the bucket is not in the configured region ({detail})"));
        }
        "InternalError" | "RequestTimeout" | "OperationAborted" => return RemoteError::Transient(text),
        "NotImplemented" => return RemoteError::Permanent(format!("the S3 service doesn't support this ({detail})")),
        _ => {}
    }
    let full = status == 507
        || matches!(
            code,
            "QuotaExceeded" | "StorageQuotaExceeded" | "XMinioStorageFull" | "XMinioAdminBucketQuotaExceeded" | "StorageFull"
                | "InsufficientStorage"
        )
        || ["cap exceeded", "quota", "storage full", "insufficient storage", "not enough space"]
            .iter()
            .any(|m| lower.contains(m));
    if full {
        return RemoteError::Quota(format!("the S3 storage is full ({detail})"));
    }
    match status {
        401 => RemoteError::Auth(text),
        403 if code == "AccessDenied" || code.is_empty() => {
            RemoteError::Permanent(format!("{status} access denied — check the key's permissions on the bucket ({detail})"))
        }
        404 => RemoteError::NotFound,
        301 => RemoteError::Permanent(format!("the bucket is not in the configured region ({detail})")),
        429 | 503 => RemoteError::RateLimited { message: text, retry_after },
        307 | 408 | 500..=599 => RemoteError::Transient(text),
        _ => RemoteError::Permanent(text),
    }
}

// ---------- Signature Version 4 ----------

/// What signs requests: the key pair and the scope it is used in.
struct SigningKey<'a> {
    access_key: &'a str,
    secret: &'a str,
    region: &'a str,
    service: &'a str,
}

impl SigningKey<'_> {
    /// The `Authorization` header of a request. `uri` and `query` are canonical (already
    /// encoded), `headers` are (name, value) and include `host` and `x-amz-date`; all of them
    /// are signed. `date` is `x-amz-date` (`20130524T000000Z`).
    fn authorization(&self, method: &str, uri: &str, query: &str, headers: &[(String, String)], payload: &str, date: &str) -> String {
        let mut hs: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" ")))
            .collect();
        hs.sort();
        let signed = hs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let canonical_headers: String = hs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let canonical = format!("{method}\n{uri}\n{query}\n{canonical_headers}\n{signed}\n{payload}");
        let day = &date[..8.min(date.len())];
        let scope = format!("{day}/{}/{}/aws4_request", self.region, self.service);
        let to_sign = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{}", hex::encode(Sha256::digest(canonical.as_bytes())));
        let mut k = hmac_sha256(format!("AWS4{}", self.secret).as_bytes(), day.as_bytes());
        for part in [self.region, self.service, "aws4_request"] {
            k = hmac_sha256(&k, part.as_bytes());
        }
        let signature = hex::encode(hmac_sha256(&k, to_sign.as_bytes()));
        format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}", self.access_key)
    }
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// SigV4's URI encoding: everything but A–Z a–z 0–9 - _ . ~ as %XX (and "/" too unless kept).
fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Query parameters encoded and sorted the way SigV4 signs them (and sent that way).
fn canonical_query(query: &[(String, String)]) -> String {
    let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    q.sort();
    q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// (year, month, day, hour, minute, second) in UTC.
fn utc(t: SystemTime) -> (i64, u32, u32, u32, u32, u32) {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (rem % 60) as u32)
}

/// `x-amz-date`: 20130524T000000Z.
fn amz_date(t: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = utc(t);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// 2026-10-21 (UTC).
fn utc_date(t: SystemTime) -> String {
    let (y, mo, d, ..) = utc(t);
    format!("{y:04}-{mo:02}-{d:02}")
}

// ---------- XML ----------

/// The contents of each `<name>…</name>` element in `xml`, in order. S3's answers never nest
/// an element inside one of the same name, which is all this needs.
fn xml_all<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{name}"), format!("</{name}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        if !after.starts_with(['>', ' ', '/', '\t', '\n', '\r']) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        if after[..gt].ends_with('/') {
            out.push("");
            rest = &after[gt + 1..];
            continue;
        }
        let body = &after[gt + 1..];
        let Some(end) = body.find(&close) else { break };
        out.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    out
}

fn xml_first<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    xml_all(xml, name).into_iter().next()
}

/// Element text with entities resolved.
fn xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e => e
                .strip_prefix("#x")
                .or_else(|| e.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| e.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A key from a listing: XML text, and URL-encoded as well when the service honoured
/// `encoding-type=url` (with "+" for spaces, as Amazon S3 does).
fn listed_key(raw: &str, url_encoded: bool) -> String {
    let s = xml_text(raw);
    if !url_encoded {
        return s;
    }
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() && hex(bytes[i + 1]).is_some() && hex(bytes[i + 2]).is_some() => {
                out.push((hex(bytes[i + 1]).unwrap_or(0) << 4) | hex(bytes[i + 2]).unwrap_or(0));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "../tests/common/s3_mock.rs"]
mod s3_mock;

#[cfg(test)]
mod tests {
    use super::*;
    use s3_mock::Fail;

    /// Part size in the tests (the mock takes parts this small).
    const UNIT: u64 = 64 * 1024;

    fn sign(k: &SigningKey, method: &str, uri: &str, query: &str, headers: &[(&str, &str)], date: &str) -> String {
        let hs: Vec<(String, String)> = headers.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let payload = hs.iter().find(|(k, _)| k == "x-amz-content-sha256").map_or(EMPTY_SHA256, |(_, v)| v.as_str());
        k.authorization(method, uri, query, &hs, payload, date)
    }

    fn signature(auth: &str) -> &str {
        auth.rsplit("Signature=").next().unwrap()
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than the block is hashed first (test case 6).
        assert_eq!(
            hex::encode(hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn signs_like_the_aws_test_suite() {
        // AWS's aws-sig-v4-test-suite: get-vanilla, get-vanilla-query-order-key-case, post-vanilla.
        let k = SigningKey {
            access_key: "AKIDEXAMPLE",
            secret: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "service",
        };
        let date = "20150830T123600Z";
        let hs = [("host", "example.amazonaws.com"), ("x-amz-date", date)];
        assert_eq!(
            sign(&k, "GET", "/", "", &hs, date),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        let q = canonical_query(&[("Param2".into(), "value2".into()), ("Param1".into(), "value1".into())]);
        assert_eq!(q, "Param1=value1&Param2=value2");
        assert_eq!(
            signature(&sign(&k, "GET", "/", &q, &hs, date)),
            "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
        );
        assert_eq!(
            signature(&sign(&k, "POST", "/", "", &hs, date)),
            "5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b"
        );
    }

    #[test]
    fn signs_like_the_s3_documentation() {
        // The examples in Amazon S3's "Signature Calculations for the Authorization Header".
        let k = SigningKey {
            access_key: "AKIAIOSFODNN7EXAMPLE",
            secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "s3",
        };
        let date = "20130524T000000Z";
        let host = "examplebucket.s3.amazonaws.com";
        let get = sign(
            &k,
            "GET",
            "/test.txt",
            "",
            &[("host", host), ("range", "bytes=0-9"), ("x-amz-content-sha256", EMPTY_SHA256), ("x-amz-date", date)],
            date,
        );
        assert_eq!(
            get,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        let payload = hex::encode(Sha256::digest(b"Welcome to Amazon S3."));
        assert_eq!(payload, "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072");
        let uri = format!("/{}", uri_encode("test$file.text", true));
        assert_eq!(uri, "/test%24file.text");
        let put = sign(
            &k,
            "PUT",
            &uri,
            "",
            &[
                ("date", "Fri, 24 May 2013 00:00:00 GMT"),
                ("host", host),
                ("x-amz-content-sha256", &payload),
                ("x-amz-date", date),
                ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ],
            date,
        );
        assert_eq!(signature(&put), "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd");
        let hs = [("host", host), ("x-amz-content-sha256", EMPTY_SHA256), ("x-amz-date", date)];
        let lifecycle = canonical_query(&[("lifecycle".into(), String::new())]);
        assert_eq!(
            signature(&sign(&k, "GET", "/", &lifecycle, &hs, date)),
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
        let list = canonical_query(&[("max-keys".into(), "2".into()), ("prefix".into(), "J".into())]);
        assert_eq!(
            signature(&sign(&k, "GET", "/", &list, &hs, date)),
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn encodes_and_dates() {
        assert_eq!(uri_encode("a b+c/d~é", true), "a%20b%2Bc/d~%C3%A9");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(
            canonical_query(&[("uploads".into(), String::new()), ("prefix".into(), "a b".into())]),
            "prefix=a%20b&uploads="
        );
        let t = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(amz_date(t(1_369_353_600)), "20130524T000000Z");
        assert_eq!(amz_date(t(951_782_400 + 3723)), "20000229T010203Z");
        assert_eq!(utc_date(t(1_735_689_599)), "2024-12-31");
        assert_eq!(utc_date(t(0)), "1970-01-01");
    }

    #[test]
    fn plans_parts_within_10000() {
        let unit = PART_UNIT as u64;
        assert_eq!(part_size(0, unit), unit);
        assert_eq!(part_size(10_000 * unit, unit), unit);
        assert_eq!(part_size(10_000 * unit + 1, unit), 2 * unit);
        for total in [100_000_000_000u64, 1 << 40, 5 << 40] {
            let p = part_size(total, unit);
            assert!(p.is_multiple_of(unit) && total.div_ceil(p) <= 10_000 && p <= 5 << 30, "{total}: {p}");
        }
        assert_eq!(part_size(5 << 40, unit), 66 * unit);
    }

    #[test]
    fn names_kept_copies() {
        assert_eq!(replaced_name("photo.jpg", "2026-10-21", 1), "photo (replaced 2026-10-21).jpg");
        assert_eq!(replaced_name("photo.jpg", "2026-10-21", 2), "photo (replaced 2026-10-21 2).jpg");
        assert_eq!(replaced_name("notes", "2026-10-21", 1), "notes (replaced 2026-10-21)");
        assert_eq!(replaced_name(".bashrc", "2026-10-21", 1), ".bashrc (replaced 2026-10-21)");
        assert_eq!(replaced_name("a.tar.gz", "2026-10-21", 1), "a.tar (replaced 2026-10-21).gz");
    }

    #[test]
    fn ids_are_relative_keys() {
        assert_eq!(child_id("", "CloudDrop", true), "CloudDrop/");
        assert_eq!(child_id("CloudDrop/", "a.txt", false), "CloudDrop/a.txt");
        assert_eq!(child_id("CloudDrop", "sub", true), "CloudDrop/sub/");
        assert_eq!(split_id("CloudDrop/sub/"), ("CloudDrop/", "sub"));
        assert_eq!(split_id("CloudDrop/sub/a.txt"), ("CloudDrop/sub/", "a.txt"));
        assert_eq!(split_id("CloudDrop/"), ("", "CloudDrop"));
        let m = folder_meta("CloudDrop/sub/");
        assert!(m.folder && m.name == "sub" && m.parents == ["CloudDrop/"]);
        assert!(folder_meta("").parents.is_empty());
    }

    #[test]
    fn reads_xml() {
        let x = "<R><KeyCount>2</KeyCount><Contents><Key>a&amp;b</Key></Contents>\
                 <Contents><Key>c%2Bd+e</Key></Contents><Prefix/><Part><PartNumber>3</PartNumber></Part></R>";
        assert_eq!(xml_all(x, "Contents").len(), 2);
        assert_eq!(xml_first(x, "Key"), Some("a&amp;b"));
        assert_eq!(xml_first(x, "KeyCount"), Some("2"));
        assert_eq!(xml_first(x, "Prefix"), Some(""));
        assert_eq!(xml_first(x, "PartNumber"), Some("3"));
        assert_eq!(xml_all(x, "Part").len(), 1);
        assert_eq!(xml_text("&quot;ab&quot; &lt;&#65;&#x42;&gt; &bogus; &"), "\"ab\" <AB> &bogus; &");
        assert_eq!(listed_key("c%2Bd+e%2", true), "c+d e%2");
        assert_eq!(listed_key("a&amp;b+c", false), "a&b+c");
        assert_eq!(xml_escape("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }

    #[test]
    fn maps_errors() {
        let e = |status, code, msg| s3_error(status, code, msg, Some(7));
        assert!(matches!(e(403, "SignatureDoesNotMatch", ""), RemoteError::Auth(_)));
        assert!(matches!(e(403, "InvalidAccessKeyId", ""), RemoteError::Auth(_)));
        assert_eq!(
            e(503, "SlowDown", "Please reduce your request rate."),
            RemoteError::RateLimited { message: "503 Please reduce your request rate.".into(), retry_after: Some(7) }
        );
        assert!(matches!(e(503, "", ""), RemoteError::RateLimited { .. }));
        assert!(matches!(e(429, "", ""), RemoteError::RateLimited { .. }));
        assert_eq!(e(404, "NoSuchKey", ""), RemoteError::NotFound);
        assert_eq!(e(404, "NoSuchUpload", ""), RemoteError::NotFound);
        assert_eq!(e(404, "", ""), RemoteError::NotFound);
        assert!(matches!(e(404, "NoSuchBucket", ""), RemoteError::Permanent(_)));
        assert!(matches!(e(403, "AccessDenied", "Cannot upload files, storage cap exceeded."), RemoteError::Quota(_)));
        assert!(matches!(e(507, "XMinioStorageFull", ""), RemoteError::Quota(_)));
        assert!(matches!(e(403, "QuotaExceeded", ""), RemoteError::Quota(_)));
        assert!(matches!(e(403, "AccessDenied", "Access Denied"), RemoteError::Permanent(m) if m.contains("permissions")));
        assert!(matches!(e(500, "InternalError", ""), RemoteError::Transient(_)));
        assert!(matches!(e(502, "", ""), RemoteError::Transient(_)));
        assert!(matches!(e(400, "InvalidArgument", "x"), RemoteError::Permanent(_)));
        assert!(matches!(e(403, "RequestTimeTooSkewed", ""), RemoteError::Permanent(m) if m.contains("clock")));
        assert!(matches!(e(400, "AuthorizationHeaderMalformed", ""), RemoteError::Permanent(m) if m.contains("region")));
        assert!(matches!(e(501, "NotImplemented", ""), RemoteError::Permanent(_)));
        assert!(xml_ok("<CopyObjectResult><ETag>x</ETag></CopyObjectResult>").is_ok());
        assert!(matches!(xml_ok("<Error><Code>InternalError</Code></Error>"), Err(RemoteError::Transient(_))));
    }

    #[test]
    fn addresses_the_bucket() {
        let cfg = |endpoint: &str, addressing: &str| S3Config {
            endpoint: endpoint.into(),
            bucket: "b".into(),
            prefix: "/a//b/../".into(),
            addressing: addressing.into(),
            ..Default::default()
        };
        let http = reqwest::Client::new();
        let aws = S3::new(http.clone(), &cfg("", ""), None);
        assert_eq!(aws.target("k 1", &[]).0, "https://b.s3.us-east-1.amazonaws.com/k%201");
        assert_eq!((aws.prefix.as_str(), aws.region.as_str()), ("a/b/", "us-east-1"));
        let minio = S3::new(http.clone(), &cfg("http://127.0.0.1:9000/", ""), None);
        assert_eq!(minio.host, "127.0.0.1:9000");
        let (url, uri, query) = minio.target("x/y.txt", &[("uploadId".into(), "a+b".into())]);
        assert_eq!(url, "http://127.0.0.1:9000/b/x/y.txt?uploadId=a%2Bb");
        assert_eq!((uri.as_str(), query.as_str()), ("/b/x/y.txt", "uploadId=a%2Bb"));
        assert_eq!(minio.target("", &[]).1, "/b");
        let virt = S3::new(http.clone(), &cfg("s3.wasabisys.com", "virtual"), None);
        assert_eq!(virt.target("k", &[]).0, "https://b.s3.wasabisys.com/k");
        let path = S3::new(http.clone(), &cfg("https://s3.eu-west-1.amazonaws.com", "path"), None);
        assert_eq!(path.target("k", &[]).0, "https://s3.eu-west-1.amazonaws.com/b/k");
        let proxied = S3::new(http, &cfg("https://example.com/s3/", ""), None);
        assert_eq!(proxied.target("k", &[]).0, "https://example.com/s3/b/k");
    }

    #[test]
    fn config_section_stays_out_of_existing_files() {
        let cfg = crate::config::Config::default();
        assert!(!toml::to_string(&cfg).unwrap().contains("[s3]"));
        let text = "provider = \"s3\"\n[s3]\nbucket = \"b\"\nprefix = \"laptop\"\n";
        let cfg: crate::config::Config = toml::from_str(text).unwrap();
        assert_eq!((cfg.provider, cfg.s3.bucket.as_str(), cfg.s3.prefix.as_str()), (crate::remote::Provider::S3, "b", "laptop"));
        assert!(toml::to_string(&cfg).unwrap().contains("[s3]"));
    }

    // ---------- against the mock ----------

    fn client(mock: &s3_mock::Shared, prefix: &str) -> S3 {
        let cfg = S3Config {
            endpoint: mock.lock().unwrap().base.clone(),
            region: s3_mock::REGION.into(),
            bucket: s3_mock::BUCKET.into(),
            prefix: prefix.into(),
            access_key_id: s3_mock::ACCESS_KEY.into(),
            addressing: String::new(),
        };
        let mut s3 = S3::new(crate::drive::http_client(), &cfg, Some(s3_mock::SECRET.into()));
        s3.part_unit = UNIT;
        s3
    }

    async fn setup() -> (s3_mock::Shared, S3) {
        let mock = s3_mock::start().await;
        mock.lock().unwrap().min_part = UNIT as usize;
        let s3 = client(&mock, "backup");
        (mock, s3)
    }

    fn content(n: usize, seed: u32) -> Vec<u8> {
        (0..n as u32).map(|i| (i.wrapping_mul(31).wrapping_add(seed) % 251) as u8).collect()
    }

    fn sha256(data: &[u8]) -> ContentHash {
        let mut h = Hasher::new(HashKind::Sha256);
        h.update(data);
        h.finish()
    }

    /// Sends `data` from `at` the way the engine does: `chunk` bytes from wherever the
    /// client says to continue.
    async fn send(s3: &S3, session: &str, data: &[u8], mut at: u64, chunk: usize) -> RemoteResult<RemoteMeta> {
        loop {
            let end = (at as usize + chunk).min(data.len());
            let piece = Bytes::copy_from_slice(&data[at as usize..end]);
            match s3.put_chunk(session, piece, at, data.len() as u64).await? {
                UploadStatus::Done(m) => return Ok(m),
                UploadStatus::Next(n) => at = n,
            }
        }
    }

    async fn upload(s3: &S3, parent: &str, name: &str, data: &[u8], chunk: usize) -> RemoteMeta {
        let session = s3.start_upload(None, name, (parent, None), data.len() as u64, "text/plain").await.unwrap();
        send(s3, &session, data, 0, chunk).await.unwrap()
    }

    async fn root(s3: &S3) -> String {
        let id = s3.create_folder("CloudDrop", &s3.top_parent().await.unwrap()).await.unwrap().id;
        assert_eq!(id, "CloudDrop/");
        id
    }

    fn today() -> String {
        utc_date(SystemTime::now())
    }

    #[tokio::test]
    async fn one_part_files_go_up_in_one_put_with_their_sha256() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let name = "a b+c&d=é(1)!.txt";
        let data = b"hello s3".to_vec();
        let m = upload(&s3, &root, name, &data, PART_UNIT).await;
        assert_eq!((m.id.as_str(), m.name.as_str(), m.parents.as_slice()), ("CloudDrop/a b+c&d=é(1)!.txt", name, &[root.clone()][..]));
        assert_eq!((m.hash.clone(), m.size), (Some(sha256(&data)), Some(8)));
        assert_eq!(s3.get(&m.id).await.unwrap(), m);
        {
            let st = mock.lock().unwrap();
            let o = &st.objects[&format!("backup/{}", m.id)];
            assert!(o.checksum.is_some() && !o.composite());
            assert_eq!(o.content_type, "text/plain");
            assert!(st.objects.contains_key("backup/CloudDrop/"), "the folder has its marker");
            assert_eq!(st.count("POST"), 0, "no multipart upload for one part");
            assert_eq!(st.bad_signatures, 0);
        }
        assert_eq!(s3.find_child(name, &root, false).await.unwrap(), vec![m.clone()]);
        assert!(s3.find_child(name, &root, true).await.unwrap().is_empty());
        assert!(s3.find_child("nope", &root, false).await.unwrap().is_empty());
        assert_eq!(s3.find_child("CloudDrop", "", true).await.unwrap(), vec![folder_meta("CloudDrop/")]);
        assert_eq!(s3.get(&root).await.unwrap(), folder_meta("CloudDrop/"));
        assert_eq!(s3.get("CloudDrop/missing").await, Err(RemoteError::NotFound));
        assert_eq!(s3.get("Elsewhere/").await, Err(RemoteError::NotFound));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("down");
        let mut f = tokio::fs::File::create(&path).await.unwrap();
        assert_eq!(s3.download(&m.id, &mut f).await.unwrap(), (sha256(&data), 8));
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(&s3.download_range(&m.id, 2, 5).await.unwrap()[..], b"llo");
        assert!(s3.download_range(&m.id, 5, 5).await.unwrap().is_empty());

        // An empty file is one (empty) PUT too.
        let empty = upload(&s3, &root, "empty", b"", PART_UNIT).await;
        assert_eq!((empty.hash, empty.size), (Some(sha256(b"")), Some(0)));
    }

    #[tokio::test]
    async fn without_service_checksums_the_md5_etag_proves_the_sha256() {
        let (mock, s3) = setup().await;
        mock.lock().unwrap().checksums = false;
        let root = root(&s3).await;
        let data = content(1000, 1);
        let m = upload(&s3, &root, "a.bin", &data, PART_UNIT).await;
        assert_eq!(m.hash, Some(sha256(&data)));
        assert_eq!(s3.get(&m.id).await.unwrap().hash, Some(sha256(&data)));
        assert!(mock.lock().unwrap().objects["backup/CloudDrop/a.bin"].checksum.is_none());

        // An ETag that isn't the content's MD5 (SSE-KMS) proves nothing: verify by download.
        mock.lock().unwrap().opaque_etags = true;
        let m = upload(&s3, &root, "b.bin", &data, PART_UNIT).await;
        assert_eq!(m.hash, None);
        assert_eq!(s3.get(&m.id).await.unwrap().hash, None);

        // Nor does an object another program put there.
        mock.lock().unwrap().put("backup/CloudDrop/c.bin", &data);
        assert_eq!(s3.get("CloudDrop/c.bin").await.unwrap().hash, None);
    }

    #[tokio::test]
    async fn a_service_refusing_checksums_is_asked_once() {
        let (mock, s3) = setup().await;
        mock.lock().unwrap().refuse_checksums = true;
        let root = root(&s3).await;
        let data = content(500, 2);
        let m = upload(&s3, &root, "a.bin", &data, PART_UNIT).await;
        assert_eq!(m.hash, Some(sha256(&data)), "proven by the MD5 ETag instead");
        assert!(!s3.checksums.load(Ordering::Relaxed));
        let big = content(3 * UNIT as usize, 3);
        let m = upload(&s3, &root, "big.bin", &big, UNIT as usize).await;
        assert_eq!(m.size, Some(big.len() as u64));
        let st = mock.lock().unwrap();
        assert_eq!(st.count("PUT backup/CloudDrop/a.bin"), 2, "refused once, then sent without");
        assert_eq!(st.count("POST backup/CloudDrop/big.bin?uploads"), 1, "not asked again");
        assert_eq!(st.content("backup/CloudDrop/big.bin"), Some(big));
    }

    #[tokio::test]
    async fn multipart_uploads_resume_after_a_crash() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let data = content(5 * UNIT as usize + 1234, 7);
        let total = data.len() as u64;
        let session = s3.start_upload(None, "big.bin", (&root, None), total, "application/octet-stream").await.unwrap();
        for i in 0..2u64 {
            let piece = Bytes::copy_from_slice(&data[(i * UNIT) as usize..((i + 1) * UNIT) as usize]);
            assert_eq!(s3.put_chunk(&session, piece, i * UNIT, total).await.unwrap(), UploadStatus::Next((i + 1) * UNIT));
        }
        // The process dies; a new one asks the session how far it got.
        drop(s3);
        let s3 = client(&mock, "backup");
        assert_eq!(s3.query_upload(&session, total).await.unwrap(), UploadStatus::Next(2 * UNIT));
        let m = send(&s3, &session, &data, 2 * UNIT, UNIT as usize).await.unwrap();
        assert_eq!((m.id.as_str(), m.size), ("CloudDrop/big.bin", Some(total)));
        assert_eq!(m.hash, None, "a multipart object's checksum is of its parts");
        assert_eq!(s3.get(&m.id).await.unwrap().hash, None);
        {
            let st = mock.lock().unwrap();
            let o = &st.objects["backup/CloudDrop/big.bin"];
            assert_eq!(o.content, data);
            assert!(o.composite() && o.etag.ends_with("-6"));
            assert_eq!(st.uploads_open(), 0);
            assert_eq!(st.count("PUT backup/CloudDrop/big.bin?partNumber"), 6, "no part sent twice");
            assert_eq!(st.bad_signatures, 0);
        }
        // Another crash before the engine recorded it: the session still answers Done.
        let s3 = client(&mock, "backup");
        match s3.query_upload(&session, total).await.unwrap() {
            UploadStatus::Done(meta) => assert_eq!(meta.id, m.id),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn chunks_smaller_than_a_part_are_held_until_a_part_is_full() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let data = content(3 * UNIT as usize + 100, 9);
        let total = data.len() as u64;
        let quarter = (UNIT / 4) as usize;
        let session = s3.start_upload(None, "q.bin", (&root, None), total, "x/y").await.unwrap();
        // Six quarters: one part sent, half a part held in memory.
        for i in 0..6 {
            let at = i * quarter;
            let piece = Bytes::copy_from_slice(&data[at..at + quarter]);
            let next = s3.put_chunk(&session, piece, at as u64, total).await.unwrap();
            assert_eq!(next, UploadStatus::Next((at + quarter) as u64));
        }
        assert_eq!(mock.lock().unwrap().count("PUT backup/CloudDrop/q.bin?partNumber"), 1);
        // A restart loses what was held: continue from the first part.
        let s3 = client(&mock, "backup");
        assert_eq!(s3.query_upload(&session, total).await.unwrap(), UploadStatus::Next(UNIT));
        // A chunk that doesn't start on a part boundary is answered with where to go on.
        let odd = Bytes::copy_from_slice(&data[quarter..2 * quarter]);
        assert_eq!(s3.put_chunk(&session, odd, quarter as u64, total).await.unwrap(), UploadStatus::Next(UNIT));
        send(&s3, &session, &data, UNIT, quarter).await.unwrap();
        let st = mock.lock().unwrap();
        assert_eq!(st.content("backup/CloudDrop/q.bin"), Some(data));
        assert_eq!(st.count("PUT backup/CloudDrop/q.bin?partNumber"), 4);
    }

    #[tokio::test]
    async fn expired_sessions_are_not_found() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let total = 2 * UNIT + 1;
        let session = s3.start_upload(None, "x.bin", (&root, None), total, "x/y").await.unwrap();
        let s: Session = serde_json::from_str(&session).unwrap();
        assert_eq!((s.key.as_str(), s.part_size, s.checksum), ("backup/CloudDrop/x.bin", UNIT, true));
        s3.abort(&s.key, s.upload_id.as_deref().unwrap()).await;
        assert_eq!(mock.lock().unwrap().uploads_open(), 0);
        assert_eq!(s3.query_upload(&session, total).await, Err(RemoteError::NotFound));
        let piece = Bytes::from(vec![0u8; UNIT as usize]);
        assert_eq!(s3.put_chunk(&session, piece, 0, total).await, Err(RemoteError::NotFound));
        assert_eq!(s3.query_upload("not json", total).await, Err(RemoteError::NotFound));
        // A one-part session that never finished starts over.
        let small = s3.start_upload(None, "s.bin", (&root, None), 10, "x/y").await.unwrap();
        assert_eq!(s3.query_upload(&small, 10).await.unwrap(), UploadStatus::Next(0));
    }

    #[tokio::test]
    async fn starting_again_aborts_the_unfinished_upload_of_the_same_file() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let total = 2 * UNIT + 1;
        s3.start_upload(None, "x.bin", (&root, None), total, "x/y").await.unwrap();
        s3.start_upload(None, "x.bin.bak", (&root, None), total, "x/y").await.unwrap();
        s3.start_upload(None, "x.bin", (&root, None), total, "x/y").await.unwrap();
        assert_eq!(mock.lock().unwrap().uploads_open(), 2, "x.bin's first upload is gone, x.bin.bak's is not");
        // A file that fits in one chunk after all is one PUT; its multipart upload is aborted.
        let data = content(total as usize, 4);
        let session = s3.start_upload(None, "y.bin", (&root, None), total, "x/y").await.unwrap();
        let m = send(&s3, &session, &data, 0, data.len()).await.unwrap();
        assert_eq!(m.hash, Some(sha256(&data)));
        assert_eq!(mock.lock().unwrap().uploads_open(), 2);
    }

    #[tokio::test]
    async fn missing_parents_are_not_found() {
        let (_mock, s3) = setup().await;
        root(&s3).await;
        assert_eq!(s3.create_folder("x", "CloudDrop/gone/").await, Err(RemoteError::NotFound));
        assert_eq!(s3.start_upload(None, "x", ("CloudDrop/gone/", None), 3, "x/y").await, Err(RemoteError::NotFound));
    }

    #[tokio::test]
    async fn folders_without_a_marker_count() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        mock.lock().unwrap().put("backup/CloudDrop/made/elsewhere.txt", b"x");
        assert_eq!(s3.get("CloudDrop/made/").await.unwrap(), folder_meta("CloudDrop/made/"));
        assert_eq!(s3.find_child("made", &root, true).await.unwrap(), vec![folder_meta("CloudDrop/made/")]);
        let sub = s3.create_folder("sub", "CloudDrop/made/").await.unwrap();
        assert_eq!(sub.id, "CloudDrop/made/sub/");
    }

    /// CloudDrop/A/ with x.txt, B/ and B/y.bin (multipart); returns (x, y).
    async fn tree(s3: &S3) -> (Vec<u8>, Vec<u8>) {
        let root = root(s3).await;
        let a = s3.create_folder("A", &root).await.unwrap().id;
        let x = content(100, 5);
        upload(s3, &a, "x.txt", &x, PART_UNIT).await;
        let b = s3.create_folder("B", &a).await.unwrap().id;
        let y = content(2 * UNIT as usize + 7, 6);
        upload(s3, &b, "y.bin", &y, UNIT as usize).await;
        (x, y)
    }

    #[tokio::test]
    async fn renaming_a_folder_in_a_versioned_bucket_deletes_the_old_keys() {
        let (mock, s3) = setup().await;
        mock.lock().unwrap().versioning = true;
        let (x, y) = tree(&s3).await;
        let moved = s3.move_rename("CloudDrop/A/", Some("Z"), None).await.unwrap();
        assert_eq!(moved, folder_meta("CloudDrop/Z/"));
        {
            let st = mock.lock().unwrap();
            assert_eq!(
                st.keys(),
                ["backup/CloudDrop/", "backup/CloudDrop/Z/", "backup/CloudDrop/Z/B/", "backup/CloudDrop/Z/B/y.bin", "backup/CloudDrop/Z/x.txt"]
            );
            assert_eq!(st.content("backup/CloudDrop/Z/x.txt"), Some(x.clone()));
            assert_eq!(st.content("backup/CloudDrop/Z/B/y.bin"), Some(y));
            assert_eq!(st.deletes, 4);
            let kept: Vec<&str> = st.noncurrent.iter().map(|(k, _)| k.as_str()).collect();
            assert!(kept.contains(&"backup/CloudDrop/A/x.txt") && kept.contains(&"backup/CloudDrop/A/B/y.bin"));
        }
        assert_eq!(s3.get("CloudDrop/Z/x.txt").await.unwrap().hash, Some(sha256(&x)));
        // Asking again after it finished is harmless.
        assert_eq!(s3.move_rename("CloudDrop/A/", Some("Z"), None).await.unwrap().id, "CloudDrop/Z/");
        assert_eq!(s3.move_rename("CloudDrop/Q/", Some("R"), None).await, Err(RemoteError::NotFound));
        assert!(matches!(s3.move_rename("CloudDrop/Z/", None, Some(("CloudDrop/Z/B/", "CloudDrop/"))).await, Err(RemoteError::Permanent(_))));
    }

    #[tokio::test]
    async fn without_versioning_a_rename_keeps_the_old_keys() {
        let (mock, s3) = setup().await;
        let (x, _) = tree(&s3).await;
        let moved = s3.move_rename("CloudDrop/A/", Some("Z"), None).await.unwrap();
        assert_eq!(moved.id, "CloudDrop/Z/");
        {
            let st = mock.lock().unwrap();
            assert_eq!(st.deletes, 0);
            for k in ["backup/CloudDrop/A/x.txt", "backup/CloudDrop/A/B/y.bin", "backup/CloudDrop/Z/x.txt", "backup/CloudDrop/Z/B/y.bin"] {
                assert!(st.objects.contains_key(k), "{k}");
            }
        }
        // A file moved into another folder and renamed.
        let m = s3.move_rename("CloudDrop/Z/x.txt", Some("w.txt"), Some(("CloudDrop/Z/B/", "CloudDrop/Z/"))).await.unwrap();
        assert_eq!((m.id.as_str(), m.hash.clone()), ("CloudDrop/Z/B/w.txt", Some(sha256(&x))));
        assert!(mock.lock().unwrap().objects.contains_key("backup/CloudDrop/Z/x.txt"));
        // A service that can't say whether it keeps versions doesn't get deletes either.
        let (mock, s3) = setup().await;
        {
            let mut st = mock.lock().unwrap();
            st.versioning = true;
            st.no_versioning_api = true;
        }
        tree(&s3).await;
        s3.move_rename("CloudDrop/A/x.txt", Some("v.txt"), None).await.unwrap();
        assert_eq!(mock.lock().unwrap().deletes, 0);
        // Nor does a key without delete permission: the rename still goes through.
        let (mock, s3) = setup().await;
        mock.lock().unwrap().versioning = true;
        tree(&s3).await;
        mock.lock().unwrap().fail.push_back(Fail::new(Method::DELETE, 403, "AccessDenied").message("Access Denied"));
        let m = s3.move_rename("CloudDrop/A/x.txt", Some("u.txt"), None).await.unwrap();
        assert_eq!(m.id, "CloudDrop/A/u.txt");
        assert!(mock.lock().unwrap().objects.contains_key("backup/CloudDrop/A/x.txt"));
    }

    #[tokio::test]
    async fn a_rename_onto_other_content_keeps_that_content_aside() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let (a, b) = (content(10, 1), content(10, 2));
        upload(&s3, &root, "a.txt", &a, PART_UNIT).await;
        upload(&s3, &root, "b.txt", &b, PART_UNIT).await;
        let m = s3.move_rename("CloudDrop/a.txt", Some("b.txt"), None).await.unwrap();
        assert_eq!(m.hash, Some(sha256(&a)));
        let st = mock.lock().unwrap();
        assert_eq!(st.content("backup/CloudDrop/b.txt"), Some(a));
        assert_eq!(st.content(&format!("backup/CloudDrop/b (replaced {}).txt", today())), Some(b));
    }

    #[tokio::test]
    async fn big_objects_are_copied_part_by_part() {
        let (mock, mut s3) = setup().await;
        mock.lock().unwrap().copy_max = 100_000;
        (s3.copy_max, s3.copy_part) = (100_000, UNIT);
        let root = root(&s3).await;
        let data = content(3 * UNIT as usize + 5, 8);
        upload(&s3, &root, "big.bin", &data, UNIT as usize).await;
        let m = s3.move_rename("CloudDrop/big.bin", Some("moved.bin"), None).await.unwrap();
        assert_eq!(m.size, Some(data.len() as u64));
        let st = mock.lock().unwrap();
        assert_eq!(st.content("backup/CloudDrop/moved.bin"), Some(data));
        assert_eq!(st.copies, 0, "no single CopyObject");
        assert_eq!(st.count("PUT backup/CloudDrop/moved.bin?partNumber"), 4);
        assert_eq!(st.uploads_open(), 0);
    }

    #[tokio::test]
    async fn keep_version_copies_the_file_beside_itself() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let data = content(50, 3);
        let m = upload(&s3, &root, "a.txt", &data, PART_UNIT).await;
        s3.keep_version(&m.id).await.unwrap();
        s3.keep_version(&m.id).await.unwrap();
        let first = format!("CloudDrop/a (replaced {}).txt", today());
        let second = format!("CloudDrop/a (replaced {} 2).txt", today());
        assert_eq!(s3.get(&first).await.unwrap().hash, Some(sha256(&data)));
        assert_eq!(s3.get(&second).await.unwrap().hash, Some(sha256(&data)));
        assert_eq!(mock.lock().unwrap().content("backup/CloudDrop/a.txt"), Some(data));
        assert_eq!(s3.keep_version("CloudDrop/gone.txt").await, Err(RemoteError::NotFound));
    }

    #[tokio::test]
    async fn an_upload_that_moves_the_file_retires_its_old_key_like_a_rename() {
        for versioning in [false, true] {
            let (mock, s3) = setup().await;
            mock.lock().unwrap().versioning = versioning;
            let root = root(&s3).await;
            let old = upload(&s3, &root, "old.txt", b"one", PART_UNIT).await;
            let session = s3.start_upload(Some(&old.id), "new.txt", (&root, Some(&root)), 3, "text/plain").await.unwrap();
            let m = send(&s3, &session, b"two", 0, PART_UNIT).await.unwrap();
            assert_eq!(m.id, "CloudDrop/new.txt");
            assert_eq!(mock.lock().unwrap().objects.contains_key("backup/CloudDrop/old.txt"), !versioning);
        }
    }

    #[tokio::test]
    async fn storage_quota_adds_up_the_prefix() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        mock.lock().unwrap().put("other/x", &[0; 1000]);
        upload(&s3, &root, "a", &[1; 10], PART_UNIT).await;
        upload(&s3, &root, "b", &[2; 20], PART_UNIT).await;
        assert_eq!(s3.storage_quota().await.unwrap(), (None, 30, 30));
        upload(&s3, &root, "c", &[3; 5], PART_UNIT).await;
        assert_eq!(s3.storage_quota().await.unwrap(), (None, 30, 30), "asked once an hour");
        // Too many objects to add up: nothing rather than part of it.
        mock.lock().unwrap().page = 2;
        let mut s3 = client(&mock, "backup");
        s3.quota_pages = 1;
        assert_eq!(s3.storage_quota().await.unwrap(), (None, 0, 0));
        let mut s3 = client(&mock, "backup");
        s3.quota_pages = 3;
        assert_eq!(s3.storage_quota().await.unwrap(), (None, 35, 35), "pages are followed");
    }

    #[tokio::test]
    async fn errors_become_neutral_kinds() {
        let (mock, s3) = setup().await;
        let root = root(&s3).await;
        let m = upload(&s3, &root, "a.txt", b"abc", PART_UNIT).await;

        // A wrong secret, found by a listing and by a HEAD (whose 403 says nothing by itself).
        mock.lock().unwrap().secret = "other".into();
        assert!(matches!(s3.get(&root).await, Err(RemoteError::Auth(_))));
        assert!(matches!(s3.get(&m.id).await, Err(RemoteError::Auth(_))));
        mock.lock().unwrap().secret = s3_mock::SECRET.into();
        let none = S3::new(crate::drive::http_client(), &S3Config { bucket: "b".into(), ..Default::default() }, None);
        assert_eq!(
            none.get("x").await,
            Err(RemoteError::Auth("no S3 secret key is saved; run `clouddrop setup --provider s3`".into()))
        );

        mock.lock().unwrap().fail.push_back(Fail::new(Method::PUT, 503, "SlowDown").retry_after(5));
        let session = s3.start_upload(None, "b.txt", (&root, None), 3, "text/plain").await.unwrap();
        assert!(matches!(
            send(&s3, &session, b"abc", 0, PART_UNIT).await,
            Err(RemoteError::RateLimited { retry_after: Some(5), .. })
        ));
        mock.lock().unwrap().fail.push_back(Fail::new(Method::HEAD, 503, "SlowDown"));
        assert!(matches!(s3.get(&m.id).await, Err(RemoteError::RateLimited { .. })));
        mock.lock().unwrap().fail.push_back(Fail::new(Method::GET, 500, "InternalError"));
        assert!(matches!(s3.get(&root).await, Err(RemoteError::Transient(_))));

        mock.lock().unwrap().cap = Some(10);
        assert!(matches!(send(&s3, &session, b"abcdefghijk", 0, PART_UNIT).await, Err(RemoteError::Quota(_))));
        mock.lock().unwrap().cap = None;

        let mut other = client(&mock, "backup");
        other.bucket = "nope".into();
        assert!(matches!(other.get(&root).await, Err(RemoteError::Permanent(m)) if m.contains("bucket")));

        // CompleteMultipartUpload failing inside a 200: retried, the upload is still there.
        let data = content(2 * UNIT as usize + 1, 1);
        let session = s3.start_upload(None, "c.bin", (&root, None), data.len() as u64, "x/y").await.unwrap();
        mock.lock().unwrap().fail.push_back(Fail::new(Method::POST, 200, "InternalError").on("uploadId").in_200());
        assert!(matches!(send(&s3, &session, &data, 0, UNIT as usize).await, Err(RemoteError::Transient(_))));
        match s3.query_upload(&session, data.len() as u64).await.unwrap() {
            UploadStatus::Done(m) => assert_eq!(m.size, Some(data.len() as u64)),
            other => panic!("{other:?}"),
        }
        assert_eq!(mock.lock().unwrap().content("backup/CloudDrop/c.bin"), Some(data));
        assert_eq!(mock.lock().unwrap().bad_signatures, 3, "the listing, the HEAD and the listing explaining it");
    }

    #[tokio::test]
    async fn remote_forwards_to_the_client() {
        let (_mock, s3) = setup().await;
        let r = crate::remote::Remote::S3(Box::new(s3));
        assert_eq!(r.provider(), crate::remote::Provider::S3);
        assert_eq!(r.caps(), &INFO.caps);
        assert_eq!(r.top_parent().await.unwrap(), "");
        let root = r.create_folder("CloudDrop", "").await.unwrap();
        assert_eq!(r.get(&root.id).await.unwrap(), root);
        assert_eq!(crate::remote::Provider::parse("s3").map(|p| p.display_name()), Some("S3-compatible storage"));
        assert_eq!(crate::remote::Provider::S3.web_url(crate::db::Kind::File, "x"), None);
    }
}
