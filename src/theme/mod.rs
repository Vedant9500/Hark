mod css;
mod impasto;
mod source;
mod system;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{CssProvider, STYLE_PROVIDER_PRIORITY_APPLICATION};
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

/// Debounce window for Settings appearance steppers (opacity/radius/font/…).
const UI_RELOAD_DEBOUNCE: Duration = Duration::from_millis(60);

/// Metadata of every file that can change the active palette, cheap enough
/// to rebuild on each launcher show.
type ThemeFingerprint = Vec<(PathBuf, u64, Option<SystemTime>)>;

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
        Self::try_load().unwrap_or_else(Self::fallback)
    }

    /// `None` when the active rice has no readable palette (missing file,
    /// transient mid-write parse, unsupported format). `apply` keeps the last
    /// good theme instead of flashing the built-in default.
    fn try_load() -> Option<Self> {
        match source::active() {
            source::Rice::Impasto => impasto::load(),
            source::Rice::Caelestia => Self::load_caelestia(),
        }
    }

    fn load_caelestia() -> Option<Self> {
        let path = source::caelestia_scheme()?;
        let raw = std::fs::read_to_string(&path).ok()?;
        let scheme = serde_json::from_str::<SchemeFile>(&raw).ok()?;

        let c = |key: &str, fallback: &str| -> String {
            scheme
                .colours
                .get(key)
                .map(|v| sanitize_hex(v))
                .unwrap_or_else(|| fallback.to_string())
        };

        let fb = Self::fallback();
        Some(Self {
            surface_container: c("surfaceContainer", &fb.surface_container),
            surface_container_high: c("surfaceContainerHigh", &fb.surface_container_high),
            on_surface: c("onSurface", &fb.on_surface),
            on_surface_variant: c("onSurfaceVariant", &fb.on_surface_variant),
            outline_variant: c("outlineVariant", &fb.outline_variant),
            primary: c("primary", &fb.primary),
        })
    }

    pub fn to_css(&self, ui: &crate::config::UiThemeConfig) -> String {
        css::render(self, ui)
    }
}

pub struct ThemeManager {
    provider: CssProvider,
    config: std::sync::Arc<crate::config::ConfigStore>,
    /// Last loaded palette colours — reused on UI-only reloads (skip disk I/O).
    cached_theme: RefCell<Theme>,
    /// System transparency/blur inputs the shell style is gated on.
    system: Cell<system::SystemStyle>,
    /// Last time the blur option was polled (`hyprctl getoption`).
    last_system_check: Cell<Option<Instant>>,
    /// Fingerprint of the active rice's palette files. The show path
    /// compares this so a missed/dead monitor cannot leave stale colours.
    theme_fp: RefCell<Option<ThemeFingerprint>>,
    /// Pending debounced UI CSS inject (Settings steppers).
    reload_debounce: RefCell<Option<glib::SourceId>>,
    /// #29: single pending palette-change debounce (monitor events coalesce).
    scheme_debounce: RefCell<Option<glib::SourceId>>,
    /// Bumps when a full scheme re-apply runs so stale debounce callbacks no-op.
    apply_gen: Cell<u64>,
    /// Palette/config directories and their monitors. Rebuilt incrementally by
    /// `watch` so a directory created after startup is picked up on the next
    /// show.
    _monitors: RefCell<Vec<(PathBuf, gio::FileMonitor)>>,
}

impl ThemeManager {
    pub fn new(config: std::sync::Arc<crate::config::ConfigStore>) -> Rc<Self> {
        let provider = CssProvider::new();
        // Headless (tests/daemon without display) must not panic: skip the
        // global install when no display is available. Previous code used
        // `expect("display")`, crashing the process outside a GTK session.
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let mgr = Rc::new(Self {
            provider,
            config,
            cached_theme: RefCell::new(Theme::fallback()),
            system: Cell::new(system::SystemStyle::default()),
            last_system_check: Cell::new(None),
            theme_fp: RefCell::new(None),
            reload_debounce: RefCell::new(None),
            scheme_debounce: RefCell::new(None),
            apply_gen: Cell::new(0),
            _monitors: RefCell::new(Vec::new()),
        });
        mgr.apply();
        mgr.watch();
        // Icon theme installs/changes must drop the memoized row-icon
        // resolutions (audit P3) — otherwise new themes never take effect.
        crate::ui::rows::ensure_icon_theme_watcher();
        mgr
    }

    /// Re-read the active palette + system appearance, update cache, inject CSS
    /// (startup + file monitor + show-path refresh).
    pub fn apply(&self) {
        self.cancel_reload_debounce();
        self.cancel_scheme_debounce();
        // A palette file caught mid-write must not flash the built-in default
        // over a good theme; the next monitor event applies the finished file.
        let theme = Theme::try_load().unwrap_or_else(|| self.cached_theme.borrow().clone());
        let style = system::load();
        self.system.set(style);
        self.last_system_check.set(Some(Instant::now()));
        let ui = self.config.snapshot().ui.clone();
        let opacity = system::effective_opacity(ui.opacity, style);
        self.provider
            .load_from_string(&css::render_with_opacity(&theme, &ui, opacity));
        system::sync_blur(opacity);
        // Row highlight spans read the accent from a thread-local (rows have
        // no theme access at bind time) — keep it in lockstep with the scheme.
        crate::ui::rows::set_highlight_accent(sanitize_hex(&theme.primary));
        *self.theme_fp.borrow_mut() = Some(theme_fingerprint());
        *self.cached_theme.borrow_mut() = theme;
        self.apply_gen.set(self.apply_gen.get().wrapping_add(1));
    }

    /// Show-path safety net: re-read the palette when its fingerprint moved
    /// (covers a monitor that failed because the rice directory did not exist
    /// yet), and periodically poll the compositor/desktop appearance.
    pub fn refresh_if_needed(self: &Rc<Self>) {
        let fingerprint = theme_fingerprint();
        let moved = {
            let last = self.theme_fp.borrow();
            last.as_ref() != Some(&fingerprint)
        };
        if moved {
            self.apply();
            // `watch` only adds directories it is not monitoring yet, so a
            // rice that appeared after startup gets its watcher here.
            self.watch();
            return;
        }

        let due = self
            .last_system_check
            .get()
            .map_or(true, |at| at.elapsed() >= system::SYSTEM_TTL);
        if due {
            self.last_system_check.set(Some(Instant::now()));
            let style = system::load();
            if style != self.system.get() {
                self.system.set(style);
                self.apply_ui_only();
                return;
            }
        }

        // A compositor reload drops runtime layer rules; re-assert on a slow
        // cadence without paying a fork on every hotkey press.
        let opacity = self
            .config
            .with(|c| system::effective_opacity(c.ui.opacity, self.system.get()));
        system::sync_blur(opacity);
    }

    /// True when the cached theme is light (used to pick a matching
    /// GtkSourceView style scheme so the code preview is theme-aware).
    pub fn is_light(&self) -> bool {
        css::is_light_theme(&self.cached_theme.borrow().surface_container)
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

    /// Inject CSS from cached theme + current UI config (no palette disk I/O).
    fn apply_ui_only(&self) {
        let ui = self.config.snapshot().ui.clone();
        let opacity = system::effective_opacity(ui.opacity, self.system.get());
        let css = css::render_with_opacity(&self.cached_theme.borrow(), &ui, opacity);
        self.provider.load_from_string(&css);
        system::sync_blur(opacity);
    }

    fn cancel_reload_debounce(&self) {
        if let Some(id) = self.reload_debounce.borrow_mut().take() {
            id.remove();
        }
    }

    /// Drop a pending scheme-monitor re-apply (e.g. a manual `apply()` won
    /// the race while the 80ms monitor timer was still queued). No-op when
    /// called from inside the scheme timer itself — it already took the id.
    fn cancel_scheme_debounce(&self) {
        if let Some(id) = self.scheme_debounce.borrow_mut().take() {
            id.remove();
        }
    }

    fn watch(self: &Rc<Self>) {
        // Additive on purpose: `refresh_if_needed` retries after a rice was
        // installed or created later, and existing monitors stay in place.
        let watched: Vec<PathBuf> = self
            ._monitors
            .borrow()
            .iter()
            .map(|(dir, _)| dir.clone())
            .collect();

        for dir in watch_dirs() {
            if watched.contains(&dir) {
                continue;
            }
            let file = gio::File::for_path(&dir);
            let Ok(monitor) =
                file.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            else {
                continue;
            };
            let this = self.clone();
            monitor.connect_changed(move |_mon, file, _other, event| {
                if is_write_event(event) && basename_is_theme_file(file) {
                    schedule_apply(&this);
                }
            });
            self._monitors.borrow_mut().push((dir, monitor));
        }
    }
}

fn watch_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for path in [
        source::caelestia_scheme(),
        source::caelestia_shell(),
        source::impasto_state(),
        source::impasto_dynamic(),
        source::impasto_palettes(),
        source::rice_marker(),
    ]
    .into_iter()
    .flatten()
    {
        let Some(dir) = path.parent().map(|p| p.to_path_buf()) else {
            continue;
        };
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

fn is_write_event(event: gio::FileMonitorEvent) -> bool {
    use gio::FileMonitorEvent::*;
    matches!(
        event,
        ChangesDoneHint | Created | Changed | Renamed | MovedIn | AttributeChanged
    )
}

fn basename_is_theme_file(file: &gio::File) -> bool {
    const THEME_FILES: &[&str] = &[
        "scheme.json",
        "shell.json",
        "state.json",
        "dynamic-colors.json",
        "settings.json",
        "Palettes.qml",
        "current",
    ];
    // Directory monitors can report events without a child name; those are
    // safe to treat as a possible write and re-apply.
    file.basename()
        .map(|name| {
            let name = name.to_string_lossy();
            THEME_FILES.iter().any(|expected| name == *expected)
        })
        .unwrap_or(true)
}

/// Coalesce monitor events into one CSS re-apply (#29).
fn schedule_apply(this: &Rc<ThemeManager>) {
    this.cancel_scheme_debounce();
    let cb = this.clone();
    let id = glib::timeout_add_local_once(Duration::from_millis(80), move || {
        *cb.scheme_debounce.borrow_mut() = None;
        cb.apply();
        // A rice installed while Hark was running may have brought new
        // palette directories with it; pick them up without waiting for a show.
        cb.watch();
    });
    *this.scheme_debounce.borrow_mut() = Some(id);
}

fn theme_fingerprint() -> ThemeFingerprint {
    let mut files = Vec::new();
    // Always included: the marker decides which source's files matter.
    if let Some(marker) = source::rice_marker() {
        files.push(marker);
    }
    match source::active() {
        source::Rice::Caelestia => {
            files.extend(source::caelestia_scheme());
            files.extend(source::caelestia_shell());
        }
        source::Rice::Impasto => {
            files.extend(source::impasto_state());
            files.extend(source::impasto_dynamic());
            files.extend(source::impasto_palettes());
        }
    }
    files
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            Some((path, meta.len(), meta.modified().ok()))
        })
        .collect()
}

/// Validate a colour from external input (scheme.json, config). Accepts an
/// optional leading `#` plus 3/4/6/8 ASCII hex digits and returns it with a
/// leading `#`; anything else falls back to `#ffffff`. Non-ASCII or malformed
/// values must not reach byte-slicing consumers (`css::rgba`,
/// `css::is_light_theme`) or Pango markup (row highlight spans).
fn sanitize_hex(v: &str) -> String {
    let h = v.trim().trim_start_matches('#');
    if matches!(h.len(), 3 | 4 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{h}")
    } else {
        "#ffffff".to_string()
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_hex;

    #[test]
    fn valid_lengths_pass_through_with_hash() {
        assert_eq!(sanitize_hex("24283b"), "#24283b");
        assert_eq!(sanitize_hex("#7aa2f7"), "#7aa2f7");
        assert_eq!(sanitize_hex("fff"), "#fff");
        assert_eq!(sanitize_hex("#ffff"), "#ffff");
        assert_eq!(sanitize_hex("#24283bff"), "#24283bff");
    }

    #[test]
    fn malformed_or_non_ascii_falls_back() {
        assert_eq!(sanitize_hex(""), "#ffffff");
        assert_eq!(sanitize_hex("xyz123"), "#ffffff");
        // 2-byte chars: byte length (4) passes a naive len check — must still
        // fall back instead of panicking downstream byte slicing.
        assert_eq!(sanitize_hex("éé"), "#ffffff");
        assert_eq!(sanitize_hex("2428日本"), "#ffffff");
    }
}
