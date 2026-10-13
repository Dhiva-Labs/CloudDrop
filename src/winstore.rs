//! Windows: the sync folder is a Cloud Files sync root (registered by
//! clouddrop-agent), so uploaded files are placeholders marked in sync and freed
//! files are placeholders without data. See docs/WINDOWS.md.

use crate::store::LocalStore;
use cloud_filter::placeholder::{ConvertOptions, Placeholder, UpdateOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;

const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
const FILE_ATTRIBUTE_PINNED: u32 = 0x0008_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

pub struct WinStore;

impl WinStore {
    /// Available once the sync folder is a registered sync root.
    pub fn new() -> Option<Self> {
        let cfg = crate::config::Config::load().ok()?;
        is_sync_root(&cfg.sync_dir).then_some(WinStore)
    }
}

/// `path` is inside a registered Cloud Files sync root.
pub fn is_sync_root(path: &Path) -> bool {
    use windows_sys::Win32::Storage::CloudFilters::{CF_SYNC_ROOT_INFO_BASIC, CfGetSyncRootInfoByPath};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = [0u8; 64];
    let mut len = 0u32;
    let hr = unsafe {
        CfGetSyncRootInfoByPath(
            wide.as_ptr(),
            CF_SYNC_ROOT_INFO_BASIC,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut len,
        )
    };
    hr >= 0
}

fn attrs(abs: &Path) -> u32 {
    std::fs::symlink_metadata(abs)
        .map(|m| m.file_attributes())
        .unwrap_or(0)
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl LocalStore for WinStore {
    fn is_dehydrated(&self, abs: &Path) -> bool {
        attrs(abs)
            & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_OFFLINE)
            != 0
    }

    fn is_pinned(&self, abs: &Path) -> bool {
        attrs(abs) & FILE_ATTRIBUTE_PINNED != 0
    }

    fn free(&self, abs: &Path, drive_id: &str) -> io::Result<()> {
        let mut ph = Placeholder::options()
            .write_access()
            .exclusive()
            .open(abs)
            .map_err(io_err)?;
        if ph.info().map_err(io_err)?.is_none() {
            ph.convert_to_placeholder(
                ConvertOptions::default().mark_in_sync().blob(drive_id.as_bytes().to_vec()),
                None,
            )
            .map_err(io_err)?;
        }
        ph.update(UpdateOptions::default().dehydrate(), None)
            .map_err(io_err)?;
        Ok(())
    }

    fn synced(&self, abs: &Path, drive_id: &str, is_dir: bool) {
        let result = (|| -> Result<(), String> {
            let mut ph = Placeholder::options()
                .write_access()
                .open(abs)
                .map_err(|e| e.to_string())?;
            if ph.info().map_err(|e| e.to_string())?.is_some() {
                ph.mark_in_sync(true, None).map_err(|e| e.to_string())?;
            } else {
                let mut opts = ConvertOptions::default()
                    .mark_in_sync()
                    .blob(drive_id.as_bytes().to_vec());
                if is_dir {
                    opts = opts.has_children();
                }
                ph.convert_to_placeholder(opts, None)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            tracing::debug!("cannot mark {} in sync: {e}", abs.display());
        }
    }

    fn pending(&self, abs: &Path) {
        if let Ok(mut ph) = Placeholder::options().write_access().open(abs)
            && let Ok(Some(_)) = ph.info()
        {
            let _ = ph.mark_in_sync(false, None);
        }
    }
}
