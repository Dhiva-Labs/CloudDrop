//! Activity tab: what CloudDrop did, newest first, grouped by day.

use crate::fmt::{self, Civil};
use crate::model::Filter;
use crate::ops::{Nav, Ops};
use crate::ui;
use adw::prelude::*;
use clouddrop::db::{Activity, ActivityKind};
use clouddrop::util::{self, human_bytes};
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const LOAD: usize = 500;
const PAGE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub icon: &'static str,
    /// Colour class for the icon: ok, info, warn.
    pub tone: &'static str,
    pub title: String,
    /// Extra line: where a file moved from, or why it failed.
    pub detail: Option<String>,
}

pub fn describe(a: &Activity) -> Described {
    let name = util::name_of(&a.path);
    let mk = |icon, tone, title: String, detail| Described { icon, tone, title, detail };
    match a.kind {
        ActivityKind::Uploaded => mk("go-up-symbolic", "ok", format!("Uploaded {name}"), None),
        ActivityKind::Updated => mk("view-refresh-symbolic", "ok", format!("Updated {name}"), None),
        ActivityKind::Moved => {
            let old = a.detail.as_deref().unwrap_or_default();
            let same_folder = util::parent_of(old) == util::parent_of(&a.path);
            let title = if same_folder { format!("Renamed {name}") } else { format!("Moved {name}") };
            let from = if same_folder { util::name_of(old) } else { old };
            mk("mail-forward-symbolic", "info", title, (!from.is_empty()).then(|| format!("from {from}")))
        }
        ActivityKind::KeptInCloud => mk(
            "weather-overcast-symbolic",
            "info",
            format!("Kept {name} in Google Drive"),
            Some("Removed from this computer".into()),
        ),
        ActivityKind::Freed => mk("drive-harddisk-symbolic", "info", format!("Freed up {name}"), None),
        ActivityKind::AutoFreed => {
            mk("drive-harddisk-symbolic", "info", format!("Automatically freed up {name}"), None)
        }
        ActivityKind::Restored => mk("folder-download-symbolic", "ok", format!("Downloaded {name}"), None),
        ActivityKind::Failed => mk(
            "dialog-warning-symbolic",
            "warn",
            format!("Couldn't upload {name}"),
            a.detail.as_deref().map(|d| d.lines().next().unwrap_or_default().to_string()),
        ),
    }
}

/// "Photos · from Trips/a.jpg · 1.2 MB · 12:09"
pub fn subtitle(a: &Activity, time: Civil) -> String {
    let d = describe(a);
    let parent = util::parent_of(&a.path);
    let mut parts = vec![if parent.is_empty() { "CloudDrop".to_string() } else { parent.to_string() }];
    if let Some(detail) = d.detail {
        parts.push(detail);
    }
    if let Some(b) = a.bytes.filter(|b| *b > 0 && a.kind != ActivityKind::Failed) {
        parts.push(human_bytes(b as u64));
    }
    parts.push(time.clock());
    parts.join(" · ")
}

pub struct ActivityPage {
    pub root: gtk::Stack,
    ops: Rc<Ops>,
    scroller: gtk::ScrolledWindow,
    content: gtk::Box,
    items: RefCell<Vec<Activity>>,
    shown: Cell<usize>,
    last_id: Cell<Option<i64>>,
    day: Cell<i64>,
}

impl ActivityPage {
    pub fn new(ops: Rc<Ops>) -> Rc<Self> {
        let content = ui::vbox(18);
        let scroller = ui::body_wide(&content, 720);
        let empty = adw::StatusPage::builder()
            .icon_name("document-open-recent-symbolic")
            .title("No Activity Yet")
            .description("Uploads, downloads and other changes will be listed here.")
            .css_classes(["compact"])
            .vexpand(true)
            .build();
        let root = gtk::Stack::new();
        root.add_named(&scroller, Some("list"));
        root.add_named(&empty, Some("empty"));
        Rc::new(ActivityPage {
            root,
            ops,
            scroller,
            content,
            items: RefCell::default(),
            shown: Cell::new(PAGE),
            last_id: Cell::new(None),
            day: Cell::new(0),
        })
    }

    /// Called every second; only reads the database when something new was logged.
    pub fn refresh(self: &Rc<Self>, visible: bool) {
        if !visible {
            return;
        }
        let Some(db) = self.ops.db() else {
            return;
        };
        let id = db.last_activity_id().unwrap_or(0);
        let today = fmt::now_civil().days();
        if self.last_id.get() == Some(id) && self.day.get() == today {
            return;
        }
        let first = self.last_id.get().is_none();
        self.last_id.set(Some(id));
        self.day.set(today);
        *self.items.borrow_mut() = db.recent_activity(LOAD).unwrap_or_default();
        if first {
            self.shown.set(PAGE);
        }
        self.rebuild();
    }

    fn rebuild(self: &Rc<Self>) {
        let adj = self.scroller.vadjustment();
        let scroll = adj.value();
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        let items = self.items.borrow();
        if items.is_empty() {
            self.root.set_visible_child_name("empty");
            return;
        }
        self.root.set_visible_child_name("list");
        let shown = &items[..items.len().min(self.shown.get())];
        let today = fmt::now_civil();
        for (heading, group) in fmt::group_by_day(shown, |a| a.at_ms, fmt::civil_from_ms, today) {
            let g = adw::PreferencesGroup::builder().title(ui::esc(&heading)).build();
            for a in group {
                g.add(&self.row(a));
            }
            self.content.append(&g);
        }
        if shown.len() < items.len() {
            let more = gtk::Button::builder()
                .label("Show Older Activity")
                .halign(gtk::Align::Center)
                .css_classes(["pill"])
                .build();
            let me = Rc::downgrade(self);
            more.connect_clicked(move |_| {
                if let Some(p) = me.upgrade() {
                    p.shown.set(p.shown.get() + PAGE);
                    p.rebuild();
                }
            });
            self.content.append(&more);
        }
        glib::idle_add_local_once(move || adj.set_value(scroll));
    }

    fn row(&self, a: &Activity) -> adw::ActionRow {
        let d = describe(a);
        let row = adw::ActionRow::builder()
            .title(&d.title)
            .subtitle(subtitle(a, fmt::civil_from_ms(a.at_ms)))
            .subtitle_lines(2)
            .use_markup(false)
            .activatable(true)
            .build();
        row.add_prefix(
            &gtk::Image::builder()
                .icon_name(d.icon)
                .pixel_size(16)
                .valign(gtk::Align::Center)
                .css_classes(["activity-icon", d.tone])
                .build(),
        );
        row.add_suffix(&gtk::Image::builder().icon_name("go-next-symbolic").css_classes(["dim-label"]).build());
        row.set_tooltip_text(Some(&a.path));
        let (ops, path) = (self.ops.clone(), a.path.clone());
        row.connect_activated(move |_| {
            ops.navigate(Nav::Files {
                dir: util::parent_of(&path).to_string(),
                filter: Filter::All,
                select: Some(path.clone()),
            });
        });
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(kind: ActivityKind, path: &str, bytes: Option<i64>, detail: Option<&str>) -> Activity {
        Activity { id: 1, at_ms: 0, kind, path: path.into(), bytes, detail: detail.map(Into::into) }
    }
    const NOON: Civil = Civil { year: 2026, month: 10, day: 13, hour: 12, minute: 9 };

    #[test]
    fn titles_and_icons() {
        let d = describe(&act(ActivityKind::Uploaded, "CloudDrop-test/hello.txt", Some(72), None));
        assert_eq!(d.title, "Uploaded hello.txt");
        assert_eq!(d.icon, "go-up-symbolic");
        assert_eq!(describe(&act(ActivityKind::Freed, "a/b.mp4", None, None)).icon, "drive-harddisk-symbolic");
        assert_eq!(describe(&act(ActivityKind::Restored, "b.mp4", None, None)).title, "Downloaded b.mp4");
        assert_eq!(describe(&act(ActivityKind::AutoFreed, "b", None, None)).title, "Automatically freed up b");
        assert_eq!(describe(&act(ActivityKind::KeptInCloud, "b", None, None)).tone, "info");
    }

    #[test]
    fn moves_and_renames() {
        let m = describe(&act(ActivityKind::Moved, "Trips/a.jpg", None, Some("Photos/a.jpg")));
        assert_eq!(m.title, "Moved a.jpg");
        assert_eq!(m.detail.as_deref(), Some("from Photos/a.jpg"));
        let r = describe(&act(ActivityKind::Moved, "Photos/new.jpg", None, Some("Photos/old.jpg")));
        assert_eq!(r.title, "Renamed new.jpg");
        assert_eq!(r.detail.as_deref(), Some("from old.jpg"));
    }

    #[test]
    fn failures_show_the_first_error_line() {
        let f = describe(&act(ActivityKind::Failed, "x.bin", Some(5), Some("403 quota exceeded\nmore text")));
        assert_eq!(f.title, "Couldn't upload x.bin");
        assert_eq!(f.tone, "warn");
        assert_eq!(f.detail.as_deref(), Some("403 quota exceeded"));
    }

    #[test]
    fn subtitles() {
        let a = act(ActivityKind::Uploaded, "CloudDrop-test/hello.txt", Some(72), None);
        assert_eq!(subtitle(&a, NOON), "CloudDrop-test · 72 B · 12:09");
        let top = act(ActivityKind::Uploaded, "hello.txt", Some(0), None);
        assert_eq!(subtitle(&top, NOON), "CloudDrop · 12:09");
        let m = act(ActivityKind::Moved, "Trips/a.jpg", Some(2048), Some("Photos/a.jpg"));
        assert_eq!(subtitle(&m, NOON), "Trips · from Photos/a.jpg · 2.0 KB · 12:09");
        let f = act(ActivityKind::Failed, "a/x.bin", Some(5), Some("boom"));
        assert_eq!(subtitle(&f, NOON), "a · boom · 12:09");
    }
}
