//! Main window: Home, Files, Activity and Settings.

use crate::activity::ActivityPage;
use crate::files::FilesPage;
use crate::home::HomePage;
use crate::ops::{Nav, Ops};
use crate::settings::SettingsPage;
use crate::snapshot::{Snapshot, summarize};
use adw::prelude::*;
use clouddrop::db::Db;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// What the window asks the application to do.
pub struct Hooks {
    pub sign_in: Box<dyn Fn()>,
    pub setup: Box<dyn Fn()>,
    pub set_tray: Box<dyn Fn(bool)>,
    pub refresh: Box<dyn Fn()>,
    /// The shared state database (opened on first use).
    pub db: Box<dyn Fn() -> Option<Arc<Db>>>,
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
    last: RefCell<Option<Snapshot>>,
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
        let ops = Ops::new(&win, &toasts, hooks);

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
        let menu = gtk::gio::Menu::new();
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
            last: RefCell::default(),
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
                if let Some(s) = last {
                    w.refresh(&s);
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

    /// Updates everything from a fresh snapshot (called every second).
    pub fn refresh(&self, s: &Snapshot) {
        self.ops.set_snapshot(s);
        let sum = summarize(s);
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
    }
}
