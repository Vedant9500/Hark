use super::dnd::{attach_path_drag, DragSession};
use crate::providers::{ResultKind, SearchResult};
use gtk::prelude::*;
use gtk::{Box as GtkBox, Label, ListBoxRow, Orientation};
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    /// Resolved icon name cache for this GTK thread (main).
    /// Key: "{symbolic_flag}\0{requested_name}\0{kind_u8}"
    static ICON_RESOLVE_CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Drop cached icon resolutions (call if icon theme / symbolic preference changes).
#[allow(dead_code)]
pub(crate) fn clear_icon_resolve_cache() {
    ICON_RESOLVE_CACHE.with(|c| c.borrow_mut().clear());
}

pub(crate) fn build_row(
    item: &SearchResult,
    _selected: bool,
    drag_session: &DragSession,
    icon_size: i32,
    symbolic_icons: bool,
) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_activatable(true);

    if let Some(conv) = &item.conversion {
        row.add_css_class("blink-conv-row");
        row.set_child(Some(&build_conversion_card(conv, item.kind)));
        return row;
    }

    // Files, folders, and apps (via .desktop path) can be dragged out.
    if let Some(path) = item.action.drag_path() {
        attach_path_drag(&row, path, drag_session);
    }

    let hbox = GtkBox::new(Orientation::Horizontal, 10);
    hbox.add_css_class("blink-row-inner");
    hbox.set_margin_start(2);
    hbox.set_margin_end(2);

    let requested = item.icon.as_deref().unwrap_or(default_icon_for_kind(item.kind));
    let resolved = resolve_row_icon(requested, item.kind, symbolic_icons);
    let icon = gtk::Image::from_icon_name(&resolved);
    icon.add_css_class("blink-row-icon");
    icon.set_pixel_size(icon_size.clamp(18, 36));
    icon.set_valign(gtk::Align::Center);
    icon.set_opacity(1.0);

    let text = GtkBox::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);

    let title = Label::new(Some(&item.title));
    title.add_css_class("blink-title");
    title.set_halign(gtk::Align::Start);
    title.set_valign(gtk::Align::Center);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    title.set_wrap(false);
    title.set_single_line_mode(true);

    let subtitle = Label::new(Some(&item.subtitle));
    subtitle.add_css_class("blink-subtitle");
    subtitle.set_halign(gtk::Align::Start);
    subtitle.set_valign(gtk::Align::Center);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(false);
    subtitle.set_single_line_mode(true);

    text.append(&title);
    text.append(&subtitle);

    let badge = Label::new(Some(kind_label(item.kind)));
    badge.add_css_class("blink-badge");
    badge.set_valign(gtk::Align::Center);

    hbox.append(&icon);
    hbox.append(&text);
    hbox.append(&badge);
    row.set_child(Some(&hbox));
    row
}



fn default_icon_for_kind(kind: ResultKind) -> &'static str {
    match kind {
        ResultKind::App => "application-x-executable",
        ResultKind::File => "text-x-generic",
        ResultKind::Folder => "folder",
        ResultKind::Calc | ResultKind::Conversion => "accessories-calculator",
        ResultKind::Command => "preferences-system",
    }
}

fn fallback_icon(kind: ResultKind, icon_name: &str) -> &'static str {
    match kind {
        ResultKind::App => "application-x-executable",
        ResultKind::Folder => "folder",
        ResultKind::File => {
            if icon_name.starts_with("image-") {
                "image-x-generic"
            } else if icon_name.starts_with("video-") {
                "video-x-generic"
            } else if icon_name.starts_with("audio-") {
                "audio-x-generic"
            } else if icon_name.contains("pdf") {
                "application-pdf"
            } else if icon_name.contains("zip")
                || icon_name.contains("tar")
                || icon_name.contains("gzip")
                || icon_name.contains("package")
            {
                "package-x-generic"
            } else if icon_name.starts_with("text-x-") || icon_name.starts_with("text-") {
                "text-x-script"
            } else {
                "text-x-generic"
            }
        }
        ResultKind::Calc | ResultKind::Conversion => "accessories-calculator",
        ResultKind::Command => "preferences-system",
    }
}

/// Resolve icon once per unique (name, kind, symbolic) for this process/thread.
fn resolve_row_icon(requested: &str, kind: ResultKind, symbolic_icons: bool) -> String {
    let key = format!("{}\0{}\0{}", symbolic_icons as u8, requested, kind_key(kind));
    ICON_RESOLVE_CACHE.with(|cache| {
        if let Some(hit) = cache.borrow().get(&key) {
            return hit.clone();
        }
        let resolved = resolve_row_icon_uncached(requested, kind, symbolic_icons);
        cache.borrow_mut().insert(key, resolved.clone());
        // Soft cap so a long session with wild mime names cannot grow forever.
        if cache.borrow().len() > 512 {
            cache.borrow_mut().clear();
            cache.borrow_mut().insert(
                format!("{}\0{}\0{}", symbolic_icons as u8, requested, kind_key(kind)),
                resolved.clone(),
            );
        }
        resolved
    })
}

fn resolve_row_icon_uncached(requested: &str, kind: ResultKind, symbolic_icons: bool) -> String {
    let mut icon_name = requested.to_string();
    let Some(display) = gtk::gdk::Display::default() else {
        return icon_name;
    };
    let theme = gtk::IconTheme::for_display(&display);

    if symbolic_icons && !icon_name.ends_with("-symbolic") {
        let candidate = format!("{icon_name}-symbolic");
        if theme.has_icon(&candidate) {
            icon_name = candidate;
        }
    }

    if theme.has_icon(&icon_name) {
        icon_name
    } else {
        fallback_icon(kind, &icon_name).to_string()
    }
}


fn kind_key(kind: ResultKind) -> u8 {
    match kind {
        ResultKind::App => 0,
        ResultKind::File => 1,
        ResultKind::Folder => 2,
        ResultKind::Calc => 3,
        ResultKind::Conversion => 4,
        ResultKind::Command => 5,
    }
}
pub(crate) fn build_conversion_card(
    conv: &crate::providers::ConversionView,
    kind: ResultKind,
) -> GtkBox {
    let outer = GtkBox::new(Orientation::Vertical, 6);
    outer.add_css_class("blink-conv-card");
    outer.set_hexpand(true);

    let header = Label::new(Some(kind_label(kind)));
    header.add_css_class("blink-conv-header");
    header.set_halign(gtk::Align::Start);
    outer.append(&header);

    let panels = GtkBox::new(Orientation::Horizontal, 0);
    panels.add_css_class("blink-conv-panels");
    panels.set_hexpand(true);

    let left = conv_panel(&conv.left_title, &conv.left_badge, true);
    let arrow = Label::new(Some("→"));
    arrow.add_css_class("blink-conv-arrow");
    arrow.set_valign(gtk::Align::Center);
    arrow.set_halign(gtk::Align::Center);
    let right = conv_panel(&conv.right_title, &conv.right_badge, false);

    panels.append(&left);
    panels.append(&arrow);
    panels.append(&right);
    outer.append(&panels);
    outer
}


fn conv_panel(title: &str, badge: &str, is_left: bool) -> GtkBox {
    let col = GtkBox::new(Orientation::Vertical, 8);
    col.add_css_class("blink-conv-panel");
    if is_left {
        col.add_css_class("blink-conv-left");
    } else {
        col.add_css_class("blink-conv-right");
    }
    col.set_hexpand(true);
    col.set_halign(gtk::Align::Fill);
    col.set_valign(gtk::Align::Center);

    let t = Label::new(Some(title));
    t.add_css_class("blink-conv-title");
    t.set_halign(if is_left {
        gtk::Align::Start
    } else {
        gtk::Align::End
    });
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    t.set_xalign(if is_left { 0.0 } else { 1.0 });

    let b = Label::new(Some(badge));
    b.add_css_class("blink-conv-badge");
    b.set_halign(if is_left {
        gtk::Align::Start
    } else {
        gtk::Align::End
    });
    b.set_ellipsize(gtk::pango::EllipsizeMode::End);

    col.append(&t);
    col.append(&b);
    col
}


pub(crate) fn kind_label(kind: ResultKind) -> &'static str {
    match kind {
        ResultKind::App => "Application",
        ResultKind::File => "File",
        ResultKind::Folder => "Folder",
        ResultKind::Calc => "Calculator",
        ResultKind::Conversion => "Conversion",
        ResultKind::Command => "Command",
    }
}
