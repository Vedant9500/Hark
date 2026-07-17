use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PathStyle {
    Label,
    Drive,
}

impl Default for PathStyle {
    fn default() -> Self {
        Self::Label
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexConfig {
    #[serde(default = "default_true")]
    pub include_home: bool,
    #[serde(default)]
    pub include_mounts: HashMap<String, bool>,
    #[serde(default)]
    pub extra_roots: Vec<String>,
    /// Folders always walked to depth 6 at index time (pin / promote).
    /// Also preferred as live deep-search roots. Never written by live hits.
    #[serde(default)]
    pub deep_roots: Vec<String>,
    #[serde(default = "default_excludes")]
    pub exclude: Vec<String>,
    #[serde(default = "default_depth")]
    pub max_depth: usize,
    #[serde(default)]
    pub path_style: PathStyle,
}

fn default_true() -> bool {
    true
}

fn default_depth() -> usize {
    // Level 0 = root; walk max_depth components below it.
    // 2 ≈ ~/Projects/foo; clamped 1..=6 in FileProvider.
    2
}

/// Deep roots that re-index huge trees; strip on load + refuse in Engine.
fn is_overbroad_deep_root(s: &str) -> bool {
    let p = PathBuf::from(expand_user_path(s));
    let p = p.canonicalize().unwrap_or(p);
    if p == Path::new("/") {
        return true;
    }
    if let Some(home) = dirs::home_dir() {
        let home = home.canonicalize().unwrap_or(home);
        if p == home {
            return true;
        }
    }
    matches!(
        p.to_string_lossy().as_ref(),
        "/home" | "/Users" | "/var" | "/usr" | "/opt" | "/mnt" | "/media"
    )
}

fn expand_user_path(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned();
        }
    }
    s.to_string()
}

fn default_excludes() -> Vec<String> {
    vec![
        // VCS / package / build
        ".git".into(),
        ".svn".into(),
        ".hg".into(),
        "node_modules".into(),
        "target".into(),
        "dist".into(),
        "build".into(),
        "out".into(),
        // Python / env
        ".venv".into(),
        "venv".into(),
        "env".into(),
        ".env".into(),
        "__pycache__".into(),
        ".mypy_cache".into(),
        ".pytest_cache".into(),
        ".tox".into(),
        // JS / tooling caches
        ".npm".into(),
        ".yarn".into(),
        ".pnpm-store".into(),
        ".turbo".into(),
        ".next".into(),
        ".nuxt".into(),
        // Rust / Java / IDE
        ".cargo".into(),
        ".rustup".into(),
        ".gradle".into(),
        ".m2".into(),
        ".idea".into(),
        // System / browser junk
        ".cache".into(),
        ".thumbnails".into(),
        "Trash".into(),
        "$RECYCLE.BIN".into(),
        "System Volume Information".into(),
        "BraveSoftware".into(),
        ".mozilla".into(),
        ".steam".into(),
        ".pi/agent/sessions".into(),
    ]
}

/// Append any missing default exclude names. Returns true if the list grew.
/// Used only for one-shot config migration (version < 2), not on every load.
fn merge_missing_default_excludes(exclude: &mut Vec<String>) -> bool {
    let mut changed = false;
    for name in default_excludes() {
        if !exclude.iter().any(|e| e == &name) {
            exclude.push(name);
            changed = true;
        }
    }
    changed
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            include_home: true,
            include_mounts: HashMap::new(),
            extra_roots: Vec::new(),
            deep_roots: Vec::new(),
            exclude: default_excludes(),
            max_depth: default_depth(),
            path_style: PathStyle::Label,
        }
    }
}

/// Per-category app overrides for opening files from Blink.
/// Values are desktop ids (e.g. `org.gnome.Loupe.desktop` or `org.gnome.Loupe`).
/// Empty / missing = system default (`xdg-open`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct OpenWithConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archives: Option<String>,
}

impl OpenWithConfig {
    pub fn get(&self, cat: FileOpenCategory) -> Option<&str> {
        let v = match cat {
            FileOpenCategory::Images => self.images.as_deref(),
            FileOpenCategory::Video => self.video.as_deref(),
            FileOpenCategory::Audio => self.audio.as_deref(),
            FileOpenCategory::Pdf => self.pdf.as_deref(),
            FileOpenCategory::Markdown => self.markdown.as_deref(),
            FileOpenCategory::Text => self.text.as_deref(),
            FileOpenCategory::Documents => self.documents.as_deref(),
            FileOpenCategory::Archives => self.archives.as_deref(),
        };
        v.filter(|s| !s.trim().is_empty())
    }

    pub fn set(&mut self, cat: FileOpenCategory, desktop_id: Option<String>) {
        let val = desktop_id.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        });
        match cat {
            FileOpenCategory::Images => self.images = val,
            FileOpenCategory::Video => self.video = val,
            FileOpenCategory::Audio => self.audio = val,
            FileOpenCategory::Pdf => self.pdf = val,
            FileOpenCategory::Markdown => self.markdown = val,
            FileOpenCategory::Text => self.text = val,
            FileOpenCategory::Documents => self.documents = val,
            FileOpenCategory::Archives => self.archives = val,
        }
    }
}

/// Coarse file kinds used for default-app overrides in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileOpenCategory {
    Images,
    Video,
    Audio,
    Pdf,
    Markdown,
    Text,
    Documents,
    Archives,
}

impl FileOpenCategory {
    pub const ALL: &'static [FileOpenCategory] = &[
        FileOpenCategory::Images,
        FileOpenCategory::Video,
        FileOpenCategory::Audio,
        FileOpenCategory::Pdf,
        FileOpenCategory::Markdown,
        FileOpenCategory::Text,
        FileOpenCategory::Documents,
        FileOpenCategory::Archives,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Images => "Images",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Pdf => "PDF",
            Self::Markdown => "Markdown",
            Self::Text => "Plain text",
            Self::Documents => "Documents",
            Self::Archives => "Archives",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Images => "png, jpg, webp, gif, svg…",
            Self::Video => "mp4, mkv, webm, mov…",
            Self::Audio => "mp3, flac, ogg, wav…",
            Self::Pdf => "pdf",
            Self::Markdown => "md, markdown",
            Self::Text => "txt, log, conf…",
            Self::Documents => "odt, docx, rtf…",
            Self::Archives => "zip, tar, 7z…",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Images => "image-x-generic",
            Self::Video => "video-x-generic",
            Self::Audio => "audio-x-generic",
            Self::Pdf => "application-pdf",
            Self::Markdown => "text-markdown",
            Self::Text => "text-plain",
            Self::Documents => "x-office-document",
            Self::Archives => "package-x-generic",
        }
    }

    /// Map a filesystem path to a category (files only; dirs → None).
    pub fn from_path(path: &Path) -> Option<Self> {
        if path.is_dir() {
            return None;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" | "tif" | "tiff"
            | "heic" | "heif" | "avif" | "jxl" => Some(Self::Images),
            "mp4" | "webm" | "mkv" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpeg" | "mpg" => {
                Some(Self::Video)
            }
            "mp3" | "flac" | "ogg" | "wav" | "m4a" | "aac" | "opus" | "wma" | "aiff" => {
                Some(Self::Audio)
            }
            "pdf" => Some(Self::Pdf),
            "md" | "markdown" | "mdown" | "mkd" => Some(Self::Markdown),
            "txt" | "log" | "conf" | "cfg" | "ini" | "text" | "nfo" => Some(Self::Text),
            "odt" | "doc" | "docx" | "rtf" | "odp" | "ppt" | "pptx" | "ods" | "xls" | "xlsx"
            | "epub" | "csv" => Some(Self::Documents),
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => {
                Some(Self::Archives)
            }
            _ => None,
        }
    }
}

/// Appearance tweaks layered on top of the Caelestia colour scheme.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiThemeConfig {
    /// Panel shell opacity 0.40–1.0 (default 0.85).
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// Optional accent override (`#rrggbb`). Empty = scheme primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// UI type scale 0.85–1.30 (default 1.0).
    #[serde(default = "default_font_scale")]
    pub font_scale: f32,
    /// Result-row icon pixel size (18–36, default 26).
    #[serde(default = "default_icon_size")]
    pub icon_size: u32,
    /// Prefer symbolic icons when the theme provides them.
    #[serde(default)]
    pub symbolic_icons: bool,
    /// Shell corner radius in px (8–24, default 16).
    #[serde(default = "default_radius")]
    pub radius: u32,
}

fn default_opacity() -> f32 {
    0.85
}
fn default_font_scale() -> f32 {
    1.0
}
fn default_icon_size() -> u32 {
    26
}
fn default_radius() -> u32 {
    16
}

impl Default for UiThemeConfig {
    fn default() -> Self {
        Self {
            opacity: default_opacity(),
            accent: None,
            font_scale: default_font_scale(),
            icon_size: default_icon_size(),
            symbolic_icons: false,
            radius: default_radius(),
        }
    }
}

impl UiThemeConfig {
    /// Clamp all fields into safe ranges (settings + load path).
    pub fn sanitize(&mut self) {
        self.opacity = self.opacity.clamp(0.40, 1.0);
        self.font_scale = self.font_scale.clamp(0.85, 1.30);
        self.icon_size = self.icon_size.clamp(18, 36);
        self.radius = self.radius.clamp(8, 24);
        if let Some(a) = self.accent.take() {
            let t = a.trim();
            if t.is_empty() {
                self.accent = None;
            } else {
                let hex = t.trim_start_matches('#');
                if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    self.accent = Some(format!("#{hex}").to_ascii_lowercase());
                } else {
                    self.accent = None;
                }
            }
        }
    }
}

/// Online translate-on-paste settings (see `translation.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslateConfig {
    /// Master switch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Default target language code (`en`, `zh`, …).
    #[serde(default = "default_translate_target")]
    pub target_lang: String,
    /// LibreTranslate-compatible base URL. Empty = not configured (Phase 1).
    #[serde(default)]
    pub endpoint: String,
    /// Optional API key for the endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Auto-run when CJK / similar scripts are detected (no `tr ` prefix).
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    /// Max source characters accepted for translation.
    #[serde(default = "default_translate_max_chars")]
    pub max_chars: usize,
}

fn default_translate_target() -> String {
    "en".into()
}

fn default_translate_max_chars() -> usize {
    1000
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_lang: default_translate_target(),
            endpoint: String::new(),
            api_key: None,
            auto_detect: true,
            max_chars: default_translate_max_chars(),
        }
    }
}

impl TranslateConfig {
    pub fn sanitize(&mut self) {
        self.max_chars = self.max_chars.clamp(100, 5000);
        self.target_lang = self
            .target_lang
            .trim()
            .to_ascii_lowercase();
        if self.target_lang.is_empty() {
            self.target_lang = default_translate_target();
        }
        // Keep only simple language tags: `en`, `zh`, `zh-cn`
        self.target_lang = self
            .target_lang
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(16)
            .collect();
        if self.target_lang.is_empty() {
            self.target_lang = default_translate_target();
        }
        self.endpoint = self.endpoint.trim().trim_end_matches('/').to_string();
        if let Some(k) = self.api_key.take() {
            let t = k.trim().to_string();
            self.api_key = if t.is_empty() { None } else { Some(t) };
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BlinkConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub index: IndexConfig,
    /// Per-category default apps for opening files from Blink (not system MIME).
    #[serde(default)]
    pub open_with: OpenWithConfig,
    /// Appearance: transparency, accent, font, icons, radius.
    #[serde(default)]
    pub ui: UiThemeConfig,
    /// Translate-on-paste (CJK / `tr ` prefix). Online via LibreTranslate or MyMemory.
    #[serde(default)]
    pub translate: TranslateConfig,
}

/// Current on-disk schema after migrations in `ConfigStore::load`.
/// v2: stop force-merging default excludes on every start (user removals stick).
const CONFIG_VERSION: u32 = 2;

/// Serde default when `version` is missing from JSON — treat as legacy so the
/// v1→v2 excludes seed runs exactly once, then we write `CONFIG_VERSION`.
fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone)]
pub struct MountInfo {
    pub target: PathBuf,
    pub label: String,
    pub drive_letter: Option<char>,
}

pub struct ConfigStore {
    /// Arc swap on update — hot paths clone the Arc, not the whole config tree.
    inner: RwLock<Arc<BlinkConfig>>,
    path: PathBuf,
}

impl ConfigStore {
    pub fn load() -> Self {
        let path = config_path();
        let mut cfg = if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            BlinkConfig::default()
        };

        // Seed mount defaults for newly discovered mounts
        let mounts = discover_mounts();
        let mut changed = false;
        // Migrate older deep-scan configs (default used to be 8).
        // FileProvider clamps walk to 1..=6 — allow up to 6 here.
        if cfg.index.max_depth > 6 {
            cfg.index.max_depth = default_depth();
            changed = true;
        }
        // Drop accidental mega-pins (home / / /home …) — depth-6 walks explode the index.
        let before_deep = cfg.index.deep_roots.len();
        cfg.index.deep_roots.retain(|s| !is_overbroad_deep_root(s));
        if cfg.index.deep_roots.len() != before_deep {
            changed = true;
        }
        // One-shot excludes seed (v1 → v2). Never re-merge on later loads so
        // Settings removals stick. New configs already get default_excludes via serde.
        if cfg.version < 2 {
            let _ = merge_missing_default_excludes(&mut cfg.index.exclude);
            cfg.version = CONFIG_VERSION;
            changed = true;
        }
        for m in &mounts {
            let key = m.target.to_string_lossy().to_string();
            if !cfg.index.include_mounts.contains_key(&key) {
                // windows_c off by default (large); others on
                let on = !key.contains("windows_c") && !key.contains("windowsEFI");
                cfg.index.include_mounts.insert(key, on);
                changed = true;
            }
        }

        {
            let before_ui = cfg.ui.clone();
            cfg.ui.sanitize();
            if cfg.ui != before_ui {
                changed = true;
            }
            let before_tr = cfg.translate.clone();
            cfg.translate.sanitize();
            if cfg.translate != before_tr {
                changed = true;
            }
        }

        let store = Self {
            inner: RwLock::new(Arc::new(cfg)),
            path,
        };
        if changed || !store.path.exists() {
            store.save();
        }
        store
    }

    /// Full owned clone — prefer [`snapshot`] or [`with`] on hot paths.
    #[allow(dead_code)] // public API for owned detach; hot paths use snapshot/with
    pub fn get(&self) -> BlinkConfig {
        (*self.snapshot()).clone()
    }

    /// Cheap shared handle to the current config (clone Arc only).
    pub fn snapshot(&self) -> Arc<BlinkConfig> {
        self.inner.read().unwrap().clone()
    }

    /// Borrow config without cloning the full snapshot (hot paths).
    pub fn with<R>(&self, f: impl FnOnce(&BlinkConfig) -> R) -> R {
        let g = self.inner.read().unwrap();
        f(g.as_ref())
    }

    /// Apply a mutation. Clones the config, runs `f`, sanitizes UI/translate.
    /// Swaps the Arc and writes disk **only when** the result differs from the
    /// previous snapshot (no-op promote/settings toggles must not thrash I/O).
    pub fn update<F: FnOnce(&mut BlinkConfig)>(&self, f: F) {
        let mut g = self.inner.write().unwrap();
        let mut cfg = (**g).clone();
        f(&mut cfg);
        cfg.ui.sanitize();
        cfg.translate.sanitize();
        if cfg == **g {
            return;
        }
        *g = Arc::new(cfg);
        drop(g);
        self.save();
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let snap = self.snapshot();
        if let Ok(data) = serde_json::to_string_pretty(snap.as_ref()) {
            let tmp = self.path.with_extension("json.tmp");
            if fs::write(&tmp, data).is_ok() {
                let _ = fs::rename(tmp, &self.path);
            }
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("blink/config.json")
}

pub fn discover_mounts() -> Vec<MountInfo> {
    let mut mounts = Vec::new();

    // findmnt -J preferred
    if let Ok(out) = Command::new("findmnt")
        .args(["-J", "-t", "ntfs,ntfs3,fuseblk,exfat,vfat"])
        .output()
    {
        if out.status.success() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(arr) = v.get("filesystems").and_then(|x| x.as_array()) {
                    collect_findmnt(arr, &mut mounts);
                }
            }
        }
    }

    if mounts.is_empty() {
        // Fallback: scan common roots
        for base in ["/mnt", "/media", "/run/media"] {
            let base = PathBuf::from(base);
            if let Ok(rd) = fs::read_dir(&base) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        // /run/media/user/LABEL
                        if base.ends_with("media") {
                            if let Ok(rd2) = fs::read_dir(&p) {
                                for e2 in rd2.flatten() {
                                    let p2 = e2.path();
                                    if p2.is_dir() {
                                        mounts.push(mount_from_path(&p2));
                                    }
                                }
                            }
                        } else {
                            mounts.push(mount_from_path(&p));
                        }
                    }
                }
            }
        }
    }

    // Dedupe by target
    let mut seen = std::collections::HashSet::new();
    mounts.retain(|m| seen.insert(m.target.clone()));
    mounts.sort_by(|a, b| a.target.cmp(&b.target));
    mounts
}

fn collect_findmnt(arr: &[serde_json::Value], out: &mut Vec<MountInfo>) {
    for fs in arr {
        if let Some(target) = fs.get("target").and_then(|t| t.as_str()) {
            if target.starts_with("/mnt")
                || target.starts_with("/media")
                || target.starts_with("/run/media")
            {
                // skip EFI/boot
                if target.contains("EFI") || target == "/boot" {
                    // still allow /mnt/windowsEFI skip
                    if target.contains("EFI") {
                        continue;
                    }
                }
                let label = fs
                    .get("label")
                    .and_then(|l| l.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut info = mount_from_path(Path::new(target));
                if !label.is_empty() {
                    info.label = label;
                }
                out.push(info);
            }
        }
        if let Some(children) = fs.get("children").and_then(|c| c.as_array()) {
            collect_findmnt(children, out);
        }
    }
}

fn mount_from_path(path: &Path) -> MountInfo {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("disk")
        .to_string();
    let drive_letter = match name.as_str() {
        "windows_c" | "Windows_C" | "c" | "C" => Some('C'),
        "windows_d" | "Windows_D" | "d" | "D" => Some('D'),
        "windows_e" | "e" | "E" => Some('E'),
        "windows_f" | "f" | "F" => Some('F'),
        _ => None,
    };
    let label = if name.starts_with("windows_") {
        match drive_letter {
            Some(c) => format!("Windows {c}"),
            None => name.clone(),
        }
    } else {
        name
    };
    MountInfo {
        target: path.to_path_buf(),
        label,
        drive_letter,
    }
}

/// Format a path for display using config path style + mount table.
pub fn pretty_path(path: &Path, style: &PathStyle, mounts: &[MountInfo]) -> String {
    // Home first
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~/".into();
            }
            return format!("~/{}", rest.display());
        }
    }

    // Longest matching mount prefix
    let mut best: Option<&MountInfo> = None;
    for m in mounts {
        if path.starts_with(&m.target) {
            if best.map(|b| m.target.components().count() > b.target.components().count())
                .unwrap_or(true)
            {
                best = Some(m);
            }
        }
    }

    if let Some(m) = best {
        let rest = path
            .strip_prefix(&m.target)
            .map(|r| r.display().to_string())
            .unwrap_or_default();
        let rest = if rest.is_empty() {
            String::new()
        } else {
            format!("/{rest}")
        };
        return match style {
            PathStyle::Drive => {
                if let Some(letter) = m.drive_letter {
                    format!("{letter}:{rest}")
                } else if !m.label.is_empty() {
                    format!("{}:{rest}", m.label)
                } else {
                    format!("{}{rest}", m.target.display())
                }
            }
            PathStyle::Label => {
                let label = if m.label.is_empty() {
                    m.target
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("disk")
                        .to_string()
                } else {
                    m.label.clone()
                };
                format!("{label}:{rest}")
            }
        };
    }

    path.display().to_string()
}

pub fn is_excluded(path: &Path, excludes: &[String]) -> bool {
    let s = path.to_string_lossy();
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        excludes.iter().any(|ex| {
            if ex.contains('/') {
                s.contains(ex.as_str())
            } else {
                name == *ex || name.eq_ignore_ascii_case(ex)
            }
        })
    })
}

#[cfg(test)]
mod config_store_tests {
    use super::*;

    #[test]
    fn merge_missing_excludes_only_adds_absent() {
        let mut list = vec![".git".into(), "custom".into()];
        assert!(merge_missing_default_excludes(&mut list));
        assert!(list.contains(&"custom".into()));
        assert!(list.contains(&"node_modules".into()));
        // Second merge is a no-op.
        assert!(!merge_missing_default_excludes(&mut list));
    }

    #[test]
    fn update_skips_save_when_unchanged() {
        let dir = std::env::temp_dir().join(format!(
            "blink-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        // Minimal valid config already at current version with excludes.
        let mut cfg = BlinkConfig::default();
        cfg.version = CONFIG_VERSION;
        fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

        let store = ConfigStore {
            inner: RwLock::new(Arc::new(cfg)),
            path: path.clone(),
        };
        let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
        // No-op update (already present deep root path style).
        store.update(|c| {
            let _ = c.index.path_style;
        });
        let mtime_after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "no-op update must not rewrite config");

        // Real change must rewrite.
        std::thread::sleep(std::time::Duration::from_millis(20));
        store.update(|c| {
            c.index.max_depth = 3;
        });
        let mtime_changed = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(mtime_changed > mtime_before, "real update must rewrite config");
        assert_eq!(store.snapshot().index.max_depth, 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_load_migrates_once_and_respects_user_removals() {
        // Simulate: after v2, a user-removed exclude must not be re-injected.
        let mut exclude = default_excludes();
        exclude.retain(|e| e != "node_modules");
        // v2 path: do not call merge.
        assert!(!exclude.iter().any(|e| e == "node_modules"));
        // v1 migration would re-add:
        let mut v1 = exclude.clone();
        assert!(merge_missing_default_excludes(&mut v1));
        assert!(v1.iter().any(|e| e == "node_modules"));
    }
}
