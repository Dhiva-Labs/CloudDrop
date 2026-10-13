//! The File Explorer cloud folder (Cloud Files API): registering the sync root,
//! and the callbacks Windows makes when a placeholder is opened, freed, deleted
//! or renamed. Upload-only rules hold here too: deletes and renames are allowed
//! locally and never touch Drive; freeing is allowed only for files whose Drive
//! copy is verified.

use anyhow::{Result, bail};
use clouddrop::config::{self, Config};
use clouddrop::db::{Db, State};
use clouddrop::drive::Drive;
use cloud_filter::error::{CResult, CloudErrorKind};
use cloud_filter::filter::{Request, SyncFilter, info, ticket};
use cloud_filter::root::{
    Connection, HydrationType, PopulationType, ProtectionMode, SecurityId, Session,
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
    let id = SyncRootIdBuilder::new("CloudDrop")
        .user_security_id(SecurityId::current_user()?)
        .account_name("default")
        .build();
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
