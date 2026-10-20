//! Runs the `clouddrop` command for what the app doesn't do in-process: setting up and signing
//! in to providers other than Google Drive (`setup --provider`, `login --no-browser`, `check`),
//! signing out of them, and switching providers (`service stop`, `reset-cloud --yes`).
//! docs/PROVIDERS.md ("What the apps read") is the contract.

use crate::rt;
use anyhow::{Context, Result, anyhow};
use gtk::glib;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::task::AbortHandle;

/// The CLI: `CLOUDDROP_BIN`, else `clouddrop` next to this program, else `clouddrop` on PATH.
pub fn bin() -> PathBuf {
    bin_from(std::env::var_os("CLOUDDROP_BIN"), std::env::current_exe().ok())
}

fn bin_from(env: Option<OsString>, exe: Option<PathBuf>) -> PathBuf {
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return p.into();
    }
    exe.map(|e| e.with_file_name("clouddrop"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "clouddrop".into())
}

/// What a finished command means: its trimmed stdout, or the error it printed.
fn outcome(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<String> {
    if code == Some(0) {
        return Ok(String::from_utf8_lossy(stdout).trim().to_string());
    }
    Err(anyhow!(error_message(code, &String::from_utf8_lossy(stderr))))
}

/// The error a failed command printed. The CLI's own last line is "clouddrop: …"; log lines
/// may come before it.
fn error_message(code: Option<i32>, stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(m) = lines.iter().rev().find_map(|l| l.strip_prefix("clouddrop: ")) {
        return m.to_string();
    }
    match (lines.last(), code) {
        (Some(l), _) => l.to_string(),
        (None, Some(n)) => format!("clouddrop exited with {n}"),
        (None, None) => "clouddrop was stopped".into(),
    }
}

fn run_with(bin: &Path, args: &[String]) -> Result<String> {
    let out = std::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", bin.display()))?;
    outcome(out.status.code(), &out.stdout, &out.stderr)
}

/// Runs the commands one after another off the main thread, stopping at the first that fails;
/// `done` gets the last one's output (a sentence such as `check`'s) or that error.
pub fn run_all(cmds: Vec<Vec<String>>, done: impl FnOnce(Result<String>) + 'static) {
    let bin = bin();
    rt::blocking(
        move || {
            let mut last = String::new();
            for args in &cmds {
                last = run_with(&bin, args)?;
            }
            Ok(last)
        },
        done,
    );
}

pub fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// `clouddrop setup` for a provider other than Google Drive: writes the config section
/// (`--set`) and the credentials (`--secret`). The assistant signs in and adds the Files
/// bookmark itself, so `--no-login --no-bookmark`.
pub fn setup_args(
    id: &str,
    folder: &Path,
    cloud_folder: &str,
    settings: &[(String, String)],
    secrets: &[(String, String)],
) -> Vec<String> {
    let mut a = args(&["setup", "--no-login", "--no-bookmark"]);
    a.push(format!("--provider={id}"));
    a.push(format!("--folder={}", folder.to_string_lossy()));
    a.push(format!("--drive-folder={}", cloud_folder.trim()));
    for (flag, pairs) in [("--set", settings), ("--secret", secrets)] {
        for (k, v) in pairs {
            a.push(flag.into());
            a.push(format!("{k}={v}"));
        }
    }
    a
}

/// The first web address in a line of output ("  https://…" in `login --no-browser`).
pub fn first_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|w| w.trim_start_matches(['<', '(', '"', '\'']))
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(|w| w.trim_end_matches(['.', ',', ';', ')', '>', '"', '\'']).to_string())
}

/// `clouddrop login --no-browser`, the browser sign-in of every provider but Google Drive.
/// `url_ready` gets the first web address it prints (on the main thread); `done` gets its
/// outcome (exit 0 = signed in). Aborting the handle kills the command.
pub fn login(url_ready: impl Fn(String) + 'static, done: impl FnOnce(Result<()>) + 'static) -> AbortHandle {
    let (tx, rx) = async_channel::unbounded::<String>();
    glib::spawn_future_local(async move {
        while let Ok(url) = rx.recv().await {
            url_ready(url);
        }
    });
    let (bin, a) = (bin(), args(&["login", "--no-browser"]));
    rt::run(async move { login_with(&bin, &a, tx).await }, done)
}

async fn login_with(bin: &Path, args: &[String], urls: async_channel::Sender<String>) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot run {}", bin.display()))?;
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(anyhow!("cannot read what clouddrop prints"));
    };
    let read_out = async {
        let mut lines = BufReader::new(stdout).lines();
        let mut sent = false;
        while let Ok(Some(line)) = lines.next_line().await {
            if !sent && let Some(url) = first_url(&line) {
                sent = true;
                let _ = urls.try_send(url);
            }
        }
    };
    let read_err = async {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text).await;
        text
    };
    let ((), err) = tokio::join!(read_out, read_err);
    let status = child.wait().await.context("clouddrop login")?;
    outcome(status.code(), b"", &err).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Tests that write a script and run it take turns: a script still open for writing in
    /// one test while another forks can't be executed ("Text file busy").
    static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        SPAWNING.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A stand-in `clouddrop` that runs `body` as a shell script.
    fn fake_cli(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("clouddrop");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn finds_the_cli() {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let gui = dir.path().join("clouddrop-gui");
        assert_eq!(bin_from(Some("/opt/cd".into()), Some(gui.clone())), PathBuf::from("/opt/cd"));
        assert_eq!(bin_from(None, Some(gui.clone())), PathBuf::from("clouddrop"), "on PATH when not beside the app");
        let next = fake_cli(dir.path(), "true");
        assert_eq!(bin_from(None, Some(gui.clone())), next);
        assert_eq!(bin_from(Some("".into()), Some(gui)), next, "an empty variable is ignored");
        assert_eq!(bin_from(None, None), PathBuf::from("clouddrop"));
    }

    #[test]
    fn output_and_errors() {
        assert_eq!(outcome(Some(0), b"  Connected to WebDAV.\n", b"").unwrap(), "Connected to WebDAV.");
        let log = b"2026-10-21T10:00:00Z  INFO connecting\nclouddrop: 401 Unauthorized: check the password\n";
        assert_eq!(outcome(Some(1), b"", log).unwrap_err().to_string(), "401 Unauthorized: check the password");
        assert_eq!(outcome(Some(1), b"", b"something odd\n").unwrap_err().to_string(), "something odd");
        assert_eq!(outcome(Some(3), b"out", b"").unwrap_err().to_string(), "clouddrop exited with 3");
        assert_eq!(outcome(None, b"", b"").unwrap_err().to_string(), "clouddrop was stopped");
    }

    #[test]
    fn runs_commands_in_order_and_stops_at_a_failure() {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let bin = fake_cli(
            dir.path(),
            &format!(
                "echo \"$*\" >> {}\nif [ \"$1\" = check ]; then echo 'clouddrop: no route to host' >&2; exit 1; fi\necho \"ran $1\"",
                log.display()
            ),
        );
        assert_eq!(run_with(&bin, &args(&["setup", "--provider=webdav"])).unwrap(), "ran setup");
        assert_eq!(run_with(&bin, &args(&["check"])).unwrap_err().to_string(), "no route to host");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "setup --provider=webdav\ncheck\n");
        let missing = dir.path().join("nope");
        assert!(run_with(&missing, &[]).unwrap_err().to_string().starts_with("cannot run"));
    }

    #[test]
    fn setup_arguments() {
        let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            setup_args(
                "webdav",
                Path::new("/home/me/CloudDrop"),
                " Backup ",
                &[kv("url", "https://dav.example.com"), kv("user", "me")],
                &[kv("password", "p w=1")],
            ),
            [
                "setup",
                "--no-login",
                "--no-bookmark",
                "--provider=webdav",
                "--folder=/home/me/CloudDrop",
                "--drive-folder=Backup",
                "--set",
                "url=https://dav.example.com",
                "--set",
                "user=me",
                "--secret",
                "password=p w=1",
            ]
        );
        assert_eq!(
            setup_args("onedrive", Path::new("/c d"), "CloudDrop", &[], &[]),
            ["setup", "--no-login", "--no-bookmark", "--provider=onedrive", "--folder=/c d", "--drive-folder=CloudDrop"]
        );
    }

    #[test]
    fn urls_in_cli_output() {
        assert_eq!(first_url("  https://login.example.com/auth?a=1&b=2"), Some("https://login.example.com/auth?a=1&b=2".into()));
        assert_eq!(first_url("Open http://127.0.0.1:8123/start."), Some("http://127.0.0.1:8123/start".into()));
        assert_eq!(first_url("Sign in at <https://cloud.example.com/login/v2/flow/abc>"), Some("https://cloud.example.com/login/v2/flow/abc".into()));
        assert_eq!(first_url("Sign in to OneDrive to let CloudDrop upload:"), None);
        assert_eq!(first_url("Waiting for the browser…"), None);
        assert_eq!(first_url("ftp://x https://a https://b"), Some("https://a".into()));
    }

    fn login_test(body: &str) -> (Result<()>, Vec<String>) {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), body);
        let (tx, rx) = async_channel::unbounded();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(login_with(&bin, &args(&["login", "--no-browser"]), tx));
        let mut urls = Vec::new();
        while let Ok(u) = rx.try_recv() {
            urls.push(u);
        }
        (res, urls)
    }

    #[test]
    fn browser_login_reports_the_link_and_the_outcome() {
        let (res, urls) = login_test(
            "echo 'Sign in to OneDrive to let CloudDrop upload:'; echo; echo '  https://login.example.com/a?b=c'; \
             echo 'Waiting for the browser…'; echo 'Again: https://other.example.com'; echo 'Signed in.'",
        );
        assert!(res.is_ok());
        assert_eq!(urls, ["https://login.example.com/a?b=c"], "only the first link");

        let (res, urls) = login_test("echo '  https://x.example.com'; echo 'clouddrop: access denied' >&2; exit 1");
        assert_eq!(res.unwrap_err().to_string(), "access denied");
        assert_eq!(urls.len(), 1);
    }

    #[test]
    fn cancelling_the_login_kills_the_command() {
        let _turn = turn();
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let bin = fake_cli(dir.path(), &format!("echo $$ > {}; echo https://x.example.com; exec sleep 30", pid_file.display()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = async_channel::unbounded();
        let task = rt.spawn(async move { login_with(&bin, &args(&["login"]), tx).await });
        rt.block_on(async { rx.recv().await.unwrap() });
        task.abort();
        let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_string();
        let proc = PathBuf::from(format!("/proc/{pid}"));
        let gone = || {
            // A killed child that nobody waited for yet is a zombie: gone for our purposes.
            std::fs::read_to_string(proc.join("stat")).map_or(true, |s| s.contains(") Z"))
        };
        for _ in 0..100 {
            if gone() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the login command is still running");
    }
}
