use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use clouddrop::actions::{self, Ctx};
use clouddrop::config::{self, Config};
use clouddrop::db::{Db, Kind, State};
use clouddrop::notifier::Notifier;
use clouddrop::control::{self, Phase};
use clouddrop::{auth, drive_for, engine, util};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "clouddrop",
    version,
    about = "Upload-only Google Drive folder: files added here are uploaded; deleting them locally keeps them in Drive."
)]
struct Cli {
    /// Also report the outcome as a desktop notification (used by the Files integration).
    #[arg(long, global = true)]
    notify: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write the config, create the folder and sign in to Google.
    Setup {
        #[arg(long, requires = "client_secret")]
        client_id: Option<String>,
        #[arg(long, requires = "client_id")]
        client_secret: Option<String>,
        /// The client JSON file downloaded from Google Cloud, instead of --client-id/--client-secret.
        #[arg(long, conflicts_with_all = ["client_id", "client_secret"])]
        client_json: Option<PathBuf>,
        /// Local folder to upload from (default ~/CloudDrop).
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Name of the top folder in My Drive.
        #[arg(long, default_value = "CloudDrop")]
        drive_folder: String,
        /// Skip the sign-in step.
        #[arg(long)]
        no_login: bool,
        /// Don't add the folder to the Files sidebar.
        #[arg(long)]
        no_bookmark: bool,
    },
    /// Sign in to Google (again).
    Login {
        /// Print the sign-in link instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Run the upload daemon (normally started by systemd).
    Run,
    /// Pause uploading (running uploads stop and resume later where they left off).
    Pause,
    /// Never free these folders (or files) automatically.
    Keep {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Let automatic space saving free these folders again.
    Unkeep {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Automatic space saving: "never", "now" (right after upload) or a number of days.
    AutoFree { when: String },
    /// Resume uploading.
    Resume,
    /// Show what is uploaded, waiting, cloud-only or failing.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Free up space: delete local copies that are safely in Drive.
    Free {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Download cloud-only files back (a folder restores everything inside it).
    Restore {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Open a file or folder in Google Drive in the browser.
    Open { path: PathBuf },
    /// Print the Google Drive link of a file or folder.
    Url { path: PathBuf },
    /// Re-queue failed uploads (all, or under the given paths).
    Retry { paths: Vec<PathBuf> },
    /// List cloud-only items directly inside a folder.
    LsCloud {
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Show or change settings: `config get [--json]`, `config set KEY VALUE`.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// The background service: status, start, stop, enable (at sign-in), disable.
    Service {
        action: ServiceAction,
        /// With `status`: print {"running": …, "at_login": …}.
        #[arg(long)]
        json: bool,
    },
    /// Sign out of Google: stops the service and forgets the sign-in.
    Logout,
    /// Start over in Drive after the CloudDrop folder there was deleted.
    ResetCloud {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the settings (the Google client secret is never shown).
    Get {
        #[arg(long)]
        json: bool,
    },
    /// Change a setting; the running service applies it within seconds.
    Set { key: String, value: String },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum ServiceAction {
    Status,
    Start,
    Stop,
    Enable,
    Disable,
}

fn main() {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CLOUDDROP_LOG")
                .unwrap_or_else(|_| "clouddrop=info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_target(false)
        .init();
    let notify = cli.notify;
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    match rt.block_on(dispatch(cli.cmd)) {
        Ok(msg) => {
            if !msg.is_empty() {
                println!("{msg}");
                if notify {
                    Notifier::new(true).send_blocking("CloudDrop", &msg);
                }
            }
        }
        Err(e) => {
            eprintln!("clouddrop: {e:#}");
            if notify {
                Notifier::new(true).send_blocking("CloudDrop", &format!("{e:#}"));
            }
            std::process::exit(1);
        }
    }
}


async fn login(cfg: &Config, browser: bool) -> Result<()> {
    auth::login(cfg, &auth::token_path(), |url| {
        println!("Sign in to Google to let CloudDrop upload to your Drive:\n\n  {url}\n");
        if browser {
            control::open_uri(url);
        }
        println!("Waiting for the browser…");
    })
    .await
}

fn resolve_all(ctx: &Ctx, paths: &[PathBuf]) -> Result<Vec<String>> {
    paths.iter().map(|p| ctx.resolve(p)).collect()
}

async fn dispatch(cmd: Cmd) -> Result<String> {
    match cmd {
        Cmd::Setup {
            client_id,
            client_secret,
            client_json,
            folder,
            drive_folder,
            no_login,
            no_bookmark,
        } => {
            let client = match (client_json, client_id, client_secret) {
                (Some(f), _, _) => Some(control::parse_client_json(&std::fs::read_to_string(&f)?)?),
                (None, Some(id), Some(secret)) => Some((id, secret)),
                _ => None,
            };
            let sync_dir = match folder {
                Some(f) => std::path::absolute(f)?,
                None => Config::load().map(|c| c.sync_dir).unwrap_or_else(|_| Config::default().sync_dir),
            };
            let cfg = control::apply_setup(&control::SetupParams {
                client,
                sync_dir,
                drive_folder,
            })?;
            println!("Config written to {}", config::config_path().display());
            if !no_bookmark && actions::add_bookmark(&cfg.sync_dir).unwrap_or(false) {
                println!("Added {} to the Files sidebar", cfg.sync_dir.display());
            }
            if !no_login {
                login(&cfg, true).await?;
            }
            Ok(format!(
                "CloudDrop is set up. Start it with:\n  {}\nThen drop files into {}",
                if cfg!(windows) || control::in_snap() {
                    "clouddrop service enable"
                } else {
                    "systemctl --user enable --now clouddrop"
                },
                cfg.sync_dir.display()
            ))
        }
        Cmd::Login { no_browser } => {
            let cfg = Config::load()?;
            login(&cfg, !no_browser).await?;
            Ok("Signed in.".into())
        }
        Cmd::Logout => {
            if control::daemon_running() {
                control::stop_service()?;
            }
            match std::fs::remove_file(auth::token_path()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
            Ok("Signed out. Uploads stop until you sign in again (clouddrop login).".into())
        }
        Cmd::Run => {
            // One engine at a time (the snap's service and an app-started one could
            // otherwise both run).
            let data = config::data_dir();
            std::fs::create_dir_all(&data)?;
            let lock = std::fs::File::create(data.join("engine.lock"))?;
            if lock.try_lock().is_err() {
                return Ok("CloudDrop is already running.".into());
            }
            // In the snap the engine may be started before setup is finished: wait.
            if control::in_snap() {
                while !(control::is_configured() && control::is_signed_in()) {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
            let cfg = Config::load()?;
            let db = Arc::new(Db::open_default()?);
            let drive = drive_for(&cfg, auth::token_path());
            let (tx, rx) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                #[cfg(unix)]
                {
                    let mut term =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("signal handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                }
                #[cfg(not(unix))]
                let _ = tokio::signal::ctrl_c().await;
                let _ = tx.send(true);
            });
            let opts = engine::RunOptions {
                config_file: Some(config::config_path()),
                ..Default::default()
            };
            engine::run_with_options(cfg, opts, db, drive, config::data_dir(), rx).await?;
            Ok(String::new())
        }
        Cmd::Status { json } => status(json),
        Cmd::Config { action } => match action {
            ConfigCmd::Get { json } => {
                let cfg = Config::load()?;
                let v = cfg.public_json();
                if json {
                    return Ok(serde_json::to_string_pretty(&v)?);
                }
                Ok(v.as_object()
                    .map(|o| {
                        o.iter()
                            .filter(|(k, _)| *k != "endpoints")
                            .map(|(k, v)| format!("{k} = {v}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default())
            }
            ConfigCmd::Set { key, value } => {
                let path = config::config_path();
                let mut cfg = Config::load_from(&path)?;
                cfg.set_key(&key, &value)?;
                cfg.save_to(&path)?;
                Ok(format!("{key} updated."))
            }
        },
        Cmd::Service { action, json } => match action {
            ServiceAction::Status if json => Ok(serde_json::json!({
                "running": control::daemon_running(),
                "at_login": control::enabled_at_login(),
            })
            .to_string()),
            ServiceAction::Status => Ok(format!(
                "{}{}",
                if control::daemon_running() { "running" } else { "stopped" },
                if control::enabled_at_login() { ", starts at sign-in" } else { "" }
            )),
            ServiceAction::Start => {
                control::start_service()?;
                Ok("CloudDrop started.".into())
            }
            ServiceAction::Stop => {
                control::stop_service()?;
                Ok("CloudDrop is stopping.".into())
            }
            ServiceAction::Enable => {
                control::enable_at_login()?;
                control::start_service()?;
                Ok("CloudDrop starts at sign-in.".into())
            }
            ServiceAction::Disable => {
                control::disable_at_login()?;
                Ok("CloudDrop no longer starts at sign-in.".into())
            }
        },
        Cmd::Keep { paths } => edit_keep(&paths, true),
        Cmd::Unkeep { paths } => edit_keep(&paths, false),
        Cmd::AutoFree { when } => {
            let days = match when.trim().to_lowercase().as_str() {
                "never" | "off" => None,
                "now" | "0" | "after-upload" => Some(0),
                n => Some(n.trim_end_matches('d').parse::<u32>().map_err(|_| {
                    anyhow::anyhow!("use never, now, or a number of days (e.g. 30)")
                })?),
            };
            let path = config::config_path();
            let mut cfg = Config::load_from(&path)?;
            cfg.auto_free_days = days;
            cfg.save_to(&path)?;
            restart_if_running();
            Ok(match days {
                None => "Automatic space saving is off.".into(),
                Some(0) => "Local copies will be freed right after upload.".into(),
                Some(d) => format!("Local copies will be freed {d} days after upload."),
            })
        }
        Cmd::Pause => {
            control::set_paused(&Ctx::load()?.db, true)?;
            Ok("Uploads paused.".into())
        }
        Cmd::Resume => {
            control::set_paused(&Ctx::load()?.db, false)?;
            Ok("Uploads resumed.".into())
        }
        Cmd::Free { paths } => {
            let ctx = Ctx::load()?;
            let rels = resolve_all(&ctx, &paths)?;
            let drive = drive_for(&ctx.cfg, auth::token_path());
            Ok(actions::free(&ctx, &drive, &rels).await?.summary())
        }
        Cmd::Restore { paths } => {
            let ctx = Ctx::load()?;
            let rels = resolve_all(&ctx, &paths)?;
            let drive = drive_for(&ctx.cfg, auth::token_path());
            Ok(actions::restore(&ctx, &drive, &rels).await?.summary())
        }
        Cmd::Open { path } => {
            let ctx = Ctx::load()?;
            let url = actions::url_for(&ctx, &ctx.resolve(&path)?)?;
            control::open_uri(&url);
            Ok(String::new())
        }
        Cmd::Url { path } => {
            let ctx = Ctx::load()?;
            actions::url_for(&ctx, &ctx.resolve(&path)?)
        }
        Cmd::Retry { paths } => {
            let ctx = Ctx::load()?;
            let mut n = 0;
            if paths.is_empty() {
                n = ctx.db.retry("")?;
            } else {
                for rel in resolve_all(&ctx, &paths)? {
                    n += ctx.db.retry(&rel)?;
                }
            }
            Ok(match n {
                0 => "No failed uploads.".into(),
                1 => "Retrying 1 upload.".into(),
                n => format!("Retrying {n} uploads."),
            })
        }
        Cmd::LsCloud { dir, json } => {
            let ctx = Ctx::load()?;
            let rel = match dir {
                Some(d) => ctx.resolve(&d)?,
                None => String::new(),
            };
            let items = actions::cloud_only_children(&ctx, &rel)?;
            if json {
                let v: Vec<_> = items
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "path": ctx.cfg.sync_dir.join(&e.path),
                            "name": util::name_of(&e.path),
                            "kind": if e.kind == Kind::Dir { "dir" } else { "file" },
                            "size": e.cloud_size,
                        })
                    })
                    .collect();
                return Ok(serde_json::to_string_pretty(&v)?);
            }
            Ok(items
                .iter()
                .map(|e| match e.kind {
                    Kind::Dir => format!("{}/", util::name_of(&e.path)),
                    Kind::File => format!(
                        "{}  ({})",
                        util::name_of(&e.path),
                        util::human_bytes(e.cloud_size.unwrap_or(0) as u64)
                    ),
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        Cmd::ResetCloud { yes } => {
            if !yes {
                bail!(
                    "this forgets every Drive copy CloudDrop knows about and uploads all local files again \
                     into a new top folder; cloud-only items whose Drive copies are gone are dropped. \
                     Re-run with --yes to continue"
                );
            }
            let ctx = Ctx::load()?;
            let (requeued, dropped) = actions::reset_cloud(&ctx)?;
            restart_if_running();
            Ok(format!("Queued {requeued} items for upload; dropped {dropped} cloud-only items."))
        }
    }
}

fn restart_if_running() {
    let _ = control::restart_if_running();
}

fn edit_keep(paths: &[PathBuf], keep: bool) -> Result<String> {
    let ctx = Ctx::load()?;
    let rels = resolve_all(&ctx, paths)?;
    let path = config::config_path();
    let mut cfg = ctx.cfg;
    for rel in &rels {
        if rel.is_empty() {
            bail!("pick folders inside the CloudDrop folder; to stop automatic freeing entirely use `clouddrop auto-free never`");
        }
        cfg.keep_local.retain(|k| k.trim_matches('/') != rel);
        if keep {
            cfg.keep_local.push(rel.clone());
        }
    }
    cfg.keep_local.sort();
    cfg.save_to(&path)?;
    restart_if_running();
    let names = rels.join(", ");
    Ok(if keep {
        format!("Always keeping {names} on this computer.")
    } else {
        format!("{names} can be freed automatically again.")
    })
}

fn status(json: bool) -> Result<String> {
    if !control::is_configured() {
        if json {
            // A settings file that exists but can't be read: show it, don't offer setup.
            let config_error = config::config_path()
                .exists()
                .then(|| Config::load().err().map(|e| format!("{e:#}")))
                .flatten();
            let v = serde_json::json!({
                "configured": false,
                "config_error": config_error,
                "signed_in": control::is_signed_in(),
                "has_builtin_client": control::builtin_client().is_some(),
                "daemon": null,
            });
            return Ok(serde_json::to_string_pretty(&v)?);
        }
        return Ok("CloudDrop isn't set up yet: run `clouddrop setup`.".into());
    }
    let ctx = Ctx::load()?;
    let counts = ctx.db.counts()?;
    let count = |kind: &str, state: State| {
        counts
            .iter()
            .filter(|(k, s, _)| k == kind && s == state.as_str())
            .map(|(_, _, n)| *n)
            .sum::<i64>()
    };
    let (cloud_bytes, cloud_only_bytes) = ctx.db.cloud_bytes()?;
    let errors = ctx.db.errors(20)?;
    let signed_in = auth::token_path().exists();
    let daemon = control::read_status(&config::data_dir()).filter(|s| s.is_live());
    let activity = match &daemon {
        None => format!("not running — {}", control::START_HINT),
        Some(s) => {
            let what = match s.phase {
                Phase::Starting => "starting".to_string(),
                Phase::Idle => "up to date".to_string(),
                Phase::Waiting => format!("{} files waiting", s.waiting),
                Phase::Uploading => format!("uploading {} of {} waiting files", s.uploads.len(), s.waiting),
                Phase::Paused => "paused — clouddrop resume".to_string(),
                Phase::Offline => "can't reach Google Drive, retrying".to_string(),
                Phase::NeedsSignIn => "needs sign-in — clouddrop login".to_string(),
                Phase::DriveFolderMissing => "Drive folder missing".to_string(),
            };
            match &s.message {
                Some(m) => format!("{what} ({m})"),
                None => what,
            }
        }
    };
    if json {
        let mut states = serde_json::Map::new();
        for s in [State::Synced, State::Pending, State::Uploading, State::CloudOnly, State::Error] {
            states.insert(s.as_str().into(), count("file", s).into());
        }
        let v = serde_json::json!({
            "configured": true,
            "paused": control::is_paused(&ctx.db),
            "root_id": ctx.db.meta_get("root_id")?,
            "folder": ctx.cfg.sync_dir,
            "drive_folder": ctx.cfg.drive_folder,
            "signed_in": signed_in,
            "has_builtin_client": control::builtin_client().is_some(),
            "daemon": daemon,
            "files": states,
            "cloud_bytes": cloud_bytes,
            "cloud_only_bytes": cloud_only_bytes,
            "errors": errors.iter().map(|e| serde_json::json!({"path": e.path, "error": e.error})).collect::<Vec<_>>(),
        });
        return Ok(serde_json::to_string_pretty(&v)?);
    }
    let mut out = vec![
        format!("Folder:       {}", ctx.cfg.sync_dir.display()),
        format!("Drive folder: My Drive/{}", ctx.cfg.drive_folder),
        format!("Signed in:    {}", if signed_in { "yes" } else { "no — run `clouddrop login`" }),
        format!("Service:      {activity}"),
        String::new(),
        format!("Uploaded:     {} files", count("file", State::Synced)),
        format!(
            "Waiting:      {} files",
            count("file", State::Pending) + count("file", State::Uploading)
        ),
        format!(
            "Cloud-only:   {} files ({} saved locally)",
            count("file", State::CloudOnly),
            util::human_bytes(cloud_only_bytes as u64)
        ),
        format!("Failed:       {} files", count("file", State::Error)),
        format!("In Drive:     {}", util::human_bytes(cloud_bytes as u64)),
    ];
    if !errors.is_empty() {
        out.push(String::new());
        out.push("Failures (`clouddrop retry` to try again):".into());
        for e in errors {
            out.push(format!("  {}: {}", e.path, e.error.unwrap_or_default()));
        }
    }
    Ok(out.join("\n"))
}
