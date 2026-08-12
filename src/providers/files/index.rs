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
pub(crate) const CACHE_VERSION: u32 = 6;

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
    pub mounts: RwLock<Vec<MountInfo>>,
    fingerprint: RwLock<String>,
    /// Ensures only one filesystem walk/cache write runs at a time.
    build_lock: Mutex<()>,
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
            mounts: RwLock::new(discover_mounts()),
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
                *self.mounts.write().unwrap_or_else(|p| p.into_inner()) = discover_mounts();
                let fp = self.compute_fingerprint();
                if !mem_fp.is_empty() && mem_fp == fp {
                    return;
                }
                self.run_build(fp, false);
                return;
            }
        }

        *self.mounts.write().unwrap_or_else(|p| p.into_inner()) = discover_mounts();
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
        *self.mounts.write().unwrap_or_else(|p| p.into_inner()) = discover_mounts();
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
        let mounts = self.mounts.read().unwrap_or_else(|p| p.into_inner());
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
        for m in &mounts {
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
                if let Some(item) = index_entry(&path, root) {
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

fn index_entry(path: &Path, root: &Path) -> Option<IndexedPath> {
    let meta = fs::symlink_metadata(path).ok()?;
    let is_dir = meta.is_dir();
    let name = path.file_name()?.to_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let depth = path
        .strip_prefix(root)
        .map(|p| p.components().count() as u16)
        .unwrap_or(path.components().count() as u16);
    Some(make_indexed(path.to_path_buf(), name, is_dir, depth))
}

pub(crate) fn make_indexed(path: PathBuf, name: String, is_dir: bool, depth: u16) -> IndexedPath {
    let path_lower = path.to_string_lossy().to_lowercase();
    let low_value = is_low_value_path(&path_lower);
    let high_value = is_high_value_path(&path_lower);
    let is_mnt = path_lower.starts_with("/mnt/");
    IndexedPath {
        name_lower: name.to_lowercase(),
        name,
        path,
        path_lower,
        is_dir,
        depth,
        low_value,
        high_value,
        is_mnt,
    }
}

fn from_cache_entry(e: CacheEntry) -> Option<IndexedPath> {
    let path = PathBuf::from(e.path);
    let name = path.file_name()?.to_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(make_indexed(path, name, e.is_dir, e.depth))
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

fn is_high_value_path(path_lower: &str) -> bool {
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
    if path_lower.starts_with("/mnt/") {
        let rest = path_lower.trim_start_matches("/mnt/");
        if rest.matches('/').count() <= 2 {
            return true;
        }
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
    let Ok(s) = fs::read_to_string(meta_path()) else {
        return true;
    };
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
            })
            .collect(),
    };
    // Compact JSON + atomic replace (no half-written cache on crash).
    if let Ok(data) = serde_json::to_vec(&cf) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, &data).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
        let _ = fs::write(
            meta_path(),
            format!("{} {} {}", CACHE_VERSION, now_secs(), fingerprint),
        );
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
}
