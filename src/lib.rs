pub mod actions;
pub mod auth;
pub mod config;
pub mod control;
pub mod credentials;
pub mod db;
pub mod drive;
pub mod engine;
pub mod hash;
pub mod jobs;
pub mod notifier;
pub mod remote;
pub mod store;
#[cfg(windows)]
pub mod winstore;
pub mod util;
pub mod webdav;

use std::path::PathBuf;
use std::sync::Arc;

/// The configured provider's client, wired to its endpoints and the given token file.
pub fn remote_for(cfg: &config::Config, token_file: PathBuf) -> Arc<remote::Remote> {
    match cfg.provider {
        remote::Provider::GoogleDrive => {
            let http = drive::http_client();
            let auth = Arc::new(auth::Auth::new(http.clone(), cfg, token_file));
            Arc::new(remote::Remote::GoogleDrive(drive::Drive::new(http, cfg.endpoints.clone(), auth)))
        }
        // The app password is in credentials.json, next to the token file in the data directory.
        remote::Provider::WebDav | remote::Provider::Nextcloud => Arc::new(remote::Remote::WebDav(
            webdav::WebDav::from_config(cfg, &token_file.with_file_name("credentials.json")),
        )),
    }
}
