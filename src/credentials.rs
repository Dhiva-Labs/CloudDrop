//! Secrets that aren't OAuth tokens (WebDAV app passwords, S3 secret keys, SFTP key
//! passphrases, MEGA and Proton sessions): `credentials.json` in the data directory, readable
//! only by you, one object per provider id. OAuth tokens stay in their own token files.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub fn path() -> PathBuf {
    crate::config::data_dir().join("credentials.json")
}

/// The provider's secrets, if any were saved.
pub fn load(provider: &str) -> Result<Option<Map<String, Value>>> {
    load_from(&path(), provider)
}

/// Replaces the provider's secrets.
pub fn save(provider: &str, secrets: Map<String, Value>) -> Result<()> {
    save_to(&path(), provider, secrets)
}

/// Forgets the provider's secrets (sign out).
pub fn forget(provider: &str) -> Result<()> {
    let file = path();
    let mut all = read_all(&file)?;
    if all.remove(provider).is_some() {
        write_all(&file, &all)?;
    }
    Ok(())
}

pub fn load_from(file: &Path, provider: &str) -> Result<Option<Map<String, Value>>> {
    Ok(read_all(file)?.remove(provider).and_then(|v| match v {
        Value::Object(m) => Some(m),
        _ => None,
    }))
}

pub fn save_to(file: &Path, provider: &str, secrets: Map<String, Value>) -> Result<()> {
    let mut all = read_all(file)?;
    all.insert(provider.to_string(), Value::Object(secrets));
    write_all(file, &all)
}

fn read_all(file: &Path) -> Result<Map<String, Value>> {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid JSON", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
    }
}

fn write_all(file: &Path, all: &Map<String, Value>) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::util::write_private(file, serde_json::to_string_pretty(all)?.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn one_object_per_provider() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials.json");
        assert_eq!(load_from(&file, "webdav").unwrap(), None);
        let m = |v: Value| v.as_object().unwrap().clone();
        save_to(&file, "webdav", m(json!({"password": "p1"}))).unwrap();
        save_to(&file, "s3", m(json!({"secret_access_key": "k"}))).unwrap();
        assert_eq!(load_from(&file, "webdav").unwrap().unwrap()["password"], "p1");
        save_to(&file, "webdav", m(json!({"password": "p2"}))).unwrap();
        assert_eq!(load_from(&file, "webdav").unwrap().unwrap()["password"], "p2");
        assert_eq!(load_from(&file, "s3").unwrap().unwrap()["secret_access_key"], "k");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
