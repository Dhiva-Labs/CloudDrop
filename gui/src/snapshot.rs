//! One read of everything the window and the top-bar icon show.

use clouddrop::config::{self, Config};
use clouddrop::control::{self, DaemonStatus, Phase, Quota};
use clouddrop::db::{Db, FolderStats};
use crate::fmt::files;
use clouddrop::util::human_bytes;
use clouddrop_tray::TrayIcon;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Counts {
    pub synced: i64,
    pub waiting: i64,
    pub cloud_only: i64,
    pub failed: i64,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub cfg: Option<Config>,
    pub signed_in: bool,
    /// Present only while the daemon is alive.
    pub daemon: Option<DaemonStatus>,
    pub paused: bool,
    pub counts: Counts,
    pub cloud_bytes: u64,
    pub cloud_only_bytes: u64,
    /// Totals over everything in the CloudDrop folder.
    pub root: FolderStats,
    pub root_id: Option<String>,
}

impl Snapshot {
    pub fn load(db: Option<&Db>) -> Self {
        let cfg = Config::load().ok().filter(|c| !c.client_id.is_empty());
        let mut s = Snapshot {
            signed_in: control::is_signed_in(),
            daemon: control::read_status(&config::data_dir()).filter(|d| d.is_live()),
            cfg,
            paused: false,
            counts: Counts::default(),
            cloud_bytes: 0,
            cloud_only_bytes: 0,
            root: FolderStats::default(),
            root_id: None,
        };
        let Some(db) = db.filter(|_| s.cfg.is_some()) else {
            return s;
        };
        s.paused = control::is_paused(db);
        s.root_id = db.meta_get("root_id").ok().flatten();
        for (kind, state, n) in db.counts().unwrap_or_default() {
            if kind != "file" {
                continue;
            }
            match state.as_str() {
                "synced" => s.counts.synced += n,
                "pending" | "uploading" => s.counts.waiting += n,
                "cloud_only" => s.counts.cloud_only += n,
                "error" => s.counts.failed += n,
                _ => {}
            }
        }
        if let Ok((all, cloud_only)) = db.cloud_bytes() {
            s.cloud_bytes = all.max(0) as u64;
            s.cloud_only_bytes = cloud_only.max(0) as u64;
        }
        s.root = db.folder_stats("").unwrap_or_default();
        s
    }

    /// Google Drive storage of the account; None until the daemon has asked.
    pub fn quota(&self) -> Option<Quota> {
        self.daemon.as_ref().and_then(|d| d.quota)
    }

    pub fn ready(&self) -> bool {
        self.cfg.is_some() && self.signed_in
    }

    /// (bytes sent, bytes total) across the uploads running now.
    pub fn progress(&self) -> (u64, u64) {
        self.daemon.as_ref().map_or((0, 0), |d| {
            d.uploads
                .iter()
                .fold((0, 0), |(s, t), u| (s + u.sent, t + u.total))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Setup,
    SignIn,
    StartService,
    Resume,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub icon: &'static str,
    pub title: String,
    pub detail: String,
    pub tray: TrayIcon,
    /// Short line for the top-bar menu.
    pub line: String,
    pub action: Option<Action>,
}

pub fn summarize(s: &Snapshot) -> Summary {
    let mk = |icon, title: String, detail: String, tray, line: String, action| Summary {
        icon,
        title,
        detail,
        tray,
        line,
        action,
    };
    if s.cfg.is_none() {
        return mk(
            "clouddrop-attention-symbolic",
            "CloudDrop isn't set up yet".into(),
            "Set it up to start uploading to Google Drive.".into(),
            TrayIcon::Attention,
            "Not set up".into(),
            Some(Action::Setup),
        );
    }
    if !s.signed_in {
        return mk(
            "clouddrop-attention-symbolic",
            "Sign in to Google".into(),
            "CloudDrop needs your permission to upload to Google Drive.".into(),
            TrayIcon::Attention,
            "Sign-in needed".into(),
            Some(Action::SignIn),
        );
    }
    let Some(d) = &s.daemon else {
        return mk(
            "clouddrop-attention-symbolic",
            "CloudDrop isn't running".into(),
            "Nothing is being uploaded until it starts.".into(),
            TrayIcon::Attention,
            "Not running".into(),
            Some(Action::StartService),
        );
    };
    let message = d.message.clone().unwrap_or_default();
    match d.phase {
        Phase::Paused => mk(
            "clouddrop-paused-symbolic",
            "Uploads are paused".into(),
            if d.waiting > 0 {
                format!("{} waiting. Unfinished uploads continue where they stopped.", files(d.waiting as i64))
            } else {
                "New files will wait until you resume.".into()
            },
            TrayIcon::Paused,
            "Paused".into(),
            Some(Action::Resume),
        ),
        Phase::NeedsSignIn => mk(
            "clouddrop-attention-symbolic",
            "Sign in again".into(),
            "Google no longer accepts CloudDrop's sign-in, so uploads are on hold.".into(),
            TrayIcon::Attention,
            "Sign-in needed".into(),
            Some(Action::SignIn),
        ),
        Phase::DriveFolderMissing => mk(
            "clouddrop-attention-symbolic",
            "The CloudDrop folder is missing in Google Drive".into(),
            capitalize(&message),
            TrayIcon::Attention,
            "Drive folder missing".into(),
            None,
        ),
        Phase::Offline => mk(
            "clouddrop-attention-symbolic",
            "Can't reach Google Drive".into(),
            format!(
                "{} waiting. CloudDrop keeps retrying on its own.",
                files(d.waiting as i64)
            ),
            TrayIcon::Attention,
            "Offline — retrying".into(),
            None,
        ),
        Phase::Starting => mk(
            "clouddrop-syncing-symbolic",
            "Starting…".into(),
            "Connecting to Google Drive.".into(),
            TrayIcon::Syncing,
            "Starting".into(),
            None,
        ),
        Phase::Uploading | Phase::Waiting => {
            let (sent, total) = s.progress();
            let pct = (sent * 100).checked_div(total).unwrap_or(0);
            let detail = if total > 0 {
                format!(
                    "{} of {} sent for the current files.",
                    human_bytes(sent),
                    human_bytes(total)
                )
            } else {
                "Waiting for a moment to upload.".into()
            };
            mk(
                "clouddrop-syncing-symbolic",
                format!("Uploading {}", files(d.waiting.max(1) as i64)),
                detail,
                TrayIcon::Syncing,
                if total > 0 {
                    format!("Uploading {} — {pct}%", files(d.waiting.max(1) as i64))
                } else {
                    format!("{} waiting", files(d.waiting as i64))
                },
                None,
            )
        }
        Phase::Idle if s.counts.failed > 0 => mk(
            "clouddrop-attention-symbolic",
            "Some files need attention".into(),
            "Everything else is safe in Google Drive.".into(),
            TrayIcon::Attention,
            format!("{} failed", files(s.counts.failed)),
            None,
        ),
        Phase::Idle => mk(
            "clouddrop-idle-symbolic",
            "Everything is uploaded".into(),
            match s.counts.synced + s.counts.cloud_only {
                0 => "Put files in your CloudDrop folder to back them up.".to_string(),
                n => format!("{} · {} safe in Google Drive", files(n), human_bytes(s.cloud_bytes)),
            },
            TrayIcon::Idle,
            "Up to date".into(),
            None,
        ),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clouddrop::control::Upload;

    fn snap(phase: Phase) -> Snapshot {
        Snapshot {
            cfg: Some(Config {
                client_id: "x.apps.googleusercontent.com".into(),
                client_secret: "s".into(),
                ..Config::default()
            }),
            signed_in: true,
            daemon: Some(DaemonStatus {
                pid: 1,
                updated_ms: 0,
                phase,
                message: None,
                uploads: vec![],
                waiting: 0,
                quota: None,
            }),
            paused: false,
            counts: Counts::default(),
            cloud_bytes: 0,
            cloud_only_bytes: 0,
            root: FolderStats::default(),
            root_id: Some("R".into()),
        }
    }

    #[test]
    fn idle_and_failures() {
        assert_eq!(summarize(&snap(Phase::Idle)).tray, TrayIcon::Idle);
        let mut s = snap(Phase::Idle);
        s.counts.failed = 2;
        let sum = summarize(&s);
        assert_eq!(sum.tray, TrayIcon::Attention);
        assert_eq!(sum.title, "Some files need attention");
        assert_eq!(sum.line, "2 files failed");
    }

    #[test]
    fn idle_subtitle_counts_files_in_drive() {
        let mut s = snap(Phase::Idle);
        assert_eq!(summarize(&s).detail, "Put files in your CloudDrop folder to back them up.");
        s.counts.synced = 2;
        s.counts.cloud_only = 1;
        s.cloud_bytes = 3 * 1024;
        assert_eq!(summarize(&s).detail, "3 files · 3.0 KB safe in Google Drive");
    }

    #[test]
    fn quota_comes_from_the_daemon() {
        let mut s = snap(Phase::Idle);
        assert_eq!(s.quota(), None);
        let q = Quota { limit: Some(15), usage: 3, usage_in_drive: 2 };
        s.daemon.as_mut().unwrap().quota = Some(q);
        assert_eq!(s.quota(), Some(q));
    }

    #[test]
    fn uploading_shows_percentage() {
        let mut s = snap(Phase::Uploading);
        let d = s.daemon.as_mut().unwrap();
        d.waiting = 3;
        d.uploads = vec![Upload { path: "a".into(), sent: 50, total: 200 }];
        let sum = summarize(&s);
        assert_eq!(sum.line, "Uploading 3 files — 25%");
        assert_eq!(sum.tray, TrayIcon::Syncing);
    }

    #[test]
    fn setup_states_come_first() {
        let mut s = snap(Phase::Idle);
        s.signed_in = false;
        assert_eq!(summarize(&s).action, Some(Action::SignIn));
        s.cfg = None;
        assert_eq!(summarize(&s).action, Some(Action::Setup));
        let mut s = snap(Phase::Idle);
        s.daemon = None;
        assert_eq!(summarize(&s).action, Some(Action::StartService));
    }
}
