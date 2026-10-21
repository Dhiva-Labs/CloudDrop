//! In-process fake of the Microsoft Graph subset the OneDrive client uses: drive items under
//! the app folder (by id and by path), folders, moves and renames, upload sessions, asynchronous
//! copies with a monitor URL, content downloads through a redirect, quota, an OAuth token
//! endpoint, and error injection. Like OneDrive it is case-insensitive, wants fragments in
//! multiples of 320 KiB, and refuses a token sent to upload, monitor or download URLs.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxPath, Query, State as AxState};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use clouddrop::hash::Hasher;
use clouddrop::remote::HashKind;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Ids look like a personal account's: the drive's cid, '!', a number.
pub const CID: &str = "0123456789abcdef";
const UNIT: usize = 320 * 1024;
const MAX_FRAGMENT: usize = 60 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub folder: bool,
    pub content: Vec<u8>,
}

impl Item {
    /// QuickXorHash of the content, in base64 as Graph sends it.
    pub fn quickxor_b64(&self) -> String {
        let mut h = Hasher::new(HashKind::QuickXor);
        h.update(&self.content);
        B64.encode(hex::decode(h.finish().value).unwrap())
    }

    fn json(&self, hashes: bool) -> Value {
        let mut v = json!({ "id": self.id, "name": self.name, "size": self.content.len() });
        if let Some(p) = &self.parent {
            v["parentReference"] = json!({ "driveId": CID, "driveType": "personal", "id": p });
        }
        if self.folder {
            v["folder"] = json!({ "childCount": 0 });
        } else if hashes {
            v["file"] = json!({ "mimeType": "application/octet-stream", "hashes": { "quickXorHash": self.quickxor_b64() } });
        } else {
            v["file"] = json!({ "mimeType": "application/octet-stream" });
        }
        v
    }
}

struct Session {
    /// Replaces this item's content; None = a new file `name` in `parent`.
    item: Option<String>,
    parent: String,
    name: String,
    replace: bool,
    total: Option<usize>,
    received: Vec<u8>,
}

struct CopyJob {
    polls_left: u32,
    fail: bool,
    item: Item,
    done: bool,
}

/// An error to answer the next Graph request with.
#[derive(Clone, Debug)]
pub struct Fail {
    pub status: u16,
    pub code: &'static str,
    pub inner: Option<&'static str>,
    pub retry_after: Option<u64>,
}

impl Fail {
    pub fn new(status: u16, code: &'static str) -> Self {
        Fail { status, code, inner: None, retry_after: None }
    }

    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    pub fn inner(mut self, code: &'static str) -> Self {
        self.inner = Some(code);
        self
    }

    fn response(&self) -> Response {
        let mut err = json!({ "code": self.code, "message": format!("injected {}", self.code) });
        if let Some(inner) = self.inner {
            err["innerError"] = json!({ "code": inner });
        }
        let mut resp = (StatusCode::from_u16(self.status).unwrap(), axum::Json(json!({ "error": err }))).into_response();
        if let Some(s) = self.retry_after {
            resp.headers_mut().insert(header::RETRY_AFTER, s.to_string().parse().unwrap());
        }
        resp
    }
}

pub struct MockState {
    pub items: HashMap<String, Item>,
    sessions: HashMap<String, Session>,
    copies: HashMap<String, CopyJob>,
    tokens: Vec<String>,
    next: u64,
    pub base: String,
    pub approot: String,
    pub quota_total: u64,
    /// Errors for the next Graph requests, in order (after the token check; not for upload,
    /// monitor or download URLs).
    pub fail_next: Vec<Fail>,
    /// Statuses for the next fragment PUTs, before they touch the session.
    pub fail_puts: Vec<u16>,
    /// Leave the hash out of this many upload completions (OneDrive sometimes adds it later).
    pub hashless_uploads: u32,
    /// Times a copy's monitor says "inProgress" before the copy is there.
    pub copy_polls: u32,
    /// The monitor reports the next copies as failed.
    pub fail_copies: bool,
    pub graph_requests: u64,
    pub fragment_puts: u64,
    pub monitor_polls: u64,
    pub downloads: u64,
    pub sessions_created: u64,
    pub refreshes: u64,
}

impl MockState {
    fn new() -> Self {
        let mut st = MockState {
            items: HashMap::new(),
            sessions: HashMap::new(),
            copies: HashMap::new(),
            tokens: Vec::new(),
            next: 0,
            base: String::new(),
            approot: String::new(),
            quota_total: 5 * 1024 * 1024 * 1024,
            fail_next: Vec::new(),
            fail_puts: Vec::new(),
            hashless_uploads: 0,
            copy_polls: 2,
            fail_copies: false,
            graph_requests: 0,
            fragment_puts: 0,
            monitor_polls: 0,
            downloads: 0,
            sessions_created: 0,
            refreshes: 0,
        };
        let root = st.add(None, "root", true, vec![]);
        let apps = st.add(Some(&root), "Apps", true, vec![]);
        st.approot = st.add(Some(&apps), "CloudDrop", true, vec![]);
        st
    }

    fn id(&mut self) -> String {
        self.next += 1;
        format!("{}!{}", CID.to_uppercase(), self.next)
    }

    fn add(&mut self, parent: Option<&str>, name: &str, folder: bool, content: Vec<u8>) -> String {
        let id = self.id();
        let item = Item { id: id.clone(), name: name.into(), parent: parent.map(str::to_string), folder, content };
        self.items.insert(id.clone(), item);
        id
    }

    /// A valid access token, as if signed in.
    pub fn issue_token(&mut self) -> String {
        self.next += 1;
        let t = format!("tok{}", self.next);
        self.tokens.push(t.clone());
        t
    }

    pub fn invalidate_tokens(&mut self) {
        self.tokens.clear();
    }

    /// Forgets every upload session, as OneDrive does when they expire.
    pub fn expire_sessions(&mut self) {
        self.sessions.clear();
    }

    pub fn graph_base(&self) -> String {
        format!("{}/v1.0", self.base)
    }

    pub fn used(&self) -> u64 {
        self.items.values().map(|i| i.content.len() as u64).sum()
    }

    /// Items directly inside `parent`, by name.
    pub fn children(&self, parent: &str) -> Vec<Item> {
        let mut v: Vec<Item> = self.items.values().filter(|i| i.parent.as_deref() == Some(parent)).cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// The item named `name` in `parent`, ignoring case like OneDrive.
    pub fn child(&self, parent: &str, name: &str) -> Option<&Item> {
        let name = name.to_lowercase();
        self.items.values().find(|i| i.parent.as_deref() == Some(parent) && i.name.to_lowercase() == name)
    }

    /// Resolves "a/b/c" from the app folder by names.
    pub fn lookup(&self, path: &str) -> Option<Item> {
        let mut cur = self.items.get(&self.approot)?.clone();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            cur = self.child(&cur.id, part)?.clone();
        }
        Some(cur)
    }

    fn is_folder(&self, id: &str) -> bool {
        self.items.get(id).is_some_and(|i| i.folder)
    }

    /// Checks the token, then answers with an injected failure if one is waiting.
    fn gate(&mut self, h: &HeaderMap) -> Option<Response> {
        self.graph_requests += 1;
        let token = h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if !token.is_some_and(|t| self.tokens.iter().any(|x| x == t)) {
            return Some(error(401, "InvalidAuthenticationToken", "Access token has expired or is not yet valid."));
        }
        if !self.fail_next.is_empty() {
            return Some(self.fail_next.remove(0).response());
        }
        None
    }
}

pub type Shared = Arc<Mutex<MockState>>;

fn error(status: u16, code: &str, message: &str) -> Response {
    let body = json!({ "error": { "code": code, "message": message } });
    (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
}

fn not_found() -> Response {
    error(404, "itemNotFound", "The resource could not be found.")
}

fn name_taken() -> Response {
    error(409, "nameAlreadyExists", "The specified item name already exists.")
}

/// Upload, monitor and download URLs are pre-authenticated; Graph refuses a token there.
fn sends_token(h: &HeaderMap) -> bool {
    h.contains_key(header::AUTHORIZATION)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// What `/items/…` addresses: an item by id, or a name in a folder (`{id}:/{name}`).
enum Target {
    Id(String),
    Child(String, String),
}

/// Splits "ID", "ID/action", "ID:/name", "ID:/name:" and "ID:/name:/action".
fn parse_target(rest: &str) -> (Target, String) {
    if let Some((id, after)) = rest.split_once(":/") {
        let (name, action) = match after.split_once(":/") {
            Some((n, a)) => (n, a),
            None => (after.trim_end_matches(':'), ""),
        };
        return (Target::Child(id.into(), percent_decode(name)), action.into());
    }
    match rest.split_once('/') {
        Some((id, action)) => (Target::Id(id.into()), action.into()),
        None => (Target::Id(rest.into()), String::new()),
    }
}

async fn token(AxState(s): AxState<Shared>, body: String) -> Response {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    let mut st = s.lock().unwrap();
    match form.get("grant_type").map(String::as_str) {
        Some("refresh_token") => {
            st.refreshes += 1;
            if form.get("refresh_token").map(String::as_str) != Some("good-refresh") {
                return (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant"}))).into_response();
            }
            let tok = st.issue_token();
            axum::Json(json!({"access_token": tok, "expires_in": 3600, "refresh_token": "good-refresh"})).into_response()
        }
        Some("authorization_code") => {
            let tok = st.issue_token();
            axum::Json(json!({"access_token": tok, "expires_in": 3600, "refresh_token": "good-refresh"})).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn drive(AxState(s): AxState<Shared>, h: HeaderMap) -> Response {
    let mut st = s.lock().unwrap();
    if let Some(r) = st.gate(&h) {
        return r;
    }
    let (total, used) = (st.quota_total, st.used());
    axum::Json(json!({
        "id": CID, "driveType": "personal",
        "quota": { "total": total, "used": used, "remaining": total.saturating_sub(used), "deleted": 0, "state": "normal" }
    }))
    .into_response()
}

async fn approot(AxState(s): AxState<Shared>, h: HeaderMap) -> Response {
    let mut st = s.lock().unwrap();
    if let Some(r) = st.gate(&h) {
        return r;
    }
    axum::Json(st.items[&st.approot].json(true)).into_response()
}

async fn items(
    AxState(s): AxState<Shared>,
    method: Method,
    uri: Uri,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let mut st = s.lock().unwrap();
    if let Some(r) = st.gate(&h) {
        return r;
    }
    let rest = uri.path().strip_prefix("/v1.0/me/drive/items/").unwrap_or_default();
    let (target, action) = parse_target(rest);
    let json_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match (&method, action.as_str(), target) {
        (&Method::GET, "", Target::Id(id)) => match st.items.get(&id) {
            Some(i) => axum::Json(i.json(true)).into_response(),
            None => not_found(),
        },
        (&Method::GET, "", Target::Child(parent, name)) => match st.child(&parent, &name) {
            Some(i) => axum::Json(i.json(true)).into_response(),
            None => not_found(),
        },
        (&Method::PATCH, "", Target::Id(id)) => update(&mut st, &id, &json_body),
        (&Method::POST, "children", Target::Id(parent)) => create_folder(&mut st, &parent, &json_body),
        (&Method::POST, "createUploadSession", target) => create_session(&mut st, target, &json_body),
        (&Method::POST, "copy", Target::Id(id)) => copy(&mut st, &id, &json_body),
        (&Method::GET, "content", Target::Id(id)) => match st.items.get(&id) {
            Some(i) if !i.folder => {
                let to = format!("{}/download/{id}?sig=preauth", st.base);
                (StatusCode::FOUND, [(header::LOCATION, to)]).into_response()
            }
            _ => not_found(),
        },
        (&Method::PUT, "content", target) => put_content(&mut st, target, &q, body.to_vec()),
        _ => error(400, "invalidRequest", "The mock does not know this request."),
    }
}

fn create_folder(st: &mut MockState, parent: &str, body: &Value) -> Response {
    if !st.is_folder(parent) {
        return not_found();
    }
    let name = body["name"].as_str().unwrap_or_default().to_string();
    if body["folder"].is_null() || name.is_empty() {
        return error(400, "invalidRequest", "Only folders are created this way.");
    }
    if st.child(parent, &name).is_some() {
        return name_taken();
    }
    let id = st.add(Some(parent), &name, true, vec![]);
    (StatusCode::CREATED, axum::Json(st.items[&id].json(true))).into_response()
}

fn update(st: &mut MockState, id: &str, body: &Value) -> Response {
    let Some(item) = st.items.get(id).cloned() else {
        return not_found();
    };
    let parent = body["parentReference"]["id"].as_str().map(str::to_string).or(item.parent.clone());
    let name = body["name"].as_str().unwrap_or(&item.name).to_string();
    let Some(parent) = parent.filter(|p| st.is_folder(p)) else {
        return not_found();
    };
    if st.child(&parent, &name).is_some_and(|other| other.id != id) {
        return name_taken();
    }
    let item = st.items.get_mut(id).unwrap();
    item.name = name;
    item.parent = Some(parent);
    axum::Json(item.json(true)).into_response()
}

fn create_session(st: &mut MockState, target: Target, body: &Value) -> Response {
    // Real OneDrive (personal accounts, 2026) refuses `fileSize` here, although the docs list it.
    if body["item"].get("fileSize").is_some() {
        return error(400, "invalidRequest", "Invalid request");
    }
    let replace = body["item"]["@microsoft.graph.conflictBehavior"].as_str() == Some("replace");
    let (item, parent, name) = match target {
        Target::Id(id) => match st.items.get(&id) {
            Some(i) if !i.folder => (Some(id), i.parent.clone().unwrap(), i.name.clone()),
            _ => return not_found(),
        },
        Target::Child(parent, name) => {
            if !st.is_folder(&parent) {
                return not_found();
            }
            match st.child(&parent, &name) {
                Some(_) if !replace => return name_taken(),
                Some(existing) => (Some(existing.id.clone()), parent, existing.name.clone()),
                None => (None, parent, name),
            }
        }
    };
    let total = None;
    let sid = st.id().replace('!', "-");
    st.sessions_created += 1;
    st.sessions.insert(sid.clone(), Session { item, parent, name, replace, total, received: vec![] });
    axum::Json(json!({
        "uploadUrl": format!("{}/upload/{sid}", st.base),
        "expirationDateTime": "2099-01-01T00:00:00Z",
        "nextExpectedRanges": ["0-"],
    }))
    .into_response()
}

fn progress(received: usize) -> Value {
    json!({ "expirationDateTime": "2099-01-01T00:00:00Z", "nextExpectedRanges": [format!("{received}-")] })
}

async fn upload(AxState(s): AxState<Shared>, method: Method, h: HeaderMap, AxPath(sid): AxPath<String>, body: Bytes) -> Response {
    let mut st = s.lock().unwrap();
    if sends_token(&h) {
        return error(401, "unauthenticated", "Do not send a token to the upload URL.");
    }
    if method == Method::PUT {
        st.fragment_puts += 1;
        if !st.fail_puts.is_empty() {
            let code = st.fail_puts.remove(0);
            return error(code, "injected", "injected fragment failure");
        }
    }
    let Some(sess) = st.sessions.get_mut(&sid) else {
        return not_found();
    };
    match method {
        Method::GET => return axum::Json(progress(sess.received.len())).into_response(),
        Method::DELETE => {
            st.sessions.remove(&sid);
            return StatusCode::NO_CONTENT.into_response();
        }
        Method::PUT => {}
        _ => return error(405, "invalidRequest", "PUT fragments here."),
    }
    let range = h.get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let Some((a, b, total)) = range
        .strip_prefix("bytes ")
        .and_then(|r| r.split_once('/'))
        .and_then(|(span, total)| Some((span.split_once('-')?, total.parse::<usize>().ok()?)))
        .and_then(|((a, b), total)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?, total)))
    else {
        return error(400, "invalidRange", "Content-Range must be bytes a-b/total.");
    };
    if sess.total.is_some_and(|t| t != total) {
        return error(400, "invalidRange", "The total size changed.");
    }
    sess.total = Some(total);
    // A file that won't fit is refused when its data arrives.
    let used = st.used();
    if used + total as u64 > st.quota_total {
        return error(507, "quotaLimitReached", "Insufficient Space Available");
    }
    let Some(sess) = st.sessions.get_mut(&sid) else {
        return not_found();
    };
    if a != sess.received.len() {
        return error(416, "invalidRange", "Fragment overlaps or skips what was received.");
    }
    if b < a || b + 1 - a != body.len() {
        return error(400, "invalidRange", "Content-Range does not match the body.");
    }
    if body.len() >= MAX_FRAGMENT {
        return error(413, "requestTooLarge", "Fragments must be under 60 MiB.");
    }
    if b + 1 < total && !body.len().is_multiple_of(UNIT) {
        return error(400, "invalidRange", "Fragments must be multiples of 320 KiB.");
    }
    sess.received.extend_from_slice(&body);
    if sess.received.len() < total {
        return (StatusCode::ACCEPTED, axum::Json(progress(sess.received.len()))).into_response();
    }
    // Complete: the session is gone from now on, and the file is there.
    let sess = st.sessions.remove(&sid).unwrap();
    let hashes = if st.hashless_uploads > 0 {
        st.hashless_uploads -= 1;
        false
    } else {
        true
    };
    match sess.item {
        Some(id) => {
            let item = st.items.get_mut(&id).unwrap();
            item.content = sess.received;
            (StatusCode::OK, axum::Json(item.json(hashes))).into_response()
        }
        None => {
            if !sess.replace && st.child(&sess.parent, &sess.name).is_some() {
                return name_taken();
            }
            let id = st.add(Some(&sess.parent), &sess.name, false, sess.received);
            (StatusCode::CREATED, axum::Json(st.items[&id].json(hashes))).into_response()
        }
    }
}

/// Simple upload (`PUT …/content`), which CloudDrop uses for empty files.
fn put_content(st: &mut MockState, target: Target, q: &HashMap<String, String>, content: Vec<u8>) -> Response {
    match target {
        Target::Id(id) => match st.items.get_mut(&id) {
            Some(i) if !i.folder => {
                i.content = content;
                axum::Json(i.json(true)).into_response()
            }
            _ => not_found(),
        },
        Target::Child(parent, name) => {
            if !st.is_folder(&parent) {
                return not_found();
            }
            let replace = q.get("@microsoft.graph.conflictBehavior").map(String::as_str) == Some("replace");
            if let Some(existing) = st.child(&parent, &name).map(|i| i.id.clone()) {
                if !replace {
                    return name_taken();
                }
                let i = st.items.get_mut(&existing).unwrap();
                i.content = content;
                return axum::Json(i.json(true)).into_response();
            }
            let id = st.add(Some(&parent), &name, false, content);
            (StatusCode::CREATED, axum::Json(st.items[&id].json(true))).into_response()
        }
    }
}

/// Starts a copy that shows up after `copy_polls` polls of its monitor URL.
fn copy(st: &mut MockState, id: &str, body: &Value) -> Response {
    let Some(src) = st.items.get(id).cloned() else {
        return not_found();
    };
    let parent = body["parentReference"]["id"].as_str().map(str::to_string).or(src.parent.clone()).unwrap();
    let name = body["name"].as_str().unwrap_or(&src.name).to_string();
    if !st.is_folder(&parent) {
        return not_found();
    }
    if st.child(&parent, &name).is_some() {
        return name_taken();
    }
    let new_id = st.id();
    let mid = new_id.replace('!', "-");
    let item = Item { id: new_id, name, parent: Some(parent), folder: false, content: src.content };
    let (polls_left, fail) = (st.copy_polls, st.fail_copies);
    st.copies.insert(mid.clone(), CopyJob { polls_left, fail, item, done: false });
    (StatusCode::ACCEPTED, [(header::LOCATION, format!("{}/monitor/{mid}", st.base))]).into_response()
}

async fn monitor(AxState(s): AxState<Shared>, h: HeaderMap, AxPath(mid): AxPath<String>) -> Response {
    let mut st = s.lock().unwrap();
    if sends_token(&h) {
        return error(401, "unauthenticated", "Do not send a token to the monitor URL.");
    }
    st.monitor_polls += 1;
    let Some(c) = st.copies.get_mut(&mid) else {
        return not_found();
    };
    if c.polls_left > 0 {
        c.polls_left -= 1;
        return (StatusCode::ACCEPTED, axum::Json(json!({ "status": "inProgress", "percentageComplete": 50.0 }))).into_response();
    }
    if c.fail {
        return axum::Json(json!({ "status": "failed", "error": { "code": "generalException", "message": "copy went wrong" } }))
            .into_response();
    }
    let item = c.item.clone();
    if !c.done {
        c.done = true;
        st.items.insert(item.id.clone(), item.clone());
    }
    axum::Json(json!({ "status": "completed", "percentageComplete": 100.0, "resourceId": item.id })).into_response()
}

/// The pre-authenticated download URL `…/content` redirects to.
async fn download(AxState(s): AxState<Shared>, h: HeaderMap, AxPath(id): AxPath<String>) -> Response {
    let mut st = s.lock().unwrap();
    if sends_token(&h) {
        return error(401, "unauthenticated", "Do not send a token to the download URL.");
    }
    st.downloads += 1;
    let Some(item) = st.items.get(&id) else {
        return not_found();
    };
    let len = item.content.len();
    let range = h
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
    match range {
        Some((a, b)) if a < len => {
            let b = b.min(len - 1);
            let part = item.content[a..=b].to_vec();
            (StatusCode::PARTIAL_CONTENT, [(header::CONTENT_RANGE, format!("bytes {a}-{b}/{len}"))], part).into_response()
        }
        Some(_) => StatusCode::RANGE_NOT_SATISFIABLE.into_response(),
        None => item.content.clone().into_response(),
    }
}

/// Starts the fake on a free port. The Graph base for clients is [`MockState::graph_base`].
pub async fn start() -> Shared {
    let shared: Shared = Arc::new(Mutex::new(MockState::new()));
    let app = Router::new()
        .route("/token", post(token))
        .route("/v1.0/me/drive", get(drive))
        .route("/v1.0/me/drive/special/approot", get(approot))
        .route("/v1.0/me/drive/items/{*rest}", any(items))
        .route("/upload/{sid}", any(upload))
        .route("/monitor/{mid}", get(monitor))
        .route("/download/{id}", get(download))
        // Fragments go up to 60 MiB, past axum's default limit; the handler checks the size.
        .layer(DefaultBodyLimit::disable())
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    shared.lock().unwrap().base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    shared
}
