//! Proton Drive (experimental), through Proton's own command-line app, `proton-drive`. The
//! engine reaches it through [`crate::remote::Remote`].
//!
//! # Why the CLI
//!
//! Proton Drive encrypts everything on the client. Proton's SDK does that cryptography, but it
//! is "not yet ready for third-party production use", and its usage guidelines for personal
//! projects ask a third-party app to use the SDK (or match its guarantees), call only Proton's
//! official endpoints, identify itself, sync from events instead of polling or walking folders,
//! leave out Proton's branding, and say: "This is a third-party application not officially
//! supported by Proton." ([`DISCLOSURE`]).
//!
//! A Rust copy of Proton's cryptography would not match the SDK's guarantees, and embedding the
//! TypeScript SDK means shipping a JavaScript runtime. Proton's CLI (MIT, built on the SDK)
//! already signs in (in the browser, keeping the session in the system keyring), encrypts and
//! talks to Proton the official way, so this client runs it, one command per call.
//!
//! # The CLI in brief
//!
//! - **Addresses.** Paths start at `/my-files`, and a path segment that is a node uid
//!   (`<volume>~<link>`) names that node directly. So `/my-files/<uid>` is any file or folder,
//!   and uids survive renames and moves: they are this client's ids.
//! - **Output.** With `--json`, `info`, `create-folder` and `rename` print the node as one line
//!   of JSON (the SDK's `NodeEntity`); `list`, `move` and `copy` print a JSON array. Errors go
//!   to stderr with exit status 1 ("You need to login first" before sign-in).
//! - **Uploads** take local files and use the local name. CloudDrop's chunks are staged in
//!   `proton-staging/` in the data directory, under the name the file gets, so an upload
//!   resumes from what is staged; the last chunk runs `filesystem upload` (a new revision when
//!   the file exists), and `info` reads the result. The SHA-1 the CLI records is checked
//!   against the staged file.
//! - **No content hash.** Proton's SHA-1 is what the uploader claims, inside encrypted
//!   metadata, so it proves nothing about what the server holds: freeing space downloads the
//!   file to compare, which the CLI decrypts and checks.
//! - **One command at a time.** Each CLI process uses the same session, and two at once could
//!   both refresh it.
//! - **Storage** isn't reported by the CLI.
//!
//! CloudDrop never lists the cloud on a timer or walks it: it looks up names in one folder
//! when it creates something there, as the guidelines allow.
//!
//! Experimental: builds offer it only with `CLOUDDROP_EXPERIMENTAL_PROVIDERS=1`
//! ([`crate::remote::EXPERIMENTAL_PROVIDERS`]), so store, snap and PPA builds don't, until
//! Proton opens the SDK to third-party apps.

use crate::hash::Hasher;
use crate::remote::{
    Capabilities, ContentHash, Field, FieldKind, HashKind, ProviderInfo, RemoteError, RemoteMeta, RemoteResult,
    SignIn, StorageQuota, UploadStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tokio::io::AsyncWriteExt;

/// What Proton asks every third-party app to say.
pub const DISCLOSURE: &str = "This is a third-party application not officially supported by Proton.";

/// The CLI's name on PATH.
pub const DEFAULT_CLI: &str = "proton-drive";

/// Where to get the CLI, for messages.
pub const CLI_DOWNLOAD: &str = "https://proton.me/download/drive/cli";

pub static INFO: ProviderInfo = ProviderInfo {
    id: "proton",
    name: "Proton Drive",
    experimental: true,
    caps: Capabilities {
        hash: None,
        pin_versions: false,
        stable_ids: true,
        resumable: true,
        case_insensitive: false,
        forbidden_chars: "",
        // Chunks only go to the staging file, so any size works.
        chunk_unit: 256 * 1024,
    },
    sign_in: SignIn::Form(&[Field {
        key: "cli",
        label: "Proton Drive CLI",
        kind: FieldKind::File,
        required: false,
        hint: "Proton's proton-drive program, if it isn't on your PATH. Sign in with \
               `proton-drive auth login` first. This is a third-party application not \
               officially supported by Proton.",
    }]),
    // The web app has no stable address for a node uid.
    web_url: |_, _| None,
};

/// `[proton]` in config.toml. The sign-in lives with the CLI, not here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtonConfig {
    /// Path to Proton's CLI; empty: `proton-drive` on PATH.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cli: String,
}

impl ProtonConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The CLI to run.
    pub fn program(&self) -> PathBuf {
        match self.cli.trim() {
            "" => PathBuf::from(DEFAULT_CLI),
            p => PathBuf::from(p),
        }
    }
}

/// Signs the CLI in (`proton-drive auth login`), in this terminal and the browser.
pub fn login(cfg: &ProtonConfig) -> anyhow::Result<()> {
    let program = cfg.program();
    let status = std::process::Command::new(&program)
        .args(["auth", "login"])
        .status()
        .map_err(|e| anyhow::anyhow!("{}", missing_cli(&program, &e)))?;
    if !status.success() {
        anyhow::bail!("`{} auth login` did not finish ({status})", program.display());
    }
    Ok(())
}

/// How long a command that only reads or changes names may take.
const QUICK: Duration = Duration::from_secs(300);

/// Personal files in the CLI's paths.
const MY_FILES: &str = "/my-files";

pub struct Proton {
    cli: PathBuf,
    staging: PathBuf,
    scratch: PathBuf,
    /// One CLI process at a time (they share one session).
    one: tokio::sync::Mutex<()>,
    root: Mutex<Option<String>>,
    /// The last file `download_range` fetched: (id, local copy).
    cached: Mutex<Option<(String, PathBuf)>>,
}

impl Proton {
    pub fn new(cfg: &ProtonConfig, data_dir: &Path) -> Self {
        Proton {
            cli: cfg.program(),
            staging: data_dir.join("proton-staging"),
            scratch: data_dir.join("proton-downloads"),
            one: tokio::sync::Mutex::new(()),
            root: Mutex::new(None),
            cached: Mutex::new(None),
        }
    }

    /// Runs `proton-drive filesystem <command> --json <options> -- <args>` and returns stdout.
    async fn fs(&self, command: &str, options: &[String], args: &[&str], limit: Option<Duration>) -> RemoteResult<String> {
        let mut argv: Vec<&str> = vec!["filesystem", command, "--json"];
        argv.extend(options.iter().map(String::as_str));
        argv.push("--");
        argv.extend(args);
        let _one = self.one.lock().await;
        let mut cmd = tokio::process::Command::new(&self.cli);
        cmd.args(&argv).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let run = cmd.output();
        let out = match limit {
            Some(t) => tokio::time::timeout(t, run)
                .await
                .map_err(|_| RemoteError::Transient(format!("Proton's CLI took over {}s ({command})", t.as_secs())))?,
            None => run.await,
        };
        let out = out.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                RemoteError::Permanent(missing_cli(&self.cli, &e))
            }
            _ => RemoteError::Transient(format!("cannot run Proton's CLI: {e}")),
        })?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(match out.status.code() {
            Some(_) => classify(&format!("{stderr}\n{stdout}"), &self.cli),
            None => RemoteError::Transient(format!("Proton's CLI stopped ({})", out.status)),
        })
    }

    async fn node(&self, command: &str, options: &[String], args: &[&str]) -> RemoteResult<RemoteMeta> {
        Ok(node_meta(&parse_json(&self.fs(command, options, args, Some(QUICK)).await?)?)?.0)
    }

    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        Ok((None, 0, 0))
    }

    /// The uid of My files.
    pub async fn top_parent(&self) -> RemoteResult<String> {
        if let Some(root) = self.root.lock().unwrap().clone() {
            return Ok(root);
        }
        let root = self.node("info", &[], &[MY_FILES]).await?.id;
        *self.root.lock().unwrap() = Some(root.clone());
        Ok(root)
    }

    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        self.node("info", &[], &[&path_of(id)?]).await
    }

    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        Ok(self
            .children(parent)
            .await?
            .into_iter()
            .filter(|m| m.name == name && m.folder == folder && !m.trashed)
            .collect())
    }

    async fn children(&self, parent: &str) -> RemoteResult<Vec<RemoteMeta>> {
        let out = self.fs("list", &[], &[&path_of(parent)?], Some(QUICK)).await?;
        match parse_json(&out)? {
            Value::Array(items) => items.iter().map(|v| node_meta(v).map(|(m, _)| m)).collect(),
            _ => Err(RemoteError::Permanent("Proton's CLI listed a folder in an unknown format".into())),
        }
    }

    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        self.node("create-folder", &[], &[&path_of(parent)?, name]).await
    }

    pub async fn move_rename(&self, id: &str, name: Option<&str>, parents: Option<(&str, &str)>) -> RemoteResult<RemoteMeta> {
        let path = path_of(id)?;
        if let Some((new_parent, _)) = parents {
            let out = self.fs("move", &[], &[&path, &path_of(new_parent)?], Some(QUICK)).await?;
            check_results(&out, "move")?;
        }
        match name {
            Some(name) => self.node("rename", &[], &[&path, name]).await,
            None => self.get(id).await,
        }
    }

    /// Copies the file next to itself as "<stem> (replaced YYYY-MM-DD).<ext>" (" (2)" and up
    /// when taken): replacing it then never loses the old content.
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        let meta = self.get(id).await?;
        let parent = meta.parents.first().cloned().ok_or(RemoteError::NotFound)?;
        let taken: Vec<String> = self.children(&parent).await?.into_iter().map(|m| m.name).collect();
        let base = crate::onedrive::replaced_name(&meta.name, &crate::onedrive::utc_date(SystemTime::now()));
        let name = (1..)
            .map(|n| if n == 1 { base.clone() } else { numbered(&base, n) })
            .find(|n| !taken.contains(n))
            .expect("a free name");
        let out = self.fs("copy", &[format!("--name={name}")], &[&path_of(id)?, &path_of(&parent)?], Some(QUICK)).await?;
        check_results(&out, "copy")
    }

    /// Starts staging an upload; the session is a small JSON object ([`Session`]).
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        _mime: &str,
    ) -> RemoteResult<String> {
        // New content for a file goes to it where it is, under its name, as a new revision.
        let (parent, name) = match existing {
            Some(id) => {
                let m = self.get(id).await?;
                (m.parents.first().cloned().unwrap_or_else(|| parents.0.to_string()), m.name)
            }
            None => (parents.0.to_string(), name.to_string()),
        };
        path_of(&parent)?;
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(RemoteError::Permanent(format!("{name:?} can't be uploaded to Proton Drive by name")));
        }
        let session = Session { dir: unique(), parent, name, size, existing: existing.map(String::from) };
        let file = self.staged(&session);
        std::fs::create_dir_all(file.parent().unwrap_or(&self.staging))
            .and_then(|_| std::fs::File::create(&file))
            .map_err(|e| local_error("cannot stage the upload", &e))?;
        serde_json::to_string(&session).map_err(|e| RemoteError::Permanent(e.to_string()))
    }

    fn session(&self, session: &str) -> RemoteResult<Session> {
        let s: Session = serde_json::from_str(session).map_err(|_| RemoteError::NotFound)?;
        // Only ever a name this client made: a fresh session otherwise.
        if s.dir.is_empty() || !s.dir.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(RemoteError::NotFound);
        }
        Ok(s)
    }

    fn staged(&self, s: &Session) -> PathBuf {
        self.staging.join(&s.dir).join(&s.name)
    }

    /// How much is staged; NotFound when the session's staging is gone.
    pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus> {
        let s = self.session(session)?;
        let have = std::fs::metadata(self.staged(&s)).map_err(|_| RemoteError::NotFound)?.len();
        if have >= total {
            return self.finish(&s, total).await.map(UploadStatus::Done);
        }
        Ok(UploadStatus::Next(have))
    }

    pub async fn put_chunk(&self, session: &str, data: bytes::Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus> {
        let s = self.session(session)?;
        let path = self.staged(&s);
        let path2 = path.clone();
        let wrote = tokio::task::spawn_blocking(move || -> std::io::Result<Option<u64>> {
            let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&path2)?;
            let have = f.metadata()?.len();
            if start > have {
                return Ok(None);
            }
            f.set_len(start)?;
            f.seek(SeekFrom::Start(start))?;
            f.write_all(&data)?;
            f.sync_data()?;
            Ok(Some(start + data.len() as u64))
        })
        .await
        .map_err(|e| RemoteError::Permanent(format!("staging task failed: {e}")))?;
        let next = match wrote {
            Ok(Some(next)) => next,
            Ok(None) => return self.query_upload(session, total).await,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(RemoteError::NotFound),
            Err(e) => return Err(local_error("cannot stage the upload", &e)),
        };
        if next >= total {
            return self.finish(&s, total).await.map(UploadStatus::Done);
        }
        Ok(UploadStatus::Next(next))
    }

    /// Uploads the staged file and returns what Proton holds.
    async fn finish(&self, s: &Session, total: u64) -> RemoteResult<RemoteMeta> {
        let file = self.staged(s);
        let (sha1, size) = file_sha1(&file)?;
        if size != total {
            return Err(RemoteError::Transient(format!("staged {size} of {total} bytes")));
        }
        let parent = path_of(&s.parent)?;
        let local = file.to_string_lossy().into_owned();
        self.fs("upload", &["--file-conflict-strategy=create-new-revision".into()], &[&local, &parent], None)
            .await?;
        let v = parse_json(&self.fs("info", &[], &[&format!("{parent}/{}", escape(&s.name))], Some(QUICK)).await?)?;
        let (meta, claimed) = node_meta(&v)?;
        if let Some(claimed) = claimed.filter(|c| !c.eq_ignore_ascii_case(&sha1)) {
            return Err(RemoteError::Transient(format!(
                "Proton Drive recorded different content for {} (SHA-1 {claimed}, uploaded {sha1})",
                s.name
            )));
        }
        let _ = std::fs::remove_dir_all(file.parent().unwrap_or(&file));
        self.forget_cached(&meta.id);
        Ok(meta)
    }

    /// Bytes `start..end` (fewer at the end of the file), from a downloaded copy kept for the
    /// next range of the same file.
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes> {
        if end <= start {
            return Ok(bytes::Bytes::new());
        }
        let cached = self.cached.lock().unwrap().clone().filter(|(i, p)| i == id && p.exists());
        let path = match cached {
            Some((_, p)) => p,
            None => {
                let p = self.fetch(id).await?;
                if let Some((_, old)) = self.cached.lock().unwrap().replace((id.to_string(), p.clone())) {
                    remove_fetched(&old);
                }
                p
            }
        };
        let mut f = std::fs::File::open(&path).map_err(|e| local_error("cannot read the download", &e))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let end = end.min(len);
        let mut buf = vec![0u8; end.saturating_sub(start) as usize];
        f.seek(SeekFrom::Start(start))
            .and_then(|_| f.read_exact(&mut buf))
            .map_err(|e| local_error("cannot read the download", &e))?;
        Ok(bytes::Bytes::from(buf))
    }

    /// Streams the file into `out`; the hash is SHA-256 (Proton reports none).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        let path = self.fetch(id).await?;
        let result = copy_hashing(&path, out).await;
        remove_fetched(&path);
        result
    }

    /// Downloads the file into a fresh folder in the data directory; returns its path.
    async fn fetch(&self, id: &str) -> RemoteResult<PathBuf> {
        let dir = self.scratch.join(unique());
        std::fs::create_dir_all(&dir).map_err(|e| local_error("cannot make room for the download", &e))?;
        let local = dir.to_string_lossy().into_owned();
        if let Err(e) = self.fs("download", &[], &[&path_of(id)?, &local], None).await {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        let file = std::fs::read_dir(&dir)
            .ok()
            .and_then(|mut entries| entries.find_map(|e| e.ok().map(|e| e.path()).filter(|p| p.is_file())));
        file.ok_or_else(|| {
            let _ = std::fs::remove_dir_all(&dir);
            RemoteError::Permanent("Proton's CLI downloaded nothing".into())
        })
    }

    fn forget_cached(&self, id: &str) {
        let mut cached = self.cached.lock().unwrap();
        if cached.as_ref().is_some_and(|(i, _)| i == id)
            && let Some((_, p)) = cached.take()
        {
            remove_fetched(&p);
        }
    }
}

impl Drop for Proton {
    fn drop(&mut self) {
        if let Some((_, p)) = self.cached.get_mut().ok().and_then(|c| c.take()) {
            remove_fetched(&p);
        }
    }
}

/// An upload being staged: `proton-staging/<dir>/<name>`, uploaded to `parent` when complete.
#[derive(Debug, Serialize, Deserialize)]
struct Session {
    dir: String,
    parent: String,
    name: String,
    size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    existing: Option<String>,
}

/// A name for a fresh folder in the data directory.
fn unique() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// Deletes a downloaded file and the folder made for it.
fn remove_fetched(file: &Path) {
    let _ = std::fs::remove_dir_all(file.parent().unwrap_or(file));
}

/// The CLI path of a node. Ids are uids, which never contain `/`.
fn path_of(id: &str) -> RemoteResult<String> {
    if id.is_empty() || id.contains(['/', '\\']) {
        return Err(RemoteError::NotFound);
    }
    Ok(format!("{MY_FILES}/{id}"))
}

/// A name as one CLI path segment (`/` is written `\/`).
fn escape(name: &str) -> String {
    name.replace('/', "\\/")
}

/// "a (replaced 2026-10-21).txt" → "a (replaced 2026-10-21) (2).txt".
fn numbered(name: &str, n: u32) -> String {
    match name.rfind(" (replaced ").and_then(|i| name[i..].find(')').map(|j| i + j + 1)) {
        Some(at) => format!("{} ({n}){}", &name[..at], &name[at..]),
        None => format!("{name} ({n})"),
    }
}

fn parse_json(out: &str) -> RemoteResult<Value> {
    serde_json::from_str(out.trim())
        .map_err(|e| RemoteError::Permanent(format!("Proton's CLI answered something unexpected ({e})")))
}

/// A node as the CLI prints it (the SDK's `NodeEntity`, possibly wrapped as `{ok, value}`), and
/// the SHA-1 its uploader recorded.
fn node_meta(v: &Value) -> RemoteResult<(RemoteMeta, Option<String>)> {
    let v = match (v.get("ok"), v.get("value")) {
        (Some(Value::Bool(true)), Some(inner)) => inner,
        (Some(Value::Bool(false)), _) => {
            return Err(RemoteError::Permanent("Proton Drive has an item it can't decrypt".into()));
        }
        _ => v,
    };
    let id = v["uid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RemoteError::Permanent("Proton's CLI answered without a node uid".into()))?;
    let name = match &v["name"] {
        Value::String(s) => Some(s.as_str()),
        n if n["ok"] == Value::Bool(true) => n["value"].as_str(),
        _ => None,
    };
    let folder = v["type"] == "folder";
    let revision = &v["activeRevision"];
    let meta = RemoteMeta {
        id: id.to_string(),
        name: name.filter(|n| !n.is_empty()).unwrap_or(id).to_string(),
        parents: v["parentUid"].as_str().map(|p| vec![p.to_string()]).unwrap_or_default(),
        folder,
        hash: None,
        size: if folder { None } else { revision["claimedSize"].as_u64() },
        trashed: !v["trashTime"].is_null(),
    };
    let sha1 = revision["claimedDigests"]["sha1"].as_str().map(str::to_ascii_lowercase);
    Ok((meta, sha1))
}

/// `move` and `copy` print one `{uid, ok, error}` per item, and exit 0 when items failed.
fn check_results(out: &str, what: &str) -> RemoteResult<()> {
    let v = parse_json(out)?;
    let failed = v.as_array().into_iter().flatten().find(|r| r["ok"] != Value::Bool(true));
    match failed {
        None => Ok(()),
        Some(r) => {
            let why = r["error"]["message"].as_str().unwrap_or("Proton Drive refused it");
            Err(match classify(why, Path::new(DEFAULT_CLI)) {
                RemoteError::Permanent(m) => RemoteError::Permanent(format!("cannot {what}: {m}")),
                other => other,
            })
        }
    }
}

/// A failed command's message as a [`RemoteError`].
fn classify(text: &str, cli: &Path) -> RemoteError {
    let lower = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("==="))
        .unwrap_or("Proton's CLI failed")
        .to_string();
    if has(&["command not found", "unknown option", "unexpected argument", "arguments, got", "not supported"]) {
        RemoteError::Permanent(format!("this version of Proton's CLI doesn't work with CloudDrop: {line}"))
    } else if has(&["login first", "not logged in", "log in again", "session expired", "unauthorized"]) {
        RemoteError::Auth(format!("run `{} auth login`", cli.display()))
    } else if has(&["quota", "insufficient storage", "not enough storage", "storage is full", "storage full"]) {
        RemoteError::Quota(line)
    } else if has(&["too many requests", "rate limit", "429"]) {
        RemoteError::RateLimited { message: line, retry_after: None }
    } else if has(&["not found", "does not exist"]) {
        RemoteError::NotFound
    } else if has(&[
        "network", "fetch failed", "econn", "etimedout", "enotfound", "eai_again", "timed out", "timeout", "offline",
        "socket", "unavailable", "502", "503", "504",
    ]) {
        RemoteError::Transient(line)
    } else {
        RemoteError::Permanent(line)
    }
}

fn missing_cli(cli: &Path, e: &std::io::Error) -> String {
    format!(
        "cannot run Proton's CLI ({}: {e}). Install proton-drive from {CLI_DOWNLOAD}, or set its path with \
         `clouddrop setup --provider proton --set cli=/path/to/proton-drive`",
        cli.display()
    )
}

fn local_error(what: &str, e: &std::io::Error) -> RemoteError {
    RemoteError::Transient(format!("{what}: {e}"))
}

/// (SHA-1 in hex, size) of a file.
fn file_sha1(path: &Path) -> RemoteResult<(String, u64)> {
    let mut f = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => RemoteError::NotFound,
        _ => local_error("cannot read the staged upload", &e),
    })?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let hash = crate::hash::hash_reader(HashKind::Sha1, &mut f).map_err(|e| local_error("cannot read the staged upload", &e))?;
    Ok((hash.value, size))
}

async fn copy_hashing(path: &Path, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
    let mut f = tokio::fs::File::open(path).await.map_err(|e| local_error("cannot read the download", &e))?;
    let mut hasher = Hasher::new(HashKind::Sha256);
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = tokio::io::AsyncReadExt::read(&mut f, &mut buf).await.map_err(|e| local_error("cannot read the download", &e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).await.map_err(|e| local_error("cannot write the download", &e))?;
        total += n as u64;
    }
    out.flush().await.map_err(|e| local_error("cannot write the download", &e))?;
    Ok((hasher.finish(), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_nodes_as_the_cli_prints_them() {
        let file = json!({
            "uid": "V~f", "parentUid": "V~p", "name": {"ok": true, "value": "a.txt"}, "type": "file",
            "creationTime": "2026-10-21T00:00:00.000Z", "trashTime": null,
            "activeRevision": {"uid": "r", "claimedSize": 5, "claimedDigests": {"sha1": "AAF4", "sha1Verified": true}},
        });
        let (m, sha1) = node_meta(&file).unwrap();
        assert_eq!((m.id.as_str(), m.name.as_str(), m.parents.as_slice()), ("V~f", "a.txt", &["V~p".to_string()][..]));
        assert_eq!((m.folder, m.size, m.trashed, m.hash), (false, Some(5), false, None));
        assert_eq!(sha1.as_deref(), Some("aaf4"));

        let folder = json!({"ok": true, "value": {"uid": "V~d", "name": {"ok": false, "error": {}}, "type": "folder",
            "trashTime": "2026-10-20T00:00:00.000Z"}});
        let (m, sha1) = node_meta(&folder).unwrap();
        assert_eq!((m.name.as_str(), m.folder, m.trashed, m.size, sha1), ("V~d", true, true, None, None));
        assert!(m.parents.is_empty());
        assert!(node_meta(&json!({"name": "x"})).is_err());
        assert!(node_meta(&json!({"ok": false, "error": {}})).is_err());
    }

    #[test]
    fn errors_by_what_the_cli_says() {
        let cli = Path::new("proton-drive");
        assert!(matches!(classify("You need to login first", cli), RemoteError::Auth(m) if m.contains("proton-drive auth login")));
        assert_eq!(classify("Node not found: a.txt", cli), RemoteError::NotFound);
        assert!(matches!(classify("Insufficient storage quota", cli), RemoteError::Quota(_)));
        assert!(matches!(classify("HTTP 429 Too Many Requests", cli), RemoteError::RateLimited { .. }));
        assert!(matches!(classify("TypeError: fetch failed", cli), RemoteError::Transient(_)));
        assert!(matches!(classify("Command not found: filesystem info", cli), RemoteError::Permanent(m) if m.contains("version")));
        assert_eq!(
            classify("===============\nName conflict on \"a\" (file) already exists", cli),
            RemoteError::Permanent("Name conflict on \"a\" (file) already exists".into())
        );
    }

    #[test]
    fn paths_and_names() {
        assert_eq!(path_of("V~x").unwrap(), "/my-files/V~x");
        assert_eq!(path_of("a/b"), Err(RemoteError::NotFound));
        assert_eq!(path_of(""), Err(RemoteError::NotFound));
        assert_eq!(escape("a/b"), "a\\/b");
        assert_eq!(numbered("a (replaced 2026-10-21).txt", 2), "a (replaced 2026-10-21) (2).txt");
        assert_eq!(numbered("README", 3), "README (3)");
        assert_ne!(unique(), unique());
    }

    #[test]
    fn config_and_listing() {
        assert_eq!(ProtonConfig::default().program(), PathBuf::from("proton-drive"));
        assert_eq!(ProtonConfig { cli: " /opt/pd ".into() }.program(), PathBuf::from("/opt/pd"));
        assert!(ProtonConfig::default().is_default());
        assert!(INFO.experimental && INFO.caps.hash.is_none() && INFO.caps.stable_ids);
        let SignIn::Form(fields) = INFO.sign_in else { panic!("a form") };
        assert!(fields.iter().all(|f| !f.required && f.kind != FieldKind::Secret), "the CLI keeps the sign-in");
        assert!(fields[0].hint.contains(DISCLOSURE));
    }

    #[test]
    fn move_and_copy_results() {
        assert!(check_results("[\n{\"uid\":\"a\",\"ok\":true}\n]\n", "move").is_ok());
        assert!(check_results("[\n]\n", "move").is_ok());
        let e = check_results("[\n{\"uid\":\"a\",\"ok\":false,\"error\":{}}\n]\n", "move").unwrap_err();
        assert_eq!(e, RemoteError::Permanent("cannot move: Proton Drive refused it".into()));
    }
}
