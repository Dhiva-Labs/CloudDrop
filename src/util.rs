use anyhow::{Context, Result};
use md5::{Digest, Md5};
use std::io::{Read, Write};
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
    /// The item at `path`, whose `symlink_metadata` is `md`.
    pub fn at(path: &Path, md: &std::fs::Metadata) -> Self {
        Self {
            size: md.len() as i64,
            mtime_ns: mtime_ns(md),
            inode: file_id::of_path(path, md),
        }
    }

    /// An open file: identity of exactly this file, whatever its path is now.
    pub fn of_file(f: &std::fs::File) -> std::io::Result<Self> {
        let md = f.metadata()?;
        Ok(Self {
            size: md.len() as i64,
            mtime_ns: mtime_ns(&md),
            inode: file_id::of_file(f, &md),
        })
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        std::fs::symlink_metadata(path).map(|m| Self::at(path, &m))
    }
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// A number that stays the same for a file across renames: the inode on Unix, the
/// NTFS file ID (mixed with the volume serial) on Windows. Used to recognise moves.
mod file_id {
    #[cfg(unix)]
    pub fn of_path(_: &std::path::Path, md: &std::fs::Metadata) -> i64 {
        std::os::unix::fs::MetadataExt::ino(md) as i64
    }

    #[cfg(unix)]
    pub fn of_file(_: &std::fs::File, md: &std::fs::Metadata) -> i64 {
        std::os::unix::fs::MetadataExt::ino(md) as i64
    }

    #[cfg(windows)]
    pub fn of_file(f: &std::fs::File, _: &std::fs::Metadata) -> i64 {
        use std::os::windows::io::AsRawHandle;
        by_handle(f.as_raw_handle()).unwrap_or(0)
    }

    #[cfg(windows)]
    pub fn of_path(path: &std::path::Path, _: &std::fs::Metadata) -> i64 {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000; // needed to open folders
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000; // never hydrate placeholders
        std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .ok()
            .and_then(|f| by_handle(f.as_raw_handle()))
            .unwrap_or(0)
    }

    #[cfg(windows)]
    fn by_handle(h: std::os::windows::io::RawHandle) -> Option<i64> {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(h as _, &mut info) } == 0 {
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Some((index ^ ((info.dwVolumeSerialNumber as u64) << 40)) as i64)
    }
}

/// `path` relative to `root` as an entry path ('/'-separated), if inside it.
pub fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?.to_str()?;
    #[cfg(windows)]
    let rel = rel.replace('\\', "/");
    Some(rel.trim_end_matches('/').to_string())
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
    let meta = LocalMeta::of_file(&f)?;
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
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts
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
