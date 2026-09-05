//! Result list rows + a fixed pool so searches rebind widgets instead of
//! allocating new trees every keystroke.
//!
//! Unused slots are **removed** from the ListBox (not merely hidden): GTK4
//! ListBox still reserves height for invisible children.
//!
//! Each slot keeps both a standard row widget and a conversion card; we
//! `set_child` the active one — no Stack here because both children can
//! inflate the row's natural height. The card's right panel *does* use a
//! two-child Stack (same widget structure, so no height jump) to crossfade
//! values as they wheel through the fixed hero slot.

use super::dnd::{DragSession, PathDragBinding};
use crate::providers::{ConversionView, ResultKind, SearchResult};
use gtk::glib;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Image, Label, ListBox, ListBoxRow, Orientation, Stack};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

/// Matches engine result cap — pool never needs more than this.
pub(crate) const ROW_POOL_CAP: usize = 25;
/// Soft cap for icon-name → resolved theme name (FIFO eviction, not full clear).
const ICON_CACHE_CAP: usize = 512;

struct IconResolveCache {
    map: HashMap<String, String>,
    /// Insertion order of keys for FIFO eviction (avoids thrashing a full wipe).
    order: VecDeque<String>,
}

impl IconResolveCache {
    fn new() -> Self {
        Self {
            map: HashMap::with_capacity(64),
            order: VecDeque::with_capacity(64),
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: String, value: String) {
        use std::collections::hash_map::Entry;
        if let Entry::Occupied(mut e) = self.map.entry(key.clone()) {
            // Refresh value; keep existing order slot.
            e.insert(value);
            return;
        }
        while self.map.len() >= ICON_CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }
}

struct IconFileCache {
    map: HashMap<String, Option<PathBuf>>,
    order: VecDeque<String>,
}

impl IconFileCache {
    fn new() -> Self {
        Self {
            map: HashMap::with_capacity(64),
            order: VecDeque::with_capacity(64),
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

thread_local! {
    static ICON_RESOLVE_CACHE: RefCell<IconResolveCache> = RefCell::new(IconResolveCache::new());
    /// `Icon=/path/app.png` → `stat()` result. The file branch runs per row
    /// per keystroke on the main thread; without this a cold/hung NFS home
    /// freezes input (audit P3). FIFO-capped like the theme cache; cleared
    /// together with it so a theme/install change re-stats once.
    static ICON_FILE_CACHE: RefCell<IconFileCache> =
        RefCell::new(IconFileCache::new());
    /// Accent hex used for match-highlight spans. Kept in sync with the
    /// active scheme by `ThemeManager::apply` so rows never need theme access.
    static HIGHLIGHT_ACCENT: RefCell<String> = RefCell::new("#7aa2f7".to_string());
}

pub(crate) fn clear_icon_resolve_cache() {
    ICON_RESOLVE_CACHE.with(|c| c.borrow_mut().clear());
    ICON_FILE_CACHE.with(|c| c.borrow_mut().clear());
}

pub(crate) fn set_highlight_accent(hex: String) {
    HIGHLIGHT_ACCENT.with(|a| *a.borrow_mut() = hex);
}

/// Wrap the char positions in `matched` with accent-colored Pango spans.
///
/// `matched` holds char indices into `title`, produced engine-side. Each
/// segment is escaped *after* slicing — escaping earlier would shift byte
/// offsets. Consecutive indices merge into one span; out-of-range indices
/// are ignored.
pub(crate) fn highlight_markup(title: &str, matched: &[usize]) -> String {
    fn flush(
        out: &mut String,
        title: &str,
        plain_start: usize,
        s: usize,
        e: usize,
        accent: &str,
    ) -> usize {
        out.push_str(&glib::markup_escape_text(&title[plain_start..s]));
        out.push_str("<span foreground=\"");
        out.push_str(accent);
        out.push_str("\">");
        out.push_str(&glib::markup_escape_text(&title[s..e]));
        out.push_str("</span>");
        e
    }

    let accent = HIGHLIGHT_ACCENT.with(|a| a.borrow().clone());
    let mut idxs = matched.to_vec();
    idxs.sort_unstable();
    idxs.dedup();

    let mut out = String::with_capacity(title.len() + idxs.len() * 31);
    let mut plain_start: usize = 0; // byte offset where the current plain run began
    let mut run: Option<(usize, usize)> = None; // byte range of the current matched run
    let mut next = 0usize; // position in idxs

    // Linear in title length: `enumerate` index IS the char index, avoiding
    // the O(n²) `title[..byte].chars().count()` rescan per char (audit P3).
    for (pos, (byte, ch)) in title.char_indices().enumerate() {
        if next < idxs.len() && idxs[next] == pos {
            next += 1;
            run = Some(match run.take() {
                Some((s, _)) => (s, byte + ch.len_utf8()),
                None => (byte, byte + ch.len_utf8()),
            });
        } else if let Some((s, e)) = run.take() {
            plain_start = flush(&mut out, title, plain_start, s, e, &accent);
        }
    }
    if let Some((s, e)) = run {
        plain_start = flush(&mut out, title, plain_start, s, e, &accent);
    }
    out.push_str(&glib::markup_escape_text(&title[plain_start..]));
    out
}

pub(crate) struct ResultRowPool {
    slots: Vec<PooledRow>,
    /// How many slots are currently attached to the list.
    attached: usize,
}

struct PooledRow {
    row: ListBoxRow,
    // standard layout (owned widgets; reparented via set_child)
    std_root: GtkBox,
    icon: Image,
    title: Label,
    subtitle: Label,
    badge: Label,
    drag: PathDragBinding,
    // conversion layout
    conv_root: GtkBox,
    conv_header: Label,
    conv_left_title: Label,
    conv_left_badge: Label,
    // Right panel crossfader: two identical panels ("a"/"b"), the invisible
    // one is primed with the incoming value, then made visible to fade.
    conv_right: Stack,
    conv_rt_a: Label,
    conv_rb_a: Label,
    conv_rt_b: Label,
    conv_rb_b: Label,
    /// Pending swap-class cleanup timer (cancelled on re-arm so an older
    /// timer cannot truncate a newer animation).
    swap_timer: std::rc::Rc<RefCell<Option<glib::SourceId>>>,
    badge_kind: ResultKind,
    showing_conv: bool,
    /// Display content of the last `bind` (audit P3 Pass 17): rebinds with
    /// identical content (cache re-render, repeated applies) skip all GTK
    /// writes. Wheel swaps rotate items so every slot differs — those still
    /// rebind fully, as the cyclic order design requires.
    bound_sig: Option<BoundSig>,
}

/// Every `bind` input that affects displayed output. Compared by value
/// without allocating; stored (cloned) only when actually rebinding.
#[derive(PartialEq, Eq)]
struct BoundSig {
    id: String,
    title: String,
    subtitle: String,
    icon: Option<String>,
    matched: Option<Vec<usize>>,
    kind: ResultKind,
    as_card: bool,
    icon_size: i32,
    symbolic: bool,
    conv: Option<(String, String, String, String)>,
    drag: Option<PathBuf>,
}

impl BoundSig {
    fn matches(&self, item: &SearchResult, as_card: bool, icon_size: i32, symbolic: bool) -> bool {
        self.as_card == as_card
            && self.icon_size == icon_size
            && self.symbolic == symbolic
            && self.kind == item.kind
            && self.id == item.id
            && self.title == item.title
            && self.subtitle == item.subtitle
            && self.icon == item.icon
            && self.matched == item.matched
            && self.drag.as_deref() == item.action.drag_path()
            && self.conv.as_ref().map(|c| (&c.0, &c.1, &c.2, &c.3))
                == item
                    .conversion
                    .as_ref()
                    .map(|c| (&c.left_title, &c.left_badge, &c.right_title, &c.right_badge))
    }

    fn capture(item: &SearchResult, as_card: bool, icon_size: i32, symbolic: bool) -> Self {
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            subtitle: item.subtitle.clone(),
            icon: item.icon.clone(),
            matched: item.matched.clone(),
            kind: item.kind,
            as_card,
            icon_size,
            symbolic,
            conv: item.conversion.as_ref().map(|c| {
                (
                    c.left_title.clone(),
                    c.left_badge.clone(),
                    c.right_title.clone(),
                    c.right_badge.clone(),
                )
            }),
            drag: item.action.drag_path().map(|p| p.to_path_buf()),
        }
    }
}

/// Direction-aware hero animation. `None` = instant (typing), `SlideUp`
/// = ↓ arrow, `SlideDown` = ↑ arrow, `Crossfade` = mouse click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeroAnim {
    SlideUp,
    SlideDown,
    Crossfade,
}

impl HeroAnim {
    pub(crate) fn from_dir(dir: i32) -> Self {
        match dir {
            d if d > 0 => Self::SlideUp,
            d if d < 0 => Self::SlideDown,
            _ => Self::Crossfade,
        }
    }
}

impl ResultRowPool {
    pub fn new(drag_session: &DragSession) -> Self {
        let mut slots = Vec::with_capacity(ROW_POOL_CAP);
        for _ in 0..ROW_POOL_CAP {
            slots.push(PooledRow::new(drag_session));
        }
        Self { slots, attached: 0 }
    }

    /// Bind `items` to the pool. Row 0 of a conversion prediction set renders
    /// as the fixed hero card; every other row is a standard row. When
    /// `hero_anim` is `Some` the card's value animates (picker wheel) with
    /// direction-aware slide; fresh query binds stay instant so typing never
    /// flickers.
    pub fn apply(
        &mut self,
        list: &ListBox,
        items: &[SearchResult],
        icon_size: i32,
        symbolic_icons: bool,
        hero_anim: Option<HeroAnim>,
    ) {
        let n = items.len().min(ROW_POOL_CAP);

        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            let as_card = i == 0 && items[i].conversion.is_some();
            self.slots[i].bind(
                &items[i],
                as_card,
                if as_card { hero_anim } else { None },
                icon_size,
                symbolic_icons,
            );
        }

        while self.attached < n {
            list.append(&self.slots[self.attached].row);
            self.attached += 1;
        }

        while self.attached > n {
            self.attached -= 1;
            let row = &self.slots[self.attached].row;
            list.remove(row);
            self.slots[self.attached].drag.set_path(None);
        }
    }

    pub fn clear(&mut self, list: &ListBox) {
        while self.attached > 0 {
            self.attached -= 1;
            let row = &self.slots[self.attached].row;
            list.remove(row);
            self.slots[self.attached].drag.set_path(None);
        }
    }

    pub fn row_at(&self, index: usize) -> Option<&ListBoxRow> {
        if index < self.attached {
            Some(&self.slots[index].row)
        } else {
            None
        }
    }
}

impl PooledRow {
    fn new(drag_session: &DragSession) -> Self {
        let row = ListBoxRow::new();
        row.set_activatable(true);
        // Don't let the row expand to fill leftover list height.
        row.set_vexpand(false);
        row.set_hexpand(true);

        // ── standard row ──────────────────────────────────────────────
        let std_root = GtkBox::new(Orientation::Horizontal, 10);
        std_root.add_css_class("hark-row-inner");
        std_root.set_margin_start(2);
        std_root.set_margin_end(2);
        std_root.set_vexpand(false);

        let icon = Image::from_icon_name("text-x-generic");
        icon.add_css_class("hark-row-icon");
        icon.set_pixel_size(26);
        icon.set_valign(gtk::Align::Center);

        let text = GtkBox::new(Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);

        let title = Label::new(None);
        title.add_css_class("hark-title");
        title.set_halign(gtk::Align::Start);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_xalign(0.0);
        title.set_wrap(false);
        title.set_single_line_mode(true);

        let subtitle = Label::new(None);
        subtitle.add_css_class("hark-subtitle");
        subtitle.set_halign(gtk::Align::Start);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        subtitle.set_xalign(0.0);
        subtitle.set_wrap(false);
        subtitle.set_single_line_mode(true);

        text.append(&title);
        text.append(&subtitle);

        let badge = Label::new(None);
        badge.add_css_class("hark-badge");
        badge.set_valign(gtk::Align::Center);

        std_root.append(&icon);
        std_root.append(&text);
        std_root.append(&badge);

        // ── conversion card ───────────────────────────────────────────
        let conv_root = GtkBox::new(Orientation::Vertical, 6);
        conv_root.add_css_class("hark-conv-card");
        conv_root.set_hexpand(true);
        conv_root.set_vexpand(false);

        let conv_header = Label::new(None);
        conv_header.add_css_class("hark-conv-header");
        conv_header.set_halign(gtk::Align::Start);
        conv_root.append(&conv_header);

        let panels = GtkBox::new(Orientation::Horizontal, 0);
        panels.add_css_class("hark-conv-panels");
        panels.set_hexpand(true);

        let (left, conv_left_title, conv_left_badge) = conv_panel_widgets(true);
        let arrow = Label::new(Some("→"));
        arrow.add_css_class("hark-conv-arrow");
        arrow.set_valign(gtk::Align::Center);
        arrow.set_halign(gtk::Align::Center);

        // Right side: two identical panels in a Stack so wheeling a new value
        // into the card crossfades value + unit together.
        let conv_right = Stack::new();
        conv_right.set_transition_type(gtk::StackTransitionType::Crossfade);
        conv_right.set_transition_duration(220);
        conv_right.set_hexpand(true);
        conv_right.set_halign(gtk::Align::Fill);
        let (right_a, conv_rt_a, conv_rb_a) = conv_panel_widgets(false);
        let (right_b, conv_rt_b, conv_rb_b) = conv_panel_widgets(false);
        conv_right.add_named(&right_a, Some("a"));
        conv_right.add_named(&right_b, Some("b"));
        conv_right.set_visible_child_name("a");

        panels.append(&left);
        panels.append(&arrow);
        panels.append(&conv_right);
        conv_root.append(&panels);

        // Default child: standard layout.
        row.set_child(Some(&std_root));

        let drag = PathDragBinding::new(drag_session.clone());
        drag.attach(&row);

        Self {
            row,
            std_root,
            icon,
            title,
            subtitle,
            badge,
            drag,
            conv_root,
            conv_header,
            conv_left_title,
            conv_left_badge,
            conv_right,
            conv_rt_a,
            conv_rb_a,
            conv_rt_b,
            conv_rb_b,
            swap_timer: std::rc::Rc::new(RefCell::new(None)),
            badge_kind: ResultKind::File,
            showing_conv: false,
            bound_sig: None,
        }
    }

    fn set_mode_std(&mut self) {
        if self.showing_conv {
            self.row.set_child(Some(&self.std_root));
            self.showing_conv = false;
        }
        self.row.remove_css_class("hark-conv-row");
    }

    fn set_mode_conv(&mut self) {
        if !self.showing_conv {
            self.row.set_child(Some(&self.conv_root));
            self.showing_conv = true;
        }
        self.row.add_css_class("hark-conv-row");
    }

    /// Row 0 of a conversion prediction set renders as the fixed hero card;
    /// sibling predictions render as standard rows so the group reads as one
    /// card plus plain rows, with values wheeling through the card.
    fn bind(
        &mut self,
        item: &SearchResult,
        as_card: bool,
        hero_anim: Option<HeroAnim>,
        icon_size: i32,
        symbolic_icons: bool,
    ) {
        // Same content as last bind and no animation requested: skip every
        // GTK write (markup rebuild, icon resolve, label writes). Allocation-
        // free check; the stored sig clones only on real rebinds below.
        if hero_anim.is_none()
            && self
                .bound_sig
                .as_ref()
                .is_some_and(|s| s.matches(item, as_card, icon_size, symbolic_icons))
        {
            return;
        }
        if as_card {
            if let Some(conv) = &item.conversion {
                self.set_mode_conv();
                self.conv_header.set_text(kind_label(item.kind));
                self.conv_left_title.set_text(&conv.left_title);
                self.conv_left_badge.set_text(&conv.left_badge);
                self.set_conv_right(conv, hero_anim);
                self.drag.set_path(None);
                self.bound_sig = Some(BoundSig::capture(item, as_card, icon_size, symbolic_icons));
                return;
            }
        }

        self.set_mode_std();

        apply_result_icon(
            &self.icon,
            item.icon.as_deref(),
            item.kind,
            symbolic_icons,
            icon_size,
        );

        match item.matched.as_deref().filter(|m| !m.is_empty()) {
            Some(matched) => self
                .title
                .set_markup(&highlight_markup(&item.title, matched)),
            None => self.title.set_text(&item.title),
        }
        self.subtitle.set_text(&item.subtitle);

        self.badge.set_text(kind_label(item.kind));
        if self.badge_kind != item.kind {
            remove_badge_kind_class(&self.badge, self.badge_kind);
            add_badge_kind_class(&self.badge, item.kind);
            self.badge_kind = item.kind;
        }

        if let Some(path) = item.action.drag_path() {
            self.drag.set_path(Some(path.to_path_buf()));
        } else {
            self.drag.set_path(None);
        }
        self.bound_sig = Some(BoundSig::capture(item, as_card, icon_size, symbolic_icons));
    }

    /// Point the card's right panel at `conv`. Animated swaps prime the
    /// invisible Stack side and flip to it; instant binds write the visible
    /// side in place so query refreshes don't shimmer.
    ///
    /// Direction-aware: ↓ → SlideUp, ↑ → SlideDown, mouse → Crossfade.
    fn set_conv_right(&self, conv: &ConversionView, hero_anim: Option<HeroAnim>) {
        let cur = if self.conv_right.visible_child_name().as_deref() == Some("b") {
            "b"
        } else {
            "a"
        };
        let (target_side, should_flip) = match hero_anim {
            None => (cur, false),
            Some(_) => (if cur == "a" { "b" } else { "a" }, true),
        };
        // Configure transition before flipping.
        if let Some(anim) = hero_anim {
            let (ty, dur) = match anim {
                HeroAnim::SlideUp => (gtk::StackTransitionType::SlideUp, 180),
                HeroAnim::SlideDown => (gtk::StackTransitionType::SlideDown, 180),
                HeroAnim::Crossfade => (gtk::StackTransitionType::Crossfade, 160),
            };
            self.conv_right.set_transition_type(ty);
            self.conv_right.set_transition_duration(dur as u32);
        }
        let (t, b) = if target_side == "b" {
            (&self.conv_rt_b, &self.conv_rb_b)
        } else {
            (&self.conv_rt_a, &self.conv_rb_a)
        };
        t.set_text(&conv.right_title);
        b.set_text(&conv.right_badge);
        if should_flip && target_side != cur {
            self.conv_right.set_visible_child_name(target_side);
        }
        // Bump the whole card for a subtle `pop` — makes instant-adjacent
        // swaps read as intentional, not a glitch. GTK CSS handles the
        // keyframe; we just toggle the class. Remove-then-add forces the
        // keyframe to retrigger on rapid repeated presses.
        if hero_anim.is_some() {
            self.conv_root.remove_css_class("hark-conv-swap");
            self.conv_root.add_css_class("hark-conv-swap");
            // Cancel the previous cleanup so rapid swaps don't let an older
            // timer strip the class mid-flight of the newer keyframe.
            if let Some(id) = self.swap_timer.borrow_mut().take() {
                id.remove();
            }
            let root = self.conv_root.clone();
            let slot = self.swap_timer.clone();
            *self.swap_timer.borrow_mut() = Some(glib::timeout_add_local_once(
                std::time::Duration::from_millis(220),
                move || {
                    root.remove_css_class("hark-conv-swap");
                    *slot.borrow_mut() = None;
                },
            ));
        }
    }
}

fn conv_panel_widgets(is_left: bool) -> (GtkBox, Label, Label) {
    let col = GtkBox::new(Orientation::Vertical, 8);
    col.add_css_class("hark-conv-panel");
    if is_left {
        col.add_css_class("hark-conv-left");
    } else {
        col.add_css_class("hark-conv-right");
    }
    col.set_hexpand(true);
    col.set_halign(gtk::Align::Fill);

    let t = Label::new(None);
    t.add_css_class("hark-conv-title");
    t.set_halign(gtk::Align::Start);
    t.set_xalign(0.0);
    if is_left {
        // Long expressions wrap to at most two lines, then ellipsize; the
        // result side stays single-line so the arrow stays visually centered.
        t.set_wrap(true);
        t.set_lines(2);
        t.set_ellipsize(gtk::pango::EllipsizeMode::End);
        t.set_valign(gtk::Align::Start);
    } else {
        t.set_wrap(false);
        t.set_ellipsize(gtk::pango::EllipsizeMode::End);
        t.set_valign(gtk::Align::Center);
    }

    let b = Label::new(None);
    b.add_css_class("hark-conv-badge");
    b.set_halign(gtk::Align::Start);

    col.append(&t);
    col.append(&b);
    (col, t, b)
}

fn add_badge_kind_class(badge: &Label, kind: ResultKind) {
    match kind {
        ResultKind::Calc | ResultKind::Conversion => badge.add_css_class("calc"),
        ResultKind::File => badge.add_css_class("file"),
        ResultKind::Folder => badge.add_css_class("folder"),
        ResultKind::App | ResultKind::Command => {}
    }
}

fn remove_badge_kind_class(badge: &Label, kind: ResultKind) {
    match kind {
        ResultKind::Calc | ResultKind::Conversion => badge.remove_css_class("calc"),
        ResultKind::File => badge.remove_css_class("file"),
        ResultKind::Folder => badge.remove_css_class("folder"),
        ResultKind::App | ResultKind::Command => {}
    }
}

/// Paint a themed icon name **or** an absolute icon file path onto `image`.
///
/// Manual / AppImage installs often ship `Icon=/path/to/app.png` in their
/// `.desktop` file. Theme lookup alone would miss those and show the generic
/// executable icon.
pub(crate) fn apply_result_icon(
    image: &Image,
    requested: Option<&str>,
    kind: ResultKind,
    symbolic_icons: bool,
    icon_size: i32,
) {
    let size = icon_size.clamp(18, 36);
    image.set_pixel_size(size);

    let requested = requested
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_icon_for_kind(kind));

    if let Some(path) = icon_file_path(requested) {
        // FileIcon + pixel-size keeps HiDPI scaling consistent with themed icons.
        let gicon = gio::FileIcon::new(&gio::File::for_path(&path));
        image.set_from_gicon(&gicon);
        return;
    }

    // Path-looking Icon= that is missing on disk → generic fallback, not theme soup.
    if looks_like_icon_path(requested) {
        image.set_icon_name(Some(fallback_icon(kind, requested)));
        return;
    }

    let resolved = resolve_row_icon(requested, kind, symbolic_icons);
    image.set_icon_name(Some(resolved.as_str()));
}

fn looks_like_icon_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("~/") || s.starts_with("file:")
}

/// Resolve `Icon=` to an on-disk image path, if it points at a real file.
///
/// Cached: the caller runs per row per keystroke on the main thread, so the
/// `is_file()` stat is memoized (FIFO 512). Non-path names return `None`
/// without touching the FS.
fn icon_file_path(s: &str) -> Option<PathBuf> {
    // Fast path: cached hit (positive or negative).
    if let Some(hit) = ICON_FILE_CACHE.with(|c| c.borrow().map.get(s).cloned()) {
        return hit;
    }
    let resolved = icon_file_path_uncached(s);
    // Present keys always hit the fast path above, so this is always an
    // insert (no refresh branch needed).
    ICON_FILE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        while cache.map.len() >= ICON_CACHE_CAP {
            if let Some(old) = cache.order.pop_front() {
                cache.map.remove(&old);
            } else {
                break;
            }
        }
        cache.order.push_back(s.to_string());
        cache.map.insert(s.to_string(), resolved.clone());
    });
    resolved
}

fn icon_file_path_uncached(s: &str) -> Option<PathBuf> {
    let raw = s.strip_prefix("file://").unwrap_or(s);
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else if raw.starts_with('/') {
        PathBuf::from(raw)
    } else {
        // Relative paths with a separator + image extension (rare, but valid).
        let p = Path::new(raw);
        if p.components().count() > 1 && is_image_path(p) {
            p.to_path_buf()
        } else {
            return None;
        }
    };
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "svg" | "xpm" | "jpg" | "jpeg" | "webp" | "gif" | "ico")
    )
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

fn resolve_row_icon(requested: &str, kind: ResultKind, symbolic_icons: bool) -> String {
    let key = format!(
        "{}\0{}\0{}",
        symbolic_icons as u8,
        requested,
        kind_key(kind)
    );
    ICON_RESOLVE_CACHE.with(|cache| {
        if let Some(hit) = cache.borrow().get(&key) {
            return hit;
        }
        let resolved = resolve_row_icon_uncached(requested, kind, symbolic_icons);
        cache.borrow_mut().insert(key, resolved.clone());
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

fn kind_label(kind: ResultKind) -> &'static str {
    match kind {
        ResultKind::App => "App",
        ResultKind::File => "File",
        ResultKind::Folder => "Folder",
        ResultKind::Calc => "Calc",
        ResultKind::Conversion => "Convert",
        ResultKind::Command => "Command",
    }
}

/// First candidate the active display's icon theme can resolve, else the last
/// (widest-coverage) name. Headless (tests) falls back the same way.
pub(crate) fn resolve_icon_name(candidates: &[&'static str]) -> &'static str {
    let fallback = candidates.last().copied().unwrap_or("text-x-generic");
    let Some(display) = gtk::gdk::Display::default() else {
        return fallback;
    };
    let theme = gtk::IconTheme::for_display(&display);
    candidates
        .iter()
        .find(|name| theme.has_icon(name))
        .copied()
        .unwrap_or(fallback)
}

#[cfg(test)]
mod highlight_tests {
    use super::{highlight_markup, set_highlight_accent};

    fn accent() -> &'static str {
        "#7aa2f7"
    }

    fn sp(s: &str) -> String {
        format!("<span foreground=\"{}\">{}</span>", accent(), s)
    }

    #[test]
    fn consecutive_run_merges_into_one_span() {
        set_highlight_accent(accent().into());
        let m = highlight_markup("Alacritty", &[0, 1, 2]);
        assert_eq!(m, format!("{}critty", sp("Ala")));
    }

    #[test]
    fn non_contiguous_runs_stay_separate() {
        set_highlight_accent(accent().into());
        // a…c…t of Alacritty → A=0, c=3, t=6
        let m = highlight_markup("Alacritty", &[0, 3, 6]);
        assert_eq!(m, format!("{}la{}ri{}ty", sp("A"), sp("c"), sp("t")));
    }

    #[test]
    fn markup_metacharacters_are_escaped_per_segment() {
        set_highlight_accent(accent().into());
        let m = highlight_markup("a<b & c>d", &[0, 1]);
        // chars 0,1 are `a`, `<`
        assert_eq!(m, format!("{}b &amp; c&gt;d", sp("a&lt;")));
    }

    #[test]
    fn multibyte_titles_map_char_indices_to_byte_spans() {
        set_highlight_accent(accent().into());
        // "日本語ランチャー": chars 0..3 are 日本語 (3 bytes each)
        let m = highlight_markup("日本語ランチャー", &[0, 1, 2]);
        assert_eq!(m, format!("{}ランチャー", sp("日本語")));
        // Run starting after multibyte prefix: "é À ünïcode" chars 2,3 = 'À',' '.
        let m = highlight_markup("é À ünïcode", &[2, 3]);
        assert_eq!(m, format!("é {}ünïcode", sp("À ")));
    }

    #[test]
    fn out_of_range_and_duplicate_indices_are_ignored() {
        set_highlight_accent(accent().into());
        let m = highlight_markup("Zed", &[1, 1, 7]);
        assert_eq!(m, format!("Z{}d", sp("e")));
    }

    #[test]
    fn empty_match_renders_escaped_plain_text() {
        set_highlight_accent(accent().into());
        assert_eq!(highlight_markup("5 < 6", &[]), "5 &lt; 6");
    }

    #[test]
    fn empty_candidates_fall_back_without_panic() {
        // Audit P3: empty slice must not index-panic (latent main-loop crash).
        assert_eq!(super::resolve_icon_name(&[]), "text-x-generic");
    }

    #[test]
    fn long_cjk_title_maps_tail_indices_linearly() {
        // Audit P3: enumerate index must equal char index even past multibyte
        // prefix (guards the O(n²)→O(n) rewrite against off-by-one).
        set_highlight_accent(accent().into());
        let title = "日本語".repeat(100) + "target";
        let base: usize = 300; // 3*100 CJK chars before "target"
        let matched: Vec<usize> = (base..base + 6).collect();
        let m = highlight_markup(&title, &matched);
        assert!(m.contains(&sp("target")), "tail run highlighted, got {m}");
        // Prefix untouched (no span bleed into CJK run).
        let prefix = glib::markup_escape_text(&"日本語".repeat(100));
        assert!(m.starts_with(prefix.as_str()));
    }

    #[test]
    fn icon_file_path_caches_negative_result() {
        // Audit P3: a cached miss must not re-stat — recreate the file
        // after the miss and the cached None must still win until clear.
        use super::{clear_icon_resolve_cache, icon_file_path};
        use std::io::Write as _;
        clear_icon_resolve_cache();
        let dir = std::env::temp_dir().join("hark-icon-neg-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("neg-icon.png");
        let _ = std::fs::remove_file(&file);
        let key = file.to_string_lossy().into_owned();
        assert_eq!(icon_file_path(&key), None);
        let _ = std::fs::File::create(&file).and_then(|mut f| f.write_all(b"png"));
        // Still None: served from the negative cache, no re-stat.
        assert_eq!(icon_file_path(&key), None);
        clear_icon_resolve_cache();
        // After clear, re-stats and sees the recreated file.
        assert!(icon_file_path(&key).is_some());
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn icon_file_path_caches_positive_result() {
        // Positive hit stays cached even if the file vanishes (documented
        // staleness tradeoff; evicted FIFO / cleared on theme change).
        use super::{clear_icon_resolve_cache, icon_file_path};
        use std::io::Write as _;
        clear_icon_resolve_cache();
        let dir = std::env::temp_dir().join("hark-icon-cache-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test-icon.png");
        let _ = std::fs::File::create(&file).and_then(|mut f| f.write_all(b"png"));
        let key = file.to_string_lossy().into_owned();
        assert!(icon_file_path(&key).is_some());
        let _ = std::fs::remove_file(&file);
        // Still Some: served from cache, no re-stat.
        assert!(icon_file_path(&key).is_some());
        clear_icon_resolve_cache();
        // After clear, re-stats and now reports missing.
        assert_eq!(icon_file_path(&key), None);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn non_path_icon_names_skip_fs_without_caching_fs_state() {
        use super::{clear_icon_resolve_cache, icon_file_path};
        clear_icon_resolve_cache();
        assert_eq!(icon_file_path("firefox"), None);
        assert_eq!(icon_file_path(""), None);
    }

    #[test]
    fn bound_sig_matches_identical_content() {
        // Audit P3 (Pass 17): skip-key must accept byte-identical rebinds.
        use super::BoundSig;
        use crate::providers::{Action, ResultKind, SearchResult};
        let item = SearchResult {
            id: "app:firefox.desktop".into(),
            title: "Firefox".into(),
            subtitle: "Web Browser".into(),
            kind: ResultKind::App,
            score: 30_000,
            icon: Some("firefox".into()),
            action: Action::OpenPath("/usr/bin/firefox".into()),
            conversion: None,
            matched: Some(vec![0, 1, 2]),
        };
        let sig = BoundSig::capture(&item, false, 26, false);
        assert!(sig.matches(&item, false, 26, false));
        // Score is not displayed — excluded from the key by design.
        let mut rescored = item.clone();
        rescored.score += 500;
        assert!(sig.matches(&rescored, false, 26, false));
    }

    #[test]
    fn bound_sig_rejects_any_display_change() {
        use super::BoundSig;
        use crate::providers::{Action, ResultKind, SearchResult};
        let base = SearchResult {
            id: "app:firefox.desktop".into(),
            title: "Firefox".into(),
            subtitle: "Web Browser".into(),
            kind: ResultKind::App,
            score: 30_000,
            icon: Some("firefox".into()),
            action: Action::OpenPath("/usr/bin/firefox".into()),
            conversion: None,
            matched: Some(vec![0, 1, 2]),
        };
        let sig = BoundSig::capture(&base, false, 26, false);
        let mut v = base.clone();
        v.title = "Firefox ESR".into();
        assert!(!sig.matches(&v, false, 26, false));
        v = base.clone();
        v.matched = Some(vec![0, 1, 2, 3]);
        assert!(!sig.matches(&v, false, 26, false));
        v = base.clone();
        v.subtitle = "Browser".into();
        assert!(!sig.matches(&v, false, 26, false));
        v = base.clone();
        v.icon = None;
        assert!(!sig.matches(&v, false, 26, false));
        v = base.clone();
        v.kind = ResultKind::File;
        assert!(!sig.matches(&v, false, 26, false));
        v = base.clone();
        v.action = Action::OpenPath("/opt/firefox/firefox".into());
        assert!(!sig.matches(&v, false, 26, false));
        assert!(!sig.matches(&base, true, 26, false));
        assert!(!sig.matches(&base, false, 32, false));
        assert!(!sig.matches(&base, false, 26, true));
    }
}
