mod action_panel;
mod dnd;
mod footer;
mod open_with;
mod preview;
pub(crate) mod rows;
mod scroll_anim;
mod settings;
mod size_anim;
mod thumbnails;

use crate::engine::{Engine, ExecuteOutcome};
use crate::providers::{
    formula_text, unformatted_value, Action, ActionSpec, ResultKind, SearchResult,
};
use crate::theme::ThemeManager;
use action_panel::ActionPanel;
use dnd::DragSession;
use footer::{action_chip_button, footer_divider, keycap_label, update_footer, FooterPrimary};
use gio::prelude::*;
use gio::Cancellable;
use gtk::gdk::Key;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, Label, ListBox,
    ListBoxRow, Orientation, PolicyType, ScrolledWindow, Stack, Viewport,
};
use preview::PreviewPanel;
use rows::{HeroAnim, ResultRowPool};
use settings::SettingsPanel;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(feature = "layer-shell")]
use std::time::{Duration, Instant};

/// Compact fixed outer width. When the media preview opens, the window widens
/// by PREVIEW_WIDTH + separator (see `preview.set_visibility_cb` below).
pub(crate) const WINDOW_WIDTH: i32 = 720;
pub(crate) const EXPANDED_WINDOW_HEIGHT: i32 = 480; // Vicinae 770×480 (1.60) / Raycast 750×474 (1.58) — 720×480=1.50 fits preview 380+90
pub(crate) const COMPACT_WINDOW_HEIGHT: i32 = 110;
/// Extra transparent margin around the rounded shell (for soft drop-shadow).
/// Keep at 0 — a non-zero square inset reads as "padding" on Sway/Hyprland
/// because the layer surface is rectangular while the card is rounded.
const SHELL_INSET: i32 = 0;
/// Debounce keystrokes before search + async deep (cuts typing CPU spikes).
/// 40ms felt jumpy — every intermediate prefix like `1` → `10` flashed
/// `No results for "1"`. 75ms coalesces fast typing while staying snappy.
const SEARCH_DEBOUNCE_MS: u64 = 75;
/// Auto-detect translate queries: longer settle so paste/IME does not spawn workers per glyph.
const TRANSLATE_DEBOUNCE_MS: u64 = 180;
/// Delay before showing `No results` when stale results exist — keeps the
/// old list visible during rapid typing instead of flashing empty.
const EMPTY_STALE_DELAY_MS: u64 = 140;
/// Fade-out duration before the layer surface unmaps (app-side close pop).
const HIDE_FADE_MS: u64 = 110;

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
    footer_action: FooterPrimary,
    preview: Rc<PreviewPanel>,
    deep_gen: Rc<Cell<u64>>,
    /// Pending search debounce timer (cancelled on each keystroke / hide).
    search_debounce: Rc<RefCell<Option<glib::SourceId>>>,
    /// Async deep/translate jobs still in flight (drives the spinner icon).
    async_pending: Rc<Cell<u32>>,
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
    /// Results middle section + separators (animated for compact idle).
    body: GtkBox,
    body_revealer: gtk::Revealer,
    footer_sep: gtk::Separator,
    shell: GtkBox,
    /// Previous non-empty query for stale-retain gate (prevents `1` → `10` flash).
    prev_query: Rc<RefCell<String>>,
    /// Delayed empty display when stale results are kept.
    empty_delay: Rc<RefCell<Option<glib::SourceId>>>,
    /// Pending fade-out timer for the app-side close pop.
    hide_delay: Rc<RefCell<Option<glib::SourceId>>>,
    /// Pending deferred `preview.clear()` armed by `hide()` (cancelled by `show()`).
    preview_clear: Rc<RefCell<Option<glib::SourceId>>>,
    /// Compact↔expanded resize tween (app-side; compositor anims are off).
    size_anim: size_anim::SizeTweener,
    #[allow(dead_code)]
    theme: Rc<ThemeManager>,
}

impl Launcher {
    pub fn new(app: &Application, engine: Arc<Engine>) -> Self {
        let window = ApplicationWindow::builder()
            .application(app)
            .css_classes(["hark-window"])
            .build();

        window.set_hide_on_close(true);
        setup_window_chrome(&window);

        let theme = ThemeManager::new(engine.config());

        // Frame hugs the shell. Expanding it leaves a transparent rectangle that
        // Hyprland layer-blur still samples around the rounded card (square halo).
        let frame = GtkBox::new(Orientation::Vertical, 0);
        frame.add_css_class("hark-frame");
        frame.set_hexpand(false);
        frame.set_vexpand(false);
        frame.set_halign(gtk::Align::Center);
        frame.set_valign(gtk::Align::Start);

        let shell = GtkBox::new(Orientation::Vertical, 0);
        shell.add_css_class("hark-shell");
        shell.set_hexpand(true);
        shell.set_vexpand(true);
        shell.set_halign(gtk::Align::Fill);
        shell.set_valign(gtk::Align::Fill);
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
        search_view.set_vexpand(true);
        search_view.set_valign(gtk::Align::Fill);

        let header = GtkBox::new(Orientation::Vertical, 0);
        header.add_css_class("hark-header");
        header.set_hexpand(true);

        let search = Entry::builder()
            .placeholder_text("Search apps, files, math, or tr translate…")
            .css_classes(["hark-search"])
            .hexpand(true)
            .build();
        search.set_primary_icon_name(Some("system-search-symbolic"));
        header.append(&search);

        let header_sep = gtk::Separator::new(Orientation::Horizontal);
        header_sep.add_css_class("hark-sep");

        let body = GtkBox::new(Orientation::Horizontal, 0);
        body.add_css_class("hark-body");
        body.set_hexpand(true);
        body.set_vexpand(true);

        // Fluid height expansion: body + header separator slide down together
        // instead of snapping between compact idle and populated results.
        let body_revealer = gtk::Revealer::new();
        body_revealer.add_css_class("hark-body-revealer");
        body_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        body_revealer.set_transition_duration(220);
        body_revealer.set_hexpand(true);
        body_revealer.set_vexpand(false);
        body_revealer.set_reveal_child(true);
        {
            let body_wrap = GtkBox::new(Orientation::Vertical, 0);
            body_wrap.append(&header_sep);
            body_wrap.append(&body);
            body_revealer.set_child(Some(&body_wrap));
        }

        let list_col = GtkBox::new(Orientation::Vertical, 0);
        list_col.add_css_class("hark-list-col");
        list_col.set_hexpand(true);
        list_col.set_vexpand(true);

        let scroll = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::External)
            .min_content_height(260)
            .max_content_height(320)
            .propagate_natural_height(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        scroll.add_css_class("hark-scroll");
        scroll.set_overlay_scrolling(false);

        let list = ListBox::new();
        list.add_css_class("hark-results");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.set_vexpand(false);
        scroll.set_child(Some(&list));

        let empty = Label::new(Some("Type to search apps, files, math, or conversions"));
        empty.add_css_class("hark-empty");
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        empty.set_justify(gtk::Justification::Center);
        empty.set_wrap(true);
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

        let preview = Rc::new(PreviewPanel::new(drag_session.clone(), theme.is_light()));
        // Preview pane only appears for media (images / video / audio).

        // Preview now lives inside constant 1001×470 surface — no window
        // widen. List expands to 1001 when preview hidden, 720+280 when shown.
        {
            let _shell = shell.clone();
            let _window = window.clone();
            preview.set_visibility_cb(move |_vis| {
                // No window resize here — window stays 1001×470 (expanded) or
                // 720×110 (compact) via apply_body_chrome. Preview just
                // toggles its own visibility inside fixed surface, so no
                // texture expansion ghost on gemi→gemin.
            });
        }

        body.append(&list_col);
        body.append(preview.separator());
        body.append(preview.widget());

        let footer_sep = gtk::Separator::new(Orientation::Horizontal);
        footer_sep.add_css_class("hark-sep");

        // Slim footer: Settings (left) · primary Enter (center) · Actions (right)
        let footer = GtkBox::new(Orientation::Horizontal, 0);
        footer.add_css_class("hark-footer");
        footer.set_hexpand(true);

        let settings_chip = action_chip_button("Settings", "Ctrl ,");
        settings_chip.set_halign(gtk::Align::Start);
        settings_chip.set_valign(gtk::Align::Center);

        let left_div = footer_divider();

        let primary = GtkBox::new(Orientation::Horizontal, 8);
        primary.add_css_class("hark-footer-primary");
        primary.set_halign(gtk::Align::Start);
        primary.set_hexpand(true);
        primary.set_valign(gtk::Align::Center);

        let footer_action = FooterPrimary::new();

        let enter_key = keycap_label("↵");
        primary.append(&footer_action.action);
        primary.append(&enter_key);
        primary.append(&footer_action.value_chip);
        primary.append(&footer_action.formula_chip);

        let actions_chip = action_chip_button("Actions", "Ctrl K");
        actions_chip.set_halign(gtk::Align::End);
        actions_chip.set_valign(gtk::Align::Center);
        actions_chip.add_css_class("hark-footer-actions");

        let action_panel = ActionPanel::new(&actions_chip);

        footer.append(&settings_chip);
        footer.append(&left_div);
        footer.append(&primary);
        footer.append(&actions_chip);

        search_view.append(&header);
        search_view.append(&body_revealer);
        search_view.append(&footer_sep);
        search_view.append(&footer);

        // App-side resize tween for compact↔expanded (see size_anim module).
        // Declared before the initial layout call below.
        let size_anim = size_anim::SizeTweener::new();

        // Compact idle: search + footer only (Raycast compact). Expanded when typing.
        {
            let compact0 = matches!(
                engine.config().snapshot().ui.layout_mode,
                crate::config::LayoutMode::Compact
            );
            apply_body_chrome(
                compact0,
                true,
                &body,
                &body_revealer,
                &footer_sep,
                Some(&scroll),
                Some(&window),
                Some(&shell),
                Some(&size_anim),
            );
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
        // Last arrow-key direction (+1 down / -1 up) for scroll lookahead.
        let nav_dir: Rc<Cell<i32>> = Rc::new(Cell::new(0));
        // Animated follow-scroll for keyboard selection; rebuilds snap.
        let scroll_anim = scroll_anim::ScrollTweener::new();
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
        let search_debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let async_pending: Rc<Cell<u32>> = Rc::new(Cell::new(0));
        let session_queries: Rc<RefCell<VecDeque<String>>> =
            Rc::new(RefCell::new(VecDeque::with_capacity(12)));
        let prev_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let empty_delay: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        // Pending fade-out timer for the app-side close pop (compositor
        // layer animation is off — no_anim — because it ghosts on resize).
        let hide_delay: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let preview_clear: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

        {
            let engine = engine.clone();
            let list = list.clone();
            let empty = empty.clone();
            let results = results.clone();
            let selected = selected.clone();
            let footer_action = footer_action.clone();
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
            let body_revealer_c = body_revealer.clone();
            let footer_sep_c = footer_sep.clone();
            let scroll_c = scroll.clone();
            let session_queries = session_queries.clone();
            let async_pending = async_pending.clone();
            let empty_delay_search = empty_delay.clone();
            let prev_query_search = prev_query.clone();
            let window_c = window.clone();
            let shell_c = shell.clone();
            let size_anim_c = size_anim.clone();
            search.connect_changed(move |entry| {
                if let Some(id) = search_debounce.borrow_mut().take() {
                    id.remove();
                }
                if let Some(id) = empty_delay_search.borrow_mut().take() {
                    id.remove();
                }
                let q = entry.text().to_string();
                note_session_query(&session_queries, &q);
                // Icon feedback is instant — don't wait for the debounce timer.
                // A keystroke invalidates any async state tied to the old query.
                async_pending.set(0);
                entry.set_primary_icon_name(Some(search_mode_icon(&q)));
                update_search_icons(entry, &async_pending);
                // Expand/collapse body immediately (don't wait for search debounce).
                // This stays instant for compact idle → typing, but no longer
                // forces a window resize on every keystroke (see apply_body_chrome).
                apply_body_chrome(
                    ui_compact.get(),
                    q.trim().is_empty(),
                    &body_c,
                    &body_revealer_c,
                    &footer_sep_c,
                    Some(&scroll_c),
                    Some(&window_c),
                    Some(&shell_c),
                    Some(&size_anim_c),
                );
                let engine = engine.clone();
                let list = list.clone();
                let empty = empty.clone();
                let results = results.clone();
                let selected = selected.clone();
                let footer_action = footer_action.clone();
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
                let body_revealer_c = body_revealer_c.clone();
                let footer_sep_c = footer_sep_c.clone();
                let scroll_c = scroll_c.clone();
                let async_pending = async_pending.clone();
                let prev_query_c = prev_query_search.clone();
                let empty_delay_c = empty_delay_search.clone();
                let window_tc = window_c.clone();
                let shell_tc = shell_c.clone();
                let size_anim_tc = size_anim_c.clone();
                // Longer settle only for auto script paste/IME (not forced `tr …`).
                let wait_ms = if engine.translate_is_auto_query(&q) {
                    TRANSLATE_DEBOUNCE_MS
                } else {
                    SEARCH_DEBOUNCE_MS
                };
                let id =
                    glib::timeout_add_local(std::time::Duration::from_millis(wait_ms), move || {
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
                            &preview,
                            &deep_gen,
                            &async_pending,
                            &search_for_deep,
                            &drag_session,
                            &suppress_select,
                            &ui_icon_size,
                            &ui_symbolic,
                            &ui_compact,
                            &body_c,
                            &body_revealer_c,
                            &footer_sep_c,
                            Some(&scroll_c),
                            &prev_query_c,
                            &empty_delay_c,
                            Some(&window_tc),
                            Some(&shell_tc),
                            Some(&size_anim_tc),
                        );
                        glib::ControlFlow::Break
                    });
                *search_debounce.borrow_mut() = Some(id);
            });
        }

        // Pointer users: click the trailing `×` to clear the query (Raycast).
        search.connect_icon_press(move |entry, pos| {
            if pos == gtk::EntryIconPosition::Secondary {
                entry.set_text("");
            }
        });

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
            let preview = preview.clone();
            let deep_gen = deep_gen.clone();
            let drag_session = drag_session.clone();
            let suppress_select = suppress_select.clone();
            let ui_icon_size = ui_icon_size.clone();
            let ui_symbolic = ui_symbolic.clone();
            let ui_compact = ui_compact.clone();
            let row_pool = row_pool.clone();
            let body_cs = body.clone();
            let body_revealer_cs = body_revealer.clone();
            let footer_sep_cs = footer_sep.clone();
            let scroll_cs = scroll.clone();
            let async_pending_cs = async_pending.clone();
            let prev_query_cs = prev_query.clone();
            let empty_delay_cs = empty_delay.clone();
            let window_cs = window.clone();
            let shell_cs = shell.clone();
            let size_anim_cs = size_anim.clone();
            Rc::new(move || {
                in_settings.set(false);
                stack.set_visible_child_name("search");
                search.grab_focus();
                // Settings may have changed icon prefs / layout.
                let ui = engine.config().snapshot().ui.clone();
                ui_icon_size.set(ui.icon_size as i32);
                ui_symbolic.set(ui.symbolic_icons);
                ui_compact.set(matches!(ui.layout_mode, crate::config::LayoutMode::Compact));
                refresh_results(
                    &engine,
                    &search.text(),
                    &list,
                    &row_pool,
                    &empty,
                    &results,
                    &selected,
                    &footer_action,
                    &preview,
                    &deep_gen,
                    &async_pending_cs,
                    &search,
                    &drag_session,
                    &suppress_select,
                    &ui_icon_size,
                    &ui_symbolic,
                    &ui_compact,
                    &body_cs,
                    &body_revealer_cs,
                    &footer_sep_cs,
                    Some(&scroll_cs),
                    &prev_query_cs,
                    &empty_delay_cs,
                    Some(&window_cs),
                    Some(&shell_cs),
                    Some(&size_anim_cs),
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

        // Secondary action panel: populate + run chosen action.
        let open_action_panel = {
            let engine = engine.clone();
            let results = results.clone();
            let selected = selected.clone();
            let action_panel = action_panel.clone();
            let window = window.clone();
            let search = search.clone();
            let session_queries = session_queries.clone();
            let open_settings = open_settings.clone();
            let list = list.clone();
            let row_pool = row_pool.clone();
            let empty = empty.clone();
            let footer_action = footer_action.clone();
            let preview = preview.clone();
            let drag_session = drag_session.clone();
            let suppress_select = suppress_select.clone();
            let ui_icon_size = ui_icon_size.clone();
            let ui_symbolic = ui_symbolic.clone();
            let ignore_focus_loss = ignore_focus_loss.clone();
            let actions_chip = actions_chip.clone();
            Rc::new(move || {
                let idx = selected.get();
                let item = match results.borrow().get(idx).cloned() {
                    Some(i) => i,
                    None => {
                        action_panel.close();
                        return;
                    }
                };
                let specs = engine.secondary_actions(&item);
                // Wire the callback *before* opening so a fast click never
                // races an empty on_activate.
                let engine = engine.clone();
                let window = window.clone();
                let search = search.clone();
                let session_queries = session_queries.clone();
                let open_settings = open_settings.clone();
                let results = results.clone();
                let selected = selected.clone();
                let list = list.clone();
                let row_pool = row_pool.clone();
                let empty = empty.clone();
                let footer_action = footer_action.clone();
                let preview = preview.clone();
                let drag_session = drag_session.clone();
                let suppress_select = suppress_select.clone();
                let ui_icon_size = ui_icon_size.clone();
                let ui_symbolic = ui_symbolic.clone();
                let ignore_focus_loss = ignore_focus_loss.clone();
                let actions_chip = actions_chip.clone();
                let item = item.clone();
                // Popover click can briefly mark the layer window inactive;
                // keep hide suppressed while the panel is open / action runs.
                ignore_focus_loss.set(true);
                {
                    let ignore_focus_loss = ignore_focus_loss.clone();
                    action_panel.set_on_activate(Rc::new(move |spec| {
                        ignore_focus_loss.set(true);
                        run_secondary_action(
                            &engine,
                            spec,
                            &item,
                            &window,
                            &search,
                            &session_queries,
                            &open_settings,
                            &results,
                            &selected,
                            &list,
                            &row_pool,
                            &empty,
                            &footer_action,
                            &preview,
                            &drag_session,
                            &suppress_select,
                            &ui_icon_size,
                            &ui_symbolic,
                            &ignore_focus_loss,
                            &actions_chip,
                        );
                    }));
                }
                if !action_panel.open_for(specs) {
                    ignore_focus_loss.set(false);
                }
            })
        };

        {
            let open_action_panel = open_action_panel.clone();
            actions_chip.connect_clicked(move |_| open_action_panel());
        }

        // Right-click a result: select it and open the Action Panel (Raycast's
        // context-menu analog). Attached to the ListBox so pooled rows share it.
        {
            let selected = selected.clone();
            let open_action_panel = open_action_panel.clone();
            let list_rc = list.clone();
            let right = gtk::GestureClick::new();
            right.set_button(3);
            right.connect_pressed(move |_, _, _, y| {
                if let Some(row) = list_rc.row_at_y(y as i32) {
                    let idx = row.index() as usize;
                    selected.set(idx);
                    list_rc.select_row(Some(&row));
                    open_action_panel();
                }
            });
            list.add_controller(right);
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
            let preview = preview.clone();
            let suppress_select = suppress_select.clone();
            let nav_dir = nav_dir.clone();
            let scroll_anim = scroll_anim.clone();
            let list_sel = list.clone();
            let row_pool = row_pool.clone();
            let ui_icon_size = ui_icon_size.clone();
            let ui_symbolic = ui_symbolic.clone();
            list.connect_row_selected(move |_, row| {
                let Some(row) = row else { return };
                // Keep keyboard/mouse selection in view even while focus stays on search.
                // Consume the arrow direction for one-row scroll lookahead AND for
                // direction-aware hero animation (↓=slide-up, ↑=slide-down, click=crossfade).
                let dir = nav_dir.replace(0);
                ensure_row_visible(row, dir, &scroll_anim);
                let idx = row.index() as usize;

                // Conversion prediction set (picker wheel): the hero card is
                // pinned to row 0. Selecting any plain row wheels that value
                // into the card (directional slide) and returns selection to the card.
                if is_conv_set(&results.borrow()) {
                    if idx != 0 && !suppress_select.get() {
                        conv_swap_to_front(&mut results.borrow_mut(), idx);
                        {
                            let rs = results.borrow();
                            let mut pool = row_pool.borrow_mut();
                            let anim = Some(HeroAnim::from_dir(dir));
                            pool.apply(&list_sel, &rs, ui_icon_size.get(), ui_symbolic.get(), anim);
                        }
                        if let Some(card) = row_pool.borrow().row_at(0).cloned() {
                            suppress_select.set(true);
                            list_sel.select_row(Some(&card));
                            suppress_select.set(false);
                        }
                    }
                    selected.set(0);
                    if suppress_select.get() {
                        return;
                    }
                    update_footer(&results, 0, &footer_action);
                    refresh_preview_at(&results, 0, &preview);
                    return;
                }

                selected.set(idx);
                if suppress_select.get() {
                    return;
                }
                update_footer(&results, idx, &footer_action);
                refresh_preview_at(&results, idx, &preview);
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
            let open_action_panel = open_action_panel.clone();
            let action_panel = action_panel.clone();
            let row_pool = row_pool.clone();
            let empty = empty.clone();
            let footer_action = footer_action.clone();
            let preview = preview.clone();
            let drag_session = drag_session.clone();
            let suppress_select = suppress_select.clone();
            let nav_dir = nav_dir.clone();
            let ui_icon_size = ui_icon_size.clone();
            let ui_symbolic = ui_symbolic.clone();
            let ignore_focus_loss = ignore_focus_loss.clone();
            let actions_chip = actions_chip.clone();
            let shell_esc = shell.clone();
            let hide_delay_esc = hide_delay.clone();
            let dismiss_settings_overlay = settings.dismiss_overlay_handle();

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
                    if window
                        .root()
                        .and_then(|r| r.focus())
                        .and_downcast::<gtk::Editable>()
                        .is_some()
                    {
                        return glib::Propagation::Proceed;
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

                // Action panel captures navigation while open.
                if action_panel.is_open() {
                    match keyval {
                        Key::Escape => {
                            action_panel.close();
                            search.grab_focus_without_selecting();
                            return glib::Propagation::Stop;
                        }
                        Key::Down | Key::j | Key::J => {
                            action_panel.move_selection(1);
                            return glib::Propagation::Stop;
                        }
                        Key::Up | Key::k | Key::K => {
                            action_panel.move_selection(-1);
                            return glib::Propagation::Stop;
                        }
                        Key::Return | Key::KP_Enter => {
                            if let Some(spec) = action_panel.activate_selected() {
                                let idx = selected.get();
                                if let Some(item) = results.borrow().get(idx).cloned() {
                                    run_secondary_action(
                                        &engine,
                                        spec,
                                        &item,
                                        &window,
                                        &search,
                                        &session_queries,
                                        &open_settings,
                                        &results,
                                        &selected,
                                        &list,
                                        &row_pool,
                                        &empty,
                                        &footer_action,
                                        &preview,
                                        &drag_session,
                                        &suppress_select,
                                        &ui_icon_size,
                                        &ui_symbolic,
                                        &ignore_focus_loss,
                                        &actions_chip,
                                    );
                                }
                            }
                            return glib::Propagation::Stop;
                        }
                        _ => {}
                    }
                }

                match keyval {
                    Key::Escape => {
                        // Raycast 2-stage lifecycle: first Escape clears a typed
                        // query; a second Escape (empty field) dismisses the app.
                        if !search.text().is_empty() {
                            search.set_text("");
                            search.set_position(0);
                        } else {
                            dismiss(&window, &shell_esc, &hide_delay_esc);
                        }
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
                        if ctrl
                            && matches!(
                                results.borrow().get(selected.get()).map(|i| i.kind),
                                Some(ResultKind::Calc) | Some(ResultKind::Conversion)
                            )
                        {
                            // Raycast calculator bindings on calc/conversion rows:
                            // ⌘↵ (Ctrl+Enter) copies the unformatted value,
                            // ⌘⇧↵ (Ctrl+Shift+Enter) copies question + answer.
                            // Instant close on copy — no toast, no linger.
                            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                            // Scoped borrow: derive the owned copy-text without
                            // cloning the whole row (same class as audit P3).
                            let text = results.borrow().get(selected.get()).and_then(|it| {
                                if shift {
                                    formula_text(it)
                                } else {
                                    unformatted_value(it)
                                }
                            });
                            if let Some(text) = text {
                                engine.execute(&Action::Copy(text));
                                window.set_visible(false);
                                return glib::Propagation::Stop;
                            }
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
                    Key::k | Key::K
                        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            && !state.contains(gtk::gdk::ModifierType::SHIFT_MASK)
                            && !state.contains(gtk::gdk::ModifierType::ALT_MASK) =>
                    {
                        open_action_panel();
                        glib::Propagation::Stop
                    }
                    Key::Tab => {
                        // Tab → autocomplete selected suggestion into the search box
                        // (↓/↑ still navigate). Soft SetQuery scopes fill their query.
                        tab_complete_selected(&results, &selected, &list, &search);
                        glib::Propagation::Stop
                    }
                    Key::Down => {
                        let conv_next = {
                            let rs = results.borrow();
                            is_conv_set(&rs)
                                .then(|| conv_nav_row(rs.len(), conv_current_rank(&rs), true))
                        };
                        if let Some(next) = conv_next {
                            nav_dir.set(1);
                            // Picker wheel: ↓ wheels the next ranked target
                            // into the fixed card; the row-selected handler
                            // performs the swap and keeps selection on it.
                            if let Some(row) = list.row_at_index(next as i32) {
                                list.select_row(Some(&row));
                                search.grab_focus_without_selecting();
                            }
                        } else {
                            let len = results.borrow().len();
                            if len > 0 {
                                nav_dir.set(1);
                                let next = (selected.get() + 1) % len;
                                selected.set(next);
                                if let Some(row) = list.row_at_index(next as i32) {
                                    list.select_row(Some(&row));
                                    search.grab_focus_without_selecting();
                                }
                            }
                        }
                        glib::Propagation::Stop
                    }
                    Key::Right => {
                        // At end of query, → opens the Action Panel (Raycast
                        // parity). Mid-text, → keeps moving the caret.
                        // `position()` counts chars, `text().len()` counts
                        // bytes — compare against the char count or non-ASCII
                        // queries never detect at-end.
                        let at_end = search.position() >= search.text().chars().count() as i32
                            && !action_panel.is_open();
                        if at_end && !results.borrow().is_empty() {
                            open_action_panel();
                            glib::Propagation::Stop
                        } else {
                            glib::Propagation::Proceed
                        }
                    }
                    Key::Up | Key::ISO_Left_Tab => {
                        let conv_next = {
                            let rs = results.borrow();
                            is_conv_set(&rs)
                                .then(|| conv_nav_row(rs.len(), conv_current_rank(&rs), false))
                        };
                        if let Some(next) = conv_next {
                            nav_dir.set(-1);
                            if let Some(row) = list.row_at_index(next as i32) {
                                list.select_row(Some(&row));
                                search.grab_focus_without_selecting();
                            }
                        } else {
                            let len = results.borrow().len();
                            if len > 0 {
                                nav_dir.set(-1);
                                let cur = selected.get();
                                let next = if cur == 0 { len - 1 } else { cur - 1 };
                                selected.set(next);
                                if let Some(row) = list.row_at_index(next as i32) {
                                    list.select_row(Some(&row));
                                    search.grab_focus_without_selecting();
                                }
                            }
                        }
                        glib::Propagation::Stop
                    }
                    Key::c | Key::C
                        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            && state.contains(gtk::gdk::ModifierType::SHIFT_MASK) =>
                    {
                        // Ctrl+Shift+C → copy path / desktop path
                        let idx = selected.get();
                        if let Some(item) = results.borrow().get(idx).cloned() {
                            for spec in engine.secondary_actions(&item) {
                                if spec.id == "copy_path" {
                                    run_secondary_action(
                                        &engine,
                                        spec,
                                        &item,
                                        &window,
                                        &search,
                                        &session_queries,
                                        &open_settings,
                                        &results,
                                        &selected,
                                        &list,
                                        &row_pool,
                                        &empty,
                                        &footer_action,
                                        &preview,
                                        &drag_session,
                                        &suppress_select,
                                        &ui_icon_size,
                                        &ui_symbolic,
                                        &ignore_focus_loss,
                                        &actions_chip,
                                    );
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                        glib::Propagation::Proceed
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
                    Key::r | Key::R
                        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            && state.contains(gtk::gdk::ModifierType::SHIFT_MASK) =>
                    {
                        // Ctrl+Shift+R → reveal in file manager
                        let idx = selected.get();
                        if let Some(item) = results.borrow().get(idx).cloned() {
                            for spec in engine.secondary_actions(&item) {
                                if spec.id == "reveal" {
                                    run_secondary_action(
                                        &engine,
                                        spec,
                                        &item,
                                        &window,
                                        &search,
                                        &session_queries,
                                        &open_settings,
                                        &results,
                                        &selected,
                                        &list,
                                        &row_pool,
                                        &empty,
                                        &footer_action,
                                        &preview,
                                        &drag_session,
                                        &suppress_select,
                                        &ui_icon_size,
                                        &ui_symbolic,
                                        &ignore_focus_loss,
                                        &actions_chip,
                                    );
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                        glib::Propagation::Proceed
                    }
                    Key::o | Key::O
                        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            && state.contains(gtk::gdk::ModifierType::SHIFT_MASK) =>
                    {
                        // Ctrl+Shift+O → Open With…
                        let idx = selected.get();
                        if let Some(item) = results.borrow().get(idx).cloned() {
                            for spec in engine.secondary_actions(&item) {
                                if spec.id == "open_with" {
                                    run_secondary_action(
                                        &engine,
                                        spec,
                                        &item,
                                        &window,
                                        &search,
                                        &session_queries,
                                        &open_settings,
                                        &results,
                                        &selected,
                                        &list,
                                        &row_pool,
                                        &empty,
                                        &footer_action,
                                        &preview,
                                        &drag_session,
                                        &suppress_select,
                                        &ui_icon_size,
                                        &ui_symbolic,
                                        &ignore_focus_loss,
                                        &actions_chip,
                                    );
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                        glib::Propagation::Proceed
                    }
                    Key::p | Key::P
                        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            && !state.contains(gtk::gdk::ModifierType::SHIFT_MASK)
                            && !state.contains(gtk::gdk::ModifierType::ALT_MASK) =>
                    {
                        // Ctrl+P → toggle media preview panel
                        preview.toggle_user_hidden();
                        let sel = selected.get();
                        refresh_preview_at(&results, sel, &preview);
                        glib::Propagation::Stop
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
            preview,
            deep_gen,
            search_debounce,
            async_pending,
            session_queries: session_queries.clone(),
            drag_session,
            suppress_select,
            ui_icon_size,
            ui_symbolic,
            ui_compact,
            body,
            body_revealer,
            footer_sep,
            shell,
            prev_query,
            empty_delay,
            hide_delay,
            preview_clear,
            size_anim,
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
        // Cancel a fade-out in progress (rapid hotkey double-tap reopens).
        if let Some(id) = self.hide_delay.borrow_mut().take() {
            id.remove();
        }
        // Cancel a pending deferred preview.clear() so it cannot blank a
        // freshly populated preview after re-show (audit P2).
        if let Some(id) = self.preview_clear.borrow_mut().take() {
            id.remove();
        }
        self.shell.remove_css_class("hark-anim-out");
        self.ignore_focus_loss.set(true);
        self.in_settings.set(false);
        self.stack.set_visible_child_name("search");
        self.session_queries.borrow_mut().clear();
        self.search.set_text("");
        // Pick up apps installed while the daemon was already running.
        // Desktop scan is cheap; without this, new .desktop entries only
        // appear after the 45m periodic refresh or a full restart.
        self.engine.reload_apps();
        // Refresh cached appearance from config (settings may have changed while hidden).
        let ui = self.engine.config().snapshot().ui.clone();
        self.ui_icon_size.set(ui.icon_size as i32);
        self.ui_symbolic.set(ui.symbolic_icons);
        self.ui_compact
            .set(matches!(ui.layout_mode, crate::config::LayoutMode::Compact));
        refresh_results(
            &self.engine,
            "",
            &self.list,
            &self.row_pool,
            &self.empty,
            &self.results,
            &self.selected,
            &self.footer_action,
            &self.preview,
            &self.deep_gen,
            &self.async_pending,
            &self.search,
            &self.drag_session,
            &self.suppress_select,
            &self.ui_icon_size,
            &self.ui_symbolic,
            &self.ui_compact,
            &self.body,
            &self.body_revealer,
            &self.footer_sep,
            None,
            &self.prev_query,
            &self.empty_delay,
            Some(&self.window),
            Some(&self.shell),
            Some(&self.size_anim),
        );
        self.settings.refresh_status();
        self.window.set_visible(true);
        self.window.present();
        // Entrance pop inside the surface — compositor layer animation is
        // off (`no_anim`) because box interpolation ghosts on resize.
        self.shell.remove_css_class("hark-anim-in");
        self.shell.add_css_class("hark-anim-in");
        let shell_in = self.shell.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(240), move || {
            shell_in.remove_css_class("hark-anim-in");
        });
        self.search.grab_focus();
        center_on_active_monitor(&self.window);

        let ignore = self.ignore_focus_loss.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            ignore.set(false);
        });
    }

    pub fn hide(&self) {
        // App-side close pop: fade the card inside the surface, unmap after.
        // Compositor layer animation is off (`no_anim` layerrule) because box
        // interpolation ghosts on resize (docs/hyprland-layer-corners.md).
        dismiss(&self.window, &self.shell, &self.hide_delay);
        // Drop pending search / deep-walk work while dismissing. Invisible
        // state clears now; `preview.clear()` waits — hiding the preview
        // panel can resize the window mid-fade.
        if let Some(id) = self.search_debounce.borrow_mut().take() {
            id.remove();
        }
        if let Some(id) = self.empty_delay.borrow_mut().take() {
            id.remove();
        }
        self.deep_gen.set(self.deep_gen.get().wrapping_add(1));
        self.async_pending.set(0);
        self.search.remove_css_class("hark-search-busy");
        self.search.set_secondary_icon_name(None);
        self.session_queries.borrow_mut().clear();
        self.prev_query.borrow_mut().clear();
        // Track the deferred clear so show() can cancel it — otherwise an
        // orphaned timer blanks a freshly populated preview after re-show.
        if let Some(id) = self.preview_clear.borrow_mut().take() {
            id.remove();
        }
        let preview = self.preview.clone();
        let slot = self.preview_clear.clone();
        let window = self.window.clone();
        *self.preview_clear.borrow_mut() = Some(glib::timeout_add_local_once(
            std::time::Duration::from_millis(HIDE_FADE_MS),
            move || {
                *slot.borrow_mut() = None;
                // Second guard behind the show() cancellation above: if
                // the window was re-shown, new content may already be
                // populated — never blank it.
                if !window.is_visible() {
                    preview.clear();
                }
            },
        ));
    }
}

/// Fade the card out inside the surface, then unmap. Compositor layer
/// animation is disabled (`no_anim` layerrule) because box interpolation
/// ghosts on surface resize — open/close animate the content instead
/// (docs/hyprland-layer-corners.md). No-op while a fade is already running
/// (rapid double-dismiss); `show()` cancels a pending fade.
fn dismiss(
    window: &ApplicationWindow,
    shell: &GtkBox,
    hide_delay: &Rc<RefCell<Option<glib::SourceId>>>,
) {
    if hide_delay.borrow().is_some() {
        return;
    }
    shell.add_css_class("hark-anim-out");
    let window = window.clone();
    let shell = shell.clone();
    let hide_delay_c = hide_delay.clone();
    *hide_delay.borrow_mut() = Some(glib::timeout_add_local_once(
        std::time::Duration::from_millis(HIDE_FADE_MS),
        move || {
            window.set_visible(false);
            shell.remove_css_class("hark-anim-out");
            *hide_delay_c.borrow_mut() = None;
        },
    ));
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
        Action::Copy(_)
        | Action::OpenSettings
        | Action::RevealPath(_)
        | Action::TrashPath(_)
        | Action::OpenWith(_)
        | Action::TogglePreview => {
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
#[allow(clippy::items_after_test_module)]
mod conv_picker_tests {
    use super::{conv_current_rank, conv_nav_row, conv_swap_to_front, is_conv_set};
    use crate::providers::{Action, ConversionView, ResultKind, SearchResult};

    fn conv(id: &str, score: i64) -> SearchResult {
        SearchResult {
            id: id.into(),
            title: format!("{id} title"),
            subtitle: String::new(),
            kind: ResultKind::Conversion,
            score,
            icon: None,
            action: Action::Copy(id.into()),
            conversion: Some(ConversionView {
                left_title: "10 kg".into(),
                left_badge: "mass".into(),
                right_title: format!("{id} value"),
                right_badge: id.into(),
            }),
            matched: None,
        }
    }

    fn plain(id: &str) -> SearchResult {
        SearchResult {
            id: id.into(),
            title: id.into(),
            subtitle: String::new(),
            kind: ResultKind::App,
            score: 0,
            icon: None,
            action: Action::LaunchApp {
                exec: id.into(),
                terminal: false,
                desktop_path: None,
            },
            conversion: None,
            matched: None,
        }
    }

    fn ids(rs: &[SearchResult]) -> Vec<&str> {
        rs.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn only_pure_multi_conversion_sets_are_picker_surface() {
        assert!(is_conv_set(&[conv("a", 3), conv("b", 2)]));
        // Single exact conversion: card row, but no wheel.
        assert!(!is_conv_set(&[conv("a", 3)]));
        // Mixed lists never wheel.
        assert!(!is_conv_set(&[conv("a", 3), plain("app")]));
        assert!(!is_conv_set(&[]));
    }

    #[test]
    fn wheel_math_down_up_wrap_and_rank_recovery() {
        // Ranks A > B > C > D (scores 40/30/20/10). Wheel is now cyclic
        // — the list under the hero always previews the next stops.
        let mut rs = vec![conv("A", 40), conv("B", 30), conv("C", 20), conv("D", 10)];
        assert_eq!(conv_current_rank(&rs), 0);
        assert_eq!(ids(&rs), ["A", "B", "C", "D"]);

        // ↓: next circular target B at row 1.
        let r = conv_nav_row(4, conv_current_rank(&rs), true);
        assert_eq!(r, 1);
        conv_swap_to_front(&mut rs, r);
        assert_eq!(ids(&rs), ["B", "C", "D", "A"]);
        assert_eq!(conv_current_rank(&rs), 1, "A still outranks B");

        // ↓ again: C is now at row 1 (directly under hero).
        let r = conv_nav_row(4, conv_current_rank(&rs), true);
        assert_eq!(r, 1);
        conv_swap_to_front(&mut rs, r);
        assert_eq!(ids(&rs), ["C", "D", "A", "B"]);
        assert_eq!(conv_current_rank(&rs), 2);

        // ↑ from C: previous circular target B is at last row.
        let r = conv_nav_row(4, conv_current_rank(&rs), false);
        assert_eq!(r, 3);
        conv_swap_to_front(&mut rs, r);
        assert_eq!(ids(&rs), ["B", "C", "D", "A"]);
        assert_eq!(conv_current_rank(&rs), 1);

        // ↓ through D then wrap to A.
        conv_swap_to_front(&mut rs, conv_nav_row(4, 1, true)); // → C
        conv_swap_to_front(&mut rs, conv_nav_row(4, 2, true)); // → D
        assert_eq!(ids(&rs), ["D", "A", "B", "C"]);
        assert_eq!(conv_current_rank(&rs), 3);
        assert_eq!(conv_nav_row(4, conv_current_rank(&rs), true), 1);

        // ↓ wraps to A, ↑ at A wraps to D.
        conv_swap_to_front(&mut rs, 1); // D → A
        assert_eq!(ids(&rs), ["A", "B", "C", "D"]);
        assert_eq!(conv_current_rank(&rs), 0);
        assert_eq!(conv_nav_row(4, 0, false), 3);
    }

    #[test]
    fn swap_rejects_card_row_and_out_of_range() {
        let mut rs = vec![conv("A", 40), conv("B", 30)];
        conv_swap_to_front(&mut rs, 0); // card row: no-op
        conv_swap_to_front(&mut rs, 9); // out of range: no-op
        assert_eq!(ids(&rs), ["A", "B"]);
        conv_swap_to_front(&mut rs, 1);
        assert_eq!(ids(&rs), ["B", "A"]);
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tab_complete_tests {
    use super::{
        chain_is_conversion, complete_path_query, completion_text_for, is_path_shaped_query,
        looks_like_calc_query, search_mode_icon,
    };
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
            matched: None,
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
        assert!(completed.starts_with("~/Documents"), "got {completed}");
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

    #[test]
    fn search_icon_morphs_with_mode() {
        assert_eq!(search_mode_icon(""), "system-search-symbolic");
        assert_eq!(search_mode_icon("  "), "system-search-symbolic");
        assert_eq!(search_mode_icon("firefox"), "system-search-symbolic");
        assert_eq!(
            search_mode_icon("tr hola"),
            "preferences-desktop-locale-symbolic"
        );
        assert_eq!(
            search_mode_icon("translate bonjour"),
            "preferences-desktop-locale-symbolic"
        );

        // Theme-dependent modes are classified here; the concrete icon name is
        // resolved against the active theme at runtime (headless tests have
        // no display, so only the classifier is asserted).
        assert!(looks_like_calc_query("25 * 4"));
        assert!(looks_like_calc_query("100 lbs in kg"));
        assert!(!looks_like_calc_query("firefox"));
        assert!(chain_is_conversion("100 lbs in kg"));
        assert!(!chain_is_conversion("25 * 4"));
        assert!(crate::providers::calc::currency::looks_like_currency_query(
            "50 eur to usd",
        ));
        assert!(!crate::providers::calc::currency::looks_like_currency_query("100 lbs in kg"));
    }
}

/// Apply an action from the secondary panel (or a keyboard shortcut).
#[allow(clippy::too_many_arguments)]
fn run_secondary_action<F: Fn() + 'static>(
    engine: &Arc<Engine>,
    spec: ActionSpec,
    item: &SearchResult,
    window: &ApplicationWindow,
    search: &Entry,
    session_queries: &Rc<RefCell<VecDeque<String>>>,
    open_settings: &Rc<F>,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    footer_action: &FooterPrimary,
    preview: &Rc<PreviewPanel>,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    ui_icon_size: &Rc<Cell<i32>>,
    ui_symbolic: &Rc<Cell<bool>>,
    ignore_focus_loss: &Rc<Cell<bool>>,
    open_with_anchor: &gtk::Button,
) {
    let finish = {
        let engine = engine.clone();
        let window = window.clone();
        let search = search.clone();
        let session_queries = session_queries.clone();
        let open_settings = open_settings.clone();
        let results = results.clone();
        let selected = selected.clone();
        let list = list.clone();
        let row_pool = row_pool.clone();
        let empty = empty.clone();
        let footer_action = footer_action.clone();
        let preview = preview.clone();
        let drag_session = drag_session.clone();
        let suppress_select = suppress_select.clone();
        let ui_icon_size = ui_icon_size.clone();
        let ui_symbolic = ui_symbolic.clone();
        let item_id = item.id.clone();
        let item_kind = item.kind;
        let item_title = item.title.clone();
        let ignore_focus_loss = ignore_focus_loss.clone();
        let open_with_anchor = open_with_anchor.clone();
        move |spec: ActionSpec| {
            let outcome = engine.execute(&spec.action);
            match outcome {
                ExecuteOutcome::OpenSettings => open_settings(),
                ExecuteOutcome::SetQuery(q) => {
                    search.set_text(&q);
                    search.set_position(-1);
                    search.grab_focus_without_selecting();
                }
                ExecuteOutcome::Launched => {
                    if matches!(spec.id, "open" | "terminal" | "reveal" | "reveal_install")
                        && !matches!(
                            item_kind,
                            ResultKind::Calc | ResultKind::Conversion | ResultKind::Command
                        )
                    {
                        let final_q = search.text().to_string();
                        let recent: Vec<String> =
                            session_queries.borrow().iter().cloned().collect();
                        engine.learn_typos(&final_q, &recent, &item_id, &item_title);
                        engine.record_usage(&item_id);
                    }
                    window.set_visible(false);
                }
                ExecuteOutcome::Refresh => {
                    // Drop the trashed row immediately so the UI feels responsive
                    // even if the on-disk index still lists the path briefly.
                    {
                        let mut rs = results.borrow_mut();
                        rs.retain(|r| r.id != item_id);
                        if let Action::TrashPath(path) = &spec.action {
                            let path_id = format!("path:{}", path.display());
                            rs.retain(|r| r.id != path_id);
                        }
                    }
                    rebind_results_from_cache(
                        &list,
                        &row_pool,
                        &empty,
                        &results,
                        &selected,
                        &footer_action,
                        &preview,
                        &drag_session,
                        &suppress_select,
                        ui_icon_size.get(),
                        ui_symbolic.get(),
                    );
                    search.grab_focus_without_selecting();
                }
                ExecuteOutcome::Failed => {
                    search.grab_focus_without_selecting();
                }
                ExecuteOutcome::OpenWith(path) => {
                    open_with::show_open_with_picker(
                        &open_with_anchor,
                        &window,
                        path,
                        ignore_focus_loss.clone(),
                    );
                }
                ExecuteOutcome::TogglePreview => {
                    preview.toggle_user_hidden();
                    // Re-apply current selection so the panel shows again when un-hidden.
                    let sel = selected.get();
                    refresh_preview_at(&results, sel, &preview);
                    search.grab_focus_without_selecting();
                }
            }
        }
    };

    if spec.destructive {
        let title = item.title.clone();
        let message = format!("Move \"{title}\" to Trash?");
        let detail = match &spec.action {
            Action::TrashPath(p) => p.display().to_string(),
            _ => title.clone(),
        };
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(&message)
            .detail(&detail)
            .buttons(["Cancel", "Move to Trash"])
            .cancel_button(0)
            .default_button(1)
            .build();
        ignore_focus_loss.set(true);
        let ignore_focus_loss = ignore_focus_loss.clone();
        let window = window.clone();
        dialog.choose(Some(&window), None::<&Cancellable>, move |result| {
            ignore_focus_loss.set(false);
            if matches!(result, Ok(1)) {
                finish(spec);
            }
        });
    } else {
        finish(spec);
    }
}

/// Re-render the list from the in-memory `results` vec (no re-search).
#[allow(clippy::too_many_arguments)]
fn rebind_results_from_cache(
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &FooterPrimary,
    preview: &Rc<PreviewPanel>,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    icon_size: i32,
    symbolic_icons: bool,
) {
    if drag_session.is_active() {
        return;
    }
    // Render directly from the borrowed vec (audit P3): the old
    // `results.borrow().clone()` copied all 25 rows per rebind.
    {
        let found = results.borrow();
        let no_hits = found.is_empty();
        let idx = selected.get().min(found.len().saturating_sub(1));
        empty.set_visible(no_hits);
        empty.set_vexpand(no_hits);
        list.set_visible(!no_hits);

        {
            let mut pool = row_pool.borrow_mut();
            if found.is_empty() {
                pool.clear(list);
            } else {
                pool.apply(list, &found, icon_size, symbolic_icons, None);
            }
        }

        selected.set(if found.is_empty() { 0 } else { idx });
    }

    if let Some(row) = row_pool.borrow().row_at(selected.get()).cloned() {
        suppress_select.set(true);
        list.select_row(Some(&row));
        suppress_select.set(false);
        update_footer(results, selected.get(), footer_action);
        let sel = selected.get();
        refresh_preview_at(results, sel, preview);
    } else {
        list.select_row(Option::<&ListBoxRow>::None);
        update_footer(results, 0, footer_action);
        preview.clear();
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
                    let recent: Vec<String> = session_queries.borrow().iter().cloned().collect();
                    engine.learn_typos(&final_q, &recent, &item.id, &item.title);
                    engine.record_usage(&item.id);
                }
                window.set_visible(false);
            }
            // Primary Enter never produces these; secondary actions handle them.
            ExecuteOutcome::Refresh
            | ExecuteOutcome::Failed
            | ExecuteOutcome::OpenWith(_)
            | ExecuteOutcome::TogglePreview => {}
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

#[allow(clippy::too_many_arguments)]
fn apply_body_chrome(
    compact: bool,
    query_empty: bool,
    body: &GtkBox,
    body_revealer: &gtk::Revealer,
    footer_sep: &gtk::Separator,
    scroll: Option<&ScrolledWindow>,
    window: Option<&ApplicationWindow>,
    shell: Option<&GtkBox>,
    size_anim: Option<&size_anim::SizeTweener>,
) {
    // Compact + idle query → search bar + footer only (no middle body).
    let show_body = !(compact && query_empty);
    let was_shown = body_revealer.reveals_child();
    footer_sep.set_visible(true);
    if was_shown != show_body {
        // Animated expand/collapse (SlideDown, 150ms) — no 0ms layout snap.
        body_revealer.set_reveal_child(show_body);
        if show_body {
            body.remove_css_class("hark-body-collapsed");
            body.set_vexpand(true);
            if let Some(s) = scroll {
                s.set_min_content_height(260);
                s.set_vexpand(true);
            }
        } else {
            body.add_css_class("hark-body-collapsed");
            body.set_vexpand(false);
            if let Some(s) = scroll {
                s.set_min_content_height(0);
                s.set_vexpand(false);
            }
        }
    } else {
        // Staying expanded/collapsed: keep stable min but allow natural
        // height to follow content (e.g. 7 rows `res` → 1 row `gemi`).
        // Don't force window resize; just ensure body reflects current mode.
        if show_body {
            body.remove_css_class("hark-body-collapsed");
            if let Some(s) = scroll {
                s.set_min_content_height(260);
            }
        }
    }
    // Variable height when expanded — window+shell follow content
    // (header+body+footer) to avoid awkward 470h empty gap for 4 rows
    // like `ge`. Both are set together to same size so no transparent
    // gap (ghost) appears between window and shell. Window stays 720
    // wide so preview show/hide doesn't widen window — gemi→gemin no
    // resize ghost. This also keeps window == shell so rounded corners
    // have no visible rectangular window backing (the "padding" square).
    let target_w = WINDOW_WIDTH;
    let target_h = if show_body {
        EXPANDED_WINDOW_HEIGHT
    } else {
        COMPACT_WINDOW_HEIGHT
    };
    match (window, shell, size_anim) {
        (Some(win), Some(sh), Some(anim)) => anim.glide(win, sh, target_w, target_h),
        (Some(win), Some(sh), None) => {
            win.set_size_request(target_w, target_h);
            sh.set_size_request(target_w, target_h);
        }
        _ => {}
    }
    body.queue_resize();
    if let Some(sh) = shell {
        sh.queue_resize();
    }
}

/// Sync the search field's trailing icon:
/// - spinner (`process-working-symbolic`, CSS-spun) while async deep/translate jobs run;
/// - otherwise a `×` clear button when text is present;
/// - nothing on an empty idle field.
fn update_search_icons(search: &Entry, async_pending: &Rc<Cell<u32>>) {
    if async_pending.get() > 0 {
        search.set_secondary_icon_name(Some("process-working-symbolic"));
        search.add_css_class("hark-search-busy");
    } else {
        search.remove_css_class("hark-search-busy");
        if search.text().trim().is_empty() {
            search.set_secondary_icon_name(None);
        } else {
            search.set_secondary_icon_name(Some("edit-clear-symbolic"));
        }
    }
}

/// Context-aware leading icon: morph the magnifier into the detected query mode
/// (calculator, conversion, clock, locale/translation) — Raycast-style glyph.
fn search_mode_icon(query: &str) -> &'static str {
    use crate::providers::translate::{looks_like_translatable_script, strip_translate_prefix};

    let q = query.trim();
    if q.is_empty() {
        return "system-search-symbolic";
    }
    let lower = q.to_ascii_lowercase();
    let (forced, text) = strip_translate_prefix(q);
    if forced || looks_like_translatable_script(text) {
        return "preferences-desktop-locale-symbolic";
    }
    if matches!(
        lower.as_str(),
        "now" | "time" | "date" | "today" | "tomorrow" | "yesterday" | "utc" | "unix" | "epoch"
    ) {
        return mode_clock_icon();
    }
    if looks_like_calc_query(q) {
        if crate::providers::calc::currency::looks_like_currency_query(q) {
            return mode_currency_icon();
        }
        if chain_is_conversion(q) {
            return mode_convert_icon();
        }
        return mode_calc_icon();
    }
    "system-search-symbolic"
}

fn chain_is_conversion(q: &str) -> bool {
    q.contains(" to ") || q.contains(" in ") || q.contains(" as ") || q.contains("->")
}

// ── Conversion picker wheel ─────────────────────────────────────────────────
// A prediction set binds as a circular queue in rank order. Row 0 is the
// fixed hero card; ↓/↑ and clicks wheel values through it picker-wheel
// style (rotate + directional slide). The queue order is the initial
// engine rank (score descending); rotations preserve that circular order
// so the row directly under the hero is always the next ↓ target —
// fixing the "list doesn't match next card" confusion.

/// A pure multi-target conversion prediction set (the picker surface).
fn is_conv_set(rs: &[SearchResult]) -> bool {
    rs.len() > 1 && rs.iter().all(|r| r.conversion.is_some())
}

/// Rank of the target currently inside the card: how many remaining targets
/// outrank it. Kept for diagnostics; wheel navigation no longer uses rank
/// (it is circular), but the value still recovers position in the original
/// sorted order.
fn conv_current_rank(rs: &[SearchResult]) -> usize {
    match rs.first() {
        Some(cur) => rs[1..].iter().filter(|r| r.score > cur.score).count(),
        None => 0,
    }
}

/// Row holding the next (down) / previous (up) target in the circular
/// queue. `rank` is ignored — the wheel is now strictly cyclic so ↓ is
/// always row 1 and ↑ is always the last row; this keeps the visual list
/// in sync with what the next key press will show.
fn conv_nav_row(len: usize, _rank: usize, down: bool) -> usize {
    if len <= 1 {
        0
    } else if down {
        1
    } else {
        len - 1
    }
}

/// Wheel one value into the card: rotate the circular queue so the chosen
/// row becomes the hero. This preserves the original rank order circularly,
/// so the list under the hero always previews the next wheel stops in
/// sequence (cyclic), not a re-sorted best-first list.
fn conv_swap_to_front(rs: &mut [SearchResult], row: usize) {
    if row > 0 && row < rs.len() {
        rs.rotate_left(row);
    }
}

/// Push row `idx` into the preview without cloning the `SearchResult`.
///
/// The old pattern (`results.borrow().first().cloned()` → `update(opt.as_ref())`)
/// cloned title/subtitle/id/action/`PathBuf` per keystroke just to pass `&`.
/// `PreviewPanel::update` clones internally what it retains, so a scoped
/// borrow alive only for the call is sufficient (audit P3 Pass 21).
fn refresh_preview_at(
    results: &Rc<RefCell<Vec<SearchResult>>>,
    idx: usize,
    preview: &Rc<PreviewPanel>,
) {
    let borrowed = results.borrow();
    preview.update(borrowed.get(idx));
}

// Mode icons resolve candidate chains against the active theme at runtime:
// no single name ships everywhere (`convert-symbolic` is breeze-only;
// Adwaita/Papirus have neither it nor a currency glyph).
fn mode_calc_icon() -> &'static str {
    crate::ui::rows::resolve_icon_name(&[
        "accessories-calculator-symbolic",
        "view-refresh-symbolic",
    ])
}

fn mode_convert_icon() -> &'static str {
    crate::ui::rows::resolve_icon_name(&[
        "convert-symbolic",
        "emblem-synchronizing-symbolic",
        "view-refresh-symbolic",
    ])
}

fn mode_currency_icon() -> &'static str {
    crate::ui::rows::resolve_icon_name(&[
        "format-currency-symbolic",
        "convert-symbolic",
        "emblem-synchronizing-symbolic",
        "view-refresh-symbolic",
    ])
}

fn mode_clock_icon() -> &'static str {
    crate::ui::rows::resolve_icon_name(&[
        "clock-symbolic",
        "preferences-system-time-symbolic",
        "view-refresh-symbolic",
    ])
}

fn looks_like_calc_query(q: &str) -> bool {
    q.bytes().any(|b| b.is_ascii_digit())
        || q.contains('+')
        || q.contains('*')
        || q.contains('/')
        || q.contains('%')
        || q.contains('^')
        || q.contains('=')
        || q.contains(" to ")
        || q.contains(" in ")
        || q.contains(" as ")
        || q.contains("->")
        || q.chars()
            .any(|c| matches!(c, '$' | '€' | '£' | '¥' | '₹' | '₩' | '₽'))
}

#[allow(clippy::too_many_arguments)]
fn refresh_results(
    engine: &Arc<Engine>,
    query: &str,
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &FooterPrimary,
    preview: &Rc<PreviewPanel>,
    deep_gen: &Rc<Cell<u64>>,
    async_pending: &Rc<Cell<u32>>,
    search_entry: &Entry,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    ui_icon_size: &Rc<Cell<i32>>,
    ui_symbolic: &Rc<Cell<bool>>,
    ui_compact: &Rc<Cell<bool>>,
    body: &GtkBox,
    body_revealer: &gtk::Revealer,
    footer_sep: &gtk::Separator,
    scroll: Option<&ScrolledWindow>,
    prev_query: &Rc<RefCell<String>>,
    empty_delay: &Rc<RefCell<Option<glib::SourceId>>>,
    window: Option<&ApplicationWindow>,
    shell: Option<&GtkBox>,
    size_anim: Option<&size_anim::SizeTweener>,
) {
    // Never rebind rows mid-drag — that would cancel the DnD session.
    if drag_session.is_active() {
        return;
    }
    // Cancel any pending delayed empty from a previous intermediate prefix.
    if let Some(id) = empty_delay.borrow_mut().take() {
        id.remove();
    }
    let icon_size = ui_icon_size.get();
    let symbolic_icons = ui_symbolic.get();
    let compact = ui_compact.get();
    let query_empty = query.trim().is_empty();

    apply_body_chrome(
        compact,
        query_empty,
        body,
        body_revealer,
        footer_sep,
        scroll,
        window,
        shell,
        size_anim,
    );

    // Invalidate any in-flight async deep/translate for a previous query.
    let gen = deep_gen.get().wrapping_add(1);
    deep_gen.set(gen);
    // This refresh re-owns the spinner: drop pending tallies from stale jobs
    // (their futures bail on the gen check without decrementing).
    async_pending.set(0);
    update_search_icons(search_entry, async_pending);

    // Compact idle: skip ranking recents — body is hidden anyway.
    let found = if compact && query_empty {
        Vec::new()
    } else {
        engine.search(query)
    };
    let no_hits = found.is_empty();
    let prev_had = !results.borrow().is_empty();

    // Stale-retain: intermediate prefixes like `1` in `win 10` often yield
    // 0 hits for 40-80ms then `10` yields hits. Flashing `No results for "1"`
    // feels jumpy. Keep the previous list visible for EMPTY_STALE_DELAY_MS
    // instead of instantly clearing; only show empty if still empty after delay.
    if no_hits && prev_had && !query_empty && !(compact && query_empty) {
        *prev_query.borrow_mut() = query.to_string();
        empty.set_visible(false);
        empty.set_vexpand(false);
        list.set_visible(true);
        // Keep the old pool/results on screen (stale) while we wait.
        let q2 = query.to_string();
        let engine2 = engine.clone();
        let list2 = list.clone();
        let row_pool2 = row_pool.clone();
        let empty2 = empty.clone();
        let results2 = results.clone();
        let selected2 = selected.clone();
        let footer2 = footer_action.clone();
        let preview2 = preview.clone();
        let search_entry2 = search_entry.clone();
        let empty_delay2 = empty_delay.clone();
        let scroll2 = scroll.cloned();
        let drag2 = drag_session.clone();
        let suppress2 = suppress_select.clone();
        let icon_size2 = icon_size;
        let symb2 = symbolic_icons;
        let id = glib::timeout_add_local(
            std::time::Duration::from_millis(EMPTY_STALE_DELAY_MS),
            move || {
                *empty_delay2.borrow_mut() = None;
                if search_entry2.text().as_str() != q2.as_str() {
                    return glib::ControlFlow::Break;
                }
                if drag2.is_active() {
                    return glib::ControlFlow::Break;
                }
                // Re-search to confirm still empty (user may have typed more).
                let still = if q2.trim().is_empty() {
                    Vec::new()
                } else {
                    engine2.search(&q2)
                };
                if still.is_empty() {
                    empty2.set_markup(&empty_state_markup(&q2));
                    empty2.set_visible(true);
                    empty2.set_vexpand(true);
                    list2.set_visible(false);
                    if let Some(s) = &scroll2 {
                        s.set_visible(false);
                    }
                    row_pool2.borrow_mut().clear(&list2);
                    *results2.borrow_mut() = Vec::new();
                    selected2.set(0);
                    suppress2.set(true);
                    list2.select_row(Option::<&ListBoxRow>::None);
                    suppress2.set(false);
                    update_footer(&results2, 0, &footer2);
                    preview2.clear();
                } else {
                    // Hits appeared (e.g. `10` after `1`); render them.
                    empty2.set_visible(false);
                    empty2.set_vexpand(false);
                    list2.set_visible(true);
                    if let Some(s) = &scroll2 {
                        s.set_visible(true);
                    }
                    row_pool2
                        .borrow_mut()
                        .apply(&list2, &still, icon_size2, symb2, None);
                    *results2.borrow_mut() = still;
                    selected2.set(0);
                    if let Some(row) = row_pool2.borrow().row_at(0).cloned() {
                        suppress2.set(true);
                        list2.select_row(Some(&row));
                        suppress2.set(false);
                        update_footer(&results2, 0, &footer2);
                        refresh_preview_at(&results2, 0, &preview2);
                    }
                }
                glib::ControlFlow::Break
            },
        );
        *empty_delay.borrow_mut() = Some(id);
        // Keep stale list on screen; still allow async translate/deep below
        // to potentially fill in hits before the delay fires.
    } else {
        // In compact idle the empty placeholder is not shown (body hidden).
        let show_empty = no_hits && !(compact && query_empty);
        if show_empty {
            if query_empty {
                empty.set_text("Type to search apps, files, math, or conversions");
            } else {
                empty.set_markup(&empty_state_markup(query));
            }
        }
        empty.set_visible(show_empty);
        empty.set_vexpand(show_empty);
        list.set_visible(!no_hits);
        // A hidden list must not leave the 260px min-content scroll ghost
        // behind the empty state — it made the page taller than any results
        // view and pushed the label off-center.
        if let Some(s) = scroll {
            s.set_visible(!no_hits);
        }

        {
            let mut pool = row_pool.borrow_mut();
            if found.is_empty() {
                pool.clear(list);
            } else {
                pool.apply(list, &found, icon_size, symbolic_icons, None);
            }
        }

        selected.set(0);
        *results.borrow_mut() = found;
        *prev_query.borrow_mut() = query.to_string();

        if let Some(row) = row_pool.borrow().row_at(0).cloned() {
            suppress_select.set(true);
            list.select_row(Some(&row));
            suppress_select.set(false);
            update_footer(results, 0, footer_action);
            refresh_preview_at(results, 0, preview);
        } else {
            list.select_row(Option::<&ListBoxRow>::None);
            update_footer(results, 0, footer_action);
            preview.clear();
        }
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
        let preview_t = preview.clone();
        let deep_gen_t = deep_gen.clone();
        let search_entry_t = search_entry.clone();
        let drag_session_t = drag_session.clone();
        let suppress_t = suppress_select.clone();
        let icon_size_t = ui_icon_size.clone();
        let symbolic_t = ui_symbolic.clone();
        let async_pending_t = async_pending.clone();
        let q_t = q.clone();
        let gen_t = gen;
        async_pending.set(async_pending.get() + 1);
        update_search_icons(search_entry, async_pending);
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
            let pending = async_pending_t.get().saturating_sub(1);
            async_pending_t.set(pending);
            update_search_icons(&search_entry_t, &async_pending_t);
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
                &preview_t,
                &drag_session_t,
                &suppress_t,
                icon_size_t.get(),
                symbolic_t.get(),
            );
        });
    }

    // Borrow only — no full SearchResult vec clone for the deep gate.
    if !engine.should_deep_search(&q, results.borrow().as_slice()) {
        return;
    }

    let engine = engine.clone();
    let list = list.clone();
    let row_pool = row_pool.clone();
    let empty = empty.clone();
    let results = results.clone();
    let selected = selected.clone();
    let footer_action = footer_action.clone();
    let preview = preview.clone();
    let deep_gen = deep_gen.clone();
    let search_entry = search_entry.clone();
    let drag_session = drag_session.clone();
    let suppress_select = suppress_select.clone();
    let ui_icon_size = ui_icon_size.clone();
    let ui_symbolic = ui_symbolic.clone();
    let async_pending = async_pending.clone();

    async_pending.set(async_pending.get() + 1);
    update_search_icons(&search_entry, &async_pending);
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
        let pending = async_pending.get().saturating_sub(1);
        async_pending.set(pending);
        update_search_icons(&search_entry, &async_pending);
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
#[allow(clippy::too_many_arguments)]
fn apply_translate_hits(
    hits: &[SearchResult],
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &FooterPrimary,
    preview: &Rc<PreviewPanel>,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    icon_size: i32,
    symbolic_icons: bool,
) {
    if drag_session.is_active() || hits.is_empty() {
        return;
    }

    // Take ownership instead of `borrow().clone()` (audit P3): existing rows
    // move into `out`, only id `String`s clone for the dedup set.
    let mut existing = std::mem::take(&mut *results.borrow_mut());
    existing.retain(|r| !crate::providers::translate::is_pending_result(r));
    let mut out = hits.to_vec();
    let mut seen: std::collections::HashSet<String> = out.iter().map(|r| r.id.clone()).collect();
    for r in existing {
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
            pool.apply(list, &out, icon_size, symbolic_icons, None);
        }
    }
    selected.set(0);
    *results.borrow_mut() = out;

    if let Some(row) = row_pool.borrow().row_at(0).cloned() {
        suppress_select.set(true);
        list.select_row(Some(&row));
        suppress_select.set(false);
        update_footer(results, 0, footer_action);
        refresh_preview_at(results, 0, preview);
    } else {
        list.select_row(Option::<&ListBoxRow>::None);
        update_footer(results, 0, footer_action);
        preview.clear();
    }
}

/// Merge async deep file hits into the current result list without clobbering
/// selection when the user has already moved.
#[allow(clippy::too_many_arguments)]
fn apply_deep_hits(
    deep_hits: &[SearchResult],
    list: &ListBox,
    row_pool: &Rc<RefCell<ResultRowPool>>,
    empty: &Label,
    results: &Rc<RefCell<Vec<SearchResult>>>,
    selected: &Rc<Cell<usize>>,
    footer_action: &FooterPrimary,
    preview: &Rc<PreviewPanel>,
    drag_session: &DragSession,
    suppress_select: &Rc<Cell<bool>>,
    icon_size: i32,
    symbolic_icons: bool,
) {
    if drag_session.is_active() {
        return;
    }

    let prev_id = results.borrow().get(selected.get()).map(|r| r.id.clone());

    // Take ownership instead of `borrow().clone()` (audit P3): existing rows
    // move, only id `String`s clone for the dedup set. Restored untouched
    // when nothing new arrives.
    let mut merged = std::mem::take(&mut *results.borrow_mut());
    let mut seen: std::collections::HashSet<String> = merged.iter().map(|r| r.id.clone()).collect();
    let mut added = 0usize;
    for r in deep_hits {
        if seen.insert(r.id.clone()) {
            merged.push(r.clone());
            added += 1;
        }
    }
    if added == 0 {
        *results.borrow_mut() = merged;
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
    let new_sel = prev_id
        .and_then(|id| merged.iter().position(|r| r.id == id))
        .unwrap_or(0);
    empty.set_visible(no_hits);
    empty.set_vexpand(no_hits);
    list.set_visible(!no_hits);
    {
        let mut pool = row_pool.borrow_mut();
        if merged.is_empty() {
            pool.clear(list);
        } else {
            pool.apply(list, &merged, icon_size, symbolic_icons, None);
        }
    }

    selected.set(new_sel);
    *results.borrow_mut() = merged;

    if let Some(row) = row_pool.borrow().row_at(new_sel).cloned() {
        suppress_select.set(true);
        list.select_row(Some(&row));
        suppress_select.set(false);
        update_footer(results, new_sel, footer_action);
        refresh_preview_at(results, new_sel, preview);
    } else {
        list.select_row(Option::<&ListBoxRow>::None);
        update_footer(results, 0, footer_action);
        preview.clear();
    }
}

/// Polished zero-hit state: centered, icon + title + hint.
/// Keep hint tied to real syntax (`in <folder>`, `*.ext` globs).
fn empty_state_markup(query: &str) -> String {
    format!(
        "<span font_size=\"28000\" alpha=\"38%\">⌕</span>\n<span font_weight=\"600\" size=\"11000\">No results for “{}”</span>\n<span alpha=\"68%\" size=\"9000\">Try <tt>name in folder</tt> to scope • <tt>*.ext</tt> for globs • check spelling</span>",
        glib::markup_escape_text(query.trim())
    )
}

/// Scroll the highlighted result into the list viewport.
///
/// Arrow-key navigation keeps focus on the search entry (so typing continues
/// uninterrupted). GTK only auto-scrolls ListBox selection when the row has
/// focus, so we drive the surrounding Viewport's adjustment ourselves.
///
/// `dir` is the last arrow direction (+1 down / -1 up / 0 other). In the
/// direction of travel we keep a one-row lookahead so the next item peeks
/// into view before the selection reaches the edge — without nudging on
/// the opposite edge (which made upward navigation feel drifty).
/// `dir != 0` glides to the target (see `scroll_anim`); `dir == 0` snaps
/// (query rebuilds / reorders — content changed, nothing to animate from).
// `allocation()` deprecated since GTK 4.12 with no direct replacement for
// reading child coordinates outside snapshot/rendering — still the right tool.
#[allow(deprecated)]
fn ensure_row_visible(row: &ListBoxRow, dir: i32, anim: &scroll_anim::ScrollTweener) {
    let Some(viewport) = row
        .ancestor(Viewport::static_type())
        .and_then(|w| w.downcast::<Viewport>().ok())
    else {
        return;
    };
    let alloc = row.allocation();
    if alloc.height() <= 0 {
        // Not laid out yet — fall back to GTK's own scroll. Any in-flight
        // glide targets the *previous* list's offsets, so stop it first.
        anim.cancel();
        viewport.scroll_to(row, None);
        return;
    }

    let adj = match viewport.vadjustment() {
        Some(adj) => adj,
        None => {
            anim.cancel();
            viewport.scroll_to(row, None);
            return;
        }
    };
    let top = alloc.y() as f64 - adj.value(); // row top in viewport space
    let bottom = top + alloc.height() as f64;
    let page = adj.page_size();
    let peek = (alloc.height() as f64).min(120.0);

    let mut value = adj.value();
    if bottom > page {
        // Row below the fold; travelling down also reveals the next row.
        value += bottom - page + if dir > 0 { peek } else { 0.0 };
    } else if top < 0.0 {
        // Row above the fold; travelling up also reveals the previous row.
        value += top - if dir < 0 { peek } else { 0.0 };
    }
    let value = value.clamp(adj.lower(), (adj.upper() - page).max(adj.lower()));
    if dir == 0 {
        // Rebuild/reorder: content changed — snap and cancel any in-flight glide.
        anim.snap(&adj, value);
    } else {
        // Keyboard travel: glide, retargeting from the current offset if a
        // glide is already running (chained hops read as one motion).
        anim.glide(&viewport, &adj, value);
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
    // Fallback toplevel geometry (non-layer-shell / X11). These also apply as
    // sane defaults before layer-shell takes over the surface.
    let outer_w = WINDOW_WIDTH + SHELL_INSET * 2;
    window.set_title(Some("Hark"));
    window.set_resizable(false);
    window.set_decorated(false);
    window.set_default_size(outer_w, -1);
    window.set_size_request(outer_w, -1);

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
            window.set_namespace(Some("hark"));
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
            // Prefer the monitor under the *pointer*, not the focused window's
            // monitor. Keyboard-only focus can lag the cursor on multi-monitor
            // setups (window on laptop, pointer on external).
            let (gdk_mon, geom_height) = match hypr_pointer_monitor() {
                Some(info) => (gdk_monitor_for_hypr(&info), Some(info.geom.height)),
                // Non-Hyprland layer-shell compositors (Sway, River, Wayfire)
                // have no hyprctl — fall back to GDK monitor geometry.
                None => (gdk_pointer_monitor(), None),
            };
            let top = geom_height.map(|h| h / 5).unwrap_or(80).max(80);
            if let Some(gdk_mon) = gdk_mon {
                window.set_monitor(Some(&gdk_mon));
            }
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Left, false);
            window.set_anchor(Edge::Right, false);
            window.set_anchor(Edge::Bottom, false);
            window.set_margin(Edge::Top, top);
            return;
        }
    }

    // Non-layer-shell toplevel (X11 / GNOME Wayland): GTK4 has no client-side
    // positioning API — `present()` (called by the caller) delegates placement
    // to the WM, which centers new toplevels by default.
    let _ = window;
}

/// GDK monitor under the pointer — used for non-Hyprland compositors where the
/// Hyprland layerrule subprocess path is unavailable (Sway, River, Wayfire).
#[cfg(feature = "layer-shell")]
fn gdk_pointer_monitor() -> Option<gtk::gdk::Monitor> {
    use gtk::gdk::prelude::*;
    let display = gtk::gdk::Display::default()?;
    let device = display.default_seat()?.pointer()?;
    // The surface under the pointer sits on the monitor the pointer is on.
    let (surface, _, _) = device.surface_at_position();
    if let Some(surface) = surface {
        display.monitor_at_surface(&surface)
    } else {
        let m = display.monitors();
        m.item(0)
            .and_then(|i| i.downcast::<gtk::gdk::Monitor>().ok())
    }
}

#[cfg(feature = "layer-shell")]
#[derive(Clone, Copy, Debug)]
struct HyprMonitorGeom {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[cfg(feature = "layer-shell")]
#[derive(Clone, Debug)]
struct HyprMonitorInfo {
    name: String,
    geom: HyprMonitorGeom,
    focused: bool,
}

/// Monitor that contains the pointer (fallback: Hyprland focused / first).
#[cfg(feature = "layer-shell")]
fn hypr_pointer_monitor() -> Option<HyprMonitorInfo> {
    let mons = fetch_hypr_monitors()?;
    // Single-monitor fast-path: 0 subprocess spawns on the toggle path.
    if let Some(single) = mons.first() {
        if mons.len() == 1 {
            return Some(single.clone());
        }
    }
    // Multi-monitor: one fast `cursorpos -j` against the cached topology.
    if let Some((cx, cy)) = hypr_cursor_pos() {
        if let Some(mon) = mons.iter().find(|m| {
            let g = &m.geom;
            cx >= g.x && cy >= g.y && cx < g.x + g.width && cy < g.y + g.height
        }) {
            return Some(mon.clone());
        }
    }
    focused_or_first(&mons)
}

/// Cached Hyprland monitor topology (5s TTL). The topology rarely changes
/// between toggles, so cache it and avoid blocking process spawns on the hotkey
/// presentation path.
#[cfg(feature = "layer-shell")]
fn fetch_hypr_monitors() -> Option<Vec<HyprMonitorInfo>> {
    const TTL: Duration = Duration::from_secs(5);
    type TopologyCache = OnceLock<Mutex<Option<(Instant, Vec<HyprMonitorInfo>)>>>;
    static CACHE: TopologyCache = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    {
        let g = cache.lock().unwrap();
        if let Some((at, mons)) = g.as_ref() {
            if at.elapsed() < TTL && !mons.is_empty() {
                return Some(mons.clone());
            }
        }
    }

    let mon_out = std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .ok()?;
    if !mon_out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&mon_out.stdout).ok()?;
    let arr = v.as_array()?;

    let mons: Vec<HyprMonitorInfo> = arr
        .iter()
        .filter_map(|m| {
            Some(HyprMonitorInfo {
                name: m.get("name")?.as_str()?.to_string(),
                geom: HyprMonitorGeom {
                    x: m.get("x")?.as_i64()? as i32,
                    y: m.get("y")?.as_i64()? as i32,
                    width: m.get("width")?.as_i64()? as i32,
                    height: m.get("height")?.as_i64()? as i32,
                },
                focused: m.get("focused").and_then(|f| f.as_bool()) == Some(true),
            })
        })
        .collect();
    if mons.is_empty() {
        return None;
    }
    *cache.lock().unwrap() = Some((Instant::now(), mons.clone()));
    Some(mons)
}

#[cfg(feature = "layer-shell")]
fn focused_or_first(mons: &[HyprMonitorInfo]) -> Option<HyprMonitorInfo> {
    mons.iter()
        .find(|m| m.focused)
        .or_else(|| mons.first())
        .cloned()
}

#[cfg(feature = "layer-shell")]
fn hypr_cursor_pos() -> Option<(i32, i32)> {
    let out = std::process::Command::new("hyprctl")
        .args(["cursorpos", "-j"])
        .output()
        .ok()?;
    if out.status.success() {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            if let (Some(x), Some(y)) = (
                v.get("x").and_then(|n| n.as_i64()),
                v.get("y").and_then(|n| n.as_i64()),
            ) {
                return Some((x as i32, y as i32));
            }
        }
    }
    // Plain "x, y" fallback
    let out = std::process::Command::new("hyprctl")
        .args(["cursorpos"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut parts = s.split([',', ' ']).filter(|p| !p.is_empty());
    let x: i32 = parts.next()?.trim().parse().ok()?;
    let y: i32 = parts.next()?.trim().parse().ok()?;
    Some((x, y))
}

/// Map a Hyprland output to a GDK monitor so layer-shell can pin the surface.
#[cfg(feature = "layer-shell")]
fn gdk_monitor_for_hypr(info: &HyprMonitorInfo) -> Option<gtk::gdk::Monitor> {
    let display = gtk::gdk::Display::default()?;
    let monitors = display.monitors();
    let n = monitors.n_items();
    let g = &info.geom;

    let mon_at = |i: u32| -> Option<gtk::gdk::Monitor> {
        monitors.item(i)?.downcast::<gtk::gdk::Monitor>().ok()
    };

    // 1) Connector name match (e.g. "HDMI-A-1", "eDP-2")
    for i in 0..n {
        if let Some(mon) = mon_at(i) {
            if mon.connector().is_some_and(|c| c.as_str() == info.name) {
                return Some(mon);
            }
        }
    }

    // 2) Geometry match (layout coordinates)
    for i in 0..n {
        if let Some(mon) = mon_at(i) {
            let rect = mon.geometry();
            if rect.x() == g.x
                && rect.y() == g.y
                && rect.width() == g.width
                && rect.height() == g.height
            {
                return Some(mon);
            }
        }
    }

    // 3) Point containment of monitor origin (scale-factor edge cases)
    for i in 0..n {
        if let Some(mon) = mon_at(i) {
            let rect = mon.geometry();
            if g.x >= rect.x()
                && g.y >= rect.y()
                && g.x < rect.x() + rect.width()
                && g.y < rect.y() + rect.height()
            {
                return Some(mon);
            }
        }
    }

    None
}
