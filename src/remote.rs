//! The cloud CloudDrop uploads to, behind one interface (see docs/PROVIDERS.md, "Adding a
//! provider").
//!
//! Each provider is a [`Provider`] variant with a static [`ProviderInfo`], and a [`Remote`]
//! variant holding its client. A client is a struct with the same async methods as
//! [`crate::drive::Drive`] (`storage_quota`, `top_parent`, `get`, `find_child`,
//! `create_folder`, `move_rename`, `keep_version`, `start_upload`, `query_upload`,
//! `put_chunk`, `download_range`, `download`); [`Remote`] forwards to it with [`each_client!`],
//! so adding a provider adds a variant and one line to each of the two `match`es here.
//! The engine only talks to [`Remote`] and the neutral types in this module.

use crate::db::Kind;
use crate::drive::Drive;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

/// Which cloud a CloudDrop folder uploads to (`provider` in config.toml, `meta.provider`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    #[serde(rename = "gdrive")]
    GoogleDrive,
}

impl Provider {
    /// Every provider, for listings (setup, `clouddrop setup --provider`).
    pub const ALL: &'static [Provider] = &[Provider::GoogleDrive];

    pub fn info(self) -> &'static ProviderInfo {
        match self {
            Provider::GoogleDrive => &crate::drive::INFO,
        }
    }

    /// The name in config.toml, the database and `status --json`.
    pub fn as_str(self) -> &'static str {
        self.info().id
    }

    /// The name people know it by ("Google Drive").
    pub fn display_name(self) -> &'static str {
        self.info().name
    }

    pub fn caps(self) -> &'static Capabilities {
        &self.info().caps
    }

    /// Web page of a file or folder, for "Open in Google Drive"; None where there is none.
    pub fn web_url(self, kind: Kind, id: &str) -> Option<String> {
        (self.info().web_url)(kind, id)
    }

    pub fn parse(s: &str) -> Option<Provider> {
        Self::ALL.iter().copied().find(|p| p.as_str() == s)
    }

    pub fn is_default(&self) -> bool {
        *self == Provider::default()
    }
}

/// What the engine and the apps need to know about a provider without a client.
#[derive(Debug)]
pub struct ProviderInfo {
    /// `provider = "…"` in config.toml, and the name of its `[…]` config section.
    pub id: &'static str,
    pub name: &'static str,
    pub caps: Capabilities,
    pub sign_in: SignIn,
    pub web_url: fn(Kind, &str) -> Option<String>,
}

/// How someone connects CloudDrop to a provider; the setup assistants are drawn from it
/// (`clouddrop providers --json`).
#[derive(Debug)]
pub enum SignIn {
    /// Google's own flow (`auth.rs`).
    Google,
    /// OAuth 2 in the browser with PKCE and a loopback redirect (no client secret), done by the
    /// generic sign-in. The client id comes from `[<id>] client_id` or the build.
    OAuth(OAuthApp),
    /// Nextcloud/ownCloud Login Flow v2: the browser hands back an app password for a server URL.
    NextcloudLoginFlow,
    /// Fields the person fills in. Values of `Secret` fields go to the credentials store
    /// ([`crate::credentials`]), the others to the provider's config section.
    Form(&'static [Field]),
}

#[derive(Debug)]
pub struct OAuthApp {
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    /// Extra query parameters for the authorisation URL (e.g. Dropbox's token_access_type=offline).
    pub extra_auth_params: &'static [(&'static str, &'static str)],
}

#[derive(Debug)]
pub struct Field {
    /// Key in the config section (or the credentials store, for secrets).
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    /// Example or explanation shown under the field.
    pub hint: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Url,
    Number,
    /// Never shown again, never written to config.toml.
    Secret,
    /// A path to a file on this computer (an SSH key).
    File,
}

/// How a provider behaves, where the engine has to do things differently.
#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    /// The hash the provider reports for file content. None: it reports none, so freeing
    /// space proves the cloud copy by downloading it and hashing it (SHA-256) instead.
    pub hash: Option<HashKind>,
    /// `keep_version` pins the current version forever (Drive's keepForever). False: the client
    /// keeps a server-side copy next to the file instead ("name (replaced 2026-10-21).ext").
    pub pin_versions: bool,
    /// Ids survive renames and moves. False (paths as ids: WebDAV, S3, SFTP): `move_rename`
    /// returns the new id, and the ids of everything beneath a moved folder change with it.
    pub stable_ids: bool,
    /// Uploads continue after an interruption (`query_upload` works on an old session).
    pub resumable: bool,
    /// Names differing only in case are the same item.
    pub case_insensitive: bool,
    /// Characters names may not contain (the engine maps them before uploading).
    pub forbidden_chars: &'static str,
    /// Upload chunks must be a multiple of this many bytes (Drive 256 KiB, OneDrive 320 KiB).
    pub chunk_unit: usize,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum RemoteError {
    /// Sign-in is missing, expired or revoked; nothing will work until `clouddrop login`.
    #[error("Google sign-in needed: {0}")]
    Auth(String),
    /// Worth retrying later (network, server error).
    #[error("{0}")]
    Transient(String),
    /// The provider asked to slow down; `retry_after` seconds if it said. Retried like Transient.
    #[error("{message}")]
    RateLimited { message: String, retry_after: Option<u64> },
    #[error("not found in Google Drive")]
    NotFound,
    /// The account's storage is full. Not retried, like Permanent.
    #[error("{0}")]
    Quota(String),
    #[error("{0}")]
    Permanent(String),
}

impl RemoteError {
    /// Worth trying again later by itself.
    pub fn is_transient(&self) -> bool {
        matches!(self, RemoteError::Transient(_) | RemoteError::RateLimited { .. })
    }
}

pub type RemoteResult<T> = Result<T, RemoteError>;

/// Where a provider client gets an OAuth access token (implemented by `auth::Auth` for Google
/// and by the generic OAuth sign-in for the others).
pub trait TokenSource: Send + Sync {
    fn token(&self) -> BoxFuture<'_, RemoteResult<String>>;
    /// After a 401: a fresh token, or `Auth` when the sign-in is no longer valid.
    fn refresh(&self) -> BoxFuture<'_, RemoteResult<String>>;
}

/// How a provider hashes file content. Values are always lowercase hex ([`crate::hash`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashKind {
    /// MD5 of the whole content (Google Drive's `md5Checksum`, S3 single-part ETags).
    Md5,
    /// SHA-1 (Nextcloud/ownCloud `oc:checksums`, Proton Drive).
    Sha1,
    /// SHA-256 (S3 `x-amz-checksum-sha256`; what verify-by-download computes).
    Sha256,
    /// Microsoft's QuickXorHash (OneDrive, SharePoint).
    QuickXor,
    /// Dropbox's content hash (SHA-256 of 4 MiB block SHA-256s).
    DropboxContent,
}

impl HashKind {
    /// The value of `entries.hash_kind`; NULL means MD5, so older databases read as they are.
    pub fn db_value(self) -> Option<&'static str> {
        match self {
            HashKind::Md5 => None,
            HashKind::Sha1 => Some("sha1"),
            HashKind::Sha256 => Some("sha256"),
            HashKind::QuickXor => Some("quickxor"),
            HashKind::DropboxContent => Some("dropbox"),
        }
    }

    /// Reads `entries.hash_kind` back; None for a kind this version doesn't know.
    pub fn from_db(v: Option<&str>) -> Option<HashKind> {
        match v {
            None => Some(HashKind::Md5),
            Some("sha1") => Some(HashKind::Sha1),
            Some("sha256") => Some(HashKind::Sha256),
            Some("quickxor") => Some(HashKind::QuickXor),
            Some("dropbox") => Some(HashKind::DropboxContent),
            Some(_) => None,
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
    /// The cloud copy has this content (same hash, of the same kind, and size); whether it's
    /// trashed is separate.
    pub fn same_content(&self, hash: &ContentHash, size: u64) -> bool {
        self.hash.as_ref() == Some(hash) && self.size == Some(size)
    }

    /// [`Self::same_content`] for an MD5 (what `entries.md5` holds today).
    pub fn holds(&self, md5: &str, size: u64) -> bool {
        self.same_content(&ContentHash::md5(md5), size)
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
/// No limit means unlimited storage (or a provider that doesn't say, like S3).
pub type StorageQuota = (Option<u64>, u64, u64);

/// The client for the configured provider.
pub enum Remote {
    GoogleDrive(Drive),
}

/// Runs `$body` with `$c` bound to whichever client `$remote` holds.
macro_rules! each_client {
    ($remote:expr, $c:ident => $body:expr) => {
        match $remote {
            Remote::GoogleDrive($c) => $body,
        }
    };
}

impl Remote {
    pub fn provider(&self) -> Provider {
        match self {
            Remote::GoogleDrive(_) => Provider::GoogleDrive,
        }
    }

    pub fn caps(&self) -> &'static Capabilities {
        self.provider().caps()
    }

    pub async fn storage_quota(&self) -> RemoteResult<StorageQuota> {
        each_client!(self, c => c.storage_quota().await)
    }

    /// The id of the folder the top CloudDrop folder lives in (Drive's "root" = My Drive;
    /// an app folder elsewhere).
    pub async fn top_parent(&self) -> RemoteResult<String> {
        each_client!(self, c => c.top_parent().await)
    }

    pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta> {
        each_client!(self, c => c.get(id).await)
    }

    /// Items named `name` directly inside `parent` that CloudDrop can see (not trashed).
    pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>> {
        each_client!(self, c => c.find_child(name, parent, folder).await)
    }

    pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta> {
        each_client!(self, c => c.create_folder(name, parent).await)
    }

    /// Renames and/or moves an item. `parents` = (new, old) when moving. The returned
    /// metadata carries the item's id afterwards (a new one where ids are paths).
    pub async fn move_rename(
        &self,
        id: &str,
        name: Option<&str>,
        parents: Option<(&str, &str)>,
    ) -> RemoteResult<RemoteMeta> {
        each_client!(self, c => c.move_rename(id, name, parents).await)
    }

    /// Keeps the current content forever, so replacing it never loses it: a pinned version,
    /// or a copy next to the file (see [`Capabilities::pin_versions`]).
    pub async fn keep_version(&self, id: &str) -> RemoteResult<()> {
        each_client!(self, c => c.keep_version(id).await)
    }

    /// Opens a resumable upload session; returns the session (kept in `entries.upload_url`;
    /// any string the client can resume from, such as a URL or a small JSON object).
    /// `existing` = upload new content for that file instead of creating one.
    pub async fn start_upload(
        &self,
        existing: Option<&str>,
        name: &str,
        parents: (&str, Option<&str>),
        size: u64,
        mime: &str,
    ) -> RemoteResult<String> {
        each_client!(self, c => c.start_upload(existing, name, parents, size, mime).await)
    }

    /// Asks a session how much it has received. NotFound = the session expired.
    pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus> {
        each_client!(self, c => c.query_upload(session, total).await)
    }

    pub async fn put_chunk(&self, session: &str, data: bytes::Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus> {
        each_client!(self, c => c.put_chunk(session, data, start, total).await)
    }

    /// Bytes `start..end` of a file's content (for downloading placeholders on demand).
    pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes> {
        each_client!(self, c => c.download_range(id, start, end).await)
    }

    /// Streams a file's content into `out`; returns (its hash, bytes written). The hash is of
    /// the provider's kind, or SHA-256 for providers that report none.
    pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)> {
        each_client!(self, c => c.download(id, out).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_names_and_config_value() {
        let p = Provider::default();
        assert_eq!((p.as_str(), p.display_name()), ("gdrive", "Google Drive"));
        assert_eq!(Provider::parse("gdrive"), Some(Provider::GoogleDrive));
        assert_eq!(Provider::parse("floppy"), None);
        #[derive(Deserialize, Serialize)]
        struct C {
            provider: Provider,
        }
        let c: C = toml::from_str("provider = \"gdrive\"").unwrap();
        assert_eq!(c.provider, Provider::GoogleDrive);
        assert_eq!(toml::to_string(&c).unwrap().trim(), "provider = \"gdrive\"");
        assert!(toml::from_str::<C>("provider = \"floppy\"").is_err());
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.as_str()), Some(*p), "ids are unique and parse back");
        }
    }

    #[test]
    fn web_urls() {
        let p = Provider::GoogleDrive;
        assert_eq!(p.web_url(Kind::Dir, "X").as_deref(), Some("https://drive.google.com/drive/folders/X"));
        assert_eq!(p.web_url(Kind::File, "Y").as_deref(), Some("https://drive.google.com/file/d/Y/view"));
    }

    #[test]
    fn same_content_needs_hash_kind_and_size() {
        let m = RemoteMeta { hash: Some(ContentHash::md5("ab")), size: Some(3), ..Default::default() };
        assert!(m.holds("ab", 3));
        assert!(!m.holds("ab", 4));
        assert!(!m.holds("cd", 3));
        assert!(!RemoteMeta { size: Some(3), ..Default::default() }.holds("ab", 3));
        let sha1 = ContentHash { kind: HashKind::Sha1, value: "ab".into() };
        assert!(!m.same_content(&sha1, 3), "same hex of another kind is not the same content");
        assert_eq!(m.md5(), Some("ab"));
    }

    #[test]
    fn hash_kinds_round_trip_through_the_database() {
        for k in [HashKind::Md5, HashKind::Sha1, HashKind::Sha256, HashKind::QuickXor, HashKind::DropboxContent] {
            assert_eq!(HashKind::from_db(k.db_value()), Some(k));
        }
        assert_eq!(HashKind::Md5.db_value(), None);
        assert_eq!(HashKind::from_db(Some("blake9")), None);
    }

    #[test]
    fn rate_limits_and_full_storage() {
        assert!(RemoteError::RateLimited { message: "slow".into(), retry_after: Some(3) }.is_transient());
        assert!(RemoteError::Transient("x".into()).is_transient());
        assert!(!RemoteError::Quota("full".into()).is_transient());
        assert_eq!(RemoteError::Quota("storage is full".into()).to_string(), "storage is full");
    }
}
