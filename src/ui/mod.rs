mod dnd;
mod footer;
mod preview;
mod rows;
mod settings;
mod thumbnails;

use crate::engine::{Engine, ExecuteOutcome};
use crate::providers::{Action, ResultKind, SearchResult};
use crate::theme::ThemeManager;
use dnd::DragSession;
use footer::{action_chip, action_chip_button, footer_divider, keycap_label, update_footer};
use gtk::gdk::Key;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, Label, ListBox,
    ListBoxRow, Orientation, PolicyType, ScrolledWindow, Stack,
};
use preview::PreviewPanel;
use rows::ResultRowPool;
use settings::SettingsPanel;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(feature = "layer-shell")]
use std::time::{Duration, Instant};

/// Compact fixed outer width. Preview never grows the window — it takes
/// horizontal space from the list column instead.
const WINDOW_WIDTH: i32 = 720;
const WINDOW_MAX_HEIGHT: i32 = 520;
/// Extra transparent margin around the rounded shell (for soft drop-shadow).
/// Keep at 0 — a non-zero square inset reads as "padding" on Sway/Hyprland
/// because the layer surface is rectangular while the card is rounded.
const SHELL_INSET: i32 = 0;
/// Debounce keystrokes before search + async deep (cuts typing CPU spikes).
const SEARCH_DEBOUNCE_MS: u64 = 40;
/// CJK / translate queries: longer settle so paste/IME does not spawn workers per glyph.
const TRANSLATE_DEBOUNCE_MS: u64 = 180;

pub struct Launcher {
    window: ApplicationWindow,
    search: Entry,
    list: ListBox,
    row_pool: Rc<RefCell<ResultRowPool>>,
    empty: Label,
    results: Rc<RefCell<Vec<SearchResult>>>,
    selected: Rc<Cell<usize>>,
    engine: Arc<Engine>,
    ignore_focus_loss: Rc<Cell<bool>>,
    stack: Stack,
    settings: SettingsPanel,
    in_settings: Rc<Cell<bool>>,
    footer_action: Label,
    footer_term: GtkBox,
    preview: Rc<PreviewPanel>,
    deep_gen: Rc<Cell<u64>>,
    /// Pending search debounce timer (cancelled on each keystroke / hide).
    search_debounce: Rc<RefCell<Option<glib::SourceId>>>,
    /// Queries typed this open session (v2 typo reformulation learning).
    session_queries: Rc<RefCell<VecDeque<String>>>,
    drag_session: DragSession,
    /// Skip footer/preview side-effects while we programmatically select a row.
    suppress_select: Rc<Cell<bool>>,
    /// Cached appearance knobs (avoid full config clone on every search).
    ui_icon_size: Rc<Cell<i32>>,
    ui_symbolic: Rc<Cell<bool>>,
    /// Raycast compact: hide results body until query is non-empty.
    ui_compact: Rc<Cell<bool>>,
    /// Results middle section + separators (toggled for compact idle).
    body: GtkBox,
    header_sep: gtk::Separator,
    footer_sep: gtk::Separator,
    #[allow(dead_code)]
    theme: Rc<ThemeManager>,
}

impl Launcher {
    pub fn new(app: &Application, engine: Arc<Engine>) -> Self {
        let window = ApplicationWindow::builder()
            .application(app)
            .title("Blink")
            .decorated(false)
            .resizable(false)
            .css_classes(["blink-window"])
            .build();

        window.set_hide_on_close(true);
        // Outer window is slightly larger than the card so rounded corners + shadow
        // are not clipped by a square surface (reads as "padding" / square corners).
        let outer_w = WINDOW_WIDTH + SHELL_INSET * 2;
        window.set_default_size(outer_w, -1);
        window.set_size_request(outer_w, -1);
        setup_window_chrome(&window);

        let theme = ThemeManager::new(engine.config());

        // Frame hugs the shell. Expanding it leaves a transparent rectangle that
        // Hyprland layer-blur still samples around the rounded card (square halo).
        let frame = GtkBox::new(Orientation::Vertical, 0);
        frame.add_css_class("blink-frame");
        frame.set_hexpand(false);
        frame.set_vexpand(false);
        frame.set_halign(gtk::Align::Center);
        frame.set_valign(gtk::Align::Start);

        let shell = GtkBox::new(Orientation::Vertical, 0);
        shell.add_css_class("blink-shell");
        shell.set_hexpand(true);
        shell.set_vexpand(false);
        shell.set_halign(gtk::Align::Fill);
        shell.set_valign(gtk::Align::Start);
        shell.set_size_request(WINDOW_WIDTH, -1);
        // Clip children to the rounded shell allocation (border-radius aware).
        shell.set_overflow(gtk::Overflow::Hidden);

        let stack = Stack::new();
        stack.set_hexpand(true);
        // Critical for compact mode: default homogeneous=true sizes to the tallest
        // page (settings ~400px), leaving a huge empty shell under search+footer.
        stack.set_hhomogeneous(true);
        stack.set_vhomogeneous(false);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);

        // ========== SEARCH VIEW ==========
        let search_view = GtkBox::new(Orientation::Vertical, 0);
        search_view.set_hexpand(true);
        search_view.set_vexpand(false);
        search_view.set_valign(gtk::Align::Start);

        let header = GtkBox::new(Orientation::Vertical, 0);
        header.add_css_class("blink-header");
        header.set_hexpand(true);

        let search = Entry::builder()
            .placeholder_text("Search apps, files, or type math…")
            .css_classes(["blink-search"])
            .hexpand(true)
            .build();
        search.set_primary_icon_name(Some("system-search-symbolic"));
        header.append(&search);

        let header_sep = gtk::Separator::new(Orientation::Horizontal);
        header_sep.add_css_class("blink-sep");

        let body = GtkBox::new(Orientation::Horizontal, 0);
        body.add_css_class("blink-body");
        body.set_hexpand(true);
        body.set_vexpand(true);

        let list_col = GtkBox::new(Orientation::Vertical, 0);
        list_col.add_css_class("blink-list-col");
        list_col.set_hexpand(true);
        list_col.set_vexpand(true);

        let scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::External)
            .min_content_height(120)
            .max_content_height(WINDOW_MAX_HEIGHT - 140)
            .propagate_natural_height(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        scroll.add_css_class("blink-scroll");
        scroll.set_overlay_scrolling(false);

        let list = ListBox::new();
        list.add_css_class("blink-results");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.set_vexpand(false);
        scroll.set_child(Some(&list));

        let empty = Label::new(Some("Type to search apps, files, math, or conversions"));
        empty.add_css_class("blink-empty");
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        empty.set_hexpand(true);
        // Only expand when it's the sole content (no results); otherwise it
        // steals vertical space under the list.
        empty.set_vexpand(false);

        list_col.append(&scroll);
        list_col.append(&empty);

        // Shared with rows + preview so focus-loss hide is suppressed mid-drag.
        let ignore_focus_loss = Rc::new(Cell::new(false));
        let drag_session = DragSession::new(ignore_focus_loss.clone());
        // Needed so DnD can release exclusive keyboard grab under layer-shell.
        drag_session.bind_window(&window);

        // Fixed 25-row pool: update in place on search (Phase 2).
        let row_pool = Rc::new(RefCell::new(ResultRowPool::new(&drag_session)));

        let preview = Rc::new(PreviewPanel::new(drag_session.clone()));
        // Preview pane only appears for media (images / video / audio).

        body.append(&list_col);
        body.append(preview.separator());
        body.append(preview.widget());

        let footer_sep = gtk::Separator::new(Orientation::Horizontal);
        footer_sep.add_css_class("blink-sep");

        // Raycast-style action bar
        let footer = GtkBox::new(Orientation::Horizontal, 0);
        footer.add_css_class("blink-footer");
        footer.set_hexpand(true);

        // Left: primary action + keycap
        let primary = GtkBox::new(Orientation::Horizontal, 8);
        primary.add_css_class("blink-footer-primary");
        primary.set_halign(gtk::Align::Start);
        primary.set_hexpand(true);
        primary.set_valign(gtk::Align::Center);

        let footer_action = Label::new(Some("Open"));
        footer_action.add_css_class("blink-footer-action");
        footer_action.set_halign(gtk::Align::Start);

        let enter_key = keycap_label("↵");
        primary.append(&footer_action);
        primary.append(&enter_key);

        // Center/right divider before actions cluster
        let mid_div = footer_divider();

        // Right: secondary actions as chips (multi-keycap shortcuts)
        let actions = GtkBox::new(Orientation::Horizontal, 2);
        actions.add_css_class("blink-footer-actions");
        actions.set_halign(gtk::Align::End);
        actions.set_valign(gtk::Align::Center);

        let footer_term = action_chip("Terminal", "Ctrl Alt ↵");
        let copy_chip = action_chip("Copy", "Ctrl C");
        let settings_chip = action_chip_button("Settings", "Ctrl ,");

        let div1 = footer_divider();
        let div2 = footer_divider();

        actions.append(&footer_term);
        actions.append(&div1);
        actions.append(&copy_chip);
        actions.append(&div2);
        actions.append(&settings_chip);

        footer.append(&primary);
        footer.append(&mid_div);
        footer.append(&actions);

        search_view.append(&header);
        search_view.append(&header_sep);
        search_view.append(&body);
        search_view.append(&footer_sep);
        search_view.append(&footer);

        // Compact idle: search + footer only (Raycast compact). Expanded when typing.
        {
            let compact0 = matches!(
                engine.config().snapshot().ui.layout_mode,
                crate::config::LayoutMode::Compact
            );
            apply_body_chrome(compact0, true, &body, &header_sep, &footer_sep, Some(&scroll));
        }

        // ========== SETTINGS VIEW ==========
        let settings = SettingsPanel::new(engine.clone(), theme.clone());

        stack.add_named(&search_view, Some("search"));
        stack.add_named(settings.widget(), Some("settings"));
        stack.set_visible_child_name("search");

        shell.append(&stack);
        frame.append(&shell);
        window.set_child(Some(&frame));

        let results: Rc<RefCell<Vec<SearchResult>>> = Rc::new(RefCell::new(Vec::new()));
        let selected: Rc<Cell<usize>> = Rc::new(Cell::new(0));
        let suppress_select: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let ui_cfg0 = engine.config().snapshot().ui.clone();
        let ui_icon_size: Rc<Cell<i32>> = Rc::new(Cell::new(ui_cfg0.icon_size as i32));
        let ui_symbolic: Rc<Cell<bool>> = Rc::new(Cell::new(ui_cfg0.symbolic_icons));
        let ui_compact: Rc<Cell<bool>> = Rc::new(Cell::new(matches!(
            ui_cfg0.layout_mode,
            crate::config::LayoutMode::Compact
        )));
        let in_settings = Rc::new(Cell::new(false));
        // Bumped on every query change; stale async deep walks are ignored.
        let deep_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        let search_debounce: Rc<RefCell<Option<glib::SourceId>>> =
            Rc::new(RefCell::new(None));
        let session_queries: Rc<RefCell<VecDeque<String>>> =
            Rc::new(RefCell::new(VecDeque::with_capacity(12)));

        {
            let engine = engine.clone();
            let list = list.clone();
            let empty = empty.clone();
            let results = results.clone();
            let selected = selected.clone();
            let footer_action = footer_action.clone();
            let footer_term = footer_term.clone();
            let preview = preview.clone();
            let deep_gen = deep_gen.clone();
            let search_for_deep = search.clone();
            let drag_session = drag_session.clone();
            let search_debounce = search_debounce.clone();
            let suppress_select = suppress_select.clone();
            let ui_icon_size = ui_icon_size.clone();
            let ui_symbolic = ui_symbolic.clone();
            let ui_compact = ui_compact.clone();
            let row_pool = row_pool.clone();
            let body_c = body.clone();
            let header_sep_c = header_sep.clone();
            let footer_sep_c = footer_sep.clone();
            let scroll_c = scroll.clone();
            let session_queries = session_queries.clone();
            search.connect_changed(move |entry| {
                if let Some(id) = search_debounce.borrow_mut().take() {
                    id.remove();
                }
                let q = entry.text().to_string();
                note_session_query(&session_queries, &q);
                // Expand/collapse body immediately (don't wait for search debounce).
                apply_body_chrome(
                    ui_compact.get(),
                    q.trim().is_empty(),
                    &body_c,
                    &header_sep_c,
                    &footer_sep_c,
                    Some(&scroll_c),
                );
                let engine = engine.clone();
                let list = list.clone();
                let empty = empty.clone();
                let results = results.clone();
                let selected = selected.clone();
                let footer_action = footer_action.clone();
                let footer_term = footer_term.clone();
                let preview = preview.clone();
                let deep_gen = deep_gen.clone();
                let search_for_deep = search_for_deep.clone();
                let drag_session = drag_session.clone();
                let debounce_slot = search_debounce.clone();
                let suppress_select = suppress_select.clone();
                let ui_icon_size = ui_icon_size.clone();
                let ui_symbolic = ui_symbolic.clone();
                let ui_compact = ui_compact.clone();
                let row_pool = row_pool.clone();
                let body_c = body_c.clone();
                let header_sep_c = header_sep_c.clone();
                let footer_sep_c = footer_sep_c.clone();
                let scroll_c = scroll_c.clone();
                // Longer settle only for auto CJK paste/IME (not forced `tr …`).
                let wait_ms = if engine.translate_is_auto_query(&q) {
                    TRANSLATE_DEBOUNCE_MS
                } else {
                    SEARCH_DEBOUNCE_MS
                };
                let id = glib::timeout_add_local(
                    std::time::Duration::from_millis(wait_ms),
                    move || {
                        *debounce_slot.borrow_mut() = None;
                        refresh_results(
                            &engine,
                            &q,
                            &list,
                            &row_pool,
                            &empty,
                            &results,
                            &selected,
                            &footer_action,
                            &footer_term,
                            &preview,
                            &deep_gen,
                            &search_for_deep,
                            &drag_session,
                            &suppress_select,
                            &ui_icon_size,
                            &ui_symbolic,
                            &ui_compact,
                            &body_c,
                            &header_sep_c,
                            &footer_sep_c,
                            Some(&scroll_c),
                        );
                        glib::ControlFlow::Break
                    },
                );
                *search_debounce.borrow_mut() = Some(id);
            });
        }

        let open_settings = {
            let stack = stack.clone();
            let in_settings = in_settings.clone();
            let settings_nav = settings.nav.clone();
            Rc::new(move || {
                in_settings.set(true);
                stack.set_visible_child_name("settings");
                if let Some(row) = settings_nav
                    .selected_row()
                    .or_else(|| settings_nav.row_at_index(0))
                {
                    settings_nav.select_row(Some(&row));
                    row.grab_focus();
                }
            })
        };

        let close_settings = {
            let stack = stack.clone();
            let in_settings = in_settings.clone();
            let search = search.clone();
            let engine = engine.clone();
            let list = list.clone();
            let empty = empty.clone();
            let results = results.clone();
            let selected = selected.clone();
            let footer_action = footer_action.clone();
            let footer_term = footer_term.clone();
            let preview = preview.clone();
            let deep_gen = deep_gen.clone();
            let drag_session = drag_session.clone();
            let suppress_select = suppress_select.clone();
            let ui_icon_size = ui_icon_size.clone();
            let ui_symbolic = ui_symbolic.clone();
            let ui_compact = ui_compact.clone();
            let row_pool = row_pool.clone();
            let body_cs = body.clone();
            let header_sep_cs = header_sep.clone();
            let footer_sep_cs = footer_sep.clone();
            let scroll_cs = scroll.clone();
            Rc::new(move || {
                in_settings.set(false);
                stack.set_visible_child_name("search");
                search.grab_focus();
                // Settings may have changed icon prefs / layout.
                let ui = engine.config().snapshot().ui.clone();
                ui_icon_size.set(ui.icon_size as i32);
                ui_symbolic.set(ui.symbolic_icons);
                ui_compact.set(matches!(
                    ui.layout_mode,
                    crate::config::LayoutMode::Compact
                ));
                refresh_results(
                    &engine,
                    &search.text(),
                    &list,
                    &row_pool,
                    &empty,
                    &results,
                    &selected,
                    &footer_action,
                    &footer_term,
                    &preview,
                    &deep_gen,
                    &search,
                    &drag_session,
                    &suppress_select,
                    &ui_icon_size,
                    &ui_symbolic,
                    &ui_compact,
                    &body_cs,
                    &header_sep_cs,
                    &footer_sep_cs,
                    Some(&scroll_cs),
                );
            })
        };

        settings.set_on_done({
            let close_settings = close_settings.clone();
            move || close_settings()
        });

        {
            let open_settings = open_settings.clone();
            settings_chip.connect_clicked(move |_| open_settings());
        }

        {
            let engine = engine.clone();
            let window = window.clone();
            let results = results.clone();
            let selected = selected.clone();
            let search = search.clone();
            let session_queries = session_queries.clone();
            let open_settings = open_settings.clone();
            list.connect_row_activated(move |_, row| {
                let idx = row.index() as usize;
                selected.set(idx);
                activate_result(
                    &engine,
                    &results,
                    idx,
                    &window,
                    &search,
                    &session_queries,
                    &open_settings,
                );
            });
        }

        {
            let selected = selected.clone();
            let results = results.clone();
            let footer_action = footer_action.clone();
            let footer_term = footer_term.clone();
            let preview = preview.clone();
            let suppress_select = suppress_select.clone();
            list.connect_row_selected(move |_, row| {
                if suppress_select.get() {
                    if let Some(row) = row {
                        selected.set(row.index() as usize);
                    }
                    return;
                }
                if let Some(row) = row {
                    let idx = row.index() as usize;
                    selected.set(idx);
                    update_footer(&results, idx, &footer_action, &footer_term);
                    let item = results.borrow().get(idx).cloned();
                    preview.update(item.as_ref());
                }
            });
        }

        {
            let engine = engine.clone();
            let window = window.clone();
            let results = results.clone();
            let selected = selected.clone();
            let search_for_activate = search.clone();
            let session_queries = session_queries.clone();
            let open_settings = open_settings.clone();
            search.connect_activate(move |_| {
                activate_result(
                    &engine,
                    &results,
                    selected.get(),
                    &window,
                    &search_for_activate,
                    &session_queries,
                    &open_settings,
                );
            });
        }

        let key = EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let engine = engine.clone();
            let window = window.clone();
            let list = list.clone();
            let search = search.clone();
            let results = results.clone();
            let selected = selected.clone();
            let session_queries = session_queries.clone();
            let in_settings = in_settings.clone();
            let close_settings = close_settings.clone();
            let open_settings = open_settings.clone();
            let dismiss_settings_overlay = {
                let settings_dismiss = settings.dismiss_overlay_handle();
                settings_dismiss
            };

            let settings_nav = settings.nav.clone();
            key.connect_key_pressed(move |_, keyval, _keycode, state| {
                if in_settings.get() {
                    if keyval == Key::Escape {
                        // Nested overlays (default-app picker) first, then leave Settings.
                        if dismiss_settings_overlay() {
                            return glib::Propagation::Stop;
                        }
                        close_settings();
                        return glib::Propagation::Stop;
                    }
                    // ↑/↓ / j/k cycle settings categories (window-level capture)
                    let n = {
                        let mut c = 0i32;
                        let mut child = settings_nav.first_child();
                        while let Some(w) = child {
                            c += 1;
                            child = w.next_sibling();
                        }
                        c
                    };
                    if n == 0 {
                        return glib::Propagation::Proceed;
                    }
                    let cur = settings_nav
                        .selected_row()
                        .map(|r| r.index())
                        .unwrap_or(0)
                        .max(0);
                    let next = match keyval {
                        Key::Down | Key::j | Key::J => Some((cur + 1) % n),
                        Key::Up | Key::k | Key::K => Some(if cur == 0 { n - 1 } else { cur - 1 }),
                        Key::Home => Some(0),
                        Key::End => Some(n - 1),
                        _ => None,
                    };
                    if let Some(idx) = next {
                        if let Some(row) = settings_nav.row_at_index(idx) {
                            settings_nav.select_row(Some(&row));
                            row.grab_focus();
                        }
                        return glib::Propagation::Stop;
                    }
                    return glib::Propagation::Proceed;
                }

                match keyval {
                    Key::Escape => {
                        window.set_visible(false);
                        glib::Propagation::Stop
                    }
                    Key::Return | Key::KP_Enter => {
                        let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                        let alt = state.contains(gtk::gdk::ModifierType::ALT_MASK)
                            || state.contains(gtk::gdk::ModifierType::META_MASK);
                        if ctrl && alt {
                            // Ctrl+Alt+Enter → open terminal at selected folder/file parent
                            let idx = selected.get();
                            if let Some(item) = results.borrow().get(idx).cloned() {
                                if engine.open_terminal_for(&item) {
                                    window.set_visible(false);
                                }
                            }
                            return glib::Propagation::Stop;
                        }
                        activate_result(
                            &engine,
                            &results,
                            selected.get(),
                            &window,
                            &search,
                            &session_queries,
                            &open_settings,
                        );
                        glib::Propagation::Stop
                    }
                    Key::comma if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                        open_settings();
                        glib::Propagation::Stop
                    }
                    Key::Tab => {
                        // Tab → autocomplete selected suggestion into the search box
                        // (↓/↑ still navigate). Soft SetQuery scopes fill their query.
                        tab_complete_selected(
                            &results,
                            &selected,
                            &list,
                            &search,
                        );
                        glib::Propagation::Stop
                    }
                    Key::Down => {
                        let len = results.borrow().len();
                        if len > 0 {
                            let next = (selected.get() + 1) % len;
                            selected.set(next);
                            if let Some(row) = list.row_at_index(next as i32) {
                                list.select_row(Some(&row));
                                search.grab_focus_without_selecting();
                            }
                        }
                        glib::Propagation::Stop
                    }
                    Key::Up | Key::ISO_Left_Tab => {
                        let len = results.borrow().len();
                        if len > 0 {
                            let cur = selected.get();
                            let next = if cur == 0 { len - 1 } else { cur - 1 };
                            selected.set(next);
                            if let Some(row) = list.row_at_index(next as i32) {
                                list.select_row(Some(&row));
                                search.grab_focus_without_selecting();
                            }
                        }
                        glib::Propagation::Stop
                    }
                    Key::c if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                        let idx = selected.get();
                        if let Some(item) = results.borrow().get(idx) {
                            if let Action::Copy(text) = &item.action {
                                engine.execute(&Action::Copy(text.clone()));
                                window.set_visible(false);
                                return glib::Propagation::Stop;
                            }
                            if matches!(item.kind, ResultKind::Calc | ResultKind::Conversion) {
                                engine.execute(&Action::Copy(item.title.clone()));
                                window.set_visible(false);
                                return glib::Propagation::Stop;
                            }
                        }
                        glib::Propagation::Proceed
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        window.add_controller(key);

        {
            let window_c = window.clone();
            let ignore_focus_loss = ignore_focus_loss.clone();
            window.connect_notify_local(Some("is-active"), move |w, _| {
                if ignore_focus_loss.get() {
                    return;
                }
                if w.is_visible() && !w.is_active() {
                    let w = window_c.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
                        if w.is_visible() && !w.is_active() {
                            w.set_visible(false);
                        }
                    });
                }
            });
        }

        window.set_visible(false);

        Self {
            window,
            search,
            list,
            row_pool,
            empty,
            results,
            selected,
            engine,
            ignore_focus_loss,
            stack,
            settings,
            in_settings,
            footer_action,
            footer_term,
            preview,
            deep_gen,
            search_debounce,
            session_queries: session_queries.clone(),
            drag_session,
            suppress_select,
            ui_icon_size,
            ui_symbolic,
            ui_compact,
            body,
            header_sep,
            footer_sep,
            theme: theme.clone(),
        }
    }

    pub fn toggle(&self) {
        if self.window.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    pub fn show(&self) {
        self.ignore_focus_loss.set(true);
        self.in_settings.set(false);
        self.stack.set_visible_child_name("search");
        self.session_queries.borrow_mut().clear();
        self.search.set_text("");
        // Refresh cached appearance from config (settings may have changed while hidden).
        let ui = self.engine.config().snapshot().ui.clone();
        self.ui_icon_size.set(ui.icon_size as i32);
        self.ui_symbolic.set(ui.symbolic_icons);
        self.ui_compact.set(matches!(
            ui.layout_mode,
            crate::config::LayoutMode::Compact
        ));
        refresh_results(
            &self.engine,
            "",
            &self.list,
            &self.row_pool,
            &self.empty,
            &self.results,
            &self.selected,
            &self.footer_action,
            &self.footer_term,
            &self.preview,
            &self.deep_gen,
            &self.search,
            &self.drag_session,
            &self.suppress_select,
            &self.ui_icon_size,
            &self.ui_symbolic,
            &self.ui_compact,
            &self.body,
            &self.header_sep,
            &self.footer_sep,
            None,
        );
        self.settings.refresh_status();
        self.window.set_visible(true);
        self.window.present();
        self.search.grab_focus();
        center_on_active_monitor(&self.window);

        let ignore = self.ignore_focus_loss.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            ignore.set(false);
        });
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
        // Drop pending search / deep-walk work while hidden.
        if let Some(id) = self.search_debounce.borrow_mut().take() {
            id.remove();
        }
        self.deep_gen.set(self.deep_gen.get().wrapping_add(1));
        self.session_queries.borrow_mut().clear();
        self.preview.clear();
    }
}

/// Fill the search entry from the selected suggestion (Tab autocomplete).
///
/// - `Action::SetQuery` soft scopes replace the whole query (e.g. `name in folder`).
/// - **Folders** always complete to a real path with a trailing `/` so further
///   typing stays scoped under that directory (path browser), not free-text.
/// - Path-shaped queries (`~/…`, `/…`, `./…`) complete files to path strings too.
/// - Apps / plain files otherwise complete to the result title (e.g. `evo` → `evo_gsmc`).
/// - If the box already matches the selected completion, advance selection and complete that
///   instead (shell-style cycle through suggestions).
fn tab_complete_selected(
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    list: &ListBox,
    search: &Entry,
) {
    let current = search.text().to_string();

    // Resolve completion while holding the results borrow, then drop it
    // before touching the list (row-selected handlers re-borrow results).
    let (idx, text) = {
        let items = results.borrow();
        let len = items.len();
        if len == 0 {
            return;
        }

        let mut idx = selected.get().min(len - 1);
        let mut completion = completion_text_for(&current, &items[idx]);
        // Already completed this hit → cycle to the next suggestion.
        if completion
            .as_ref()
            .is_some_and(|c| c == &current || c.eq_ignore_ascii_case(&current))
        {
            idx = (idx + 1) % len;
            completion = completion_text_for(&current, &items[idx]);
        }
        (idx, completion)
    };

    selected.set(idx);
    if let Some(row) = list.row_at_index(idx as i32) {
        list.select_row(Some(&row));
    }

    if let Some(text) = text {
        // Avoid no-op set_text (would re-trigger search debounce unnecessarily).
        if text != current {
            search.set_text(&text);
            search.set_position(-1);
        }
    }
    search.grab_focus_without_selecting();
}

/// Best string to put in the search box for `item`, given the current query.
fn completion_text_for(current: &str, item: &SearchResult) -> Option<String> {
    match &item.action {
        Action::SetQuery(q) => Some(q.clone()),
        Action::OpenPath(path) => {
            let cur = current.trim_start();
            // Folders: always enter path-browser mode under this directory.
            // Free-text title completion alone (`test` → `test_fyps`) loses the
            // parent context, so later queries like `data` match every data dir.
            if matches!(item.kind, ResultKind::Folder) || path.is_dir() {
                return Some(complete_path_query(cur, path));
            }
            // Already typing a path → complete the file to a path string.
            if is_path_shaped_query(cur) {
                return Some(complete_path_query(cur, path));
            }
            Some(item.title.clone())
        }
        Action::LaunchApp { .. } | Action::OpenTerminal(_) => Some(item.title.clone()),
        Action::Copy(_) | Action::OpenSettings => {
            // Calc / conversion / settings: title is still a useful fill-in.
            if item.title.is_empty() {
                None
            } else {
                Some(item.title.clone())
            }
        }
    }
}

fn is_path_shaped_query(q: &str) -> bool {
    q.starts_with('/')
        || q.starts_with('~')
        || q.starts_with("./")
        || q.starts_with("../")
        // Relative multi-segment paths (`test_fyps/evo`) also stay path-scoped.
        || q.contains('/')
}

/// Format `path` for the search box, preserving path style when possible.
///
/// Folders always get a trailing `/` so the engine stays in path-completion
/// mode and the next keystrokes list children of that directory.
fn complete_path_query(query: &str, path: &std::path::Path) -> String {
    let full = path.to_string_lossy();
    let dir_slash = path.is_dir();
    let with_slash = |mut s: String| -> String {
        if dir_slash && !s.ends_with('/') {
            s.push('/');
        }
        s
    };

    // Prefer `~/…` when the target lives under home, unless the user is already
    // typing an absolute `/…` path (keep their style).
    let prefer_tilde = !query.starts_with('/');
    if prefer_tilde {
        if let Some(home) = dirs::home_dir() {
            let home_s = home.to_string_lossy();
            if let Some(rest) = full.strip_prefix(home_s.as_ref()) {
                // Only strip when we matched a real path prefix boundary.
                let ok = rest.is_empty() || rest.starts_with('/');
                if ok {
                    let rest = rest.trim_start_matches('/');
                    return with_slash(if rest.is_empty() {
                        "~/".into()
                    } else {
                        format!("~/{rest}")
                    });
                }
            }
        }
    }

    // Absolute (or outside home): real path. Folders get a trailing `/`.
    with_slash(full.into_owned())
}

#[cfg(test)]
mod tab_complete_tests {
    use super::{complete_path_query, completion_text_for, is_path_shaped_query};
    use crate::providers::{Action, ResultKind, SearchResult};
    use std::path::PathBuf;

    fn item(title: &str, kind: ResultKind, action: Action) -> SearchResult {
        SearchResult {
            id: "t".into(),
            title: title.into(),
            subtitle: String::new(),
            kind,
            score: 0,
            icon: None,
            action,
            conversion: None,
        }
    }

    #[test]
    fn path_shaped_detection() {
        assert!(is_path_shaped_query("~/Doc"));
        assert!(is_path_shaped_query("~"));
        assert!(is_path_shaped_query("/usr/bi"));
        assert!(is_path_shaped_query("./src"));
        assert!(is_path_shaped_query("test_fyps/evo"));
        assert!(!is_path_shaped_query("evo"));
        assert!(!is_path_shaped_query("f evo"));
    }

    #[test]
    fn title_completion_for_apps_and_files() {
        let app = item(
            "evo_gsmc",
            ResultKind::App,
            Action::LaunchApp {
                exec: "evo".into(),
                terminal: false,
                desktop_path: None,
            },
        );
        assert_eq!(
            completion_text_for("evo", &app).as_deref(),
            Some("evo_gsmc")
        );

        // Plain free-text file: title is enough (Enter opens it).
        let file = item(
            "evo_gsmc",
            ResultKind::File,
            Action::OpenPath(PathBuf::from("/tmp/projects/evo_gsmc")),
        );
        assert_eq!(
            completion_text_for("evo", &file).as_deref(),
            Some("evo_gsmc")
        );
    }

    #[test]
    fn folder_completion_scopes_under_path() {
        // Regression: Tab on a free-text folder hit must NOT leave a bare name
        // (`test_fyps`) that turns the next query into global free-text search.
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let folder = home.join("test_fyps");
        let hit = item(
            "test_fyps",
            ResultKind::Folder,
            Action::OpenPath(folder.clone()),
        );
        let completed = completion_text_for("test", &hit).expect("folder completion");
        assert!(
            completed.ends_with("test_fyps/") || completed.ends_with("test_fyps"),
            "got {completed}"
        );
        assert!(
            completed.starts_with("~/") || completed.starts_with('/'),
            "folder Tab should produce a path-shaped query, got {completed}"
        );
        // Prefer path-browser form with trailing slash when path is a dir.
        // (is_dir may be false if the path does not exist on this machine —
        // still require path shape above.)
        if folder.is_dir() {
            assert!(
                completed.ends_with('/'),
                "existing folder should end with /, got {completed}"
            );
        }
    }

    #[test]
    fn set_query_soft_scope() {
        let hint = item(
            "glassbox",
            ResultKind::Folder,
            Action::SetQuery("notes in glassbox".into()),
        );
        assert_eq!(
            completion_text_for("notes in gla", &hint).as_deref(),
            Some("notes in glassbox")
        );
    }

    #[test]
    fn home_path_completion_preserves_tilde() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let target = home.join("Documents");
        let completed = complete_path_query("~/Doc", &target);
        assert!(
            completed.starts_with("~/Documents"),
            "got {completed}"
        );
    }

    #[test]
    fn free_text_folder_prefers_tilde_under_home() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let target = home.join("projects").join("test_fyps");
        let completed = complete_path_query("test", &target);
        assert!(
            completed.starts_with("~/projects/test_fyps"),
            "got {completed}"
        );
        // Trailing slash only when the path is a real directory on disk.
        if target.is_dir() {
            assert!(completed.ends_with('/'), "got {completed}");
        }
    }
}

fn activate_result<F: Fn()>(
    engine: &Engine,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    idx: usize,
    window: &ApplicationWindow,
    search: &Entry,
    session_queries: &Rc<RefCell<VecDeque<String>>>,
    open_settings: &Rc<F>,
) {
    let item = results.borrow().get(idx).cloned();
    if let Some(item) = item {
        match engine.execute(&item.action) {
            ExecuteOutcome::OpenSettings => {
                open_settings();
            }
            ExecuteOutcome::SetQuery(q) => {
                // Soft completion: keep launcher open, fill the scoped query.
                search.set_text(&q);
                search.set_position(-1);
                search.grab_focus_without_selecting();
            }
            ExecuteOutcome::Launched => {
                if !matches!(
                    item.kind,
                    ResultKind::Calc | ResultKind::Conversion | ResultKind::Command
                ) {
                    let final_q = search.text().to_string();
                    let recent: Vec<String> =
                        session_queries.borrow().iter().cloned().collect();
                    engine.learn_typos(&final_q, &recent, &item.id, &item.title);
                    engine.record_usage(&item.id);
                }
                window.set_visible(false);
            }
        }
    }
}

/// Remember a query fragment for typo reformulation learning (v2).
fn note_session_query(session: &Rc<RefCell<VecDeque<String>>>, q: &str) {
    let q = q.trim().to_lowercase();
    if q.chars().count() < 2 {
        return;
    }
    // Skip pure path/math noise — learning only wants free-text tokens.
    if q.contains('/') || q.contains('~') || q.contains('%') || q.contains('=') {
        return;
    }
    let mut g = session.borrow_mut();
    if g.back().map(|s| s.as_str()) == Some(q.as_str()) {
        return;
    }
    g.push_back(q);
    while g.len() > 12 {
        g.pop_front();
    }
}

fn apply_body_chrome(
    compact: bool,
    query_empty: bool,
    body: &GtkBox,
    header_sep: &gtk::Separator,
    footer_sep: &gtk::Separator,
    scroll: Option<&ScrolledWindow>,
) {
    // Compact + idle query → search bar + footer only (no middle body).
    let show_body = !(compact && query_empty);
    body.set_visible(show_body);
    header_sep.set_visible(show_body);
    // Keep a hairline above the footer when body is hidden (compact bar look).
    footer_sep.set_visible(true);
    if show_body {
        body.remove_css_class("blink-body-collapsed");
        body.set_vexpand(true);
        if let Some(s) = scroll {
            s.set_min_content_height(120);
            s.set_vexpand(true);
        }
    } else {
        body.add_css_class("blink-body-collapsed");
        body.set_vexpand(false);
        if let Some(s) = scroll {
            s.set_min_content_height(0);
            s.set_vexpand(false);
        }
    }
    // Force the window/shell to re-measure after hide/show (layer-shell surfaces
    // often keep the previous allocation until a size request refresh).
    if let Some(toplevel) = body.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
        // Natural height: drop any previous fixed height from expanded state.
        toplevel.set_default_size(toplevel.default_size().0.max(1), -1);
        toplevel.queue_resize();
    } else {
        body.queue_resize();
    }
}

fn refresh_results(
    engine: &Arc<Engine>,
    query: &str,
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &Label,
    footer_term: &GtkBox,
    preview: &Rc<PreviewPanel>,
    deep_gen: &Rc<Cell<u64>>,
    search_entry: &Entry,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    ui_icon_size: &Rc<Cell<i32>>,
    ui_symbolic: &Rc<Cell<bool>>,
    ui_compact: &Rc<Cell<bool>>,
    body: &GtkBox,
    header_sep: &gtk::Separator,
    footer_sep: &gtk::Separator,
    scroll: Option<&ScrolledWindow>,
) {
    // Never rebind rows mid-drag — that would cancel the DnD session.
    if drag_session.is_active() {
        return;
    }
    let icon_size = ui_icon_size.get();
    let symbolic_icons = ui_symbolic.get();
    let compact = ui_compact.get();
    let query_empty = query.trim().is_empty();

    apply_body_chrome(compact, query_empty, body, header_sep, footer_sep, scroll);

    // Invalidate any in-flight async deep/translate for a previous query.
    let gen = deep_gen.get().wrapping_add(1);
    deep_gen.set(gen);

    // Compact idle: skip ranking recents — body is hidden anyway.
    let found = if compact && query_empty {
        Vec::new()
    } else {
        engine.search(query)
    };
    let no_hits = found.is_empty();
    // In compact idle the empty placeholder is not shown (body hidden).
    let show_empty = no_hits && !(compact && query_empty);
    empty.set_visible(show_empty);
    empty.set_vexpand(show_empty);
    list.set_visible(!no_hits);

    {
        let mut pool = row_pool.borrow_mut();
        if found.is_empty() {
            pool.clear(list);
        } else {
            pool.apply(list, &found, icon_size, symbolic_icons);
        }
    }

    selected.set(0);
    *results.borrow_mut() = found;

    if let Some(row) = row_pool.borrow().row_at(0).map(|r| r.clone()) {
        suppress_select.set(true);
        list.select_row(Some(&row));
        suppress_select.set(false);
        update_footer(results, 0, footer_action, footer_term);
        let item = results.borrow().first().cloned();
        preview.update(item.as_ref());
    } else {
        list.select_row(Option::<&ListBoxRow>::None);
        update_footer(results, 0, footer_action, footer_term);
        preview.clear();
    }

    let q = query.to_string();
    if q.trim().is_empty() {
        return;
    }

    // Async translate (network). UI path only did cache/pending — never curl on main.
    if engine.should_translate_network(&q) {
        let engine_t = engine.clone();
        let list_t = list.clone();
        let row_pool_t = row_pool.clone();
        let empty_t = empty.clone();
        let results_t = results.clone();
        let selected_t = selected.clone();
        let footer_action_t = footer_action.clone();
        let footer_term_t = footer_term.clone();
        let preview_t = preview.clone();
        let deep_gen_t = deep_gen.clone();
        let search_entry_t = search_entry.clone();
        let drag_session_t = drag_session.clone();
        let suppress_t = suppress_select.clone();
        let icon_size_t = ui_icon_size.clone();
        let symbolic_t = ui_symbolic.clone();
        let q_t = q.clone();
        let gen_t = gen;
        let (tx_t, rx_t) = async_channel::bounded::<Vec<SearchResult>>(1);
        schedule_translate_job(engine_t.clone(), q_t.clone(), gen_t, tx_t);
        glib::spawn_future_local(async move {
            let Ok(hits) = rx_t.recv().await else {
                return;
            };
            if deep_gen_t.get() != gen_t {
                return;
            }
            if search_entry_t.text().as_str() != q_t.as_str() {
                return;
            }
            if hits.is_empty() {
                return;
            }
            apply_translate_hits(
                &hits,
                &list_t,
                &row_pool_t,
                &empty_t,
                &results_t,
                &selected_t,
                &footer_action_t,
                &footer_term_t,
                &preview_t,
                &drag_session_t,
                &suppress_t,
                icon_size_t.get(),
                symbolic_t.get(),
            );
        });
    }

    let current = results.borrow().clone();
    if !engine.should_deep_search(&q, &current) {
        return;
    }

    let engine = engine.clone();
    let list = list.clone();
    let row_pool = row_pool.clone();
    let empty = empty.clone();
    let results = results.clone();
    let selected = selected.clone();
    let footer_action = footer_action.clone();
    let footer_term = footer_term.clone();
    let preview = preview.clone();
    let deep_gen = deep_gen.clone();
    let search_entry = search_entry.clone();
    let drag_session = drag_session.clone();
    let suppress_select = suppress_select.clone();
    let ui_icon_size = ui_icon_size.clone();
    let ui_symbolic = ui_symbolic.clone();

    let (tx, rx) = async_channel::bounded::<Vec<SearchResult>>(1);
    schedule_deep_job(engine.clone(), q.clone(), gen, tx);

    glib::spawn_future_local(async move {
        let Ok(deep_hits) = rx.recv().await else {
            return;
        };
        if deep_gen.get() != gen {
            return;
        }
        if search_entry.text().as_str() != q.as_str() {
            return;
        }
        if deep_hits.is_empty() {
            return;
        }
        apply_deep_hits(
            &deep_hits,
            &list,
            &row_pool,
            &empty,
            &results,
            &selected,
            &footer_action,
            &footer_term,
            &preview,
            &drag_session,
            &suppress_select,
            ui_icon_size.get(),
            ui_symbolic.get(),
        );
    });
}

/// Single-flight deep walk: coalesce concurrent requests; worker always runs latest.
fn schedule_deep_job(
    engine: Arc<Engine>,
    query: String,
    gen: u64,
    reply: async_channel::Sender<Vec<SearchResult>>,
) {
    #[derive(Clone)]
    struct Job {
        engine: Arc<Engine>,
        query: String,
        #[allow(dead_code)]
        gen: u64,
        reply: async_channel::Sender<Vec<SearchResult>>,
    }
    static LATEST: OnceLock<Mutex<Option<Job>>> = OnceLock::new();
    static BUSY: AtomicBool = AtomicBool::new(false);

    let slot = LATEST.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap() = Some(Job {
        engine,
        query,
        gen,
        reply,
    });

    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    std::thread::spawn(move || loop {
        let job = {
            let mut g = LATEST.get().unwrap().lock().unwrap();
            g.take()
        };
        let Some(job) = job else {
            BUSY.store(false, Ordering::Release);
            if LATEST.get().unwrap().lock().unwrap().is_some()
                && BUSY
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                continue;
            }
            break;
        };
        let hits = job.engine.search_files_deep(&job.query);
        let _ = job.reply.send_blocking(hits);
    });
}

/// Single-flight translate network fetch (latest query wins).
fn schedule_translate_job(
    engine: Arc<Engine>,
    query: String,
    gen: u64,
    reply: async_channel::Sender<Vec<SearchResult>>,
) {
    #[derive(Clone)]
    struct Job {
        engine: Arc<Engine>,
        query: String,
        #[allow(dead_code)]
        gen: u64,
        reply: async_channel::Sender<Vec<SearchResult>>,
    }
    static LATEST: OnceLock<Mutex<Option<Job>>> = OnceLock::new();
    static BUSY: AtomicBool = AtomicBool::new(false);

    let slot = LATEST.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap() = Some(Job {
        engine,
        query,
        gen,
        reply,
    });

    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    std::thread::spawn(move || loop {
        let job = {
            let mut g = LATEST.get().unwrap().lock().unwrap();
            g.take()
        };
        let Some(job) = job else {
            BUSY.store(false, Ordering::Release);
            if LATEST.get().unwrap().lock().unwrap().is_some()
                && BUSY
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                continue;
            }
            break;
        };
        let hits = job.engine.search_translate_network(&job.query);
        let _ = job.reply.send_blocking(hits);
    });
}

/// Replace pending translate row with network result (success or soft-fail).
fn apply_translate_hits(
    hits: &[SearchResult],
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &Label,
    footer_term: &GtkBox,
    preview: &Rc<PreviewPanel>,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    icon_size: i32,
    symbolic_icons: bool,
) {
    if drag_session.is_active() || hits.is_empty() {
        return;
    }

    let mut merged = results.borrow().clone();
    merged.retain(|r| !crate::providers::translate::is_pending_result(r));
    let mut out = hits.to_vec();
    let mut seen: std::collections::HashSet<String> = out.iter().map(|r| r.id.clone()).collect();
    for r in merged {
        if seen.insert(r.id.clone()) {
            out.push(r);
        }
    }
    out.truncate(25);

    let no_hits = out.is_empty();
    empty.set_visible(no_hits);
    empty.set_vexpand(no_hits);
    list.set_visible(!no_hits);
    {
        let mut pool = row_pool.borrow_mut();
        if out.is_empty() {
            pool.clear(list);
        } else {
            pool.apply(list, &out, icon_size, symbolic_icons);
        }
    }
    selected.set(0);
    *results.borrow_mut() = out;

    if let Some(row) = row_pool.borrow().row_at(0).map(|r| r.clone()) {
        suppress_select.set(true);
        list.select_row(Some(&row));
        suppress_select.set(false);
        update_footer(results, 0, footer_action, footer_term);
        let item = results.borrow().first().cloned();
        preview.update(item.as_ref());
    } else {
        list.select_row(Option::<&ListBoxRow>::None);
        update_footer(results, 0, footer_action, footer_term);
        preview.clear();
    }
}

/// Merge async deep file hits into the current result list without clobbering
/// selection when the user has already moved.
fn apply_deep_hits(
    deep_hits: &[SearchResult],
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &Label,
    footer_term: &GtkBox,
    preview: &Rc<PreviewPanel>,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    icon_size: i32,
    symbolic_icons: bool,
) {
    if drag_session.is_active() {
        return;
    }

    let prev_id = results
        .borrow()
        .get(selected.get())
        .map(|r| r.id.clone());

    let mut merged = results.borrow().clone();
    let mut seen: std::collections::HashSet<String> =
        merged.iter().map(|r| r.id.clone()).collect();
    let mut added = 0usize;
    for r in deep_hits {
        if seen.insert(r.id.clone()) {
            merged.push(r.clone());
            added += 1;
        }
    }
    if added == 0 {
        return;
    }

    merged.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| kind_rank_ui(a.kind).cmp(&kind_rank_ui(b.kind)))
            .then_with(|| a.title.cmp(&b.title))
    });
    merged.truncate(25);

    let no_hits = merged.is_empty();
    empty.set_visible(no_hits);
    empty.set_vexpand(no_hits);
    list.set_visible(!no_hits);
    {
        let mut pool = row_pool.borrow_mut();
        if merged.is_empty() {
            pool.clear(list);
        } else {
            pool.apply(list, &merged, icon_size, symbolic_icons);
        }
    }

    let new_sel = prev_id
        .and_then(|id| merged.iter().position(|r| r.id == id))
        .unwrap_or(0);
    selected.set(new_sel);
    *results.borrow_mut() = merged;

    if let Some(row) = row_pool.borrow().row_at(new_sel).map(|r| r.clone()) {
        suppress_select.set(true);
        list.select_row(Some(&row));
        suppress_select.set(false);
        update_footer(results, new_sel, footer_action, footer_term);
        let item = results.borrow().get(new_sel).cloned();
        preview.update(item.as_ref());
    } else {
        list.select_row(Option::<&ListBoxRow>::None);
        update_footer(results, 0, footer_action, footer_term);
        preview.clear();
    }
}

fn kind_rank_ui(k: ResultKind) -> u8 {
    match k {
        ResultKind::Calc | ResultKind::Conversion => 0,
        ResultKind::Command => 1,
        ResultKind::App => 2,
        ResultKind::Folder => 3,
        ResultKind::File => 4,
    }
}

fn setup_window_chrome(window: &ApplicationWindow) {
    #[cfg(feature = "layer-shell")]
    {
        use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
        if gtk4_layer_shell::is_supported() {
            window.init_layer_shell();
            window.set_layer(Layer::Overlay);
            window.set_keyboard_mode(KeyboardMode::Exclusive);
            window.set_anchor(Edge::Top, false);
            window.set_anchor(Edge::Bottom, false);
            window.set_anchor(Edge::Left, false);
            window.set_anchor(Edge::Right, false);
            window.set_margin(Edge::Top, 0);
            window.set_margin(Edge::Bottom, 0);
            window.set_margin(Edge::Left, 0);
            window.set_margin(Edge::Right, 0);
            window.set_namespace(Some("blink"));
            return;
        }
    }

    window.set_modal(true);
}

fn center_on_active_monitor(window: &ApplicationWindow) {
    #[cfg(feature = "layer-shell")]
    {
        use gtk4_layer_shell::{Edge, LayerShell};
        if gtk4_layer_shell::is_supported() {
            if let Some((_x, _y, _w, h)) = hypr_focused_monitor() {
                let top = (h / 5).max(80);
                window.set_anchor(Edge::Top, true);
                window.set_anchor(Edge::Left, false);
                window.set_anchor(Edge::Right, false);
                window.set_anchor(Edge::Bottom, false);
                window.set_margin(Edge::Top, top);
            }
            return;
        }
    }
    let _ = window;
}

#[cfg(feature = "layer-shell")]
fn hypr_focused_monitor() -> Option<(i32, i32, i32, i32)> {
    // hyprctl is a process spawn — cache a few seconds across rapid toggles.
    const TTL: Duration = Duration::from_secs(2);
    static CACHE: OnceLock<Mutex<Option<(Instant, (i32, i32, i32, i32))>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    {
        let g = cache.lock().unwrap();
        if let Some((at, geom)) = *g {
            if at.elapsed() < TTL {
                return Some(geom);
            }
        }
    }

    let out = std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = v.as_array()?;
    let mon = arr
        .iter()
        .find(|m| m.get("focused").and_then(|f| f.as_bool()) == Some(true))
        .or_else(|| arr.first())?;
    let geom = (
        mon.get("x")?.as_i64()? as i32,
        mon.get("y")?.as_i64()? as i32,
        mon.get("width")?.as_i64()? as i32,
        mon.get("height")?.as_i64()? as i32,
    );
    *cache.lock().unwrap() = Some((Instant::now(), geom));
    Some(geom)
}
