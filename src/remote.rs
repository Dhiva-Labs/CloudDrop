//! The cloud CloudDrop uploads to, behind one interface (see docs/PROVIDERS.md).
//!
//! Google Drive is the only provider today. Each new one becomes a [`Provider`] and a
//! [`Remote`] variant; the engine only talks to [`Remote`] and the neutral types here.

use crate::db::Kind;
use crate::drive::Drive;
use serde::{Deserialize, Serialize};

/// Which cloud a CloudDrop folder uploads to (`provider` in config.toml, `meta.provider`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    #[serde(rename = "gdrive")]
    GoogleDrive,
}

impl Provider {
    /// The name in config.toml, the database and `status --json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::GoogleDrive => "gdrive",
        }
    }

    /// The name people know it by.
    pub fn display_name(self) -> &'static str {
        match self {
            Provider::GoogleDrive => "Google Drive",
        }
    }

    /// The hash the provider reports for file content (what "identical" is checked with).
    pub fn hash_kind(self) -> HashKind {
        match self {
            Provider::GoogleDrive => HashKind::Md5,
        }
    }

    /// Web page of a file or folder, for "Open in Google Drive".
    pub fn web_url(self, kind: Kind, id: &str) -> String {
        match (self, kind) {
            (Provider::GoogleDrive, Kind::Dir) => format!("https://drive.google.com/drive/folders/{id}"),
            (Provider::GoogleDrive, Kind::File) => format!("https://drive.google.com/file/d/{id}/view"),
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Provider::default()
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum RemoteError {
    /// Sign-in is missing, expired or revoked; nothing will work until `clouddrop login`.
    #[error("Google sign-in needed: {0}")]
    Auth(String),
    /// Worth retrying later (network, rate limit, server error).
    #[error("{0}")]
    Transient(String),
    #[error("not found in Google Drive")]
    NotFound,
    #[error("{0}")]
    Permanent(String),
}

pub type RemoteResult<T> = Result<T, RemoteError>;

/// How a provider hashes file content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashKind {
    /// MD5 of the whole content, lowercase hex (Google Drive's `md5Checksum`).
    Md5,
}

impl HashKind {
    /// The value of `entries.hash_kind`; NULL means MD5, so older databases read as they are.
    pub fn db_value(self) -> Option<&'static str> {
        match self {
            HashKind::Md5 => None,
        }
    }
}

/// A content hash as a provider reports it, or as computed locally for that provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHash {
    pub kind: HashKind,
    /// Lowercase hex.
    pub value: String,
}

impl ContentHash {
    pub fn md5(hex: impl Into<String>) -> Self {
        ContentHash { kind: HashKind::Md5, value: hex.into() }
    }

    /// Whether this hash is `value` of the same kind (`entries.md5` holds MD5s today).
    pub fn is_md5(&self, value: &str) -> bool {
        self.kind == HashKind::Md5 && self.value == value
    }
}

/// A file or folder in the cloud.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteMeta {
    pub id: String,
    pub name: String,
    pub parents: Vec<String>,
    pub folder: bool,
    /// Content hash, for files whose provider reports one.
    pub hash: Option<ContentHash>,
    pub size: Option<u64>,
    /// In the provider's trash (not deleted, but not where CloudDrop put it either).
    pub trashed: bool,
}

impl RemoteMeta {
    /// The cloud copy has this content (same MD5 and size); whether it's trashed is separate.
    pub fn holds(&self, md5: &str, size: u64) -> bool {
        self.hash.as_ref().is_some_and(|h| h.is_md5(md5)) && self.size == Some(size)
    }

    /// The MD5 the provider reports, if any.
    pub fn md5(&self) -> Option<&str> {
        self.hash.as_ref().filter(|h| h.kind == HashKind::Md5).map(|h| h.value.as_str())
    }
}

#[derive(Debug, PartialEq)]
pub enum UploadStatus {
    Done(RemoteMeta),
    /// Upload continues from this byte offset.
    Next(u64),
}

/// Storage of the signed-in account: (limit, usage, usage by files in the cloud drive).
/// No limit means unlimited storage.
pub type StorageQuota = (Option<u64>, u64, u64);

/// The client for the configured provider.
pub enum Remote {
    GoogleDrive(Drive),
}

impl Remote {
    pub fn provider(&self) -> Provider {
        match self {
            Remote::GoogleDrive(_) => Provider::GoogleDrive,
        }
    }

    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        match self {
            Remote::GoogleDrive(d) => d.storage_quota().await,
        }
    }

    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        match self {
            Remote::GoogleDrive(d) => d.get(id).await,
        }
    }

    /// Items named `name` directly inside `parent` that CloudDrop can see (not trashed).
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        match self {
            Remote::GoogleDrive(d) => d.find_child(name, parent, folder).await,
        }
    }

    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        match self {
            Remote::GoogleDrive(d) => d.create_folder(name, parent).await,
        }
    }

    /// Renames and/or moves an item. `parents` = (new, old) when moving.
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        match self {
            Remote::GoogleDrive(d) => d.update_meta(id, name, parents).await,
        }
    }

    /// Keeps the current content forever, so replacing it never loses it.
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        match self {
            Remote::GoogleDrive(d) => d.pin_head_revision(id).await,
        }
    }

    /// Opens a resumable upload session; returns the session (kept in `entries.upload_url`).
    /// `existing` = upload new content for that file instead of creating one.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        mime: &str,
    ) -> RemoteResult<String> {
        match self {
            Remote::GoogleDrive(d) => d.start_upload(existing, name, parents, size, mime).await,
        }
    }

    /// Asks a session how much it has received. NotFound = the session expired.
    pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus> {
        match self {
            Remote::GoogleDrive(d) => d.query_upload(session, total).await,
        }
    }

    pub async fn put_chunk(&self, session: &str, data: bytes::Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus> {
        match self {
            Remote::GoogleDrive(d) => d.put_chunk(session, data, start, total).await,
        }
    }

    /// Bytes `start..end` of a file's content (for downloading placeholders on demand).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes> {
        match self {
            Remote::GoogleDrive(d) => d.download_range(id, start, end).await,
        }
    }

    /// Streams a file's content into `out`; returns (its hash, bytes written).
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        match self {
            Remote::GoogleDrive(d) => {
                let (md5, n) = d.download(id, out).await?;
                Ok((ContentHash::md5(md5), n))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_names_and_config_value() {
        let p = Provider::default();
        assert_eq!((p.as_str(), p.display_name()), ("gdrive", "Google Drive"));
        #[derive(Deserialize, Serialize)]
        struct C {
            provider: Provider,
        }
        let c: C = toml::from_str("provider = \"gdrive\"").unwrap();
        assert_eq!(c.provider, Provider::GoogleDrive);
        assert_eq!(toml::to_string(&c).unwrap().trim(), "provider = \"gdrive\"");
        assert!(toml::from_str::<C>("provider = \"floppy\"").is_err());
    }

    #[test]
    fn web_urls() {
        let p = Provider::GoogleDrive;
        assert_eq!(p.web_url(Kind::Dir, "X"), "https://drive.google.com/drive/folders/X");
        assert_eq!(p.web_url(Kind::File, "Y"), "https://drive.google.com/file/d/Y/view");
    }

    #[test]
    fn same_content_needs_hash_and_size() {
        let m = RemoteMeta { hash: Some(ContentHash::md5("ab")), size: Some(3), ..Default::default() };
        assert!(m.holds("ab", 3));
        assert!(!m.holds("ab", 4));
        assert!(!m.holds("cd", 3));
        assert!(!RemoteMeta { size: Some(3), ..Default::default() }.holds("ab", 3));
        assert_eq!(m.md5(), Some("ab"));
        assert_eq!(HashKind::Md5.db_value(), None);
    }
}
