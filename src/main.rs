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
    /// Start over in Drive after the CloudDrop folder there was deleted.
    ResetCloud {
        #[arg(long)]
        yes: bool,
    },
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

fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

async fn login(cfg: &Config, browser: bool) -> Result<()> {
    auth::login(cfg, &auth::token_path(), |url| {
        println!("Sign in to Google to let CloudDrop upload to your Drive:\n\n  {url}\n");
        if browser {
            open_in_browser(url);
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
                "CloudDrop is set up. Start it with:\n  systemctl --user enable --now clouddrop\n\
                 Then drop files into {}",
                cfg.sync_dir.display()
            ))
        }
        Cmd::Login { no_browser } => {
            let cfg = Config::load()?;
            login(&cfg, !no_browser).await?;
            Ok("Signed in.".into())
        }
        Cmd::Run => {
            let cfg = Config::load()?;
            let db = Arc::new(Db::open_default()?);
            let drive = drive_for(&cfg, auth::token_path());
            let (tx, rx) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                let _ = tx.send(true);
            });
            engine::run(cfg, db, drive, config::data_dir(), rx).await?;
            Ok(String::new())
        }
        Cmd::Status { json } => status(json),
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
            open_in_browser(&url);
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
            Ok(format!(
                "Queued {requeued} items for upload; dropped {dropped} cloud-only items. \
                 Restart the daemon: systemctl --user restart clouddrop"
            ))
        }
    }
}

fn restart_if_running() {
    if control::service_active() {
        let _ = control::restart_service();
    }
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
        None => "not running — systemctl --user start clouddrop".to_string(),
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
            "folder": ctx.cfg.sync_dir,
            "drive_folder": ctx.cfg.drive_folder,
            "signed_in": signed_in,
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
