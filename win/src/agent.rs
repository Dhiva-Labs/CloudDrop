//! Startup and shutdown: one instance per user, engine on a background thread,
//! notification-area icon on the main thread.

use clouddrop::config::{self, Config};
use clouddrop::db::{Db, Kind, State};
use clouddrop::store::{LocalStore, PlainStore};
use clouddrop::winstore::WinStore;
use clouddrop::{auth, control, engine};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::watch;

/// Set when the agent should exit (Quit from the icon, or `clouddrop service stop`).
pub static EXIT: AtomicBool = AtomicBool::new(false);

pub fn main() {
    init_logging();
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
    let dir = config::data_dir();
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

fn background(rt: tokio::runtime::Handle, quit: watch::Receiver<bool>) {
    // The app writes the config and the sign-in; wait for both.
    while !(control::is_configured() && control::is_signed_in()) {
        if *quit.borrow() || EXIT.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => return tracing::error!("cannot load settings: {e:#}"),
    };
    let _ = std::fs::create_dir_all(&cfg.sync_dir);
    let db = match Db::open_default() {
        Ok(db) => Arc::new(db),
        Err(e) => return tracing::error!("cannot open the state database: {e:#}"),
    };
    let drive = clouddrop::drive_for(&cfg, auth::token_path());

    let connection = match crate::cloud::connect(&cfg, drive.clone(), rt.clone()) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("File Explorer integration unavailable, using plain files: {e:#}");
            None
        }
    };
    let store: Arc<dyn LocalStore> = if connection.is_some() {
        Arc::new(WinStore)
    } else {
        Arc::new(PlainStore)
    };
    if connection.is_some() {
        mark_existing(&db, &cfg, store.as_ref());
    }

    let opts = engine::RunOptions {
        config_file: Some(config::config_path()),
        store,
    };
    if let Err(e) = rt.block_on(engine::run_with_options(
        cfg,
        opts,
        db,
        drive,
        config::data_dir(),
        quit,
    )) {
        tracing::error!("engine stopped: {e:#}");
    }
    drop(connection);
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
