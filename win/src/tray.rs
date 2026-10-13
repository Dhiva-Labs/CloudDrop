//! The notification-area icon: status at a glance and quick actions. Runs the
//! main thread's Windows message loop until Quit.

use crate::agent::EXIT;
use clouddrop::config::{self, Config};
use clouddrop::control::{self, Phase};
use clouddrop::db::{Db, Kind};
use std::sync::atomic::Ordering;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

const SIZE: u32 = 32;
static IDLE: &[u8] = include_bytes!("../assets/tray-idle.rgba");
static SYNCING: &[u8] = include_bytes!("../assets/tray-syncing.rgba");
static PAUSED: &[u8] = include_bytes!("../assets/tray-paused.rgba");
static ATTENTION: &[u8] = include_bytes!("../assets/tray-attention.rgba");

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Idle,
    Syncing,
    Paused,
    Attention,
}

fn icon(look: Look) -> Icon {
    let raw = match look {
        Look::Idle => IDLE,
        Look::Syncing => SYNCING,
        Look::Paused => PAUSED,
        Look::Attention => ATTENTION,
    };
    Icon::from_rgba(raw.to_vec(), SIZE, SIZE).expect("tray icon")
}

/// (look, one-line status, paused?)
fn current() -> (Look, String, bool) {
    if !control::is_configured() {
        return (Look::Attention, "Not set up — open CloudDrop".into(), false);
    }
    if !control::is_signed_in() {
        return (Look::Attention, "Sign-in needed".into(), false);
    }
    let Some(s) = control::read_status(&config::data_dir()).filter(|s| s.is_live()) else {
        return (Look::Syncing, "Starting…".into(), false);
    };
    let files = |n: u64| if n == 1 { "1 file".to_string() } else { format!("{n} files") };
    match s.phase {
        Phase::Idle => (Look::Idle, "Up to date".into(), false),
        Phase::Paused => (Look::Paused, "Paused".into(), true),
        Phase::Uploading | Phase::Waiting => {
            let (sent, total) = s.uploads.iter().fold((0, 0), |(a, b), u| (a + u.sent, b + u.total));
            let pct = (sent * 100).checked_div(total).unwrap_or(0);
            (Look::Syncing, format!("Uploading {} — {pct}%", files(s.waiting.max(1))), false)
        }
        Phase::Starting => (Look::Syncing, "Starting…".into(), false),
        Phase::Offline => (Look::Attention, "Offline — retrying".into(), false),
        Phase::NeedsSignIn => (Look::Attention, "Sign-in needed".into(), false),
        Phase::DriveFolderMissing => (Look::Attention, "Drive folder missing".into(), false),
    }
}

fn open(target: &str) {
    let _ = std::process::Command::new("explorer.exe").arg(target).spawn();
}

/// The CloudDrop app: next to this exe in a plain install.
fn show_app() {
    if let Ok(me) = std::env::current_exe() {
        let app = me.with_file_name("clouddrop_app.exe");
        if app.exists() {
            let _ = std::process::Command::new(app).spawn();
            return;
        }
    }
    if let Ok(cfg) = Config::load() {
        open(&cfg.sync_dir.display().to_string());
    }
}

pub fn run() {
    let status = MenuItem::new("CloudDrop", false, None);
    let open_folder = MenuItem::new("Open CloudDrop Folder", true, None);
    let open_drive = MenuItem::new("Open in Google Drive", true, None);
    let show = MenuItem::new("Show CloudDrop", true, None);
    let pause = MenuItem::new("Pause Uploads", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    for item in [
        &status as &dyn tray_icon::menu::IsMenuItem,
        &PredefinedMenuItem::separator(),
        &open_folder,
        &open_drive,
        &show,
        &PredefinedMenuItem::separator(),
        &pause,
        &PredefinedMenuItem::separator(),
        &quit,
    ] {
        let _ = menu.append(item);
    }

    let (mut look, mut line, mut paused) = current();
    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(format!("CloudDrop — {line}"))
        .with_icon(icon(look))
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("cannot show the notification-area icon: {e}");
            // Keep running without an icon until asked to stop.
            while !EXIT.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            return;
        }
    };
    status.set_text(&line);
    pause.set_text(if paused { "Resume Uploads" } else { "Pause Uploads" });

    message_loop(|| {
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == open_folder.id() {
                if let Ok(cfg) = Config::load() {
                    open(&cfg.sync_dir.display().to_string());
                }
            } else if ev.id == open_drive.id() {
                if let Ok(Some(id)) = Db::open_default().and_then(|db| db.meta_get("root_id")) {
                    open(&clouddrop::actions::drive_url(Kind::Dir, &id));
                }
            } else if ev.id == show.id() {
                show_app();
            } else if ev.id == pause.id() {
                if let Ok(db) = Db::open_default() {
                    let _ = control::set_paused(&db, !paused);
                }
            } else if ev.id == quit.id() {
                if let Ok(db) = Db::open_default() {
                    let _ = control::request_stop(&db);
                }
                EXIT.store(true, Ordering::SeqCst);
            }
        }
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_app();
            }
        }
        let (l, text, p) = current();
        if l != look {
            let _ = tray.set_icon(Some(icon(l)));
            look = l;
        }
        if text != line {
            let _ = tray.set_tooltip(Some(format!("CloudDrop — {text}")));
            status.set_text(&text);
            line = text;
        }
        if p != paused {
            pause.set_text(if p { "Resume Uploads" } else { "Pause Uploads" });
            paused = p;
        }
        !EXIT.load(Ordering::SeqCst)
    });
}

/// Pumps window messages, calling `tick` about twice a second until it returns false.
fn message_loop(mut tick: impl FnMut() -> bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, SetTimer, TranslateMessage,
    };
    unsafe {
        SetTimer(std::ptr::null_mut(), 0, 500, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if !tick() {
                break;
            }
        }
    }
}
