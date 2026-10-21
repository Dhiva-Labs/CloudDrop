//! First-run assistant: welcome → where to upload → Google key (Google Drive without a
//! built-in client) → folders → sign in (or a form's Connect) → done. The same steps, from
//! "where to upload", set up another folder as a new connection (Add Another Folder…).

use crate::providers::{self, Choice, FormField, SignInKind};
use crate::{cli, rt, signin, ui};
use adw::prelude::*;
use clouddrop::config::{self, Config};
use clouddrop::connections::{self, Connection};
use clouddrop::control::{self, SetupParams};
use clouddrop::db::Db;
use clouddrop::remote::{FieldKind, Provider};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use tokio::task::AbortHandle;

const STEPS: [(&str, &str, &str); 6] = [
    (
        "1. Create a project",
        "Any name works, for example \"CloudDrop\".",
        "https://console.cloud.google.com/projectcreate",
    ),
    (
        "2. Turn on the Google Drive API",
        "Make sure your new project is selected, then click Enable.",
        "https://console.cloud.google.com/apis/library/drive.googleapis.com",
    ),
    (
        "3. Set up the consent screen",
        "Click Get started, enter an app name and your email, choose External.",
        "https://console.cloud.google.com/auth/overview",
    ),
    (
        "4. Allow access to CloudDrop's own files",
        "Add or remove scopes → add …/auth/drive.file → Save.",
        "https://console.cloud.google.com/auth/scopes",
    ),
    (
        "5. Publish the app",
        "Click Publish app, so your sign-in doesn't expire after 7 days.",
        "https://console.cloud.google.com/auth/audience",
    ),
    (
        "6. Create a Desktop app client",
        "Application type: Desktop app → Create → Download JSON.",
        "https://console.cloud.google.com/auth/clients/create",
    ),
];

/// Where the assistant starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Welcome,
    /// At the configured provider's sign-in (form providers have no sign-in dialog).
    SignIn,
    /// Settings › Switch Provider…: choose another cloud; everything uploads again.
    Switch,
    /// Add Another Folder…: a new connection, another folder uploading to a cloud of its own.
    /// It is named after its folder; leaving before the end removes what was written.
    AddFolder,
}

struct Wiz {
    client: Option<(String, String)>,
    folder: PathBuf,
    folder_locked: bool,
    drive_folder: String,
    sidebar: bool,
    login: Option<AbortHandle>,
    url: Rc<RefCell<Option<String>>>,
    /// Every provider offered, and the one chosen.
    choices: Vec<Choice>,
    chosen: Choice,
    /// The provider CloudDrop uploads to now.
    current: Choice,
    switching: bool,
    /// Switching wrote the new provider's settings (the old engine was stopped first).
    switch_started: bool,
    /// Setting up a new connection; `previous` is the one the window showed.
    adding: bool,
    previous: Connection,
    /// Reached the end: closing the assistant keeps what it set up.
    finished: bool,
}

/// A folder for another connection: `~/CloudDrop 2`, `~/CloudDrop 3`… the first no
/// connection uses.
fn new_folder() -> PathBuf {
    let home = gtk::glib::home_dir();
    (2..)
        .map(|n| home.join(format!("CloudDrop {n}")))
        .find(|p| connections::folder_in_use(p, "").is_none())
        .expect("unbounded")
}

/// The Google client another connection uses, for a new Google Drive connection.
fn any_google_client() -> Option<(String, String)> {
    connections::all()
        .iter()
        .filter_map(|c| c.load().ok())
        .find(|c| c.provider == Provider::GoogleDrive && !c.client_id.is_empty())
        .map(|c| (c.client_id, c.client_secret))
}

/// The cloud folder name for a new connection's folder: the folder's own name.
fn cloud_folder_for(folder: &std::path::Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "CloudDrop".into())
}

impl Wiz {
    fn new(start: Start) -> Self {
        let (switching, adding) = (start == Start::Switch, start == Start::AddFolder);
        let previous = connections::current();
        if adding {
            let choices = providers::all();
            let folder = new_folder();
            return Wiz {
                client: any_google_client(),
                drive_folder: cloud_folder_for(&folder),
                folder,
                folder_locked: false,
                sidebar: true,
                login: None,
                url: Rc::default(),
                chosen: choices[0].clone(),
                current: choices[0].clone(),
                choices,
                switching: false,
                switch_started: false,
                adding,
                previous,
                finished: false,
            };
        }
        let cfg = Config::load().ok();
        let locked_root = Db::open_default()
            .ok()
            .and_then(|db| db.meta_get("sync_root").ok().flatten())
            .map(PathBuf::from);
        let defaults = Config::default();
        let choices = providers::all();
        let current = Choice::current(cfg.as_ref());
        let chosen = choices.iter().find(|c| c.id == current.id).unwrap_or(&choices[0]).clone();
        Wiz {
            client: cfg
                .as_ref()
                .filter(|c| !c.client_id.is_empty())
                .map(|c| (c.client_id.clone(), c.client_secret.clone())),
            folder: locked_root
                .clone()
                .or_else(|| cfg.as_ref().map(|c| c.sync_dir.clone()))
                .unwrap_or(defaults.sync_dir),
            folder_locked: locked_root.is_some(),
            drive_folder: cfg.map(|c| c.drive_folder).unwrap_or(defaults.drive_folder),
            sidebar: true,
            login: None,
            url: Rc::default(),
            choices,
            chosen,
            current,
            switching,
            switch_started: false,
            adding,
            previous,
            finished: false,
        }
    }

    fn params(&self) -> SetupParams {
        SetupParams {
            client: self.client.clone(),
            sync_dir: self.folder.clone(),
            drive_folder: self.drive_folder.clone(),
        }
    }

    /// "Keep Current Sign-in": the chosen provider is the one signed in now.
    fn can_keep_sign_in(&self) -> bool {
        !self.switching && !self.adding && self.chosen.id == self.current.id && self.chosen.signed_in()
    }

    /// Adding a folder: the new connection everything is written to from now on, named after
    /// the folder chosen. Until its config is written it follows the folder; then it stays.
    fn name_new_connection(&self) {
        if !self.adding {
            return;
        }
        let cur = connections::current();
        if cur != self.previous && cur.exists() {
            return;
        }
        let Ok(conn) = Connection::named(&connections::suggest_name(&self.folder)) else {
            return;
        };
        if cur != self.previous && cur != conn {
            connections::discard_unfinished(&cur);
        }
        connections::select(&conn);
    }

    /// Leaving before the end: what was written for the new connection goes, and the window
    /// shows the connection it showed before.
    fn abandon(&self) {
        if !self.adding || self.finished {
            return;
        }
        let cur = connections::current();
        connections::select(&self.previous);
        if cur == self.previous {
            return;
        }
        if cur.exists() {
            rt::blocking(move || connections::remove(&cur, std::time::Duration::from_secs(10)), |res| {
                if let Err(e) = res {
                    eprintln!("cannot remove the unfinished folder: {e:#}");
                }
            });
        } else {
            connections::discard_unfinished(&cur);
        }
    }
}

/// What the assistant's pages share.
struct Cx {
    win: adw::ApplicationWindow,
    nav: adw::NavigationView,
    st: Rc<RefCell<Wiz>>,
    done: adw::NavigationPage,
}

impl Cx {
    /// Runs `then` once it's safe to write the chosen provider's settings: when switching,
    /// the old provider's engine is stopped first, so it uploads nothing more into a database
    /// that is about to move on.
    fn before_writing(self: &Rc<Self>, then: impl FnOnce() + 'static) {
        self.st.borrow().name_new_connection();
        let mut w = self.st.borrow_mut();
        if !w.switching || w.switch_started {
            drop(w);
            return then();
        }
        w.switch_started = true;
        drop(w);
        // Not running is fine too.
        cli::run_all(vec![cli::args(&["service", "stop"])], move |_| then());
    }

    /// The chosen provider works. When switching, everything goes up again into it
    /// (`reset-cloud`); then Done. Errors go to `error`.
    fn connected(self: &Rc<Self>, error: &gtk::Label) {
        if !self.st.borrow().switching {
            return self.nav.push(&self.done);
        }
        let (cx, error) = (self.clone(), error.clone());
        cli::run_all(vec![cli::args(&["reset-cloud", "--yes"])], move |res| match res {
            Ok(_) => cx.nav.push(&cx.done),
            Err(e) => show_error(&error, &format!("{e:#}")),
        });
    }

    /// The CLI's `setup` for the chosen provider (not Google Drive).
    fn setup_args(&self, settings: &[(String, String)], secrets: &[(String, String)]) -> Vec<String> {
        let w = self.st.borrow();
        cli::setup_args(&w.chosen.id, &w.folder, &w.drive_folder, settings, secrets)
    }
}

fn show_error(label: &gtk::Label, text: &str) {
    label.set_label(text);
    label.set_visible(true);
}

/// Builds the assistant window. `on_finish(show_tray)` runs when the user clicks Finish.
pub fn build(app: &adw::Application, start: Start, on_finish: impl Fn(bool) + 'static) -> adw::ApplicationWindow {
    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title(if start == Start::AddFolder { "Add Another Folder" } else { "Set Up CloudDrop" })
        .default_width(620)
        .default_height(760)
        .build();
    let nav = adw::NavigationView::new();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&nav));
    win.set_content(Some(&toasts));

    let st = Rc::new(RefCell::new(Wiz::new(start)));
    let done = done_page(&win, &st, Rc::new(on_finish));
    let cx = Rc::new(Cx { win: win.clone(), nav: nav.clone(), st: st.clone(), done });
    let folder = folder_page(&cx);
    let key = control::builtin_client().is_none().then(|| key_page(&win, &nav, &st, &folder));
    let provider = provider_page(&cx, key.clone(), folder.clone());
    let welcome = welcome_page(&nav, &provider);
    match start {
        Start::Welcome => nav.add(&welcome),
        Start::Switch | Start::AddFolder => nav.add(&provider),
        Start::SignIn => {
            // Signed out: straight to the sign-in, with the way back to every page before it.
            let mut stack = vec![welcome, provider];
            if st.borrow().chosen.is_google()
                && let Some(k) = key
            {
                stack.push(k);
            }
            stack.push(folder);
            stack.push(signin_for(&cx));
            nav.replace(&stack);
        }
    }

    // Stop a pending browser sign-in when the assistant goes away.
    let st2 = st.clone();
    win.connect_close_request(move |_| {
        if let Some(h) = st2.borrow_mut().login.take() {
            h.abort();
        }
        st2.borrow().abandon();
        gtk::glib::Propagation::Proceed
    });
    win
}

fn welcome_page(nav: &adw::NavigationView, next: &adw::NavigationPage) -> adw::NavigationPage {
    let content = ui::vbox(18);
    let icon = gtk::Image::builder()
        .icon_name(control::APP_ID)
        .pixel_size(128)
        .halign(gtk::Align::Center)
        .css_classes(["welcome-logo"])
        .build();
    content.append(&icon);
    content.append(&ui::label("Welcome to CloudDrop", &["title-1"]));
    content.append(&ui::label(
        "Anything you put in your CloudDrop folder is uploaded to the cloud. \
         Remove it from this computer whenever you like — it stays there.",
        &["body"],
    ));
    let points = adw::PreferencesGroup::new();
    for (icon, title, sub) in [
        ("folder-symbolic", "Keeps your folders", "The same folder structure appears in the cloud."),
        ("clouddrop-syncing-symbolic", "Uploads by itself", "New and changed files go up in the background."),
        ("security-high-symbolic", "Never deletes in the cloud", "Free up space here without losing anything."),
    ] {
        let row = adw::ActionRow::builder().title(title).subtitle(sub).build();
        row.add_prefix(&gtk::Image::builder().icon_name(icon).valign(gtk::Align::Center).css_classes(["feature-icon"]).build());
        points.add(&row);
    }
    content.append(&points);
    let start = ui::pill("Get Started", true);
    let (nav, next) = (nav.clone(), next.clone());
    start.connect_clicked(move |_| nav.push(&next));
    ui::nav_page("Welcome", &content, Some(&start))
}

/// The provider page's sentence under its title.
fn provider_intro(switching: bool, adding: bool, current: &str) -> String {
    if adding {
        return "Pick the cloud another folder uploads to. Your other folders carry on as they are.".into();
    }
    if switching {
        format!(
            "Everything in your CloudDrop folder is uploaded again to the place you pick. \
             Nothing is deleted from {current}."
        )
    } else {
        "Pick the cloud CloudDrop uploads to. You can switch later in Settings.".into()
    }
}

fn provider_page(cx: &Rc<Cx>, key: Option<adw::NavigationPage>, folder: adw::NavigationPage) -> adw::NavigationPage {
    let content = ui::vbox(18);
    content.append(&ui::label("Choose where to upload", &["title-2"]));
    let (switching, adding, current, choices, chosen) = {
        let w = cx.st.borrow();
        (w.switching, w.adding, w.current.name.clone(), w.choices.clone(), w.chosen.id.clone())
    };
    content.append(&ui::label(&provider_intro(switching, adding, &current), &["dim-label"]));

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let mut group: Option<gtk::CheckButton> = None;
    let mut checks = Vec::new();
    for c in choices {
        let check = gtk::CheckButton::builder().valign(gtk::Align::Center).active(c.id == chosen).build();
        match &group {
            Some(g) => check.set_group(Some(g)),
            None => group = Some(check.clone()),
        }
        let text = ui::vbox(2);
        text.set_hexpand(true);
        text.append(&ui::start_label(&c.name, &["heading"]));
        if !c.description.is_empty() {
            text.append(&ui::start_label(&c.description, &["dim-label", "caption"]));
        }
        if c.own_folder {
            let own = gtk::Box::builder().spacing(6).margin_top(2).build();
            own.append(&gtk::Image::builder().icon_name("security-high-symbolic").pixel_size(12).css_classes(["own-folder"]).build());
            own.append(&ui::start_label("CloudDrop only sees its own folder", &["caption"]));
            text.append(&own);
        }
        let row_box = gtk::Box::builder().spacing(12).css_classes(["provider-row"]).build();
        row_box.append(&gtk::Image::builder().icon_name(c.icon).pixel_size(32).valign(gtk::Align::Center).build());
        row_box.append(&text);
        row_box.append(&check);
        let row = gtk::ListBoxRow::builder().child(&row_box).activatable(true).build();
        row.update_property(&[gtk::accessible::Property::Label(&c.name)]);
        checks.push(check.clone());
        {
            let st = cx.st.clone();
            check.connect_toggled(move |b| {
                if b.is_active() {
                    st.borrow_mut().chosen = c.clone();
                }
            });
        }
        list.append(&row);
    }
    // A click anywhere on a row chooses it.
    list.connect_row_activated(move |_, row| {
        if let Some(check) = usize::try_from(row.index()).ok().and_then(|i| checks.get(i)) {
            check.set_active(true);
        }
    });
    content.append(&list);

    let cont = ui::pill("Continue", true);
    {
        let cx = cx.clone();
        cont.connect_clicked(move |_| {
            let google = cx.st.borrow().chosen.is_google();
            match (&key, google) {
                (Some(k), true) => cx.nav.push(k),
                _ => cx.nav.push(&folder),
            }
        });
    }
    let (page, header) = ui::nav_page_with_header("Upload To", &content, Some(&cont));
    if adding {
        let cancel = gtk::Button::with_label("Cancel");
        let win = cx.win.clone();
        cancel.connect_clicked(move |_| win.close());
        header.pack_start(&cancel);
    } else if switching {
        // Nothing is written until the sign-in step: leaving before it changes nothing.
        let cancel = gtk::Button::with_label("Cancel");
        let win = cx.win.clone();
        cancel.connect_clicked(move |_| win.close());
        header.pack_start(&cancel);
        let st = cx.st.clone();
        page.connect_showing(move |_| cancel.set_visible(!st.borrow().switch_started));
    }
    page
}

fn key_page(
    win: &adw::ApplicationWindow,
    nav: &adw::NavigationView,
    st: &Rc<RefCell<Wiz>>,
    next: &adw::NavigationPage,
) -> adw::NavigationPage {
    let content = ui::vbox(24);
    content.append(&ui::label("Connect a Google key", &["title-2"]));
    content.append(&ui::label(
        "Google asks every app that uploads to Drive to use a key from your own Google \
         account. It takes about five minutes, once, and the key only lets CloudDrop \
         see the files it uploads.",
        &["dim-label"],
    ));

    let steps = adw::PreferencesGroup::builder()
        .title("Create your key")
        .description("Each button opens the right page in Google Cloud.")
        .build();
    for (title, sub, url) in STEPS {
        steps.add(&ui::link_row(title, sub, url));
    }
    content.append(&steps);

    let group = adw::PreferencesGroup::builder()
        .title("Add your key")
        .description("Choose the JSON file you downloaded, or paste the two values.")
        .build();
    let choose = adw::ActionRow::builder()
        .title("Choose Downloaded Key File…")
        .activatable(true)
        .build();
    choose.add_prefix(&gtk::Image::from_icon_name("document-open-symbolic"));
    let id_row = adw::EntryRow::builder().title("Client ID").build();
    let secret_row = adw::PasswordEntryRow::builder().title("Client secret").build();
    let error = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();
    group.add(&choose);
    group.add(&id_row);
    group.add(&secret_row);
    content.append(&group);
    content.append(&error);

    let client = st.borrow().client.clone();
    if let Some((id, secret)) = client {
        id_row.set_text(&id);
        secret_row.set_text(&secret);
    }

    let cont = ui::pill("Continue", true);
    let validate = {
        let (id_row, secret_row, cont) = (id_row.clone(), secret_row.clone(), cont.clone());
        move || {
            let ok = control::looks_like_client_id(&id_row.text())
                && !secret_row.text().trim().is_empty();
            cont.set_sensitive(ok);
        }
    };
    validate();
    let fill = {
        let (id_row, secret_row, error) = (id_row.clone(), secret_row.clone(), error.clone());
        move |text: &str| match control::parse_client_json(text) {
            Ok((id, secret)) => {
                id_row.set_text(&id);
                secret_row.set_text(&secret);
                error.set_visible(false);
            }
            Err(e) => {
                error.set_label(&format!("{e:#}"));
                error.set_visible(true);
            }
        }
    };
    {
        // Pasting the whole JSON into the ID field works too.
        let fill = fill.clone();
        let validate = validate.clone();
        id_row.connect_changed(move |row| {
            let text = row.text();
            if text.trim_start().starts_with('{') {
                fill(&text);
            }
            validate();
        });
    }
    {
        let validate = validate.clone();
        secret_row.connect_changed(move |_| validate());
    }
    {
        let win = win.clone();
        choose.connect_activated(move |_| {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Google key file (JSON)"));
            filter.add_suffix("json");
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let dialog = gtk::FileDialog::builder()
                .title("Choose the Downloaded Key File")
                .modal(true)
                .filters(&filters)
                .initial_folder(&gtk::gio::File::for_path(
                    gtk::glib::user_special_dir(gtk::glib::UserDirectory::Downloads)
                        .unwrap_or_else(gtk::glib::home_dir),
                ))
                .build();
            let fill = fill.clone();
            dialog.open(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res
                    && let Some(path) = file.path()
                {
                    match std::fs::read_to_string(&path) {
                        Ok(text) => fill(&text),
                        Err(e) => fill(&format!("cannot read {}: {e}", path.display())),
                    }
                }
            });
        });
    }
    {
        let (nav, next, st) = (nav.clone(), next.clone(), st.clone());
        let (id_row, secret_row) = (id_row.clone(), secret_row.clone());
        cont.connect_clicked(move |_| {
            st.borrow_mut().client = Some((
                id_row.text().trim().to_string(),
                secret_row.text().trim().to_string(),
            ));
            nav.push(&next);
        });
    }
    ui::nav_page("Google Key", &content, Some(&cont))
}


/// The folder page's cloud half: (group title, its description, the name field's title).
fn cloud_folder_texts(c: &Choice) -> (String, String, String) {
    (
        format!("In {}", c.name),
        format!("Your files appear in this folder in {}, with the same subfolders.", c.root_label()),
        format!("Folder name in {}", c.root_label()),
    )
}

fn folder_page(cx: &Rc<Cx>) -> adw::NavigationPage {
    let st = &cx.st;
    let content = ui::vbox(24);
    content.append(&ui::label("Choose your folders", &["title-2"]));

    let local = adw::PreferencesGroup::builder().title("On this computer").build();
    let adding = st.borrow().adding;
    let folder_row = adw::ActionRow::builder()
        .title(if adding { "Folder to upload" } else { "CloudDrop folder" })
        .subtitle(ui::pretty_path(&st.borrow().folder))
        .build();
    folder_row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
    let change = gtk::Button::builder()
        .label("Change…")
        .valign(gtk::Align::Center)
        .build();
    if st.borrow().folder_locked {
        change.set_sensitive(false);
        change.set_tooltip_text(Some("The folder can't be changed after CloudDrop has used it"));
    }
    folder_row.add_suffix(&change);
    local.add(&folder_row);
    let sidebar = adw::SwitchRow::builder()
        .title("Show in the Files sidebar")
        .active(true)
        .build();
    local.add(&sidebar);
    content.append(&local);

    let remote = adw::PreferencesGroup::new();
    let drive_row = adw::EntryRow::new();
    drive_row.set_text(&st.borrow().drive_folder);
    remote.add(&drive_row);
    content.append(&remote);

    {
        let (win, st, folder_row, drive_row) = (cx.win.clone(), st.clone(), folder_row.clone(), drive_row.clone());
        change.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title(if adding { "Choose the Folder to Upload" } else { "Choose the CloudDrop Folder" })
                .modal(true)
                .initial_folder(&gtk::gio::File::for_path(gtk::glib::home_dir()))
                .build();
            let (st, folder_row, drive_row) = (st.clone(), folder_row.clone(), drive_row.clone());
            dialog.select_folder(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                if let Ok(f) = res
                    && let Some(p) = f.path()
                {
                    folder_row.set_subtitle(&ui::pretty_path(&p));
                    // A new connection's cloud folder follows its folder's name until edited.
                    if adding && drive_row.text().trim() == cloud_folder_for(&st.borrow().folder) {
                        drive_row.set_text(&cloud_folder_for(&p));
                    }
                    st.borrow_mut().folder = p;
                }
            });
        });
    }
    let cont = ui::pill("Continue", true);
    {
        let cont = cont.clone();
        drive_row.connect_changed(move |r| cont.set_sensitive(!r.text().trim().is_empty()));
    }
    {
        let cx = cx.clone();
        let drive_row = drive_row.clone();
        cont.connect_clicked(move |_| {
            let mut w = cx.st.borrow_mut();
            w.drive_folder = drive_row.text().trim().to_string();
            w.sidebar = sidebar.is_active();
            drop(w);
            // A page of its own for the provider chosen now.
            cx.nav.push(&signin_for(&cx));
        });
    }
    let page = ui::nav_page("Folders", &content, Some(&cont));
    // The provider may have changed since this page was built.
    let st = st.clone();
    page.connect_showing(move |_| {
        let (title, description, field) = cloud_folder_texts(&st.borrow().chosen);
        remote.set_title(&title);
        remote.set_description(Some(&description));
        drive_row.set_title(&field);
    });
    page
}

/// The chosen provider's sign-in page.
fn signin_for(cx: &Rc<Cx>) -> adw::NavigationPage {
    let chosen = cx.st.borrow().chosen.clone();
    match &chosen.sign_in {
        SignInKind::Google => google_signin_page(cx),
        SignInKind::OAuth | SignInKind::Nextcloud => browser_signin_page(cx, &chosen),
        SignInKind::Form(fields) => connect_page(cx, &chosen, fields),
    }
}

/// The sign-in page's title, text and button for a browser sign-in.
fn signin_texts(c: &Choice) -> (String, String, String) {
    match c.sign_in {
        SignInKind::Google => (
            "Sign in to Google".into(),
            "Your browser will open Google's sign-in page. Choose your account and allow \
             CloudDrop to add files to your Drive."
                .into(),
            "Sign In with Google".into(),
        ),
        SignInKind::Nextcloud => (
            format!("Sign in to {}", c.name),
            "Enter your server's address. Your browser opens it so you can log in and allow \
             CloudDrop, which gets an app password of its own — never your password."
                .into(),
            "Sign In".into(),
        ),
        _ => (
            format!("Sign in to {}", c.name),
            format!(
                "Your browser will open {}'s sign-in page. Choose your account and allow \
                 CloudDrop to add files to its folder.",
                c.name
            ),
            format!("Sign In with {}", c.name),
        ),
    }
}

/// Logo, title and text at the top of a sign-in page.
fn signin_header(content: &gtk::Box, c: &Choice) {
    content.append(&gtk::Image::builder().icon_name(control::APP_ID).pixel_size(96).build());
    let (title, text, _) = signin_texts(c);
    content.append(&ui::label(&title, &["title-2"]));
    content.append(&ui::label(&text, &["dim-label"]));
}

/// The spinner, waiting line, link button and error of a browser sign-in.
struct Waiting {
    spinner: gtk::Spinner,
    label: gtk::Label,
    copy: gtk::Button,
    error: gtk::Label,
}

impl Waiting {
    fn new(content: &gtk::Box, url: &Rc<RefCell<Option<String>>>) -> Self {
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .width_request(32)
            .height_request(32)
            .visible(false)
            .build();
        let label = ui::label("Waiting for you to finish in the browser…", &["dim-label"]);
        label.set_visible(false);
        let copy = signin::copy_link_button(url);
        copy.set_visible(false);
        let error = ui::label("", &["error"]);
        error.set_visible(false);
        for w in [spinner.upcast_ref::<gtk::Widget>(), label.upcast_ref(), copy.upcast_ref(), error.upcast_ref()] {
            content.append(w);
        }
        Waiting { spinner, label, copy, error }
    }

    fn show(&self, on: bool) {
        self.spinner.set_visible(on);
        self.label.set_visible(on);
        if on {
            self.copy.set_visible(true);
        }
    }
}

/// Starts a browser sign-in (Google in-process, the others through the CLI) and moves on
/// when it finishes.
fn browser_login(cx: &Rc<Cx>, google: Option<Config>, go: &gtk::Button, idle_label: &str, wait: &Rc<Waiting>) {
    if let Some(h) = cx.st.borrow_mut().login.take() {
        h.abort();
    }
    go.set_label("Open Sign-in Again");
    wait.show(true);
    let url = cx.st.borrow().url.clone();
    let url_ready = {
        let copy = wait.copy.clone();
        move |u| {
            *url.borrow_mut() = Some(u);
            copy.set_sensitive(true);
        }
    };
    let done = {
        let (cx, go, wait, idle_label) = (cx.clone(), go.clone(), wait.clone(), idle_label.to_string());
        move |res: anyhow::Result<()>| {
            cx.st.borrow_mut().login = None;
            wait.show(false);
            go.set_label(&idle_label);
            match res {
                Ok(()) => cx.connected(&wait.error),
                Err(e) => show_error(&wait.error, &format!("Sign-in didn't finish: {e:#}")),
            }
        }
    };
    let handle = match google {
        Some(cfg) => signin::start(cfg, url_ready, done),
        None => signin::start_cli(url_ready, done),
    };
    cx.st.borrow_mut().login = Some(handle);
}

/// Google Drive: the config written in-process, then Google's own browser sign-in.
fn google_signin_page(cx: &Rc<Cx>) -> adw::NavigationPage {
    let content = ui::vbox(18);
    let chosen = cx.st.borrow().chosen.clone();
    signin_header(&content, &chosen);
    if control::builtin_client().is_none() {
        content.append(&ui::label(
            "If Google says the app isn't verified, that's expected for your own key: \
             choose Advanced → Go to (your app name).",
            &["dim-label", "caption"],
        ));
    }
    let wait = Rc::new(Waiting::new(&content, &cx.st.borrow().url));
    let (_, _, button) = signin_texts(&chosen);
    let go = ui::pill(&button, true);
    let keep = gtk::Button::builder()
        .label("Keep Current Sign-in")
        .halign(gtk::Align::Center)
        .css_classes(["flat"])
        .visible(cx.st.borrow().can_keep_sign_in())
        .build();
    content.append(&go);
    content.append(&keep);

    {
        let (cx, wait) = (cx.clone(), wait.clone());
        keep.connect_clicked(move |_| match write_google_config(&cx) {
            Ok(_) => cx.connected(&wait.error),
            Err(e) => show_error(&wait.error, &format!("{e:#}")),
        });
    }
    {
        let (cx, wait) = (cx.clone(), wait.clone());
        go.connect_clicked(move |go| {
            wait.error.set_visible(false);
            let (cx2, wait, go, button) = (cx.clone(), wait.clone(), go.clone(), button.clone());
            cx.before_writing(move || match write_google_config(&cx2) {
                Ok(cfg) => browser_login(&cx2, Some(cfg), &go, &button, &wait),
                Err(e) => show_error(&wait.error, &format!("{e:#}")),
            });
        });
    }
    ui::nav_page("Sign In", &content, None)
}

/// Writes Google Drive's settings; makes Google Drive the provider again after another one.
fn write_google_config(cx: &Rc<Cx>) -> anyhow::Result<Config> {
    let mut cfg = control::apply_setup(&cx.st.borrow().params())?;
    if cfg.provider != Provider::GoogleDrive {
        cfg.provider = Provider::GoogleDrive;
        cfg.save_to(&config::config_path())?;
    }
    Ok(cfg)
}

/// OAuth providers (OneDrive, Dropbox) and Nextcloud's Login Flow: `clouddrop setup`, then
/// `clouddrop login --no-browser`.
fn browser_signin_page(cx: &Rc<Cx>, chosen: &Choice) -> adw::NavigationPage {
    let content = ui::vbox(18);
    signin_header(&content, chosen);
    let nextcloud = chosen.sign_in == SignInKind::Nextcloud;
    let server = adw::EntryRow::builder().title("Server address").input_purpose(gtk::InputPurpose::Url).build();
    let server_hint = ui::start_label("Enter an address that starts with https://", &["error", "caption"]);
    server_hint.set_visible(false);
    if nextcloud {
        server.set_text(saved_settings(&chosen.id).get("url").map(String::as_str).unwrap_or_default());
        let group = adw::PreferencesGroup::builder().description("For example https://cloud.example.com").build();
        group.add(&server);
        content.append(&group);
        content.append(&server_hint);
    }
    let wait = Rc::new(Waiting::new(&content, &cx.st.borrow().url));
    let (_, _, button) = signin_texts(chosen);
    let go = ui::pill(&button, true);
    let keep = gtk::Button::builder()
        .label("Keep Current Sign-in")
        .halign(gtk::Align::Center)
        .css_classes(["flat"])
        .visible(cx.st.borrow().can_keep_sign_in())
        .build();
    content.append(&go);
    content.append(&keep);
    if nextcloud {
        let check = {
            let (go, hint) = (go.clone(), server_hint.clone());
            move |r: &adw::EntryRow| {
                let text = r.text();
                go.set_sensitive(providers::looks_like_web_address(&text));
                hint.set_visible(!text.trim().is_empty() && !providers::looks_like_web_address(&text));
            }
        };
        check(&server);
        server.connect_changed(check);
    }

    {
        let (cx, wait) = (cx.clone(), wait.clone());
        keep.connect_clicked(move |_| {
            let (cx2, wait) = (cx.clone(), wait.clone());
            // The settings may have changed (the folder name); the sign-in stays.
            cli::run_all(vec![cx.setup_args(&[], &[])], move |res| match res {
                Ok(_) => cx2.connected(&wait.error),
                Err(e) => show_error(&wait.error, &format!("{e:#}")),
            });
        });
    }
    {
        let (cx, wait, server) = (cx.clone(), wait.clone(), server.clone());
        go.connect_clicked(move |go| {
            wait.error.set_visible(false);
            let settings = if nextcloud { vec![("url".to_string(), server.text().trim().to_string())] } else { vec![] };
            let args = cx.setup_args(&settings, &[]);
            let (cx2, wait, go, button) = (cx.clone(), wait.clone(), go.clone(), button.clone());
            go.set_sensitive(false);
            cx.before_writing(move || {
                cli::run_all(vec![args], move |res| {
                    go.set_sensitive(true);
                    match res {
                        Ok(_) => browser_login(&cx2, None, &go, &button, &wait),
                        Err(e) => show_error(&wait.error, &format!("{e:#}")),
                    }
                })
            });
        });
    }
    ui::nav_page("Sign In", &content, None)
}

/// The Connect page's title and text for a form provider.
fn connect_texts(c: &Choice) -> (String, String) {
    (
        format!("Connect to {}", c.name),
        format!(
            "Enter your {} details. Passwords and keys are saved apart from the settings, readable only by you.",
            c.name
        ),
    )
}

/// One field of a provider's form.
fn field_row(win: &adw::ApplicationWindow, f: &FormField, saved: Option<&String>) -> adw::EntryRow {
    let row: adw::EntryRow = if f.kind == FieldKind::Secret {
        adw::PasswordEntryRow::new().upcast()
    } else {
        adw::EntryRow::new()
    };
    row.set_title(&providers::field_title(f));
    match f.kind {
        FieldKind::Url => row.set_input_purpose(gtk::InputPurpose::Url),
        FieldKind::Number => row.set_input_purpose(gtk::InputPurpose::Digits),
        _ => {}
    }
    // Secrets never come back from the settings.
    if f.kind != FieldKind::Secret
        && let Some(v) = saved
    {
        row.set_text(v);
    }
    if f.kind == FieldKind::File {
        let choose = gtk::Button::builder().label("Choose…").valign(gtk::Align::Center).build();
        let (win, row2, title) = (win.clone(), row.clone(), f.label.clone());
        choose.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title(title.as_str())
                .modal(true)
                .initial_folder(&gtk::gio::File::for_path(gtk::glib::home_dir()))
                .build();
            let row = row2.clone();
            dialog.open(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res
                    && let Some(p) = file.path()
                {
                    row.set_text(&p.to_string_lossy());
                }
            });
        });
        row.add_suffix(&choose);
    }
    row
}

/// Form providers (WebDAV, S3, SFTP, MEGA): the fields, Test Connection (`setup` with
/// `--set`/`--secret`, then `check`), and Continue once the test worked.
fn connect_page(cx: &Rc<Cx>, chosen: &Choice, fields: &[FormField]) -> adw::NavigationPage {
    let content = ui::vbox(18);
    let (title, text) = connect_texts(chosen);
    content.append(&ui::start_label(&title, &["title-2"]));
    content.append(&ui::start_label(&text, &["dim-label"]));

    let saved = saved_settings(&chosen.id);
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    for f in fields {
        // A group per field, so its hint sits right with it.
        let group = adw::PreferencesGroup::new();
        if !f.hint.is_empty() {
            group.set_description(Some(&f.hint));
        }
        let row = field_row(&cx.win, f, saved.get(&f.key));
        group.add(&row);
        let problem = ui::start_label("", &["error", "caption"]);
        problem.set_visible(false);
        content.append(&group);
        content.append(&problem);
        rows.push(row);
        problems.push(problem);
    }

    let testing = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).visible(false).build();
    testing.append(&gtk::Spinner::builder().spinning(true).build());
    testing.append(&ui::label("Testing the connection…", &["dim-label"]));
    let result = ui::label("", &[]);
    result.set_visible(false);
    let error = ui::label("", &["error"]);
    error.set_visible(false);
    let test = gtk::Button::builder().label("Test Connection").halign(gtk::Align::Center).css_classes(["pill"]).build();
    content.append(&testing);
    content.append(&result);
    content.append(&error);
    content.append(&test);
    let cont = ui::pill("Continue", true);
    cont.set_sensitive(false);

    let rows = Rc::new(rows);
    let fields: Rc<Vec<FormField>> = Rc::new(fields.to_vec());
    let values = {
        let rows = rows.clone();
        move || rows.iter().map(|r| r.text().to_string()).collect::<Vec<_>>()
    };
    // Any change needs a new test; problems show under their field.
    let changed = {
        let (fields, values, test, cont, result, error) =
            (fields.clone(), values.clone(), test.clone(), cont.clone(), result.clone(), error.clone());
        move || {
            let v = values();
            for ((f, value), label) in fields.iter().zip(&v).zip(&problems) {
                match providers::check_value(f.kind, value) {
                    Err(why) => show_error(label, why),
                    Ok(()) => label.set_visible(false),
                }
            }
            test.set_sensitive(providers::form_ready(&fields, &v));
            cont.set_sensitive(false);
            result.set_visible(false);
            error.set_visible(false);
        }
    };
    changed();
    let changed = Rc::new(changed);
    for r in rows.iter() {
        let changed = changed.clone();
        r.connect_changed(move |_| changed());
    }
    {
        let (cx, fields, values, cont) = (cx.clone(), fields.clone(), values.clone(), cont.clone());
        let (testing, result, error, name) = (testing.clone(), result.clone(), error.clone(), chosen.name.clone());
        test.connect_clicked(move |test| {
            let (settings, secrets) = providers::form_values(&fields, &values());
            let cmds = vec![cx.setup_args(&settings, &secrets), cli::args(&["check"])];
            test.set_sensitive(false);
            testing.set_visible(true);
            result.set_visible(false);
            error.set_visible(false);
            let (test, cont, testing, result, error, name) =
                (test.clone(), cont.clone(), testing.clone(), result.clone(), error.clone(), name.clone());
            cx.before_writing(move || {
                cli::run_all(cmds, move |res| {
                    testing.set_visible(false);
                    test.set_sensitive(true);
                    match res {
                        Ok(msg) => {
                            result.set_label(&test_ok_text(&msg, &name));
                            result.set_css_classes(&["success"]);
                            result.set_visible(true);
                            cont.set_sensitive(true);
                        }
                        Err(e) => show_error(&error, &format!("Couldn't connect: {e:#}")),
                    }
                })
            });
        });
    }
    {
        let (cx, error) = (cx.clone(), error.clone());
        cont.connect_clicked(move |_| cx.connected(&error));
    }
    ui::nav_page("Connect", &content, Some(&cont))
}

/// What a successful Test Connection says: `check`'s sentence, or one of its own.
fn test_ok_text(check: &str, name: &str) -> String {
    let check = check.trim();
    if check.is_empty() { format!("Connected to {name}.") } else { check.to_string() }
}

/// The provider's own section of config.toml (`[webdav] url = …`), to prefill its page.
/// Secrets are never in it.
fn saved_settings(id: &str) -> HashMap<String, String> {
    let text = std::fs::read_to_string(config::config_path()).unwrap_or_default();
    section(&text, id)
}

fn section(toml_text: &str, id: &str) -> HashMap<String, String> {
    let Ok(table) = toml_text.parse::<toml::Table>() else {
        return HashMap::new();
    };
    let Some(toml::Value::Table(t)) = table.get(id) else {
        return HashMap::new();
    };
    t.iter()
        .filter_map(|(k, v)| match v {
            toml::Value::String(s) => Some((k.clone(), s.clone())),
            toml::Value::Integer(n) => Some((k.clone(), n.to_string())),
            toml::Value::Boolean(b) => Some((k.clone(), b.to_string())),
            _ => None,
        })
        .collect()
}

fn done_page(
    win: &adw::ApplicationWindow,
    st: &Rc<RefCell<Wiz>>,
    on_finish: Rc<dyn Fn(bool)>,
) -> adw::NavigationPage {
    let content = ui::vbox(18);
    content.append(
        &gtk::Image::builder()
            .icon_name("clouddrop-idle-symbolic")
            .pixel_size(96)
            .css_classes(["success"])
            .build(),
    );
    content.append(&ui::label("You're all set", &["title-1"]));
    let hint = ui::label("", &["body"]);
    content.append(&hint);

    let status = adw::PreferencesGroup::new();
    let service_row = adw::ActionRow::builder()
        .title("Starting CloudDrop…")
        .subtitle("It runs in the background and starts when you log in.")
        .build();
    let service_icon = gtk::Spinner::builder().spinning(true).build();
    service_row.add_prefix(&service_icon);
    let retry = gtk::Button::builder()
        .label("Try Again")
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    service_row.add_suffix(&retry);
    status.add(&service_row);
    content.append(&status);

    let options = adw::PreferencesGroup::builder().title("Options").build();
    let tray = adw::SwitchRow::builder()
        .title("Show icon in the top bar")
        .subtitle("See upload status and pause or resume from anywhere.")
        .active(true)
        .build();
    options.add(&tray);
    if control::files_extension_installed() {
        let files_row = adw::ActionRow::builder()
            .title("Show upload status in Files")
            .subtitle("Files needs to restart once; its open windows will close.")
            .build();
        let restart = gtk::Button::builder()
            .label("Restart Files")
            .valign(gtk::Align::Center)
            .build();
        restart.connect_clicked(|b| {
            b.set_sensitive(false);
            let b = b.clone();
            rt::blocking(control::restart_files, move |res| {
                b.set_label(if res.is_ok() { "Restarted" } else { "Couldn't Restart" });
            });
        });
        files_row.add_suffix(&restart);
        options.add(&files_row);
    }
    content.append(&options);

    let open = ui::pill("Open CloudDrop Folder", false);
    content.append(&open);
    let finish = ui::pill("Finish", true);

    let start_service = {
        let (row, icon, retry) = (service_row.clone(), service_icon.clone(), retry.clone());
        move |adding: bool| {
            row.set_title("Starting CloudDrop…");
            icon.set_visible(true);
            retry.set_visible(false);
            let (row, icon, retry) = (row.clone(), icon.clone(), retry.clone());
            // The service runs every folder: one more is picked up without a restart.
            let start = move || if adding && control::daemon_running() { Ok(()) } else { control::enable_service() };
            rt::blocking(start, move |res| {
                icon.set_visible(false);
                match res {
                    Ok(()) => {
                        row.set_title("CloudDrop is running");
                        row.set_subtitle("It runs in the background and starts when you log in.");
                    }
                    Err(e) => {
                        let running = control::read_status(&clouddrop::config::data_dir())
                            .is_some_and(|s| s.is_live());
                        if running {
                            row.set_title("CloudDrop is running");
                            row.set_subtitle(&format!("It may not start by itself at login: {e:#}"));
                        } else {
                            row.set_title("CloudDrop couldn't start");
                            row.set_subtitle(&format!("{e:#}"));
                        }
                        retry.set_visible(true);
                    }
                }
            });
        }
    };
    {
        let (start_service, st) = (start_service.clone(), st.clone());
        retry.connect_clicked(move |_| start_service(st.borrow().adding));
    }

    let page = ui::nav_page("Done", &content, Some(&finish));
    page.set_can_pop(false);
    {
        let st = st.clone();
        let hint = hint.clone();
        page.connect_shown(move |_| {
            st.borrow_mut().finished = true;
            let w = st.borrow();
            hint.set_label(&format!(
                "Drop files into {} and they'll show up in {} → {}.",
                ui::pretty_path(&w.folder),
                w.chosen.root_label(),
                w.drive_folder
            ));
            if w.sidebar {
                let _ = clouddrop::actions::add_bookmark(&w.folder);
            }
            start_service(w.adding);
        });
    }
    {
        let st = st.clone();
        open.connect_clicked(move |_| control::open_uri(&ui::file_uri(&st.borrow().folder)));
    }
    {
        let win = win.clone();
        finish.connect_clicked(move |_| {
            on_finish(tray.is_active());
            win.close();
        });
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(id: &str) -> Choice {
        providers::fakes().into_iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn google_drive_pages_say_what_they_always_did() {
        let g = Choice::of(Provider::GoogleDrive);
        assert_eq!(
            signin_texts(&g),
            (
                "Sign in to Google".to_string(),
                "Your browser will open Google's sign-in page. Choose your account and allow CloudDrop to add files to your Drive."
                    .to_string(),
                "Sign In with Google".to_string()
            )
        );
        assert_eq!(
            cloud_folder_texts(&g),
            (
                "In Google Drive".to_string(),
                "Your files appear in this folder in My Drive, with the same subfolders.".to_string(),
                "Folder name in My Drive".to_string()
            )
        );
    }

    #[test]
    fn other_providers_are_named_on_their_pages() {
        let one = fake("onedrive");
        let (title, text, button) = signin_texts(&one);
        assert_eq!(title, "Sign in to OneDrive");
        assert_eq!(
            text,
            "Your browser will open OneDrive's sign-in page. Choose your account and allow CloudDrop to add files to its folder."
        );
        assert_eq!(button, "Sign In with OneDrive");
        assert_eq!(cloud_folder_texts(&one).2, "Folder name in OneDrive");
        let (title, text, button) = signin_texts(&fake("nextcloud"));
        assert_eq!((title.as_str(), button.as_str()), ("Sign in to Nextcloud", "Sign In"));
        assert!(text.starts_with("Enter your server's address."));
        let (title, text) = connect_texts(&fake("webdav"));
        assert_eq!(title, "Connect to WebDAV");
        assert_eq!(text, "Enter your WebDAV details. Passwords and keys are saved apart from the settings, readable only by you.");
    }

    #[test]
    fn the_provider_page_explains_switching() {
        assert_eq!(provider_intro(false, false, "Google Drive"), "Pick the cloud CloudDrop uploads to. You can switch later in Settings.");
        assert_eq!(
            provider_intro(true, false, "Google Drive"),
            "Everything in your CloudDrop folder is uploaded again to the place you pick. Nothing is deleted from Google Drive."
        );
        assert_eq!(
            provider_intro(false, true, "Google Drive"),
            "Pick the cloud another folder uploads to. Your other folders carry on as they are."
        );
    }

    #[test]
    fn a_new_folder_names_its_cloud_folder() {
        assert_eq!(cloud_folder_for(std::path::Path::new("/home/me/Work")), "Work");
        assert_eq!(cloud_folder_for(std::path::Path::new("/")), "CloudDrop");
    }

    #[test]
    fn test_connection_wording() {
        assert_eq!(test_ok_text("  Connected to nas.local as me.
", "WebDAV"), "Connected to nas.local as me.");
        assert_eq!(test_ok_text("", "WebDAV"), "Connected to WebDAV.");
    }

    #[test]
    fn saved_settings_come_from_the_providers_section() {
        let text = r#"sync_dir = "/home/me/CloudDrop"
provider = "webdav"

[webdav]
url = "https://nas/dav"
user = "me"
port = 443
"#;
        let s = section(text, "webdav");
        assert_eq!(s.get("url").map(String::as_str), Some("https://nas/dav"));
        assert_eq!(s.get("user").map(String::as_str), Some("me"));
        assert_eq!(s.get("port").map(String::as_str), Some("443"));
        assert!(section(text, "s3").is_empty());
        assert!(section("not = [toml", "webdav").is_empty());
        assert!(section(r#"webdav = "a string, not a section""#, "webdav").is_empty());
    }
}
