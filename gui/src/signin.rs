//! Browser sign-in to Google, run off the main thread.

use crate::rt;
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

/// Sign-in dialog for an already set-up app (expired or revoked sign-in).
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
    let dialog = adw::AlertDialog::new(
        Some("Sign in to Google"),
        Some("Your browser opened Google's sign-in page. Choose your account and allow CloudDrop, then come back here."),
    );
    let url: Rc<RefCell<Option<String>>> = Rc::default();
    let spinner = adw::Spinner::builder().width_request(32).height_request(32).build();
    let copy = copy_link_button(&url);
    let extra = crate::ui::vbox(12);
    extra.append(&spinner);
    extra.append(&copy);
    dialog.set_extra_child(Some(&extra));
    dialog.add_response("cancel", "Cancel");

    let finished = Rc::new(Cell::new(false));
    let abort = start(
        cfg,
        {
            let url = url.clone();
            let copy = copy.clone();
            move |u| {
                *url.borrow_mut() = Some(u);
                copy.set_sensitive(true);
            }
        },
        {
            let dialog = dialog.clone();
            let finished = finished.clone();
            let spinner = spinner.clone();
            move |res| {
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
        },
    );
    dialog.connect_response(None, move |_, _| {
        if !finished.get() {
            abort.abort();
        }
    });
    dialog.present(Some(parent));
}
