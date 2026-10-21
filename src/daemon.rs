//! The daemon: every connection's engine in one process (docs/PROVIDERS.md › "Several clouds
//! at once"). [`supervise`] starts an engine for each connection that is set up and signed in,
//! stops the engine of a connection that was removed, starts again one that ended, and stops
//! them all when asked (`daemon.stop`, or the shutdown signal). Engines share nothing but the
//! process: each has its own database, status file, pause switch and sign-in.

use crate::config::Config;
use crate::connections::{self, Connection};
use crate::control::{self, DaemonBeat};
use crate::db::Db;
use crate::engine::{self, RunOptions};
use crate::remote::Provider;
use crate::store::LocalStore;
use crate::util::now_ms;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::Instrument;

/// How often the connections are looked at (added, removed, signed in).
const LOOK_EVERY: Duration = Duration::from_secs(2);
/// An engine that stopped on request (a restart) starts again after this.
const AFTER_STOP: Duration = Duration::from_secs(1);
/// An engine that failed starts again after this, doubling up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// An engine that ran this long before failing starts its backoff afresh.
const RAN_WELL: Duration = Duration::from_secs(120);

/// What an engine was started with; when the config changes these, it is started again.
#[derive(Debug, Clone, PartialEq)]
struct Started {
    folder: PathBuf,
    provider: Provider,
    drive_folder: String,
}

impl Started {
    fn of(cfg: &Config) -> Self {
        Self { folder: cfg.sync_dir.clone(), provider: cfg.provider, drive_folder: cfg.drive_folder.clone() }
    }
}

struct Running {
    started: Started,
    since: Instant,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<()>>,
}

#[derive(Default)]
struct Held {
    not_before: Option<Instant>,
    failures: u32,
}

/// The connection has what its engine needs: a complete config and a sign-in.
pub fn ready(conn: &Connection, cfg: &Config) -> bool {
    control::configured(cfg) && control::signed_in(cfg.provider, &conn.data)
}

/// Runs one engine per connection until `shutdown` flips or `daemon.stop` appears. `launch`
/// runs one connection's engine until its receiver flips (see [`run_engine`]).
pub async fn supervise<L, F>(mut launch: L, mut shutdown: watch::Receiver<bool>) -> Result<()>
where
    L: FnMut(Connection, Config, watch::Receiver<bool>) -> F,
    F: Future<Output = Result<()>> + Send + 'static,
{
    // A request left over from before this run must not stop it.
    let _ = std::fs::remove_file(control::daemon_stop_file());
    let mut running: HashMap<String, Running> = HashMap::new();
    let mut held: HashMap<String, Held> = HashMap::new();
    let mut overlapping: HashSet<String> = HashSet::new();
    loop {
        reap(&mut running, &mut held).await;
        let conns = connections::all();
        for (name, r) in &running {
            if !conns.iter().any(|c| &c.name == name) {
                tracing::info!("the connection \"{name}\" was removed; stopping its engine");
                let _ = r.stop.send(true);
            }
        }
        let mut folders: Vec<(String, PathBuf)> = Vec::new();
        for conn in conns {
            let Ok(cfg) = conn.load() else {
                continue;
            };
            if let Some((other, _)) = folders.iter().find(|(_, f)| connections::folders_overlap(f, &cfg.sync_dir)) {
                if overlapping.insert(conn.name.clone()) {
                    tracing::warn!(
                        "not uploading the connection \"{}\": its folder {} overlaps the one of \"{other}\"",
                        conn.name,
                        cfg.sync_dir.display()
                    );
                }
                if let Some(r) = running.get(&conn.name) {
                    let _ = r.stop.send(true);
                }
                continue;
            }
            overlapping.remove(&conn.name);
            folders.push((conn.name.clone(), cfg.sync_dir.clone()));
            let started = Started::of(&cfg);
            if let Some(r) = running.get(&conn.name) {
                if r.started != started && !*r.stop.borrow() {
                    tracing::info!("the folder, cloud or cloud folder of \"{}\" changed; restarting its engine", conn.name);
                    let _ = r.stop.send(true);
                    held.entry(conn.name.clone()).or_default().not_before = None;
                }
                continue;
            }
            if !ready(&conn, &cfg) || held.get(&conn.name).and_then(|h| h.not_before).is_some_and(|t| Instant::now() < t) {
                continue;
            }
            let (stop, rx) = watch::channel(false);
            let span = if conn.is_default() {
                tracing::Span::none()
            } else {
                tracing::info_span!("connection", name = %conn.name)
            };
            tracing::info!("starting the engine of \"{}\" ({})", conn.name, cfg.provider.display_name());
            let name = conn.name.clone();
            let task = tokio::spawn(launch(conn, cfg, rx).instrument(span));
            running.insert(name, Running { started, since: Instant::now(), stop, task });
        }
        let mut names: Vec<String> = running.iter().filter(|(_, r)| !*r.stop.borrow()).map(|(n, _)| n.clone()).collect();
        names.sort();
        let beat = DaemonBeat { pid: std::process::id(), updated_ms: now_ms(), connections: names };
        if let Err(e) = std::fs::create_dir_all(crate::config::base_data_dir()).map_err(anyhow::Error::from).and_then(|_| control::write_daemon(&beat)) {
            tracing::debug!("cannot write the daemon's heartbeat: {e:#}");
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(LOOK_EVERY) => {}
        }
        if *shutdown.borrow() {
            break;
        }
        if control::daemon_stop_file().exists() {
            let _ = std::fs::remove_file(control::daemon_stop_file());
            tracing::info!("stopping on request");
            break;
        }
    }
    for r in running.values() {
        let _ = r.stop.send(true);
    }
    for (name, r) in running {
        match tokio::time::timeout(Duration::from_secs(15), r.task).await {
            Ok(Ok(Err(e))) => tracing::warn!("the engine of \"{name}\" stopped with an error: {e:#}"),
            Ok(_) => {}
            Err(_) => tracing::warn!("the engine of \"{name}\" didn't stop in time"),
        }
    }
    let _ = std::fs::remove_file(control::daemon_file());
    Ok(())
}

/// Collects the engines that ended and decides when each may start again.
async fn reap(running: &mut HashMap<String, Running>, held: &mut HashMap<String, Held>) {
    let ended: Vec<String> = running.iter().filter(|(_, r)| r.task.is_finished()).map(|(n, _)| n.clone()).collect();
    for name in ended {
        let r = running.remove(&name).expect("listed");
        let h = held.entry(name.clone()).or_default();
        let failed = match r.task.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("{e:#}")),
            Err(e) => Some(format!("{e}")),
        };
        match failed {
            None => {
                h.failures = 0;
                h.not_before = Some(Instant::now() + AFTER_STOP);
            }
            Some(why) => {
                if r.since.elapsed() >= RAN_WELL {
                    h.failures = 0;
                }
                h.failures += 1;
                let wait = (FIRST_BACKOFF * 2u32.saturating_pow(h.failures - 1)).min(MAX_BACKOFF);
                tracing::warn!("the engine of \"{name}\" stopped: {why}; trying again in {}s", wait.as_secs());
                h.not_before = Some(Instant::now() + wait);
            }
        }
    }
}

/// One connection's engine, with its own database, client, status file and notifications,
/// until `stop` flips. `store` is how its local copies are kept (Windows: its sync root's
/// placeholders).
pub async fn run_engine(conn: Connection, cfg: Config, stop: watch::Receiver<bool>, store: Arc<dyn LocalStore>) -> Result<()> {
    std::fs::create_dir_all(&conn.data)?;
    let db = Arc::new(Db::open(&conn.db_path())?);
    let remote = crate::remote_in(&cfg, &conn.data, &conn.config);
    let opts = RunOptions { config_file: Some(conn.config.clone()), store, label: conn.notification_label() };
    engine::run_with_options(cfg, opts, db, remote, conn.data.clone(), stop).await
}
