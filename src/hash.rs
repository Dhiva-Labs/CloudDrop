//! Content hashes. A provider reports one for each stored file; CloudDrop computes
//! the same kind locally, and a local copy is freed only when the two are equal.

use crate::util::LocalMeta;
use md5::{Digest, Md5};
use std::io::Read;
use std::path::Path;

/// How a provider fingerprints content. Google Drive: MD5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashKind {
    Md5,
}

impl HashKind {
    /// The `hash_kind` column value: NULL for MD5, the only kind before schema 3.
    pub fn to_db(self) -> Option<&'static str> {
        match self {
            HashKind::Md5 => None,
        }
    }

    pub fn from_db(s: Option<&str>) -> Option<Self> {
        match s {
            None | Some("md5") => Some(HashKind::Md5),
            Some(_) => None,
        }
    }

    pub fn hasher(self) -> Hasher {
        match self {
            HashKind::Md5 => Hasher::Md5(Md5::new()),
        }
    }
}

/// A content hash: equal only when both the kind and the (lowercase hex) value are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHash {
    pub kind: HashKind,
    pub value: String,
}

impl ContentHash {
    pub fn md5(value: impl Into<String>) -> Self {
        Self { kind: HashKind::Md5, value: value.into() }
    }
}

/// Computes a hash incrementally (while a download streams, say).
pub enum Hasher {
    Md5(Md5),
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Md5(h) => h.update(data),
        }
    }

    pub fn finish(self) -> ContentHash {
        match self {
            Hasher::Md5(h) => ContentHash::md5(hex::encode(h.finalize())),
        }
    }
}

/// Hash of everything left in `r`.
pub fn hash_reader(kind: HashKind, r: &mut impl Read) -> std::io::Result<ContentHash> {
    let mut hasher = kind.hasher();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish())
}

/// Hashes a file, returning (hash, metadata before reading).
pub fn hash_file(kind: HashKind, path: &Path) -> std::io::Result<(ContentHash, LocalMeta)> {
    let mut f = std::fs::File::open(path)?;
    let meta = LocalMeta::of_file(&f)?;
    Ok((hash_reader(kind, &mut f)?, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_drive() {
        let h = hash_reader(HashKind::Md5, &mut &b"hello"[..]).unwrap();
        assert_eq!(h, ContentHash::md5("5d41402abc4b2a76b9719d911017c592"));
        assert_eq!(HashKind::from_db(HashKind::Md5.to_db()), Some(HashKind::Md5));
        assert_eq!(HashKind::from_db(Some("sha256")), None, "a kind from a newer version");
    }
}
