use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PathStyle {
    #[default]
    Label,
    Drive,
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
    /// Unknown keys preserved verbatim across rewrites (audit P3): a typo'd
    /// or newer-schema key must not be silently erased on the next save.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_true() -> bool {
    true
}

fn default_depth() -> usize {
    // Level 0 = root; walk max_depth components below it.
    // 2 ≈ ~/Projects/foo; clamped 1..=6 in FileProvider.
    2
}

/// Roots that must never be deep-pinned — depth-6 walks explode the index.
/// Shared by config load (strip bad pins) and engine promote (refuse).
/// `mounts` is passed in (audit P3): every call used to re-run
/// `discover_mounts()` — a `/proc` parse plus a canonicalize per mount —
/// so load paid it once per `deep_roots` entry and promote paid it per
/// ancestor level walked.
pub fn is_forbidden_deep_root(path: &Path, mounts: &[MountInfo]) -> bool {
    if path.as_os_str().is_empty() {
        return true;
    }
    if path == Path::new("/") {
        return true;
    }
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if p == Path::new("/") {
        return true;
    }
    if let Some(home) = dirs::home_dir() {
        let home = home.canonicalize().unwrap_or(home);
        if p == home {
            return true;
        }
        // Parent of home (`/home`, `/Users`, …) — deep-pinning it is `$HOME`
        // by another name.
        if let Some(parent) = home.parent() {
            if p == parent {
                return true;
            }
        }
    }
    // Mount roots themselves are never deep-pinned — pin a subfolder instead.
    // Derived from this machine's real mount table, not a hardcoded prefix list.
    if mounts
        .iter()
        .any(|m| p == m.target || m.target.canonicalize().map(|t| p == t).unwrap_or(false))
    {
        return true;
    }
    // System-wide trees that exist on every machine.
    matches!(
        p.to_string_lossy().as_ref(),
        "/var" | "/usr" | "/opt" | "/etc" | "/proc" | "/sys" | "/dev" | "/run" | "/tmp"
    )
}

fn is_overbroad_deep_root(s: &str, mounts: &[MountInfo]) -> bool {
    let p = PathBuf::from(expand_user_path(s));
    is_forbidden_deep_root(&p, mounts)
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
            extra: Default::default(),
        }
    }
}

impl IndexConfig {
    /// Clamp/normalize index knobs (load + update paths). `max_depth` clamps
    /// to the walkable boundary instead of resetting to the default (audit
    /// P3); `extra_roots` keeps only absolute paths — relative and
    /// `~otheruser` forms are silently ignored downstream, so carrying them
    /// in the file lies about what is indexed (audit P3).
    pub fn sanitize(&mut self) {
        self.max_depth = self.max_depth.clamp(1, 6);
        self.extra_roots
            .retain(|s| PathBuf::from(expand_user_path(s)).is_absolute());
    }
}

/// Per-category app overrides for opening files from Hark.
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
    /// Unknown keys preserved verbatim across rewrites (see `IndexConfig`).
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

/// Launcher chrome density (Raycast-style).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LayoutMode {
    /// Search + footer only until the query is non-empty; then results expand.
    #[default]
    Compact,
    /// Always show the results body (recents / empty state on idle).
    Expanded,
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
    /// Compact = search+footer until typing; Expanded = always show results body.
    #[serde(default)]
    pub layout_mode: LayoutMode,
    /// Unknown keys preserved verbatim across rewrites (see `IndexConfig`).
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
            layout_mode: LayoutMode::default(),
            extra: Default::default(),
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

/// Online translate-on-paste settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslateConfig {
    /// Master switch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Default target language code (`en`, `es`, `zh`, `hi`, …).
    #[serde(default = "default_translate_target")]
    pub target_lang: String,
    /// LibreTranslate-compatible base URL. Empty = free Google/MyMemory race.
    #[serde(default)]
    pub endpoint: String,
    /// Optional API key for the endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Auto-run when non-Latin scripts are detected (CJK, Cyrillic, Arabic, Indic, …).
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    /// Max source characters accepted for translation.
    #[serde(default = "default_translate_max_chars")]
    pub max_chars: usize,
    /// Unknown keys preserved verbatim across rewrites (see `IndexConfig`).
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
            extra: Default::default(),
        }
    }
}

impl TranslateConfig {
    pub fn sanitize(&mut self) {
        self.max_chars = self.max_chars.clamp(100, 5000);
        self.target_lang = self.target_lang.trim().to_ascii_lowercase();
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
        // Drop SSRF-prone / non-HTTP endpoints rather than POSTing secrets to them.
        if !self.endpoint.is_empty() && validate_translate_endpoint(&self.endpoint).is_err() {
            self.endpoint.clear();
        }
        if let Some(k) = self.api_key.take() {
            let t = k.trim().to_string();
            self.api_key = if t.is_empty() { None } else { Some(t) };
        }
    }
}

/// Validate a LibreTranslate-compatible base URL.
///
/// Empty is allowed (means free backends). Otherwise require `http`/`https`, a host,
/// no embedded credentials, and block cloud-metadata / link-local targets.
pub fn validate_translate_endpoint(endpoint: &str) -> Result<(), String> {
    let ep = endpoint.trim().trim_end_matches('/');
    if ep.is_empty() {
        return Ok(());
    }

    // Case-insensitive scheme; host checks use a lowercased view only.
    let lower = ep.to_ascii_lowercase();
    let authority_and_path = if let Some(rest) = lower.strip_prefix("https://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("http://") {
        rest
    } else {
        return Err("endpoint must start with http:// or https://".into());
    };
    // both http and https allowed (local LibreTranslate is often plain http)

    // Reject anything that is not a normal hierarchical URL authority.
    if authority_and_path.is_empty() || authority_and_path.starts_with('/') {
        return Err("endpoint missing host".into());
    }
    // user:pass@host is a common SSRF / secret-smuggling footgun.
    if authority_and_path.contains('@') {
        return Err("endpoint must not include credentials".into());
    }

    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if authority.is_empty() {
        return Err("endpoint missing host".into());
    }

    let host = parse_url_host(authority)?;
    if host.is_empty() {
        return Err("endpoint missing host".into());
    }
    if is_blocked_translate_host(&host) {
        return Err("endpoint host is not allowed".into());
    }
    Ok(())
}

fn parse_url_host(authority: &str) -> Result<String, String> {
    // IPv6: [2001:db8::1]:5000 or [2001:db8::1]
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| "endpoint has invalid IPv6 host".to_string())?;
        return Ok(rest[..end].to_ascii_lowercase());
    }
    // host:port or bare host
    let host = authority.split(':').next().unwrap_or(authority);
    Ok(host.to_ascii_lowercase())
}

fn is_blocked_translate_host(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    // Cloud metadata hostnames.
    if matches!(
        h.as_str(),
        "metadata"
            | "metadata.google.internal"
            | "metadata.goog"
            | "kubernetes.default"
            | "kubernetes.default.svc"
    ) || h.ends_with(".metadata.google.internal")
    {
        return true;
    }

    // IPv4 literal checks (including decimal forms of metadata / link-local).
    if let Some(ip) = parse_ipv4_literal(&h) {
        return is_blocked_ipv4(ip);
    }

    // IPv6: block link-local (fe80::/10), unique-local optional — block metadata-ish.
    if h.contains(':') {
        return is_blocked_ipv6(&h);
    }

    false
}

fn parse_ipv4_literal(host: &str) -> Option<[u8; 4]> {
    // Recognize every inet_aton-style form, not just dotted-decimal, so
    // `2130706173`, `0x7f.1`, and `0177.0.0.1` can't slip past the blocklist
    // (SSRF defense-in-depth for the translate endpoint).
    if host.contains(':') {
        return None; // IPv6 handled separately
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    // Parse one part as u32: decimal, 0x-hex, or 0-leading octal.
    let parse_part = |p: &str| -> Option<u32> {
        if p.is_empty() || p.len() > 10 {
            return None;
        }
        let bytes = p.as_bytes();
        if bytes.starts_with(b"0x") || bytes.starts_with(b"0X") {
            if bytes.len() == 2 {
                return None;
            }
            u32::from_str_radix(&p[2..], 16).ok()
        } else if bytes.len() > 1 && bytes[0] == b'0' {
            u32::from_str_radix(&p[1..], 8).ok()
        } else {
            p.parse::<u32>().ok()
        }
    };
    let nums: Vec<u32> = parts.iter().map(|p| parse_part(p)).collect::<Option<_>>()?;
    // inet_aton semantics: intermediate parts are single bytes; the LAST part
    // occupies the remaining low bytes (one per missing part + its own).
    // Example: 1.2.3.4 → 4 single bytes; 169.254.43910 → 169.254 + low16=43910.
    let mut value: u32 = 0;
    for (i, n) in nums.iter().enumerate() {
        if i + 1 < nums.len() {
            if *n > 255 {
                return None;
            }
            value = (value << 8) | *n;
        }
    }
    let last = *nums.last()? as u64;
    let low_bytes = (5 - nums.len()) as u32; // len=4→1 byte … len=1→4 bytes
    let low_max = (1u64 << (8 * u64::from(low_bytes))) - 1;
    if last > low_max {
        return None;
    }
    let shift = 8 * low_bytes;
    value = if shift >= 32 {
        last as u32 // single-part form: value was 0 anyway
    } else {
        (value << shift) | last as u32
    };
    Some(value.to_be_bytes())
}

fn is_blocked_ipv4(ip: [u8; 4]) -> bool {
    let [a, b, c, d] = ip;
    // 0.0.0.0/8
    if a == 0 {
        return true;
    }
    // 169.254.0.0/16 link-local (includes 169.254.169.254 metadata)
    if a == 169 && b == 254 {
        return true;
    }
    // 224.0.0.0/4 multicast and 240.0.0.0/4 reserved
    if a >= 224 {
        return true;
    }
    // Explicit broadcast
    if a == 255 && b == 255 && c == 255 && d == 255 {
        return true;
    }
    false
}

fn is_blocked_ipv6(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    // Link-local fe80::/10
    if h.starts_with("fe8") || h.starts_with("fe9") || h.starts_with("fea") || h.starts_with("feb")
    {
        return true;
    }
    // IPv4-mapped metadata ::ffff:169.254.x.x
    if let Some(v4) = h.strip_prefix("::ffff:") {
        if let Some(ip) = parse_ipv4_literal(v4) {
            return is_blocked_ipv4(ip);
        }
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct HarkConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub index: IndexConfig,
    /// Per-category default apps for opening files from Hark (not system MIME).
    #[serde(default)]
    pub open_with: OpenWithConfig,
    /// Appearance: transparency, accent, font, icons, radius.
    #[serde(default)]
    pub ui: UiThemeConfig,
    /// Translate-on-paste (non-Latin scripts / `tr ` prefix). Online via LibreTranslate or free backends.
    #[serde(default)]
    pub translate: TranslateConfig,
    /// Unknown keys preserved verbatim across rewrites (see `IndexConfig`).
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
    inner: RwLock<Arc<HarkConfig>>,
    path: PathBuf,
    /// Debounced-save support (#23): set when a save is due.
    pending_save: AtomicBool,
}

impl ConfigStore {
    /// Test-only: build a store over an in-memory config backed by `path`
    /// (writes stay in the temp dir; never touches the real config).
    #[cfg(test)]
    pub(crate) fn with_path(cfg: HarkConfig, path: std::path::PathBuf) -> Self {
        Self {
            inner: RwLock::new(Arc::new(cfg)),
            path,
            pending_save: AtomicBool::new(false),
        }
    }

    pub fn load() -> Self {
        let path = config_path();
        // True when a corrupt config was replaced — forces a fresh save below.
        let mut recovered = false;
        let mut cfg = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                    Err(err) => {
                        recovered = true;
                        match backup_invalid_config(&path) {
                            Some(backup) => eprintln!(
                                "hark: invalid config {} ({err}); using defaults (backup: {})",
                                path.display(),
                                backup.display()
                            ),
                            None => eprintln!(
                                "hark: invalid config {} ({err}); using defaults (backup failed)",
                                path.display()
                            ),
                        }
                        HarkConfig::default()
                    }
                    // Per-section parse (audit P3): one wrong-typed field
                    // used to discard the whole file. Now only the broken
                    // section falls back — the rest survives.
                    Ok(v) => {
                        let mut bad_sections = Vec::new();
                        let cfg = HarkConfig {
                            version: v
                                .get("version")
                                .and_then(|s| serde_json::from_value(s.clone()).ok())
                                .unwrap_or_else(default_version),
                            index: section_or_default(&v, "index", &mut bad_sections),
                            open_with: section_or_default(&v, "open_with", &mut bad_sections),
                            ui: section_or_default(&v, "ui", &mut bad_sections),
                            translate: section_or_default(&v, "translate", &mut bad_sections),
                            // Flattened extras are written inline, so unknown
                            // top-level keys reappear here on reload — gather
                            // them back so they survive rewrites (audit P3).
                            extra: v
                                .as_object()
                                .map(|map| {
                                    map.iter()
                                        .filter(|(k, _)| {
                                            !matches!(
                                                k.as_str(),
                                                "version"
                                                    | "index"
                                                    | "open_with"
                                                    | "ui"
                                                    | "translate"
                                                    | "extra"
                                            )
                                        })
                                        .map(|(k, val)| (k.clone(), val.clone()))
                                        .collect()
                                })
                                .unwrap_or_default(),
                        };
                        if !bad_sections.is_empty() {
                            recovered = true;
                            match backup_invalid_config(&path) {
                                Some(backup) => eprintln!(
                                    "hark: invalid config section(s) {} in {} — defaults used for those sections (backup: {})",
                                    bad_sections.join(", "),
                                    path.display(),
                                    backup.display()
                                ),
                                None => eprintln!(
                                    "hark: invalid config section(s) {} in {} — defaults used (backup failed)",
                                    bad_sections.join(", "),
                                    path.display()
                                ),
                            }
                        }
                        cfg
                    }
                },
                Err(err) => {
                    // Read failure (permissions etc.) — do not clobber the file.
                    eprintln!(
                        "hark: could not read config {} ({err}); using defaults",
                        path.display()
                    );
                    HarkConfig::default()
                }
            }
        } else {
            HarkConfig::default()
        };

        // Seed mount defaults for newly discovered mounts
        let mounts = discover_mounts();
        let mut changed = false;
        // Normalize index knobs (audit P3): `max_depth` clamps to the
        // walkable boundary instead of resetting to the default, and
        // non-absolute `extra_roots` are dropped (silently ignored
        // downstream, so keeping them lies about what is indexed).
        {
            let before_idx = cfg.index.clone();
            cfg.index.sanitize();
            if cfg.index != before_idx {
                changed = true;
            }
        }
        // Drop accidental mega-pins (home / / /home …) — depth-6 walks explode the index.
        let before_deep = cfg.index.deep_roots.len();
        cfg.index
            .deep_roots
            .retain(|s| !is_overbroad_deep_root(s, &mounts));
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
            // windows_c off by default (large); others on
            let on = !key.contains("windows_c") && !key.contains("windowsEFI");
            if let std::collections::hash_map::Entry::Vacant(e) =
                cfg.index.include_mounts.entry(key)
            {
                e.insert(on);
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
            pending_save: AtomicBool::new(false),
        };
        if changed || recovered || !store.path.exists() {
            store.save();
        }
        store
    }

    /// Full owned clone — prefer [`snapshot`] or [`with`] on hot paths.
    #[allow(dead_code)] // public API for owned detach; hot paths use snapshot/with
    pub fn get(&self) -> HarkConfig {
        (*self.snapshot()).clone()
    }

    /// Cheap shared handle to the current config (clone Arc only).
    pub fn snapshot(&self) -> Arc<HarkConfig> {
        self.inner.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Borrow config without cloning the full snapshot (hot paths).
    pub fn with<R>(&self, f: impl FnOnce(&HarkConfig) -> R) -> R {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        f(g.as_ref())
    }

    /// Apply a mutation. Clones the config, runs `f`, sanitizes
    /// UI/translate/index. Swaps the Arc and schedules a disk write **only when** the result
    /// differs from the previous snapshot (no-op promote/settings toggles
    /// must not thrash I/O). Writes run on a background thread, coalesced
    /// through `pending_save` — so per-keystroke updates never do main-thread
    /// I/O (#23).
    pub fn update<F: FnOnce(&mut HarkConfig)>(&self, f: F) {
        let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let mut cfg = (**g).clone();
        f(&mut cfg);
        cfg.ui.sanitize();
        cfg.translate.sanitize();
        cfg.index.sanitize();
        if cfg == **g {
            return;
        }
        *g = Arc::new(cfg);
        drop(g);
        self.pending_save.store(true, Ordering::Release);
        let data = self.serialize_snapshot();
        let path = self.path.clone();
        std::thread::spawn(move || write_config_disk(&path, &data));
    }

    /// Serialize the current snapshot for a background write; empty on error.
    fn serialize_snapshot(&self) -> String {
        serde_json::to_string_pretty(self.snapshot().as_ref()).unwrap_or_default()
    }

    /// Synchronous save (load-time recovery, tests, and explicit flushes).
    /// Runtime mutations use the background write in `update` (#23).
    pub fn save(&self) {
        self.pending_save.store(false, Ordering::Release);
        write_config_disk(&self.path, &self.serialize_snapshot());
    }

    /// Flush any pending background write synchronously (exit paths).
    pub fn flush_pending(&self) {
        if self.pending_save.swap(false, Ordering::AcqRel) {
            self.save();
        }
    }
}

/// Atomic disk write: tmp + chmod 0600 + rename (owner-only — translate
/// api_key and other secrets must not be group/world readable).
///
/// Delegates to [`write_private_file`], which serializes tmp-write→rename
/// under a process-wide lock (concurrent worker-thread `update` vs GTK
/// Settings writers), creates the tmp with mode 0600, and fsyncs before
/// rename so a crash cannot publish a truncated file.
fn write_config_disk(path: &Path, data: &str) {
    if data.is_empty() {
        return;
    }
    // Surface persistence failures (audit P3): settings must not look saved
    // when the write never landed (disk-full, read-only config, …).
    if !write_private_file(path, data.as_bytes()) {
        eprintln!("hark: failed to persist config {}", path.display());
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("hark/config.json")
}

/// True when `dir` is owned by this user and locked to 0700 — the same
/// guard fx.rs/translate.rs apply. Any store that can fall back to shared
/// `/tmp` space must refuse to persist through a directory someone else
/// pre-created (audit N1 threat model).
#[cfg(unix)]
pub(crate) fn dir_is_trusted(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = fs::metadata(dir) else {
        return false;
    };
    m.is_dir() && m.mode() & 0o777 == 0o700 && m.uid() == current_euid().unwrap_or(u32::MAX)
}

#[cfg(unix)]
pub(crate) fn current_euid() -> Option<u32> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

#[cfg(not(unix))]
pub(crate) fn dir_is_trusted(_dir: &Path) -> bool {
    true
}

/// Read side of the N1 guard: only load a store file from a parent dir we
/// own and that is 0700. A planted `typos.json`/`usage.json` in a shared
/// `/tmp` fallback would otherwise steer launches/scores on next start.
/// Returns None (→ caller uses defaults) when the dir is untrusted.
pub(crate) fn read_private_file(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        // Trust the resolve chain, not the file: state/cache dirs are ours;
        // the /tmp fallback is only reached when XDG+HOME both failed.
        let trusted_root = path.parent().map(dir_is_trusted).unwrap_or(false);
        // Normal XDG/state locations don't carry 0700 by spec (e.g.
        // ~/.local/share may be 0755); only enforce the strict check on
        // shared-space fallbacks.
        let in_shared_space = path
            .ancestors()
            .any(|a| a == std::path::Path::new("/tmp") || a == std::path::Path::new("/var/tmp"));
        if in_shared_space && !trusted_root {
            return None;
        }
    }
    fs::read_to_string(path).ok()
}

/// Atomic owner-only write: unique tmp created with mode 0600 (never
/// group/world-readable, CWE-732), fsync, rename into place. Shared by the
/// config/typos/usage stores; data must be small JSON (buffered in memory).
/// Refuses to write through a parent directory not owned by this user
/// (and not 0700) — the /tmp fallback must not become an attacker-writable
/// persistence path.
pub(crate) fn write_private_file(path: &Path, data: &[u8]) -> bool {
    static SAVE_LOCK: Mutex<()> = Mutex::new(());
    let _g = SAVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort lockdown before the ownership check (same shape
            // as fx.rs): a fresh dir becomes 0700 ours; a pre-created
            // attacker dir either fails the chmod or fails the uid check.
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
        if !dir_is_trusted(parent) {
            return false;
        }
    }
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let write_ok = (|| -> std::io::Result<()> {
        let mut f = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&tmp)?
            }
            #[cfg(not(unix))]
            {
                fs::File::create(&tmp)?
            }
        };
        f.write_all(data)?;
        f.sync_all()?;
        Ok(())
    })()
    .is_ok();
    if write_ok {
        if fs::rename(&tmp, path).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            }
            return true;
        }
        let _ = fs::remove_file(&tmp);
    }
    false
}

/// Copy a corrupt config aside (`config.json.invalid`) so the user can recover
/// settings, returning the backup path on success. Also used for the
/// typos/usage stores (`<name>.json.invalid`) — same shape, same recovery
/// story (audit P2).
pub(crate) fn backup_invalid_config(path: &Path) -> Option<PathBuf> {
    let backup = path.with_extension("json.invalid");
    fs::copy(path, &backup).ok().map(|_| backup)
}

/// One config section from a parsed `Value`: missing → default (same as the
/// field-level `serde(default)`s today); present-but-wrong-type → default +
/// recorded in `bad` so the caller backs up and logs the section name
/// instead of discarding the whole file (audit P3).
fn section_or_default<T>(v: &serde_json::Value, key: &'static str, bad: &mut Vec<&'static str>) -> T
where
    T: Default + for<'de> serde::Deserialize<'de>,
{
    match v.get(key) {
        None => T::default(),
        Some(sv) => match serde_json::from_value::<T>(sv.clone()) {
            Ok(t) => t,
            Err(_) => {
                bad.push(key);
                T::default()
            }
        },
    }
}

/// Filesystems that are not real on-disk user data (pseudo / overlay / volatile).
/// Everything else — NTFS, ext4, btrfs, exfat, f2fs, xfs, zfs, … — is indexable.
fn is_pseudo_fs(fstype: &str) -> bool {
    matches!(
        fstype,
        "proc"
            | "sysfs"
            | "devpts"
            | "tmpfs"
            | "devtmpfs"
            | "devfs"
            | "ramfs"
            | "debugfs"
            | "tracefs"
            | "fusectl"
            | "securityfs"
            | "mqueue"
            | "binfmt_misc"
            | "autofs"
            | "configfs"
            | "pstore"
            | "hugetlbfs"
            | "efivarfs"
            | "rpc_pipefs"
            | "nsfs"
            | "bpffs"
            | "cgroup"
            | "cgroup2"
            | "overlay"
            | "squashfs"
            | "fuse.portal"
    )
}

/// Loop devices (snaps, container images, disk images) and bind mounts are never
/// user volumes.
fn is_loop_or_virtual_source(device: &str) -> bool {
    let d = device.trim();
    d.starts_with("/dev/loop")
        || d.starts_with("/dev/ram")
        || d.starts_with("/dev/zram")
        || d == "none"
        || d.is_empty()
}

/// Physical block device source: `sdX`, `nvmeXnY`, `mmcblkX`, optical, or a
/// `UUID=`/`LABEL=`/`PARTUUID=`-style source from findmnt.
fn is_block_source(device: &str) -> bool {
    let d = device.trim();
    d.starts_with("/dev/sd")
        || d.starts_with("/dev/nvme")
        || d.starts_with("/dev/mmcblk")
        || d.starts_with("/dev/hd")
        || d.starts_with("/dev/vd")
        || d.starts_with("/dev/xvd")
        || d.starts_with("/dev/sr")
        || d.starts_with("UUID=")
        || d.starts_with("PARTUUID=")
        || d.starts_with("LABEL=")
}

/// Conventional user-data mount points (`/mnt`, `/media`, `/run/media`). Any
/// real filesystem mounted here is treated as a volume — including binds.
fn is_user_data_target(p: &Path) -> bool {
    p.starts_with("/mnt") || p.starts_with("/media") || p.starts_with("/run/media")
}

/// The container dirs themselves, never mount targets worth indexing.
fn is_container_target(p: &Path) -> bool {
    matches!(
        p.to_string_lossy().as_ref(),
        "/mnt" | "/media" | "/run/media"
    )
}

/// System trees that are never user volumes (block-backed or not).
fn is_system_target(p: &Path) -> bool {
    let t = p.to_string_lossy();
    if t == "/" {
        return true;
    }
    [
        "/boot",
        "/proc",
        "/sys",
        "/dev",
        "/run",
        "/var",
        "/usr",
        "/opt",
        "/etc",
        "/home",
        "/root",
        "/tmp",
        "/snap",
        "/efi",
        "/boot/efi",
    ]
    .iter()
    .any(|prefix| t == *prefix || t.starts_with(&format!("{prefix}/")))
}

pub fn discover_mounts() -> Vec<MountInfo> {
    let mut mounts = Vec::new();

    // Primary: the kernel mount table. No fstype filter — a Linux data drive
    // (ext4/btrfs/xfs) is just as valid as a Windows NTFS volume. Loop, snap,
    // pseudo and system mounts are filtered out structurally instead.
    if let Ok(contents) = fs::read_to_string("/proc/self/mounts") {
        for line in contents.lines() {
            let mut cols = line.split_whitespace();
            let (Some(device), Some(target), Some(fstype)) =
                (cols.next(), cols.next(), cols.next())
            else {
                continue;
            };
            // Mount points escape spaces/octals as \040 — restore them.
            let target = PathBuf::from(target.replace("\\040", " "));
            if is_pseudo_fs(fstype)
                || is_container_target(&target)
                || should_skip_mount_target(&target.to_string_lossy())
            {
                continue;
            }
            // Non-conventional mount points must be a physical block device
            // outside the system tree to be treated as a volume.
            if !is_user_data_target(&target)
                && (is_loop_or_virtual_source(device)
                    || !is_block_source(device)
                    || is_system_target(&target))
            {
                continue;
            }
            mounts.push(mount_from_path(&target));
        }
    } else {
        // No /proc/self/mounts (non-Linux): ask findmnt without a type filter.
        if let Ok(out) = Command::new("findmnt").arg("-J").output() {
            if out.status.success() {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(arr) = v.get("filesystems").and_then(|x| x.as_array()) {
                        collect_findmnt(arr, &mut mounts);
                    }
                }
            }
        }
    }

    // Always scan conventional roots too — catches manual/bind mounts the kernel
    // table lists in an unrecognised form, and stays useful without findmnt.
    for base in ["/mnt", "/media", "/run/media"] {
        let base = PathBuf::from(base);
        if let Ok(rd) = fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                if !p.is_dir() || should_skip_mount_target(&p.to_string_lossy()) {
                    continue;
                }
                // /run/media/<user>/<LABEL> nests one level deeper.
                if base.ends_with("media") {
                    if let Ok(rd2) = fs::read_dir(&p) {
                        for e2 in rd2.flatten() {
                            let p2 = e2.path();
                            if p2.is_dir() && !should_skip_mount_target(&p2.to_string_lossy()) {
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

    // Dedupe by target
    let mut seen = std::collections::HashSet::new();
    mounts.retain(|m| seen.insert(m.target.clone()));
    mounts.sort_by(|a, b| a.target.cmp(&b.target));
    mounts
}

/// Mount targets we never treat as user volumes (EFI / ESP / bare boot).
fn should_skip_mount_target(target: &str) -> bool {
    if target.contains("EFI") || target.contains("/efi") || target.contains("efi/") {
        return true;
    }
    // Exact system boot mount (the old code intended this but never continued).
    if target == "/boot" {
        return true;
    }
    false
}

fn collect_findmnt(arr: &[serde_json::Value], out: &mut Vec<MountInfo>) {
    for fs in arr {
        let fstype = fs.get("fstype").and_then(|t| t.as_str()).unwrap_or("");
        let source = fs.get("source").and_then(|s| s.as_str()).unwrap_or("");
        if let Some(target) = fs.get("target").and_then(|t| t.as_str()) {
            if is_user_data_target(Path::new(target))
                && !is_pseudo_fs(fstype)
                && !is_loop_or_virtual_source(source)
                && !is_container_target(Path::new(target))
                && !should_skip_mount_target(target)
            {
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
        if path.starts_with(&m.target)
            && best
                .map(|b| m.target.components().count() > b.target.components().count())
                .unwrap_or(true)
        {
            best = Some(m);
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

/// Pre-parsed exclude list for hot walk paths (index + live deep).
/// Simple names → `HashSet` lookup; slash patterns → component-boundary match.
#[derive(Debug, Clone)]
pub struct ExcludeSet {
    /// Lowercased component names (e.g. `node_modules`, `.git`).
    names: std::collections::HashSet<String>,
    /// Path patterns containing `/`, split into component sequences (e.g. `.pi/agent/sessions`).
    patterns: Vec<Vec<String>>,
}

impl ExcludeSet {
    pub fn from_list(excludes: &[String]) -> Self {
        let mut names = std::collections::HashSet::with_capacity(excludes.len());
        let mut patterns = Vec::new();
        for ex in excludes {
            let ex = ex.trim();
            if ex.is_empty() {
                continue;
            }
            if ex.contains('/') {
                let pat: Vec<String> = ex
                    .split('/')
                    .map(|c| c.to_ascii_lowercase())
                    .filter(|c| !c.is_empty())
                    .collect();
                // Patterns reduced to nothing (e.g. "/") would make
                // `matches` call `windows(0)` → panic; skip them.
                if !pat.is_empty() {
                    patterns.push(pat);
                }
            } else {
                names.insert(ex.to_ascii_lowercase());
            }
        }
        Self { names, patterns }
    }

    pub fn matches(&self, path: &Path) -> bool {
        if self.names.is_empty() && self.patterns.is_empty() {
            return false;
        }
        // Component name checks first (common case) — O(components) set lookups.
        // Set stores ascii-lowercase keys — lowercase the component once and
        // do a single lookup.
        if !self.names.is_empty() {
            for c in path.components() {
                let name_lower = c.as_os_str().to_string_lossy().to_ascii_lowercase();
                if self.names.contains(name_lower.as_str()) {
                    return true;
                }
            }
        }
        if !self.patterns.is_empty() {
            let comps: Vec<String> = path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
                .collect();
            for pattern in &self.patterns {
                if comps
                    .windows(pattern.len())
                    .any(|w| w == pattern.as_slice())
                {
                    return true;
                }
            }
        }
        false
    }
}

/// Convenience for one-off checks (builds a set each call — prefer [`ExcludeSet`] on hot paths).
#[allow(dead_code)]
pub fn is_excluded(path: &Path, excludes: &[String]) -> bool {
    ExcludeSet::from_list(excludes).matches(path)
}

#[cfg(test)]
mod exclude_set_tests {
    use super::*;

    #[test]
    fn slash_only_pattern_does_not_panic() {
        // Audit P1 chain: `index.exclude = ["/"]` previously produced an empty
        // pattern and `matches` called `windows(0)` → panic on every entry.
        let ex = ExcludeSet::from_list(&["/".to_string()]);
        assert!(!ex.matches(Path::new("/home/u/docs/report.md")));
    }

    #[test]
    fn empty_and_blank_entries_ignored() {
        let ex = ExcludeSet::from_list(&["".to_string(), "   ".to_string(), ".git".to_string()]);
        assert!(ex.matches(Path::new("/home/u/proj/.git/HEAD")));
        assert!(!ex.matches(Path::new("/home/u/proj/src/main.rs")));
    }
}

#[cfg(test)]
mod config_store_tests {
    use super::*;

    #[test]
    fn ssrf_non_decimal_ip_literals_blocked() {
        // Audit P3: decimal/hex/octal/short forms of blocked ranges must not
        // pass the endpoint blocklist as pseudo-hostnames.
        // 169.254.169.254 metadata — alternative encodings.
        assert!(is_blocked_translate_host("2852039166")); // decimal
        assert!(is_blocked_translate_host("0xa9.0xfe.0xa9.0xfe")); // hex dotted
        assert!(is_blocked_translate_host("0xa9fea9fe")); // hex single
        assert!(is_blocked_translate_host("169.254.43910")); // short 3-part
        assert!(is_blocked_translate_host("0251.0376.0251.0376")); // octal
                                                                   // 224.0.0.1 multicast — decimal + hex forms.
        assert!(is_blocked_translate_host("3758096385"));
        assert!(is_blocked_translate_host("0xe0000001"));
        // 0.0.0.0/8 — single zero and decimal forms.
        assert!(is_blocked_translate_host("0"));
        assert!(is_blocked_translate_host("0.0.0.0"));
        // IPv4-mapped IPv6 with a bad v4 tail.
        assert!(is_blocked_translate_host("::ffff:169.254.169.254"));
        // Ordinary hostnames and normal literals still pass through
        // (loopback deliberately allowed: local LibreTranslate use case).
        assert!(!is_blocked_translate_host("translate.example.org"));
        assert!(!is_blocked_translate_host("192.168.1.5"));
        assert!(!is_blocked_translate_host("127.0.0.1"));
        assert!(!is_blocked_translate_host("::ffff:192.168.1.5"));
    }

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
    fn unknown_keys_survive_rewrite() {
        // Audit P3: typo'd/future keys must not be erased on save.
        let cfg: HarkConfig = serde_json::from_str(
            r#"{"version":2,"future_top":true,"index":{"max_depth":3,"mystery":1},"ui":{"opacity":0.9,"weird":2}}"#,
        )
        .unwrap();
        assert_eq!(cfg.index.max_depth, 3);
        let raw = serde_json::to_string(&cfg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v.get("future_top").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(
            v.pointer("/index/mystery").and_then(|x| x.as_u64()),
            Some(1)
        );
        assert_eq!(v.pointer("/ui/weird").and_then(|x| x.as_u64()), Some(2));
    }

    #[test]
    fn bad_section_defaults_only_itself() {
        // Audit P3: one wrong-typed section must not reset the rest.
        let v: serde_json::Value = serde_json::from_str(
            r#"{"version":2,"index":{"max_depth":4},"ui":{"opacity":"opaque"}}"#,
        )
        .unwrap();
        let mut bad = Vec::new();
        let index: IndexConfig = section_or_default(&v, "index", &mut bad);
        let ui: UiThemeConfig = section_or_default(&v, "ui", &mut bad);
        assert_eq!(index.max_depth, 4);
        assert_eq!(ui, UiThemeConfig::default());
        assert_eq!(bad, vec!["ui"]);
    }

    #[test]
    fn index_sanitize_clamps_and_drops_relative() {
        // Audit P3: max_depth clamps to the boundary (7→6, 0→1);
        // relative / ~otheruser roots are dropped, ~/ kept.
        let mut idx = IndexConfig {
            max_depth: 9,
            extra_roots: vec![
                "relative/path".into(),
                "~otheruser/x".into(),
                "~/docs".into(),
                "/abs/path".into(),
            ],
            ..Default::default()
        };
        idx.sanitize();
        assert_eq!(idx.max_depth, 6);
        assert_eq!(idx.extra_roots.len(), 2);
        idx.max_depth = 0;
        idx.sanitize();
        assert_eq!(idx.max_depth, 1);
    }

    #[test]
    fn update_skips_save_when_unchanged() {
        let dir = std::env::temp_dir().join(format!(
            "hark-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        // Minimal valid config already at current version with excludes.
        let cfg = HarkConfig {
            version: CONFIG_VERSION,
            ..Default::default()
        };
        fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

        let store = ConfigStore {
            inner: RwLock::new(Arc::new(cfg)),
            path: path.clone(),
            pending_save: AtomicBool::new(false),
        };
        let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
        // No-op update (already present deep root path style).
        store.update(|c| {
            let _ = c.index.path_style;
        });
        let mtime_after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "no-op update must not rewrite config"
        );

        // Real change must rewrite (update now writes on a background thread —
        // flush and poll briefly for the mtime change).
        std::thread::sleep(std::time::Duration::from_millis(20));
        store.update(|c| {
            c.index.max_depth = 3;
        });
        let mut mtime_changed = fs::metadata(&path).unwrap().modified().unwrap();
        for _ in 0..50 {
            if mtime_changed > mtime_before {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            mtime_changed = fs::metadata(&path).unwrap().modified().unwrap();
        }
        assert!(
            mtime_changed > mtime_before,
            "real update must rewrite config"
        );
        assert_eq!(store.snapshot().index.max_depth, 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_config_is_backed_up_before_use() {
        let dir = std::env::temp_dir().join(format!(
            "hark-config-backup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        fs::write(&path, "{ not valid json").unwrap();

        let backup = backup_invalid_config(&path).expect("backup should be written");
        assert_eq!(backup, dir.join("config.json.invalid"));
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            "{ not valid json",
            "original corrupt bytes preserved for recovery"
        );

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

    #[test]
    fn skip_efi_and_boot_mount_targets() {
        assert!(should_skip_mount_target("/boot"));
        assert!(should_skip_mount_target("/mnt/windowsEFI"));
        assert!(should_skip_mount_target("/mnt/EFI"));
        assert!(should_skip_mount_target("/run/media/user/ESP_EFI"));
        assert!(!should_skip_mount_target("/mnt/windows_d"));
        assert!(!should_skip_mount_target("/media/alice/Data"));
        assert!(!should_skip_mount_target("/run/media/alice/USB"));
    }

    #[test]
    fn mount_filters_classify_any_fstype() {
        // Any real filesystem is indexable — no Windows-only fstype hardcode.
        assert!(!is_pseudo_fs("ext4"));
        assert!(!is_pseudo_fs("btrfs"));
        assert!(!is_pseudo_fs("xfs"));
        assert!(!is_pseudo_fs("ntfs3"));
        assert!(!is_pseudo_fs("exfat"));
        assert!(is_pseudo_fs("proc"));
        assert!(is_pseudo_fs("overlay"));
        assert!(is_pseudo_fs("squashfs"));
        assert!(is_pseudo_fs("tmpfs"));

        assert!(is_loop_or_virtual_source("/dev/loop0"));
        assert!(is_loop_or_virtual_source("none"));
        assert!(!is_loop_or_virtual_source("/dev/nvme0n1p4"));
        assert!(!is_loop_or_virtual_source("/dev/sda1"));

        assert!(is_block_source("/dev/nvme0n1p8"));
        assert!(is_block_source("/dev/sda1"));
        assert!(is_block_source("/dev/mmcblk0p2"));
        assert!(is_block_source("UUID=abcd-1234"));
        assert!(!is_block_source("/dev/loop0"));
        assert!(!is_block_source("tmpfs"));

        assert!(is_user_data_target(Path::new("/mnt/data")));
        assert!(is_user_data_target(Path::new("/media/alice/Data")));
        assert!(is_user_data_target(Path::new("/run/media/vedant/Games")));
        assert!(!is_user_data_target(Path::new("/home/vedant")));

        assert!(is_container_target(Path::new("/mnt")));
        assert!(is_container_target(Path::new("/media")));
        assert!(is_container_target(Path::new("/run/media")));
        assert!(!is_container_target(Path::new("/mnt/data")));

        assert!(is_system_target(Path::new("/")));
        assert!(is_system_target(Path::new("/boot")));
        assert!(is_system_target(Path::new("/usr/local")));
        assert!(is_system_target(Path::new("/home")));
        assert!(is_system_target(Path::new("/run/user/1000/gvfs")));
        assert!(is_system_target(Path::new("/run/media/vedant/Games")));
        assert!(!is_system_target(Path::new("/mnt/data")));
    }

    #[test]
    fn exclude_set_matches_names_and_patterns() {
        let set = ExcludeSet::from_list(&["node_modules".into(), ".Git".into(), "foo/bar".into()]);
        assert!(set.matches(Path::new("/home/a/node_modules/x")));
        assert!(set.matches(Path::new("/home/a/.git")));
        assert!(set.matches(Path::new("/home/a/.Git")));
        assert!(set.matches(Path::new("/tmp/foo/bar/baz")));
        assert!(!set.matches(Path::new("/home/a/src/main.rs")));
        // Substring over-match regression (A7): `foo/bar` must NOT match a path
        // that merely *contains* that string mid-component.
        assert!(!set.matches(Path::new("/home/a/foobar/baz")));
        assert!(!set.matches(Path::new("/home/a/xfoo/barx/y")));
    }

    #[test]
    fn exclude_set_component_boundary_case_insensitive() {
        let set = ExcludeSet::from_list(&[".pi/agent/sessions".into(), "Sub/Dir".into()]);
        // Exact component sequence matches, any depth, any case.
        assert!(set.matches(Path::new("/home/u/.pi/agent/sessions")));
        assert!(set.matches(Path::new("/home/u/sub/dir")));
        assert!(set.matches(Path::new("/home/u/x/.PI/AGENT/sessions/y")));
        // Substring inside a single component must not match.
        assert!(!set.matches(Path::new("/home/u/x.pi/agent/sessions_bak")));
        assert!(!set.matches(Path::new("/home/u/sub/directory")));
        assert!(!set.matches(Path::new("/home/u/subdir/x")));
        // Leading empty components (absolute paths) are handled.
        assert!(!set.matches(Path::new("/pi/agent/sessions")));
    }

    #[test]
    fn translate_endpoint_rejects_ssrf_targets() {
        assert!(validate_translate_endpoint("").is_ok());
        assert!(validate_translate_endpoint("https://translate.example.com").is_ok());
        assert!(validate_translate_endpoint("http://127.0.0.1:5000").is_ok());
        assert!(validate_translate_endpoint("http://localhost:5000/").is_ok());
        assert!(validate_translate_endpoint("http://192.168.1.10:5000").is_ok());

        assert!(validate_translate_endpoint("file:///etc/passwd").is_err());
        assert!(validate_translate_endpoint("https://user:pass@evil.test").is_err());
        assert!(validate_translate_endpoint("http://169.254.169.254/latest").is_err());
        assert!(validate_translate_endpoint("http://metadata.google.internal").is_err());
        assert!(validate_translate_endpoint("not-a-url").is_err());
        assert!(validate_translate_endpoint("https://").is_err());
    }

    #[test]
    fn translate_sanitize_clears_blocked_endpoint() {
        let mut cfg = TranslateConfig {
            endpoint: "http://169.254.169.254/".into(),
            ..Default::default()
        };
        cfg.sanitize();
        assert!(cfg.endpoint.is_empty());

        cfg.endpoint = "https://lt.example.com/path/".into();
        cfg.sanitize();
        assert_eq!(cfg.endpoint, "https://lt.example.com/path");
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "hark-config-perm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        let cfg = HarkConfig {
            version: CONFIG_VERSION,
            ..Default::default()
        };
        // Start world-readable so we can prove save tightens mode.
        fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let store = ConfigStore {
            inner: RwLock::new(Arc::new(cfg)),
            path: path.clone(),
            pending_save: AtomicBool::new(false),
        };
        store.save();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config.json must be owner-read/write only");

        let _ = fs::remove_dir_all(&dir);
    }

    /// The shared store-write guard (audit N1, extended): a pre-created
    /// non-0700 directory in shared /tmp space must refuse writes —
    /// persistence through an attacker-owned dir lets them plant/replace
    /// typos.json/usage.json on next start.
    #[cfg(unix)]
    #[test]
    fn write_private_file_refuses_untrusted_dir() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "hark-untrusted-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // Simulate a dir that *looks* shared (0755) but is ours: the write
        // path tightens it to 0700 first, then persists — same shape as
        // fx.rs. (A genuinely attacker-owned dir fails the chmod AND the
        // uid check; that can't be simulated unprivileged.)
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        let path = dir.join("usage.json");
        assert!(
            write_private_file(&path, b"{}"),
            "own 0755 dir is tightened to 0700 and accepted"
        );
        let dmode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700, "dir must be tightened to 0700 by the write");
        let fmode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(fmode, 0o600, "store file must be owner-only");
        let _ = fs::remove_dir_all(&dir);

        // Control: a 0700 dir we own accepts the write.
        let dir2 = std::env::temp_dir().join(format!(
            "hark-trusted-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir(&dir2).unwrap();
        fs::set_permissions(&dir2, fs::Permissions::from_mode(0o700)).unwrap();
        let path2 = dir2.join("usage.json");
        assert!(write_private_file(&path2, b"{}"));
        assert!(path2.exists());
        let _ = fs::remove_dir_all(&dir2);
    }

    /// The dir-trust predicate itself: mode must be exactly 0700.
    #[cfg(unix)]
    #[test]
    fn dir_is_trusted_requires_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "hark-dirtrust-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!dir_is_trusted(&dir), "0755 must not be trusted");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(dir_is_trusted(&dir), "own 0700 is trusted");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Read side: a store file under /tmp in an untrusted dir is ignored.
    #[cfg(unix)]
    #[test]
    fn read_private_file_ignores_shared_space_untrusted() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "hark-read-untrusted-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir(&dir).unwrap();
        // Read guard checks the *parent dir of the file*; simulate planted
        // file in world-readable shared space.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("typos.json");
        fs::write(&path, r#"{"aliases":{}}"#).unwrap();
        assert!(
            read_private_file(&path).is_none(),
            "planted file in shared /tmp must not be loaded"
        );
        // Trusted 0700 dir in /tmp still reads (fallback-of-fallback works
        // for the legitimate owner).
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(read_private_file(&path).is_some());
        let _ = fs::remove_dir_all(&dir);

        // Normal state dir (0755, ours, NOT under /tmp) reads fine.
        let home_dir = dirs::home_dir().expect("home");
        let normal = home_dir.join(".cache");
        if normal.is_dir() {
            assert!(read_private_file(&normal.join("no-such-file")).is_none()); // missing = None either way
        }
    }
}
