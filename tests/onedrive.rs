//! The OneDrive client against the fake Graph in `common/onedrive_mock.rs`.

mod common;

use clouddrop::hash::Hasher;
use clouddrop::onedrive::OneDrive;
use clouddrop::remote::{ContentHash, HashKind, RemoteError, RemoteMeta, RemoteResult, TokenSource, UploadStatus};
use common::onedrive_mock::{self, Fail, Shared};
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const UNIT: usize = 320 * 1024;

/// A signed-in account: a token the fake accepts, and refreshes that ask it for a new one
/// (or, once revoked, hand back one it refuses).
struct Tokens {
    mock: Shared,
    current: Mutex<String>,
    refreshes: AtomicU64,
    revoked: bool,
}

impl TokenSource for Tokens {
    fn token(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(async move { Ok(self.current.lock().unwrap().clone()) })
    }

    fn refresh(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(async move {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            let t = if self.revoked { "revoked".to_string() } else { self.mock.lock().unwrap().issue_token() };
            *self.current.lock().unwrap() = t.clone();
            Ok(t)
        })
    }
}

struct Setup {
    mock: Shared,
    tokens: Arc<Tokens>,
    od: OneDrive,
}

impl Setup {
    async fn new() -> Self {
        let mock = onedrive_mock::start().await;
        Self::on(mock, false)
    }

    /// Another client on the same account, like CloudDrop after a restart.
    fn on(mock: Shared, revoked: bool) -> Self {
        let token = mock.lock().unwrap().issue_token();
        let tokens = Arc::new(Tokens { mock: mock.clone(), current: Mutex::new(token), refreshes: AtomicU64::new(0), revoked });
        let base = mock.lock().unwrap().graph_base();
        let od = OneDrive::new(clouddrop::drive::http_client(), base, tokens.clone());
        Setup { mock, tokens, od }
    }

    fn fail(&self, f: Fail) {
        self.mock.lock().unwrap().fail_next.push(f);
    }

    fn content(&self, id: &str) -> Vec<u8> {
        self.mock.lock().unwrap().items[id].content.clone()
    }

    /// Uploads `data` as `name` (new, or over `existing`), `chunk` bytes at a time.
    async fn upload(&self, existing: Option<&str>, name: &str, parent: &str, data: &[u8], chunk: usize) -> RemoteMeta {
        let session = self.od.start_upload(existing, name, (parent, None), data.len() as u64, "text/plain").await.unwrap();
        self.send(&session, data, 0, chunk).await
    }

    async fn send(&self, session: &str, data: &[u8], mut offset: usize, chunk: usize) -> RemoteMeta {
        loop {
            let end = (offset + chunk).min(data.len());
            let part = bytes::Bytes::copy_from_slice(&data[offset..end]);
            match self.od.put_chunk(session, part, offset as u64, data.len() as u64).await.unwrap() {
                UploadStatus::Done(m) => return m,
                UploadStatus::Next(n) => offset = n as usize,
            }
        }
    }
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 % 251) as u8).collect()
}

fn quickxor(data: &[u8]) -> ContentHash {
    let mut h = Hasher::new(HashKind::QuickXor);
    h.update(data);
    h.finish()
}

#[tokio::test]
async fn app_folder_folders_and_lookups() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    assert_eq!(top, s.mock.lock().unwrap().approot);
    let asked = s.mock.lock().unwrap().graph_requests;
    assert_eq!(s.od.top_parent().await.unwrap(), top);
    assert_eq!(s.mock.lock().unwrap().graph_requests, asked, "the app folder's id is remembered");

    assert!(s.od.find_child("CloudDrop", &top, true).await.unwrap().is_empty());
    let root = s.od.create_folder("CloudDrop", &top).await.unwrap();
    assert!(root.folder);
    assert_eq!((root.name.as_str(), root.parents.as_slice()), ("CloudDrop", &[top.clone()][..]));
    // Names ignore case, and a folder that's already there is the one to use.
    let found = s.od.find_child("clouddrop", &top, true).await.unwrap();
    assert_eq!(found.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), [root.id.as_str()]);
    assert_eq!(s.od.create_folder("CLOUDDROP", &top).await.unwrap().id, root.id);

    // Awkward names survive the trip through the URL path.
    let odd = "Q3 #1 & 50% café.txt";
    let f = s.upload(None, odd, &root.id, b"hello", UNIT).await;
    assert_eq!(f.name, odd);
    let hits = s.od.find_child(odd, &root.id, false).await.unwrap();
    assert_eq!(hits, std::slice::from_ref(&f));
    assert!(s.od.find_child(odd, &root.id, true).await.unwrap().is_empty(), "not a folder");
    assert_eq!(s.od.get(&f.id).await.unwrap(), f);

    // Rename, then move (PATCH with parentReference); the id stays.
    let sub = s.od.create_folder("Sub", &root.id).await.unwrap();
    let renamed = s.od.move_rename(&f.id, Some("b.txt"), None).await.unwrap();
    assert_eq!((renamed.id.as_str(), renamed.name.as_str()), (f.id.as_str(), "b.txt"));
    let moved = s.od.move_rename(&f.id, None, Some((&sub.id, &root.id))).await.unwrap();
    assert_eq!(moved.parents, std::slice::from_ref(&sub.id));
    assert_eq!(s.mock.lock().unwrap().lookup("CloudDrop/Sub/b.txt").unwrap().id, f.id);
    assert_eq!(s.od.move_rename(&f.id, None, None).await.unwrap(), moved);

    // Missing items and parents.
    assert_eq!(s.od.get("0123456789ABCDEF!999").await, Err(RemoteError::NotFound));
    assert_eq!(s.od.create_folder("x", "0123456789ABCDEF!999").await, Err(RemoteError::NotFound));
    let lost = s.od.start_upload(None, "x.txt", ("0123456789ABCDEF!999", None), 5, "text/plain").await;
    assert_eq!(lost, Err(RemoteError::NotFound));
    let r = s.od.move_rename(&f.id, None, Some(("0123456789ABCDEF!999", &sub.id))).await;
    assert_eq!(r, Err(RemoteError::NotFound));
}

#[tokio::test]
async fn upload_in_fragments_with_matching_hash() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let bytes = data(3 * UNIT + 1234);
    let m = s.upload(None, "big.bin", &top, &bytes, UNIT).await;
    assert_eq!(m.hash, Some(quickxor(&bytes)), "the client's hex matches crate::hash of the bytes");
    assert!(m.same_content(&quickxor(&bytes), bytes.len() as u64));
    assert_eq!(s.content(&m.id), bytes);
    let st = s.mock.lock().unwrap();
    assert_eq!((st.sessions_created, st.fragment_puts), (1, 4));
}

#[tokio::test]
async fn chunks_of_60_mib_go_up_in_smaller_fragments() {
    // Graph refuses a fragment of 60 MiB or more (and so does the fake).
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let bytes = data(60 * 1024 * 1024 + 10);
    let m = s.upload(None, "huge.bin", &top, &bytes, bytes.len()).await;
    assert_eq!(m.size, Some(bytes.len() as u64));
    assert!(s.content(&m.id) == bytes);
    assert_eq!(s.mock.lock().unwrap().fragment_puts, 2);
}

#[tokio::test]
async fn resumes_after_an_interruption() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let bytes = data(4 * UNIT + 7);
    let session = s.od.start_upload(None, "resume.bin", (&top, None), bytes.len() as u64, "").await.unwrap();
    for i in 0..2 {
        let part = bytes::Bytes::copy_from_slice(&bytes[i * UNIT..(i + 1) * UNIT]);
        let next = s.od.put_chunk(&session, part, (i * UNIT) as u64, bytes.len() as u64).await.unwrap();
        assert_eq!(next, UploadStatus::Next(((i + 1) * UNIT) as u64));
    }
    // The third fragment is cut off.
    s.mock.lock().unwrap().fail_puts.push(500);
    let part = bytes::Bytes::copy_from_slice(&bytes[2 * UNIT..3 * UNIT]);
    let cut = s.od.put_chunk(&session, part, 2 * UNIT as u64, bytes.len() as u64).await;
    assert!(matches!(cut, Err(RemoteError::Transient(_))), "{cut:?}");

    // CloudDrop restarts with the session it kept, and carries on where OneDrive is.
    let again = Setup::on(s.mock.clone(), false);
    let at = again.od.query_upload(&session, bytes.len() as u64).await.unwrap();
    assert_eq!(at, UploadStatus::Next(2 * UNIT as u64));
    let m = again.send(&session, &bytes, 2 * UNIT, UNIT).await;
    assert_eq!(s.content(&m.id), bytes);
    assert_eq!(m.hash, Some(quickxor(&bytes)));
    assert_eq!(s.mock.lock().unwrap().sessions_created, 1);

    // Done: OneDrive forgets the session.
    assert_eq!(again.od.query_upload(&session, bytes.len() as u64).await, Err(RemoteError::NotFound));
}

#[tokio::test]
async fn resent_fragment_asks_where_to_go_on() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let bytes = data(2 * UNIT + 5);
    let session = s.od.start_upload(None, "twice.bin", (&top, None), bytes.len() as u64, "").await.unwrap();
    let first = bytes::Bytes::copy_from_slice(&bytes[..UNIT]);
    assert_eq!(s.od.put_chunk(&session, first.clone(), 0, bytes.len() as u64).await.unwrap(), UploadStatus::Next(UNIT as u64));
    // The answer got lost and the engine sends it again: OneDrive says 416, the client asks.
    assert_eq!(s.od.put_chunk(&session, first, 0, bytes.len() as u64).await.unwrap(), UploadStatus::Next(UNIT as u64));
    let m = s.send(&session, &bytes, UNIT, UNIT).await;
    assert_eq!(s.content(&m.id), bytes);
}

#[tokio::test]
async fn expired_session_is_not_found() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let bytes = data(2 * UNIT);
    let session = s.od.start_upload(None, "late.bin", (&top, None), bytes.len() as u64, "").await.unwrap();
    let part = bytes::Bytes::copy_from_slice(&bytes[..UNIT]);
    s.od.put_chunk(&session, part, 0, bytes.len() as u64).await.unwrap();
    s.mock.lock().unwrap().expire_sessions();
    assert_eq!(s.od.query_upload(&session, bytes.len() as u64).await, Err(RemoteError::NotFound));
    let part = bytes::Bytes::copy_from_slice(&bytes[UNIT..]);
    assert_eq!(s.od.put_chunk(&session, part, UNIT as u64, bytes.len() as u64).await, Err(RemoteError::NotFound));
    // Something that isn't one of our sessions (a Drive URL left behind) is expired too.
    assert_eq!(s.od.query_upload("https://example.com/upload", 3).await, Err(RemoteError::NotFound));
}

#[tokio::test]
async fn replacing_content_names_and_places_the_file() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let a = s.od.create_folder("A", &top).await.unwrap();
    let b = s.od.create_folder("B", &top).await.unwrap();
    let first = s.upload(None, "notes.txt", &a.id, b"version one", UNIT).await;

    // A new file can't take a name that's in use.
    let clash = s.od.start_upload(None, "NOTES.txt", (&a.id, None), 3, "").await;
    assert!(matches!(clash, Err(RemoteError::Permanent(ref m)) if m.contains("409")), "{clash:?}");

    let bytes = data(UNIT + 3);
    let session = s
        .od
        .start_upload(Some(&first.id), "renamed.txt", (&b.id, Some(&a.id)), bytes.len() as u64, "")
        .await
        .unwrap();
    let m = s.send(&session, &bytes, 0, UNIT).await;
    assert_eq!((m.id.as_str(), m.name.as_str(), m.parents.as_slice()), (first.id.as_str(), "renamed.txt", &[b.id.clone()][..]));
    assert_eq!(m.hash, Some(quickxor(&bytes)));
    assert_eq!(s.content(&first.id), bytes);
}

#[tokio::test]
async fn empty_files() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let session = s.od.start_upload(None, "empty.txt", (&top, None), 0, "").await.unwrap();
    assert_eq!(s.od.query_upload(&session, 0).await.unwrap(), UploadStatus::Next(0));
    let m = s.send(&session, b"", 0, UNIT).await;
    assert_eq!((m.size, m.hash.clone()), (Some(0), Some(quickxor(b""))));
    assert_eq!(s.mock.lock().unwrap().sessions_created, 0, "no session for nothing");

    // Emptying a file that had content.
    let full = s.upload(None, "full.txt", &top, b"something", UNIT).await;
    let session = s.od.start_upload(Some(&full.id), "full.txt", (&top, Some(&top)), 0, "").await.unwrap();
    let m = s.send(&session, b"", 0, UNIT).await;
    assert_eq!((m.id.as_str(), m.size), (full.id.as_str(), Some(0)));
    assert!(s.content(&full.id).is_empty());
}

#[tokio::test]
async fn missing_hash_is_asked_for_again() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    s.mock.lock().unwrap().hashless_uploads = 1;
    let bytes = data(UNIT + 9);
    let m = s.upload(None, "late-hash.bin", &top, &bytes, UNIT).await;
    assert_eq!(m.hash, Some(quickxor(&bytes)));
}

#[tokio::test]
async fn keep_version_leaves_a_copy() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let dir = s.od.create_folder("Docs", &top).await.unwrap();
    let f = s.upload(None, "report.pdf", &dir.id, b"old content", UNIT).await;

    s.od.keep_version(&f.id).await.unwrap();
    let today = {
        let date = |name: &str| name.strip_prefix("report (replaced ").and_then(|r| r.strip_suffix(").pdf")).map(str::to_string);
        let st = s.mock.lock().unwrap();
        let names: Vec<String> = st.children(&dir.id).into_iter().map(|i| i.name).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let copy = names.iter().map(String::as_str).find_map(date).expect("a dated copy");
        assert_eq!(copy.len(), "2026-10-21".len());
        assert!(st.monitor_polls >= 3, "followed the monitor until the copy was done");
        copy
    };

    // New content goes over the file; the copy keeps the old.
    let session = s.od.start_upload(Some(&f.id), "report.pdf", (&dir.id, Some(&dir.id)), 11, "").await.unwrap();
    s.send(&session, b"new content", 0, UNIT).await;
    let copy = s.od.find_child(&format!("report (replaced {today}).pdf"), &dir.id, false).await.unwrap();
    assert_eq!(copy.len(), 1);
    assert_eq!(s.content(&copy[0].id), b"old content");
    assert_eq!(copy[0].hash, Some(quickxor(b"old content")));
    assert_eq!(s.content(&f.id), b"new content");

    // Replaced again the same day: the next copy gets a number.
    s.od.keep_version(&f.id).await.unwrap();
    let second = s.od.find_child(&format!("report (replaced {today}) (2).pdf"), &dir.id, false).await.unwrap();
    assert_eq!(s.content(&second[0].id), b"new content");

    // A copy OneDrive gives up on is an error, not a silent loss.
    s.mock.lock().unwrap().fail_copies = true;
    let failed = s.od.keep_version(&f.id).await;
    assert!(matches!(failed, Err(RemoteError::Permanent(ref m)) if m.contains("copy went wrong")), "{failed:?}");
}

#[tokio::test]
async fn downloads_follow_the_redirect_without_the_token() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    let bytes = data(UNIT + 100);
    let m = s.upload(None, "dl.bin", &top, &bytes, UNIT).await;

    // The fake refuses a token on the download URL, so these pass only without one.
    assert_eq!(s.od.download_range(&m.id, 5, 15).await.unwrap(), bytes[5..15]);
    assert_eq!(s.od.download_range(&m.id, UNIT as u64, bytes.len() as u64 + 50).await.unwrap(), bytes[UNIT..]);
    assert!(s.od.download_range(&m.id, 7, 7).await.unwrap().is_empty());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out");
    let mut out = tokio::fs::File::create(&path).await.unwrap();
    let (hash, n) = s.od.download(&m.id, &mut out).await.unwrap();
    assert_eq!((hash, n), (quickxor(&bytes), bytes.len() as u64));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(s.mock.lock().unwrap().downloads, 3);
    assert_eq!(s.od.download_range("0123456789ABCDEF!999", 0, 3).await, Err(RemoteError::NotFound));
}

#[tokio::test]
async fn quota_from_the_drive() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();
    s.upload(None, "q.bin", &top, &data(1000), UNIT).await;
    s.mock.lock().unwrap().quota_total = 1_000_000;
    assert_eq!(s.od.storage_quota().await.unwrap(), (Some(1_000_000), 1000, 1000));
    // An upload that won't fit is refused when its first fragment arrives (OneDrive refuses
    // `fileSize` in the session request, so it can't be refused earlier).
    let session = s.od.start_upload(None, "huge.bin", (&top, None), 2_000_000, "").await.unwrap();
    let full = s.od.put_chunk(&session, bytes::Bytes::from(data(UNIT)), 0, 2_000_000).await;
    assert!(matches!(full, Err(RemoteError::Quota(_))), "{full:?}");
}

#[tokio::test]
async fn expired_token_is_refreshed_once() {
    let s = Setup::new().await;
    s.mock.lock().unwrap().invalidate_tokens();
    assert!(s.od.top_parent().await.is_ok());
    assert_eq!(s.tokens.refreshes.load(Ordering::SeqCst), 1);

    // Revoked sign-in: the refreshed token is refused too, and that's Auth.
    let revoked = Setup::on(s.mock.clone(), true);
    s.mock.lock().unwrap().invalidate_tokens();
    let r = revoked.od.storage_quota().await;
    assert!(matches!(r, Err(RemoteError::Auth(_))), "{r:?}");
    assert_eq!(revoked.tokens.refreshes.load(Ordering::SeqCst), 1, "refreshes once, not forever");
}

#[tokio::test]
async fn graph_errors_map_to_remote_errors() {
    let s = Setup::new().await;
    let top = s.od.top_parent().await.unwrap();

    s.fail(Fail::new(429, "activityLimitReached").retry_after(7));
    let r = s.od.get(&top).await;
    assert!(matches!(r, Err(RemoteError::RateLimited { retry_after: Some(7), .. })), "{r:?}");

    s.fail(Fail::new(503, "serviceNotAvailable").retry_after(30));
    let r = s.od.find_child("x", &top, false).await;
    assert!(matches!(r, Err(RemoteError::RateLimited { retry_after: Some(30), .. })), "{r:?}");

    s.fail(Fail::new(507, "insufficientStorage"));
    let r = s.od.start_upload(None, "a.txt", (&top, None), 3, "").await;
    assert_eq!(r, Err(RemoteError::Quota("Your OneDrive is full.".into())));

    s.fail(Fail::new(403, "accessDenied").inner("quotaLimitReached"));
    assert!(matches!(s.od.create_folder("x", &top).await, Err(RemoteError::Quota(_))));

    s.fail(Fail::new(500, "generalException"));
    assert!(matches!(s.od.storage_quota().await, Err(RemoteError::Transient(_))));

    s.fail(Fail::new(404, "itemNotFound"));
    assert_eq!(s.od.keep_version(&top).await, Err(RemoteError::NotFound));

    s.fail(Fail::new(400, "invalidRequest"));
    let r = s.od.move_rename(&top, Some("y"), None).await;
    assert_eq!(r, Err(RemoteError::Permanent("OneDrive: injected invalidRequest (400)".into())));

    // Fragment uploads see the same mapping.
    let session = s.od.start_upload(None, "frag.bin", (&top, None), 10, "").await.unwrap();
    s.mock.lock().unwrap().fail_puts.push(503);
    let r = s.od.put_chunk(&session, bytes::Bytes::from_static(b"0123456789"), 0, 10).await;
    assert!(matches!(r, Err(RemoteError::RateLimited { .. })), "{r:?}");
    s.mock.lock().unwrap().fail_puts.push(507);
    let r = s.od.put_chunk(&session, bytes::Bytes::from_static(b"0123456789"), 0, 10).await;
    assert!(matches!(r, Err(RemoteError::Quota(_))), "{r:?}");
    assert!(matches!(
        s.od.put_chunk(&session, bytes::Bytes::from_static(b"0123456789"), 0, 10).await,
        Ok(UploadStatus::Done(_))
    ));
}
