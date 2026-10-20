//! SFTP client: uploads into one directory on an SSH server. The engine reaches it through
//! [`crate::remote::Remote`].
//!
//! Pure Rust over SSH (`russh`, `russh-sftp`). Ids are paths relative to the configured base
//! directory ("" is the base itself), so they change with every rename and move
//! (`stable_ids: false`). There is no server-side hash: freeing space verifies by download.
//! Uploads go to `.clouddrop-<random>.part` beside the target and are renamed into place once
//! complete, so a half-written file never looks finished. Nothing is ever deleted: a name that
//! is already taken moves aside as `<stem> (replaced YYYY-MM-DD).<ext>` first.
//!
//! The server's host key is pinned (`[sftp] host_key`, a `SHA256:…` fingerprint); an empty
//! pin trusts the first key seen and writes it into config.toml.

use crate::config::Config;
use crate::hash::Hasher;
use crate::remote::{
    Capabilities, ContentHash, Field, FieldKind, HashKind, Provider, ProviderInfo, RemoteError, RemoteMeta,
    RemoteResult, SignIn, StorageQuota, UploadStatus,
};
use crate::util::{join_rel, name_of, numbered_name, parent_of};
use futures_util::StreamExt;
use russh::client;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Uploads continue at multiples of this (and the engine sends chunks of it).
const CHUNK_UNIT: usize = 1024 * 1024;
/// Bytes of writes (or reads) in flight on the connection at once. A cut can leave holes only
/// in the last this-many bytes of a partial file, so resuming starts at least this far back.
const IN_FLIGHT: usize = 2 * 1024 * 1024;
/// Largest read or write request (OpenSSH accepts 255 KiB with `limits@openssh.com`).
const MAX_REQUEST: u64 = 255 * 1024;
/// Request size every SFTP server must accept.
const MIN_REQUEST: u32 = 32 * 1024;
/// Downloads and copies move this much per pipelined batch.
const BATCH: u64 = 8 * 1024 * 1024;
/// Less free space than this on a failed write means the disk is full.
const FULL_BELOW: u64 = 4 * 1024 * 1024;
const PART_PREFIX: &str = ".clouddrop-";
const PART_SUFFIX: &str = ".part";

pub static INFO: ProviderInfo = ProviderInfo {
    id: "sftp",
    name: "SFTP server",
    caps: Capabilities {
        hash: None,
        pin_versions: false,
        stable_ids: false,
        resumable: true,
        case_insensitive: false,
        forbidden_chars: "",
        chunk_unit: CHUNK_UNIT,
    },
    sign_in: SignIn::Form(&FIELDS),
    web_url: |_, _| None,
};

static FIELDS: [Field; 8] = [
    Field { key: "host", label: "Server", kind: FieldKind::Text, required: true, hint: "sftp.example.com" },
    Field { key: "port", label: "Port", kind: FieldKind::Number, required: false, hint: "22" },
    Field { key: "user", label: "User name", kind: FieldKind::Text, required: true, hint: "Your account on the server" },
    Field {
        key: "path",
        label: "Folder on the server",
        kind: FieldKind::Text,
        required: false,
        hint: "/srv/backup or ~/backup; empty means your home folder",
    },
    Field {
        key: "key_file",
        label: "SSH key",
        kind: FieldKind::File,
        required: false,
        hint: "~/.ssh/id_ed25519; leave empty to use ssh-agent or a password",
    },
    Field {
        key: "password",
        label: "Password",
        kind: FieldKind::Secret,
        required: false,
        hint: "Only if the server signs in with a password",
    },
    Field {
        key: "passphrase",
        label: "Key passphrase",
        kind: FieldKind::Secret,
        required: false,
        hint: "Only if the SSH key has one",
    },
    Field {
        key: "host_key",
        label: "Server fingerprint",
        kind: FieldKind::Text,
        required: false,
        hint: "SHA256:…, filled in when you test the connection",
    },
];

/// `[sftp]` in config.toml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SftpConfig {
    pub host: String,
    #[serde(deserialize_with = "port_from_number_or_text")]
    pub port: u16,
    pub user: String,
    /// The directory CloudDrop's folder goes in; `~/…` and relative paths start at the
    /// account's home, empty means the home itself.
    pub path: String,
    /// Private key to sign in with; empty = ssh-agent, or the password.
    pub key_file: String,
    /// The server's host key fingerprint (`SHA256:…`), pinned on first use.
    pub host_key: String,
}

impl Default for SftpConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: String::new(),
            path: String::new(),
            key_file: String::new(),
            host_key: String::new(),
        }
    }
}

impl SftpConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Forms may hand the port over as text ("2222"); empty means 22.
fn port_from_number_or_text<'de, D: Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Port {
        Number(u16),
        Text(String),
    }
    match Port::deserialize(d)? {
        Port::Number(n) => Ok(n),
        Port::Text(t) if t.trim().is_empty() => Ok(22),
        Port::Text(t) => t.trim().parse().map_err(|_| serde::de::Error::custom("port must be a number")),
    }
}

/// The password and key passphrase (`credentials.json`, "sftp").
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    pub password: Option<String>,
    pub passphrase: Option<String>,
}

impl Secrets {
    pub fn from_map(m: &serde_json::Map<String, serde_json::Value>) -> Self {
        let get = |k: &str| m.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
        Secrets { password: get("password"), passphrase: get("passphrase") }
    }
}

/// An in-progress upload, kept by the engine as an opaque string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UploadSession {
    /// The partial file beside the target (`.clouddrop-<random>.part`).
    part: String,
    target: String,
    /// The file whose content this replaces, where it was before (it may have moved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replaces: Option<String>,
}

impl UploadSession {
    fn encode(&self) -> String {
        serde_json::to_string(self).expect("session serialises")
    }

    /// A session this client wrote, or NotFound (expired, or another provider's).
    fn decode(s: &str) -> RemoteResult<Self> {
        let session: UploadSession = serde_json::from_str(s).map_err(|_| RemoteError::NotFound)?;
        let part_name = name_of(&session.part);
        let ours = part_name.starts_with(PART_PREFIX)
            && part_name.ends_with(PART_SUFFIX)
            && parent_of(&session.part) == parent_of(&session.target)
            && check_id(&session.part).is_ok()
            && check_id(&session.target).is_ok()
            && !session.target.is_empty()
            && session.replaces.as_deref().is_none_or(|r| check_id(r).is_ok() && !r.is_empty());
        if ours { Ok(session) } else { Err(RemoteError::NotFound) }
    }
}

/// Where an interrupted upload continues, given the partial file's size: below it everything
/// arrived (writes in flight at the cut may have left holes only in the last [`IN_FLIGHT`]
/// bytes), rounded down to the chunk unit.
fn resume_offset(written: u64) -> u64 {
    let safe = written.saturating_sub(IN_FLIGHT as u64);
    safe - safe % CHUNK_UNIT as u64
}

/// Ids are relative paths below the base: no empty, "." or ".." parts, no leading slash.
fn check_id(id: &str) -> RemoteResult<()> {
    if id.is_empty() || id.split('/').all(|p| !p.is_empty() && p != "." && p != "..") {
        Ok(())
    } else {
        Err(RemoteError::Permanent(format!("\"{id}\" is not a path CloudDrop made")))
    }
}

fn child_id(parent: &str, name: &str) -> RemoteResult<String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(RemoteError::Permanent(format!("\"{name}\" cannot be a name on an SFTP server")));
    }
    check_id(parent)?;
    Ok(join_rel(parent, name))
}

/// "report.pdf" → "report (replaced 2026-10-21).pdf"; the n-th of a day gets " (n)" as well.
fn replaced_name(name: &str, date: &str, n: u32) -> String {
    let base = match name.rfind('.') {
        Some(i) if i > 0 => format!("{} (replaced {date}){}", &name[..i], &name[i..]),
        _ => format!("{name} (replaced {date})"),
    };
    if n > 1 { numbered_name(&base, n) } else { base }
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// The comparable part of a fingerprint: accepts `ssh-keygen -l` output as pasted.
fn normalize_fingerprint(s: &str) -> String {
    let s = s.trim();
    let token = s.split_whitespace().find(|t| t.starts_with("SHA256:")).unwrap_or(s);
    token.trim_end_matches('=').to_string()
}

fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

/// The base directory as the server should resolve it: `~/x` → `x` (relative paths start at
/// the home directory), empty → the home directory.
fn base_request(path: &str) -> String {
    let p = path.trim();
    match p {
        "" | "~" => ".".into(),
        _ => p.strip_prefix("~/").unwrap_or(p).to_string(),
    }
}

/// The connection broke (or never answered): reconnect before the next request.
fn is_connection_loss(e: &SftpError) -> bool {
    match e {
        SftpError::Status(s) => matches!(s.status_code, StatusCode::NoConnection | StatusCode::ConnectionLost),
        SftpError::IO(_) | SftpError::Timeout | SftpError::UnexpectedBehavior(_) | SftpError::UnexpectedPacket => true,
        SftpError::Limited(_) => false,
    }
}

/// What a failed SFTP request means for the engine. `what` completes "could not …";
/// `disk_full`: the server had (almost) no space left when it failed.
fn classify(e: &SftpError, what: &str, disk_full: bool) -> RemoteError {
    match e {
        SftpError::Status(s) => {
            let detail = match s.error_message.trim() {
                "" | "Failure" | "failure" => String::new(),
                m => format!(" ({m})"),
            };
            match s.status_code {
                StatusCode::NoSuchFile => RemoteError::NotFound,
                StatusCode::PermissionDenied => {
                    RemoteError::Permanent(format!("The SFTP server denied permission to {what}."))
                }
                StatusCode::Failure if disk_full => RemoteError::Quota("The SFTP server's disk is full.".into()),
                StatusCode::OpUnsupported => {
                    RemoteError::Permanent(format!("The SFTP server does not support what it takes to {what}."))
                }
                StatusCode::NoConnection | StatusCode::ConnectionLost => {
                    RemoteError::Transient("The connection to the SFTP server was lost.".into())
                }
                _ => RemoteError::Permanent(format!("The SFTP server could not {what}{detail}.")),
            }
        }
        SftpError::Timeout => RemoteError::Transient("The SFTP server stopped answering.".into()),
        SftpError::IO(m) | SftpError::UnexpectedBehavior(m) => {
            RemoteError::Transient(format!("The connection to the SFTP server was lost ({m})."))
        }
        SftpError::UnexpectedPacket => RemoteError::Transient("The SFTP server sent something unexpected.".into()),
        SftpError::Limited(m) => RemoteError::Permanent(format!("The SFTP server could not {what} ({m}).")),
    }
}

/// Failures of SSH itself (before any SFTP request).
fn classify_ssh(e: &russh::Error, server: &str) -> RemoteError {
    use russh::Error as E;
    match e {
        E::NoCommonAlgo { .. } | E::Version | E::UnknownAlgo | E::WrongServerSig | E::KexInit => {
            RemoteError::Permanent(format!("Cannot set up a secure connection to {server}: {e}."))
        }
        E::ChannelOpenFailure(_) | E::RequestDenied => {
            RemoteError::Permanent(format!("{server} does not offer SFTP to this account."))
        }
        _ => RemoteError::Transient(format!("Cannot reach the SFTP server {server}: {e}.")),
    }
}

fn status_ok(p: Packet) -> Result<(), SftpError> {
    match p {
        Packet::Status(s) if s.status_code == StatusCode::Ok => Ok(()),
        Packet::Status(s) => Err(SftpError::Status(s)),
        _ => Err(SftpError::UnexpectedPacket),
    }
}

/// An SSH string (length-prefixed) in an extension request.
fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// Accepts the server's host key if it matches the pin (any key when there is none) and
/// remembers which key it saw.
struct HostKeyCheck {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => fingerprint(key),
            PublicKeyOrCertificate::Certificate(cert) => fingerprint(&PublicKey::from(cert.public_key().clone())),
        };
        let ok = self.expected.as_deref().is_none_or(|e| e == fp);
        *self.seen.lock().unwrap() = Some(fp);
        Ok(ok)
    }
}

fn ssh_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        nodelay: true,
        ..Default::default()
    })
}

async fn tcp(host: &str, port: u16) -> RemoteResult<tokio::net::TcpStream> {
    let server = format!("{host}:{port}");
    match tokio::time::timeout(Duration::from_secs(20), tokio::net::TcpStream::connect((host, port))).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(RemoteError::Transient(format!("Cannot reach the SFTP server {server}: {e}."))),
        Err(_) => Err(RemoteError::Transient(format!("The SFTP server {server} did not answer."))),
    }
}

/// Connects to `host:port` far enough to see its host key, and returns the key's fingerprint
/// (`SHA256:…`) for the setup assistant to show and pin.
pub async fn fetch_host_key(host: &str, port: u16) -> RemoteResult<String> {
    let seen = Arc::new(Mutex::new(None));
    // Expect a key no server has, so the handshake stops right after showing it.
    let check = HostKeyCheck { expected: Some(String::new()), seen: seen.clone() };
    let stream = tcp(host, port).await?;
    let server = format!("{host}:{port}");
    let res = tokio::time::timeout(Duration::from_secs(30), client::connect_stream(ssh_config(), stream, check)).await;
    let seen = seen.lock().unwrap().take();
    if let Some(fp) = seen {
        return Ok(fp);
    }
    Err(match res {
        Err(_) => RemoteError::Transient(format!("The SFTP server {server} did not answer.")),
        Ok(Err(e)) => classify_ssh(&e, &server),
        Ok(Ok(_)) => RemoteError::Permanent(format!("{server} did not show a host key.")),
    })
}

#[derive(Debug, Default, Clone, Copy)]
struct Extensions {
    posix_rename: bool,
    statvfs: bool,
    copy_data: bool,
    fsync: bool,
}

/// One SSH connection with its SFTP session.
struct Conn {
    ssh: client::Handle<HostKeyCheck>,
    sftp: RawSftpSession,
    /// The base directory, absolute, as the server spells it.
    base: String,
    ext: Extensions,
    read_len: u32,
    write_len: u32,
    broken: AtomicBool,
}

impl Conn {
    fn path(&self, id: &str) -> String {
        match (id, self.base.as_str()) {
            ("", b) => b.to_string(),
            (id, "/") => format!("/{id}"),
            (id, b) => format!("{b}/{id}"),
        }
    }

    fn alive(&self) -> bool {
        !self.broken.load(Ordering::Relaxed) && !self.ssh.is_closed()
    }
}

pub struct Sftp {
    s: Box<Settings>,
    pinned: Mutex<Option<String>>,
    conn: tokio::sync::Mutex<Option<Arc<Conn>>>,
}

struct Settings {
    cfg: SftpConfig,
    secrets: Secrets,
    /// config.toml, where a first-use host key gets pinned.
    pin_to: Option<PathBuf>,
    agent_socket: Option<PathBuf>,
    no_extensions: bool,
}

impl Sftp {
    /// `pin_to`: the config file to write a first-use host key into.
    pub fn new(cfg: SftpConfig, secrets: Secrets, pin_to: Option<PathBuf>) -> Self {
        let pinned = Some(normalize_fingerprint(&cfg.host_key)).filter(|k| !k.is_empty());
        Sftp {
            s: Box::new(Settings { cfg, secrets, pin_to, agent_socket: None, no_extensions: false }),
            pinned: Mutex::new(pinned),
            conn: tokio::sync::Mutex::new(None),
        }
    }

    /// Uses this ssh-agent socket instead of `SSH_AUTH_SOCK`.
    pub fn with_agent_socket(mut self, path: PathBuf) -> Self {
        self.s.agent_socket = Some(path);
        self
    }

    /// Behaves as if the server offered no SFTP extensions, so the plain-SFTP fallbacks run
    /// (for tests).
    #[doc(hidden)]
    pub fn without_extensions(mut self) -> Self {
        self.s.no_extensions = true;
        self
    }

    /// The host key this client trusts (configured, or pinned on first use).
    pub fn host_key(&self) -> Option<String> {
        self.pinned.lock().unwrap().clone()
    }

    fn server(&self) -> String {
        format!("{}:{}", self.s.cfg.host, self.s.cfg.port)
    }

    /// The live connection, connecting (again) first when there is none.
    async fn conn(&self) -> RemoteResult<Arc<Conn>> {
        let mut slot = self.conn.lock().await;
        if let Some(c) = slot.as_ref()
            && c.alive()
        {
            return Ok(c.clone());
        }
        *slot = None;
        let c = Arc::new(self.connect().await?);
        *slot = Some(c.clone());
        Ok(c)
    }

    async fn connect(&self) -> RemoteResult<Conn> {
        if self.s.cfg.host.trim().is_empty() || self.s.cfg.user.trim().is_empty() {
            return Err(RemoteError::Auth("the SFTP server and user name are not set up yet".into()));
        }
        let server = self.server();
        let expected = self.pinned.lock().unwrap().clone();
        let seen = Arc::new(Mutex::new(None));
        let check = HostKeyCheck { expected: expected.clone(), seen: seen.clone() };
        let stream = tcp(&self.s.cfg.host, self.s.cfg.port).await?;
        let connected = tokio::time::timeout(Duration::from_secs(30), client::connect_stream(ssh_config(), stream, check)).await;
        let mut ssh = match connected {
            Err(_) => return Err(RemoteError::Transient(format!("The SFTP server {server} did not answer."))),
            Ok(Err(russh::Error::UnknownKey)) => {
                let got = seen.lock().unwrap().clone().unwrap_or_default();
                return Err(RemoteError::Permanent(format!(
                    "The SFTP server {server} identified itself with a different host key than the pinned one \
                     (expected {}, got {got}). If the server was reinstalled, check its new key and pin it again \
                     in the SFTP settings; otherwise someone may be intercepting the connection.",
                    expected.unwrap_or_default()
                )));
            }
            Ok(Err(e)) => return Err(classify_ssh(&e, &server)),
            Ok(Ok(h)) => h,
        };
        let seen = seen.lock().unwrap().take();
        if expected.is_none()
            && let Some(fp) = seen
        {
            self.pin(fp).await;
        }
        self.authenticate(&mut ssh).await?;

        let channel = ssh.channel_open_session().await.map_err(|e| classify_ssh(&e, &server))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| classify_ssh(&e, &server))?;
        let sftp_cfg = russh_sftp::client::Config { request_timeout_secs: 300, ..Default::default() };
        let mut sftp = RawSftpSession::new_with_config(channel.into_stream(), sftp_cfg);
        let version = sftp
            .init()
            .await
            .map_err(|_| RemoteError::Permanent(format!("{server} does not offer SFTP to this account.")))?;
        let has = |name: &str, v: &str| !self.s.no_extensions && version.extensions.get(name).is_some_and(|x| x == v);
        let ext = Extensions {
            posix_rename: has("posix-rename@openssh.com", "1"),
            statvfs: has("statvfs@openssh.com", "2"),
            copy_data: has("copy-data", "1"),
            fsync: has("fsync@openssh.com", "1"),
        };
        let (mut read_len, mut write_len) = (MIN_REQUEST, MIN_REQUEST);
        if has("limits@openssh.com", "1") {
            let limits = russh_sftp::client::rawsession::Limits::from(
                sftp.limits().await.map_err(|e| classify(&e, "start", false))?,
            );
            let clamp = |n: Option<u64>| n.map_or(MIN_REQUEST, |n| n.min(MAX_REQUEST) as u32);
            (read_len, write_len) = (clamp(limits.read_len), clamp(limits.write_len));
            sftp.set_limits(limits);
        }
        let wanted = base_request(&self.s.cfg.path);
        let base = match sftp.realpath(wanted.as_str()).await {
            Ok(name) => name.files.into_iter().next().map(|f| f.filename),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => {
                return Err(RemoteError::Permanent(format!(
                    "The folder \"{}\" does not exist on the SFTP server.",
                    self.s.cfg.path
                )));
            }
            Err(e) => return Err(classify(&e, &format!("find the folder \"{}\"", self.s.cfg.path), false)),
        }
        .ok_or_else(|| RemoteError::Transient("The SFTP server did not resolve the folder.".into()))?;
        let base = match base.trim_end_matches('/') {
            "" => "/".to_string(),
            b => b.to_string(),
        };
        // realpath resolves a missing last part too; the folder must exist.
        match sftp.stat(base.as_str()).await {
            Ok(a) if a.attrs.is_dir() => {}
            Ok(_) | Err(SftpError::Status(_)) => {
                return Err(RemoteError::Permanent(format!(
                    "The folder \"{}\" does not exist on the SFTP server.",
                    self.s.cfg.path
                )));
            }
            Err(e) => return Err(classify(&e, "find the folder", false)),
        }
        tracing::info!("connected to SFTP server {server} ({})", base);
        Ok(Conn {
            ssh,
            sftp,
            base,
            ext,
            read_len,
            write_len,
            broken: AtomicBool::new(false),
        })
    }

    /// Trusts `fp` from now on, and writes it into config.toml if that still has no pin.
    async fn pin(&self, fp: String) {
        *self.pinned.lock().unwrap() = Some(fp.clone());
        let Some(path) = self.s.pin_to.clone() else {
            return;
        };
        let cfg = self.s.cfg.clone();
        let written = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            if !path.exists() {
                return Ok(false);
            }
            let mut c = Config::load_from(&path)?;
            if c.provider != Provider::Sftp
                || c.sftp.host != cfg.host
                || c.sftp.port != cfg.port
                || !c.sftp.host_key.is_empty()
            {
                return Ok(false);
            }
            c.sftp.host_key = fp;
            c.save_to(&path)?;
            Ok(true)
        })
        .await;
        match written {
            Ok(Ok(true)) => tracing::info!("pinned the SFTP server's host key in the config"),
            Ok(Ok(false)) => {}
            Ok(Err(e)) => tracing::warn!("could not pin the SFTP host key in the config: {e:#}"),
            Err(e) => tracing::warn!("could not pin the SFTP host key in the config: {e}"),
        }
    }

    /// Signs in with the key file, else the password, else whatever ssh-agent holds.
    async fn authenticate(&self, ssh: &mut client::Handle<HostKeyCheck>) -> RemoteResult<()> {
        let user = self.s.cfg.user.trim().to_string();
        let server = self.server();
        let lost = |e: russh::Error| classify_ssh(&e, &server);
        let key_file = self.s.cfg.key_file.trim();
        if !key_file.is_empty() {
            let path = expand_home(key_file);
            let pass = self.s.secrets.passphrase.clone().or_else(|| self.s.secrets.password.clone());
            let shown = path.display().to_string();
            // Decrypting a key (bcrypt) takes a while: off the async threads.
            let loaded = tokio::task::spawn_blocking(move || russh::keys::load_secret_key(&path, pass.as_deref()))
                .await
                .map_err(|e| RemoteError::Permanent(format!("key task failed: {e}")))?;
            let key = match loaded {
                Ok(k) => k,
                Err(russh::keys::Error::KeyIsEncrypted) => {
                    return Err(RemoteError::Auth(format!("the SSH key {shown} needs its passphrase")));
                }
                Err(e) => return Err(RemoteError::Auth(format!("cannot use the SSH key {shown}: {e}"))),
            };
            let hash = if key.algorithm().is_rsa() { rsa_hash(ssh).await } else { None };
            let res = ssh
                .authenticate_publickey(user.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(lost)?;
            if res.success() {
                return Ok(());
            }
        }
        if let Some(pw) = &self.s.secrets.password {
            if ssh.authenticate_password(user.as_str(), pw.as_str()).await.map_err(lost)?.success() {
                return Ok(());
            }
        } else if key_file.is_empty() {
            let Some(mut agent) = agent(self.s.agent_socket.as_deref()).await else {
                return Err(RemoteError::Auth(
                    "no SSH key, password or ssh-agent to sign in to the SFTP server with".into(),
                ));
            };
            let ids = agent.request_identities().await.unwrap_or_default();
            for id in ids {
                let key = id.public_key().into_owned();
                let hash = if key.algorithm().is_rsa() { rsa_hash(ssh).await } else { None };
                match ssh.authenticate_publickey_with(user.as_str(), key, hash, &mut agent).await {
                    Ok(r) if r.success() => return Ok(()),
                    Ok(_) => {}
                    Err(russh::AgentAuthError::Send(_)) => {
                        return Err(RemoteError::Transient(format!("The connection to {server} was lost.")));
                    }
                    Err(russh::AgentAuthError::Key(e)) => tracing::debug!("ssh-agent could not sign: {e}"),
                }
            }
        }
        Err(RemoteError::Auth(format!("{server} did not accept the sign-in as {user}")))
    }

    /// Request failed on `c`: what it means, and whether to reconnect.
    fn fail(&self, c: &Conn, e: SftpError, what: &str) -> RemoteError {
        if is_connection_loss(&e) {
            c.broken.store(true, Ordering::Relaxed);
        }
        classify(&e, what, false)
    }

    /// [`Self::fail`] for requests that store data, telling a full disk apart (the protocol
    /// says only "Failure" for it).
    async fn fail_storing(&self, c: &Conn, e: SftpError, what: &str) -> RemoteError {
        let failure = matches!(&e, SftpError::Status(s) if s.status_code == StatusCode::Failure);
        if failure
            && c.ext.statvfs
            && c.alive()
            && let Ok(v) = c.sftp.statvfs(c.base.as_str()).await
            && v.blocks_avail.saturating_mul(v.fragment_size.max(1)) < FULL_BELOW
        {
            return classify(&e, what, true);
        }
        self.fail(c, e, what)
    }

    async fn stat(&self, c: &Conn, id: &str) -> RemoteResult<FileAttributes> {
        check_id(id)?;
        let path = c.path(id);
        match c.sftp.stat(path.as_str()).await {
            Ok(a) => Ok(a.attrs),
            Err(e) => Err(self.fail(c, e, &format!("look up {path}"))),
        }
    }

    /// Whether `id` exists (NotFound → false).
    async fn exists(&self, c: &Conn, id: &str) -> RemoteResult<bool> {
        match self.stat(c, id).await {
            Ok(_) => Ok(true),
            Err(RemoteError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn meta(&self, c: &Conn, id: &str, attrs: &FileAttributes) -> RemoteMeta {
        let folder = attrs.is_dir();
        let name = if id.is_empty() { name_of(&c.base).to_string() } else { name_of(id).to_string() };
        RemoteMeta {
            id: id.to_string(),
            name,
            parents: if id.is_empty() { Vec::new() } else { vec![parent_of(id).to_string()] },
            folder,
            hash: None,
            size: if folder { None } else { attrs.size },
            trashed: false,
        }
    }

    async fn meta_of(&self, c: &Conn, id: &str) -> RemoteResult<RemoteMeta> {
        let attrs = self.stat(c, id).await?;
        Ok(self.meta(c, id, &attrs))
    }

    /// Renames without replacing anything (plain SFTP rename refuses an existing target).
    async fn rename(&self, c: &Conn, from: &str, to: &str) -> RemoteResult<()> {
        let (a, b) = (c.path(from), c.path(to));
        match c.sftp.rename(a.as_str(), b.as_str()).await {
            Ok(_) => Ok(()),
            Err(e) => Err(self.fail_storing(c, e, &format!("rename {a} to {b}")).await),
        }
    }

    /// Renames `from` over `to`, replacing it: atomically with `posix-rename@openssh.com`,
    /// else by moving `to` aside, renaming, and removing the replaced file.
    async fn replace(&self, c: &Conn, from: &str, to: &str) -> RemoteResult<()> {
        if c.ext.posix_rename {
            let (a, b) = (c.path(from), c.path(to));
            let mut data = Vec::new();
            put_str(&mut data, &a);
            put_str(&mut data, &b);
            let res = match c.sftp.extended("posix-rename@openssh.com", data).await {
                Ok(p) => status_ok(p),
                Err(e) => Err(e),
            };
            return match res {
                Ok(()) => Ok(()),
                Err(e) => Err(self.fail_storing(c, e, &format!("rename {a} to {b}")).await),
            };
        }
        if !self.exists(c, to).await? {
            return self.rename(c, from, to).await;
        }
        let old = child_id(parent_of(to), &format!("{PART_PREFIX}{:016x}.old", rand::random::<u64>()))?;
        self.rename(c, to, &old).await?;
        if let Err(e) = self.rename(c, from, to).await {
            let _ = self.rename(c, &old, to).await;
            return Err(e);
        }
        let path = c.path(&old);
        if let Err(e) = c.sftp.remove(path.as_str()).await {
            tracing::warn!("could not remove {path}: {}", self.fail(c, e, "remove"));
        }
        Ok(())
    }

    /// A free name in `parent` for an old copy of `name` ("x (replaced 2026-10-21).txt").
    async fn aside_name(&self, c: &Conn, parent: &str, name: &str) -> RemoteResult<String> {
        let date = today();
        for n in 1..1000 {
            let id = child_id(parent, &replaced_name(name, &date, n))?;
            if !self.exists(c, &id).await? {
                return Ok(id);
            }
        }
        Err(RemoteError::Permanent(format!("too many replaced copies of {name} today")))
    }

    /// Makes room at `id` without losing what is there: an existing item moves aside.
    async fn clear(&self, c: &Conn, id: &str) -> RemoteResult<()> {
        if !self.exists(c, id).await? {
            return Ok(());
        }
        let aside = self.aside_name(c, parent_of(id), name_of(id)).await?;
        tracing::info!("SFTP: {id} is taken; moving it aside to {aside}");
        self.rename(c, id, &aside).await
    }

    async fn open(&self, c: &Conn, id: &str, flags: OpenFlags) -> RemoteResult<String> {
        let path = c.path(id);
        let writing = flags.contains(OpenFlags::WRITE);
        match c.sftp.open(path.as_str(), flags, FileAttributes::empty()).await {
            Ok(h) => Ok(h.handle),
            Err(e) if writing => Err(self.fail_storing(c, e, &format!("write {path}")).await),
            Err(e) => Err(self.fail(c, e, &format!("read {path}"))),
        }
    }

    async fn close(&self, c: &Conn, handle: String, id: &str) -> RemoteResult<()> {
        match c.sftp.close(handle).await {
            Ok(_) => Ok(()),
            Err(e) => Err(self.fail_storing(c, e, &format!("write {}", c.path(id))).await),
        }
    }

    /// Writes `data` at `offset`, [`IN_FLIGHT`] bytes at a time; stops sending at the first
    /// failure.
    async fn write_at(&self, c: &Conn, handle: &str, offset: u64, data: &[u8], id: &str) -> RemoteResult<()> {
        let piece = c.write_len as usize;
        let window = (IN_FLIGHT / piece).max(1);
        // Indices, not slices: borrowed stream items make the future not `Send`.
        let writes = futures_util::stream::iter((0..data.len()).step_by(piece))
            .map(|a| {
                let p = data[a..(a + piece).min(data.len())].to_vec();
                c.sftp.write(handle, offset + a as u64, p)
            })
            .buffered(window);
        futures_util::pin_mut!(writes);
        while let Some(r) = writes.next().await {
            if let Err(e) = r {
                return Err(self.fail_storing(c, e, &format!("write {}", c.path(id))).await);
            }
        }
        Ok(())
    }

    /// Up to `len` bytes from `offset` (fewer at the end of the file), [`IN_FLIGHT`] bytes
    /// at a time.
    async fn read_at(&self, c: &Conn, handle: &str, mut offset: u64, len: u64, id: &str) -> RemoteResult<Vec<u8>> {
        let end = offset.saturating_add(len);
        let mut out = Vec::with_capacity(len.min(BATCH) as usize);
        let piece = u64::from(c.read_len);
        let window = (IN_FLIGHT / piece as usize).max(1);
        'restart: while offset < end {
            let starts = (offset..end).step_by(piece as usize);
            let reads = futures_util::stream::iter(starts)
                .map(|o| {
                    let n = piece.min(end - o) as u32;
                    async move { (n, c.sftp.read(handle, o, n).await) }
                })
                .buffered(window);
            futures_util::pin_mut!(reads);
            while let Some((asked, r)) = reads.next().await {
                match r {
                    Ok(d) => {
                        let got = d.data.len() as u64;
                        out.extend_from_slice(&d.data);
                        offset += got;
                        if got < u64::from(asked) {
                            // A short read (allowed anywhere): ask again from where it stopped.
                            continue 'restart;
                        }
                    }
                    Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break 'restart,
                    Err(e) => return Err(self.fail(c, e, &format!("read {}", c.path(id)))),
                }
            }
        }
        Ok(out)
    }

    /// Copies `from` into a new file `to` (which must not exist): on the server with the
    /// `copy-data` extension (OpenSSH 9.0 and later), else by reading and writing it here.
    async fn copy(&self, c: &Conn, from: &str, to: &str) -> RemoteResult<()> {
        let part = child_id(parent_of(to), &format!("{PART_PREFIX}{:016x}{PART_SUFFIX}", rand::random::<u64>()))?;
        let src = self.open(c, from, OpenFlags::READ).await?;
        let dst = match self.open(c, &part, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::EXCLUDE).await {
            Ok(h) => h,
            Err(e) => {
                let _ = c.sftp.close(src).await;
                return Err(e);
            }
        };
        let copied = self.copy_handles(c, &src, &dst, from, &part).await;
        let _ = c.sftp.close(src).await;
        let closed = self.close(c, dst, &part).await;
        copied.and(closed)?;
        self.rename(c, &part, to).await
    }

    async fn copy_handles(&self, c: &Conn, src: &str, dst: &str, from: &str, part: &str) -> RemoteResult<()> {
        if c.ext.copy_data {
            let mut data = Vec::new();
            put_str(&mut data, src);
            data.extend_from_slice(&0u64.to_be_bytes()); // read offset
            data.extend_from_slice(&0u64.to_be_bytes()); // length: 0 = to the end
            put_str(&mut data, dst);
            data.extend_from_slice(&0u64.to_be_bytes()); // write offset
            let res = match c.sftp.extended("copy-data", data).await {
                Ok(p) => status_ok(p),
                Err(e) => Err(e),
            };
            return match res {
                Ok(()) => Ok(()),
                Err(e) => Err(self.fail_storing(c, e, &format!("copy {}", c.path(from))).await),
            };
        }
        let mut offset = 0;
        loop {
            let data = self.read_at(c, src, offset, BATCH, from).await?;
            if data.is_empty() {
                return Ok(());
            }
            self.write_at(c, dst, offset, &data, part).await?;
            offset += data.len() as u64;
        }
    }

    /// Puts a finished partial file in place of the target (see [`UploadSession`]).
    async fn complete(&self, c: &Conn, s: &UploadSession, total: u64) -> RemoteResult<RemoteMeta> {
        match self.stat(c, &s.part).await?.size {
            Some(n) if n == total => {}
            _ => return Err(RemoteError::NotFound),
        }
        match s.replaces.as_deref() {
            // Replacing a file that has moved since: move it along first, so it ends up
            // where it belongs with new content (as a provider with stable ids would).
            Some(old) if old != s.target => {
                self.clear(c, &s.target).await?;
                match self.rename(c, old, &s.target).await {
                    Ok(()) | Err(RemoteError::NotFound) => {}
                    Err(e) => return Err(e),
                }
            }
            Some(_) => {}
            None => self.clear(c, &s.target).await?,
        }
        self.replace(c, &s.part, &s.target).await?;
        self.meta_of(c, &s.target).await
    }

    // ---------- the provider interface ----------

    /// Space on the server's disk (`statvfs@openssh.com`), as (size, used, used); nothing
    /// known without the extension.
    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        let c = self.conn().await?;
        if !c.ext.statvfs {
            return Ok((None, 0, 0));
        }
        let v = match c.sftp.statvfs(c.base.as_str()).await {
            Ok(v) => v,
            Err(e) => return Err(self.fail(&c, e, "read how much space is free")),
        };
        let unit = v.fragment_size.max(1);
        let used = v.blocks.saturating_sub(v.blocks_free).saturating_mul(unit);
        // What this account can fill: used plus what is free to it (excludes root's reserve).
        let size = used.saturating_add(v.blocks_avail.saturating_mul(unit));
        Ok((Some(size), used, used))
    }

    /// The configured directory.
    pub async fn top_parent(&self) -> RemoteResult<String> {
        Ok(String::new())
    }

    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        let c = self.conn().await?;
        self.meta_of(&c, id).await
    }

    /// The item called `name` in `parent`, if there is one (and it's a folder, if asked).
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        let c = self.conn().await?;
        let id = child_id(parent, name)?;
        match self.meta_of(&c, &id).await {
            Ok(m) if m.folder || !folder => Ok(vec![m]),
            Ok(_) | Err(RemoteError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        let c = self.conn().await?;
        let id = child_id(parent, name)?;
        match self.meta_of(&c, &id).await {
            Ok(m) if m.folder => return Ok(m),
            Ok(_) => self.clear(&c, &id).await?,
            Err(RemoteError::NotFound) => {}
            Err(e) => return Err(e),
        }
        let path = c.path(&id);
        if let Err(e) = c.sftp.mkdir(path.as_str(), FileAttributes::empty()).await {
            let err = self.fail_storing(&c, e, &format!("create the folder {path}")).await;
            // Made by someone else meanwhile?
            return match self.meta_of(&c, &id).await {
                Ok(m) if m.folder => Ok(m),
                _ => Err(err),
            };
        }
        self.meta_of(&c, &id).await
    }

    /// Renames and/or moves an item; the returned id is its new path. Something already at
    /// the new path moves aside first.
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        let c = self.conn().await?;
        check_id(id)?;
        if id.is_empty() {
            return Err(RemoteError::Permanent("the SFTP folder itself cannot be moved".into()));
        }
        let parent = parents.map_or(parent_of(id), |(new, _)| new);
        let new_id = child_id(parent, name.unwrap_or(name_of(id)))?;
        if new_id != id {
            if !self.exists(&c, id).await? {
                return Err(RemoteError::NotFound);
            }
            self.clear(&c, &new_id).await?;
            self.rename(&c, id, &new_id).await?;
        }
        self.meta_of(&c, &new_id).await
    }

    /// Keeps the current content as "<stem> (replaced YYYY-MM-DD).<ext>" beside the file.
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        let c = self.conn().await?;
        if self.meta_of(&c, id).await?.folder {
            return Ok(());
        }
        let aside = self.aside_name(&c, parent_of(id), name_of(id)).await?;
        self.copy(&c, id, &aside).await
    }

    /// Creates the partial file the content goes into; the session names it and the target.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        _size: u64,
        _mime: &str,
    ) -> RemoteResult<String> {
        let c = self.conn().await?;
        let target = child_id(parents.0, name)?;
        if let Some(old) = existing {
            check_id(old)?;
            // Like a vanished Drive file: NotFound makes the engine upload a new one.
            self.stat(&c, old).await?;
        }
        let part = child_id(parents.0, &format!("{PART_PREFIX}{:016x}{PART_SUFFIX}", rand::random::<u64>()))?;
        let h = self.open(&c, &part, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::EXCLUDE).await?;
        self.close(&c, h, &part).await?;
        Ok(UploadSession { part, target, replaces: existing.map(str::to_string) }.encode())
    }

    /// Continues where the partial file is known to be complete; NotFound once it's gone.
    pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus> {
        let s = UploadSession::decode(session)?;
        let c = self.conn().await?;
        match self.stat(&c, &s.part).await?.size.unwrap_or(0) {
            n if n > total => Err(RemoteError::NotFound),
            n => Ok(UploadStatus::Next(resume_offset(n))),
        }
    }

    pub async fn put_chunk(&self, session: &str, data: bytes::Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus> {
        let s = UploadSession::decode(session)?;
        let end = start + data.len() as u64;
        if end > total {
            return Err(RemoteError::Permanent("the upload got more data than the file has".into()));
        }
        let c = self.conn().await?;
        if !data.is_empty() {
            let h = self.open(&c, &s.part, OpenFlags::WRITE).await?;
            let written = self.write_at(&c, &h, start, &data, &s.part).await;
            let synced = if written.is_ok() && end == total && c.ext.fsync {
                match c.sftp.fsync(h.as_str()).await {
                    Ok(_) => Ok(()),
                    Err(e) => Err(self.fail_storing(&c, e, &format!("write {}", c.path(&s.part))).await),
                }
            } else {
                Ok(())
            };
            let closed = self.close(&c, h, &s.part).await;
            written.and(synced).and(closed)?;
        }
        if end < total {
            return Ok(UploadStatus::Next(end));
        }
        Ok(UploadStatus::Done(self.complete(&c, &s, total).await?))
    }

    /// Bytes `start..end` of a file's content (fewer if the file is shorter).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes> {
        if end <= start {
            return Ok(bytes::Bytes::new());
        }
        let c = self.conn().await?;
        check_id(id)?;
        let h = self.open(&c, id, OpenFlags::READ).await?;
        let data = self.read_at(&c, &h, start, end - start, id).await;
        let _ = c.sftp.close(h).await;
        Ok(bytes::Bytes::from(data?))
    }

    /// Streams a file's content into `out`; returns (its SHA-256, bytes written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        let c = self.conn().await?;
        check_id(id)?;
        let h = self.open(&c, id, OpenFlags::READ).await?;
        let res = async {
            let mut hasher = Hasher::new(HashKind::Sha256);
            let mut total = 0u64;
            loop {
                let data = self.read_at(&c, &h, total, BATCH, id).await?;
                if data.is_empty() {
                    break;
                }
                hasher.update(&data);
                total += data.len() as u64;
                out.write_all(&data)
                    .await
                    .map_err(|e| RemoteError::Permanent(format!("cannot write download: {e}")))?;
            }
            out.flush()
                .await
                .map_err(|e| RemoteError::Permanent(format!("cannot write download: {e}")))?;
            Ok((hasher.finish(), total))
        }
        .await;
        let _ = c.sftp.close(h).await;
        res
    }
}

/// The best RSA signature hash the server takes (None: plain ssh-rsa).
async fn rsa_hash(ssh: &client::Handle<HostKeyCheck>) -> Option<HashAlg> {
    ssh.best_supported_rsa_hash().await.ok().flatten().flatten()
}

type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

#[cfg(unix)]
async fn agent(socket: Option<&Path>) -> Option<Agent> {
    let path = socket
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from))?;
    AgentClient::connect_uds(path).await.ok().map(AgentClient::dynamic)
}

/// Windows: OpenSSH's agent (named pipe), else Pageant.
#[cfg(windows)]
async fn agent(socket: Option<&Path>) -> Option<Agent> {
    let pipe = socket
        .map(|p| p.as_os_str().to_owned())
        .or_else(|| std::env::var_os("SSH_AUTH_SOCK"))
        .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
    if let Ok(a) = AgentClient::connect_named_pipe(&pipe).await {
        return Some(a.dynamic());
    }
    AgentClient::connect_pageant().await.ok().map(AgentClient::dynamic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Kind;
    use russh_sftp::protocol::Status;

    fn status(code: StatusCode, msg: &str) -> SftpError {
        SftpError::Status(Status { id: 1, status_code: code, error_message: msg.into(), language_tag: "en".into() })
    }

    #[test]
    fn sessions_round_trip_and_reject_strangers() {
        let s = UploadSession {
            part: "CloudDrop/a/.clouddrop-00ff00ff00ff00ff.part".into(),
            target: "CloudDrop/a/report.pdf".into(),
            replaces: Some("CloudDrop/old.pdf".into()),
        };
        assert_eq!(UploadSession::decode(&s.encode()).unwrap(), s);
        let new = UploadSession { replaces: None, ..s.clone() };
        assert!(!new.encode().contains("replaces"));
        assert_eq!(UploadSession::decode(&new.encode()).unwrap(), new);
        for bad in [
            "https://www.googleapis.com/upload/drive/v3/files?upload_id=x".to_string(),
            r#"{"url":"https://dav.example.com/x","target":"a"}"#.to_string(),
            // Only CloudDrop's own partial files, beside the target, inside the base.
            UploadSession { part: "CloudDrop/a/notes.txt".into(), ..s.clone() }.encode(),
            UploadSession { part: "CloudDrop/.clouddrop-1.part".into(), ..s.clone() }.encode(),
            UploadSession { part: "../.clouddrop-1.part".into(), target: "../x".into(), replaces: None }.encode(),
            UploadSession { replaces: Some("../../etc/passwd".into()), ..s.clone() }.encode(),
        ] {
            assert_eq!(UploadSession::decode(&bad), Err(RemoteError::NotFound), "{bad}");
        }
    }

    #[test]
    fn resume_starts_where_no_write_was_in_flight() {
        const M: u64 = 1024 * 1024;
        assert_eq!(resume_offset(0), 0);
        assert_eq!(resume_offset(2 * M), 0);
        assert_eq!(resume_offset(3 * M), M);
        assert_eq!(resume_offset(3 * M + 5), M);
        assert_eq!(resume_offset(20 * M), 18 * M);
        for n in [0, 1, M - 1, 7 * M + 12345, 100 * M] {
            let r = resume_offset(n);
            assert!(r + IN_FLIGHT as u64 <= n.max(IN_FLIGHT as u64) && r % CHUNK_UNIT as u64 == 0, "{n}");
        }
    }

    #[test]
    fn errors_map_to_what_the_engine_does_next() {
        assert_eq!(classify(&status(StatusCode::NoSuchFile, "No such file"), "read x", false), RemoteError::NotFound);
        let denied = classify(&status(StatusCode::PermissionDenied, "Permission denied"), "write /srv/a.txt", false);
        assert_eq!(denied, RemoteError::Permanent("The SFTP server denied permission to write /srv/a.txt.".into()));
        assert!(matches!(classify(&status(StatusCode::Failure, "Failure"), "write x", true), RemoteError::Quota(_)));
        assert_eq!(
            classify(&status(StatusCode::Failure, "Failure"), "write x", false),
            RemoteError::Permanent("The SFTP server could not write x.".into())
        );
        assert_eq!(
            classify(&status(StatusCode::Failure, "Is a directory"), "write x", false),
            RemoteError::Permanent("The SFTP server could not write x (Is a directory).".into())
        );
        assert!(matches!(classify(&status(StatusCode::OpUnsupported, ""), "copy x", false), RemoteError::Permanent(_)));
        for lost in [
            SftpError::IO("broken pipe".into()),
            SftpError::Timeout,
            SftpError::UnexpectedBehavior("session closed".into()),
            status(StatusCode::ConnectionLost, ""),
        ] {
            assert!(is_connection_loss(&lost), "{lost:?}");
            assert!(classify(&lost, "read x", false).is_transient(), "{lost:?}");
        }
        assert!(!is_connection_loss(&status(StatusCode::PermissionDenied, "")));
        assert!(!is_connection_loss(&status(StatusCode::NoSuchFile, "")));

        let server = "h:22";
        assert!(classify_ssh(&russh::Error::Disconnect, server).is_transient());
        assert!(classify_ssh(&russh::Error::IO(std::io::ErrorKind::ConnectionReset.into()), server).is_transient());
        assert!(matches!(classify_ssh(&russh::Error::Version, server), RemoteError::Permanent(_)));
        assert!(matches!(classify_ssh(&russh::Error::RequestDenied, server), RemoteError::Permanent(_)));
    }

    #[test]
    fn ids_are_paths_inside_the_base() {
        assert!(check_id("").is_ok() && check_id("CloudDrop/a b/c.txt").is_ok());
        for bad in ["/etc", "a//b", "a/../b", "..", "./a", "a/"] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
        assert_eq!(child_id("", "CloudDrop").unwrap(), "CloudDrop");
        assert_eq!(child_id("CloudDrop", "a.txt").unwrap(), "CloudDrop/a.txt");
        for bad in ["", ".", "..", "a/b"] {
            assert!(child_id("CloudDrop", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn kept_copies_are_named_by_date() {
        assert_eq!(replaced_name("report.pdf", "2026-10-21", 1), "report (replaced 2026-10-21).pdf");
        assert_eq!(replaced_name("report.pdf", "2026-10-21", 2), "report (replaced 2026-10-21) (2).pdf");
        assert_eq!(replaced_name("Makefile", "2026-10-21", 1), "Makefile (replaced 2026-10-21)");
        assert_eq!(replaced_name(".bashrc", "2026-10-21", 1), ".bashrc (replaced 2026-10-21)");
        assert_eq!(today().len(), 10);
    }

    #[test]
    fn fingerprints_compare_as_pasted() {
        let fp = "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8";
        assert_eq!(normalize_fingerprint(fp), fp);
        assert_eq!(normalize_fingerprint(&format!(" {fp}= ")), fp);
        assert_eq!(normalize_fingerprint(&format!("256 {fp} root@host (ED25519)")), fp);
    }

    #[test]
    fn base_directory_requests() {
        assert_eq!(base_request(""), ".");
        assert_eq!(base_request("~"), ".");
        assert_eq!(base_request("~/backup"), "backup");
        assert_eq!(base_request("/srv/backup/"), "/srv/backup/");
    }

    #[test]
    fn config_section() {
        #[derive(Deserialize)]
        struct C {
            sftp: SftpConfig,
        }
        let c: C = toml::from_str("[sftp]\nhost = \"h\"\nport = \"2222\"\nuser = \"me\"").unwrap();
        assert_eq!((c.sftp.host.as_str(), c.sftp.port, c.sftp.user.as_str()), ("h", 2222, "me"));
        let c: C = toml::from_str("[sftp]\nport = 2200").unwrap();
        assert_eq!(c.sftp.port, 2200);
        let c: C = toml::from_str("[sftp]\nport = \"\"").unwrap();
        assert_eq!(c.sftp.port, 22);
        assert!(toml::from_str::<C>("[sftp]\nport = \"ssh\"").is_err());
        assert!(SftpConfig::default().is_default());
        let secrets = Secrets::from_map(serde_json::json!({"password": "", "passphrase": "p"}).as_object().unwrap());
        assert_eq!((secrets.password, secrets.passphrase.as_deref()), (None, Some("p")));
    }

    #[test]
    fn provider_info() {
        assert_eq!((INFO.id, INFO.name), ("sftp", "SFTP server"));
        assert!(INFO.caps.hash.is_none() && !INFO.caps.stable_ids && !INFO.caps.pin_versions && INFO.caps.resumable);
        assert_eq!((INFO.web_url)(Kind::File, "a"), None);
        let SignIn::Form(fields) = &INFO.sign_in else { panic!("a form") };
        let keys: Vec<_> = fields.iter().map(|f| f.key).collect();
        assert_eq!(keys, ["host", "port", "user", "path", "key_file", "password", "passphrase", "host_key"]);
        assert!(fields.iter().filter(|f| f.kind == FieldKind::Secret).all(|f| f.key == "password" || f.key == "passphrase"));
    }
}
