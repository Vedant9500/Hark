use super::dnd::{clear_drag_thumbnail_memo, DragSession, PathDragBinding};
use super::thumbnails::{freedesktop_thumbnail, store_freedesktop_thumbnail};
use crate::providers::{Action, ResultKind, SearchResult};
use gtk::gdk::{self, Texture};
use gtk::gdk_pixbuf::Pixbuf;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, ContentFit, Image, Label, Orientation, Picture, ScrolledWindow,
    Separator, Stack,
};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::Accessor;
use sourceview5::prelude::*;
use sourceview5::{LanguageManager, StyleSchemeManager, View};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

/// Max image file size we'll try to decode (bytes).
const MAX_IMAGE_BYTES: u64 = 40 * 1024 * 1024;
/// Max source-file size we'll show in the code preview (bytes).
const MAX_CODE_BYTES: u64 = 2 * 1024 * 1024;
/// Upstream GtkSourceView disables highlighting past 2000 chars in one line
/// (`LINE_MAX_SUPPORTED_CHARS`, 2019-04-19) after a 2 s stall. Pre-empt it:
/// measure in the worker, skip highlight + truncate display so the main
/// thread never layouts a ~2M-char minified line (audit P2 Pass 16, CWE-400).
const MAX_CODE_LINE_CHARS: usize = 2000;
/// Past this many lines, highlighting is disabled (audit suggested 20k;
/// 5k keeps `set_text` + re-highlight bounded for a 380px panel).
const MAX_CODE_LINES: usize = 5000;
/// Display cap: first N lines shown, each truncated to `MAX_CODE_LINE_CHARS`.
const DISPLAY_MAX_LINES: usize = 500;
/// Preview panel width.
pub const PREVIEW_WIDTH: i32 = 280;
/// Image frame inside the panel (4:3).
const IMAGE_FRAME_WIDTH: i32 = PREVIEW_WIDTH - 32;
const IMAGE_FRAME_HEIGHT: i32 = IMAGE_FRAME_WIDTH * 3 / 4; // 4:3
/// Decode target (≈2× frame for HiDPI). Keeps textures small.
const DECODE_MAX_PX: i32 = IMAGE_FRAME_WIDTH * 2;
/// How many decoded textures to keep in RAM.
const TEXTURE_CACHE_CAP: usize = 24;
/// Skip decode work while the user is still arrowing through results.
const LOAD_DEBOUNCE: Duration = Duration::from_millis(45);
/// Hard deadline for ffmpeg/pdftoppm so a hung encoder can't wedge the single
/// preview worker (busy flag would otherwise never clear).
const CONVERTER_TIMEOUT: Duration = Duration::from_secs(10);

/// Filesystem identity so in-place edits invalidate the texture cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileFp {
    len: u64,
    /// mtime as unix nanos; 0 if unknown.
    mtime_ns: u128,
}

impl FileFp {
    fn from_meta(meta: &std::fs::Metadata) -> Self {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self {
            len: meta.len(),
            mtime_ns,
        }
    }
}

#[derive(Clone)]
struct CachedTexture {
    texture: Texture,
    dims_label: String,
    fp: FileFp,
}

/// Latest-wins decode request (only one worker runs at a time).
#[derive(Clone)]
struct DecodeRequest {
    path: PathBuf,
    gen: u64,
    fp: FileFp,
}

/// Worker-measured code preview: display text is already truncated so the
/// main thread never calls `set_text` with a ~2M-char minified line.
struct CodePreview {
    display: String,
    total_lines: usize,
    /// Capped at `MAX_CODE_LINE_CHARS + 1` (`MAX+1` means "over threshold").
    max_line_chars: usize,
    truncated: bool,
    highlight_off: bool,
}

/// Measure + truncate off-main-thread. Per-line char counting is capped at
/// `MAX+1` iterations so a 2 MiB single line costs ~2k char steps, not ~2M.
/// Short lines (`byte len <= MAX`) skip counting (char len can't exceed it).
fn prepare_code_preview(text: &str) -> CodePreview {
    let mut total_lines = 0usize;
    let mut max_line_chars = 0usize;
    let mut display = String::with_capacity(text.len().min(64 * 1024));
    let mut display_lines = 0usize;
    for line in text.lines() {
        total_lines += 1;
        let over_bytes = line.len() > MAX_CODE_LINE_CHARS;
        let char_len = if over_bytes {
            line.chars().take(MAX_CODE_LINE_CHARS + 1).count()
        } else {
            // Byte len bounds char len here; exact count is cheap (<=2000).
            line.chars().count()
        };
        if char_len > max_line_chars {
            max_line_chars = char_len;
            if max_line_chars > MAX_CODE_LINE_CHARS {
                max_line_chars = MAX_CODE_LINE_CHARS + 1;
            }
        }
        if display_lines < DISPLAY_MAX_LINES {
            if display_lines > 0 {
                display.push('\n');
            }
            if char_len > MAX_CODE_LINE_CHARS {
                display.extend(line.chars().take(MAX_CODE_LINE_CHARS));
            } else {
                display.push_str(line);
            }
            display_lines += 1;
        }
    }
    // Empty file: `lines()` yields nothing; keep display empty, not truncated.
    let highlight_off = max_line_chars > MAX_CODE_LINE_CHARS || total_lines > MAX_CODE_LINES;
    let truncated = total_lines > DISPLAY_MAX_LINES || max_line_chars > MAX_CODE_LINE_CHARS;
    CodePreview {
        display,
        total_lines,
        max_line_chars,
        truncated,
        highlight_off,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Code,
    Other,
}

impl MediaKind {
    fn is_previewable(self) -> bool {
        matches!(
            self,
            MediaKind::Image
                | MediaKind::Video
                | MediaKind::Audio
                | MediaKind::Document
                | MediaKind::Code
        )
    }
}

/// Panel visibility callback — fired when the preview shows/hides so the
/// launcher can widen/narrow the window.
type VisibilityCallback = Rc<RefCell<Option<Rc<dyn Fn(bool)>>>>;

pub struct PreviewPanel {
    pub root: GtkBox,
    pub sep: gtk::Separator,
    stack: Stack,
    icon: Image,
    icon_title: Label,
    icon_sub: Label,
    icon_meta: Label,
    icon_type: Label,
    picture: Picture,
    image_title: Label,
    image_meta: Label,
    image_dims: Label,
    code_view: View,
    code_title: Label,
    code_meta: Label,
    code_dims: Label,
    gen: Rc<Cell<u64>>,
    last_path: Rc<RefCell<Option<PathBuf>>>,
    /// Path → decoded preview texture (LRU via `cache_order`).
    cache: Rc<RefCell<HashMap<PathBuf, CachedTexture>>>,
    cache_order: Rc<RefCell<Vec<PathBuf>>>,
    debounce: Rc<RefCell<Option<glib::SourceId>>>,
    /// Pending / in-flight decode — always the latest selection only.
    inflight: Rc<RefCell<Option<DecodeRequest>>>,
    worker_busy: Rc<Cell<bool>>,
    /// Drag source bound to the current preview file path.
    drag: PathDragBinding,
    /// User forced the panel off (Ctrl+P / Toggle Preview) until toggled back.
    user_hidden: Rc<Cell<bool>>,
    /// Notify the launcher when panel visibility changes (window widening).
    on_visibility: VisibilityCallback,
}

impl PreviewPanel {
    pub fn new(drag_session: DragSession, is_light: bool) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("hark-preview");
        root.set_size_request(PREVIEW_WIDTH, 380);
        root.set_width_request(PREVIEW_WIDTH);
        root.set_height_request(380);
        root.set_hexpand(false);
        root.set_vexpand(true);
        root.set_halign(Align::Fill);
        root.set_valign(gtk::Align::Fill);
        // Hidden until a media result is selected.
        root.set_visible(false);

        let sep = gtk::Separator::new(Orientation::Vertical);
        sep.add_css_class("hark-preview-sep");
        sep.set_visible(false);

        let stack = Stack::new();
        stack.add_css_class("hark-preview-stack");
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(90);

        // Generic icon + metadata (video / audio)
        let icon_view = GtkBox::new(Orientation::Vertical, 10);
        icon_view.add_css_class("hark-preview-body");
        icon_view.set_hexpand(true);
        icon_view.set_vexpand(true);
        icon_view.set_halign(Align::Fill);
        icon_view.set_valign(Align::Center);

        let icon = Image::from_icon_name("text-x-generic");
        icon.add_css_class("hark-preview-icon");
        icon.set_pixel_size(72);
        icon.set_halign(Align::Center);

        let icon_type = Label::new(None);
        icon_type.add_css_class("hark-preview-badge");
        icon_type.set_halign(Align::Center);

        let icon_title = Label::new(None);
        icon_title.add_css_class("hark-preview-title");
        icon_title.set_halign(Align::Center);
        icon_title.set_wrap(true);
        icon_title.set_max_width_chars(28);
        icon_title.set_justify(gtk::Justification::Center);
        icon_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        icon_title.set_lines(2);

        let icon_sub = Label::new(None);
        icon_sub.add_css_class("hark-preview-sub");
        icon_sub.set_halign(Align::Center);
        icon_sub.set_wrap(true);
        icon_sub.set_max_width_chars(30);
        icon_sub.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        icon_sub.set_justify(gtk::Justification::Center);

        let icon_meta = Label::new(None);
        icon_meta.add_css_class("hark-preview-meta");
        icon_meta.set_halign(Align::Center);
        icon_meta.set_wrap(true);
        icon_meta.set_justify(gtk::Justification::Center);

        icon_view.append(&icon);
        icon_view.append(&icon_type);
        icon_view.append(&icon_title);
        icon_view.append(&icon_sub);
        icon_view.append(&icon_meta);
        stack.add_named(&icon_view, Some("icon"));

        // Image preview
        let image_view = GtkBox::new(Orientation::Vertical, 8);
        image_view.add_css_class("hark-preview-body");
        image_view.set_hexpand(true);
        image_view.set_vexpand(true);

        let picture = Picture::new();
        picture.add_css_class("hark-preview-picture");
        picture.set_content_fit(ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_vexpand(false);
        picture.set_halign(Align::Fill);
        picture.set_valign(Align::Center);
        // Fixed 4:3 frame so previews stay consistent.
        picture.set_size_request(IMAGE_FRAME_WIDTH, IMAGE_FRAME_HEIGHT);

        let image_title = Label::new(None);
        image_title.add_css_class("hark-preview-title");
        image_title.set_halign(Align::Start);
        image_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        image_title.set_xalign(0.0);

        let image_dims = Label::new(None);
        image_dims.add_css_class("hark-preview-meta");
        image_dims.set_halign(Align::Start);
        image_dims.set_xalign(0.0);

        let image_meta = Label::new(None);
        image_meta.add_css_class("hark-preview-meta");
        image_meta.set_halign(Align::Start);
        image_meta.set_wrap(true);
        image_meta.set_xalign(0.0);

        let meta_block = GtkBox::new(Orientation::Vertical, 2);
        meta_block.add_css_class("hark-preview-meta-block");
        meta_block.append(&image_title);
        meta_block.append(&image_dims);
        meta_block.append(&image_meta);

        image_view.append(&picture);
        image_view.append(&Separator::new(Orientation::Horizontal));
        image_view.append(&meta_block);
        stack.add_named(&image_view, Some("image"));

        // Syntax-highlighted code preview (GtkSourceView 5).
        let code_view = View::new();
        code_view.add_css_class("hark-preview-code");
        code_view.set_show_line_numbers(true);
        code_view.set_editable(false);
        code_view.set_cursor_visible(false);
        code_view.set_monospace(true);
        code_view.set_wrap_mode(gtk::WrapMode::Char);
        code_view.set_hexpand(true);
        code_view.set_vexpand(true);
        if let Ok(buf) = code_view.buffer().downcast::<sourceview5::Buffer>() {
            buf.set_highlight_syntax(true);
            // Theme-aware syntax colors: pick a built-in scheme matching the
            // current theme's lightness so dark themes don't show a white
            // background / dark text from GtkSourceView's default "classic" scheme.
            let mgr = StyleSchemeManager::default();
            let id = if is_light { "Adwaita" } else { "Adwaita-dark" };
            if let Some(scheme) = mgr.scheme(id) {
                buf.set_style_scheme(Some(&scheme));
            }
        }

        let code_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .min_content_height(120)
            .max_content_height(380)
            .hexpand(true)
            .vexpand(true)
            .build();
        code_scroll.add_css_class("hark-preview-code-scroll");
        code_scroll.set_child(Some(&code_view));

        let code_title = Label::new(None);
        code_title.add_css_class("hark-preview-title");
        code_title.set_halign(Align::Start);
        code_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        code_title.set_xalign(0.0);

        let code_dims = Label::new(None);
        code_dims.add_css_class("hark-preview-meta");
        code_dims.set_halign(Align::Start);
        code_dims.set_xalign(0.0);

        let code_meta = Label::new(None);
        code_meta.add_css_class("hark-preview-meta");
        code_meta.set_halign(Align::Start);
        code_meta.set_wrap(true);
        code_meta.set_xalign(0.0);

        let code_meta_block = GtkBox::new(Orientation::Vertical, 2);
        code_meta_block.add_css_class("hark-preview-meta-block");
        code_meta_block.append(&code_title);
        code_meta_block.append(&code_dims);
        code_meta_block.append(&code_meta);

        let code_view_box = GtkBox::new(Orientation::Vertical, 8);
        code_view_box.add_css_class("hark-preview-body");
        code_view_box.set_hexpand(true);
        code_view_box.set_vexpand(true);
        code_view_box.append(&code_scroll);
        code_view_box.append(&Separator::new(Orientation::Horizontal));
        code_view_box.append(&code_meta_block);
        stack.add_named(&code_view_box, Some("code"));

        stack.set_visible_child_name("icon");
        root.append(&stack);

        // Drag the underlying file from the whole preview panel (image or icon view).
        let drag = PathDragBinding::new(drag_session);
        drag.attach(&root);

        // Crash/kill between converter write and cleanup orphans PNGs here
        // (audit P3) — sweep entries older than an hour once per process.
        sweep_converter_scratch();

        Self {
            root,
            sep,
            stack,
            icon,
            icon_title,
            icon_sub,
            icon_meta,
            icon_type,
            picture,
            image_title,
            image_meta,
            image_dims,
            code_view,
            code_title,
            code_meta,
            code_dims,
            gen: Rc::new(Cell::new(0)),
            last_path: Rc::new(RefCell::new(None)),
            cache: Rc::new(RefCell::new(HashMap::new())),
            cache_order: Rc::new(RefCell::new(Vec::new())),
            debounce: Rc::new(RefCell::new(None)),
            inflight: Rc::new(RefCell::new(None)),
            worker_busy: Rc::new(Cell::new(false)),
            drag,
            user_hidden: Rc::new(Cell::new(false)),
            on_visibility: Rc::new(RefCell::new(None)),
        }
    }

    pub fn widget(&self) -> &GtkBox {
        &self.root
    }

    pub fn separator(&self) -> &gtk::Separator {
        &self.sep
    }

    fn set_panel_visible(&self, visible: bool) {
        let vis = visible && !self.user_hidden.get();
        if vis != self.root.is_visible() {
            // Clone the callback out before invoking — calling it with the
            // borrow held would panic if the body re-enters this RefCell.
            if let Some(cb) = self.on_visibility.borrow().clone() {
                cb(vis);
            }
        }
        self.root.set_visible(vis);
        self.sep.set_visible(vis);
    }

    /// Register a callback fired whenever the panel toggles between shown/hidden
    /// (used by the launcher to widen/narrow the window around the preview).
    pub fn set_visibility_cb(&self, cb: impl Fn(bool) + 'static) {
        *self.on_visibility.borrow_mut() = Some(Rc::new(cb));
    }

    /// Flip user hide flag. Returns `true` when the panel is now user-hidden.
    pub fn toggle_user_hidden(&self) -> bool {
        let next = !self.user_hidden.get();
        self.user_hidden.set(next);
        if next {
            self.set_panel_visible(false);
        }
        next
    }

    pub fn clear(&self) {
        self.cancel_debounce();
        self.gen.set(self.gen.get().wrapping_add(1));
        *self.last_path.borrow_mut() = None;
        *self.inflight.borrow_mut() = None;
        self.drag.set_path(None);
        self.picture.set_paintable(Option::<&gdk::Paintable>::None);
        // Drop the sourceview text so up to 2 MiB of previously previewed
        // source doesn't stay resident after hide (audit P3).
        self.code_view.buffer().set_text("");
        // Release the memoized drag texture with the preview so it doesn't
        // pin GPU memory for the process lifetime (audit P3).
        clear_drag_thumbnail_memo();
        self.stack.set_visible_child_name("icon");
        self.set_panel_visible(false);
    }

    pub fn update(self: &Rc<Self>, item: Option<&SearchResult>) {
        let Some(item) = item else {
            self.clear();
            return;
        };

        // Preview is media-only: images get a picture frame; video/audio get icon detail.
        // Apps, folders, docs, calc, etc. never open the panel.
        if !matches!(item.kind, ResultKind::File | ResultKind::Folder) {
            self.clear();
            return;
        }

        let path = match &item.action {
            Action::OpenPath(p) | Action::OpenTerminal(p) => p.clone(),
            _ => {
                self.clear();
                return;
            }
        };

        // #25: stat off the main thread — sync fs::metadata stalls the UI on
        // NFS/FUSE. Bump gen so a stale probe from an older selection is
        // dropped when it comes back.
        self.cancel_debounce();
        let gen = self.gen.get().wrapping_add(1);
        self.gen.set(gen);
        let this = self.clone();
        let item = item.clone();
        let this_path = path.clone();
        let (tx, rx) = async_channel::bounded::<Option<(std::fs::Metadata, PathBuf)>>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(std::fs::metadata(&this_path).ok().map(|m| (m, this_path)));
        });
        glib::spawn_future_local(async move {
            let Some((fs_meta, path)) = rx.recv().await.ok().flatten() else {
                return;
            };
            // Gen check only: last_path is set later by the queue_* paths,
            // so comparing it here would drop every fresh selection.
            if this.gen.get() != gen {
                return; // a newer selection superseded this probe
            }
            this.apply_probed_metadata(&path, &fs_meta, &item);
        });
    }

    /// Post-probe continuation of `update` (#25): everything that needed the
    /// stat result, back on the main thread.
    fn apply_probed_metadata(
        self: &Rc<Self>,
        path: &Path,
        fs_meta: &std::fs::Metadata,
        item: &SearchResult,
    ) {
        if fs_meta.is_dir() {
            self.clear();
            return;
        }

        let media = media_kind(path);
        if !media.is_previewable() {
            self.clear();
            return;
        }

        let fp = FileFp::from_meta(fs_meta);
        let meta = file_meta_from(path, fs_meta);
        self.set_panel_visible(true);
        // Always offer the real file path for DnD from the preview.
        self.drag.set_path(Some(path.to_path_buf()));

        let is_pdf = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false);

        // Images, video first-frame, and PDF page 1 share the picture pipeline.
        if media == MediaKind::Image || media == MediaKind::Video || is_pdf {
            self.queue_image_load(path.to_path_buf(), item.title.clone(), meta, fp);
            return;
        }

        // Code files: syntax-highlighted source preview.
        if media == MediaKind::Code {
            self.queue_code_load(path.to_path_buf(), item.title.clone(), meta, fp);
            return;
        }

        // Audio: ID3/Vorbis/MP4 tags + duration + embedded art.
        if media == MediaKind::Audio {
            self.queue_audio_load(path.to_path_buf(), item.clone(), meta, fp);
            return;
        }

        // Non-PDF documents stay icon + metadata.
        self.cancel_debounce();
        self.gen.set(self.gen.get().wrapping_add(1));
        *self.last_path.borrow_mut() = Some(path.to_path_buf());
        *self.inflight.borrow_mut() = None;
        let badge = media_badge(media);
        let icon_name = item
            .icon
            .as_deref()
            .unwrap_or_else(|| icon_for_media(media));
        let detail = match media {
            MediaKind::Video => format!("Video file\n{meta}"),
            MediaKind::Document => format!("Document\n{meta}"),
            _ => meta,
        };
        self.show_icon_preview(icon_name, badge, &item.title, &item.subtitle, Some(&detail));
    }

    fn cancel_debounce(&self) {
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
    }

    fn show_icon_preview(
        &self,
        icon: &str,
        badge: &str,
        title: &str,
        subtitle: &str,
        meta: Option<&str>,
    ) {
        self.picture.set_paintable(Option::<&gdk::Paintable>::None);
        Self::show_icon_preview_shared(
            &self.icon,
            &self.icon_type,
            &self.icon_title,
            &self.icon_sub,
            &self.icon_meta,
            &self.stack,
            icon,
            badge,
            title,
            subtitle,
            meta,
        );
    }

    fn show_image_chrome(&self, title: &str, meta: &str, dims: &str) {
        self.image_title.set_text(title);
        self.image_meta.set_text(meta);
        self.image_dims.set_text(dims);
        self.stack.set_visible_child_name("image");
    }

    /// Shared icon-view renderer used by both `show_icon_preview` and the
    /// audio worker (which only has borrowed widget handles).
    #[allow(clippy::too_many_arguments)]
    fn show_icon_preview_shared(
        icon: &Image,
        icon_type: &Label,
        icon_title: &Label,
        icon_sub: &Label,
        icon_meta: &Label,
        stack: &Stack,
        icon_name: &str,
        badge: &str,
        title: &str,
        subtitle: &str,
        meta: Option<&str>,
    ) {
        icon.set_icon_name(Some(resolve_icon_name(icon_name)));
        icon_type.set_text(badge);
        icon_title.set_text(title);
        icon_sub.set_text(subtitle);
        if let Some(m) = meta {
            icon_meta.set_text(m);
            icon_meta.set_visible(true);
        } else {
            icon_meta.set_text("");
            icon_meta.set_visible(false);
        }
        stack.set_visible_child_name("icon");
    }

    /// Cache hit only when path *and* filesystem fingerprint match.
    fn apply_cached(&self, path: &Path, fp: FileFp, title: &str, meta: &str) -> bool {
        let hit = {
            let map = self.cache.borrow();
            match map.get(path) {
                Some(c) if c.fp == fp => Some(c.clone()),
                _ => None,
            }
        };
        let Some(cached) = hit else {
            // Stale fingerprint: drop so we don't keep serving old pixels later.
            if self.cache.borrow().get(path).is_some_and(|c| c.fp != fp) {
                self.evict_cache(path);
            }
            return false;
        };
        self.touch_cache(path);
        self.gen.set(self.gen.get().wrapping_add(1));
        *self.last_path.borrow_mut() = Some(path.to_path_buf());
        *self.inflight.borrow_mut() = None;
        self.show_image_chrome(title, meta, &cached.dims_label);
        self.picture.set_paintable(Some(&cached.texture));
        true
    }

    fn touch_cache(&self, path: &Path) {
        let mut order = self.cache_order.borrow_mut();
        if let Some(i) = order.iter().position(|p| p == path) {
            let p = order.remove(i);
            order.push(p);
        }
    }

    fn evict_cache(&self, path: &Path) {
        self.cache.borrow_mut().remove(path);
        let mut order = self.cache_order.borrow_mut();
        if let Some(i) = order.iter().position(|p| p == path) {
            order.remove(i);
        }
    }

    fn insert_cache(
        cache: &RefCell<HashMap<PathBuf, CachedTexture>>,
        order: &RefCell<Vec<PathBuf>>,
        path: PathBuf,
        entry: CachedTexture,
    ) {
        let mut map = cache.borrow_mut();
        let mut order = order.borrow_mut();
        if map.contains_key(&path) {
            if let Some(i) = order.iter().position(|p| p == &path) {
                order.remove(i);
            }
        }
        map.insert(path.clone(), entry);
        order.push(path);
        while order.len() > TEXTURE_CACHE_CAP {
            if let Some(old) = order.first().cloned() {
                order.remove(0);
                map.remove(&old);
            } else {
                break;
            }
        }
    }

    /// Immediate cache path; debounce + single-flight decode while arrowing.
    /// `fp` comes from the single metadata probe in `update` (no second FS hit).
    fn queue_image_load(&self, path: PathBuf, title: String, meta: String, fp: FileFp) {
        // Already showing this exact file revision — nothing to do.
        if self.last_path.borrow().as_ref() == Some(&path)
            && self.picture.paintable().is_some()
            && self.cache.borrow().get(&path).is_some_and(|c| c.fp == fp)
        {
            return;
        }

        // Cache hit (path + mtime/size): paint immediately.
        if self.apply_cached(&path, fp, &title, &meta) {
            self.cancel_debounce();
            return;
        }

        // Full image decode is memory-heavy; video/PDF only extract a small frame.
        let kind = media_kind(&path);
        let size_cap = match kind {
            MediaKind::Image => MAX_IMAGE_BYTES,
            MediaKind::Video | MediaKind::Document => 2 * 1024 * 1024 * 1024, // 2 GiB
            _ => MAX_IMAGE_BYTES,
        };
        if fp.len > size_cap {
            self.cancel_debounce();
            self.gen.set(self.gen.get().wrapping_add(1));
            *self.last_path.borrow_mut() = Some(path);
            *self.inflight.borrow_mut() = None;
            self.show_image_chrome(&title, &meta, "File too large to preview");
            self.picture.set_paintable(Option::<&gdk::Paintable>::None);
            return;
        }

        self.show_image_chrome(&title, &meta, "Loading preview…");
        self.picture.set_paintable(Option::<&gdk::Paintable>::None);
        *self.last_path.borrow_mut() = Some(path.clone());

        // Debounce so rapid ↑/↓ only keeps the latest request.
        // FreeDesktop thumb lookup + decode both run on the worker (not main).
        self.cancel_debounce();
        let gen = self.gen.get().wrapping_add(1);
        self.gen.set(gen);

        let req = DecodeRequest {
            path: path.clone(),
            gen,
            fp,
        };
        *self.inflight.borrow_mut() = Some(req);

        let gen_cell = self.gen.clone();
        let last_path = self.last_path.clone();
        let inflight = self.inflight.clone();
        let debounce = self.debounce.clone();
        let path_check = path;

        // Shared handles for kicking the single worker after debounce.
        let picture = self.picture.clone();
        let dims = self.image_dims.clone();
        let stack = self.stack.clone();
        let cache = self.cache.clone();
        let cache_order = self.cache_order.clone();
        let worker_busy = self.worker_busy.clone();

        let id = glib::timeout_add_local(LOAD_DEBOUNCE, move || {
            *debounce.borrow_mut() = None;
            if gen_cell.get() != gen {
                return glib::ControlFlow::Break;
            }
            if last_path.borrow().as_ref() != Some(&path_check) {
                return glib::ControlFlow::Break;
            }
            // Start (or queue behind the single worker) the latest inflight request.
            Self::pump_worker(
                picture.clone(),
                dims.clone(),
                stack.clone(),
                gen_cell.clone(),
                last_path.clone(),
                cache.clone(),
                cache_order.clone(),
                inflight.clone(),
                worker_busy.clone(),
                debounce.clone(),
            );
            glib::ControlFlow::Break
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    /// Load a source file off-main-thread, then render it with GtkSourceView.
    /// Same debounce/latest-wins pattern as image loads.
    fn queue_code_load(&self, path: PathBuf, title: String, meta: String, fp: FileFp) {
        if fp.len > MAX_CODE_BYTES {
            self.cancel_debounce();
            self.gen.set(self.gen.get().wrapping_add(1));
            *self.last_path.borrow_mut() = Some(path);
            *self.inflight.borrow_mut() = None;
            self.show_code_chrome(&title, &meta, "File too large to preview");
            return;
        }

        self.show_code_chrome(&title, &meta, "Loading…");
        self.cancel_debounce();
        let gen = self.gen.get().wrapping_add(1);
        self.gen.set(gen);
        *self.last_path.borrow_mut() = Some(path.clone());
        *self.inflight.borrow_mut() = None;

        let gen_cell = self.gen.clone();
        let last_path = self.last_path.clone();
        let debounce = self.debounce.clone();
        let path_check = path;

        let view = self.code_view.clone();
        let dims = self.code_dims.clone();
        let stack = self.stack.clone();

        let id = glib::timeout_add_local(LOAD_DEBOUNCE, move || {
            *debounce.borrow_mut() = None;
            if gen_cell.get() != gen || last_path.borrow().as_ref() != Some(&path_check) {
                return glib::ControlFlow::Break;
            }
            let (tx, rx) = async_channel::bounded::<Option<CodePreview>>(1);
            let path_worker = path_check.clone();
            std::thread::spawn(move || {
                // Re-enforce the size gate here: the file may have grown (or
                // been replaced) since the stat in update() — `take()` bounds
                // the read (TOCTOU, audit P3). Lossy decode serves UTF-16 /
                // Latin-1 sources instead of "Could not load file" (audit P3).
                // Long-line/line-count measuring also happens here so the main
                // thread never layouts a minified asset (audit P2 Pass 16).
                let preview = std::fs::File::open(&path_worker)
                    .ok()
                    .and_then(|f| {
                        use std::io::Read as _;
                        let mut buf = Vec::new();
                        f.take(MAX_CODE_BYTES + 1).read_to_end(&mut buf).ok()?;
                        if buf.len() as u64 > MAX_CODE_BYTES {
                            return None;
                        }
                        Some(String::from_utf8_lossy(&buf).into_owned())
                    })
                    .map(|text| prepare_code_preview(&text));
                let _ = tx.send_blocking(preview);
            });
            let gen_cell2 = gen_cell.clone();
            let last_path2 = last_path.clone();
            let path_check2 = path_check.clone();
            let view = view.clone();
            let dims = dims.clone();
            let stack = stack.clone();
            let meta2 = meta.clone();
            glib::spawn_future_local(async move {
                let preview = rx.recv().await.ok().flatten();
                if gen_cell2.get() != gen || last_path2.borrow().as_ref() != Some(&path_check2) {
                    return;
                }
                let Some(preview) = preview else {
                    dims.set_text("Could not load file");
                    stack.set_visible_child_name("code");
                    return;
                };
                view.buffer().set_text(&preview.display);
                if let Ok(buf) = view.buffer().downcast::<sourceview5::Buffer>() {
                    if preview.highlight_off {
                        // Minified/huge file: skip highlight entirely (upstream
                        // GtkSourceView would stall ~2 s then disable it
                        // anyway). Clearing language avoids leaking the
                        // previous file's highlighting.
                        buf.set_highlight_syntax(false);
                        buf.set_language(None);
                    } else {
                        // Re-enable after a previous long file turned it off,
                        // then clear stale language when guessing fails.
                        buf.set_highlight_syntax(true);
                        let lang = guess_language(&path_check2);
                        buf.set_language(lang.as_ref());
                    }
                }
                let lines_label = if preview.total_lines > 0 {
                    let mut label = format!(
                        "{} line{} · {meta2}",
                        preview.total_lines,
                        if preview.total_lines == 1 { "" } else { "s" }
                    );
                    if preview.truncated {
                        if preview.max_line_chars > MAX_CODE_LINE_CHARS {
                            label.push_str(&format!(
                                " · long line truncated ({}+ chars)",
                                MAX_CODE_LINE_CHARS
                            ));
                        } else {
                            label.push_str(&format!(
                                " · truncated (first {DISPLAY_MAX_LINES} lines)"
                            ));
                        }
                    }
                    label
                } else {
                    meta2
                };
                dims.set_text(&lines_label);
                stack.set_visible_child_name("code");
            });
            glib::ControlFlow::Break
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    fn show_code_chrome(&self, title: &str, meta: &str, dims: &str) {
        self.code_title.set_text(title);
        self.code_meta.set_text(meta);
        self.code_dims.set_text(dims);
        self.stack.set_visible_child_name("code");
    }

    /// Probe audio tags + embedded art off-main, then render album art (when
    /// present) through the picture pipeline or fall back to icon + metadata.
    fn queue_audio_load(&self, path: PathBuf, item: SearchResult, meta: String, _fp: FileFp) {
        self.cancel_debounce();
        let gen = self.gen.get().wrapping_add(1);
        self.gen.set(gen);
        *self.last_path.borrow_mut() = Some(path.clone());
        *self.inflight.borrow_mut() = None;

        let gen_cell = self.gen.clone();
        let last_path = self.last_path.clone();
        let debounce = self.debounce.clone();
        let path_check = path.clone();

        let picture = self.picture.clone();
        let dims = self.image_dims.clone();
        let stack = self.stack.clone();
        let image_title = self.image_title.clone();
        let image_meta = self.image_meta.clone();
        let icon = self.icon.clone();
        let icon_title = self.icon_title.clone();
        let icon_sub = self.icon_sub.clone();
        let icon_meta = self.icon_meta.clone();
        let icon_type = self.icon_type.clone();
        let item2 = item.clone();

        let id = glib::timeout_add_local(LOAD_DEBOUNCE, move || {
            *debounce.borrow_mut() = None;
            if gen_cell.get() != gen || last_path.borrow().as_ref() != Some(&path_check) {
                return glib::ControlFlow::Break;
            }
            let (tx, rx) = async_channel::bounded::<Option<AudioMeta>>(1);
            let path_worker = path_check.clone();
            std::thread::spawn(move || {
                let _ = tx.send_blocking(read_audio_meta(&path_worker));
            });
            let gen_cell2 = gen_cell.clone();
            let last_path2 = last_path.clone();
            let path_check2 = path_check.clone();
            let meta2 = meta.clone();
            let picture = picture.clone();
            let dims = dims.clone();
            let stack = stack.clone();
            let image_title = image_title.clone();
            let image_meta = image_meta.clone();
            let icon = icon.clone();
            let icon_title = icon_title.clone();
            let icon_sub = icon_sub.clone();
            let icon_meta = icon_meta.clone();
            let icon_type = icon_type.clone();
            let item2 = item2.clone();
            glib::spawn_future_local(async move {
                let info = rx.recv().await.ok().flatten();
                if gen_cell2.get() != gen || last_path2.borrow().as_ref() != Some(&path_check2) {
                    return;
                }
                match info {
                    Some(mut info) if info.cover.is_some() => {
                        // Album art → picture pipeline.
                        let dims_label = info.dims_label.clone();
                        let px = info.cover.take();
                        if let Some(px) = px {
                            if let Some(tex) = texture_from_pixels(px) {
                                image_title.set_text(&info.headline);
                                image_meta.set_text(&info.meta_line);
                                dims.set_text(&dims_label);
                                picture.set_paintable(Some(&tex));
                                stack.set_visible_child_name("image");
                                return;
                            }
                        }
                        Self::audio_icon_fallback(
                            &icon,
                            &icon_title,
                            &icon_sub,
                            &icon_meta,
                            &icon_type,
                            &item2,
                            &info,
                            &stack,
                        );
                    }
                    Some(info) => {
                        Self::audio_icon_fallback(
                            &icon,
                            &icon_title,
                            &icon_sub,
                            &icon_meta,
                            &icon_type,
                            &item2,
                            &info,
                            &stack,
                        );
                    }
                    None => {
                        let badge = "Audio";
                        let icon_name = item2
                            .icon
                            .as_deref()
                            .unwrap_or_else(|| icon_for_media(MediaKind::Audio));
                        let detail = format!("Audio file\n{meta2}");
                        Self::show_icon_preview_shared(
                            &icon,
                            &icon_type,
                            &icon_title,
                            &icon_sub,
                            &icon_meta,
                            &stack,
                            icon_name,
                            badge,
                            &item2.title,
                            &item2.subtitle,
                            Some(&detail),
                        );
                    }
                }
            });
            glib::ControlFlow::Break
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    #[allow(clippy::too_many_arguments)]
    fn audio_icon_fallback(
        icon: &Image,
        icon_title: &Label,
        icon_sub: &Label,
        icon_meta: &Label,
        icon_type: &Label,
        item: &SearchResult,
        info: &AudioMeta,
        stack: &Stack,
    ) {
        let icon_name = item
            .icon
            .as_deref()
            .unwrap_or_else(|| icon_for_media(MediaKind::Audio));
        Self::show_icon_preview_shared(
            icon,
            icon_type,
            icon_title,
            icon_sub,
            icon_meta,
            stack,
            icon_name,
            "Audio",
            &info.headline,
            &info.sub_line,
            Some(&info.meta_line),
        );
    }

    /// At most one decode thread. Latest `inflight` wins; never stacks workers.
    /// If a debounce is still pending when a job finishes, wait for it (user still scrolling).
    #[allow(clippy::too_many_arguments)]
    fn pump_worker(
        picture: Picture,
        dims: Label,
        stack: Stack,
        gen_cell: Rc<Cell<u64>>,
        last_path: Rc<RefCell<Option<PathBuf>>>,
        cache: Rc<RefCell<HashMap<PathBuf, CachedTexture>>>,
        cache_order: Rc<RefCell<Vec<PathBuf>>>,
        inflight: Rc<RefCell<Option<DecodeRequest>>>,
        worker_busy: Rc<Cell<bool>>,
        debounce: Rc<RefCell<Option<glib::SourceId>>>,
    ) {
        if worker_busy.get() {
            return;
        }
        let Some(req) = inflight.borrow().clone() else {
            return;
        };
        // Stale slot (selection moved on before debounce fired).
        if gen_cell.get() != req.gen || last_path.borrow().as_ref() != Some(&req.path) {
            *inflight.borrow_mut() = None;
            return;
        }

        worker_busy.set(true);
        let (tx, rx) = async_channel::bounded::<Option<DecodedPixels>>(1);
        let path_worker = req.path.clone();
        std::thread::spawn(move || {
            // Thumb path resolve + decode/extract stay off the GTK main loop.
            let decoded = decode_preview_media(&path_worker);
            // Best-effort: write FreeDesktop thumb so next open / other apps hit cache.
            if let Some(ref px) = decoded {
                if freedesktop_thumbnail(&path_worker).is_none() {
                    let _ = store_freedesktop_thumbnail(
                        &path_worker,
                        px.width,
                        px.height,
                        px.rowstride,
                        px.has_alpha,
                        &px.pixels,
                    );
                }
            }
            let _ = tx.send_blocking(decoded);
        });

        let picture2 = picture.clone();
        let dims2 = dims.clone();
        let stack2 = stack.clone();
        let gen_cell2 = gen_cell.clone();
        let last_path2 = last_path.clone();
        let cache2 = cache.clone();
        let cache_order2 = cache_order.clone();
        let inflight2 = inflight.clone();
        let worker_busy2 = worker_busy.clone();
        let debounce2 = debounce.clone();
        let req_path = req.path.clone();
        let req_gen = req.gen;
        let req_fp = req.fp;

        glib::spawn_future_local(async move {
            let decoded = rx.recv().await.ok().flatten();
            worker_busy2.set(false);

            let still_current =
                gen_cell2.get() == req_gen && last_path2.borrow().as_ref() == Some(&req_path);

            if still_current {
                // Clear this request only if nothing newer replaced it.
                {
                    let mut slot = inflight2.borrow_mut();
                    if slot.as_ref().is_some_and(|r| r.gen == req_gen) {
                        *slot = None;
                    }
                }
                match decoded {
                    Some(px) => {
                        let dims_label = px.dims_label.clone();
                        if let Some(tex) = texture_from_pixels(px) {
                            dims2.set_text(&dims_label);
                            picture2.set_paintable(Some(&tex));
                            stack2.set_visible_child_name("image");
                            Self::insert_cache(
                                &cache2,
                                &cache_order2,
                                req_path,
                                CachedTexture {
                                    texture: tex,
                                    dims_label,
                                    fp: req_fp,
                                },
                            );
                        } else {
                            dims2.set_text("Could not load preview");
                            picture2.set_paintable(Option::<&gdk::Paintable>::None);
                        }
                    }
                    None => {
                        dims2.set_text("Could not load preview");
                        picture2.set_paintable(Option::<&gdk::Paintable>::None);
                    }
                }
            }

            // Newer selection while we were busy: run it only if debounce already settled.
            // If a debounce timer is pending, that callback will pump when the user stops.
            if inflight2.borrow().is_some() && debounce2.borrow().is_none() {
                Self::pump_worker(
                    picture2,
                    dims2,
                    stack2,
                    gen_cell2,
                    last_path2,
                    cache2,
                    cache_order2,
                    inflight2,
                    worker_busy2,
                    debounce2,
                );
            }
        });
    }
}

/// Send-safe decoded pixels (Pixbuf/GObject is !Send).
struct DecodedPixels {
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
    pixels: Vec<u8>,
    dims_label: String,
}

/// Off-main audio tag/art probe result (Send-safe, owns no GObject).
struct AudioMeta {
    /// `"Title — Artist"` (or fallbacks) for the headline label.
    headline: String,
    /// `"Artist · Album"` (or fallback) for the subtitle line.
    sub_line: String,
    /// `"3:24 · 8.1 MB · MP3"` merged meta string.
    meta_line: String,
    /// Decoded embedded cover art (already scaled), if the file has one.
    cover: Option<DecodedPixels>,
    /// `"3:24"` label for the picture view dims line.
    dims_label: String,
}

/// Best-effort audio tag probe. Never fails the preview path — returns `None`
/// only for genuinely unreadable/empty files.
fn read_audio_meta(path: &Path) -> Option<AudioMeta> {
    let file = lofty::read_from_path(path).ok()?;
    let properties = file.properties().clone();
    let duration = properties.duration();

    let tag = file.primary_tag().or_else(|| file.first_tag());
    let title = tag.and_then(Accessor::title).map(|s| s.to_string());
    let artist = tag.and_then(Accessor::artist).map(|s| s.to_string());
    let album = tag.and_then(Accessor::album).map(|s| s.to_string());

    let duration_label = if duration.is_zero() {
        String::new()
    } else {
        format_duration(duration)
    };

    let headline = title.clone().unwrap_or_else(|| {
        path.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Audio".into())
    });

    let sub_line = match (&artist, &album) {
        (Some(a), Some(al)) => format!("{a} · {al}"),
        (Some(a), None) => a.clone(),
        (None, Some(al)) => al.clone(),
        _ => String::new(),
    };

    // "3:24 · 8.1 MB · MP3" (duration first when present).
    let mut parts: Vec<String> = Vec::with_capacity(3);
    if !duration_label.is_empty() {
        parts.push(duration_label.clone());
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    parts.push(format_size(size));
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        parts.push(ext.to_ascii_uppercase());
    }
    let meta_line = parts.join(" · ");

    // Embedded cover art (first picture), decoded + scaled off-main.
    let cover = tag
        .and_then(|t| t.pictures().first())
        .and_then(|pic| decode_picture_bytes(pic.data(), pic.mime_type()));

    Some(AudioMeta {
        headline,
        sub_line,
        meta_line,
        cover,
        dims_label: if duration_label.is_empty() {
            "Audio".into()
        } else {
            format!("Audio · {duration_label}")
        },
    })
}

/// Decode raw album-art bytes (JPEG/PNG/…) into scaled preview pixels.
fn decode_picture_bytes(
    data: &[u8],
    mime: Option<&lofty::picture::MimeType>,
) -> Option<DecodedPixels> {
    use std::io::Cursor;
    let pixbuf = match mime {
        Some(m) => {
            let loader = gtk::gdk_pixbuf::PixbufLoader::with_mime_type(m.as_str()).ok()?;
            loader.write(data).ok()?;
            loader.close().ok()?;
            loader.pixbuf()?
        }
        None => Pixbuf::from_read(Cursor::new(data.to_vec())).ok()?,
    };
    // #26: preserve aspect ratio — scaling to a forced square stretched album
    // covers. Fit within DECODE_MAX_PX on the long edge only.
    let w = pixbuf.width();
    let h = pixbuf.height();
    let scaled = if w.max(h) > DECODE_MAX_PX {
        let ratio = DECODE_MAX_PX as f64 / w.max(h) as f64;
        let nw = ((w as f64) * ratio).round().max(1.0) as i32;
        let nh = ((h as f64) * ratio).round().max(1.0) as i32;
        pixbuf.scale_simple(nw, nh, gtk::gdk_pixbuf::InterpType::Bilinear)?
    } else {
        pixbuf
    };
    let mut px = pixbuf_to_pixels(&scaled)?;
    px.dims_label = String::new();
    Some(px)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Best-effort language detection for the code preview (extension-based).
fn guess_language(path: &Path) -> Option<sourceview5::Language> {
    let filename = path.file_name().and_then(|s| s.to_str())?;
    let mgr = LanguageManager::default();
    mgr.guess_language(Some(filename), None)
}

/// Unified off-main decode for images, video first-frame, and PDF page 1.
fn decode_preview_media(path: &Path) -> Option<DecodedPixels> {
    // FreeDesktop cache first (images we generated, or system thumbnailers).
    if let Some(thumb) = freedesktop_thumbnail(path) {
        if let Some(px) = decode_thumb_or_scaled(&thumb, path) {
            return Some(px);
        }
    }

    let kind = media_kind(path);
    let is_pdf = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false);

    if kind == MediaKind::Video {
        return decode_video_frame(path);
    }
    if is_pdf {
        return decode_pdf_page(path);
    }
    if kind == MediaKind::Image {
        return decode_image_scaled(path);
    }
    None
}

/// Prefer FreeDesktop thumb; fall back to scaled original (single open each).
fn decode_thumb_or_scaled(thumb: &Path, original: &Path) -> Option<DecodedPixels> {
    if let Ok(pb) = Pixbuf::from_file(thumb) {
        if let Some(mut px) = pixbuf_to_pixels(&pb) {
            let kind = media_kind(original);
            let is_pdf = original
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("pdf"))
                .unwrap_or(false);
            px.dims_label = if kind == MediaKind::Video {
                format!("Video · {} × {}", px.width, px.height)
            } else if is_pdf {
                format!("PDF · {} × {}", px.width, px.height)
            } else {
                format!("Thumbnail · {} × {}", px.width, px.height)
            };
            return Some(px);
        }
    }
    // Thumb corrupt — fall through by kind.
    let kind = media_kind(original);
    if kind == MediaKind::Video {
        return decode_video_frame(original);
    }
    if original
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
    {
        return decode_pdf_page(original);
    }
    decode_image_scaled(original)
}

/// User-private scratch space for converter output (ffmpeg/pdftoppm PNG
/// frames). Lives under the XDG cache rather than shared `/tmp` and is
/// forced to 0700 so another local user can neither reach nor pre-plant the
/// files we decode. `None` → caller soft-fails like a missing converter.
fn converter_scratch_dir() -> Option<PathBuf> {
    let dir = dirs::cache_dir()
        .or_else(dirs::home_dir)?
        .join("hark/preview");
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // create_dir_all honours umask; force user-private afterwards.
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

/// Remove converter outputs orphaned by a crash (audit P3): anything older
/// than an hour in our exclusive scratch dir. Fresh files may belong to an
/// in-flight decode, so they are left alone. Best-effort — failures ignored.
fn sweep_converter_scratch() {
    let Some(dir) = converter_scratch_dir() else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    const MAX_AGE_SECS: u64 = 3600;
    for entry in rd.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs() > MAX_AGE_SECS)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Best-effort OS-entropy token so output filenames are unpredictable across
/// processes (tiny local copy of the calc provider's /dev/urandom read — no
/// cross-module import). Falls back to time+pid entropy.
fn random_token() -> String {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut buf = [0u8; 8];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            if f.read_exact(&mut buf).is_ok() {
                return buf.iter().map(|b| format!("{b:02x}")).collect();
            }
        }
    }
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{:x}-{}", nanos, std::process::id())
}

/// Pre-create the converter output file with 0600 so the encoder keeps our
/// private inode instead of making a umask-world-readable one (unix only;
/// other targets just truncate-create).
#[cfg(unix)]
fn create_private_file(path: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .is_ok()
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> bool {
    std::fs::File::create(path).is_ok()
}

/// First video frame via `ffmpeg` (optional — fails soft if missing).
fn decode_video_frame(path: &Path) -> Option<DecodedPixels> {
    // Unique per-call name inside a 0700 dir: nothing predictable to race.
    let tmp_dir = converter_scratch_dir()?;
    let token = format!(
        "v-{}-{}-{}",
        random_token(),
        std::process::id(),
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("vid")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(24)
            .collect::<String>()
    );
    let out = tmp_dir.join(format!("{token}.png"));
    if !create_private_file(&out) {
        return None;
    }

    // Seek a bit past 0 so black intro frames are less common; fall back to 0s.
    let mut ok = run_ffmpeg_frame(path, &out, "0.5");
    if !ok || !out.is_file() {
        ok = run_ffmpeg_frame(path, &out, "0");
    }
    if !ok || !out.is_file() {
        let _ = std::fs::remove_file(&out);
        return None;
    }

    let result = Pixbuf::from_file_at_scale(&out, DECODE_MAX_PX, DECODE_MAX_PX, true)
        .ok()
        .and_then(|pb| {
            let mut px = pixbuf_to_pixels(&pb)?;
            px.dims_label = format!("Video · {} × {}", px.width, px.height);
            Some(px)
        });
    let _ = std::fs::remove_file(&out);
    result
}

/// Run `cmd` with a hard deadline. A hung ffmpeg/pdftoppm must never block
/// this worker thread forever — past the deadline we SIGKILL and reap so the
/// caller's busy flag always clears.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> bool {
    // Detach child stdio: an inherited supervisor pipe could fill and stall
    // the child until the deadline kill.
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let poll = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
        if waited >= timeout {
            let _ = child.kill();
            let _ = child.wait(); // reap — no zombie
            return false;
        }
        std::thread::sleep(poll);
        waited += poll;
    }
}

/// Minimal environment for external decoders (audit Pass 16): ffmpeg /
/// pdftoppm run with user-controlled file arguments, so an inherited env
/// (LD_PRELOAD, FFREPORT, HOME hooks…) must not reach them. PATH is kept
/// for discoverability but only standard system prefixes are honored —
/// resolves the binary the same way a login shell would, without
/// executing anything out of a user-writable directory.
fn converter_command(bin: &str) -> Command {
    let mut cmd = Command::new(resolve_system_binary(bin).unwrap_or_else(|| bin.into()));
    cmd.env_clear();
    cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
    if let Some(home) = std::env::var_os("HOME") {
        cmd.env("HOME", home);
    }
    // ffmpeg needs no AVFoundation/fontconfig vars on Linux; pdftoppm
    // needs none beyond HOME. Locale only affects log strings.
    cmd.env("LC_ALL", "C");
    cmd
}

/// Absolute path for `bin` from the standard system prefixes, first hit
/// wins. None means "not found" → caller falls back to the bare name
/// (PATH lookup above still constrains it to the same prefixes).
fn resolve_system_binary(bin: &str) -> Option<PathBuf> {
    for dir in ["/usr/local/bin", "/usr/bin", "/bin"] {
        let p = Path::new(dir).join(bin);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn run_ffmpeg_frame(path: &Path, out: &Path, ss: &str) -> bool {
    let mut cmd = converter_command("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-ss", ss, "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-an",
            "-vf",
            &format!("scale='min({DECODE_MAX_PX},iw)':-2"),
            "-y",
        ])
        .arg(out);
    run_with_timeout(cmd, CONVERTER_TIMEOUT)
}

/// PDF page 1 via `pdftoppm` (poppler-utils). Soft-fail if missing.
fn decode_pdf_page(path: &Path) -> Option<DecodedPixels> {
    // Same private scratch dir + unpredictable name as the video path.
    let tmp_dir = converter_scratch_dir()?;
    let token = format!(
        "p-{}-{}-{}",
        random_token(),
        std::process::id(),
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("pdf")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(24)
            .collect::<String>()
    );
    let prefix = tmp_dir.join(&token);
    // pdftoppm -singlefile -png writes `{prefix}.png`
    let out = tmp_dir.join(format!("{token}.png"));
    if !create_private_file(&out) {
        return None;
    }

    let mut cmd = converter_command("pdftoppm");
    cmd.args([
        "-f",
        "1",
        "-l",
        "1",
        "-singlefile",
        "-png",
        "-scale-to",
        &DECODE_MAX_PX.to_string(),
    ])
    .arg(path)
    .arg(&prefix);
    let ok = run_with_timeout(cmd, CONVERTER_TIMEOUT);

    if !ok || !out.is_file() {
        let _ = std::fs::remove_file(&out);
        return None;
    }

    let result = Pixbuf::from_file(&out).ok().and_then(|pb| {
        let mut px = pixbuf_to_pixels(&pb)?;
        px.dims_label = format!("PDF page 1 · {} × {}", px.width, px.height);
        Some(px)
    });
    let _ = std::fs::remove_file(&out);
    result
}

/// Off-main-thread scaled decode. `file_info` is header-only (cheap); full
/// pixel work is a single `from_file_at_scale` open — both stay off the GTK loop.
fn decode_image_scaled(path: &Path) -> Option<DecodedPixels> {
    let (native_w, native_h) = Pixbuf::file_info(path)
        .map(|(_, w, h)| (w, h))
        .unwrap_or((0, 0));
    let pixbuf = Pixbuf::from_file_at_scale(path, DECODE_MAX_PX, DECODE_MAX_PX, true).ok()?;
    // EXIF Orientation is metadata, not pixels: `from_file_at_scale` decodes
    // phone photos sideways without this (audit P3). `None` = no/invalid tag,
    // keep the pixbuf as decoded. (This tree never writes the shared
    // FreeDesktop thumbnail cache, so there is no cache-poisoning leg —
    // only the preview decode needed the fix.)
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let mut px = pixbuf_to_pixels(&pixbuf)?;
    px.dims_label = if native_w > 0 && native_h > 0 {
        format!("{native_w} × {native_h}")
    } else {
        format!("{} × {}", px.width, px.height)
    };
    Some(px)
}

fn pixbuf_to_pixels(pixbuf: &Pixbuf) -> Option<DecodedPixels> {
    let width = pixbuf.width();
    let height = pixbuf.height();
    let rowstride = pixbuf.rowstride();
    let has_alpha = pixbuf.has_alpha();
    let n_channels = pixbuf.n_channels();
    let bits = pixbuf.bits_per_sample();
    if width <= 0 || height <= 0 || bits != 8 || (n_channels != 3 && n_channels != 4) {
        return None;
    }

    // Copy pixel buffer before pixbuf drops (pixels() is a raw view).
    let pixels = unsafe {
        let slice = pixbuf.pixels();
        slice.to_vec()
    };

    Some(DecodedPixels {
        width,
        height,
        rowstride,
        has_alpha,
        pixels,
        dims_label: format!("{width} × {height}"),
    })
}

fn texture_from_pixels(px: DecodedPixels) -> Option<Texture> {
    if px.width <= 0 || px.height <= 0 || px.pixels.is_empty() {
        return None;
    }
    // Move pixels into GBytes (no extra copy of the buffer).
    let width = px.width;
    let height = px.height;
    let rowstride = px.rowstride;
    let has_alpha = px.has_alpha;
    let bytes = glib::Bytes::from_owned(px.pixels);
    let pixbuf = Pixbuf::from_bytes(
        &bytes,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        has_alpha,
        8,
        width,
        height,
        rowstride,
    );
    Some(Texture::for_pixbuf(&pixbuf))
}

pub fn media_kind(path: &Path) -> MediaKind {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return MediaKind::Other;
    };
    // Case-insensitive match without allocating a lowercased String.
    if ext_is(
        ext,
        &[
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "tif", "tiff", "heic",
            "heif", "avif", "jxl",
        ],
    ) {
        return MediaKind::Image;
    }
    if ext_is(
        ext,
        &[
            "mp4", "webm", "mkv", "mov", "avi", "m4v", "wmv", "flv", "mpeg", "mpg",
        ],
    ) {
        return MediaKind::Video;
    }
    if ext_is(
        ext,
        &[
            "mp3", "flac", "ogg", "wav", "m4a", "aac", "opus", "wma", "aiff",
        ],
    ) {
        return MediaKind::Audio;
    }
    if ext_is(
        ext,
        &[
            "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "epub", "xls", "xlsx", "ppt", "pptx",
            "csv",
        ],
    ) {
        return MediaKind::Document;
    }
    if ext_is(
        ext,
        &["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst"],
    ) {
        return MediaKind::Archive;
    }
    if ext_is(
        ext,
        &[
            "rs", "py", "js", "ts", "tsx", "jsx", "go", "c", "h", "cpp", "hpp", "java", "kt",
            "swift", "rb", "php", "sh", "bash", "zsh", "toml", "yaml", "yml", "json", "xml",
            "html", "css", "scss", "vue", "svelte", "lua", "sql",
        ],
    ) {
        return MediaKind::Code;
    }
    MediaKind::Other
}

#[inline]
fn ext_is(ext: &str, list: &[&str]) -> bool {
    list.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

fn icon_for_media(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image-x-generic",
        MediaKind::Video => "video-x-generic",
        MediaKind::Audio => "audio-x-generic",
        MediaKind::Document => "x-office-document",
        MediaKind::Archive => "package-x-generic",
        MediaKind::Code => "text-x-script",
        MediaKind::Other => "text-x-generic",
    }
}

/// Prefer the requested icon name; fall back to a known-good FreeDesktop icon.
fn resolve_icon_name(name: &str) -> &str {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if theme.has_icon(name) {
            return name;
        }
    }
    if name.starts_with("image-") {
        "image-x-generic"
    } else if name.starts_with("video-") {
        "video-x-generic"
    } else if name.starts_with("audio-") {
        "audio-x-generic"
    } else if name.contains("pdf") {
        "application-pdf"
    } else if name.contains("zip")
        || name.contains("tar")
        || name.contains("gzip")
        || name.contains("package")
        || name.contains("bzip")
    {
        "package-x-generic"
    } else if name.starts_with("text-") || name.starts_with("application-json") {
        "text-x-generic"
    } else if name.starts_with("x-office-") {
        "x-office-document"
    } else if name == "folder" {
        "folder"
    } else {
        "text-x-generic"
    }
}

fn media_badge(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "Image",
        MediaKind::Video => "Video",
        MediaKind::Audio => "Audio",
        MediaKind::Document => "Document",
        MediaKind::Archive => "Archive",
        MediaKind::Code => "Code",
        MediaKind::Other => "File",
    }
}

fn file_meta_from(path: &Path, meta: &std::fs::Metadata) -> String {
    let mut parts = Vec::with_capacity(3);
    parts.push(format_size(meta.len()));
    if let Ok(modified) = meta.modified() {
        parts.push(format_modified(modified));
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        parts.push(ext.to_ascii_uppercase());
    }
    parts.join(" · ")
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_modified(time: SystemTime) -> String {
    let Ok(elapsed) = time.elapsed() else {
        return "Modified recently".into();
    };
    let secs = elapsed.as_secs();
    if secs < 60 {
        "Modified just now".into()
    } else if secs < 3600 {
        format!("Modified {}m ago", secs / 60)
    } else if secs < 86400 {
        format!("Modified {}h ago", secs / 3600)
    } else if secs < 86400 * 30 {
        format!("Modified {}d ago", secs / 86400)
    } else {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => {
                let dt = chrono::DateTime::from_timestamp(d.as_secs() as i64, 0);
                dt.map(|d| d.format("Modified %b %e, %Y").to_string())
                    .unwrap_or_else(|| "Modified".into())
            }
            Err(_) => "Modified".into(),
        }
    }
}

#[cfg(test)]
mod code_preview_tests {
    use super::{prepare_code_preview, DISPLAY_MAX_LINES, MAX_CODE_LINE_CHARS};

    #[test]
    fn short_file_passes_through_with_highlight() {
        let p = prepare_code_preview("fn main() {}\nlet x = 1;\n");
        assert_eq!(p.total_lines, 2);
        assert!(!p.truncated);
        assert!(!p.highlight_off);
        assert_eq!(p.display, "fn main() {}\nlet x = 1;");
    }

    #[test]
    fn single_minified_line_disables_highlight_and_truncates() {
        // Audit P2 Pass 16: ~2M-char minified line froze the main loop.
        let long = "a".repeat(100_000);
        let p = prepare_code_preview(&long);
        assert_eq!(p.total_lines, 1);
        assert!(p.highlight_off);
        assert!(p.truncated);
        assert_eq!(p.display.chars().count(), MAX_CODE_LINE_CHARS);
        assert_eq!(p.max_line_chars, MAX_CODE_LINE_CHARS + 1);
    }

    #[test]
    fn many_lines_truncate_display_but_keep_highlight_when_lines_short() {
        let text = (0..DISPLAY_MAX_LINES + 100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let p = prepare_code_preview(&text);
        assert_eq!(p.total_lines, DISPLAY_MAX_LINES + 100);
        assert!(p.truncated);
        assert!(!p.highlight_off);
        assert_eq!(p.display.lines().count(), DISPLAY_MAX_LINES);
    }

    #[test]
    fn empty_file_is_not_truncated() {
        let p = prepare_code_preview("");
        assert_eq!(p.total_lines, 0);
        assert!(!p.truncated);
        assert!(!p.highlight_off);
        assert!(p.display.is_empty());
    }
}

#[cfg(test)]
mod exif_orientation_tests {
    use super::decode_image_scaled;
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};

    /// Minimal EXIF APP1 segment declaring Orientation = 6 (rotate 90 CW),
    /// little-endian TIFF with a single IFD entry.
    fn app1_orientation_6() -> Vec<u8> {
        let seg = vec![
            0xFF, 0xE1, // APP1 marker
            0x00, 0x22, // length: 34 (includes these 2 bytes)
            0x45, 0x78, 0x69, 0x66, 0x00, 0x00, // "Exif\0\0"
            0x49, 0x49, 0x2A, 0x00, // "II*\0" little-endian
            0x08, 0x00, 0x00, 0x00, // IFD0 offset = 8
            0x01, 0x00, // 1 entry
            0x12, 0x01, // tag 0x0112 Orientation
            0x03, 0x00, // type SHORT
            0x01, 0x00, 0x00, 0x00, // count 1
            0x06, 0x00, 0x00, 0x00, // value 6
            0x00, 0x00, 0x00, 0x00, // next IFD = none
        ];
        assert_eq!(seg.len(), 2 + 34);
        seg
    }

    fn write_jpeg(path: &std::path::Path, with_exif: bool) {
        // Non-square so a 90° rotation visibly swaps dimensions.
        let pb = Pixbuf::new(Colorspace::Rgb, false, 8, 4, 2).expect("pixbuf");
        let mut bytes = pb
            .save_to_bufferv("jpeg", &[])
            .expect("jpeg encode")
            .to_vec();
        assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "SOI marker");
        if with_exif {
            let mut tagged = bytes[..2].to_vec();
            tagged.extend_from_slice(&app1_orientation_6());
            tagged.extend_from_slice(&bytes[2..]);
            bytes = tagged;
        }
        std::fs::write(path, &bytes).unwrap();
    }

    #[test]
    fn exif_orientation_6_rotates_preview() {
        // Audit P3: phone photos decoded sideways without orientation applied.
        let dir = std::env::temp_dir().join(format!(
            "hark-exif-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let plain = dir.join("plain.jpg");
        write_jpeg(&plain, false);
        let px = decode_image_scaled(&plain).expect("plain decodes");
        // `from_file_at_scale` fits the long edge to DECODE_MAX_PX (up or
        // down); what matters is the tagged decode swaps the axes.
        let (w, h) = (px.width, px.height);
        assert!(w > h, "2:1 landscape must stay landscape, got {w}x{h}");

        let rotated = dir.join("rotated.jpg");
        write_jpeg(&rotated, true);
        let px = decode_image_scaled(&rotated).expect("tagged decodes");
        assert_eq!(
            (px.width, px.height),
            (h, w),
            "orientation 6 must swap dimensions"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
