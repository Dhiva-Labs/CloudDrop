//! Browser sign-in, run off the main thread: Google's own flow in-process, and
//! `clouddrop login --no-browser` for the other providers that sign in in the browser.

use crate::providers::{Choice, SignInKind};
use crate::{cli, rt};
use adw::prelude::*;
use clouddrop::config::Config;
use clouddrop::control;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tokio::task::AbortHandle;

/// Opens the browser for sign-in. `url_ready` receives the link (for "Copy Link").
pub fn start(
    cfg: Config,
    url_ready: impl Fn(String) + 'static,
    done: impl FnOnce(anyhow::Result<()>) + 'static,
) -> AbortHandle {
    let (tx, rx) = async_channel::unbounded::<String>();
    glib::spawn_future_local(async move {
        while let Ok(url) = rx.recv().await {
            url_ready(url);
        }
    });
    rt::run(
        async move {
            clouddrop::auth::login(&cfg, &clouddrop::auth::token_path(), move |url| {
                control::open_uri(url);
                let _ = tx.try_send(url.to_string());
            })
            .await
        },
        done,
    )
}

/// The browser sign-in of a provider other than Google Drive (OAuth, Nextcloud's Login Flow):
/// opens the link `clouddrop login --no-browser` prints. Aborting the handle stops it.
pub fn start_cli(
    url_ready: impl Fn(String) + 'static,
    done: impl FnOnce(anyhow::Result<()>) + 'static,
) -> AbortHandle {
    cli::login(
        move |url| {
            control::open_uri(&url);
            url_ready(url);
        },
        done,
    )
}

/// The sign-in dialog's heading and text for a provider.
pub fn dialog_text(c: &Choice) -> (String, String) {
    match c.sign_in {
        SignInKind::Google => (
            "Sign in to Google".into(),
            "Your browser opened Google's sign-in page. Choose your account and allow CloudDrop, then come back here.".into(),
        ),
        SignInKind::Nextcloud => (
            format!("Sign in to {}", c.name),
            "Your browser opened your server's login page. Log in and allow CloudDrop, then come back here.".into(),
        ),
        _ => (
            format!("Sign in to {}", c.name),
            format!(
                "Your browser opened {}'s sign-in page. Choose your account and allow CloudDrop, then come back here.",
                c.name
            ),
        ),
    }
}

/// Copy-link button that becomes active once the sign-in URL is known.
pub fn copy_link_button(url: &Rc<RefCell<Option<String>>>) -> gtk::Button {
    let b = gtk::Button::builder()
        .label("Copy Sign-in Link")
        .halign(gtk::Align::Center)
        .sensitive(false)
        .css_classes(["flat"])
        .tooltip_text("If the browser didn't open, paste this link into it")
        .build();
    let url = url.clone();
    b.connect_clicked(move |b| {
        if let Some(u) = url.borrow().as_ref() {
            b.clipboard().set_text(u);
            b.set_label("Link Copied");
        }
    });
    b
}

/// Sign-in dialog for an already set-up app (expired or revoked sign-in). Form providers have
/// no browser sign-in; the app opens the setup assistant's Connect step for them instead.
pub fn dialog(parent: &impl IsA<gtk::Widget>, on_done: impl Fn() + 'static) {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            let d = adw::AlertDialog::new(Some("CloudDrop isn't set up"), Some(&format!("{e:#}")));
            d.add_response("ok", "OK");
            d.present(Some(parent));
            return;
        }
    };
    let choice = Choice::current(Some(&cfg));
    let (heading, body) = dialog_text(&choice);
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
    let url: Rc<RefCell<Option<String>>> = Rc::default();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .build();
    let copy = copy_link_button(&url);
    let extra = crate::ui::vbox(12);
    extra.append(&spinner);
    extra.append(&copy);
    dialog.set_extra_child(Some(&extra));
    dialog.add_response("cancel", "Cancel");

    let finished = Rc::new(Cell::new(false));
    let url_ready = {
        let url = url.clone();
        let copy = copy.clone();
        move |u| {
            *url.borrow_mut() = Some(u);
            copy.set_sensitive(true);
        }
    };
    let done = {
        let dialog = dialog.clone();
        let finished = finished.clone();
        let spinner = spinner.clone();
        move |res: anyhow::Result<()>| {
            finished.set(true);
            match res {
                Ok(()) => {
                    dialog.close();
                    on_done();
                }
                Err(e) => {
                    spinner.set_visible(false);
                    dialog.set_heading(Some("Sign-in didn't finish"));
                    dialog.set_body(&format!("{e:#}"));
                }
            }
        }
    };
    let abort = if choice.is_google() { start(cfg, url_ready, done) } else { start_cli(url_ready, done) };
    dialog.connect_response(None, move |_, _| {
        if !finished.get() {
            abort.abort();
        }
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::fakes;
    use clouddrop::remote::Provider;

    #[test]
    fn dialog_wording_per_provider() {
        assert_eq!(
            dialog_text(&Choice::of(Provider::GoogleDrive)),
            (
                "Sign in to Google".to_string(),
                "Your browser opened Google's sign-in page. Choose your account and allow CloudDrop, then come back here."
                    .to_string()
            )
        );
        let f = fakes();
        let (h, b) = dialog_text(&f[0]);
        assert_eq!(h, "Sign in to OneDrive");
        assert_eq!(b, "Your browser opened OneDrive's sign-in page. Choose your account and allow CloudDrop, then come back here.");
        let (h, b) = dialog_text(&f[1]);
        assert_eq!(h, "Sign in to Nextcloud");
        assert_eq!(b, "Your browser opened your server's login page. Log in and allow CloudDrop, then come back here.");
    }
}
