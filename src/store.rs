//! How local copies are represented. On Linux a freed file is simply gone; on
//! Windows the sync folder is a Cloud Files sync root, so a freed file stays in
//! Explorer as a placeholder without data, and uploaded files are placeholders
//! marked "in sync" (Explorer's green check).

use std::io;
use std::path::Path;
use std::sync::Arc;

pub trait LocalStore: Send + Sync {
    /// The item is a placeholder without local data: reading it would download it,
    /// so the engine must never hash it. Always false on Linux.
    fn is_dehydrated(&self, _abs: &Path) -> bool {
        false
    }

    /// Marked "always keep on this device" in the file manager (in addition to the
    /// config's `keep_local` folders).
    fn is_pinned(&self, _abs: &Path) -> bool {
        false
    }

    /// Removes the local data of a file already verified against Drive (`drive_id`).
    fn free(&self, abs: &Path, _drive_id: &str) -> io::Result<()> {
        std::fs::remove_file(abs)
    }

    /// The local file at `abs` now matches the Drive file `drive_id`.
    fn synced(&self, _abs: &Path, _drive_id: &str, _is_dir: bool) {}

    /// The local file at `abs` has changes that are not in Drive yet.
    fn pending(&self, _abs: &Path) {}

    /// "Free up space" was chosen in the file manager for this file, which still
    /// has its data. The engine frees it after the usual Drive check.
    fn free_requested(&self, _abs: &Path) -> bool {
        false
    }

    /// Frees `abs` if this process may, or asks the engine to (`Ok(false)`): on
    /// Windows only the agent that owns the sync root can remove a file's data.
    fn request_free(&self, abs: &Path, drive_id: &str) -> io::Result<bool> {
        self.free(abs, drive_id).map(|()| true)
    }

    /// Brings back the data of a placeholder (Windows downloads it on access).
    fn hydrate(&self, _abs: &Path) -> io::Result<()> {
        Ok(())
    }
}

/// Plain files: Linux, and Windows when the sync root isn't available.
pub struct PlainStore;

impl LocalStore for PlainStore {}

/// The store for this platform, for the folder of the connection being worked on.
pub fn platform() -> Arc<dyn LocalStore> {
    match crate::config::Config::load() {
        Ok(cfg) => for_folder(&cfg.sync_dir),
        Err(_) => Arc::new(PlainStore),
    }
}

/// The store for a sync folder: placeholders where it is a File Explorer cloud folder (Windows),
/// plain files otherwise.
pub fn for_folder(_dir: &std::path::Path) -> Arc<dyn LocalStore> {
    #[cfg(windows)]
    {
        if crate::winstore::is_sync_root(_dir) {
            return Arc::new(crate::winstore::WinStore);
        }
    }
    Arc::new(PlainStore)
}
