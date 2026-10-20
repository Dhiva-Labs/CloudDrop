//! The whole engine against each provider's fake server, the way people use it: upload a
//! folder, rename it, edit a file, free up space (proved by the provider's hash, or by
//! downloading) and download a file back. The providers' own tests check their clients;
//! these check that the engine and every client fit together.

mod common;

use clouddrop::credentials;
use clouddrop::dropbox::Dropbox;
use clouddrop::mega::MegaConfig;
use clouddrop::onedrive::OneDrive;
use clouddrop::remote::{Provider, Remote, RemoteResult, TokenSource};
use clouddrop::s3::{S3, S3Config};
use clouddrop::webdav::{WebDav, WebDavConfig};
use clouddrop::{db::State, sftp};
use common::{Harness, dropbox_mock, mega_mock, onedrive_mock, read, s3_mock, webdav_mock};
use futures_util::future::BoxFuture;
use std::sync::{Arc, Mutex};

/// An OAuth sign-in on a fake server: a fresh token from it on every refresh.
struct FakeTokens {
    issue: Box<dyn Fn() -> String + Send + Sync>,
    current: Mutex<String>,
}

impl FakeTokens {
    fn new(issue: impl Fn() -> String + Send + Sync + 'static) -> Arc<Self> {
        let current = Mutex::new(issue());
        Arc::new(FakeTokens { issue: Box::new(issue), current })
    }
}

impl TokenSource for FakeTokens {
    fn token(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(async move { Ok(self.current.lock().unwrap().clone()) })
    }

    fn refresh(&self) -> BoxFuture<'_, RemoteResult<String>> {
        Box::pin(async move {
            let t = (self.issue)();
            *self.current.lock().unwrap() = t.clone();
            Ok(t)
        })
    }
}

/// Upload, rename a folder, edit, free, restore.
async fn everyday_use(mut h: Harness) {
    let provider = h.cfg.provider;
    h.write("Trip/a.txt", "alpha");
    h.write("Trip/Sub/b.txt", "bravo");
    h.write("top.txt", "top");
    h.start();
    // Not just `settle`: signing in can take a while (MEGA derives its key with PBKDF2), and the
    // queue is empty until the engine has looked at the folder.
    h.wait("the first uploads", |h| {
        ["Trip/a.txt", "Trip/Sub/b.txt", "top.txt"].iter().all(|p| h.state(p) == Some(State::Synced))
    })
    .await;
    h.settle().await;

    // A folder renamed locally is renamed in the cloud; path-based providers rewrite the ids
    // of everything beneath it.
    std::fs::rename(h.path("Trip"), h.path("Journey")).unwrap();
    h.settle().await;
    for p in ["Journey/a.txt", "Journey/Sub/b.txt"] {
        assert_eq!(h.state(p), Some(State::Synced), "{provider:?}: {p} after the rename");
    }

    // An edit is uploaded as new content of the same file.
    h.write("top.txt", "top, edited");
    h.settle().await;
    assert_eq!(h.state("top.txt"), Some(State::Synced), "{provider:?}: edit uploaded");

    // Free up space: only after the cloud copy is proved identical.
    let report = h.free(&["Journey"]).await.unwrap();
    assert_eq!(report.freed_files, 2, "{provider:?}: {report:?}");
    h.wait("cloud-only", |h| {
        h.state("Journey/a.txt") == Some(State::CloudOnly) && h.state("Journey/Sub/b.txt") == Some(State::CloudOnly)
    })
    .await;
    assert!(!h.path("Journey/a.txt").exists());

    // And back.
    h.restore(&["Journey/Sub/b.txt"]).await.unwrap();
    assert_eq!(read(&h.path("Journey/Sub/b.txt")), "bravo", "{provider:?}: downloaded back");
    h.settle().await;
    assert_eq!(h.state("Journey/Sub/b.txt"), Some(State::Synced));
    h.stop().await;
}

#[tokio::test]
async fn onedrive() {
    let mock = onedrive_mock::start().await;
    let base = mock.lock().unwrap().graph_base();
    let m = mock.clone();
    let tokens = FakeTokens::new(move || m.lock().unwrap().issue_token());
    let mut h = Harness::new().await;
    h.use_client(Arc::new(Remote::OneDrive(OneDrive::new(clouddrop::drive::http_client(), base, tokens))));
    everyday_use(h).await;
}

#[tokio::test]
async fn dropbox() {
    let mock = dropbox_mock::start().await;
    let (api, content) = {
        let st = mock.lock().unwrap();
        (st.api.clone(), st.content.clone())
    };
    let m = mock.clone();
    let tokens = FakeTokens::new(move || m.lock().unwrap().issue_token());
    let mut h = Harness::new().await;
    h.use_client(Arc::new(Remote::Dropbox(Dropbox::new(clouddrop::drive::http_client(), api, content, tokens))));
    everyday_use(h).await;
}

async fn webdav(nextcloud: bool) {
    let mock = webdav_mock::start(nextcloud).await;
    let url = mock.lock().unwrap().url();
    let provider = if nextcloud { Provider::Nextcloud } else { Provider::WebDav };
    let cfg = WebDavConfig { url, user: webdav_mock::USER.into(), folder: "Backups".into(), server: None };
    let client = WebDav::new(clouddrop::webdav::http_client(), provider, cfg, Some(webdav_mock::PASSWORD.into()));
    let mut h = Harness::new().await;
    h.use_client(Arc::new(Remote::WebDav(Box::new(client))));
    everyday_use(h).await;
}

#[tokio::test]
async fn plain_webdav() {
    webdav(false).await;
}

#[tokio::test]
async fn nextcloud() {
    webdav(true).await;
}

#[tokio::test]
async fn s3() {
    let mock = s3_mock::start().await;
    let cfg = S3Config {
        endpoint: mock.lock().unwrap().base.clone(),
        region: s3_mock::REGION.into(),
        bucket: s3_mock::BUCKET.into(),
        prefix: "laptop".into(),
        access_key_id: s3_mock::ACCESS_KEY.into(),
        addressing: String::new(),
    };
    let client = S3::new(clouddrop::drive::http_client(), &cfg, Some(s3_mock::SECRET.into()));
    let mut h = Harness::new().await;
    h.use_client(Arc::new(Remote::S3(Box::new(client))));
    everyday_use(h).await;
}

#[cfg(unix)]
#[tokio::test]
async fn sftp_server() {
    let Some(sshd) = common::sftp_mock::Sshd::start() else {
        return;
    };
    let client = sftp::Sftp::new(sshd.config(sshd.port), sftp::Secrets::default(), None);
    let mut h = Harness::new().await;
    h.use_client(Arc::new(Remote::Sftp(client)));
    everyday_use(h).await;
}

#[tokio::test]
async fn mega() {
    let mock = mega_mock::start_mega_mock().await;
    let mut h = Harness::new().await;
    // What `clouddrop setup --provider mega` saves; the client trades the password for a session.
    let mut secrets = serde_json::Map::new();
    secrets.insert("password".into(), mega_mock::PASSWORD.into());
    credentials::save_to(&h.data.join("credentials.json"), "mega", secrets).unwrap();
    h.cfg.provider = Provider::Mega;
    h.cfg.mega = MegaConfig { email: mega_mock::EMAIL.into(), api_url: mock.lock().unwrap().base.clone() };
    let client = clouddrop::remote_for(&h.cfg, h.token_path());
    h.use_client(client);
    everyday_use(h).await;
}

