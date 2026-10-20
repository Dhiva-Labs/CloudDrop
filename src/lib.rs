pub mod actions;
pub mod auth;
pub mod config;
pub mod control;
pub mod db;
pub mod engine;
pub mod hash;
pub mod jobs;
pub mod notifier;
pub mod remote;
pub mod store;
#[cfg(windows)]
pub mod winstore;
pub mod util;

use std::path::PathBuf;
use std::sync::Arc;

/// Client of the configured provider, signed in with the given token file.
pub fn remote_for(cfg: &config::Config, token_file: PathBuf) -> Arc<remote::Remote> {
    Arc::new(remote::Remote::new(cfg, token_file))
}
