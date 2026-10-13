use anyhow::{Context, Result};
use md5::{Digest, Md5};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parent of a relative entry path ("" for top-level entries).
pub fn parent_of(rel: &str) -> &str {
    rel.rfind('/').map(|i| &rel[..i]).unwrap_or("")
}

pub fn name_of(rel: &str) -> &str {
    rel.rfind('/').map(|i| &rel[i + 1..]).unwrap_or(rel)
}

pub fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// Local file metadata the engine tracks to detect changes and moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMeta {
    pub size: i64,
    pub mtime_ns: i64,
    pub inode: i64,
}

impl LocalMeta {
    pub fn of(md: &std::fs::Metadata) -> Self {
        Self {
            size: md.size() as i64,
            mtime_ns: md.mtime() * 1_000_000_000 + md.mtime_nsec(),
            inode: md.ino() as i64,
        }
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        std::fs::symlink_metadata(path).map(|m| Self::of(&m))
    }
}

/// md5 (hex) of everything left in `r`.
pub fn md5_reader(r: &mut impl Read) -> std::io::Result<String> {
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Hashes a file, returning (md5 hex, metadata before reading).
pub fn md5_file(path: &Path) -> std::io::Result<(String, LocalMeta)> {
    let mut f = std::fs::File::open(path)?;
    let meta = LocalMeta::of(&f.metadata()?);
    Ok((md5_reader(&mut f)?, meta))
}

/// "report.pdf" → "report (2).pdf"; ".bashrc" → ".bashrc (2)".
pub fn numbered_name(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({n}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({n})"),
    }
}

pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp-write");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert_eq!(parent_of("a/b/c.txt"), "a/b");
        assert_eq!(parent_of("c.txt"), "");
        assert_eq!(name_of("a/b/c.txt"), "c.txt");
        assert_eq!(join_rel("", "x"), "x");
        assert_eq!(join_rel("a", "x"), "a/x");
    }

    #[test]
    fn numbered() {
        assert_eq!(numbered_name("report.pdf", 2), "report (2).pdf");
        assert_eq!(numbered_name(".bashrc", 3), ".bashrc (3)");
        assert_eq!(numbered_name("Makefile", 2), "Makefile (2)");
        assert_eq!(numbered_name("a.tar.gz", 2), "a.tar (2).gz");
    }

    #[test]
    fn bytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
    }
}
