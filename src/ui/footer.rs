use crate::providers::{ResultKind, SearchResult};
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Label, Orientation};
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) fn update_footer(
    results: &Rc<RefCell<Vec<SearchResult>>>,
    idx: usize,
    footer_action: &Label,
) {
    let item = results.borrow().get(idx).cloned();
    let label = match item.as_ref() {
        Some(i) if matches!(i.action, crate::providers::Action::SetQuery(_)) => "Use Scope",
        Some(i) => match i.kind {
            ResultKind::Calc | ResultKind::Conversion => "Copy Result",
            ResultKind::Command => "Open",
            // Files / folders / apps are also draggable (drag path out of the row).
            ResultKind::Folder | ResultKind::File | ResultKind::App => "Open · Drag",
        },
        None => "Open",
    };
    footer_action.set_text(label);
}

pub(crate) fn keycap_label(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("hark-keycap");
    l
}

/// Render a shortcut as one or more keycaps (e.g. "Ctrl B" → [Ctrl] [B]).
pub(crate) fn keycaps(keys: &str) -> GtkBox {
    let box_ = GtkBox::new(Orientation::Horizontal, 3);
    box_.set_valign(gtk::Align::Center);
    for part in keys.split_whitespace() {
        if part.is_empty() {
            continue;
        }
        box_.append(&keycap_label(part));
    }
    box_
}

pub(crate) fn footer_divider() -> Label {
    let l = Label::new(Some("│"));
    l.add_css_class("hark-footer-div");
    l
}

#[allow(dead_code)]
pub(crate) fn action_chip(label: &str, keys: &str) -> GtkBox {
    let box_ = GtkBox::new(Orientation::Horizontal, 6);
    box_.add_css_class("hark-action-chip");
    box_.set_valign(gtk::Align::Center);

    let name = Label::new(Some(label));
    name.add_css_class("hark-action-label");

    box_.append(&name);
    box_.append(&keycaps(keys));
    box_
}

pub(crate) fn action_chip_button(label: &str, keys: &str) -> Button {
    let box_ = GtkBox::new(Orientation::Horizontal, 6);
    box_.set_valign(gtk::Align::Center);

    let name = Label::new(Some(label));
    name.add_css_class("hark-action-label");
    box_.append(&name);
    box_.append(&keycaps(keys));

    let btn = Button::new();
    btn.add_css_class("hark-action-chip");
    btn.add_css_class("hark-action-btn");
    btn.set_has_frame(false);
    btn.set_child(Some(&box_));
    btn
}
