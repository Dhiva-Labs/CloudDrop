//! Startup and shutdown: one instance per user, engine on a background thread,
//! notification-area icon on the main thread.

use clouddrop::config::{self, Config};
use clouddrop::connections::Connection;
use clouddrop::db::{Db, Kind, State};
use clouddrop::store::{LocalStore, PlainStore};
use clouddrop::winstore::WinStore;
use clouddrop::{control, daemon};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::watch;

/// Set when the agent should exit (Quit from the icon, or `clouddrop service stop`).
pub static EXIT: AtomicBool = AtomicBool::new(false);

pub fn main() {
    init_logging();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--selftest") {
        let dir = args.get(2).map(std::path::PathBuf::from).unwrap_or_else(|| {
            std::env::temp_dir().join("CloudDropSelfTest")
        });
        if let Err(e) = crate::cloud::selftest(&dir) {
            tracing::error!("self-test failed: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).map(String::as_str) == Some("--unregister") {
        if let Err(e) = unregister() {
            tracing::error!("cannot remove the folder from File Explorer: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    if !single_instance() {
        return;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let (quit_tx, quit_rx) = watch::channel(false);
    let handle = rt.handle().clone();
    let background = std::thread::spawn(move || {
        background(handle, quit_rx);
        EXIT.store(true, Ordering::SeqCst);
    });
    crate::tray::run();
    let _ = quit_tx.send(true);
    let _ = background.join();
    rt.shutdown_timeout(Duration::from_secs(3));
}

fn init_logging() {
    let dir = config::base_data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("agent.log"));
    let filter = tracing_subscriber::EnvFilter::try_from_env("CLOUDDROP_LOG")
        .unwrap_or_else(|_| "clouddrop=info,clouddrop_agent=info".into());
    if let Ok(file) = file {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();
    }
}

/// `clouddrop-agent --unregister`, run by the uninstaller: every connection's folder stops
/// being a cloud folder.
fn unregister() -> anyhow::Result<()> {
    if control::daemon_running() {
        let _ = control::stop_service();
        for _ in 0..30 {
            if !control::daemon_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    crate::cloud::unregister_all()
}

/// A named mutex makes a second launch exit quietly.
fn single_instance() -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\CloudDropAgent\0".encode_utf16().collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        // The handle is deliberately kept for the life of the process.
        !h.is_null() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Every connection's engine (`daemon::supervise`), each with its folder as a File Explorer
/// cloud folder, until Quit or `clouddrop service stop`.
fn background(rt: tokio::runtime::Handle, quit: watch::Receiver<bool>) {
    // Cloud folders of connections removed while the agent wasn't running.
    crate::cloud::forget_removed();
    let handle = rt.clone();
    let supervise = daemon::supervise(
        move |conn, cfg, stop| run_connection(conn, cfg, stop, handle.clone()),
        quit,
    );
    if let Err(e) = rt.block_on(supervise) {
        tracing::error!("CloudDrop stopped: {e:#}");
    }
}

/// One connection: its folder connected as a cloud folder, and its engine until `stop`. When the
/// connection was removed, its folder stops being a cloud folder.
async fn run_connection(
    conn: Connection,
    cfg: Config,
    stop: watch::Receiver<bool>,
    rt: tokio::runtime::Handle,
) -> anyhow::Result<()> {
    let _ = std::fs::create_dir_all(&cfg.sync_dir);
    let remote = clouddrop::remote_in(&cfg, &conn.data, &conn.config);
    let link = match crate::cloud::Link::open(&conn, &cfg, remote.clone(), rt) {
        Ok(link) => Some(link),
        Err(e) => {
            tracing::warn!("File Explorer integration unavailable for {}, using plain files: {e:#}", cfg.sync_dir.display());
            None
        }
    };
    let store: Arc<dyn LocalStore> = if link.is_some() {
        Arc::new(WinStore)
    } else {
        Arc::new(PlainStore)
    };
    if link.is_some()
        && let Ok(db) = conn.open_db()
    {
        mark_existing(&db, &cfg, store.as_ref());
    }
    let result = daemon::run_engine(conn.clone(), cfg.clone(), stop, remote, store).await;
    drop(link);
    if !conn.exists() {
        crate::cloud::forget(&conn.name, &cfg.sync_dir);
    }
    result
}

/// Files uploaded before the folder became a sync root get Explorer's in-sync mark.
fn mark_existing(db: &Db, cfg: &Config, store: &dyn LocalStore) {
    for e in db.subtree("").unwrap_or_default() {
        if e.state == State::Synced
            && let Some(id) = &e.drive_id
        {
            store.synced(&cfg.sync_dir.join(&e.path), id, e.kind == Kind::Dir);
        }
    }
}
