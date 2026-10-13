//! The daemon: watches the sync folder, keeps the state database current and
//! schedules Drive jobs.
//!
//! The engine task is the only writer of entry rows while it runs (the CLI's
//! `free`/`restore` edit rows of idle entries only). Jobs report back through a
//! channel; an entry renamed or deleted while its job runs is tracked in
//! `inflight`, so results always land on the entry's current path.

use crate::config::Config;
use crate::control::{self, DaemonStatus, Phase, Upload};
use crate::db::{ActivityKind, Db, Entry, Kind, State};
use crate::drive::{Drive, DriveError};
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
const DAY_MS: i64 = 24 * 3600 * 1000;

struct Inflight {
    id: u64,
    /// The local item disappeared while the job ran.
    deleted: bool,
    placement_only: bool,
    /// Checking a synced file before freeing its local copy (not an upload).
    free: bool,
    is_file: bool,
    /// Cancelled by a pause; its entry is re-queued once the task has stopped.
    aborted: bool,
    progress: (u64, u64),
    handle: tokio::task::JoinHandle<()>,
}

type StatusKey = (
    Phase,
    Option<String>,
    Vec<Upload>,
    u64,
    Option<control::Quota>,
);

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
    drive: Arc<Drive>,
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

fn build_ignore(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).with_context(|| format!("bad ignore pattern {p:?}"))?);
    }
    // Our own temp files, whatever the config says.
    b.add(Glob::new(".clouddrop-*")?);
    Ok(b.build()?)
}

/// The Drive copy can't be trusted: upload the local file again. If Drive still has
/// a (different) file, the local content becomes its new version and the other
/// version is pinned so it is never purged; otherwise a new Drive file is made.
pub fn mark_for_reupload(e: &mut Entry, drive_file_exists: bool) {
    if drive_file_exists {
        e.pin_prev = true;
    } else {
        e.drive_id = None;
        e.cloud_name = None;
        e.cloud_parent = None;
    }
    e.md5 = None;
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

/// Runs the daemon until `shutdown` flips to true. `data_dir` holds token.json and status.json.
pub async fn run(
    cfg: Config,
    db: Arc<Db>,
    drive: Arc<Drive>,
    data_dir: PathBuf,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    run_with_options(cfg, RunOptions::default(), db, drive, data_dir, shutdown).await
}

pub async fn run_with_options(
    cfg: Config,
    opts: RunOptions,
    db: Arc<Db>,
    drive: Arc<Drive>,
    data_dir: PathBuf,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let RunOptions { config_file, store } = opts;
    let root = cfg.sync_dir.clone();
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
    db.bind_root(&root)?;
    let _ = control::clear_stop(&db); // a stale request must not stop this run
    let reset = db.reset_uploading()?;
    if reset > 0 {
        tracing::info!("resuming {reset} interrupted uploads");
    }

    let (job_tx, mut job_rx) = mpsc::unbounded_channel();
    let mut eng = Engine {
        ignore: build_ignore(&cfg.ignore)?,
        notifier: Notifier::new(cfg.notifications),
        root,
        cfg,
        db,
        drive,
        root_id: String::new(),
        dirty: HashMap::new(),
        inflight: HashMap::new(),
        next_id: 1,
        job_tx,
        token_path: data_dir.join("token.json"),
        data_dir,
        paused: None,
        user_paused: false,
        last_transient: None,
        last_status: None,
        last_status_write: Instant::now(),
        quota: None,
        last_quota: None,
        last_auto_free: None,
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
            tracing::warn!(
                "cannot watch {}: {e}; falling back to a rescan every minute",
                eng.root.display()
            );
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
                    let msg = e.to_string();
                    tracing::warn!("Drive folder unavailable: {msg}");
                    if msg != last_msg && !matches!(e, DriveError::Transient(_)) {
                        self.notifier.send("CloudDrop is paused", &msg);
                    }
                    let phase = match e {
                        DriveError::Auth(_) => Phase::NeedsSignIn,
                        DriveError::Transient(_) => Phase::Offline,
                        _ => Phase::DriveFolderMissing,
                    };
                    last_msg = msg.clone();
                    let wait = match e {
                        DriveError::Transient(_) | DriveError::Auth(_) => 30,
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

    async fn try_root(&self) -> Result<String, DriveError> {
        if let Ok(Some(id)) = self.db.meta_get("root_id") {
            return match self.drive.get(&id).await {
                Ok(m) if !m.trashed => Ok(id),
                Ok(_) => Err(DriveError::Permanent(format!(
                    "the \"{}\" folder is in your Google Drive trash — restore it to resume uploads",
                    self.cfg.drive_folder
                ))),
                Err(DriveError::NotFound) => Err(DriveError::Permanent(format!(
                    "the \"{}\" folder was deleted from Google Drive — run `clouddrop reset-cloud` to upload everything again",
                    self.cfg.drive_folder
                ))),
                Err(e) => Err(e),
            };
        }
        let name = &self.cfg.drive_folder;
        let id = match self
            .drive
            .find_child(name, "root", true)
            .await?
            .into_iter()
            .next()
        {
            Some(existing) => existing.id,
            None => self.drive.create_folder(name, "root").await?.id,
        };
        self.db
            .meta_set("root_id", &id)
            .map_err(|e| DriveError::Permanent(e.to_string()))?;
        Ok(id)
    }

    // ---------- paths ----------

    fn rel(&mut self, abs: &Path) -> Option<String> {
        if abs.strip_prefix(&self.root).is_err() {
            return None;
        }
        let rel = util::rel_path(&self.root, abs);
        if rel.is_none() {
            self.warn_once(&format!(
                "skipping {}: name is not valid UTF-8",
                abs.display()
            ));
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
        e.cloud_name.as_deref() == Some(util::name_of(&e.path))
            && e.cloud_parent.is_some()
            && e.cloud_parent == self.parent_drive_id(&e.path)
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
            self.inflight
                .insert(format!("{to}{}", &k[from.len()..]), job);
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
                let to = ev
                    .paths
                    .first()
                    .and_then(|p| self.rel(p))
                    .filter(|r| !r.is_empty());
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
            self.activity(
                ActivityKind::KeptInCloud,
                &dest,
                moved.cloud_size,
                Some(&e.path),
            );
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
            self.dirty
                .entry(rel.to_string())
                .or_insert_with(Instant::now);
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
            && self
                .db
                .subtree(&e.path)
                .map(|s| s.len() > 1)
                .unwrap_or(false);
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
                && self
                    .get(&e.path)
                    .is_some_and(|cur| cur.state != State::CloudOnly)
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
            || new.drive_folder != self.cfg.drive_folder
            || new.client_id != self.cfg.client_id
            || new.endpoints != self.cfg.endpoints
        {
            tracing::warn!(
                "folder, Drive folder or Google client changed; restart CloudDrop to apply"
            );
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
        let (drive, tx) = (self.drive.clone(), self.job_tx.clone());
        tokio::spawn(async move {
            match drive.storage_quota().await {
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
            || self
                .last_auto_free
                .is_some_and(|t| t.elapsed() < AUTO_FREE_EVERY)
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
        if self
            .inflight
            .values()
            .any(|j| !j.aborted && j.is_file && !j.free)
        {
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

    /// Publishes status.json for the desktop app: on change, and as a heartbeat.
    fn write_status(&mut self, phase: Option<(Phase, Option<String>)>) {
        let waiting = self.db.count_waiting().unwrap_or(0);
        let (phase, message) = phase.unwrap_or_else(|| self.current_phase(waiting));
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
        if !(changed && since >= Duration::from_millis(500))
            && since < Duration::from_secs(2)
            && self.last_status.is_some()
        {
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
            let Some(parent_id) = self.parent_drive_id(&e.path) else {
                continue; // parent folder not in Drive yet
            };
            let placement_only = e.state == State::CloudOnly;
            let name = util::name_of(&e.path).to_string();

            // Nothing to send: already named and placed right.
            if (e.kind == Kind::Dir || placement_only) && e.drive_id.is_some() && self.placed(&e) {
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

    fn dispatch(
        &mut self,
        e: Entry,
        name: String,
        parent_id: String,
        placement_only: bool,
        free: bool,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        let path = e.path.clone();
        let is_file = e.kind == Kind::File;
        let total = e.local_size.unwrap_or(0) as u64;
        let spec = JobSpec {
            id,
            abs: self.abs(&path),
            name,
            parent_id,
            placement_only,
            free,
            chunk: self.cfg.chunk_size,
            entry: e,
        };
        tracing::debug!("job {id}: {path}{}", if free { " (free)" } else { "" });
        let handle = tokio::spawn(jobs::run(spec, self.drive.clone(), self.job_tx.clone()));
        self.inflight.insert(
            path,
            Inflight {
                id,
                deleted: false,
                placement_only,
                free,
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
                let content_changed = e.kind == Kind::File && e.md5 != d.md5;
                let had_cloud_copy = e.md5.is_some();
                let came_back_from_cloud = e.pin_prev;
                e.drive_id = Some(d.drive_id);
                e.cloud_name = Some(d.cloud_name);
                e.cloud_parent = Some(d.cloud_parent);
                if e.kind == Kind::File {
                    e.md5 = d.md5;
                    e.cloud_size = d.size;
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
                    let kind = if had_cloud_copy {
                        ActivityKind::Updated
                    } else {
                        ActivityKind::Uploaded
                    };
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
                self.dirty
                    .insert(path.to_string(), Instant::now() - Duration::from_secs(3600));
            }
            JobOutcome::Failed(err) => {
                let base = if job.placement_only {
                    State::CloudOnly
                } else {
                    State::Pending
                };
                match err {
                    DriveError::Auth(msg) => {
                        e.state = base;
                        self.pause(msg);
                    }
                    DriveError::Transient(msg) => {
                        e.attempts += 1;
                        e.retry_at = Some(now_ms() + backoff_ms(e.attempts));
                        e.state = base;
                        tracing::warn!("{path}: {msg} (retry #{})", e.attempts);
                        self.last_transient = Some((Instant::now(), msg.clone()));
                        e.error = Some(msg);
                    }
                    DriveError::NotFound | DriveError::Permanent(_) => {
                        let msg = err.to_string();
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
                if let Err(err) = self.store.free(&abs, &id) {
                    tracing::warn!("cannot free {path}: {err}");
                    return;
                }
                e.state = State::CloudOnly;
                self.put(&e);
                self.batch.freed_files += 1;
                self.batch.freed_bytes += checked.size as u64;
                tracing::info!("freed {path} ({})", util::human_bytes(checked.size as u64));
                self.activity(ActivityKind::AutoFreed, &path, Some(checked.size), None);
            }
            JobOutcome::FreeSkip(skip @ (FreeSkip::CloudDiffers | FreeSkip::CloudGone)) => {
                tracing::warn!(
                    "{path}: Drive copy is not identical ({skip:?}); uploading the local copy again"
                );
                mark_for_reupload(&mut e, skip == FreeSkip::CloudDiffers);
                self.put(&e);
            }
            JobOutcome::FreeSkip(FreeSkip::LocalChanged) | JobOutcome::Vanished => {
                self.mark_dirty(path)
            }
            JobOutcome::Failed(DriveError::Auth(msg)) => self.pause(msg),
            JobOutcome::Failed(err) => tracing::debug!("{path}: free check failed: {err}"),
            JobOutcome::Done(_) => {}
        }
    }

    // ---------- pause, notifications ----------

    fn pause(&mut self, msg: String) {
        if self.paused.is_none() {
            tracing::warn!("pausing uploads: {msg}");
            self.notifier
                .send("CloudDrop needs you to sign in again", &msg);
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
        self.notifier.send(
            "CloudDrop could not upload a file",
            &format!("{path}: {msg}"),
        );
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
        let count = |n: u64| {
            if n == 1 {
                "1 file".to_string()
            } else {
                format!("{n} files")
            }
        };
        if b.files > 0 {
            self.notifier.send(
                "Uploaded to Google Drive",
                &format!("{} ({})", count(b.files), util::human_bytes(b.bytes)),
            );
        }
        if b.freed_files > 0 {
            self.notifier.send(
                "Freed up space",
                &format!(
                    "{} freed on this computer ({}); they stay in Google Drive",
                    util::human_bytes(b.freed_bytes),
                    count(b.freed_files)
                ),
            );
        }
        self.idle_since = None;
    }
}
