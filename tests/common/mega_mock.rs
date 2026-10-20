//! In-process fake of MEGA's API (the `cs` commands CloudDrop uses), its upload servers and
//! download URLs. It holds one test account and does MEGA's server side of the cryptography
//! (salted sign-in, the RSA-wrapped session id, the hashcash challenge) with its own small
//! implementation, independent of `clouddrop::mega`, so tests can also decrypt what was
//! uploaded and check its MAC.

use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as AxPath, Query, State as AxState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use num_bigint::BigUint;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub const EMAIL: &str = "tester@example.com";
pub const PASSWORD: &str = "correct horse battery staple";

// A 2048-bit RSA key made for these tests (p, q, d, u = p⁻¹ mod q), as MEGA keeps an account's.
const RSA_P: &str = concat!(
    "f46d7930b93d165f7dbddb0afd59be140329a84250f7c4b814456cf5c4ac7a76d0d6ad4677fd79b0b7b5c58e86096a93",
    "01d5fcfe291de4b83f61a744a826832b328c72a03ff46a53936cbdb38f1658c84d433a37bf03aefd4f3e2621b68b53d4",
    "3d845e1746527ff480411c61c2e19f77acd3542bae8e083bd435573378a4e319",
);
const RSA_Q: &str = concat!(
    "ef6167c0e69ee2d6e984bfefe57e4df8e1a0c4b2c0b5b91258ca652ddbbdb3ce6bbbdc6022ccbfc88600d95b547bb362",
    "c666a6295e2f2ce41672a63f380b2feb9b8a1d59cd6b305f7e9f02ba577ac5bd8b0a876aa7438991372c47c04441b7cc",
    "abcfe2051a1bc64c9c7436fe1399ea0f97f02d6472fbd3d3f93dbc13f45a86ef",
);
const RSA_D: &str = concat!(
    "68b552a5c3ffc0ae387664186b0257bb9662e3855f74bace150313cbef28ba0f28ca247988d9ddac24e399f34cee8600",
    "a4019439eb8e0c4d7fd145be863db69ff5f23cdb35c296597dbef69410558a1403edbd4461dc860baa6c2b3a70b60a1c",
    "bf7b6d3fd46ddd385149884cc991bbae6123e41273017b0744bc1346ffe0833c68c1f2def670f1553aaea44b3e1fad38",
    "a3f32246ba318e5b5075f652dd4888ba2f2697f0f1ce737b4f2c2d08643c0ac108b82d72bebfd5deb4aff2db48cf65f8",
    "8f07d6b0019ba6b743c7dbfa68178f6b2cf67ba6e52ea6b6cccae236afecb32e8b5e9425c8445f166a3fcb480174709d",
    "5de7fcc8eaba1ea72658c073323e269",
);
const RSA_U: &str = concat!(
    "98ce7cee72c34772429bbee5d90174f66c99ed05403421f461254bb254a39dc0590eb05fcb11e257ed3dc05197279633",
    "26704c8908333781ef04722401b13ea92b81ffc03b29421d10ced2d1724ad64e37b76621c6346273c1d9aa3ccc910c02",
    "211a5c22ee41a9ec8ecb0f1a96a8ff0458a1f3da43bb573d0deff79ee07f326e",
);

fn big(hex: &str) -> BigUint {
    BigUint::parse_bytes(hex.as_bytes(), 16).unwrap()
}

pub fn b64e(b: &[u8]) -> String {
    B64.encode(b)
}

pub fn b64d(s: &str) -> Vec<u8> {
    B64.decode(s).unwrap()
}

fn cipher(key: &[u8]) -> Aes128 {
    let k: [u8; 16] = key.try_into().unwrap();
    Aes128::new(&k.into())
}

fn ecb(key: &[u8], data: &[u8], encrypt: bool) -> Vec<u8> {
    let c = cipher(key);
    let mut out = data.to_vec();
    for b in out.as_chunks_mut::<16>().0 {
        if encrypt {
            c.encrypt_block(b.into());
        } else {
            c.decrypt_block(b.into());
        }
    }
    out
}

fn enc1(c: &Aes128, block: [u8; 16]) -> [u8; 16] {
    let mut b = block;
    c.encrypt_block((&mut b).into());
    b
}

fn mpi(n: &BigUint) -> Vec<u8> {
    let mut v = (n.bits() as u16).to_be_bytes().to_vec();
    v.extend(n.to_bytes_be());
    v
}

/// MEGA's chunk boundaries, written out the long way.
pub fn boundaries(size: u64) -> Vec<u64> {
    let mut v = vec![0];
    let mut step = 128 * 1024;
    while *v.last().unwrap() < size {
        let next = (v.last().unwrap() + step).min(size);
        v.push(next);
        step = (step + 128 * 1024).min(1024 * 1024);
    }
    v
}

#[derive(Clone, Debug)]
pub struct Node {
    pub h: String,
    pub p: String,
    pub t: i64,
    /// Attributes and key exactly as the client sent them.
    pub a: String,
    pub k: String,
    /// Encrypted content.
    pub data: Vec<u8>,
}

struct Upload {
    size: u64,
    data: Vec<u8>,
    got: BTreeMap<u64, u64>,
}

pub struct MegaState {
    pub base: String,
    pub master: [u8; 16],
    pub user: String,
    salt: [u8; 32],
    pub nodes: BTreeMap<String, Node>,
    pub root: String,
    pub rubbish: String,
    sessions: Vec<String>,
    uploads: HashMap<String, Upload>,
    /// Completion token → upload id.
    tokens: HashMap<String, String>,
    next: u64,
    /// Demand a proof of work of this easiness on sign-ins.
    pub hashcash: Option<u8>,
    pub hashcash_solved: u64,
    /// The 2FA code sign-ins need, if any.
    pub mfa: Option<String>,
    pub logins: u64,
    pub fetches: u64,
    pub chunk_posts: u64,
    /// Answer the next commands of this name with these error codes.
    pub fail: HashMap<String, VecDeque<i64>>,
    /// Answer the Nth chunk POST (1-based, counting all) with this body instead of storing it.
    pub fail_chunk: HashMap<u64, String>,
    pub limit: u64,
}

pub type Shared = Arc<Mutex<MegaState>>;

impl MegaState {
    fn handle(&mut self) -> String {
        self.next += 1;
        format!("N{:07}", self.next)
    }

    fn keys(&self) -> ([u8; 16], [u8; 16]) {
        let mut dk = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha512>(PASSWORD.as_bytes(), &self.salt, 100_000, &mut dk);
        (dk[..16].try_into().unwrap(), dk[16..].try_into().unwrap())
    }

    /// Ends every session (as "log out of all devices" does).
    pub fn kill_sessions(&mut self) {
        self.sessions.clear();
    }

    /// A node's decrypted key (32 bytes for files, 16 for folders).
    pub fn node_key(&self, h: &str) -> Vec<u8> {
        let n = &self.nodes[h];
        let (_, k) = n.k.split_once(':').unwrap();
        ecb(&self.master, &b64d(k), false)
    }

    fn content_key(&self, h: &str) -> ([u8; 16], [u8; 8], [u8; 8]) {
        let k = self.node_key(h);
        if k.len() == 16 {
            return (k.try_into().unwrap(), [0; 8], [0; 8]);
        }
        let aes: [u8; 16] = std::array::from_fn(|i| k[i] ^ k[16 + i]);
        (aes, k[16..24].try_into().unwrap(), k[24..32].try_into().unwrap())
    }

    pub fn name(&self, h: &str) -> String {
        let (key, _, _) = self.content_key(h);
        let c = cipher(&key);
        let enc = b64d(&self.nodes[h].a);
        let mut prev = [0u8; 16];
        let mut plain = vec![];
        for block in enc.chunks(16) {
            let mut b: [u8; 16] = block.try_into().unwrap();
            c.decrypt_block((&mut b).into());
            plain.extend(b.iter().zip(prev).map(|(x, p)| x ^ p));
            prev = block.try_into().unwrap();
        }
        let text = String::from_utf8(plain).unwrap();
        let json = text.strip_prefix("MEGA").unwrap().trim_end_matches('\0');
        let v: Value = serde_json::from_str(json).unwrap();
        v["n"].as_str().unwrap().to_string()
    }

    /// A file's plaintext.
    pub fn content(&self, h: &str) -> Vec<u8> {
        let (key, nonce, _) = self.content_key(h);
        let c = cipher(&key);
        let mut out = self.nodes[h].data.clone();
        for (i, block) in out.chunks_mut(16).enumerate() {
            let mut ctr = [0u8; 16];
            ctr[..8].copy_from_slice(&nonce);
            ctr[8..].copy_from_slice(&(i as u64).to_be_bytes());
            for (b, k) in block.iter_mut().zip(enc1(&c, ctr)) {
                *b ^= k;
            }
        }
        out
    }

    /// The file's content matches the MAC in its key.
    pub fn mac_ok(&self, h: &str) -> bool {
        let (key, nonce, mac) = self.content_key(h);
        let c = cipher(&key);
        let plain = self.content(h);
        let mut file = [0u8; 16];
        for w in boundaries(plain.len() as u64).windows(2) {
            let mut m = [0u8; 16];
            m[..8].copy_from_slice(&nonce);
            m[8..].copy_from_slice(&nonce);
            for block in plain[w[0] as usize..w[1] as usize].chunks(16) {
                for (x, b) in m.iter_mut().zip(block) {
                    *x ^= b;
                }
                m = enc1(&c, m);
            }
            for (x, y) in file.iter_mut().zip(m) {
                *x ^= y;
            }
            file = enc1(&c, file);
        }
        let condensed: Vec<u8> = (0..4).map(|i| file[i] ^ file[i + 4]).chain((0..4).map(|i| file[i + 8] ^ file[i + 12])).collect();
        condensed == mac
    }

    pub fn children(&self, parent: &str) -> Vec<Node> {
        self.nodes.values().filter(|n| n.p == parent).cloned().collect()
    }

    /// Resolves "a/b/c" below the cloud drive by name; None if missing or ambiguous.
    pub fn lookup(&self, path: &str) -> Option<Node> {
        let mut parent = self.root.clone();
        let mut found = None;
        for part in path.split('/') {
            let hits: Vec<Node> = self.children(&parent).into_iter().filter(|n| n.t <= 1 && self.name(&n.h) == part).collect();
            if hits.len() != 1 {
                return None;
            }
            parent = hits[0].h.clone();
            found = Some(hits[0].clone());
        }
        found
    }

    /// Every item below `parent`, as "rel/path" → content (folders end in "/").
    pub fn tree(&self, parent: &str, prefix: &str, out: &mut Vec<(String, String)>) {
        let mut kids = self.children(parent);
        kids.sort_by_key(|n| self.name(&n.h));
        for n in kids {
            let p = if prefix.is_empty() { self.name(&n.h) } else { format!("{prefix}/{}", self.name(&n.h)) };
            if n.t == 1 {
                out.push((format!("{p}/"), String::new()));
                self.tree(&n.h, &p, out);
            } else {
                out.push((p, String::from_utf8_lossy(&self.content(&n.h)).to_string()));
            }
        }
    }

    /// Moves a node to the rubbish bin (what the web app's delete does).
    pub fn trash(&mut self, h: &str) {
        let rubbish = self.rubbish.clone();
        self.nodes.get_mut(h).unwrap().p = rubbish;
    }

    fn json(&self, n: &Node) -> Value {
        let mut v = json!({"h": n.h, "p": n.p, "u": self.user, "t": n.t, "ts": 1_760_000_000});
        if n.t <= 1 {
            v["a"] = json!(n.a);
            v["k"] = json!(n.k);
        }
        if n.t == 0 {
            v["s"] = json!(n.data.len());
        }
        v
    }

    fn used(&self) -> u64 {
        self.nodes.values().map(|n| n.data.len() as u64).sum()
    }
}

fn hashcash_ok(token: &str, easiness: u8, nonce: &str) -> bool {
    let e = u32::from(easiness);
    let threshold = (((e & 63) << 1) + 1) << ((e >> 6) * 7 + 3);
    let mut h = Sha256::new();
    h.update(b64d(nonce));
    h.update(b64d(token).repeat(262_144));
    let d = h.finalize();
    u32::from_be_bytes([d[0], d[1], d[2], d[3]]) <= threshold
}

const HASHCASH_TOKEN: &str = "wFqIT_wY3tYKcrm5zqwaUoWym3ZCz32cCsrJOgYBgihtpaWUhGyWJ--EY-zfwI-i";

/// The answer to one command: a JSON value or an error code.
fn command(st: &mut MegaState, sid: Option<&str>, h: &HeaderMap, c: &Value) -> Result<Value, Box<Response>> {
    let a = c["a"].as_str().unwrap_or_default().to_string();
    if let Some(code) = st.fail.get_mut(&a).and_then(VecDeque::pop_front) {
        return Ok(json!(code));
    }
    let authed = sid.is_some_and(|s| st.sessions.iter().any(|x| x == s));
    if !matches!(a.as_str(), "us0" | "us") && !authed {
        return Err(Box::new(axum::Json(json!(-15)).into_response()));
    }
    match a.as_str() {
        "us0" => {
            if c["user"] != EMAIL {
                return Ok(json!(-9));
            }
            Ok(json!({"v": 2, "s": b64e(&st.salt)}))
        }
        "us" => {
            if let Some(e) = st.hashcash {
                let answer = h.get("X-Hashcash").and_then(|v| v.to_str().ok()).unwrap_or_default();
                let parts: Vec<&str> = answer.split(':').collect();
                let solved = parts.len() == 3 && parts[1] == HASHCASH_TOKEN && hashcash_ok(parts[1], e, parts[2]);
                if !solved {
                    let challenge = format!("1:{e}:1731410499:{HASHCASH_TOKEN}");
                    return Err(Box::new((StatusCode::PAYMENT_REQUIRED, [("X-Hashcash", challenge)]).into_response()));
                }
                st.hashcash_solved += 1;
            }
            let (pw_key, auth) = st.keys();
            if c["user"] != EMAIL || c["uh"] != b64e(&auth) {
                return Ok(json!(-9));
            }
            if let Some(code) = &st.mfa
                && c["mfa"].as_str() != Some(code.as_str())
            {
                return Ok(json!(-26));
            }
            st.logins += 1;
            let mut sid = rand::random::<[u8; 16]>().to_vec();
            sid.extend(st.user.as_bytes());
            sid.extend(rand::random::<[u8; 16]>());
            st.sessions.push(b64e(&sid));
            // m: the session id at the top of len(p) + len(q) - 2 = 254 bytes.
            let mut m = sid.clone();
            m.resize(254, 0x5a);
            let (p, q) = (big(RSA_P), big(RSA_Q));
            let csid = BigUint::from_bytes_be(&m).modpow(&BigUint::from(65537u32), &(&p * &q));
            let mut privk: Vec<u8> = [p, q, big(RSA_D), big(RSA_U)].iter().flat_map(mpi).collect();
            privk.resize(privk.len().div_ceil(16) * 16, 0);
            Ok(json!({
                "k": b64e(&ecb(&pw_key, &st.master, true)),
                "privk": b64e(&ecb(&st.master, &privk, true)),
                "csid": b64e(&mpi(&csid)),
                "u": st.user,
            }))
        }
        "f" => {
            st.fetches += 1;
            let f: Vec<Value> = st.nodes.values().map(|n| st.json(n)).collect();
            Ok(json!({"f": f, "ok": [], "sn": "x"}))
        }
        "uq" => {
            let used = st.used();
            let mut per = Map::new();
            per.insert(st.root.clone(), json!([used, 0, 0, 0, 0]));
            Ok(json!({"mstrg": st.limit, "cstrg": used + 1000, "cstrgn": per}))
        }
        "u" => {
            let size = c["s"].as_u64().unwrap();
            if st.used() + size > st.limit {
                return Ok(json!(-17));
            }
            let id = st.handle();
            st.uploads.insert(id.clone(), Upload { size, data: vec![0; size as usize], got: BTreeMap::new() });
            Ok(json!({"p": format!("{}/ul/{id}", st.base)}))
        }
        "p" => {
            let target = c["t"].as_str().unwrap_or_default().to_string();
            if !st.nodes.get(&target).is_some_and(|n| n.t >= 1) {
                return Ok(json!(-9));
            }
            let mut made = vec![];
            for n in c["n"].as_array().unwrap() {
                let src = n["h"].as_str().unwrap();
                let data = if n["t"] == 1 {
                    vec![]
                } else if let Some(up) = st.tokens.get(src) {
                    st.uploads[up].data.clone()
                } else if let Some(existing) = st.nodes.get(src) {
                    existing.data.clone()
                } else {
                    return Ok(json!(-9));
                };
                if let Some(ov) = n["ov"].as_str()
                    && !st.nodes.contains_key(ov)
                {
                    return Ok(json!(-9));
                }
                let h = st.handle();
                let node = Node {
                    h: h.clone(),
                    p: target.clone(),
                    t: n["t"].as_i64().unwrap(),
                    a: n["a"].as_str().unwrap().to_string(),
                    k: format!("{}:{}", st.user, n["k"].as_str().unwrap()),
                    data,
                };
                made.push(st.json(&node));
                st.nodes.insert(h.clone(), node);
                // Versioning on: the old file becomes a child (previous version) of the new one.
                if let Some(ov) = n["ov"].as_str() {
                    st.nodes.get_mut(ov).unwrap().p = h.clone();
                    let old = st.json(&st.nodes[ov]);
                    made.push(old);
                }
            }
            Ok(json!({"f": made}))
        }
        "a" => {
            let Some(n) = st.nodes.get_mut(c["n"].as_str().unwrap_or_default()) else { return Ok(json!(-9)) };
            n.a = c["at"].as_str().unwrap().to_string();
            Ok(json!(0))
        }
        "m" => {
            let to = c["t"].as_str().unwrap_or_default().to_string();
            if !st.nodes.contains_key(&to) {
                return Ok(json!(-9));
            }
            let Some(n) = st.nodes.get_mut(c["n"].as_str().unwrap_or_default()) else { return Ok(json!(-9)) };
            n.p = to;
            Ok(json!(0))
        }
        "g" => {
            let h = c["n"].as_str().unwrap_or_default();
            let Some(n) = st.nodes.get(h).filter(|n| n.t == 0) else { return Ok(json!(-9)) };
            Ok(json!({"g": format!("{}/dl/{h}", st.base), "s": n.data.len(), "at": n.a}))
        }
        _ => Ok(json!(-2)),
    }
}

async fn cs(AxState(s): AxState<Shared>, h: HeaderMap, Query(q): Query<HashMap<String, String>>, body: String) -> Response {
    let cmds: Vec<Value> = serde_json::from_str(&body).unwrap();
    let mut st = s.lock().unwrap();
    let mut out = vec![];
    for c in &cmds {
        match command(&mut st, q.get("sid").map(String::as_str), &h, c) {
            Ok(v) => out.push(v),
            Err(resp) => return *resp,
        }
    }
    axum::Json(Value::Array(out)).into_response()
}

async fn upload(AxState(s): AxState<Shared>, AxPath((id, offset)): AxPath<(String, u64)>, body: Bytes) -> Response {
    let mut st = s.lock().unwrap();
    st.chunk_posts += 1;
    let n = st.chunk_posts;
    if let Some(answer) = st.fail_chunk.remove(&n) {
        return answer.into_response();
    }
    let Some(up) = st.uploads.get_mut(&id) else { return StatusCode::NOT_FOUND.into_response() };
    let end = offset + body.len() as u64;
    // Real clients send whole MEGA chunks; insist on it.
    let b = boundaries(up.size);
    if !b.contains(&offset) || !b.contains(&end) || end > up.size || (body.is_empty() && up.size > 0) {
        return (StatusCode::BAD_REQUEST, "-2").into_response();
    }
    up.data[offset as usize..end as usize].copy_from_slice(&body);
    up.got.insert(offset, end);
    let mut covered = 0;
    for (&a, &z) in &up.got {
        if a <= covered {
            covered = covered.max(z);
        }
    }
    if covered < up.size {
        return "".into_response();
    }
    let token = b64e(&rand::random::<[u8; 27]>());
    st.tokens.insert(token.clone(), id);
    token.into_response()
}

async fn download(AxState(s): AxState<Shared>, AxPath((h, range)): AxPath<(String, String)>) -> Response {
    let st = s.lock().unwrap();
    let Some(n) = st.nodes.get(&h) else { return StatusCode::NOT_FOUND.into_response() };
    let (a, z) = range.split_once('-').unwrap();
    let (a, z): (usize, usize) = (a.parse().unwrap(), z.parse().unwrap());
    let z = (z + 1).min(n.data.len());
    n.data[a.min(z)..z].to_vec().into_response()
}

pub async fn start_mega_mock() -> Shared {
    let master: [u8; 16] = rand::random();
    let mut st = MegaState {
        base: String::new(),
        master,
        user: b64e(&rand::random::<[u8; 8]>()),
        salt: rand::random(),
        nodes: BTreeMap::new(),
        root: "ROOT0001".into(),
        rubbish: "RUBBISH1".into(),
        sessions: vec![],
        uploads: HashMap::new(),
        tokens: HashMap::new(),
        next: 0,
        hashcash: None,
        hashcash_solved: 0,
        mfa: None,
        logins: 0,
        fetches: 0,
        chunk_posts: 0,
        fail: HashMap::new(),
        fail_chunk: HashMap::new(),
        limit: 20 << 30,
    };
    for (h, t) in [("ROOT0001", 2), ("VAULT001", 3), ("RUBBISH1", 4)] {
        st.nodes.insert(h.into(), Node { h: h.into(), p: String::new(), t, a: String::new(), k: String::new(), data: vec![] });
    }
    let shared: Shared = Arc::new(Mutex::new(st));
    let app = Router::new()
        .route("/cs", post(cs))
        .route("/ul/{id}/{offset}", post(upload))
        .route("/dl/{h}/{range}", get(download))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    shared.lock().unwrap().base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    shared
}
