//! Small widget helpers shared by the setup assistant and the main window.

use adw::prelude::*;
use std::path::Path;

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

pub fn pill(text: &str, suggested: bool) -> gtk::Button {
    let b = gtk::Button::builder()
        .label(text)
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    if suggested {
        b.add_css_class("suggested-action");
    }
    b
}

pub fn flat_icon(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    b.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    b
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// A scrolling, width-limited page body with standard margins.
pub fn body(content: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(600)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .child(content)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build()
}

/// A navigation page with a header bar, scrolling body and an optional bottom button.
pub fn nav_page(title: &str, content: &impl IsA<gtk::Widget>, bottom: Option<&gtk::Button>) -> adw::NavigationPage {
    nav_page_with_header(title, content, bottom).0
}

/// [`nav_page`], with its header bar for extra buttons.
pub fn nav_page_with_header(
    title: &str,
    content: &impl IsA<gtk::Widget>,
    bottom: Option<&gtk::Button>,
) -> (adw::NavigationPage, adw::HeaderBar) {
    let header = adw::HeaderBar::new();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&body(content)));
    if let Some(b) = bottom {
        let bar = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .margin_top(12)
            .margin_bottom(18)
            .build();
        bar.append(b);
        tv.add_bottom_bar(&bar);
    }
    (adw::NavigationPage::builder().title(title).child(&tv).build(), header)
}

/// A left-aligned wrapping label (form hints, provider descriptions).
pub fn start_label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().label(text).wrap(true).xalign(0.0).build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

/// A row that opens a web page (used for the Google Cloud steps).
pub fn link_row(title: &str, subtitle: &str, url: &'static str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    let open = flat_icon("adw-external-link-symbolic", "Open in your browser");
    open.connect_clicked(move |_| clouddrop::control::open_uri(url));
    row.add_suffix(&open);
    row.set_activatable_widget(Some(&open));
    row
}

/// "~/CloudDrop" style display of a path.
pub fn pretty_path(p: &Path) -> String {
    if let Some(home) = dirs_home()
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    p.display().to_string()
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(Into::into)
}

pub fn file_uri(p: &Path) -> String {
    gtk::gio::File::for_path(p).uri().to_string()
}

pub fn toast(overlay: &adw::ToastOverlay, text: &str) {
    let t = adw::Toast::new(text);
    t.set_timeout(5);
    overlay.add_toast(t);
}

const CSS: &str = "
.status-pill { border-radius: 999px; padding: 1px 9px; font-size: 0.85em; font-weight: 600; }
.status-ok { color: @success_color; background-color: alpha(@success_color, 0.13); }
.status-cloud { color: alpha(@window_fg_color, 0.75); background-color: alpha(@window_fg_color, 0.09); }
.status-wait { color: @warning_color; background-color: alpha(@warning_color, 0.16); }
.status-busy { color: @accent_color; background-color: alpha(@accent_bg_color, 0.16); }
.status-fail { color: @error_color; background-color: alpha(@error_color, 0.14); }
.status-text-fail { color: @error_color; }
.status-text-wait { color: @warning_color; }
.drop-active { outline: 2px dashed @accent_bg_color; outline-offset: -8px; background-color: alpha(@accent_bg_color, 0.08); }
.tile { padding: 14px 10px; }
.tile image { color: @accent_color; }
.storage-card { padding: 16px; }
.storage-card progressbar trough, .storage-card progressbar progress { min-height: 8px; }
.storage-card progressbar.warning progress { background-color: @warning_color; }
.storage-card progressbar.critical progress { background-color: @error_color; }
.activity-icon { border-radius: 999px; padding: 8px; color: alpha(@window_fg_color, 0.75); background-color: alpha(@window_fg_color, 0.08); }
.activity-icon.ok { color: @success_color; background-color: alpha(@success_color, 0.13); }
.activity-icon.info { color: alpha(@window_fg_color, 0.75); background-color: alpha(@window_fg_color, 0.08); }
.activity-icon.warn { color: @error_color; background-color: alpha(@error_color, 0.14); }
.crumb { padding: 2px 6px; min-height: 24px; }
.hero { padding: 28px 24px 24px 24px; background-image: linear-gradient(135deg, alpha(#41d9ef, 0.16), alpha(#2f7cf0, 0.10), alpha(#4b3fd8, 0.16)); }
.hero-badge { border-radius: 999px; padding: 18px; }
.hero-badge.ok { color: @success_color; background-color: alpha(@success_color, 0.15); }
.hero-badge.busy { color: @accent_color; background-color: alpha(@accent_bg_color, 0.18); }
.hero-badge.paused { color: alpha(@window_fg_color, 0.7); background-color: alpha(@window_fg_color, 0.09); }
.hero-badge.warn { color: @warning_color; background-color: alpha(@warning_color, 0.16); }
.hero progressbar trough, .hero progressbar progress { min-height: 6px; }
.welcome-logo { padding: 16px 40px; background-image: radial-gradient(circle closest-side, alpha(#2f7cf0, 0.24), alpha(#2f7cf0, 0)); }
.feature-icon { border-radius: 999px; padding: 8px; color: @accent_color; background-color: alpha(@accent_bg_color, 0.16); }
.drop-zone { border: 2px dashed alpha(@window_fg_color, 0.18); border-radius: 18px; margin: 24px; }
columnview.files row { min-height: 40px; }
.provider-row { padding: 10px 12px; }
.connection-row { padding: 6px 8px; }
.own-folder { color: @success_color; }
";

/// Loads the app's small stylesheet (status pills, cards, the Home hero, drop zones).
#[allow(deprecated)]
pub fn install_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// Like `body` but with a custom maximum width.
pub fn body_wide(content: &impl IsA<gtk::Widget>, max: i32) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(max)
        .tightening_threshold(max - 100)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .child(content)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build()
}

/// Escapes text for Pango markup (file names may contain `&` or `<`).
pub fn esc(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

/// Content type icon for a file name, or the folder icon.
pub fn file_icon(name: &str, is_dir: bool) -> gtk::gio::Icon {
    if is_dir {
        return gtk::gio::ThemedIcon::new("folder").upcast();
    }
    let (ctype, _) = gtk::gio::content_type_guess(Some(name), None);
    gtk::gio::content_type_get_icon(&ctype)
}
