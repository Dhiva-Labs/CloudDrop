//! The SFTP client against a real OpenSSH server (see `common::sftp_mock`). Each test skips,
//! with a note, where sshd is not available.

#![cfg(unix)]

mod common;

use clouddrop::hash::Hasher;
use clouddrop::remote::{HashKind, Remote, RemoteError, RemoteMeta, UploadStatus};
use clouddrop::sftp::{Secrets, Sftp, SftpConfig, fetch_host_key};
use common::sftp_mock::{Proxy, Sshd, fingerprint_of, keygen};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const MIB: usize = 1024 * 1024;

macro_rules! sshd {
    () => {
        match Sshd::start() {
            Some(s) => s,
            None => return,
        }
    };
}

fn client(cfg: SftpConfig) -> Remote {
    Remote::Sftp(Sftp::new(cfg, Secrets::default(), None))
}

/// Bytes that differ everywhere (so a misplaced chunk shows).
fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn sha256(data: &[u8]) -> String {
    let mut h = Hasher::new(HashKind::Sha256);
    h.update(data);
    h.finish().value
}

/// Uploads `data` the way the engine does, in `chunk`-sized pieces.
async fn upload(r: &Remote, existing: Option<&str>, name: &str, parents: (&str, Option<&str>), data: &[u8]) -> RemoteMeta {
    let total = data.len() as u64;
    let session = r.start_upload(existing, name, parents, total, "application/octet-stream").await.unwrap();
    send_from(r, &session, data, 0).await
}

async fn send_from(r: &Remote, session: &str, data: &[u8], mut offset: u64) -> RemoteMeta {
    let total = data.len() as u64;
    loop {
        let end = (offset + 8 * MIB as u64).min(total);
        let chunk = bytes::Bytes::copy_from_slice(&data[offset as usize..end as usize]);
        match r.put_chunk(session, chunk, offset, total).await.unwrap() {
            UploadStatus::Done(m) => return m,
            UploadStatus::Next(n) => {
                assert_eq!(n, end);
                offset = n;
            }
        }
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

fn today() -> String {
    // Same day as the client's keep-aside names, read the same way.
    let out = Command::new("date").arg("+%Y-%m-%d").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn host_key_is_pinned_and_a_different_one_refused() {
    let s = sshd!();
    let fp = s.fingerprint();
    assert_eq!(fetch_host_key("127.0.0.1", s.port).await.unwrap(), fp);

    // The pinned key, also as pasted from `ssh-keygen -l`.
    let mut cfg = s.config(s.port);
    cfg.host_key = format!("256 {fp} root@server (ED25519)");
    let root = client(cfg).get("").await.unwrap();
    assert!(root.folder && root.parents.is_empty() && root.id.is_empty());

    // Another server's key: refused, saying so.
    let other = s.dir.path().join("other_host");
    keygen(&other, "");
    let other_fp = fingerprint_of(&other.with_extension("pub"));
    let mut cfg = s.config(s.port);
    cfg.host_key = other_fp.clone();
    match client(cfg).get("").await {
        Err(RemoteError::Permanent(m)) => {
            assert!(m.contains("different host key") && m.contains(&other_fp) && m.contains(&fp), "{m}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // No pin yet: trusted on first use, and written into the config.
    let file = s.dir.path().join("config.toml");
    let mut cfg = s.config(s.port);
    cfg.host_key.clear();
    let text = format!(
        "provider = \"sftp\"\nsync_dir = \"{}\"\n\n[sftp]\nhost = \"127.0.0.1\"\nport = {}\nuser = \"{}\"\n",
        s.dir.path().join("sync").display(),
        s.port,
        s.user
    );
    std::fs::write(&file, text).unwrap();
    let c = Sftp::new(cfg.clone(), Secrets::default(), Some(file.clone()));
    assert_eq!(c.host_key(), None);
    c.get("").await.unwrap();
    assert_eq!(c.host_key().as_deref(), Some(fp.as_str()));
    let saved = clouddrop::config::Config::load_from(&file).unwrap();
    assert_eq!(saved.sftp.host_key, fp);
    assert_eq!(saved.sftp.user, s.user);
}

#[tokio::test(flavor = "multi_thread")]
async fn signs_in_with_keys_passphrases_and_the_agent() {
    let s = sshd!();
    let stranger = s.new_key("stranger", "", false);
    let mut cfg = s.config(s.port);
    cfg.key_file = stranger.display().to_string();
    assert!(matches!(client(cfg).get("").await, Err(RemoteError::Auth(_))));

    let locked = s.new_key("locked", "open sesame", true);
    let mut cfg = s.config(s.port);
    cfg.key_file = locked.display().to_string();
    match client(cfg.clone()).get("").await {
        Err(RemoteError::Auth(m)) => assert!(m.contains("passphrase"), "{m}"),
        other => panic!("expected a passphrase request, got {other:?}"),
    }
    let secrets = Secrets { passphrase: Some("open sesame".into()), password: None };
    Remote::Sftp(Sftp::new(cfg.clone(), secrets, None)).get("").await.unwrap();

    // Neither key nor password nor agent.
    cfg.key_file.clear();
    let none = Sftp::new(cfg.clone(), Secrets::default(), None).with_agent_socket(s.dir.path().join("no-agent"));
    assert!(matches!(none.get("").await, Err(RemoteError::Auth(_))));

    // ssh-agent holding the (passphrase-free) authorised key.
    let sock = s.dir.path().join("agent.sock");
    let Ok(mut agent) = Command::new("ssh-agent").arg("-D").arg("-a").arg(&sock).stdout(Stdio::null()).spawn() else {
        eprintln!("note: no ssh-agent here; skipping the agent sign-in");
        return;
    };
    for _ in 0..100 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let added = Command::new("ssh-add").arg(&s.key).env("SSH_AUTH_SOCK", &sock).stderr(Stdio::null()).status().unwrap();
    assert!(added.success());
    let with_agent = Sftp::new(cfg, Secrets::default(), None).with_agent_socket(sock);
    let signed_in = with_agent.get("").await;
    let _ = agent.kill();
    let _ = agent.wait();
    assert!(signed_in.unwrap().folder);
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_resumes_after_a_cut() {
    let s = sshd!();
    let proxy = Proxy::start(s.port).await;
    let r = client(s.config(proxy.port));
    let top = r.top_parent().await.unwrap();
    let folder = r.create_folder("CloudDrop", &top).await.unwrap();
    assert_eq!((folder.id.as_str(), folder.folder), ("CloudDrop", true));

    let data = pattern(20 * MIB + 12_345, 1);
    let total = data.len() as u64;
    let session = r.start_upload(None, "big.bin", ("CloudDrop", None), total, "application/octet-stream").await.unwrap();
    let first = bytes::Bytes::copy_from_slice(&data[..8 * MIB]);
    assert_eq!(r.put_chunk(&session, first, 0, total).await.unwrap(), UploadStatus::Next(8 * MIB as u64));
    // Nothing shows under the real name while it uploads.
    assert!(!s.file("CloudDrop/big.bin").exists());

    // The connection drops 3 MiB into the second chunk.
    proxy.cut_after(3 * MIB as u64);
    let second = bytes::Bytes::copy_from_slice(&data[8 * MIB..16 * MIB]);
    let err = r.put_chunk(&session, second, 8 * MIB as u64, total).await.unwrap_err();
    assert!(err.is_transient(), "{err:?}");

    // The next call reconnects; so would a fresh process with the saved session.
    let UploadStatus::Next(offset) = r.query_upload(&session, total).await.unwrap() else {
        panic!("not finished yet")
    };
    assert!(offset % MIB as u64 == 0 && (6 * MIB as u64..=11 * MIB as u64).contains(&offset), "{offset}");
    let fresh = client(s.config(proxy.port));
    assert_eq!(fresh.query_upload(&session, total).await.unwrap(), UploadStatus::Next(offset));

    let meta = send_from(&fresh, &session, &data, offset).await;
    assert_eq!((meta.id.as_str(), meta.name.as_str(), meta.size), ("CloudDrop/big.bin", "big.bin", Some(total)));
    assert_eq!(meta.parents, ["CloudDrop"]);
    assert!(meta.hash.is_none() && !meta.folder);
    assert!(std::fs::read(s.file("CloudDrop/big.bin")).unwrap() == data, "content arrived intact");
    assert_eq!(names_in(&s.file("CloudDrop")), ["big.bin"], "no partial file left");

    // Verify by download: SHA-256 of what came back.
    let out = s.dir.path().join("download");
    let mut f = tokio::fs::File::create(&out).await.unwrap();
    let (hash, n) = r.download("CloudDrop/big.bin", &mut f).await.unwrap();
    assert_eq!((hash.kind, hash.value, n), (HashKind::Sha256, sha256(&data), total));

    // The session is spent; strangers' sessions are unknown.
    assert_eq!(r.query_upload(&session, total).await, Err(RemoteError::NotFound));
    assert_eq!(r.query_upload("https://example.com/upload?id=1", total).await, Err(RemoteError::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn renames_and_moves_change_ids() {
    let s = sshd!();
    let r = client(s.config(s.port));
    r.create_folder("CloudDrop", "").await.unwrap();
    let a = r.create_folder("A", "CloudDrop").await.unwrap();
    assert_eq!((a.id.as_str(), a.parents.as_slice()), ("CloudDrop/A", &["CloudDrop".to_string()][..]));
    assert_eq!(r.create_folder("A", "CloudDrop").await.unwrap(), a, "an existing folder is reused");
    assert_eq!(r.create_folder("X", "CloudDrop/missing").await, Err(RemoteError::NotFound));
    upload(&r, None, "x.txt", ("CloudDrop/A", None), b"x").await;

    // Renaming a folder changes its id and its children's.
    let b = r.move_rename("CloudDrop/A", Some("B"), None).await.unwrap();
    assert_eq!((b.id.as_str(), b.name.as_str(), b.folder), ("CloudDrop/B", "B", true));
    assert_eq!(r.get("CloudDrop/A").await, Err(RemoteError::NotFound));
    assert_eq!(r.get("CloudDrop/B/x.txt").await.unwrap().size, Some(1));

    // Moving and renaming a file at once.
    r.create_folder("C", "CloudDrop").await.unwrap();
    let y = r
        .move_rename("CloudDrop/B/x.txt", Some("y.txt"), Some(("CloudDrop/C", "CloudDrop/B")))
        .await
        .unwrap();
    assert_eq!((y.id.as_str(), y.parents.as_slice()), ("CloudDrop/C/y.txt", &["CloudDrop/C".to_string()][..]));

    // Moving a folder into another.
    let moved = r.move_rename("CloudDrop/B", None, Some(("CloudDrop/C", "CloudDrop"))).await.unwrap();
    assert_eq!(moved.id, "CloudDrop/C/B");
    assert_eq!(r.move_rename("CloudDrop/nope", Some("n"), None).await, Err(RemoteError::NotFound));

    // A taken name: what was there moves aside, nothing is lost.
    upload(&r, None, "z.txt", ("CloudDrop/C", None), b"old z").await;
    let z = r.move_rename("CloudDrop/C/y.txt", Some("z.txt"), None).await.unwrap();
    assert_eq!(z.id, "CloudDrop/C/z.txt");
    assert_eq!(std::fs::read(s.file("CloudDrop/C/z.txt")).unwrap(), b"x");
    let aside = format!("z (replaced {}).txt", today());
    assert_eq!(std::fs::read(s.file(&format!("CloudDrop/C/{aside}"))).unwrap(), b"old z");
    assert_eq!(names_in(&s.file("CloudDrop/C")), ["B", aside.as_str(), "z.txt"]);

    // find_child: by name, folders only when asked.
    assert_eq!(r.find_child("z.txt", "CloudDrop/C", false).await.unwrap(), [z]);
    assert!(r.find_child("z.txt", "CloudDrop/C", true).await.unwrap().is_empty());
    assert_eq!(r.find_child("B", "CloudDrop/C", true).await.unwrap()[0].id, "CloudDrop/C/B");
    assert!(r.find_child("none", "CloudDrop/C", false).await.unwrap().is_empty());
    assert!(r.find_child("x", "CloudDrop/gone", false).await.unwrap().is_empty());
}

async fn keep_version_keeps_dated_copies(r: Remote, s: &Sshd) {
    let date = today();
    r.create_folder("CloudDrop", "").await.unwrap();
    let doc = upload(&r, None, "doc.txt", ("CloudDrop", None), b"version 1").await;
    r.keep_version(&doc.id).await.unwrap();
    let kept1 = s.file(&format!("CloudDrop/doc (replaced {date}).txt"));
    assert_eq!(std::fs::read(&kept1).unwrap(), b"version 1");

    let doc2 = upload(&r, Some(&doc.id), "doc.txt", ("CloudDrop", Some("CloudDrop")), b"version 2").await;
    assert_eq!((doc2.id.as_str(), doc2.size), ("CloudDrop/doc.txt", Some(9)));
    assert_eq!(std::fs::read(s.file("CloudDrop/doc.txt")).unwrap(), b"version 2");
    r.keep_version(&doc2.id).await.unwrap();
    let kept2 = s.file(&format!("CloudDrop/doc (replaced {date}) (2).txt"));
    assert_eq!(std::fs::read(&kept2).unwrap(), b"version 2");
    assert_eq!(std::fs::read(&kept1).unwrap(), b"version 1");

    // New content for a file that moved meanwhile: it moves, then changes.
    r.create_folder("sub", "CloudDrop").await.unwrap();
    let moved = upload(&r, Some(&doc2.id), "moved.txt", ("CloudDrop/sub", Some("CloudDrop")), b"version 3").await;
    assert_eq!(moved.id, "CloudDrop/sub/moved.txt");
    assert_eq!(std::fs::read(s.file("CloudDrop/sub/moved.txt")).unwrap(), b"version 3");
    assert!(!s.file("CloudDrop/doc.txt").exists());

    // A new upload onto a taken name keeps what was there.
    upload(&r, None, "moved.txt", ("CloudDrop/sub", None), b"version 4").await;
    assert_eq!(std::fs::read(s.file("CloudDrop/sub/moved.txt")).unwrap(), b"version 4");
    let aside = s.file(&format!("CloudDrop/sub/moved (replaced {date}).txt"));
    assert_eq!(std::fs::read(aside).unwrap(), b"version 3");

    // An empty file, and an empty folder's keep_version does nothing.
    let empty = upload(&r, None, "empty", ("CloudDrop", None), b"").await;
    assert_eq!(empty.size, Some(0));
    r.keep_version("CloudDrop/sub").await.unwrap();
    assert_eq!(r.keep_version("CloudDrop/missing.txt").await, Err(RemoteError::NotFound));

    for dir in ["CloudDrop", "CloudDrop/sub"] {
        assert!(names_in(&s.file(dir)).iter().all(|n| !n.starts_with(".clouddrop-")), "no partial files in {dir}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_version_copies_on_the_server() {
    let s = sshd!();
    keep_version_keeps_dated_copies(client(s.config(s.port)), &s).await;
}

/// Servers without OpenSSH's extensions: copies stream through the client, replacing renames
/// go through a temporary name, and there is no quota.
#[tokio::test(flavor = "multi_thread")]
async fn keep_version_and_uploads_without_extensions() {
    let s = sshd!();
    let plain = Sftp::new(s.config(s.port), Secrets::default(), None).without_extensions();
    assert_eq!(plain.storage_quota().await.unwrap(), (None, 0, 0));
    keep_version_keeps_dated_copies(Remote::Sftp(plain), &s).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ranged_downloads_and_quota() {
    let s = sshd!();
    let r = client(s.config(s.port));
    r.create_folder("CloudDrop", "").await.unwrap();
    let data = pattern(3 * MIB + 7, 2);
    let total = data.len() as u64;
    let m = upload(&r, None, "photo.jpg", ("CloudDrop", None), &data).await;

    let part = r.download_range(&m.id, 5, 2_000_005).await.unwrap();
    assert!(part[..] == data[5..2_000_005]);
    let tail = r.download_range(&m.id, total - 10, total + 100).await.unwrap();
    assert!(tail[..] == data[data.len() - 10..], "a range past the end stops at the end");
    assert!(r.download_range(&m.id, 10, 10).await.unwrap().is_empty());
    assert_eq!(r.download_range("CloudDrop/none.jpg", 0, 10).await, Err(RemoteError::NotFound));

    let (limit, used, used_here) = r.storage_quota().await.unwrap();
    let limit = limit.expect("statvfs@openssh.com gives the disk size");
    assert!(limit > 0 && used > 0 && used <= limit && used_here == used, "{limit} {used} {used_here}");
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_say_what_happened() {
    let s = sshd!();
    let proxy = Proxy::start(s.port).await;
    let r = client(s.config(proxy.port));
    r.create_folder("CloudDrop", "").await.unwrap();

    // A dropped connection is transient, and the next call reconnects by itself.
    proxy.cut();
    match r.get("CloudDrop").await {
        Ok(_) => {}
        Err(e) => {
            assert!(e.is_transient(), "{e:?}");
            r.get("CloudDrop").await.unwrap();
        }
    }

    // Permission denied: permanent, in plain words.
    std::fs::create_dir(s.file("CloudDrop/locked")).unwrap();
    set_mode(&s.file("CloudDrop/locked"), 0o500);
    let denied = r.create_folder("inner", "CloudDrop/locked").await;
    let denied_upload = r.start_upload(None, "a.txt", ("CloudDrop/locked", None), 1, "text/plain").await;
    set_mode(&s.file("CloudDrop/locked"), 0o700);
    match denied {
        Err(RemoteError::Permanent(m)) => assert!(m.contains("denied permission") && m.contains("locked/inner"), "{m}"),
        other => panic!("expected permission denied, got {other:?}"),
    }
    assert!(matches!(denied_upload, Err(RemoteError::Permanent(m)) if m.contains("denied permission")));

    // A base folder that isn't there.
    let mut cfg = s.config(s.port);
    cfg.path = s.file("nope").display().to_string();
    match client(cfg).get("").await {
        Err(RemoteError::Permanent(m)) => assert!(m.contains("does not exist"), "{m}"),
        other => panic!("expected a missing folder, got {other:?}"),
    }

    // Nothing listening.
    drop(proxy);
    let mut cfg = s.config(s.port);
    cfg.port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert!(client(cfg).get("").await.unwrap_err().is_transient());
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}
