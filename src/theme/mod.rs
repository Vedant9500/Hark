mod css;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{CssProvider, STYLE_PROVIDER_PRIORITY_APPLICATION};
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Debounce window for Settings appearance steppers (opacity/radius/font/…).
const UI_RELOAD_DEBOUNCE: Duration = Duration::from_millis(60);

#[derive(Debug, Deserialize)]
struct SchemeFile {
    #[serde(default)]
    colours: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub surface_container: String,
    pub surface_container_high: String,
    pub on_surface: String,
    pub on_surface_variant: String,
    pub outline_variant: String,
    pub primary: String,
}

impl Theme {
    pub fn fallback() -> Self {
        Self {
            surface_container: "#24283b".into(),
            surface_container_high: "#2a3048".into(),
            on_surface: "#c0caf5".into(),
            on_surface_variant: "#565f89".into(),
            outline_variant: "#414868".into(),
            primary: "#7aa2f7".into(),
        }
    }

    pub fn load() -> Self {
        let path = scheme_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::fallback();
        };
        let Ok(scheme) = serde_json::from_str::<SchemeFile>(&raw) else {
            return Self::fallback();
        };

        let c = |key: &str, fallback: &str| -> String {
            scheme
                .colours
                .get(key)
                .map(|v| normalize_hex(v))
                .unwrap_or_else(|| fallback.to_string())
        };

        let fb = Self::fallback();
        Self {
            surface_container: c("surfaceContainer", &fb.surface_container),
            surface_container_high: c("surfaceContainerHigh", &fb.surface_container_high),
            on_surface: c("onSurface", &fb.on_surface),
            on_surface_variant: c("onSurfaceVariant", &fb.on_surface_variant),
            outline_variant: c("outlineVariant", &fb.outline_variant),
            primary: c("primary", &fb.primary),
        }
    }

    pub fn to_css(&self, ui: &crate::config::UiThemeConfig) -> String {
        css::render(self, ui)
    }
}

pub struct ThemeManager {
    provider: CssProvider,
    config: std::sync::Arc<crate::config::ConfigStore>,
    /// Last loaded scheme colours — reused on UI-only reloads (skip scheme.json).
    cached_theme: RefCell<Theme>,
    /// Pending debounced UI CSS inject (Settings steppers).
    reload_debounce: RefCell<Option<glib::SourceId>>,
    /// Bumps when a full scheme re-apply runs so stale debounce callbacks no-op.
    apply_gen: Cell<u64>,
    _monitor: RefCell<Option<gio::FileMonitor>>,
}

impl ThemeManager {
    pub fn new(config: std::sync::Arc<crate::config::ConfigStore>) -> Rc<Self> {
        let provider = CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &provider,
            STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let mgr = Rc::new(Self {
            provider,
            config,
            cached_theme: RefCell::new(Theme::fallback()),
            reload_debounce: RefCell::new(None),
            apply_gen: Cell::new(0),
            _monitor: RefCell::new(None),
        });
        mgr.apply();
        mgr.watch();
        mgr
    }

    /// Re-read scheme.json, update cache, inject CSS (startup + file monitor).
    pub fn apply(&self) {
        self.cancel_reload_debounce();
        let theme = Theme::load();
        let ui = self.config.snapshot().ui.clone();
        self.provider.load_from_string(&theme.to_css(&ui));
        // Row highlight spans read the accent from a thread-local (rows have
        // no theme access at bind time) — keep it in lockstep with the scheme.
        crate::ui::rows::set_highlight_accent(theme.primary.clone());
        *self.cached_theme.borrow_mut() = theme;
        self.apply_gen.set(self.apply_gen.get().wrapping_add(1));
    }

    /// UI-only refresh: reuse cached scheme colours, re-read config UI knobs.
    /// Debounced so rapid appearance steppers share one CSS inject.
    pub fn reload(self: &Rc<Self>) {
        if let Some(id) = self.reload_debounce.borrow_mut().take() {
            id.remove();
        }
        let this = self.clone();
        let gen = self.apply_gen.get();
        let id = glib::timeout_add_local_once(UI_RELOAD_DEBOUNCE, move || {
            *this.reload_debounce.borrow_mut() = None;
            // Scheme monitor ran apply() while we waited — drop this UI inject.
            if this.apply_gen.get() != gen {
                return;
            }
            this.apply_ui_only();
        });
        *self.reload_debounce.borrow_mut() = Some(id);
    }

    /// Inject CSS from cached theme + current UI config (no scheme disk I/O).
    fn apply_ui_only(&self) {
        let ui = self.config.snapshot().ui.clone();
        let css = self.cached_theme.borrow().to_css(&ui);
        self.provider.load_from_string(&css);
    }

    fn cancel_reload_debounce(&self) {
        if let Some(id) = self.reload_debounce.borrow_mut().take() {
            id.remove();
        }
    }

    fn watch(self: &Rc<Self>) {
        let path = scheme_path();
        // Watch parent dir so atomic renames are caught.
        let watch_path = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| path.clone());

        let file = gio::File::for_path(&watch_path);
        let Ok(monitor) =
            file.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            // Fallback when FileMonitor fails: apply once now. Do **not** poll every
            // few seconds — that keeps the CPU awake for battery life. User can
            // restart Hark or toggle the panel to pick up a new scheme.json.
            self.apply();
            return;
        };

        let this = self.clone();
        let scheme_name = path.file_name().map(|s| s.to_os_string());
        monitor.connect_changed(move |_mon, file, _other, event| {
            use gio::FileMonitorEvent::*;
            match event {
                ChangesDoneHint | Created | Changed | Renamed | MovedIn | AttributeChanged => {
                    if let Some(expected) = &scheme_name {
                        if let Some(name) = file.basename() {
                            if &name != expected {
                                // Also accept any scheme.json path under the dir
                                if name.to_string_lossy() != "scheme.json" {
                                    return;
                                }
                            }
                        }
                    }
                    let this = this.clone();
                    // Debounce: caelestia may write multiple times
                    glib::timeout_add_local_once(Duration::from_millis(80), move || {
                        this.apply();
                    });
                }
                _ => {}
            }
        });

        *self._monitor.borrow_mut() = Some(monitor);
    }
}

fn scheme_path() -> PathBuf {
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state).join("caelestia/scheme.json");
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/state/caelestia/scheme.json")
}

fn normalize_hex(v: &str) -> String {
    let v = v.trim().trim_start_matches('#');
    format!("#{v}")
}
