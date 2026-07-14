//! Drag-and-drop source helpers for result rows and the media preview.
//!
//! Offers real filesystem paths (`GdkFileList` / `file://`) so Telegram,
//! Nautilus, browsers, etc. receive a file — not pixels.
//!
//! Critical Wayland/layer-shell detail: while a drag is active we must
//! suppress Blink's auto-hide-on-focus-loss, otherwise the window dies
//! mid-drag and the session is cancelled.

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
}

impl DragSession {
    pub fn new(ignore_focus_loss: Rc<Cell<bool>>) -> Self {
        Self {
            ignore_focus_loss,
            active: Rc::new(Cell::new(false)),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.get()
    }
}

/// Attach a file-URI drag source to any widget (result row, preview, …).
///
/// Short clicks still activate the row; GTK only starts the drag after the
/// movement threshold.
pub fn attach_path_drag(widget: &impl IsA<Widget>, path: &Path, session: &DragSession) {
    if !path.exists() {
        return;
    }

    let path = path.to_path_buf();
    let source = DragSource::new();
    source.set_actions(DragAction::COPY);
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
            session.active.set(true);
            session.ignore_focus_loss.set(true);
            set_drag_icon(src, &path);
        });
    }

    {
        let session = session.clone();
        source.connect_drag_end(move |_src, _drag, _delete| {
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
        source.set_actions(DragAction::COPY);
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
                session.active.set(true);
                session.ignore_focus_loss.set(true);
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

fn end_session(session: &DragSession) {
    session.active.set(false);
    // Keep ignore_focus_loss a beat longer so any already-queued 80ms hide
    // timer still sees the guard, and so focus settling after drop does not
    // instantly kill the window. Then clear the guard; if Blink is no longer
    // the active window (user dropped into another app), hide like a normal
    // focus-loss would have.
    let ignore = session.ignore_focus_loss.clone();
    ignore.set(true);
    glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
        ignore.set(false);
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

fn content_for_path(path: &Path) -> ContentProvider {
    let file = gio::File::for_path(path);
    // GdkFileList is the modern, multi-app-friendly payload on Wayland.
    let list = FileList::from_array(&[file]);
    ContentProvider::for_value(&list.to_value())
}

fn set_drag_icon(source: &DragSource, path: &Path) {
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
