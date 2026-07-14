use crate::providers::{Action, ResultKind, SearchResult};
use gtk::gdk::{self, Texture};
use gtk::gdk_pixbuf::Pixbuf;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, ContentFit, Image, Label, Orientation, Picture, Separator, Stack,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

    fn of(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|m| Self::from_meta(&m))
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
        matches!(self, MediaKind::Image | MediaKind::Video | MediaKind::Audio)
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
}

impl PreviewPanel {
    pub fn new() -> Self {
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.add_css_class("blink-preview");
        root.set_size_request(PREVIEW_WIDTH, -1);
        root.set_width_request(PREVIEW_WIDTH);
        root.set_hexpand(false);
        root.set_vexpand(true);
        root.set_halign(Align::Fill);
        // Hidden until a media result is selected.
        root.set_visible(false);

        let sep = gtk::Separator::new(Orientation::Vertical);
        sep.add_css_class("blink-preview-sep");
        sep.set_visible(false);

        let stack = Stack::new();
        stack.add_css_class("blink-preview-stack");
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(90);

        // Generic icon + metadata (video / audio)
        let icon_view = GtkBox::new(Orientation::Vertical, 10);
        icon_view.add_css_class("blink-preview-body");
        icon_view.set_hexpand(true);
        icon_view.set_vexpand(true);
        icon_view.set_halign(Align::Fill);
        icon_view.set_valign(Align::Center);

        let icon = Image::from_icon_name("text-x-generic");
        icon.add_css_class("blink-preview-icon");
        icon.set_pixel_size(72);
        icon.set_halign(Align::Center);

        let icon_type = Label::new(None);
        icon_type.add_css_class("blink-preview-badge");
        icon_type.set_halign(Align::Center);

        let icon_title = Label::new(None);
        icon_title.add_css_class("blink-preview-title");
        icon_title.set_halign(Align::Center);
        icon_title.set_wrap(true);
        icon_title.set_max_width_chars(28);
        icon_title.set_justify(gtk::Justification::Center);
        icon_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        icon_title.set_lines(2);

        let icon_sub = Label::new(None);
        icon_sub.add_css_class("blink-preview-sub");
        icon_sub.set_halign(Align::Center);
        icon_sub.set_wrap(true);
        icon_sub.set_max_width_chars(30);
        icon_sub.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        icon_sub.set_justify(gtk::Justification::Center);

        let icon_meta = Label::new(None);
        icon_meta.add_css_class("blink-preview-meta");
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
        image_view.add_css_class("blink-preview-body");
        image_view.set_hexpand(true);
        image_view.set_vexpand(true);

        let picture = Picture::new();
        picture.add_css_class("blink-preview-picture");
        picture.set_content_fit(ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_vexpand(false);
        picture.set_halign(Align::Fill);
        picture.set_valign(Align::Center);
        // Fixed 4:3 frame so previews stay consistent.
        picture.set_size_request(IMAGE_FRAME_WIDTH, IMAGE_FRAME_HEIGHT);

        let image_title = Label::new(None);
        image_title.add_css_class("blink-preview-title");
        image_title.set_halign(Align::Start);
        image_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        image_title.set_xalign(0.0);

        let image_dims = Label::new(None);
        image_dims.add_css_class("blink-preview-meta");
        image_dims.set_halign(Align::Start);
        image_dims.set_xalign(0.0);

        let image_meta = Label::new(None);
        image_meta.add_css_class("blink-preview-meta");
        image_meta.set_halign(Align::Start);
        image_meta.set_wrap(true);
        image_meta.set_xalign(0.0);

        let meta_block = GtkBox::new(Orientation::Vertical, 2);
        meta_block.add_css_class("blink-preview-meta-block");
        meta_block.append(&image_title);
        meta_block.append(&image_dims);
        meta_block.append(&image_meta);

        image_view.append(&picture);
        image_view.append(&Separator::new(Orientation::Horizontal));
        image_view.append(&meta_block);
        stack.add_named(&image_view, Some("image"));

        stack.set_visible_child_name("icon");
        root.append(&stack);

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
        }
    }

    pub fn widget(&self) -> &GtkBox {
        &self.root
    }

    pub fn separator(&self) -> &gtk::Separator {
        &self.sep
    }

    fn set_panel_visible(&self, visible: bool) {
        self.root.set_visible(visible);
        self.sep.set_visible(visible);
    }

    pub fn is_visible(&self) -> bool {
        self.root.is_visible()
    }

    pub fn clear(&self) {
        self.cancel_debounce();
        self.gen.set(self.gen.get().wrapping_add(1));
        *self.last_path.borrow_mut() = None;
        *self.inflight.borrow_mut() = None;
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

        if path.is_dir() {
            self.clear();
            return;
        }

        let media = media_kind(&path);
        if !media.is_previewable() {
            self.clear();
            return;
        }

        let meta = file_meta(&path);
        self.set_panel_visible(true);

        if media == MediaKind::Image {
            self.queue_image_load(path, item.title.clone(), meta);
        } else {
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
                _ => meta,
            };
            self.show_icon_preview(icon_name, badge, &item.title, &item.subtitle, Some(&detail));
        }
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

    fn insert_cache(cache: &RefCell<HashMap<PathBuf, CachedTexture>>, order: &RefCell<Vec<PathBuf>>, path: PathBuf, entry: CachedTexture) {
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
    fn queue_image_load(&self, path: PathBuf, title: String, meta: String) {
        let Some(fp) = FileFp::of(&path) else {
            self.show_image_chrome(&title, &meta, "Could not load preview");
            self.picture.set_paintable(Option::<&gdk::Paintable>::None);
            *self.last_path.borrow_mut() = Some(path);
            return;
        };

        // Already showing this exact file revision — nothing to do.
        if self.last_path.borrow().as_ref() == Some(&path)
            && self.picture.paintable().is_some()
            && self
                .cache
                .borrow()
                .get(&path)
                .is_some_and(|c| c.fp == fp)
        {
            return;
        }

        // Cache hit (path + mtime/size): paint immediately.
        if self.apply_cached(&path, fp, &title, &meta) {
            self.cancel_debounce();
            return;
        }

        if fp.len > MAX_IMAGE_BYTES {
            self.cancel_debounce();
            self.gen.set(self.gen.get().wrapping_add(1));
            *self.last_path.borrow_mut() = Some(path);
            *self.inflight.borrow_mut() = None;
            self.show_image_chrome(&title, &meta, "Image too large to preview");
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
            // Thumb path resolve + decode stay off the GTK main loop.
            let decoded = match freedesktop_thumbnail(&path_worker) {
                Some(thumb) => decode_thumb_or_scaled(&thumb, &path_worker),
                None => decode_image_scaled(&path_worker),
            };
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

            let still_current = gen_cell2.get() == req_gen
                && last_path2.borrow().as_ref() == Some(&req_path);

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

/// Prefer FreeDesktop thumb; fall back to scaled original (single open each).
fn decode_thumb_or_scaled(thumb: &Path, original: &Path) -> Option<DecodedPixels> {
    if let Some(mut px) = pixbuf_to_pixels(&Pixbuf::from_file(thumb).ok()?) {
        // Thumb dims are not native image dims — label as thumbnail.
        px.dims_label = format!("Thumbnail · {} × {}", px.width, px.height);
        return Some(px);
    }
    decode_image_scaled(original)
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
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" | "tif" | "tiff"
        | "heic" | "heif" | "avif" | "jxl" => MediaKind::Image,
        "mp4" | "webm" | "mkv" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpeg" | "mpg" => {
            MediaKind::Video
        }
        "mp3" | "flac" | "ogg" | "wav" | "m4a" | "aac" | "opus" | "wma" | "aiff" => {
            MediaKind::Audio
        }
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "epub" | "xls" | "xlsx"
        | "ppt" | "pptx" | "csv" => MediaKind::Document,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => MediaKind::Archive,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "c" | "h" | "cpp" | "hpp" | "java"
        | "kt" | "swift" | "rb" | "php" | "sh" | "bash" | "zsh" | "toml" | "yaml" | "yml"
        | "json" | "xml" | "html" | "css" | "scss" | "vue" | "svelte" | "lua" | "sql" => {
            MediaKind::Code
        }
        _ => MediaKind::Other,
    }
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

fn file_meta(path: &Path) -> String {
    let mut parts = Vec::new();
    if let Ok(meta) = std::fs::metadata(path) {
        parts.push(format_size(meta.len()));
        if let Ok(modified) = meta.modified() {
            parts.push(format_modified(modified));
        }
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

/// FreeDesktop thumbnail path (`~/.cache/thumbnails/{large,normal}/md5(uri).png`).
fn freedesktop_thumbnail(path: &Path) -> Option<PathBuf> {
    let canon = path.canonicalize().ok()?;
    // file:// URI — percent-encode non-ascii is overkill for local paths here
    let uri = format!("file://{}", canon.display());
    let digest = md5_hex(uri.as_bytes());
    let base = dirs::home_dir()?.join(".cache/thumbnails");
    for size in ["large", "normal", "x-large"] {
        let p = base.join(size).join(format!("{digest}.png"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn md5_hex(message: &[u8]) -> String {
    let d = md5_bytes(message);
    let mut s = String::with_capacity(32);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Compact MD5 for FreeDesktop thumbnail names.
fn md5_bytes(message: &[u8]) -> [u8; 16] {
    fn f(x: u32, y: u32, z: u32) -> u32 {
        (x & y) | (!x & z)
    }
    fn g(x: u32, y: u32, z: u32) -> u32 {
        (x & z) | (y & !z)
    }
    fn h(x: u32, y: u32, z: u32) -> u32 {
        x ^ y ^ z
    }
    fn i(x: u32, y: u32, z: u32) -> u32 {
        y ^ (x | !z)
    }

    let mut msg = message.to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let s: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (j, item) in m.iter_mut().enumerate() {
            let o = j * 4;
            *item = u32::from_le_bytes([chunk[o], chunk[o + 1], chunk[o + 2], chunk[o + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for j in 0..64 {
            let (fval, gval) = if j < 16 {
                (f(b, c, d), j)
            } else if j < 32 {
                (g(b, c, d), (5 * j + 1) % 16)
            } else if j < 48 {
                (h(b, c, d), (3 * j + 5) % 16)
            } else {
                (i(b, c, d), (7 * j) % 16)
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(fval)
                    .wrapping_add(k[j])
                    .wrapping_add(m[gval])
                    .rotate_left(s[j]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}
