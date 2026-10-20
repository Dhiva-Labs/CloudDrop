//! In-process fake of the Dropbox API v2 subset CloudDrop uses, on two hosts like the real
//! thing: the API host (RPC calls, OAuth) and the content host (uploads, downloads). Names are
//! case-insensitive, ids are `id:…`, an endpoint's own errors are 409s with an `error_summary`,
//! and faults can be injected per route.
//!
//! It uses nothing from CloudDrop, so `src/dropbox.rs` includes it for its unit tests too.

#![allow(dead_code)]

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Dropbox hashes content in blocks of this size.
pub const BLOCK: usize = 4 * 1024 * 1024;

/// Dropbox's content hash, computed here independently of the client's.
pub fn content_hash(data: &[u8]) -> String {
    let mut all = Sha256::new();
    for block in data.chunks(BLOCK) {
        all.update(Sha256::digest(block));
    }
    hex::encode(all.finalize())
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub name: String,
    /// The folder it is in; None = the app folder itself.
    pub parent: Option<String>,
    pub folder: bool,
    pub content: Vec<u8>,
    pub rev: String,
    pub deleted: bool,
}

/// Done to the next call of a route instead of (or after) answering it.
#[derive(Clone, Debug, PartialEq)]
pub enum Fault {
    /// Answers with this status and a text body, without doing anything.
    Status(u16),
    /// 429 with `Retry-After` in seconds.
    RateLimited(u64),
    /// Does the work, then fails as if the connection broke before the reply arrived.
    LostReply,
}

pub struct MockState {
    pub items: HashMap<String, Item>,
    /// Upload sessions: what each has received.
    pub sessions: HashMap<String, Vec<u8>>,
    tokens: Vec<String>,
    expired: Vec<String>,
    next: u64,
    /// The refresh token the token endpoint accepts.
    pub refresh_token: String,
    pub refreshes: u64,
    pub folders_created: u64,
    /// Every call by route (`files/get_metadata`, `oauth2/token`, …), in order.
    pub calls: Vec<String>,
    /// Faults for coming calls of a route, used in order.
    pub faults: Vec<(String, Fault)>,
    /// The account's storage, in bytes.
    pub allocated: u64,
    /// Base URLs of the two hosts.
    pub api: String,
    pub content: String,
}

impl Default for MockState {
    fn default() -> Self {
        MockState {
            items: HashMap::new(),
            sessions: HashMap::new(),
            tokens: Vec::new(),
            expired: Vec::new(),
            next: 0,
            refresh_token: "good-refresh".into(),
            refreshes: 0,
            folders_created: 0,
            calls: Vec::new(),
            faults: Vec::new(),
            allocated: 2 << 30,
            api: String::new(),
            content: String::new(),
        }
    }
}

/// What a path argument names.
enum Found {
    Root,
    Item(String),
    Missing,
    Malformed,
}

impl MockState {
    fn counter(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    pub fn issue_token(&mut self) -> String {
        let t = format!("tok{}", self.counter());
        self.tokens.push(t.clone());
        t
    }

    /// Every access token issued so far stops working (`expired_access_token`).
    pub fn expire_tokens(&mut self) {
        let old = std::mem::take(&mut self.tokens);
        self.expired.extend(old);
    }

    /// Dropbox forgets every upload session.
    pub fn expire_sessions(&mut self) {
        self.sessions.clear();
    }

    pub fn fail(&mut self, route: &str, fault: Fault) {
        self.faults.push((route.to_string(), fault));
    }

    /// How many calls of `route` were made.
    pub fn count(&self, route: &str) -> usize {
        self.calls.iter().filter(|c| *c == route).count()
    }

    pub fn used(&self) -> u64 {
        self.items.values().filter(|i| !i.deleted).map(|i| i.content.len() as u64).sum()
    }

    pub fn children(&self, parent: Option<&str>) -> Vec<&Item> {
        let mut v: Vec<&Item> = self
            .items
            .values()
            .filter(|i| !i.deleted && i.parent.as_deref() == parent)
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// The item called `name` in `parent`, whatever the case.
    fn find(&self, parent: Option<&str>, name: &str) -> Option<&Item> {
        let lower = name.to_lowercase();
        self.children(parent).into_iter().find(|i| i.name.to_lowercase() == lower)
    }

    /// "/Folder/name" of a live item.
    pub fn path_of(&self, id: &str) -> String {
        let mut parts = Vec::new();
        let mut cur = self.items.get(id);
        while let Some(i) = cur {
            parts.push(i.name.clone());
            cur = i.parent.as_deref().and_then(|p| self.items.get(p));
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn find_path(&self, path: &str) -> Found {
        if path.is_empty() {
            return Found::Root;
        }
        if path.starts_with("id:") {
            return match self.items.get(path) {
                Some(i) if !i.deleted => Found::Item(path.to_string()),
                _ => Found::Missing,
            };
        }
        let Some(rest) = path.strip_prefix('/') else {
            return Found::Malformed;
        };
        if rest.split('/').any(|p| p.is_empty() || p.contains(['\\', '\u{7f}'])) {
            return Found::Malformed;
        }
        let mut parent: Option<String> = None;
        for part in rest.split('/') {
            match self.find(parent.as_deref(), part) {
                Some(i) => parent = Some(i.id.clone()),
                None => return Found::Missing,
            }
        }
        parent.map_or(Found::Missing, Found::Item)
    }

    /// The live item at a path (case-insensitive) or with an id.
    pub fn lookup(&self, path: &str) -> Option<Item> {
        match self.find_path(path) {
            Found::Item(id) => self.items.get(&id).cloned(),
            _ => None,
        }
    }

    /// Every live item as ("/path", content); folders as ("/path/", "").
    pub fn tree(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .items
            .values()
            .filter(|i| !i.deleted)
            .map(|i| {
                let p = self.path_of(&i.id);
                if i.folder {
                    (format!("{p}/"), String::new())
                } else {
                    (p, String::from_utf8_lossy(&i.content).to_string())
                }
            })
            .collect();
        out.sort();
        out
    }

    /// Someone deletes `path` (and what is in it) on dropbox.com.
    pub fn delete(&mut self, path: &str) {
        let Found::Item(id) = self.find_path(path) else { return };
        self.items.get_mut(&id).unwrap().deleted = true;
        loop {
            let inside: Vec<String> = self
                .items
                .values()
                .filter(|i| !i.deleted && i.parent.as_ref().is_some_and(|p| self.items[p].deleted))
                .map(|i| i.id.clone())
                .collect();
            if inside.is_empty() {
                return;
            }
            for id in inside {
                self.items.get_mut(&id).unwrap().deleted = true;
            }
        }
    }

    fn add(&mut self, parent: Option<String>, name: &str, folder: bool, content: Vec<u8>) -> String {
        let id = format!("id:mock{}", self.counter());
        let rev = format!("{:012x}", self.counter());
        if folder {
            self.folders_created += 1;
        }
        let item = Item { id: id.clone(), name: name.to_string(), parent, folder, content, rev, deleted: false };
        self.items.insert(id.clone(), item);
        id
    }

    /// Puts a file at "/a/b/name" directly, making folders on the way; returns its id.
    pub fn put_file(&mut self, path: &str, content: &[u8]) -> String {
        let (dir, name) = split(path).expect("a path like /a/b.txt");
        let parent = self.ensure_dirs(dir).unwrap_or_else(|_| panic!("a file is in the way of {path}"));
        self.add(parent, name, false, content.to_vec())
    }

    /// Folders along `dir` ("" or "/a/b"), made where missing, as Dropbox does for writes.
    fn ensure_dirs(&mut self, dir: &str) -> Result<Option<String>, Box<Response>> {
        let mut parent: Option<String> = None;
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            parent = Some(match self.find(parent.as_deref(), part) {
                Some(i) if i.folder => i.id.clone(),
                Some(_) => return Err(Box::new(conflict("path", "file"))),
                None => self.add(parent, part, true, Vec::new()),
            });
        }
        Ok(parent)
    }

    /// "name (1).ext", "name (2).ext", … whichever is free in `parent`.
    fn free_name(&self, parent: Option<&str>, name: &str) -> String {
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => name.split_at(i),
            _ => (name, ""),
        };
        (1..)
            .map(|n| format!("{stem} ({n}){ext}"))
            .find(|n| self.find(parent, n).is_none())
            .unwrap()
    }

    /// Metadata as Dropbox sends it; `tagged` adds ".tag" (where the API returns the `Metadata` union).
    fn meta(&self, id: &str, tagged: bool) -> Value {
        let i = &self.items[id];
        let display = self.path_of(id);
        let mut v = json!({
            "name": i.name, "id": i.id, "path_lower": display.to_lowercase(), "path_display": display,
        });
        if !i.folder {
            v["rev"] = json!(i.rev);
            v["size"] = json!(i.content.len());
            v["content_hash"] = json!(content_hash(&i.content));
            v["client_modified"] = json!("2026-10-21T08:00:00Z");
            v["server_modified"] = json!("2026-10-21T08:00:00Z");
            v["is_downloadable"] = json!(true);
        }
        if tagged {
            v[".tag"] = json!(if i.folder { "folder" } else { "file" });
        }
        v
    }

    fn check_token(&self, h: &HeaderMap) -> Option<Response> {
        let token = h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if self.tokens.iter().any(|t| t == token) {
            return None;
        }
        let tag = if self.expired.iter().any(|t| t == token) { "expired_access_token" } else { "invalid_access_token" };
        let body = json!({"error_summary": format!("{tag}/.."), "error": {".tag": tag}});
        Some((StatusCode::UNAUTHORIZED, axum::Json(body)).into_response())
    }
}

/// "/a/b/name" → ("/a/b", "name"); "/name" → ("", "name").
fn split(path: &str) -> Option<(&str, &str)> {
    let i = path.rfind('/')?;
    let (dir, name) = (&path[..i], &path[i + 1..]);
    if path.starts_with('/') && !name.is_empty() { Some((dir, name)) } else { None }
}

pub type Shared = Arc<Mutex<MockState>>;

/// An endpoint's own error: 409 with a summary like "path/not_found/..".
fn endpoint_error(summary: &str, error: Value) -> Response {
    let body = json!({"error_summary": format!("{summary}/.."), "error": error});
    (StatusCode::CONFLICT, axum::Json(body)).into_response()
}

fn not_found(field: &str) -> Response {
    endpoint_error(&format!("{field}/not_found"), json!({".tag": field, field: {".tag": "not_found"}}))
}

fn conflict(field: &str, kind: &str) -> Response {
    endpoint_error(
        &format!("{field}/conflict/{kind}"),
        json!({".tag": field, field: {".tag": "conflict", "conflict": {".tag": kind}}}),
    )
}

fn bad_request(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, msg.to_string()).into_response()
}

fn kind(i: &Item) -> &'static str {
    if i.folder { "folder" } else { "file" }
}

/// JSON safe for an HTTP header: non-ASCII as \u escapes (what Dropbox does in Dropbox-API-Result).
fn header_json(v: &Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for u in c.encode_utf16(&mut [0u16; 2]) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Host {
    Api,
    Content,
}

type Handler = fn(&mut MockState, &Value, Bytes, &HeaderMap) -> Response;

/// One endpoint: checks the token, applies faults, reads the argument (JSON body on the API
/// host, `Dropbox-API-Arg` on the content host), then runs `f`.
fn route(name: &'static str, host: Host, f: Handler) -> MethodRouter<Shared> {
    post(move |State(s): State<Shared>, h: HeaderMap, body: Bytes| async move {
        dispatch(&s, name, host, &h, body, f)
    })
}

fn dispatch(s: &Shared, name: &str, host: Host, h: &HeaderMap, body: Bytes, f: Handler) -> Response {
    let mut st = s.lock().unwrap();
    st.calls.push(name.to_string());
    if let Some(denied) = st.check_token(h) {
        return denied;
    }
    let fault = st.faults.iter().position(|(r, _)| r == name).map(|i| st.faults.remove(i).1);
    match fault {
        Some(Fault::Status(code)) => {
            return (StatusCode::from_u16(code).unwrap(), "injected failure").into_response();
        }
        Some(Fault::RateLimited(secs)) => {
            let body = json!({
                "error_summary": "too_many_requests/..",
                "error": {"reason": {".tag": "too_many_requests"}, "retry_after": secs},
            });
            return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, secs.to_string())], axum::Json(body))
                .into_response();
        }
        _ => {}
    }
    let content_type = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let (arg, body) = match host {
        Host::Api => {
            if content_type != "application/json" {
                return bad_request("Bad HTTP \"Content-Type\" header; expecting \"application/json\"");
            }
            match serde_json::from_slice(&body) {
                Ok(v) => (v, Bytes::new()),
                Err(_) => return bad_request("request body: could not decode input as JSON"),
            }
        }
        Host::Content => {
            if name.starts_with("files/upload_session/") && content_type != "application/octet-stream" {
                return bad_request("Bad HTTP \"Content-Type\" header; expecting \"application/octet-stream\"");
            }
            let Some(raw) = h.get("Dropbox-API-Arg") else {
                return bad_request("Missing Dropbox-API-Arg header");
            };
            if raw.as_bytes().iter().any(|b| !b.is_ascii() || *b == 0x7f) {
                return bad_request("Dropbox-API-Arg must be ASCII; escape everything else as \\uXXXX");
            }
            match serde_json::from_slice(raw.as_bytes()) {
                Ok(v) => (v, body),
                Err(_) => return bad_request("Dropbox-API-Arg: could not decode input as JSON"),
            }
        }
    };
    let resp = f(&mut st, &arg, body, h);
    if fault == Some(Fault::LostReply) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "connection reset").into_response();
    }
    resp
}

fn space_usage(st: &mut MockState, _: &Value, _: Bytes, _: &HeaderMap) -> Response {
    axum::Json(json!({
        "used": st.used(),
        "allocation": {".tag": "individual", "allocated": st.allocated},
    }))
    .into_response()
}

fn get_metadata(st: &mut MockState, arg: &Value, _: Bytes, _: &HeaderMap) -> Response {
    match st.find_path(arg["path"].as_str().unwrap_or("")) {
        Found::Root => bad_request("request body: path: the root folder is unsupported"),
        Found::Malformed => bad_request("request body: path: did not match pattern"),
        Found::Missing => not_found("path"),
        Found::Item(id) => axum::Json(st.meta(&id, true)).into_response(),
    }
}

fn create_folder(st: &mut MockState, arg: &Value, _: Bytes, _: &HeaderMap) -> Response {
    let path = arg["path"].as_str().unwrap_or("");
    let Some((dir, name)) = split(path).filter(|_| !matches!(st.find_path(path), Found::Malformed)) else {
        return bad_request("request body: path: did not match pattern");
    };
    let parent = match st.ensure_dirs(dir) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };
    let mut name = name.to_string();
    if let Some(existing) = st.find(parent.as_deref(), &name) {
        if arg["autorename"].as_bool() != Some(true) {
            return conflict("path", kind(existing));
        }
        name = st.free_name(parent.as_deref(), &name);
    }
    let id = st.add(parent, &name, true, Vec::new());
    axum::Json(json!({"metadata": st.meta(&id, false)})).into_response()
}

/// move_v2 and copy_v2: (source id, destination folder, destination name), or the error.
fn relocation(st: &mut MockState, arg: &Value) -> Result<(String, Option<String>, String), Box<Response>> {
    let id = match st.find_path(arg["from_path"].as_str().unwrap_or("")) {
        Found::Item(id) => id,
        Found::Missing => return Err(Box::new(not_found("from_lookup"))),
        Found::Root | Found::Malformed => {
            return Err(Box::new(bad_request("request body: from_path: did not match pattern")));
        }
    };
    let to = arg["to_path"].as_str().unwrap_or("");
    let Some((dir, name)) = split(to).filter(|_| !matches!(st.find_path(to), Found::Malformed)) else {
        return Err(Box::new(bad_request("request body: to_path: did not match pattern")));
    };
    let parent = st.ensure_dirs(dir).map_err(|_| Box::new(conflict("to", "file")))?;
    let mut name = name.to_string();
    // The item itself counts: Dropbox does not rename by case alone.
    if let Some(existing) = st.find(parent.as_deref(), &name) {
        if arg["autorename"].as_bool() != Some(true) {
            return Err(Box::new(conflict("to", kind(existing))));
        }
        name = st.free_name(parent.as_deref(), &name);
    }
    Ok((id, parent, name))
}

fn move_item(st: &mut MockState, arg: &Value, _: Bytes, _: &HeaderMap) -> Response {
    let (id, parent, name) = match relocation(st, arg) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let mut up = parent.clone();
    while let Some(p) = up {
        if p == id {
            return endpoint_error("cant_move_folder_into_itself", json!({".tag": "cant_move_folder_into_itself"}));
        }
        up = st.items[&p].parent.clone();
    }
    let item = st.items.get_mut(&id).unwrap();
    item.parent = parent;
    item.name = name;
    axum::Json(json!({"metadata": st.meta(&id, true)})).into_response()
}

fn copy_item(st: &mut MockState, arg: &Value, _: Bytes, _: &HeaderMap) -> Response {
    let (id, parent, name) = match relocation(st, arg) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let source = st.items[&id].clone();
    if source.folder {
        return bad_request("the mock copies files only");
    }
    if st.used() + source.content.len() as u64 > st.allocated {
        return endpoint_error("insufficient_quota", json!({".tag": "insufficient_quota"}));
    }
    let copy = st.add(parent, &name, false, source.content);
    axum::Json(json!({"metadata": st.meta(&copy, true)})).into_response()
}

fn session_start(st: &mut MockState, _: &Value, body: Bytes, _: &HeaderMap) -> Response {
    let sid = format!("sess{}", st.counter());
    st.sessions.insert(sid.clone(), body.to_vec());
    axum::Json(json!({"session_id": sid})).into_response()
}

/// The session a cursor names, if it is at the cursor's offset; else the lookup error.
fn lookup<'a>(st: &'a mut MockState, cursor: &Value) -> Result<&'a mut Vec<u8>, Value> {
    let sid = cursor["session_id"].as_str().unwrap_or("");
    let Some(data) = st.sessions.get_mut(sid) else {
        return Err(json!({".tag": "not_found"}));
    };
    if cursor["offset"].as_u64() != Some(data.len() as u64) {
        return Err(json!({".tag": "incorrect_offset", "correct_offset": data.len()}));
    }
    Ok(data)
}

fn session_append(st: &mut MockState, arg: &Value, body: Bytes, _: &HeaderMap) -> Response {
    match lookup(st, &arg["cursor"]) {
        Ok(data) => {
            data.extend_from_slice(&body);
            axum::Json(Value::Null).into_response()
        }
        Err(e) => {
            let tag = e[".tag"].as_str().unwrap().to_string();
            endpoint_error(&tag, e)
        }
    }
}

fn session_finish(st: &mut MockState, arg: &Value, body: Bytes, _: &HeaderMap) -> Response {
    let mut data = match lookup(st, &arg["cursor"]) {
        Ok(data) => data.clone(),
        Err(e) => {
            let summary = format!("lookup_failed/{}", e[".tag"].as_str().unwrap());
            return endpoint_error(&summary, json!({".tag": "lookup_failed", "lookup_failed": e}));
        }
    };
    data.extend_from_slice(&body);
    let commit = &arg["commit"];
    let path = commit["path"].as_str().unwrap_or("");
    let mode = commit["mode"].as_str().or(commit["mode"][".tag"].as_str()).unwrap_or("add");
    let autorename = commit["autorename"].as_bool() == Some(true);
    // Where it goes: an existing file, or a new one in `parent`.
    let (existing, parent, name) = match st.find_path(path) {
        Found::Item(id) if st.items[&id].folder => return conflict("path", "folder"),
        Found::Item(id) => {
            let (parent, name) = (st.items[&id].parent.clone(), st.items[&id].name.clone());
            (Some(id), parent, name)
        }
        // Stands in for whatever Dropbox says about the id of a deleted file.
        Found::Missing if path.starts_with("id:") => {
            return endpoint_error("path/malformed_path", json!({".tag": "path", "path": {".tag": "malformed_path"}}));
        }
        Found::Missing => {
            let (dir, name) = split(path).unwrap();
            match st.ensure_dirs(dir) {
                Ok(parent) => (None, parent, name.to_string()),
                Err(resp) => return *resp,
            }
        }
        Found::Root | Found::Malformed => return bad_request("request body: commit.path: did not match pattern"),
    };
    let replacing = existing.as_ref().map_or(0, |id| st.items[id].content.len() as u64);
    if st.used() - replacing + data.len() as u64 > st.allocated {
        return endpoint_error("path/insufficient_space", json!({".tag": "path", "path": {".tag": "insufficient_space"}}));
    }
    let sid = arg["cursor"]["session_id"].as_str().unwrap().to_string();
    st.sessions.remove(&sid);
    let id = match existing {
        Some(id) => {
            let i = &st.items[&id];
            let may_replace = mode == "overwrite" || (mode == "update" && commit["mode"]["update"] == json!(i.rev));
            if i.content == data {
                id
            } else if may_replace {
                let rev = format!("{:012x}", st.counter());
                let i = st.items.get_mut(&id).unwrap();
                i.content = data;
                i.rev = rev;
                id
            } else if autorename {
                let name = st.free_name(parent.as_deref(), &name);
                st.add(parent, &name, false, data)
            } else {
                return conflict("path", "file");
            }
        }
        None => st.add(parent, &name, false, data),
    };
    axum::Json(st.meta(&id, false)).into_response()
}

fn download(st: &mut MockState, arg: &Value, _: Bytes, h: &HeaderMap) -> Response {
    let id = match st.find_path(arg["path"].as_str().unwrap_or("")) {
        Found::Item(id) => id,
        Found::Missing => return not_found("path"),
        Found::Root | Found::Malformed => return bad_request("request body: path: did not match pattern"),
    };
    if st.items[&id].folder {
        return endpoint_error("path/not_file", json!({".tag": "path", "path": {".tag": "not_file"}}));
    }
    let content = st.items[&id].content.clone();
    let result = header_json(&st.meta(&id, false));
    let range = h
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'));
    let Some((a, b)) = range else {
        return (StatusCode::OK, [("Dropbox-API-Result", result)], content).into_response();
    };
    let len = content.len();
    let start: usize = a.parse().unwrap_or(0);
    let end = b.parse::<usize>().map_or(len, |b| (b + 1).min(len));
    if start >= end {
        return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    }
    let content_range = format!("bytes {start}-{}/{len}", end - 1);
    (
        StatusCode::PARTIAL_CONTENT,
        [("Dropbox-API-Result", result), (header::CONTENT_RANGE.as_str(), content_range)],
        content[start..end].to_vec(),
    )
        .into_response()
}

/// The OAuth token endpoint: refresh tokens and authorisation codes.
async fn token(State(s): State<Shared>, body: String) -> Response {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    let mut st = s.lock().unwrap();
    st.calls.push("oauth2/token".into());
    match form.get("grant_type").map(String::as_str) {
        Some("refresh_token") => {
            st.refreshes += 1;
            if form.get("refresh_token") != Some(&st.refresh_token) {
                let body = json!({"error": "invalid_grant", "error_description": "refresh token is invalid or revoked"});
                return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
            }
            let tok = st.issue_token();
            axum::Json(json!({"access_token": tok, "token_type": "bearer", "expires_in": 14400})).into_response()
        }
        Some("authorization_code") => {
            let tok = st.issue_token();
            axum::Json(json!({
                "access_token": tok, "token_type": "bearer", "expires_in": 14400,
                "refresh_token": st.refresh_token, "account_id": "dbid:mock", "uid": "1",
                "scope": "account_info.read files.content.read files.content.write files.metadata.read",
            }))
            .into_response()
        }
        _ => bad_request("unsupported grant_type"),
    }
}

/// Stands in for Dropbox's consent page: approves at once and redirects back.
async fn authorize(Query(q): Query<HashMap<String, String>>) -> Response {
    let target = format!(
        "{}?code=mock-code&state={}",
        q.get("redirect_uri").cloned().unwrap_or_default(),
        q.get("state").cloned().unwrap_or_default()
    );
    (StatusCode::FOUND, [(header::LOCATION, target)]).into_response()
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// Starts both hosts; their base URLs are in `api` and `content`.
pub async fn start() -> Shared {
    let shared: Shared = Arc::new(Mutex::new(MockState::default()));
    let api = Router::new()
        .route("/oauth2/token", post(token))
        .route("/oauth2/authorize", get(authorize))
        .route("/2/users/get_space_usage", route("users/get_space_usage", Host::Api, space_usage))
        .route("/2/files/get_metadata", route("files/get_metadata", Host::Api, get_metadata))
        .route("/2/files/create_folder_v2", route("files/create_folder_v2", Host::Api, create_folder))
        .route("/2/files/move_v2", route("files/move_v2", Host::Api, move_item))
        .route("/2/files/copy_v2", route("files/copy_v2", Host::Api, copy_item))
        .with_state(shared.clone());
    let content = Router::new()
        .route("/2/files/upload_session/start", route("files/upload_session/start", Host::Content, session_start))
        .route(
            "/2/files/upload_session/append_v2",
            route("files/upload_session/append_v2", Host::Content, session_append),
        )
        .route(
            "/2/files/upload_session/finish",
            route("files/upload_session/finish", Host::Content, session_finish),
        )
        .route("/2/files/download", route("files/download", Host::Content, download))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(shared.clone());
    let (api, content) = (serve(api).await, serve(content).await);
    {
        let mut st = shared.lock().unwrap();
        st.api = api;
        st.content = content;
    }
    shared
}
