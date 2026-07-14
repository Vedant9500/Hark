use super::dnd::{attach_path_drag, DragSession};
use crate::providers::{ResultKind, SearchResult};
use gtk::prelude::*;
use gtk::{Box as GtkBox, Label, ListBoxRow, Orientation};

pub(crate) fn build_row(
    item: &SearchResult,
    _selected: bool,
    drag_session: &DragSession,
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

    let icon_name = item.icon.as_deref().unwrap_or(match item.kind {
        ResultKind::App => "application-x-executable",
        ResultKind::File => "text-x-generic",
        ResultKind::Folder => "folder",
        ResultKind::Calc | ResultKind::Conversion => "accessories-calculator",
        ResultKind::Command => "preferences-system",
    });
    // Resolve through the display icon theme so missing specific names fall back cleanly.
    let display = gtk::gdk::Display::default();
    let icon = if let Some(display) = display {
        let theme = gtk::IconTheme::for_display(&display);
        if theme.has_icon(icon_name) {
            gtk::Image::from_icon_name(icon_name)
        } else {
            // Fallbacks when a specific mime icon isn't installed.
            let fallback = match item.kind {
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
            };
            gtk::Image::from_icon_name(fallback)
        }
    } else {
        gtk::Image::from_icon_name(icon_name)
    };
    icon.add_css_class("blink-row-icon");
    icon.set_pixel_size(26);
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
