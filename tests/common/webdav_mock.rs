//! In-process fake WebDAV server for the `webdav` and `nextcloud` providers.
//!
//! Plain mode: files under `/dav`, hrefs as whole URLs with Apache-style namespace prefixes and
//! light encoding, no quota, no checksums, and a PUT must say its length (no chunked transfer
//! encoding). Nextcloud mode: `status.php`, the OCS capabilities, the DAV principal (the user id
//! differs from the login name), `remote.php/dav/files/<id>` with `oc:checksums` and quota,
//! chunked upload v2 (`remote.php/dav/uploads/<id>`), `X-Hash`, `OC-Checksum` checked like a
//! recent Nextcloud does, and Login Flow v2. Both: Basic auth, error injection, missing parents
//! (409), `Overwrite: F` (412), storage full (507), and no DELETE.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// The login name (Login Flow and Basic auth).
pub const USER: &str = "Alice@example.com";
/// Nextcloud's id for that account: its files are at remote.php/dav/files/alice.
pub const USER_ID: &str = "alice";
pub const PASSWORD: &str = "app-password-1";

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Folder,
    File { content: Vec<u8>, checksum: Option<String> },
}

#[derive(Debug, Default)]
pub struct ChunkUpload {
    /// The Destination named when the upload folder was made (chunked upload v2).
    pub destination: Option<String>,
    pub chunks: BTreeMap<String, Vec<u8>>,
}

/// An answer to give instead of the real one, to the first request that matches.
#[derive(Clone, Debug)]
pub struct Fail {
    /// "" = any method.
    pub method: &'static str,
    /// Part of the request's decoded path ("" = any); chunk uploads show as "/uploads/<id>/<n>".
    pub path: &'static str,
    pub status: u16,
    pub retry_after: Option<u64>,
}

impl Fail {
    pub fn new(method: &'static str, path: &'static str, status: u16) -> Self {
        Fail { method, path, status, retry_after: None }
    }
}

#[derive(Default)]
pub struct DavState {
    pub nextcloud: bool,
    /// http://127.0.0.1:port
    pub base: String,
    /// Everything by path below the files' top level: "/" itself, "/a/b.txt".
    pub nodes: BTreeMap<String, Node>,
    pub uploads: HashMap<String, ChunkUpload>,
    /// Storage limit in bytes (Nextcloud mode reports it as quota).
    pub quota: Option<u64>,
    pub fail: VecDeque<Fail>,
    /// Every request: (method, decoded path).
    pub log: Vec<(String, String)>,
    /// The headers of the last request of each method.
    pub headers: HashMap<String, HeaderMap>,
    /// Login Flow polls answered "not yet" before the flow completes.
    pub login_polls: u32,
    login_tokens: Vec<String>,
    /// Answer GETs in full even when they ask for a range.
    pub ignore_range: bool,
    next: u64,
}

pub type Shared = Arc<Mutex<DavState>>;

enum Target {
    Files(String),
    Uploads(String, Option<String>),
    Principal,
    Status,
    Capabilities,
    LoginStart,
    LoginPoll,
    Other,
}

impl DavState {
    /// The address people would configure.
    pub fn url(&self) -> String {
        if self.nextcloud { self.base.clone() } else { format!("{}/dav/", self.base) }
    }

    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        match self.nodes.get(path) {
            Some(Node::File { content, .. }) => Some(content.clone()),
            _ => None,
        }
    }

    pub fn checksum(&self, path: &str) -> Option<String> {
        match self.nodes.get(path) {
            Some(Node::File { checksum, .. }) => checksum.clone(),
            _ => None,
        }
    }

    pub fn is_folder(&self, path: &str) -> bool {
        self.nodes.get(path) == Some(&Node::Folder)
    }

    /// Puts a file there, as another client would (folders above it too).
    pub fn seed(&mut self, path: &str, content: &[u8]) {
        let mut p = String::new();
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        for s in &segs[..segs.len() - 1] {
            p = format!("{p}/{s}");
            self.nodes.insert(p.clone(), Node::Folder);
        }
        self.nodes.insert(path.to_string(), Node::File { content: content.to_vec(), checksum: None });
    }

    /// Requests of `method` whose path contains `part`.
    pub fn count(&self, method: &str, part: &str) -> usize {
        self.log.iter().filter(|(m, p)| m == method && p.contains(part)).count()
    }

    pub fn used(&self) -> u64 {
        self.nodes
            .values()
            .map(|n| match n {
                Node::File { content, .. } => content.len() as u64,
                Node::Folder => 0,
            })
            .sum()
    }

    fn route(&self, raw: &str) -> Target {
        let segs: Vec<String> = raw.split('/').filter(|s| !s.is_empty()).map(decode).collect();
        let s: Vec<&str> = segs.iter().map(String::as_str).collect();
        let files = |rest: &[&str]| Target::Files(format!("/{}", rest.join("/")));
        if !self.nextcloud {
            return match s.as_slice() {
                ["dav", rest @ ..] => files(rest),
                _ => Target::Other,
            };
        }
        match s.as_slice() {
            ["status.php"] => Target::Status,
            ["ocs", "v1.php", "cloud", "capabilities"] => Target::Capabilities,
            ["index.php", "login", "v2"] => Target::LoginStart,
            ["login", "v2", "poll"] => Target::LoginPoll,
            ["remote.php", "dav"] => Target::Principal,
            ["remote.php", "dav", "files", USER_ID, rest @ ..] => files(rest),
            ["remote.php", "dav", "uploads", USER_ID, id] => Target::Uploads(id.to_string(), None),
            ["remote.php", "dav", "uploads", USER_ID, id, chunk] => {
                Target::Uploads(id.to_string(), Some(chunk.to_string()))
            }
            _ => Target::Other,
        }
    }

    /// Where a Destination header points (a whole URL or a path), below the files' top level.
    fn destination(&self, h: &HeaderMap) -> Option<String> {
        let d = h.get("Destination")?.to_str().ok()?;
        let path = d.strip_prefix(&self.base).unwrap_or(d);
        match self.route(path) {
            Target::Files(p) => Some(p),
            _ => None,
        }
    }

    fn respond(&mut self, method: &str, raw: &str, h: &HeaderMap, body: Bytes) -> Response {
        let target = self.route(raw);
        let shown = match &target {
            Target::Files(p) => p.clone(),
            Target::Uploads(id, c) => format!("/uploads/{id}/{}", c.as_deref().unwrap_or("")),
            _ => raw.to_string(),
        };
        self.log.push((method.to_string(), shown.clone()));
        self.headers.insert(method.to_string(), h.clone());
        if let Some(i) = self
            .fail
            .iter()
            .position(|f| (f.method.is_empty() || f.method == method) && shown.contains(f.path))
        {
            let f = self.fail.remove(i).unwrap();
            let mut r = (StatusCode::from_u16(f.status).unwrap(), "injected").into_response();
            if let Some(s) = f.retry_after {
                r.headers_mut().insert(header::RETRY_AFTER, s.into());
            }
            return r;
        }
        match target {
            Target::Other => return status(404),
            Target::Status => {
                return axum::Json(serde_json::json!({
                    "installed": true, "maintenance": false, "needsDbUpgrade": false,
                    "version": "29.0.4.1", "versionstring": "29.0.4", "edition": "",
                    "productname": "Nextcloud", "extendedSupport": false
                }))
                .into_response();
            }
            Target::LoginStart if method == "POST" => {
                self.next += 1;
                let token = format!("token-{}", self.next);
                self.login_tokens.push(token.clone());
                return axum::Json(serde_json::json!({
                    "poll": { "token": token, "endpoint": format!("{}/login/v2/poll", self.base) },
                    "login": format!("{}/login/v2/flow/{token}", self.base),
                }))
                .into_response();
            }
            Target::LoginPoll if method == "POST" => {
                let form: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
                let Some(i) = form.get("token").and_then(|t| self.login_tokens.iter().position(|x| x == t)) else {
                    return status(404);
                };
                if self.login_polls > 0 {
                    self.login_polls -= 1;
                    return status(404);
                }
                self.login_tokens.remove(i);
                return axum::Json(serde_json::json!({
                    "server": self.base, "loginName": USER, "appPassword": PASSWORD
                }))
                .into_response();
            }
            Target::LoginStart | Target::LoginPoll => return status(405),
            _ => {}
        }
        if !authed(h) {
            let mut r = self.error(401, "No public access to this resource.");
            r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"Nextcloud\""));
            return r;
        }
        match target {
            Target::Capabilities => {
                if h.get("OCS-APIRequest").is_none() {
                    return status(412);
                }
                axum::Json(serde_json::json!({ "ocs": {
                    "meta": { "status": "ok", "statuscode": 100 },
                    "data": { "version": { "major": 29 }, "capabilities": { "dav": { "chunking": "1.0" } } }
                }}))
                .into_response()
            }
            Target::Principal if method == "PROPFIND" => xml(format!(
                "{}<d:response><d:href>/remote.php/dav/</d:href><d:propstat><d:prop><d:current-user-principal>\
                 <d:href>/remote.php/dav/principals/users/{USER_ID}/</d:href></d:current-user-principal></d:prop>\
                 <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>",
                NC_OPEN
            )),
            Target::Files(p) => self.files(method, &p, h, body),
            Target::Uploads(id, chunk) => self.chunked(method, &id, chunk.as_deref(), h, body),
            _ => status(405),
        }
    }

    fn error(&self, code: u16, message: &str) -> Response {
        let st = StatusCode::from_u16(code).unwrap();
        if !self.nextcloud {
            return (st, message.to_string()).into_response();
        }
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\
             <s:exception>Sabre\\DAV\\Exception</s:exception><s:message>{}</s:message></d:error>",
            esc(message)
        );
        (st, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
    }

    fn parent_is_folder(&self, path: &str) -> bool {
        let parent = match path.rfind('/') {
            Some(0) | None => "/",
            Some(i) => &path[..i],
        };
        self.is_folder(parent)
    }

    /// Writes a file; checks the parent, room left and `OC-Checksum` (Nextcloud).
    fn write(&mut self, path: &str, content: Vec<u8>, h: &HeaderMap) -> Result<bool, Response> {
        if !self.parent_is_folder(path) {
            return Err(self.error(409, "Parent folder missing"));
        }
        let old = match self.nodes.get(path) {
            Some(Node::Folder) => return Err(self.error(409, "A folder is in the way")),
            Some(Node::File { content, .. }) => Some(content.len() as u64),
            None => None,
        };
        if let Some(limit) = self.quota
            && self.used() - old.unwrap_or(0) + content.len() as u64 > limit
        {
            return Err(self.error(507, "Insufficient Storage"));
        }
        let mut checksum = None;
        if self.nextcloud
            && let Some(sent) = h.get("OC-Checksum").and_then(|v| v.to_str().ok())
        {
            let want = sent.strip_prefix("SHA1:").unwrap_or_default().to_lowercase();
            if want != sha1_hex(&content) {
                return Err(self.error(400, "The computed checksum does not match the one received from the client."));
            }
            checksum = Some(sent.to_string());
        }
        self.nodes.insert(path.to_string(), Node::File { content, checksum });
        Ok(old.is_some())
    }

    fn files(&mut self, method: &str, path: &str, h: &HeaderMap, body: Bytes) -> Response {
        match method {
            "PROPFIND" => {
                let Some(node) = self.nodes.get(path) else {
                    return self.error(404, "File not found");
                };
                let mut entries = vec![(path.to_string(), node.clone())];
                let depth = h.get("Depth").and_then(|v| v.to_str().ok()).unwrap_or("1");
                if depth != "0" && *node == Node::Folder {
                    entries.extend(
                        self.nodes
                            .iter()
                            .filter(|(p, _)| p.as_str() != path && parent_of(p) == path)
                            .map(|(p, n)| (p.clone(), n.clone())),
                    );
                }
                self.multistatus(&entries)
            }
            "MKCOL" => {
                if self.nodes.contains_key(path) {
                    return self.error(405, "The resource you tried to create already exists");
                }
                if !self.parent_is_folder(path) {
                    return self.error(409, "Parent node does not exist");
                }
                self.nodes.insert(path.to_string(), Node::Folder);
                status(201)
            }
            "PUT" => {
                let chunked = h
                    .get(header::TRANSFER_ENCODING)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("chunked"));
                if !self.nextcloud && (chunked || h.get(header::CONTENT_LENGTH).is_none()) {
                    return status(411);
                }
                let sha1 = sha1_hex(&body);
                match self.write(path, body.to_vec(), h) {
                    Ok(replaced) => {
                        let mut r = status(if replaced { 204 } else { 201 });
                        if self.nextcloud && h.get("X-Hash").is_some_and(|v| v == "sha1") {
                            r.headers_mut().insert("X-Hash-SHA1", sha1.parse().unwrap());
                        }
                        r
                    }
                    Err(r) => r,
                }
            }
            "GET" => match self.nodes.get(path) {
                Some(Node::File { content, checksum }) => {
                    let range = h
                        .get(header::RANGE)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|r| r.strip_prefix("bytes="))
                        .and_then(|r| r.split_once('-'))
                        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
                    let mut r = match range.filter(|_| !self.ignore_range) {
                        Some((a, b)) if a < content.len() => {
                            let b = b.min(content.len() - 1);
                            (StatusCode::PARTIAL_CONTENT, content[a..=b].to_vec()).into_response()
                        }
                        Some(_) => status(416),
                        None => content.clone().into_response(),
                    };
                    if self.nextcloud
                        && let Some(c) = checksum
                    {
                        r.headers_mut().insert("OC-Checksum", c.parse().unwrap());
                    }
                    r
                }
                Some(Node::Folder) => status(405),
                None => self.error(404, "File not found"),
            },
            "MOVE" | "COPY" => {
                let Some(dest) = self.destination(h) else {
                    return self.error(502, "Destination is on another server");
                };
                if !self.nodes.contains_key(path) {
                    return self.error(404, "Source not found");
                }
                if !self.parent_is_folder(&dest) {
                    return self.error(409, "Destination parent missing");
                }
                let overwrite = h.get("Overwrite").is_none_or(|v| v != "F");
                let existed = self.nodes.contains_key(&dest);
                if existed && !overwrite {
                    return self.error(412, "Destination exists");
                }
                let moved: Vec<(String, Node)> = self
                    .nodes
                    .iter()
                    .filter(|(p, _)| *p == path || p.starts_with(&format!("{path}/")))
                    .map(|(p, n)| (format!("{dest}{}", &p[path.len()..]), n.clone()))
                    .collect();
                if method == "COPY"
                    && let Some(limit) = self.quota
                {
                    let extra: u64 = moved
                        .iter()
                        .map(|(_, n)| match n {
                            Node::File { content, .. } => content.len() as u64,
                            Node::Folder => 0,
                        })
                        .sum();
                    if self.used() + extra > limit {
                        return self.error(507, "Insufficient Storage");
                    }
                }
                if existed {
                    self.nodes.retain(|p, _| *p != dest && !p.starts_with(&format!("{dest}/")));
                }
                if method == "MOVE" {
                    self.nodes.retain(|p, _| p != path && !p.starts_with(&format!("{path}/")));
                }
                self.nodes.extend(moved);
                status(if existed { 204 } else { 201 })
            }
            _ => status(405),
        }
    }

    fn chunked(&mut self, method: &str, id: &str, chunk: Option<&str>, h: &HeaderMap, body: Bytes) -> Response {
        let dest_header = h.get("Destination").and_then(|v| v.to_str().ok()).map(str::to_string);
        match (method, chunk) {
            ("MKCOL", None) => {
                if self.uploads.contains_key(id) {
                    return self.error(405, "The resource you tried to create already exists");
                }
                self.uploads.insert(id.to_string(), ChunkUpload { destination: dest_header, ..Default::default() });
                status(201)
            }
            ("PROPFIND", None) => {
                let Some(up) = self.uploads.get(id) else {
                    return self.error(404, "Upload not found");
                };
                let mut out = format!(
                    "{NC_OPEN}<d:response><d:href>/remote.php/dav/uploads/{USER_ID}/{id}/</d:href>{}</d:response>",
                    propstat("<d:resourcetype><d:collection/></d:resourcetype>")
                );
                for (name, data) in &up.chunks {
                    out.push_str(&format!(
                        "<d:response><d:href>/remote.php/dav/uploads/{USER_ID}/{id}/{name}</d:href>{}</d:response>",
                        propstat(&format!("<d:resourcetype/><d:getcontentlength>{}</d:getcontentlength>", data.len()))
                    ));
                }
                out.push_str("</d:multistatus>");
                xml(out)
            }
            ("PUT", Some(name)) if name != ".file" => {
                let Some(up) = self.uploads.get_mut(id) else {
                    return status(404);
                };
                if up.destination.is_some() && up.destination != dest_header {
                    return status(400);
                }
                up.chunks.insert(name.to_string(), body.to_vec());
                status(201)
            }
            ("MOVE", Some(".file")) => {
                let Some(dest) = self.destination(h) else {
                    return status(400);
                };
                let Some(up) = self.uploads.get(id) else {
                    return self.error(404, "Upload not found");
                };
                let content: Vec<u8> = up.chunks.values().flatten().copied().collect();
                let total = h.get("OC-Total-Length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok());
                if total != Some(content.len()) {
                    return self.error(400, "Chunks on server do not sum up to the expected size");
                }
                match self.write(&dest, content, h) {
                    Ok(replaced) => {
                        self.uploads.remove(id);
                        status(if replaced { 204 } else { 201 })
                    }
                    Err(r) => r,
                }
            }
            _ => status(405),
        }
    }

    fn href(&self, path: &str, folder: bool) -> String {
        // Nextcloud: a path, encoded like PHP's rawurlencode. Others: like some servers, a whole
        // URL with only the bare minimum encoded and non-ASCII left raw.
        let (mut h, encode): (String, fn(&str) -> String) = if self.nextcloud {
            (format!("/remote.php/dav/files/{USER_ID}"), raw_url_encode)
        } else {
            (format!("{}/dav", self.base), light_encode)
        };
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            h.push('/');
            h.push_str(&encode(seg));
        }
        if folder {
            h.push('/');
        }
        esc(&h)
    }

    fn multistatus(&self, entries: &[(String, Node)]) -> Response {
        let mut out = String::new();
        if self.nextcloud {
            out.push_str(NC_OPEN);
            for (path, node) in entries {
                let folder = *node == Node::Folder;
                out.push_str(&format!("<d:response><d:href>{}</d:href>", self.href(path, folder)));
                match node {
                    Node::Folder => {
                        let used = self.used();
                        let available = self.quota.map_or(-3, |q| q.saturating_sub(used) as i64);
                        out.push_str(&propstat(&format!(
                            "<d:resourcetype><d:collection/></d:resourcetype>\
                             <d:quota-used-bytes>{used}</d:quota-used-bytes>\
                             <d:quota-available-bytes>{available}</d:quota-available-bytes>"
                        )));
                        out.push_str(&missing("<d:getcontentlength/><oc:checksums/>"));
                    }
                    Node::File { content, checksum } => {
                        let mut props = format!("<d:resourcetype/><d:getcontentlength>{}</d:getcontentlength>", content.len());
                        match checksum {
                            Some(c) => props.push_str(&format!("<oc:checksums><oc:checksum>{c}</oc:checksum></oc:checksums>")),
                            None => out.push_str(&missing("<oc:checksums/>")),
                        }
                        out.push_str(&propstat(&props));
                    }
                }
                out.push_str("</d:response>");
            }
            out.push_str("</d:multistatus>");
        } else {
            out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\" xmlns:ns0=\"DAV:\">");
            for (path, node) in entries {
                let folder = *node == Node::Folder;
                out.push_str(&format!("<D:response xmlns:lp1=\"DAV:\"><D:href>{}</D:href>", self.href(path, folder)));
                let props = match node {
                    Node::Folder => "<lp1:resourcetype><D:collection/></lp1:resourcetype>".to_string(),
                    Node::File { content, .. } => {
                        format!("<lp1:resourcetype/><lp1:getcontentlength>{}</lp1:getcontentlength>", content.len())
                    }
                };
                out.push_str(&format!("<D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>"));
                out.push_str(
                    "<D:propstat><D:prop><ns1:checksums xmlns:ns1=\"http://owncloud.org/ns\"/><D:quota-used-bytes/>\
                     </D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response>",
                );
            }
            out.push_str("</D:multistatus>");
        }
        xml(out)
    }
}

const NC_OPEN: &str = "<?xml version=\"1.0\"?>\n<d:multistatus xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\" \
    xmlns:oc=\"http://owncloud.org/ns\" xmlns:nc=\"http://nextcloud.org/ns\">";

fn propstat(props: &str) -> String {
    format!("<d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>")
}

fn missing(props: &str) -> String {
    format!("<d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>")
}

fn xml(body: String) -> Response {
    (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
}

fn status(code: u16) -> Response {
    StatusCode::from_u16(code).unwrap().into_response()
}

fn authed(h: &HeaderMap) -> bool {
    let Some(b64) = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Basic ")) else {
        return false;
    };
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
    decoded == format!("{USER}:{PASSWORD}").as_bytes()
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

pub fn sha1_hex(data: &[u8]) -> String {
    hex::encode(Sha1::digest(data))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("paths are UTF-8")
}

/// PHP's rawurlencode, as Nextcloud encodes hrefs.
fn raw_url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn light_encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3f"),
            c if c.is_control() => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

async fn handle(State(s): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // A request cut off half-way changes nothing, as on a real server.
    let Ok(body) = axum::body::to_bytes(body, usize::MAX).await else {
        s.lock().unwrap().log.push((format!("{} (cut off)", parts.method), parts.uri.path().to_string()));
        return status(400);
    };
    let mut st = s.lock().unwrap();
    st.respond(parts.method.as_str(), parts.uri.path(), &parts.headers, body)
}

pub async fn start(nextcloud: bool) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(DavState { nextcloud, ..Default::default() }));
    shared.lock().unwrap().nodes.insert("/".into(), Node::Folder);
    let app = Router::new().fallback(handle).with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    shared.lock().unwrap().base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    shared
}
