//! The Proton Drive client against the fake `proton-drive` (`examples/fake_proton_drive.rs`):
//! the commands it runs, a staged upload resumed after a crash, new revisions, keep_version,
//! moves and renames under stable ids, downloads (whole and ranges) and errors.

mod common;

use clouddrop::proton::{Proton, ProtonConfig};
use clouddrop::remote::{Remote, RemoteError, RemoteMeta, UploadStatus};
use common::proton_fake::FakeProton;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

struct Setup {
    fake: FakeProton,
    _tmp: tempfile::TempDir,
    data: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        Setup { fake: FakeProton::new(), _tmp: tmp, data }
    }

    fn client(&self) -> Remote {
        Remote::Proton(Proton::new(&self.fake.config(), &self.data))
    }

    fn staged(&self) -> usize {
        std::fs::read_dir(self.data.join("proton-staging")).map_or(0, |d| d.count())
    }
}

fn content(len: usize, seed: u32) -> Vec<u8> {
    (0..len as u32).map(|i| (i.wrapping_mul(31).wrapping_add(seed) % 251) as u8).collect()
}

async fn send(r: &Remote, session: &str, data: &[u8], mut offset: u64, chunk: usize) -> UploadStatus {
    loop {
        let end = (offset as usize + chunk).min(data.len());
        let piece = bytes::Bytes::copy_from_slice(&data[offset as usize..end]);
        match r.put_chunk(session, piece, offset, data.len() as u64).await.unwrap() {
            UploadStatus::Next(n) => offset = n,
            done => return done,
        }
    }
}

async fn upload(r: &Remote, existing: Option<&str>, name: &str, parent: &str, data: &[u8]) -> RemoteMeta {
    let session = r.start_upload(existing, name, (parent, None), data.len() as u64, "").await.unwrap();
    match send(r, &session, data, 0, 256 * 1024).await {
        UploadStatus::Done(m) => m,
        other => panic!("upload didn't finish: {other:?}"),
    }
}

async fn download(r: &Remote, id: &str, dir: &Path) -> Vec<u8> {
    let path = dir.join(format!("dl-{}", id.len()));
    let mut f = tokio::fs::File::create(&path).await.unwrap();
    let (hash, n) = r.download(id, &mut f).await.unwrap();
    drop(f);
    let data = std::fs::read(&path).unwrap();
    assert_eq!(n as usize, data.len());
    assert_eq!(hash.value, hex::encode(Sha256::digest(&data)), "SHA-256 of what was written");
    data
}

#[tokio::test]
async fn folders_uploads_and_stable_ids() {
    let s = Setup::new();
    let r = s.client();
    let root = r.top_parent().await.unwrap();
    assert!(root.contains('~'), "a node uid: {root}");
    assert!(r.find_child("CloudDrop", &root, true).await.unwrap().is_empty());
    let top = r.create_folder("CloudDrop", &root).await.unwrap();
    assert!(top.folder);
    assert_eq!(top.parents, vec![root.clone()]);
    assert_eq!(r.find_child("CloudDrop", &root, true).await.unwrap(), vec![top.clone()]);
    assert!(r.find_child("CloudDrop", &root, false).await.unwrap().is_empty(), "not a file");

    // Names that look like options or hold path characters.
    let docs = r.create_folder("-docs", &top.id).await.unwrap();
    let data = content(700_000, 1);
    let name = if cfg!(windows) { "a b.txt" } else { "a \\ b?.txt" };
    let f = upload(&r, None, name, &docs.id, &data).await;
    assert_eq!((f.name.as_str(), f.size, f.folder, f.hash.clone()), (name, Some(700_000), false, None));
    assert_eq!(f.parents, vec![docs.id.clone()]);
    assert_eq!(s.staged(), 0, "staging is removed once uploaded");
    assert_eq!(r.get(&f.id).await.unwrap(), f);

    // Renaming and moving keep the id.
    let other = r.create_folder("other", &top.id).await.unwrap();
    let moved = r.move_rename(&f.id, Some("--b.txt"), Some((&other.id, &docs.id))).await.unwrap();
    assert_eq!((moved.id.as_str(), moved.name.as_str()), (f.id.as_str(), "--b.txt"));
    assert_eq!(moved.parents, vec![other.id.clone()]);
    let renamed = r.move_rename(&docs.id, Some("Docs"), None).await.unwrap();
    assert_eq!((renamed.id, renamed.name), (docs.id.clone(), "Docs".to_string()));
    assert_eq!(download(&r, &f.id, s.data.as_path()).await, data);

    // An empty file.
    let empty = upload(&r, None, "empty", &top.id, &[]).await;
    assert_eq!(empty.size, Some(0));
    assert!(download(&r, &empty.id, s.data.as_path()).await.is_empty());

    assert!(s.fake.overlaps().is_empty(), "one command at a time");
    let calls = s.fake.calls();
    assert!(calls.iter().all(|c| c.starts_with("filesystem ") && c.contains(" --json ") && c.contains(" -- ")), "{calls:?}");
}

#[tokio::test]
async fn upload_resumes_from_what_is_staged() {
    let s = Setup::new();
    let data = content(3 * 1024 * 1024 + 7, 2);
    let (session, parent) = {
        let r = s.client();
        let parent = r.top_parent().await.unwrap();
        let session = r.start_upload(None, "big.bin", (&parent, None), data.len() as u64, "").await.unwrap();
        let first = bytes::Bytes::copy_from_slice(&data[..1024 * 1024]);
        assert_eq!(r.put_chunk(&session, first, 0, data.len() as u64).await.unwrap(), UploadStatus::Next(1 << 20));
        (session, parent)
    };
    // CloudDrop restarts: a new client picks the session up where the staging stopped.
    let r = s.client();
    assert_eq!(r.query_upload(&session, data.len() as u64).await.unwrap(), UploadStatus::Next(1 << 20));
    // A chunk sent again from an earlier offset replaces what follows it.
    let again = bytes::Bytes::copy_from_slice(&data[512 * 1024..1024 * 1024]);
    assert_eq!(r.put_chunk(&session, again, 512 * 1024, data.len() as u64).await.unwrap(), UploadStatus::Next(1 << 20));
    // A chunk past the end asks for what's missing.
    let ahead = bytes::Bytes::copy_from_slice(&data[2 << 20..3 << 20]);
    assert_eq!(r.put_chunk(&session, ahead, 2 << 20, data.len() as u64).await.unwrap(), UploadStatus::Next(1 << 20));
    let UploadStatus::Done(m) = send(&r, &session, &data, 1 << 20, 1 << 20).await else { panic!("not done") };
    assert_eq!((m.name.as_str(), m.size, m.parents.clone()), ("big.bin", Some(data.len() as u64), vec![parent]));
    assert_eq!(download(&r, &m.id, s.data.as_path()).await, data);
    // A finished session is gone: the engine starts a new one.
    assert_eq!(r.query_upload(&session, data.len() as u64).await, Err(RemoteError::NotFound));
    assert_eq!(r.query_upload("not a session", 1).await, Err(RemoteError::NotFound));
    assert_eq!(r.query_upload(r#"{"dir":"../x","parent":"p","name":"n","size":1}"#, 1).await, Err(RemoteError::NotFound));
}

#[tokio::test]
async fn keep_version_then_new_revision() {
    let s = Setup::new();
    let r = s.client();
    let root = r.top_parent().await.unwrap();
    let old = content(1000, 3);
    let f = upload(&r, None, "report.pdf", &root, &old).await;
    r.keep_version(&f.id).await.unwrap();
    r.keep_version(&f.id).await.unwrap();
    let new = content(2000, 4);
    // New content for the file keeps its id (a new revision), even if the name passed differs.
    let g = upload(&r, Some(&f.id), "ignored.pdf", &root, &new).await;
    assert_eq!((g.id.as_str(), g.name.as_str(), g.size), (f.id.as_str(), "report.pdf", Some(2000)));
    assert_eq!(download(&r, &f.id, s.data.as_path()).await, new);
    assert_eq!(r.find_child("report.pdf", &root, false).await.unwrap(), vec![g]);
    let copies: Vec<String> = s
        .fake
        .calls()
        .iter()
        .filter(|c| c.starts_with("filesystem copy"))
        .map(|c| c.split("--name=").nth(1).unwrap().split(" -- ").next().unwrap().to_string())
        .collect();
    assert_eq!(copies.len(), 2);
    assert!(copies[0].starts_with("report (replaced 20") && copies[0].ends_with(").pdf"), "{copies:?}");
    assert_eq!(copies[1], copies[0].replace(").pdf", ") (2).pdf"));
    let first = r.find_child(&copies[0], &root, false).await.unwrap();
    assert_eq!(download(&r, &first[0].id, s.data.as_path()).await, old);
}

#[tokio::test]
async fn ranges_come_from_one_download() {
    let s = Setup::new();
    let r = s.client();
    let root = r.top_parent().await.unwrap();
    let data = content(100_000, 5);
    let f = upload(&r, None, "video.mp4", &root, &data).await;
    let before = s.fake.calls().len();
    assert_eq!(r.download_range(&f.id, 0, 10).await.unwrap()[..], data[..10]);
    assert_eq!(r.download_range(&f.id, 99_990, 200_000).await.unwrap()[..], data[99_990..]);
    assert!(r.download_range(&f.id, 5, 5).await.unwrap().is_empty());
    let downloads = s.fake.calls()[before..].iter().filter(|c| c.starts_with("filesystem download")).count();
    assert_eq!(downloads, 1, "the second range reads the kept copy");
    // New content makes the kept copy stale.
    let newer = content(50, 6);
    upload(&r, Some(&f.id), "video.mp4", &root, &newer).await;
    assert_eq!(r.download_range(&f.id, 0, 50).await.unwrap()[..], newer[..]);
    drop(r);
    let left = std::fs::read_dir(s.data.join("proton-downloads")).map_or(0, |d| d.count());
    assert_eq!(left, 0, "downloads are cleaned up");
}

#[tokio::test]
async fn errors_from_the_cli() {
    let s = Setup::new();
    let r = s.client();
    let root = r.top_parent().await.unwrap();
    let missing = format!("{}~{:0>22}", "V".repeat(22), 999);
    assert_eq!(r.get(&missing).await, Err(RemoteError::NotFound));
    assert_eq!(r.get("a/b").await, Err(RemoteError::NotFound));
    let dup = r.create_folder("x", &root).await.unwrap();
    assert!(matches!(r.create_folder("x", &root).await, Err(RemoteError::Permanent(_))));
    let y = r.create_folder("y", &root).await.unwrap();
    let inner = r.create_folder("x", &y.id).await.unwrap();
    assert!(matches!(r.move_rename(&inner.id, None, Some((&root, &y.id))).await, Err(RemoteError::Permanent(m)) if m.contains("move")));
    assert_eq!(r.get(&dup.id).await.unwrap().parents, vec![root.clone()]);

    s.fake.full(true);
    let session = r.start_upload(None, "f", (&root, None), 3, "").await.unwrap();
    let full = r.put_chunk(&session, bytes::Bytes::from_static(b"abc"), 0, 3).await;
    assert!(matches!(full, Err(RemoteError::Quota(_))), "{full:?}");
    s.fake.full(false);
    assert!(matches!(r.query_upload(&session, 3).await, Ok(UploadStatus::Done(m)) if m.size == Some(3)), "retried from the staging");

    s.fake.signed_in(false);
    let auth = r.get(&root).await;
    assert!(matches!(&auth, Err(RemoteError::Auth(m)) if m.contains("auth login")), "{auth:?}");
    s.fake.signed_in(true);

    let nowhere = Remote::Proton(Proton::new(&ProtonConfig { cli: s.data.join("no-such-cli").to_string_lossy().into() }, &s.data));
    let e = nowhere.top_parent().await;
    assert!(matches!(&e, Err(RemoteError::Permanent(m)) if m.contains("proton.me/download/drive/cli")), "{e:?}");
}
