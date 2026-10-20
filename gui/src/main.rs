//! CloudDrop desktop app: setup assistant, home/files/activity/settings window and top-bar icon.
//!
//! `clouddrop-gui` opens the window (or the setup assistant on first run);
//! `clouddrop-gui --tray` only shows the top-bar icon (used at login). A second
//! launch talks to the running instance instead of starting another one.

mod activity;
mod cells;
mod dbwatch;
mod files;
mod fmt;
mod fsops;
mod home;
mod model;
mod ops;
mod rt;
mod settings;
mod setup;
mod signin;
mod snapshot;
mod ui;
mod window;

use adw::prelude::*;
use clouddrop::control::{self, APP_ID};
use clouddrop::db::{Db, Kind};
use clouddrop_tray::{Tray, TrayCommand, TrayState};
use gtk::{gio, glib};
use snapshot::{RateMeter, Snapshot, summarize};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use window::{Hooks, MainWindow};

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

fn tray_state(s: &Snapshot) -> TrayState {
    let sum = summarize(s);
    TrayState {
        icon: sum.tray,
        status_line: sum.line,
        paused: s.paused,
        can_pause: s.ready(),
        has_drive_folder: s.root_id.is_some(),
    }
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
        if self.db.borrow().is_none() && control::is_configured() {
            *self.db.borrow_mut() = Db::open_default().ok().map(Arc::new);
        }
        self.db.borrow().clone()
    }

    fn snapshot(&self) -> Snapshot {
        let db = self.db();
        Snapshot::load(db.as_deref())
    }

    fn refresh(self: &Rc<Self>) {
        let mut s = self.snapshot();
        {
            let mut rate = self.rate.borrow_mut();
            rate.add(s.daemon.as_ref());
            s.rate = rate.bytes_per_second();
        }
        let window = self.window.borrow().clone();
        if let Some(w) = window {
            w.refresh(&s);
        }
        let state = tray_state(&s);
        let tray = self.tray.borrow().clone();
        if let Some(tray) = tray
            && self.tray_state.borrow().as_ref() != Some(&state)
        {
            *self.tray_state.borrow_mut() = Some(state.clone());
            rt::runtime().spawn(async move { tray.update(state).await });
        }
        *self.snap.borrow_mut() = Some(s);
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
            self.show_setup();
        }
    }

    fn hooks(self: &Rc<Self>) -> Rc<Hooks> {
        let (a, b, c, d, e) = (self.clone(), self.clone(), self.clone(), self.clone(), self.clone());
        Rc::new(Hooks {
            sign_in: Box::new(move || a.sign_in()),
            setup: Box::new(move || b.show_setup()),
            set_tray: Box::new(move |on| c.set_tray(on)),
            refresh: Box::new(move || d.refresh()),
            db: Box::new(move || e.db()),
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
                if let Some(s) = snap {
                    w.refresh(&s);
                }
                *self.window.borrow_mut() = Some(w.clone());
                w
            }
        };
        w.present();
    }

    fn show_setup(self: &Rc<Self>) {
        let wizard = self.wizard.borrow().clone();
        if let Some(w) = wizard {
            w.present();
            return;
        }
        let me = self.clone();
        let wiz = setup::build(&self.adw, move |tray| {
            if let Err(e) = control::set_autostart(tray, &exe()) {
                eprintln!("autostart: {e:#}");
            }
            if tray {
                me.ensure_tray();
            }
            *me.db.borrow_mut() = None;
            me.refresh();
            me.show_window();
        });
        let me = self.clone();
        wiz.connect_destroy(move |_| {
            me.wizard.borrow_mut().take();
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
        if !control::is_configured() {
            return self.show_setup();
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
        let initial = tray_state(&snap.unwrap_or_else(|| self.snapshot()));
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
                    control::open_uri(&provider.web_url(Kind::Dir, id));
                }
            }
            TrayCommand::TogglePause => {
                let paused = snap.is_some_and(|s| s.paused);
                if let Ok(db) = Db::open_default() {
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

/// The top-bar icon's lock, if no other CloudDrop process holds it.
fn take_tray_lock() -> Option<std::fs::File> {
    let dir = clouddrop::config::data_dir();
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
