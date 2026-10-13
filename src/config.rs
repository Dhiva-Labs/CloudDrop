use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default upload chunk: 8 MiB (Drive requires multiples of 256 KiB).
pub const DEFAULT_CHUNK: usize = 8 * 1024 * 1024;
const CHUNK_UNIT: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sync_dir: PathBuf,
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

fn is_default_chunk(n: &usize) -> bool {
    *n == DEFAULT_CHUNK
}

impl Default for Config {
    fn default() -> Self {
        Self {
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
        .join("clouddrop/config.toml")
}

pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("CLOUDDROP_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
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

    /// `rel` is inside a folder marked "always keep on this computer".
    pub fn keeps_local(&self, rel: &str) -> bool {
        self.keep_local.iter().any(|k| {
            let k = k.trim_matches('/');
            k.is_empty() || rel == k || rel.starts_with(&format!("{k}/"))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
