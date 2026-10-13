//! CloudDrop top-bar icon.
//!
//! A small wrapper around [`ksni`] that publishes CloudDrop as a
//! StatusNotifierItem (the "tray" protocol used by GNOME's AppIndicator
//! extension, KDE Plasma and most other desktops).
//!
//! The application owns the truth: it builds a [`TrayState`] and pushes it with
//! [`Tray::update`]; the user's clicks come back as [`TrayCommand`]s through the
//! callback given to [`Tray::spawn`].
//!
//! # Runtime
//!
//! [`Tray::spawn`] must be called inside a running multi-threaded tokio runtime
//! and that runtime must stay alive for as long as the icon is wanted: the
//! D-Bus service is a task on it. The `on_command` callback runs on one of its
//! worker threads while the tray's internal lock is held, so it must return
//! quickly and must not wait for [`Tray::update`] (or other calls into this
//! crate); hand the command to your main loop (a channel send,
//! `glib::MainContext::invoke`, ...) and return.
//!
//! # When there is no tray
//!
//! [`Tray::spawn`] returns an `Err` (it neither hangs nor panics) when the
//! session has no `org.kde.StatusNotifierWatcher`, for example on stock GNOME
//! without the AppIndicator extension, or when the shell has not started it
//! yet during login. Callers should log the error and carry on without an icon
//! (or retry later); the application must remain usable from its window.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use anyhow::Result;
use anyhow::anyhow;
use ksni::menu::StandardItem;
use ksni::{Category, MenuItem, Status, ToolTip, TrayMethods};

const ID: &str = "clouddrop";
const TITLE: &str = "CloudDrop";
/// Upper bound for talking to the session bus while registering. Registering
/// normally takes milliseconds; the bound only matters if the bus (or a
/// bus-activated watcher) stalls.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Which of the four symbolic icons the top bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    Idle,
    Syncing,
    Paused,
    Attention,
}

impl TrayIcon {
    /// Icon-theme name; the files live in `data/icons/hicolor/symbolic/status`.
    fn icon_name(self) -> &'static str {
        match self {
            TrayIcon::Idle => "clouddrop-idle-symbolic",
            TrayIcon::Syncing => "clouddrop-syncing-symbolic",
            TrayIcon::Paused => "clouddrop-paused-symbolic",
            TrayIcon::Attention => "clouddrop-attention-symbolic",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrayState {
    pub icon: TrayIcon,
    /// One-line status shown as the first (insensitive) menu item and tooltip,
    /// e.g. "Up to date", "Uploading 3 files — 45%".
    pub status_line: String,
    /// Uploads are paused: the pause item reads "Resume Uploads" instead of
    /// "Pause Uploads".
    pub paused: bool,
    /// Show the pause/resume item at all (false before setup is complete).
    pub can_pause: bool,
    /// Show "Open in Google Drive" (false until the Drive folder exists).
    pub has_drive_folder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowWindow,
    OpenFolder,
    OpenInDrive,
    TogglePause,
    Quit,
}

/// Handle to the published top-bar icon. Dropping it removes the icon.
pub struct Tray {
    handle: ksni::Handle<SniTray>,
    /// The state last pushed, so repeated identical updates cost nothing.
    last: tokio::sync::Mutex<TrayState>,
}

impl Tray {
    /// Registers the StatusNotifierItem on the session bus. Must be called
    /// inside a tokio runtime.
    ///
    /// `icon_theme_path`: directory containing our icons (hicolor layout root,
    /// e.g. `~/.local/share/icons` or `<repo>/data/icons`) so the shell finds
    /// them even if not installed system-wide.
    ///
    /// `on_command` is called from a tokio worker thread whenever the user
    /// picks a menu item or clicks the icon (left click = `ShowWindow`).
    ///
    /// Fails if the session bus is unreachable or no StatusNotifierWatcher
    /// owns its name (see the crate docs).
    pub async fn spawn(
        initial: TrayState,
        icon_theme_path: Option<PathBuf>,
        on_command: impl Fn(TrayCommand) + Send + Sync + 'static,
    ) -> Result<Tray> {
        let sni = SniTray {
            state: initial.clone(),
            icon_theme_path: icon_theme_path
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            on_command: Arc::new(on_command),
        };
        let handle = tokio::time::timeout(SPAWN_TIMEOUT, sni.spawn())
            .await
            .map_err(|_| anyhow!("timed out registering the top-bar icon on the session bus"))?
            .map_err(|e| anyhow!("cannot show the top-bar icon: {e}"))?;
        Ok(Tray {
            handle,
            last: tokio::sync::Mutex::new(initial),
        })
    }

    /// Replaces the shown state (icon, tooltip, menu labels). Cheap no-op if
    /// unchanged.
    pub async fn update(&self, state: TrayState) {
        let mut last = self.last.lock().await;
        if *last == state {
            return;
        }
        self.handle.update(|t| t.state = state.clone()).await;
        *last = state;
    }

    /// Removes the icon.
    pub async fn shutdown(&self) {
        self.handle.shutdown().await;
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // Without this the icon would outlive the handle. The request is queued
        // synchronously, so no runtime is needed here.
        drop(self.handle.shutdown());
    }
}

/// What the ksni service publishes. Everything it reports is derived from
/// `state`.
struct SniTray {
    state: TrayState,
    icon_theme_path: String,
    on_command: Arc<dyn Fn(TrayCommand) + Send + Sync>,
}

impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        ID.into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn title(&self) -> String {
        TITLE.into()
    }

    fn status(&self) -> Status {
        match self.state.icon {
            TrayIcon::Attention => Status::NeedsAttention,
            _ => Status::Active,
        }
    }

    fn icon_theme_path(&self) -> String {
        self.icon_theme_path.clone()
    }

    fn icon_name(&self) -> String {
        self.state.icon.icon_name().into()
    }

    /// Shown instead of `icon_name` while the status is `NeedsAttention`.
    fn attention_icon_name(&self) -> String {
        TrayIcon::Attention.icon_name().into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.state.icon.icon_name().into(),
            icon_pixmap: Vec::new(),
            title: TITLE.into(),
            description: escape_markup(&self.state.status_line),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.on_command)(TrayCommand::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        menu_model(&self.state)
            .into_iter()
            .map(|entry| match entry {
                Entry::Status(label) => StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                Entry::Separator => MenuItem::Separator,
                Entry::Action(label, command) => StandardItem {
                    label: label.into(),
                    activate: Box::new(move |this: &mut Self| (this.on_command)(command)),
                    ..Default::default()
                }
                .into(),
            })
            .collect()
    }
}

/// One row of the context menu, before it is turned into D-Bus menu items.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    /// The disabled first row. The label is already menu-escaped.
    Status(String),
    Separator,
    Action(&'static str, TrayCommand),
}

/// The context menu for a state: status line, folder/Drive/window shortcuts,
/// the optional pause toggle, then Quit.
fn menu_model(state: &TrayState) -> Vec<Entry> {
    use Entry::{Action, Separator};

    let mut menu = vec![
        Entry::Status(escape_mnemonics(&state.status_line)),
        Separator,
        Action("Open CloudDrop Folder", TrayCommand::OpenFolder),
    ];
    if state.has_drive_folder {
        menu.push(Action("Open in Google Drive", TrayCommand::OpenInDrive));
    }
    menu.push(Action("Show CloudDrop", TrayCommand::ShowWindow));
    if state.can_pause {
        menu.push(Separator);
        let label = if state.paused {
            "Resume Uploads"
        } else {
            "Pause Uploads"
        };
        menu.push(Action(label, TrayCommand::TogglePause));
    }
    menu.push(Separator);
    menu.push(Action("Quit", TrayCommand::Quit));
    menu
}

/// dbusmenu treats `_` as the mnemonic marker (`__` is a literal underscore),
/// so a file name in the status line would otherwise lose its underscores.
fn escape_mnemonics(text: &str) -> String {
    text.replace('_', "__")
}

/// Tooltip descriptions accept a subset of HTML; show the text literally.
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ksni::Tray as _;

    use super::*;

    fn state() -> TrayState {
        TrayState {
            icon: TrayIcon::Idle,
            status_line: "Up to date".into(),
            paused: false,
            can_pause: true,
            has_drive_folder: true,
        }
    }

    fn labels(state: &TrayState) -> Vec<String> {
        menu_model(state)
            .into_iter()
            .map(|e| match e {
                Entry::Status(s) => s,
                Entry::Separator => "---".into(),
                Entry::Action(label, _) => label.into(),
            })
            .collect()
    }

    #[test]
    fn icon_names_are_the_installed_files() {
        assert_eq!(TrayIcon::Idle.icon_name(), "clouddrop-idle-symbolic");
        assert_eq!(TrayIcon::Syncing.icon_name(), "clouddrop-syncing-symbolic");
        assert_eq!(TrayIcon::Paused.icon_name(), "clouddrop-paused-symbolic");
        assert_eq!(
            TrayIcon::Attention.icon_name(),
            "clouddrop-attention-symbolic"
        );
    }

    #[test]
    fn full_menu_order() {
        assert_eq!(
            labels(&state()),
            [
                "Up to date",
                "---",
                "Open CloudDrop Folder",
                "Open in Google Drive",
                "Show CloudDrop",
                "---",
                "Pause Uploads",
                "---",
                "Quit",
            ]
        );
    }

    #[test]
    fn paused_state_offers_resume() {
        let s = TrayState {
            paused: true,
            ..state()
        };
        assert!(labels(&s).contains(&"Resume Uploads".to_string()));
        assert!(!labels(&s).contains(&"Pause Uploads".to_string()));
    }

    #[test]
    fn drive_item_needs_a_drive_folder() {
        let s = TrayState {
            has_drive_folder: false,
            ..state()
        };
        assert!(!labels(&s).contains(&"Open in Google Drive".to_string()));
    }

    #[test]
    fn pause_group_is_hidden_without_double_separators() {
        let s = TrayState {
            can_pause: false,
            has_drive_folder: false,
            ..state()
        };
        assert_eq!(
            labels(&s),
            [
                "Up to date",
                "---",
                "Open CloudDrop Folder",
                "Show CloudDrop",
                "---",
                "Quit"
            ]
        );
    }

    #[test]
    fn status_line_underscores_survive_mnemonic_parsing() {
        let s = TrayState {
            status_line: "Uploading my_file_1.txt".into(),
            ..state()
        };
        assert_eq!(labels(&s)[0], "Uploading my__file__1.txt");
    }

    type Seen = Arc<Mutex<Vec<TrayCommand>>>;

    fn recorder() -> (Arc<dyn Fn(TrayCommand) + Send + Sync>, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (Arc::new(move |c| sink.lock().unwrap().push(c)), seen)
    }

    fn sni(state: TrayState) -> (SniTray, Seen) {
        let (on_command, seen) = recorder();
        let tray = SniTray {
            state,
            icon_theme_path: "/icons".into(),
            on_command,
        };
        (tray, seen)
    }

    #[test]
    fn tooltip_is_title_plus_status_line_with_markup_escaped() {
        let (tray, _) = sni(TrayState {
            status_line: "3 files <5 MB & more".into(),
            ..state()
        });
        let tip = tray.tool_tip();
        assert_eq!(tip.title, "CloudDrop");
        assert_eq!(tip.description, "3 files &lt;5 MB &amp; more");
        assert_eq!(tip.icon_name, "clouddrop-idle-symbolic");
    }

    #[test]
    fn only_attention_needs_attention() {
        let (mut tray, _) = sni(state());
        for icon in [TrayIcon::Idle, TrayIcon::Syncing, TrayIcon::Paused] {
            tray.state.icon = icon;
            assert_eq!(tray.status(), Status::Active);
            assert_eq!(tray.icon_name(), icon.icon_name());
        }
        tray.state.icon = TrayIcon::Attention;
        assert_eq!(tray.status(), Status::NeedsAttention);
        assert_eq!(tray.icon_name(), "clouddrop-attention-symbolic");
        assert_eq!(tray.attention_icon_name(), "clouddrop-attention-symbolic");
    }

    #[test]
    fn handle_can_be_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Tray>();
    }

    #[test]
    fn identity() {
        let (tray, _) = sni(state());
        assert_eq!(tray.id(), "clouddrop");
        assert_eq!(tray.title(), "CloudDrop");
        assert_eq!(tray.category(), Category::ApplicationStatus);
        assert_eq!(tray.icon_theme_path(), "/icons");
    }

    #[test]
    fn menu_items_and_left_click_report_their_commands() {
        let (mut tray, seen) = sni(state());

        let mut disabled = 0;
        for item in tray.menu() {
            if let MenuItem::Standard(item) = item {
                if item.enabled {
                    (item.activate)(&mut tray);
                } else {
                    disabled += 1;
                }
            }
        }
        tray.activate(0, 0);

        assert_eq!(disabled, 1, "only the status line is insensitive");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                TrayCommand::OpenFolder,
                TrayCommand::OpenInDrive,
                TrayCommand::ShowWindow,
                TrayCommand::TogglePause,
                TrayCommand::Quit,
                TrayCommand::ShowWindow,
            ]
        );
    }
}
