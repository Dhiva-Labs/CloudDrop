//! The WebDAV / Nextcloud client against the fake server, in plain WebDAV and Nextcloud mode.

mod common;

use bytes::Bytes;
use clouddrop::hash::Hasher;
use clouddrop::remote::{HashKind, Provider, RemoteError, RemoteMeta, UploadStatus};
use clouddrop::webdav::{self, ServerInfo, WebDav, WebDavConfig};
use common::webdav_mock::{self as mock, Fail, Shared, sha1_hex};
use std::time::Duration;

const MIB: usize = 1 << 20;

fn client(m: &Shared, folder: &str) -> WebDav {
    let mut c = client_with(m, folder, Some(mock::PASSWORD));
    // The fake server takes chunks of any size.
    c.set_min_chunk(1);
    c
}

fn client_with(m: &Shared, folder: &str, password: Option<&str>) -> WebDav {
    let st = m.lock().unwrap();
    let provider = if st.nextcloud { Provider::Nextcloud } else { Provider::WebDav };
    let cfg = WebDavConfig { url: st.url(), user: mock::USER.into(), folder: folder.into(), server: None };
    WebDav::new(webdav::http_client(), provider, cfg, password.map(str::to_string))
}

async fn both() -> [Shared; 2] {
    [mock::start(false).await, mock::start(true).await]
}

fn mode(m: &Shared) -> &'static str {
    if m.lock().unwrap().nextcloud { "nextcloud" } else { "plain" }
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

fn sha256(data: &[u8]) -> String {
    let mut h = Hasher::new(HashKind::Sha256);
    h.update(data);
    h.finish().value
}

/// Uploads the way the engine does: a session, then chunks until the server says done.
async fn upload(c: &WebDav, existing: Option<&str>, parent: &str, name: &str, content: &[u8], chunk: usize) -> RemoteMeta {
    let s = c.start_upload(existing, name, (parent, None), content.len() as u64, "text/plain").await.unwrap();
    send(c, &s, content, 0, chunk).await.unwrap()
}

async fn send(c: &WebDav, session: &str, content: &[u8], mut off: u64, chunk: usize) -> Result<RemoteMeta, RemoteError> {
    let total = content.len() as u64;
    loop {
        let end = (off as usize + chunk).min(content.len());
        let piece = Bytes::copy_from_slice(&content[off as usize..end]);
        match c.put_chunk(session, piece, off, total).await? {
            UploadStatus::Done(m) => return Ok(m),
            UploadStatus::Next(n) => off = n,
        }
    }
}

async fn download(c: &WebDav, id: &str) -> Result<(Vec<u8>, String), RemoteError> {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("download");
    let mut f = tokio::fs::File::create(&p).await.unwrap();
    let (hash, n) = c.download(id, &mut f).await?;
    drop(f);
    let got = std::fs::read(&p).unwrap();
    assert_eq!(n as usize, got.len());
    assert_eq!(hash.kind, HashKind::Sha256);
    Ok((got, hash.value))
}

/// Names in `folder` (a mock path) that start with `prefix`.
fn names(m: &Shared, folder: &str, prefix: &str) -> Vec<String> {
    let st = m.lock().unwrap();
    st.nodes
        .keys()
        .filter_map(|p| p.strip_prefix(&format!("{folder}/")))
        .filter(|n| !n.contains('/') && n.starts_with(prefix))
        .map(str::to_string)
        .collect()
}

fn no_deletes(m: &Shared) {
    assert_eq!(m.lock().unwrap().count("DELETE", ""), 0, "CloudDrop never deletes in the cloud");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_and_folders_round_trip() {
    for m in both().await {
        let c = client(&m, "Backups/Laptop");
        let top = c.top_parent().await.unwrap();
        assert_eq!(top, "/Backups/Laptop", "{}", mode(&m));
        assert!(m.lock().unwrap().is_folder("/Backups/Laptop"), "the configured folder is made");
        let root = c.create_folder("CloudDrop", &top).await.unwrap();
        assert_eq!((root.id.as_str(), root.folder), ("/Backups/Laptop/CloudDrop", true));
        assert_eq!(c.create_folder("CloudDrop", &top).await.unwrap().id, root.id, "already there is fine");
        assert_eq!(c.find_child("CloudDrop", &top, true).await.unwrap().len(), 1);

        let meta = upload(&c, None, &root.id, "a.txt", b"hello world", MIB).await;
        assert_eq!(meta.id, "/Backups/Laptop/CloudDrop/a.txt");
        assert_eq!((meta.size, meta.hash.as_ref(), meta.folder), (Some(11), None, false));
        assert_eq!(meta.parents, [root.id.clone()]);
        assert_eq!(c.get(&meta.id).await.unwrap(), meta);
        assert_eq!(c.find_child("a.txt", &root.id, false).await.unwrap(), [meta.clone()]);
        assert!(c.find_child("a.txt", &root.id, true).await.unwrap().is_empty(), "not a folder");
        assert!(c.find_child("b.txt", &root.id, false).await.unwrap().is_empty());
        assert_eq!(c.get("/Backups/Laptop/CloudDrop/b.txt").await, Err(RemoteError::NotFound));

        let (got, hash) = download(&c, &meta.id).await.unwrap();
        assert_eq!((got.as_slice(), hash), (&b"hello world"[..], sha256(b"hello world")));
        assert_eq!(&c.download_range(&meta.id, 2, 5).await.unwrap()[..], b"llo");
        assert!(c.download_range(&meta.id, 5, 5).await.unwrap().is_empty());
        assert_eq!(m.lock().unwrap().file("/Backups/Laptop/CloudDrop/a.txt").unwrap(), b"hello world");

        // Replacing content keeps the file where it is.
        let again = upload(&c, Some(&meta.id), &root.id, "a.txt", b"new", MIB).await;
        assert_eq!((again.id.as_str(), again.size), (meta.id.as_str(), Some(3)));
        no_deletes(&m);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn top_level_works_as_the_folder() {
    for m in both().await {
        let c = client(&m, "");
        assert_eq!(c.top_parent().await.unwrap(), "/");
        let root = c.create_folder("CloudDrop", "/").await.unwrap();
        assert_eq!((root.id.as_str(), root.parents.as_slice()), ("/CloudDrop", &["/".to_string()][..]));
        let top = c.get("/").await.unwrap();
        assert!(top.folder && top.parents.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nextcloud_is_detected_and_recorded() {
    let m = mock::start(true).await;
    let info = client(&m, "").probe().await.unwrap();
    assert_eq!(
        info,
        ServerInfo { product: "Nextcloud".into(), version: "29.0.4".into(), user_id: mock::USER_ID.into(), chunking: true }
    );
    // With the result saved, a start doesn't look again; files are under the account's id.
    let st = m.lock().unwrap().url();
    let cfg = WebDavConfig { url: st, user: mock::USER.into(), folder: "F".into(), server: Some(info) };
    let c = WebDav::new(webdav::http_client(), Provider::Nextcloud, cfg, Some(mock::PASSWORD.into()));
    let before = m.lock().unwrap().count("GET", "status.php");
    c.top_parent().await.unwrap();
    assert_eq!(m.lock().unwrap().count("GET", "status.php"), before);
    assert!(m.lock().unwrap().is_folder("/F"));

    // A plain server is plain WebDAV at the address given.
    let plain = mock::start(false).await;
    assert_eq!(client(&plain, "").probe().await.unwrap(), ServerInfo::default());
    // And a wrong password shows up at once.
    assert!(matches!(client_with(&m, "", Some("wrong")).probe().await, Err(RemoteError::Auth(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_rename_changes_the_ids_beneath_it() {
    for m in both().await {
        let c = client(&m, "");
        let a = c.create_folder("A", "/").await.unwrap();
        let b = c.create_folder("B", &a.id).await.unwrap();
        let x = upload(&c, None, &b.id, "x.txt", b"x", MIB).await;
        assert_eq!(x.id, "/A/B/x.txt");

        let renamed = c.move_rename(&a.id, Some("Renamed"), None).await.unwrap();
        assert_eq!((renamed.id.as_str(), renamed.name.as_str(), renamed.folder), ("/Renamed", "Renamed", true));
        assert_eq!(c.get("/A/B/x.txt").await, Err(RemoteError::NotFound), "{}", mode(&m));
        let moved = c.get("/Renamed/B/x.txt").await.unwrap();
        assert_eq!((moved.size, moved.parents.as_slice()), (Some(1), &["/Renamed/B".to_string()][..]));

        // Moving B up to the top: (new parent, old parent).
        let b2 = c.move_rename("/Renamed/B", None, Some(("/", "/Renamed"))).await.unwrap();
        assert_eq!((b2.id.as_str(), b2.parents.as_slice()), ("/B", &["/".to_string()][..]));
        assert_eq!(c.get("/B/x.txt").await.unwrap().size, Some(1));
        assert_eq!(c.move_rename("/B/x.txt", None, None).await.unwrap().id, "/B/x.txt", "nothing to do");

        // A folder never lands on another one.
        c.create_folder("C", "/").await.unwrap();
        let err = c.move_rename("/B", Some("C"), None).await.unwrap_err();
        assert!(matches!(err, RemoteError::Permanent(_)), "{err:?}");
        assert!(m.lock().unwrap().file("/B/x.txt").is_some());

        // Moving where the folder is gone, or from where the item is gone.
        assert_eq!(c.move_rename("/B/x.txt", None, Some(("/Nope", "/B"))).await, Err(RemoteError::NotFound));
        assert_eq!(c.move_rename("/Gone", Some("G"), None).await, Err(RemoteError::NotFound));
        no_deletes(&m);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_onto_an_unknown_file_keeps_that_file() {
    for m in both().await {
        let c = client(&m, "");
        m.lock().unwrap().seed("/R/b.txt", b"theirs");
        let a = upload(&c, None, "/R", "a.txt", b"mine", MIB).await;
        let b = c.move_rename(&a.id, Some("b.txt"), None).await.unwrap();
        assert_eq!(b.id, "/R/b.txt");
        {
            let st = m.lock().unwrap();
            assert_eq!(st.file("/R/b.txt").unwrap(), b"mine");
        }
        let aside = names(&m, "/R", "b (replaced ");
        assert_eq!(aside.len(), 1, "{}: {aside:?}", mode(&m));
        assert_eq!(m.lock().unwrap().file(&format!("/R/{}", aside[0])).unwrap(), b"theirs");
        no_deletes(&m);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn odd_names_survive() {
    let odd = [
        "a b.txt",
        "100% #1?.txt",
        "a%20b.txt",
        "über naïve 日本 ☃.txt",
        "R&D <draft> \"q\" 'x'.txt",
        "semi;colon=eq+plus,comma@at$.txt",
        "(paren) [square] {curly} ~tilde.txt",
    ];
    for m in both().await {
        let c = client(&m, "");
        let folder = c.create_folder("Ünïcode & #hash %25", "/").await.unwrap();
        assert!(m.lock().unwrap().is_folder("/Ünïcode & #hash %25"));
        for (i, name) in odd.iter().enumerate() {
            let content = format!("content {i}");
            let meta = upload(&c, None, &folder.id, name, content.as_bytes(), MIB).await;
            assert_eq!(meta.name, *name);
            let path = format!("/Ünïcode & #hash %25/{name}");
            assert_eq!(m.lock().unwrap().file(&path).unwrap(), content.as_bytes(), "{}: {name}", mode(&m));
            assert_eq!(c.find_child(name, &folder.id, false).await.unwrap().len(), 1, "{name}");
            assert_eq!(download(&c, &meta.id).await.unwrap().0, content.as_bytes());
        }
        {
            let st = m.lock().unwrap();
            assert_ne!(st.file("/Ünïcode & #hash %25/a b.txt"), st.file("/Ünïcode & #hash %25/a%20b.txt"));
        }
        // Renaming to an odd name and moving the odd folder.
        let id = format!("{}/a b.txt", folder.id);
        let r = c.move_rename(&id, Some("#1 & 2 %.txt"), None).await.unwrap();
        assert!(m.lock().unwrap().file("/Ünïcode & #hash %25/#1 & 2 %.txt").is_some(), "{r:?}");
        let moved = c.move_rename(&folder.id, Some("ok ünïcode"), None).await.unwrap();
        assert_eq!(c.find_child("#1 & 2 %.txt", &moved.id, false).await.unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nextcloud_uploads_in_chunks() {
    let m = mock::start(true).await;
    let c = client(&m, "");
    let content = data(3 * MIB + MIB / 2);
    let meta = upload(&c, None, "/", "big.bin", &content, MIB).await;
    assert_eq!((meta.id.as_str(), meta.size), ("/big.bin", Some(content.len() as u64)));
    let st = m.lock().unwrap();
    assert_eq!(st.file("/big.bin").unwrap(), content);
    assert_eq!(st.count("MKCOL", "/uploads/"), 1);
    assert_eq!(st.count("PUT", "/uploads/"), 4);
    assert_eq!(st.count("MOVE", "/.file"), 1);
    assert_eq!(st.count("PUT", "/big.bin"), 0, "no plain PUT");
    let mv = &st.headers["MOVE"];
    let sha1 = sha1_hex(&content);
    assert_eq!(mv["OC-Checksum"], format!("SHA1:{sha1}").as_str());
    assert_eq!(mv["OC-Total-Length"], content.len().to_string().as_str());
    assert!(mv["Destination"].to_str().unwrap().ends_with("/remote.php/dav/files/alice/big.bin"));
    assert!(st.headers["PUT"]["Destination"].to_str().unwrap().ends_with("/big.bin"), "chunked upload v2");
    assert_eq!(st.checksum("/big.bin").unwrap(), format!("SHA1:{sha1}"));
    assert!(st.uploads.is_empty(), "the server cleans up after assembling");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chunked_upload_continues_in_a_new_process() {
    let m = mock::start(true).await;
    let content = data(3 * MIB + 100);
    let total = content.len() as u64;
    let c = client(&m, "");
    let s = c.start_upload(None, "big.bin", ("/", None), total, "application/octet-stream").await.unwrap();
    // Asked before anything was sent (a crash right after the session was saved).
    assert_eq!(c.query_upload(&s, total).await.unwrap(), UploadStatus::Next(0));
    for i in 0..2 {
        let piece = Bytes::copy_from_slice(&content[i * MIB..(i + 1) * MIB]);
        assert_eq!(c.put_chunk(&s, piece, (i * MIB) as u64, total).await.unwrap(), UploadStatus::Next(((i + 1) * MIB) as u64));
    }
    drop(c);

    // After a restart: the server still has two chunks.
    let c2 = client(&m, "");
    assert_eq!(c2.query_upload(&s, total).await.unwrap(), UploadStatus::Next(2 * MIB as u64));
    // A chunk from the wrong place is answered with where to continue.
    let wrong = Bytes::copy_from_slice(&content[MIB..2 * MIB]);
    assert_eq!(c2.put_chunk(&s, wrong, MIB as u64, total).await.unwrap(), UploadStatus::Next(2 * MIB as u64));
    let meta = send(&c2, &s, &content, 2 * MIB as u64, MIB).await.unwrap();
    assert_eq!(meta.size, Some(total));
    assert_eq!(m.lock().unwrap().file("/big.bin").unwrap(), content);
    assert_eq!(m.lock().unwrap().count("PUT", "/uploads/"), 4);

    // Every chunk sent but the assembly failed: the next process finishes it.
    let s = c2.start_upload(Some("/big.bin"), "big.bin", ("/", None), total, "").await.unwrap();
    let other = data(2 * MIB + 7).iter().map(|b| b ^ 0x55).collect::<Vec<u8>>();
    let total = other.len() as u64;
    m.lock().unwrap().fail.push_back(Fail::new("MOVE", "/.file", 500));
    let err = send(&c2, &s, &other, 0, MIB).await.unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    let c3 = client(&m, "");
    let UploadStatus::Done(meta) = c3.query_upload(&s, total).await.unwrap() else { panic!("should be done") };
    assert_eq!((meta.id.as_str(), meta.size), ("/big.bin", Some(total)));
    assert_eq!(m.lock().unwrap().file("/big.bin").unwrap(), other);
    // A session the server no longer has starts over.
    assert_eq!(c3.query_upload(&s, total).await.unwrap(), UploadStatus::Next(0));
    assert_eq!(c3.query_upload("not a session", total).await, Err(RemoteError::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_webdav_sends_one_put_and_starts_over_when_cut() {
    let m = mock::start(false).await;
    let c = client(&m, "");
    let content = data(3 * MIB + MIB / 2);
    let total = content.len() as u64;
    let meta = upload(&c, None, "/", "big.bin", &content, MIB).await;
    assert_eq!(meta.size, Some(total));
    {
        let st = m.lock().unwrap();
        assert_eq!(st.file("/big.bin").unwrap(), content);
        assert_eq!(st.count("PUT", "/big.bin"), 1, "one PUT with a known length");
        assert_eq!(st.headers["PUT"][axum::http::header::CONTENT_LENGTH], total.to_string().as_str());
    }

    // Cut off half-way: the server keeps nothing of it, and the upload starts over.
    let s = c.start_upload(Some("/big.bin"), "big.bin", ("/", None), total, "").await.unwrap();
    let first = Bytes::copy_from_slice(&content[..MIB]);
    assert_eq!(c.put_chunk(&s, first, 0, total).await.unwrap(), UploadStatus::Next(MIB as u64));
    assert_eq!(c.query_upload(&s, total).await.unwrap(), UploadStatus::Next(0));
    let rest = Bytes::copy_from_slice(&content[MIB..2 * MIB]);
    assert_eq!(c.put_chunk(&s, rest, MIB as u64, total).await.unwrap(), UploadStatus::Next(0), "a PUT can't continue");
    let changed: Vec<u8> = content.iter().map(|b| b.wrapping_add(1)).collect();
    send(&c, &s, &changed, 0, MIB).await.unwrap();
    assert_eq!(m.lock().unwrap().file("/big.bin").unwrap(), changed);

    // Another process can't continue a PUT either.
    let s = c.start_upload(Some("/big.bin"), "big.bin", ("/", None), total, "").await.unwrap();
    let piece = Bytes::copy_from_slice(&content[MIB..2 * MIB]);
    assert_eq!(client(&m, "").put_chunk(&s, piece, MIB as u64, total).await.unwrap(), UploadStatus::Next(0));
    no_deletes(&m);
}

#[tokio::test(flavor = "multi_thread")]
async fn small_chunks_and_whole_files_go_as_one_put_on_nextcloud() {
    let m = mock::start(true).await;
    // Nextcloud on S3 storage needs chunks of 5 MiB: smaller ones go as one PUT.
    let c = client_with(&m, "", Some(mock::PASSWORD));
    let content = data(3 * MIB);
    upload(&c, None, "/", "mid.bin", &content, MIB).await;
    {
        let st = m.lock().unwrap();
        assert_eq!((st.count("MKCOL", "/uploads/"), st.count("PUT", "/mid.bin")), (0, 1));
        assert_eq!(st.headers["PUT"]["X-Hash"], "sha1", "asks for the server's own SHA-1");
        assert_eq!(st.file("/mid.bin").unwrap(), content);
    }

    // The whole file in one chunk: one PUT that names its SHA-1.
    upload(&c, None, "/", "small.txt", b"small", MIB).await;
    {
        let st = m.lock().unwrap();
        assert_eq!(st.headers["PUT"]["OC-Checksum"], format!("SHA1:{}", sha1_hex(b"small")).as_str());
        assert_eq!(st.checksum("/small.txt").unwrap(), format!("SHA1:{}", sha1_hex(b"small")));
    }

    // An empty file too.
    let empty = upload(&c, None, "/", "empty", b"", MIB).await;
    assert_eq!(empty.size, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_version_copies_the_old_content_aside() {
    for m in both().await {
        let c = client(&m, "");
        let x = upload(&c, None, "/", "x.txt", b"old", MIB).await;
        c.keep_version(&x.id).await.unwrap();
        c.keep_version(&x.id).await.unwrap();
        let mut aside = names(&m, "", "x (replaced ");
        aside.sort();
        assert_eq!(aside.len(), 2, "{}: {aside:?}", mode(&m));
        let date = &aside[0]["x (replaced ".len().."x (replaced ".len() + 10];
        assert!(date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-', "{date}");
        assert_eq!(aside, [format!("x (replaced {date}) (2).txt"), format!("x (replaced {date}).txt")]);
        upload(&c, Some(&x.id), "/", "x.txt", b"new", MIB).await;
        {
            let st = m.lock().unwrap();
            assert_eq!(st.file("/x.txt").unwrap(), b"new");
            assert_eq!(st.file(&format!("/x (replaced {date}).txt")).unwrap(), b"old");
            assert_eq!(st.headers["COPY"]["Overwrite"], "F", "never overwrites");
        }
        assert_eq!(c.keep_version("/gone.txt").await, Err(RemoteError::NotFound));

        let dot = upload(&c, None, "/", ".bashrc", b"rc", MIB).await;
        c.keep_version(&dot.id).await.unwrap();
        assert_eq!(names(&m, "", ".bashrc (replaced ").len(), 1);
        no_deletes(&m);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_upload_never_overwrites_an_unknown_file() {
    for m in both().await {
        let c = client(&m, "");
        m.lock().unwrap().seed("/R/x.txt", b"theirs");
        let meta = upload(&c, None, "/R", "x.txt", b"mine", MIB).await;
        assert_eq!(meta.id, "/R/x.txt");
        assert_eq!(m.lock().unwrap().file("/R/x.txt").unwrap(), b"mine");
        let aside = names(&m, "/R", "x (replaced ");
        assert_eq!(aside.len(), 1, "{}", mode(&m));
        assert_eq!(m.lock().unwrap().file(&format!("/R/{}", aside[0])).unwrap(), b"theirs");

        // A folder in the way stays, and so does everything in it.
        m.lock().unwrap().seed("/R/d/inner.txt", b"in");
        let err = c.start_upload(None, "d", ("/R", None), 1, "").await.unwrap_err();
        assert!(matches!(err, RemoteError::Permanent(_)), "{err:?}");
        assert!(m.lock().unwrap().file("/R/d/inner.txt").is_some());
        // A folder that isn't there.
        assert_eq!(c.start_upload(None, "y.txt", ("/Nope", None), 1, "").await, Err(RemoteError::NotFound));
        assert_eq!(c.create_folder("sub", "/Nope").await, Err(RemoteError::NotFound));
        no_deletes(&m);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn quota_and_full_storage() {
    let nc = mock::start(true).await;
    let c = client(&nc, "");
    assert_eq!(c.storage_quota().await.unwrap(), (None, 0, 0), "unlimited");
    upload(&c, None, "/", "a.bin", &data(100), MIB).await;
    nc.lock().unwrap().quota = Some(10_000);
    assert_eq!(c.storage_quota().await.unwrap(), (Some(10_000), 100, 100));
    nc.lock().unwrap().quota = Some(150);
    let err = c.start_upload(None, "b.bin", ("/", None), 100, "").await.unwrap();
    let full = c.put_chunk(&err, Bytes::from(data(100)), 0, 100).await.unwrap_err();
    assert!(matches!(full, RemoteError::Quota(_)), "{full:?}");
    assert!(!full.is_transient());
    // Chunked: the assembly is what runs out of room.
    let s = c.start_upload(None, "c.bin", ("/", None), 3 * MIB as u64, "").await.unwrap();
    let full = send(&c, &s, &data(3 * MIB), 0, MIB).await.unwrap_err();
    assert!(matches!(full, RemoteError::Quota(_)), "{full:?}");
    // And keeping a version needs room too.
    let full = c.keep_version("/a.bin").await.unwrap_err();
    assert!(matches!(full, RemoteError::Quota(_)), "{full:?}");

    let plain = mock::start(false).await;
    assert_eq!(client(&plain, "").storage_quota().await.unwrap(), (None, 0, 0), "plain WebDAV says nothing");
    plain.lock().unwrap().quota = Some(10);
    let c = client(&plain, "");
    let s = c.start_upload(None, "big.bin", ("/", None), 3 * MIB as u64, "").await.unwrap();
    let full = send(&c, &s, &data(3 * MIB), 0, MIB).await.unwrap_err();
    assert!(matches!(full, RemoteError::Quota(_)), "{full:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn nextcloud_login_flow_v2() {
    let m = mock::start(true).await;
    let base = m.lock().unwrap().base.clone();
    m.lock().unwrap().login_polls = 2;
    let mut flow = webdav::nextcloud_login_start(&format!("{base}/remote.php/dav/files/whoever/")).await.unwrap();
    assert!(flow.login_url.starts_with(&format!("{base}/login/v2/flow/")), "{}", flow.login_url);
    assert_eq!(flow.poll_endpoint, format!("{base}/login/v2/poll"));
    assert_eq!((flow.interval, flow.timeout), (Duration::from_secs(2), Duration::from_secs(1200)));
    flow.interval = Duration::from_millis(10);
    let login = webdav::nextcloud_login_poll(&flow).await.unwrap();
    assert_eq!((login.server.as_str(), login.login_name.as_str(), login.app_password.as_str()), (base.as_str(), mock::USER, mock::PASSWORD));

    // What it hands back is enough to connect.
    let cfg = login.settings(&WebDavConfig { folder: "Backups".into(), ..Default::default() });
    let password = login.secrets()["password"].as_str().unwrap().to_string();
    let c = WebDav::new(webdav::http_client(), Provider::Nextcloud, cfg, Some(password));
    assert_eq!(c.probe().await.unwrap().user_id, mock::USER_ID);
    assert_eq!(c.top_parent().await.unwrap(), "/Backups");

    // Not finished in time.
    m.lock().unwrap().login_polls = 1000;
    let mut flow = webdav::nextcloud_login_start(&base).await.unwrap();
    (flow.interval, flow.timeout) = (Duration::from_millis(10), Duration::from_millis(60));
    assert!(matches!(webdav::nextcloud_login_poll(&flow).await, Err(RemoteError::Auth(_))));

    // A server without it.
    let plain = mock::start(false).await;
    let url = plain.lock().unwrap().base.clone();
    assert!(matches!(webdav::nextcloud_login_start(&url).await, Err(RemoteError::Permanent(_))));
    assert!(webdav::nextcloud_login_start("not a url").await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_become_remote_errors() {
    for m in both().await {
        let c = client(&m, "");
        upload(&c, None, "/", "a.txt", b"a", MIB).await;
        let fail = |f: Fail| m.lock().unwrap().fail.push_back(f);

        assert!(matches!(client_with(&m, "", Some("wrong")).get("/a.txt").await, Err(RemoteError::Auth(_))));
        assert!(matches!(client_with(&m, "", None).get("/a.txt").await, Err(RemoteError::Auth(_))));
        fail(Fail { retry_after: Some(7), ..Fail::new("PROPFIND", "/a.txt", 429) });
        let e = c.get("/a.txt").await.unwrap_err();
        assert!(matches!(e, RemoteError::RateLimited { retry_after: Some(7), .. }), "{e:?}");
        assert!(e.is_transient());
        fail(Fail::new("PROPFIND", "", 503));
        assert!(matches!(c.get("/a.txt").await, Err(RemoteError::Transient(_))));
        fail(Fail { retry_after: Some(3), ..Fail::new("PROPFIND", "", 503) });
        assert!(matches!(c.get("/a.txt").await, Err(RemoteError::RateLimited { retry_after: Some(3), .. })));
        fail(Fail::new("PROPFIND", "", 423));
        assert!(matches!(c.get("/a.txt").await, Err(RemoteError::Transient(_))));
        fail(Fail::new("PROPFIND", "", 403));
        assert!(matches!(c.get("/a.txt").await, Err(RemoteError::Permanent(_))));
        assert_eq!(c.get("/b.txt").await, Err(RemoteError::NotFound));
        fail(Fail::new("MKCOL", "", 507));
        assert!(matches!(c.create_folder("f", "/").await, Err(RemoteError::Quota(_))));
        fail(Fail::new("PUT", "", 507));
        let s = c.start_upload(None, "b.txt", ("/", None), 1, "").await.unwrap();
        assert!(matches!(c.put_chunk(&s, Bytes::from_static(b"b"), 0, 1).await, Err(RemoteError::Quota(_))));
        fail(Fail::new("PUT", "", 401));
        assert!(matches!(c.put_chunk(&s, Bytes::from_static(b"b"), 0, 1).await, Err(RemoteError::Auth(_))));
        // A failure half-way through a streamed or chunked upload.
        let content = data(3 * MIB);
        fail(Fail::new("PUT", "", 503));
        let s = c.start_upload(None, "c.bin", ("/", None), content.len() as u64, "").await.unwrap();
        let e = send(&c, &s, &content, 0, MIB).await.unwrap_err();
        assert!(e.is_transient(), "{}: {e:?}", mode(&m));
        assert_eq!(send(&c, &s, &content, 0, MIB).await.unwrap().size, Some(content.len() as u64));
        no_deletes(&m);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_must_match_the_nextcloud_checksum() {
    let m = mock::start(true).await;
    let c = client(&m, "");
    let meta = upload(&c, None, "/", "x.txt", b"content", MIB).await;
    assert_eq!(download(&c, &meta.id).await.unwrap().0, b"content");
    // The server's record disagrees with what it sends: never trust that copy.
    if let Some(mock::Node::File { checksum, .. }) = m.lock().unwrap().nodes.get_mut("/x.txt") {
        *checksum = Some(format!("SHA1:{}", sha1_hex(b"something else")));
    }
    assert!(matches!(download(&c, &meta.id).await, Err(RemoteError::Permanent(_))));
    // Ranges where the server ignores Range.
    m.lock().unwrap().ignore_range = true;
    assert_eq!(&c.download_range(&meta.id, 1, 4).await.unwrap()[..], b"ont");
    assert_eq!(&c.download_range(&meta.id, 5, 99).await.unwrap()[..], b"nt");
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_for_reads_the_section_and_credentials() {
    let m = mock::start(true).await;
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token.json");
    let mut secrets = serde_json::Map::new();
    secrets.insert("password".into(), mock::PASSWORD.into());
    clouddrop::credentials::save_to(&dir.path().join("credentials.json"), "nextcloud", secrets).unwrap();
    let url = m.lock().unwrap().url();
    let cfg = clouddrop::config::Config {
        provider: Provider::Nextcloud,
        nextcloud: WebDavConfig { url, user: mock::USER.into(), folder: "Laptop".into(), server: None },
        ..Default::default()
    };
    let remote = clouddrop::remote_for(&cfg, token.clone());
    assert_eq!(remote.provider(), Provider::Nextcloud);
    assert!(remote.caps().resumable && remote.caps().hash.is_none());
    assert_eq!(remote.top_parent().await.unwrap(), "/Laptop");
    assert_eq!(remote.storage_quota().await.unwrap(), (None, 0, 0));

    // The webdav provider reads its own section and credentials: none saved here.
    let cfg = clouddrop::config::Config { provider: Provider::WebDav, webdav: cfg.nextcloud.clone(), ..Default::default() };
    let remote = clouddrop::remote_for(&cfg, token);
    assert_eq!(remote.provider(), Provider::WebDav);
    assert!(!remote.caps().resumable);
    assert!(matches!(remote.top_parent().await, Err(RemoteError::Auth(_))));

    // config.toml: the section is written when set, and absent otherwise.
    let text = toml::to_string_pretty(&cfg).unwrap();
    assert!(text.contains("provider = \"webdav\"") && text.contains("[webdav]") && !text.contains("[nextcloud]"), "{text}");
    let back: clouddrop::config::Config = toml::from_str(&text).unwrap();
    assert_eq!(back.webdav, cfg.webdav);
    let plain = toml::to_string_pretty(&clouddrop::config::Config::default()).unwrap();
    assert!(!plain.contains("webdav") && !plain.contains("nextcloud"));
}
