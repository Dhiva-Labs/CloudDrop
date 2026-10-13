pub mod actions;
pub mod auth;
pub mod config;
pub mod control;
pub mod db;
pub mod drive;
pub mod engine;
pub mod jobs;
pub mod notifier;
pub mod store;
#[cfg(windows)]
pub mod winstore;
pub mod util;

use std::path::PathBuf;
use std::sync::Arc;

/// Drive client wired to the configured endpoints and the given token file.
pub fn drive_for(cfg: &config::Config, token_file: PathBuf) -> Arc<drive::Drive> {
    let http = drive::http_client();
    let auth = Arc::new(auth::Auth::new(http.clone(), cfg, token_file));
    Arc::new(drive::Drive::new(http, cfg.endpoints.clone(), auth))
}
