use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use clouddrop::actions::{self, Ctx};
use clouddrop::config::{self, Config};
use clouddrop::db::{Db, Kind, State};
use clouddrop::notifier::Notifier;
use clouddrop::control::{self, Phase};
use clouddrop::remote::{Provider, Remote, RemoteError, SignIn};
use clouddrop::connections::{self, Connection};
use clouddrop::{auth, daemon, engine, oauth, remote_for, util};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "clouddrop",
    version,
    about = "Upload-only cloud folder (Google Drive and others): files added here are uploaded; deleting them locally keeps them in the cloud."
)]
struct Cli {
    /// Also report the outcome as a desktop notification (used by the Files integration).
    #[arg(long, global = true)]
    notify: bool,
    /// The connection (folder and cloud) to work on: a name from `clouddrop connections`.
    /// Default: the one whose folder holds the path given, else the current directory, else "default".
    #[arg(long, global = true, value_name = "NAME")]
    connection: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// What `setup` and `connections add` write.
#[derive(clap::Args)]
struct SetupArgs {
    /// Where to upload: an id from `clouddrop providers` (default: Google Drive, or the one set up before).
    #[arg(long)]
    provider: Option<String>,
    /// A setting of the provider (its config section), e.g. --set url=https://cloud.example.com/remote.php/dav/files/me
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = key_value)]
    settings: Vec<(String, String)>,
    /// A secret of the provider (password, key), kept in the credentials file, never in the config.
    #[arg(long = "secret", value_name = "KEY=VALUE", value_parser = key_value)]
    secrets: Vec<(String, String)>,
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
    /// Name of the top folder in the cloud (in My Drive for Google Drive).
    #[arg(long, default_value = "CloudDrop")]
    drive_folder: String,
    /// Don't add the folder to the Files sidebar.
    #[arg(long)]
    no_bookmark: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write the config, create the folder and sign in (to Google Drive unless --provider says otherwise).
    Setup {
        #[command(flatten)]
        args: SetupArgs,
        /// Skip the sign-in step.
        #[arg(long)]
        no_login: bool,
    },
    /// List the connections (each a folder uploading to a cloud), add one, or remove one.
    Connections {
        #[command(subcommand)]
        action: Option<ConnectionsCmd>,
        #[arg(long)]
        json: bool,
    },
    /// Sign in again, for providers that sign in through the browser.
    Login {
        /// Print the sign-in link instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// List the clouds CloudDrop can upload to, and how each signs in.
    Providers {
        #[arg(long)]
        json: bool,
    },
    /// Connect with the saved settings and say whether it works.
    Check {
        #[arg(long)]
        json: bool,
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
        /// Every connection (with --json: an array of them).
        #[arg(long)]
        all: bool,
    },
    /// Free up space: delete local copies that are safely in the cloud.
    Free {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Download cloud-only files back (a folder restores everything inside it).
    Restore {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Open a file or folder in the cloud's web page (Google Drive, …).
    Open { path: PathBuf },
    /// Print the web link of a file or folder in the cloud.
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
    /// Sign out: stops the service and forgets the sign-in.
    Logout,
    /// Start over in the cloud after the CloudDrop folder there was deleted.
    ResetCloud {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ConnectionsCmd {
    /// Upload another folder to a cloud: writes the new connection's settings (sign in with
    /// `clouddrop login --connection NAME`).
    Add {
        /// Its name: letters, digits, spaces, '-', '_' or '.'.
        name: String,
        #[command(flatten)]
        args: SetupArgs,
    },
    /// Stop uploading a folder and forget its settings and what was uploaded. Nothing in the
    /// folder or in the cloud is deleted.
    Remove { name: String },
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
    match rt.block_on(dispatch(cli.connection, cli.cmd)) {
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

/// Parses `key=value` (--set, --secret).
fn key_value(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
        _ => Err("expected KEY=VALUE".into()),
    }
}

/// Runs the provider's browser sign-in. None: it has none (its setup saved what it needs);
/// the text says what to do instead.
async fn login(cfg: &Config, browser: bool) -> Result<Option<String>> {
    let p = cfg.provider;
    let (id, name) = (p.as_str(), p.display_name());
    let show = |url: &str, intro: String| {
        println!("{intro}\n\n  {url}\n");
        if browser {
            control::open_uri(url);
        }
        println!("Waiting for the browser…");
    };
    match &p.info().sign_in {
        SignIn::Google => {
            auth::login(cfg, &auth::token_path(), |url| {
                show(url, "Sign in to Google to let CloudDrop upload to your Drive:".into())
            })
            .await?;
        }
        SignIn::OAuth(_) => {
            let client = oauth::client_for(cfg, p).ok_or_else(|| {
                anyhow!("this build has no {name} client id: set one with `clouddrop setup --provider {id} --set client_id=…`")
            })?;
            oauth::login(&client, &oauth::token_path(id), |url| {
                show(url, format!("Sign in to {name} to let CloudDrop upload there:"))
            })
            .await?;
        }
        SignIn::NextcloudLoginFlow => {
            let server = cfg.nextcloud.url.trim();
            if server.is_empty() {
                bail!("set the server first: `clouddrop setup --provider {id} --set url=https://cloud.example.com`");
            }
            let flow = clouddrop::webdav::nextcloud_login_start(server).await?;
            show(&flow.login_url, format!("Sign in to {name} and grant CloudDrop access:"));
            let login = clouddrop::webdav::nextcloud_login_poll(&flow).await?;
            let mut new = cfg.clone();
            new.nextcloud = login.settings(&cfg.nextcloud);
            new.save_to(&config::config_path())?;
            clouddrop::credentials::save(id, login.secrets())?;
            println!("Signed in to {} as {}.", login.server, login.login_name);
        }
        SignIn::Form(_) => {
            return Ok(Some(format!(
                "{name} has no browser sign-in, so there is nothing to do here: `clouddrop setup --provider {id}` \
                 saves its settings (--set) and password (--secret), and `clouddrop check` tests them."
            )));
        }
    }
    Ok(None)
}

/// Connects with the saved settings: (whether the top folder exists, storage).
async fn check(remote: &Remote, cfg: &Config) -> Result<(bool, Option<clouddrop::remote::StorageQuota>), RemoteError> {
    let top = remote.top_parent().await?;
    let name = engine::cloud_safe_name(&cfg.drive_folder, remote.caps().forbidden_chars);
    let exists = !remote.find_child(&name, &top, true).await?.is_empty();
    let quota = match remote.storage_quota().await {
        Ok(q) => Some(q),
        Err(RemoteError::Auth(m)) => return Err(RemoteError::Auth(m)),
        Err(_) => None,
    };
    Ok((exists, quota))
}

fn error_kind(e: &RemoteError) -> &'static str {
    match e {
        RemoteError::Auth(_) => "needs_sign_in",
        RemoteError::Transient(_) | RemoteError::RateLimited { .. } => "offline",
        RemoteError::Quota(_) => "storage_full",
        RemoteError::NotFound => "not_found",
        RemoteError::Permanent(_) => "error",
    }
}

fn resolve_all(ctx: &Ctx, paths: &[PathBuf]) -> Result<Vec<String>> {
    paths.iter().map(|p| ctx.resolve(p)).collect()
}

/// The paths a command names, which pick its connection when --connection doesn't.
fn paths_of(cmd: &Cmd) -> Vec<PathBuf> {
    match cmd {
        Cmd::Free { paths }
        | Cmd::Restore { paths }
        | Cmd::Keep { paths }
        | Cmd::Unkeep { paths }
        | Cmd::Retry { paths } => paths.clone(),
        Cmd::Open { path } | Cmd::Url { path } => vec![path.clone()],
        Cmd::LsCloud { dir: Some(d), .. } => vec![d.clone()],
        _ => Vec::new(),
    }
}

/// Writes the selected connection's config, creates its folder and (unless `no_login`) signs in.
async fn setup(a: SetupArgs, no_login: bool) -> Result<String> {
    let SetupArgs {
        provider,
        settings,
        secrets,
        client_id,
        client_secret,
        client_json,
        folder,
        drive_folder,
        no_bookmark,
    } = a;
    let existing = Config::load().ok();
    let provider = match provider {
        Some(id) => Provider::parse(&id)
            .ok_or_else(|| anyhow!("unknown provider {id:?}; `clouddrop providers` lists them"))?,
        None => existing.as_ref().map(|c| c.provider).unwrap_or_default(),
    };
    let sync_dir = match folder {
        Some(f) => connections::lexical(&f),
        None => existing.as_ref().map(|c| c.sync_dir.clone()).unwrap_or_else(|| Config::default().sync_dir),
    };
    let cfg = if provider == Provider::GoogleDrive {
        if !settings.is_empty() || !secrets.is_empty() {
            bail!("Google Drive has no --set or --secret settings; use --client-id/--client-secret or --client-json");
        }
        if existing.as_ref().is_some_and(|c| c.provider != provider) {
            let path = config::config_path();
            let mut cfg = Config::load_from(&path)?;
            control::switch_provider(&mut cfg, provider)?;
            cfg.save_to(&path)?;
        }
        let client = match (client_json, client_id, client_secret) {
            (Some(f), _, _) => Some(control::parse_client_json(&std::fs::read_to_string(&f)?)?),
            (None, Some(id), Some(secret)) => Some((id, secret)),
            _ => None,
        };
        control::apply_setup(&control::SetupParams {
            client,
            sync_dir,
            drive_folder,
        })?
    } else {
        if client_id.is_some() || client_json.is_some() {
            bail!("--client-id and --client-json are for Google Drive; other providers take --set client_id=…");
        }
        control::apply_provider_setup(&control::ProviderSetup {
            provider,
            sync_dir,
            drive_folder,
            settings,
            secrets,
        })?
    };
    println!("Config written to {}", config::config_path().display());
    if let Some(old) = control::bound_elsewhere(cfg.provider) {
        println!(
            "This folder has uploaded to {old} so far. Run `clouddrop reset-cloud --yes` to upload \
             everything to {} instead; nothing is deleted from {old}.",
            cfg.provider.display_name()
        );
    }
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

async fn dispatch(connection: Option<String>, cmd: Cmd) -> Result<String> {
    match &cmd {
        Cmd::Run if connection.is_some() => bail!("`clouddrop run` uploads every connection; leave out --connection"),
        Cmd::Run | Cmd::Connections { .. } | Cmd::Providers { .. } | Cmd::Service { .. } | Cmd::Status { all: true, .. } => {}
        _ => {
            let conn = connections::choose(connection.as_deref(), &paths_of(&cmd))?;
            if !conn.is_default() && !conn.exists() && !matches!(cmd, Cmd::Setup { .. }) {
                bail!("there is no connection named \"{}\"; `clouddrop connections` lists them", conn.name);
            }
            connections::select(&conn);
        }
    }
    match cmd {
        Cmd::Setup { args, no_login } => setup(args, no_login).await,
        Cmd::Connections { action: None, json } => list_connections(json),
        Cmd::Connections { action: Some(ConnectionsCmd::Add { name, args }), .. } => {
            connections::check_name(name.trim())?;
            let conn = Connection::named(&name)?;
            if conn.exists() {
                bail!("there is already a connection named \"{}\"", conn.name);
            }
            if args.folder.is_none() {
                bail!("a new connection needs a folder of its own: --folder DIR");
            }
            connections::select(&conn);
            let folder = connections::lexical(args.folder.as_deref().expect("checked"));
            connections::check_folder(&folder, &conn.name)?;
            let result = setup(args, true).await;
            if result.is_err() && !conn.exists() {
                // Nothing usable was written: leave no half-made connection behind.
                let _ = std::fs::remove_dir_all(&conn.data);
                if let Some(dir) = conn.config.parent() {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
            result?;
            let cfg = conn.load()?;
            let next = match cfg.provider.info().sign_in {
                SignIn::Google | SignIn::OAuth(_) | SignIn::NextcloudLoginFlow => format!(
                    "Sign in with `clouddrop login --connection \"{}\"`; CloudDrop starts uploading it then.",
                    conn.name
                ),
                SignIn::Form(_) => format!(
                    "`clouddrop check --connection \"{}\"` tests the settings; CloudDrop uploads it from now on.",
                    conn.name
                ),
            };
            Ok(format!(
                "Added \"{}\": {} uploads to {}. {next}",
                conn.name,
                cfg.sync_dir.display(),
                cfg.provider.display_name()
            ))
        }
        Cmd::Connections { action: Some(ConnectionsCmd::Remove { name }), .. } => {
            let conn = Connection::named(&name)?;
            let cfg = conn.load().ok();
            connections::remove(&conn, std::time::Duration::from_secs(10))?;
            Ok(match cfg {
                Some(c) => format!(
                    "Removed \"{}\". {} and everything in it stay where they are, and nothing was deleted from {}.",
                    conn.name,
                    c.sync_dir.display(),
                    c.provider.display_name()
                ),
                None => format!("Removed \"{}\".", conn.name),
            })
        }
        Cmd::Login { no_browser } => {
            let cfg = Config::load()?;
            Ok(login(&cfg, !no_browser).await?.unwrap_or_else(|| "Signed in.".into()))
        }
        Cmd::Logout => {
            // Only this connection's engine stops (the daemon starts it again after sign-in).
            let conn = connections::current();
            if conn.live_status().is_some() {
                control::request_stop(&conn.open_db()?)?;
            }
            control::sign_out()?;
            Ok("Signed out. Uploads stop until you sign in again (clouddrop login).".into())
        }
        Cmd::Providers { json } => {
            let all = control::providers_json();
            if json {
                return Ok(serde_json::to_string_pretty(&all)?);
            }
            Ok(Provider::ALL
                .iter()
                .map(|p| {
                    let how = match p.info().sign_in {
                        SignIn::Google | SignIn::OAuth(_) => "browser sign-in",
                        SignIn::NextcloudLoginFlow => "app password from the server",
                        SignIn::Form(_) => "settings and password",
                    };
                    let verify = if p.caps().hash.is_none() { ", verifies by download" } else { "" };
                    format!("{:<10} {} ({how}{verify})", p.as_str(), p.display_name())
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        Cmd::Check { json } => {
            let cfg = Config::load()?;
            let provider = cfg.provider;
            let name = provider.display_name();
            let remote = remote_for(&cfg, auth::token_path());
            let result = check(&remote, &cfg).await;
            let message = match &result {
                Ok((exists, quota)) => {
                    let folder = if *exists {
                        format!("The \"{}\" folder is there.", cfg.drive_folder)
                    } else {
                        format!("CloudDrop creates the \"{}\" folder when it starts.", cfg.drive_folder)
                    };
                    let storage = match quota {
                        Some((Some(limit), usage, _)) => {
                            format!(" {} of {} used.", util::human_bytes(*usage), util::human_bytes(*limit))
                        }
                        Some((None, usage, _)) if *usage > 0 => format!(" {} used.", util::human_bytes(*usage)),
                        _ => String::new(),
                    };
                    format!("Connected to {name}. {folder}{storage}")
                }
                Err(e) => engine::describe(e, provider),
            };
            if json {
                let v = match &result {
                    Ok((exists, quota)) => serde_json::json!({
                        "ok": true, "provider": provider.as_str(), "provider_name": name,
                        "message": message, "folder_exists": exists,
                        "quota": quota.map(|(limit, usage, _)| serde_json::json!({"limit": limit, "usage": usage})),
                    }),
                    Err(e) => serde_json::json!({
                        "ok": false, "provider": provider.as_str(), "provider_name": name,
                        "message": message, "error_kind": error_kind(e),
                    }),
                };
                return Ok(serde_json::to_string_pretty(&v)?);
            }
            match result {
                Ok(_) => Ok(message),
                Err(_) => bail!("cannot connect to {name}: {message}"),
            }
        }
        Cmd::Run => {
            // One daemon at a time (the snap's service and an app-started one could
            // otherwise both run).
            let data = config::base_data_dir();
            std::fs::create_dir_all(&data)?;
            let lock = std::fs::File::create(data.join("engine.lock"))?;
            if lock.try_lock().is_err() {
                return Ok("CloudDrop is already running.".into());
            }
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
            // One engine per connection, each started once it is set up and signed in.
            daemon::supervise(
                |conn, cfg, stop| {
                    let remote = clouddrop::remote_in(&cfg, &conn.data, &conn.config);
                    let store = clouddrop::store::for_folder(&cfg.sync_dir);
                    daemon::run_engine(conn, cfg, stop, remote, store)
                },
                rx,
            )
            .await?;
            Ok(String::new())
        }
        Cmd::Status { json, all: false } => status(&connections::current(), json),
        Cmd::Status { json, all: true } => {
            let all = connections::all();
            if json {
                let list = all.iter().map(status_json).collect::<Result<Vec<_>>>()?;
                return Ok(serde_json::to_string_pretty(&list)?);
            }
            if all.is_empty() {
                return Ok("CloudDrop isn't set up yet: run `clouddrop setup`.".into());
            }
            let mut out = Vec::new();
            for conn in &all {
                out.push(format!("== {} ==\n{}", conn.name, status(conn, false)?));
            }
            Ok(out.join("\n\n"))
        }
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
            if let Ok(db) = Db::open_default() {
                restart_engine(&db);
            }
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
            let remote = remote_for(&ctx.cfg, auth::token_path());
            Ok(actions::free(&ctx, &remote, &rels).await?.summary())
        }
        Cmd::Restore { paths } => {
            let ctx = Ctx::load()?;
            let rels = resolve_all(&ctx, &paths)?;
            let remote = remote_for(&ctx.cfg, auth::token_path());
            Ok(actions::restore(&ctx, &remote, &rels).await?.summary())
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
                    "this forgets every cloud copy CloudDrop knows about and uploads all local files again \
                     into a new top folder (in the configured provider); cloud-only items are dropped from \
                     the list, nothing is deleted in the cloud. Re-run with --yes to continue"
                );
            }
            let ctx = Ctx::load()?;
            let (requeued, dropped) = actions::reset_cloud(&ctx)?;
            restart_engine(&ctx.db);
            Ok(format!("Queued {requeued} items for upload; dropped {dropped} cloud-only items."))
        }
    }
}

/// Restarts the selected connection's engine, if it runs, for changes it can't apply live.
fn restart_engine(db: &Db) {
    let _ = control::restart_engine(db, &config::data_dir());
}

fn edit_keep(paths: &[PathBuf], keep: bool) -> Result<String> {
    let ctx = Ctx::load()?;
    let rels = resolve_all(&ctx, paths)?;
    let path = config::config_path();
    let mut cfg = ctx.cfg.clone();
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
    restart_engine(&ctx.db);
    let names = rels.join(", ");
    Ok(if keep {
        format!("Always keeping {names} on this computer.")
    } else {
        format!("{names} can be freed automatically again.")
    })
}

/// The connection's config when it is set up (Google Drive also needs its client).
fn configured(conn: &Connection) -> Option<Config> {
    conn.load().ok().filter(control::configured)
}

/// `status --json` of one connection.
fn status_json(conn: &Connection) -> Result<serde_json::Value> {
    let Some(cfg) = configured(conn) else {
        // A settings file that exists but can't be read: show it, don't offer setup.
        let config_error = conn.exists().then(|| conn.load().err().map(|e| format!("{e:#}"))).flatten();
        let provider = conn.load().map(|c| c.provider).unwrap_or_default();
        return Ok(serde_json::json!({
            "configured": false,
            "config_error": config_error,
            "signed_in": control::signed_in(provider, &conn.data),
            "has_builtin_client": control::builtin_client().is_some(),
            "daemon": null,
            "connection": conn.name,
            "label": conn.label(None),
        }));
    };
    let ctx = Ctx::load_for(cfg, conn.open_db()?)?;
    let counts = ctx.db.counts()?;
    let count = |state: State| {
        counts
            .iter()
            .filter(|(k, s, _)| k == "file" && s == state.as_str())
            .map(|(_, _, n)| *n)
            .sum::<i64>()
    };
    let (cloud_bytes, cloud_only_bytes) = ctx.db.cloud_bytes()?;
    let errors = ctx.db.errors(20)?;
    let provider = ctx.cfg.provider;
    let mut states = serde_json::Map::new();
    for s in [State::Synced, State::Pending, State::Uploading, State::CloudOnly, State::Error] {
        states.insert(s.as_str().into(), count(s).into());
    }
    Ok(serde_json::json!({
        "configured": true,
        "provider": provider.as_str(),
        "provider_name": provider.display_name(),
        "paused": control::is_paused(&ctx.db),
        "root_id": ctx.db.meta_get("root_id")?,
        "folder": ctx.cfg.sync_dir,
        "drive_folder": ctx.cfg.drive_folder,
        "signed_in": control::signed_in(provider, &conn.data),
        "has_builtin_client": control::builtin_client().is_some(),
        "daemon": conn.live_status(),
        "files": states,
        "cloud_bytes": cloud_bytes,
        "cloud_only_bytes": cloud_only_bytes,
        "errors": errors.iter().map(|e| serde_json::json!({"path": e.path, "error": e.error})).collect::<Vec<_>>(),
        "connection": conn.name,
        "label": conn.label(Some(&ctx.cfg)),
    }))
}

fn status(conn: &Connection, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(&status_json(conn)?)?);
    }
    let Some(cfg) = configured(conn) else {
        return Ok(if conn.is_default() {
            "CloudDrop isn't set up yet: run `clouddrop setup`.".into()
        } else {
            format!("\"{}\" isn't set up yet: run `clouddrop setup --connection \"{}\"`.", conn.name, conn.name)
        });
    };
    let ctx = Ctx::load_for(cfg, conn.open_db()?)?;
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
    let provider = ctx.cfg.provider;
    let signed_in = control::signed_in(provider, &conn.data);
    let daemon = conn.live_status();
    let activity = match &daemon {
        None if control::daemon_running() && !signed_in => "waiting for sign-in".to_string(),
        None => format!("not running — {}", control::START_HINT),
        Some(s) => {
            let what = match s.phase {
                Phase::Starting => "starting".to_string(),
                Phase::Idle => "up to date".to_string(),
                Phase::Waiting => format!("{} files waiting", s.waiting),
                Phase::Uploading => format!("uploading {} of {} waiting files", s.uploads.len(), s.waiting),
                Phase::Paused => "paused — clouddrop resume".to_string(),
                Phase::Offline => format!("can't reach {}, retrying", provider.display_name()),
                Phase::NeedsSignIn => "needs sign-in — clouddrop login".to_string(),
                Phase::DriveFolderMissing => format!("{} folder missing", engine::short_name(provider)),
            };
            match &s.message {
                Some(m) => format!("{what} ({m})"),
                None => what,
            }
        }
    };
    let google = provider == Provider::GoogleDrive;
    let mut out = vec![
        format!("Folder:       {}", ctx.cfg.sync_dir.display()),
        if google {
            format!("Drive folder: My Drive/{}", ctx.cfg.drive_folder)
        } else {
            format!("Cloud folder: {}/{}", provider.display_name(), ctx.cfg.drive_folder)
        },
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
        format!("{}{}", if google { "In Drive:     " } else { "In the cloud: " }, util::human_bytes(cloud_bytes as u64)),
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

/// `clouddrop connections [--json]`.
fn list_connections(json: bool) -> Result<String> {
    let all = connections::all();
    if json {
        let list: Vec<serde_json::Value> = all
            .iter()
            .map(|c| {
                let cfg = c.load();
                let ok = cfg.as_ref().ok();
                serde_json::json!({
                    "name": c.name,
                    "label": c.label(ok),
                    "folder": ok.map(|c| c.sync_dir.clone()),
                    "provider": ok.map(|c| c.provider.as_str()),
                    "provider_name": ok.map(|c| c.provider.display_name()),
                    "drive_folder": ok.map(|c| c.drive_folder.clone()),
                    "config": c.config,
                    "data_dir": c.data,
                    "configured": ok.is_some_and(control::configured),
                    "error": cfg.as_ref().err().map(|e| format!("{e:#}")),
                })
            })
            .collect();
        return Ok(serde_json::to_string_pretty(&list)?);
    }
    if all.is_empty() {
        return Ok("No connections yet: `clouddrop setup` sets up the first one.".into());
    }
    let width = all.iter().map(|c| c.name.chars().count()).max().unwrap_or(0);
    Ok(all
        .iter()
        .map(|c| match c.load() {
            Ok(cfg) => {
                let state = match c.live_status() {
                    Some(s) => format!("{:?}", s.phase).to_lowercase(),
                    None => "not running".into(),
                };
                format!(
                    "{:<width$}  {} → {} ({state})",
                    c.name,
                    cfg.sync_dir.display(),
                    cfg.provider.display_name()
                )
            }
            Err(e) => format!("{:<width$}  can't be read: {e:#}", c.name),
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
