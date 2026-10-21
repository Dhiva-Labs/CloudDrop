//! Settings tab: account, folders, upload behaviour, startup.

use crate::ops::Ops;
use crate::providers::Choice;
use crate::snapshot::Snapshot;
use crate::{cli, fsops, rt, ui};
use adw::prelude::*;
use clouddrop::connections;
use clouddrop::control;
use clouddrop::db::Kind;
use clouddrop::util;
use gtk::gio;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The choices offered for automatic space saving.
pub const AUTO_FREE_PRESETS: [Option<u32>; 5] = [None, Some(0), Some(7), Some(30), Some(90)];

pub fn auto_free_label(days: Option<u32>) -> String {
    match days {
        None => "Never".into(),
        Some(0) => "Right after uploading".into(),
        Some(1) => "After 1 day".into(),
        Some(n) => format!("After {n} days"),
    }
}

/// The presets, plus the configured value when it is a custom one.
pub fn auto_free_options(current: Option<u32>) -> Vec<Option<u32>> {
    let mut opts = AUTO_FREE_PRESETS.to_vec();
    if !opts.contains(&current) {
        let at = opts.iter().position(|o| o.is_some_and(|d| Some(d) > current)).unwrap_or(opts.len());
        opts.insert(at, current);
    }
    opts
}

pub fn keep_summary(n: usize) -> String {
    match n {
        0 => "None".into(),
        1 => "1 folder".into(),
        n => format!("{n} folders"),
    }
}

/// The page's sentences that name the provider.
pub struct Texts {
    pub space: String,
    pub auto_free: String,
    pub account_group: &'static str,
    pub switch: String,
}

pub fn texts(p: &Choice) -> Texts {
    let cloud = &p.name;
    Texts {
        space: format!("Keep this computer light: uploaded files can be removed here and stay in {cloud}."),
        auto_free: format!(
            "Local copies are removed only after CloudDrop checks\n{cloud} has an identical copy{}",
            if p.verify_by_download { ", by downloading it once" } else { "" }
        ),
        account_group: if p.is_google() { "Google Account" } else { "Account" },
        switch: format!("Switch to another cloud. Everything is uploaded again; nothing is deleted from {cloud}."),
    }
}

/// "Switch to another cloud?": what switching does to the files in `cloud`.
pub fn switch_dialog_text(cloud: &str) -> (&'static str, String) {
    (
        "Switch to another cloud?",
        format!(
            "CloudDrop will upload everything in your CloudDrop folder again, to the cloud you choose next. \
             Nothing is deleted from {cloud}. Files that are only in {cloud} (freed from this computer) stay \
             there, but CloudDrop stops listing them."
        ),
    )
}

/// "Remove this folder?": what removing a connection does, and doesn't.
pub fn remove_dialog_text(folder: &str, cloud: &str) -> (&'static str, String) {
    (
        "Remove this folder from CloudDrop?",
        format!(
            "CloudDrop stops uploading {folder} to {cloud} and forgets what it uploaded from it. \
             Nothing is deleted: the folder keeps its files, and {cloud} keeps everything uploaded so far."
        ),
    )
}

pub struct SettingsPage {
    pub page: adw::PreferencesPage,
    ops: Rc<Ops>,
    space: adw::PreferencesGroup,
    auto_free: adw::ComboRow,
    auto_opts: RefCell<Vec<Option<u32>>>,
    keep: adw::ExpanderRow,
    keep_rows: RefCell<Vec<adw::ActionRow>>,
    keep_add: adw::ActionRow,
    shown_keep: RefCell<Option<Vec<String>>>,
    account_group: adw::PreferencesGroup,
    account: adw::ActionRow,
    account_icon: gtk::Image,
    sign_in: gtk::Button,
    sign_out: gtk::Button,
    switch: adw::ActionRow,
    folder: adw::ActionRow,
    drive: adw::ActionRow,
    open_drive: gtk::Button,
    parallel: adw::SpinRow,
    settle: adw::SpinRow,
    notifications: adw::SwitchRow,
    ignore: adw::EntryRow,
    tray: adw::SwitchRow,
    service: adw::ActionRow,
    /// Suppresses change handlers while the rows are being filled in.
    filling: Cell<bool>,
    filled: Cell<bool>,
    last: RefCell<Option<Snapshot>>,
}

impl SettingsPage {
    pub fn new(ops: Rc<Ops>) -> Rc<Self> {
        let page = adw::PreferencesPage::new();
        let win = ops.win.clone();
        let hooks = ops.hooks().clone();

        let space = adw::PreferencesGroup::builder()
            .title("Space Saving")
            .description("Keep this computer light: uploaded files can be removed here and stay in Google Drive.")
            .build();
        let labels: Vec<String> = AUTO_FREE_PRESETS.iter().map(|d| auto_free_label(*d)).collect();
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let auto_free = adw::ComboRow::builder()
            .title("Free up space automatically")
            .subtitle("Local copies are removed only after CloudDrop checks\nGoogle Drive has an identical copy")
            .model(&gtk::StringList::new(&label_refs))
            .build();
        let keep = adw::ExpanderRow::builder()
            .title("Always keep on this computer")
            .subtitle("None")
            .build();
        keep.add_prefix(&gtk::Image::from_icon_name("view-pin-symbolic"));
        let keep_add = adw::ActionRow::builder().title("Add Folder…").activatable(true).build();
        keep_add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        keep.add_row(&keep_add);
        space.add(&auto_free);
        space.add(&keep);
        page.add(&space);

        let account_group = adw::PreferencesGroup::builder().title("Google Account").build();
        let account = adw::ActionRow::builder().title("Google Drive").build();
        let account_icon = gtk::Image::from_icon_name("avatar-default-symbolic");
        account.add_prefix(&account_icon);
        let sign_in = gtk::Button::builder().label("Sign In Again").valign(gtk::Align::Center).build();
        let sign_out = gtk::Button::builder()
            .label("Sign Out")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        account.add_suffix(&sign_in);
        account.add_suffix(&sign_out);
        account_group.add(&account);
        let switch = adw::ActionRow::builder().title("Upload somewhere else").build();
        switch.add_prefix(&gtk::Image::from_icon_name("view-refresh-symbolic"));
        let switch_btn = gtk::Button::builder().label("Switch Provider…").valign(gtk::Align::Center).build();
        switch.add_suffix(&switch_btn);
        account_group.add(&switch);
        page.add(&account_group);

        let folders = adw::PreferencesGroup::builder().title("Folders").build();
        let folder = adw::ActionRow::builder().title("On this computer").build();
        let open_local = ui::flat_icon("folder-open-symbolic", "Open folder");
        folder.add_suffix(&open_local);
        let drive = adw::ActionRow::builder().title("In Google Drive").build();
        let open_drive = ui::flat_icon("adw-external-link-symbolic", "Open in Google Drive");
        drive.add_suffix(&open_drive);
        folders.add(&folder);
        folders.add(&drive);
        let add_folder = adw::ActionRow::builder()
            .title("Upload another folder")
            .subtitle("Each folder can go to a cloud of its own")
            .build();
        let add_btn = gtk::Button::builder().label("Add Another Folder…").valign(gtk::Align::Center).build();
        add_folder.add_suffix(&add_btn);
        folders.add(&add_folder);
        let remove_folder = adw::ActionRow::builder()
            .title("Stop uploading this folder")
            .subtitle("Nothing is deleted here or in the cloud")
            .build();
        let remove_btn = gtk::Button::builder()
            .label("Remove This Folder…")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        remove_folder.add_suffix(&remove_btn);
        folders.add(&remove_folder);
        page.add(&folders);

        let uploading = adw::PreferencesGroup::builder().title("Uploading").build();
        let parallel = adw::SpinRow::with_range(1.0, 8.0, 1.0);
        parallel.set_title("Files at a time");
        parallel.set_subtitle("More is faster on fast connections");
        let settle = adw::SpinRow::with_range(0.0, 120.0, 1.0);
        settle.set_title("Wait after a change (seconds)");
        settle.set_subtitle("Gives programs time to finish writing a file before it uploads");
        let notifications = adw::SwitchRow::builder()
            .title("Notifications")
            .subtitle("Tell me when uploads finish or something needs attention")
            .build();
        let ignore = adw::EntryRow::builder()
            .title("Ignored files (comma-separated patterns)")
            .show_apply_button(true)
            .build();
        for r in [parallel.upcast_ref::<gtk::Widget>(), settle.upcast_ref(), notifications.upcast_ref(), ignore.upcast_ref()] {
            uploading.add(r);
        }
        page.add(&uploading);

        let startup = adw::PreferencesGroup::builder().title("Background").build();
        let tray = adw::SwitchRow::builder()
            .title("Show icon in the top bar")
            .subtitle("Starts with your session")
            .build();
        let service = adw::ActionRow::builder().title("Upload service").build();
        let restart_btn = gtk::Button::builder().label("Restart").valign(gtk::Align::Center).build();
        service.add_suffix(&restart_btn);
        startup.add(&tray);
        startup.add(&service);
        if control::files_extension_installed() {
            let files = adw::ActionRow::builder()
                .title("Files integration")
                .subtitle("Status icons and right-click actions in Files. Restart Files if they're missing.")
                .build();
            let restart_files = gtk::Button::builder().label("Restart Files").valign(gtk::Align::Center).build();
            restart_files.connect_clicked(|b| {
                let b = b.clone();
                rt::blocking(control::restart_files, move |r| {
                    b.set_label(if r.is_ok() { "Restarted" } else { "Couldn't Restart" });
                });
            });
            files.add_suffix(&restart_files);
            startup.add(&files);
        }
        page.add(&startup);

        let me = Rc::new(SettingsPage {
            page,
            ops,
            space,
            auto_free,
            auto_opts: RefCell::new(AUTO_FREE_PRESETS.to_vec()),
            keep,
            keep_rows: RefCell::default(),
            keep_add: keep_add.clone(),
            shown_keep: RefCell::default(),
            account_group,
            account,
            account_icon,
            sign_in: sign_in.clone(),
            sign_out: sign_out.clone(),
            switch,
            folder,
            drive,
            open_drive: open_drive.clone(),
            parallel,
            settle,
            notifications,
            ignore,
            tray,
            service,
            filling: Cell::new(false),
            filled: Cell::new(false),
            last: RefCell::default(),
        });
        {
            let me2 = Rc::downgrade(&me);
            me.auto_free.connect_selected_notify(move |row| {
                if let Some(me) = me2.upgrade()
                    && !me.filling.get()
                {
                    // Copy the choice out first: saving refreshes this page, which may rebuild the list.
                    let days = me.auto_opts.borrow().get(row.selected() as usize).copied();
                    if let Some(days) = days {
                        me.save(move |c| c.auto_free_days = days);
                    }
                }
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            keep_add.connect_activated(move |_| {
                if let Some(me) = me2.upgrade() {
                    me.choose_keep_folder();
                }
            });
        }

        {
            let hooks = hooks.clone();
            sign_in.connect_clicked(move |_| (hooks.sign_in)());
        }
        {
            let (win, hooks, me2) = (win.clone(), hooks.clone(), Rc::downgrade(&me));
            sign_out.connect_clicked(move |_| {
                let Some(provider) = me2.upgrade().map(|m| m.ops.provider()) else {
                    return;
                };
                let d = adw::AlertDialog::new(
                    Some("Sign out?"),
                    Some(&format!(
                        "Uploads stop until you sign in again. Nothing is removed from {} or this computer.",
                        provider.name
                    )),
                );
                d.add_response("cancel", "Cancel");
                d.add_response("out", "Sign Out");
                d.set_response_appearance("out", adw::ResponseAppearance::Destructive);
                let (hooks, me2) = (hooks.clone(), me2.clone());
                d.connect_response(Some("out"), move |_, _| {
                    if provider.is_google() {
                        // Its engine stops too; the background service starts it again after sign-in.
                        let conn = connections::current();
                        if conn.live_status().is_some()
                            && let Ok(db) = conn.open_db()
                        {
                            let _ = control::request_stop(&db);
                        }
                        if let Err(e) = control::sign_out()
                            && let Some(me) = me2.upgrade()
                        {
                            me.ops.toast(&format!("{e:#}"));
                        }
                        (hooks.refresh)();
                        return;
                    }
                    // The engine knows where each provider keeps its sign-in.
                    let (hooks, me2) = (hooks.clone(), me2.clone());
                    cli::run_all(vec![cli::args(&["logout"])], move |res| {
                        if let Err(e) = res
                            && let Some(me) = me2.upgrade()
                        {
                            me.ops.toast(&format!("{e:#}"));
                        }
                        (hooks.refresh)();
                    });
                });
                d.present(Some(&win));
            });
        }
        {
            let (win, hooks, me2) = (win.clone(), hooks.clone(), Rc::downgrade(&me));
            switch_btn.connect_clicked(move |_| {
                let Some(provider) = me2.upgrade().map(|m| m.ops.provider()) else {
                    return;
                };
                let (heading, body) = switch_dialog_text(&provider.name);
                let d = adw::AlertDialog::new(Some(heading), Some(&body));
                d.add_response("cancel", "Cancel");
                d.add_response("choose", "Choose Cloud…");
                d.set_response_appearance("choose", adw::ResponseAppearance::Suggested);
                let hooks = hooks.clone();
                d.connect_response(Some("choose"), move |_, _| (hooks.setup)(crate::setup::Start::Switch));
                d.present(Some(&win));
            });
        }
        {
            let hooks = hooks.clone();
            add_btn.connect_clicked(move |_| (hooks.setup)(crate::setup::Start::AddFolder));
        }
        {
            let (win, hooks, me2) = (win.clone(), hooks.clone(), Rc::downgrade(&me));
            remove_btn.connect_clicked(move |_| {
                let Some(me) = me2.upgrade() else {
                    return;
                };
                let Some((folder, cloud)) = me
                    .last
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.cfg.as_ref().map(|c| (ui::pretty_path(&c.sync_dir), s.provider.name.clone())))
                else {
                    return;
                };
                let (heading, body) = remove_dialog_text(&folder, &cloud);
                let d = adw::AlertDialog::new(Some(heading), Some(&body));
                d.add_response("cancel", "Cancel");
                d.add_response("remove", "Remove Folder");
                d.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                let (hooks, me2) = (hooks.clone(), me2.clone());
                d.connect_response(Some("remove"), move |_, _| {
                    let conn = connections::current();
                    let (hooks, me2, folder) = (hooks.clone(), me2.clone(), folder.clone());
                    rt::blocking(
                        move || connections::remove(&conn, std::time::Duration::from_secs(10)),
                        move |res| {
                            if let Some(me) = me2.upgrade() {
                                me.ops.toast(&match res {
                                    Ok(()) => format!("{folder} is no longer uploaded; nothing was deleted"),
                                    Err(e) => format!("{e:#}"),
                                });
                            }
                            (hooks.refresh)();
                        },
                    );
                });
                d.present(Some(&win));
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            open_local.connect_clicked(move |_| {
                if let Some(cfg) = me2.upgrade().and_then(|m| m.last.borrow().as_ref().and_then(|s| s.cfg.clone())) {
                    control::open_uri(&ui::file_uri(&cfg.sync_dir));
                }
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            open_drive.connect_clicked(move |_| {
                if let Some(m) = me2.upgrade()
                    && let Some(s) = m.last.borrow().as_ref()
                    && let Some(id) = &s.root_id
                {
                    let provider = s.cfg.as_ref().map(|c| c.provider).unwrap_or_default();
                    if let Some(url) = provider.web_url(Kind::Dir, id) {
                        control::open_uri(&url);
                    }
                }
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            me.parallel.connect_notify_local(Some("value"), move |row, _| {
                if let Some(me) = me2.upgrade() {
                    let v = row.value() as usize;
                    me.save(move |c| c.parallel_uploads = v);
                }
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            me.settle.connect_notify_local(Some("value"), move |row, _| {
                if let Some(me) = me2.upgrade() {
                    let v = row.value() as u64;
                    me.save(move |c| c.settle_secs = v);
                }
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            me.notifications.connect_active_notify(move |row| {
                if let Some(me) = me2.upgrade() {
                    let v = row.is_active();
                    me.save(move |c| c.notifications = v);
                }
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            me.ignore.connect_apply(move |row| {
                if let Some(me) = me2.upgrade() {
                    let list: Vec<String> = row
                        .text()
                        .split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect();
                    me.save(move |c| c.ignore = list);
                }
            });
        }
        {
            let (me2, hooks) = (Rc::downgrade(&me), hooks.clone());
            me.tray.connect_active_notify(move |row| {
                if me2.upgrade().is_some_and(|m| !m.filling.get()) {
                    (hooks.set_tray)(row.is_active());
                }
            });
        }
        {
            let me2 = Rc::downgrade(&me);
            restart_btn.connect_clicked(move |_| {
                if let Some(me) = me2.upgrade() {
                    me.restart_now();
                }
            });
        }
        me
    }

    /// Edits the config file and restarts the service shortly after (edits are batched).
    fn save(self: &Rc<Self>, edit: impl FnOnce(&mut clouddrop::config::Config)) {
        if self.filling.get() {
            return;
        }
        self.ops.edit_config("Settings saved", edit);
    }

    fn restart_now(self: &Rc<Self>) {
        self.ops.restart_now("Upload service restarted");
    }

    fn choose_keep_folder(self: &Rc<Self>) {
        let Some(cfg) = self.ops.cfg() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Choose a Folder to Keep")
            .modal(true)
            .initial_folder(&gio::File::for_path(&cfg.sync_dir))
            .build();
        let me = self.clone();
        dialog.select_folder(Some(&self.ops.win), gio::Cancellable::NONE, move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else {
                return; // cancelled
            };
            let Some(rel) = fsops::rel_in_sync(&cfg.sync_dir, &path) else {
                return me.ops.toast(&format!(
                    "Choose a folder inside {}",
                    ui::pretty_path(&cfg.sync_dir)
                ));
            };
            if fsops::add_keep(&cfg.keep_local, &rel).is_none() {
                return me.ops.toast(&format!("“{}” is already kept on this computer", util::name_of(&rel)));
            }
            me.ops.edit_config(&format!("“{}” will always stay on this computer", util::name_of(&rel)), |c| {
                if let Some(list) = fsops::add_keep(&c.keep_local, &rel) {
                    c.keep_local = list;
                }
            });
        });
    }

    fn remove_keep(self: &Rc<Self>, rel: String) {
        let name = util::name_of(&rel).to_string();
        self.ops.edit_config(&format!("“{name}” can be freed up automatically again"), |c| {
            c.keep_local.retain(|k| k.trim_matches('/') != rel);
        });
    }

    fn show_keep(self: &Rc<Self>, list: &[String]) {
        for r in self.keep_rows.borrow_mut().drain(..) {
            self.keep.remove(&r);
        }
        let mut rows = self.keep_rows.borrow_mut();
        for rel in list {
            let parent = util::parent_of(rel);
            let row = adw::ActionRow::builder()
                .title(util::name_of(rel))
                .subtitle(if parent.is_empty() { "CloudDrop".to_string() } else { parent.to_string() })
                .use_markup(false)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
            let remove = ui::flat_icon("list-remove-symbolic", "Stop keeping this folder");
            let (me, rel) = (Rc::downgrade(self), rel.clone());
            remove.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.remove_keep(rel.clone());
                }
            });
            row.add_suffix(&remove);
            self.keep.add_row(&row);
            rows.push(row);
        }
        // the "Add Folder…" row stays last
        self.keep.remove(&self.keep_add);
        self.keep.add_row(&self.keep_add);
        self.keep.set_subtitle(&keep_summary(list.len()));
    }

    /// The window shows another connection: its settings are filled in afresh.
    pub fn reset(&self) {
        self.filled.set(false);
        self.shown_keep.borrow_mut().take();
    }

    pub fn refresh(self: &Rc<Self>, s: &Snapshot) {
        let p = &s.provider;
        let t = texts(p);
        self.space.set_description(Some(&t.space));
        self.auto_free.set_subtitle(&t.auto_free);
        self.account_group.set_title(t.account_group);
        self.account.set_title(&p.name);
        self.account_icon.set_icon_name(Some(if p.is_google() { "avatar-default-symbolic" } else { p.icon }));
        self.switch.set_subtitle(&t.switch);
        self.drive.set_title(&format!("In {}", p.name));
        self.open_drive.set_tooltip_text(Some(&format!("Open in {}", p.name)));
        self.open_drive.set_visible(p.can_open_web());
        self.account.set_subtitle(if s.signed_in { "Signed in" } else { "Not signed in" });
        self.sign_in.set_label(if s.signed_in { "Sign In Again" } else { "Sign In" });
        self.sign_out.set_visible(s.signed_in);
        self.service.set_subtitle(if s.daemon.is_some() { "Running" } else { "Not running" });
        if let Some(cfg) = &s.cfg {
            self.folder.set_subtitle(&ui::pretty_path(&cfg.sync_dir));
            self.drive.set_subtitle(&format!("{} → {}", p.root_label(), cfg.drive_folder));
            self.sync_space_saving(cfg);
            if !self.filled.get() {
                self.filling.set(true);
                self.parallel.set_value(cfg.parallel_uploads as f64);
                self.settle.set_value(cfg.settle_secs as f64);
                self.notifications.set_active(cfg.notifications);
                self.ignore.set_text(&cfg.ignore.join(", "));
                self.tray.set_active(control::autostart_enabled());
                self.filling.set(false);
                self.filled.set(true);
            }
        }
        *self.last.borrow_mut() = Some(s.clone());
    }

    /// Shows the automatic-freeing choice and the kept folders from the config on disk.
    fn sync_space_saving(self: &Rc<Self>, cfg: &clouddrop::config::Config) {
        let want = auto_free_options(cfg.auto_free_days);
        self.filling.set(true);
        if *self.auto_opts.borrow() != want {
            let labels: Vec<String> = want.iter().map(|d| auto_free_label(*d)).collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            self.auto_free.set_model(Some(&gtk::StringList::new(&refs)));
            *self.auto_opts.borrow_mut() = want.clone();
        }
        let idx = want.iter().position(|d| *d == cfg.auto_free_days).unwrap_or(0) as u32;
        if self.auto_free.selected() != idx {
            self.auto_free.set_selected(idx);
        }
        self.filling.set(false);

        if self.shown_keep.borrow().as_ref() != Some(&cfg.keep_local) {
            *self.shown_keep.borrow_mut() = Some(cfg.keep_local.clone());
            self.show_keep(&cfg.keep_local);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_texts() {
        let g = texts(&Choice::of(clouddrop::remote::Provider::GoogleDrive));
        assert_eq!(g.space, "Keep this computer light: uploaded files can be removed here and stay in Google Drive.");
        assert_eq!(g.auto_free, "Local copies are removed only after CloudDrop checks\nGoogle Drive has an identical copy");
        assert_eq!(g.account_group, "Google Account");
        let dav = crate::providers::fakes().into_iter().find(|c| c.id == "webdav").unwrap();
        let w = texts(&dav);
        assert_eq!(w.auto_free, "Local copies are removed only after CloudDrop checks\nWebDAV has an identical copy, by downloading it once");
        assert_eq!(w.account_group, "Account");
        assert_eq!(w.switch, "Switch to another cloud. Everything is uploaded again; nothing is deleted from WebDAV.");
        let (h, b) = switch_dialog_text("Google Drive");
        assert_eq!(h, "Switch to another cloud?");
        assert_eq!(
            b,
            "CloudDrop will upload everything in your CloudDrop folder again, to the cloud you choose next. \
             Nothing is deleted from Google Drive. Files that are only in Google Drive (freed from this computer) \
             stay there, but CloudDrop stops listing them."
        );
    }

    use super::*;

    #[test]
    fn removing_a_folder_says_nothing_is_deleted() {
        let (h, b) = remove_dialog_text("~/Work", "Nextcloud");
        assert_eq!(h, "Remove this folder from CloudDrop?");
        assert_eq!(
            b,
            "CloudDrop stops uploading ~/Work to Nextcloud and forgets what it uploaded from it. Nothing is \
             deleted: the folder keeps its files, and Nextcloud keeps everything uploaded so far."
        );
    }

    #[test]
    fn auto_free_labels() {
        assert_eq!(auto_free_label(None), "Never");
        assert_eq!(auto_free_label(Some(0)), "Right after uploading");
        assert_eq!(auto_free_label(Some(1)), "After 1 day");
        assert_eq!(auto_free_label(Some(7)), "After 7 days");
        assert_eq!(auto_free_label(Some(90)), "After 90 days");
    }

    #[test]
    fn presets_are_offered_as_they_are() {
        for preset in AUTO_FREE_PRESETS {
            assert_eq!(auto_free_options(preset), AUTO_FREE_PRESETS.to_vec());
        }
    }

    #[test]
    fn custom_values_keep_their_place_in_the_list() {
        assert_eq!(
            auto_free_options(Some(14)),
            vec![None, Some(0), Some(7), Some(14), Some(30), Some(90)]
        );
        assert_eq!(
            auto_free_options(Some(365)),
            vec![None, Some(0), Some(7), Some(30), Some(90), Some(365)]
        );
        assert_eq!(
            auto_free_options(Some(3)),
            vec![None, Some(0), Some(3), Some(7), Some(30), Some(90)]
        );
        assert_eq!(auto_free_label(Some(14)), "After 14 days");
    }

    #[test]
    fn keep_summary_counts_folders() {
        assert_eq!(keep_summary(0), "None");
        assert_eq!(keep_summary(1), "1 folder");
        assert_eq!(keep_summary(3), "3 folders");
    }
}
