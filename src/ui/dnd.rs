//! Drag-and-drop source helpers for result rows and the media preview.
//!
//! Offers real filesystem paths (`GdkFileList` / `GFile` / `text/uri-list`) so
//! Telegram, Nautilus, browsers, etc. receive a file — not pixels.
//!
//! Critical Wayland/layer-shell details:
//! - suppress Blink's auto-hide-on-focus-loss while a drag is active
//! - release exclusive keyboard grab during drag so drop targets can focus
//! - offer COPY|MOVE|ASK (Hyprland often prefers MOVE; we never delete)

use super::thumbnails::freedesktop_thumbnail;
use gtk::gdk::{self, ContentProvider, DragAction, FileList};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{DragSource, Widget};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Shared drag session flags used by the launcher window.
#[derive(Clone)]
pub struct DragSession {
    /// Suppress auto-hide while true (focus leaves Blink during drop).
    pub ignore_focus_loss: Rc<Cell<bool>>,
    /// True between drag-begin and drag-end/cancel — skip list rebuilds.
    pub active: Rc<Cell<bool>>,
    /// Layer-shell window that needs keyboard-mode toggled during drag.
    window: Rc<RefCell<Option<gtk::ApplicationWindow>>>,
}

impl DragSession {
    pub fn new(ignore_focus_loss: Rc<Cell<bool>>) -> Self {
        Self {
            ignore_focus_loss,
            active: Rc::new(Cell::new(false)),
            window: Rc::new(RefCell::new(None)),
        }
    }

    pub fn bind_window(&self, window: &gtk::ApplicationWindow) {
        *self.window.borrow_mut() = Some(window.clone());
    }

    pub fn is_active(&self) -> bool {
        self.active.get()
    }
}

/// Attach a file-URI drag source to any widget (result row, preview, …).
///
/// Short clicks still activate the row; GTK only starts the drag after the
/// movement threshold.
#[allow(dead_code)]
pub fn attach_path_drag(widget: &impl IsA<Widget>, path: &Path, session: &DragSession) {
    if !path.exists() {
        return;
    }

    let path = path.to_path_buf();
    let source = DragSource::new();
    // Hyprland often advertises MOVE as preferred; COPY-only sources get
    // rejected by some targets. We never delete on MOVE.
    source.set_actions(DragAction::COPY | DragAction::MOVE | DragAction::ASK);
    // Capture so the drag gesture wins over ListBox single-click activate
    // once the pointer moves past the drag threshold.
    source.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let path = path.clone();
        source.connect_prepare(move |_src, _x, _y| Some(content_for_path(&path)));
    }

    {
        let session = session.clone();
        let path = path.clone();
        source.connect_drag_begin(move |src, _drag| {
            begin_session(&session);
            set_drag_icon(src, &path);
        });
    }

    {
        let session = session.clone();
        source.connect_drag_end(move |_src, _drag, _delete| {
            // Never honor delete_data — we only ever copy paths out.
            end_session(&session);
        });
    }

    {
        let session = session.clone();
        source.connect_drag_cancel(move |_src, _drag, _reason| {
            end_session(&session);
            false
        });
    }

    widget.add_controller(source);
}

/// Live path binding for a long-lived widget (preview panel).
///
/// Controllers are hard to remove in GTK4, so one `DragSource` stays attached
/// and `set_path` swaps the payload.
#[derive(Clone)]
pub struct PathDragBinding {
    path: Rc<RefCell<Option<PathBuf>>>,
    attached: Rc<Cell<bool>>,
    session: DragSession,
}

impl PathDragBinding {
    pub fn new(session: DragSession) -> Self {
        Self {
            path: Rc::new(RefCell::new(None)),
            attached: Rc::new(Cell::new(false)),
            session,
        }
    }

    pub fn attach(&self, widget: &impl IsA<Widget>) {
        if self.attached.get() {
            return;
        }
        self.attached.set(true);

        let source = DragSource::new();
        source.set_actions(DragAction::COPY | DragAction::MOVE | DragAction::ASK);
        source.set_propagation_phase(gtk::PropagationPhase::Capture);

        {
            let path = self.path.clone();
            source.connect_prepare(move |_src, _x, _y| {
                let guard = path.borrow();
                guard.as_ref().map(|p| content_for_path(p))
            });
        }

        {
            let session = self.session.clone();
            let path = self.path.clone();
            source.connect_drag_begin(move |src, _drag| {
                begin_session(&session);
                if let Some(p) = path.borrow().as_ref() {
                    set_drag_icon(src, p);
                }
            });
        }

        {
            let session = self.session.clone();
            source.connect_drag_end(move |_src, _drag, _delete| {
                end_session(&session);
            });
        }

        {
            let session = self.session.clone();
            source.connect_drag_cancel(move |_src, _drag, _reason| {
                end_session(&session);
                false
            });
        }

        widget.add_controller(source);
    }

    pub fn set_path(&self, path: Option<PathBuf>) {
        match path {
            Some(p) if p.exists() => *self.path.borrow_mut() = Some(p),
            _ => *self.path.borrow_mut() = None,
        }
    }
}

fn begin_session(session: &DragSession) {
    session.active.set(true);
    session.ignore_focus_loss.set(true);
    // Exclusive keyboard mode can starve drop targets under layer-shell.
    // OnDemand keeps Blink usable for Escape but lets other surfaces focus.
    set_layer_keyboard_ondemand(session);
}

fn end_session(session: &DragSession) {
    session.active.set(false);
    // Keep ignore_focus_loss a beat longer so any already-queued hide timer
    // still sees the guard, and so focus settling after drop does not
    // instantly kill the window / cancel a late data transfer.
    //
    // Dolphin/Nautilus "Copy here / Move here" often finish the file transfer
    // *after* drag-end; hiding the layer-shell surface too early cancels the
    // Wayland data source and the drop silently does nothing.
    let ignore = session.ignore_focus_loss.clone();
    ignore.set(true);
    // Stay OnDemand until the transfer window closes — Exclusive + hide races
    // with portal/file-manager drop completion under Hyprland.
    set_layer_keyboard_ondemand(session);

    let session = session.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
        ignore.set(false);
        set_layer_keyboard_exclusive(&session);
        // gio::Application::default is the running app (not gtk::Application::default,
        // which would construct a brand-new empty Application).
        let Some(app) = gio::Application::default() else {
            return;
        };
        let Ok(app) = app.downcast::<gtk::Application>() else {
            return;
        };
        if app.active_window().is_some() {
            // Still focused on Blink — keep open.
            return;
        }
        // Focus is elsewhere after the drop: hide like normal focus-loss.
        for w in app.windows() {
            if w.is_visible() && !w.is_active() {
                w.set_visible(false);
            }
        }
    });
}

fn set_layer_keyboard_ondemand(session: &DragSession) {
    #[cfg(feature = "layer-shell")]
    {
        use gtk4_layer_shell::{KeyboardMode, LayerShell};
        if let Some(window) = session.window.borrow().as_ref() {
            if gtk4_layer_shell::is_supported() && window.is_layer_window() {
                window.set_keyboard_mode(KeyboardMode::OnDemand);
            }
        }
    }
    #[cfg(not(feature = "layer-shell"))]
    {
        let _ = session;
    }
}

fn set_layer_keyboard_exclusive(session: &DragSession) {
    #[cfg(feature = "layer-shell")]
    {
        use gtk4_layer_shell::{KeyboardMode, LayerShell};
        if let Some(window) = session.window.borrow().as_ref() {
            if gtk4_layer_shell::is_supported() && window.is_layer_window() {
                window.set_keyboard_mode(KeyboardMode::Exclusive);
            }
        }
    }
    #[cfg(not(feature = "layer-shell"))]
    {
        let _ = session;
    }
}

/// Offer the formats most drop targets expect on Wayland:
/// - `GdkFileList` (GTK4 native + portals)
/// - single `GFile`
/// - classic `text/uri-list` (Qt / browsers / many non-GTK apps)
fn content_for_path(path: &Path) -> ContentProvider {
    let file = gio::File::for_path(path);
    let uri = file.uri();

    // RFC 2483: one URI per line, CRLF-terminated, including a trailing blank line.
    let uri_list = format!("{uri}\r\n");
    let uri_bytes = glib::Bytes::from_owned(uri_list.into_bytes());

    let list = FileList::from_array(&[file.clone()]);

    ContentProvider::new_union(&[
        ContentProvider::for_value(&list.to_value()),
        ContentProvider::for_value(&file.to_value()),
        ContentProvider::for_bytes("text/uri-list", &uri_bytes),
    ])
}

fn set_drag_icon(source: &DragSource, path: &Path) {
    // Prefer a FreeDesktop thumbnail for images (cheap, already on disk).
    // Fall back to a themed mime icon so non-image files still look right.
    if let Some(thumb) = drag_thumbnail_icon(path) {
        // Hotspot near the top-left of the thumb so the pointer feels attached.
        source.set_icon(Some(&thumb), 12, 12);
        return;
    }

    let display = source
        .widget()
        .map(|w| w.display())
        .or_else(gdk::Display::default);

    let Some(display) = display else {
        return;
    };

    let theme = gtk::IconTheme::for_display(&display);
    let icon_name = drag_icon_name(path);
    let paintable = theme.lookup_icon(
        icon_name,
        &[],
        48,
        1,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::empty(),
    );

    source.set_icon(Some(&paintable), 24, 24);
}

/// Load a small drag icon from the FreeDesktop thumbnail cache when present.
///
/// Sync and main-thread, but thumbs are tiny PNGs already decoded by the
/// desktop — no full-image decode and no coupling to the preview LRU (row
/// drags often happen without a preview texture).
///
/// Memoizes the last path → texture so repeated drag-begin on the same row
/// skips canonicalize + multi-slot `is_file` probes.
fn drag_thumbnail_icon(path: &Path) -> Option<gdk::Texture> {
    if path.is_dir() || !is_image_path(path) {
        return None;
    }

    thread_local! {
        static MEMO: RefCell<Option<(PathBuf, Option<gdk::Texture>)>> =
            RefCell::new(None);
    }

    MEMO.with(|slot| {
        if let Some((prev, tex)) = slot.borrow().as_ref() {
            if prev.as_path() == path {
                return tex.clone();
            }
        }
        let tex = freedesktop_thumbnail(path)
            .and_then(|thumb| gdk::Texture::from_filename(&thumb).ok());
        *slot.borrow_mut() = Some((path.to_path_buf(), tex.clone()));
        tex
    })
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "svg"
            | "avif"
            | "jxl"
            | "heic"
            | "heif"
            | "tif"
            | "tiff"
            | "ico"
    )
}

fn drag_icon_name(path: &Path) -> &'static str {
    if path.is_dir() {
        return "folder";
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "avif" | "jxl" | "heic"
        | "tif" | "tiff" => "image-x-generic",
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" => "video-x-generic",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "opus" | "aac" => "audio-x-generic",
        "pdf" => "application-pdf",
        "zip" | "tar" | "gz" | "xz" | "7z" | "rar" | "bz2" => "package-x-generic",
        "desktop" => "application-x-executable",
        "rs" | "py" | "js" | "ts" | "go" | "c" | "cpp" | "h" | "java" | "sh" => "text-x-script",
        "txt" | "md" | "rst" | "log" => "text-x-generic",
        _ => "text-x-generic",
    }
}
