//! The MEGA client against the fake MEGA in `common::mega_mock`: sign-in, a resumed upload,
//! downloads (whole and ranges), keep_version and replacing, moves, errors, and the engine
//! uploading a folder.

mod common;

use clouddrop::config::Config;
use clouddrop::db::Db;
use clouddrop::mega::MegaConfig;
use clouddrop::remote::{HashKind, Provider, Remote, RemoteError, RemoteMeta, UploadStatus};
use common::mega_mock::{self, EMAIL, PASSWORD, Shared};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIB: usize = 1 << 20;

struct Setup {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    sync: PathBuf,
    mock: Shared,
}

impl Setup {
    async fn new() -> Self {
        let mock = mega_mock::start_mega_mock().await;
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let sync = tmp.path().join("CloudDrop");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&sync).unwrap();
        let s = Setup { _tmp: tmp, data, sync, mock };
        s.sign_in(PASSWORD, None);
        s
    }

    fn creds_file(&self) -> PathBuf {
        self.data.join("credentials.json")
    }

    /// What `clouddrop setup --provider mega` saves: the password and code, as secrets.
    fn sign_in(&self, password: &str, mfa: Option<&str>) {
        let mut m = Map::new();
        m.insert("password".into(), json!(password));
        if let Some(code) = mfa {
            m.insert("mfa".into(), json!(code));
        }
        clouddrop::credentials::save_to(&self.creds_file(), "mega", m).unwrap();
    }

    fn creds(&self) -> Map<String, Value> {
        clouddrop::credentials::load_from(&self.creds_file(), "mega").unwrap().unwrap_or_default()
    }

    fn cfg(&self) -> Config {
        Config {
            provider: Provider::Mega,
            mega: MegaConfig { email: EMAIL.to_uppercase(), api_url: self.mock.lock().unwrap().base.clone() },
            sync_dir: self.sync.clone(),
            settle_secs: 1,
            notifications: false,
            chunk_size: MIB,
            ..Config::default()
        }
    }

    /// A client as the engine gets one (a fresh one is a fresh process).
    fn remote(&self) -> Arc<Remote> {
        clouddrop::remote_for(&self.cfg(), self.data.join("token.json"))
    }
}

fn content(len: usize, seed: u32) -> Vec<u8> {
    (0..len as u32).map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(seed) >> 13) as u8).collect()
}

/// Sends `data` from `offset` the way the engine does, `chunk` bytes at a time; stops after
/// `puts` chunks if given.
async fn send(r: &Remote, session: &str, data: &[u8], mut offset: u64, chunk: usize, puts: Option<usize>) -> UploadStatus {
    let total = data.len() as u64;
    let mut n = 0;
    loop {
        let want = (data.len() - offset as usize).min(chunk);
        let piece = bytes::Bytes::copy_from_slice(&data[offset as usize..offset as usize + want]);
        match r.put_chunk(session, piece, offset, total).await.unwrap() {
            UploadStatus::Done(m) => return UploadStatus::Done(m),
            UploadStatus::Next(next) => {
                assert!(next > offset, "the upload progresses");
                offset = next;
                n += 1;
                if puts == Some(n) {
                    return UploadStatus::Next(offset);
                }
            }
        }
    }
}

async fn upload(r: &Remote, existing: Option<&str>, name: &str, parent: &str, data: &[u8]) -> RemoteMeta {
    let session = r.start_upload(existing, name, (parent, Some(parent)), data.len() as u64, "").await.unwrap();
    match send(r, &session, data, 0, 2 * MIB, None).await {
        UploadStatus::Done(m) => m,
        other => panic!("upload not done: {other:?}"),
    }
}

async fn download(r: &Remote, id: &str, dir: &Path) -> Vec<u8> {
    let path = dir.join(format!("dl-{id}"));
    let mut f = tokio::fs::File::create(&path).await.unwrap();
    let (hash, n) = r.download(id, &mut f).await.unwrap();
    let got = std::fs::read(&path).unwrap();
    assert_eq!(n, got.len() as u64);
    assert_eq!(hash.kind, HashKind::Sha256);
    assert_eq!(hash.value, hex::encode(Sha256::digest(&got)));
    got
}

#[tokio::test]
async fn signs_in_once_and_keeps_only_the_session() {
    let s = Setup::new().await;
    s.mock.lock().unwrap().hashcash = Some(220);
    let r = s.remote();
    let (limit, used, in_drive) = r.storage_quota().await.unwrap();
    assert_eq!(limit, Some(20 << 30));
    assert!(used >= in_drive);
    let creds = s.creds();
    let mut keys: Vec<&str> = creds.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["email", "master_key", "sid", "user"], "the password and code are gone");
    {
        let m = s.mock.lock().unwrap();
        assert_eq!((m.logins, m.hashcash_solved), (1, 1));
    }

    // Another process reuses the session.
    let r2 = s.remote();
    assert_eq!(r2.top_parent().await.unwrap(), "ROOT0001");
    assert_eq!(s.mock.lock().unwrap().logins, 1);

    // Sessions ended on MEGA's side: sign-in needed.
    s.mock.lock().unwrap().kill_sessions();
    assert!(matches!(r2.storage_quota().await, Err(RemoteError::Auth(_))));
}

#[tokio::test]
async fn wrong_password_and_two_factor_codes() {
    let s = Setup::new().await;
    s.sign_in("not the password", None);
    let r = s.remote();
    assert!(matches!(r.top_parent().await, Err(RemoteError::Auth(_))));
    assert!(s.creds().is_empty(), "a rejected password is not kept");
    assert!(matches!(r.top_parent().await, Err(RemoteError::Auth(_))), "and nothing to sign in with");

    s.mock.lock().unwrap().mfa = Some("123456".into());
    s.sign_in(PASSWORD, None);
    match s.remote().top_parent().await {
        Err(RemoteError::Auth(m)) => assert!(m.contains("two-factor"), "{m}"),
        other => panic!("{other:?}"),
    }
    s.sign_in(PASSWORD, Some("000000"));
    assert!(matches!(s.remote().top_parent().await, Err(RemoteError::Auth(_))));
    s.sign_in(PASSWORD, Some(" 123456 "));
    assert_eq!(s.remote().top_parent().await.unwrap(), "ROOT0001");
    assert!(!s.creds().contains_key("mfa"));
}

#[tokio::test]
async fn upload_resumes_after_a_crash_and_reads_back() {
    let s = Setup::new().await;
    let r = s.remote();
    let root = r.top_parent().await.unwrap();
    let folder = r.create_folder("CloudDrop", &root).await.unwrap();
    assert!(folder.folder);
    assert_eq!(r.find_child("CloudDrop", &root, true).await.unwrap().len(), 1);

    let data = content(6 * MIB + 12_345, 1);
    let total = data.len() as u64;
    let session = r.start_upload(None, "big.bin", (&folder.id, None), total, "application/octet-stream").await.unwrap();
    // Two 2 MiB chunks, each sent up to its last MEGA chunk boundary (1.875 MiB, then 3.5 MiB).
    let UploadStatus::Next(at) = send(&r, &session, &data, 0, 2 * MIB, Some(2)).await else { panic!() };
    assert_eq!(at, 3584 * 1024);
    drop(r);

    // A new process picks the same session up where the server left it.
    let r = s.remote();
    assert_eq!(r.query_upload(&session, total).await.unwrap(), UploadStatus::Next(at));
    let posts_before = s.mock.lock().unwrap().chunk_posts;
    let UploadStatus::Done(m) = send(&r, &session, &data, at, 2 * MIB, None).await else { panic!() };
    assert_eq!(s.mock.lock().unwrap().chunk_posts - posts_before, 2, "only the rest was sent");
    assert_eq!((m.name.as_str(), m.size, m.parents.clone()), ("big.bin", Some(total), vec![folder.id.clone()]));
    assert!(m.hash.is_none() && !m.trashed);
    // A finished session leaves no progress file behind.
    assert_eq!(r.query_upload(&session, total).await.unwrap(), UploadStatus::Next(0));
    {
        let mock = s.mock.lock().unwrap();
        assert_eq!(mock.content(&m.id), data);
        assert!(mock.mac_ok(&m.id), "the MAC in the file's key matches its content");
        assert_eq!(mock.name(&m.id), "big.bin");
        assert_eq!(mock.children(&folder.id).len(), 1);
    }

    assert_eq!(r.get(&m.id).await.unwrap(), m);
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(download(&r, &m.id, tmp.path()).await, data);
    for (a, z) in [(0, 10), (5, 100_000), (1_234_567, 3_000_001), (total - 7, total + 100), (17, 17)] {
        let got = r.download_range(&m.id, a, z).await.unwrap();
        assert_eq!(&got[..], &data[a as usize..z.min(total) as usize], "range {a}..{z}");
    }

    // Empty files work too.
    let empty = upload(&r, None, "empty.txt", &folder.id, b"").await;
    assert_eq!(empty.size, Some(0));
    assert!(download(&r, &empty.id, tmp.path()).await.is_empty());
}

#[tokio::test]
async fn keep_version_then_replace() {
    let s = Setup::new().await;
    let r = s.remote();
    let root = r.top_parent().await.unwrap();
    let folder = r.create_folder("CloudDrop", &root).await.unwrap().id;
    let v1 = upload(&r, None, "notes.txt", &folder, b"first draft").await;

    r.keep_version(&v1.id).await.unwrap();
    r.keep_version(&v1.id).await.unwrap();
    let kept: Vec<RemoteMeta> = {
        let mock = s.mock.lock().unwrap();
        mock.children(&folder)
            .into_iter()
            .filter(|n| mock.name(&n.h).starts_with("notes (replaced 20") && mock.name(&n.h).ends_with(").txt"))
            .map(|n| RemoteMeta { id: n.h, ..Default::default() })
            .collect()
    };
    assert_eq!(kept.len(), 1, "kept once, even when asked twice");
    assert_eq!(s.mock.lock().unwrap().content(&kept[0].id), b"first draft");

    let v2 = upload(&r, Some(&v1.id), "notes.txt", &folder, b"second draft, longer").await;
    assert_ne!(v2.id, v1.id, "new content is a new node");
    let found = r.find_child("notes.txt", &folder, false).await.unwrap();
    assert_eq!(found.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), std::slice::from_ref(&v2.id));
    {
        let mock = s.mock.lock().unwrap();
        assert_eq!(mock.content(&v2.id), b"second draft, longer");
        assert_eq!(mock.nodes[&v1.id].p, v2.id, "the old content is the new file's previous version");
        assert_eq!(mock.content(&v1.id), b"first draft");
    }
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(download(&r, &kept[0].id, tmp.path()).await, b"first draft");

    // Replacing a file that has meanwhile gone: start_upload says so.
    assert_eq!(
        r.start_upload(Some("GONE0000"), "x", (&folder, None), 1, "").await,
        Err(RemoteError::NotFound)
    );
}

#[tokio::test]
async fn moves_renames_trash_and_errors() {
    let s = Setup::new().await;
    let r = s.remote();
    let root = r.top_parent().await.unwrap();
    let a = r.create_folder("A", &root).await.unwrap().id;
    let b = r.create_folder("B", &root).await.unwrap().id;
    let f = upload(&r, None, "f.txt", &a, b"hello").await;

    let moved = r.move_rename(&f.id, Some("g.txt"), Some((&b, &a))).await.unwrap();
    assert_eq!((moved.id.as_str(), moved.name.as_str(), moved.parents.clone()), (f.id.as_str(), "g.txt", vec![b.clone()]));
    {
        let mock = s.mock.lock().unwrap();
        assert_eq!((mock.name(&f.id), mock.nodes[&f.id].p.clone()), ("g.txt".to_string(), b.clone()));
    }
    let renamed = r.move_rename(&a, Some("A2"), None).await.unwrap();
    assert_eq!(renamed.name, "A2");

    // Missing parents.
    assert_eq!(r.create_folder("x", "NOPE0000").await, Err(RemoteError::NotFound));
    assert_eq!(r.start_upload(None, "x", ("NOPE0000", None), 1, "").await, Err(RemoteError::NotFound));
    assert!(r.find_child("x", "NOPE0000", false).await.unwrap().is_empty());
    assert_eq!(r.get("NOPE0000").await, Err(RemoteError::NotFound));

    // Moved to the rubbish bin on the web: a client with a fresh tree sees it as trashed.
    s.mock.lock().unwrap().trash(&b);
    let r2 = s.remote();
    assert!(r2.get(&f.id).await.unwrap().trashed);
    assert!(r2.find_child("g.txt", &b, false).await.unwrap().is_empty());
    assert_eq!(r2.create_folder("x", &b).await, Err(RemoteError::NotFound));

    // MEGA busy once: retried. Busy for longer: rate limited.
    s.mock.lock().unwrap().fail.insert("uq".into(), [-3].into());
    r.storage_quota().await.unwrap();
    s.mock.lock().unwrap().fail.insert("uq".into(), [-3, -3, -3].into());
    assert!(matches!(r.storage_quota().await, Err(RemoteError::RateLimited { .. })));
    s.mock.lock().unwrap().fail.insert("uq".into(), [-4].into());
    assert!(matches!(r.storage_quota().await, Err(RemoteError::RateLimited { .. })));

    // Storage full.
    s.mock.lock().unwrap().limit = 10;
    assert!(matches!(r.start_upload(None, "big", (&root, None), 100, "").await, Err(RemoteError::Quota(_))));
    s.mock.lock().unwrap().limit = 1 << 30;

    // An upload server that can't go on: start the upload again (NotFound).
    let data = content(3 * MIB, 2);
    let session = r.start_upload(None, "c.bin", (&root, None), data.len() as u64, "").await.unwrap();
    let next_post = s.mock.lock().unwrap().chunk_posts + 1;
    s.mock.lock().unwrap().fail_chunk.insert(next_post, "-4".into());
    let first = bytes::Bytes::copy_from_slice(&data[..2 * MIB]);
    assert_eq!(r.put_chunk(&session, first, 0, data.len() as u64).await, Err(RemoteError::NotFound));
    // Chunks smaller than MEGA's are refused clearly rather than stalling.
    let small = bytes::Bytes::copy_from_slice(&data[..64 * 1024]);
    assert!(matches!(r.put_chunk(&session, small, 0, data.len() as u64).await, Err(RemoteError::Permanent(_))));
}

#[tokio::test]
async fn engine_uploads_a_folder_into_mega() {
    let s = Setup::new().await;
    let write = |rel: &str, data: &[u8]| {
        let p = s.sync.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    };
    let big = content(2 * MIB + 777, 3);
    write("notes.txt", b"hello MEGA");
    write("docs/report.txt", b"quarterly numbers");
    write("big.bin", &big);

    let cfg = s.cfg();
    let db = Arc::new(Db::open(&s.data.join("state.db")).unwrap());
    let (tx, rx) = tokio::sync::watch::channel(false);
    let opts = clouddrop::engine::RunOptions { config_file: None, store: Arc::new(clouddrop::store::PlainStore) };
    let engine = tokio::spawn(clouddrop::engine::run_with_options(cfg, opts, db, s.remote(), s.data.clone(), rx));

    let want = vec![
        ("big.bin".to_string(), String::from_utf8_lossy(&big).to_string()),
        ("docs/".to_string(), String::new()),
        ("docs/report.txt".to_string(), "quarterly numbers".to_string()),
        ("notes.txt".to_string(), "hello MEGA".to_string()),
    ];
    let start = Instant::now();
    loop {
        let tree = {
            let m = s.mock.lock().unwrap();
            let mut out = vec![];
            if let Some(top) = m.lookup("CloudDrop") {
                m.tree(&top.h, "", &mut out);
            }
            out
        };
        if tree == want {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "timed out; cloud: {:?}", tree.iter().map(|t| &t.0).collect::<Vec<_>>());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    {
        let m = s.mock.lock().unwrap();
        let top = m.lookup("CloudDrop").unwrap();
        for n in m.children(&top.h).iter().filter(|n| n.t == 0) {
            assert!(m.mac_ok(&n.h));
        }
    }
    tx.send(true).unwrap();
    engine.await.unwrap().unwrap();
}
