//! The daemon: watches the sync folder, keeps the state database current and
//! schedules Drive jobs.
//!
//! The engine task is the only writer of entry rows while it runs (the CLI's
//! `free`/`restore` edit rows of idle entries only). Jobs report back through a
//! channel; an entry renamed or deleted while its job runs is tracked in
//! `inflight`, so results always land on the entry's current path.
//!
//! Provider differences come from [`Capabilities`]: the hash kind (or verify by download),
//! ids that are paths (a moved folder moves the ids beneath it), and name rules (forbidden
//! characters, names that differ only in case).

use crate::config::Config;
use crate::control::{self, DaemonStatus, Phase, Upload};
use crate::db::{ActivityKind, Db, Entry, Kind, State};
use crate::remote::{Capabilities, Provider, Remote, RemoteError, SignIn};
use crate::jobs::{self, FreeSkip, JobEvent, JobOutcome, JobSpec};
use crate::notifier::Notifier;
use crate::store::LocalStore;
use crate::util::{self, LocalMeta, now_ms};
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::event::{AccessKind, AccessMode, ModifyKind, RenameMode};
use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, watch};

const TICK: Duration = Duration::from_millis(250);
const AUTH_RECHECK: Duration = Duration::from_secs(600);
const MAX_CONTENT_RETRIES: i64 = 5;
const QUOTA_EVERY: Duration = Duration::from_secs(600);
const AUTO_FREE_EVERY: Duration = Duration::from_secs(30);
/// How long a pinned placeholder's download may take before it's tried again.
const HYDRATE_RETRY: Duration = Duration::from_secs(120);
const DAY_MS: i64 = 24 * 3600 * 1000;

struct Inflight {
    id: u64,
    /// The local item disappeared while the job ran.
    deleted: bool,
    placement_only: bool,
    /// Checking a synced file before freeing its local copy (not an upload).
    free: bool,
    /// A free check that downloads the cloud copy to prove it (no hash from the provider).
    verifying: bool,
    /// (cloud parent id, name) the job gives the item, so others there don't take the name.
    target: Option<(String, String)>,
    is_file: bool,
    /// Cancelled by a pause; its entry is re-queued once the task has stopped.
    aborted: bool,
    progress: (u64, u64),
    handle: tokio::task::JoinHandle<()>,
}

type StatusKey = (Phase, Option<String>, Vec<Upload>, u64, Option<control::Quota>);

#[derive(Default)]
struct Batch {
    files: u64,
    bytes: u64,
    freed_files: u64,
    freed_bytes: u64,
}

pub struct Engine {
    cfg: Config,
    root: PathBuf,
    db: Arc<Db>,
    remote: Arc<Remote>,
    provider: Provider,
    caps: Capabilities,
    ignore: GlobSet,
    root_id: String,
    /// Paths with recent filesystem events, processed once quiet for `settle_secs`.
    dirty: HashMap<String, Instant>,
    inflight: HashMap<String, Inflight>,
    next_id: u64,
    job_tx: mpsc::UnboundedSender<JobEvent>,
    data_dir: PathBuf,
    token_path: PathBuf,
    notifier: Notifier,
    /// Sign-in problem: (message, token file mtime then, when).
    paused: Option<(String, Option<SystemTime>, Instant)>,
    /// Paused from the app/CLI (meta "paused").
    user_paused: bool,
    last_transient: Option<(Instant, String)>,
    last_status: Option<StatusKey>,
    last_status_write: Instant,
    quota: Option<control::Quota>,
    last_quota: Option<Instant>,
    last_auto_free: Option<Instant>,
    /// Placeholders being downloaded because they were pinned ("Always keep on
    /// this device"), with when the download started.
    hydrating: HashMap<String, Instant>,
    store: Arc<dyn LocalStore>,
    config_file: Option<PathBuf>,
    config_mtime: Option<SystemTime>,
    last_config_check: Instant,
    /// `clouddrop service stop` asked us to exit.
    stop_now: bool,
    batch: Batch,
    idle_since: Option<Instant>,
    needs_rescan: bool,
    /// Windows: the "from" half of a rename, waiting for its "to" event.
    rename_from: Option<String>,
    last_rescan: Instant,
    last_error_note: Option<Instant>,
    warned: HashSet<String>,
}

fn backoff_ms(attempts: i64) -> i64 {
    let secs = 5i64.saturating_mul(1 << attempts.clamp(1, 10).saturating_sub(1));
    secs.min(900) * 1000
}

/// How long to wait before retrying after `err`: the backoff, or longer if the provider
/// asked for that (`Retry-After`).
fn retry_delay_ms(err: &RemoteError, attempts: i64) -> i64 {
    let asked = match err {
        RemoteError::RateLimited { retry_after: Some(secs), .. } => (*secs).min(24 * 3600) as i64 * 1000,
        _ => 0,
    };
    backoff_ms(attempts).max(asked)
}

/// What to tell people about a cloud error, naming the provider (Google Drive's wording is
/// as it always was).
pub fn describe(err: &RemoteError, provider: Provider) -> String {
    let name = provider.display_name();
    match err {
        RemoteError::Auth(m) => {
            let who = match provider.info().sign_in {
                SignIn::Google => "Google",
                _ => name,
            };
            format!("{who} sign-in needed: {m}")
        }
        RemoteError::NotFound => format!("not found in {name}"),
        RemoteError::Quota(m) if m.trim().is_empty() => format!("{name} storage is full"),
        RemoteError::Quota(m) => format!("{name} storage is full: {m}"),
        other => other.to_string(),
    }
}

/// The provider's name in short phrases ("in Drive"): Drive for Google Drive, as before.
pub fn short_name(provider: Provider) -> &'static str {
    if provider == Provider::GoogleDrive { "Drive" } else { provider.display_name() }
}

/// `name` without the characters a provider forbids: each becomes its full-width look-alike
/// (`:` → `：`, `?` → `？`), a control character its symbol (`\n` → `␊`), anything else `_`.
/// Look-alikes keep names readable and tell the originals apart.
/// The upload chunk for a provider: the configured size rounded down to a multiple of what the
/// provider requires (OneDrive 320 KiB, Dropbox 4 MiB…), and at least one unit.
pub fn upload_chunk(configured: usize, unit: usize) -> usize {
    let unit = unit.max(1);
    (configured - configured % unit).max(unit)
}

pub fn cloud_safe_name(name: &str, forbidden: &str) -> String {
    if !name.contains(|c| forbidden.contains(c)) {
        return name.to_string();
    }
    name.chars()
        .map(|c| {
            if !forbidden.contains(c) {
                return c;
            }
            let alike = match c {
                '!'..='~' => char::from_u32(c as u32 - 0x21 + 0xFF01),
                '\0'..='\x1f' => char::from_u32(c as u32 + 0x2400),
                '\x7f' => Some('\u{2421}'),
                _ => None,
            };
            alike.filter(|a| !forbidden.contains(*a)).unwrap_or('_')
        })
        .collect()
}

/// `name` is `base` or `base` numbered by the "name (2).ext" rule.
fn is_numbered_of(name: &str, base: &str) -> bool {
    if name == base {
        return true;
    }
    let (stem, ext) = match base.rfind('.') {
        Some(i) if i > 0 => (&base[..i], &base[i..]),
        _ => (base, ""),
    };
    name.strip_prefix(stem)
        .and_then(|r| r.strip_suffix(ext))
        .and_then(|r| r.strip_prefix(" ("))
        .and_then(|r| r.strip_suffix(')'))
        .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u32>().is_ok_and(|n| n >= 2))
}

/// The cloud name for an item whose safe name is `base`, among the names `taken` in the same
/// cloud folder: `current` while it is still `base` or a numbered `base` and free, otherwise
/// the first free one of `base`, "base (2)", "base (3)", … Names that differ only in case
/// clash where the provider ignores case.
pub fn choose_cloud_name(base: &str, current: Option<&str>, taken: &[String], case_insensitive: bool) -> String {
    let key = |s: &str| if case_insensitive { s.to_lowercase() } else { s.to_string() };
    let taken: HashSet<String> = taken.iter().map(|t| key(t)).collect();
    if let Some(c) = current
        && is_numbered_of(c, base)
        && !taken.contains(&key(c))
    {
        return c.to_string();
    }
    std::iter::once(base.to_string())
        .chain((2..).map(|n| util::numbered_name(base, n)))
        .find(|n| !taken.contains(&key(n)))
        .expect("unbounded")
}

fn build_ignore(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).with_context(|| format!("bad ignore pattern {p:?}"))?);
    }
    // Our own temp files, whatever the config says.
    b.add(Glob::new(".clouddrop-*")?);
    Ok(b.build()?)
}

/// The cloud copy can't be trusted: upload the local file again. If the cloud still has
/// a (different) file, the local content becomes its new version and the other
/// version is kept (pinned, or copied aside) so it is never lost; otherwise a new file is made.
pub fn mark_for_reupload(e: &mut Entry, drive_file_exists: bool) {
    if drive_file_exists {
        e.pin_prev = true;
    } else {
        e.drive_id = None;
        e.cloud_name = None;
        e.cloud_parent = None;
    }
    e.set_content_hash(None);
    e.uploaded_at = None;
    e.requeue();
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Optional parts of a daemon run.
pub struct RunOptions {
    /// Apply edits to this config file while running (no restart needed except for
    /// the folder, Drive folder name, Google client or endpoints).
    pub config_file: Option<PathBuf>,
    /// How local copies are represented (plain files, or Windows placeholders).
    pub store: Arc<dyn LocalStore>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            config_file: None,
            store: crate::store::platform(),
        }
    }
}

/// Runs the daemon until `shutdown` flips to true. `data_dir` holds the sign-in, status.json
/// and temporary verify downloads.
pub async fn run(
    cfg: Config,
    db: Arc<Db>,
    remote: Arc<Remote>,
    data_dir: PathBuf,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    run_with_options(cfg, RunOptions::default(), db, remote, data_dir, shutdown).await
}

pub async fn run_with_options(
    cfg: Config,
    opts: RunOptions,
    db: Arc<Db>,
    remote: Arc<Remote>,
    data_dir: PathBuf,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let caps = remote.caps().clone();
    run_with_caps(cfg, opts, caps, db, remote, data_dir, shutdown).await
}

/// [`run_with_options`] as if the provider had `caps`, so tests can apply another provider's
/// rules (no hash, ids that are paths, name rules) to the Google Drive mock.
pub async fn run_with_caps(
    cfg: Config,
    opts: RunOptions,
    caps: Capabilities,
    db: Arc<Db>,
    remote: Arc<Remote>,
    data_dir: PathBuf,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let RunOptions { config_file, store } = opts;
    let root = cfg.sync_dir.clone();
    std::fs::create_dir_all(&root)
        .with_context(|| format!("cannot create {}", root.display()))?;
    db.bind_root(&root)?;
    db.bind_provider(cfg.provider)?;
    let _ = control::clear_stop(&db); // a stale request must not stop this run
    let reset = db.reset_uploading()?;
    if reset > 0 {
        tracing::info!("resuming {reset} interrupted uploads");
    }
    jobs::remove_stale_downloads(&data_dir);

    let (job_tx, mut job_rx) = mpsc::unbounded_channel();
    let remote_provider = remote.provider();
    let mut eng = Engine {
        ignore: build_ignore(&cfg.ignore)?,
        notifier: Notifier::new(cfg.notifications),
        root,
        cfg,
        db,
        provider: remote.provider(),
        caps,
        remote,
        root_id: String::new(),
        dirty: HashMap::new(),
        inflight: HashMap::new(),
        next_id: 1,
        job_tx,
        token_path: control::sign_in_file(remote_provider, &data_dir),
        data_dir,
        paused: None,
        user_paused: false,
        last_transient: None,
        last_status: None,
        last_status_write: Instant::now(),
        quota: None,
        last_quota: None,
        last_auto_free: None,
        hydrating: HashMap::new(),
        store,
        config_mtime: config_file.as_deref().and_then(mtime),
        config_file,
        last_config_check: Instant::now(),
        stop_now: false,
        batch: Batch::default(),
        idle_since: None,
        needs_rescan: false,
        rename_from: None,
        last_rescan: Instant::now(),
        last_error_note: None,
        warned: HashSet::new(),
    };

    eng.write_status(Some((Phase::Starting, None)));
    match eng.ensure_root(&mut shutdown).await {
        Some(id) => eng.root_id = id,
        None => {
            let _ = std::fs::remove_file(control::status_path(&eng.data_dir));
            return Ok(());
        }
    }

    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel();
    let watcher = notify::recommended_watcher(move |res| {
        let _ = fs_tx.send(res);
    })
    .and_then(|mut w| w.watch(&eng.root, RecursiveMode::Recursive).map(|_| w));
    let _watcher = match watcher {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("cannot watch {}: {e}; falling back to a rescan every minute", eng.root.display());
            eng.cfg.rescan_secs = eng.cfg.rescan_secs.min(60);
            None
        }
    };

    eng.rescan();
    tracing::info!("watching {}", eng.root.display());

    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if eng.stop_now {
            tracing::info!("stopping on request");
            let _ = control::clear_stop(&eng.db);
            break;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            Some(ev) = fs_rx.recv() => eng.on_fs(ev),
            Some(ev) = job_rx.recv() => eng.on_job(ev),
            _ = tick.tick() => eng.on_tick(),
        }
    }
    for (_, job) in eng.inflight.drain() {
        job.handle.abort();
    }
    let _ = std::fs::remove_file(control::status_path(&eng.data_dir));
    Ok(())
}

impl Engine {
    // ---------- Drive root ----------

    /// Finds or creates the top folder in My Drive, waiting out network/sign-in problems.
    async fn ensure_root(&mut self, shutdown: &mut watch::Receiver<bool>) -> Option<String> {
        let mut last_msg = String::new();
        loop {
            match self.try_root().await {
                Ok(id) => return Some(id),
                Err(e) => {
                    let msg = describe(&e, self.provider);
                    tracing::warn!("cloud folder unavailable: {msg}");
                    if msg != last_msg && !e.is_transient() {
                        self.notifier.send("CloudDrop is paused", &msg);
                    }
                    let phase = match e {
                        RemoteError::Auth(_) => Phase::NeedsSignIn,
                        _ if e.is_transient() => Phase::Offline,
                        _ => Phase::DriveFolderMissing,
                    };
                    last_msg = msg.clone();
                    let wait = match e {
                        RemoteError::Auth(_) => 30,
                        _ if e.is_transient() => (retry_delay_ms(&e, 1) / 1000).max(30),
                        _ => 120,
                    };
                    // Retry early when `clouddrop login` writes a new token.
                    let token_mtime = mtime(&self.token_path);
                    for _ in 0..wait {
                        self.write_status(Some((phase, Some(msg.clone()))));
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                            _ = shutdown.changed() => return None,
                        }
                        if mtime(&self.token_path) != token_mtime {
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn try_root(&self) -> Result<String, RemoteError> {
        let provider = self.provider.display_name();
        if let Ok(Some(id)) = self.db.meta_get("root_id") {
            return match self.remote.get(&id).await {
                Ok(m) if !m.trashed => Ok(id),
                Ok(_) => Err(RemoteError::Permanent(format!(
                    "the \"{}\" folder is in your {provider} trash — restore it to resume uploads",
                    self.cfg.drive_folder
                ))),
                Err(RemoteError::NotFound) => Err(RemoteError::Permanent(format!(
                    "the \"{}\" folder was deleted from {provider} — run `clouddrop reset-cloud` to upload everything again",
                    self.cfg.drive_folder
                ))),
                Err(e) => Err(e),
            };
        }
        let name = cloud_safe_name(&self.cfg.drive_folder, self.caps.forbidden_chars);
        let top = self.remote.top_parent().await?;
        let id = match self.remote.find_child(&name, &top, true).await?.into_iter().next() {
            Some(existing) => existing.id,
            None => self.remote.create_folder(&name, &top).await?.id,
        };
        self.db
            .meta_set("root_id", &id)
            .map_err(|e| RemoteError::Permanent(e.to_string()))?;
        Ok(id)
    }

    // ---------- paths ----------

    fn rel(&mut self, abs: &Path) -> Option<String> {
        if abs.strip_prefix(&self.root).is_err() {
            return None;
        }
        let rel = util::rel_path(&self.root, abs);
        if rel.is_none() {
            self.warn_once(&format!("skipping {}: name is not valid UTF-8", abs.display()));
        }
        rel
    }

    fn ignored(&self, rel: &str) -> bool {
        rel.split('/').any(|part| self.ignore.is_match(part))
    }

    fn warn_once(&mut self, msg: &str) {
        if self.warned.insert(msg.to_string()) {
            tracing::warn!("{msg}");
        }
    }

    fn abs(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn mark_dirty(&mut self, rel: String) {
        if !rel.is_empty() && !self.ignored(&rel) {
            self.dirty.insert(rel, Instant::now());
        }
    }

    /// Drive id of the folder that should contain `rel`, if it exists yet.
    fn parent_drive_id(&self, rel: &str) -> Option<String> {
        let parent = util::parent_of(rel);
        if parent.is_empty() {
            return Some(self.root_id.clone());
        }
        self.db.get(parent).ok().flatten()?.drive_id
    }

    fn placed(&self, e: &Entry) -> bool {
        let Some(parent) = self.parent_drive_id(&e.path) else {
            return false;
        };
        e.cloud_parent.as_deref() == Some(parent.as_str())
            && e.cloud_name.as_deref() == Some(self.cloud_name_for(e, &parent).as_str())
    }

    /// The name `e` gets in the cloud folder `parent_id`: its local name, with the provider's
    /// forbidden characters replaced and numbered "name (2).ext" where it would clash with
    /// another item placed there (or being placed there now).
    fn cloud_name_for(&self, e: &Entry, parent_id: &str) -> String {
        let local = util::name_of(&e.path);
        let caps = &self.caps;
        if caps.forbidden_chars.is_empty() && !caps.case_insensitive {
            return local.to_string();
        }
        let base = cloud_safe_name(local, caps.forbidden_chars);
        let mut taken: Vec<String> = self
            .db
            .cloud_names_in(parent_id)
            .unwrap_or_else(|err| {
                tracing::error!("database read failed for names in {parent_id}: {err}");
                Vec::new()
            })
            .into_iter()
            .filter(|(p, _)| *p != e.path)
            .map(|(_, n)| n)
            .collect();
        taken.extend(self.inflight.iter().filter(|(p, _)| **p != e.path).filter_map(|(_, j)| {
            j.target.as_ref().filter(|(pid, _)| pid == parent_id).map(|(_, n)| n.clone())
        }));
        let current = e.cloud_name.as_deref().filter(|_| e.cloud_parent.as_deref() == Some(parent_id));
        choose_cloud_name(&base, current, &taken, caps.case_insensitive)
    }

    /// Where ids are paths, renaming or moving a folder changes the ids beneath it: nothing
    /// beneath a folder starts while the folder's job runs, and a folder waits for the jobs
    /// beneath it.
    fn waits_for_move(&self, e: &Entry) -> bool {
        if self.caps.stable_ids {
            return false;
        }
        let mut p = util::parent_of(&e.path);
        while !p.is_empty() {
            if self.inflight.contains_key(p) {
                return true;
            }
            p = util::parent_of(p);
        }
        let inside = format!("{}/", e.path);
        e.kind == Kind::Dir && self.inflight.keys().any(|k| k.starts_with(&inside))
    }

    /// A path not used by any entry or local file: "x (2).pdf", "x (3).pdf", …
    fn unique_path(&self, rel: &str) -> String {
        let (parent, name) = (util::parent_of(rel), util::name_of(rel));
        (2..)
            .map(|n| util::join_rel(parent, &util::numbered_name(name, n)))
            .find(|p| {
                self.db.get(p).ok().flatten().is_none()
                    && std::fs::symlink_metadata(self.abs(p)).is_err()
            })
            .expect("unbounded")
    }

    fn rekey_inflight(&mut self, from: &str, to: &str) {
        let keys: Vec<String> = self
            .inflight
            .keys()
            .filter(|k| *k == from || k.starts_with(&format!("{from}/")))
            .cloned()
            .collect();
        for k in keys {
            let job = self.inflight.remove(&k).expect("key");
            self.inflight.insert(format!("{to}{}", &k[from.len()..]), job);
        }
    }

    fn put(&self, e: &Entry) {
        if let Err(err) = self.db.put(e) {
            tracing::error!("database write failed for {}: {err}", e.path);
        }
    }

    fn activity(&self, kind: ActivityKind, path: &str, bytes: Option<i64>, detail: Option<&str>) {
        if let Err(err) = self.db.log_activity(kind, path, bytes, detail) {
            tracing::debug!("cannot record activity: {err}");
        }
    }

    fn get(&self, rel: &str) -> Option<Entry> {
        self.db.get(rel).unwrap_or_else(|err| {
            tracing::error!("database read failed for {rel}: {err}");
            None
        })
    }

    // ---------- filesystem events ----------

    fn on_fs(&mut self, ev: notify::Result<notify::Event>) {
        let ev = match ev {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!("watch error: {e}; rescanning");
                self.needs_rescan = true;
                return;
            }
        };
        if ev.need_rescan() {
            self.needs_rescan = true;
        }
        let rename_from = self.rename_from.take();
        match ev.kind {
            // Windows reports a rename as a "from" event immediately followed by a
            // "to" event; Linux sends one event with both paths.
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) if cfg!(windows) => {
                if let Some(rel) = ev.paths.first().and_then(|p| self.rel(p)) {
                    self.mark_dirty(rel.clone());
                    self.rename_from = Some(rel).filter(|r| !r.is_empty());
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) if cfg!(windows) => {
                let to = ev.paths.first().and_then(|p| self.rel(p)).filter(|r| !r.is_empty());
                if let Some(t) = &to {
                    self.mark_dirty(t.clone());
                }
                self.on_rename(rename_from, to);
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
                let from = self.rel(&ev.paths[0]).filter(|r| !r.is_empty());
                let to = self.rel(&ev.paths[1]).filter(|r| !r.is_empty());
                self.on_rename(from, to);
            }
            EventKind::Access(AccessKind::Close(AccessMode::Write))
            | EventKind::Create(_)
            | EventKind::Modify(_)
            | EventKind::Remove(_) => {
                for p in &ev.paths {
                    if let Some(rel) = self.rel(p) {
                        self.mark_dirty(rel);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_rename(&mut self, from: Option<String>, to: Option<String>) {
        match (from, to) {
            (Some(f), Some(t)) => match (self.ignored(&f), self.ignored(&t)) {
                (true, true) => {}
                (true, false) => self.mark_dirty(t),
                (false, true) => self.mark_dirty(f),
                (false, false) => {
                    self.rename_entries(&f, &t);
                    self.mark_dirty(t);
                }
            },
            (Some(f), None) => self.mark_dirty(f),
            (None, Some(t)) => self.mark_dirty(t),
            (None, None) => {}
        }
    }

    /// Applies a local rename/move to the database so Drive gets a rename instead of a re-upload.
    fn rename_entries(&mut self, from: &str, to: &str) {
        let Some(fe) = self.get(from) else {
            return;
        };
        match fe.kind {
            Kind::File => {
                if fe.drive_id.is_none() && !self.inflight.contains_key(from) {
                    // Never uploaded (often an editor's temp file renamed over the
                    // real one): just a new/changed file at `to`.
                    let _ = self.db.delete(from);
                    return;
                }
                if let Some(te) = self.get(to) {
                    self.displace(te);
                }
            }
            Kind::Dir => {
                if !self.db.subtree(to).unwrap_or_default().is_empty() {
                    // Destination already has history; treat as delete + new folder.
                    self.mark_dirty(from.to_string());
                    return;
                }
            }
        }
        if let Err(err) = self.db.move_subtree(from, to) {
            tracing::error!("cannot record move {from} -> {to}: {err}");
            self.needs_rescan = true;
            return;
        }
        if fe.drive_id.is_some() {
            self.activity(ActivityKind::Moved, to, None, Some(from));
        }
        self.rekey_inflight(from, to);
        if !self.inflight.contains_key(to)
            && let Some(mut e) = self.get(to)
        {
            e.requeue();
            self.put(&e);
        }
        tracing::info!("moved {from} -> {to}");
    }

    /// Moves an entry out of the way of a local item that took its path. Its Drive
    /// copy is kept and renamed "name (n)" so nothing in the cloud is lost.
    fn displace(&mut self, e: Entry) {
        let dest = self.unique_path(&e.path);
        if self.db.move_subtree(&e.path, &dest).is_err() {
            return;
        }
        self.rekey_inflight(&e.path, &dest);
        let Some(mut moved) = self.get(&dest) else {
            return;
        };
        if let Some(job) = self.inflight.get_mut(&dest) {
            job.deleted = true;
            return;
        }
        if moved.in_cloud() || self.db.subtree(&dest).map(|s| s.len() > 1).unwrap_or(false) {
            moved.state = State::CloudOnly;
            moved.move_pending = moved.drive_id.is_some();
            moved.upload_url = None;
            moved.error = None;
            moved.retry_at = None;
            self.put(&moved);
            tracing::info!("kept the cloud copy of {} as {dest}", e.path);
            self.activity(ActivityKind::KeptInCloud, &dest, moved.cloud_size, Some(&e.path));
        } else {
            let _ = self.db.delete(&dest);
        }
    }

    // ---------- reconciling one path ----------

    fn process_dirty(&mut self) {
        let settle = Duration::from_secs(self.cfg.settle_secs);
        let now = Instant::now();
        let mut ready: Vec<String> = self
            .dirty
            .iter()
            .filter(|(p, t)| now.duration_since(**t) >= settle && !self.inflight.contains_key(*p))
            .map(|(p, _)| p.clone())
            .collect();
        // Arrivals before departures, so a move whose rename event we missed is matched
        // by inode while the old entry is still active.
        ready.sort_by_cached_key(|p| {
            let gone = std::fs::symlink_metadata(self.abs(p)).is_err();
            (gone, p.matches('/').count(), p.clone())
        });
        for p in ready {
            self.dirty.remove(&p);
            self.process_path(&p, false);
        }
    }

    /// Brings the entry for `rel` (and, for folders, everything inside) in line with disk.
    fn process_path(&mut self, rel: &str, from_scan: bool) {
        if rel.is_empty() || self.ignored(rel) {
            return;
        }
        if self.inflight.contains_key(rel) {
            // Re-check once its job has reported back.
            self.dirty.entry(rel.to_string()).or_insert_with(Instant::now);
            return;
        }
        if from_scan && self.dirty.contains_key(rel) {
            return; // still settling
        }
        let md = match std::fs::symlink_metadata(self.abs(rel)) {
            Ok(md) => md,
            Err(_) => return self.handle_removed(rel),
        };
        if md.file_type().is_symlink() {
            self.warn_once(&format!("skipping symlink {rel}"));
            return;
        }
        if !md.is_dir() && !md.is_file() {
            return;
        }
        self.ensure_ancestors(rel);
        let meta = LocalMeta::at(&self.abs(rel), &md);
        if md.is_dir() {
            self.process_dir(rel, meta);
            self.scan_dir(rel);
        } else {
            self.process_file(rel, meta);
        }
    }

    fn scan_dir(&mut self, rel: &str) {
        let Ok(rd) = std::fs::read_dir(self.abs(rel)) else {
            return;
        };
        let mut names: Vec<String> = Vec::new();
        for ent in rd.flatten() {
            match ent.file_name().into_string() {
                Ok(n) => names.push(n),
                Err(n) => self.warn_once(&format!(
                    "skipping {}: name is not valid UTF-8",
                    self.abs(rel).join(n).display()
                )),
            }
        }
        names.sort();
        for n in names {
            self.process_path(&util::join_rel(rel, &n), true);
        }
    }

    /// Makes sure every folder above `rel` has an active entry.
    fn ensure_ancestors(&mut self, rel: &str) {
        let mut chain = Vec::new();
        let mut p = util::parent_of(rel);
        while !p.is_empty() {
            chain.push(p.to_string());
            p = util::parent_of(p);
        }
        for dir in chain.into_iter().rev() {
            let active = self
                .get(&dir)
                .is_some_and(|e| e.kind == Kind::Dir && e.state != State::CloudOnly);
            if !active && let Ok(md) = std::fs::symlink_metadata(self.abs(&dir)) {
                self.process_dir(&dir, LocalMeta::at(&self.abs(&dir), &md));
            }
        }
    }

    /// A new local item may be a tracked one that moved without a rename event we
    /// could pair (moved into a just-renamed folder, or while the daemon was stopped).
    /// Matches by inode, plus size and mtime for files. Folders only match entries
    /// whose removal hasn't been processed: inode numbers of deleted folders get reused.
    fn adopt_moved(&mut self, rel: &str, kind: Kind, meta: LocalMeta) -> bool {
        let candidates = self.db.by_inode(meta.inode).unwrap_or_default();
        let Some(old) = candidates.into_iter().find(|e| {
            e.kind == kind
                && e.path != rel
                && !self.inflight.contains_key(&e.path)
                && match kind {
                    Kind::Dir => e.state != State::CloudOnly,
                    Kind::File => {
                        e.local_size == Some(meta.size) && e.local_mtime_ns == Some(meta.mtime_ns)
                    }
                }
                // Gone from its old path (and not a hard link still there).
                && LocalMeta::read(&self.abs(&e.path))
                    .map(|m| m.inode != meta.inode)
                    .unwrap_or(true)
        }) else {
            return false;
        };
        let was_cloud_only = old.state == State::CloudOnly;
        self.rename_entries(&old.path, rel);
        let Some(mut e) = self.get(rel) else {
            return false;
        };
        if was_cloud_only && kind == Kind::File {
            // Same inode, size and mtime: almost surely the same content; if not,
            // never let it overwrite the cloud copy unpinned.
            e.pin_prev = true;
        }
        e.set_local(Some(meta));
        self.put(&e);
        true
    }

    fn process_dir(&mut self, rel: &str, meta: LocalMeta) {
        match self.get(rel) {
            None => {
                if !self.adopt_moved(rel, Kind::Dir, meta) {
                    self.put(&Entry::new(rel, Kind::Dir, Some(meta)));
                }
            }
            Some(e) if e.kind == Kind::File => {
                self.displace(e);
                self.put(&Entry::new(rel, Kind::Dir, Some(meta)));
            }
            Some(mut e) if e.state == State::CloudOnly => {
                // Folder is back locally: reuse its Drive folder.
                e.requeue();
                e.move_pending = false;
                e.set_local(Some(meta));
                self.put(&e);
            }
            Some(mut e) => {
                if e.inode != Some(meta.inode) {
                    e.set_local(Some(meta));
                    self.put(&e);
                }
            }
        }
    }

    fn process_file(&mut self, rel: &str, meta: LocalMeta) {
        if self.store.is_dehydrated(&self.abs(rel)) {
            return self.process_placeholder(rel, meta);
        }
        self.hydrating.remove(rel);
        match self.get(rel) {
            None => {
                if !self.adopt_moved(rel, Kind::File, meta) {
                    self.put(&Entry::new(rel, Kind::File, Some(meta)));
                }
            }
            Some(e) if e.kind == Kind::Dir => {
                self.displace(e);
                self.put(&Entry::new(rel, Kind::File, Some(meta)));
            }
            Some(mut e) if e.state == State::CloudOnly => {
                // A file reappeared where a cloud-only one was (downloaded back, or a new
                // file). Same content → nothing to upload; different → new revision,
                // keeping the old one forever.
                e.requeue();
                e.pin_prev = e.drive_id.is_some();
                e.move_pending = false;
                e.set_local(Some(meta));
                self.put(&e);
            }
            Some(mut e) => {
                if e.local() != Some(meta) {
                    e.requeue();
                    e.set_local(Some(meta));
                    self.put(&e);
                    self.store.pending(&self.abs(rel));
                } else if e.state == State::Synced && self.store.free_requested(&self.abs(rel)) {
                    // "Free up space" in the file manager: same check as always.
                    if self.waits_for_move(&e) {
                        self.dirty.insert(rel.to_string(), Instant::now());
                    } else {
                        self.dispatch(e, String::new(), String::new(), false, true);
                    }
                }
            }
        }
    }

    /// A placeholder without local data (Windows): never read it. If it was a
    /// local file before, the file manager freed it.
    fn process_placeholder(&mut self, rel: &str, meta: LocalMeta) {
        let Some(mut e) = self.get(rel) else {
            return; // not ours
        };
        if e.kind != Kind::File {
            return;
        }
        if e.state != State::CloudOnly && e.in_cloud() {
            e.state = State::CloudOnly;
            e.upload_url = None;
            e.upload_md5 = None;
            e.error = None;
            e.retry_at = None;
            self.activity(ActivityKind::Freed, rel, e.cloud_size, None);
        }
        e.set_local(Some(meta));
        self.put(&e);

        // "Always keep on this device": download it now.
        if self.store.is_pinned(&self.abs(rel)) {
            self.start_hydrate(rel, false);
        }
    }

    /// Downloads a placeholder's data in the background. It comes back through
    /// process_file as a known file, so nothing is uploaded.
    fn start_hydrate(&mut self, rel: &str, asked: bool) {
        let abs = self.abs(rel);
        let recent = self.hydrating.get(rel).is_some_and(|t| t.elapsed() < HYDRATE_RETRY);
        if !self.store.is_dehydrated(&abs) || (recent && !asked) {
            return;
        }
        self.hydrating.insert(rel.to_string(), Instant::now());
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(err) = store.hydrate(&abs) {
                tracing::warn!("cannot download {}: {err}", abs.display());
            }
        });
    }

    /// The local item at `rel` is gone. Drive is never touched: whatever it holds
    /// becomes cloud-only; entries that never reached Drive are dropped.
    fn handle_removed(&mut self, rel: &str) {
        let rows = self.db.subtree(rel).unwrap_or_default();
        for e in rows.into_iter().rev() {
            if let Some(job) = self.inflight.get_mut(&e.path) {
                job.deleted = true;
                continue;
            }
            if e.state == State::CloudOnly {
                continue;
            }
            self.settle_removed(e);
        }
    }

    fn settle_removed(&mut self, mut e: Entry) {
        let has_children = e.kind == Kind::Dir
            && self.db.subtree(&e.path).map(|s| s.len() > 1).unwrap_or(false);
        if e.in_cloud() || has_children {
            e.state = State::CloudOnly;
            e.upload_url = None;
            e.upload_md5 = None;
            e.pin_prev = false;
            e.attempts = 0;
            e.retry_at = None;
            e.error = None;
            if e.kind == Kind::File {
                self.activity(ActivityKind::KeptInCloud, &e.path, e.cloud_size, None);
            }
            self.put(&e);
        } else {
            let _ = self.db.delete(&e.path);
        }
    }

    // ---------- full rescan ----------

    /// Reconciles the whole folder: startup, inotify overflow, and periodically.
    fn rescan(&mut self) {
        self.needs_rescan = false;
        self.last_rescan = Instant::now();
        let started = Instant::now();
        self.scan_dir("");

        for e in self.db.subtree("").unwrap_or_default() {
            if e.state != State::CloudOnly
                && !self.inflight.contains_key(&e.path)
                && std::fs::symlink_metadata(self.abs(&e.path)).is_err()
                && self.get(&e.path).is_some_and(|cur| cur.state != State::CloudOnly)
            {
                self.handle_removed(&e.path);
            }
        }
        tracing::debug!("rescan took {:?}", started.elapsed());
    }

    // ---------- scheduling ----------

    /// Applies settings edited in the config file while running.
    fn reload_config(&mut self) {
        let Some(path) = self.config_file.clone() else {
            return;
        };
        if self.last_config_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_config_check = Instant::now();
        let m = mtime(&path);
        if m == self.config_mtime {
            return;
        }
        self.config_mtime = m;
        let new = match Config::load_from(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("ignoring config change: {e:#}");
                return;
            }
        };
        if new.sync_dir != self.cfg.sync_dir
            || new.provider != self.cfg.provider
            || new.drive_folder != self.cfg.drive_folder
            || new.client_id != self.cfg.client_id
            || new.endpoints != self.cfg.endpoints
        {
            tracing::warn!("folder, provider, cloud folder or Google client changed; restart CloudDrop to apply");
        }
        if new.ignore != self.cfg.ignore {
            match build_ignore(&new.ignore) {
                Ok(set) => {
                    self.ignore = set;
                    self.needs_rescan = true;
                }
                Err(e) => tracing::warn!("ignore patterns not applied: {e:#}"),
            }
        }
        self.notifier = Notifier::new(new.notifications);
        if new.auto_free_days != self.cfg.auto_free_days || new.keep_local != self.cfg.keep_local {
            self.last_auto_free = None;
        }
        self.cfg.parallel_uploads = new.parallel_uploads;
        self.cfg.settle_secs = new.settle_secs;
        self.cfg.rescan_secs = new.rescan_secs;
        self.cfg.notifications = new.notifications;
        self.cfg.ignore = new.ignore;
        self.cfg.auto_free_days = new.auto_free_days;
        self.cfg.keep_local = new.keep_local;
        tracing::info!("settings updated");
    }

    fn on_tick(&mut self) {
        self.reload_config();
        if control::stop_requested(&self.db) {
            self.stop_now = true;
        }
        for rel in control::take_hydrate_requests(&self.db) {
            self.start_hydrate(&rel, true);
        }
        if self.needs_rescan
            || self.last_rescan.elapsed() >= Duration::from_secs(self.cfg.rescan_secs)
        {
            self.rescan();
        }
        self.process_dirty();
        self.check_pause();
        self.check_user_pause();
        self.schedule();
        self.schedule_auto_free();
        self.refresh_quota();
        self.idle_notify();
        self.write_status(None);
    }

    /// Fetches account storage every few minutes for the desktop app.
    fn refresh_quota(&mut self) {
        if self.paused.is_some() || self.last_quota.is_some_and(|t| t.elapsed() < QUOTA_EVERY) {
            return;
        }
        self.last_quota = Some(Instant::now());
        let (remote, tx) = (self.remote.clone(), self.job_tx.clone());
        tokio::spawn(async move {
            match remote.storage_quota().await {
                Ok((limit, usage, in_drive)) => {
                    let _ = tx.send(JobEvent::Quota(limit, usage, in_drive));
                }
                Err(e) => tracing::debug!("storage quota unavailable: {e}"),
            }
        });
    }

    /// Automatic space saving: frees local copies of files uploaded long enough ago,
    /// after the same Drive checksum check as Free Up Space.
    fn schedule_auto_free(&mut self) {
        let Some(days) = self.cfg.auto_free_days else {
            return;
        };
        if self.paused.is_some()
            || self.user_paused
            || self.last_auto_free.is_some_and(|t| t.elapsed() < AUTO_FREE_EVERY)
            || self.inflight.len() >= self.cfg.parallel_uploads
        {
            return;
        }
        self.last_auto_free = Some(Instant::now());
        let cutoff = now_ms() - days as i64 * DAY_MS;
        let candidates = match self.db.free_candidates(cutoff, 32) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!("cannot read free candidates: {err}");
                return;
            }
        };
        for e in candidates {
            if self.inflight.len() >= self.cfg.parallel_uploads {
                break;
            }
            if self.cfg.keeps_local(&e.path)
                || self.inflight.contains_key(&e.path)
                || self.dirty.contains_key(&e.path)
                || self.store.is_pinned(&self.abs(&e.path))
                || self.waits_for_move(&e)
            {
                continue;
            }
            self.dispatch(e, String::new(), String::new(), false, true);
        }
    }

    /// Follows the pause switch in the database; pausing cancels running uploads
    /// (their sessions are saved, so resuming continues where they stopped).
    fn check_user_pause(&mut self) {
        let want = control::is_paused(&self.db);
        if want != self.user_paused {
            self.user_paused = want;
            if want {
                tracing::info!("paused");
                for job in self.inflight.values_mut() {
                    job.handle.abort();
                    job.aborted = true;
                }
            } else {
                tracing::info!("resumed");
            }
        }
        let stopped: Vec<String> = self
            .inflight
            .iter()
            .filter(|(_, j)| j.aborted && j.handle.is_finished())
            .map(|(p, _)| p.clone())
            .collect();
        for path in stopped {
            let job = self.inflight.remove(&path).expect("in flight");
            let Some(mut e) = self.get(&path) else {
                continue;
            };
            if job.deleted {
                self.settle_removed(e);
            } else if e.state == State::Uploading {
                e.state = State::Pending;
                self.put(&e);
            }
        }
    }

    fn current_phase(&self, waiting: u64) -> (Phase, Option<String>) {
        if self.user_paused {
            return (Phase::Paused, None);
        }
        if let Some((msg, _, _)) = &self.paused {
            return (Phase::NeedsSignIn, Some(msg.clone()));
        }
        if self.inflight.values().any(|j| !j.aborted && j.is_file && !j.free) {
            return (Phase::Uploading, None);
        }
        if waiting > 0 {
            if let Some((when, msg)) = &self.last_transient
                && when.elapsed() < Duration::from_secs(120)
            {
                return (Phase::Offline, Some(msg.clone()));
            }
            return (Phase::Waiting, None);
        }
        (Phase::Idle, None)
    }

    /// Publishes status.json for the desktop app: on change, and as a heartbeat. While free
    /// checks download files to prove them, `message` says so ([`control::VERIFYING`]).
    fn write_status(&mut self, phase: Option<(Phase, Option<String>)>) {
        let waiting = self.db.count_waiting().unwrap_or(0);
        let (phase, mut message) = phase.unwrap_or_else(|| self.current_phase(waiting));
        if message.is_none()
            && matches!(phase, Phase::Idle | Phase::Waiting | Phase::Uploading)
            && self.inflight.values().any(|j| j.verifying && !j.aborted)
        {
            message = Some(control::VERIFYING.to_string());
        }
        let mut uploads: Vec<Upload> = self
            .inflight
            .iter()
            .filter(|(_, j)| j.is_file && !j.aborted && !j.placement_only && !j.free)
            .map(|(p, j)| Upload {
                path: p.clone(),
                sent: j.progress.0,
                total: j.progress.1,
            })
            .collect();
        uploads.sort_by(|a, b| a.path.cmp(&b.path));
        let key = (phase, message, uploads, waiting, self.quota);
        let since = self.last_status_write.elapsed();
        let changed = self.last_status.as_ref() != Some(&key);
        if !(changed && since >= Duration::from_millis(500)) && since < Duration::from_secs(2) && self.last_status.is_some() {
            return;
        }
        let status = DaemonStatus {
            pid: std::process::id(),
            updated_ms: now_ms(),
            phase: key.0,
            message: key.1.clone(),
            uploads: key.2.clone(),
            waiting: key.3,
            quota: key.4,
        };
        if let Err(e) = control::write_status(&self.data_dir, &status) {
            tracing::debug!("cannot write status: {e}");
        }
        self.last_status = Some(key);
        self.last_status_write = Instant::now();
    }

    fn schedule(&mut self) {
        if self.paused.is_some()
            || self.user_paused
            || self.inflight.len() >= self.cfg.parallel_uploads
        {
            return;
        }
        let settle_ns = self.cfg.settle_secs as i64 * 1_000_000_000;
        let now_ns = now_ms() * 1_000_000;
        let candidates = match self.db.ready_jobs(now_ms(), 64) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!("cannot read the queue: {err}");
                return;
            }
        };
        for mut e in candidates {
            if self.inflight.len() >= self.cfg.parallel_uploads {
                break;
            }
            if self.inflight.contains_key(&e.path) || self.dirty.contains_key(&e.path) {
                continue;
            }
            // Give writers a moment even if we missed their events.
            if e.kind == Kind::File
                && e.state == State::Pending
                && e.local_mtime_ns.is_some_and(|m| now_ns - m < settle_ns)
            {
                continue;
            }
            let parent = util::parent_of(&e.path);
            if !parent.is_empty() && self.inflight.contains_key(parent) {
                continue;
            }
            if self.waits_for_move(&e) {
                continue;
            }
            let Some(parent_id) = self.parent_drive_id(&e.path) else {
                continue; // parent folder not in Drive yet
            };
            let placement_only = e.state == State::CloudOnly;
            let name = self.cloud_name_for(&e, &parent_id);
            let placed = e.cloud_name.as_deref() == Some(name.as_str())
                && e.cloud_parent.as_deref() == Some(parent_id.as_str());

            // Nothing to send: already named and placed right.
            if (e.kind == Kind::Dir || placement_only) && e.drive_id.is_some() && placed {
                if placement_only {
                    e.move_pending = false;
                } else {
                    e.state = State::Synced;
                }
                self.put(&e);
                if e.state == State::Synced
                    && let Some(id) = &e.drive_id
                {
                    self.store.synced(&self.abs(&e.path), id, true);
                }
                continue;
            }
            if placement_only && e.drive_id.is_none() {
                e.move_pending = false;
                self.put(&e);
                continue;
            }

            if !placement_only {
                e.state = State::Uploading;
                self.put(&e);
            }
            self.dispatch(e, name, parent_id, placement_only, false);
        }
    }

    fn dispatch(&mut self, e: Entry, name: String, parent_id: String, placement_only: bool, free: bool) {
        let id = self.next_id;
        self.next_id += 1;
        let path = e.path.clone();
        let is_file = e.kind == Kind::File;
        let total = e.local_size.unwrap_or(0) as u64;
        let target = (!free).then(|| (parent_id.clone(), name.clone()));
        let spec = JobSpec {
            id,
            abs: self.abs(&path),
            name,
            parent_id,
            placement_only,
            free,
            chunk: upload_chunk(self.cfg.chunk_size, self.caps.chunk_unit),
            entry: e,
            caps: self.caps.clone(),
            tmp_dir: self.data_dir.clone(),
        };
        tracing::debug!("job {id}: {path}{}", if free { " (free)" } else { "" });
        let handle = tokio::spawn(jobs::run(spec, self.remote.clone(), self.job_tx.clone()));
        self.inflight.insert(
            path,
            Inflight {
                id,
                deleted: false,
                placement_only,
                free,
                verifying: free && self.caps.hash.is_none(),
                target,
                is_file,
                aborted: false,
                progress: (0, total),
                handle,
            },
        );
    }

    fn path_of_job(&self, id: u64) -> Option<String> {
        self.inflight
            .iter()
            .find(|(_, j)| j.id == id)
            .map(|(p, _)| p.clone())
    }

    fn on_job(&mut self, ev: JobEvent) {
        match ev {
            JobEvent::Quota(limit, usage, usage_in_drive) => {
                self.quota = Some(control::Quota {
                    limit,
                    usage,
                    usage_in_drive,
                });
            }
            JobEvent::Progress { id, sent, total } => {
                if let Some(job) = self.inflight.values_mut().find(|j| j.id == id) {
                    job.progress = (sent, total);
                }
            }
            JobEvent::Session { id, url, md5 } => {
                if let Some(path) = self.path_of_job(id)
                    && let Some(mut e) = self.get(&path)
                {
                    e.upload_url = Some(url);
                    e.upload_md5 = Some(md5);
                    self.put(&e);
                }
            }
            JobEvent::Finished { id, outcome } => {
                let Some(path) = self.path_of_job(id) else {
                    return;
                };
                let job = self.inflight.remove(&path).expect("in flight");
                self.apply(&path, job, outcome);
                self.schedule();
            }
        }
    }

    fn apply(&mut self, path: &str, job: Inflight, outcome: JobOutcome) {
        let Some(mut e) = self.get(path) else {
            return;
        };
        if job.free {
            return self.apply_free(e, job, outcome);
        }
        match outcome {
            JobOutcome::FreeReady(_) | JobOutcome::FreeSkip(_) => {}
            JobOutcome::Done(d) => {
                let content_changed = e.kind == Kind::File && d.hash.is_some() && e.content_hash() != d.hash;
                let had_cloud_copy = e.md5.is_some();
                let came_back_from_cloud = e.pin_prev;
                e.drive_id = Some(d.drive_id);
                e.cloud_name = Some(d.cloud_name);
                e.cloud_parent = Some(d.cloud_parent);
                if e.kind == Kind::File && d.hash.is_some() {
                    e.set_content_hash(d.hash);
                    e.cloud_size = d.size;
                }
                if let Some(old) = &d.old_id
                    && let Err(err) = self.db.put_with_new_id(&e, old)
                {
                    tracing::error!("cannot record the new id of {path}: {err}");
                    self.needs_rescan = true;
                }
                e.upload_url = None;
                e.upload_md5 = None;
                e.error = None;
                e.retry_at = None;
                e.move_pending = false;
                if job.placement_only {
                    e.state = State::CloudOnly;
                    return self.put(&e);
                }
                e.pin_prev = false;
                self.last_transient = None;
                if content_changed {
                    e.uploaded_at = Some(now_ms());
                } else if came_back_from_cloud {
                    // Came back from cloud-only unchanged (downloaded back): keep it here
                    // until it changes, like a restore.
                    e.uploaded_at = None;
                }
                if d.uploaded_bytes > 0 {
                    self.batch.files += 1;
                    self.batch.bytes += d.uploaded_bytes;
                    tracing::info!("uploaded {path} ({})", util::human_bytes(d.uploaded_bytes));
                    let kind = if had_cloud_copy { ActivityKind::Updated } else { ActivityKind::Uploaded };
                    self.activity(kind, path, Some(d.uploaded_bytes as i64), None);
                    if self.cfg.auto_free_days == Some(0) {
                        self.last_auto_free = None; // free promptly after uploads
                    }
                }
                if job.deleted {
                    e.state = State::CloudOnly;
                    return self.put(&e);
                }
                if let Some(l) = d.local {
                    e.set_local(Some(l));
                }
                if !d.content_ok {
                    e.attempts += 1;
                    if e.attempts >= MAX_CONTENT_RETRIES {
                        e.state = State::Error;
                        e.error = Some("the file kept changing while it was uploading".into());
                    } else {
                        e.state = State::Pending;
                        e.retry_at = Some(now_ms() + backoff_ms(e.attempts));
                    }
                    return self.put(&e);
                }
                e.attempts = 0;
                let local_same = match LocalMeta::read(&self.abs(path)) {
                    Ok(now) => e.kind == Kind::Dir || e.local() == Some(now),
                    Err(_) => false,
                };
                e.state = if local_same && self.placed(&e) {
                    State::Synced
                } else {
                    State::Pending
                };
                self.put(&e);
                if e.state == State::Synced
                    && let Some(id) = &e.drive_id
                {
                    self.store.synced(&self.abs(path), id, e.kind == Kind::Dir);
                }
                if !local_same {
                    self.mark_dirty(path.to_string());
                }
            }
            JobOutcome::Vanished => {
                if job.deleted {
                    return self.settle_removed(e);
                }
                e.state = State::Pending;
                self.put(&e);
                // Let the next pass see the deletion (or the replacement file).
                self.dirty.insert(path.to_string(), Instant::now() - Duration::from_secs(3600));
            }
            JobOutcome::Failed(err) => {
                let base = if job.placement_only { State::CloudOnly } else { State::Pending };
                match &err {
                    RemoteError::Auth(msg) => {
                        e.state = base;
                        self.pause(msg.clone());
                    }
                    RemoteError::Transient(_) | RemoteError::RateLimited { .. } => {
                        let msg = err.to_string();
                        e.attempts += 1;
                        e.retry_at = Some(now_ms() + retry_delay_ms(&err, e.attempts));
                        e.state = base;
                        tracing::warn!("{path}: {msg} (retry #{})", e.attempts);
                        self.last_transient = Some((Instant::now(), msg.clone()));
                        e.error = Some(msg);
                    }
                    // Storage full is shown like any upload that can't succeed by itself.
                    RemoteError::NotFound | RemoteError::Quota(_) | RemoteError::Permanent(_) => {
                        let msg = describe(&err, self.provider);
                        tracing::error!("{path}: {msg}");
                        if job.placement_only {
                            e.move_pending = false;
                            e.state = State::CloudOnly;
                        } else {
                            e.state = State::Error;
                        }
                        e.error = Some(msg.clone());
                        self.note_error(path, &msg);
                        if !job.placement_only {
                            self.activity(ActivityKind::Failed, path, e.local_size, Some(&msg));
                        }
                    }
                }
                if job.deleted {
                    return self.settle_removed(e);
                }
                self.put(&e);
            }
        }
    }

    /// Result of a free check: delete the local copy only if it is still exactly
    /// the file that was verified against Drive.
    fn apply_free(&mut self, mut e: Entry, job: Inflight, outcome: JobOutcome) {
        let path = e.path.clone();
        match outcome {
            _ if job.deleted => self.mark_dirty(path),
            JobOutcome::FreeReady(checked) => {
                let abs = self.abs(&path);
                let unchanged = e.state == State::Synced
                    && LocalMeta::read(&abs).ok() == Some(checked)
                    && e.local() == Some(checked);
                if !unchanged {
                    self.mark_dirty(path);
                    return;
                }
                let id = e.drive_id.clone().unwrap_or_default();
                let asked = self.store.free_requested(&abs);
                if let Err(err) = self.store.free(&abs, &id) {
                    tracing::warn!("cannot free {path}: {err}");
                    return;
                }
                e.state = State::CloudOnly;
                self.put(&e);
                self.batch.freed_files += 1;
                self.batch.freed_bytes += checked.size as u64;
                tracing::info!("freed {path} ({})", util::human_bytes(checked.size as u64));
                let kind = if asked { ActivityKind::Freed } else { ActivityKind::AutoFreed };
                self.activity(kind, &path, Some(checked.size), None);
            }
            JobOutcome::FreeSkip(skip @ (FreeSkip::CloudDiffers | FreeSkip::CloudGone)) => {
                tracing::warn!("{path}: cloud copy is not identical ({skip:?}); uploading the local copy again");
                mark_for_reupload(&mut e, skip == FreeSkip::CloudDiffers);
                self.put(&e);
            }
            JobOutcome::FreeSkip(FreeSkip::LocalChanged) | JobOutcome::Vanished => self.mark_dirty(path),
            JobOutcome::Failed(RemoteError::Auth(msg)) => self.pause(msg),
            JobOutcome::Failed(err) => tracing::debug!("{path}: free check failed: {err}"),
            JobOutcome::Done(_) => {}
        }
    }

    // ---------- pause, notifications ----------

    fn pause(&mut self, msg: String) {
        if self.paused.is_none() {
            tracing::warn!("pausing uploads: {msg}");
            self.notifier.send("CloudDrop needs you to sign in again", &msg);
        }
        self.paused = Some((msg, mtime(&self.token_path), Instant::now()));
    }

    fn check_pause(&mut self) {
        if let Some((_, token_mtime, since)) = &self.paused
            && (mtime(&self.token_path) != *token_mtime || since.elapsed() >= AUTH_RECHECK)
        {
            tracing::info!("retrying after sign-in problem");
            self.paused = None;
        }
    }

    fn note_error(&mut self, path: &str, msg: &str) {
        if self
            .last_error_note
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        self.last_error_note = Some(Instant::now());
        self.notifier
            .send("CloudDrop could not upload a file", &format!("{path}: {msg}"));
    }

    fn idle_notify(&mut self) {
        if !self.inflight.is_empty() || (self.batch.files == 0 && self.batch.freed_files == 0) {
            self.idle_since = None;
            return;
        }
        let since = *self.idle_since.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_secs(3) {
            return;
        }
        let b = std::mem::take(&mut self.batch);
        let count = |n: u64| if n == 1 { "1 file".to_string() } else { format!("{n} files") };
        let provider = self.provider.display_name();
        if b.files > 0 {
            self.notifier.send(
                &format!("Uploaded to {provider}"),
                &format!("{} ({})", count(b.files), util::human_bytes(b.bytes)),
            );
        }
        if b.freed_files > 0 {
            self.notifier.send(
                "Freed up space",
                &format!(
                    "{} freed on this computer ({}); they stay in {provider}",
                    util::human_bytes(b.freed_bytes),
                    count(b.freed_files)
                ),
            );
        }
        self.idle_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_chunks_fit_each_provider() {
        let mib = 1024 * 1024;
        assert_eq!(upload_chunk(8 * mib, 256 * 1024), 8 * mib);
        assert_eq!(upload_chunk(8 * mib, 320 * 1024), 25 * 320 * 1024, "OneDrive: a multiple of 320 KiB");
        assert_eq!(upload_chunk(8 * mib, 4 * mib), 8 * mib);
        assert_eq!(upload_chunk(mib, 4 * mib), 4 * mib, "at least one unit");
        assert_eq!(upload_chunk(8 * mib, 0), 8 * mib);
    }

    #[test]
    fn forbidden_characters_become_look_alikes() {
        let windows = "\"*:<>?\\|";
        assert_eq!(cloud_safe_name("plain name.txt", windows), "plain name.txt");
        assert_eq!(cloud_safe_name("a:b?.txt", windows), "a：b？.txt");
        assert_eq!(cloud_safe_name("<\"x\"|*\\>", windows), "＜＂x＂｜＊＼＞");
        assert_eq!(cloud_safe_name("tab\there", "\t"), "tab␉here");
        assert_eq!(cloud_safe_name("x\u{7f}", "\u{7f}"), "x\u{2421}");
        assert_eq!(cloud_safe_name("café", "é"), "caf_", "no look-alike: underscore");
        assert_eq!(cloud_safe_name("a:b", ":："), "a_b", "never a look-alike that is forbidden too");
        // Different originals stay different.
        assert_ne!(cloud_safe_name("a:b", windows), cloud_safe_name("a?b", windows));
    }

    #[test]
    fn numbered_names_are_recognised() {
        assert!(is_numbered_of("report.pdf", "report.pdf"));
        assert!(is_numbered_of("report (2).pdf", "report.pdf"));
        assert!(is_numbered_of("report (13).pdf", "report.pdf"));
        assert!(is_numbered_of("Makefile (3)", "Makefile"));
        assert!(is_numbered_of(".bashrc (2)", ".bashrc"));
        assert!(!is_numbered_of("report (1).pdf", "report.pdf"));
        assert!(!is_numbered_of("report (x).pdf", "report.pdf"));
        assert!(!is_numbered_of("report ().pdf", "report.pdf"));
        assert!(!is_numbered_of("Report.pdf", "report.pdf"));
        assert!(!is_numbered_of("report (2).txt", "report.pdf"));
    }

    #[test]
    fn cloud_names_avoid_clashes_and_stay_put() {
        let taken = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(choose_cloud_name("a.txt", None, &taken(&[]), true), "a.txt");
        assert_eq!(choose_cloud_name("a.txt", None, &taken(&["A.TXT"]), true), "a (2).txt");
        assert_eq!(choose_cloud_name("a.txt", None, &taken(&["A.TXT"]), false), "a.txt");
        assert_eq!(choose_cloud_name("a.txt", None, &taken(&["a.txt", "A (2).txt"]), true), "a (3).txt");
        // An item keeps its numbered name even after the clash is gone...
        assert_eq!(choose_cloud_name("a.txt", Some("a (2).txt"), &taken(&[]), true), "a (2).txt");
        // ...but not one that is no longer its name, or that someone else holds now.
        assert_eq!(choose_cloud_name("b.txt", Some("a (2).txt"), &taken(&[]), true), "b.txt");
        assert_eq!(choose_cloud_name("a.txt", Some("a (2).txt"), &taken(&["A (2).TXT"]), true), "a.txt");
        // A case-only rename is a rename.
        assert_eq!(choose_cloud_name("report.pdf", Some("Report.pdf"), &taken(&[]), true), "report.pdf");
    }

    #[test]
    fn errors_name_the_provider_and_keep_google_wording() {
        let p = Provider::GoogleDrive;
        assert_eq!(describe(&RemoteError::Auth("x".into()), p), "Google sign-in needed: x");
        assert_eq!(describe(&RemoteError::NotFound, p), "not found in Google Drive");
        assert_eq!(describe(&RemoteError::Quota(String::new()), p), "Google Drive storage is full");
        assert_eq!(describe(&RemoteError::Quota("403 quota".into()), p), "Google Drive storage is full: 403 quota");
        assert_eq!(describe(&RemoteError::Permanent("400 bad".into()), p), "400 bad");
        assert_eq!(short_name(p), "Drive");
    }

    #[test]
    fn retry_after_sets_the_minimum_wait() {
        let limited = |s| RemoteError::RateLimited { message: "slow down".into(), retry_after: s };
        assert_eq!(retry_delay_ms(&RemoteError::Transient("x".into()), 1), 5_000);
        assert_eq!(retry_delay_ms(&limited(None), 2), 10_000);
        assert_eq!(retry_delay_ms(&limited(Some(120)), 1), 120_000);
        assert_eq!(retry_delay_ms(&limited(Some(1)), 3), 20_000, "never less than the backoff");
    }
}
