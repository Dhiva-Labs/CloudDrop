//! The seam between the engine and a cloud provider. `Remote` has one variant per
//! provider (only Google Drive so far; docs/PROVIDERS.md has the plan), so every
//! call site sees every provider and no trait objects are needed.

pub mod gdrive;

use crate::config::Config;
use crate::control::Quota;
use crate::db::Kind;
use crate::hash::{ContentHash, HashKind};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Which cloud CloudDrop uploads to (`provider` in the config).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    #[serde(rename = "gdrive")]
    GoogleDrive,
}

impl Provider {
    /// The name used in the config, status.json and the database.
    pub fn id(self) -> &'static str {
        match self {
            Provider::GoogleDrive => "gdrive",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Provider::GoogleDrive => "Google Drive",
        }
    }

    pub fn capabilities(self) -> Capabilities {
        match self {
            Provider::GoogleDrive => Capabilities {
                hash: HashKind::Md5,
                resumable: true,
                keep_version: KeepVersion::Pin,
                real_folders: true,
                names: NameRules { forbidden: &[], case_insensitive: false, max_len: None },
            },
        }
    }

    /// The item's page in the provider's web app; None where there is none.
    pub fn web_url(self, kind: Kind, id: &str) -> Option<String> {
        match self {
            Provider::GoogleDrive => Some(match kind {
                Kind::Dir => format!("https://drive.google.com/drive/folders/{id}"),
                Kind::File => format!("https://drive.google.com/file/d/{id}/view"),
            }),
        }
    }
}

/// What a provider offers, so the engine can adapt to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// The hash the provider reports for stored content.
    pub hash: HashKind,
    /// An interrupted upload continues where it stopped.
    pub resumable: bool,
    pub keep_version: KeepVersion,
    /// Folders are items with ids of their own, not just prefixes of names.
    pub real_folders: bool,
    pub names: NameRules,
}

/// How replaced content is kept forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepVersion {
    /// The old revision is pinned in place (Drive `keepForever`).
    Pin,
    /// A server-side copy of the old content is made next to the file.
    Copy,
}

/// What a provider accepts as a file or folder name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRules {
    pub forbidden: &'static [char],
    /// "A.txt" and "a.txt" name the same item.
    pub case_insensitive: bool,
    /// Longest name in bytes; None = no practical limit.
    pub max_len: Option<usize>,
}

// The wording names Google Drive, the only provider so far: the apps show these texts.
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum RemoteError {
    /// Sign-in is missing, expired or revoked; nothing will work until `clouddrop login`.
    #[error("Google sign-in needed: {0}")]
    Auth(String),
    #[error("not found in Google Drive")]
    NotFound,
    /// The account's storage is full.
    #[error("{0}")]
    Quota(String),
    /// Too many requests; retried with the same backoff as `Transient`.
    #[error("{message}")]
    RateLimited { message: String, retry_after: Option<Duration> },
    /// Worth retrying later (network, server error).
    #[error("{0}")]
    Transient(String),
    #[error("{0}")]
    Permanent(String),
}

impl RemoteError {
    /// Worth retrying later, with backoff.
    pub fn is_transient(&self) -> bool {
        matches!(self, RemoteError::Transient(_) | RemoteError::RateLimited { .. })
    }
}

pub type RResult<T> = Result<T, RemoteError>;

/// What the provider reports about one stored item.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteMeta {
    pub id: String,
    pub name: String,
    pub folder: bool,
    pub parents: Vec<String>,
    /// Files: the provider's hash of the stored content.
    pub hash: Option<ContentHash>,
    pub size: Option<u64>,
    /// In the provider's trash: not a usable copy.
    pub trashed: bool,
}

#[derive(Debug, PartialEq)]
pub enum UploadStatus {
    Done(RemoteMeta),
    /// Upload continues from this byte offset.
    Next(u64),
}

/// The signed-in account's storage, and who it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Storage {
    pub quota: Quota,
    /// Email address, when the provider reports it.
    pub account: Option<String>,
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        // Resumable uploads answer 308 without Location; never treat it as a redirect.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(120))
        .user_agent(concat!("clouddrop/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

/// A signed-in client of one provider. Ids are the provider's own (kept in `drive_id`).
pub enum Remote {
    GoogleDrive(gdrive::Drive),
}

impl Remote {
    /// The configured provider, signed in with the token in `token_file`.
    pub fn new(cfg: &Config, token_file: PathBuf) -> Self {
        match cfg.provider {
            Provider::GoogleDrive => {
                let http = http_client();
                let auth = Arc::new(crate::auth::Auth::new(http.clone(), cfg, token_file));
                Remote::GoogleDrive(gdrive::Drive::new(http, cfg.endpoints.clone(), auth))
            }
        }
    }

    pub fn provider(&self) -> Provider {
        match self {
            Remote::GoogleDrive(_) => Provider::GoogleDrive,
        }
    }

    pub fn capabilities(&self) -> Capabilities {
        self.provider().capabilities()
    }

    pub fn web_url(&self, kind: Kind, id: &str) -> Option<String> {
        self.provider().web_url(kind, id)
    }

    /// Finds or creates the top folder `name`; returns its id.
    pub async fn ensure_root(&self, name: &str) -> RResult<String> {
        match self {
            Remote::GoogleDrive(d) => d.ensure_root(name).await,
        }
    }

    /// Usable (not trashed) items named `name` directly inside `parent`.
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RResult<Vec<RemoteMeta>> {
        match self {
            Remote::GoogleDrive(d) => d.find_child(name, parent, folder).await,
        }
    }

    pub async fn create_folder(&self, name: &str, parent: &str) -> RResult<RemoteMeta> {
        match self {
            Remote::GoogleDrive(d) => d.create_folder(name, parent).await,
        }
    }

    pub async fn get(&self, id: &str) -> RResult<RemoteMeta> {
        match self {
            Remote::GoogleDrive(d) => d.get(id).await,
        }
    }

    /// Renames and/or moves an item. `parents` = (new, old) when moving.
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RResult<RemoteMeta> {
        match self {
            Remote::GoogleDrive(d) => d.move_rename(id, name, parents).await,
        }
    }

    /// Makes sure the file's current content is never lost when it is replaced.
    pub async fn keep_version(&self, id: &str) -> RResult<()> {
        match self {
            Remote::GoogleDrive(d) => d.keep_version(id).await,
        }
    }

    /// Opens an upload session; returns it as an opaque string (kept in `upload_url`).
    /// `existing` = upload new content for that file instead of creating one;
    /// `parents` = (parent, its previous parent if known).
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        mime: &str,
    ) -> RResult<String> {
        match self {
            Remote::GoogleDrive(d) => d.start_upload(existing, name, parents, size, mime).await,
        }
    }

    /// Asks a session how much it has received. NotFound = the session expired.
    pub async fn query_upload(&self, session: &str, total: u64) -> RResult<UploadStatus> {
        match self {
            Remote::GoogleDrive(d) => d.query_upload(session, total).await,
        }
    }

    pub async fn put_chunk(
        &self,
        session: &str,
        data: bytes::Bytes,
        start: u64,
        total: u64,
    ) -> RResult<UploadStatus> {
        match self {
            Remote::GoogleDrive(d) => d.put_chunk(session, data, start, total).await,
        }
    }

    /// Bytes `start..end` of a file's content (for downloading placeholders on demand).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RResult<bytes::Bytes> {
        match self {
            Remote::GoogleDrive(d) => d.download_range(id, start, end).await,
        }
    }

    /// Streams a file's content into `out`; returns (its hash, bytes written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RResult<(ContentHash, u64)> {
        match self {
            Remote::GoogleDrive(d) => d.download(id, out).await,
        }
    }

    pub async fn quota(&self) -> RResult<Storage> {
        match self {
            Remote::GoogleDrive(d) => d.quota().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let p = Provider::GoogleDrive;
        assert_eq!(p.web_url(Kind::Dir, "X").unwrap(), "https://drive.google.com/drive/folders/X");
        assert_eq!(p.web_url(Kind::File, "Y").unwrap(), "https://drive.google.com/file/d/Y/view");
    }

    #[test]
    fn provider_names_in_config() {
        #[derive(Deserialize)]
        struct C {
            provider: Provider,
        }
        let c: C = toml::from_str("provider = \"gdrive\"").unwrap();
        assert_eq!(c.provider, Provider::GoogleDrive);
        assert_eq!(c.provider.id(), "gdrive");
        assert!(toml::from_str::<C>("provider = \"elsewhere\"").is_err());
    }
}
