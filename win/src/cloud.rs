//! The File Explorer cloud folder (Cloud Files API): registering the sync root,
//! and the callbacks Windows makes when a placeholder is opened, freed, deleted
//! or renamed. Upload-only rules hold here too: deletes and renames are allowed
//! locally and never touch the cloud; freeing is allowed only for files whose cloud
//! copy is verified (the provider's hash, or a download where it reports none).

use anyhow::{Result, bail};
use clouddrop::config::Config;
use clouddrop::connections::Connection as ConnectionInfo;
use clouddrop::db::{Db, State};
use clouddrop::jobs::{local_kind, proven};
use clouddrop::remote::Remote;
use clouddrop::store::LocalStore;
use clouddrop::winstore::{WinStore, is_sync_root};
use cloud_filter::error::{CResult, CloudErrorKind};
use cloud_filter::filter::{Request, SyncFilter, info, ticket};
use cloud_filter::root::{
    Connection, HydrationType, PopulationType, ProtectionMode, SecurityId, Session, SyncRootId,
    SyncRootIdBuilder, SyncRootInfo, active_roots, is_supported,
};
use cloud_filter::utility::WriteAt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Download unit when Explorer opens a placeholder; the API wants multiples of 4 KiB.
const CHUNK: u64 = 4 << 20;
const PAGE: u64 = 4096;

pub struct Filter {
    rt: tokio::runtime::Handle,
    remote: Arc<Remote>,
    root: PathBuf,
    db_path: PathBuf,
    /// The connection's data directory, for temporary verify downloads.
    data_dir: PathBuf,
}

/// The provider name of every CloudDrop sync root: `CloudDrop!<user SID>!<connection name>`.
const PROVIDER: &str = "CloudDrop";

/// How File Explorer's navigation pane names a connection's folder: "CloudDrop" for the default
/// connection (as it always did), "CloudDrop - Work" for the others.
pub fn display_name(conn: &ConnectionInfo) -> String {
    if conn.is_default() { PROVIDER.to_string() } else { format!("{PROVIDER} - {}", conn.name) }
}

/// Registers a connection's folder with File Explorer (once) and connects to it.
fn connect(
    conn: &ConnectionInfo,
    cfg: &Config,
    remote: Arc<Remote>,
    rt: tokio::runtime::Handle,
) -> Result<Connection<Filter>> {
    if !is_supported()? {
        bail!("this version of Windows has no cloud files support");
    }
    let id = sync_root_id(&conn.name)?;
    if id.is_registered()? && !same_folder(&id.info()?.path(), &cfg.sync_dir) {
        // The connection's folder changed (removed and added again while the agent wasn't
        // running): the old folder stops being a cloud folder.
        let old = id.info()?.path();
        release(&old);
        id.unregister()?;
        tracing::info!("{} is no longer a cloud folder", old.display());
    }
    if !id.is_registered()? {
        let exe = std::env::current_exe()?;
        let info = SyncRootInfo::default()
            .with_display_name(display_name(conn))
            .with_icon(format!("{},0", exe.display()))
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_hydration_type(HydrationType::Full)
            .with_population_type(PopulationType::AlwaysFull)
            .with_allow_pinning(true)
            .with_protection_mode(ProtectionMode::Personal)
            .with_path(&cfg.sync_dir)?;
        id.register(info)?;
        tracing::info!("registered {} with File Explorer", cfg.sync_dir.display());
    }
    let filter = Filter {
        rt,
        remote,
        root: cfg.sync_dir.clone(),
        db_path: conn.db_path(),
        data_dir: conn.data.clone(),
    };
    Ok(Session::new().connect(&cfg.sync_dir, filter)?)
}

/// A connection's folder connected to File Explorer, held on a thread of its own from connect
/// to disconnect; dropping it disconnects.
pub struct Link {
    done: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Link {
    pub fn open(conn: &ConnectionInfo, cfg: &Config, remote: Arc<Remote>, rt: tokio::runtime::Handle) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let (done, done_rx) = std::sync::mpsc::channel::<()>();
        let (conn, cfg) = (conn.clone(), cfg.clone());
        let thread = std::thread::spawn(move || match connect(&conn, &cfg, remote, rt) {
            Ok(connection) => {
                let _ = ready_tx.send(Ok(()));
                let _ = done_rx.recv();
                drop(connection);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        });
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { done, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => bail!("connecting the folder to File Explorer failed"),
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.done.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn sync_root_id(name: &str) -> Result<SyncRootId> {
    Ok(SyncRootIdBuilder::new(PROVIDER)
        .user_security_id(SecurityId::current_user()?)
        .account_name(name)
        .build())
}

fn same_folder(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

/// This user's CloudDrop sync roots: (connection name, id, folder).
fn our_sync_roots() -> Result<Vec<(String, SyncRootId, PathBuf)>> {
    let prefix = sync_root_id("")?.to_os_string().to_string_lossy().into_owned();
    Ok(active_roots()?
        .into_iter()
        .filter_map(|info| {
            let id = info.id();
            let text = id.to_os_string().to_string_lossy().into_owned();
            let name = text.strip_prefix(&prefix)?.to_string();
            Some((name, id, info.path()))
        })
        .collect())
}

/// A removed connection's folder stops being a cloud folder: files with their data become
/// ordinary files; placeholders of freed files, which have none, are removed (they stay in the
/// cloud). Nothing in the cloud is touched.
pub fn forget(name: &str, sync_dir: &Path) {
    if is_sync_root(sync_dir) {
        release(sync_dir);
    }
    match sync_root_id(name).and_then(|id| Ok(if id.is_registered()? { Some(id) } else { None })) {
        Ok(Some(id)) => match id.unregister() {
            Ok(()) => tracing::info!("{} is no longer a cloud folder", sync_dir.display()),
            Err(e) => tracing::warn!("cannot remove {} from File Explorer: {e}", sync_dir.display()),
        },
        Ok(None) => {}
        Err(e) => tracing::warn!("cannot look up the cloud folder of \"{name}\": {e:#}"),
    }
}

/// At start: the cloud folders of connections that were removed (or moved to another folder)
/// while the agent wasn't running stop being cloud folders.
pub fn forget_removed() {
    let roots = match our_sync_roots() {
        Ok(r) => r,
        Err(e) => return tracing::warn!("cannot list the cloud folders: {e:#}"),
    };
    for (name, id, path) in roots {
        let kept = ConnectionInfo::named(&name)
            .ok()
            .filter(ConnectionInfo::exists)
            .and_then(|c| c.load().ok())
            .is_some_and(|cfg| same_folder(&cfg.sync_dir, &path));
        if kept {
            continue;
        }
        if is_sync_root(&path) {
            release(&path);
        }
        match id.unregister() {
            Ok(()) => tracing::info!("{} (\"{name}\") is no longer a cloud folder", path.display()),
            Err(e) => tracing::warn!("cannot remove {} from File Explorer: {e}", path.display()),
        }
    }
}

/// Uninstall: every connection's folder stops being a cloud folder. Run with the agent stopped.
pub fn unregister_all() -> Result<()> {
    for (name, id, path) in our_sync_roots()? {
        if is_sync_root(&path) {
            release(&path);
        }
        id.unregister()?;
        tracing::info!("removed {} (\"{name}\") from File Explorer", path.display());
    }
    Ok(())
}

/// Turns the placeholders under `dir` back into ordinary files and folders.
/// Placeholders without local data would be unreadable without CloudDrop, so they
/// are removed; those files stay in Drive.
pub fn release(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(md) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if md.is_dir() {
            release(&path);
        } else if WinStore.is_dehydrated(&path) {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("cannot remove {}: {e}", path.display());
            }
            continue;
        }
        if let Err(e) = clouddrop::winstore::make_plain(&path) {
            tracing::warn!("cannot make {} an ordinary file: {e}", path.display());
        }
    }
}

impl Filter {
    /// Entry path for a full path from a callback (drive-letter case may differ).
    fn rel(&self, path: &Path) -> Option<String> {
        clouddrop::util::rel_path(&self.root, path).or_else(|| {
            let p = path.to_string_lossy().to_lowercase();
            let r = self.root.to_string_lossy().to_lowercase();
            p.strip_prefix(&r)
                .map(|rest| rest.trim_start_matches('\\').replace('\\', "/"))
        })
    }

    /// The file's content is in the cloud, identical to what was uploaded: the provider's
    /// hash matches, or, where it reports none, a download of the cloud copy does.
    fn verified_in_drive(&self, path: &Path) -> bool {
        let Some(rel) = self.rel(path) else {
            return false;
        };
        let Ok(db) = Db::open(&self.db_path) else {
            return false;
        };
        let Ok(Some(e)) = db.get(&rel) else {
            return false;
        };
        let caps = self.remote.caps();
        let (Some(id), Some(hash), Some(size)) = (e.drive_id.clone(), e.content_hash(), e.cloud_size) else {
            return false;
        };
        if e.state != State::Synced || hash.kind != local_kind(caps) {
            return false;
        }
        let tmp_dir = self.data_dir.clone();
        let remote: &Remote = &self.remote;
        let check = async {
            match remote.get(&id).await {
                Ok(m) if !m.trashed => proven(remote, caps, &m, &hash, size as u64, &tmp_dir).await,
                Ok(_) => Ok(false),
                Err(err) => Err(err),
            }
        };
        match self.rt.block_on(check) {
            Ok(same) => same,
            Err(err) => {
                tracing::warn!("cannot check {rel} in the cloud: {err}");
                false
            }
        }
    }

    /// The cloud id the database has for `path`. Where ids are paths, renaming a folder leaves
    /// the ids kept in the placeholders beneath it out of date; the database has the new ones.
    fn current_id(&self, path: &Path) -> Option<String> {
        if self.remote.caps().stable_ids {
            return None;
        }
        let rel = self.rel(path)?;
        Db::open(&self.db_path).ok()?.get(&rel).ok()??.drive_id
    }
}

impl SyncFilter for Filter {
    /// A placeholder was opened: download the requested range from Drive.
    fn fetch_data(
        &self,
        request: Request,
        ticket: ticket::FetchData,
        info: info::FetchData,
    ) -> CResult<()> {
        let blob = std::str::from_utf8(request.file_blob())
            .map_err(|_| CloudErrorKind::InvalidRequest)?
            .to_string();
        let id = self.current_id(&request.path()).unwrap_or(blob);
        if id.is_empty() {
            return Err(CloudErrorKind::InvalidRequest);
        }
        let size = request.file_size();
        let range = info.required_file_range();
        let end = range.end.min(size);
        let mut pos = range.start - range.start % PAGE;
        while pos < end {
            let to = (pos + CHUNK).min(size);
            let data = self
                .rt
                .block_on(self.remote.download_range(&id, pos, to))
                .map_err(|e| {
                    tracing::warn!("download of {} failed: {e}", request.path().display());
                    CloudErrorKind::NetworkUnavailable
                })?;
            if data.len() as u64 != to - pos {
                return Err(CloudErrorKind::Unsuccessful);
            }
            ticket
                .write_at(&data, pos)
                .map_err(|_| CloudErrorKind::Unsuccessful)?;
            let _ = ticket.report_progress(size, to);
            pos = to;
        }
        Ok(())
    }

    /// "Free up space" in Explorer: only once the cloud copy is verified.
    fn dehydrate(
        &self,
        request: Request,
        ticket: ticket::Dehydrate,
        _info: info::Dehydrate,
    ) -> CResult<()> {
        let path = request.path();
        if self.verified_in_drive(&path) {
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        } else {
            tracing::warn!("kept {}: its cloud copy couldn't be verified", path.display());
            Err(CloudErrorKind::DehydrationDisallowed)
        }
    }

    /// Deleting locally never deletes in Drive; the engine records it as cloud-only.
    fn delete(&self, _request: Request, ticket: ticket::Delete, _info: info::Delete) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    /// Renames and moves are mirrored to Drive by the engine.
    fn rename(&self, _request: Request, ticket: ticket::Rename, _info: info::Rename) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }
}

/// `clouddrop-agent --selftest DIR`: the File Explorer cloud-folder steps in a
/// temporary sync root, for checking an installed Microsoft Store package (CI runs
/// it inside the package). Writes "ok" or the failure to `DIR.result`.
pub fn selftest(dir: &Path) -> Result<()> {
    struct Fixed(Vec<u8>);
    impl SyncFilter for Fixed {
        fn fetch_data(&self, _r: Request, ticket: ticket::FetchData, info: info::FetchData) -> CResult<()> {
            let range = info.required_file_range();
            let start = (range.start - range.start % PAGE) as usize;
            let end = (range.end as usize).min(self.0.len());
            ticket
                .write_at(&self.0[start..end], start as u64)
                .map_err(|_| CloudErrorKind::Unsuccessful)
        }
        fn dehydrate(&self, _r: Request, ticket: ticket::Dehydrate, _i: info::Dehydrate) -> CResult<()> {
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        }
        fn delete(&self, _r: Request, ticket: ticket::Delete, _i: info::Delete) -> CResult<()> {
            ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
        }
    }

    std::fs::create_dir_all(dir)?;
    let content: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let id = SyncRootIdBuilder::new("CloudDropSelfTest")
        .user_security_id(SecurityId::current_user()?)
        .account_name("selftest")
        .build();
    let exe = std::env::current_exe()?;
    id.register(
        SyncRootInfo::default()
            .with_display_name("CloudDrop self-test")
            .with_icon(format!("{},0", exe.display()))
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_hydration_type(HydrationType::Full)
            .with_population_type(PopulationType::AlwaysFull)
            .with_allow_pinning(true)
            .with_protection_mode(ProtectionMode::Personal)
            .with_path(dir)?,
    )?;
    let result = Session::new()
        .connect(dir, Fixed(content.clone()))
        .map_err(anyhow::Error::from)
        .and_then(|connection| {
            let steps = (|| -> Result<()> {
                let store = WinStore;
                let file = dir.join("a.bin");
                std::fs::write(&file, &content)?;
                store.synced(&file, "self-test-1", false);
                if !clouddrop::winstore::is_placeholder_path(&file)? {
                    bail!("a file didn't become a cloud file");
                }
                let sub = dir.join("Folder");
                std::fs::create_dir_all(&sub)?;
                store.synced(&sub, "self-test-2", true);
                if !clouddrop::winstore::is_placeholder_path(&sub)? {
                    bail!("a folder didn't become a cloud folder");
                }
                store.free(&file, "self-test-1")?;
                if !store.is_dehydrated(&file) {
                    bail!("freeing didn't remove the local data");
                }
                store.hydrate(&file)?;
                if store.is_dehydrated(&file) || std::fs::read(&file)? != content {
                    bail!("downloading back didn't restore the data");
                }
                Ok(())
            })();
            drop(connection);
            release(dir);
            steps
        });
    let _ = id.unregister();
    let report = dir.with_extension("result");
    match &result {
        Ok(()) => std::fs::write(&report, "ok")?,
        Err(e) => std::fs::write(&report, format!("fail: {e:#}"))?,
    }
    result
}
