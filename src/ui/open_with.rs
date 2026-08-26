//! In-window Open With picker (layer-shell safe).
//!
//! External GTK/portal Open With dialogs often fail under exclusive keyboard
//! layer-shell, so we list GIO-compatible apps in a Hark popover instead.

use gio::prelude::*;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, GestureClick, Image, Label, ListBox, ListBoxRow, Orientation, Popover,
    PositionType, ScrolledWindow, Widget,
};
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Max apps shown (recommended first, then the rest).
const MAX_APPS: usize = 40;

/// Show a floating Open With list for `path`, parented to `anchor`.
///
/// Keeps the launcher open until the user picks an app or dismisses the popover.
pub fn show_open_with_picker(
    anchor: &impl IsA<Widget>,
    window: &gtk::ApplicationWindow,
    path: PathBuf,
    ignore_focus_loss: Rc<Cell<bool>>,
) {
    if !path.exists() {
        eprintln!("hark: open with: path missing: {}", path.display());
        return;
    }

    ignore_focus_loss.set(true);

    // ow-sync: the content-type probe (sync query_info) and app enumeration
    // (cold app-DB scan) jank the popover open — run both off-thread, open
    // the popover immediately with a loading row, and fill when ready.
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();

    let popover = Popover::new();
    popover.set_parent(anchor);
    popover.set_position(PositionType::Top);
    popover.set_autohide(true);
    popover.set_has_arrow(false);
    popover.add_css_class("hark-action-panel");
    popover.add_css_class("hark-open-with");

    let outer = GtkBox::new(Orientation::Vertical, 4);
    outer.add_css_class("hark-action-panel-inner");
    outer.set_margin_top(6);
    outer.set_margin_bottom(6);
    outer.set_margin_start(6);
    outer.set_margin_end(6);

    let header = Label::new(Some("Open With"));
    header.add_css_class("hark-action-panel-header");
    header.set_halign(Align::Start);
    header.set_margin_start(6);

    let sub = Label::new(Some(&file_name));
    sub.add_css_class("hark-action-panel-shortcut");
    sub.set_halign(Align::Start);
    sub.set_margin_start(6);
    sub.set_margin_bottom(4);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    sub.set_max_width_chars(36);

    let scroll = ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .max_content_height(280)
        .propagate_natural_height(true)
        .propagate_natural_width(true)
        .build();

    let list = ListBox::new();
    list.add_css_class("hark-action-panel-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);

    let apps_rc = Rc::new(std::cell::RefCell::new(Vec::<gio::AppInfo>::new()));

    let firing = Rc::new(Cell::new(false));
    // Weak capture: activate_row is stored in per-row GestureClick handlers,
    // so a strong popover here would cycle popover → row → handler → popover.
    let popover_w = popover.downgrade();
    let activate_row = {
        let apps_rc = apps_rc.clone();
        let path = path.clone();
        let window = window.clone();
        let popover_c = popover_w.clone();
        let ignore = ignore_focus_loss.clone();
        let firing = firing.clone();
        Rc::new(move |row: &ListBoxRow| {
            if firing.get() {
                return;
            }
            firing.set(true);
            let name = row.widget_name();
            if name.as_str() == "__system_default__" {
                let mut xdg = std::process::Command::new("xdg-open");
                xdg.arg(&path);
                match crate::providers::files::spawn_and_reap(xdg) {
                    Ok(_child) => {
                        if let Some(p) = popover_c.upgrade() {
                            p.popdown();
                        }
                        ignore.set(false);
                        window.set_visible(false);
                    }
                    Err(err) => {
                        // Surface the failure — keep the window open and reset
                        // the firing latch so the user can retry.
                        firing.set(false);
                        eprintln!("hark: xdg-open spawn failed: {err}");
                        if let Some(p) = popover_c.upgrade() {
                            p.popdown();
                        }
                    }
                }
                return;
            }

            let idx = row.index() as usize;
            let app = apps_rc.borrow().get(idx).cloned();
            if let Some(app) = app {
                let file = gio::File::for_path(&path);
                match app.launch(&[file], None::<&gio::AppLaunchContext>) {
                    Ok(()) => {
                        if let Some(p) = popover_c.upgrade() {
                            p.popdown();
                        }
                        ignore.set(false);
                        window.set_visible(false);
                    }
                    Err(err) => {
                        firing.set(false);
                        eprintln!("hark: open with launch failed: {}", err.message());
                    }
                }
            } else {
                firing.set(false);
            }
        })
    };

    // Loading placeholder row — replaced when the async enumeration lands.
    {
        let row = ListBoxRow::new();
        row.add_css_class("hark-action-panel-row");
        row.set_sensitive(false);
        row.set_widget_name("__loading__");
        let line = GtkBox::new(Orientation::Horizontal, 10);
        line.set_margin_top(8);
        line.set_margin_bottom(8);
        line.set_margin_start(8);
        line.set_margin_end(8);
        line.set_can_target(false);
        let label = Label::new(Some("Finding apps…"));
        label.add_css_class("hark-action-panel-label");
        label.set_can_target(false);
        line.append(&label);
        row.set_child(Some(&line));
        list.append(&row);
    }

    // Always offer xdg-open / system default as last resort.
    {
        let row = ListBoxRow::new();
        row.add_css_class("hark-action-panel-row");
        row.set_activatable(true);
        row.set_widget_name("__system_default__");

        let line = GtkBox::new(Orientation::Horizontal, 10);
        line.set_margin_top(6);
        line.set_margin_bottom(6);
        line.set_margin_start(8);
        line.set_margin_end(8);
        line.set_can_target(false);

        let icon = Image::from_icon_name("emblem-system-symbolic");
        icon.set_pixel_size(22);
        icon.set_can_target(false);
        line.append(&icon);

        let name = Label::new(Some("System default (xdg-open)"));
        name.add_css_class("hark-action-panel-label");
        name.set_halign(Align::Start);
        name.set_hexpand(true);
        name.set_can_target(false);
        line.append(&name);
        row.set_child(Some(&line));

        {
            let activate_row = activate_row.clone();
            let click = GestureClick::new();
            click.set_button(1);
            click.connect_released(move |gesture, n_press, _x, _y| {
                if n_press != 1 || gesture.current_button() != 1 {
                    return;
                }
                if let Some(widget) = gesture.widget() {
                    if let Ok(r) = widget.downcast::<ListBoxRow>() {
                        activate_row(&r);
                    }
                }
            });
            row.add_controller(click);
        }

        list.append(&row);
    }

    scroll.set_child(Some(&list));
    outer.append(&header);
    outer.append(&sub);
    outer.append(&scroll);
    popover.set_child(Some(&outer));

    {
        let activate_row = activate_row.clone();
        list.connect_row_activated(move |_, row| {
            activate_row(row);
        });
    }

    {
        let ignore = ignore_focus_loss.clone();
        // Weak self-capture: a strong one here is a reference cycle
        // (popover owns the signal handler that would own the popover).
        let popover_w = popover.downgrade();
        popover.connect_closed(move |_| {
            ignore.set(false);
            // Detach so the next Open With creates a clean popover.
            if let Some(p) = popover_w.upgrade() {
                p.unparent();
            }
        });
    }

    popover.popup();
    list.grab_focus();

    // ow-sync: enumerate off-thread, then fill the list on the main loop.
    {
        let probe_path = path.clone();
        let (tx, rx) = async_channel::bounded::<(String, Vec<(Option<String>, String)>)>(1);
        std::thread::spawn(move || {
            let content_type = content_type_for_path(&probe_path);
            let type_label = gio::content_type_get_description(&content_type).to_string();
            // gio::AppInfo is !Send — transfer (id, name) pairs and re-resolve
            // to DesktopAppInfo objects on the main thread.
            let apps: Vec<(Option<String>, String)> = apps_for_content_type(&content_type)
                .into_iter()
                .map(|app| (app.id().map(|s| s.to_string()), app.name().to_string()))
                .collect();
            let _ = tx.send_blocking((type_label, apps));
        });
        let list = list.clone();
        let apps_rc = apps_rc.clone();
        let sub = sub.clone();
        let activate_row = activate_row.clone();
        let popover_w = popover_w.clone();
        let file_name2 = file_name.clone();
        glib::spawn_future_local(async move {
            let Ok((type_label, apps)) = rx.recv().await else {
                return;
            };
            // Popover closed before results arrived — nothing to fill.
            let Some(popover) = popover_w.upgrade() else {
                return;
            };
            if !popover.is_visible() {
                return;
            }
            sub.set_text(&format!("{file_name2} · {type_label}"));
            // Drop the loading row.
            if let Some(row) = list
                .first_child()
                .and_then(|c| c.downcast::<ListBoxRow>().ok())
            {
                if row.widget_name().as_str() == "__loading__" {
                    list.remove(&row);
                }
            }
            if apps.is_empty() {
                let row = ListBoxRow::new();
                row.add_css_class("hark-action-panel-row");
                row.set_sensitive(false);
                let line = GtkBox::new(Orientation::Horizontal, 10);
                line.set_margin_top(8);
                line.set_margin_bottom(8);
                line.set_margin_start(8);
                line.set_margin_end(8);
                line.set_can_target(false);
                let label = Label::new(Some("No compatible apps found"));
                label.add_css_class("hark-action-panel-label");
                label.set_can_target(false);
                line.append(&label);
                row.set_child(Some(&line));
                list.prepend(&row);
                return;
            }
            let mut resolved: Vec<gio::AppInfo> = Vec::with_capacity(apps.len());
            for (id, name) in &apps {
                let app = match id.as_deref().and_then(gio::DesktopAppInfo::new) {
                    Some(d) => d.upcast::<gio::AppInfo>(),
                    None => {
                        // No desktop id — fall back to a commandline AppInfo.
                        match gio::AppInfo::create_from_commandline(
                            name,
                            Some(name.as_str()),
                            gio::AppInfoCreateFlags::NONE,
                        ) {
                            Ok(a) => a,
                            Err(e) => {
                                eprintln!(
                                    "hark: open with: cannot resolve {name}: {}",
                                    e.message()
                                );
                                continue;
                            }
                        }
                    }
                };
                resolved.push(app);

                let row = ListBoxRow::new();
                row.add_css_class("hark-action-panel-row");
                row.set_activatable(true);

                let line = GtkBox::new(Orientation::Horizontal, 10);
                line.set_margin_top(6);
                line.set_margin_bottom(6);
                line.set_margin_start(8);
                line.set_margin_end(8);
                line.set_can_target(false);

                let icon = if let Some(gicon) = resolved.last().and_then(|a| a.icon()) {
                    Image::from_gicon(&gicon)
                } else {
                    Image::from_icon_name("application-x-executable")
                };
                icon.set_pixel_size(22);
                icon.set_can_target(false);
                line.append(&icon);

                let texts = GtkBox::new(Orientation::Vertical, 0);
                texts.set_hexpand(true);
                texts.set_halign(Align::Start);
                texts.set_can_target(false);

                let name_l = Label::new(Some(name));
                name_l.add_css_class("hark-action-panel-label");
                name_l.set_halign(Align::Start);
                name_l.set_xalign(0.0);
                name_l.set_can_target(false);
                texts.append(&name_l);

                if let Some(id) = id.as_deref() {
                    let id_l = Label::new(Some(id));
                    id_l.add_css_class("hark-action-panel-shortcut");
                    id_l.set_halign(Align::Start);
                    id_l.set_xalign(0.0);
                    id_l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    id_l.set_max_width_chars(32);
                    id_l.set_can_target(false);
                    texts.append(&id_l);
                }

                line.append(&texts);
                row.set_child(Some(&line));

                {
                    let activate_row = activate_row.clone();
                    let click = GestureClick::new();
                    click.set_button(1);
                    click.connect_released(move |gesture, n_press, _x, _y| {
                        if n_press != 1 || gesture.current_button() != 1 {
                            return;
                        }
                        if let Some(widget) = gesture.widget() {
                            if let Ok(r) = widget.downcast::<ListBoxRow>() {
                                activate_row(&r);
                            }
                        }
                    });
                    row.add_controller(click);
                }

                list.insert(&row, -1);
            }
            *apps_rc.borrow_mut() = resolved;
            if let Some(row) = list.row_at_index(0) {
                list.select_row(Some(&row));
            }
        });
    }
}

fn content_type_for_path(path: &Path) -> String {
    if path.is_dir() {
        return "inode/directory".into();
    }
    let file = gio::File::for_path(path);
    if let Ok(info) = file.query_info(
        "standard::content-type",
        gio::FileQueryInfoFlags::NONE,
        None::<&gio::Cancellable>,
    ) {
        if let Some(ct) = info.content_type() {
            return ct.to_string();
        }
    }
    let (guess, _) = gio::content_type_guess(Some(path), &[]);
    guess.to_string()
}

fn apps_for_content_type(ctype: &str) -> Vec<gio::AppInfo> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    let push = |app: gio::AppInfo, seen: &mut HashSet<String>, out: &mut Vec<gio::AppInfo>| {
        let key = app
            .id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| app.name().to_string());
        if key.is_empty() {
            return;
        }
        if seen.insert(key) {
            out.push(app);
        }
    };

    for app in gio::AppInfo::recommended_for_type(ctype) {
        push(app, &mut seen, &mut out);
    }
    for app in gio::AppInfo::all_for_type(ctype) {
        push(app, &mut seen, &mut out);
    }

    // Some MIME types only have a default, not in all_for_type listings.
    if let Some(app) = gio::AppInfo::default_for_type(ctype, false) {
        push(app, &mut seen, &mut out);
    }

    out.truncate(MAX_APPS);
    out
}
