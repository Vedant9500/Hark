use crate::config::{
    default_mount_enabled, discover_mounts, FileOpenCategory, LayoutMode, PathStyle, UiThemeConfig,
};
use crate::engine::Engine;
use crate::theme::ThemeManager;
use gtk::gdk::Key;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Box as GtkBox, Button, CheckButton, Entry, EventControllerFocus, EventControllerKey, Image,
    Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, Separator,
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
        id: "typos",
        title: "Typo aliases",
        subtitle: "Learned search corrections",
        icon: "input-keyboard-symbolic",
    },
    Category {
        id: "tools",
        title: "Tools",
        subtitle: "Translation & extras",
        icon: "applications-utilities-symbolic",
    },
];

/// Callback stored in a settings overlay slot (no args).
type OnDoneSlot = Rc<RefCell<Option<Box<dyn Fn()>>>>;
/// Callback stored in a settings overlay slot (returns whether one was open).
type OnDoneBoolSlot = Rc<RefCell<Option<Box<dyn Fn() -> bool>>>>;

pub struct SettingsPanel {
    pub root: GtkBox,
    status: Label,
    pub nav: ListBox,
    engine: Arc<Engine>,
    #[allow(dead_code)]
    theme: Rc<ThemeManager>,
    on_done: OnDoneSlot,
    /// Closes in-panel overlays (e.g. default-app picker). Returns true if one was open.
    dismiss_overlay: OnDoneBoolSlot,
}

impl SettingsPanel {
    pub fn new(engine: Arc<Engine>, theme: Rc<ThemeManager>) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("hark-settings");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_overflow(gtk::Overflow::Hidden);
        // Same footprint as the expanded launcher view — the stack is
        // vhomogeneous=false, so without this the window shrinks/jumps when
        // switching between search and settings.
        root.set_size_request(super::WINDOW_WIDTH, super::EXPANDED_WINDOW_HEIGHT);

        // Dual panel body (no bulky top chrome — Esc closes)
        let split = GtkBox::new(Orientation::Horizontal, 0);
        split.add_css_class("hark-settings-split");
        split.set_hexpand(true);
        split.set_vexpand(true);

        // --- Left nav ---
        let nav_col = GtkBox::new(Orientation::Vertical, 0);
        nav_col.add_css_class("hark-settings-nav-col");
        nav_col.set_hexpand(false);
        nav_col.set_vexpand(true);

        let search = Entry::builder()
            .placeholder_text("Search…")
            .hexpand(true)
            .build();
        search.add_css_class("hark-settings-search");
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
        nav_scroll.add_css_class("hark-settings-nav-scroll");

        let nav = ListBox::new();
        nav.add_css_class("hark-settings-nav");
        nav.set_selection_mode(gtk::SelectionMode::Single);
        nav.set_hexpand(false);

        for (i, cat) in CATEGORIES.iter().enumerate() {
            let row = ListBoxRow::new();
            row.add_css_class("hark-settings-nav-row");
            row.set_selectable(true);

            let item = GtkBox::new(Orientation::Horizontal, 10);
            item.add_css_class("hark-settings-nav-item");
            item.set_margin_start(10);
            item.set_margin_end(10);
            item.set_margin_top(7);
            item.set_margin_bottom(7);
            item.set_valign(gtk::Align::Center);

            let icon = Image::from_icon_name(cat.icon);
            icon.add_css_class("hark-settings-nav-icon");
            icon.set_pixel_size(16);
            icon.set_valign(gtk::Align::Center);

            let name = Label::new(Some(cat.title));
            name.add_css_class("hark-settings-nav-title");
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
                        let visible = q.is_empty()
                            || title.contains(&q)
                            || sub.contains(&q)
                            || id.contains(&q);
                        row.set_visible(visible);
                    }
                    child = next;
                }
                // Keep selection on something visible (audit A1): a filter
                // hiding the selected row left the stack showing a hidden
                // category. Selecting fires row_selected, so the stack follows.
                let selected_visible = nav.selected_row().is_some_and(|r| r.is_visible());
                if !selected_visible {
                    let mut first: Option<ListBoxRow> = None;
                    let mut c = nav.first_child();
                    while let Some(w) = c {
                        let next = w.next_sibling();
                        if let Ok(row) = w.downcast::<ListBoxRow>() {
                            if row.is_visible() {
                                first = Some(row);
                                break;
                            }
                        }
                        c = next;
                    }
                    nav.select_row(first.as_ref());
                }
            });
        }

        nav_scroll.set_child(Some(&nav));
        nav_col.append(&nav_scroll);

        // Bottom close hint in nav
        let nav_footer = GtkBox::new(Orientation::Horizontal, 6);
        nav_footer.add_css_class("hark-settings-nav-footer");
        nav_footer.set_margin_start(12);
        nav_footer.set_margin_end(12);
        nav_footer.set_margin_top(6);
        nav_footer.set_margin_bottom(10);

        let esc = Label::new(Some("esc"));
        esc.add_css_class("hark-keycap");
        let close_hint = Label::new(Some("Close"));
        close_hint.add_css_class("hark-settings-nav-footer-label");
        close_hint.set_halign(gtk::Align::Start);
        close_hint.set_hexpand(true);

        let done = Button::with_label("Done");
        done.add_css_class("hark-settings-btn");
        done.add_css_class("hark-settings-primary");
        done.add_css_class("hark-settings-done");
        done.set_halign(gtk::Align::End);

        nav_footer.append(&esc);
        nav_footer.append(&close_hint);
        nav_footer.append(&done);
        nav_col.append(&nav_footer);

        split.append(&nav_col);
        split.append(&Separator::new(Orientation::Vertical));

        // --- Right content stack ---
        let content_stack = gtk::Stack::new();
        content_stack.add_css_class("hark-settings-content-stack");
        content_stack.set_hexpand(true);
        content_stack.set_vexpand(true);
        content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        content_stack.set_transition_duration(100);

        let cfg = engine.config().snapshot();

        let (indexing_page, status, sources_card) = build_indexing_page(&engine, &cfg);
        content_stack.add_named(&indexing_page, Some("indexing"));

        let folders_page = build_folders_page(&engine);
        content_stack.add_named(&folders_page, Some("folders"));

        let exclusions_page = build_exclusions_page(&engine);
        content_stack.add_named(&exclusions_page, Some("exclusions"));

        let dismiss_overlay: OnDoneBoolSlot = Rc::new(RefCell::new(None));

        let defaults_page = build_defaults_page(&engine, dismiss_overlay.clone());
        content_stack.add_named(&defaults_page, Some("defaults"));

        let display_page = build_display_page(&engine, &cfg);
        content_stack.add_named(&display_page, Some("display"));

        let appearance_page = build_appearance_page(&engine, &theme, &cfg);
        content_stack.add_named(&appearance_page, Some("appearance"));

        let typos_page = build_typos_page(&engine);
        content_stack.add_named(&typos_page, Some("typos"));

        let tools_page = build_tools_page(&engine, &cfg);
        content_stack.add_named(&tools_page, Some("tools"));

        content_stack.set_visible_child_name("indexing");
        split.append(&content_stack);
        root.append(&split);

        {
            let content_stack = content_stack.clone();
            let dismiss_overlay = dismiss_overlay.clone();
            let engine = engine.clone();
            let sources_card = sources_card.clone();
            // Only the Default-apps page can host an overlay (picker), so only
            // leaving it can have something to dismiss (audit A7).
            let last_page: Rc<RefCell<String>> = Rc::new(RefCell::new("indexing".into()));
            nav.connect_row_selected(move |_, row| {
                if let Some(row) = row {
                    let id = row.widget_name().to_string();
                    let mut last = last_page.borrow_mut();
                    if *last == "defaults" && id != "defaults" {
                        if let Some(cb) = dismiss_overlay.borrow().as_ref() {
                            let _ = cb();
                        }
                    }
                    *last = id.clone();
                    drop(last);
                    if id == "indexing" {
                        // Mounts come and go (USB) while the panel lives —
                        // rebuild from the live table on every visit (A5).
                        refill_sources_card(&sources_card, &engine);
                    }
                    if !id.is_empty() {
                        content_stack.set_visible_child_name(&id);
                    }
                }
            });
        }

        // Keyboard: ↑/↓ or j/k cycle categories; Home/End jump
        {
            let nav = nav.clone();
            let root_for_keys = root.clone();
            let key = EventControllerKey::new();
            key.set_propagation_phase(gtk::PropagationPhase::Capture);
            key.connect_key_pressed(move |_, keyval, _, _| {
                if root_for_keys
                    .root()
                    .and_then(|r| r.focus())
                    .and_downcast::<gtk::Editable>()
                    .is_some()
                {
                    return glib::Propagation::Proceed;
                }
                let n = CATEGORIES.len() as i32;
                if n == 0 {
                    return glib::Propagation::Proceed;
                }
                let cur = nav.selected_row().map(|r| r.index()).unwrap_or(0).max(0);
                // Direction-aware scan: row_at_index counts hidden
                // (filtered-out) rows, so step to the next *visible* row.
                // Returns None when no visible row exists in that direction.
                let step_visible = |start: i32, dir: i32| -> Option<i32> {
                    let mut i = start;
                    while (0..n).contains(&i) {
                        if let Some(row) = nav.row_at_index(i) {
                            if row.is_visible() {
                                return Some(i);
                            }
                        }
                        i += dir;
                    }
                    None
                };
                let next = match keyval {
                    Key::Down | Key::j | Key::J | Key::Page_Down => {
                        step_visible((cur + 1).min(n - 1), 1)
                            // Wrap so ↓ on the last visible row cycles to the top.
                            .or_else(|| step_visible(0, 1))
                    }
                    Key::Up | Key::k | Key::K | Key::Page_Up => step_visible((cur - 1).max(0), -1)
                        // Wrap so ↑ on the first visible row cycles to the bottom.
                        .or_else(|| step_visible(n - 1, -1)),
                    Key::Home => step_visible(0, 1),
                    Key::End => step_visible(n - 1, -1),
                    _ => None,
                };
                // No match at all → let the key propagate. Matched but every
                // row hidden / no visible target → don't swallow either.
                let Some(idx) = next else {
                    return if matches!(
                        keyval,
                        Key::Down
                            | Key::Up
                            | Key::j
                            | Key::J
                            | Key::k
                            | Key::K
                            | Key::Home
                            | Key::End
                            | Key::Page_Down
                            | Key::Page_Up
                    ) {
                        // Navigation key with nowhere to go — still stop so
                        // focus doesn't jump out of the list, but only when
                        // at least one row is visible (all-filtered lets it
                        // propagate to the search entry).
                        let any_visible =
                            (0..n).any(|i| nav.row_at_index(i).is_some_and(|r| r.is_visible()));
                        if any_visible {
                            glib::Propagation::Stop
                        } else {
                            glib::Propagation::Proceed
                        }
                    } else {
                        glib::Propagation::Proceed
                    };
                };
                if let Some(row) = nav.row_at_index(idx) {
                    nav.select_row(Some(&row));
                    row.grab_focus();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            root.add_controller(key);
        }

        let on_done: OnDoneSlot = Rc::new(RefCell::new(None));
        let fire_done: Rc<dyn Fn()> = Rc::new({
            let on_done = on_done.clone();
            move || {
                if let Some(cb) = on_done.borrow().as_ref() {
                    cb();
                }
            }
        });
        {
            let fire_done = fire_done.clone();
            done.connect_clicked(move |_| fire_done());
        }
        // The esc/Close hint is clickable too (audit A4): a static "Close"
        // label next to the button read as dead UI. Attached to the labels
        // only (not the footer box) so Done clicks can't double-fire.
        {
            let foot_click = gtk::GestureClick::new();
            let fire_done = fire_done.clone();
            foot_click.connect_pressed(move |_, _, _, _| fire_done());
            esc.add_controller(foot_click);
        }
        {
            let foot_click = gtk::GestureClick::new();
            let fire_done = fire_done.clone();
            foot_click.connect_pressed(move |_, _, _, _| fire_done());
            close_hint.add_controller(foot_click);
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

    /// Cloneable handle for window-level key capture (Esc).
    /// Closes nested settings UI (app picker, etc.); returns true if something was dismissed.
    pub fn dismiss_overlay_handle(&self) -> impl Fn() -> bool + 'static {
        let dismiss = self.dismiss_overlay.clone();
        move || dismiss.borrow().as_ref().map(|cb| cb()).unwrap_or(false)
    }

    pub fn refresh_status(&self) {
        self.status.set_text(&self.engine.format_index_status());
    }

    pub fn widget(&self) -> &GtkBox {
        &self.root
    }
}

/// Page frame: sticky title header + scrolling body. No header icon — the nav
/// already shows the selected category's icon, so repeating it here was noise
/// next to every title (audit B1).
fn page_shell(title: &str, subtitle: &str) -> (GtkBox, GtkBox) {
    let outer = GtkBox::new(Orientation::Vertical, 0);
    outer.add_css_class("hark-settings-page");
    outer.set_hexpand(true);
    outer.set_vexpand(true);

    // Header rides INSIDE the scrollable content, attached to the body 1:1:
    // it slides away at exactly content speed on scroll-down and reappears
    // proportionally on the slightest scroll-up. Pure scroll position — no
    // thresholds, no hide/show state — so no flicker, no hijacked feel on
    // trackpads or short pages (the Revealer attempt had all three: toggling
    // the header resized the viewport into a feedback loop, and per-emission
    // deltas never accumulated on smooth scroll input).
    let header = GtkBox::new(Orientation::Horizontal, 10);
    header.add_css_class("hark-settings-page-header");
    header.set_margin_start(16);
    header.set_margin_end(16);
    header.set_margin_top(16);
    header.set_margin_bottom(4);

    let head_text = GtkBox::new(Orientation::Vertical, 2);
    head_text.set_hexpand(true);

    let t = Label::new(Some(title));
    t.add_css_class("hark-settings-page-title");
    t.set_halign(gtk::Align::Start);
    t.set_xalign(0.0);

    let s = Label::new(Some(subtitle));
    s.add_css_class("hark-settings-page-sub");
    s.set_halign(gtk::Align::Start);
    s.set_xalign(0.0);
    s.set_wrap(true);

    head_text.append(&t);
    head_text.append(&s);
    header.append(&head_text);

    let scroll = ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(280)
        .max_content_height(420)
        .propagate_natural_height(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    // Always-visible scrollbars (audit A6): overlay indicators only appear on
    // hover, so clipped pages (Rebuild, Reset) gave no cue there was more
    // below. Matches the main results list, which also disables overlay.
    scroll.set_overlay_scrolling(false);

    let body = GtkBox::new(Orientation::Vertical, 14);
    body.add_css_class("hark-settings-body");
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(10);
    body.set_margin_bottom(16);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.append(&header);
    content.append(&body);

    scroll.set_child(Some(&content));

    // Scroll-edge fades (audit B5): rows scrolling under the viewport rim feel
    // wrong (screenshot: half-rows touching top/bottom). Two click-through
    // scrims fade in/out with scroll position — opacity only, never layout, so
    // this cannot feed back into the scroll itself. Short pages stay at 0.
    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    overlay.set_vexpand(true);
    overlay.set_child(Some(&scroll));

    let fade_top = GtkBox::new(Orientation::Horizontal, 0);
    fade_top.add_css_class("hark-fade-top");
    fade_top.set_halign(gtk::Align::Fill);
    fade_top.set_valign(gtk::Align::Start);
    fade_top.set_vexpand(false);
    fade_top.set_can_target(false);
    // Full-bleed: spans the complete window width, edge to edge.
    fade_top.set_opacity(0.0);

    let fade_bottom = GtkBox::new(Orientation::Horizontal, 0);
    fade_bottom.add_css_class("hark-fade-bottom");
    fade_bottom.set_halign(gtk::Align::Fill);
    fade_bottom.set_valign(gtk::Align::End);
    fade_bottom.set_vexpand(false);
    fade_bottom.set_can_target(false);
    fade_bottom.set_opacity(0.0);

    overlay.add_overlay(&fade_top);
    overlay.add_overlay(&fade_bottom);

    {
        scroll.vadjustment().connect_value_changed(move |adj| {
            const FADE_PX: f64 = 28.0;
            let value = adj.value();
            let max = (adj.upper() - adj.page_size()).max(0.0);
            let (top, bottom) = if max <= 1.0 {
                (0.0, 0.0)
            } else {
                (
                    (value / FADE_PX).clamp(0.0, 1.0),
                    ((max - value) / FADE_PX).clamp(0.0, 1.0),
                )
            };
            fade_top.set_opacity(top);
            fade_bottom.set_opacity(bottom);
        });
    }

    outer.append(&overlay);
    (outer, body)
}

fn build_indexing_page(
    engine: &Arc<Engine>,
    cfg: &crate::config::HarkConfig,
) -> (GtkBox, Label, GtkBox) {
    let (outer, body) = page_shell(
        "Indexing",
        "Choose which locations Hark searches and rebuild the file index.",
    );

    body.append(&group_label("Scan depth"));

    let depth_card = GtkBox::new(Orientation::Vertical, 0);
    depth_card.add_css_class("hark-settings-card");

    let depth_row = setting_row(
        "Levels from each root",
        Some(&depth_help_text(cfg.index.max_depth.clamp(1, 6))),
    );

    let stepper = GtkBox::new(Orientation::Horizontal, 4);
    stepper.set_valign(gtk::Align::Center);

    let depth_dec = Button::with_label("−");
    depth_dec.add_css_class("hark-settings-btn");
    depth_dec.add_css_class("hark-settings-icon-btn");
    depth_dec.set_tooltip_text(Some("Shallower (faster, fewer files)"));

    let depth_val = Label::new(Some(&format!("{}", cfg.index.max_depth.clamp(1, 6))));
    depth_val.add_css_class("hark-settings-stepper-val");
    depth_val.set_width_chars(2);
    depth_val.set_halign(gtk::Align::Center);

    let depth_inc = Button::with_label("+");
    depth_inc.add_css_class("hark-settings-btn");
    depth_inc.add_css_class("hark-settings-icon-btn");
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
    caps.add_css_class("hark-hint");
    caps.add_css_class("hark-settings-card-footer");
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
            let mut changed = false;
            engine.config().update(|c| {
                let d = c.index.max_depth.clamp(1, 6);
                next = d.saturating_sub(1).max(1);
                changed = next != d;
                c.index.max_depth = next;
            });
            depth_val.set_text(&format!("{next}"));
            if let Some(h) = &depth_hint {
                h.set_text(&depth_help_text(next));
            }
            if changed {
                engine.force_reindex();
            }
        });
    }
    {
        let engine = engine.clone();
        let depth_val = depth_val.clone();
        let depth_hint = depth_hint.clone();
        depth_inc.connect_clicked(move |_| {
            let mut next = 2usize;
            let mut changed = false;
            engine.config().update(|c| {
                let d = c.index.max_depth.clamp(1, 6);
                next = (d + 1).min(6);
                changed = next != d;
                c.index.max_depth = next;
            });
            depth_val.set_text(&format!("{next}"));
            if let Some(h) = &depth_hint {
                h.set_text(&depth_help_text(next));
            }
            if changed {
                engine.force_reindex();
            }
        });
    }

    body.append(&group_label("Sources"));

    let sources = GtkBox::new(Orientation::Vertical, 0);
    sources.add_css_class("hark-settings-card");
    refill_sources_card(&sources, engine);
    body.append(&sources);

    body.append(&group_label("Index"));

    let rebuild_row = GtkBox::new(Orientation::Vertical, 10);
    rebuild_row.add_css_class("hark-settings-card");

    let rebuild_head = setting_row(
        "Rebuild index now",
        Some("Force a full re-scan of all enabled sources."),
    );
    let rebuild = Button::with_label("Rebuild");
    rebuild.add_css_class("hark-settings-btn");
    rebuild.add_css_class("hark-settings-primary");
    rebuild.set_valign(gtk::Align::Center);
    rebuild_head.append(&rebuild);
    rebuild_row.append(&rebuild_head);

    let status = Label::new(Some(&engine.format_index_status()));
    status.add_css_class("hark-hint");
    status.add_css_class("hark-settings-card-footer");
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);
    rebuild_row.append(&Separator::new(Orientation::Horizontal));
    rebuild_row.append(&status);

    {
        let engine = engine.clone();
        let status = status.clone();
        let rebuild_btn = rebuild.clone();
        rebuild.connect_clicked(move |_| {
            // Guard double-click: force_reindex spawns a worker per click.
            if engine.index_progress().running {
                return;
            }
            rebuild_btn.set_sensitive(false);
            status.set_text("Indexing… 0 files");
            engine.force_reindex();
            let status = status.clone();
            let engine = engine.clone();
            let rebuild_btn = rebuild_btn.clone();
            glib_timeout_poll_index(engine, status, rebuild_btn, 0);
        });
    }

    body.append(&rebuild_row);

    (outer, status, sources)
}

/// (Re)build the Sources card from the live mount table + config snapshot.
/// Called once at construction and on every visit to Indexing (audit A5):
/// volumes come and go while the panel lives, and a stale list hides newly
/// plugged drives until restart.
fn refill_sources_card(card: &GtkBox, engine: &Arc<Engine>) {
    while let Some(c) = card.first_child() {
        card.remove(&c);
    }
    let cfg = engine.config().snapshot();
    let home_row = check_setting_row("Home directory (~)", None, cfg.index.include_home);
    {
        let engine = engine.clone();
        let cb = home_row.1.clone();
        cb.connect_toggled(move |btn| {
            // Source toggles must reindex immediately (audit P2): otherwise
            // results stay stale until the next periodic rebuild.
            engine.config().update(|c| {
                c.index.include_home = btn.is_active();
            });
            engine.force_reindex();
        });
    }
    card.append(&home_row.0);

    let mounts = discover_mounts();
    for m in mounts.iter() {
        card.append(&Separator::new(Orientation::Horizontal));
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
            .unwrap_or_else(|| default_mount_enabled(&m.target));
        let (row, cb) = check_setting_row(&label, None, enabled);
        {
            let engine = engine.clone();
            let key = key.clone();
            cb.connect_toggled(move |btn| {
                engine.config().update(|c| {
                    c.index.include_mounts.insert(key.clone(), btn.is_active());
                });
                engine.force_reindex();
            });
        }
        card.append(&row);
    }
}

fn build_typos_page(engine: &Arc<Engine>) -> GtkBox {
    let (outer, body) = page_shell(
        "Typo aliases",
        "Hark learns near-miss searches (e.g. wats → WhatsApp). Manage them here.",
    );

    let hint = Label::new(Some(
        "Aliases are learned automatically when you open a result after a typo or rewrite. \
Manual pins always rank strongly. Stored in ~/.local/state/hark/typos.json.",
    ));
    hint.add_css_class("hark-hint");
    hint.set_wrap(true);
    hint.set_halign(gtk::Align::Start);
    hint.set_margin_bottom(8);
    body.append(&hint);

    let list = GtkBox::new(Orientation::Vertical, 0);
    list.add_css_class("hark-settings-card");
    list.add_css_class("hark-settings-list");
    refill_typo_list(&list, engine);

    // Refresh when the page is shown again (after learning more typos).
    {
        let list = list.clone();
        let engine = engine.clone();
        outer.connect_map(move |_| {
            refill_typo_list(&list, &engine);
        });
    }

    let actions = GtkBox::new(Orientation::Horizontal, 8);
    actions.set_margin_top(4);
    actions.set_margin_bottom(10);
    let clear = Button::with_label("Forget all");
    clear.add_css_class("hark-settings-btn");
    clear.set_tooltip_text(Some("Remove every learned and manual alias"));
    {
        let engine = engine.clone();
        let list = list.clone();
        clear.connect_clicked(move |_| {
            engine.clear_typo_aliases();
            refill_typo_list(&list, &engine);
        });
    }
    actions.append(&clear);

    body.append(&list);
    body.append(&actions);

    body.append(&group_label("Add manually"));
    let add_hint = Label::new(Some(
        "Pin a typo to an app name or file path (e.g. typo “wats”, open “WhatsApp”).",
    ));
    add_hint.add_css_class("hark-hint");
    add_hint.set_wrap(true);
    add_hint.set_halign(gtk::Align::Start);
    add_hint.set_margin_bottom(6);
    body.append(&add_hint);

    let add_col = GtkBox::new(Orientation::Vertical, 6);
    let typo_entry = Entry::builder()
        .placeholder_text("When I type… (e.g. wats)")
        .hexpand(true)
        .build();
    typo_entry.add_css_class("hark-settings-entry");
    let target_entry = Entry::builder()
        .placeholder_text("Open… (app name or path)")
        .hexpand(true)
        .build();
    target_entry.add_css_class("hark-settings-entry");

    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    let status = Label::new(None);
    status.add_css_class("hark-hint");
    status.set_halign(gtk::Align::Start);
    status.set_hexpand(true);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_xalign(0.0);
    let add = Button::with_label("Add");
    add.add_css_class("hark-settings-btn");
    add.add_css_class("hark-settings-primary");
    {
        let engine = engine.clone();
        let typo_entry = typo_entry.clone();
        let target_entry = target_entry.clone();
        let list = list.clone();
        let status = status.clone();
        add.connect_clicked(move |_| {
            let alias = typo_entry.text().to_string();
            let target = target_entry.text().to_string();
            match engine.add_typo_alias(&alias, &target) {
                Ok(label) => {
                    status.set_text(&format!("Pinned → {label}"));
                    typo_entry.set_text("");
                    target_entry.set_text("");
                    refill_typo_list(&list, &engine);
                }
                Err(e) => status.set_text(&e),
            }
        });
    }
    add_row.append(&status);
    add_row.append(&add);

    add_col.append(&typo_entry);
    add_col.append(&target_entry);
    add_col.append(&add_row);
    body.append(&add_col);

    outer
}

fn refill_typo_list(list: &GtkBox, engine: &Arc<Engine>) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let items = engine.list_typo_aliases();
    if items.is_empty() {
        let empty = Label::new(Some(
            "No aliases yet — mistype, open the right result, and Hark will learn.",
        ));
        empty.add_css_class("hark-hint");
        empty.add_css_class("hark-settings-list-row");
        empty.set_halign(gtk::Align::Start);
        empty.set_wrap(true);
        list.append(&empty);
        return;
    }
    for (i, a) in items.iter().enumerate() {
        if i > 0 {
            list.append(&Separator::new(Orientation::Horizontal));
        }
        list.append(&typo_alias_row(a, engine));
    }
}

fn typo_alias_row(alias: &crate::typos::TypoAlias, engine: &Arc<Engine>) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    row.add_css_class("hark-settings-list-row");

    let text_col = GtkBox::new(Orientation::Vertical, 2);
    text_col.set_hexpand(true);
    text_col.set_halign(gtk::Align::Start);

    let target_name = engine.result_display_name(&alias.id);
    let title = Label::new(Some(&format!("{}  →  {}", alias.alias, target_name)));
    title.set_halign(gtk::Align::Start);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("hark-settings-list-label");

    let strength = if alias.count >= 2 {
        "strong"
    } else {
        "learning"
    };
    let sub = Label::new(Some(&format!(
        "{strength} · seen {}× · {}",
        alias.count, alias.id
    )));
    sub.set_halign(gtk::Align::Start);
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    sub.add_css_class("hark-hint");

    text_col.append(&title);
    text_col.append(&sub);

    let rm = Button::with_label("×");
    rm.add_css_class("hark-settings-btn");
    rm.add_css_class("hark-settings-icon-btn");
    rm.set_tooltip_text(Some("Forget this alias"));
    {
        let engine = engine.clone();
        let key = alias.alias.clone();
        // Weak capture: the button lives inside `row`, so a strong `row`
        // here cycles row → button → handler → row and leaks the subtree
        // after detach (audit P3, same class as open_with popover cycles).
        let row_w = row.downgrade();
        rm.connect_clicked(move |_| {
            engine.remove_typo_alias(&key);
            let Some(row) = row_w.upgrade() else {
                return;
            };
            if let Some(parent) = row.parent() {
                if let Ok(box_) = parent.downcast::<GtkBox>() {
                    if let Some(prev) = row.prev_sibling() {
                        if prev.type_().name() == "GtkSeparator" {
                            box_.remove(&prev);
                        }
                    } else if let Some(next) = row.next_sibling() {
                        if next.type_().name() == "GtkSeparator" {
                            box_.remove(&next);
                        }
                    }
                    box_.remove(&row);
                    // If list empty, refill empty state
                    if box_.first_child().is_none() {
                        refill_typo_list(&box_, &engine);
                    }
                }
            }
        });
    }

    row.append(&text_col);
    row.append(&rm);
    row
}

fn build_folders_page(engine: &Arc<Engine>) -> GtkBox {
    let (outer, body) = page_shell(
        "Extra folders",
        "Add folders outside home/mounts. They are indexed at the same depth.",
    );

    let list = GtkBox::new(Orientation::Vertical, 0);
    list.add_css_class("hark-settings-card");
    list.add_css_class("hark-settings-list");
    refill_extra_list(&list, engine);

    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    add_row.set_margin_top(2);
    let entry = Entry::builder()
        .placeholder_text("/path/to/folder")
        .hexpand(true)
        .build();
    entry.add_css_class("hark-settings-entry");
    let add = Button::with_label("Add");
    add.add_css_class("hark-settings-btn");
    add.add_css_class("hark-settings-primary");
    let extra_status = Label::new(None);
    extra_status.add_css_class("hark-hint");
    extra_status.set_halign(gtk::Align::Start);
    extra_status.set_wrap(true);
    extra_status.set_margin_top(4);
    {
        let engine = engine.clone();
        let entry_cb = entry.clone();
        let list_cb = list.clone();
        let extra_status_cb = extra_status.clone();
        let do_add = Rc::new(move || {
            let raw = entry_cb.text().to_string().trim().to_string();
            if raw.is_empty() {
                extra_status_cb.set_text("Enter a folder path");
                return;
            }
            // Normalize like the pin path does so `~/x` and `/home/u/x`
            // can't both be stored as separate rows. Reject relative and
            // `~otheruser` forms (audit P3): they are silently ignored
            // downstream, and `update` sanitizing would drop them anyway.
            let p = crate::providers::files::expand_user_path(&raw)
                .to_string_lossy()
                .to_string();
            if !std::path::Path::new(&p).is_absolute() {
                extra_status_cb.set_text("Path must be absolute (e.g. /mnt/data/projects)");
                return;
            }
            if engine
                .config()
                .snapshot()
                .index
                .extra_roots
                .iter()
                .any(|r| r == &p)
            {
                extra_status_cb.set_text("Already listed");
                entry_cb.set_text("");
                return;
            }
            let mut changed = false;
            engine.config().update(|c| {
                if !c.index.extra_roots.iter().any(|r| r == &p) {
                    c.index.extra_roots.push(p.clone());
                    changed = true;
                }
            });
            if changed {
                engine.force_reindex();
            }
            extra_status_cb.set_text("");
            entry_cb.set_text("");
            refill_extra_list(&list_cb, &engine);
        });
        {
            let do_add = do_add.clone();
            add.connect_clicked(move |_| do_add());
        }
        {
            let do_add = do_add.clone();
            entry.clone().connect_activate(move |_| do_add());
        }
    }
    add_row.append(&entry);
    add_row.append(&add);

    body.append(&list);
    body.append(&add_row);
    body.append(&extra_status);

    // Deep roots — always indexed to depth 6, preferred by live deep search.
    body.append(&group_label("Deep roots"));
    let deep_hint = Label::new(Some(
        "Pinned folders always get depth 6 in the index and are preferred for live deep search. \
         Opening a deep file can auto-promote its parent project folder.",
    ));
    deep_hint.add_css_class("hark-hint");
    deep_hint.set_wrap(true);
    deep_hint.set_halign(gtk::Align::Start);
    deep_hint.set_margin_bottom(6);
    body.append(&deep_hint);

    let deep_list = GtkBox::new(Orientation::Vertical, 0);
    deep_list.add_css_class("hark-settings-card");
    deep_list.add_css_class("hark-settings-list");
    refill_deep_list(&deep_list, engine);

    let deep_add_row = GtkBox::new(Orientation::Horizontal, 8);
    deep_add_row.set_margin_top(2);
    let deep_entry = Entry::builder()
        .placeholder_text("~/projects/my-app")
        .hexpand(true)
        .build();
    deep_entry.add_css_class("hark-settings-entry");
    let deep_add = Button::with_label("Pin");
    deep_add.add_css_class("hark-settings-btn");
    deep_add.add_css_class("hark-settings-primary");
    let deep_status = Label::new(None);
    deep_status.add_css_class("hark-hint");
    deep_status.set_halign(gtk::Align::Start);
    deep_status.set_wrap(true);
    deep_status.set_margin_top(4);
    {
        let engine = engine.clone();
        let deep_entry_cb = deep_entry.clone();
        let deep_list_cb = deep_list.clone();
        let deep_status_cb = deep_status.clone();
        let do_pin = Rc::new(move || {
            let p = deep_entry_cb.text().to_string().trim().to_string();
            if p.is_empty() {
                deep_status_cb.set_text("Enter a folder path");
                return;
            }
            let path = crate::providers::files::expand_user_path(&p);
            match engine.promote_deep_root(&path) {
                Ok(_) => {
                    deep_status_cb.set_text("");
                    deep_entry_cb.set_text("");
                    refill_deep_list(&deep_list_cb, &engine);
                }
                Err(e) => deep_status_cb.set_text(&e),
            }
        });
        {
            let do_pin = do_pin.clone();
            deep_add.connect_clicked(move |_| do_pin());
        }
        {
            let do_pin = do_pin.clone();
            deep_entry.clone().connect_activate(move |_| do_pin());
        }
    }
    deep_add_row.append(&deep_entry);
    deep_add_row.append(&deep_add);

    body.append(&deep_list);
    body.append(&deep_add_row);
    body.append(&deep_status);
    outer
}

fn build_exclusions_page(engine: &Arc<Engine>) -> GtkBox {
    let (outer, body) = page_shell(
        "Exclusions",
        "Folders or path fragments that are never indexed (e.g. node_modules, .git).",
    );

    let list = GtkBox::new(Orientation::Vertical, 0);
    list.add_css_class("hark-settings-card");
    list.add_css_class("hark-settings-list");
    refill_exclude_list(&list, engine);

    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    add_row.set_margin_top(2);
    let entry = Entry::builder()
        .placeholder_text("name or path fragment")
        .hexpand(true)
        .build();
    entry.add_css_class("hark-settings-entry");
    let add = Button::with_label("Add");
    add.add_css_class("hark-settings-btn");
    add.add_css_class("hark-settings-primary");
    let excl_status = Label::new(None);
    excl_status.add_css_class("hark-hint");
    excl_status.set_halign(gtk::Align::Start);
    excl_status.set_wrap(true);
    excl_status.set_margin_top(4);
    {
        let engine = engine.clone();
        let entry_cb = entry.clone();
        let list_cb = list.clone();
        let excl_status_cb = excl_status.clone();
        let do_add = Rc::new(move || {
            let p = entry_cb.text().to_string().trim().to_string();
            if p.is_empty() {
                excl_status_cb.set_text("Enter a name or path fragment");
                return;
            }
            if engine
                .config()
                .snapshot()
                .index
                .exclude
                .iter()
                .any(|x| x == &p)
            {
                excl_status_cb.set_text("Already excluded");
                entry_cb.set_text("");
                return;
            }
            let mut changed = false;
            engine.config().update(|c| {
                if !c.index.exclude.contains(&p) {
                    c.index.exclude.push(p.clone());
                    changed = true;
                }
            });
            if changed {
                engine.force_reindex();
            }
            excl_status_cb.set_text("");
            entry_cb.set_text("");
            refill_exclude_list(&list_cb, &engine);
        });
        {
            let do_add = do_add.clone();
            add.connect_clicked(move |_| do_add());
        }
        {
            let do_add = do_add.clone();
            entry.clone().connect_activate(move |_| do_add());
        }
    }
    add_row.append(&entry);
    add_row.append(&add);

    body.append(&list);
    body.append(&add_row);
    body.append(&excl_status);
    outer
}

fn build_defaults_page(engine: &Arc<Engine>, dismiss_overlay: OnDoneBoolSlot) -> GtkBox {
    let (outer, body) = page_shell(
        "Default apps",
        "Choose which app Hark uses for each file kind. Empty means system default (xdg-open).",
    );

    // Host stack: list page ↔ in-panel app picker (no extra Window — layer-shell exclusive
    // keyboard grab cannot focus a separate modal, which deadlocks Esc / interaction).
    let host = gtk::Stack::new();
    host.add_css_class("hark-settings-defaults-host");
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
    card.add_css_class("hark-settings-card");

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
        "These overrides apply only inside Hark — they do not change system MIME defaults.",
    ));
    hint.add_css_class("hark-hint");
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
    row.add_css_class("hark-settings-list-row");
    row.set_hexpand(true);

    let icon = Image::from_icon_name(cat.icon());
    icon.set_pixel_size(18);
    icon.set_valign(gtk::Align::Center);

    let text = GtkBox::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_halign(gtk::Align::Start);
    text.set_valign(gtk::Align::Center);

    let title = Label::new(Some(cat.label()));
    title.add_css_class("hark-settings-list-label");
    title.set_halign(gtk::Align::Start);
    title.set_xalign(0.0);

    let current = engine
        .config()
        .snapshot()
        .open_with
        .get(cat)
        .map(|s| s.to_string());
    let sub_text = format_open_with_label(engine, current.as_deref());
    let sub = Label::new(Some(&sub_text));
    sub.add_css_class("hark-settings-list-sub");
    sub.set_halign(gtk::Align::Start);
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_max_width_chars(36);
    sub.set_tooltip_text(Some(cat.subtitle()));

    text.append(&title);
    text.append(&sub);

    let choose = Button::with_label("Choose…");
    choose.add_css_class("hark-settings-btn");
    choose.set_valign(gtk::Align::Center);

    let reset = Button::with_label("System");
    reset.add_css_class("hark-settings-btn");
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
            Some(name) => format!("{name} (Hark)"),
            None => format!("{id} (Hark)"),
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
    root.add_css_class("hark-settings-picker");
    root.set_hexpand(true);
    root.set_vexpand(true);

    let top = GtkBox::new(Orientation::Horizontal, 8);
    top.set_margin_bottom(8);

    let back = Button::with_label("← Back");
    back.add_css_class("hark-settings-btn");
    back.set_halign(gtk::Align::Start);

    let head = Label::new(Some(&format!(
        "Open {} with…",
        cat.label().to_ascii_lowercase()
    )));
    head.add_css_class("hark-settings-page-title");
    head.set_halign(gtk::Align::Start);
    head.set_hexpand(true);
    head.set_xalign(0.0);

    top.append(&back);
    top.append(&head);

    let sub = Label::new(Some(cat.subtitle()));
    sub.add_css_class("hark-hint");
    sub.set_halign(gtk::Align::Start);
    sub.set_margin_bottom(8);

    let search = Entry::builder()
        .placeholder_text("Filter apps…")
        .hexpand(true)
        .build();
    search.add_css_class("hark-settings-search");
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
    list.add_css_class("hark-settings-nav");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);

    // System default row first
    {
        let row = ListBoxRow::new();
        row.add_css_class("hark-settings-nav-row");
        row.set_widget_name("__system__");
        let item = GtkBox::new(Orientation::Horizontal, 10);
        item.set_margin_start(10);
        item.set_margin_end(10);
        item.set_margin_top(8);
        item.set_margin_bottom(8);
        let icon = Image::from_icon_name("emblem-system-symbolic");
        icon.set_pixel_size(18);
        let name = Label::new(Some("System default"));
        name.add_css_class("hark-settings-nav-title");
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
        row.add_css_class("hark-settings-nav-row");
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

        let icon = Image::from_icon_name("application-x-executable");
        // Manual installs often use absolute Icon= paths — use shared loader.
        super::rows::apply_result_icon(
            &icon,
            if app.icon.is_empty() {
                None
            } else {
                Some(app.icon.as_str())
            },
            crate::providers::ResultKind::App,
            false,
            18,
        );
        icon.set_valign(gtk::Align::Center);

        let name = Label::new(Some(&app.name));
        name.add_css_class("hark-settings-nav-title");
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

fn build_display_page(engine: &Arc<Engine>, cfg: &crate::config::HarkConfig) -> GtkBox {
    let (outer, body) = page_shell(
        "Display",
        "Control how indexed paths appear in search results.",
    );

    body.append(&group_label("Path format"));

    let style_card = GtkBox::new(Orientation::Vertical, 0);
    style_card.add_css_class("hark-settings-card");

    let label_style = CheckButton::with_label("Label  ·  Projects:/path");
    let drive_style = CheckButton::with_label("Drive  ·  D:/path");
    label_style.add_css_class("hark-settings-radio");
    drive_style.add_css_class("hark-settings-radio");
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
    label_box.add_css_class("hark-settings-list-row");
    label_box.append(&label_style);

    let drive_box = GtkBox::new(Orientation::Vertical, 2);
    drive_box.add_css_class("hark-settings-list-row");
    drive_box.append(&drive_style);

    style_card.append(&label_box);
    style_card.append(&Separator::new(Orientation::Horizontal));
    style_card.append(&drive_box);
    body.append(&style_card);

    let hint = Label::new(Some(
        "Label uses friendly mount names. Drive uses letter-style prefixes when available.",
    ));
    hint.add_css_class("hark-hint");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    body.append(&hint);

    outer
}

/// Horizontal setting row: title (+ optional subtitle) on the left, control on the right.
fn setting_row(title: &str, subtitle: Option<&str>) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 12);
    row.add_css_class("hark-settings-list-row");
    row.set_hexpand(true);

    let text = GtkBox::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_halign(gtk::Align::Start);
    text.set_valign(gtk::Align::Center);

    let t = Label::new(Some(title));
    t.add_css_class("hark-settings-list-label");
    t.set_halign(gtk::Align::Start);
    t.set_xalign(0.0);
    // Long values (e.g. mount labels) must truncate, not push the row's
    // control out of the card (audit B2).
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&t);

    if let Some(sub) = subtitle {
        let s = Label::new(Some(sub));
        s.add_css_class("hark-settings-list-sub");
        s.set_halign(gtk::Align::Start);
        s.set_xalign(0.0);
        s.set_wrap(true);
        // No max_width_chars cap (audit B3): the old 48-char cap wrapped
        // subtitles earlier than the available width, stretching cards
        // (e.g. Tools auto-detect to three lines). Wrap at allocation.
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
    cb.add_css_class("hark-settings-check");
    cb.update_property(&[gtk::accessible::Property::Label(title)]);
    row.append(&cb);
    // Whole-row click toggles (audit A3): the bare 20px box was the only hit
    // target and the title/subtitle area was dead. Capture + claim runs before
    // the CheckButton's own click handling, so a direct hit on the box cannot
    // double-toggle. Keyboard (Tab + Space) still works natively on the box.
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let cb_w = cb.downgrade();
    click.connect_pressed(move |gesture, _, _, _| {
        if let Some(cb) = cb_w.upgrade() {
            cb.set_active(!cb.is_active());
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    row.add_controller(click);
    (row, cb)
}

fn group_label(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("hark-settings-section");
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
    let roots = engine.config().snapshot().index.extra_roots.clone();
    if roots.is_empty() {
        let empty = Label::new(Some("No extra folders yet"));
        empty.add_css_class("hark-hint");
        empty.add_css_class("hark-settings-list-row");
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
    let roots = engine.config().snapshot().index.deep_roots.clone();
    if roots.is_empty() {
        let empty = Label::new(Some("No deep roots yet — pin a project folder"));
        empty.add_css_class("hark-hint");
        empty.add_css_class("hark-settings-list-row");
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
    let items = engine.config().snapshot().index.exclude.clone();
    if items.is_empty() {
        let empty = Label::new(Some("No exclusions"));
        empty.add_css_class("hark-hint");
        empty.add_css_class("hark-settings-list-row");
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
    row.add_css_class("hark-settings-list-row");
    let lab = Label::new(Some(text));
    lab.set_halign(gtk::Align::Start);
    lab.set_hexpand(true);
    lab.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    lab.add_css_class("hark-settings-list-label");
    let rm = Button::with_label("×");
    rm.add_css_class("hark-settings-btn");
    rm.add_css_class("hark-settings-icon-btn");
    {
        let engine = engine.clone();
        let text = text.to_string();
        // Weak capture: the button lives inside `row`, so a strong `row`
        // here cycles row → button → handler → row and leaks the subtree.
        let row_w = row.downgrade();
        rm.connect_clicked(move |_| {
            match kind {
                ListKind::Extra => {
                    let mut changed = false;
                    engine.config().update(|c| {
                        let before = c.index.extra_roots.len();
                        c.index.extra_roots.retain(|x| x != &text);
                        changed = c.index.extra_roots.len() != before;
                    });
                    if changed {
                        engine.force_reindex();
                    }
                }
                ListKind::Exclude => {
                    let mut changed = false;
                    engine.config().update(|c| {
                        let before = c.index.exclude.len();
                        c.index.exclude.retain(|x| x != &text);
                        changed = c.index.exclude.len() != before;
                    });
                    if changed {
                        engine.force_reindex();
                    }
                }
                ListKind::Deep => {
                    engine.remove_deep_root(&text);
                }
            }
            let Some(row) = row_w.upgrade() else {
                return;
            };
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
                        empty.add_css_class("hark-hint");
                        empty.add_css_class("hark-settings-list-row");
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

fn glib_timeout_poll_index(engine: Arc<Engine>, status: Label, rebuild: Button, n: u32) {
    // 200ms * 9000 = 30min safety cap. Was 60s then froze at "Indexing…" —
    // depth 6 + Windows D easily exceeds 60s. Poll until the worker reports
    // done, then re-enable Rebuild.
    if n > 9000 {
        status.set_text(&engine.format_index_status());
        rebuild.set_sensitive(true);
        return;
    }
    glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
        status.set_text(&engine.format_index_status());
        if engine.index_progress().running {
            glib_timeout_poll_index(engine, status, rebuild, n + 1);
        } else {
            rebuild.set_sensitive(true);
        }
    });
}

// N14 guard removed: accent entry now commits on Enter/blur (see
// commit_entry_on_idle) so preset set_text no longer re-fires a handler.

fn build_appearance_page(
    engine: &Arc<Engine>,
    theme: &Rc<ThemeManager>,
    cfg: &crate::config::HarkConfig,
) -> GtkBox {
    let (outer, body) = page_shell(
        "Appearance",
        "Tweak layout density, transparency, accent colour, type scale, and icons. \
         Colours still follow your Caelestia scheme.",
    );

    let ui = cfg.ui.clone();

    // --- Opacity ---
    body.append(&group_label("Panel"));

    let panel_card = GtkBox::new(Orientation::Vertical, 0);
    panel_card.add_css_class("hark-settings-card");

    let compact_active = matches!(ui.layout_mode, LayoutMode::Compact);
    let (layout_row, layout_cb) = check_setting_row(
        "Compact layout",
        Some("Search bar + footer only until you type (Raycast compact)"),
        compact_active,
    );
    {
        let engine = engine.clone();
        layout_cb.connect_toggled(move |btn| {
            let compact = btn.is_active();
            engine.config().update(|c| {
                c.ui.layout_mode = if compact {
                    LayoutMode::Compact
                } else {
                    LayoutMode::Expanded
                };
            });
        });
    }
    panel_card.append(&layout_row);
    panel_card.append(&Separator::new(Orientation::Horizontal));

    let opacity_row = setting_row(
        "Transparency",
        Some(&format!("{:.0}% opaque", ui.opacity * 100.0)),
    );
    let opacity_stepper = GtkBox::new(Orientation::Horizontal, 4);
    opacity_stepper.set_valign(gtk::Align::Center);
    let op_dec = Button::with_label("−");
    op_dec.add_css_class("hark-settings-btn");
    op_dec.add_css_class("hark-settings-icon-btn");
    let op_val = Label::new(Some(&format!("{:.0}%", ui.opacity * 100.0)));
    op_val.add_css_class("hark-settings-stepper-val");
    op_val.set_width_chars(4);
    let op_inc = Button::with_label("+");
    op_inc.add_css_class("hark-settings-btn");
    op_inc.add_css_class("hark-settings-icon-btn");
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
    r_dec.add_css_class("hark-settings-btn");
    r_dec.add_css_class("hark-settings-icon-btn");
    let r_val = Label::new(Some(&format!("{}", ui.radius)));
    r_val.add_css_class("hark-settings-stepper-val");
    r_val.set_width_chars(3);
    let r_inc = Button::with_label("+");
    r_inc.add_css_class("hark-settings-btn");
    r_inc.add_css_class("hark-settings-icon-btn");
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
    colour_card.add_css_class("hark-settings-card");

    let accent_row = setting_row("Accent override", Some("Empty = Caelestia primary"));
    let accent_entry = Entry::builder()
        .placeholder_text("#7aa2f7")
        .hexpand(false)
        .width_chars(10)
        .build();
    accent_entry.add_css_class("hark-settings-entry");
    if let Some(a) = &ui.accent {
        accent_entry.set_text(a);
    }
    accent_row.append(&accent_entry);
    colour_card.append(&accent_row);

    // Commit on Enter/blur (not per keystroke): typing "#7aa2f7" passes
    // through invalid prefixes ("#", "#7") which sanitize() would clear to
    // None, flickering the accent + rewriting config on every key.
    {
        let engine = engine.clone();
        let theme = theme.clone();
        commit_entry_on_idle(&accent_entry, move |text| {
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
    presets.add_css_class("hark-settings-list-row");
    let preset_label = Label::new(Some("Quick accents"));
    preset_label.add_css_class("hark-settings-row-title");
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
        btn.add_css_class("hark-settings-btn");
        btn.add_css_class("hark-settings-link");
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
    type_card.add_css_class("hark-settings-card");

    let font_row = setting_row(
        "Font scale",
        Some(&format!("{:.0}%", ui.font_scale * 100.0)),
    );
    let font_stepper = GtkBox::new(Orientation::Horizontal, 4);
    let f_dec = Button::with_label("−");
    f_dec.add_css_class("hark-settings-btn");
    f_dec.add_css_class("hark-settings-icon-btn");
    let f_val = Label::new(Some(&format!("{:.0}%", ui.font_scale * 100.0)));
    f_val.add_css_class("hark-settings-stepper-val");
    f_val.set_width_chars(4);
    let f_inc = Button::with_label("+");
    f_inc.add_css_class("hark-settings-btn");
    f_inc.add_css_class("hark-settings-icon-btn");
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
    i_dec.add_css_class("hark-settings-btn");
    i_dec.add_css_class("hark-settings-icon-btn");
    let i_val = Label::new(Some(&format!("{}", ui.icon_size)));
    i_val.add_css_class("hark-settings-stepper-val");
    i_val.set_width_chars(3);
    let i_inc = Button::with_label("+");
    i_inc.add_css_class("hark-settings-btn");
    i_inc.add_css_class("hark-settings-icon-btn");
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
            super::rows::clear_icon_resolve_cache();
            theme.reload();
        });
    }
    type_card.append(&sym_row);

    body.append(&type_card);

    // Reset
    body.append(&group_label("Reset"));
    let reset_card = GtkBox::new(Orientation::Vertical, 0);
    reset_card.add_css_class("hark-settings-card");
    let reset_row = setting_row(
        "Restore defaults",
        Some("Opacity, accent, font, icons, radius, layout"),
    );
    let reset_btn = Button::with_label("Reset appearance");
    reset_btn.add_css_class("hark-settings-btn");
    {
        let engine = engine.clone();
        let theme = theme.clone();
        let accent_entry = accent_entry.clone();
        let op_val = op_val.clone();
        let r_val = r_val.clone();
        let f_val = f_val.clone();
        let i_val = i_val.clone();
        let layout_cb = layout_cb.clone();
        let sym_cb = sym_cb.clone();
        let opacity_hint = opacity_hint.clone();
        let radius_hint = radius_hint.clone();
        let font_hint = font_hint.clone();
        let icon_hint = icon_hint.clone();
        reset_btn.connect_clicked(move |_| {
            let def = UiThemeConfig::default();
            engine.config().update(|c| c.ui = def.clone());
            // Idle-commit model: set_text does not fire a handler, so no
            // double config write here (was N14 double with live-changed).
            accent_entry.set_text("");
            op_val.set_text(&format!("{:.0}%", def.opacity * 100.0));
            if let Some(h) = &opacity_hint {
                h.set_text(&format!("{:.0}% opaque", def.opacity * 100.0));
            }
            r_val.set_text(&format!("{}", def.radius));
            if let Some(h) = &radius_hint {
                h.set_text(&format!("{}px", def.radius));
            }
            f_val.set_text(&format!("{:.0}%", def.font_scale * 100.0));
            if let Some(h) = &font_hint {
                h.set_text(&format!("{:.0}%", def.font_scale * 100.0));
            }
            i_val.set_text(&format!("{}", def.icon_size));
            if let Some(h) = &icon_hint {
                h.set_text(&format!("{}px", def.icon_size));
            }
            let want_compact = matches!(def.layout_mode, LayoutMode::Compact);
            if layout_cb.is_active() != want_compact {
                layout_cb.set_active(want_compact);
            }
            // GTK only emits `toggled` on actual change — force the checkbox
            // back in sync with the reset config.
            if sym_cb.is_active() != def.symbolic_icons {
                sym_cb.set_active(def.symbolic_icons);
            }
            theme.reload();
        });
    }
    reset_row.append(&reset_btn);
    reset_card.append(&reset_row);
    body.append(&reset_card);

    let note = Label::new(Some(
        "Base colours come from Caelestia (~/.local/state/caelestia/scheme.json). \
         Accent override only changes the highlight colour. Icon size applies on the next \
         search refresh.",
    ));
    note.add_css_class("hark-hint");
    note.set_halign(gtk::Align::Start);
    note.set_wrap(true);
    body.append(&note);

    outer
}

/// Commit an entry's text on Enter or focus loss instead of per keystroke
/// (audit P3): typing an endpoint char-by-char can persist a half-typed
/// (sanitize-cleared) value if the daemon exits mid-typing, and every
/// api_key keystroke rewrites the secret to disk. Initial `set_text` runs
/// before handlers attach, so programmatic fills are unaffected.
fn commit_entry_on_idle(entry: &Entry, commit: impl Fn(String) + 'static) {
    let commit = Rc::new(commit);
    {
        let entry = entry.clone();
        let commit = commit.clone();
        entry
            .clone()
            .connect_activate(move |_| commit(entry.text().to_string()));
    }
    let focus = EventControllerFocus::new();
    {
        let entry = entry.clone();
        let commit = commit.clone();
        focus.connect_leave(move |_| commit(entry.text().to_string()));
    }
    entry.add_controller(focus);
}

fn build_tools_page(engine: &Arc<Engine>, cfg: &crate::config::HarkConfig) -> GtkBox {
    let (outer, body) = page_shell(
        "Tools",
        "Optional helpers. Turning a tool off stops all related background work.",
    );

    body.append(&group_label("Translation"));

    let card = GtkBox::new(Orientation::Vertical, 0);
    card.add_css_class("hark-settings-card");

    // Transient inline errors (e.g. rejected endpoint). Shared by the commit
    // handlers below; cleared on the next successful commit.
    let tools_status = Label::new(None);
    tools_status.add_css_class("hark-hint");
    tools_status.set_halign(gtk::Align::Start);
    tools_status.set_wrap(true);
    tools_status.set_margin_top(4);

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
        "Auto-detect foreign-script paste",
        Some(
            "CJK, Cyrillic, Arabic, Hindi, Thai, Greek, Hebrew, … without a tr prefix. \
             Ignored when translation is disabled.",
        ),
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
    let target_row = setting_row(
        "Target language",
        Some("BCP-47 code, e.g. en / es / zh / ja / hi / ar / ru"),
    );
    let target_entry = Entry::builder()
        .placeholder_text("en")
        .hexpand(false)
        .width_chars(8)
        .build();
    target_entry.add_css_class("hark-settings-entry");
    target_entry.set_text(&cfg.translate.target_lang);
    target_row.append(&target_entry);
    card.append(&target_row);
    {
        let engine = engine.clone();
        let target_entry = target_entry.clone();
        let tools_status = tools_status.clone();
        commit_entry_on_idle(&target_entry.clone(), move |text| {
            engine.config().update(|c| {
                c.translate.target_lang = text;
            });
            // Reflect sanitize (lowercase/strip) back so the field never shows
            // a value that isn't stored (audit J5). set_text is safe here:
            // idle-commit only fires on Enter/blur, not on set_text.
            let saved = engine.config().snapshot().translate.target_lang.clone();
            if target_entry.text().as_str() != saved {
                target_entry.set_text(&saved);
            }
            tools_status.set_text("");
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
    ep_entry.add_css_class("hark-settings-entry");
    ep_entry.set_text(&cfg.translate.endpoint);
    // Field truncates long URLs (audit S1) — full value on hover.
    if cfg.translate.endpoint.is_empty() {
        ep_entry.set_tooltip_text(None);
    } else {
        ep_entry.set_tooltip_text(Some(&cfg.translate.endpoint));
    }
    ep_row.append(&ep_entry);
    card.append(&ep_row);
    {
        let engine = engine.clone();
        let ep_entry = ep_entry.clone();
        let tools_status = tools_status.clone();
        commit_entry_on_idle(&ep_entry.clone(), move |text| {
            let t = text.trim().to_string();
            // Inline validation (audit J5): sanitize() would silently clear a
            // bad endpoint on next load; refuse here with a reason instead.
            if let Err(e) = crate::config::validate_translate_endpoint(&t) {
                tools_status.set_text(&format!("Endpoint ignored: {e}"));
                return;
            }
            engine.config().update(|c| {
                c.translate.endpoint = t;
            });
            let saved = engine.config().snapshot().translate.endpoint.clone();
            if ep_entry.text().as_str() != saved {
                ep_entry.set_text(&saved);
            }
            if saved.is_empty() {
                ep_entry.set_tooltip_text(None);
            } else {
                ep_entry.set_tooltip_text(Some(&saved));
            }
            tools_status.set_text("");
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
    key_entry.add_css_class("hark-settings-entry");
    if let Some(k) = &cfg.translate.api_key {
        key_entry.set_text(k);
    }
    key_row.append(&key_entry);
    card.append(&key_row);
    {
        let engine = engine.clone();
        commit_entry_on_idle(&key_entry, move |text| {
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

    // Dependent rows dim when the master switch is off (audit J3): the
    // auto-detect subtitle already says "Ignored when translation is
    // disabled" but the rows stayed fully sensitive, inviting dead edits.
    // Sensitivity propagates from the row box to its children.
    auto_row.set_sensitive(cfg.translate.enabled);
    target_row.set_sensitive(cfg.translate.enabled);
    ep_row.set_sensitive(cfg.translate.enabled);
    key_row.set_sensitive(cfg.translate.enabled);
    {
        let auto_row = auto_row.clone();
        let target_row = target_row.clone();
        let ep_row = ep_row.clone();
        let key_row = key_row.clone();
        en_cb.connect_toggled(move |btn| {
            let on = btn.is_active();
            auto_row.set_sensitive(on);
            target_row.set_sensitive(on);
            ep_row.set_sensitive(on);
            key_row.set_sensitive(on);
        });
    }

    body.append(&card);
    body.append(&tools_status);

    let note = Label::new(Some(
        "Paste non-Latin text (or type tr … / tr en es Hello). Shows Translating… then fills \
         in (network off the UI thread). Empty endpoint uses free Google/MyMemory. Prefer local \
         LibreTranslate for privacy. Explicit direction: tr <src> <tgt> <text>.",
    ));
    note.add_css_class("hark-hint");
    note.set_halign(gtk::Align::Start);
    note.set_wrap(true);
    body.append(&note);

    outer
}
