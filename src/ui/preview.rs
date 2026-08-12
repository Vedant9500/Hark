use super::dnd::{DragSession, PathDragBinding};
use super::thumbnails::{freedesktop_thumbnail, store_freedesktop_thumbnail};
use crate::providers::{Action, ResultKind, SearchResult};
use gtk::gdk::{self, Texture};
use gtk::gdk_pixbuf::Pixbuf;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, ContentFit, Image, Label, Orientation, Picture, Separator, Stack};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

/// Max image file size we'll try to decode (bytes).
const MAX_IMAGE_BYTES: u64 = 40 * 1024 * 1024;
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
            MediaKind::Image | MediaKind::Video | MediaKind::Audio | MediaKind::Document
        )
    }
}

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
}

impl PreviewPanel {
    pub fn new(drag_session: DragSession) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("hark-preview");
        root.set_size_request(PREVIEW_WIDTH, -1);
        root.set_width_request(PREVIEW_WIDTH);
        root.set_hexpand(false);
        root.set_vexpand(true);
        root.set_halign(Align::Fill);
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

        stack.set_visible_child_name("icon");
        root.append(&stack);

        // Drag the underlying file from the whole preview panel (image or icon view).
        let drag = PathDragBinding::new(drag_session);
        drag.attach(&root);

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
            gen: Rc::new(Cell::new(0)),
            last_path: Rc::new(RefCell::new(None)),
            cache: Rc::new(RefCell::new(HashMap::new())),
            cache_order: Rc::new(RefCell::new(Vec::new())),
            debounce: Rc::new(RefCell::new(None)),
            inflight: Rc::new(RefCell::new(None)),
            worker_busy: Rc::new(Cell::new(false)),
            drag,
            user_hidden: Rc::new(Cell::new(false)),
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
        self.root.set_visible(vis);
        self.sep.set_visible(vis);
    }

    /// Flip user hide flag. Returns `true` when the panel is now user-hidden.
    pub fn toggle_user_hidden(&self) -> bool {
        let next = !self.user_hidden.get();
        self.user_hidden.set(next);
        if next {
            self.root.set_visible(false);
            self.sep.set_visible(false);
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
        self.stack.set_visible_child_name("icon");
        self.set_panel_visible(false);
    }

    pub fn update(&self, item: Option<&SearchResult>) {
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

        // One metadata probe for dir check, size/mtime labels, and texture fingerprint.
        let fs_meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                self.clear();
                return;
            }
        };
        if fs_meta.is_dir() {
            self.clear();
            return;
        }

        let media = media_kind(&path);
        if !media.is_previewable() {
            self.clear();
            return;
        }

        let fp = FileFp::from_meta(&fs_meta);
        let meta = file_meta_from(&path, &fs_meta);
        self.set_panel_visible(true);
        // Always offer the real file path for DnD from the preview.
        self.drag.set_path(Some(path.clone()));

        let is_pdf = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false);

        // Images, video first-frame, and PDF page 1 share the picture pipeline.
        if media == MediaKind::Image || media == MediaKind::Video || is_pdf {
            self.queue_image_load(path, item.title.clone(), meta, fp);
            return;
        }

        // Audio / non-PDF documents stay icon + metadata.
        self.cancel_debounce();
        self.gen.set(self.gen.get().wrapping_add(1));
        *self.last_path.borrow_mut() = Some(path.clone());
        *self.inflight.borrow_mut() = None;
        let badge = media_badge(media);
        let icon_name = item
            .icon
            .as_deref()
            .unwrap_or_else(|| icon_for_media(media));
        let detail = match media {
            MediaKind::Video => format!("Video file\n{meta}"),
            MediaKind::Audio => format!("Audio file\n{meta}"),
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
        self.icon.set_icon_name(Some(resolve_icon_name(icon)));
        self.icon_type.set_text(badge);
        self.icon_title.set_text(title);
        self.icon_sub.set_text(subtitle);
        if let Some(m) = meta {
            self.icon_meta.set_text(m);
            self.icon_meta.set_visible(true);
        } else {
            self.icon_meta.set_text("");
            self.icon_meta.set_visible(false);
        }
        self.stack.set_visible_child_name("icon");
    }

    fn show_image_chrome(&self, title: &str, meta: &str, dims: &str) {
        self.image_title.set_text(title);
        self.image_meta.set_text(meta);
        self.image_dims.set_text(dims);
        self.stack.set_visible_child_name("image");
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
    if let Some(mut px) = pixbuf_to_pixels(&Pixbuf::from_file(thumb).ok()?) {
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

/// First video frame via `ffmpeg` (optional — fails soft if missing).
fn decode_video_frame(path: &Path) -> Option<DecodedPixels> {
    let tmp_dir = std::env::temp_dir().join("hark-preview");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let token = format!(
        "v-{}-{}",
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
    let _ = std::fs::remove_file(&out);

    // Seek a bit past 0 so black intro frames are less common; fall back to 0s.
    let mut ok = run_ffmpeg_frame(path, &out, "0.5");
    if !ok || !out.is_file() {
        let _ = std::fs::remove_file(&out);
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

fn run_ffmpeg_frame(path: &Path, out: &Path, ss: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-ss", ss, "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-an",
            "-vf",
            &format!("scale='min({DECODE_MAX_PX},iw)':-2"),
            "-y",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// PDF page 1 via `pdftoppm` (poppler-utils). Soft-fail if missing.
fn decode_pdf_page(path: &Path) -> Option<DecodedPixels> {
    let tmp_dir = std::env::temp_dir().join("hark-preview");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let token = format!(
        "p-{}-{}",
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
    let _ = std::fs::remove_file(&out);

    let ok = Command::new("pdftoppm")
        .args([
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
        .arg(&prefix)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

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
