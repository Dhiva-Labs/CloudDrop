//! Home tab: one calm summary, two storage cards and the everyday actions.

use crate::fmt;
use crate::model::Filter;
use crate::ops::{Nav, Ops};
use crate::snapshot::{Action, Snapshot, summarize};
use crate::{rt, ui};
use adw::prelude::*;
use clouddrop::control::{self, Quota};
use clouddrop::db::Kind;
use clouddrop::util::{self, human_bytes};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct UploadRow {
    path: String,
    row: adw::ActionRow,
    bar: gtk::ProgressBar,
}

/// One storage card: title, headline, bar and a footnote.
struct Card {
    root: gtk::Box,
    headline: gtk::Label,
    bar: gtk::ProgressBar,
    note: gtk::Label,
}

impl Card {
    fn new(icon: &str, title: &str) -> (Card, gtk::Box) {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .css_classes(["card", "storage-card"])
            .build();
        let head = gtk::Box::builder().spacing(8).build();
        head.append(&gtk::Image::from_icon_name(icon));
        head.append(&gtk::Label::builder().label(title).xalign(0.0).hexpand(true).css_classes(["heading"]).build());
        let headline = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-3", "numeric"])
            .margin_top(6)
            .build();
        let bar = gtk::ProgressBar::builder().valign(gtk::Align::Center).build();
        let note = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
        root.append(&head);
        root.append(&headline);
        root.append(&bar);
        root.append(&note);
        (Card { root, headline, bar, note }, head)
    }

    fn set_fraction(&self, frac: Option<f64>) {
        match frac {
            Some(f) => {
                self.bar.set_visible(true);
                self.bar.set_fraction(f.clamp(0.0, 1.0));
                self.bar.remove_css_class("warning");
                self.bar.remove_css_class("critical");
                if f >= 0.95 {
                    self.bar.add_css_class("critical");
                } else if f >= 0.85 {
                    self.bar.add_css_class("warning");
                }
            }
            None => self.bar.set_visible(false),
        }
    }
}

/// A big square action button.
fn tile(icon: &str, label: &str) -> (gtk::Button, gtk::Image, gtk::Label) {
    let image = gtk::Image::builder().icon_name(icon).pixel_size(28).build();
    let text = gtk::Label::builder().label(label).wrap(true).justify(gtk::Justification::Center).build();
    let inner = ui::vbox(8);
    inner.set_halign(gtk::Align::Center);
    inner.append(&image);
    inner.append(&text);
    let button = gtk::Button::builder().child(&inner).css_classes(["card", "tile"]).hexpand(true).build();
    (button, image, text)
}

pub struct HomePage {
    pub root: gtk::Box,
    /// The two storage cards; stacked when the window is narrow.
    pub cards: gtk::Box,
    ops: Rc<Ops>,
    banner: adw::Banner,
    icon: gtk::Image,
    title: gtk::Label,
    detail: gtk::Label,
    action_btn: gtk::Button,
    action: Cell<Option<Action>>,
    uploads: adw::PreferencesGroup,
    upload_rows: RefCell<Vec<UploadRow>>,
    add_tile: gtk::Button,
    free_tile: gtk::Button,
    pause_tile: gtk::Button,
    pause_icon: gtk::Image,
    pause_label: gtk::Label,
    drive: Card,
    computer: Card,
    last: RefCell<Option<Snapshot>>,
}

impl HomePage {
    pub fn new(ops: Rc<Ops>) -> Rc<Self> {
        let banner = adw::Banner::builder().button_label("Review").build();

        let content = ui::vbox(20);
        // Hero
        let hero = ui::vbox(6);
        let icon = gtk::Image::builder().pixel_size(64).margin_bottom(6).build();
        let title = ui::label("", &["title-1"]);
        let detail = ui::label("", &["dim-label"]);
        let action_btn = ui::pill("", true);
        action_btn.set_visible(false);
        action_btn.set_margin_top(10);
        hero.append(&icon);
        hero.append(&title);
        hero.append(&detail);
        hero.append(&action_btn);
        content.append(&hero);

        let uploads = adw::PreferencesGroup::builder().title("Uploading Now").visible(false).build();
        content.append(&uploads);

        // Quick actions
        let tiles = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(2)
            .max_children_per_line(4)
            .row_spacing(12)
            .column_spacing(12)
            .activate_on_single_click(false)
            .build();
        let (open_tile, _, _) = tile("folder-open-symbolic", "Open Folder");
        open_tile.set_tooltip_text(Some("Open the CloudDrop folder in Files"));
        let (add_tile, _, _) = tile("list-add-symbolic", "Add Files…");
        add_tile.set_tooltip_text(Some("Copy files into CloudDrop to upload them"));
        let (free_tile, _, _) = tile("drive-harddisk-symbolic", "Free Up Space…");
        free_tile.set_tooltip_text(Some("Remove local copies of uploaded files; they stay in Google Drive"));
        let (pause_tile, pause_icon, pause_label) = tile("media-playback-pause-symbolic", "Pause Uploads");
        for t in [&open_tile, &add_tile, &free_tile, &pause_tile] {
            let child = gtk::FlowBoxChild::builder().focusable(false).child(t).build();
            tiles.append(&child);
        }
        content.append(&tiles);

        // Storage
        let (drive, drive_head) = Card::new("weather-overcast-symbolic", "Google Drive");
        let open_drive = ui::flat_icon("adw-external-link-symbolic", "Open in Google Drive");
        drive_head.append(&open_drive);
        let (computer, computer_head) = Card::new("computer-symbolic", "This computer");
        let open_local = ui::flat_icon("folder-open-symbolic", "Open folder");
        computer_head.append(&open_local);
        let cards = gtk::Box::builder().spacing(12).homogeneous(true).build();
        cards.append(&drive.root);
        cards.append(&computer.root);
        content.append(&cards);

        let root = ui::vbox(0);
        root.append(&banner);
        root.append(&ui::body_wide(&content, 720));

        let me = Rc::new(HomePage {
            root,
            cards,
            ops: ops.clone(),
            banner,
            icon,
            title,
            detail,
            action_btn,
            action: Cell::new(None),
            uploads,
            upload_rows: RefCell::default(),
            add_tile,
            free_tile,
            pause_tile,
            pause_icon,
            pause_label,
            drive,
            computer,
            last: RefCell::default(),
        });
        me.connect(&open_tile, &open_drive, &open_local);
        me
    }

    fn connect(self: &Rc<Self>, open_tile: &gtk::Button, open_drive: &gtk::Button, open_local: &gtk::Button) {
        {
            let ops = self.ops.clone();
            open_tile.connect_clicked(move |_| ops.open_folder());
            let ops = self.ops.clone();
            open_local.connect_clicked(move |_| ops.open_folder());
        }
        {
            let me = Rc::downgrade(self);
            open_drive.connect_clicked(move |_| {
                if let Some(me) = me.upgrade()
                    && let Some(id) = me.last.borrow().as_ref().and_then(|s| s.root_id.clone())
                {
                    me.ops.open_in_drive(Kind::Dir, &id);
                }
            });
        }
        {
            let ops = self.ops.clone();
            self.add_tile.connect_clicked(move |_| ops.add_files(String::new()));
        }
        {
            let me = Rc::downgrade(self);
            self.free_tile.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.confirm_free();
                }
            });
        }
        {
            let ops = self.ops.clone();
            self.pause_tile.connect_clicked(move |_| ops.set_paused(!ops.paused()));
        }
        {
            let ops = self.ops.clone();
            self.banner.connect_button_clicked(move |_| {
                ops.navigate(Nav::Files { dir: String::new(), filter: Filter::Failed, select: None });
            });
        }
        {
            let me = Rc::downgrade(self);
            self.action_btn.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.run_action();
                }
            });
        }
    }

    fn run_action(self: &Rc<Self>) {
        let hooks = self.ops.hooks().clone();
        match self.action.get() {
            Some(Action::Setup) => (hooks.setup)(),
            Some(Action::SignIn) => (hooks.sign_in)(),
            Some(Action::Resume) => self.ops.set_paused(false),
            Some(Action::StartService) => {
                let ops = self.ops.clone();
                rt::blocking(control::enable_service, move |res| {
                    if let Err(e) = res {
                        ops.toast(&format!("{e:#}"));
                    }
                    ops.refresh();
                });
            }
            None => {}
        }
    }

    fn confirm_free(self: &Rc<Self>) {
        let Some(synced) = self.last.borrow().as_ref().map(|s| s.counts.synced) else {
            return;
        };
        self.ops.confirm_free(
            vec![String::new()],
            "Free up space?",
            &format!(
                "This removes the local copies of all {synced} uploaded files. They stay in \
                 Google Drive, and you can download them again from Files at any time. \
                 Folders are kept.",
            ),
            || {},
        );
    }

    pub fn update(&self, s: &Snapshot) {
        let sum = summarize(s);
        self.icon.set_icon_name(Some(sum.icon));
        self.title.set_label(&sum.title);
        self.detail.set_label(&sum.detail);
        self.action.set(sum.action);
        match sum.action {
            Some(a) => {
                self.action_btn.set_label(match a {
                    Action::Setup => "Set Up CloudDrop",
                    Action::SignIn => "Sign In",
                    Action::StartService => "Start CloudDrop",
                    Action::Resume => "Resume Uploads",
                });
                self.action_btn.set_visible(true);
            }
            None => self.action_btn.set_visible(false),
        }

        let failed = s.counts.failed;
        self.banner.set_title(&format!("{} couldn't be uploaded", fmt::files(failed)));
        self.banner.set_revealed(failed > 0);

        self.refresh_uploads(s);

        self.add_tile.set_sensitive(s.ready());
        self.free_tile.set_sensitive(s.counts.synced > 0 && !self.ops.busy());
        self.pause_tile.set_sensitive(s.ready());
        let (icon, label) = if s.paused {
            ("media-playback-start-symbolic", "Resume Uploads")
        } else {
            ("media-playback-pause-symbolic", "Pause Uploads")
        };
        self.pause_icon.set_icon_name(Some(icon));
        self.pause_label.set_label(label);

        self.update_drive(s);
        self.update_computer(s);
        *self.last.borrow_mut() = Some(s.clone());
    }

    fn update_drive(&self, s: &Snapshot) {
        let c = &self.drive;
        let holds = format!("CloudDrop holds {}", human_bytes(s.cloud_bytes));
        match s.quota() {
            Some(Quota { limit: Some(limit), usage, .. }) if limit > 0 => {
                c.headline.set_label(&format!("{} of {} used", human_bytes(usage), human_bytes(limit)));
                c.set_fraction(Some(usage as f64 / limit as f64));
                c.note.set_label(&holds);
            }
            Some(Quota { usage, .. }) => {
                c.headline.set_label(&format!("{} used", human_bytes(usage)));
                c.set_fraction(None);
                c.note.set_label(&format!("{holds} · unlimited storage"));
            }
            None => {
                c.headline.set_label(if s.daemon.is_some() { "Checking…" } else { "Not connected" });
                c.set_fraction(None);
                c.note.set_label(&holds);
            }
        }
        c.headline.set_css_classes(if s.quota().is_some() {
            &["title-3", "numeric"]
        } else {
            &["title-3", "dim-label"]
        });
    }

    fn update_computer(&self, s: &Snapshot) {
        let c = &self.computer;
        let total = s.root.bytes.max(0) as u64;
        let freed = s.root.cloud_only_bytes.max(0) as u64;
        let stored = total.saturating_sub(freed);
        if s.root.files == 0 {
            c.headline.set_label("Nothing yet");
            c.set_fraction(None);
            c.note.set_label("Files you add to CloudDrop show up here");
            return;
        }
        c.headline.set_label(&if freed == 0 { "Nothing freed yet".to_string() } else { format!("{} freed", human_bytes(freed)) });
        c.set_fraction(Some(if total == 0 { 0.0 } else { freed as f64 / total as f64 }));
        c.note.set_label(&format!(
            "{} still stored here · {} on this computer",
            human_bytes(stored),
            fmt::files(s.root.local)
        ));
    }

    fn refresh_uploads(&self, s: &Snapshot) {
        let uploads = s.daemon.as_ref().map(|d| d.uploads.clone()).unwrap_or_default();
        let mut rows = self.upload_rows.borrow_mut();
        let same = rows.len() == uploads.len() && rows.iter().zip(&uploads).all(|(r, u)| r.path == u.path);
        if !same {
            for r in rows.drain(..) {
                self.uploads.remove(&r.row);
            }
            for u in &uploads {
                let parent = util::parent_of(&u.path);
                let row = adw::ActionRow::builder()
                    .title(util::name_of(&u.path))
                    .subtitle(if parent.is_empty() { "CloudDrop".to_string() } else { parent.to_string() })
                    .use_markup(false)
                    .build();
                let bar = gtk::ProgressBar::builder()
                    .valign(gtk::Align::Center)
                    .width_request(150)
                    .show_text(true)
                    .build();
                row.add_suffix(&bar);
                self.uploads.add(&row);
                rows.push(UploadRow { path: u.path.clone(), row, bar });
            }
        }
        for (r, u) in rows.iter().zip(&uploads) {
            let frac = if u.total > 0 { u.sent as f64 / u.total as f64 } else { 0.0 };
            r.bar.set_fraction(frac);
            r.bar.set_text(Some(&format!("{} / {}", human_bytes(u.sent), human_bytes(u.total))));
        }
        self.uploads.set_visible(!uploads.is_empty());
    }
}
