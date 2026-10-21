//! Main window: Home, Files, Activity and Settings.

use crate::activity::ActivityPage;
use crate::files::FilesPage;
use crate::home::HomePage;
use crate::ops::{Nav, Ops};
use crate::settings::SettingsPage;
use crate::snapshot::{Snapshot, summarize};
use crate::ui;
use adw::prelude::*;
use clouddrop::db::Db;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// What the window asks the application to do.
pub struct Hooks {
    pub sign_in: Box<dyn Fn()>,
    /// Opens the setup assistant: from the start, at its sign-in step, to switch providers, or
    /// for another folder (a new connection).
    pub setup: Box<dyn Fn(crate::setup::Start)>,
    pub set_tray: Box<dyn Fn(bool)>,
    pub refresh: Box<dyn Fn()>,
    /// The shared state database (opened on first use).
    pub db: Box<dyn Fn() -> Option<Arc<Db>>>,
    /// Shows another connection (by name).
    pub switch: Box<dyn Fn(String)>,
}

/// One connection in the switcher.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnRow {
    pub name: String,
    pub label: String,
    pub provider: String,
    /// Its status icon and line ("Up to date", "Paused").
    pub icon: &'static str,
    pub line: String,
}

/// The switcher's sentence under a connection's name.
pub fn conn_subtitle(r: &ConnRow) -> String {
    format!("{} · {}", r.provider, r.line)
}

/// The header's connection switcher: the shown connection's status and name; its popover lists
/// every connection with its status, and "Add Another Folder…".
struct Switcher {
    button: gtk::MenuButton,
    icon: gtk::Image,
    label: gtk::Label,
    popover: gtk::Popover,
    list: gtk::ListBox,
    /// The rows shown, as (name, label, provider), and each row's status icon, subtitle and check.
    shown: RefCell<Vec<(String, String, String)>>,
    rows: RefCell<Vec<(gtk::Image, gtk::Label, gtk::Image)>>,
}

impl Switcher {
    fn new(hooks: &Rc<Hooks>) -> Switcher {
        let icon = gtk::Image::from_icon_name("clouddrop-idle-symbolic");
        let label = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(20).build();
        let child = gtk::Box::builder().spacing(6).build();
        child.append(&icon);
        child.append(&label);
        child.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["navigation-sidebar"]).build();
        let add = gtk::Button::builder().css_classes(["flat"]).build();
        add.set_child(Some(&adw::ButtonContent::builder().icon_name("list-add-symbolic").label("Add Another Folder…").build()));
        let content = ui::vbox(6);
        content.append(&list);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&add);
        let popover = gtk::Popover::builder().child(&content).build();
        let button = gtk::MenuButton::builder()
            .child(&child)
            .popover(&popover)
            .css_classes(["flat"])
            .tooltip_text("Choose a Folder")
            .visible(false)
            .build();
        {
            let (hooks, popover) = (hooks.clone(), popover.clone());
            add.connect_clicked(move |_| {
                popover.popdown();
                (hooks.setup)(crate::setup::Start::AddFolder);
            });
        }
        Switcher { button, icon, label, popover, list, shown: RefCell::default(), rows: RefCell::default() }
    }

    fn update(&self, hooks: &Rc<Hooks>, current: &str, icon: &str, conns: &[ConnRow]) {
        self.button.set_visible(conns.len() > 1);
        if conns.len() <= 1 {
            return;
        }
        self.icon.set_icon_name(Some(icon));
        let label = conns.iter().find(|c| c.name == current).map(|c| c.label.clone()).unwrap_or_default();
        self.label.set_label(&label);
        self.button.update_property(&[gtk::accessible::Property::Label(&format!("Folder: {label}"))]);
        let key: Vec<(String, String, String)> =
            conns.iter().map(|c| (c.name.clone(), c.label.clone(), c.provider.clone())).collect();
        if *self.shown.borrow() != key {
            while let Some(row) = self.list.first_child() {
                self.list.remove(&row);
            }
            let mut rows = Vec::new();
            for c in conns {
                let status = gtk::Image::from_icon_name(c.icon);
                let text = ui::vbox(2);
                text.set_hexpand(true);
                text.append(&ui::start_label(&c.label, &["heading"]));
                let sub = ui::start_label(&conn_subtitle(c), &["dim-label", "caption"]);
                text.append(&sub);
                let check = gtk::Image::from_icon_name("object-select-symbolic");
                let row_box = gtk::Box::builder().spacing(10).css_classes(["connection-row"]).build();
                row_box.append(&status);
                row_box.append(&text);
                row_box.append(&check);
                let row = gtk::ListBoxRow::builder().child(&row_box).activatable(true).build();
                row.update_property(&[gtk::accessible::Property::Label(&c.label)]);
                let (hooks, popover, name) = (hooks.clone(), self.popover.clone(), c.name.clone());
                let gesture = gtk::GestureClick::new();
                gesture.connect_released(move |_, _, _, _| {
                    popover.popdown();
                    (hooks.switch)(name.clone());
                });
                row.add_controller(gesture);
                self.list.append(&row);
                rows.push((status, sub, check));
            }
            *self.rows.borrow_mut() = rows;
            *self.shown.borrow_mut() = key;
        }
        for ((status, sub, check), c) in self.rows.borrow().iter().zip(conns) {
            status.set_icon_name(Some(c.icon));
            status.set_tooltip_text(Some(&c.line));
            sub.set_label(&conn_subtitle(c));
            check.set_opacity(if c.name == current { 1.0 } else { 0.0 });
        }
    }
}

pub struct MainWindow {
    pub win: adw::ApplicationWindow,
    ops: Rc<Ops>,
    stack: adw::ViewStack,
    home: Rc<HomePage>,
    files: Rc<FilesPage>,
    activity: Rc<ActivityPage>,
    settings: Rc<SettingsPage>,
    /// The status at a glance, at the start of the header bar (opens Home).
    status: gtk::Button,
    /// With several connections, in place of `status`.
    switcher: Switcher,
    hooks: Rc<Hooks>,
    last: RefCell<Option<Snapshot>>,
    last_conns: RefCell<Vec<ConnRow>>,
}

impl MainWindow {
    pub fn new(app: &adw::Application, hooks: Rc<Hooks>) -> Rc<Self> {
        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("CloudDrop")
            .default_width(900)
            .default_height(740)
            .build();
        let toasts = adw::ToastOverlay::new();
        let ops = Ops::new(&win, &toasts, hooks.clone());

        let home = HomePage::new(ops.clone());
        let files = FilesPage::new(ops.clone());
        let activity = ActivityPage::new(ops.clone());
        let settings = SettingsPage::new(ops.clone());

        let stack = adw::ViewStack::new();
        stack.add_titled_with_icon(&home.root, Some("home"), "Home", "go-home-symbolic");
        stack.add_titled_with_icon(&files.root, Some("files"), "Files", "folder-symbolic");
        stack.add_titled_with_icon(&activity.root, Some("activity"), "Activity", "document-open-recent-symbolic");
        stack.add_titled_with_icon(&settings.page, Some("settings"), "Settings", "emblem-system-symbolic");

        let header = adw::HeaderBar::new();
        let status = gtk::Button::builder()
            .icon_name("clouddrop-idle-symbolic")
            .css_classes(["flat"])
            .build();
        header.pack_start(&status);
        let conn_switcher = Switcher::new(&hooks);
        header.pack_start(&conn_switcher.button);
        let menu = gtk::gio::Menu::new();
        menu.append(Some("Add Another Folder…"), Some("app.add-folder"));
        menu.append(Some("About CloudDrop"), Some("app.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        header.pack_end(&menu_button);
        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
        header.set_title_widget(Some(&switcher));
        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
        toasts.set_child(Some(&stack));
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&toasts));
        tv.add_bottom_bar(&switcher_bar);
        win.set_content(Some(&tv));

        // Narrow window: tabs move to the bottom, cards stack, the table drops columns.
        if let Ok(cond) = adw::BreakpointCondition::parse("max-width: 560sp") {
            let bp = adw::Breakpoint::new(cond);
            bp.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
            bp.add_setter(&header, "title-widget", Some(&None::<gtk::Widget>.to_value()));
            bp.add_setter(&home.cards, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
            bp.add_setter(&home.tiles, "max-children-per-line", Some(&2_u32.to_value()));
            for col in &files.columns {
                bp.add_setter(col, "visible", Some(&false.to_value()));
            }
            bp.add_setter(&files.status_column, "fixed-width", Some(&190_i32.to_value()));
            for content in &files.narrow_labels {
                bp.add_setter(content, "label", Some(&"".to_value()));
            }
            win.add_breakpoint(bp);
        }

        let w = Rc::new(MainWindow {
            win,
            ops: ops.clone(),
            stack,
            home,
            files,
            activity,
            settings,
            status,
            switcher: conn_switcher,
            hooks,
            last: RefCell::default(),
            last_conns: RefCell::default(),
        });
        let weak = Rc::downgrade(&w);
        w.status.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.stack.set_visible_child_name("home");
            }
        });
        let weak = Rc::downgrade(&w);
        ops.set_nav(move |nav| {
            if let Some(w) = weak.upgrade() {
                w.navigate(nav);
            }
        });
        let weak = Rc::downgrade(&w);
        w.stack.connect_visible_child_name_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                let last = w.last.borrow().clone();
                let conns = w.last_conns.borrow().clone();
                if let Some(s) = last {
                    w.refresh(&s, &conns);
                }
            }
        });
        w
    }

    pub fn present(&self) {
        self.win.present();
    }

    pub fn toast(&self, text: &str) {
        self.ops.toast(text);
    }

    fn navigate(&self, nav: Nav) {
        match nav {
            Nav::Files { dir, filter, select } => {
                self.stack.set_visible_child_name("files");
                self.files.show(&dir, filter, select);
            }
            Nav::Activity => self.stack.set_visible_child_name("activity"),
        }
    }

    /// Updates everything from a fresh snapshot of the connection shown, and every
    /// connection's status for the switcher (called every second).
    pub fn refresh(&self, s: &Snapshot, conns: &[ConnRow]) {
        let switched = self.last.borrow().as_ref().is_some_and(|l| l.connection != s.connection);
        if switched {
            // Another connection: its own folders, history and settings.
            self.files.reset();
            self.activity.reset();
            self.settings.reset();
            self.stack.set_visible_child_name("home");
        }
        self.ops.set_snapshot(s);
        let sum = summarize(s);
        let several = conns.len() > 1;
        self.status.set_visible(!several);
        self.switcher.update(&self.hooks, &s.connection, sum.icon, conns);
        self.win.set_title(Some(&if several { format!("{} — CloudDrop", s.label) } else { "CloudDrop".to_string() }));
        self.status.set_icon_name(sum.icon);
        self.status.set_tooltip_text(Some(&sum.line));
        self.status.update_property(&[gtk::accessible::Property::Label(&format!("Status: {}", sum.line))]);
        let page = self.stack.visible_child_name();
        let page = page.as_deref().unwrap_or("home");
        self.home.update(s, page == "home");
        self.settings.refresh(s);
        self.files.refresh(s, page == "files");
        self.activity.refresh(page == "activity");
        *self.last.borrow_mut() = Some(s.clone());
        *self.last_conns.borrow_mut() = conns.to_vec();
    }
}
