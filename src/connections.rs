//! Connections: one sync folder uploading to one provider, with its own config file and data
//! directory (docs/PROVIDERS.md › "Several clouds at once").
//!
//! The default connection is config.toml and the data directory as they always were
//! (`CLOUDDROP_CONFIG`, `CLOUDDROP_DATA_DIR`). Every other connection has
//! `connections/<name>/config.toml` beside config.toml and `connections/<name>/` in the data
//! directory. A connection exists once its config file does.
//!
//! The CLI and the desktop app work on one connection at a time: [`select`] makes
//! [`crate::config::config_path`] and [`crate::config::data_dir`] (and everything built on them:
//! `Config::load`, `Db::open_default`, token and credentials files) that connection's. The daemon
//! runs every connection at once and passes each one's paths explicitly.

use crate::config::{self, Config};
use crate::control::{self, DaemonStatus};
use crate::db::Db;
use anyhow::{Result, anyhow, bail};
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;

/// The name of the connection that was the whole installation before there were several.
pub const DEFAULT: &str = "default";
const MAX_NAME: usize = 32;
const DIR: &str = "connections";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    /// Its config.toml.
    pub config: PathBuf,
    /// Its data directory: state.db, status.json, the sign-in, credentials.json.
    pub data: PathBuf,
}

static CURRENT: RwLock<Option<Connection>> = RwLock::new(None);

/// The connection the CLI or the app works on: the one [`select`]ed, else the default one.
pub fn current() -> Connection {
    CURRENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(Connection::default_one)
}

/// Makes `conn` the one `config::config_path()` and `config::data_dir()` point at.
pub fn select(conn: &Connection) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(conn.clone());
}

/// Where the named connections' config files are: `connections/` beside config.toml.
pub fn config_root() -> PathBuf {
    config::base_config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join(DIR)
}

/// Where the named connections' data directories are: `connections/` in the data directory.
pub fn data_root() -> PathBuf {
    config::base_data_dir().join(DIR)
}

impl Connection {
    /// The installation's own config.toml and data directory.
    pub fn default_one() -> Self {
        Self {
            name: DEFAULT.into(),
            config: config::base_config_path(),
            data: config::base_data_dir(),
        }
    }

    /// The connection called `name`, existing or not: `default` (in any case) is the default
    /// one, and a name matching an existing connection's ignoring case is that one.
    pub fn named(name: &str) -> Result<Self> {
        let name = name.trim();
        if name.eq_ignore_ascii_case(DEFAULT) {
            return Ok(Self::default_one());
        }
        check_name(name)?;
        let name = existing_names()
            .into_iter()
            .find(|n| n.to_lowercase() == name.to_lowercase())
            .unwrap_or_else(|| name.to_string());
        Ok(Self {
            config: config_root().join(&name).join("config.toml"),
            data: data_root().join(&name),
            name,
        })
    }

    pub fn is_default(&self) -> bool {
        self.name == DEFAULT
    }

    /// It has a config file.
    pub fn exists(&self) -> bool {
        self.config.is_file()
    }

    pub fn load(&self) -> Result<Config> {
        Config::load_from(&self.config)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data.join("state.db")
    }

    pub fn open_db(&self) -> Result<Db> {
        Db::open(&self.db_path())
    }

    /// Google Drive's token file; other providers' sign-ins are next to it.
    pub fn token_file(&self) -> PathBuf {
        self.data.join("token.json")
    }

    /// Its engine's status, while it is running.
    pub fn live_status(&self) -> Option<DaemonStatus> {
        control::read_status(&self.data).filter(DaemonStatus::is_live)
    }

    /// What the apps call it: its name, or for the default connection its folder's name.
    pub fn label(&self, cfg: Option<&Config>) -> String {
        if !self.is_default() {
            return self.name.clone();
        }
        cfg.and_then(|c| c.sync_dir.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "CloudDrop".into())
    }

    /// What its notifications add to their titles: nothing for the default connection, so they
    /// read as they always did.
    pub fn notification_label(&self) -> Option<String> {
        (!self.is_default()).then(|| self.name.clone())
    }
}

/// A name for a new connection: 1 to 32 letters, digits, spaces, `-`, `_` or `.`, starting
/// with a letter or digit and not ending in a space or dot (Windows drops those); not a
/// Windows device name.
pub fn check_name(name: &str) -> Result<()> {
    let bad = || {
        anyhow!(
            "{name:?} can't be a connection name: use up to {MAX_NAME} letters, digits, spaces, \
             '-', '_' or '.', starting with a letter or digit"
        )
    };
    let first = name.chars().next().ok_or_else(bad)?;
    if name.chars().count() > MAX_NAME
        || !first.is_alphanumeric()
        || name.ends_with([' ', '.'])
        || !name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err(bad());
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if device {
        bail!("{name:?} is a name Windows reserves; pick another");
    }
    if name.eq_ignore_ascii_case(DEFAULT) {
        bail!("\"{DEFAULT}\" is the first connection's name; pick another");
    }
    Ok(())
}

/// A name for a new connection uploading `folder`: its folder's name made valid (characters a
/// name can't have become '-'), numbered "Work 2", "Work 3"… when taken.
pub fn suggest_name(folder: &Path) -> String {
    let base: String = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '-' })
        .collect();
    let base = base.trim_start_matches(|c: char| !c.is_alphanumeric());
    let mut base: String = base.chars().take(MAX_NAME - 3).collect();
    while base.ends_with([' ', '.']) {
        base.pop();
    }
    if check_name(&base).is_err() {
        base = if base.is_empty() { "Folder".into() } else { format!("{base} folder") };
        if check_name(&base).is_err() {
            base = "Folder".into();
        }
    }
    let taken: Vec<String> = existing_names().iter().map(|n| n.to_lowercase()).collect();
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base} {n}")))
        .find(|n| !taken.contains(&n.to_lowercase()))
        .expect("unbounded")
}

/// Leaves nothing behind of a connection whose setup didn't finish: no config file means its
/// directories hold nothing anyone needs. The default connection is never touched.
pub fn discard_unfinished(conn: &Connection) {
    if conn.is_default() || conn.exists() {
        return;
    }
    let _ = std::fs::remove_dir_all(&conn.data);
    if let Some(dir) = conn.config.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The names of the connections in `connections/` that have a config file, in no order.
fn existing_names() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(config_root()) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| e.path().join("config.toml").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| check_name(n).is_ok())
        .collect()
}

/// Every connection there is (has a config file): the default one first, then by name.
pub fn all() -> Vec<Connection> {
    let mut names = existing_names();
    names.sort_by_key(|n| (n.to_lowercase(), n.clone()));
    let default = Connection::default_one();
    default
        .exists()
        .then_some(default)
        .into_iter()
        .chain(names.into_iter().filter_map(|n| Connection::named(&n).ok()))
        .collect()
}

/// `p` made absolute and with `.` and `..` resolved without touching the disk.
pub fn lexical(p: &Path) -> PathBuf {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The folder as compared: resolved where it exists (links), ignoring case on Windows.
fn comparable(p: &Path) -> PathBuf {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| lexical(p));
    // canonicalize gives \\?\C:\… on Windows; compare plain paths.
    let text = p.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    if cfg!(windows) { PathBuf::from(text.to_lowercase()) } else { PathBuf::from(text) }
}

/// The same folder, or one inside the other.
pub fn folders_overlap(a: &Path, b: &Path) -> bool {
    let (a, b) = (comparable(a), comparable(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Another connection (not `except`) whose folder is `folder`, holds it or is inside it.
pub fn folder_in_use(folder: &Path, except: &str) -> Option<(Connection, PathBuf)> {
    all().into_iter().filter(|c| c.name != except).find_map(|c| {
        let dir = c.load().ok()?.sync_dir;
        folders_overlap(folder, &dir).then_some((c, dir))
    })
}

/// Refuses a folder another connection already uploads (or one around or inside it).
pub fn check_folder(folder: &Path, except: &str) -> Result<()> {
    if let Some((other, dir)) = folder_in_use(folder, except) {
        let what = if comparable(folder) == comparable(&dir) {
            "is"
        } else if comparable(folder).starts_with(comparable(&dir)) {
            "is inside"
        } else {
            "holds"
        };
        bail!(
            "{} {what} the folder of the connection \"{}\" ({}); each connection needs a folder of its own",
            folder.display(),
            other.name,
            dir.display()
        );
    }
    Ok(())
}

/// The connection whose sync folder holds `path`.
pub fn for_path(path: &Path) -> Option<Connection> {
    let path = comparable(path);
    all().into_iter().find(|c| c.load().is_ok_and(|cfg| path.starts_with(comparable(&cfg.sync_dir))))
}

/// The connection a command works on: the one named, else the one whose folder holds the first
/// of `paths` (relative to the current directory), else the one holding the current directory,
/// else the default one.
pub fn choose(name: Option<&str>, paths: &[PathBuf]) -> Result<Connection> {
    if let Some(n) = name {
        return Connection::named(n);
    }
    let cwd = std::env::current_dir().ok();
    let first = paths.first().map(|p| match &cwd {
        Some(d) if p.is_relative() => d.join(p),
        _ => p.clone(),
    });
    Ok(first
        .and_then(|p| for_path(&p))
        .or_else(|| cwd.and_then(|d| for_path(&d)))
        .unwrap_or_else(Connection::default_one))
}

/// Forgets a connection: its config file first (a running daemon stops its engine within
/// seconds), then, once its engine has stopped, its config and data directories. The default
/// connection keeps the directories (the others are in them) and loses only its own files.
/// Nothing in the sync folder or the cloud is touched.
pub fn remove(conn: &Connection, wait: std::time::Duration) -> Result<()> {
    if !conn.exists() {
        bail!("there is no connection named \"{}\"", conn.name);
    }
    std::fs::remove_file(&conn.config)?;
    let until = std::time::Instant::now() + wait;
    while conn.live_status().is_some() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if conn.live_status().is_some() {
        bail!(
            "CloudDrop is still uploading to \"{}\"; its settings are gone, so it stops soon — run this again to finish",
            conn.name
        );
    }
    let mut last = Ok(());
    // The engine may still be letting go of its files (Windows refuses to delete open ones).
    for _ in 0..20 {
        last = forget_files(conn);
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    last
}

fn forget_files(conn: &Connection) -> Result<()> {
    let gone = |r: std::io::Result<()>| match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    if !conn.is_default() {
        if let Some(dir) = conn.config.parent() {
            gone(std::fs::remove_dir_all(dir))?;
        }
        gone(std::fs::remove_dir_all(&conn.data))?;
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(&conn.data) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // What belongs to the installation, not to the default connection.
        if matches!(
            name.as_str(),
            DIR | "engine.lock" | "daemon.json" | "daemon.stop" | "tray.lock" | "agent.log" | "app.json"
        ) {
            continue;
        }
        let path = e.path();
        if path.is_dir() {
            gone(std::fs::remove_dir_all(&path))?;
        } else {
            gone(std::fs::remove_file(&path))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["Work", "photos-2026", "My Laptop", "a.b_c", "Über", "x"] {
            check_name(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in ["", " lead", "-x", "trail ", "dot.", "a/b", "a\\b", "a!b", "default", "DEFAULT", "con", "Nul.txt", "COM1", "lpt9"] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(check_name(&"x".repeat(32)).is_ok());
        assert!(check_name(&"x".repeat(33)).is_err());
        assert!(check_name("COMx").is_ok() && check_name("console").is_ok());
    }

    #[test]
    fn overlapping_folders() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("A"), d.path().join("B"));
        std::fs::create_dir_all(a.join("inner")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert!(folders_overlap(&a, &a));
        assert!(folders_overlap(&a, &a.join("inner")));
        assert!(folders_overlap(&a.join("inner"), &a));
        assert!(folders_overlap(&a.join("not yet/../inner"), &a));
        assert!(!folders_overlap(&a, &b));
        assert!(!folders_overlap(&d.path().join("A2"), &a), "a longer name is not inside");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&a, d.path().join("link")).unwrap();
            assert!(folders_overlap(&d.path().join("link"), &a), "links resolve");
        }
    }

    #[test]
    fn suggested_names_are_valid() {
        for (folder, want) in [
            ("/home/me/Work", "Work"),
            ("/home/me/My Photos!", "My Photos-"),
            ("/home/me/.hidden", "hidden"),
            ("/home/me/con", "con folder"),
            ("/home/me/default", "default folder"),
            ("/", "Folder"),
            ("/home/me/trailing. ", "trailing"),
        ] {
            let got = suggest_name(Path::new(folder));
            assert_eq!(got, want, "{folder}");
            check_name(&got).unwrap();
        }
        assert!(suggest_name(Path::new(&format!("/x/{}", "y".repeat(60)))).chars().count() <= MAX_NAME);
    }

    #[test]
    fn labels() {
        let named = Connection { name: "Work".into(), config: "c".into(), data: "d".into() };
        let default = Connection { name: DEFAULT.into(), config: "c".into(), data: "d".into() };
        let cfg = Config { sync_dir: "/home/me/Stuff".into(), ..Config::default() };
        assert_eq!(named.label(Some(&cfg)), "Work");
        assert_eq!(default.label(Some(&cfg)), "Stuff");
        assert_eq!(default.label(None), "CloudDrop");
        assert_eq!((named.notification_label(), default.notification_label()), (Some("Work".into()), None));
    }
}
