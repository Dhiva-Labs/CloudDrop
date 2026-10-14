//! Windows: the sync folder is a Cloud Files sync root (registered by
//! clouddrop-agent), so uploaded files are placeholders marked in sync and freed
//! files are placeholders without data. See docs/WINDOWS.md.

use crate::store::LocalStore;
use cloud_filter::placeholder::{ConvertOptions, PinOptions, PinState, Placeholder, UpdateOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;

const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
const FILE_ATTRIBUTE_PINNED: u32 = 0x0008_0000;
const FILE_ATTRIBUTE_UNPINNED: u32 = 0x0010_0000;
/// HRESULT_FROM_WIN32(ERROR_CLOUD_FILE_ACCESS_DENIED): not the sync root's owner.
const CLOUD_ACCESS_DENIED: i32 = 0x8007_018B_u32 as i32;
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

/// Opens an item for Cloud Files calls with a plain Win32 handle (they reject the
/// protected handle from cloud-filter's `Placeholder::options()` as invalid).
/// Attribute and permission access only, so opening never downloads a placeholder.
/// `exclusive` fails while another program has the file open.
fn open(abs: &Path, exclusive: bool) -> io::Result<Placeholder> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const WRITE_DAC: u32 = 0x0004_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000; // needed to open folders
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | WRITE_DAC)
        .share_mode(if exclusive { 0 } else { 7 })
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(abs)?;
    Ok(Placeholder::from(file))
}

/// Whether the open item is a placeholder. (cloud-filter's `Placeholder::info`
/// panics on placeholders that carry fewer than 4 bytes of data.)
fn is_placeholder(ph: &Placeholder) -> io::Result<bool> {
    use windows_sys::Win32::Storage::CloudFilters::{CF_PLACEHOLDER_INFO_BASIC, CfGetPlaceholderInfo};
    const NOT_A_CLOUD_FILE: i32 = 0x8007_0178_u32 as i32;
    let mut buf = vec![0u8; 8192];
    let mut len = 0u32;
    let hr = unsafe {
        CfGetPlaceholderInfo(
            ph.inner_handle().handle().0,
            CF_PLACEHOLDER_INFO_BASIC,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut len,
        )
    };
    match hr {
        hr if hr >= 0 => Ok(true),
        NOT_A_CLOUD_FILE => Ok(false),
        hr => Err(hr_err(hr)),
    }
}

fn hr_err(hr: i32) -> io::Error {
    io::Error::other(format!("Cloud Files error {:#010x}", hr as u32))
}

/// `abs` is a Cloud Files placeholder.
pub fn is_placeholder_path(abs: &Path) -> io::Result<bool> {
    is_placeholder(&open(abs, false)?)
}

/// Turns a placeholder back into an ordinary file or folder (uninstall). Only for
/// items whose data is local.
pub fn make_plain(abs: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::CloudFilters::{CF_REVERT_FLAG_NONE, CfRevertPlaceholder};
    let ph = open(abs, false)?;
    if !is_placeholder(&ph)? {
        return Ok(());
    }
    let hr = unsafe {
        CfRevertPlaceholder(ph.inner_handle().handle().0, CF_REVERT_FLAG_NONE, std::ptr::null_mut())
    };
    if hr < 0 {
        return Err(hr_err(hr));
    }
    Ok(())
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
        let mut ph = open(abs, true)?;
        if !is_placeholder(&ph)? {
            ph.convert_to_placeholder(
                ConvertOptions::default().mark_in_sync().blob(drive_id.as_bytes().to_vec()),
                None,
            )
            .map_err(io_err)?;
        }
        ph.update(UpdateOptions::default().dehydrate(), None)
            .map_err(|e| {
                if e.code().0 == CLOUD_ACCESS_DENIED {
                    io::Error::new(io::ErrorKind::PermissionDenied, e.to_string())
                } else {
                    io_err(e)
                }
            })?;
        // Done: opening it later should keep the downloaded copy, not free it again.
        let _ = ph.mark_pin(PinState::Unspecified, PinOptions::default());
        Ok(())
    }

    fn free_requested(&self, abs: &Path) -> bool {
        attrs(abs) & FILE_ATTRIBUTE_UNPINNED != 0 && !self.is_dehydrated(abs)
    }

    fn request_free(&self, abs: &Path, drive_id: &str) -> io::Result<bool> {
        match self.free(abs, drive_id) {
            Ok(()) => Ok(true),
            // Same as Explorer's "Free up space": the agent sees it and frees the file.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                let mut ph = open(abs, false)?;
                ph.mark_pin(PinState::Unpinned, PinOptions::default())
                    .map_err(io_err)?;
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn hydrate(&self, abs: &Path) -> io::Result<()> {
        // Reading is what downloads a placeholder, from any process.
        let mut f = std::fs::File::open(abs)?;
        std::io::copy(&mut f, &mut std::io::sink())?;
        Ok(())
    }

    fn synced(&self, abs: &Path, drive_id: &str, _is_dir: bool) {
        let result = (|| -> Result<(), String> {
            let mut ph = open(abs, false).map_err(|e| e.to_string())?;
            if is_placeholder(&ph).map_err(|e| e.to_string())? {
                ph.mark_in_sync(true, None).map_err(|e| e.to_string())?;
            } else {
                // Folders too: no on-demand population flag, which Windows rejects in
                // a sync root whose folders are always complete (PopulationType::AlwaysFull).
                let opts = ConvertOptions::default()
                    .mark_in_sync()
                    .blob(drive_id.as_bytes().to_vec());
                ph.convert_to_placeholder(opts, None)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("cannot mark {} in sync: {e}", abs.display());
        }
    }

    fn pending(&self, abs: &Path) {
        if let Ok(mut ph) = open(abs, false)
            && let Ok(true) = is_placeholder(&ph)
        {
            let _ = ph.mark_in_sync(false, None);
        }
    }
}
