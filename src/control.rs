//! Shared by the CLI and the desktop app: setup, daemon status, pause/resume,
//! and the systemd service / autostart plumbing.

use crate::config::{self, Config};
use crate::db::Db;
use crate::util::now_ms;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const SERVICE: &str = "clouddrop";
pub const APP_ID: &str = "com.dhivalabs.CloudDrop";

// ---------- daemon status ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    /// Everything uploaded.
    Idle,
    /// Files queued, waiting for their turn or for a retry.
    Waiting,
    Uploading,
    /// Paused by the user.
    Paused,
    /// Recent network/server failures; retrying with backoff.
    Offline,
    NeedsSignIn,
    /// The top folder in Drive was trashed or deleted.
    DriveFolderMissing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upload {
    pub path: String,
    pub sent: u64,
    pub total: u64,
}

/// Google Drive storage of the signed-in account, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    /// None = unlimited.
    pub limit: Option<u64>,
    /// Everything counted against the limit (Drive, Gmail, Photos).
    pub usage: u64,
    pub usage_in_drive: u64,
}

/// What the daemon is doing, written to `status.json` every couple of seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub updated_ms: i64,
    pub phase: Phase,
    pub message: Option<String>,
    pub uploads: Vec<Upload>,
    pub waiting: u64,
    /// Refreshed every few minutes; absent until first fetched.
    #[serde(default)]
    pub quota: Option<Quota>,
}

impl DaemonStatus {
    /// Written recently by a process that still exists.
    pub fn is_live(&self) -> bool {
        now_ms() - self.updated_ms < 10_000 && process_exists(self.pid)
    }
}

#[cfg(target_os = "linux")]
fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_exists(_pid: u32) -> bool {
    true
}

pub fn status_path(data_dir: &Path) -> PathBuf {
    data_dir.join("status.json")
}

pub fn read_status(data_dir: &Path) -> Option<DaemonStatus> {
    let text = std::fs::read_to_string(status_path(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_status(data_dir: &Path, status: &DaemonStatus) -> Result<()> {
    let path = status_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(status)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Asks a running daemon to exit (works where there is no service manager).
pub fn request_stop(db: &Db) -> Result<()> {
    db.meta_set("stop", "1")
}

pub fn stop_requested(db: &Db) -> bool {
    db.meta_get("stop").ok().flatten().as_deref() == Some("1")
}

pub fn clear_stop(db: &Db) -> Result<()> {
    db.meta_delete("stop")
}

const HYDRATE: &str = "hydrate/";

/// Asks the running service to download a placeholder's data (Windows: only
/// the agent that owns the sync root can be relied on to do that).
pub fn request_hydrate(db: &Db, rel: &str) -> Result<()> {
    db.meta_set(&format!("{HYDRATE}{rel}"), "1")
}

pub fn take_hydrate_requests(db: &Db) -> Vec<String> {
    db.meta_take(HYDRATE).unwrap_or_default()
}

pub fn is_paused(db: &Db) -> bool {
    db.meta_get("paused").ok().flatten().as_deref() == Some("1")
}

/// Pauses or resumes uploads; the daemon picks it up within a second.
pub fn set_paused(db: &Db, paused: bool) -> Result<()> {
    db.meta_set("paused", if paused { "1" } else { "0" })
}

// ---------- setup ----------

/// OAuth client compiled into release builds (CLOUDDROP_CLIENT_ID/SECRET at build time).
pub fn builtin_client() -> Option<(&'static str, &'static str)> {
    match (option_env!("CLOUDDROP_CLIENT_ID"), option_env!("CLOUDDROP_CLIENT_SECRET")) {
        (Some(id), Some(secret)) if !id.is_empty() && !secret.is_empty() => Some((id, secret)),
        _ => None,
    }
}

/// Reads the client id/secret from the JSON file Google Cloud offers for download.
pub fn parse_client_json(text: &str) -> Result<(String, String)> {
    let v: serde_json::Value =
        serde_json::from_str(text.trim()).context("this is not a Google client JSON file")?;
    let inner = ["installed", "web"]
        .iter()
        .find_map(|k| v.get(*k))
        .unwrap_or(&v);
    let id = inner["client_id"].as_str().unwrap_or_default().trim();
    let secret = inner["client_secret"].as_str().unwrap_or_default().trim();
    if id.is_empty() || secret.is_empty() {
        bail!("the file has no client_id/client_secret — download the JSON of a \"Desktop app\" client");
    }
    Ok((id.to_string(), secret.to_string()))
}

pub fn looks_like_client_id(id: &str) -> bool {
    let id = id.trim();
    id.ends_with(".apps.googleusercontent.com") && id.len() > ".apps.googleusercontent.com".len()
}

pub struct SetupParams {
    /// None = use the built-in client.
    pub client: Option<(String, String)>,
    pub sync_dir: PathBuf,
    pub drive_folder: String,
}

/// Writes the config (keeping other settings) and creates the folder.
pub fn apply_setup(p: &SetupParams) -> Result<Config> {
    let path = config::config_path();
    let mut cfg = Config::load_from(&path).unwrap_or_default();
    let (id, secret) = match &p.client {
        Some((id, secret)) => (id.trim().to_string(), secret.trim().to_string()),
        None => builtin_client()
            .map(|(i, s)| (i.to_string(), s.to_string()))
            .or_else(|| {
                (!cfg.client_id.is_empty()).then(|| (cfg.client_id.clone(), cfg.client_secret.clone()))
            })
            .ok_or_else(|| anyhow!("a Google client ID and secret are needed"))?,
    };
    if !looks_like_client_id(&id) {
        bail!("the client ID should end with .apps.googleusercontent.com");
    }
    if let Ok(db_root) = Db::open_default().and_then(|db| db.meta_get("sync_root"))
        && let Some(root) = db_root
        && Path::new(&root) != std::path::absolute(&p.sync_dir)?
    {
        bail!("CloudDrop already uses {root}; the folder can't be changed after setup");
    }
    cfg.client_id = id;
    cfg.client_secret = secret;
    cfg.sync_dir = std::path::absolute(&p.sync_dir)?;
    cfg.drive_folder = p.drive_folder.trim().to_string();
    cfg.validate()?;
    std::fs::create_dir_all(&cfg.sync_dir)
        .with_context(|| format!("cannot create {}", cfg.sync_dir.display()))?;
    cfg.save_to(&path)?;
    Ok(cfg)
}

pub fn is_configured() -> bool {
    Config::load().is_ok_and(|c| !c.client_id.is_empty() && !c.client_secret.is_empty())
}

/// How to start the background service, for messages.
pub const START_HINT: &str = if cfg!(windows) {
    "clouddrop service start"
} else {
    "systemctl --user start clouddrop"
};

pub fn is_signed_in() -> bool {
    crate::auth::token_path().exists()
}

pub fn sign_out() -> Result<()> {
    match std::fs::remove_file(crate::auth::token_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

// ---------- background service (systemd on Linux, login item on Windows) ----------

/// Whether the daemon is running right now (judged by its live status file).
pub fn daemon_running() -> bool {
    read_status(&config::data_dir()).is_some_and(|s| s.is_live())
}

#[cfg(windows)]
mod winsvc {
    use anyhow::{Context, Result, bail};
    use std::path::PathBuf;
    use std::process::Command;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    /// The startup task declared in the Microsoft Store package's manifest.
    const STARTUP_TASK: &str = "CloudDropAgent";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    /// The background agent (GUI subsystem, no console) if installed, else this CLI.
    fn agent() -> Result<(PathBuf, Vec<&'static str>)> {
        let me = std::env::current_exe()?;
        let agent = me.with_file_name("clouddrop-agent.exe");
        Ok(if agent.exists() { (agent, vec![]) } else { (me, vec!["run"]) })
    }

    fn reg(args: &[&str]) -> Result<bool> {
        use std::os::windows::process::CommandExt;
        Ok(Command::new("reg")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("cannot run reg.exe")?
            .status
            .success())
    }

    /// Running from the Microsoft Store package, which starts the agent at sign-in
    /// through its startup task (a Run key written from a package isn't used).
    pub fn packaged() -> bool {
        use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
        const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
        let mut len = 0u32;
        unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) == ERROR_INSUFFICIENT_BUFFER }
    }

    fn startup_task() -> Result<windows::ApplicationModel::StartupTask> {
        let id = windows::core::HSTRING::from(STARTUP_TASK);
        Ok(windows::ApplicationModel::StartupTask::GetAsync(&id)?.join()?)
    }

    pub fn enabled() -> bool {
        use windows::ApplicationModel::StartupTaskState as S;
        if packaged() {
            return startup_task()
                .ok()
                .and_then(|t| t.State().ok())
                .is_some_and(|s| s == S::Enabled || s == S::EnabledByPolicy);
        }
        reg(&["query", RUN_KEY, "/v", "CloudDrop"]).unwrap_or(false)
    }

    pub fn enable() -> Result<()> {
        use windows::ApplicationModel::StartupTaskState as S;
        if packaged() {
            return match startup_task()?.RequestEnableAsync()?.join()? {
                S::Enabled | S::EnabledByPolicy => Ok(()),
                S::DisabledByUser => {
                    bail!("starting at sign-in is turned off in Settings › Apps › Startup; turn CloudDrop on there")
                }
                S::DisabledByPolicy => bail!("your organization doesn't allow CloudDrop to start at sign-in"),
                _ => bail!("Windows didn't let CloudDrop start at sign-in"),
            };
        }
        let (exe, args) = agent()?;
        let mut cmd = format!("\"{}\"", exe.display());
        for a in args {
            cmd.push(' ');
            cmd.push_str(a);
        }
        if !reg(&["add", RUN_KEY, "/v", "CloudDrop", "/t", "REG_SZ", "/d", &cmd, "/f"])? {
            bail!("could not register CloudDrop to start at sign-in");
        }
        Ok(())
    }

    pub fn disable() -> Result<()> {
        if packaged() {
            startup_task()?.Disable()?;
            return Ok(());
        }
        reg(&["delete", RUN_KEY, "/v", "CloudDrop", "/f"])?;
        Ok(())
    }

    pub fn start() -> Result<()> {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        let (exe, args) = agent()?;
        // No inherited pipes: a caller reading this command's output (the app) would
        // otherwise wait for the long-running agent to close them.
        Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
            .spawn()
            .context("cannot start CloudDrop")?;
        Ok(())
    }
}

/// Running from the CloudDrop snap. snapd runs the background service (a user
/// daemon started at login) and systemctl isn't available inside the snap.
pub fn in_snap() -> bool {
    cfg!(unix) && std::env::var_os("SNAP_NAME").is_some_and(|n| n == "clouddrop")
}

/// Starts `clouddrop run` in the background from inside the snap.
fn snap_spawn_daemon() -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let snap = std::env::var_os("SNAP").ok_or_else(|| anyhow!("SNAP is not set"))?;
        Command::new(PathBuf::from(snap).join("bin/clouddrop"))
            .arg("run")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .context("cannot start CloudDrop")?;
    }
    Ok(())
}

/// Stops the daemon: via systemd on Linux, by request through the database elsewhere.
pub fn stop_service() -> Result<()> {
    #[cfg(unix)]
    if service_installed() && !in_snap() {
        systemctl(&["stop", SERVICE])?;
        return Ok(());
    }
    request_stop(&Db::open_default()?)
}

/// Starts the daemon at sign-in from now on.
pub fn enable_at_login() -> Result<()> {
    #[cfg(windows)]
    return winsvc::enable();
    #[cfg(unix)]
    {
        if in_snap() {
            return Ok(()); // snapd starts it at every login
        }
        if !service_installed() {
            bail!("the clouddrop service is not installed");
        }
        systemctl(&["daemon-reload"])?;
        if !systemctl(&["enable", SERVICE])? {
            bail!("could not enable the clouddrop service");
        }
        Ok(())
    }
}

pub fn disable_at_login() -> Result<()> {
    #[cfg(windows)]
    return winsvc::disable();
    #[cfg(unix)]
    {
        if in_snap() {
            bail!(
                "in the snap, the background service starts at every login; to turn that off run \
                 `systemctl --user disable snap.clouddrop.daemon`"
            );
        }
        systemctl(&["disable", SERVICE])?;
        Ok(())
    }
}

pub fn enabled_at_login() -> bool {
    #[cfg(windows)]
    return winsvc::enabled();
    #[cfg(unix)]
    service_enabled()
}

/// Starts the daemon now if it isn't running.
pub fn start_service() -> Result<()> {
    if daemon_running() {
        return Ok(());
    }
    #[cfg(windows)]
    return winsvc::start();
    #[cfg(unix)]
    {
        if in_snap() {
            return snap_spawn_daemon();
        }
        if !systemctl(&["start", SERVICE])? {
            bail!("could not start the clouddrop service — see `journalctl --user -u clouddrop`");
        }
        Ok(())
    }
}

// ---------- systemd service ----------

fn systemctl(args: &[&str]) -> Result<bool> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("systemctl is not available")?;
    Ok(out.status.success())
}

pub fn service_installed() -> bool {
    if in_snap() {
        return true;
    }
    let user = dirs::config_dir().map(|d| d.join("systemd/user/clouddrop.service"));
    let candidates = [
        user,
        Some(PathBuf::from("/usr/lib/systemd/user/clouddrop.service")),
        Some(PathBuf::from("/etc/systemd/user/clouddrop.service")),
    ];
    candidates.into_iter().flatten().any(|p| p.exists())
}

pub fn service_active() -> bool {
    if in_snap() {
        return daemon_running();
    }
    systemctl(&["is-active", "--quiet", SERVICE]).unwrap_or(false)
}

pub fn service_enabled() -> bool {
    if in_snap() {
        return true;
    }
    systemctl(&["is-enabled", "--quiet", SERVICE]).unwrap_or(false)
}

/// Enables the daemon at login and (re)starts it so it picks up a new config.
pub fn enable_service() -> Result<()> {
    if in_snap() {
        return restart_service();
    }
    if !service_installed() {
        bail!("the clouddrop service is not installed — run install.sh");
    }
    systemctl(&["daemon-reload"])?;
    if !systemctl(&["enable", SERVICE])? {
        bail!("could not enable the clouddrop service");
    }
    if !systemctl(&["restart", SERVICE])? {
        bail!("could not start the clouddrop service — see `journalctl --user -u clouddrop`");
    }
    Ok(())
}

/// Restarts the service if it is running, for changes it can't apply live.
pub fn restart_if_running() -> Result<()> {
    #[cfg(unix)]
    if service_active() {
        restart_service()?;
    }
    #[cfg(windows)]
    if daemon_running() {
        stop_service()?;
        for _ in 0..40 {
            if !daemon_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        start_service()?;
    }
    Ok(())
}

pub fn restart_service() -> Result<()> {
    if in_snap() {
        if daemon_running() {
            request_stop(&Db::open_default()?)?;
            for _ in 0..40 {
                if !daemon_running() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
        return snap_spawn_daemon();
    }
    if !systemctl(&["restart", SERVICE])? {
        bail!("could not restart the clouddrop service — see `journalctl --user -u clouddrop`");
    }
    Ok(())
}

// ---------- desktop integration ----------

fn autostart_file() -> Option<PathBuf> {
    // The snap's tray app declares `autostart: clouddrop-tray.desktop`.
    let name = if in_snap() { "clouddrop-tray".to_string() } else { APP_ID.to_string() };
    dirs::config_dir().map(|d| d.join(format!("autostart/{name}.desktop")))
}

pub fn autostart_enabled() -> bool {
    autostart_file().is_some_and(|p| {
        std::fs::read_to_string(p).is_ok_and(|t| !t.contains("X-GNOME-Autostart-enabled=false"))
    })
}

/// Starts the top-bar icon at login (`exec` = the desktop app binary).
pub fn set_autostart(enabled: bool, exec: &Path) -> Result<()> {
    let file = autostart_file().ok_or_else(|| anyhow!("no config dir"))?;
    if !enabled {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\nType=Application\nName=CloudDrop\n\
             Comment=Google Drive upload status in the top bar\n\
             Exec=\"{}\" --tray\nIcon={APP_ID}\nTerminal=false\nNoDisplay=true\n\
             X-GNOME-Autostart-enabled=true\n",
            exec.display()
        ),
    )?;
    Ok(())
}

pub fn files_extension_installed() -> bool {
    let user = dirs::data_dir().map(|d| d.join("nautilus-python/extensions/clouddrop_nautilus.py"));
    [user, Some(PathBuf::from("/usr/share/nautilus-python/extensions/clouddrop_nautilus.py"))]
        .into_iter()
        .flatten()
        .any(|p| p.exists())
}

/// Quits Files so it reloads extensions next time it opens (open windows close).
pub fn restart_files() -> Result<()> {
    Command::new("nautilus")
        .arg("-q")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("cannot run nautilus")?;
    Ok(())
}

/// Opens a link or file in its default app (the browser for web links).
pub fn open_uri(uri: &str) {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", uri]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(uri);
        c
    };
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_google_client_json() {
        let installed = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"p","client_secret":"test-secret","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(
            parse_client_json(installed).unwrap(),
            ("123-abc.apps.googleusercontent.com".into(), "test-secret".into())
        );
        let bare = r#"{"client_id":"1.apps.googleusercontent.com","client_secret":"s"}"#;
        assert!(parse_client_json(bare).is_ok());
        assert!(parse_client_json(r#"{"installed":{}}"#).is_err());
        assert!(parse_client_json("not json").is_err());
    }

    #[test]
    fn client_id_shape() {
        assert!(looks_like_client_id("12-ab.apps.googleusercontent.com"));
        assert!(!looks_like_client_id(".apps.googleusercontent.com"));
        assert!(!looks_like_client_id("not-a-client-id"));
    }

    #[test]
    fn status_liveness() {
        let mut s = DaemonStatus {
            pid: std::process::id(),
            updated_ms: now_ms(),
            phase: Phase::Idle,
            message: None,
            uploads: vec![],
            waiting: 0,
            quota: None,
        };
        assert!(s.is_live());
        s.updated_ms -= 60_000;
        assert!(!s.is_live());
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), &s).unwrap();
        assert_eq!(read_status(dir.path()), Some(s));
    }
}
