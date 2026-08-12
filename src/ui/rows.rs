//! Result list rows + a fixed pool so searches rebind widgets instead of
//! allocating new trees every keystroke.
//!
//! Unused slots are **removed** from the ListBox (not merely hidden): GTK4
//! ListBox still reserves height for invisible children.
//!
//! Each slot keeps both a standard row widget and a conversion card; we
//! `set_child` the active one. A Stack is avoided because both children can
//! inflate the row's natural height.

use super::dnd::{DragSession, PathDragBinding};
use crate::providers::{ResultKind, SearchResult};
use gtk::prelude::*;
use gtk::{Box as GtkBox, Image, Label, ListBox, ListBoxRow, Orientation};
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

thread_local! {
    static ICON_RESOLVE_CACHE: RefCell<IconResolveCache> = RefCell::new(IconResolveCache::new());
}

pub(crate) fn clear_icon_resolve_cache() {
    ICON_RESOLVE_CACHE.with(|c| c.borrow_mut().clear());
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
    conv_right_title: Label,
    conv_right_badge: Label,
    badge_kind: ResultKind,
    showing_conv: bool,
}

impl ResultRowPool {
    pub fn new(drag_session: &DragSession) -> Self {
        let mut slots = Vec::with_capacity(ROW_POOL_CAP);
        for _ in 0..ROW_POOL_CAP {
            slots.push(PooledRow::new(drag_session));
        }
        Self { slots, attached: 0 }
    }

    pub fn apply(
        &mut self,
        list: &ListBox,
        items: &[SearchResult],
        icon_size: i32,
        symbolic_icons: bool,
    ) {
        let n = items.len().min(ROW_POOL_CAP);

        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            self.slots[i].bind(&items[i], icon_size, symbolic_icons);
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
        let (right, conv_right_title, conv_right_badge) = conv_panel_widgets(false);

        panels.append(&left);
        panels.append(&arrow);
        panels.append(&right);
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
            conv_right_title,
            conv_right_badge,
            badge_kind: ResultKind::File,
            showing_conv: false,
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

    fn bind(&mut self, item: &SearchResult, icon_size: i32, symbolic_icons: bool) {
        if let Some(conv) = &item.conversion {
            self.set_mode_conv();
            self.conv_header.set_text(kind_label(item.kind));
            self.conv_left_title.set_text(&conv.left_title);
            self.conv_left_badge.set_text(&conv.left_badge);
            self.conv_right_title.set_text(&conv.right_title);
            self.conv_right_badge.set_text(&conv.right_badge);
            self.drag.set_path(None);
            return;
        }

        self.set_mode_std();

        apply_result_icon(
            &self.icon,
            item.icon.as_deref(),
            item.kind,
            symbolic_icons,
            icon_size,
        );

        self.title.set_text(&item.title);
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
    t.set_wrap(true);
    t.set_xalign(0.0);

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
fn icon_file_path(s: &str) -> Option<PathBuf> {
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
