//! CloudDrop desktop app: setup assistant, home/files/activity/settings window and top-bar icon.
//!
//! `clouddrop-gui` opens the window (or the setup assistant on first run);
//! `clouddrop-gui --tray` only shows the top-bar icon (used at login). A second
//! launch talks to the running instance instead of starting another one.

mod activity;
mod cells;
mod cli;
mod dbwatch;
mod files;
mod fmt;
mod fsops;
mod home;
mod model;
mod ops;
mod providers;
mod rt;
mod settings;
mod setup;
mod signin;
mod snapshot;
mod ui;
mod window;

use adw::prelude::*;
use clouddrop::connections::{self, Connection};
use clouddrop::control::{self, APP_ID};
use clouddrop::db::{Db, Kind};
use clouddrop_tray::{Tray, TrayCommand, TrayConnection, TrayState};
use gtk::{gio, glib};
use snapshot::{RateMeter, Snapshot, summarize};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use window::{ConnRow, Hooks, MainWindow};

struct App {
    adw: adw::Application,
    window: RefCell<Option<Rc<MainWindow>>>,
    wizard: RefCell<Option<adw::ApplicationWindow>>,
    tray: RefCell<Option<Arc<Tray>>>,
    tray_starting: Cell<bool>,
    /// Failed attempts to show the icon (the shell may not be ready right after login).
    tray_attempts: Cell<u32>,
    /// Held while this process shows the top-bar icon, so a second CloudDrop
    /// process (the snap runs the window and the icon separately) doesn't add one.
    tray_lock: RefCell<Option<std::fs::File>>,
    tray_state: RefCell<Option<TrayState>>,
    tray_tx: async_channel::Sender<TrayCommand>,
    tray_rx: RefCell<Option<async_channel::Receiver<TrayCommand>>>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    db: RefCell<Option<Arc<Db>>>,
    snap: RefCell<Option<Snapshot>>,
    /// Every connection's status (the shown one's is `snap`), for the switcher and the icon.
    all: RefCell<Vec<Snapshot>>,
    /// The other connections' databases, opened once each.
    other_dbs: RefCell<HashMap<String, Arc<Db>>>,
    /// There were connections at the last look (so removing the last one starts setup again).
    had_connections: Cell<bool>,
    /// Upload speed, measured across the once-a-second snapshots.
    rate: RefCell<RateMeter>,
    started: Cell<bool>,
}

/// Where our icons live: the installed hicolor theme, or the source tree in development.
fn icon_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let installed = glib::user_data_dir().join("icons");
    if installed
        .join("hicolor/scalable/status/clouddrop-idle-symbolic.svg")
        .exists()
    {
        dirs.push(installed);
    }
    let dev = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/icons"));
    if dev.is_dir() {
        dirs.push(dev);
    }
    dirs
}

/// How urgent a connection's state is, for the icon's look with several.
fn urgency(icon: clouddrop_tray::TrayIcon) -> u8 {
    use clouddrop_tray::TrayIcon as T;
    match icon {
        T::Idle => 0,
        T::Paused => 1,
        T::Syncing => 2,
        T::Attention => 3,
    }
}

/// The top-bar icon for the shown connection `s` and all of them (`all`, `s` among them): with
/// several, the most urgent one's look and line, and a row for each.
fn tray_state(s: &Snapshot, all: &[Snapshot]) -> TrayState {
    let sum = summarize(s);
    let mut state = TrayState {
        icon: sum.tray,
        status_line: sum.line,
        paused: s.paused,
        can_pause: s.ready(),
        has_drive_folder: s.root_id.is_some(),
        provider_name: s.provider.name.clone(),
        web_links: s.provider.can_open_web(),
        connections: Vec::new(),
    };
    if all.len() > 1 {
        let sums: Vec<_> = all.iter().map(summarize).collect();
        state.connections = all
            .iter()
            .zip(&sums)
            .map(|(c, sum)| TrayConnection {
                name: c.connection.clone(),
                label: c.label.clone(),
                status_line: sum.line.clone(),
                paused: c.paused,
                can_pause: c.ready(),
                has_drive_folder: c.root_id.is_some(),
                provider_name: c.provider.name.clone(),
                web_links: c.provider.can_open_web(),
            })
            .collect();
        let (worst, wsum) = all.iter().zip(&sums).max_by_key(|(_, s)| urgency(s.tray)).expect("several");
        state.icon = wsum.tray;
        state.status_line =
            if urgency(wsum.tray) == 0 { "Up to date".into() } else { format!("{}: {}", worst.label, wsum.line) };
    }
    state
}

/// The switcher's rows.
fn conn_rows(all: &[Snapshot]) -> Vec<ConnRow> {
    all.iter()
        .map(|c| {
            let sum = summarize(c);
            ConnRow {
                name: c.connection.clone(),
                label: c.label.clone(),
                provider: c.provider.name.clone(),
                icon: sum.icon,
                line: sum.line,
            }
        })
        .collect()
}

/// `app.json` in the data directory: the connection the window showed last.
fn remembered_file() -> PathBuf {
    clouddrop::config::base_data_dir().join("app.json")
}

fn remembered() -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(remembered_file()).ok()?).ok()?;
    v["connection"].as_str().map(str::to_string)
}

fn remember(name: &str) {
    let file = remembered_file();
    let mut v: serde_json::Value = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    v["connection"] = serde_json::json!(name);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, v.to_string());
}

fn exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clouddrop-gui"))
}

impl App {
    fn startup(self: &Rc<Self>) {
        if self.started.replace(true) {
            return;
        }
        if let Some(display) = gtk::gdk::Display::default() {
            let theme = gtk::IconTheme::for_display(&display);
            for d in icon_dirs() {
                theme.add_search_path(&d);
            }
        }
        // Before any window: the setup assistant uses it too.
        ui::install_css();
        let about = gio::SimpleAction::new("about", None);
        let me = self.clone();
        about.connect_activate(move |_, _| me.show_about());
        self.adw.add_action(&about);
        let add_folder = gio::SimpleAction::new("add-folder", None);
        let me = self.clone();
        add_folder.connect_activate(move |_, _| me.show_setup(setup::Start::AddFolder));
        self.adw.add_action(&add_folder);
        // The connection shown last time, if it is still there.
        if let Some(name) = remembered()
            && let Ok(conn) = Connection::named(&name)
            && conn.exists()
        {
            connections::select(&conn);
        }
        if let Some(rx) = self.tray_rx.borrow_mut().take() {
            let me = self.clone();
            glib::spawn_future_local(async move {
                while let Ok(cmd) = rx.recv().await {
                    me.on_tray(cmd);
                }
            });
        }
        let me = self.clone();
        glib::timeout_add_seconds_local(1, move || {
            me.refresh();
            glib::ControlFlow::Continue
        });
        self.refresh();
        if control::autostart_enabled() {
            self.ensure_tray();
        }
    }

    /// The shared state database, opened on first use once CloudDrop is configured.
    fn db(&self) -> Option<Arc<Db>> {
        if self.db.borrow().is_none() && providers::is_configured() {
            *self.db.borrow_mut() = Db::open_default().ok().map(Arc::new);
        }
        self.db.borrow().clone()
    }

    fn snapshot(&self) -> Snapshot {
        let db = self.db();
        Snapshot::load(db.as_deref())
    }

    /// Shows the connection `name` in the window (and remembers it).
    fn switch_to(self: &Rc<Self>, name: &str) {
        let Ok(conn) = Connection::named(name) else {
            return;
        };
        if conn == connections::current() {
            return;
        }
        connections::select(&conn);
        remember(&conn.name);
        *self.db.borrow_mut() = None;
        *self.rate.borrow_mut() = RateMeter::default();
        self.refresh();
    }

    /// A status snapshot of a connection the window doesn't show.
    fn other(&self, conn: &Connection) -> Snapshot {
        let mut dbs = self.other_dbs.borrow_mut();
        if !dbs.contains_key(&conn.name)
            && conn.exists()
            && let Ok(db) = conn.open_db()
        {
            dbs.insert(conn.name.clone(), Arc::new(db));
        }
        Snapshot::status_of(conn, dbs.get(&conn.name).map(|d| d.as_ref()))
    }

    /// The connection's database: the shown one's, or another's.
    fn db_of(&self, name: &str) -> Option<Arc<Db>> {
        if connections::current().name == name {
            return self.db();
        }
        let conn = Connection::named(name).ok().filter(Connection::exists)?;
        self.other(&conn);
        self.other_dbs.borrow().get(name).cloned()
    }

    fn refresh(self: &Rc<Self>) {
        let list = connections::all();
        let current = connections::current();
        // The connection shown was removed (here or elsewhere): show another. Not while the
        // assistant sets up a new one, which isn't in the list until it is written.
        if self.wizard.borrow().is_none() && !list.is_empty() && !list.contains(&current) {
            let first = list[0].name.clone();
            connections::select(&list[0]);
            remember(&first);
            *self.db.borrow_mut() = None;
        }
        if self.wizard.borrow().is_none() && list.is_empty() && !current.is_default() {
            connections::select(&Connection::default_one());
            *self.db.borrow_mut() = None;
        }
        self.other_dbs.borrow_mut().retain(|name, _| list.iter().any(|c| &c.name == name));
        let mut s = self.snapshot();
        {
            let mut rate = self.rate.borrow_mut();
            rate.add(s.daemon.as_ref());
            s.rate = rate.bytes_per_second();
        }
        let all: Vec<Snapshot> =
            list.iter().map(|c| if c.name == s.connection { s.clone() } else { self.other(c) }).collect();
        let window = self.window.borrow().clone();
        if let Some(w) = &window {
            w.refresh(&s, &conn_rows(&all));
        }
        // The last folder was removed: set up again.
        let had = self.had_connections.replace(!list.is_empty());
        if had && list.is_empty() && self.wizard.borrow().is_none() && window.is_some_and(|w| w.win.is_visible()) {
            self.show_setup(setup::Start::Welcome);
        }
        let state = tray_state(&s, &all);
        let tray = self.tray.borrow().clone();
        if let Some(tray) = tray
            && self.tray_state.borrow().as_ref() != Some(&state)
        {
            *self.tray_state.borrow_mut() = Some(state.clone());
            rt::runtime().spawn(async move { tray.update(state).await });
        }
        *self.snap.borrow_mut() = Some(s);
        *self.all.borrow_mut() = all;
    }

    /// Opens the window, or the setup assistant if CloudDrop isn't ready yet.
    fn present(self: &Rc<Self>) {
        let wizard = self.wizard.borrow().clone();
        if let Some(w) = wizard {
            w.present();
            return;
        }
        let ready = self.snap.borrow().as_ref().is_some_and(Snapshot::ready);
        if ready {
            self.show_window();
        } else {
            self.show_setup(setup::Start::Welcome);
        }
    }

    fn hooks(self: &Rc<Self>) -> Rc<Hooks> {
        let (a, b, c, d, e, f) = (self.clone(), self.clone(), self.clone(), self.clone(), self.clone(), self.clone());
        Rc::new(Hooks {
            sign_in: Box::new(move || a.sign_in()),
            setup: Box::new(move |start| b.show_setup(start)),
            set_tray: Box::new(move |on| c.set_tray(on)),
            refresh: Box::new(move || d.refresh()),
            db: Box::new(move || e.db()),
            switch: Box::new(move |name| f.switch_to(&name)),
        })
    }

    fn show_window(self: &Rc<Self>) {
        let existing = self.window.borrow().clone();
        let w = match existing {
            Some(w) => w,
            None => {
                let w = MainWindow::new(&self.adw, self.hooks());
                let me = self.clone();
                w.win.connect_close_request(move |win| {
                    if me.tray.borrow().is_some() {
                        // Keep running in the top bar.
                        win.set_visible(false);
                        glib::Propagation::Stop
                    } else {
                        me.window.borrow_mut().take();
                        glib::Propagation::Proceed
                    }
                });
                let snap = self.snap.borrow().clone();
                let all = self.all.borrow().clone();
                if let Some(s) = snap {
                    w.refresh(&s, &conn_rows(&all));
                }
                *self.window.borrow_mut() = Some(w.clone());
                w
            }
        };
        w.present();
    }

    fn show_setup(self: &Rc<Self>, start: setup::Start) {
        let wizard = self.wizard.borrow().clone();
        if let Some(w) = wizard {
            w.present();
            return;
        }
        let me = self.clone();
        let wiz = setup::build(&self.adw, start, move |tray| {
            if let Err(e) = control::set_autostart(tray, &exe()) {
                eprintln!("autostart: {e:#}");
            }
            if tray {
                me.ensure_tray();
            }
            // The window shows the connection just set up.
            remember(&connections::current().name);
            *me.db.borrow_mut() = None;
            me.refresh();
            me.show_window();
        });
        let me = self.clone();
        // On close rather than destroy: the assistant's own closures keep it alive, so GTK
        // never disposes it.
        wiz.connect_close_request(move |_| {
            me.wizard.borrow_mut().take();
            // Leaving "Add Another Folder" early goes back to the connection shown before.
            *me.db.borrow_mut() = None;
            // Closed without finishing (switching providers, then Cancel): back to the window
            // it replaced, if there is one.
            let window = me.window.borrow().clone();
            if let Some(w) = window
                && !w.win.is_visible()
            {
                me.refresh();
                w.present();
            }
            glib::Propagation::Proceed
        });
        let window = self.window.borrow().clone();
        if let Some(w) = window {
            w.win.set_visible(false);
        }
        wiz.present();
        *self.wizard.borrow_mut() = Some(wiz);
    }

    fn show_about(self: &Rc<Self>) {
        let parent = self.window.borrow().as_ref().map(|w| w.win.clone());
        let about = adw::AboutDialog::builder()
            .application_name("CloudDrop")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .comments("An upload-only cloud folder: everything you put in it is uploaded, and nothing is ever deleted in the cloud.")
            .developer_name("Dhiva Labs")
            .website("https://github.com/Dhiva-Labs/CloudDrop")
            .issue_url("https://github.com/Dhiva-Labs/CloudDrop/issues")
            .license_type(gtk::License::MitX11)
            .copyright("© 2026 Dhiva Labs")
            .build();
        about.add_link("Privacy Policy", "https://github.com/Dhiva-Labs/CloudDrop/blob/main/PRIVACY.md");
        about.present(parent.as_ref());
    }

    fn sign_in(self: &Rc<Self>) {
        if !providers::is_configured() {
            return self.show_setup(setup::Start::Welcome);
        }
        // Form providers (WebDAV, S3, SFTP) have no browser sign-in: their Connect page.
        let provider = self.snap.borrow().as_ref().map(|s| s.provider.clone());
        if provider.is_some_and(|p| p.is_form()) {
            return self.show_setup(setup::Start::SignIn);
        }
        self.show_window();
        let parent = self.window.borrow().as_ref().map(|w| w.win.clone());
        let me = self.clone();
        if let Some(parent) = parent {
            signin::dialog(&parent, move || {
                me.refresh();
                let window = me.window.borrow().clone();
                if let Some(w) = window {
                    w.toast("Signed in — uploads continue");
                }
            });
        }
    }

    fn set_tray(self: &Rc<Self>, on: bool) {
        if let Err(e) = control::set_autostart(on, &exe()) {
            let window = self.window.borrow().clone();
            if let Some(w) = window {
                w.toast(&format!("{e:#}"));
            }
        }
        if on {
            self.ensure_tray();
        } else {
            self.remove_tray();
        }
    }

    fn ensure_tray(self: &Rc<Self>) {
        if self.tray.borrow().is_some() || self.tray_starting.replace(true) {
            return;
        }
        if self.tray_lock.borrow().is_none() {
            match take_tray_lock() {
                Some(lock) => *self.tray_lock.borrow_mut() = Some(lock),
                None => {
                    // Another CloudDrop process shows the icon.
                    self.tray_starting.set(false);
                    return;
                }
            }
        }
        // Keep running without windows while the icon is shown.
        *self.hold.borrow_mut() = Some(self.adw.hold());
        let snap = self.snap.borrow().clone();
        let all = self.all.borrow().clone();
        let initial = tray_state(&snap.unwrap_or_else(|| self.snapshot()), &all);
        let tx = self.tray_tx.clone();
        let icons = icon_dirs().into_iter().next();
        let me = self.clone();
        let state = initial.clone();
        rt::run(
            async move {
                Tray::spawn(state, icons, move |cmd| {
                    let _ = tx.try_send(cmd);
                })
                .await
            },
            move |res| {
                me.tray_starting.set(false);
                match res {
                    Ok(tray) => {
                        me.tray_attempts.set(0);
                        *me.tray.borrow_mut() = Some(Arc::new(tray));
                        *me.tray_state.borrow_mut() = Some(initial);
                    }
                    Err(e) => {
                        let attempts = me.tray_attempts.get() + 1;
                        me.tray_attempts.set(attempts);
                        if attempts < 6 {
                            // Right after login the shell's icon host may not be up yet.
                            let me = me.clone();
                            glib::timeout_add_seconds_local_once(5, move || {
                                me.hold.borrow_mut().take();
                                me.ensure_tray();
                            });
                            return;
                        }
                        eprintln!("top-bar icon unavailable: {e:#}");
                        me.tray_attempts.set(0);
                        me.hold.borrow_mut().take();
                        let window = me.window.borrow().clone();
                        if let Some(w) = window {
                            w.toast("The top-bar icon needs the AppIndicator extension (on by default in Ubuntu)");
                        }
                    }
                }
            },
        );
    }

    fn remove_tray(&self) {
        if let Some(tray) = self.tray.borrow_mut().take() {
            rt::runtime().spawn(async move { tray.shutdown().await });
        }
        self.tray_state.borrow_mut().take();
        self.hold.borrow_mut().take();
        self.tray_lock.borrow_mut().take();
    }

    fn on_tray(self: &Rc<Self>, cmd: TrayCommand) {
        let snap = self.snap.borrow().clone();
        match cmd {
            TrayCommand::ShowWindow => self.present(),
            TrayCommand::OpenFolder => match snap.and_then(|s| s.cfg) {
                Some(cfg) => control::open_uri(&ui::file_uri(&cfg.sync_dir)),
                None => self.present(),
            },
            TrayCommand::OpenInDrive => {
                if let Some(s) = snap
                    && let Some(id) = &s.root_id
                {
                    let provider = s.cfg.as_ref().map(|c| c.provider).unwrap_or_default();
                    if let Some(url) = provider.web_url(Kind::Dir, id) {
                        control::open_uri(&url);
                    }
                }
            }
            TrayCommand::TogglePause => {
                let paused = snap.is_some_and(|s| s.paused);
                if let Ok(db) = Db::open_default() {
                    let _ = control::set_paused(&db, !paused);
                }
                self.refresh();
            }
            TrayCommand::OpenFolderOf(name) => {
                if let Some(cfg) = Connection::named(&name).ok().and_then(|c| c.load().ok()) {
                    control::open_uri(&ui::file_uri(&cfg.sync_dir));
                }
            }
            TrayCommand::OpenInDriveOf(name) => {
                let s = self.all.borrow().iter().find(|s| s.connection == name).cloned();
                if let Some(s) = s
                    && let (Some(id), Some(cfg)) = (&s.root_id, &s.cfg)
                    && let Some(url) = cfg.provider.web_url(Kind::Dir, id)
                {
                    control::open_uri(&url);
                }
            }
            TrayCommand::TogglePauseOf(name) => {
                let paused = self.all.borrow().iter().find(|s| s.connection == name).is_some_and(|s| s.paused);
                if let Some(db) = self.db_of(&name) {
                    let _ = control::set_paused(&db, !paused);
                }
                self.refresh();
            }
            TrayCommand::Quit => {
                self.remove_tray();
                for w in self.adw.windows() {
                    w.destroy();
                }
                self.adw.quit();
            }
        }
    }
}

/// The top-bar icon's lock, if no other CloudDrop process holds it (one icon shows every
/// connection, so it is the installation's, not a connection's).
fn take_tray_lock() -> Option<std::fs::File> {
    let dir = clouddrop::config::base_data_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::File::create(dir.join("tray.lock")).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

fn main() -> glib::ExitCode {
    // In the snap, owning the app's D-Bus name needs a slot that the store
    // reviews by hand, so each process runs on its own there.
    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if control::in_snap() {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let adw_app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();
    let (tray_tx, tray_rx) = async_channel::unbounded();
    let app = Rc::new(App {
        adw: adw_app.clone(),
        window: RefCell::default(),
        wizard: RefCell::default(),
        tray: RefCell::default(),
        tray_starting: Cell::new(false),
        tray_attempts: Cell::new(0),
        tray_lock: RefCell::default(),
        tray_state: RefCell::default(),
        tray_tx,
        tray_rx: RefCell::new(Some(tray_rx)),
        hold: RefCell::default(),
        db: RefCell::default(),
        snap: RefCell::default(),
        all: RefCell::default(),
        other_dbs: RefCell::default(),
        had_connections: Cell::new(false),
        rate: RefCell::default(),
        started: Cell::new(false),
    });
    {
        let app = app.clone();
        adw_app.connect_command_line(move |_, cmd| {
            let tray_only = cmd.arguments().iter().any(|a| a.to_str() == Some("--tray"));
            app.startup();
            if tray_only {
                app.ensure_tray();
            } else {
                app.present();
            }
            glib::ExitCode::SUCCESS
        });
    }
    {
        let app = app.clone();
        adw_app.connect_activate(move |_| {
            app.startup();
            app.present();
        });
    }
    adw_app.run()
}
