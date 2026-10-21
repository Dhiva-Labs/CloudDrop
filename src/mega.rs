//! MEGA (mega.nz): a client of MEGA's own JSON API, just the calls CloudDrop needs. The engine
//! reaches it through [`crate::remote::Remote`].
//!
//! # Why a native client
//!
//! MEGA encrypts everything on the client, so whatever talks to it does MEGA's cryptography
//! itself. The spike (October 2026) weighed three ways:
//!
//! - **MEGA's C++ SDK** (BSD-2) does everything, but it is a large C++ build with its own
//!   dependencies, to vendor for the offline PPA build and cross-build for Windows. Kept as the
//!   fallback.
//! - **The `mega` crate** (0.8.0, MIT/Apache, last release October 2024) signs in (2FA too) and
//!   resumes sessions, but sends a file in one request (an upload can't resume), downloads only
//!   whole files (Windows placeholders need ranges), can't copy a file (keep_version), and
//!   brings about 30 crates, several of them a second version of one CloudDrop already has
//!   (reqwest, rand, sha2, base64, thiserror).
//! - **This client**: the dozen API commands CloudDrop needs, on `aes`, `pbkdf2` and
//!   `num-bigint`. It resumes uploads after a crash, reads ranges, and fits the provider
//!   contract as it is. Chosen.
//!
//! The protocol follows MEGA's C++ SDK; its unit-test vectors check the cryptography below.
//!
//! # MEGA in brief
//!
//! - **Sign-in.** `us0` gives the account's salt. PBKDF2-HMAC-SHA512 (100 000 rounds) of the
//!   password makes a password key and an auth key (older accounts use MEGA's earlier AES-based
//!   derivation). `us` checks the auth key and the 2FA code, and answers with the master key
//!   (encrypted with the password key) and a session id, RSA-encrypted with the account's
//!   private key (itself encrypted with the master key). Only the session id and the master key
//!   are kept, in credentials.json, never the password. MEGA sometimes asks for a proof of work
//!   first (HTTP 402 with `X-Hashcash`); the client does it.
//! - **Nodes.** Files and folders are nodes with stable 8-character handles. Each has its own
//!   AES key, stored encrypted with the master key, and its name is in attributes encrypted with
//!   that key. `f` fetches the whole tree, so the client caches it ([`TREE_TTL`]).
//! - **Content.** AES-128-CTR with a per-file key and nonce. Integrity is a CBC-MAC of each
//!   chunk (128 KiB, 256 KiB, … up to 1 MiB, then 1 MiB each), folded into a 64-bit MAC kept in
//!   the file's key. The server never sees the plaintext and reports no content hash, so
//!   freeing space verifies by downloading, which checks that MAC too.
//! - **Uploads.** `u` gives an upload URL; encrypted chunks are POSTed to `url/offset`, the last
//!   one answers a completion token, and `p` creates the node with its key and attributes. The
//!   server doesn't say how much it holds, so the client records each confirmed chunk, with the
//!   MAC so far, in `mega-uploads/` in the data directory: that is what lets an upload resume.
//! - **Replacing a file** uploads a new node (a new handle) that names the old one as its
//!   previous version (`ov`): with MEGA's file versioning on (the default) the old content stays
//!   in the file's version history; with it off, MEGA drops it. `keep_version` makes a copy
//!   first either way, so CloudDrop never relies on that setting.

use crate::db::Kind;
use crate::hash::Hasher;
use crate::remote::{
    Capabilities, ContentHash, Field, FieldKind, HashKind, ProviderInfo, RemoteError, RemoteMeta, RemoteResult,
    SignIn, StorageQuota, UploadStatus,
};
use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures_util::StreamExt;
use num_bigint::BigUint;
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncWriteExt;

pub static INFO: ProviderInfo = ProviderInfo {
    experimental: false,
    id: "mega",
    name: "MEGA",
    caps: Capabilities {
        // MEGA's fingerprint (a sparse CRC and the mtime) is no proof of content; the
        // download is, and it also checks MEGA's own MAC.
        hash: None,
        pin_versions: false,
        stable_ids: true,
        resumable: true,
        case_insensitive: false,
        forbidden_chars: "",
        // Uploads go in whole MEGA chunks, which are 1 MiB apart after the first 4.5 MiB: a
        // chunk of at least this much always reaches the next chunk boundary.
        chunk_unit: CHUNK_MAX as usize,
    },
    sign_in: SignIn::Form(&[
        Field { key: "email", label: "Email", kind: FieldKind::Text, required: true, hint: "" },
        Field {
            key: "password",
            label: "Password",
            kind: FieldKind::Secret,
            required: true,
            hint: "Used once to sign in; CloudDrop keeps only the session",
        },
        Field {
            key: "mfa",
            label: "Two-factor code",
            kind: FieldKind::Secret,
            required: false,
            hint: "The 6-digit code from your authenticator app, if two-factor authentication is on",
        },
    ]),
    web_url: |kind, id| match kind {
        Kind::Dir => Some(format!("https://mega.nz/fm/{id}")),
        // MEGA's web app has no address for a single file of your own (only share links).
        Kind::File => None,
    },
};

pub const DEFAULT_API: &str = "https://g.api.mega.co.nz";

/// `[mega]` in config.toml. The password is never written anywhere: the client signs in once
/// with it and keeps only the session (see [`crate::credentials`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MegaConfig {
    pub email: String,
    /// MEGA's API server; tests point it at a local fake.
    #[serde(skip_serializing_if = "is_default_api")]
    pub api_url: String,
}

impl Default for MegaConfig {
    fn default() -> Self {
        Self { email: String::new(), api_url: DEFAULT_API.into() }
    }
}

impl MegaConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn is_default_api(s: &str) -> bool {
    s == DEFAULT_API
}

/// How long the cached node tree answers `get` and `find_child` before it is fetched again.
/// The client's own changes update it at once; this bounds how late changes made elsewhere
/// (MEGA's apps, the web) show up, such as a file moved to the rubbish bin.
pub const TREE_TTL: Duration = Duration::from_secs(30);
/// An unknown handle or parent fetches the tree again, but not more often than this.
const MISS_REFETCH: Duration = Duration::from_secs(5);
/// Download URLs are reused for this long.
const URL_TTL: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Cryptography

/// MEGA's chunks: 128 KiB, 256 KiB, … 1 MiB (eight growing chunks, 4.5 MiB in all), then 1 MiB.
const CHUNK_STEP: u64 = 128 * 1024;
const CHUNK_MAX: u64 = 8 * CHUNK_STEP;
const RAMP_END: u64 = 36 * CHUNK_STEP;

/// The chunk boundary after `pos` (the end of the chunk holding `pos`).
fn chunk_end(pos: u64) -> u64 {
    let mut start = 0;
    for i in 1..=8 {
        let end = start + i * CHUNK_STEP;
        if pos < end {
            return end;
        }
        start = end;
    }
    RAMP_END + ((pos - RAMP_END) / CHUNK_MAX + 1) * CHUNK_MAX
}

/// The last chunk boundary at or before `pos`.
fn chunk_floor(pos: u64) -> u64 {
    let mut start = 0;
    for i in 1..=8 {
        let end = start + i * CHUNK_STEP;
        if pos < end {
            return start;
        }
        start = end;
    }
    RAMP_END + (pos - RAMP_END) / CHUNK_MAX * CHUNK_MAX
}

fn b64e(data: &[u8]) -> String {
    B64.encode(data)
}

/// MEGA's base64 is URL-safe without padding; standard characters and padding are accepted too.
fn b64d(s: &str) -> Option<Vec<u8>> {
    let s: String = s.trim_end_matches('=').chars().map(|c| match c { '+' => '-', '/' => '_', c => c }).collect();
    B64.decode(s).ok()
}

fn b64_array<const N: usize>(s: &str) -> Option<[u8; N]> {
    b64d(s)?.try_into().ok()
}

fn aes(key: &[u8; 16]) -> Aes128 {
    Aes128::new(&(*key).into())
}

fn encrypt_block(c: &Aes128, block: &mut [u8; 16]) {
    c.encrypt_block(block.into());
}

fn xor16(a: &mut [u8; 16], b: &[u8]) {
    for (x, y) in a.iter_mut().zip(b) {
        *x ^= y;
    }
}

/// AES-ECB over whole 16-byte blocks (how MEGA wraps keys with the master key).
fn ecb_encrypt(key: &[u8; 16], data: &mut [u8]) {
    let c = aes(key);
    for b in data.as_chunks_mut::<16>().0 {
        c.encrypt_block(b.into());
    }
}

fn ecb_decrypt(key: &[u8; 16], data: &mut [u8]) {
    let c = aes(key);
    for b in data.as_chunks_mut::<16>().0 {
        c.decrypt_block(b.into());
    }
}

/// Node attributes: `MEGA{"n":"name",…}`, zero-padded, AES-CBC with a zero IV.
fn encrypt_attrs(key: &[u8; 16], attrs: &Map<String, Value>) -> String {
    let mut buf = format!("MEGA{}", Value::Object(attrs.clone())).into_bytes();
    buf.resize(buf.len().div_ceil(16) * 16, 0);
    let c = aes(key);
    let mut prev = [0u8; 16];
    for block in buf.as_chunks_mut::<16>().0 {
        xor16(&mut prev, block);
        encrypt_block(&c, &mut prev);
        *block = prev;
    }
    b64e(&buf)
}

fn decrypt_attrs(key: &[u8; 16], s: &str) -> Option<Map<String, Value>> {
    let mut buf = b64d(s)?;
    if buf.is_empty() || !buf.len().is_multiple_of(16) {
        return None;
    }
    let c = aes(key);
    let mut prev = [0u8; 16];
    for b in buf.as_chunks_mut::<16>().0 {
        let cur = *b;
        c.decrypt_block(b.into());
        xor16(b, &prev);
        prev = cur;
    }
    let text = buf.strip_prefix(b"MEGA{\"")?;
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    let json = [b"{\"".as_slice(), &text[..end]].concat();
    match serde_json::from_slice(&json).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Encrypts or decrypts (the same thing) `data` found at byte `offset` of a file: AES-CTR whose
/// counter block is the file's nonce and the big-endian number of the 16-byte block.
fn ctr_apply(c: &Aes128, nonce: &[u8; 8], mut offset: u64, data: &mut [u8]) {
    let mut i = 0;
    while i < data.len() {
        let mut ks = [0u8; 16];
        ks[..8].copy_from_slice(nonce);
        ks[8..].copy_from_slice(&(offset / 16).to_be_bytes());
        encrypt_block(c, &mut ks);
        let skip = (offset % 16) as usize;
        let n = (16 - skip).min(data.len() - i);
        for (d, k) in data[i..i + n].iter_mut().zip(&ks[skip..skip + n]) {
            *d ^= k;
        }
        i += n;
        offset += n as u64;
    }
}

/// The MACs of a file's plaintext as it streams by: a CBC-MAC per MEGA chunk (starting from
/// nonce‖nonce, the last block zero-padded), each folded into the file's MAC.
struct Macs {
    cipher: Aes128,
    nonce: [u8; 8],
    pos: u64,
    chunk_start: u64,
    chunk_end: u64,
    chunk: [u8; 16],
    block: [u8; 16],
    in_block: usize,
    file: [u8; 16],
}

impl Macs {
    /// Starts at `pos`, a chunk boundary, with the file's MAC of everything before it.
    fn new(key: &[u8; 16], nonce: [u8; 8], pos: u64, file_so_far: [u8; 16]) -> Self {
        Macs {
            cipher: aes(key),
            nonce,
            pos,
            chunk_start: pos,
            chunk_end: chunk_end(pos),
            chunk: Self::iv(&nonce),
            block: [0; 16],
            in_block: 0,
            file: file_so_far,
        }
    }

    fn iv(nonce: &[u8; 8]) -> [u8; 16] {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(nonce);
        iv[8..].copy_from_slice(nonce);
        iv
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let room = ((self.chunk_end - self.pos) as usize).min(16 - self.in_block);
            let n = room.min(data.len());
            self.block[self.in_block..self.in_block + n].copy_from_slice(&data[..n]);
            self.in_block += n;
            self.pos += n as u64;
            data = &data[n..];
            if self.in_block == 16 {
                self.mac_block();
            }
            if self.pos == self.chunk_end {
                self.end_chunk();
            }
        }
    }

    fn mac_block(&mut self) {
        let block = self.block;
        xor16(&mut self.chunk, &block);
        encrypt_block(&self.cipher, &mut self.chunk);
        self.block = [0; 16];
        self.in_block = 0;
    }

    fn end_chunk(&mut self) {
        if self.in_block > 0 {
            self.mac_block();
        }
        let chunk = self.chunk;
        xor16(&mut self.file, &chunk);
        encrypt_block(&self.cipher, &mut self.file);
        self.chunk_start = self.pos;
        self.chunk_end = chunk_end(self.pos);
        self.chunk = Self::iv(&self.nonce);
    }

    /// The file's MAC so far, ending any partial (last) chunk.
    fn finish(mut self) -> [u8; 16] {
        if self.pos > self.chunk_start {
            self.end_chunk();
        }
        self.file
    }
}

/// The 64-bit MAC MEGA keeps in a file's key: the 128-bit file MAC folded in half twice.
fn condense(mac: &[u8; 16]) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (i, o) in out.iter_mut().enumerate() {
        let j = if i < 4 { i } else { i + 4 };
        *o = mac[j] ^ mac[j + 4];
    }
    out
}

/// A file's 32-byte node key: the content key XORed with nonce‖MAC, then the nonce and MAC.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FileKey {
    aes: [u8; 16],
    nonce: [u8; 8],
    mac: [u8; 8],
}

impl FileKey {
    fn pack(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[..16].copy_from_slice(&self.aes);
        out[16..24].copy_from_slice(&self.nonce);
        out[24..].copy_from_slice(&self.mac);
        let tail: [u8; 16] = out[16..].try_into().unwrap();
        let head: &mut [u8; 16] = (&mut out[..16]).try_into().unwrap();
        xor16(head, &tail);
        out
    }

    fn unpack(k: &[u8]) -> Option<FileKey> {
        let k: &[u8; 32] = k.try_into().ok()?;
        let mut aes: [u8; 16] = k[..16].try_into().unwrap();
        xor16(&mut aes, &k[16..]);
        Some(FileKey { aes, nonce: k[16..24].try_into().unwrap(), mac: k[24..].try_into().unwrap() })
    }
}

/// The key that decrypts a node's attributes: a folder's key, or a file's content key.
fn attr_key(key: &[u8]) -> Option<[u8; 16]> {
    match key.len() {
        16 => key.try_into().ok(),
        32 => FileKey::unpack(key).map(|k| k.aes),
        _ => None,
    }
}

/// Sign-in keys of a current ("v2") account: PBKDF2-HMAC-SHA512 of the password, 100 000
/// rounds; the first half decrypts the master key, the second is the auth key sent as `uh`.
fn derive_v2(password: &str, salt: &[u8]) -> ([u8; 16], [u8; 16]) {
    let mut dk = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha512>(password.as_bytes(), salt, 100_000, &mut dk);
    (dk[..16].try_into().unwrap(), dk[16..].try_into().unwrap())
}

/// The password key of an older ("v1") account (the SDK's `pw_key`): the password's UTF-16
/// code units packed into bytes the way MEGA's JavaScript did, cut into 16-byte AES keys that
/// encrypt a fixed block 65 536 times over.
fn prepare_key_v1(password: &str) -> [u8; 16] {
    let units: Vec<u16> = password.encode_utf16().collect();
    let mut words = vec![0u32; units.len().div_ceil(4)];
    for (j, &u) in units.iter().enumerate() {
        words[j / 4] |= ((u as u64) << (24 - 8 * (j % 4))) as u32;
    }
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    let keys: Vec<Aes128> = bytes[..units.len()]
        .chunks(16)
        .map(|c| {
            let mut k = [0u8; 16];
            k[..c.len()].copy_from_slice(c);
            aes(&k)
        })
        .collect();
    let mut pkey = [0x93, 0xC4, 0x67, 0xE3, 0x7D, 0xB0, 0xC7, 0xA4, 0xD1, 0xBE, 0x3F, 0x81, 0x01, 0x52, 0xCB, 0x56];
    for _ in 0..65536 {
        for k in &keys {
            encrypt_block(k, &mut pkey);
        }
    }
    pkey
}

/// The `uh` of a v1 account (the SDK's `stringhash64`): the e-mail's bytes XORed into 16
/// bytes, encrypted 16 384 times with the password key, then words 0 and 2.
fn email_hash_v1(email: &str, key: &[u8; 16]) -> [u8; 8] {
    let mut h = [0u8; 16];
    for (i, b) in email.bytes().enumerate() {
        h[i % 16] ^= b;
    }
    let c = aes(key);
    for _ in 0..16384 {
        encrypt_block(&c, &mut h);
    }
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&h[..4]);
    out[4..].copy_from_slice(&h[8..12]);
    out
}

/// MEGA's multi-precision integers: a 16-bit big-endian bit count, then the bytes.
fn read_mpis(data: &[u8], count: usize) -> Option<Vec<BigUint>> {
    let mut out = Vec::with_capacity(count);
    let mut p = 0;
    for _ in 0..count {
        let bits = u16::from_be_bytes(data.get(p..p + 2)?.try_into().ok()?) as usize;
        let len = bits.div_ceil(8);
        p += 2;
        out.push(BigUint::from_bytes_be(data.get(p..p + len)?));
        p += len;
    }
    Some(out)
}

/// The account's RSA private key (p, q, d and u = p⁻¹ mod q), used once per sign-in.
struct RsaKey {
    p: BigUint,
    q: BigUint,
    d: BigUint,
    u: BigUint,
}

impl RsaKey {
    fn parse(data: &[u8]) -> Option<RsaKey> {
        let mut v = read_mpis(data, 4)?.into_iter();
        let key = RsaKey { p: v.next()?, q: v.next()?, d: v.next()?, u: v.next()? };
        let one = BigUint::from(1u8);
        (key.p > one && key.q > one).then_some(key)
    }

    /// m = c^d mod pq, by the Chinese remainder theorem as the SDK does it.
    fn decrypt(&self, c: &BigUint) -> BigUint {
        let one = BigUint::from(1u8);
        let xp = (c % &self.p).modpow(&(&self.d % (&self.p - &one)), &self.p);
        let xq = (c % &self.q).modpow(&(&self.d % (&self.q - &one)), &self.q);
        let diff = (&xq + &self.q - (&xp % &self.q)) % &self.q;
        xp + (diff * &self.u % &self.q) * &self.p
    }

    /// The session id in a sign-in's `csid`: the top 43 bytes of the decrypted number, read as
    /// len(p) + len(q) - 2 bytes (the SDK's `AsymmCipher::decrypt`).
    fn session_id(&self, csid: &[u8]) -> Option<Vec<u8>> {
        let c = read_mpis(csid, 1)?.pop()?;
        let m = self.decrypt(&c).to_bytes_be();
        let bytes = |n: &BigUint| n.bits().div_ceil(8) as usize;
        let len = (bytes(&self.p) + bytes(&self.q) - 2).max(m.len());
        let mut buf = vec![0u8; len - m.len()];
        buf.extend_from_slice(&m);
        buf.truncate(SID_LEN);
        (buf.len() == SID_LEN).then_some(buf)
    }
}

const SID_LEN: usize = 43;

/// MEGA's login proof of work (the SDK's `gencash`): a 4-byte nonce such that SHA-256 of the
/// nonce followed by the 48-byte token repeated 262 144 times starts with a 32-bit number no
/// bigger than the threshold the easiness gives. Returns the smallest such nonce, in base64.
fn hashcash(token: &str, easiness: u8) -> Option<String> {
    let token = b64d(token).filter(|t| t.len() == 48)?;
    let e = u32::from(easiness);
    let threshold = (((e & 63) << 1) + 1) << ((e >> 6) * 7 + 3);
    let body = token.repeat(262_144);
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8) as u32;
    let best = std::sync::atomic::AtomicU32::new(u32::MAX);
    std::thread::scope(|s| {
        for w in 0..workers {
            let (body, best) = (&body, &best);
            s.spawn(move || {
                let mut n = w;
                while n < best.load(Ordering::Relaxed) {
                    let mut h = Sha256::new();
                    h.update(n.to_be_bytes());
                    h.update(body);
                    let d = h.finalize();
                    if u32::from_be_bytes([d[0], d[1], d[2], d[3]]) <= threshold {
                        best.fetch_min(n, Ordering::Relaxed);
                        return;
                    }
                    let Some(next) = n.checked_add(workers) else { return };
                    n = next;
                }
            });
        }
    });
    let n = best.into_inner();
    (n != u32::MAX).then(|| b64e(&n.to_be_bytes()))
}

// ---------------------------------------------------------------------------
// Errors

/// MEGA's API answers errors as negative numbers.
fn api_error(code: i64) -> RemoteError {
    match code {
        -15 => RemoteError::Auth("the MEGA session has ended; sign in to MEGA again".into()),
        -26 => RemoteError::Auth("MEGA needs the current two-factor code; sign in to MEGA again".into()),
        -3 => RemoteError::RateLimited { message: "MEGA is busy; trying again later".into(), retry_after: None },
        -4 | -6 | -19 => RemoteError::RateLimited { message: "MEGA asked to slow down".into(), retry_after: None },
        -18 => RemoteError::Transient("MEGA is temporarily unavailable".into()),
        -17 | -24 => RemoteError::Quota("Your MEGA storage is full".into()),
        -29 => RemoteError::Quota("Your MEGA account is over its storage quota and limited until you free space".into()),
        -9 => RemoteError::NotFound,
        -16 => RemoteError::Permanent("This MEGA account is blocked; see mega.nz".into()),
        -11 => RemoteError::Permanent("MEGA refused access".into()),
        -14 => RemoteError::Permanent("MEGA could not decrypt this item".into()),
        -28 => RemoteError::Permanent("This MEGA business account has expired".into()),
        c => RemoteError::Permanent(format!("MEGA refused the request (error {c})")),
    }
}

/// Errors from MEGA's upload servers, where some codes mean "start this upload again".
fn upload_error(code: i64) -> RemoteError {
    match code {
        // -4: this server cannot continue the upload; -8/-9: the upload URL has expired.
        -4 | -8 | -9 | -14 => RemoteError::NotFound,
        c => api_error(c),
    }
}

fn net_error(e: reqwest::Error) -> RemoteError {
    RemoteError::Transient(format!("network error: {e}"))
}

fn http_error(status: StatusCode, resp: &Response) -> RemoteError {
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    match status.as_u16() {
        429 | 509 => RemoteError::RateLimited { message: format!("MEGA asked to slow down ({status})"), retry_after },
        408 | 500..=599 => RemoteError::Transient(format!("MEGA is unavailable ({status})")),
        _ => RemoteError::Permanent(format!("MEGA refused the request ({status})")),
    }
}

// ---------------------------------------------------------------------------
// The client

/// A signed-in session: what credentials.json keeps instead of the password.
#[derive(Debug, Clone)]
struct Session {
    sid: String,
    master: [u8; 16],
    /// The account's user handle (owner of its own nodes' keys).
    user: String,
}

#[derive(Debug, Clone)]
struct Node {
    handle: String,
    /// Empty for the roots.
    parent: String,
    /// 0 file, 1 folder, 2 cloud drive root, 3 vault, 4 rubbish bin.
    kind: i64,
    /// Decrypted attributes (`n` is the name); None when this client can't decrypt the node.
    attrs: Option<Map<String, Value>>,
    /// The decrypted node key: 32 bytes for files, 16 for folders.
    key: Option<Vec<u8>>,
    size: u64,
}

impl Node {
    fn name(&self) -> Option<&str> {
        self.attrs.as_ref()?.get("n")?.as_str()
    }
}

struct Tree {
    nodes: HashMap<String, Node>,
    root: String,
    fetched: Instant,
}

impl Tree {
    /// Ends in the rubbish bin (or in nothing this account owns).
    fn trashed(&self, handle: &str) -> bool {
        let mut h = handle;
        for _ in 0..256 {
            match self.nodes.get(h) {
                Some(n) if n.kind == 4 => return true,
                Some(n) if n.kind >= 2 => return false,
                Some(n) => h = &n.parent,
                None => return false,
            }
        }
        false
    }

    fn meta(&self, n: &Node) -> RemoteMeta {
        RemoteMeta {
            id: n.handle.clone(),
            name: n.name().unwrap_or_default().to_string(),
            parents: if n.parent.is_empty() { vec![] } else { vec![n.parent.clone()] },
            folder: n.kind != 0,
            hash: None,
            size: (n.kind == 0).then_some(n.size),
            trashed: self.trashed(&n.handle),
        }
    }
}

/// An upload session as the engine keeps it (`entries.upload_url`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UploadSession {
    /// Names the progress file in `mega-uploads/`.
    id: String,
    url: String,
    key: String,
    nonce: String,
    size: u64,
    parent: String,
    name: String,
    /// The file this upload replaces (it becomes the new file's previous version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ov: Option<String>,
}

/// What the upload server has confirmed: everything before `offset`, and the file MAC of it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Progress {
    offset: u64,
    mac: String,
    /// The completion token, once the last chunk is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// A `p` (create the node) was sent, so a retry first looks for the node it made.
    #[serde(default)]
    finishing: bool,
}

pub struct Mega {
    http: reqwest::Client,
    api: String,
    email: String,
    creds_file: PathBuf,
    uploads_dir: PathBuf,
    st: Box<State>,
    seq: AtomicU64,
}

/// The session and caches (boxed, so [`crate::remote::Remote`] stays small).
#[derive(Default)]
struct State {
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
    tree: Mutex<Option<Tree>>,
    refreshing: tokio::sync::Mutex<()>,
    urls: Mutex<HashMap<String, (String, Instant)>>,
}

impl Mega {
    /// `data_dir` holds credentials.json (the session) and the upload progress files.
    pub fn new(http: reqwest::Client, cfg: &MegaConfig, data_dir: &Path) -> Self {
        Self {
            http,
            api: cfg.api_url.trim_end_matches('/').to_string(),
            email: cfg.email.trim().to_ascii_lowercase(),
            creds_file: data_dir.join("credentials.json"),
            uploads_dir: data_dir.join("mega-uploads"),
            st: Box::default(),
            seq: AtomicU64::new(rand::random::<u32>() as u64),
        }
    }

    // --- API plumbing

    /// Sends one command; returns its answer. Retries MEGA's "busy" twice, and does the proof
    /// of work when MEGA asks for one.
    async fn request(&self, sid: Option<&str>, cmd: &Value) -> RemoteResult<Value> {
        let body = Value::Array(vec![cmd.clone()]);
        let mut hashcash_header: Option<String> = None;
        let mut busy = 0;
        let mut challenges = 0;
        loop {
            let id = self.seq.fetch_add(1, Ordering::Relaxed);
            let mut url = format!("{}/cs?id={id}", self.api);
            if let Some(sid) = sid {
                url.push_str("&sid=");
                url.push_str(sid);
            }
            let mut req = self.http.post(&url).timeout(Duration::from_secs(300)).json(&body);
            if let Some(h) = &hashcash_header {
                req = req.header("X-Hashcash", h);
            }
            let resp = req.send().await.map_err(net_error)?;
            let status = resp.status();
            if status == StatusCode::PAYMENT_REQUIRED
                && let Some(challenge) = resp.headers().get("X-Hashcash").and_then(|v| v.to_str().ok())
                && challenges < 2
            {
                challenges += 1;
                hashcash_header = Some(solve_challenge(challenge).await?);
                continue;
            }
            if !status.is_success() {
                return Err(http_error(status, &resp));
            }
            let v: Value = resp
                .json()
                .await
                .map_err(|e| RemoteError::Transient(format!("bad answer from MEGA: {e}")))?;
            let answer = match v {
                Value::Array(mut a) if !a.is_empty() => a.swap_remove(0),
                other => other,
            };
            let code = answer.as_i64().or_else(|| answer.get("err").and_then(Value::as_i64));
            match code {
                Some(-3) if busy < 2 => {
                    busy += 1;
                    tokio::time::sleep(Duration::from_millis(250 * 4u64.pow(busy - 1))).await;
                }
                Some(c) if c < 0 => return Err(api_error(c)),
                _ => return Ok(answer),
            }
        }
    }

    /// A command in the signed-in session.
    async fn api(&self, cmd: Value) -> RemoteResult<Value> {
        let s = self.session().await?;
        let r = self.request(Some(&s.sid), &cmd).await;
        if matches!(r, Err(RemoteError::Auth(_))) {
            // Read credentials.json again next time: a new sign-in may have replaced it.
            *self.st.session.lock().await = None;
        }
        r
    }

    async fn session(&self) -> RemoteResult<Arc<Session>> {
        let mut guard = self.st.session.lock().await;
        if let Some(s) = guard.as_ref() {
            return Ok(s.clone());
        }
        let creds = crate::credentials::load_from(&self.creds_file, "mega")
            .map_err(|e| RemoteError::Permanent(format!("cannot read the MEGA sign-in: {e:#}")))?
            .unwrap_or_default();
        let session = match saved_session(&creds) {
            Some(s) => s,
            None => {
                let Some(password) = creds.get("password").and_then(Value::as_str) else {
                    return Err(RemoteError::Auth("MEGA sign-in needed: run `clouddrop setup --provider mega`".into()));
                };
                let mfa = creds.get("mfa").and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty());
                let result = self.login(password, mfa).await;
                match &result {
                    Ok(s) => self.save_session(s)?,
                    // A wrong password or code: forget them, as a new setup brings new ones.
                    Err(RemoteError::Auth(_)) | Err(RemoteError::Permanent(_)) => self.save_creds(Map::new())?,
                    Err(_) => {}
                }
                result?
            }
        };
        let s = Arc::new(session);
        *guard = Some(s.clone());
        Ok(s)
    }

    fn save_creds(&self, m: Map<String, Value>) -> RemoteResult<()> {
        crate::credentials::save_to(&self.creds_file, "mega", m)
            .map_err(|e| RemoteError::Permanent(format!("cannot save the MEGA session: {e:#}")))
    }

    /// Keeps the session (and the master key, which decrypts the account's files), never the
    /// password.
    fn save_session(&self, s: &Session) -> RemoteResult<()> {
        let mut m = Map::new();
        m.insert("email".into(), json!(self.email));
        m.insert("user".into(), json!(s.user));
        m.insert("sid".into(), json!(s.sid));
        m.insert("master_key".into(), json!(b64e(&s.master)));
        self.save_creds(m)
    }

    /// Signs in with the e-mail and password (and 2FA code).
    async fn login(&self, password: &str, mfa: Option<&str>) -> RemoteResult<Session> {
        if self.email.is_empty() {
            return Err(RemoteError::Auth("no MEGA e-mail address in the settings".into()));
        }
        let pre = self.request(None, &json!({"a": "us0", "user": self.email})).await?;
        let (pw_key, uh) = match (pre["v"].as_i64(), pre["s"].as_str()) {
            (Some(2), Some(salt)) => {
                let salt = b64d(salt).ok_or_else(|| RemoteError::Permanent("MEGA sent a bad account salt".into()))?;
                let password = password.to_string();
                let (k, auth) = tokio::task::spawn_blocking(move || derive_v2(&password, &salt))
                    .await
                    .map_err(|e| RemoteError::Permanent(format!("sign-in task failed: {e}")))?;
                (k, b64e(&auth))
            }
            (Some(1), _) => {
                let (password, email) = (password.to_string(), self.email.clone());
                tokio::task::spawn_blocking(move || {
                    let k = prepare_key_v1(&password);
                    (k, b64e(&email_hash_v1(&email, &k)))
                })
                .await
                .map_err(|e| RemoteError::Permanent(format!("sign-in task failed: {e}")))?
            }
            _ => return Err(RemoteError::Permanent("MEGA answered the sign-in in a way CloudDrop doesn't know".into())),
        };
        let mut cmd = json!({"a": "us", "user": self.email, "uh": uh});
        if let Some(code) = mfa {
            cmd["mfa"] = json!(code);
        }
        let r = match self.request(None, &cmd).await {
            Err(RemoteError::NotFound) => {
                return Err(RemoteError::Auth("MEGA did not accept this e-mail address and password".into()));
            }
            Err(RemoteError::Auth(_)) if mfa.is_none() => {
                return Err(RemoteError::Auth(
                    "this MEGA account uses two-factor authentication: sign in again with the current code".into(),
                ));
            }
            other => other?,
        };
        let bad = || RemoteError::Permanent("MEGA's sign-in answer did not check out".into());
        let mut master: [u8; 16] = r["k"].as_str().and_then(b64_array).ok_or_else(bad)?;
        ecb_decrypt(&pw_key, &mut master);
        let user = r["u"].as_str().ok_or_else(bad)?.to_string();
        let Some(csid) = r["csid"].as_str().and_then(b64d) else {
            return Err(RemoteError::Permanent(
                "this MEGA account isn't fully set up: sign in once at mega.nz first".into(),
            ));
        };
        let mut privk = r["privk"].as_str().and_then(b64d).filter(|p| p.len().is_multiple_of(16)).ok_or_else(bad)?;
        ecb_decrypt(&master, &mut privk);
        let rsa = RsaKey::parse(&privk).ok_or_else(bad)?;
        let sid = tokio::task::spawn_blocking(move || rsa.session_id(&csid))
            .await
            .map_err(|e| RemoteError::Permanent(format!("sign-in task failed: {e}")))?
            .ok_or_else(bad)?;
        // The session id carries the user handle: a check that every key above was right.
        if std::str::from_utf8(&sid[16..27]).ok().and_then(b64d) != b64d(&user) {
            return Err(bad());
        }
        Ok(Session { sid: b64e(&sid), master, user })
    }

    // --- The node tree

    /// Makes sure the cached tree is at most `max_age` old.
    async fn fresh_tree(&self, max_age: Duration) -> RemoteResult<()> {
        let is_fresh = |t: &Option<Tree>| t.as_ref().is_some_and(|t| t.fetched.elapsed() < max_age);
        if is_fresh(&self.st.tree.lock().unwrap()) {
            return Ok(());
        }
        let _one_at_a_time = self.st.refreshing.lock().await;
        if is_fresh(&self.st.tree.lock().unwrap()) {
            return Ok(());
        }
        let s = self.session().await?;
        let r = self.api(json!({"a": "f", "c": 1, "r": 1})).await?;
        let mut share_keys = HashMap::new();
        for ok in r["ok"].as_array().into_iter().flatten() {
            if let (Some(h), Some(mut k)) = (ok["h"].as_str(), ok["k"].as_str().and_then(b64_array::<16>)) {
                ecb_decrypt(&s.master, &mut k);
                share_keys.insert(h.to_string(), k);
            }
        }
        let mut nodes = HashMap::new();
        let mut root = None;
        for v in r["f"].as_array().into_iter().flatten() {
            if let Some(n) = parse_node(&s, &share_keys, v) {
                if n.kind == 2 {
                    root = Some(n.handle.clone());
                }
                nodes.insert(n.handle.clone(), n);
            }
        }
        let root = root.ok_or_else(|| RemoteError::Transient("MEGA did not send the cloud drive".into()))?;
        tracing::debug!("MEGA: {} nodes", nodes.len());
        *self.st.tree.lock().unwrap() = Some(Tree { nodes, root, fetched: Instant::now() });
        Ok(())
    }

    /// Reads the cached tree (fetching it when older than [`TREE_TTL`]).
    async fn with_tree<T>(&self, f: impl FnOnce(&Tree) -> T) -> RemoteResult<T> {
        self.fresh_tree(TREE_TTL).await?;
        Ok(f(self.st.tree.lock().unwrap().as_ref().expect("tree")))
    }

    /// A node by handle; an unknown one fetches the tree again first (at most every few seconds).
    async fn node(&self, handle: &str) -> RemoteResult<Node> {
        if let Some(n) = self.with_tree(|t| t.nodes.get(handle).cloned()).await? {
            return Ok(n);
        }
        self.fresh_tree(MISS_REFETCH).await?;
        self.with_tree(|t| t.nodes.get(handle).cloned()).await?.ok_or(RemoteError::NotFound)
    }

    /// A folder (or the root) that isn't in the rubbish bin.
    async fn folder(&self, handle: &str) -> RemoteResult<Node> {
        let n = self.node(handle).await?;
        let trashed = self.with_tree(|t| t.trashed(handle)).await?;
        if n.kind == 0 || n.kind == 4 || trashed {
            return Err(RemoteError::NotFound);
        }
        Ok(n)
    }

    /// Puts nodes from a `p` answer into the cached tree; returns them.
    fn apply_nodes(&self, s: &Session, answer: &Value) -> Vec<RemoteMeta> {
        let mut guard = self.st.tree.lock().unwrap();
        let Some(tree) = guard.as_mut() else { return vec![] };
        let parsed: Vec<Node> = answer["f"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| parse_node(s, &HashMap::new(), v))
            .collect();
        for n in &parsed {
            tree.nodes.insert(n.handle.clone(), n.clone());
        }
        parsed.iter().map(|n| tree.meta(n)).collect()
    }

    fn update_node(&self, handle: &str, f: impl FnOnce(&mut Node)) -> Option<RemoteMeta> {
        let mut guard = self.st.tree.lock().unwrap();
        let tree = guard.as_mut()?;
        f(tree.nodes.get_mut(handle)?);
        Some(tree.meta(&tree.nodes[handle]))
    }

    // --- The provider contract

    /// (limit, usage, usage of the cloud drive). Versions count towards usage.
    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        let root = self.top_parent().await?;
        let r = self.api(json!({"a": "uq", "strg": 1, "v": 2})).await?;
        let used = r["cstrg"].as_u64().unwrap_or(0);
        let in_drive = r["cstrgn"][&root][0].as_u64().unwrap_or(used);
        Ok((r["mstrg"].as_u64(), used, in_drive))
    }

    /// The cloud drive's root.
    pub async fn top_parent(&self) -> RemoteResult<String> {
        self.with_tree(|t| t.root.clone()).await
    }

    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        let n = self.node(id).await?;
        self.with_tree(|t| t.meta(&n)).await
    }

    /// Items named `name` directly inside `parent`, not in the rubbish bin. MEGA allows several
    /// items with one name, so there can be more than one.
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        if self.node(parent).await.is_err() {
            return Ok(vec![]);
        }
        self.with_tree(|t| {
            let mut found: Vec<RemoteMeta> = t
                .nodes
                .values()
                .filter(|n| n.parent == parent && n.kind <= 1 && n.name() == Some(name))
                .filter(|n| !folder || n.kind == 1)
                .map(|n| t.meta(n))
                .filter(|m| !m.trashed)
                .collect();
            found.sort_by(|a, b| a.id.cmp(&b.id));
            found
        })
        .await
    }

    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        self.folder(parent).await?;
        let s = self.session().await?;
        let key: [u8; 16] = rand::random();
        let mut attrs = Map::new();
        attrs.insert("n".into(), json!(name));
        let mut wrapped = key;
        ecb_encrypt(&s.master, &mut wrapped);
        let r = self
            .api(json!({"a": "p", "t": parent, "n": [{
                "h": "xxxxxxxx", "t": 1, "a": encrypt_attrs(&key, &attrs), "k": b64e(&wrapped),
            }]}))
            .await?;
        self.apply_nodes(&s, &r)
            .into_iter()
            .find(|m| m.folder)
            .ok_or_else(|| RemoteError::Transient("MEGA did not return the new folder".into()))
    }

    /// Renames and/or moves an item. `parents` = (new, old) when moving.
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        let node = self.node(id).await?;
        if let Some(name) = name.filter(|n| Some(*n) != node.name()) {
            let (Some(mut attrs), Some(key)) = (node.attrs.clone(), node.key.as_deref().and_then(attr_key)) else {
                return Err(RemoteError::Permanent("CloudDrop cannot decrypt this MEGA item".into()));
            };
            attrs.insert("n".into(), json!(name));
            self.api(json!({"a": "a", "n": id, "at": encrypt_attrs(&key, &attrs)})).await?;
            self.update_node(id, |n| n.attrs = Some(attrs));
        }
        if let Some((to, _)) = parents.filter(|(to, _)| *to != node.parent) {
            self.folder(to).await?;
            self.api(json!({"a": "m", "n": id, "t": to})).await?;
            self.update_node(id, |n| n.parent = to.to_string());
        }
        self.get(id).await
    }

    /// Copies the file to "<stem> (replaced YYYY-MM-DD).<ext>" next to it, so replacing its
    /// content never loses the old one (MEGA's versions can be switched off and deleted).
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        let node = self.node(id).await?;
        let (Some(name), Some(mut attrs), Some(key)) = (node.name(), node.attrs.clone(), node.key.clone()) else {
            return Err(RemoteError::Permanent("CloudDrop cannot decrypt this MEGA item".into()));
        };
        if node.kind != 0 {
            return Ok(());
        }
        let copy_name = replaced_name(name, &today());
        // Already kept (a retry after a crash): the copy shares the file's key.
        let kept = self
            .with_tree(|t| {
                t.nodes.values().any(|n| {
                    n.parent == node.parent && n.name() == Some(copy_name.as_str()) && n.key.as_ref() == Some(&key)
                })
            })
            .await?;
        if kept {
            return Ok(());
        }
        let s = self.session().await?;
        attrs.insert("n".into(), json!(copy_name));
        let attr_key = attr_key(&key).expect("file key");
        let mut wrapped = key.clone();
        ecb_encrypt(&s.master, &mut wrapped);
        let r = self
            .api(json!({"a": "p", "t": node.parent, "n": [{
                "h": id, "t": 0, "a": encrypt_attrs(&attr_key, &attrs), "k": b64e(&wrapped),
            }]}))
            .await?;
        self.apply_nodes(&s, &r);
        Ok(())
    }

    /// Opens an upload; returns the session (JSON with the upload URL, the file's key and nonce,
    /// and where it goes). `existing` = the new content replaces that file.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        _mime: &str,
    ) -> RemoteResult<String> {
        let parent = parents.0;
        self.folder(parent).await?;
        if let Some(id) = existing {
            let n = self.node(id).await?;
            if n.kind != 0 || self.with_tree(|t| t.trashed(id)).await? {
                return Err(RemoteError::NotFound);
            }
        }
        let r = self.api(json!({"a": "u", "s": size, "ssl": 2})).await?;
        let url = r["p"]
            .as_str()
            .ok_or_else(|| RemoteError::Transient("MEGA did not return an upload address".into()))?;
        self.prune_progress();
        let session = UploadSession {
            id: hex::encode(rand::random::<[u8; 12]>()),
            url: url.to_string(),
            key: b64e(&rand::random::<[u8; 16]>()),
            nonce: b64e(&rand::random::<[u8; 8]>()),
            size,
            parent: parent.to_string(),
            name: name.to_string(),
            ov: existing.map(str::to_string),
        };
        Ok(serde_json::to_string(&session).expect("session json"))
    }

    /// What the upload server has confirmed so far, as recorded after each chunk. NotFound =
    /// start again (the upload URL expired, or the record is unusable).
    pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus> {
        let sess = parse_session(session, total)?;
        let progress = self.load_progress(&sess.id);
        if progress.token.is_some() {
            return self.finish_upload(&sess, progress).await.map(UploadStatus::Done);
        }
        if progress.offset > 0 && progress.offset >= total {
            return Err(RemoteError::NotFound);
        }
        Ok(UploadStatus::Next(progress.offset))
    }

    /// Encrypts and sends `data` (from byte `start`) up to the last MEGA chunk boundary in it,
    /// or to the end of the file; returns where to continue from.
    pub async fn put_chunk(&self, session: &str, data: bytes::Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus> {
        let sess = parse_session(session, total)?;
        let mut progress = self.load_progress(&sess.id);
        if progress.token.is_some() {
            return self.finish_upload(&sess, progress).await.map(UploadStatus::Done);
        }
        if start != progress.offset {
            return Ok(UploadStatus::Next(progress.offset));
        }
        let end = start + data.len() as u64;
        if end > total {
            return Err(RemoteError::Permanent("upload chunk goes past the end of the file".into()));
        }
        let stop = if end == total { total } else { chunk_floor(end) };
        if stop <= start && total > 0 {
            return Err(RemoteError::Permanent(format!(
                "MEGA uploads need chunks of at least {} MiB (chunk_size)",
                CHUNK_MAX >> 20
            )));
        }
        let bad = || RemoteError::Permanent("the upload session is damaged".into());
        let key: [u8; 16] = b64_array(&sess.key).ok_or_else(bad)?;
        let nonce: [u8; 8] = b64_array(&sess.nonce).ok_or_else(bad)?;
        let mac_so_far: [u8; 16] = if progress.mac.is_empty() { [0; 16] } else { b64_array(&progress.mac).ok_or_else(bad)? };

        let mut body = data.slice(..(stop - start) as usize).to_vec();
        let (body, mac) = tokio::task::spawn_blocking(move || {
            let mut macs = Macs::new(&key, nonce, start, mac_so_far);
            macs.update(&body);
            ctr_apply(&aes(&key), &nonce, start, &mut body);
            (body, macs.finish())
        })
        .await
        .map_err(|e| RemoteError::Permanent(format!("encrypt task failed: {e}")))?;

        let resp = self
            .http
            .post(format!("{}/{start}", sess.url))
            .timeout(Duration::from_secs(600))
            .body(body)
            .send()
            .await
            .map_err(net_error)?;
        let status = resp.status();
        if matches!(status.as_u16(), 403 | 404 | 410) {
            return Err(RemoteError::NotFound);
        }
        if !status.is_success() {
            return Err(http_error(status, &resp));
        }
        let answer = resp.bytes().await.map_err(net_error)?;
        let token = upload_answer(&answer)?;
        progress.offset = stop;
        progress.mac = b64e(&mac);
        progress.token = token;
        if progress.token.is_none() && stop == total {
            // The last chunk went in but no token came back: this upload can't be finished.
            self.forget_progress(&sess.id);
            return Err(RemoteError::NotFound);
        }
        self.save_progress(&sess.id, &progress)?;
        if progress.token.is_some() {
            return self.finish_upload(&sess, progress).await.map(UploadStatus::Done);
        }
        Ok(UploadStatus::Next(stop))
    }

    /// Creates the file node from a completed upload (`p`), replacing `ov` if given.
    async fn finish_upload(&self, sess: &UploadSession, mut progress: Progress) -> RemoteResult<RemoteMeta> {
        let bad = || RemoteError::Permanent("the upload session is damaged".into());
        let file_key = FileKey {
            aes: b64_array(&sess.key).ok_or_else(bad)?,
            nonce: b64_array(&sess.nonce).ok_or_else(bad)?,
            mac: condense(&b64_array(&progress.mac).ok_or_else(bad)?),
        }
        .pack();
        let mine = |t: &Tree| {
            let mut v: Vec<&Node> = t
                .nodes
                .values()
                .filter(|n| n.kind == 0 && n.parent == sess.parent && n.key.as_deref() == Some(&file_key[..]))
                .collect();
            v.sort_by_key(|n| n.handle.clone());
            v.first().map(|n| t.meta(n))
        };
        if progress.finishing {
            // An earlier attempt may have created the node before its answer was lost.
            self.fresh_tree(Duration::ZERO).await?;
            if let Some(m) = self.with_tree(mine).await? {
                self.forget_progress(&sess.id);
                return Ok(m);
            }
        }
        progress.finishing = true;
        self.save_progress(&sess.id, &progress)?;

        let s = self.session().await?;
        let token = progress.token.clone().ok_or_else(bad)?;
        let mut attrs = Map::new();
        attrs.insert("n".into(), json!(sess.name));
        let attrs = encrypt_attrs(&FileKey::unpack(&file_key).expect("key").aes, &attrs);
        let mut wrapped = file_key;
        ecb_encrypt(&s.master, &mut wrapped);
        let mut ov = sess.ov.clone();
        if let Some(old) = &ov {
            // A previous version lives in the same folder: move the old file first if needed.
            match self.node(old).await {
                Ok(n) if n.parent != sess.parent => {
                    self.api(json!({"a": "m", "n": old, "t": sess.parent})).await?;
                    self.update_node(old, |n| n.parent = sess.parent.clone());
                }
                Ok(_) => {}
                Err(RemoteError::NotFound) => ov = None,
                Err(e) => return Err(e),
            }
        }
        let put = |ov: &Option<String>| {
            let mut n = json!({"h": token, "t": 0, "a": attrs, "k": b64e(&wrapped)});
            if let Some(old) = ov {
                n["ov"] = json!(old);
            }
            json!({"a": "p", "t": sess.parent, "n": [n]})
        };
        let r = match self.api(put(&ov)).await {
            // The replaced file vanished meanwhile: create the file without it.
            Err(RemoteError::NotFound) if ov.is_some() => self.api(put(&None)).await?,
            other => other?,
        };
        self.apply_nodes(&s, &r);
        self.forget_progress(&sess.id);
        match self.with_tree(mine).await? {
            Some(m) => Ok(m),
            None => Err(RemoteError::Transient("MEGA did not return the uploaded file".into())),
        }
    }

    /// A download URL for the file, with its key and size.
    async fn download_url(&self, id: &str, fresh: bool) -> RemoteResult<(String, FileKey, u64)> {
        let node = self.node(id).await?;
        let key = node
            .key
            .as_deref()
            .filter(|_| node.kind == 0)
            .and_then(FileKey::unpack)
            .ok_or_else(|| RemoteError::Permanent("CloudDrop cannot decrypt this MEGA file".into()))?;
        if !fresh
            && let Some((url, at)) = self.st.urls.lock().unwrap().get(id)
            && at.elapsed() < URL_TTL
        {
            return Ok((url.clone(), key, node.size));
        }
        let r = self.api(json!({"a": "g", "g": 1, "n": id, "ssl": 2})).await?;
        if let Some(e) = r["e"].as_i64().filter(|e| *e < 0) {
            return Err(api_error(e));
        }
        let url = r["g"]
            .as_str()
            .ok_or_else(|| RemoteError::Transient("MEGA did not return a download address".into()))?
            .to_string();
        let size = r["s"].as_u64().unwrap_or(node.size);
        self.st.urls.lock().unwrap().insert(id.to_string(), (url.clone(), Instant::now()));
        Ok((url, key, size))
    }

    /// GETs bytes `start..=last` of the file's encrypted content, getting a new URL once if the
    /// old one stopped working.
    async fn fetch(&self, id: &str, start: u64, last: u64, timeout: Duration) -> RemoteResult<(Response, FileKey)> {
        let mut fresh = false;
        loop {
            let (url, key, _) = self.download_url(id, fresh).await?;
            let resp = self
                .http
                .get(format!("{url}/{start}-{last}"))
                .timeout(timeout)
                .send()
                .await
                .map_err(net_error)?;
            let status = resp.status();
            if status.is_success() {
                return Ok((resp, key));
            }
            if matches!(status.as_u16(), 403 | 404 | 410) && !fresh {
                fresh = true;
                continue;
            }
            return Err(http_error(status, &resp));
        }
    }

    /// Bytes `start..end` of a file's content (for downloading placeholders on demand).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes> {
        let size = self.node(id).await?.size;
        let end = end.min(size);
        if end <= start {
            return Ok(bytes::Bytes::new());
        }
        let from = start / 16 * 16;
        let (resp, key) = self.fetch(id, from, end - 1, Duration::from_secs(600)).await?;
        let mut data = resp
            .bytes()
            .await
            .map_err(|e| RemoteError::Transient(format!("download interrupted: {e}")))?
            .to_vec();
        if data.len() as u64 != end - from {
            return Err(RemoteError::Transient("MEGA sent a short range".into()));
        }
        ctr_apply(&aes(&key.aes), &key.nonce, from, &mut data);
        Ok(bytes::Bytes::from(data).slice((start - from) as usize..))
    }

    /// Streams a file's content into `out`, checking MEGA's MAC; returns (its SHA-256, bytes
    /// written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        let size = self.node(id).await?.size;
        let write_err = |e: std::io::Error| RemoteError::Permanent(format!("cannot write download: {e}"));
        let mut hasher = Hasher::new(HashKind::Sha256);
        let mut total = 0u64;
        let key = if size == 0 {
            self.download_url(id, false).await?.1
        } else {
            let (resp, key) = self.fetch(id, 0, size - 1, Duration::from_secs(24 * 3600)).await?;
            let cipher = aes(&key.aes);
            let mut macs = Macs::new(&key.aes, key.nonce, 0, [0; 16]);
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let mut chunk = chunk.map_err(|e| RemoteError::Transient(format!("download interrupted: {e}")))?.to_vec();
                if total + chunk.len() as u64 > size {
                    return Err(RemoteError::Transient("MEGA sent more than the file holds".into()));
                }
                ctr_apply(&cipher, &key.nonce, total, &mut chunk);
                macs.update(&chunk);
                hasher.update(&chunk);
                total += chunk.len() as u64;
                out.write_all(&chunk).await.map_err(write_err)?;
            }
            if total != size {
                return Err(RemoteError::Transient("download interrupted".into()));
            }
            if condense(&macs.finish()) != key.mac {
                return Err(RemoteError::Permanent("the file in MEGA failed its integrity check".into()));
            }
            key
        };
        if size == 0 && key.mac != [0; 8] {
            return Err(RemoteError::Permanent("the file in MEGA failed its integrity check".into()));
        }
        out.flush().await.map_err(write_err)?;
        Ok((hasher.finish(), total))
    }

    // --- Upload progress files

    fn progress_path(&self, id: &str) -> PathBuf {
        self.uploads_dir.join(format!("{id}.json"))
    }

    fn load_progress(&self, id: &str) -> Progress {
        std::fs::read(self.progress_path(id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_progress(&self, id: &str, p: &Progress) -> RemoteResult<()> {
        let io = |e: anyhow::Error| RemoteError::Permanent(format!("cannot record upload progress: {e:#}"));
        std::fs::create_dir_all(&self.uploads_dir).map_err(|e| io(e.into()))?;
        crate::util::write_private(&self.progress_path(id), &serde_json::to_vec(p).expect("progress json")).map_err(io)
    }

    fn forget_progress(&self, id: &str) {
        let _ = std::fs::remove_file(self.progress_path(id));
    }

    /// Removes progress files of uploads abandoned over a month ago.
    fn prune_progress(&self) {
        let Ok(dir) = std::fs::read_dir(&self.uploads_dir) else { return };
        for e in dir.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(30 * 24 * 3600));
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// The session kept in credentials.json, if one is.
fn saved_session(creds: &Map<String, Value>) -> Option<Session> {
    Some(Session {
        sid: creds.get("sid")?.as_str()?.to_string(),
        master: b64_array(creds.get("master_key")?.as_str()?)?,
        user: creds.get("user")?.as_str()?.to_string(),
    })
}

fn parse_session(session: &str, total: u64) -> RemoteResult<UploadSession> {
    let s: UploadSession = serde_json::from_str(session).map_err(|_| RemoteError::NotFound)?;
    if s.size != total {
        return Err(RemoteError::NotFound);
    }
    Ok(s)
}

/// What an upload server answers a chunk: nothing (or "0") until the last one, which gets the
/// completion token (36 characters; binary from some servers); a negative number is an error.
fn upload_answer(body: &[u8]) -> RemoteResult<Option<String>> {
    let text = std::str::from_utf8(body).map(str::trim);
    match text {
        Ok("") | Ok("0") => Ok(None),
        Ok(t) if t.starts_with('-') && t[1..].bytes().all(|b| b.is_ascii_digit()) => {
            Err(upload_error(t.parse().unwrap_or(-1)))
        }
        Ok(t) if t.len() == 36 && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') => {
            Ok(Some(t.to_string()))
        }
        _ if body.len() == 36 => Ok(Some(b64e(body))),
        _ => Err(RemoteError::Transient("unexpected answer from MEGA's upload server".into())),
    }
}

/// Decrypts a node from `f` or `p`. Its key is "owner:key", or several separated by "/"; the
/// account's own entry is wrapped with the master key, a share's with that share's key.
fn parse_node(s: &Session, share_keys: &HashMap<String, [u8; 16]>, v: &Value) -> Option<Node> {
    let handle = v["h"].as_str()?.to_string();
    let kind = v["t"].as_i64()?;
    let mut node = Node {
        handle,
        parent: v["p"].as_str().unwrap_or_default().to_string(),
        kind,
        attrs: None,
        key: None,
        size: v["s"].as_u64().unwrap_or(0),
    };
    if kind > 1 {
        return Some(node);
    }
    let key = v["k"].as_str().unwrap_or_default().split('/').find_map(|entry| {
        let (owner, k) = entry.split_once(':')?;
        let wrap = if owner == s.user { s.master } else { *share_keys.get(owner)? };
        let mut k = b64d(k).filter(|k| k.len() == if kind == 0 { 32 } else { 16 })?;
        ecb_decrypt(&wrap, &mut k);
        Some(k)
    });
    if let Some(k) = key {
        node.attrs = attr_key(&k).and_then(|ak| decrypt_attrs(&ak, v["a"].as_str()?));
        node.key = Some(k);
    }
    Some(node)
}

async fn solve_challenge(header: &str) -> RemoteResult<String> {
    // "1:<easiness>:<timestamp>:<token>"
    let parts: Vec<&str> = header.trim().split(':').collect();
    let (Some(&"1"), Some(easiness), Some(token)) =
        (parts.first(), parts.get(1).and_then(|e| e.parse::<u8>().ok()), parts.get(3))
    else {
        return Err(RemoteError::Transient("MEGA asked for a proof of work CloudDrop doesn't know".into()));
    };
    let token = token.to_string();
    let nonce = tokio::task::spawn_blocking({
        let token = token.clone();
        move || hashcash(&token, easiness)
    })
    .await
    .map_err(|e| RemoteError::Permanent(format!("proof-of-work task failed: {e}")))?
    .ok_or_else(|| RemoteError::Transient("MEGA's proof of work could not be done".into()))?;
    Ok(format!("1:{token}:{nonce}"))
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
    let days = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86400) as i64;
    civil_date(days)
}

/// Days since 1970-01-01 → "YYYY-MM-DD" (Howard Hinnant's civil_from_days).
fn civil_date(days: i64) -> String {
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

    fn unhex(s: &str) -> Vec<u8> {
        hex::decode(s.replace(' ', "")).unwrap()
    }

    /// The SDK's `CtrCrypt_RegressionVectors` (tests/unit/Crypto_test.cpp): key 0f0e…00, and
    /// ctriv 0x0102030405060708 as the SDK stores it in memory (little-endian).
    fn sdk_cipher() -> ([u8; 16], [u8; 8]) {
        let key: [u8; 16] = std::array::from_fn(|i| 15 - i as u8);
        (key, 0x0102030405060708u64.to_le_bytes())
    }

    /// Encrypts like the SDK's `ctr_crypt(…, mac, encrypt)`: returns (ciphertext, chunk MAC).
    fn sdk_ctr(plain: &[u8], pos: u64) -> (Vec<u8>, [u8; 16]) {
        let (key, nonce) = sdk_cipher();
        let mut macs = Macs::new(&key, nonce, chunk_floor(pos), [0; 16]);
        macs.pos = pos;
        macs.update(plain);
        if macs.in_block > 0 {
            macs.mac_block();
        }
        let mut data = plain.to_vec();
        ctr_apply(&aes(&key), &nonce, pos, &mut data);
        (data, macs.chunk)
    }

    #[test]
    fn ctr_and_chunk_macs_match_the_sdk_vectors() {
        let (ct, mac) = sdk_ctr(&unhex("00112233445566778899AABBCCDDEEFF"), 0);
        assert_eq!(ct, unhex("74715294CD039CC2DB115C360582C4CC"));
        assert_eq!(mac.to_vec(), unhex("F4BB477DBF92CFF2DEA959DA9FD4F4C2"));

        let plain: Vec<u8> = (0..48).collect();
        let (ct, mac) = sdk_ctr(&plain, 0);
        assert_eq!(
            ct,
            unhex(
                "746172A48D53FCB25B81FC86 C552243C685504C065519247 340D5FBFE7166DA641C8EFC8 24A89F7D798403B5A6ED9BD4"
            )
        );
        assert_eq!(mac.to_vec(), unhex("A9166E2F3109F0CBEA56D338BEDBF895"));

        let (ct, mac) = sdk_ctr(&unhex("DEADBEEFCAFEBABE0123456789ABCDEF"), 64);
        assert_eq!(ct, unhex("15F9B7DBBF87D8630A28B29350FEF268"));
        assert_eq!(mac.to_vec(), unhex("B9C5E482138DAAC609D53C5395843F71"));

        let (ct, _) = sdk_ctr(&unhex("AABBCCDDEEFF00112233445566778899"), 0);
        assert_eq!(ct, unhex("DEDBBC7A67A9FAA471BBB2D8AF28A2AA"));

        // 20 bytes: the MAC zero-pads the last block.
        let plain: Vec<u8> = (0..20u8).map(|i| 0xF0u8.wrapping_add(i)).collect();
        let (ct, mac) = sdk_ctr(&plain, 0);
        assert_eq!(ct, unhex("849182547DA30C42AB710C7635A2D4CC784514D0"));
        assert_eq!(mac.to_vec(), unhex("52DF3197967374 95C02D27F5FB80E1BD"));
    }

    #[test]
    fn condensed_mac_folds_chunk_macs() {
        // Fold of the two SDK chunk MACs above, from a Python port of the SDK's `macsmac`.
        let (key, _) = sdk_cipher();
        let c = aes(&key);
        let mut file = [0u8; 16];
        for m in ["F4BB477DBF92CFF2DEA959DA9FD4F4C2", "A9166E2F3109F0CBEA56D338BEDBF895"] {
            xor16(&mut file, &unhex(m));
            encrypt_block(&c, &mut file);
        }
        assert_eq!(hex::encode(condense(&file)), "ec88e3cc75a1dbb0");
    }

    #[test]
    fn chunk_boundaries() {
        let k = 1024;
        let ramp = [0, 128 * k, 384 * k, 768 * k, 1280 * k, 1920 * k, 2688 * k, 3584 * k, 4608 * k];
        for w in ramp.windows(2) {
            assert_eq!(chunk_end(w[0]), w[1]);
            assert_eq!(chunk_end(w[1] - 1), w[1]);
            assert_eq!(chunk_floor(w[1] - 1), w[0]);
            assert_eq!(chunk_floor(w[1]), w[1]);
        }
        assert_eq!(chunk_end(4608 * k), 5632 * k);
        assert_eq!(chunk_floor(8 << 20), 7680 * k, "8 MiB is not a boundary; 7.5 MiB is");
        assert_eq!(chunk_floor(10 << 30), (10 << 30) - 512 * k);
    }

    #[test]
    fn macs_stream_in_any_pieces() {
        let key = [7u8; 16];
        let nonce = [9u8; 8];
        let data: Vec<u8> = (0..(5 << 20) + 333u32).map(|i| (i * 31 % 251) as u8).collect();
        let whole = {
            let mut m = Macs::new(&key, nonce, 0, [0; 16]);
            m.update(&data);
            m.finish()
        };
        let mut m = Macs::new(&key, nonce, 0, [0; 16]);
        for piece in data.chunks(100_003) {
            m.update(piece);
        }
        assert_eq!(m.finish(), whole);
        // Resuming at a boundary with the MAC so far gives the same MAC.
        let cut = chunk_floor(3 << 20) as usize;
        let mut first = Macs::new(&key, nonce, 0, [0; 16]);
        first.update(&data[..cut]);
        let mut rest = Macs::new(&key, nonce, cut as u64, first.finish());
        rest.update(&data[cut..]);
        assert_eq!(rest.finish(), whole);
        assert_eq!(Macs::new(&key, nonce, 0, [0; 16]).finish(), [0; 16], "an empty file's MAC is zero");
    }

    #[test]
    fn ctr_round_trips_at_any_offset() {
        let c = aes(&[3u8; 16]);
        let nonce = [1u8; 8];
        let plain: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let mut whole = plain.clone();
        ctr_apply(&c, &nonce, 0, &mut whole);
        assert_ne!(whole, plain);
        let mut part = plain[37..500].to_vec();
        ctr_apply(&c, &nonce, 37, &mut part);
        assert_eq!(part, whole[37..500]);
        ctr_apply(&c, &nonce, 0, &mut whole);
        assert_eq!(whole, plain);
    }

    #[test]
    fn v2_key_derivation() {
        // PBKDF2-HMAC-SHA512, 100 000 rounds: the same value from Node's crypto.pbkdf2Sync and
        // Python's hashlib (both OpenSSL).
        let salt: Vec<u8> = (0..32).collect();
        let (k, auth) = derive_v2("correct horse battery staple", &salt);
        assert_eq!(hex::encode(k), "88aa99bab648e0a15a6dcfd127cb7d9f");
        assert_eq!(b64e(&auth), "F9Gj-gyqbKnMl-KglgC7wA");
    }

    #[test]
    fn v1_key_derivation() {
        // Self-consistency only: values from an independent Python port of the SDK's `pw_key`
        // and `stringhash64` (no published vectors exist), including astral characters.
        for (pw, key, uh) in [
            ("password", "640339725e6ebd13a25f0052129f7cb1", "hcVR9BZsYoY"),
            ("a much longer pass phrase, over sixteen", "981853af6539cf6e79a936cedf36ab45", "oKfwZfNszvs"),
            ("pässwörd €𝄞", "d09b38c6c59dd37d8009e5f02af32185", "xuwunRdFwas"),
        ] {
            let k = prepare_key_v1(pw);
            assert_eq!(hex::encode(k), key, "{pw}");
            assert_eq!(b64e(&email_hash_v1("user@example.com", &k)), uh, "{pw}");
        }
    }

    #[test]
    fn attributes_round_trip() {
        let key: [u8; 16] = std::array::from_fn(|i| i as u8);
        let mut attrs = Map::new();
        attrs.insert("n".into(), json!("report.pdf"));
        let enc = encrypt_attrs(&key, &attrs);
        // From a Python port (OpenSSL AES-CBC, zero IV).
        assert_eq!(enc, "pYwltwx0hwfhQyMKBJyeIJqHayOitcwNl-LYdIs8NPg");
        assert_eq!(decrypt_attrs(&key, &enc), Some(attrs));
        assert_eq!(decrypt_attrs(&[1; 16], &enc), None, "a wrong key gives no attributes");
        let mut long = Map::new();
        long.insert("n".into(), json!("ünïcødé name that is longer than a few blocks.txt"));
        long.insert("c".into(), json!("fingerprint"));
        assert_eq!(decrypt_attrs(&key, &encrypt_attrs(&key, &long)), Some(long));
    }

    #[test]
    fn file_keys_pack_and_unpack() {
        let k = FileKey { aes: [1; 16], nonce: [2; 8], mac: [3; 8] };
        let packed = k.pack();
        assert_eq!(&packed[..16], &[3u8, 3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2]);
        assert_eq!(FileKey::unpack(&packed), Some(k));
        assert_eq!(attr_key(&packed), Some([1; 16]));
        assert_eq!(attr_key(&[5; 16]), Some([5; 16]));
        assert_eq!(FileKey::unpack(&[0; 31]), None);
    }

    #[test]
    fn rsa_crt_decrypts() {
        // Textbook key: p 61, q 53, e 17, d 2753, u = p⁻¹ mod q = 20.
        let mpi = |n: u32| {
            let b = BigUint::from(n);
            let mut v = (b.bits() as u16).to_be_bytes().to_vec();
            v.extend(b.to_bytes_be());
            v
        };
        let blob: Vec<u8> = [61, 53, 2753, 20].into_iter().flat_map(mpi).collect();
        let key = RsaKey::parse(&blob).unwrap();
        for m in [0u32, 1, 65, 3000, 3232] {
            let c = BigUint::from(m).modpow(&BigUint::from(17u8), &BigUint::from(3233u32));
            assert_eq!(key.decrypt(&c), BigUint::from(m));
        }
        assert!(read_mpis(&[0x00, 0x10, 0x01], 1).is_none(), "truncated");
    }

    #[test]
    fn hashcash_matches_the_sdk_rule() {
        // Tokens from the SDK's hashcash_test.cpp; the smallest valid nonces from a Python port
        // of its `gencash`/`validateHashcash`.
        assert_eq!(hashcash("wFqIT_wY3tYKcrm5zqwaUoWym3ZCz32cCsrJOgYBgihtpaWUhGyWJ--EY-zfwI-i", 220).as_deref(), Some("AAAAAg"));
        assert_eq!(hashcash("K4QHo4I6XmnLNNsFqutTwObWZMClxf7ov--5OHLdGXSMHRwN8bLvrUTlpnhXVdtO", 220).as_deref(), Some("AAAAAw"));
        assert_eq!(hashcash("short", 220), None);
    }

    #[test]
    fn upload_server_answers() {
        assert_eq!(upload_answer(b""), Ok(None));
        assert_eq!(upload_answer(b"0"), Ok(None));
        let token = "a".repeat(36);
        assert_eq!(upload_answer(token.as_bytes()), Ok(Some(token)));
        assert_eq!(upload_answer(&[0xff; 36]), Ok(Some(b64e(&[0xff; 36]))));
        assert_eq!(upload_answer(b"-4"), Err(RemoteError::NotFound));
        assert!(matches!(upload_answer(b"-17"), Err(RemoteError::Quota(_))));
        assert!(matches!(upload_answer(b"what"), Err(RemoteError::Transient(_))));
    }

    #[test]
    fn errors_map_to_remote_errors() {
        assert!(matches!(api_error(-15), RemoteError::Auth(_)));
        assert!(matches!(api_error(-3), RemoteError::RateLimited { .. }));
        assert!(matches!(api_error(-4), RemoteError::RateLimited { .. }));
        assert!(matches!(api_error(-17), RemoteError::Quota(_)));
        assert_eq!(api_error(-9), RemoteError::NotFound);
        assert!(matches!(api_error(-2), RemoteError::Permanent(_)));
    }

    #[test]
    fn names_dates_and_base64() {
        assert_eq!(replaced_name("report.pdf", "2026-10-21"), "report (replaced 2026-10-21).pdf");
        assert_eq!(replaced_name(".bashrc", "2026-10-21"), ".bashrc (replaced 2026-10-21)");
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(20_747), "2026-10-21");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(b64d("+/8="), b64d("-_8"));
        assert_eq!(today().len(), 10);
    }

    #[test]
    fn config_section_stays_out_of_untouched_files() {
        assert!(MegaConfig::default().is_default());
        let c = MegaConfig { email: "a@b.c".into(), ..Default::default() };
        assert_eq!(toml::to_string(&c).unwrap().trim(), "email = \"a@b.c\"");
        let back: MegaConfig = toml::from_str("email = \"a@b.c\"").unwrap();
        assert_eq!(back, c);
    }
}
