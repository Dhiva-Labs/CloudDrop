//! First-run assistant: Google key → folder → sign in → done.

use crate::{rt, signin, ui};
use adw::prelude::*;
use clouddrop::config::Config;
use clouddrop::control::{self, SetupParams};
use clouddrop::db::Db;
use std::cell::RefCell;
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

struct Wiz {
    client: Option<(String, String)>,
    folder: PathBuf,
    folder_locked: bool,
    drive_folder: String,
    sidebar: bool,
    login: Option<AbortHandle>,
    url: Rc<RefCell<Option<String>>>,
}

impl Wiz {
    fn new() -> Self {
        let cfg = Config::load().ok();
        let locked_root = Db::open_default()
            .ok()
            .and_then(|db| db.meta_get("sync_root").ok().flatten())
            .map(PathBuf::from);
        let defaults = Config::default();
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
        }
    }

    fn params(&self) -> SetupParams {
        SetupParams {
            client: self.client.clone(),
            sync_dir: self.folder.clone(),
            drive_folder: self.drive_folder.clone(),
        }
    }
}

/// Builds the assistant window. `on_finish(show_tray)` runs when the user clicks Finish.
pub fn build(app: &adw::Application, on_finish: impl Fn(bool) + 'static) -> adw::ApplicationWindow {
    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("Set Up CloudDrop")
        .default_width(620)
        .default_height(760)
        .build();
    let nav = adw::NavigationView::new();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&nav));
    win.set_content(Some(&toasts));

    let st = Rc::new(RefCell::new(Wiz::new()));
    let done = done_page(&win, &st, Rc::new(on_finish));
    let signin = signin_page(&nav, &st, &done);
    let folder = folder_page(&win, &nav, &st, &signin);
    let key = if control::builtin_client().is_none() {
        Some(key_page(&win, &nav, &st, &folder))
    } else {
        None
    };
    let next = key.clone().unwrap_or_else(|| folder.clone());
    nav.add(&welcome_page(&nav, &next));

    // Stop a pending browser sign-in when the assistant goes away.
    let st2 = st.clone();
    win.connect_close_request(move |_| {
        if let Some(h) = st2.borrow_mut().login.take() {
            h.abort();
        }
        gtk::glib::Propagation::Proceed
    });
    win
}

fn welcome_page(nav: &adw::NavigationView, next: &adw::NavigationPage) -> adw::NavigationPage {
    let content = ui::vbox(18);
    let icon = gtk::Image::builder()
        .icon_name(control::APP_ID)
        .pixel_size(128)
        .margin_top(12)
        .build();
    content.append(&icon);
    content.append(&ui::label("Welcome to CloudDrop", &["title-1"]));
    content.append(&ui::label(
        "Anything you put in your CloudDrop folder is uploaded to Google Drive. \
         Remove it from this computer whenever you like — it stays in Drive.",
        &["body"],
    ));
    let points = adw::PreferencesGroup::new();
    for (icon, title, sub) in [
        ("folder-symbolic", "Keeps your folders", "The same folder structure appears in My Drive."),
        ("clouddrop-syncing-symbolic", "Uploads by itself", "New and changed files go up in the background."),
        ("user-trash-symbolic", "Never deletes in Drive", "Free up space here without losing anything."),
    ] {
        let row = adw::ActionRow::builder().title(title).subtitle(sub).build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        points.add(&row);
    }
    content.append(&points);
    let start = ui::pill("Get Started", true);
    let (nav, next) = (nav.clone(), next.clone());
    start.connect_clicked(move |_| nav.push(&next));
    ui::nav_page("Welcome", &content, Some(&start))
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

fn folder_page(
    win: &adw::ApplicationWindow,
    nav: &adw::NavigationView,
    st: &Rc<RefCell<Wiz>>,
    next: &adw::NavigationPage,
) -> adw::NavigationPage {
    let content = ui::vbox(24);
    content.append(&ui::label("Choose your folders", &["title-2"]));

    let local = adw::PreferencesGroup::builder().title("On this computer").build();
    let folder_row = adw::ActionRow::builder()
        .title("CloudDrop folder")
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

    let remote = adw::PreferencesGroup::builder()
        .title("In Google Drive")
        .description("Your files appear in this folder in My Drive, with the same subfolders.")
        .build();
    let drive_row = adw::EntryRow::builder().title("Folder name in My Drive").build();
    drive_row.set_text(&st.borrow().drive_folder);
    remote.add(&drive_row);
    content.append(&remote);

    {
        let (win, st, folder_row) = (win.clone(), st.clone(), folder_row.clone());
        change.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Choose the CloudDrop Folder")
                .modal(true)
                .initial_folder(&gtk::gio::File::for_path(gtk::glib::home_dir()))
                .build();
            let (st, folder_row) = (st.clone(), folder_row.clone());
            dialog.select_folder(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                if let Ok(f) = res
                    && let Some(p) = f.path()
                {
                    folder_row.set_subtitle(&ui::pretty_path(&p));
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
        let (nav, next, st) = (nav.clone(), next.clone(), st.clone());
        cont.connect_clicked(move |_| {
            let mut w = st.borrow_mut();
            w.drive_folder = drive_row.text().trim().to_string();
            w.sidebar = sidebar.is_active();
            drop(w);
            nav.push(&next);
        });
    }
    ui::nav_page("Folders", &content, Some(&cont))
}

fn signin_page(
    nav: &adw::NavigationView,
    st: &Rc<RefCell<Wiz>>,
    next: &adw::NavigationPage,
) -> adw::NavigationPage {
    let content = ui::vbox(18);
    content.append(
        &gtk::Image::builder()
            .icon_name(control::APP_ID)
            .pixel_size(96)
            .build(),
    );
    content.append(&ui::label("Sign in to Google", &["title-2"]));
    content.append(&ui::label(
        "Your browser will open Google's sign-in page. Choose your account and allow \
         CloudDrop to add files to your Drive.",
        &["dim-label"],
    ));
    if control::builtin_client().is_none() {
        content.append(&ui::label(
            "If Google says the app isn't verified, that's expected for your own key: \
             choose Advanced → Go to (your app name).",
            &["dim-label", "caption"],
        ));
    }
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .visible(false)
        .build();
    let waiting = ui::label("Waiting for you to finish in the browser…", &["dim-label"]);
    waiting.set_visible(false);
    let copy = signin::copy_link_button(&st.borrow().url);
    copy.set_visible(false);
    let error = ui::label("", &["error"]);
    error.set_visible(false);
    let go = ui::pill("Sign In with Google", true);
    let keep = gtk::Button::builder()
        .label("Keep Current Sign-in")
        .halign(gtk::Align::Center)
        .css_classes(["flat"])
        .visible(control::is_signed_in())
        .build();
    for w in [spinner.upcast_ref::<gtk::Widget>(), waiting.upcast_ref(), copy.upcast_ref(), error.upcast_ref()] {
        content.append(w);
    }
    content.append(&go);
    content.append(&keep);

    {
        let (nav, next, st) = (nav.clone(), next.clone(), st.clone());
        let error = error.clone();
        keep.connect_clicked(move |_| match control::apply_setup(&st.borrow().params()) {
            Ok(_) => nav.push(&next),
            Err(e) => {
                error.set_label(&format!("{e:#}"));
                error.set_visible(true);
            }
        });
    }
    {
        let (nav, next, st) = (nav.clone(), next.clone(), st.clone());
        let (spinner, waiting, copy, error) = (spinner.clone(), waiting.clone(), copy.clone(), error.clone());
        go.connect_clicked(move |go| {
            error.set_visible(false);
            let cfg = match control::apply_setup(&st.borrow().params()) {
                Ok(cfg) => cfg,
                Err(e) => {
                    error.set_label(&format!("{e:#}"));
                    error.set_visible(true);
                    return;
                }
            };
            if let Some(h) = st.borrow_mut().login.take() {
                h.abort();
            }
            go.set_label("Open Sign-in Again");
            spinner.set_visible(true);
            waiting.set_visible(true);
            copy.set_visible(true);
            let url = st.borrow().url.clone();
            let copy2 = copy.clone();
            let handle = signin::start(
                cfg,
                move |u| {
                    *url.borrow_mut() = Some(u);
                    copy2.set_sensitive(true);
                },
                {
                    let (nav, next, st) = (nav.clone(), next.clone(), st.clone());
                    let (spinner, waiting, error, go) = (spinner.clone(), waiting.clone(), error.clone(), go.clone());
                    move |res| {
                        st.borrow_mut().login = None;
                        spinner.set_visible(false);
                        waiting.set_visible(false);
                        go.set_label("Sign In with Google");
                        match res {
                            Ok(()) => nav.push(&next),
                            Err(e) => {
                                error.set_label(&format!("Sign-in didn't finish: {e:#}"));
                                error.set_visible(true);
                            }
                        }
                    }
                },
            );
            st.borrow_mut().login = Some(handle);
        });
    }
    ui::nav_page("Sign In", &content, None)
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
        move || {
            row.set_title("Starting CloudDrop…");
            icon.set_visible(true);
            retry.set_visible(false);
            let (row, icon, retry) = (row.clone(), icon.clone(), retry.clone());
            rt::blocking(control::enable_service, move |res| {
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
        let start_service = start_service.clone();
        retry.connect_clicked(move |_| start_service());
    }

    let page = ui::nav_page("Done", &content, Some(&finish));
    page.set_can_pop(false);
    {
        let st = st.clone();
        let hint = hint.clone();
        page.connect_shown(move |_| {
            let w = st.borrow();
            hint.set_label(&format!(
                "Drop files into {} and they'll show up in My Drive → {}.",
                ui::pretty_path(&w.folder),
                w.drive_folder
            ));
            if w.sidebar {
                let _ = clouddrop::actions::add_bookmark(&w.folder);
            }
            start_service();
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
