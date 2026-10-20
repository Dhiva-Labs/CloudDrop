pub mod actions;
pub mod auth;
pub mod config;
pub mod control;
pub mod credentials;
pub mod db;
pub mod drive;
pub mod dropbox;
pub mod engine;
pub mod hash;
pub mod jobs;
pub mod mega;
pub mod notifier;
pub mod oauth;
pub mod onedrive;
pub mod remote;
pub mod s3;
pub mod sftp;
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
        // Until the generic OAuth sign-in is wired up, every call asks for sign-in.
        remote::Provider::OneDrive => Arc::new(remote::Remote::OneDrive(onedrive::OneDrive::new(
            drive::http_client(),
            onedrive::GRAPH_BASE,
            Arc::new(onedrive::NotSignedIn),
        ))),
        remote::Provider::Dropbox => {
            // Until the browser sign-in is wired up: every call asks for a sign-in.
            let tokens = Arc::new(dropbox::NotSignedIn);
            let (api, content) = (&cfg.dropbox.api_base, &cfg.dropbox.content_base);
            Arc::new(remote::Remote::Dropbox(dropbox::Dropbox::new(drive::http_client(), api, content, tokens)))
        }
        // The app password is in credentials.json, next to the token file in the data directory.
        remote::Provider::WebDav | remote::Provider::Nextcloud => Arc::new(remote::Remote::WebDav(
            webdav::WebDav::from_config(cfg, &token_file.with_file_name("credentials.json")),
        )),
        remote::Provider::S3 => {
            Arc::new(remote::Remote::S3(Box::new(s3::S3::with_saved_secret(drive::http_client(), &cfg.s3))))
        }
        remote::Provider::Sftp => {
            let secrets = credentials::load("sftp").unwrap_or_else(|e| {
                tracing::warn!("cannot read the SFTP password: {e:#}");
                None
            });
            let secrets = sftp::Secrets::from_map(&secrets.unwrap_or_default());
            let client = sftp::Sftp::new(cfg.sftp.clone(), secrets, Some(config::config_path()));
            Arc::new(remote::Remote::Sftp(client))
        }
        // The session lives in credentials.json next to the token file (the data directory).
        remote::Provider::Mega => {
            let data_dir = token_file.parent().map(PathBuf::from).unwrap_or_default();
            Arc::new(remote::Remote::Mega(mega::Mega::new(drive::http_client(), &cfg.mega, &data_dir)))
        }
    }
}
