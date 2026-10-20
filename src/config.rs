use crate::remote::Provider;
use crate::webdav::WebDavConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default upload chunk: 8 MiB (Drive requires multiples of 256 KiB).
pub const DEFAULT_CHUNK: usize = 8 * 1024 * 1024;
const CHUNK_UNIT: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The cloud to upload to; absent = Google Drive (the only one so far).
    #[serde(skip_serializing_if = "Provider::is_default")]
    pub provider: Provider,
    pub sync_dir: PathBuf,
    /// Name of the top folder in the cloud. `remote_folder` is read as well, the name it
    /// will have once there is more than one provider.
    #[serde(alias = "remote_folder")]
    pub drive_folder: String,
    pub client_id: String,
    pub client_secret: String,
    pub parallel_uploads: usize,
    pub settle_secs: u64,
    pub rescan_secs: u64,
    pub notifications: bool,
    pub ignore: Vec<String>,
    /// Free local copies automatically this many days after upload (0 = right after).
    /// Absent = never.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_free_days: Option<u32>,
    /// Folders (relative to sync_dir) that automatic space saving never touches.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keep_local: Vec<String>,
    #[serde(skip_serializing_if = "is_default_chunk")]
    pub chunk_size: usize,
    #[serde(skip_serializing_if = "Endpoints::is_default")]
    pub endpoints: Endpoints,
    #[serde(default, skip_serializing_if = "OneDriveConfig::is_default")]
    pub onedrive: OneDriveConfig,
    #[serde(skip_serializing_if = "crate::dropbox::DropboxConfig::is_default")]
    pub dropbox: crate::dropbox::DropboxConfig,
    /// `[webdav]`: a WebDAV server (its app password is in credentials.json).
    #[serde(default, skip_serializing_if = "WebDavConfig::is_unset")]
    pub webdav: WebDavConfig,
    /// `[nextcloud]`: a Nextcloud server, signed in through the browser.
    #[serde(default, skip_serializing_if = "WebDavConfig::is_unset")]
    pub nextcloud: WebDavConfig,
}

/// Google endpoints; overridable so tests can point at a local fake.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Endpoints {
    pub auth_url: String,
    pub token_url: String,
    pub api_base: String,
    pub upload_base: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            api_base: "https://www.googleapis.com/drive/v3".into(),
            upload_base: "https://www.googleapis.com/upload/drive/v3".into(),
        }
    }
}

impl Endpoints {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `[onedrive]`: which Azure app signs in, and which accounts may.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OneDriveConfig {
    /// The app registration's client id; empty = the one CloudDrop is built with.
    pub client_id: String,
    /// "common" (personal, work and school accounts), "consumers", "organizations" or one
    /// organisation's tenant id ([`crate::onedrive::oauth_endpoints`]).
    pub tenant: String,
}

impl Default for OneDriveConfig {
    fn default() -> Self {
        Self { client_id: String::new(), tenant: "common".into() }
    }
}

impl OneDriveConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn is_default_chunk(n: &usize) -> bool {
    *n == DEFAULT_CHUNK
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: Provider::default(),
            sync_dir: dirs::home_dir().unwrap_or_default().join("CloudDrop"),
            drive_folder: "CloudDrop".into(),
            client_id: String::new(),
            client_secret: String::new(),
            parallel_uploads: 2,
            settle_secs: 2,
            rescan_secs: 600,
            notifications: true,
            ignore: default_ignore(),
            auto_free_days: None,
            keep_local: Vec::new(),
            chunk_size: DEFAULT_CHUNK,
            endpoints: Endpoints::default(),
            onedrive: OneDriveConfig::default(),
            dropbox: Default::default(),
            webdav: WebDavConfig::default(),
            nextcloud: WebDavConfig::default(),
        }
    }
}

pub fn default_ignore() -> Vec<String> {
    [
        ".clouddrop-*",
        ".goutputstream-*",
        "*.part",
        "*.crdownload",
        "*.tmp",
        "~$*",
        ".~lock.*#",
        "*.swp",
        "*~",
        ".Trash-*",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CLOUDDROP_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clouddrop")
        .join("config.toml")
}

pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("CLOUDDROP_DATA_DIR") {
        return PathBuf::from(p);
    }
    // Local (not roaming) app data on Windows; ~/.local/share on Linux.
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clouddrop")
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "no config at {} — run `clouddrop setup --client-id … --client-secret …` first",
                path.display()
            )
        })?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.sync_dir.is_absolute() {
            bail!("sync_dir must be an absolute path");
        }
        if self.drive_folder.trim().is_empty() {
            bail!("drive_folder must not be empty");
        }
        if self.chunk_size == 0 || !self.chunk_size.is_multiple_of(CHUNK_UNIT) {
            bail!("chunk_size must be a positive multiple of 256 KiB");
        }
        if self.parallel_uploads == 0 {
            bail!("parallel_uploads must be at least 1");
        }
        Ok(())
    }

    /// Changes one user-editable setting from text (used by `clouddrop config set`
    /// and the desktop apps). The running service picks the change up by itself.
    pub fn set_key(&mut self, key: &str, value: &str) -> Result<()> {
        let v = value.trim();
        let num = |what: &str| -> Result<u64> {
            v.parse::<u64>().map_err(|_| anyhow::anyhow!("{what} must be a whole number"))
        };
        match key {
            "parallel_uploads" => {
                let n = num(key)?;
                if !(1..=16).contains(&n) {
                    bail!("parallel_uploads must be between 1 and 16");
                }
                self.parallel_uploads = n as usize;
            }
            "settle_secs" => self.settle_secs = num(key)?.min(3600),
            "rescan_secs" => self.rescan_secs = num(key)?.max(30),
            "notifications" => {
                self.notifications = match v {
                    "true" | "on" | "yes" | "1" => true,
                    "false" | "off" | "no" | "0" => false,
                    _ => bail!("notifications must be true or false"),
                }
            }
            "ignore" => {
                self.ignore = v
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            }
            "auto_free_days" => {
                self.auto_free_days = match v {
                    "never" | "off" | "" => None,
                    "now" | "after_upload" => Some(0),
                    _ => Some(num(key)?.min(3650) as u32),
                }
            }
            "keep_local" => {
                self.keep_local = v
                    .split(',')
                    .map(|p| p.trim().trim_matches('/').to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            }
            other => bail!(
                "unknown or read-only setting {other:?}; editable: parallel_uploads, settle_secs, \
                 rescan_secs, notifications, ignore, auto_free_days, keep_local"
            ),
        }
        self.validate()
    }

    /// The config as JSON for desktop apps, with the client secret withheld.
    pub fn public_json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "client_secret".into(),
                serde_json::Value::Bool(!self.client_secret.is_empty()),
            );
            obj.insert("auto_free_days".into(), serde_json::json!(self.auto_free_days));
            obj.insert("keep_local".into(), serde_json::json!(self.keep_local));
            obj.insert("config_path".into(), serde_json::json!(config_path()));
            obj.insert("data_dir".into(), serde_json::json!(data_dir()));
        }
        v
    }

    /// `rel` is inside a folder marked "always keep on this computer".
    pub fn keeps_local(&self, rel: &str) -> bool {
        self.keep_local.iter().any(|k| {
            let k = k.trim_matches('/');
            k.is_empty() || rel == k || rel.starts_with(&format!("{k}/"))
        })
    }

    /// The provider's `[<id>]` section as it is written to config.toml (empty when it has none).
    pub fn provider_section(&self, provider: Provider) -> toml::Table {
        toml::Table::try_from(self)
            .ok()
            .and_then(|mut t| t.remove(provider.as_str()))
            .and_then(|v| match v {
                toml::Value::Table(t) => Some(t),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Sets `key` in the provider's `[<id>]` section (`clouddrop setup --set key=value`), through
    /// the provider's own section struct so its types and checks apply. `known`: the key is one
    /// of the provider's (a form field, or `client_id`), so it is accepted even when it equals
    /// the default and isn't written.
    pub fn set_provider_value(&mut self, provider: Provider, key: &str, value: &str, known: bool) -> Result<()> {
        *self = set_in_section(&*self, provider.as_str(), key, value, known)
            .with_context(|| format!("cannot set {key} for {}", provider.display_name()))?;
        Ok(())
    }

    /// Writes the config with owner-only permissions (it holds the OAuth client secret).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
        crate::util::write_private(path, text.as_bytes())
    }
}

/// `whole` with `[section] key = value`. The value is tried as text, then as what it reads as in
/// TOML (a number, true/false) for fields of other types. Fails when neither fits, or when the
/// section has no such setting (it would be silently dropped) unless the key is `known`.
fn set_in_section<T: Serialize + DeserializeOwned>(
    whole: &T,
    section: &str,
    key: &str,
    value: &str,
    known: bool,
) -> Result<T> {
    let table = toml::Table::try_from(whole)?;
    let mut tries = vec![toml::Value::String(value.to_string())];
    if let Ok(t) = toml::from_str::<toml::Table>(&format!("v = {value}"))
        && let Some(v) = t.get("v").filter(|v| !v.is_str())
    {
        tries.push(v.clone());
    }
    let mut last_err = None;
    for v in tries {
        let mut t = table.clone();
        let sec = t
            .entry(section)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(sec) = sec.as_table_mut() else {
            bail!("{section} is a setting, not a section");
        };
        sec.insert(key.to_string(), v);
        match t.try_into::<T>() {
            Ok(back) => {
                let kept = toml::Table::try_from(&back)?
                    .get(section)
                    .and_then(|s| s.get(key))
                    .is_some();
                if !kept && !known {
                    bail!("there is no setting {key:?} in [{section}]");
                }
                return Ok(back);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow!("invalid value {value:?}: {}", last_err.map(|e| e.to_string()).unwrap_or_default().trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_set_through_their_struct() {
        #[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
        #[serde(default)]
        struct Sftp {
            host: String,
            port: u16,
            #[serde(skip_serializing_if = "String::is_empty")]
            path: String,
        }
        #[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
        #[serde(default)]
        struct Whole {
            sync_dir: String,
            sftp: Sftp,
        }
        let w = Whole::default();
        let w = set_in_section(&w, "sftp", "host", "example.com", false).unwrap();
        let w = set_in_section(&w, "sftp", "port", "2222", false).unwrap();
        assert_eq!((w.sftp.host.as_str(), w.sftp.port), ("example.com", 2222));
        let w = set_in_section(&w, "sftp", "host", "1234", false).unwrap();
        assert_eq!(w.sftp.host, "1234", "text first, for text fields that look like numbers");
        assert!(set_in_section(&w, "sftp", "port", "many", false).is_err());
        assert!(set_in_section(&w, "sftp", "hots", "typo", false).is_err(), "unknown settings are refused");
        assert!(set_in_section(&w, "sftp", "path", "", false).is_err(), "dropped as the default…");
        assert!(set_in_section(&w, "sftp", "path", "", true).is_ok(), "…which is fine for a known key");
        assert!(set_in_section(&w, "sync_dir", "x", "y", false).is_err());
    }

    #[test]
    fn google_drive_has_no_section() {
        let mut c = Config::default();
        assert!(c.provider_section(Provider::GoogleDrive).is_empty());
        assert!(c.set_provider_value(Provider::GoogleDrive, "url", "x", false).is_err());
    }

    #[test]
    fn set_key_validates_and_maps() {
        let mut c = Config::default();
        c.set_key("parallel_uploads", "4").unwrap();
        c.set_key("notifications", "off").unwrap();
        c.set_key("auto_free_days", "now").unwrap();
        c.set_key("keep_local", "Work/, Photos ,").unwrap();
        c.set_key("ignore", "*.tmp, *.bak").unwrap();
        assert_eq!((c.parallel_uploads, c.notifications, c.auto_free_days), (4, false, Some(0)));
        assert_eq!(c.keep_local, ["Work", "Photos"]);
        assert_eq!(c.ignore, ["*.tmp", "*.bak"]);
        c.set_key("auto_free_days", "never").unwrap();
        assert_eq!(c.auto_free_days, None);
        assert!(c.set_key("parallel_uploads", "0").is_err());
        assert!(c.set_key("sync_dir", "/x").is_err());
        assert_eq!(c.public_json()["client_secret"], serde_json::json!(false));
    }

    #[test]
    fn keep_local_matches_folders_not_prefixes() {
        let cfg = Config { keep_local: vec!["Work".into(), "Photos/2026/".into()], ..Config::default() };
        assert!(cfg.keeps_local("Work"));
        assert!(cfg.keeps_local("Work/a.txt"));
        assert!(cfg.keeps_local("Photos/2026/x.jpg"));
        assert!(!cfg.keeps_local("Workshop/a.txt"));
        assert!(!cfg.keeps_local("Photos/2025/x.jpg"));
    }

    #[test]
    fn new_options_round_trip_and_default_off() {
        let cfg = Config { auto_free_days: Some(7), keep_local: vec!["Work".into()], ..Config::default() };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.auto_free_days, Some(7));
        assert_eq!(back.keep_local, ["Work"]);
        let old: Config = toml::from_str("sync_dir = \"/x\"").unwrap();
        assert_eq!(old.auto_free_days, None);
    }

    #[test]
    fn provider_defaults_to_google_drive_and_stays_out_of_the_file() {
        let old: Config = toml::from_str("sync_dir = \"/x\"\ndrive_folder = \"Backup\"").unwrap();
        assert_eq!(old.provider, Provider::GoogleDrive);
        assert!(!toml::to_string_pretty(&old).unwrap().contains("provider"));
        let new: Config = toml::from_str("provider = \"gdrive\"\nremote_folder = \"Backup\"").unwrap();
        assert_eq!((new.provider, new.drive_folder.as_str()), (Provider::GoogleDrive, "Backup"));
        assert!(toml::from_str::<Config>("provider = \"floppy\"").is_err());
    }
}
