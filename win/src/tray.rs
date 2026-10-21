//! The notification-area icon: status at a glance and quick actions. Runs the
//! main thread's Windows message loop until Quit.
//!
//! With one connection the menu is as it always was. With several, it lists each one (its name
//! and status line) with a submenu: Open Folder, Open in …, Pause/Resume. The icon shows the most
//! urgent state among them.

use crate::agent::EXIT;
use clouddrop::config::Config;
use clouddrop::connections::{self, Connection};
use clouddrop::control::{self, Phase};
use clouddrop::db::Kind;
use clouddrop::remote::Provider;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const SIZE: u32 = 32;
static IDLE: &[u8] = include_bytes!("../assets/tray-idle.rgba");
static SYNCING: &[u8] = include_bytes!("../assets/tray-syncing.rgba");
static PAUSED: &[u8] = include_bytes!("../assets/tray-paused.rgba");
static ATTENTION: &[u8] = include_bytes!("../assets/tray-attention.rgba");
/// How often the list of connections is read again.
const LIST_EVERY: Duration = Duration::from_secs(2);

/// Ordered by urgency: the icon shows the most urgent connection's look.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Look {
    Idle,
    Paused,
    Syncing,
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

/// One connection as the menu shows it.
#[derive(Clone)]
struct Row {
    conn: Connection,
    label: String,
    look: Look,
    line: String,
    paused: bool,
    provider: Provider,
}

/// A connection's look, one-line status and whether it is paused.
fn row(conn: &Connection) -> Row {
    let cfg = conn.load().ok();
    let provider = cfg.as_ref().map(|c| c.provider).unwrap_or_default();
    let label = conn.label(cfg.as_ref());
    let make = |look, line: &str, paused| Row {
        conn: conn.clone(),
        label: label.clone(),
        look,
        line: line.to_string(),
        paused,
        provider,
    };
    // Google Drive needs its client; the other providers keep their settings elsewhere.
    let Some(cfg) = cfg.filter(control::configured) else {
        return make(Look::Attention, "Not set up — open CloudDrop", false);
    };
    // Only Google Drive's sign-in is a file here; the engine reports the others' (NeedsSignIn).
    if cfg.provider.is_default() && !control::signed_in(cfg.provider, &conn.data) {
        return make(Look::Attention, "Sign-in needed", false);
    }
    let Some(s) = conn.live_status() else {
        return make(Look::Syncing, "Starting…", false);
    };
    let files = |n: u64| if n == 1 { "1 file".to_string() } else { format!("{n} files") };
    // Proving cloud copies by downloading them (control::VERIFYING); the phase stays as it is.
    let verifying = s.message.as_deref().is_some_and(|m| m.to_lowercase().starts_with("verifying"));
    if verifying && matches!(s.phase, Phase::Idle | Phase::Waiting | Phase::Uploading) {
        return make(Look::Syncing, "Verifying…", false);
    }
    match s.phase {
        Phase::Idle => make(Look::Idle, "Up to date", false),
        Phase::Paused => make(Look::Paused, "Paused", true),
        Phase::Uploading | Phase::Waiting => {
            let (sent, total) = s.uploads.iter().fold((0, 0), |(a, b), u| (a + u.sent, b + u.total));
            let pct = (sent * 100).checked_div(total).unwrap_or(0);
            make(Look::Syncing, &format!("Uploading {} — {pct}%", files(s.waiting.max(1))), false)
        }
        Phase::Starting => make(Look::Syncing, "Starting…", false),
        Phase::Offline => make(Look::Attention, "Offline — retrying", false),
        Phase::NeedsSignIn => make(Look::Attention, "Sign-in needed", false),
        Phase::DriveFolderMissing => {
            let line = if cfg.provider.is_default() { "Drive folder missing" } else { "Cloud folder missing" };
            make(Look::Attention, line, false)
        }
    }
}

/// The icon's look and the status line for all of them: the one connection's, or the most
/// urgent one's, named.
fn overall(rows: &[Row]) -> (Look, String) {
    match rows {
        [] => (Look::Attention, "Not set up — open CloudDrop".into()),
        [one] => (one.look, one.line.clone()),
        _ => {
            let worst = rows.iter().max_by_key(|r| r.look).expect("not empty");
            if worst.look == Look::Idle {
                (Look::Idle, "Up to date".into())
            } else {
                (worst.look, format!("{}: {}", worst.label, worst.line))
            }
        }
    }
}

/// The web page of a connection's cloud folder, once it exists. Asked with the folder's real id:
/// some providers link only ids of their own form.
fn web_url(conn: &Connection, provider: Provider) -> Option<String> {
    let id = conn.open_db().ok()?.meta_get("root_id").ok()??;
    provider.web_url(Kind::Dir, &id)
}

fn open(target: &str) {
    let _ = std::process::Command::new("explorer.exe").arg(target).spawn();
}

fn open_folder(conn: &Connection) {
    if let Ok(cfg) = conn.load() {
        open(&cfg.sync_dir.display().to_string());
    }
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

#[derive(Clone)]
enum Action {
    OpenFolder(Connection),
    OpenWeb(Connection, Provider),
    TogglePause(Connection),
    Show,
    Quit,
}

/// The menu as built for a set of connections; rebuilt when the set changes.
struct Built {
    /// (name, label, has a web page) of each connection, in order.
    key: Vec<(String, String, bool)>,
    actions: HashMap<MenuId, Action>,
    status: MenuItem,
    /// Per connection: its submenu (several) and its pause item.
    rows: Vec<(Option<Submenu>, MenuItem)>,
    /// What the items show now: the status line, and each connection's line and pause state.
    shown: Option<(String, Vec<(String, bool)>)>,
}

fn build(rows: &[Row]) -> (Menu, Built) {
    let menu = Menu::new();
    let mut actions = HashMap::new();
    let mut add = |item: &MenuItem, action: Action| {
        actions.insert(item.id().clone(), action);
    };
    let status = MenuItem::new("CloudDrop", false, None);
    let _ = menu.append(&status);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let mut built_rows = Vec::new();
    let mut key = Vec::new();
    let pause_text = |paused: bool| if paused { "Resume Uploads" } else { "Pause Uploads" };
    if let [one] = rows {
        let url = web_url(&one.conn, one.provider);
        key.push((one.conn.name.clone(), one.label.clone(), url.is_some()));
        let folder = MenuItem::new("Open CloudDrop Folder", true, None);
        add(&folder, Action::OpenFolder(one.conn.clone()));
        let _ = menu.append(&folder);
        // Providers without web pages (WebDAV, S3, SFTP) have nothing to open.
        if url.is_some() {
            let web = MenuItem::new(format!("Open in {}", one.provider.display_name()), true, None);
            add(&web, Action::OpenWeb(one.conn.clone(), one.provider));
            let _ = menu.append(&web);
        }
        let show = MenuItem::new("Show CloudDrop", true, None);
        add(&show, Action::Show);
        let pause = MenuItem::new(pause_text(one.paused), true, None);
        add(&pause, Action::TogglePause(one.conn.clone()));
        let items: [&dyn IsMenuItem; 4] = [&show, &PredefinedMenuItem::separator(), &pause, &PredefinedMenuItem::separator()];
        for item in items {
            let _ = menu.append(item);
        }
        built_rows.push((None, pause));
    } else {
        for r in rows {
            let url = web_url(&r.conn, r.provider);
            key.push((r.conn.name.clone(), r.label.clone(), url.is_some()));
            let sub = Submenu::new(format!("{} — {}", r.label, r.line), true);
            let folder = MenuItem::new("Open Folder", true, None);
            add(&folder, Action::OpenFolder(r.conn.clone()));
            let _ = sub.append(&folder);
            if url.is_some() {
                let web = MenuItem::new(format!("Open in {}", r.provider.display_name()), true, None);
                add(&web, Action::OpenWeb(r.conn.clone(), r.provider));
                let _ = sub.append(&web);
            }
            let pause = MenuItem::new(pause_text(r.paused), true, None);
            add(&pause, Action::TogglePause(r.conn.clone()));
            let _ = sub.append(&pause);
            let _ = menu.append(&sub);
            built_rows.push((Some(sub), pause));
        }
        let show = MenuItem::new("Show CloudDrop", true, None);
        add(&show, Action::Show);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&show);
        let _ = menu.append(&PredefinedMenuItem::separator());
    }
    let quit = MenuItem::new("Quit", true, None);
    add(&quit, Action::Quit);
    let _ = menu.append(&quit);
    (menu, Built { key, actions, status, rows: built_rows, shown: None })
}

/// Puts the current lines and pause labels on a built menu, where they changed.
fn label(built: &mut Built, rows: &[Row], line: &str) {
    let now = (line.to_string(), rows.iter().map(|r| (r.line.clone(), r.paused)).collect::<Vec<_>>());
    if built.shown.as_ref() == Some(&now) {
        return;
    }
    built.status.set_text(line);
    for ((sub, pause), r) in built.rows.iter().zip(rows) {
        if let Some(sub) = sub {
            sub.set_text(format!("{} — {}", r.label, r.line));
        }
        pause.set_text(if r.paused { "Resume Uploads" } else { "Pause Uploads" });
    }
    built.shown = Some(now);
}

fn act(action: &Action, rows: &[Row]) {
    match action {
        Action::OpenFolder(conn) => open_folder(conn),
        Action::OpenWeb(conn, provider) => {
            if let Some(url) = web_url(conn, *provider) {
                open(&url);
            }
        }
        Action::TogglePause(conn) => {
            let paused = rows.iter().find(|r| r.conn.name == conn.name).is_some_and(|r| r.paused);
            if let Ok(db) = conn.open_db() {
                let _ = control::set_paused(&db, !paused);
            }
        }
        Action::Show => show_app(),
        Action::Quit => EXIT.store(true, Ordering::SeqCst),
    }
}

pub fn run() {
    let mut conns = connections::all();
    let mut listed = Instant::now();
    let mut rows: Vec<Row> = conns.iter().map(row).collect();
    let (mut look, mut line) = overall(&rows);
    let (menu, mut built) = build(&rows);
    let tray: TrayIcon = match TrayIconBuilder::new()
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
                std::thread::sleep(Duration::from_secs(1));
            }
            return;
        }
    };
    label(&mut built, &rows, &line);

    message_loop(|| {
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if let Some(action) = built.actions.get(&ev.id) {
                act(action, &rows);
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
        if listed.elapsed() >= LIST_EVERY {
            conns = connections::all();
            listed = Instant::now();
        }
        rows = conns.iter().map(row).collect();
        let (l, text) = overall(&rows);
        // Connections added, removed or renamed, or a cloud folder that now has a web page.
        let key: Vec<(String, String, bool)> = rows
            .iter()
            .zip(&built.key)
            .map(|(r, k)| (r.conn.name.clone(), r.label.clone(), k.2))
            .collect();
        let changed = rows.len() != built.key.len()
            || key.iter().zip(&built.key).any(|(a, b)| a.0 != b.0 || a.1 != b.1)
            || (listed.elapsed() < Duration::from_millis(600)
                && rows.iter().zip(&built.key).any(|(r, k)| web_url(&r.conn, r.provider).is_some() != k.2));
        if changed {
            let (menu, b) = build(&rows);
            tray.set_menu(Some(Box::new(menu)));
            built = b;
        }
        label(&mut built, &rows, &text);
        if l != look {
            let _ = tray.set_icon(Some(icon(l)));
            look = l;
        }
        if text != line {
            let _ = tray.set_tooltip(Some(format!("CloudDrop — {text}")));
            line = text;
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
