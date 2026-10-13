//! Table cells for the Files tab: icon and name, status, size and date. Cells refill
//! themselves when their row changes in place.

use crate::fmt::{self, Civil, Status};
use crate::fsops;
use crate::model::{ItemObj, item_date_ms, item_name};
use crate::ui;
use adw::prelude::*;
use clouddrop::db::{DirItem, Kind, State};
use clouddrop::util::{self, human_bytes};
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

struct Slot<S> {
    state: Rc<S>,
    bound: Option<(ItemObj, glib::SignalHandlerId)>,
}

/// A factory whose cells refill themselves when their item changes in place.
fn cell_factory<S: 'static>(
    build: impl Fn() -> (gtk::Widget, S) + 'static,
    fill: impl Fn(&S, &DirItem) + 'static,
    on_setup: impl Fn(&gtk::ListItem, &gtk::Widget) + 'static,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let slots: Rc<RefCell<HashMap<gtk::ListItem, Slot<S>>>> = Rc::default();
    let fill = Rc::new(fill);

    {
        let slots = slots.clone();
        factory.connect_setup(move |_, obj| {
            let Some(li) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let (widget, state) = build();
            on_setup(li, &widget);
            li.set_child(Some(&widget));
            slots.borrow_mut().insert(li.clone(), Slot { state: Rc::new(state), bound: None });
        });
    }
    {
        let (slots, fill) = (slots.clone(), fill.clone());
        factory.connect_bind(move |_, obj| {
            let Some(li) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(item) = li.item().and_downcast::<ItemObj>() else {
                return;
            };
            let mut map = slots.borrow_mut();
            let Some(slot) = map.get_mut(li) else {
                return;
            };
            fill(&slot.state, &item.item());
            let (state, fill) = (Rc::downgrade(&slot.state), fill.clone());
            let id = item.connect_changed(move |o| {
                if let Some(state) = state.upgrade() {
                    fill(&state, &o.item());
                }
            });
            slot.bound = Some((item, id));
        });
    }
    {
        let slots = slots.clone();
        factory.connect_unbind(move |_, obj| {
            let Some(li) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if let Some(slot) = slots.borrow_mut().get_mut(li)
                && let Some((item, id)) = slot.bound.take()
            {
                item.disconnect(id);
            }
        });
    }
    factory.connect_teardown(move |_, obj| {
        if let Some(li) = obj.downcast_ref::<gtk::ListItem>() {
            slots.borrow_mut().remove(li);
        }
    });
    factory
}

/// What cells need to know beyond their own item.
pub struct ViewCtx {
    /// Rows come from several folders, so show where each one lives.
    pub flat: Cell<bool>,
    pub keep: RefCell<Vec<String>>,
    pub now: Cell<Civil>,
}

struct NameCell {
    icon: gtk::Image,
    name: gtk::Label,
    sub: gtk::Label,
    pin: gtk::Image,
}

pub fn name_factory(ctx: Rc<ViewCtx>, on_setup: impl Fn(&gtk::ListItem, &gtk::Widget) + 'static) -> gtk::SignalListItemFactory {
    cell_factory(
        || {
            let row = gtk::Box::builder().spacing(10).margin_start(4).margin_end(4).build();
            let icon = gtk::Image::builder().pixel_size(24).build();
            let text = ui::vbox(0);
            text.set_valign(gtk::Align::Center);
            text.set_hexpand(true);
            let name = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            let pin = gtk::Image::builder()
                .icon_name("view-pin-symbolic")
                .pixel_size(14)
                .tooltip_text("Always kept on this computer")
                .css_classes(["dim-label"])
                .visible(false)
                .build();
            let title = gtk::Box::builder().spacing(6).build();
            title.append(&name);
            title.append(&pin);
            let sub = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Start)
                .css_classes(["caption", "dim-label"])
                .visible(false)
                .build();
            text.append(&title);
            text.append(&sub);
            row.append(&icon);
            row.append(&text);
            (row.upcast(), NameCell { icon, name, sub, pin })
        },
        move |c: &NameCell, it: &DirItem| {
            let e = &it.entry;
            let is_dir = e.kind == Kind::Dir;
            let name = item_name(it);
            c.icon.set_from_gicon(&ui::file_icon(name, is_dir));
            c.icon.set_opacity(if e.state == State::CloudOnly { 0.55 } else { 1.0 });
            c.name.set_label(name);
            c.name.set_tooltip_text(Some(name));
            if e.state == State::CloudOnly {
                c.name.add_css_class("dim-label");
            } else {
                c.name.remove_css_class("dim-label");
            }
            let flat = ctx.flat.get();
            if flat {
                let parent = util::parent_of(&e.path);
                c.sub.set_label(if parent.is_empty() { "CloudDrop" } else { parent });
            }
            c.sub.set_visible(flat);
            c.pin.set_visible(is_dir && fsops::covered(&ctx.keep.borrow(), &e.path));
        },
        on_setup,
    )
}

pub fn status_factory(on_setup: impl Fn(&gtk::ListItem, &gtk::Widget) + 'static) -> gtk::SignalListItemFactory {
    cell_factory(
        || {
            let l = gtk::Label::builder()
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::Center)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .margin_start(4)
                .build();
            (l.clone().upcast(), l)
        },
        |l: &gtk::Label, it: &DirItem| {
            let e = &it.entry;
            l.set_tooltip_text(None);
            match (&it.stats, e.kind) {
                (Some(s), Kind::Dir) if e.state != State::Error => {
                    l.set_label(&fmt::folder_summary(s));
                    let tone = match fmt::folder_status(s) {
                        Some(Status::Failed) => "status-text-fail",
                        Some(_) => "status-text-wait",
                        None => "dim-label",
                    };
                    l.set_css_classes(&[tone]);
                }
                _ => {
                    let st = Status::of(e.state);
                    l.set_label(st.label());
                    l.set_css_classes(&["status-pill", st.css()]);
                    if e.state == State::Error || e.error.is_some() {
                        l.set_tooltip_text(e.error.as_deref());
                    }
                }
            }
        },
        on_setup,
    )
}

pub fn size_factory(on_setup: impl Fn(&gtk::ListItem, &gtk::Widget) + 'static) -> gtk::SignalListItemFactory {
    cell_factory(
        || {
            let l = gtk::Label::builder().xalign(0.0).css_classes(["numeric"]).build();
            (l.clone().upcast(), l)
        },
        |l: &gtk::Label, it: &DirItem| {
            let empty_dir = it.stats.is_some_and(|s| s.files == 0);
            l.set_label(&if empty_dir { "—".to_string() } else { human_bytes(it.bytes.max(0) as u64) });
        },
        on_setup,
    )
}

pub fn date_factory(ctx: Rc<ViewCtx>, on_setup: impl Fn(&gtk::ListItem, &gtk::Widget) + 'static) -> gtk::SignalListItemFactory {
    cell_factory(
        || {
            let l = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
            (l.clone().upcast(), l)
        },
        move |l: &gtk::Label, it: &DirItem| match item_date_ms(&it.entry) {
            Some(ms) => {
                l.set_label(&fmt::short_when(fmt::civil_from_ms(ms), ctx.now.get()));
                l.set_tooltip_text(it.entry.uploaded_at.map(|u| {
                    let c = fmt::civil_from_ms(u);
                    format!("Uploaded {} {}", fmt::short_when(c, ctx.now.get()), if c.days() == ctx.now.get().days() { String::new() } else { c.clock() })
                    .trim()
                    .to_string()
                }).as_deref());
            }
            None => {
                l.set_label("—");
                l.set_tooltip_text(None);
            }
        },
        on_setup,
    )
}
