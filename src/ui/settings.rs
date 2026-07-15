use crate::config::{discover_mounts, FileOpenCategory, PathStyle, UiThemeConfig};
use crate::engine::Engine;
use crate::theme::ThemeManager;
use gtk::gdk::Key;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, CheckButton, Entry, EventControllerKey, Image, Label, ListBox,
    ListBoxRow, Orientation, ScrolledWindow, Separator,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

struct Category {
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    icon: &'static str,
}

const CATEGORIES: &[Category] = &[
    Category {
        id: "indexing",
        title: "Indexing",
        subtitle: "Sources, depth & rebuild",
        icon: "folder-saved-search-symbolic",
    },
    Category {
        id: "folders",
        title: "Extra folders",
        subtitle: "Additional search roots",
        icon: "folder-symbolic",
    },
    Category {
        id: "exclusions",
        title: "Exclusions",
        subtitle: "Names always skipped",
        icon: "edit-delete-symbolic",
    },
    Category {
        id: "defaults",
        title: "Default apps",
        subtitle: "Open files with…",
        icon: "preferences-desktop-default-applications-symbolic",
    },
    Category {
        id: "display",
        title: "Display",
        subtitle: "How paths are shown",
        icon: "preferences-desktop-display-symbolic",
    },
    Category {
        id: "appearance",
        title: "Appearance",
        subtitle: "Opacity, colours, icons, type",
        icon: "preferences-desktop-theme-symbolic",
    },
    Category {
        id: "tools",
        title: "Tools",
        subtitle: "Translation & extras",
        icon: "applications-utilities-symbolic",
    },
];

pub struct SettingsPanel {
    pub root: GtkBox,
    status: Label,
    pub nav: ListBox,
    engine: Arc<Engine>,
    #[allow(dead_code)]
    theme: Rc<ThemeManager>,
    on_done: Rc<RefCell<Option<Box<dyn Fn()>>>>,
    /// Closes in-panel overlays (e.g. default-app picker). Returns true if one was open.
    dismiss_overlay: Rc<RefCell<Option<Box<dyn Fn() -> bool>>>>,
}

impl SettingsPanel {
    pub fn new(engine: Arc<Engine>, theme: Rc<ThemeManager>) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("blink-settings");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_overflow(gtk::Overflow::Hidden);

        // Dual panel body (no bulky top chrome — Esc closes)
        let split = GtkBox::new(Orientation::Horizontal, 0);
        split.add_css_class("blink-settings-split");
        split.set_hexpand(true);
        split.set_vexpand(true);

        // --- Left nav ---
        let nav_col = GtkBox::new(Orientation::Vertical, 0);
        nav_col.add_css_class("blink-settings-nav-col");
        nav_col.set_hexpand(false);
        nav_col.set_vexpand(true);

        let search = Entry::builder()
            .placeholder_text("Search…")
            .hexpand(true)
            .build();
        search.add_css_class("blink-settings-search");
        search.set_primary_icon_name(Some("system-search-symbolic"));
        search.set_margin_start(10);
        search.set_margin_end(10);
        search.set_margin_top(12);
        search.set_margin_bottom(8);
        nav_col.append(&search);

        let nav_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(196)
            .max_content_width(220)
            .hexpand(false)
            .vexpand(true)
            .build();
        nav_scroll.add_css_class("blink-settings-nav-scroll");

        let nav = ListBox::new();
        nav.add_css_class("blink-settings-nav");
        nav.set_selection_mode(gtk::SelectionMode::Single);
        nav.set_hexpand(false);

        for (i, cat) in CATEGORIES.iter().enumerate() {
            let row = ListBoxRow::new();
            row.add_css_class("blink-settings-nav-row");
            row.set_selectable(true);

            let item = GtkBox::new(Orientation::Horizontal, 10);
            item.add_css_class("blink-settings-nav-item");
            item.set_margin_start(10);
            item.set_margin_end(10);
            item.set_margin_top(7);
            item.set_margin_bottom(7);
            item.set_valign(gtk::Align::Center);

            let icon = Image::from_icon_name(cat.icon);
            icon.add_css_class("blink-settings-nav-icon");
            icon.set_pixel_size(16);
            icon.set_valign(gtk::Align::Center);

            let name = Label::new(Some(cat.title));
            name.add_css_class("blink-settings-nav-title");
            name.set_halign(gtk::Align::Start);
            name.set_hexpand(true);
            name.set_xalign(0.0);

            item.append(&icon);
            item.append(&name);
            row.set_child(Some(&item));
            row.set_widget_name(cat.id);
            // Keep subtitle searchable via data attribute-ish name on row
            row.set_tooltip_text(Some(cat.subtitle));
            nav.append(&row);
            if i == 0 {
                nav.select_row(Some(&row));
            }
        }

        // Filter nav by search text
        {
            let nav = nav.clone();
            search.connect_changed(move |entry| {
                let q = entry.text().to_lowercase();
                let mut child = nav.first_child();
                while let Some(w) = child {
                    let next = w.next_sibling();
                    if let Ok(row) = w.downcast::<ListBoxRow>() {
                        let id = row.widget_name().to_string();
                        let title = CATEGORIES
                            .iter()
                            .find(|c| c.id == id)
                            .map(|c| c.title.to_lowercase())
                            .unwrap_or_default();
                        let sub = CATEGORIES
                            .iter()
                            .find(|c| c.id == id)
                            .map(|c| c.subtitle.to_lowercase())
                            .unwrap_or_default();
                        let visible =
                            q.is_empty() || title.contains(&q) || sub.contains(&q) || id.contains(&q);
                        row.set_visible(visible);
                    }
                    child = next;
                }
            });
        }

        nav_scroll.set_child(Some(&nav));
        nav_col.append(&nav_scroll);

        // Bottom close hint in nav
        let nav_footer = GtkBox::new(Orientation::Horizontal, 6);
        nav_footer.add_css_class("blink-settings-nav-footer");
        nav_footer.set_margin_start(12);
        nav_footer.set_margin_end(12);
        nav_footer.set_margin_top(6);
        nav_footer.set_margin_bottom(10);

        let esc = Label::new(Some("esc"));
        esc.add_css_class("blink-keycap");
        let close_hint = Label::new(Some("Close"));
        close_hint.add_css_class("blink-settings-nav-footer-label");
        close_hint.set_halign(gtk::Align::Start);
        close_hint.set_hexpand(true);

        let done = Button::with_label("Done");
        done.add_css_class("blink-settings-btn");
        done.add_css_class("blink-settings-done");
        done.set_halign(gtk::Align::End);

        nav_footer.append(&esc);
        nav_footer.append(&close_hint);
        nav_footer.append(&done);
        nav_col.append(&nav_footer);

        split.append(&nav_col);
        split.append(&Separator::new(Orientation::Vertical));

        // --- Right content stack ---
        let content_stack = gtk::Stack::new();
        content_stack.add_css_class("blink-settings-content-stack");
        content_stack.set_hexpand(true);
        content_stack.set_vexpand(true);
        content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        content_stack.set_transition_duration(100);

        let cfg = engine.config().get();

        let (indexing_page, status) = build_indexing_page(&engine, &cfg);
        content_stack.add_named(&indexing_page, Some("indexing"));

        let folders_page = build_folders_page(&engine);
        content_stack.add_named(&folders_page, Some("folders"));

        let exclusions_page = build_exclusions_page(&engine);
        content_stack.add_named(&exclusions_page, Some("exclusions"));

        let dismiss_overlay: Rc<RefCell<Option<Box<dyn Fn() -> bool>>>> =
            Rc::new(RefCell::new(None));

        let defaults_page = build_defaults_page(&engine, dismiss_overlay.clone());
        content_stack.add_named(&defaults_page, Some("defaults"));

        let display_page = build_display_page(&engine, &cfg);
        content_stack.add_named(&display_page, Some("display"));

        let appearance_page = build_appearance_page(&engine, &theme, &cfg);
        content_stack.add_named(&appearance_page, Some("appearance"));

        let tools_page = build_tools_page(&engine, &cfg);
        content_stack.add_named(&tools_page, Some("tools"));

        content_stack.set_visible_child_name("indexing");
        split.append(&content_stack);
        root.append(&split);

        {
            let content_stack = content_stack.clone();
            let dismiss_overlay = dismiss_overlay.clone();
            nav.connect_row_selected(move |_, row| {
                // Leaving Default apps closes any in-page picker.
                if let Some(cb) = dismiss_overlay.borrow().as_ref() {
                    let _ = cb();
                }
                if let Some(row) = row {
                    let id = row.widget_name();
                    if !id.is_empty() {
                        content_stack.set_visible_child_name(&id);
                    }
                }
            });
        }

        // Keyboard: ↑/↓ or j/k cycle categories; Home/End jump
        {
            let nav = nav.clone();
            let key = EventControllerKey::new();
            key.set_propagation_phase(gtk::PropagationPhase::Capture);
            key.connect_key_pressed(move |_, keyval, _, _| {
                let n = CATEGORIES.len() as i32;
                if n == 0 {
                    return glib::Propagation::Proceed;
                }
                let cur = nav
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(0)
                    .max(0);
                let next = match keyval {
                    Key::Down | Key::j | Key::J => Some((cur + 1) % n),
                    Key::Up | Key::k | Key::K => Some(if cur == 0 { n - 1 } else { cur - 1 }),
                    Key::Home => Some(0),
                    Key::End => Some(n - 1),
                    Key::Page_Down => Some((cur + 1).min(n - 1)),
                    Key::Page_Up => Some((cur - 1).max(0)),
                    _ => None,
                };
                if let Some(idx) = next {
                    if let Some(row) = nav.row_at_index(idx) {
                        if row.is_visible() {
                            nav.select_row(Some(&row));
                            row.grab_focus();
                        }
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            root.add_controller(key);
        }

        let on_done: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        {
            let on_done = on_done.clone();
            done.connect_clicked(move |_| {
                if let Some(cb) = on_done.borrow().as_ref() {
                    cb();
                }
            });
        }

        Self {
            root,
            status,
            nav,
            engine,
            theme,
            on_done,
            dismiss_overlay,
        }
    }

    pub fn set_on_done<F: Fn() + 'static>(&self, f: F) {
        *self.on_done.borrow_mut() = Some(Box::new(f));
    }

    /// Close nested settings UI (app picker, etc.). `true` if something was dismissed.
    pub fn dismiss_overlay(&self) -> bool {
        self.dismiss_overlay
            .borrow()
            .as_ref()
            .map(|cb| cb())
            .unwrap_or(false)
    }

    /// Cloneable handle for window-level key capture (Esc).
    pub fn dismiss_overlay_handle(&self) -> impl Fn() -> bool + 'static {
        let dismiss = self.dismiss_overlay.clone();
        move || {
            dismiss
                .borrow()
                .as_ref()
                .map(|cb| cb())
                .unwrap_or(false)
        }
    }

    pub fn refresh_status(&self) {
        self.status
            .set_text(&self.engine.format_index_status());
    }

    pub fn widget(&self) -> &GtkBox {
        &self.root
    }
}

fn page_shell(icon: &str, title: &str, subtitle: &str) -> (GtkBox, GtkBox) {
    let outer = GtkBox::new(Orientation::Vertical, 0);
    outer.add_css_class("blink-settings-page");
    outer.set_hexpand(true);
    outer.set_vexpand(true);

    // Sticky page header
    let header = GtkBox::new(Orientation::Horizontal, 10);
    header.add_css_class("blink-settings-page-header");
    header.set_margin_start(20);
    header.set_margin_end(20);
    header.set_margin_top(16);
    header.set_margin_bottom(4);

    let icon_w = Image::from_icon_name(icon);
    icon_w.add_css_class("blink-settings-page-icon");
    icon_w.set_pixel_size(18);
    icon_w.set_valign(gtk::Align::Center);

    let head_text = GtkBox::new(Orientation::Vertical, 2);
    head_text.set_hexpand(true);

    let t = Label::new(Some(title));
    t.add_css_class("blink-settings-page-title");
    t.set_halign(gtk::Align::Start);
    t.set_xalign(0.0);

    let s = Label::new(Some(subtitle));
    s.add_css_class("blink-settings-page-sub");
    s.set_halign(gtk::Align::Start);
    s.set_xalign(0.0);
    s.set_wrap(true);

    head_text.append(&t);
    head_text.append(&s);
    header.append(&icon_w);
    header.append(&head_text);
    outer.append(&header);

    let scroll = ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(280)
        .max_content_height(420)
        .propagate_natural_height(true)
        .hexpand(true)
        .vexpand(true)
        .build();

    let body = GtkBox::new(Orientation::Vertical, 14);
    body.add_css_class("blink-settings-body");
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(12);
    body.set_margin_bottom(18);

    scroll.set_child(Some(&body));
    outer.append(&scroll);
    (outer, body)
}

fn build_indexing_page(
    engine: &Arc<Engine>,
    cfg: &crate::config::BlinkConfig,
) -> (GtkBox, Label) {
    let (outer, body) = page_shell(
        "folder-saved-search-symbolic",
        "Indexing",
        "Choose which locations Blink searches and rebuild the file index.",
    );

    body.append(&group_label("Scan depth"));

    let depth_card = GtkBox::new(Orientation::Vertical, 0);
    depth_card.add_css_class("blink-settings-card");

    let depth_row = setting_row(
        "Levels from each root",
        Some(&depth_help_text(cfg.index.max_depth.clamp(1, 6))),
    );

    let stepper = GtkBox::new(Orientation::Horizontal, 4);
    stepper.set_valign(gtk::Align::Center);

    let depth_dec = Button::with_label("−");
    depth_dec.add_css_class("blink-settings-btn");
    depth_dec.add_css_class("blink-settings-icon-btn");
    depth_dec.set_tooltip_text(Some("Shallower (faster, fewer files)"));

    let depth_val = Label::new(Some(&format!("{}", cfg.index.max_depth.clamp(1, 6))));
    depth_val.add_css_class("blink-settings-stepper-val");
    depth_val.set_width_chars(2);
    depth_val.set_halign(gtk::Align::Center);

    let depth_inc = Button::with_label("+");
    depth_inc.add_css_class("blink-settings-btn");
    depth_inc.add_css_class("blink-settings-icon-btn");
    depth_inc.set_tooltip_text(Some("Deeper (slower, more files)"));

    stepper.append(&depth_dec);
    stepper.append(&depth_val);
    stepper.append(&depth_inc);
    depth_row.append(&stepper);
    depth_card.append(&depth_row);

    let caps = Label::new(Some(&format!(
        "Cap {} items · rebuild TTL 30m · skips .git, .venv, node_modules, …",
        crate::providers::files::MAX_INDEX
    )));
    caps.add_css_class("blink-hint");
    caps.add_css_class("blink-settings-card-footer");
    caps.set_halign(gtk::Align::Start);
    caps.set_wrap(true);
    depth_card.append(&Separator::new(Orientation::Horizontal));
    depth_card.append(&caps);
    body.append(&depth_card);

    // Wire depth buttons — update the row subtitle label
    let depth_hint = depth_row
        .first_child() // text col
        .and_then(|c| c.last_child()) // subtitle
        .and_then(|c| c.downcast::<Label>().ok());

    {
        let engine = engine.clone();
        let depth_val = depth_val.clone();
        let depth_hint = depth_hint.clone();
        depth_dec.connect_clicked(move |_| {
            let mut next = 2usize;
            engine.config().update(|c| {
                let d = c.index.max_depth.clamp(1, 6);
                next = d.saturating_sub(1).max(1);
                c.index.max_depth = next;
            });
            depth_val.set_text(&format!("{next}"));
            if let Some(h) = &depth_hint {
                h.set_text(&depth_help_text(next));
            }
            engine.force_reindex();
        });
    }
    {
        let engine = engine.clone();
        let depth_val = depth_val.clone();
        let depth_hint = depth_hint.clone();
        depth_inc.connect_clicked(move |_| {
            let mut next = 2usize;
            engine.config().update(|c| {
                let d = c.index.max_depth.clamp(1, 6);
                next = (d + 1).min(6);
                c.index.max_depth = next;
            });
            depth_val.set_text(&format!("{next}"));
            if let Some(h) = &depth_hint {
                h.set_text(&depth_help_text(next));
            }
            engine.force_reindex();
        });
    }

    body.append(&group_label("Sources"));

    let sources = GtkBox::new(Orientation::Vertical, 0);
    sources.add_css_class("blink-settings-card");

    let home_row = check_setting_row(
        "Home directory (~)",
        None,
        cfg.index.include_home,
    );
    {
        let engine = engine.clone();
        let cb = home_row.1.clone();
        cb.connect_toggled(move |btn| {
            engine.config().update(|c| {
                c.index.include_home = btn.is_active();
            });
        });
    }
    sources.append(&home_row.0);

    let mounts = discover_mounts();
    for (i, m) in mounts.iter().enumerate() {
        sources.append(&Separator::new(Orientation::Horizontal));
        let key = m.target.to_string_lossy().to_string();
        let label = if m.label.is_empty() {
            key.clone()
        } else {
            format!("{}  ({})", m.label, key)
        };
        let enabled = cfg
            .index
            .include_mounts
            .get(&key)
            .copied()
            .unwrap_or(true);
        let (row, cb) = check_setting_row(&label, None, enabled);
        {
            let engine = engine.clone();
            let key = key.clone();
            cb.connect_toggled(move |btn| {
                engine.config().update(|c| {
                    c.index
                        .include_mounts
                        .insert(key.clone(), btn.is_active());
                });
            });
        }
        let _ = i;
        sources.append(&row);
    }
    body.append(&sources);

    body.append(&group_label("Index"));

    let rebuild_row = GtkBox::new(Orientation::Vertical, 10);
    rebuild_row.add_css_class("blink-settings-card");

    let rebuild_head = setting_row(
        "Rebuild index now",
        Some("Force a full re-scan of all enabled sources."),
    );
    let rebuild = Button::with_label("Rebuild");
    rebuild.add_css_class("blink-settings-btn");
    rebuild.add_css_class("blink-settings-primary");
    rebuild.set_valign(gtk::Align::Center);
    rebuild_head.append(&rebuild);
    rebuild_row.append(&rebuild_head);

    let status = Label::new(Some(&engine.format_index_status()));
    status.add_css_class("blink-hint");
    status.add_css_class("blink-settings-card-footer");
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);
    rebuild_row.append(&Separator::new(Orientation::Horizontal));
    rebuild_row.append(&status);

    {
        let engine = engine.clone();
        let status = status.clone();
        rebuild.connect_clicked(move |_| {
            status.set_text("Indexing… 0 files");
            engine.force_reindex();
            let status = status.clone();
            let engine = engine.clone();
            glib_timeout_poll_index(engine, status, 0);
        });
    }

    body.append(&rebuild_row);

    (outer, status)
}

fn build_folders_page(engine: &Arc<Engine>) -> GtkBox {
    let (outer, body) = page_shell(
        "folder-symbolic",
        "Extra folders",
        "Add folders outside home/mounts. They are indexed at the same depth.",
    );

    let list = GtkBox::new(Orientation::Vertical, 0);
    list.add_css_class("blink-settings-card");
    list.add_css_class("blink-settings-list");
    refill_extra_list(&list, engine);

    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    add_row.set_margin_top(2);
    let entry = Entry::builder()
        .placeholder_text("/path/to/folder")
        .hexpand(true)
        .build();
    entry.add_css_class("blink-settings-entry");
    let add = Button::with_label("Add");
    add.add_css_class("blink-settings-btn");
    add.add_css_class("blink-settings-primary");
    {
        let engine = engine.clone();
        let entry = entry.clone();
        let list = list.clone();
        add.connect_clicked(move |_| {
            let p = entry.text().to_string().trim().to_string();
            if p.is_empty() {
                return;
            }
            engine.config().update(|c| {
                if !c.index.extra_roots.contains(&p) {
                    c.index.extra_roots.push(p);
                }
            });
            entry.set_text("");
            refill_extra_list(&list, &engine);
        });
    }
    add_row.append(&entry);
    add_row.append(&add);

    body.append(&list);
    body.append(&add_row);

    // Deep roots — always indexed to depth 6, preferred by live deep search.
    body.append(&group_label("Deep roots"));
    let deep_hint = Label::new(Some(
        "Pinned folders always get depth 6 in the index and are preferred for live deep search. \
         Opening a deep file can auto-promote its parent project folder.",
    ));
    deep_hint.add_css_class("blink-hint");
    deep_hint.set_wrap(true);
    deep_hint.set_halign(gtk::Align::Start);
    deep_hint.set_margin_bottom(6);
    body.append(&deep_hint);

    let deep_list = GtkBox::new(Orientation::Vertical, 0);
    deep_list.add_css_class("blink-settings-card");
    deep_list.add_css_class("blink-settings-list");
    refill_deep_list(&deep_list, engine);

    let deep_add_row = GtkBox::new(Orientation::Horizontal, 8);
    deep_add_row.set_margin_top(2);
    let deep_entry = Entry::builder()
        .placeholder_text("~/projects/my-app")
        .hexpand(true)
        .build();
    deep_entry.add_css_class("blink-settings-entry");
    let deep_add = Button::with_label("Pin");
    deep_add.add_css_class("blink-settings-btn");
    deep_add.add_css_class("blink-settings-primary");
    {
        let engine = engine.clone();
        let deep_entry = deep_entry.clone();
        let deep_list = deep_list.clone();
        deep_add.connect_clicked(move |_| {
            let p = deep_entry.text().to_string().trim().to_string();
            if p.is_empty() {
                return;
            }
            let path = crate::providers::files::expand_user_path(&p);
            engine.promote_deep_root(&path);
            deep_entry.set_text("");
            refill_deep_list(&deep_list, &engine);
        });
    }
    deep_add_row.append(&deep_entry);
    deep_add_row.append(&deep_add);

    body.append(&deep_list);
    body.append(&deep_add_row);
    outer
}

fn build_exclusions_page(engine: &Arc<Engine>) -> GtkBox {
    let (outer, body) = page_shell(
        "edit-delete-symbolic",
        "Exclusions",
        "Folder or path fragments that are never indexed (e.g. node_modules, .git).",
    );

    let list = GtkBox::new(Orientation::Vertical, 0);
    list.add_css_class("blink-settings-card");
    list.add_css_class("blink-settings-list");
    refill_exclude_list(&list, engine);

    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    add_row.set_margin_top(2);
    let entry = Entry::builder()
        .placeholder_text("name or path fragment")
        .hexpand(true)
        .build();
    entry.add_css_class("blink-settings-entry");
    let add = Button::with_label("Add");
    add.add_css_class("blink-settings-btn");
    add.add_css_class("blink-settings-primary");
    {
        let engine = engine.clone();
        let entry = entry.clone();
        let list = list.clone();
        add.connect_clicked(move |_| {
            let p = entry.text().to_string().trim().to_string();
            if p.is_empty() {
                return;
            }
            engine.config().update(|c| {
                if !c.index.exclude.contains(&p) {
                    c.index.exclude.push(p);
                }
            });
            entry.set_text("");
            refill_exclude_list(&list, &engine);
        });
    }
    add_row.append(&entry);
    add_row.append(&add);

    body.append(&list);
    body.append(&add_row);
    outer
}

fn build_defaults_page(
    engine: &Arc<Engine>,
    dismiss_overlay: Rc<RefCell<Option<Box<dyn Fn() -> bool>>>>,
) -> GtkBox {
    let (outer, body) = page_shell(
        "preferences-desktop-default-applications-symbolic",
        "Default apps",
        "Choose which app Blink uses for each file kind. Empty means system default (xdg-open).",
    );

    // Host stack: list page ↔ in-panel app picker (no extra Window — layer-shell exclusive
    // keyboard grab cannot focus a separate modal, which deadlocks Esc / interaction).
    let host = gtk::Stack::new();
    host.add_css_class("blink-settings-defaults-host");
    host.set_hexpand(true);
    host.set_vexpand(true);
    host.set_transition_type(gtk::StackTransitionType::Crossfade);
    host.set_transition_duration(100);

    let list_page = GtkBox::new(Orientation::Vertical, 0);
    list_page.set_hexpand(true);
    list_page.set_vexpand(true);

    // Move page header + body content under list_page by re-parenting from outer.
    // `outer` currently has header then scroll(body). Keep structure: outer → host → pages.
    // Simpler: put group/card/hint into list_page, host into body.
    list_page.append(&group_label("Open with"));

    let card = GtkBox::new(Orientation::Vertical, 0);
    card.add_css_class("blink-settings-card");

    let host_rc = Rc::new(host.clone());
    let picker_open = Rc::new(Cell::new(false));

    {
        let host = host.clone();
        let picker_open = picker_open.clone();
        *dismiss_overlay.borrow_mut() = Some(Box::new(move || {
            if picker_open.get() {
                picker_open.set(false);
                // Drop any previous picker page named "picker"
                if let Some(child) = host.child_by_name("picker") {
                    host.remove(&child);
                }
                host.set_visible_child_name("list");
                true
            } else {
                false
            }
        }));
    }

    for (i, cat) in FileOpenCategory::ALL.iter().enumerate() {
        if i > 0 {
            card.append(&Separator::new(Orientation::Horizontal));
        }
        card.append(&defaults_category_row(
            engine,
            *cat,
            host_rc.clone(),
            picker_open.clone(),
        ));
    }
    list_page.append(&card);

    let hint = Label::new(Some(
        "These overrides apply only inside Blink — they do not change system MIME defaults.",
    ));
    hint.add_css_class("blink-hint");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    hint.set_margin_top(10);
    list_page.append(&hint);

    host.add_named(&list_page, Some("list"));
    host.set_visible_child_name("list");
    body.append(&host);

    outer
}

fn defaults_category_row(
    engine: &Arc<Engine>,
    cat: FileOpenCategory,
    host: Rc<gtk::Stack>,
    picker_open: Rc<Cell<bool>>,
) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 10);
    row.add_css_class("blink-settings-list-row");
    row.set_hexpand(true);

    let icon = Image::from_icon_name(cat.icon());
    icon.set_pixel_size(18);
    icon.set_valign(gtk::Align::Center);

    let text = GtkBox::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_halign(gtk::Align::Start);
    text.set_valign(gtk::Align::Center);

    let title = Label::new(Some(cat.label()));
    title.add_css_class("blink-settings-list-label");
    title.set_halign(gtk::Align::Start);
    title.set_xalign(0.0);

    let current = engine.config().get().open_with.get(cat).map(|s| s.to_string());
    let sub_text = format_open_with_label(engine, current.as_deref());
    let sub = Label::new(Some(&sub_text));
    sub.add_css_class("blink-settings-list-sub");
    sub.set_halign(gtk::Align::Start);
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_max_width_chars(36);
    sub.set_tooltip_text(Some(cat.subtitle()));

    text.append(&title);
    text.append(&sub);

    let choose = Button::with_label("Choose…");
    choose.add_css_class("blink-settings-btn");
    choose.set_valign(gtk::Align::Center);

    let reset = Button::with_label("System");
    reset.add_css_class("blink-settings-btn");
    reset.set_valign(gtk::Align::Center);
    reset.set_tooltip_text(Some("Use system default (xdg-open)"));
    reset.set_sensitive(current.is_some());

    {
        let engine = engine.clone();
        let sub = sub.clone();
        let reset = reset.clone();
        let host = host.clone();
        let picker_open = picker_open.clone();
        choose.connect_clicked(move |_| {
            show_app_picker(
                host.clone(),
                picker_open.clone(),
                engine.clone(),
                cat,
                sub.clone(),
                reset.clone(),
            );
        });
    }
    {
        let engine = engine.clone();
        let sub = sub.clone();
        let reset_btn = reset.clone();
        reset.connect_clicked(move |_| {
            engine.config().update(|c| c.open_with.set(cat, None));
            sub.set_text("System default");
            reset_btn.set_sensitive(false);
        });
    }

    row.append(&icon);
    row.append(&text);
    row.append(&choose);
    row.append(&reset);
    row
}

fn format_open_with_label(engine: &Engine, desktop_id: Option<&str>) -> String {
    match desktop_id {
        None => "System default".into(),
        Some(id) => match engine.app_display_name(id) {
            Some(name) => format!("{name} (Blink)"),
            None => format!("{id} (Blink)"),
        },
    }
}

fn show_app_picker(
    host: Rc<gtk::Stack>,
    picker_open: Rc<Cell<bool>>,
    engine: Arc<Engine>,
    cat: FileOpenCategory,
    status_label: Label,
    reset_btn: Button,
) {
    // Replace any existing picker page.
    if let Some(child) = host.child_by_name("picker") {
        host.remove(&child);
    }

    let root = GtkBox::new(Orientation::Vertical, 0);
    root.add_css_class("blink-settings-picker");
    root.set_hexpand(true);
    root.set_vexpand(true);

    let top = GtkBox::new(Orientation::Horizontal, 8);
    top.set_margin_bottom(8);

    let back = Button::with_label("← Back");
    back.add_css_class("blink-settings-btn");
    back.set_halign(gtk::Align::Start);

    let head = Label::new(Some(&format!(
        "Open {} with…",
        cat.label().to_ascii_lowercase()
    )));
    head.add_css_class("blink-settings-page-title");
    head.set_halign(gtk::Align::Start);
    head.set_hexpand(true);
    head.set_xalign(0.0);

    top.append(&back);
    top.append(&head);

    let sub = Label::new(Some(cat.subtitle()));
    sub.add_css_class("blink-hint");
    sub.set_halign(gtk::Align::Start);
    sub.set_margin_bottom(8);

    let search = Entry::builder()
        .placeholder_text("Filter apps…")
        .hexpand(true)
        .build();
    search.add_css_class("blink-settings-search");
    search.set_primary_icon_name(Some("system-search-symbolic"));
    search.set_margin_bottom(8);

    let scroll = ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .min_content_height(220)
        .build();

    let list = ListBox::new();
    list.add_css_class("blink-settings-nav");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);

    // System default row first
    {
        let row = ListBoxRow::new();
        row.add_css_class("blink-settings-nav-row");
        row.set_widget_name("__system__");
        let item = GtkBox::new(Orientation::Horizontal, 10);
        item.set_margin_start(10);
        item.set_margin_end(10);
        item.set_margin_top(8);
        item.set_margin_bottom(8);
        let icon = Image::from_icon_name("emblem-system-symbolic");
        icon.set_pixel_size(18);
        let name = Label::new(Some("System default"));
        name.add_css_class("blink-settings-nav-title");
        name.set_halign(gtk::Align::Start);
        name.set_hexpand(true);
        name.set_xalign(0.0);
        item.append(&icon);
        item.append(&name);
        row.set_child(Some(&item));
        list.append(&row);
    }

    let apps = engine.list_apps_for_picker();
    for app in &apps {
        let row = ListBoxRow::new();
        row.add_css_class("blink-settings-nav-row");
        row.set_widget_name(&app.desktop_id);
        row.set_tooltip_text(Some(&format!(
            "{} · {}",
            app.name,
            if app.comment.is_empty() {
                app.desktop_id.as_str()
            } else {
                app.comment.as_str()
            }
        )));

        let item = GtkBox::new(Orientation::Horizontal, 10);
        item.set_margin_start(10);
        item.set_margin_end(10);
        item.set_margin_top(7);
        item.set_margin_bottom(7);

        let icon = if app.icon.is_empty() {
            Image::from_icon_name("application-x-executable")
        } else {
            Image::from_icon_name(&app.icon)
        };
        icon.set_pixel_size(18);
        icon.set_valign(gtk::Align::Center);

        let name = Label::new(Some(&app.name));
        name.add_css_class("blink-settings-nav-title");
        name.set_halign(gtk::Align::Start);
        name.set_hexpand(true);
        name.set_xalign(0.0);

        item.append(&icon);
        item.append(&name);
        row.set_child(Some(&item));
        list.append(&row);
    }

    {
        let list = list.clone();
        search.connect_changed(move |entry| {
            let q = entry.text().to_lowercase();
            let mut child = list.first_child();
            while let Some(w) = child {
                let next = w.next_sibling();
                if let Ok(row) = w.downcast::<ListBoxRow>() {
                    let id = row.widget_name().to_string();
                    if id == "__system__" {
                        row.set_visible(true);
                    } else {
                        let tip = row.tooltip_text().unwrap_or_default().to_lowercase();
                        let visible =
                            q.is_empty() || tip.contains(&q) || id.to_lowercase().contains(&q);
                        row.set_visible(visible);
                    }
                }
                child = next;
            }
        });
    }

    let close_picker = {
        let host = host.clone();
        let picker_open = picker_open.clone();
        Rc::new(move || {
            picker_open.set(false);
            if let Some(child) = host.child_by_name("picker") {
                host.remove(&child);
            }
            host.set_visible_child_name("list");
        })
    };

    {
        let engine = engine.clone();
        let status_label = status_label.clone();
        let reset_btn = reset_btn.clone();
        let close_picker = close_picker.clone();
        list.connect_row_activated(move |_, row| {
            let id = row.widget_name().to_string();
            if id == "__system__" {
                engine.config().update(|c| c.open_with.set(cat, None));
                status_label.set_text("System default");
                reset_btn.set_sensitive(false);
            } else {
                let desktop_id = id.clone();
                engine
                    .config()
                    .update(|c| c.open_with.set(cat, Some(desktop_id.clone())));
                status_label.set_text(&format_open_with_label(&engine, Some(&desktop_id)));
                reset_btn.set_sensitive(true);
            }
            close_picker();
        });
    }

    {
        let close_picker = close_picker.clone();
        back.connect_clicked(move |_| close_picker());
    }

    // Esc on the picker closes back to the list (does not leave Settings).
    {
        let close_picker = close_picker.clone();
        let key = EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == Key::Escape {
                close_picker();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        root.add_controller(key);
    }

    scroll.set_child(Some(&list));
    root.append(&top);
    root.append(&sub);
    root.append(&search);
    root.append(&scroll);

    host.add_named(&root, Some("picker"));
    host.set_visible_child_name("picker");
    picker_open.set(true);
    search.grab_focus();
}

fn build_display_page(engine: &Arc<Engine>, cfg: &crate::config::BlinkConfig) -> GtkBox {
    let (outer, body) = page_shell(
        "preferences-desktop-display-symbolic",
        "Display",
        "Control how indexed paths appear in search results.",
    );

    body.append(&group_label("Path format"));

    let style_card = GtkBox::new(Orientation::Vertical, 0);
    style_card.add_css_class("blink-settings-card");

    let label_style = CheckButton::with_label("Label  ·  Projects:/path");
    let drive_style = CheckButton::with_label("Drive  ·  D:/path");
    label_style.add_css_class("blink-settings-radio");
    drive_style.add_css_class("blink-settings-radio");
    drive_style.set_group(Some(&label_style));
    match cfg.index.path_style {
        PathStyle::Label => label_style.set_active(true),
        PathStyle::Drive => drive_style.set_active(true),
    }
    {
        let engine = engine.clone();
        label_style.connect_toggled(move |btn| {
            if btn.is_active() {
                engine
                    .config()
                    .update(|c| c.index.path_style = PathStyle::Label);
            }
        });
    }
    {
        let engine = engine.clone();
        drive_style.connect_toggled(move |btn| {
            if btn.is_active() {
                engine
                    .config()
                    .update(|c| c.index.path_style = PathStyle::Drive);
            }
        });
    }

    let label_box = GtkBox::new(Orientation::Vertical, 2);
    label_box.add_css_class("blink-settings-list-row");
    label_box.append(&label_style);

    let drive_box = GtkBox::new(Orientation::Vertical, 2);
    drive_box.add_css_class("blink-settings-list-row");
    drive_box.append(&drive_style);

    style_card.append(&label_box);
    style_card.append(&Separator::new(Orientation::Horizontal));
    style_card.append(&drive_box);
    body.append(&style_card);

    let hint = Label::new(Some(
        "Label uses friendly mount names. Drive uses letter-style prefixes when available.",
    ));
    hint.add_css_class("blink-hint");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    body.append(&hint);

    outer
}

/// Horizontal setting row: title (+ optional subtitle) on the left, control on the right.
fn setting_row(title: &str, subtitle: Option<&str>) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 12);
    row.add_css_class("blink-settings-list-row");
    row.set_hexpand(true);

    let text = GtkBox::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_halign(gtk::Align::Start);
    text.set_valign(gtk::Align::Center);

    let t = Label::new(Some(title));
    t.add_css_class("blink-settings-list-label");
    t.set_halign(gtk::Align::Start);
    t.set_xalign(0.0);
    text.append(&t);

    if let Some(sub) = subtitle {
        let s = Label::new(Some(sub));
        s.add_css_class("blink-settings-list-sub");
        s.set_halign(gtk::Align::Start);
        s.set_xalign(0.0);
        s.set_wrap(true);
        s.set_max_width_chars(48);
        text.append(&s);
    }

    row.append(&text);
    row
}

fn check_setting_row(title: &str, subtitle: Option<&str>, active: bool) -> (GtkBox, CheckButton) {
    let row = setting_row(title, subtitle);
    let cb = CheckButton::new();
    cb.set_active(active);
    cb.set_valign(gtk::Align::Center);
    cb.add_css_class("blink-settings-check");
    row.append(&cb);
    (row, cb)
}

fn group_label(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("blink-settings-section");
    l.set_halign(gtk::Align::Start);
    l.set_xalign(0.0);
    l
}

fn depth_help_text(depth: usize) -> String {
    let example = match depth {
        1 => "e.g. ~/Projects only (very fast)",
        2 => "e.g. ~/Projects/foo  ·  default (recommended)",
        3 => "e.g. ~/Projects/foo/src  ·  ~4× more files",
        4 => "e.g. one level deeper  ·  ~8× more files",
        5 => "deep trees  ·  slower index & search",
        _ => "maximum  ·  large indexes, use only if needed",
    };
    format!(
        "{depth} level{} from each root · {example}",
        if depth == 1 { "" } else { "s" }
    )
}

fn refill_extra_list(list: &GtkBox, engine: &Arc<Engine>) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let roots = engine.config().get().index.extra_roots;
    if roots.is_empty() {
        let empty = Label::new(Some("No extra folders yet"));
        empty.add_css_class("blink-hint");
        empty.add_css_class("blink-settings-list-row");
        empty.set_halign(gtk::Align::Start);
        list.append(&empty);
        return;
    }
    for (i, p) in roots.iter().enumerate() {
        if i > 0 {
            list.append(&Separator::new(Orientation::Horizontal));
        }
        list.append(&removable_row(p, engine, ListKind::Extra));
    }
}

fn refill_deep_list(list: &GtkBox, engine: &Arc<Engine>) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let roots = engine.config().get().index.deep_roots;
    if roots.is_empty() {
        let empty = Label::new(Some("No deep roots yet — pin a project folder"));
        empty.add_css_class("blink-hint");
        empty.add_css_class("blink-settings-list-row");
        empty.set_halign(gtk::Align::Start);
        list.append(&empty);
        return;
    }
    for (i, p) in roots.iter().enumerate() {
        if i > 0 {
            list.append(&Separator::new(Orientation::Horizontal));
        }
        list.append(&removable_row(p, engine, ListKind::Deep));
    }
}

#[derive(Clone, Copy)]
enum ListKind {
    Extra,
    Exclude,
    Deep,
}

fn refill_exclude_list(list: &GtkBox, engine: &Arc<Engine>) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let items = engine.config().get().index.exclude;
    if items.is_empty() {
        let empty = Label::new(Some("No exclusions"));
        empty.add_css_class("blink-hint");
        empty.add_css_class("blink-settings-list-row");
        empty.set_halign(gtk::Align::Start);
        list.append(&empty);
        return;
    }
    for (i, p) in items.iter().enumerate() {
        if i > 0 {
            list.append(&Separator::new(Orientation::Horizontal));
        }
        list.append(&removable_row(p, engine, ListKind::Exclude));
    }
}

fn removable_row(text: &str, engine: &Arc<Engine>, kind: ListKind) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    row.add_css_class("blink-settings-list-row");
    let lab = Label::new(Some(text));
    lab.set_halign(gtk::Align::Start);
    lab.set_hexpand(true);
    lab.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    lab.add_css_class("blink-settings-list-label");
    let rm = Button::with_label("×");
    rm.add_css_class("blink-settings-btn");
    rm.add_css_class("blink-settings-icon-btn");
    {
        let engine = engine.clone();
        let text = text.to_string();
        let row = row.clone();
        rm.connect_clicked(move |_| {
            match kind {
                ListKind::Extra => {
                    engine.config().update(|c| {
                        c.index.extra_roots.retain(|x| x != &text);
                    });
                }
                ListKind::Exclude => {
                    engine.config().update(|c| {
                        c.index.exclude.retain(|x| x != &text);
                    });
                }
                ListKind::Deep => {
                    engine.remove_deep_root(&text);
                }
            }
            if let Some(parent) = row.parent() {
                if let Ok(box_) = parent.downcast::<GtkBox>() {
                    // Remove preceding separator if present
                    if let Some(prev) = row.prev_sibling() {
                        if prev.css_classes().iter().any(|c| c == "horizontal")
                            || prev.type_().name() == "GtkSeparator"
                        {
                            box_.remove(&prev);
                        }
                    } else if let Some(next) = row.next_sibling() {
                        if next.type_().name() == "GtkSeparator" {
                            box_.remove(&next);
                        }
                    }
                    box_.remove(&row);
                    if box_.first_child().is_none() {
                        let empty = Label::new(Some(match kind {
                            ListKind::Extra => "No extra folders yet",
                            ListKind::Exclude => "No exclusions",
                            ListKind::Deep => "No deep roots yet — pin a project folder",
                        }));
                        empty.add_css_class("blink-hint");
                        empty.add_css_class("blink-settings-list-row");
                        empty.set_halign(gtk::Align::Start);
                        box_.append(&empty);
                    }
                }
            }
        });
    }
    row.append(&lab);
    row.append(&rm);
    row
}

fn glib_timeout_poll_index(engine: Arc<Engine>, status: Label, n: u32) {
    if n > 300 {
        status.set_text(&engine.format_index_status());
        return;
    }
    glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
        let p = engine.index_progress();
        status.set_text(&engine.format_index_status());
        if p.running {
            glib_timeout_poll_index(engine, status, n + 1);
        }
    });
}


fn build_appearance_page(
    engine: &Arc<Engine>,
    theme: &Rc<ThemeManager>,
    cfg: &crate::config::BlinkConfig,
) -> GtkBox {
    let (outer, body) = page_shell(
        "preferences-desktop-theme-symbolic",
        "Appearance",
        "Tweak transparency, accent colour, type scale, and icons. Colours still follow your Caelestia scheme.",
    );

    let ui = cfg.ui.clone();

    // --- Opacity ---
    body.append(&group_label("Panel"));

    let panel_card = GtkBox::new(Orientation::Vertical, 0);
    panel_card.add_css_class("blink-settings-card");

    let opacity_row = setting_row(
        "Transparency",
        Some(&format!("{:.0}% opaque", ui.opacity * 100.0)),
    );
    let opacity_stepper = GtkBox::new(Orientation::Horizontal, 4);
    opacity_stepper.set_valign(gtk::Align::Center);
    let op_dec = Button::with_label("−");
    op_dec.add_css_class("blink-settings-btn");
    op_dec.add_css_class("blink-settings-icon-btn");
    let op_val = Label::new(Some(&format!("{:.0}%", ui.opacity * 100.0)));
    op_val.add_css_class("blink-settings-stepper-val");
    op_val.set_width_chars(4);
    let op_inc = Button::with_label("+");
    op_inc.add_css_class("blink-settings-btn");
    op_inc.add_css_class("blink-settings-icon-btn");
    opacity_stepper.append(&op_dec);
    opacity_stepper.append(&op_val);
    opacity_stepper.append(&op_inc);
    opacity_row.append(&opacity_stepper);
    panel_card.append(&opacity_row);

    let opacity_hint = opacity_row
        .first_child()
        .and_then(|c| c.last_child())
        .and_then(|c| c.downcast::<Label>().ok());

    {
        let engine = engine.clone();
        let theme = theme.clone();
        let op_val = op_val.clone();
        let opacity_hint = opacity_hint.clone();
        op_dec.connect_clicked(move |_| {
            let mut next = 0.85f32;
            engine.config().update(|c| {
                next = (c.ui.opacity - 0.05).clamp(0.40, 1.0);
                c.ui.opacity = next;
            });
            op_val.set_text(&format!("{:.0}%", next * 100.0));
            if let Some(h) = &opacity_hint {
                h.set_text(&format!("{:.0}% opaque", next * 100.0));
            }
            theme.reload();
        });
    }
    {
        let engine = engine.clone();
        let theme = theme.clone();
        let op_val = op_val.clone();
        let opacity_hint = opacity_hint.clone();
        op_inc.connect_clicked(move |_| {
            let mut next = 0.85f32;
            engine.config().update(|c| {
                next = (c.ui.opacity + 0.05).clamp(0.40, 1.0);
                c.ui.opacity = next;
            });
            op_val.set_text(&format!("{:.0}%", next * 100.0));
            if let Some(h) = &opacity_hint {
                h.set_text(&format!("{:.0}% opaque", next * 100.0));
            }
            theme.reload();
        });
    }

    panel_card.append(&Separator::new(Orientation::Horizontal));

    // Corner radius
    let radius_row = setting_row("Corner radius", Some(&format!("{}px", ui.radius)));
    let radius_stepper = GtkBox::new(Orientation::Horizontal, 4);
    let r_dec = Button::with_label("−");
    r_dec.add_css_class("blink-settings-btn");
    r_dec.add_css_class("blink-settings-icon-btn");
    let r_val = Label::new(Some(&format!("{}", ui.radius)));
    r_val.add_css_class("blink-settings-stepper-val");
    r_val.set_width_chars(3);
    let r_inc = Button::with_label("+");
    r_inc.add_css_class("blink-settings-btn");
    r_inc.add_css_class("blink-settings-icon-btn");
    radius_stepper.append(&r_dec);
    radius_stepper.append(&r_val);
    radius_stepper.append(&r_inc);
    radius_row.append(&radius_stepper);
    panel_card.append(&radius_row);

    let radius_hint = radius_row
        .first_child()
        .and_then(|c| c.last_child())
        .and_then(|c| c.downcast::<Label>().ok());

    {
        let engine = engine.clone();
        let theme = theme.clone();
        let r_val = r_val.clone();
        let radius_hint = radius_hint.clone();
        r_dec.connect_clicked(move |_| {
            let mut next = 16u32;
            engine.config().update(|c| {
                next = c.ui.radius.saturating_sub(1).max(8);
                c.ui.radius = next;
            });
            r_val.set_text(&format!("{next}"));
            if let Some(h) = &radius_hint {
                h.set_text(&format!("{next}px"));
            }
            theme.reload();
        });
    }
    {
        let engine = engine.clone();
        let theme = theme.clone();
        let r_val = r_val.clone();
        let radius_hint = radius_hint.clone();
        r_inc.connect_clicked(move |_| {
            let mut next = 16u32;
            engine.config().update(|c| {
                next = (c.ui.radius + 1).min(24);
                c.ui.radius = next;
            });
            r_val.set_text(&format!("{next}"));
            if let Some(h) = &radius_hint {
                h.set_text(&format!("{next}px"));
            }
            theme.reload();
        });
    }

    body.append(&panel_card);

    // --- Accent ---
    body.append(&group_label("Colours"));

    let colour_card = GtkBox::new(Orientation::Vertical, 0);
    colour_card.add_css_class("blink-settings-card");

    let accent_row = setting_row(
        "Accent override",
        Some("Empty = Caelestia primary"),
    );
    let accent_entry = Entry::builder()
        .placeholder_text("#7aa2f7")
        .hexpand(false)
        .width_chars(10)
        .build();
    accent_entry.add_css_class("blink-settings-entry");
    if let Some(a) = &ui.accent {
        accent_entry.set_text(a);
    }
    accent_row.append(&accent_entry);
    colour_card.append(&accent_row);

    {
        let engine = engine.clone();
        let theme = theme.clone();
        accent_entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            engine.config().update(|c| {
                let t = text.trim();
                if t.is_empty() {
                    c.ui.accent = None;
                } else {
                    c.ui.accent = Some(t.to_string());
                }
            });
            theme.reload();
        });
    }

    colour_card.append(&Separator::new(Orientation::Horizontal));

    let presets = GtkBox::new(Orientation::Horizontal, 6);
    presets.add_css_class("blink-settings-list-row");
    let preset_label = Label::new(Some("Quick accents"));
    preset_label.add_css_class("blink-settings-row-title");
    preset_label.set_halign(gtk::Align::Start);
    preset_label.set_hexpand(true);
    presets.append(&preset_label);

    for (name, hex) in [
        ("Blue", "#7aa2f7"),
        ("Cyan", "#7dcfff"),
        ("Magenta", "#bb9af7"),
        ("Green", "#9ece6a"),
        ("Orange", "#ff9e64"),
        ("Red", "#f7768e"),
        ("Reset", ""),
    ] {
        let btn = Button::with_label(name);
        btn.add_css_class("blink-settings-btn");
        btn.add_css_class("blink-settings-link");
        let engine = engine.clone();
        let theme = theme.clone();
        let accent_entry = accent_entry.clone();
        let hex = hex.to_string();
        btn.connect_clicked(move |_| {
            if hex.is_empty() {
                accent_entry.set_text("");
                engine.config().update(|c| c.ui.accent = None);
            } else {
                accent_entry.set_text(&hex);
                engine.config().update(|c| c.ui.accent = Some(hex.clone()));
            }
            theme.reload();
        });
        presets.append(&btn);
    }
    colour_card.append(&presets);
    body.append(&colour_card);

    // --- Type ---
    body.append(&group_label("Type & icons"));

    let type_card = GtkBox::new(Orientation::Vertical, 0);
    type_card.add_css_class("blink-settings-card");

    let font_row = setting_row(
        "Font scale",
        Some(&format!("{:.0}%", ui.font_scale * 100.0)),
    );
    let font_stepper = GtkBox::new(Orientation::Horizontal, 4);
    let f_dec = Button::with_label("−");
    f_dec.add_css_class("blink-settings-btn");
    f_dec.add_css_class("blink-settings-icon-btn");
    let f_val = Label::new(Some(&format!("{:.0}%", ui.font_scale * 100.0)));
    f_val.add_css_class("blink-settings-stepper-val");
    f_val.set_width_chars(4);
    let f_inc = Button::with_label("+");
    f_inc.add_css_class("blink-settings-btn");
    f_inc.add_css_class("blink-settings-icon-btn");
    font_stepper.append(&f_dec);
    font_stepper.append(&f_val);
    font_stepper.append(&f_inc);
    font_row.append(&font_stepper);
    type_card.append(&font_row);

    let font_hint = font_row
        .first_child()
        .and_then(|c| c.last_child())
        .and_then(|c| c.downcast::<Label>().ok());

    {
        let engine = engine.clone();
        let theme = theme.clone();
        let f_val = f_val.clone();
        let font_hint = font_hint.clone();
        f_dec.connect_clicked(move |_| {
            let mut next = 1.0f32;
            engine.config().update(|c| {
                next = ((c.ui.font_scale * 100.0).round() - 5.0).max(85.0) / 100.0;
                c.ui.font_scale = next;
            });
            f_val.set_text(&format!("{:.0}%", next * 100.0));
            if let Some(h) = &font_hint {
                h.set_text(&format!("{:.0}%", next * 100.0));
            }
            theme.reload();
        });
    }
    {
        let engine = engine.clone();
        let theme = theme.clone();
        let f_val = f_val.clone();
        let font_hint = font_hint.clone();
        f_inc.connect_clicked(move |_| {
            let mut next = 1.0f32;
            engine.config().update(|c| {
                next = ((c.ui.font_scale * 100.0).round() + 5.0).min(130.0) / 100.0;
                c.ui.font_scale = next;
            });
            f_val.set_text(&format!("{:.0}%", next * 100.0));
            if let Some(h) = &font_hint {
                h.set_text(&format!("{:.0}%", next * 100.0));
            }
            theme.reload();
        });
    }

    type_card.append(&Separator::new(Orientation::Horizontal));

    let icon_row = setting_row("Icon size", Some(&format!("{}px", ui.icon_size)));
    let icon_stepper = GtkBox::new(Orientation::Horizontal, 4);
    let i_dec = Button::with_label("−");
    i_dec.add_css_class("blink-settings-btn");
    i_dec.add_css_class("blink-settings-icon-btn");
    let i_val = Label::new(Some(&format!("{}", ui.icon_size)));
    i_val.add_css_class("blink-settings-stepper-val");
    i_val.set_width_chars(3);
    let i_inc = Button::with_label("+");
    i_inc.add_css_class("blink-settings-btn");
    i_inc.add_css_class("blink-settings-icon-btn");
    icon_stepper.append(&i_dec);
    icon_stepper.append(&i_val);
    icon_stepper.append(&i_inc);
    icon_row.append(&icon_stepper);
    type_card.append(&icon_row);

    let icon_hint = icon_row
        .first_child()
        .and_then(|c| c.last_child())
        .and_then(|c| c.downcast::<Label>().ok());

    {
        let engine = engine.clone();
        let theme = theme.clone();
        let i_val = i_val.clone();
        let icon_hint = icon_hint.clone();
        i_dec.connect_clicked(move |_| {
            let mut next = 26u32;
            engine.config().update(|c| {
                next = c.ui.icon_size.saturating_sub(2).max(18);
                c.ui.icon_size = next;
            });
            i_val.set_text(&format!("{next}"));
            if let Some(h) = &icon_hint {
                h.set_text(&format!("{next}px"));
            }
            theme.reload();
        });
    }
    {
        let engine = engine.clone();
        let theme = theme.clone();
        let i_val = i_val.clone();
        let icon_hint = icon_hint.clone();
        i_inc.connect_clicked(move |_| {
            let mut next = 26u32;
            engine.config().update(|c| {
                next = (c.ui.icon_size + 2).min(36);
                c.ui.icon_size = next;
            });
            i_val.set_text(&format!("{next}"));
            if let Some(h) = &icon_hint {
                h.set_text(&format!("{next}px"));
            }
            theme.reload();
        });
    }

    type_card.append(&Separator::new(Orientation::Horizontal));

    let (sym_row, sym_cb) = check_setting_row(
        "Prefer symbolic icons",
        Some("Use -symbolic variants when the icon theme provides them"),
        ui.symbolic_icons,
    );
    {
        let engine = engine.clone();
        let theme = theme.clone();
        sym_cb.connect_toggled(move |btn| {
            let on = btn.is_active();
            engine.config().update(|c| c.ui.symbolic_icons = on);
            theme.reload();
        });
    }
    type_card.append(&sym_row);

    body.append(&type_card);

    // Reset
    body.append(&group_label("Reset"));
    let reset_card = GtkBox::new(Orientation::Vertical, 0);
    reset_card.add_css_class("blink-settings-card");
    let reset_row = setting_row("Restore defaults", Some("Opacity, accent, font, icons, radius"));
    let reset_btn = Button::with_label("Reset appearance");
    reset_btn.add_css_class("blink-settings-btn");
    {
        let engine = engine.clone();
        let theme = theme.clone();
        let accent_entry = accent_entry.clone();
        let op_val = op_val.clone();
        let r_val = r_val.clone();
        let f_val = f_val.clone();
        let i_val = i_val.clone();
        reset_btn.connect_clicked(move |_| {
            engine.config().update(|c| c.ui = UiThemeConfig::default());
            accent_entry.set_text("");
            op_val.set_text("85%");
            r_val.set_text("16");
            f_val.set_text("100%");
            i_val.set_text("26");
            theme.reload();
        });
    }
    reset_row.append(&reset_btn);
    reset_card.append(&reset_row);
    body.append(&reset_card);

    let note = Label::new(Some(
        "Base colours come from Caelestia (~/.local/state/caelestia/scheme.json). Accent override only changes the highlight colour. Icon size applies on the next search refresh.",
    ));
    note.add_css_class("blink-hint");
    note.set_halign(gtk::Align::Start);
    note.set_wrap(true);
    body.append(&note);

    outer
}



fn build_tools_page(engine: &Arc<Engine>, cfg: &crate::config::BlinkConfig) -> GtkBox {
    let (outer, body) = page_shell(
        "applications-utilities-symbolic",
        "Tools",
        "Optional helpers. Turning a tool off stops all related background work.",
    );

    body.append(&group_label("Translation"));

    let card = GtkBox::new(Orientation::Vertical, 0);
    card.add_css_class("blink-settings-card");

    let (en_row, en_cb) = check_setting_row(
        "Enable translation",
        Some("When off: no network, cache, or translate work at all."),
        cfg.translate.enabled,
    );
    {
        let engine = engine.clone();
        en_cb.connect_toggled(move |btn| {
            let on = btn.is_active();
            engine.config().update(|c| c.translate.enabled = on);
        });
    }
    card.append(&en_row);

    card.append(&Separator::new(Orientation::Horizontal));

    let (auto_row, auto_cb) = check_setting_row(
        "Auto-detect CJK paste",
        Some("Without a tr prefix. Ignored when translation is disabled."),
        cfg.translate.auto_detect,
    );
    {
        let engine = engine.clone();
        auto_cb.connect_toggled(move |btn| {
            let on = btn.is_active();
            engine.config().update(|c| c.translate.auto_detect = on);
        });
    }
    card.append(&auto_row);

    card.append(&Separator::new(Orientation::Horizontal));

    // Target language
    let target_row = setting_row("Target language", Some("BCP-47 code, e.g. en / zh / ja / hi"));
    let target_entry = Entry::builder()
        .placeholder_text("en")
        .hexpand(false)
        .width_chars(8)
        .build();
    target_entry.add_css_class("blink-settings-entry");
    target_entry.set_text(&cfg.translate.target_lang);
    target_row.append(&target_entry);
    card.append(&target_row);
    {
        let engine = engine.clone();
        target_entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            engine.config().update(|c| {
                c.translate.target_lang = text;
            });
        });
    }

    card.append(&Separator::new(Orientation::Horizontal));

    // Endpoint
    let ep_row = setting_row(
        "API endpoint",
        Some("LibreTranslate base URL. Empty = free MyMemory fallback"),
    );
    let ep_entry = Entry::builder()
        .placeholder_text("https://libretranslate.example")
        .hexpand(true)
        .build();
    ep_entry.add_css_class("blink-settings-entry");
    ep_entry.set_text(&cfg.translate.endpoint);
    ep_row.append(&ep_entry);
    card.append(&ep_row);
    {
        let engine = engine.clone();
        ep_entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            engine.config().update(|c| {
                c.translate.endpoint = text;
            });
        });
    }

    card.append(&Separator::new(Orientation::Horizontal));

    // API key
    let key_row = setting_row("API key", Some("Optional for self-hosted LibreTranslate"));
    let key_entry = Entry::builder()
        .placeholder_text("(optional)")
        .hexpand(true)
        .visibility(false)
        .build();
    key_entry.add_css_class("blink-settings-entry");
    if let Some(k) = &cfg.translate.api_key {
        key_entry.set_text(k);
    }
    key_row.append(&key_entry);
    card.append(&key_row);
    {
        let engine = engine.clone();
        key_entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            engine.config().update(|c| {
                let t = text.trim();
                c.translate.api_key = if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                };
            });
        });
    }

    body.append(&card);

    let note = Label::new(Some(
        "Paste Chinese (or type tr hello). Text is sent to the configured endpoint when not cached. Prefer a local LibreTranslate for privacy. See translation.md.",
    ));
    note.add_css_class("blink-hint");
    note.set_halign(gtk::Align::Start);
    note.set_wrap(true);
    body.append(&note);

    outer
}
