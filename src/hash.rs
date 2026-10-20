//! Content hashes the way each provider computes them, so CloudDrop can check that the cloud
//! holds exactly what is on this computer. Values are lowercase hex whatever the provider's
//! own encoding (OneDrive sends QuickXorHash in base64; its client converts).

use crate::remote::{ContentHash, HashKind};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io::Read;

/// Hashes content incrementally in one provider's way.
pub struct Hasher {
    kind: HashKind,
    state: State,
}

enum State {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
    QuickXor(QuickXor),
    Dropbox(DropboxContent),
}

impl Hasher {
    pub fn new(kind: HashKind) -> Self {
        let state = match kind {
            HashKind::Md5 => State::Md5(Md5::new()),
            HashKind::Sha1 => State::Sha1(Sha1::new()),
            HashKind::Sha256 => State::Sha256(Sha256::new()),
            HashKind::QuickXor => State::QuickXor(QuickXor::default()),
            HashKind::DropboxContent => State::Dropbox(DropboxContent::default()),
        };
        Hasher { kind, state }
    }

    pub fn update(&mut self, data: &[u8]) {
        match &mut self.state {
            State::Md5(h) => h.update(data),
            State::Sha1(h) => h.update(data),
            State::Sha256(h) => h.update(data),
            State::QuickXor(h) => h.update(data),
            State::Dropbox(h) => h.update(data),
        }
    }

    pub fn finish(self) -> ContentHash {
        let value = match self.state {
            State::Md5(h) => hex::encode(h.finalize()),
            State::Sha1(h) => hex::encode(h.finalize()),
            State::Sha256(h) => hex::encode(h.finalize()),
            State::QuickXor(h) => hex::encode(h.finish()),
            State::Dropbox(h) => hex::encode(h.finish()),
        };
        ContentHash { kind: self.kind, value }
    }
}

/// Hashes everything `r` yields.
pub fn hash_reader(kind: HashKind, r: &mut impl Read) -> std::io::Result<ContentHash> {
    let mut h = Hasher::new(kind);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(h.finish());
        }
        h.update(&buf[..n]);
    }
}

/// Microsoft's QuickXorHash (OneDrive and SharePoint): a 160-bit XOR of the bytes, each shifted
/// 11 bits further than the last, with the length XORed into the last 8 bytes. A port of the
/// reference C# implementation in the OneDrive API documentation.
#[derive(Default)]
struct QuickXor {
    data: [u64; 3],
    shift_so_far: usize,
    length: u64,
}

const QX_WIDTH: usize = 160;
const QX_SHIFT: usize = 11;

impl QuickXor {
    fn update(&mut self, bytes: &[u8]) {
        let mut index = self.shift_so_far / 64;
        let mut offset = self.shift_so_far % 64;
        for i in 0..bytes.len().min(QX_WIDTH) {
            let last = index == self.data.len() - 1;
            let bits = if last { 32 } else { 64 };
            if offset <= bits - 8 {
                for &b in bytes[i..].iter().step_by(QX_WIDTH) {
                    self.data[index] ^= u64::from(b) << offset;
                }
            } else {
                let next = if last { 0 } else { index + 1 };
                let mut x = 0u8;
                for &b in bytes[i..].iter().step_by(QX_WIDTH) {
                    x ^= b;
                }
                self.data[index] ^= u64::from(x) << offset;
                self.data[next] ^= u64::from(x) >> (bits - offset);
            }
            offset += QX_SHIFT;
            while offset >= bits {
                index = if last { 0 } else { index + 1 };
                offset -= bits;
            }
        }
        self.shift_so_far = (self.shift_so_far + QX_SHIFT * (bytes.len() % QX_WIDTH)) % QX_WIDTH;
        self.length += bytes.len() as u64;
    }

    fn finish(self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[..8].copy_from_slice(&self.data[0].to_le_bytes());
        out[8..16].copy_from_slice(&self.data[1].to_le_bytes());
        out[16..].copy_from_slice(&self.data[2].to_le_bytes()[..4]);
        for (o, l) in out[12..].iter_mut().zip(self.length.to_le_bytes()) {
            *o ^= l;
        }
        out
    }
}

/// Dropbox's content hash: SHA-256 of the concatenated SHA-256s of each 4 MiB block.
#[derive(Default)]
struct DropboxContent {
    overall: Sha256,
    block: Sha256,
    in_block: usize,
}

const DROPBOX_BLOCK: usize = 4 * 1024 * 1024;

impl DropboxContent {
    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let take = (DROPBOX_BLOCK - self.in_block).min(data.len());
            self.block.update(&data[..take]);
            self.in_block += take;
            data = &data[take..];
            if self.in_block == DROPBOX_BLOCK {
                self.overall.update(self.block.finalize_reset());
                self.in_block = 0;
            }
        }
    }

    fn finish(mut self) -> [u8; 32] {
        if self.in_block > 0 {
            self.overall.update(self.block.finalize());
        }
        self.overall.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;

    fn hash(kind: HashKind, data: &[u8]) -> String {
        let mut h = Hasher::new(kind);
        h.update(data);
        h.finish().value
    }

    /// The same content fed in uneven pieces hashes the same as in one go.
    fn chunked(kind: HashKind, data: &[u8], sizes: &[usize]) -> String {
        let mut h = Hasher::new(kind);
        let mut rest = data;
        for &n in sizes.iter().cycle() {
            if rest.is_empty() {
                break;
            }
            let n = n.min(rest.len());
            h.update(&rest[..n]);
            rest = &rest[n..];
        }
        h.finish().value
    }

    #[test]
    fn quickxor_matches_rclone_vectors() {
        // From rclone's backend/onedrive/quickxorhash tests (input and hash in base64).
        for (input, expected) in [
            ("", "AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            ("Sg==", "SgAAAAAAAAAAAAAAAQAAAAAAAAA="),
            ("/aNzzCFPS/A=", "/RjFHJgRgicsAR4ACAAAAAAAAAA="),
            ("T6LYJIfDh81JrAK309H2JMJTXis=", "zBTHrspn3mEcohlJdIUAbjGNaNg="),
            ("DWAAX5/CIfrmErgZa8ot6ZraeSbu", "LR2Z0PjuRYGKQB/mhQAuMrAGZbQ="),
            ("NbYXsp5/K6mR+NmHwExjvWeWDJFnXTKWVlzYHoesp2E=", "wjuAuWDiq04qDt1R8hHWDDcwVoQ="),
            ("0n7nl3YJtipy6yeUbVPWtc2h45WbF9u8hTz5tNwj3dZZwfXWkk+GN3g=", "YJLNK7JR64j9aODWfqDvEe/u6NU="),
        ] {
            let data = B64.decode(input).unwrap();
            let want = hex::encode(B64.decode(expected).unwrap());
            assert_eq!(hash(HashKind::QuickXor, &data), want, "input {input}");
            assert_eq!(chunked(HashKind::QuickXor, &data, &[3, 1, 7]), want, "chunked {input}");
        }
    }

    #[test]
    fn quickxor_streams_across_the_160_byte_width() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 31 % 251) as u8).collect();
        let whole = hash(HashKind::QuickXor, &data);
        assert_eq!(chunked(HashKind::QuickXor, &data, &[159, 1, 161, 320, 7]), whole);
        assert_eq!(chunked(HashKind::QuickXor, &data, &[1]), whole);
    }

    #[test]
    fn dropbox_content_hash() {
        // Expected values from an independent Python reference of Dropbox's documented algorithm.
        assert_eq!(
            hash(HashKind::DropboxContent, b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash(HashKind::DropboxContent, b"hello"),
            "9595c9df90075148eb06860365df33584b75bff782a510c6cd4883a419833d50"
        );
        let big: Vec<u8> = (0..(9 << 20) as u32).map(|i| (i * 31 % 251) as u8).collect();
        let want = "3006ece4dbd44b786c435dbf7ef8efa79b59d681654ae73107d86de955cb3322";
        let got = hash(HashKind::DropboxContent, &big);
        assert_eq!(got, want);
        assert_eq!(chunked(HashKind::DropboxContent, &big, &[1 << 20, 3, 5 << 20]), want);
    }

    #[test]
    fn plain_digests() {
        assert_eq!(hash(HashKind::Md5, b"hello"), "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(hash(HashKind::Sha1, b"hello"), "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
        assert_eq!(
            hash(HashKind::Sha256, b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let mut r: &[u8] = b"hello";
        assert_eq!(hash_reader(HashKind::Md5, &mut r).unwrap().value, "5d41402abc4b2a76b9719d911017c592");
    }
}
