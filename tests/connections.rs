//! Several clouds at once (docs/PROVIDERS.md › "Several clouds at once"): the real `clouddrop`
//! CLI and daemon, in a sandbox, with connections uploading side by side to fake servers.
//!
//! The sandbox points HOME and the XDG directories at a temporary directory, so nothing here
//! touches a real CloudDrop. The tests never run `clouddrop service …`, which would talk to a
//! real systemd unit if one is installed; they stop the daemon with `daemon.stop`, as
//! `clouddrop service stop` does where there is no service manager.

mod common;

use clouddrop::auth::{Token, save_token};
use clouddrop::config::{Config, Endpoints};
use common::{s3_mock, webdav_mock};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Sandbox {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        Self { config: root.join("config/config.toml"), data: root.join("data"), root, _tmp: tmp }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_clouddrop"));
        c.args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("XDG_DATA_HOME", self.root.join("home/.local/share"))
            .env("CLOUDDROP_CONFIG", &self.config)
            .env("CLOUDDROP_DATA_DIR", &self.data)
            .env("CLOUDDROP_LOG", "clouddrop=debug")
            .env_remove("SNAP_NAME")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdin(Stdio::null());
        c
    }

    /// Runs a command that must succeed; its stdout.
    fn ok(&self, args: &[&str]) -> String {
        self.ok_in(&self.root, args)
    }

    fn ok_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.cmd(args).current_dir(dir).output().unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "clouddrop {args:?} failed:\n{stderr}\n{stdout}");
        stdout.into_owned()
    }

    /// Runs a command that must fail; its error.
    fn fails(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(!out.status.success(), "clouddrop {args:?} should have failed:\n{}", String::from_utf8_lossy(&out.stdout));
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn json(&self, args: &[&str]) -> Value {
        self.json_in(&self.root, args)
    }

    fn json_in(&self, dir: &Path, args: &[&str]) -> Value {
        let text = self.ok_in(dir, args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{args:?}: {e}\n{text}"))
    }

    fn folder(&self, name: &str) -> PathBuf {
        let p = self.root.join("home").join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The default connection: Google Drive on the fake, signed in.
    fn default_on_drive(&self, base: &str, folder: &Path) {
        let cfg = Config {
            sync_dir: folder.to_path_buf(),
            client_id: "cid.apps.googleusercontent.com".into(),
            client_secret: "secret".into(),
            settle_secs: 1,
            notifications: false,
            chunk_size: 256 * 1024,
            endpoints: Endpoints {
                auth_url: format!("{base}/auth"),
                token_url: format!("{base}/token"),
                api_base: format!("{base}/drive/v3"),
                upload_base: format!("{base}/upload/drive/v3"),
            },
            ..Config::default()
        };
        cfg.save_to(&self.config).unwrap();
        save_token(
            &self.data.join("token.json"),
            &Token { access_token: "stale".into(), refresh_token: "good-refresh".into(), expires_at: 0 },
        )
        .unwrap();
    }

    /// `connections add` for a WebDAV server on the fake.
    fn add_webdav(&self, name: &str, url: &str, folder: &Path) -> String {
        let out = self.ok(&[
            "connections", "add", name, "--provider", "webdav", "--folder", &folder.to_string_lossy(),
            "--set", &format!("url={url}"), "--set", &format!("user={}", webdav_mock::USER),
            "--set", "folder=Backups", "--secret", &format!("password={}", webdav_mock::PASSWORD),
            "--no-bookmark",
        ]);
        self.quick(name);
        out
    }

    /// Settles fast and stays quiet (no desktop notifications from the tests).
    fn quick(&self, name: &str) {
        self.ok(&["--connection", name, "config", "set", "settle_secs", "1"]);
        self.ok(&["--connection", name, "config", "set", "notifications", "false"]);
    }

    fn daemon(&self) -> Daemon {
        let log = std::fs::File::create(self.root.join("daemon.log")).unwrap();
        let child = self.cmd(&["run"]).stdout(Stdio::null()).stderr(log).spawn().unwrap();
        Daemon { child, log: self.root.join("daemon.log"), stop: self.data.join("daemon.stop") }
    }

    fn connection_data(&self, name: &str) -> PathBuf {
        self.data.join("connections").join(name)
    }

    fn connection_config(&self, name: &str) -> PathBuf {
        self.config.parent().unwrap().join("connections").join(name).join("config.toml")
    }
}

struct Daemon {
    child: Child,
    log: PathBuf,
    stop: PathBuf,
}

impl Daemon {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Asks it to stop (`daemon.stop`, what `clouddrop service stop` writes without systemd).
    fn stop(mut self) {
        std::fs::write(&self.stop, b"").unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "the daemon failed:\n{}", self.log());
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let log = self.log();
        let _ = self.child.kill();
        panic!("the daemon didn't stop on request:\n{log}");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn wait(what: &str, daemon: &Daemon, cond: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(45);
    while !cond() {
        if Instant::now() > until {
            panic!("timed out waiting for {what}; daemon log:\n{}", daemon.log());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn status_file(data: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(data.join("status.json")).ok()?).ok()
}

fn phase(data: &Path) -> Option<String> {
    status_file(data).and_then(|v| v["phase"].as_str().map(str::to_string))
}

fn names(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect()
}

fn entry_state(db: &Path, rel: &str) -> Option<String> {
    let db = clouddrop::db::Db::open(db).ok()?;
    db.get(rel).ok()?.map(|e| e.state.as_str().to_string())
}

/// Google Drive and WebDAV side by side; S3 added and WebDAV removed while the daemon runs, the
/// others carrying on untouched.
#[tokio::test(flavor = "multi_thread")]
async fn two_clouds_side_by_side() {
    let drive = common::start_mock().await;
    let dav = webdav_mock::start(false).await;
    let s3 = s3_mock::start().await;
    let sb = Sandbox::new();
    let (home, work, photos) = (sb.folder("CloudDrop"), sb.folder("Work"), sb.folder("Photos"));
    let drive_base = drive.lock().unwrap().base.clone();
    sb.default_on_drive(&drive_base, &home);
    let added = sb.add_webdav("work", &dav.lock().unwrap().url(), &work);
    assert!(added.contains("Added \"work\""), "{added}");

    let list = sb.json(&["connections", "--json"]);
    assert_eq!(names(&list), ["default", "work"]);
    assert_eq!(list[1]["drive_folder"], "Work", "a new connection's cloud folder is named after its folder");
    assert_eq!(list[0]["label"], "CloudDrop", "the default connection is shown as its folder");
    assert_eq!((list[1]["label"].as_str(), list[1]["provider"].as_str()), (Some("work"), Some("webdav")));
    assert_eq!(list[1]["configured"], true);
    assert_eq!((list[0]["signed_in"].as_bool(), list[1]["signed_in"].as_bool()), (Some(true), Some(true)));
    assert_eq!(Path::new(list[1]["data_dir"].as_str().unwrap()), sb.connection_data("work"));
    assert!(sb.connection_config("work").is_file());

    std::fs::write(home.join("a.txt"), "for Drive").unwrap();
    std::fs::write(work.join("w.txt"), "for WebDAV").unwrap();
    let daemon = sb.daemon();
    wait("both clouds to have their file", &daemon, || {
        drive.lock().unwrap().lookup("CloudDrop/a.txt").is_some_and(|i| i.content == b"for Drive")
            && dav.lock().unwrap().file("/Backups/Work/w.txt").as_deref() == Some(b"for WebDAV".as_slice())
    })
    .await;
    // Each cloud got only its own folder.
    assert!(dav.lock().unwrap().file("/Backups/Work/a.txt").is_none());
    assert!(drive.lock().unwrap().lookup("CloudDrop/w.txt").is_none());
    let beat: Value = serde_json::from_slice(&std::fs::read(sb.data.join("daemon.json")).unwrap()).unwrap();
    assert_eq!(beat["connections"], serde_json::json!(["default", "work"]));
    let pid = beat["pid"].as_u64().unwrap();
    wait("both engines to report", &daemon, || status_file(&sb.data).is_some() && status_file(&sb.connection_data("work")).is_some()).await;

    // status --json: today's object for the default connection; --all: one per connection.
    let one = sb.json(&["status", "--json"]);
    assert_eq!((one["connection"].as_str(), one["provider"].as_str()), (Some("default"), Some("gdrive")));
    for key in ["configured", "signed_in", "paused", "root_id", "folder", "drive_folder", "daemon", "files", "errors"] {
        assert!(one.get(key).is_some(), "status --json lost {key}: {one}");
    }
    let inside = sb.json_in(&work, &["status", "--json"]);
    assert_eq!(inside["connection"], "work", "the folder you're in picks the connection");
    let all = sb.json(&["status", "--json", "--all"]);
    assert_eq!(names_by(&all, "connection"), ["default", "work"]);
    assert_eq!(all[1]["provider_name"], "WebDAV");
    wait("work's upload counted", &daemon, || sb.json(&["--connection", "work", "status", "--json"])["files"]["synced"] == 1).await;

    // Pausing one connection leaves the other uploading.
    sb.ok(&["--connection", "work", "pause"]);
    wait("work paused", &daemon, || phase(&sb.connection_data("work")).as_deref() == Some("paused")).await;
    std::fs::write(home.join("b.txt"), "still going").unwrap();
    std::fs::write(work.join("x.txt"), "waits").unwrap();
    wait("Drive's second file", &daemon, || drive.lock().unwrap().lookup("CloudDrop/b.txt").is_some()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(dav.lock().unwrap().file("/Backups/Work/x.txt").is_none(), "work is paused");
    assert_ne!(phase(&sb.data).as_deref(), Some("paused"));
    sb.ok(&["--connection", "work", "resume"]);
    wait("work resumed", &daemon, || dav.lock().unwrap().file("/Backups/Work/x.txt").is_some()).await;

    // A path picks its connection: freeing a file of Work frees it there (WebDAV has no hash,
    // so this downloads the cloud copy to prove it first).
    wait("w.txt synced", &daemon, || entry_state(&sb.connection_data("work").join("state.db"), "w.txt").as_deref() == Some("synced")).await;
    let freed = sb.ok(&["free", &work.join("w.txt").to_string_lossy()]);
    assert!(freed.to_lowercase().contains("freed"), "{freed}");
    assert!(!work.join("w.txt").exists());
    assert_eq!(entry_state(&sb.connection_data("work").join("state.db"), "w.txt").as_deref(), Some("cloud_only"));

    // A third connection, added while the daemon runs, starts without restarting the others.
    let default_pid = status_file(&sb.data).unwrap()["pid"].as_u64();
    let s3_base = s3.lock().unwrap().base.clone();
    sb.ok(&[
        "connections", "add", "Photos", "--provider", "s3", "--folder", &photos.to_string_lossy(),
        "--set", &format!("endpoint={s3_base}"), "--set", &format!("region={}", s3_mock::REGION),
        "--set", &format!("bucket={}", s3_mock::BUCKET), "--set", "prefix=laptop",
        "--set", &format!("access_key_id={}", s3_mock::ACCESS_KEY),
        "--secret", &format!("secret_access_key={}", s3_mock::SECRET), "--no-bookmark",
    ]);
    sb.quick("Photos");
    std::fs::write(photos.join("p.jpg"), "a photo").unwrap();
    wait("the photo in S3", &daemon, || {
        s3.lock().unwrap().content("laptop/Photos/p.jpg").as_deref() == Some(b"a photo".as_slice())
    })
    .await;
    assert_eq!(names(&sb.json(&["connections", "--json"])), ["default", "Photos", "work"], "default first, then by name");
    assert_eq!(status_file(&sb.data).unwrap()["pid"].as_u64(), default_pid);
    let beat: Value = serde_json::from_slice(&std::fs::read(sb.data.join("daemon.json")).unwrap()).unwrap();
    assert_eq!(beat["pid"].as_u64(), Some(pid), "the same daemon");

    // A folder can't be shared or nested, and names are unique ignoring case.
    let err = sb.fails(&["connections", "add", "nested", "--folder", &home.join("Inner").to_string_lossy(), "--no-bookmark"]);
    assert!(err.contains("is inside the folder of the connection \"default\""), "{err}");
    let err = sb.fails(&["connections", "add", "PHOTOS", "--folder", &sb.folder("Other").to_string_lossy(), "--no-bookmark"]);
    assert!(err.contains("already a connection named \"Photos\""), "{err}");

    // Removing one stops it; the others go on, and nothing is deleted on disk or in the cloud.
    let removed = sb.ok(&["connections", "remove", "work"]);
    assert!(removed.contains("Removed \"work\""), "{removed}");
    assert!(!sb.connection_data("work").exists() && !sb.connection_config("work").exists());
    assert_eq!(std::fs::read_to_string(work.join("x.txt")).unwrap(), "waits", "the folder keeps its files");
    assert!(dav.lock().unwrap().file("/Backups/Work/w.txt").is_some(), "the cloud keeps its files");
    wait("the heartbeat without work", &daemon, || {
        std::fs::read(sb.data.join("daemon.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|v| v["connections"] == serde_json::json!(["Photos", "default"]))
    })
    .await;
    std::fs::write(work.join("late.txt"), "not uploaded").unwrap();
    std::fs::write(home.join("c.txt"), "the default goes on").unwrap();
    wait("Drive's third file", &daemon, || drive.lock().unwrap().lookup("CloudDrop/c.txt").is_some()).await;
    assert!(dav.lock().unwrap().file("/Backups/Work/late.txt").is_none());
    assert_eq!(names(&sb.json(&["connections", "--json"])), ["default", "Photos"]);
    let err = sb.fails(&["--connection", "work", "status"]);
    assert!(err.contains("no connection named \"work\""), "{err}");

    daemon.stop();
    assert!(!sb.data.join("daemon.json").exists(), "the heartbeat goes with the daemon");
    assert!(status_file(&sb.data).is_none() && status_file(&sb.connection_data("Photos")).is_none());
}

fn names_by(list: &Value, key: &str) -> Vec<String> {
    list.as_array().unwrap().iter().map(|c| c[key].as_str().unwrap().to_string()).collect()
}

/// `connections add` for a WebDAV server that is never reached.
fn webdav_add(name: &str, folder: &Path, user: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = ["connections", "add", name, "--provider", "webdav", "--no-bookmark", "--folder"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    a.push(folder.to_string_lossy().into_owned());
    a.extend(["--set".into(), "url=http://127.0.0.1:9/dav/".into(), "--secret".into(), "password=pw".into()]);
    if let Some(u) = user {
        a.extend(["--set".into(), format!("user={u}")]);
    }
    a
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// What the CLI checks and refuses, without a daemon.
#[test]
fn connections_cli_rules() {
    let sb = Sandbox::new();
    assert!(sb.ok(&["connections"]).contains("No connections yet"));
    assert_eq!(sb.json(&["connections", "--json"]), serde_json::json!([]));
    assert_eq!(sb.json(&["status", "--json", "--all"]), serde_json::json!([]));
    let not_set_up = sb.json(&["status", "--json"]);
    assert_eq!((not_set_up["configured"].as_bool(), not_set_up["connection"].as_str()), (Some(false), Some("default")));

    let (home, other, third) = (sb.folder("CloudDrop"), sb.folder("Other"), sb.folder("Third"));
    for bad in ["default", "a/b", "_x", "dot.", "con"] {
        let err = sb.fails(&strs(&webdav_add(bad, &other, Some("me"))));
        assert!(err.contains("connection name") || err.contains("reserves") || err.contains("first connection"), "{bad}: {err}");
    }
    let err = sb.fails(&["connections", "add", "nofolder", "--provider", "webdav", "--no-bookmark"]);
    assert!(err.contains("--folder"), "{err}");
    let err = sb.fails(&strs(&webdav_add("halfway", &other, None)));
    assert!(err.contains("User name"), "{err}");
    assert!(!sb.connection_config("halfway").exists() && !sb.connection_data("halfway").exists(), "a failed add leaves nothing");

    // The default connection through `setup --connection default`, then two more.
    sb.ok(&[
        "--connection", "default", "setup", "--provider", "webdav", "--folder", &home.to_string_lossy(),
        "--set", "url=http://127.0.0.1:9/dav/", "--set", "user=me", "--secret", "password=pw", "--no-login", "--no-bookmark",
    ]);
    assert!(sb.ok(&strs(&webdav_add("Other", &other, Some("me")))).contains("Added \"Other\""));
    let err = sb.fails(&strs(&webdav_add("again", &other, Some("me"))));
    assert!(err.contains("is the folder of the connection \"Other\""), "{err}");
    let err = sb.fails(&strs(&webdav_add("around", &sb.root.join("home"), Some("me"))));
    assert!(err.contains("holds the folder"), "{err}");
    // `setup` with a new name makes a connection too.
    sb.ok(&[
        "--connection", "third", "setup", "--provider", "webdav", "--folder", &third.to_string_lossy(),
        "--set", "url=http://127.0.0.1:9/dav/", "--set", "user=me3", "--secret", "password=pw3", "--no-login", "--no-bookmark",
    ]);
    assert_eq!(names(&sb.json(&["connections", "--json"])), ["default", "Other", "third"]);
    let creds = |dir: &Path| std::fs::read_to_string(dir.join("credentials.json")).unwrap_or_default();
    assert!(creds(&sb.connection_data("third")).contains("pw3"), "secrets stay in the connection's own data");
    assert!(!creds(&sb.data).contains("pw3"));

    // Settings go to the connection named, or to the one whose folder you're in.
    sb.ok(&["--connection", "other", "config", "set", "parallel_uploads", "5"]);
    let cfg = |dir: &Path| sb.json_in(dir, &["config", "get", "--json"])["parallel_uploads"].as_u64();
    assert_eq!(cfg(&other), Some(5), "names match ignoring case");
    assert_eq!(cfg(&home), Some(2));
    assert_eq!(cfg(&sb.root), Some(2), "outside every folder: the default connection");

    let err = sb.fails(&["--connection", "nope", "pause"]);
    assert!(err.contains("no connection named \"nope\""), "{err}");
    let err = sb.fails(&["--connection", "other", "run"]);
    assert!(err.contains("every connection"), "{err}");
    let err = sb.fails(&["connections", "remove", "nope"]);
    assert!(err.contains("no connection named"), "{err}");

    // Removing the default connection keeps the others.
    sb.ok(&["connections", "remove", "default"]);
    assert!(!sb.config.exists() && !sb.data.join("credentials.json").exists());
    assert_eq!(names(&sb.json(&["connections", "--json"])), ["Other", "third"]);
    assert!(sb.connection_data("Other").join("credentials.json").is_file());
    assert!(home.is_dir(), "its folder stays");
}
