use crate::config::{discover_mounts, ConfigStore, ExcludeSet, MountInfo};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

/// Hard cap on indexed entries (walk stops when reached).
pub const MAX_INDEX: usize = 100_000;
/// Re-walk roots at most this often when fingerprint is unchanged.
pub const INDEX_TTL_SECS: u64 = 30 * 60;
/// Bump when on-disk cache layout changes.
pub(crate) const CACHE_VERSION: u32 = 8;

/// In-memory search entry (derived fields filled on load/build).
#[derive(Debug, Clone)]
pub(crate) struct IndexedPath {
    pub path: PathBuf,
    pub name: String,
    pub name_lower: String,
    pub path_lower: String,
    pub is_dir: bool,
    pub depth: u16,
    pub low_value: bool,
    pub high_value: bool,
    pub is_mnt: bool,
}

/// Compact on-disk row — only what cannot be derived from `path`.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    #[serde(rename = "p")]
    path: String,
    #[serde(rename = "d")]
    is_dir: bool,
    #[serde(rename = "n")]
    depth: u16,
    /// Under a discovered mount root (old caches default to false).
    #[serde(rename = "m", default)]
    mnt: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    #[serde(default)]
    fingerprint: String,
    /// Whether the cap was reached while walking pinned deep roots (serde
    /// default keeps older caches readable).
    #[serde(default)]
    capped_by_deep: bool,
    items: Vec<CacheEntry>,
}

pub(crate) struct IndexState {
    pub index: RwLock<Vec<IndexedPath>>,
    pub indexing: AtomicBool,
    pub progress: AtomicUsize,
    pub capped: AtomicBool,
    /// Cap was hit during the pinned deep-roots phase → surfaced as a warning.
    pub capped_by_deep: AtomicBool,
    pub config: Arc<ConfigStore>,
    /// Mount snapshot shared per search via cheap `Arc` clone (audit P3
    /// Pass 21): mounts change only on mount events, so the per-keystroke
    /// deep `Vec` clone (2 heap allocs per `MountInfo`) is pure churn.
    pub mounts: RwLock<Arc<[MountInfo]>>,
    fingerprint: RwLock<String>,
    /// Ensures only one filesystem walk/cache write runs at a time.
    build_lock: Mutex<()>,
}

/// Resets `indexing` when the build scope exits — including via panic — so
/// the UI never wedges on "indexing…" (the explicit `store(false)` at the
/// end of `ensure_fresh` still runs on the happy path).
struct IndexingGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for IndexingGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

impl IndexState {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self {
            index: RwLock::new(Vec::new()),
            indexing: AtomicBool::new(false),
            progress: AtomicUsize::new(0),
            capped: AtomicBool::new(false),
            capped_by_deep: AtomicBool::new(false),
            config,
            mounts: RwLock::new(discover_mounts().into()),
            fingerprint: RwLock::new(String::new()),
            build_lock: Mutex::new(()),
        }
    }

    /// Load cache if valid; rebuild only when stale, fingerprint mismatch, or missing.
    /// Must run off the GTK UI thread (startup / periodic / force_reindex bg threads).
    ///
    /// Battery path: if RAM already holds a fresh index (TTL + fingerprint), skip
    /// disk re-read and mount rediscovery. Periodic timer hits this every 45m.
    pub fn ensure_fresh(&self) {
        // Battery path: if RAM holds a non-empty index and meta TTL is OK,
        // recompute fingerprint against *cached* mounts (no findmnt).
        // Only rediscover mounts when TTL expires or that fingerprint mismatches
        // (settings change or mounts list actually drifted after a real refresh).
        {
            let n = self.index.read().unwrap_or_else(|p| p.into_inner()).len();
            if n > 0 && !cache_ttl_stale() {
                let mem_fp = self
                    .fingerprint
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                let fp_cached_mounts = self.compute_fingerprint();
                if !mem_fp.is_empty() && mem_fp == fp_cached_mounts {
                    return; // no I/O beyond meta TTL file already checked
                }
                // Config roots/depth/excludes changed, or mount set in RAM is stale.
                *self.mounts.write().unwrap_or_else(|p| p.into_inner()) = discover_mounts().into();
                let fp = self.compute_fingerprint();
                if !mem_fp.is_empty() && mem_fp == fp {
                    return;
                }
                self.run_build(fp, false);
                return;
            }
        }

        *self.mounts.write().unwrap_or_else(|p| p.into_inner()) = discover_mounts().into();
        let fp = self.compute_fingerprint();

        if let Some((items, cached_fp, cached_by_deep)) = load_cache() {
            let n = items.len();
            *self.index.write().unwrap_or_else(|p| p.into_inner()) = items;
            self.progress.store(n, Ordering::Relaxed);
            self.capped.store(n >= MAX_INDEX, Ordering::Relaxed);
            self.capped_by_deep
                .store(n >= MAX_INDEX && cached_by_deep, Ordering::Relaxed);
            *self.fingerprint.write().unwrap_or_else(|p| p.into_inner()) = cached_fp.clone();

            let ttl_ok = !cache_ttl_stale();
            let fp_ok = cached_fp == fp;
            if ttl_ok && fp_ok {
                return;
            }
        }

        self.run_build(fp, false);
    }

    pub fn force_rebuild(&self) {
        clear_cache();
        *self.mounts.write().unwrap_or_else(|p| p.into_inner()) = discover_mounts().into();
        let fp = self.compute_fingerprint();
        self.run_build(fp, true);
    }

    fn run_build(&self, fingerprint: String, force: bool) {
        // Serialize walks + cache writes; recover if a previous holder panicked.
        let _guard = self.build_lock.lock().unwrap_or_else(|p| p.into_inner());

        // A concurrent ensure_fresh may have finished the same fingerprint while we waited.
        if !force {
            let n = self.index.read().unwrap_or_else(|p| p.into_inner()).len();
            let mem_fp = self
                .fingerprint
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if n > 0 && !mem_fp.is_empty() && mem_fp == fingerprint && !cache_ttl_stale() {
                // Ensure a panicked prior builder cannot leave the UI stuck on "indexing".
                self.indexing.store(false, Ordering::Relaxed);
                return;
            }
        }

        self.indexing.store(true, Ordering::Relaxed);
        // Drop guard: a panicking builder must not wedge the UI on
        // "indexing…" until the next successful build (audit P3).
        let _indexing_guard = IndexingGuard {
            flag: &self.indexing,
        };
        self.progress.store(0, Ordering::Relaxed);
        self.capped.store(false, Ordering::Relaxed);
        self.capped_by_deep.store(false, Ordering::Relaxed);
        let items = self.build_index();
        let n = items.len();
        let hit_cap = n >= MAX_INDEX;
        let capped_by_deep = self.capped_by_deep.load(Ordering::Relaxed);
        // Disk write before swap so a crash mid-write doesn't leave empty RAM index.
        save_cache(&items, &fingerprint, capped_by_deep);
        *self.index.write().unwrap_or_else(|p| p.into_inner()) = items;
        *self.fingerprint.write().unwrap_or_else(|p| p.into_inner()) = fingerprint;
        self.progress.store(n, Ordering::Relaxed);
        self.capped.store(hit_cap, Ordering::Relaxed);
        self.indexing.store(false, Ordering::Relaxed);
    }

    fn compute_fingerprint(&self) -> String {
        let cfg = self.config.snapshot();
        // Snapshot the Arc, then drop the lock before the loop: the guard
        // must not be held across hashing (and previously across WalkDir).
        let mounts: Arc<[MountInfo]> = self
            .mounts
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let mut roots: Vec<String> = Vec::new();
        if cfg.index.include_home {
            if let Some(home) = dirs::home_dir() {
                roots.push(home.display().to_string());
            }
        }
        for m in mounts.iter() {
            let key = m.target.to_string_lossy().to_string();
            let enabled = cfg.index.include_mounts.get(&key).copied().unwrap_or(true);
            if enabled {
                roots.push(key);
            }
        }
        for extra in &cfg.index.extra_roots {
            roots.push(extra.clone());
        }
        for deep in &cfg.index.deep_roots {
            roots.push(format!("deep:{deep}"));
        }
        roots.sort();
        let mut hasher = DefaultHasher::new();
        CACHE_VERSION.hash(&mut hasher);
        cfg.index.max_depth.hash(&mut hasher);
        cfg.index.include_home.hash(&mut hasher);
        // Config roots + excludes only — content freshness is handled by TTL.
        // (Root mtimes change constantly and would thrash rebuilds.)
        for r in &roots {
            r.hash(&mut hasher);
        }
        let mut excl = cfg.index.exclude.clone();
        excl.sort();
        for e in excl {
            e.hash(&mut hasher);
        }
        format!("{:x}", hasher.finish())
    }

    fn build_index(&self) -> Vec<IndexedPath> {
        let cfg = self.config.snapshot();
        // Cheap `Arc` snapshot (audit P3): no per-build deep `Vec` clone.
        let mounts = self
            .mounts
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let excludes = ExcludeSet::from_list(&cfg.index.exclude);
        let max_depth = cfg.index.max_depth.clamp(1, 6);

        let mut roots: Vec<PathBuf> = Vec::new();
        if cfg.index.include_home {
            if let Some(home) = dirs::home_dir() {
                roots.push(home);
            }
        }
        for m in mounts.iter() {
            let key = m.target.to_string_lossy().to_string();
            let enabled = cfg.index.include_mounts.get(&key).copied().unwrap_or(true);
            if enabled && m.target.is_dir() {
                roots.push(m.target.clone());
            }
        }
        for extra in &cfg.index.extra_roots {
            let p = expand_user(extra);
            if p.is_dir() {
                roots.push(p);
            }
        }

        // Pinned deep roots: always walk to depth 6 (independent of global depth).
        let mut deep_roots: Vec<PathBuf> = Vec::new();
        for deep in &cfg.index.deep_roots {
            let p = expand_user(deep);
            if p.is_dir() {
                deep_roots.push(p);
            }
        }

        let mut items = Vec::with_capacity(4096);
        let mut seen = std::collections::HashSet::new();

        // Enabled mount roots, used to tag entries as mounted (ranking + pretty
        // display), derived from discovery — not a hardcoded `/mnt/` prefix.
        let mnt_targets: Vec<PathBuf> = mounts
            .iter()
            .filter(|m| {
                let key = m.target.to_string_lossy().to_string();
                cfg.index.include_mounts.get(&key).copied().unwrap_or(true)
            })
            .map(|m| m.target.clone())
            .collect();

        // Explicit params avoid overlapping borrows with `items.len()` checks.
        let walk_root = |root: &PathBuf,
                         depth: usize,
                         items: &mut Vec<IndexedPath>,
                         seen: &mut std::collections::HashSet<PathBuf>|
         -> bool {
            if !root.exists() {
                return false;
            }
            for entry in WalkDir::new(root)
                .follow_links(false)
                .max_depth(depth)
                .into_iter()
                .filter_entry(|e| should_descend(e.path(), root, &excludes))
                .flatten()
            {
                let path = entry.path().to_path_buf();
                if path == *root {
                    continue;
                }
                if !seen.insert(path.clone()) {
                    continue;
                }
                if should_skip_entry(&path, &excludes) {
                    continue;
                }
                let is_mnt = mnt_targets.iter().any(|t| path.starts_with(t));
                if let Some(item) = index_entry(&path, root, is_mnt) {
                    items.push(item);
                    if items.len() >= MAX_INDEX {
                        self.capped.store(true, Ordering::Relaxed);
                        return true;
                    }
                    if items.len() % 250 == 0 {
                        self.progress.store(items.len(), Ordering::Relaxed);
                    }
                }
            }
            false
        };

        for root in &roots {
            if walk_root(root, max_depth, &mut items, &mut seen) {
                break;
            }
        }
        if items.len() < MAX_INDEX {
            for root in &deep_roots {
                if walk_root(root, 6, &mut items, &mut seen) {
                    // Cap hit while walking a pinned deep root — flag it so the
                    // UI can warn that deep roots crowded out regular results.
                    self.capped_by_deep.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }

        self.progress.store(items.len(), Ordering::Relaxed);
        items
    }
}

fn index_entry(path: &Path, root: &Path, mnt: bool) -> Option<IndexedPath> {
    let meta = fs::symlink_metadata(path).ok()?;
    let is_dir = if meta.file_type().is_symlink() {
        // Follow the target so shortcuts to directories classify as folders.
        // Broken links resolve to nothing → drop the entry.
        fs::metadata(path).ok()?.is_dir()
    } else {
        meta.is_dir()
    };
    let name = path.file_name()?.to_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let depth = path
        .strip_prefix(root)
        .map(|p| p.components().count() as u16)
        .unwrap_or(path.components().count() as u16);
    Some(make_indexed(path.to_path_buf(), name, is_dir, depth, mnt))
}

pub(crate) fn make_indexed(
    path: PathBuf,
    name: String,
    is_dir: bool,
    depth: u16,
    mnt: bool,
) -> IndexedPath {
    let path_lower = path.to_string_lossy().to_lowercase();
    let low_value = is_low_value_path(&path_lower);
    let high_value = is_high_value_path(&path_lower, mnt, depth);
    IndexedPath {
        name_lower: name.to_lowercase(),
        name,
        path,
        path_lower,
        is_dir,
        depth,
        low_value,
        high_value,
        is_mnt: mnt,
    }
}

fn from_cache_entry(e: CacheEntry) -> Option<IndexedPath> {
    let path = PathBuf::from(e.path);
    let name = path.file_name()?.to_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(make_indexed(path, name, e.is_dir, e.depth, e.mnt))
}

fn is_low_value_path(path_lower: &str) -> bool {
    const BAD: &[&str] = &[
        "/.cache/",
        "/.gradle/",
        "/.m2/",
        "/.npm/",
        "/.cargo/",
        "/node_modules/",
        "/target/",
        "/bravesoftware/",
        "/.mozilla/",
        "/.steam/",
        "/__pycache__/",
        "/.pi/agent/sessions/",
        "/$recycle.bin/",
        "/system volume information/",
    ];
    BAD.iter().any(|b| path_lower.contains(b))
}

/// Cached `"$HOME/"` in lowercase (trailing slash). Computed once per process —
/// index build / cache load used to call `dirs::home_dir()` + `format!` per entry.
fn home_prefix_lower() -> Option<&'static str> {
    static HOME: OnceLock<Option<String>> = OnceLock::new();
    HOME.get_or_init(|| {
        dirs::home_dir().map(|home| {
            let mut s = home.to_string_lossy().to_lowercase();
            if !s.ends_with('/') {
                s.push('/');
            }
            s
        })
    })
    .as_deref()
}

fn is_high_value_path(path_lower: &str, mnt: bool, depth: u16) -> bool {
    if let Some(home_prefix) = home_prefix_lower() {
        if let Some(rest) = path_lower.strip_prefix(home_prefix) {
            if rest.matches('/').count() <= 1 {
                return true;
            }
            for p in [
                "projects/",
                "code/",
                "dev/",
                "src/",
                "documents/",
                "downloads/",
                "desktop/",
                "hark/",
                ".config/",
            ] {
                if rest.starts_with(p) {
                    return true;
                }
            }
        }
    }
    // Shallow entries on any mounted volume are high-value (`D:/`-style tops).
    if mnt && depth <= 2 {
        return true;
    }
    false
}

pub(crate) fn should_descend(path: &Path, root: &Path, excludes: &ExcludeSet) -> bool {
    if path == root {
        return true;
    }
    !should_skip_entry(path, excludes)
}

/// Shared with on-demand live deep search (must stay in sync with indexing).
pub(crate) fn should_skip_entry(path: &Path, excludes: &ExcludeSet) -> bool {
    if excludes.matches(path) || should_always_skip(path) {
        return true;
    }
    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
        if is_encoded_session_name(name) {
            return true;
        }
        // Machine-generated / transient entries — sensor dumps, download
        // fragments, counter-indexed frames, date-named recording dirs.
        // Human-authored files (finance.csv, shopping.csv, changelog.md) pass.
        if is_generated_filename(name) || is_generated_dirname(name) {
            return true;
        }
        if name == ".env"
            || name.starts_with(".env.")
            || name.ends_with(".env")
            || name == ".envrc"
            || name == ".env.local"
            || name == "credentials.json"
            || name == "secrets.json"
            // Private key material — never surface in launcher results.
            || name == "id_rsa"
            || name == "id_dsa"
            || name == "id_ecdsa"
            || name == "id_ed25519"
            || name.ends_with(".pem")
            || name == "private-keys-v1.d"
        {
            return true;
        }
        // Never index crypto material dirs. Allowlist only user-facing config trees.
        if name.starts_with('.') && path.is_dir() && !matches!(name, ".config" | ".local") {
            return true;
        }
    }
    false
}

fn should_always_skip(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        let name = s.as_ref();
        matches!(
            name,
            ".git"
                | ".svn"
                | ".hg"
                | ".ssh"
                | ".gnupg"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | "out"
                | ".cache"
                | ".npm"
                | ".yarn"
                | ".pnpm-store"
                | ".turbo"
                | ".next"
                | ".nuxt"
                | ".cargo"
                | ".rustup"
                | "__pycache__"
                | ".mypy_cache"
                | ".pytest_cache"
                | ".tox"
                | "venv"
                | ".venv"
                | "env"
                | ".idea"
                | ".vscode"
                | ".steam"
                | ".mozilla"
                | ".gradle"
                | ".m2"
                | "BraveSoftware"
                | "google-chrome"
                | "chromium"
                | "$RECYCLE.BIN"
                | "System Volume Information"
                | "pagefile.sys"
                | "hiberfil.sys"
                | "swapfile.sys"
                | "lost+found"
                | "Windows"
                | "Program Files"
                | "Program Files (x86)"
                | "ProgramData"
        ) || name.eq_ignore_ascii_case("$Recycle.Bin")
    })
}

pub(crate) fn is_encoded_session_name(name: &str) -> bool {
    name.starts_with("--") && name.ends_with("--") && name.len() > 4
}

/// Dir names that are almost always tool bulk output — prune the whole subtree.
/// (Sensor/ML/recording dumps: `artifacts`, `recordings`, `runs`, …)
const GENERATED_DIR_NAMES: &[&str] = &[
    "artifacts",
    "outputs",
    "runs",
    "checkpoints",
    "recordings",
    "frames",
    "dumps",
    "snapshots",
    "cache",
    "caches",
    "tmp",
    "temp",
    "backups",
    "trash",
    ".trash-1000",
    "captures",
    "sessions",
];

/// Filename words that mark a timestamped file as machine-generated, not
/// human-authored. Matched on whole-word boundaries so `changelog.md` (contains
/// "log") survives while `log_20260813.txt` is pruned.
const GENERATED_KEYWORDS: &[&str] = &[
    "test",
    "tmp",
    "temp",
    "snap",
    "snapshot",
    "frame",
    "capture",
    "record",
    "recording",
    "session",
    "run",
    "output",
    "log",
    "img",
    "image",
    "photo",
    "screen",
    "screenshot",
    "backup",
    "dump",
    "sample",
    "batch",
    "landmark",
    "artifact",
];

/// Camera / phone dumps (always timestamped): IMG_2026…, VID_2026…, PXL_…
const CAMERA_PREFIXES: &[&str] = &["img_", "vid_", "pxl_", "dsc_", "dscn_", "photo_", "video_"];

/// Date-shaped digit runs — the strongest machine-generated signal.
/// `20260403_112819_…` (8-digit), `2026_08_13`, `13-08-2026`.
/// Deliberately loose: it only ever *combines* with a keyword to prune files,
/// while date-named *dirs* are pruned outright (humans don't name folders by
/// pure timestamp).
fn has_datestamp(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut run = 0usize;
    for &b in bytes {
        if b.is_ascii_digit() {
            run += 1;
            if run >= 6 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    // Separated groups: `2026_08_13` / `13-08-2026` (year + month + day).
    let groups: Vec<&str> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|g| !g.is_empty())
        .collect();
    for w in groups.windows(3) {
        let (a, b, c) = (w[0].len(), w[1].len(), w[2].len());
        if (a == 4 && (b == 1 || b == 2) && (c == 1 || c == 2))
            || ((a == 1 || a == 2) && (b == 1 || b == 2) && c == 4)
        {
            return true;
        }
    }
    false
}

/// Whole-word `kw` presence in `s` (ASCII separators on both sides).
fn stem_has_keyword(s: &str, kw: &str) -> bool {
    let bytes = s.as_bytes();
    let mut from = 0;
    while let Some(rel) = s[from..].find(kw) {
        let i = from + rel;
        let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let after = i + kw.len();
        let after_ok = after == bytes.len() || !bytes[after].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = after;
    }
    false
}

fn is_generated_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // Camera / phone dumps are always timestamped.
    if CAMERA_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // Transient download / editor junk.
    if lower.ends_with(".part")
        || lower.ends_with(".tmp")
        || lower.ends_with(".crdownload")
        || lower.ends_with(".download")
        || lower.ends_with(".lock")
        || lower.ends_with(".swp")
        || lower.ends_with(".swo")
        || lower.ends_with('~')
        || lower == ".ds_store"
        || lower == "thumbs.db"
        || lower == "desktop.ini"
    {
        return true;
    }
    // Pure counter dumps: `000000.png`, `1234567.csv`.
    let stem = lower.split('.').next().unwrap_or(&lower);
    if !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    // Timestamped AND machine-y — `test_20_13082026.csv`, `frame_2026_08_13.png`.
    if !has_datestamp(stem) {
        return false;
    }
    GENERATED_KEYWORDS
        .iter()
        .any(|kw| stem_has_keyword(stem, kw))
}

fn is_generated_dirname(name: &str) -> bool {
    // Dirs almost never carry extensions; files do. A dotted name goes through
    // the filename rules instead, so `2026_08_13_notes.md` survives.
    if name.contains('.') {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if GENERATED_DIR_NAMES.contains(&lower.as_str()) {
        return true;
    }
    // Date-named dump dirs: `20260403_112819_46d7d6_eye_crops`, `runs_2026_08_13`.
    has_datestamp(&lower)
}

pub(crate) fn expand_user(q: &str) -> PathBuf {
    if let Some(rest) = q.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if q == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(q)
}

fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("hark/file-index.json")
}

fn meta_path() -> PathBuf {
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("hark/file-index.meta")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_ttl_stale() -> bool {
    match fs::read_to_string(meta_path()) {
        Ok(s) => meta_contents_stale(&s),
        // Meta unreadable (read-only/full cache dir): fall back to the cache
        // file's own mtime so one failed meta write doesn't force a full
        // rebuild walk every cycle (audit P3).
        Err(_) => cache_file_stale_fallback(),
    }
}

/// Pure TTL decision over meta contents (`version ts fingerprint`).
fn meta_contents_stale(s: &str) -> bool {
    let parts: Vec<&str> = s.split_whitespace().collect();
    // meta: version ts fingerprint
    if parts.len() < 2 {
        return true;
    }
    let Ok(ver) = parts[0].parse::<u32>() else {
        return true;
    };
    let Ok(ts) = parts[1].parse::<u64>() else {
        return true;
    };
    ver != CACHE_VERSION || now_secs().saturating_sub(ts) > INDEX_TTL_SECS
}

/// TTL decision from the cache file's own mtime; missing/odd mtime → stale.
fn cache_file_stale_fallback() -> bool {
    fs::metadata(cache_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs() > INDEX_TTL_SECS)
        .unwrap_or(true)
}

fn load_cache() -> Option<(Vec<IndexedPath>, String, bool)> {
    let data = fs::read(cache_path()).ok()?;
    let cf: CacheFile = serde_json::from_slice(&data).ok()?;
    if cf.version != CACHE_VERSION {
        return None;
    }
    let mut items = Vec::with_capacity(cf.items.len());
    for e in cf.items {
        if let Some(item) = from_cache_entry(e) {
            items.push(item);
        }
    }
    Some((items, cf.fingerprint, cf.capped_by_deep))
}

fn save_cache(items: &[IndexedPath], fingerprint: &str, capped_by_deep: bool) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let cf = CacheFile {
        version: CACHE_VERSION,
        fingerprint: fingerprint.to_string(),
        capped_by_deep,
        items: items
            .iter()
            .map(|i| CacheEntry {
                path: i.path.to_string_lossy().into_owned(),
                is_dir: i.is_dir,
                depth: i.depth,
                mnt: i.is_mnt,
            })
            .collect(),
    };
    // Compact JSON + atomic replace (no half-written cache on crash).
    // Meta stamp only after the rename lands: a failed write/rename must keep
    // the old meta so `cache_ttl_stale` still reports the cache as stale.
    if let Ok(data) = serde_json::to_vec(&cf) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, &data).is_ok() && fs::rename(&tmp, &path).is_ok() {
            let _ = fs::write(
                meta_path(),
                format!("{} {} {}", CACHE_VERSION, now_secs(), fingerprint),
            );
        }
    }
}

fn clear_cache() {
    let _ = fs::remove_file(cache_path());
    let _ = fs::remove_file(meta_path());
    let _ = fs::remove_file(cache_path().with_extension("json.tmp"));
}

#[cfg(feature = "bench")]
pub fn cache_bytes_on_disk() -> Option<u64> {
    fs::metadata(cache_path()).ok().map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExcludeSet;
    use std::path::Path;

    #[test]
    fn skips_ssh_gnupg_and_key_material() {
        let excludes = ExcludeSet::from_list(&[]);
        assert!(should_skip_entry(Path::new("/home/u/.ssh"), &excludes));
        assert!(should_skip_entry(Path::new("/home/u/.gnupg"), &excludes));
        assert!(should_skip_entry(
            Path::new("/home/u/.ssh/id_ed25519"),
            &excludes
        ));
        assert!(should_skip_entry(Path::new("/tmp/id_rsa"), &excludes));
        assert!(should_skip_entry(Path::new("/tmp/cert.pem"), &excludes));
        assert!(should_skip_entry(
            Path::new("/home/u/.gnupg/private-keys-v1.d"),
            &excludes
        ));
        // Still allow normal config trees and ordinary files.
        assert!(!should_skip_entry(Path::new("/home/u/.config"), &excludes));
        assert!(!should_skip_entry(Path::new("/home/u/.local"), &excludes));
        assert!(!should_skip_entry(
            Path::new("/home/u/notes.txt"),
            &excludes
        ));
    }

    #[test]
    fn human_files_survive_artifact_filter() {
        let excludes = ExcludeSet::from_list(&[]);
        assert!(!should_skip_entry(
            Path::new("/mnt/windows_d/finance.csv"),
            &excludes
        ));
        assert!(!should_skip_entry(
            Path::new("/mnt/windows_d/shopping.csv"),
            &excludes
        ));
        assert!(!should_skip_entry(
            Path::new("/home/u/changelog.md"),
            &excludes
        ));
        assert!(!should_skip_entry(
            Path::new("/home/u/report_2026.pdf"),
            &excludes
        ));
        assert!(!should_skip_entry(
            Path::new("/home/u/2026_08_13_notes.md"),
            &excludes
        ));
        assert!(!should_skip_entry(
            Path::new("/home/u/notes.txt"),
            &excludes
        ));
    }

    #[test]
    fn machine_dumps_pruned() {
        let excludes = ExcludeSet::from_list(&[]);
        // Date-named recording dirs (the Eye Tracker pattern) — whole subtree.
        assert!(should_skip_entry(
            Path::new("/mnt/windows_d/Eye Tracker/data/raw/20260403_112819_46d7d6_eye_crops"),
            &excludes
        ));
        assert!(should_skip_entry(
            Path::new(
                "/mnt/windows_d/Eye Tracker/data/raw/20260403_112819_46d7d6_landmark_features.csv"
            ),
            &excludes
        ));
        // Timestamped + tool keyword.
        assert!(should_skip_entry(
            Path::new("/home/u/test_20_13082026.csv"),
            &excludes
        ));
        assert!(should_skip_entry(
            Path::new("/home/u/frame_2026_08_13.png"),
            &excludes
        ));
        assert!(should_skip_entry(
            Path::new("/home/u/log_20260813.txt"),
            &excludes
        ));
        assert!(should_skip_entry(
            Path::new("/home/u/backup_20260813.zip"),
            &excludes
        ));
        // Transient download / editor junk.
        assert!(should_skip_entry(
            Path::new("/home/u/file.download"),
            &excludes
        ));
        assert!(should_skip_entry(Path::new("/home/u/file.part"), &excludes));
        assert!(should_skip_entry(Path::new("/home/u/notes.md~"), &excludes));
        // Camera dumps.
        assert!(should_skip_entry(
            Path::new("/home/u/IMG_20260813_123456.jpg"),
            &excludes
        ));
        // Pure counter files.
        assert!(should_skip_entry(
            Path::new("/mnt/windows_d/frames/000000.png"),
            &excludes
        ));
        // Generated tool dirs prune their whole subtree (the walk prunes them
        // at the dir boundary; here we match the dir name itself).
        assert!(should_skip_entry(
            Path::new("/mnt/windows_d/proj/runs"),
            &excludes
        ));
        assert!(should_skip_entry(
            Path::new("/mnt/windows_d/proj/artifacts"),
            &excludes
        ));
        assert!(should_skip_entry(
            Path::new("/mnt/windows_d/tmp"),
            &excludes
        ));
    }

    #[test]
    fn symlink_to_dir_indexes_as_folder() {
        let dir = std::env::temp_dir().join(format!("hark_index_test_{}", std::process::id()));
        let target = dir.join("real");
        let link = dir.join("shortcut");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::remove_file(&link).ok();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let item = index_entry(&link, &dir, false).expect("symlink dir should index");
        assert!(item.is_dir, "symlink to dir must classify as folder");
        assert_eq!(item.name, "shortcut");

        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn broken_symlink_not_indexed() {
        let dir = std::env::temp_dir().join(format!("hark_link_test_{}", std::process::id()));
        let link = dir.join("ghost");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::remove_file(&link).ok();
        std::os::unix::fs::symlink(dir.join("does_not_exist"), &link).unwrap();

        assert!(index_entry(&link, &dir, false).is_none());

        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mounts_snapshot_shares_arc_allocation() {
        // Audit P3 Pass 21: per-search snapshots must be `Arc` bumps, not
        // deep `Vec` copies — two consecutive snapshots share one allocation.
        let dir = std::env::temp_dir().join(format!("hark_cfg_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(ConfigStore::with_path(
            crate::config::HarkConfig::default(),
            dir.join("config.json"),
        ));
        let state = IndexState::new(store);
        let a = state
            .mounts
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let b = state
            .mounts
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        assert!(Arc::ptr_eq(&a, &b));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn meta_contents_ttl_decision() {
        // Audit P3: fresh meta → not stale; old version / old ts / garbage
        // → stale (previous inline behavior, now unit-pinned).
        let now = now_secs();
        assert!(!meta_contents_stale(&format!(
            "{CACHE_VERSION} {now} abc123"
        )));
        assert!(meta_contents_stale(&format!(
            "{} {} abc123",
            CACHE_VERSION,
            now.saturating_sub(INDEX_TTL_SECS + 1)
        )));
        assert!(meta_contents_stale(&format!(
            "{} {now} abc123",
            CACHE_VERSION + 1
        )));
        assert!(meta_contents_stale("garbage"));
        assert!(meta_contents_stale("8"));
    }
}
