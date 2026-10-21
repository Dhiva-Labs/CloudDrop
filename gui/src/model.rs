//! What the Files tab shows: filters, sorting, queries against the database, and the
//! list-store item that lets rows update in place.

use crate::fmt::Status;
use clouddrop::db::{Db, DirItem, Entry, FolderStats, Kind, State};
use clouddrop::util;
use gtk::glib;
use gtk::glib::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Ref, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

// ---------- filters ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    OnComputer,
    CloudOnly,
    Uploading,
    Failed,
}

impl Filter {
    pub const ALL: [Filter; 5] = [
        Filter::All,
        Filter::OnComputer,
        Filter::CloudOnly,
        Filter::Uploading,
        Filter::Failed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All files",
            Filter::OnComputer => "On this computer",
            Filter::CloudOnly => "Cloud-only",
            Filter::Uploading => "Uploading",
            Filter::Failed => "Failed",
        }
    }

    pub fn index(self) -> u32 {
        Filter::ALL.iter().position(|f| *f == self).unwrap_or(0) as u32
    }

    pub fn from_index(i: u32) -> Filter {
        Filter::ALL.get(i as usize).copied().unwrap_or_default()
    }

    pub fn matches_state(self, state: State) -> bool {
        match self {
            Filter::All => true,
            Filter::OnComputer => state != State::CloudOnly,
            Filter::CloudOnly => state == State::CloudOnly,
            Filter::Uploading => matches!(state, State::Pending | State::Uploading),
            Filter::Failed => state == State::Error,
        }
    }

    /// Folders match when something beneath them does.
    pub fn matches(self, item: &DirItem) -> bool {
        match (item.entry.kind, &item.stats) {
            (Kind::Dir, Some(s)) => match self {
                Filter::All => true,
                Filter::OnComputer => s.local > 0,
                Filter::CloudOnly => s.cloud_only > 0,
                Filter::Uploading => s.waiting > 0,
                Filter::Failed => s.failed > 0,
            },
            _ => self.matches_state(item.entry.state),
        }
    }

    /// Title and text of the empty list; `cloud` names where CloudDrop uploads.
    pub fn empty_text(self, cloud: &str) -> (&'static str, String) {
        match self {
            Filter::All => ("Nothing here yet", String::new()),
            Filter::OnComputer => ("No files on this computer", format!("Everything in this folder is kept only in {cloud}.")),
            Filter::CloudOnly => (
                "No cloud-only files",
                format!("Files you free up or remove from this computer stay in {cloud} and show up here."),
            ),
            Filter::Uploading => ("Nothing is uploading", "Everything here has been uploaded.".into()),
            Filter::Failed => ("No failed uploads", "Every file here was uploaded successfully.".into()),
        }
    }
}

// ---------- sorting ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Status,
    Size,
    Date,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

pub fn item_name(it: &DirItem) -> &str {
    util::name_of(&it.entry.path)
}

pub fn item_status(it: &DirItem) -> Status {
    match (&it.stats, it.entry.kind) {
        (Some(s), Kind::Dir) => crate::fmt::folder_status(s).unwrap_or(Status::of(it.entry.state)),
        _ => Status::of(it.entry.state),
    }
}

/// Modified time in unix ms: the file's own, else when it was uploaded.
pub fn item_date_ms(e: &Entry) -> Option<i64> {
    e.local_mtime_ns
        .filter(|ns| *ns > 0)
        .map(|ns| ns / 1_000_000)
        .or(e.uploaded_at)
}

/// Folders first, then by the chosen column (name as tie-break).
pub fn sort_items(items: &mut [DirItem], sort: Sort) {
    items.sort_by(|a, b| {
        let dirs = (a.entry.kind != Kind::Dir).cmp(&(b.entry.kind != Kind::Dir));
        if dirs != Ordering::Equal {
            return dirs;
        }
        let by_name = || item_name(a).to_lowercase().cmp(&item_name(b).to_lowercase());
        let main = match sort.key {
            SortKey::Name => by_name(),
            SortKey::Status => item_status(a).rank().cmp(&item_status(b).rank()),
            SortKey::Size => a.bytes.cmp(&b.bytes),
            SortKey::Date => item_date_ms(&a.entry).cmp(&item_date_ms(&b.entry)),
        };
        let main = if sort.descending { main.reverse() } else { main };
        main.then_with(by_name)
    });
}

// ---------- minimal list updates ----------

/// How to turn the rows on screen into a new list while keeping what the user is doing
/// (scroll position, selection) for every row that stays.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// The order changed (a re-sort): replace everything.
    Replace,
    Edit {
        /// `(start, len)` runs to remove, last run first so earlier indices stay valid.
        removals: Vec<(usize, usize)>,
        /// `(at, from, to)`: insert `new[from..to]` at index `at`, first run first.
        inserts: Vec<(usize, usize, usize)>,
    },
}

pub fn plan_update(old: &[&str], new: &[&str]) -> Plan {
    let new_index: HashMap<&str, usize> = new.iter().enumerate().map(|(i, p)| (*p, i)).collect();
    // Rows that stay must keep their relative order, otherwise this is a re-sort.
    let mut last = None;
    for p in old {
        if let Some(&i) = new_index.get(p) {
            if last.is_some_and(|l| i < l) {
                return Plan::Replace;
            }
            last = Some(i);
        }
    }
    let mut removals: Vec<(usize, usize)> = Vec::new();
    for (idx, p) in old.iter().enumerate().rev() {
        if new_index.contains_key(p) {
            continue;
        }
        match removals.last_mut() {
            Some((start, len)) if *start == idx + 1 => {
                *start = idx;
                *len += 1;
            }
            _ => removals.push((idx, 1)),
        }
    }
    let old_set: HashSet<&str> = old.iter().copied().collect();
    let mut inserts: Vec<(usize, usize, usize)> = Vec::new();
    for (i, p) in new.iter().enumerate() {
        if old_set.contains(p) {
            continue;
        }
        match inserts.last_mut() {
            Some((_, _, to)) if *to == i => *to = i + 1,
            _ => inserts.push((i, i, i + 1)),
        }
    }
    Plan::Edit { removals, inserts }
}

// ---------- list-store item ----------

mod imp {
    use super::*;
    use glib::subclass::Signal;
    use std::sync::OnceLock;

    #[derive(Default)]
    pub struct ItemObj {
        pub data: RefCell<Option<DirItem>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemObj {
        const NAME: &'static str = "CloudDropItem";
        type Type = super::ItemObj;
    }

    impl ObjectImpl for ItemObj {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    /// One row of the file browser. Rows listen for `changed` and refill themselves.
    pub struct ItemObj(ObjectSubclass<imp::ItemObj>);
}

impl ItemObj {
    pub fn new(item: DirItem) -> Self {
        let obj: Self = glib::Object::new();
        *obj.imp().data.borrow_mut() = Some(item);
        obj
    }

    pub fn item(&self) -> Ref<'_, DirItem> {
        Ref::map(self.imp().data.borrow(), |d| d.as_ref().expect("item set at creation"))
    }

    pub fn path(&self) -> String {
        self.item().entry.path.clone()
    }

    /// Replaces the data and tells bound rows to refill (no-op if nothing changed).
    pub fn update(&self, item: DirItem) {
        if *self.item() == item {
            return;
        }
        *self.imp().data.borrow_mut() = Some(item);
        self.touch();
    }

    pub fn touch(&self) {
        self.emit_by_name::<()>("changed", &[]);
    }

    pub fn connect_changed(&self, f: impl Fn(&Self) + 'static) -> glib::SignalHandlerId {
        self.connect_local("changed", false, move |args| {
            let obj = args[0].get::<ItemObj>().expect("signal source");
            f(&obj);
            None
        })
    }
}

// ---------- what the selected items allow ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keep {
    /// Not a single folder.
    #[default]
    Unavailable,
    Off,
    On,
    /// Kept because a parent folder is kept; can't be switched off here.
    Inherited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caps {
    pub count: usize,
    pub download: bool,
    pub free: bool,
    pub open_drive: bool,
    pub show: bool,
    pub retry: bool,
    pub keep: Keep,
}

pub fn caps_for(items: &[DirItem], keep_state: &dyn Fn(&str) -> Keep) -> Caps {
    let mut c = Caps { count: items.len(), show: !items.is_empty(), ..Caps::default() };
    for it in items {
        let e = &it.entry;
        match (e.kind, &it.stats) {
            (Kind::Dir, Some(s)) => {
                c.download |= s.cloud_only > 0 || e.state == State::CloudOnly;
                c.free |= s.local - s.waiting - s.failed > 0;
                c.retry |= s.failed > 0 || e.state == State::Error;
            }
            _ => {
                c.download |= e.state == State::CloudOnly;
                c.free |= e.state == State::Synced;
                c.retry |= e.state == State::Error;
            }
        }
    }
    if let [only] = items {
        c.open_drive = only.entry.drive_id.is_some();
        if only.entry.kind == Kind::Dir {
            c.keep = keep_state(&only.entry.path);
        }
    }
    c
}

// ---------- queries ----------

/// Rows shown at most when a filter lists files across a whole tree.
pub const FLAT_LIMIT: usize = 5000;
pub const SEARCH_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Dir(String),
    Search(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub scope: Scope,
    pub filter: Filter,
}

impl Request {
    /// Several rows can come from different folders, so show where they live.
    pub fn is_flat(&self) -> bool {
        matches!(self.scope, Scope::Search(_)) || self.filter != Filter::All
    }
}

#[derive(Debug, Default)]
pub struct Listing {
    pub items: Vec<DirItem>,
    /// More matched than are shown.
    pub truncated: bool,
    /// Totals for the folder being viewed (searches: none).
    pub stats: Option<FolderStats>,
    /// The folder actually shown (a vanished folder falls back to its nearest parent).
    pub dir: String,
}

/// Nearest ancestor of `dir` (or `dir` itself) that is still tracked; "" = the top.
pub fn resolve_dir(db: &Db, dir: &str) -> String {
    let mut d = dir;
    while !d.is_empty() {
        if db.get(d).ok().flatten().is_some_and(|e| e.kind == Kind::Dir) {
            return d.to_string();
        }
        d = util::parent_of(d);
    }
    String::new()
}

fn file_item(e: Entry) -> DirItem {
    DirItem { bytes: e.size(), stats: None, entry: e }
}

pub fn run_query(db: &Db, req: &Request) -> anyhow::Result<Listing> {
    match &req.scope {
        Scope::Dir(dir) => {
            let dir = resolve_dir(db, dir);
            let stats = db.folder_stats(&dir)?;
            if req.filter == Filter::All {
                return Ok(Listing { items: db.list_dir(&dir)?, truncated: false, stats: Some(stats), dir });
            }
            let mut items = Vec::new();
            let mut truncated = false;
            for e in db.subtree(&dir)? {
                if e.kind != Kind::File || !req.filter.matches_state(e.state) {
                    continue;
                }
                if items.len() >= FLAT_LIMIT {
                    truncated = true;
                    break;
                }
                items.push(file_item(e));
            }
            Ok(Listing { items, truncated, stats: Some(stats), dir })
        }
        Scope::Search(needle) => {
            let hits = db.search(needle, SEARCH_LIMIT + 1)?;
            let mut items: Vec<DirItem> = hits.into_iter().filter(|i| req.filter.matches(i)).collect();
            let truncated = items.len() > SEARCH_LIMIT;
            items.truncate(SEARCH_LIMIT);
            Ok(Listing { items, truncated, stats: None, dir: String::new() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clouddrop::db::Entry;

    fn file(path: &str, state: State, size: i64) -> DirItem {
        let mut e = Entry::new(path, Kind::File, None);
        e.state = state;
        e.cloud_size = Some(size);
        e.local_size = Some(size);
        file_item(e)
    }

    fn dir(path: &str, stats: FolderStats) -> DirItem {
        let mut e = Entry::new(path, Kind::Dir, None);
        e.state = State::Synced;
        DirItem { bytes: stats.bytes, stats: Some(stats), entry: e }
    }

    #[test]
    fn filters_match_files_and_folders() {
        let synced = file("a", State::Synced, 1);
        let cloud = file("b", State::CloudOnly, 1);
        let wait = file("c", State::Pending, 1);
        let up = file("d", State::Uploading, 1);
        let bad = file("e", State::Error, 1);
        assert!(Filter::All.matches(&bad));
        assert!(Filter::OnComputer.matches(&synced) && !Filter::OnComputer.matches(&cloud));
        assert!(Filter::CloudOnly.matches(&cloud) && !Filter::CloudOnly.matches(&synced));
        assert!(Filter::Uploading.matches(&wait) && Filter::Uploading.matches(&up));
        assert!(Filter::Failed.matches(&bad) && !Filter::Failed.matches(&wait));

        let d = dir("f", FolderStats { files: 4, local: 3, cloud_only: 1, ..Default::default() });
        assert!(Filter::OnComputer.matches(&d));
        assert!(Filter::CloudOnly.matches(&d));
        assert!(!Filter::Failed.matches(&d));
        let empty = dir("g", FolderStats::default());
        assert!(Filter::All.matches(&empty) && !Filter::CloudOnly.matches(&empty));
        assert_eq!(Filter::from_index(Filter::Failed.index()), Filter::Failed);
        assert_eq!(Filter::from_index(99), Filter::All);
    }

    #[test]
    fn sorting_keeps_folders_first() {
        let mut v = vec![
            file("b.txt", State::Synced, 10),
            dir("Zeta", FolderStats::default()),
            file("A.txt", State::CloudOnly, 30),
            dir("alpha", FolderStats::default()),
            file("c.txt", State::Error, 20),
        ];
        let names = |v: &[DirItem]| v.iter().map(|i| item_name(i).to_string()).collect::<Vec<_>>();
        sort_items(&mut v, Sort::default());
        assert_eq!(names(&v), ["alpha", "Zeta", "A.txt", "b.txt", "c.txt"]);
        sort_items(&mut v, Sort { key: SortKey::Name, descending: true });
        assert_eq!(names(&v), ["Zeta", "alpha", "c.txt", "b.txt", "A.txt"]);
        sort_items(&mut v, Sort { key: SortKey::Size, descending: true });
        assert_eq!(&names(&v)[2..], ["A.txt", "c.txt", "b.txt"]);
        sort_items(&mut v, Sort { key: SortKey::Status, descending: false });
        assert_eq!(&names(&v)[2..], ["c.txt", "A.txt", "b.txt"]);
    }

    #[test]
    fn dates_prefer_the_files_own_time() {
        let mut e = Entry::new("a", Kind::File, None);
        assert_eq!(item_date_ms(&e), None);
        e.uploaded_at = Some(5_000);
        assert_eq!(item_date_ms(&e), Some(5_000));
        e.local_mtime_ns = Some(2_000_000_000);
        assert_eq!(item_date_ms(&e), Some(2_000));
    }

    #[test]
    fn minimal_splices() {
        let edit = |removals: Vec<(usize, usize)>, inserts: Vec<(usize, usize, usize)>| Plan::Edit { removals, inserts };
        assert_eq!(plan_update(&["a", "b", "c"], &["a", "b", "c"]), edit(vec![], vec![]));
        // one row replaced in the middle
        assert_eq!(plan_update(&["a", "b", "c"], &["a", "x", "c"]), edit(vec![(1, 1)], vec![(1, 1, 2)]));
        // appended
        assert_eq!(plan_update(&["a", "b"], &["a", "b", "c"]), edit(vec![], vec![(2, 2, 3)]));
        // a row removed near the top while another appears at the end: the rows in between stay
        assert_eq!(
            plan_update(&["a", "b", "c", "d"], &["a", "c", "d", "z"]),
            edit(vec![(1, 1)], vec![(3, 3, 4)])
        );
        // adjacent removals and insertions merge into runs; removals come last-first
        assert_eq!(
            plan_update(&["a", "b", "c", "d", "e"], &["a", "x", "y", "e"]),
            edit(vec![(1, 3)], vec![(1, 1, 3)])
        );
        assert_eq!(
            plan_update(&["a", "b", "c", "d"], &["b", "d"]),
            edit(vec![(2, 1), (0, 1)], vec![])
        );
        assert_eq!(plan_update(&[], &["a"]), edit(vec![], vec![(0, 0, 1)]));
        assert_eq!(plan_update(&["a"], &[]), edit(vec![(0, 1)], vec![]));
        // a different order is a re-sort
        assert_eq!(plan_update(&["a", "b", "c"], &["c", "b", "a"]), Plan::Replace);
    }

    #[test]
    fn capabilities_follow_the_selection() {
        let keep = |_: &str| Keep::Off;
        let c = caps_for(&[file("a", State::Synced, 1)], &keep);
        assert!(c.free && !c.download && !c.retry && c.show);
        let c = caps_for(&[file("a", State::CloudOnly, 1), file("b", State::Error, 1)], &keep);
        assert!(c.download && c.retry && !c.free && !c.open_drive);
        let mut f = file("a", State::Synced, 1);
        f.entry.drive_id = Some("ID".into());
        assert!(caps_for(&[f], &keep).open_drive);

        let d = dir("D", FolderStats { files: 5, local: 4, cloud_only: 1, waiting: 1, failed: 1, ..Default::default() });
        let c = caps_for(std::slice::from_ref(&d), &keep);
        assert!(c.download && c.free && c.retry);
        assert_eq!(c.keep, Keep::Off);
        let only_pending = dir("E", FolderStats { files: 2, local: 2, waiting: 2, ..Default::default() });
        assert!(!caps_for(&[only_pending], &keep).free);
        // keep toggle only for a single folder
        let c = caps_for(&[d.clone(), d], &keep);
        assert_eq!(c.keep, Keep::Unavailable);
        assert_eq!(caps_for(&[], &keep), Caps::default());
    }

    #[test]
    fn queries_against_a_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("state.db")).unwrap();
        let put = |path: &str, kind: Kind, state: State| {
            let mut e = Entry::new(path, kind, None);
            e.state = state;
            e.cloud_size = Some(10);
            e.local_size = Some(10);
            db.put(&e).unwrap();
        };
        put("Photos", Kind::Dir, State::Synced);
        put("Photos/a.jpg", Kind::File, State::Synced);
        put("Photos/b.jpg", Kind::File, State::CloudOnly);
        put("Photos/Trip", Kind::Dir, State::Synced);
        put("Photos/Trip/c.jpg", Kind::File, State::Error);
        put("top.txt", Kind::File, State::Pending);

        let dir_req = |d: &str, f| Request { scope: Scope::Dir(d.into()), filter: f };
        let l = run_query(&db, &dir_req("Photos", Filter::All)).unwrap();
        let names: Vec<_> = l.items.iter().map(|i| item_name(i).to_string()).collect();
        assert_eq!(names, ["Trip", "a.jpg", "b.jpg"]);
        assert_eq!(l.stats.unwrap().files, 3);

        let l = run_query(&db, &dir_req("", Filter::CloudOnly)).unwrap();
        assert_eq!(l.items.len(), 1);
        assert_eq!(l.items[0].entry.path, "Photos/b.jpg");
        let l = run_query(&db, &dir_req("", Filter::Failed)).unwrap();
        assert_eq!(l.items[0].entry.path, "Photos/Trip/c.jpg");
        let l = run_query(&db, &dir_req("Photos/Trip", Filter::OnComputer)).unwrap();
        assert_eq!(l.items.len(), 1);

        // a folder that no longer exists falls back to the nearest one that does
        let l = run_query(&db, &dir_req("Photos/Gone/Deeper", Filter::All)).unwrap();
        assert_eq!(l.dir, "Photos");

        let s = Request { scope: Scope::Search("JPG".into()), filter: Filter::All };
        assert_eq!(run_query(&db, &s).unwrap().items.len(), 3);
        let s = Request { scope: Scope::Search("jpg".into()), filter: Filter::Failed };
        assert_eq!(run_query(&db, &s).unwrap().items.len(), 1);
        assert!(s.is_flat());
        assert!(!dir_req("", Filter::All).is_flat());
    }
}
