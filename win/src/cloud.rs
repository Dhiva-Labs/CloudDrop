//! The File Explorer cloud folder (Cloud Files API): registering the sync root,
//! and the callbacks Windows makes when a placeholder is opened, freed, deleted
//! or renamed. Upload-only rules hold here too: deletes and renames are allowed
//! locally and never touch Drive; freeing is allowed only for files whose Drive
//! copy is verified.

use anyhow::{Result, bail};
use clouddrop::config::{self, Config};
use clouddrop::db::{Db, State};
use clouddrop::drive::Drive;
use clouddrop::store::LocalStore;
use clouddrop::winstore::{WinStore, is_sync_root};
use cloud_filter::error::{CResult, CloudErrorKind};
use cloud_filter::filter::{Request, SyncFilter, info, ticket};
use cloud_filter::root::{
    Connection, HydrationType, PopulationType, ProtectionMode, SecurityId, Session, SyncRootId,
    SyncRootIdBuilder, SyncRootInfo, is_supported,
};
use cloud_filter::utility::WriteAt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Download unit when Explorer opens a placeholder; the API wants multiples of 4 KiB.
const CHUNK: u64 = 4 << 20;
const PAGE: u64 = 4096;

pub struct Filter {
    rt: tokio::runtime::Handle,
    drive: Arc<Drive>,
    root: PathBuf,
    db_path: PathBuf,
}

/// Registers the sync folder with File Explorer (once) and connects to it.
pub fn connect(cfg: &Config, drive: Arc<Drive>, rt: tokio::runtime::Handle) -> Result<Connection<Filter>> {
    if !is_supported()? {
        bail!("this version of Windows has no cloud files support");
    }
    let id = sync_root_id()?;
    if !id.is_registered()? {
        let exe = std::env::current_exe()?;
        let info = SyncRootInfo::default()
            .with_display_name("CloudDrop")
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
        drive,
        root: cfg.sync_dir.clone(),
        db_path: config::data_dir().join("state.db"),
    };
    Ok(Session::new().connect(&cfg.sync_dir, filter)?)
}

fn sync_root_id() -> Result<SyncRootId> {
    Ok(SyncRootIdBuilder::new("CloudDrop")
        .user_security_id(SecurityId::current_user()?)
        .account_name("default")
        .build())
}

/// Uninstall: the folder stops being a cloud folder in File Explorer. Run with
/// the agent stopped.
pub fn unregister(sync_dir: Option<&Path>) -> Result<()> {
    if let Some(dir) = sync_dir.filter(|d| is_sync_root(d)) {
        release(dir);
    }
    let id = sync_root_id()?;
    if id.is_registered()? {
        id.unregister()?;
        tracing::info!("removed the CloudDrop folder from File Explorer");
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

    /// The file's content is in Drive, identical to what was uploaded.
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
        let (Some(id), Some(md5)) = (e.drive_id.clone(), e.md5.clone()) else {
            return false;
        };
        if e.state != State::Synced {
            return false;
        }
        match self.rt.block_on(self.drive.get(&id)) {
            Ok(m) => {
                !m.trashed
                    && m.md5_checksum.as_deref() == Some(md5.as_str())
                    && m.size_bytes() == e.cloud_size.map(|s| s as u64)
            }
            Err(err) => {
                tracing::warn!("cannot check {rel} in Drive: {err}");
                false
            }
        }
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
        let id = std::str::from_utf8(request.file_blob())
            .map_err(|_| CloudErrorKind::InvalidRequest)?
            .to_string();
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
                .block_on(self.drive.download_range(&id, pos, to))
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

    /// "Free up space" in Explorer: only once Drive's checksum matches.
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
            tracing::warn!("kept {}: its Drive copy couldn't be verified", path.display());
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
