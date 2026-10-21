//! Things every tab can do: run cloud actions, copy files in, edit the config, show toasts,
//! jump to another tab.

use crate::model::Filter;
use crate::providers::Choice;
use crate::snapshot::Snapshot;
use crate::window::Hooks;
use crate::{fsops, rt, ui};
use adw::prelude::*;
use clouddrop::actions::{self, Ctx};
use clouddrop::config::{self, Config};
use clouddrop::control;
use clouddrop::db::{Db, Kind};
use clouddrop::util;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// A request to switch tabs.
#[derive(Debug, Clone)]
pub enum Nav {
    Files { dir: String, filter: Filter, select: Option<String> },
    Activity,
}

type NavHandler = Rc<dyn Fn(Nav)>;

pub struct Ops {
    pub win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    hooks: Rc<Hooks>,
    cfg: RefCell<Option<Config>>,
    provider: RefCell<Choice>,
    paused: Cell<bool>,
    busy: Cell<u32>,
    nav: RefCell<Option<NavHandler>>,
    restart: RefCell<Option<glib::SourceId>>,
    restart_msg: RefCell<String>,
}

impl Ops {
    pub fn new(win: &adw::ApplicationWindow, toasts: &adw::ToastOverlay, hooks: Rc<Hooks>) -> Rc<Self> {
        Rc::new(Ops {
            win: win.clone(),
            toasts: toasts.clone(),
            hooks,
            cfg: RefCell::default(),
            provider: RefCell::new(Choice::current(None)),
            paused: Cell::new(false),
            busy: Cell::new(0),
            nav: RefCell::default(),
            restart: RefCell::default(),
            restart_msg: RefCell::default(),
        })
    }

    pub fn set_nav(&self, f: impl Fn(Nav) + 'static) {
        *self.nav.borrow_mut() = Some(Rc::new(f));
    }

    pub fn navigate(&self, to: Nav) {
        let f = self.nav.borrow().clone();
        if let Some(f) = f {
            f(to);
        }
    }

    pub fn set_snapshot(&self, s: &Snapshot) {
        *self.cfg.borrow_mut() = s.cfg.clone();
        *self.provider.borrow_mut() = s.provider.clone();
        self.paused.set(s.paused);
    }

    /// The cloud CloudDrop uploads to, for names and "Open in …".
    pub fn provider(&self) -> Choice {
        self.provider.borrow().clone()
    }

    pub fn cfg(&self) -> Option<Config> {
        self.cfg.borrow().clone()
    }

    pub fn sync_dir(&self) -> Option<PathBuf> {
        self.cfg.borrow().as_ref().map(|c| c.sync_dir.clone())
    }

    pub fn paused(&self) -> bool {
        self.paused.get()
    }

    /// A cloud action (free up space, download) is running.
    pub fn busy(&self) -> bool {
        self.busy.get() > 0
    }

    pub fn db(&self) -> Option<Arc<Db>> {
        (self.hooks.db)()
    }

    pub fn refresh(&self) {
        (self.hooks.refresh)();
    }

    pub fn hooks(&self) -> &Rc<Hooks> {
        &self.hooks
    }

    // ---------- feedback ----------

    pub fn toast(&self, text: &str) {
        ui::toast(&self.toasts, text);
    }

    pub fn toast_with_button(&self, text: &str, button: &str, on_click: impl Fn() + 'static) {
        let t = adw::Toast::new(text);
        t.set_timeout(8);
        t.set_button_label(Some(button));
        t.connect_button_clicked(move |_| on_click());
        self.toasts.add_toast(t);
    }

    // ---------- simple actions ----------

    pub fn set_paused(&self, paused: bool) {
        let result = self
            .db()
            .ok_or_else(|| anyhow::anyhow!("CloudDrop isn't set up yet"))
            .and_then(|db| control::set_paused(&db, paused));
        match result {
            Ok(()) => {
                self.paused.set(paused);
                self.toast(if paused { "Uploads paused" } else { "Uploads resumed" });
                self.refresh();
            }
            Err(e) => self.toast(&format!("{e:#}")),
        }
    }

    pub fn retry(&self, prefixes: &[String]) {
        let Some(db) = self.db() else {
            return;
        };
        let mut total = 0;
        for p in prefixes {
            match db.retry(p) {
                Ok(n) => total += n,
                Err(e) => return self.toast(&format!("{e:#}")),
            }
        }
        self.toast(&match total {
            0 => "Nothing to retry".to_string(),
            1 => "Retrying 1 upload".to_string(),
            n => format!("Retrying {n} uploads"),
        });
        self.refresh();
    }

    pub fn open_folder(&self) {
        if let Some(dir) = self.sync_dir() {
            control::open_uri(&ui::file_uri(&dir));
        }
    }

    pub fn open_in_drive(&self, kind: Kind, id: &str) {
        let provider = self.cfg.borrow().as_ref().map(|c| c.provider).unwrap_or_default();
        if let Some(url) = provider.web_url(kind, id) {
            control::open_uri(&url);
        }
    }

    /// Opens a file with its default app, or a folder in Files.
    pub fn open_local(&self, rel: &str) {
        if let Some(dir) = self.sync_dir() {
            control::open_uri(&ui::file_uri(&dir.join(rel)));
        }
    }

    /// Reveals an item in the file manager (its folder, if the item itself is gone).
    pub fn show_in_files(&self, rel: &str) {
        let Some(root) = self.sync_dir() else {
            return;
        };
        let abs = root.join(rel);
        if abs.symlink_metadata().is_err() {
            // Cloud-only: show the nearest folder that exists.
            let mut up = abs.parent();
            while let Some(p) = up {
                if p.is_dir() {
                    return control::open_uri(&ui::file_uri(p));
                }
                up = p.parent();
            }
            return control::open_uri(&ui::file_uri(&root));
        }
        let parent = abs.parent().unwrap_or(&root).to_path_buf();
        gtk::FileLauncher::new(Some(&gio::File::for_path(&abs))).open_containing_folder(
            Some(&self.win),
            gio::Cancellable::NONE,
            move |res| {
                if res.is_err() {
                    control::open_uri(&ui::file_uri(&parent));
                }
            },
        );
    }

    // ---------- cloud actions ----------

    /// Removes verified local copies under each path.
    pub fn free(self: &Rc<Self>, rels: Vec<String>, done: impl FnOnce() + 'static) {
        self.busy.set(self.busy.get() + 1);
        let me = self.clone();
        rt::run(
            async move {
                let ctx = Ctx::load()?;
                let remote = clouddrop::remote_for(&ctx.cfg, clouddrop::auth::token_path());
                anyhow::Ok(actions::free(&ctx, &remote, &rels).await?.summary())
            },
            move |res| {
                me.busy.set(me.busy.get() - 1);
                me.toast(&res.unwrap_or_else(|e| format!("{e:#}")));
                me.refresh();
                done();
            },
        );
    }

    /// Downloads cloud-only files under each path.
    pub fn restore(self: &Rc<Self>, rels: Vec<String>, what: &str, done: impl FnOnce() + 'static) {
        self.busy.set(self.busy.get() + 1);
        self.toast(&format!("Downloading {what}…"));
        let me = self.clone();
        rt::run(
            async move {
                let ctx = Ctx::load()?;
                let remote = clouddrop::remote_for(&ctx.cfg, clouddrop::auth::token_path());
                anyhow::Ok(actions::restore(&ctx, &remote, &rels).await?.summary())
            },
            move |res| {
                me.busy.set(me.busy.get() - 1);
                me.toast(&res.unwrap_or_else(|e| format!("{e:#}")));
                me.refresh();
                done();
            },
        );
    }

    /// Asks before freeing; `done` runs after the action finished (not if cancelled).
    pub fn confirm_free(self: &Rc<Self>, rels: Vec<String>, heading: &str, body: &str, done: impl FnOnce() + 'static) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("free", "Free Up Space");
        dialog.set_response_appearance("free", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let me = self.clone();
        let done = RefCell::new(Some(done));
        dialog.connect_response(Some("free"), move |_, _| {
            if let Some(done) = done.borrow_mut().take() {
                me.free(rels.clone(), done);
            }
        });
        dialog.present(Some(&self.win));
    }

    // ---------- adding files ----------

    /// Lets the user pick files and copies them into `dest_rel` ("" = the top folder).
    pub fn add_files(self: &Rc<Self>, dest_rel: String) {
        let dialog = gtk::FileDialog::builder().title("Add Files to CloudDrop").modal(true).build();
        let me = self.clone();
        dialog.open_multiple(Some(&self.win), gio::Cancellable::NONE, move |res| {
            let Ok(list) = res else {
                return; // cancelled
            };
            let paths: Vec<PathBuf> = list
                .iter::<gio::File>()
                .filter_map(Result::ok)
                .filter_map(|f| f.path())
                .collect();
            me.copy_in(dest_rel.clone(), paths);
        });
    }

    /// Copies files and folders into `dest_rel` without replacing anything.
    pub fn copy_in(self: &Rc<Self>, dest_rel: String, sources: Vec<PathBuf>) {
        let (Some(root), Some(db)) = (self.sync_dir(), self.db()) else {
            return self.toast("CloudDrop isn't ready yet");
        };
        if sources.is_empty() {
            return;
        }
        let dest_name = if dest_rel.is_empty() { ui::root_name() } else { util::name_of(&dest_rel).to_string() };
        let dest_dir = root.join(&dest_rel);
        let me = self.clone();
        rt::blocking(
            move || {
                let taken = |p: &Path| {
                    p.strip_prefix(&root)
                        .ok()
                        .and_then(|r| r.to_str())
                        .is_some_and(|r| db.get(r).ok().flatten().is_some())
                };
                fsops::copy_into(&dest_dir, &sources, &taken)
            },
            move |report| {
                me.toast(&report.summary(&dest_name));
                me.refresh();
            },
        );
    }

    // ---------- configuration ----------

    /// Edits the config file now and restarts this connection's engine shortly after (several
    /// quick edits share one restart; the others carry on). `message` is shown once it's back.
    pub fn edit_config(self: &Rc<Self>, message: &str, edit: impl FnOnce(&mut Config)) -> bool {
        let path = config::config_path();
        let result = Config::load_from(&path).and_then(|mut c| {
            edit(&mut c);
            c.validate()?;
            c.save_to(&path)
        });
        if let Err(e) = result {
            self.toast(&format!("Couldn't save: {e:#}"));
            return false;
        }
        *self.restart_msg.borrow_mut() = message.to_string();
        if let Some(id) = self.restart.borrow_mut().take() {
            id.remove();
        }
        let me = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            if let Some(me) = me.upgrade() {
                me.restart.borrow_mut().take();
                me.restart_engine();
            }
        });
        *self.restart.borrow_mut() = Some(id);
        // No immediate refresh: the caller may be a widget in the middle of handling a click
        // (a combo row's popup, a row's remove button) that the next refresh would rebuild.
        // The one-second tick shows the new settings.
        true
    }

    pub fn restart_now(self: &Rc<Self>, message: &str) {
        *self.restart_msg.borrow_mut() = message.to_string();
        self.restart_service();
    }

    /// Restarts the shown connection's engine (the background service starts it again at once).
    fn restart_engine(self: &Rc<Self>) {
        let Some(db) = self.db() else {
            return;
        };
        let msg = std::mem::take(&mut *self.restart_msg.borrow_mut());
        self.toast(&match control::restart_engine(&db, &config::data_dir()) {
            Ok(()) if msg.is_empty() => "Settings saved".to_string(),
            Ok(()) => msg,
            Err(e) => format!("{e:#}"),
        });
    }

    fn restart_service(self: &Rc<Self>) {
        let me = self.clone();
        rt::blocking(control::restart_service, move |res| {
            let msg = std::mem::take(&mut *me.restart_msg.borrow_mut());
            me.toast(&match res {
                Ok(()) if msg.is_empty() => "Settings saved".to_string(),
                Ok(()) => msg,
                Err(e) => format!("{e:#}"),
            });
        });
    }
}
