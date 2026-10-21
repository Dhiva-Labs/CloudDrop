pub mod actions;
pub mod auth;
pub mod config;
pub mod connections;
pub mod control;
pub mod credentials;
pub mod daemon;
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
pub mod proton;
pub mod remote;
pub mod s3;
pub mod sftp;
pub mod store;
#[cfg(windows)]
pub mod winstore;
pub mod util;
pub mod webdav;

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The configured provider's client, wired to its endpoints and the given token file (in the
/// data directory of the connection [`connections::current`]).
pub fn remote_for(cfg: &config::Config, token_file: PathBuf) -> Arc<remote::Remote> {
    let data_dir = token_file.parent().map(PathBuf::from).unwrap_or_default();
    remote_in(cfg, &data_dir, &config::config_path())
}

/// A connection's client: its sign-in and secrets are in `data_dir`, and `config_file` is where
/// the client writes back what it learns (SFTP's host key).
pub fn remote_in(cfg: &config::Config, data_dir: &Path, config_file: &Path) -> Arc<remote::Remote> {
    let token_file = data_dir.join("token.json");
    let credentials = data_dir.join("credentials.json");
    match cfg.provider {
        remote::Provider::GoogleDrive => {
            let http = drive::http_client();
            let auth = Arc::new(auth::Auth::new(http.clone(), cfg, token_file));
            Arc::new(remote::Remote::GoogleDrive(drive::Drive::new(http, cfg.endpoints.clone(), auth)))
        }
        remote::Provider::OneDrive => {
            let http = drive::http_client();
            let tokens = oauth_tokens(cfg, &token_file, &http).unwrap_or_else(|| Arc::new(onedrive::NotSignedIn));
            Arc::new(remote::Remote::OneDrive(onedrive::OneDrive::new(http, onedrive::GRAPH_BASE, tokens)))
        }
        remote::Provider::Dropbox => {
            let http = drive::http_client();
            let tokens = oauth_tokens(cfg, &token_file, &http).unwrap_or_else(|| Arc::new(dropbox::NotSignedIn));
            let (api, content) = (&cfg.dropbox.api_base, &cfg.dropbox.content_base);
            Arc::new(remote::Remote::Dropbox(dropbox::Dropbox::new(http, api, content, tokens)))
        }
        // The app password is in credentials.json, next to the token file in the data directory.
        remote::Provider::WebDav | remote::Provider::Nextcloud => {
            Arc::new(remote::Remote::WebDav(Box::new(webdav::WebDav::from_config(cfg, &credentials))))
        }
        remote::Provider::S3 => Arc::new(remote::Remote::S3(Box::new(s3::S3::with_saved_secret(
            drive::http_client(),
            &cfg.s3,
            &credentials,
        )))),
        remote::Provider::Sftp => {
            let secrets = credentials::load_from(&credentials, "sftp").unwrap_or_else(|e| {
                tracing::warn!("cannot read the SFTP password: {e:#}");
                None
            });
            let secrets = sftp::Secrets::from_map(&secrets.unwrap_or_default());
            let client = sftp::Sftp::new(cfg.sftp.clone(), secrets, Some(config_file.to_path_buf()));
            Arc::new(remote::Remote::Sftp(client))
        }
        // The session lives in credentials.json in the data directory.
        remote::Provider::Mega => {
            Arc::new(remote::Remote::Mega(mega::Mega::new(drive::http_client(), &cfg.mega, data_dir)))
        }
        // Proton's CLI keeps the sign-in; uploads are staged in the data directory.
        remote::Provider::Proton => Arc::new(remote::Remote::Proton(proton::Proton::new(&cfg.proton, data_dir))),
    }
}

/// The browser sign-in of an OAuth provider: `token-<id>.json` beside `token_file`, in the data
/// directory. None when this build has no client id for it (every call then asks to sign in).
fn oauth_tokens(
    cfg: &config::Config,
    token_file: &std::path::Path,
    http: &reqwest::Client,
) -> Option<Arc<dyn remote::TokenSource>> {
    let client = oauth::client_for(cfg, cfg.provider)?;
    let path = token_file.with_file_name(format!("token-{}.json", cfg.provider.as_str()));
    Some(Arc::new(oauth::TokenFile::new(http.clone(), &client, path)))
}
