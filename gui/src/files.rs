//! Files tab: a browser for everything in CloudDrop, on this computer or only in Drive.

use crate::cells::{ViewCtx, date_factory, name_factory, size_factory, status_factory};
use crate::dbwatch::DbWatch;
use crate::fmt;
use crate::model::{
    self, Caps, Filter, ItemObj, Keep, Plan, Request, Scope, Sort, SortKey, caps_for, item_name,
    plan_update, run_query, sort_items,
};
use crate::ops::Ops;
use crate::snapshot::Snapshot;
use crate::{fsops, rt, ui};
use adw::prelude::*;
use clouddrop::db::{Db, DirItem, Kind, State};
use clouddrop::util::{self, human_bytes};
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

// ---------- the page ----------

struct ViewState {
    dir: String,
    search: String,
    filter: Filter,
    sort: Sort,
}

struct Columns {
    name: gtk::ColumnViewColumn,
    status: gtk::ColumnViewColumn,
    size: gtk::ColumnViewColumn,
    date: gtk::ColumnViewColumn,
}

struct Bar {
    root: gtk::Box,
    label: gtk::Label,
    download: gtk::Button,
    free: gtk::Button,
    retry: gtk::Button,
    keep: gtk::ToggleButton,
    drive: gtk::Button,
    show: gtk::Button,
}

struct Acts {
    open: gio::SimpleAction,
    download: gio::SimpleAction,
    free: gio::SimpleAction,
    drive: gio::SimpleAction,
    show: gio::SimpleAction,
    retry: gio::SimpleAction,
    keep: gio::SimpleAction,
}

type Hub = Rc<RefCell<Weak<FilesPage>>>;
type KeyHandler = Box<dyn Fn(&Rc<FilesPage>)>;

pub struct FilesPage {
    pub root: adw::ToolbarView,
    /// Columns that disappear in a narrow window.
    pub columns: Vec<gtk::ColumnViewColumn>,
    /// The status column, narrower in a narrow window.
    pub status_column: gtk::ColumnViewColumn,
    /// Button contents of the selection bar whose text disappears in a narrow window.
    pub narrow_labels: Vec<adw::ButtonContent>,
    ops: Rc<Ops>,
    back: gtk::Button,
    crumbs: gtk::Box,
    crumbs_scroll: gtk::ScrolledWindow,
    search: gtk::SearchEntry,
    filter: gtk::DropDown,
    summary: gtk::Label,
    retry_all: gtk::Button,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    empty_add: gtk::Button,
    store: gio::ListStore,
    selection: gtk::MultiSelection,
    view: gtk::ColumnView,
    cols: Columns,
    bar: Bar,
    acts: Acts,
    popover: gtk::PopoverMenu,
    ctx: Rc<ViewCtx>,
    state: RefCell<ViewState>,
    /// What the last query returned, before sorting.
    raw: RefCell<Vec<DirItem>>,
    in_flight: Cell<bool>,
    dirty: Cell<bool>,
    stale: Cell<bool>,
    reset_view: Cell<bool>,
    pending_select: RefCell<Option<String>>,
    /// Ignore widget signals while the page sets its own widgets.
    syncing: Cell<bool>,
    crumbs_for: RefCell<Option<(String, String)>>,
    last_busy: Cell<bool>,
    watch: DbWatch,
    /// A connection of our own for listings, so a big query on a worker thread never makes
    /// the once-a-second snapshot on the main thread wait for the shared connection.
    browse_db: RefCell<Option<Arc<Db>>>,
}

fn keep_state(keep: &[String], path: &str) -> Keep {
    if keep.iter().any(|k| k.trim_matches('/') == path) {
        Keep::On
    } else if fsops::covered(keep, path) {
        Keep::Inherited
    } else {
        Keep::Off
    }
}

/// A button with icon and text. Returns the content too, so a narrow window can drop the text.
fn label_button(icon: &str, label: &str, tooltip: &str) -> (gtk::Button, adw::ButtonContent) {
    let content = adw::ButtonContent::builder().icon_name(icon).label(label).build();
    let button = gtk::Button::builder().child(&content).tooltip_text(tooltip).build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    (button, content)
}

impl FilesPage {
    pub fn new(ops: Rc<Ops>) -> Rc<Self> {
        let hub: Hub = Rc::default();
        let ctx = Rc::new(ViewCtx {
            flat: Cell::new(false),
            keep: RefCell::default(),
            now: Cell::new(fmt::now_civil()),
        });

        // ---- list ----
        let store = gio::ListStore::new::<ItemObj>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let view = gtk::ColumnView::builder()
            .model(&selection)
            .enable_rubberband(true)
            .show_row_separators(false)
            .css_classes(["files", "data-table"])
            .build();

        let right_click = {
            let hub = hub.clone();
            move |li: &gtk::ListItem, widget: &gtk::Widget| {
                let click = gtk::GestureClick::builder().button(3).build();
                let (hub, li) = (hub.clone(), li.downgrade());
                click.connect_pressed(move |g, _, x, y| {
                    let (Some(page), Some(li)) = (hub.borrow().upgrade(), li.upgrade()) else {
                        return;
                    };
                    if let Some(w) = g.widget() {
                        page.context_menu(li.position(), &w, x, y);
                    }
                });
                widget.add_controller(click);
            }
        };
        let dummy = || gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal);
        let name = gtk::ColumnViewColumn::builder()
            .title("Name")
            .expand(true)
            .resizable(true)
            .sorter(&dummy())
            .factory(&name_factory(ctx.clone(), right_click.clone()))
            .build();
        let status = gtk::ColumnViewColumn::builder()
            .title("Status")
            .fixed_width(250)
            .sorter(&dummy())
            .factory(&status_factory(right_click.clone()))
            .build();
        let size = gtk::ColumnViewColumn::builder()
            .title("Size")
            .fixed_width(90)
            .sorter(&dummy())
            .factory(&size_factory(right_click.clone()))
            .build();
        let date = gtk::ColumnViewColumn::builder()
            .title("Modified")
            .fixed_width(120)
            .sorter(&dummy())
            .factory(&date_factory(ctx.clone(), right_click))
            .build();
        for c in [&name, &status, &size, &date] {
            view.append_column(c);
        }
        let scroller = gtk::ScrolledWindow::builder().child(&view).vexpand(true).hexpand(true).build();

        let empty_add = ui::pill("Add Files…", true);
        let empty = adw::StatusPage::builder().vexpand(true).child(&empty_add).css_classes(["compact"]).build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));

        // ---- toolbars ----
        let back = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Back")
            .sensitive(false)
            .valign(gtk::Align::Center)
            .build();
        let crumbs = gtk::Box::builder().spacing(2).valign(gtk::Align::Center).build();
        let crumbs_scroll = gtk::ScrolledWindow::builder()
            .child(&crumbs)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build();
        let (add, _) = label_button("list-add-symbolic", "Add Files", "Copy files into this folder");
        add.set_valign(gtk::Align::Center);
        let row1 = gtk::Box::builder().spacing(6).css_classes(["toolbar"]).build();
        row1.append(&back);
        row1.append(&crumbs_scroll);
        row1.append(&add);

        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search all files")
            .hexpand(true)
            .search_delay(250)
            .build();
        let labels: Vec<&str> = Filter::ALL.iter().map(|f| f.label()).collect();
        let filter = gtk::DropDown::from_strings(&labels);
        filter.set_tooltip_text(Some("Show only some files"));
        let row2 = gtk::Box::builder().spacing(6).css_classes(["toolbar"]).build();
        row2.append(&search);
        row2.append(&filter);

        let summary = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dim-label"])
            .build();
        let retry_all = gtk::Button::builder().label("Retry All").css_classes(["flat"]).visible(false).build();
        let row3 = gtk::Box::builder().margin_start(14).margin_end(8).margin_bottom(2).build();
        row3.append(&summary);
        row3.append(&retry_all);

        let top = ui::vbox(0);
        top.append(&row1);
        top.append(&row2);
        top.append(&row3);

        // ---- selection bar ----
        let bar_label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["heading"])
            .build();
        let (download, download_text) = label_button("folder-download-symbolic", "Download", "Download to this computer");
        let (free, free_text) = label_button("clouddrop-free-up-space-symbolic", "Free Up Space", "Remove the local copy; it stays in Google Drive");
        let (retry, retry_text) = label_button("view-refresh-symbolic", "Retry", "Try the upload again");
        let keep = gtk::ToggleButton::builder()
            .icon_name("view-pin-symbolic")
            .tooltip_text("Always keep this folder on this computer")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .visible(false)
            .build();
        keep.update_property(&[gtk::accessible::Property::Label("Always keep on this computer")]);
        let drive = ui::flat_icon("adw-external-link-symbolic", "Open in Google Drive");
        let show = ui::flat_icon("folder-open-symbolic", "Show in Files");
        let clear = ui::flat_icon("window-close-symbolic", "Clear selection (Esc)");
        let bar_root = gtk::Box::builder().spacing(6).css_classes(["toolbar"]).build();
        for w in [bar_label.upcast_ref::<gtk::Widget>(), download.upcast_ref(), free.upcast_ref(), retry.upcast_ref(), keep.upcast_ref(), drive.upcast_ref(), show.upcast_ref(), clear.upcast_ref()] {
            bar_root.append(w);
        }
        let bar = Bar { root: bar_root, label: bar_label, download, free, retry, keep, drive, show };

        let root = adw::ToolbarView::new();
        root.add_top_bar(&top);
        root.set_content(Some(&stack));
        root.add_bottom_bar(&bar.root);
        root.set_reveal_bottom_bars(false);

        // ---- actions and the context menu ----
        let mk = |n: &str| gio::SimpleAction::new(n, None);
        let acts = Acts {
            open: mk("open"),
            download: mk("download"),
            free: mk("free"),
            drive: mk("open-drive"),
            show: mk("show"),
            retry: mk("retry"),
            keep: gio::SimpleAction::new_stateful("keep", None, &false.to_variant()),
        };
        let group = gio::SimpleActionGroup::new();
        for a in [&acts.open, &acts.download, &acts.free, &acts.drive, &acts.show, &acts.retry, &acts.keep] {
            group.add_action(a);
        }
        root.insert_action_group("files", Some(&group));
        let menu = gio::Menu::new();
        let hide_if_disabled = |label: &str, action: &str| {
            let item = gio::MenuItem::new(Some(label), Some(action));
            item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
            item
        };
        let s1 = gio::Menu::new();
        s1.append_item(&hide_if_disabled("Open", "files.open"));
        s1.append_item(&hide_if_disabled("Download", "files.download"));
        s1.append_item(&hide_if_disabled("Free Up Space", "files.free"));
        s1.append_item(&hide_if_disabled("Retry Upload", "files.retry"));
        let s2 = gio::Menu::new();
        s2.append(Some("Open in Google Drive"), Some("files.open-drive"));
        s2.append(Some("Show in Files"), Some("files.show"));
        let s3 = gio::Menu::new();
        s3.append_item(&hide_if_disabled("Always Keep on This Computer", "files.keep"));
        for s in [&s1, &s2, &s3] {
            menu.append_section(None, s);
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(&view);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        {
            let popover = popover.clone();
            view.connect_destroy(move |_| popover.unparent());
        }

        let page = Rc::new(FilesPage {
            root,
            columns: vec![size.clone(), date.clone()],
            status_column: status.clone(),
            narrow_labels: vec![download_text, free_text, retry_text],
            ops,
            back,
            crumbs,
            crumbs_scroll,
            search,
            filter,
            summary,
            retry_all,
            stack,
            empty,
            empty_add,
            store,
            selection,
            view,
            cols: Columns { name, status, size, date },
            bar,
            acts,
            popover,
            ctx,
            state: RefCell::new(ViewState { dir: String::new(), search: String::new(), filter: Filter::All, sort: Sort::default() }),
            raw: RefCell::default(),
            in_flight: Cell::new(false),
            dirty: Cell::new(false),
            stale: Cell::new(true),
            reset_view: Cell::new(true),
            pending_select: RefCell::default(),
            syncing: Cell::new(false),
            crumbs_for: RefCell::default(),
            last_busy: Cell::new(false),
            watch: DbWatch::new(),
            browse_db: RefCell::default(),
        });
        *hub.borrow_mut() = Rc::downgrade(&page);
        page.connect(&add, &clear);
        page.rebuild_crumbs();
        page
    }

    fn connect(self: &Rc<Self>, add: &gtk::Button, clear: &gtk::Button) {
        let weak = Rc::downgrade(self);
        macro_rules! with_page {
            ($body:expr) => {{
                let me = weak.clone();
                move |_: &_| {
                    if let Some(page) = me.upgrade() {
                        let f: &dyn Fn(&Rc<FilesPage>) = &$body;
                        f(&page);
                    }
                }
            }};
        }
        self.back.connect_clicked(with_page!(|p| p.go_back()));
        add.connect_clicked(with_page!(|p| {
            let dir = p.state.borrow().dir.clone();
            p.ops.add_files(dir);
        }));
        self.empty_add.connect_clicked(with_page!(|p| {
            let dir = p.state.borrow().dir.clone();
            p.ops.add_files(dir);
        }));
        clear.connect_clicked(with_page!(|p| {
            p.selection.unselect_all();
        }));
        self.retry_all.connect_clicked(with_page!(|p| {
            let dir = p.state.borrow().dir.clone();
            p.ops.retry(&[dir]);
            p.reload();
        }));

        let me = weak.clone();
        self.search.connect_search_changed(move |entry| {
            if let Some(p) = me.upgrade()
                && !p.syncing.get()
            {
                let text = entry.text().trim().to_string();
                if p.state.borrow().search == text {
                    return; // the page itself cleared it (the signal arrives after a short delay)
                }
                p.state.borrow_mut().search = text;
                p.reset_view.set(true);
                p.reload();
            }
        });
        let me = weak.clone();
        self.filter.connect_selected_notify(move |dd| {
            if let Some(p) = me.upgrade()
                && !p.syncing.get()
            {
                p.state.borrow_mut().filter = Filter::from_index(dd.selected());
                p.reset_view.set(true);
                p.reload();
            }
        });
        let me = weak.clone();
        self.view.connect_activate(move |_, pos| {
            if let Some(p) = me.upgrade() {
                p.activate_row(pos);
            }
        });
        let me = weak.clone();
        self.selection.connect_selection_changed(move |_, _, _| {
            if let Some(p) = me.upgrade() {
                p.update_selection_ui();
            }
        });

        // sorting by clicking column headers
        let me = weak.clone();
        if let Some(sorter) = self.view.sorter() {
            sorter.connect_changed(move |sorter, _| {
                let Some(p) = me.upgrade() else {
                    return;
                };
                let Some(cs) = sorter.downcast_ref::<gtk::ColumnViewSorter>() else {
                    return;
                };
                let Some(col) = cs.primary_sort_column() else {
                    return;
                };
                let key = if col == p.cols.status {
                    SortKey::Status
                } else if col == p.cols.size {
                    SortKey::Size
                } else if col == p.cols.date {
                    SortKey::Date
                } else {
                    SortKey::Name
                };
                let sort = Sort { key, descending: cs.primary_sort_order() == gtk::SortType::Descending };
                if p.state.borrow().sort != sort {
                    p.state.borrow_mut().sort = sort;
                    p.resort();
                }
            });
        }
        self.view.sort_by_column(Some(&self.cols.name), gtk::SortType::Ascending);

        // actions
        macro_rules! act {
            ($action:ident, |$p:ident| $body:expr) => {{
                let me = weak.clone();
                self.acts.$action.connect_activate(move |_, _| {
                    if let Some($p) = me.upgrade() {
                        $body
                    }
                });
            }};
        }
        act!(open, |p| {
            if let Some(pos) = p.single_selected() {
                p.activate_row(pos);
            }
        });
        act!(download, |p| p.download_selected());
        act!(free, |p| p.free_selected());
        act!(drive, |p| p.open_selected_in_drive());
        act!(show, |p| {
            if let Some(it) = p.selected().first() {
                p.ops.show_in_files(&it.entry.path);
            }
        });
        act!(retry, |p| p.retry_selected());
        let me = weak.clone();
        self.acts.keep.connect_activate(move |action, _| {
            if let Some(p) = me.upgrade() {
                let on = action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
                p.set_keep(!on);
            }
        });
        for (button, action) in [
            (self.bar.download.upcast_ref::<gtk::Actionable>(), "download"),
            (self.bar.free.upcast_ref(), "free"),
            (self.bar.retry.upcast_ref(), "retry"),
            (self.bar.keep.upcast_ref(), "keep"),
            (self.bar.drive.upcast_ref(), "open-drive"),
            (self.bar.show.upcast_ref(), "show"),
        ] {
            button.set_action_name(Some(&format!("files.{action}")));
        }

        // keyboard
        let keys = gtk::ShortcutController::new();
        let add_key = |trigger: &str, f: KeyHandler| {
            let me = weak.clone();
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(p) = me.upgrade() {
                    f(&p);
                }
                glib::Propagation::Stop
            });
            if let Some(t) = gtk::ShortcutTrigger::parse_string(trigger) {
                keys.add_shortcut(gtk::Shortcut::new(Some(t), Some(action)));
            }
        };
        add_key("<Alt>Left", Box::new(|p| p.go_back()));
        add_key("BackSpace", Box::new(|p| p.go_back()));
        add_key("Escape", Box::new(|p| {
            p.selection.unselect_all();
        }));
        add_key("Menu", Box::new(|p| p.keyboard_menu()));
        add_key("<Shift>F10", Box::new(|p| p.keyboard_menu()));
        self.view.add_controller(keys);
        let find = gtk::ShortcutController::new();
        find.set_scope(gtk::ShortcutScope::Managed);
        let me = weak.clone();
        find.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>f"),
            Some(gtk::CallbackAction::new(move |_, _| {
                if let Some(p) = me.upgrade() {
                    p.search.grab_focus();
                }
                glib::Propagation::Stop
            })),
        ));
        self.root.add_controller(find);

        // drag and drop from other apps
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let me = weak.clone();
        drop.connect_drop(move |_, value, _, _| {
            let Some(p) = me.upgrade() else {
                return false;
            };
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths = list.files().iter().filter_map(|f| f.path()).collect();
            let dir = p.state.borrow().dir.clone();
            p.ops.copy_in(dir, paths);
            true
        });
        let stack = self.stack.clone();
        drop.connect_current_drop_notify(move |dt| {
            if dt.current_drop().is_some() {
                stack.add_css_class("drop-active");
            } else {
                stack.remove_css_class("drop-active");
            }
        });
        self.stack.add_controller(drop);
    }

    // ---------- navigation ----------

    /// Jumps to a folder (optionally with a filter), clearing any search.
    pub fn show(self: &Rc<Self>, dir: &str, filter: Filter, select: Option<String>) {
        {
            let mut st = self.state.borrow_mut();
            st.dir = dir.to_string();
            st.search.clear();
            st.filter = filter;
        }
        self.syncing.set(true);
        self.search.set_text("");
        self.filter.set_selected(filter.index());
        self.syncing.set(false);
        *self.pending_select.borrow_mut() = select;
        self.reset_view.set(true);
        self.reload();
    }

    fn open_dir(self: &Rc<Self>, dir: &str) {
        self.show(dir, Filter::All, None);
    }

    fn go_back(self: &Rc<Self>) {
        let (dir, search, filter) = {
            let st = self.state.borrow();
            (st.dir.clone(), st.search.clone(), st.filter)
        };
        if !search.is_empty() {
            self.show(&dir, filter, None);
        } else if !dir.is_empty() {
            let parent = util::parent_of(&dir).to_string();
            self.show(&parent, filter, Some(dir));
        } else if filter != Filter::All {
            self.show("", Filter::All, None);
        }
    }

    fn rebuild_crumbs(self: &Rc<Self>) {
        let (dir, search) = {
            let st = self.state.borrow();
            (st.dir.clone(), st.search.clone())
        };
        if self.crumbs_for.borrow().as_ref() == Some(&(dir.clone(), search.clone())) {
            return;
        }
        *self.crumbs_for.borrow_mut() = Some((dir.clone(), search.clone()));
        while let Some(c) = self.crumbs.first_child() {
            self.crumbs.remove(&c);
        }
        let mut segments: Vec<(String, String)> = vec![("CloudDrop".into(), String::new())];
        let mut acc = String::new();
        if search.is_empty() {
            for part in dir.split('/').filter(|p| !p.is_empty()) {
                acc = util::join_rel(&acc, part);
                segments.push((part.to_string(), acc.clone()));
            }
        }
        let n = segments.len();
        let current_is_last = search.is_empty();
        for (i, (label, path)) in segments.into_iter().enumerate() {
            if i > 0 {
                let sep = gtk::Image::builder()
                    .icon_name("go-next-symbolic")
                    .pixel_size(12)
                    .css_classes(["dim-label"])
                    .build();
                self.crumbs.append(&sep);
            }
            if current_is_last && i == n - 1 {
                self.crumbs.append(
                    &gtk::Label::builder()
                        .label(&label)
                        .css_classes(["heading", "crumb"])
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .build(),
                );
            } else {
                let b = gtk::Button::builder().label(&label).css_classes(["flat", "crumb"]).build();
                let me = Rc::downgrade(self);
                b.connect_clicked(move |_| {
                    if let Some(p) = me.upgrade() {
                        let filter = p.state.borrow().filter;
                        p.show(&path, filter, None);
                    }
                });
                self.crumbs.append(&b);
            }
        }
        if !search.is_empty() {
            self.crumbs.append(
                &gtk::Image::builder().icon_name("go-next-symbolic").pixel_size(12).css_classes(["dim-label"]).build(),
            );
            self.crumbs.append(
                &gtk::Label::builder()
                    .label(format!("Search “{search}”"))
                    .css_classes(["heading", "crumb"])
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build(),
            );
        }
        let scroll = self.crumbs_scroll.clone();
        glib::idle_add_local_once(move || {
            let adj = scroll.hadjustment();
            adj.set_value(adj.upper() - adj.page_size());
        });
    }

    // ---------- loading ----------

    fn request(&self) -> Request {
        let st = self.state.borrow();
        let scope = if st.search.is_empty() { Scope::Dir(st.dir.clone()) } else { Scope::Search(st.search.clone()) };
        Request { scope, filter: st.filter }
    }

    fn reload(self: &Rc<Self>) {
        if self.in_flight.get() {
            self.dirty.set(true);
            return;
        }
        let Some(db) = self.browse_db() else {
            return;
        };
        let req = self.request();
        self.in_flight.set(true);
        let me = self.clone();
        rt::blocking(
            move || run_query(&db, &req).map(|l| (req, l)),
            move |res| {
                me.in_flight.set(false);
                match res {
                    Ok((req, listing)) if req == me.request() => me.apply(&req, listing),
                    Ok(_) => me.dirty.set(true),
                    Err(e) => me.ops.toast(&format!("Couldn't read the file list: {e:#}")),
                }
                if me.dirty.replace(false) {
                    me.reload();
                }
            },
        );
    }

    fn browse_db(&self) -> Option<Arc<Db>> {
        self.ops.db()?; // not set up yet: nothing to open
        if self.browse_db.borrow().is_none() {
            *self.browse_db.borrow_mut() = Db::open_default().ok().map(Arc::new);
        }
        self.browse_db.borrow().clone()
    }

    fn resort(&self) {
        let mut items = self.raw.borrow().clone();
        sort_items(&mut items, self.state.borrow().sort);
        self.set_items(items);
    }

    fn apply(self: &Rc<Self>, req: &Request, listing: model::Listing) {
        if matches!(req.scope, Scope::Dir(_)) {
            self.state.borrow_mut().dir = listing.dir.clone();
        }
        self.ctx.now.set(fmt::now_civil());
        let flat = req.is_flat();
        let flat_changed = self.ctx.flat.replace(flat) != flat;
        *self.raw.borrow_mut() = listing.items;
        self.resort();
        if flat_changed {
            self.touch_all();
        }
        self.rebuild_crumbs();
        self.update_chrome(req, &listing.stats, listing.truncated);
        if self.reset_view.replace(false) {
            self.selection.unselect_all();
            if self.store.n_items() > 0 {
                self.view.scroll_to(0, None::<&gtk::ColumnViewColumn>, gtk::ListScrollFlags::NONE, None);
            }
        }
        if let Some(path) = self.pending_select.borrow_mut().take()
            && let Some(pos) = self.position_of(&path)
        {
            self.view.scroll_to(pos, None::<&gtk::ColumnViewColumn>, gtk::ListScrollFlags::SELECT, None);
        }
        self.update_selection_ui();
    }

    /// Updates the rows with the fewest changes, so scroll position and selection survive.
    fn set_items(&self, items: Vec<DirItem>) {
        let n = self.store.n_items();
        let objs: Vec<ItemObj> = (0..n).filter_map(|i| self.store.item(i).and_downcast::<ItemObj>()).collect();
        let old_paths: Vec<String> = objs.iter().map(|o| o.path()).collect();
        let old: Vec<&str> = old_paths.iter().map(String::as_str).collect();
        let new: Vec<&str> = items.iter().map(|i| i.entry.path.as_str()).collect();
        match plan_update(&old, &new) {
            Plan::Replace => {
                let all: Vec<ItemObj> = items.into_iter().map(ItemObj::new).collect();
                self.store.splice(0, n, &all);
            }
            Plan::Edit { removals, inserts } => {
                for (start, len) in removals {
                    self.store.splice(start as u32, len as u32, &[] as &[ItemObj]);
                }
                let by_path: HashMap<&str, &ItemObj> = old.iter().copied().zip(objs.iter()).collect();
                for (at, from, to) in inserts {
                    let added: Vec<ItemObj> = items[from..to].iter().cloned().map(ItemObj::new).collect();
                    self.store.splice(at as u32, 0, &added);
                }
                for item in &items {
                    if let Some(obj) = by_path.get(item.entry.path.as_str()) {
                        obj.update(item.clone());
                    }
                }
            }
        }
    }

    fn touch_all(&self) {
        for i in 0..self.store.n_items() {
            if let Some(o) = self.store.item(i).and_downcast::<ItemObj>() {
                o.touch();
            }
        }
    }

    fn position_of(&self, path: &str) -> Option<u32> {
        (0..self.store.n_items()).find(|i| {
            self.store
                .item(*i)
                .and_downcast::<ItemObj>()
                .is_some_and(|o| o.item().entry.path == path)
        })
    }

    fn update_chrome(&self, req: &Request, stats: &Option<clouddrop::db::FolderStats>, truncated: bool) {
        let n = self.store.n_items() as usize;
        let (dir, search, filter) = {
            let st = self.state.borrow();
            (st.dir.clone(), st.search.clone(), st.filter)
        };
        // back button
        let can_back = !search.is_empty() || !dir.is_empty() || filter != Filter::All;
        self.back.set_sensitive(can_back);
        self.back.set_tooltip_text(Some(if !search.is_empty() {
            "Back to the folder"
        } else if !dir.is_empty() {
            "Back to the parent folder (Alt+←)"
        } else {
            "Back to all files"
        }));
        self.retry_all.set_visible(filter == Filter::Failed && n > 0);

        // summary line
        let summary = if matches!(req.scope, Scope::Search(_)) || filter != Filter::All {
            let total: i64 = self.raw.borrow().iter().map(|i| i.bytes).sum();
            let mut s = if n == 0 { String::new() } else { format!("{} · {}", fmt::items(n), human_bytes(total.max(0) as u64)) };
            if truncated {
                s.push_str(&format!(" · only the first {n} are shown, narrow it down with search"));
            }
            s
        } else if let Some(st) = stats.filter(|s| s.files > 0) {
            let mut s = format!("{} · {}", fmt::files(st.files), human_bytes(st.bytes.max(0) as u64));
            if st.cloud_only > 0 {
                s.push_str(&format!(" · {} only in Google Drive", st.cloud_only));
            }
            s
        } else {
            String::new()
        };
        self.summary.set_label(&summary);

        // empty states
        if n > 0 {
            self.stack.set_visible_child_name("list");
            return;
        }
        let (icon, title, text, add) = if !search.is_empty() {
            ("edit-find-symbolic", "No Results".to_string(), format!("Nothing in CloudDrop is named like “{search}”."), false)
        } else if filter != Filter::All {
            let (t, d) = filter.empty_text();
            ("folder-symbolic", t.to_string(), d.to_string(), false)
        } else if dir.is_empty() {
            (
                "clouddrop-syncing-symbolic",
                "CloudDrop Is Empty".to_string(),
                "Drop files here or click Add Files to start uploading to Google Drive.".to_string(),
                true,
            )
        } else {
            ("clouddrop-syncing-symbolic", "This Folder Is Empty".to_string(), "Drop files here to add them to this folder.".to_string(), true)
        };
        // Where files can be added, the empty folder is drawn as a drop zone.
        if add {
            self.empty.add_css_class("drop-zone");
        } else {
            self.empty.remove_css_class("drop-zone");
        }
        self.empty.set_icon_name(Some(icon));
        self.empty.set_title(&title);
        self.empty.set_description(Some(&text));
        self.empty_add.set_visible(add);
        self.stack.set_visible_child_name("empty");
    }

    /// Called once per second with the latest snapshot.
    pub fn refresh(self: &Rc<Self>, s: &Snapshot, visible: bool) {
        let keep = s.cfg.as_ref().map(|c| c.keep_local.clone()).unwrap_or_default();
        if *self.ctx.keep.borrow() != keep {
            *self.ctx.keep.borrow_mut() = keep;
            self.touch_all();
            self.update_selection_ui();
        }
        if self.watch.changed() {
            self.stale.set(true);
        }
        if visible && self.stale.replace(false) {
            self.reload();
        }
        let busy = self.ops.busy();
        if self.last_busy.replace(busy) != busy {
            self.update_selection_ui();
        }
    }

    // ---------- selection and actions ----------

    fn selected(&self) -> Vec<DirItem> {
        let bits = self.selection.selection();
        (0..bits.size())
            .filter_map(|i| self.store.item(bits.nth(i as u32)).and_downcast::<ItemObj>())
            .map(|o| o.item().clone())
            .collect()
    }

    fn single_selected(&self) -> Option<u32> {
        let bits = self.selection.selection();
        (bits.size() == 1).then(|| bits.nth(0))
    }

    fn caps(&self, items: &[DirItem]) -> Caps {
        let keep = self.ctx.keep.borrow();
        caps_for(items, &|p| keep_state(&keep, p))
    }

    fn update_selection_ui(&self) {
        let items = self.selected();
        let caps = self.caps(&items);
        let idle = !self.ops.busy();
        let a = &self.acts;
        a.open.set_enabled(caps.count == 1);
        a.download.set_enabled(caps.download && idle);
        a.free.set_enabled(caps.free && idle);
        a.drive.set_enabled(caps.open_drive);
        a.show.set_enabled(caps.show);
        a.retry.set_enabled(caps.retry);
        a.keep.set_enabled(matches!(caps.keep, Keep::On | Keep::Off));
        a.keep.set_state(&(caps.keep != Keep::Off && caps.keep != Keep::Unavailable).to_variant());

        let b = &self.bar;
        b.download.set_visible(caps.download);
        b.free.set_visible(caps.free);
        b.retry.set_visible(caps.retry);
        b.keep.set_visible(caps.keep != Keep::Unavailable);
        b.keep.set_tooltip_text(Some(if caps.keep == Keep::Inherited {
            "Kept because a folder above it is always kept"
        } else {
            "Always keep this folder on this computer"
        }));
        b.drive.set_visible(caps.count == 1);
        b.drive.set_sensitive(caps.open_drive);
        b.show.set_visible(caps.show);
        b.label.set_label(&match items.as_slice() {
            [one] => item_name(one).to_string(),
            _ => format!("{} selected", caps.count),
        });
        self.root.set_reveal_bottom_bars(caps.count > 0);
    }

    fn activate_row(self: &Rc<Self>, pos: u32) {
        let Some(item) = self.store.item(pos).and_downcast::<ItemObj>() else {
            return;
        };
        let it = item.item().clone();
        let path = it.entry.path.clone();
        match it.entry.kind {
            Kind::Dir => self.open_dir(&path),
            Kind::File if it.entry.state != State::CloudOnly => self.ops.open_local(&path),
            Kind::File => {
                let me = Rc::downgrade(self);
                let name = item_name(&it).to_string();
                self.ops.toast_with_button(&format!("“{name}” is only in Google Drive"), "Download", move || {
                    if let Some(p) = me.upgrade() {
                        let rel = vec![path.clone()];
                        let me2 = p.clone();
                        p.ops.restore(rel, &format!("“{name}”"), move || me2.reload());
                    }
                });
            }
        }
    }

    fn what(items: &[DirItem]) -> String {
        match items {
            [one] => format!("“{}”", item_name(one)),
            many => fmt::items(many.len()),
        }
    }

    fn download_selected(self: &Rc<Self>) {
        let items = self.selected();
        let rels = items.iter().map(|i| i.entry.path.clone()).collect();
        let me = self.clone();
        self.ops.restore(rels, &Self::what(&items), move || me.reload());
    }

    fn free_selected(self: &Rc<Self>) {
        let items = self.selected();
        let rels: Vec<String> = items.iter().map(|i| i.entry.path.clone()).collect();
        let body = match items.as_slice() {
            [one] if one.entry.kind == Kind::File => format!(
                "This removes the local copy of {}. It stays in Google Drive, and you can download it again at any time.",
                Self::what(&items)
            ),
            _ => format!(
                "This removes the local copies of the uploaded files in {}. They stay in Google Drive, \
                 and you can download them again at any time. Folders are kept.",
                Self::what(&items)
            ),
        };
        let me = self.clone();
        self.ops.confirm_free(rels, "Free up space?", &body, move || me.reload());
    }

    fn retry_selected(self: &Rc<Self>) {
        let rels: Vec<String> = self.selected().iter().map(|i| i.entry.path.clone()).collect();
        self.ops.retry(&rels);
        self.reload();
    }

    fn open_selected_in_drive(&self) {
        if let [one] = self.selected().as_slice()
            && let Some(id) = &one.entry.drive_id
        {
            self.ops.open_in_drive(one.entry.kind, id);
        }
    }

    fn set_keep(self: &Rc<Self>, on: bool) {
        let Some(it) = self.selected().into_iter().next() else {
            return;
        };
        let path = it.entry.path.clone();
        let name = item_name(&it).to_string();
        let msg = if on {
            format!("“{name}” will always stay on this computer")
        } else {
            format!("“{name}” can be freed up automatically again")
        };
        self.ops.edit_config(&msg, |c| {
            if on {
                if let Some(list) = fsops::add_keep(&c.keep_local, &path) {
                    c.keep_local = list;
                }
            } else {
                c.keep_local.retain(|k| k.trim_matches('/') != path);
            }
        });
    }

    // ---------- context menu ----------

    fn context_menu(&self, pos: u32, cell: &gtk::Widget, x: f64, y: f64) {
        if !self.selection.is_selected(pos) {
            self.selection.select_item(pos, true);
        }
        self.update_selection_ui();
        let Some(p) = cell.compute_point(&self.view, &gtk::graphene::Point::new(x as f32, y as f32)) else {
            return;
        };
        self.popover.set_pointing_to(Some(&gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1)));
        self.popover.popup();
    }

    fn keyboard_menu(&self) {
        if self.selection.selection().size() > 0 {
            self.popover.set_pointing_to(Some(&gdk::Rectangle::new(40, 40, 1, 1)));
            self.popover.popup();
        }
    }
}
