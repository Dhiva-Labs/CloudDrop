//! One read of everything the window and the top-bar icon show, and the upload speed
//! measured across reads.

use clouddrop::config::{self, Config};
use clouddrop::control::{self, DaemonStatus, Phase, Quota};
use clouddrop::db::{Db, FolderStats, Kind};
use crate::fmt::{self, files};
use crate::providers::{self, Choice};
use std::collections::HashMap;
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
    /// The configured provider (Google Drive when there is no config), for names and links.
    pub provider: Choice,
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
    /// Upload speed in bytes per second, once measured (set by the app from its [`RateMeter`]).
    pub rate: Option<f64>,
}

impl Snapshot {
    pub fn load(db: Option<&Db>) -> Self {
        let cfg = Config::load().ok().filter(providers::usable);
        let provider = Choice::current(cfg.as_ref());
        let mut s = Snapshot {
            signed_in: provider.signed_in(),
            provider,
            daemon: control::read_status(&config::data_dir()).filter(|d| d.is_live()),
            cfg,
            paused: false,
            counts: Counts::default(),
            cloud_bytes: 0,
            cloud_only_bytes: 0,
            root: FolderStats::default(),
            root_id: None,
            rate: None,
        };
        let Some(db) = db.filter(|_| s.cfg.is_some()) else {
            return s;
        };
        s.paused = control::is_paused(db);
        s.root_id = db.meta_get("root_id").ok().flatten();
        // Whether the cloud folder has a web page, from its real id: some providers link only
        // ids of their own form (OneDrive personal), so a made-up one can't tell.
        if let (Some(id), Some(cfg)) = (&s.root_id, &s.cfg) {
            s.provider.web_links = cfg.provider.web_url(Kind::Dir, id).is_some();
        }
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

    /// The provider's storage of the account; None until the daemon has asked.
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

    /// Bytes still to send: the rest of the uploads running now and every file waiting.
    pub fn remaining(&self) -> u64 {
        (self.root.waiting_bytes.max(0) as u64).saturating_sub(self.progress().0)
    }
}

/// Bytes per second over the last few seconds of uploading.
///
/// Time comes from the heartbeat's own `updated_ms`, so the reading doesn't depend on when the
/// app happened to look. Bytes are the growth of each upload's `sent` between two heartbeats;
/// an upload that finished between them counts only up to its last report.
#[derive(Debug)]
pub struct RateMeter {
    window_ms: i64,
    min_span_ms: i64,
    samples: Vec<(i64, u64)>,
    last_sent: HashMap<String, u64>,
    last_ms: Option<i64>,
    total: u64,
}

impl Default for RateMeter {
    fn default() -> Self {
        RateMeter::new(8000, 2000)
    }
}

impl RateMeter {
    pub fn new(window_ms: i64, min_span_ms: i64) -> Self {
        RateMeter { window_ms, min_span_ms, samples: Vec::new(), last_sent: HashMap::new(), last_ms: None, total: 0 }
    }

    /// Feeds one status read. A moment between files (waiting, no upload running) counts as
    /// time without bytes; anything else (idle, paused, offline...) starts the reading over.
    pub fn add(&mut self, d: Option<&DaemonStatus>) {
        let Some(d) = d.filter(|d| matches!(d.phase, Phase::Uploading | Phase::Waiting)) else {
            self.samples.clear();
            self.last_sent.clear();
            self.last_ms = None;
            return;
        };
        if self.last_ms.is_some_and(|last| d.updated_ms <= last) {
            return;
        }
        let sent: HashMap<String, u64> = d.uploads.iter().map(|u| (u.path.clone(), u.sent)).collect();
        for (path, now) in &sent {
            if let Some(before) = self.last_sent.get(path) {
                self.total += now.saturating_sub(*before);
            }
        }
        self.last_sent = sent;
        self.last_ms = Some(d.updated_ms);
        self.samples.push((d.updated_ms, self.total));
        let oldest = d.updated_ms - self.window_ms;
        self.samples.retain(|(ms, _)| *ms >= oldest);
    }

    /// None until there is enough to measure.
    pub fn bytes_per_second(&self) -> Option<f64> {
        let (&(t0, b0), &(t1, b1)) = (self.samples.first()?, self.samples.last()?);
        if t1 - t0 < self.min_span_ms {
            return None;
        }
        Some((b1 - b0) as f64 * 1000.0 / (t1 - t0) as f64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Setup,
    SignIn,
    /// Sign in for form providers: the setup assistant's Connect step.
    Connect,
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
    /// How far the uploads running now are (0..1), while uploading.
    pub fraction: Option<f64>,
    /// "2.4 MB/s · about 3 minutes left", once the speed is known.
    pub speed: Option<String>,
}

pub fn summarize(s: &Snapshot) -> Summary {
    let p = &s.provider;
    let cloud = p.name.as_str();
    let mk = |icon, title: String, detail: String, tray, line: String, action| Summary {
        icon,
        title,
        detail,
        tray,
        line,
        action,
        fraction: None,
        speed: None,
    };
    if s.cfg.is_none() {
        return mk(
            "clouddrop-attention-symbolic",
            "CloudDrop isn't set up yet".into(),
            "Set it up to start uploading to the cloud.".into(),
            TrayIcon::Attention,
            "Not set up".into(),
            Some(Action::Setup),
        );
    }
    let sign_in = if p.is_form() { Action::Connect } else { Action::SignIn };
    if !s.signed_in {
        let (title, detail) = if p.is_google() {
            ("Sign in to Google".to_string(), "CloudDrop needs your permission to upload to Google Drive.".to_string())
        } else if p.is_form() {
            (format!("Connect to {cloud}"), format!("CloudDrop needs your {cloud} details to upload."))
        } else {
            (format!("Sign in to {cloud}"), format!("CloudDrop needs your permission to upload to {cloud}."))
        };
        return mk(
            "clouddrop-attention-symbolic",
            title,
            detail,
            TrayIcon::Attention,
            "Sign-in needed".into(),
            Some(sign_in),
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
    if let Some(v) = verifying_summary(d, p) {
        return v;
    }
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
            format!(
                "{} no longer accepts CloudDrop's sign-in, so uploads are on hold.",
                if p.is_google() { "Google" } else { cloud }
            ),
            TrayIcon::Attention,
            "Sign-in needed".into(),
            Some(sign_in),
        ),
        Phase::DriveFolderMissing => mk(
            "clouddrop-attention-symbolic",
            format!("The CloudDrop folder is missing in {cloud}"),
            capitalize(&message),
            TrayIcon::Attention,
            if p.is_google() { "Drive folder missing" } else { "Cloud folder missing" }.into(),
            None,
        ),
        Phase::Offline => mk(
            "clouddrop-attention-symbolic",
            format!("Can't reach {cloud}"),
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
            format!("Connecting to {cloud}."),
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
            Summary {
                fraction: if total > 0 { Some(sent as f64 / total as f64) } else { None },
                speed: speed_line(s),
                ..mk(
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
        }
        Phase::Idle if s.counts.failed > 0 => mk(
            "clouddrop-attention-symbolic",
            "Some files need attention".into(),
            format!("Everything else is safe in {cloud}."),
            TrayIcon::Attention,
            format!("{} failed", files(s.counts.failed)),
            None,
        ),
        Phase::Idle => mk(
            "clouddrop-idle-symbolic",
            "Everything is uploaded".into(),
            match s.counts.synced + s.counts.cloud_only {
                0 => "Put files in your CloudDrop folder to back them up.".to_string(),
                n => format!("{} · {} safe in {cloud}", files(n), human_bytes(s.cloud_bytes)),
            },
            TrayIcon::Idle,
            "Up to date".into(),
            None,
        ),
    }
}

/// "Verifying…": freeing up space proves cloud copies by downloading them, for providers
/// without a content hash. The engine says so in the daemon's `message` ("Verifying…",
/// `control::VERIFYING`) while the phase stays idle, waiting or uploading. Home's hero and the
/// Files summary line both ask here.
pub fn is_verifying(d: &DaemonStatus) -> bool {
    matches!(d.phase, Phase::Idle | Phase::Waiting | Phase::Uploading)
        && d.message.as_deref().is_some_and(|m| m.to_lowercase().starts_with("verifying"))
}

fn verifying_summary(d: &DaemonStatus, p: &Choice) -> Option<Summary> {
    is_verifying(d).then(|| verifying_text(d.message.as_deref(), p))
}

/// The Verifying summary: a sentence of its own for the engine's bare "Verifying…", or a
/// longer message as it is.
pub fn verifying_text(message: Option<&str>, p: &Choice) -> Summary {
    let message = message.unwrap_or_default().trim();
    let bare = ["", "verifying", "verifying…", "verifying..."].contains(&message.to_lowercase().as_str());
    Summary {
        icon: "clouddrop-syncing-symbolic",
        title: "Verifying…".into(),
        detail: if bare {
            format!("Making sure {} holds identical copies before freeing up space.", p.name)
        } else {
            capitalize(message)
        },
        tray: TrayIcon::Syncing,
        line: "Verifying…".into(),
        action: None,
        fraction: None,
        speed: None,
    }
}

/// What Home's cloud storage card says.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageText {
    pub headline: String,
    pub note: String,
    pub fraction: Option<f64>,
    /// Nothing known yet (dim headline).
    pub muted: bool,
}

pub fn cloud_storage(s: &Snapshot) -> StorageText {
    let holds = format!("CloudDrop holds {}", human_bytes(s.cloud_bytes));
    let text = |headline: String, note: String, fraction, muted| StorageText { headline, note, fraction, muted };
    let q = s.quota();
    if let Some(Quota { limit: Some(limit), usage, .. }) = q
        && limit > 0
    {
        return text(
            format!("{} of {} used", human_bytes(usage), human_bytes(limit)),
            holds,
            Some(usage as f64 / limit as f64),
            false,
        );
    }
    // Providers without a quota (S3) report none at all, or nothing used of no limit.
    let p = &s.provider;
    if p.has_quota == Some(false) || (!p.is_google() && q.is_some_and(|q| q.usage == 0)) {
        return text(holds, format!("{} doesn't report free space", p.name), None, false);
    }
    match q {
        Some(Quota { usage, .. }) => text(format!("{} used", human_bytes(usage)), format!("{holds} · unlimited storage"), None, false),
        None => text(if s.daemon.is_some() { "Checking…" } else { "Not connected" }.into(), holds, None, true),
    }
}

/// "2.4 MB/s · about 3 minutes left"; just the speed when nothing is known to be waiting.
fn speed_line(s: &Snapshot) -> Option<String> {
    let rate = s.rate.filter(|r| *r >= 1.0)?;
    Some(match s.remaining() {
        0 => fmt::speed(rate),
        left => format!("{} · {}", fmt::speed(rate), fmt::time_left((left as f64 / rate).ceil() as u64)),
    })
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
            provider: Choice::of(clouddrop::remote::Provider::GoogleDrive),
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
            rate: None,
        }
    }

    fn uploading(ms: i64, sent: &[(&str, u64)]) -> DaemonStatus {
        DaemonStatus {
            pid: 1,
            updated_ms: ms,
            phase: Phase::Uploading,
            message: None,
            uploads: sent.iter().map(|(p, s)| Upload { path: (*p).into(), sent: *s, total: 1 << 30 }).collect(),
            waiting: 0,
            quota: None,
        }
    }

    #[test]
    fn rate_measures_growth_over_the_heartbeats() {
        let mut m = RateMeter::default();
        m.add(Some(&uploading(0, &[("a", 0)])));
        assert_eq!(m.bytes_per_second(), None);
        m.add(Some(&uploading(1000, &[("a", 1000)])));
        assert_eq!(m.bytes_per_second(), None, "less than two seconds measured");
        m.add(Some(&uploading(2000, &[("a", 2000)])));
        assert_eq!(m.bytes_per_second(), Some(1000.0));
        m.add(Some(&uploading(3000, &[("a", 3000), ("b", 0)])));
        m.add(Some(&uploading(4000, &[("a", 4000), ("b", 1000)])));
        assert_eq!(m.bytes_per_second(), Some(1250.0));
    }

    #[test]
    fn rate_skips_repeats_and_stops_counting_finished_uploads() {
        let mut m = RateMeter::default();
        m.add(Some(&uploading(0, &[("a", 0)])));
        m.add(Some(&uploading(1000, &[("a", 500)])));
        m.add(Some(&uploading(1000, &[("a", 500)])));
        m.add(Some(&uploading(2000, &[("b", 0)])));
        assert_eq!(m.bytes_per_second(), Some(250.0));
    }

    #[test]
    fn rate_window_and_restart() {
        let mut m = RateMeter::new(2000, 2000);
        m.add(Some(&uploading(0, &[("a", 0)])));
        m.add(Some(&uploading(1000, &[("a", 10000)])));
        m.add(Some(&uploading(2000, &[("a", 10100)])));
        m.add(Some(&uploading(3000, &[("a", 10200)])));
        assert_eq!(m.bytes_per_second(), Some(100.0));

        let mut m = RateMeter::default();
        m.add(Some(&uploading(0, &[("a", 0)])));
        m.add(Some(&uploading(2000, &[("a", 2000)])));
        let mut gap = uploading(4000, &[]);
        gap.phase = Phase::Waiting;
        m.add(Some(&gap));
        assert_eq!(m.bytes_per_second(), Some(500.0), "a moment between files slows the reading");
        let mut paused = uploading(5000, &[]);
        paused.phase = Phase::Paused;
        m.add(Some(&paused));
        assert_eq!(m.bytes_per_second(), None);
    }

    #[test]
    fn uploading_progress_speed_and_time_left() {
        let mut s = snap(Phase::Uploading);
        let d = s.daemon.as_mut().unwrap();
        d.waiting = 2;
        d.uploads = vec![Upload { path: "a".into(), sent: 1024 * 1024, total: 4 * 1024 * 1024 }];
        let sum = summarize(&s);
        assert_eq!(sum.fraction, Some(0.25));
        assert_eq!(sum.speed, None);
        s.rate = Some(512.0 * 1024.0);
        assert_eq!(summarize(&s).speed.as_deref(), Some("512.0 KB/s"));
        s.root.waiting_bytes = (4 + 300) * 1024 * 1024;
        assert_eq!(summarize(&s).speed.as_deref(), Some("512.0 KB/s · about 10 minutes left"));
        let mut idle = snap(Phase::Idle);
        idle.rate = Some(1000.0);
        assert_eq!((summarize(&idle).fraction, summarize(&idle).speed), (None, None));
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
    fn storage_card_quota_or_what_cloudrop_holds() {
        let quota = |limit, usage| Quota { limit, usage, usage_in_drive: 0 };
        let mut s = snap(Phase::Idle);
        s.cloud_bytes = 1024;
        s.daemon.as_mut().unwrap().quota = Some(quota(Some(4096), 1024));
        let t = cloud_storage(&s);
        assert_eq!((t.headline.as_str(), t.note.as_str(), t.fraction, t.muted), ("1.0 KB of 4.0 KB used", "CloudDrop holds 1.0 KB", Some(0.25), false));
        s.daemon.as_mut().unwrap().quota = Some(quota(None, 0));
        let t = cloud_storage(&s);
        assert_eq!((t.headline.as_str(), t.note.as_str()), ("0 B used", "CloudDrop holds 1.0 KB · unlimited storage"), "Google: unchanged");
        s.daemon.as_mut().unwrap().quota = None;
        assert_eq!((cloud_storage(&s).headline.as_str(), cloud_storage(&s).muted), ("Checking…", true));
        s.daemon = None;
        assert_eq!(cloud_storage(&s).headline, "Not connected");

        let mut s = snap(Phase::Idle);
        s.cloud_bytes = 2048;
        s.provider = provider("s3");
        let t = cloud_storage(&s);
        assert_eq!((t.headline.as_str(), t.note.as_str(), t.muted), ("CloudDrop holds 2.0 KB", "S3 doesn't report free space", false));
        s.provider = provider("webdav");
        s.daemon.as_mut().unwrap().quota = Some(quota(None, 0));
        assert_eq!(cloud_storage(&s).headline, "CloudDrop holds 2.0 KB", "nothing used of no limit: no quota");
        s.daemon.as_mut().unwrap().quota = Some(quota(Some(1024), 512));
        assert_eq!(cloud_storage(&s).headline, "512 B of 1.0 KB used");
        s.provider = provider("onedrive");
        s.daemon.as_mut().unwrap().quota = None;
        assert_eq!(cloud_storage(&s).headline, "Checking…", "OneDrive reports one once asked");
    }

    fn provider(id: &str) -> Choice {
        crate::providers::fakes().into_iter().find(|c| c.id == id).unwrap()
    }

    fn texts(sum: &Summary) -> (&str, &str, &str) {
        (&sum.title, &sum.detail, &sum.line)
    }

    #[test]
    fn google_drive_wording_is_unchanged() {
        let mut s = snap(Phase::Idle);
        s.counts.synced = 1;
        s.cloud_bytes = 1024;
        assert_eq!(summarize(&s).detail, "1 file · 1.0 KB safe in Google Drive");
        s.counts.failed = 1;
        assert_eq!(summarize(&s).detail, "Everything else is safe in Google Drive.");
        let mut s = snap(Phase::DriveFolderMissing);
        s.daemon.as_mut().unwrap().message = Some("it was moved to the trash".into());
        assert_eq!(
            texts(&summarize(&s)),
            ("The CloudDrop folder is missing in Google Drive", "It was moved to the trash", "Drive folder missing")
        );
        assert_eq!(summarize(&snap(Phase::Offline)).title, "Can't reach Google Drive");
        assert_eq!(summarize(&snap(Phase::Starting)).detail, "Connecting to Google Drive.");
        let sum = summarize(&snap(Phase::NeedsSignIn));
        assert_eq!(sum.detail, "Google no longer accepts CloudDrop's sign-in, so uploads are on hold.");
        assert_eq!(sum.action, Some(Action::SignIn));
        let mut s = snap(Phase::Idle);
        s.signed_in = false;
        let sum = summarize(&s);
        assert_eq!(texts(&sum), ("Sign in to Google", "CloudDrop needs your permission to upload to Google Drive.", "Sign-in needed"));
        assert_eq!(sum.action, Some(Action::SignIn));
        s.cfg = None;
        assert_eq!(summarize(&s).detail, "Set it up to start uploading to the cloud.");
    }

    #[test]
    fn other_providers_are_named() {
        let mut s = snap(Phase::Idle);
        s.provider = provider("onedrive");
        assert_eq!(s.provider.name, "OneDrive");
        s.counts.synced = 2;
        s.cloud_bytes = 2048;
        assert_eq!(summarize(&s).detail, "2 files · 2.0 KB safe in OneDrive");
        s.counts.failed = 1;
        assert_eq!(summarize(&s).detail, "Everything else is safe in OneDrive.");
        for (phase, title, detail, line) in [
            (Phase::Offline, "Can't reach OneDrive", "0 files waiting. CloudDrop keeps retrying on its own.", "Offline — retrying"),
            (Phase::Starting, "Starting…", "Connecting to OneDrive.", "Starting"),
            (Phase::DriveFolderMissing, "The CloudDrop folder is missing in OneDrive", "", "Cloud folder missing"),
            (
                Phase::NeedsSignIn,
                "Sign in again",
                "OneDrive no longer accepts CloudDrop's sign-in, so uploads are on hold.",
                "Sign-in needed",
            ),
        ] {
            let mut s = snap(phase);
            s.provider = provider("onedrive");
            assert_eq!(texts(&summarize(&s)), (title, detail, line), "{phase:?}");
        }
        let mut s = snap(Phase::Idle);
        s.signed_in = false;
        s.provider = provider("nextcloud");
        let sum = summarize(&s);
        assert_eq!((sum.title.as_str(), sum.detail.as_str()), ("Sign in to Nextcloud", "CloudDrop needs your permission to upload to Nextcloud."));
        assert_eq!(sum.action, Some(Action::SignIn));
    }

    #[test]
    fn form_providers_connect_instead_of_signing_in() {
        let mut s = snap(Phase::Idle);
        s.signed_in = false;
        s.provider = provider("webdav");
        let sum = summarize(&s);
        assert_eq!((sum.title.as_str(), sum.detail.as_str()), ("Connect to WebDAV", "CloudDrop needs your WebDAV details to upload."));
        assert_eq!(sum.action, Some(Action::Connect));
        let mut s = snap(Phase::NeedsSignIn);
        s.provider = provider("webdav");
        assert_eq!(summarize(&s).action, Some(Action::Connect));
    }

    #[test]
    fn verifying_shows_while_idle_or_waiting() {
        let mut s = snap(Phase::Idle);
        s.provider = provider("webdav");
        s.counts.failed = 1;
        s.daemon.as_mut().unwrap().message = Some("checking 3 files".into());
        assert!(summarize(&s).title != "Verifying…", "only a message that starts with Verifying");
        s.daemon.as_mut().unwrap().message = Some("verifying 3 files before freeing them".into());
        let sum = summarize(&s);
        assert_eq!(texts(&sum), ("Verifying…", "Verifying 3 files before freeing them", "Verifying…"));
        assert_eq!((sum.icon, sum.tray, sum.action), ("clouddrop-syncing-symbolic", TrayIcon::Syncing, None));
        s.daemon.as_mut().unwrap().phase = Phase::Waiting;
        assert_eq!(summarize(&s).title, "Verifying…");
        // Exactly what the engine writes (control::VERIFYING), also while uploading.
        s.daemon.as_mut().unwrap().message = Some("Verifying…".into());
        s.daemon.as_mut().unwrap().phase = Phase::Uploading;
        let sum = summarize(&s);
        assert_eq!((sum.title.as_str(), sum.detail.as_str()), ("Verifying…", "Making sure WebDAV holds identical copies before freeing up space."));
        for phase in [Phase::Paused, Phase::Offline] {
            s.daemon.as_mut().unwrap().phase = phase;
            assert!(!is_verifying(s.daemon.as_ref().unwrap()), "{phase:?}");
        }
        let own = verifying_text(None, &s.provider);
        assert_eq!(own.detail, "Making sure WebDAV holds identical copies before freeing up space.");
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
