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
    Orientation, PolicyType, ScrolledWindow, Stack,
};
use preview::PreviewPanel;
use rows::build_row;
use settings::SettingsPanel;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

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

pub struct Launcher {
    window: ApplicationWindow,
    search: Entry,
    list: ListBox,
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
    drag_session: DragSession,
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
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);

        // ========== SEARCH VIEW ==========
        let search_view = GtkBox::new(Orientation::Vertical, 0);
        search_view.set_hexpand(true);

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
        scroll.set_child(Some(&list));

        let empty = Label::new(Some("Type to search apps, files, math, or conversions"));
        empty.add_css_class("blink-empty");
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        empty.set_hexpand(true);
        empty.set_vexpand(true);

        list_col.append(&scroll);
        list_col.append(&empty);

        // Shared with rows + preview so focus-loss hide is suppressed mid-drag.
        let ignore_focus_loss = Rc::new(Cell::new(false));
        let drag_session = DragSession::new(ignore_focus_loss.clone());
        // Needed so DnD can release exclusive keyboard grab under layer-shell.
        drag_session.bind_window(&window);

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
        let in_settings = Rc::new(Cell::new(false));
        // Bumped on every query change; stale async deep walks are ignored.
        let deep_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        let search_debounce: Rc<RefCell<Option<glib::SourceId>>> =
            Rc::new(RefCell::new(None));

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
            search.connect_changed(move |entry| {
                if let Some(id) = search_debounce.borrow_mut().take() {
                    id.remove();
                }
                let q = entry.text().to_string();
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
                let id = glib::timeout_add_local(
                    std::time::Duration::from_millis(SEARCH_DEBOUNCE_MS),
                    move || {
                        *debounce_slot.borrow_mut() = None;
                        refresh_results(
                            &engine,
                            &q,
                            &list,
                            &empty,
                            &results,
                            &selected,
                            &footer_action,
                            &footer_term,
                            &preview,
                            &deep_gen,
                            &search_for_deep,
                            &drag_session,
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
            Rc::new(move || {
                in_settings.set(false);
                stack.set_visible_child_name("search");
                search.grab_focus();
                refresh_results(
                    &engine,
                    &search.text(),
                    &list,
                    &empty,
                    &results,
                    &selected,
                    &footer_action,
                    &footer_term,
                    &preview,
                    &deep_gen,
                    &search,
                    &drag_session,
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
            let open_settings = open_settings.clone();
            list.connect_row_activated(move |_, row| {
                let idx = row.index() as usize;
                selected.set(idx);
                activate_result(&engine, &results, idx, &window, &search, &open_settings);
            });
        }

        {
            let selected = selected.clone();
            let results = results.clone();
            let footer_action = footer_action.clone();
            let footer_term = footer_term.clone();
            let preview = preview.clone();
            list.connect_row_selected(move |_, row| {
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
            let open_settings = open_settings.clone();
            search.connect_activate(move |_| {
                activate_result(
                    &engine,
                    &results,
                    selected.get(),
                    &window,
                    &search_for_activate,
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
                            &open_settings,
                        );
                        glib::Propagation::Stop
                    }
                    Key::comma if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                        open_settings();
                        glib::Propagation::Stop
                    }
                    Key::Down | Key::Tab => {
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
            drag_session,
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
        self.search.set_text("");
        refresh_results(
            &self.engine,
            "",
            &self.list,
            &self.empty,
            &self.results,
            &self.selected,
            &self.footer_action,
            &self.footer_term,
            &self.preview,
            &self.deep_gen,
            &self.search,
            &self.drag_session,
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
        self.preview.clear();
    }
}

fn activate_result<F: Fn()>(
    engine: &Engine,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    idx: usize,
    window: &ApplicationWindow,
    search: &Entry,
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
                    engine.record_usage(&item.id);
                }
                window.set_visible(false);
            }
        }
    }
}

fn refresh_results(
    engine: &Arc<Engine>,
    query: &str,
    list: &ListBox,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &Label,
    footer_term: &GtkBox,
    preview: &Rc<PreviewPanel>,
    deep_gen: &Rc<Cell<u64>>,
    search_entry: &Entry,
    drag_session: &DragSession,
) {
    // Never tear down rows mid-drag — that cancels the DnD session.
    if drag_session.is_active() {
        return;
    }
    let ui = engine.config().get().ui;
    let icon_size = ui.icon_size as i32;
    let symbolic_icons = ui.symbolic_icons;


    // Invalidate any in-flight async deep walk for a previous query.
    let gen = deep_gen.get().wrapping_add(1);
    deep_gen.set(gen);

    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let found = engine.search(query);
    empty.set_visible(found.is_empty());
    list.set_visible(!found.is_empty());

    for (i, item) in found.iter().enumerate() {
        let row = build_row(item, i == 0, drag_session, icon_size, symbolic_icons);
        list.append(&row);
    }

    selected.set(0);
    *results.borrow_mut() = found;

    if let Some(row) = list.row_at_index(0) {
        list.select_row(Some(&row));
        // Explicitly refresh chrome even if selection signal is coalesced.
        update_footer(results, 0, footer_action, footer_term);
        let item = results.borrow().first().cloned();
        preview.update(item.as_ref());
    } else {
        update_footer(results, 0, footer_action, footer_term);
        preview.clear();
    }

    // Async live deep: schedule only when index results are weak / specific query.
    let q = query.to_string();
    if q.trim().is_empty() {
        return;
    }
    let current = results.borrow().clone();
    if !engine.should_deep_search(&q, &current) {
        return;
    }

    let engine = engine.clone();
    let list = list.clone();
    let empty = empty.clone();
    let results = results.clone();
    let selected = selected.clone();
    let footer_action = footer_action.clone();
    let footer_term = footer_term.clone();
    let preview = preview.clone();
    let deep_gen = deep_gen.clone();
    let search_entry = search_entry.clone();
    let drag_session = drag_session.clone();

    // Worker → main thread (event-driven; no 16 ms poll waking the UI loop).
    let (tx, rx) = async_channel::bounded::<Vec<SearchResult>>(1);
    let q_worker = q.clone();
    let engine_worker = engine.clone();
    std::thread::spawn(move || {
        let deep = engine_worker.search_files_deep(&q_worker);
        let _ = tx.send_blocking(deep);
    });

    glib::spawn_future_local(async move {
        let Ok(deep_hits) = rx.recv().await else {
            return;
        };
        // Stale generation or user already typed something else.
        if deep_gen.get() != gen {
            return;
        }
        if search_entry.text().as_str() != q.as_str() {
            return;
        }
        if deep_hits.is_empty() {
            return;
        }
        let ui = engine.config().get().ui;
        apply_deep_hits(
            &deep_hits,
            &list,
            &empty,
            &results,
            &selected,
            &footer_action,
            &footer_term,
            &preview,
            &drag_session,
            ui.icon_size as i32,
            ui.symbolic_icons,
        );
    });
}

/// Merge async deep file hits into the current result list without clobbering
/// selection when the user has already moved.
fn apply_deep_hits(
    deep_hits: &[SearchResult],
    list: &ListBox,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &Label,
    footer_term: &GtkBox,
    preview: &Rc<PreviewPanel>,
    drag_session: &DragSession,
    icon_size: i32,
    symbolic_icons: bool,
) {
    // Rebuilding rows would cancel an active drag.
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

    // Re-sort like Engine::search (score, kind, title).
    merged.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| kind_rank_ui(a.kind).cmp(&kind_rank_ui(b.kind)))
            .then_with(|| a.title.cmp(&b.title))
    });
    merged.truncate(25);

    // Rebuild list rows.
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    empty.set_visible(merged.is_empty());
    list.set_visible(!merged.is_empty());
    for item in merged.iter() {
        let row = build_row(item, false, drag_session, icon_size, symbolic_icons);
        list.append(&row);
    }

    // Restore selection by id when possible.
    let new_sel = prev_id
        .and_then(|id| merged.iter().position(|r| r.id == id))
        .unwrap_or(0);
    selected.set(new_sel);
    *results.borrow_mut() = merged;

    if let Some(row) = list.row_at_index(new_sel as i32) {
        list.select_row(Some(&row));
        update_footer(results, new_sel, footer_action, footer_term);
        let item = results.borrow().get(new_sel).cloned();
        preview.update(item.as_ref());
    } else {
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
    Some((
        mon.get("x")?.as_i64()? as i32,
        mon.get("y")?.as_i64()? as i32,
        mon.get("width")?.as_i64()? as i32,
        mon.get("height")?.as_i64()? as i32,
    ))
}
