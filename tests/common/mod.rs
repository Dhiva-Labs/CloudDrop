//! In-process fake of the Google OAuth token endpoint and the Drive v3 subset
//! CloudDrop uses, plus a harness that runs the engine against it.
//!
//! Switches on `MockState` make it behave like other providers (no content hashes, ids that
//! are paths, forbidden characters, names unique regardless of case), and `Harness::with_caps`
//! runs the engine with that provider's capabilities, so the engine's handling of them is
//! tested without a second provider.

#![allow(dead_code)]

pub mod onedrive_mock;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as AxPath, Query, State as AxState};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use clouddrop::actions::{self, Ctx, FreeReport, RestoreReport};
use clouddrop::auth::{Token, save_token};
use clouddrop::config::{Config, Endpoints};
use clouddrop::db::{Db, Entry, State};
use clouddrop::remote::Capabilities;
use md5::{Digest, Md5};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub const FOLDER: &str = "application/vnd.google-apps.folder";

#[derive(Clone, Debug)]
pub struct Rev {
    pub id: String,
    pub md5: String,
    pub keep_forever: bool,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub parents: Vec<String>,
    pub content: Vec<u8>,
    pub trashed: bool,
    pub revisions: Vec<Rev>,
}

impl Item {
    pub fn md5(&self) -> String {
        hex::encode(Md5::digest(&self.content))
    }

    fn json(&self) -> Value {
        let mut v = json!({
            "id": self.id, "name": self.name, "mimeType": self.mime,
            "parents": self.parents, "trashed": self.trashed,
        });
        if self.mime != FOLDER {
            v["md5Checksum"] = json!(self.md5());
            v["size"] = json!(self.content.len().to_string());
            if let Some(r) = self.revisions.last() {
                v["headRevisionId"] = json!(r.id);
            }
        }
        v
    }
}

struct Session {
    file_id: Option<String>,
    name: String,
    parents: Vec<String>,
    move_parents: Option<(String, String)>,
    total: usize,
    received: Vec<u8>,
    result: Option<Value>,
}

#[derive(Default)]
pub struct MockState {
    pub items: HashMap<String, Item>,
    sessions: HashMap<String, Session>,
    tokens: Vec<String>,
    next: u64,
    pub sessions_created: u64,
    pub chunk_puts: u64,
    pub folders_created: u64,
    pub refreshes: u64,
    /// Statuses to answer the next chunk PUTs with, before touching the session.
    pub fail_puts: Vec<u16>,
    /// Status to answer the Nth chunk PUT with (1-based count of all chunk PUTs).
    pub fail_at: HashMap<u64, u16>,
    /// Slows every chunk PUT down (to act while an upload is in flight).
    pub put_delay_ms: u64,
    pub base: String,
    /// Like a provider without content hashes: no md5Checksum in metadata.
    pub no_hashes: bool,
    /// Like WebDAV/S3/SFTP: an item's id is its path ("root/CloudDrop/a/b.txt"), so a rename or
    /// move changes its id and the ids of everything beneath it; old ids answer 404.
    pub path_ids: bool,
    /// Characters names may not contain (400).
    pub forbidden: String,
    /// Names in a folder are unique ignoring case (409 for a clash).
    pub case_insensitive: bool,
    /// File content downloads (`alt=media`).
    pub downloads: u64,
    /// Slows every content download down (to watch a verify-by-download happen).
    pub media_delay_ms: u64,
    /// Every token request's form, in order.
    pub token_forms: Vec<HashMap<String, String>>,
}

impl MockState {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    pub fn children(&self, parent: &str) -> Vec<Item> {
        let mut v: Vec<Item> = self
            .items
            .values()
            .filter(|i| i.parents.iter().any(|p| p == parent) && !i.trashed)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Resolves "a/b/c" from My Drive root by names; None if missing or ambiguous.
    pub fn lookup(&self, path: &str) -> Option<Item> {
        let mut parent = "root".to_string();
        let mut found = None;
        for part in path.split('/') {
            let hits: Vec<Item> = self
                .children(&parent)
                .into_iter()
                .filter(|i| i.name == part)
                .collect();
            if hits.len() != 1 {
                return None;
            }
            parent = hits[0].id.clone();
            found = Some(hits[0].clone());
        }
        found
    }

    /// Every non-folder file below `parent`, as "rel/path" -> content.
    pub fn tree(&self, parent: &str, prefix: &str, out: &mut Vec<(String, String)>) {
        for i in self.children(parent) {
            let p = if prefix.is_empty() { i.name.clone() } else { format!("{prefix}/{}", i.name) };
            if i.mime == FOLDER {
                out.push((format!("{p}/"), String::new()));
                self.tree(&i.id, &p, out);
            } else {
                out.push((p, String::from_utf8_lossy(&i.content).to_string()));
            }
        }
    }

    pub fn invalidate_tokens(&mut self) {
        self.tokens.clear();
    }

    /// An item as the API shows it.
    fn view(&self, item: &Item) -> Value {
        let mut v = item.json();
        if self.no_hashes {
            v.as_object_mut().unwrap().remove("md5Checksum");
        }
        v
    }

    /// The id for a new item: a path where ids are paths.
    fn new_id(&mut self, prefix: &str, parent: &str, name: &str) -> String {
        if self.path_ids { format!("{parent}/{name}") } else { self.id(prefix) }
    }

    /// Refuses a name the provider wouldn't take for `parent` (`except` = the item being renamed).
    fn name_problem(&self, name: &str, parent: &str, except: Option<&str>) -> Option<Response> {
        if name.contains(|c| self.forbidden.contains(c)) {
            return Some((StatusCode::BAD_REQUEST, format!("invalid name {name:?}")).into_response());
        }
        let clash = self.children(parent).into_iter().any(|i| {
            Some(i.id.as_str()) != except
                && if self.case_insensitive { i.name.to_lowercase() == name.to_lowercase() } else { self.path_ids && i.name == name }
        });
        clash.then(|| (StatusCode::CONFLICT, format!("{name:?} already exists")).into_response())
    }

    /// Where ids are paths: gives `old` (named `name` in `parent` now) its new id, and moves the
    /// ids beneath it along. Returns the new id.
    fn rekey(&mut self, old: &str) -> String {
        if !self.path_ids {
            return old.to_string();
        }
        let item = &self.items[old];
        let new = format!("{}/{}", item.parents[0], item.name);
        let prefix = format!("{old}/");
        let moved = |s: &str| if s == old || s.starts_with(&prefix) { Some(format!("{new}{}", &s[old.len()..])) } else { None };
        let keys: Vec<String> = self.items.keys().filter(|k| moved(k).is_some()).cloned().collect();
        for k in keys {
            let mut item = self.items.remove(&k).unwrap();
            item.id = moved(&k).unwrap();
            for p in item.parents.iter_mut() {
                if let Some(np) = moved(p) {
                    *p = np;
                }
            }
            self.items.insert(item.id.clone(), item);
        }
        new
    }
}

pub type Shared = Arc<Mutex<MockState>>;

fn authed(st: &MockState, h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| st.tokens.iter().any(|x| x == t))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": {"code": 401, "message": "Invalid Credentials"}}))).into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, axum::Json(json!({"error": {"code": 404, "message": "File not found"}}))).into_response()
}

async fn token(AxState(s): AxState<Shared>, body: String) -> Response {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    let mut st = s.lock().unwrap();
    st.token_forms.push(form.clone());
    let tok = st.id("tok");
    st.tokens.push(tok.clone());
    match form.get("grant_type").map(String::as_str) {
        Some("refresh_token") => {
            st.refreshes += 1;
            if form.get("refresh_token").map(String::as_str) != Some("good-refresh") {
                return (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant"}))).into_response();
            }
            axum::Json(json!({"access_token": tok, "expires_in": 3600})).into_response()
        }
        Some("authorization_code") => axum::Json(json!({
            "access_token": tok, "expires_in": 3600, "refresh_token": "good-refresh"
        }))
        .into_response(),
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// Pulls the quoted literals out of "name = 'x' and 'p' in parents and …".
fn quoted(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = q.chars();
    while let Some(c) = chars.next() {
        if c == '\'' {
            let mut s = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => s.extend(chars.next()),
                    '\'' => break,
                    c => s.push(c),
                }
            }
            out.push(s);
        }
    }
    out
}

async fn list(AxState(s): AxState<Shared>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let st = s.lock().unwrap();
    if !authed(&st, &h) {
        return unauthorized();
    }
    let lits = quoted(q.get("q").map(String::as_str).unwrap_or(""));
    let (name, parent) = (&lits[0], &lits[1]);
    let mime = lits.get(2);
    let files: Vec<Value> = st
        .children(parent)
        .into_iter()
        .filter(|i| {
            let same = if st.case_insensitive { i.name.to_lowercase() == name.to_lowercase() } else { &i.name == name };
            same && mime.is_none_or(|m| &i.mime == m)
        })
        .map(|i| st.view(&i))
        .collect();
    axum::Json(json!({ "files": files })).into_response()
}

async fn create(AxState(s): AxState<Shared>, h: HeaderMap, axum::Json(body): axum::Json<Value>) -> Response {
    let mut st = s.lock().unwrap();
    if !authed(&st, &h) {
        return unauthorized();
    }
    let name = body["name"].as_str().unwrap().to_string();
    let parents: Vec<String> = body["parents"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
    if let Some(refused) = st.name_problem(&name, &parents[0], None) {
        return refused;
    }
    let id = st.new_id("F", &parents[0], &name);
    let item = Item {
        id: id.clone(),
        name,
        mime: body["mimeType"].as_str().unwrap_or("application/octet-stream").to_string(),
        parents,
        content: vec![],
        trashed: false,
        revisions: vec![],
    };
    st.folders_created += 1;
    let v = st.view(&item);
    st.items.insert(id, item);
    axum::Json(v).into_response()
}

/// GET files/{id} (ids may contain '/' where they are paths).
async fn get_file(
    AxState(s): AxState<Shared>,
    h: HeaderMap,
    AxPath(id): AxPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let delay = s.lock().unwrap().media_delay_ms;
    if delay > 0 && q.get("alt").map(String::as_str) == Some("media") {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let mut st = s.lock().unwrap();
    if !authed(&st, &h) {
        return unauthorized();
    }
    let Some(item) = st.items.get(&id).cloned() else {
        return not_found();
    };
    if q.get("alt").map(String::as_str) == Some("media") {
        st.downloads += 1;
        if let Some(range) = h.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|r| r.strip_prefix("bytes=")) {
            let (a, b) = range.split_once('-').unwrap();
            let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
            let end = (b + 1).min(item.content.len());
            return item.content[a.min(end)..end].to_vec().into_response();
        }
        return item.content.into_response();
    }
    axum::Json(st.view(&item)).into_response()
}

/// PATCH files/{id} and files/{id}/revisions/{rev}.
async fn patch_any(
    s: AxState<Shared>,
    h: HeaderMap,
    AxPath(rest): AxPath<String>,
    q: Query<HashMap<String, String>>,
    body: axum::Json<Value>,
) -> Response {
    match rest.rsplit_once("/revisions/") {
        Some((id, rev)) => update_revision(s, h, id.to_string(), rev.to_string(), body).await,
        None => update_file(s, h, rest, q, body).await,
    }
}

async fn update_file(
    AxState(s): AxState<Shared>,
    h: HeaderMap,
    id: String,
    Query(q): Query<HashMap<String, String>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut st = s.lock().unwrap();
    if !authed(&st, &h) {
        return unauthorized();
    }
    let Some(item) = st.items.get(&id) else {
        return not_found();
    };
    let name = body["name"].as_str().map_or(item.name.clone(), str::to_string);
    let parent = q.get("addParents").cloned().unwrap_or_else(|| item.parents[0].clone());
    if let Some(refused) = st.name_problem(&name, &parent, Some(&id)) {
        return refused;
    }
    let item = st.items.get_mut(&id).unwrap();
    item.name = name;
    if let (Some(add), Some(rm)) = (q.get("addParents"), q.get("removeParents")) {
        item.parents.retain(|p| p != rm);
        item.parents.push(add.clone());
    }
    let id = st.rekey(&id);
    axum::Json(st.view(&st.items[&id])).into_response()
}

async fn update_revision(
    AxState(s): AxState<Shared>,
    h: HeaderMap,
    id: String,
    rev: String,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut st = s.lock().unwrap();
    if !authed(&st, &h) {
        return unauthorized();
    }
    let Some(r) = st
        .items
        .get_mut(&id)
        .and_then(|i| i.revisions.iter_mut().find(|r| r.id == rev))
    else {
        return not_found();
    };
    r.keep_forever = body["keepForever"].as_bool().unwrap_or(false);
    axum::Json(json!({ "id": rev })).into_response()
}

fn start_session(st: &mut MockState, h: &HeaderMap, file_id: Option<String>, q: &HashMap<String, String>, body: &Value) -> Response {
    if !authed(st, h) {
        return unauthorized();
    }
    if let Some(id) = &file_id
        && !st.items.contains_key(id)
    {
        return not_found();
    }
    let name = body["name"].as_str().unwrap_or_default();
    let parent = match &file_id {
        Some(id) => q.get("addParents").cloned().unwrap_or_else(|| st.items[id].parents[0].clone()),
        None => body["parents"][0].as_str().unwrap_or_default().to_string(),
    };
    if !name.is_empty()
        && let Some(refused) = st.name_problem(name, &parent, file_id.as_deref())
    {
        return refused;
    }
    let total: usize = h
        .get("X-Upload-Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap();
    let sid = st.id("S");
    st.sessions_created += 1;
    st.sessions.insert(
        sid.clone(),
        Session {
            file_id,
            name: body["name"].as_str().unwrap_or_default().to_string(),
            parents: body["parents"]
                .as_array()
                .map(|a| a.iter().map(|p| p.as_str().unwrap().to_string()).collect())
                .unwrap_or_default(),
            move_parents: q.get("addParents").zip(q.get("removeParents")).map(|(a, r)| (a.clone(), r.clone())),
            total,
            received: vec![],
            result: None,
        },
    );
    let loc = format!("{}/session/{sid}", st.base);
    (StatusCode::OK, [(header::LOCATION, loc)]).into_response()
}

async fn upload_create(
    AxState(s): AxState<Shared>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut st = s.lock().unwrap();
    start_session(&mut st, &h, None, &q, &body)
}

async fn upload_update(
    AxState(s): AxState<Shared>,
    h: HeaderMap,
    AxPath(id): AxPath<String>, // may contain '/' where ids are paths
    Query(q): Query<HashMap<String, String>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut st = s.lock().unwrap();
    start_session(&mut st, &h, Some(id), &q, &body)
}

fn range_reply(received: usize) -> Response {
    let mut resp = StatusCode::PERMANENT_REDIRECT.into_response();
    if received > 0 {
        resp.headers_mut()
            .insert(header::RANGE, format!("bytes=0-{}", received - 1).parse().unwrap());
    }
    resp
}

async fn session_put(AxState(s): AxState<Shared>, h: HeaderMap, AxPath(sid): AxPath<String>, body: Bytes) -> Response {
    let delay = s.lock().unwrap().put_delay_ms;
    if delay > 0 && !body.is_empty() {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let mut st = s.lock().unwrap();
    if !authed(&st, &h) {
        return unauthorized();
    }
    let range = h.get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let spec = range.strip_prefix("bytes ").unwrap_or("");
    let is_query = spec.starts_with("*/") && body.is_empty();
    if !is_query {
        st.chunk_puts += 1;
        let n = st.chunk_puts;
        let injected = if st.fail_puts.is_empty() { st.fail_at.remove(&n) } else { Some(st.fail_puts.remove(0)) };
        if let Some(code) = injected {
            return StatusCode::from_u16(code).unwrap().into_response();
        }
    }
    let next_id = st.next + 1;
    let Some(sess) = st.sessions.get_mut(&sid) else {
        return not_found();
    };
    if let Some(done) = &sess.result {
        return axum::Json(done.clone()).into_response();
    }
    if !spec.starts_with("*/") {
        let (span, _) = spec.split_once('/').unwrap();
        let (a, b) = span.split_once('-').unwrap();
        let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
        if a != sess.received.len() || b + 1 - a != body.len() {
            return (StatusCode::BAD_REQUEST, "bad range").into_response();
        }
        sess.received.extend_from_slice(&body);
    }
    if sess.received.len() < sess.total {
        return range_reply(sess.received.len());
    }
    // Complete: create or update the file.
    let content = std::mem::take(&mut sess.received);
    let (file_id, name, parents, move_parents) =
        (sess.file_id.clone(), sess.name.clone(), sess.parents.clone(), sess.move_parents.clone());
    st.next = next_id;
    let rev = Rev { id: format!("R{next_id}"), md5: hex::encode(Md5::digest(&content)), keep_forever: false };
    let item = match file_id {
        Some(id) => {
            let Some(item) = st.items.get_mut(&id) else {
                return not_found(); // renamed away (ids are paths) while uploading
            };
            item.content = content;
            if !name.is_empty() {
                item.name = name;
            }
            if let Some((add, rm)) = move_parents {
                item.parents.retain(|p| *p != rm);
                item.parents.push(add);
            }
            item.revisions.push(rev);
            let id = st.rekey(&id);
            st.items[&id].clone()
        }
        None => {
            let id = st.new_id("D", &parents[0], &name);
            if st.items.contains_key(&id) {
                return (StatusCode::CONFLICT, "exists").into_response();
            }
            let item = Item {
                id: id.clone(),
                name,
                mime: "application/octet-stream".into(),
                parents,
                content,
                trashed: false,
                revisions: vec![rev],
            };
            st.items.insert(id, item.clone());
            item
        }
    };
    let v = st.view(&item);
    st.sessions.get_mut(&sid).unwrap().result = Some(v.clone());
    (StatusCode::CREATED, axum::Json(v)).into_response()
}

async fn about(AxState(s): AxState<Shared>, h: HeaderMap) -> Response {
    let st = s.lock().unwrap();
    if !authed(&st, &h) {
        return unauthorized();
    }
    let used: usize = st.items.values().map(|i| i.content.len()).sum();
    axum::Json(json!({ "storageQuota": {
        "limit": "16106127360", "usage": (used + 1000).to_string(), "usageInDrive": used.to_string()
    }}))
    .into_response()
}

/// Stands in for Google's consent page: approves at once and redirects back.
async fn auth(Query(q): Query<HashMap<String, String>>) -> Response {
    let target = format!(
        "{}/?code=mock-code&state={}",
        q.get("redirect_uri").cloned().unwrap_or_default(),
        q.get("state").cloned().unwrap_or_default()
    );
    (StatusCode::FOUND, [(header::LOCATION, target)]).into_response()
}

pub async fn start_mock() -> Shared {
    start_mock_at("127.0.0.1:0").await
}

pub async fn start_mock_at(addr: &str) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(MockState::default()));
    let app = Router::new()
        .route("/token", post(token))
        .route("/auth", get(auth))
        .route("/drive/v3/about", get(about))
        .route("/drive/v3/files", get(list).post(create))
        .route("/drive/v3/files/{*id}", get(get_file).patch(patch_any))
        .route("/upload/drive/v3/files", post(upload_create))
        .route("/upload/drive/v3/files/{*id}", patch(upload_update))
        .route("/session/{sid}", put(session_put))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    shared.lock().unwrap().base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    shared
}

// ---------------------------------------------------------------------------

pub struct Harness {
    pub tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub data: PathBuf,
    pub cfg: Config,
    pub mock: Shared,
    pub db: Arc<Db>,
    /// Run the engine and the CLI actions as if the provider had these capabilities.
    pub caps: Option<Capabilities>,
    engine: Option<(watch::Sender<bool>, tokio::task::JoinHandle<anyhow::Result<()>>)>,
}

/// Google Drive's capabilities, to change for a test (`Capabilities { hash: None, ..drive_caps() }`).
pub fn drive_caps() -> Capabilities {
    clouddrop::drive::INFO.caps.clone()
}

impl Harness {
    pub async fn new() -> Self {
        Self::with(|_| {}).await
    }

    /// The engine and actions apply `caps` (another provider's rules) to the Drive mock.
    pub async fn with_caps(caps: Capabilities, tweak: impl FnOnce(&mut Config)) -> Self {
        let mut h = Self::with(tweak).await;
        h.caps = Some(caps);
        h
    }

    pub fn caps(&self) -> Capabilities {
        self.caps.clone().unwrap_or_else(|| self.remote().caps().clone())
    }

    /// `clouddrop free`, with the harness's capabilities.
    pub async fn free(&self, rels: &[&str]) -> anyhow::Result<FreeReport> {
        let rels: Vec<String> = rels.iter().map(|r| r.to_string()).collect();
        actions::free_with(&self.ctx(), &self.remote(), &self.caps(), &rels).await
    }

    /// `clouddrop restore`, with the harness's capabilities.
    pub async fn restore(&self, rels: &[&str]) -> anyhow::Result<RestoreReport> {
        let rels: Vec<String> = rels.iter().map(|r| r.to_string()).collect();
        actions::restore_with(&self.ctx(), &self.remote(), &self.caps(), &rels).await
    }

    pub async fn with(tweak: impl FnOnce(&mut Config)) -> Self {
        let mock = start_mock().await;
        let base = mock.lock().unwrap().base.clone();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("CloudDrop");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg = Config {
            sync_dir: root.clone(),
            client_id: "cid".into(),
            client_secret: "secret".into(),
            settle_secs: 1,
            notifications: false,
            chunk_size: 256 * 1024,
            endpoints: Endpoints {
                auth_url: format!("{base}/auth"),
                token_url: format!("{base}/token"),
                api_base: format!("{base}/drive/v3"),
                upload_base: format!("{base}/upload/drive/v3"),
            },
            ..Config::default()
        };
        tweak(&mut cfg);
        save_token(
            &data.join("token.json"),
            &Token { access_token: "stale".into(), refresh_token: "good-refresh".into(), expires_at: 0 },
        )
        .unwrap();
        let db = Arc::new(Db::open(&data.join("state.db")).unwrap());
        Self { tmp, root, data, cfg, mock, db, caps: None, engine: None }
    }

    pub fn token_path(&self) -> PathBuf {
        self.data.join("token.json")
    }

    pub fn remote(&self) -> Arc<clouddrop::remote::Remote> {
        clouddrop::remote_for(&self.cfg, self.token_path())
    }

    /// A second database handle, like a separate `clouddrop` CLI process.
    pub fn ctx(&self) -> Ctx {
        Ctx { cfg: self.cfg.clone(), db: Db::open(&self.data.join("state.db")).unwrap() }
    }

    pub fn start(&mut self) {
        self.start_with_store(std::sync::Arc::new(clouddrop::store::PlainStore));
    }

    pub fn start_with_store(&mut self, store: std::sync::Arc<dyn clouddrop::store::LocalStore>) {
        assert!(self.engine.is_none());
        let (tx, rx) = watch::channel(false);
        let opts = clouddrop::engine::RunOptions { config_file: None, store };
        let handle = tokio::spawn(clouddrop::engine::run_with_caps(
            self.cfg.clone(),
            opts,
            self.caps(),
            self.db.clone(),
            self.remote(),
            self.data.clone(),
            rx,
        ));
        self.engine = Some((tx, handle));
    }

    pub async fn stop(&mut self) {
        if let Some((tx, handle)) = self.engine.take() {
            tx.send(true).unwrap();
            handle.await.unwrap().unwrap();
        }
    }

    /// Reopens the database (after deleting it, for example).
    pub fn reopen_db(&mut self) {
        self.db = Arc::new(Db::open(&self.data.join("state.db")).unwrap());
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    pub fn entry(&self, rel: &str) -> Option<Entry> {
        self.db.get(rel).unwrap()
    }

    pub fn state(&self, rel: &str) -> Option<State> {
        self.entry(rel).map(|e| e.state)
    }

    pub fn cloud(&self, rel: &str) -> Option<Item> {
        let st = self.mock.lock().unwrap();
        let root = st.lookup(&self.cfg.drive_folder)?;
        if rel.is_empty() {
            return Some(root);
        }
        st.lookup(&format!("{}/{rel}", self.cfg.drive_folder))
    }

    pub fn cloud_tree(&self) -> Vec<(String, String)> {
        let st = self.mock.lock().unwrap();
        let mut out = Vec::new();
        if let Some(root) = st.lookup(&self.cfg.drive_folder) {
            st.tree(&root.id, "", &mut out);
        }
        out
    }

    pub fn stats(&self) -> (u64, u64) {
        let st = self.mock.lock().unwrap();
        (st.sessions_created, st.folders_created)
    }

    /// True when nothing is waiting or uploading.
    pub fn idle(&self) -> bool {
        self.db
            .subtree("")
            .unwrap()
            .iter()
            .all(|e| !matches!(e.state, State::Pending | State::Uploading) && !e.move_pending)
    }

    pub async fn wait(&self, what: &str, cond: impl Fn(&Harness) -> bool) {
        self.wait_for(what, Duration::from_secs(30), cond).await
    }

    pub async fn wait_for(&self, what: &str, limit: Duration, cond: impl Fn(&Harness) -> bool) {
        let start = Instant::now();
        while !cond(self) {
            if start.elapsed() > limit {
                let rows: Vec<String> = self
                    .db
                    .subtree("")
                    .unwrap()
                    .iter()
                    .map(|e| format!("{} {:?} {:?}", e.path, e.state, e.error))
                    .collect();
                panic!("timed out waiting for {what}\nentries:\n{}\ncloud: {:#?}", rows.join("\n"), self.cloud_tree());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn settle(&self) {
        // Let the watcher deliver events, then wait for the queue to drain.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        self.wait("queue to drain", |h| h.idle()).await;
    }
}

pub fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// Simulates Windows placeholders on Linux: a "dehydrated" file is an empty file the
/// store remembers; `free` turns a file into one.
#[derive(Default)]
pub struct FakeStore {
    pub dehydrated: Mutex<std::collections::HashSet<PathBuf>>,
    pub pinned: Mutex<Vec<PathBuf>>,
    pub unpinned: Mutex<std::collections::HashSet<PathBuf>>,
    /// Content of dehydrated files, for hydrating them again.
    pub saved: Mutex<HashMap<PathBuf, Vec<u8>>>,
    pub synced: Mutex<Vec<(PathBuf, String)>>,
    pub pending: Mutex<Vec<PathBuf>>,
}

impl FakeStore {
    /// Explorer freeing a file itself (what Windows' storage settings can do).
    pub fn dehydrate_externally(&self, abs: &Path) {
        self.save(abs);
        self.dehydrated.lock().unwrap().insert(abs.to_path_buf());
        std::fs::write(abs, b"").unwrap();
    }

    /// Explorer's "Free up space": marks the file; CloudDrop has to free it.
    pub fn unpin_externally(&self, abs: &Path) {
        self.unpinned.lock().unwrap().insert(abs.to_path_buf());
        touch_attributes(abs);
    }

    /// Explorer's "Always keep on this device".
    pub fn pin_externally(&self, abs: &Path) {
        self.pinned.lock().unwrap().push(abs.to_path_buf());
        touch_attributes(abs);
    }

    fn save(&self, abs: &Path) {
        if let Ok(content) = std::fs::read(abs) {
            self.saved.lock().unwrap().insert(abs.to_path_buf(), content);
        }
    }

    /// What opening a placeholder does (Windows downloads it).
    pub fn hydrate_externally(&self, abs: &Path, content: &[u8]) {
        self.dehydrated.lock().unwrap().remove(abs);
        std::fs::write(abs, content).unwrap();
    }
}

/// An attribute-only change, like a pin state change on Windows: briefly
/// read-only, then back.
fn touch_attributes(abs: &Path) {
    let original = std::fs::metadata(abs).unwrap().permissions();
    let mut read_only = original.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(abs, read_only).unwrap();
    std::fs::set_permissions(abs, original).unwrap();
}

impl clouddrop::store::LocalStore for FakeStore {
    fn is_dehydrated(&self, abs: &Path) -> bool {
        abs.exists() && self.dehydrated.lock().unwrap().contains(abs)
    }
    fn is_pinned(&self, abs: &Path) -> bool {
        self.pinned.lock().unwrap().iter().any(|p| abs.starts_with(p))
    }
    fn free(&self, abs: &Path, _drive_id: &str) -> std::io::Result<()> {
        self.save(abs);
        self.unpinned.lock().unwrap().remove(abs);
        self.dehydrated.lock().unwrap().insert(abs.to_path_buf());
        std::fs::write(abs, b"")
    }
    fn free_requested(&self, abs: &Path) -> bool {
        self.unpinned.lock().unwrap().contains(abs) && !self.is_dehydrated(abs)
    }
    fn hydrate(&self, abs: &Path) -> std::io::Result<()> {
        let content = self.saved.lock().unwrap().get(abs).cloned().unwrap_or_default();
        self.dehydrated.lock().unwrap().remove(abs);
        std::fs::write(abs, content)
    }
    fn synced(&self, abs: &Path, drive_id: &str, _is_dir: bool) {
        self.synced.lock().unwrap().push((abs.to_path_buf(), drive_id.to_string()));
    }
    fn pending(&self, abs: &Path) {
        self.pending.lock().unwrap().push(abs.to_path_buf());
    }
}
