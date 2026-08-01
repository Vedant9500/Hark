mod hot;
mod index;
mod live_cache;
mod search;

use crate::config::{pretty_path, ConfigStore, ExcludeSet};
use crate::providers::{Action, ResultKind, SearchResult};
use crate::usage::UsageStore;
use fuzzy_matcher::skim::SkimMatcherV2;
use gio::prelude::*;
use hot::HotPaths;
#[cfg(feature = "bench")]
pub use index::cache_bytes_on_disk;
use index::IndexState;
pub use index::MAX_INDEX;
use live_cache::LiveCache;
pub use search::{is_path_glob_query, is_scoped_file_query, DeepMode};

/// If `q` starts with `f`/`file`/`folder` + whitespace (ASCII case-insensitive),
/// return the remainder (may be empty). Shared by the engine (force-files gate)
/// and the live-cache key normalizer so `File foo` and `file foo` map to the
/// same cache key.
pub fn strip_force_files_prefix(q: &str) -> Option<&str> {
    let bytes = q.as_bytes();
    // Match longest prefix first.
    for pref in ["folder", "file", "f"] {
        let pb = pref.as_bytes();
        if bytes.len() >= pb.len() && bytes[..pb.len()].eq_ignore_ascii_case(pb) {
            let rest = &q[pb.len()..];
            if rest.is_empty() {
                return Some(rest);
            }
            // Require whitespace after the keyword so `firefox` is not force-files.
            let b0 = rest.as_bytes()[0];
            if b0 == b' ' || b0 == b'\t' {
                return Some(rest.trim_start());
            }
        }
    }
    None
}
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

pub struct FileProvider {
    state: IndexState,
    matcher: SkimMatcherV2,
    live_cache: LiveCache,
    /// Frequently opened paths ∩ index (free-text phase-1). See `hot` module.
    hot: HotPaths,
    /// One-slot memo: normalized query → is confident scoped `in` (avoids double index parse).
    scoped_memo: Mutex<Option<(String, bool)>>,
}

#[derive(Debug, Clone, Copy)]
pub struct IndexProgress {
    pub count: usize,
    pub running: bool,
    pub capped: bool,
    /// Cap was hit while walking pinned deep roots (warn in UI).
    pub capped_by_deep: bool,
    pub max: usize,
}

impl FileProvider {
    pub fn new_empty(config: Arc<ConfigStore>, usage: Arc<UsageStore>) -> Self {
        Self {
            state: IndexState::new(config),
            matcher: SkimMatcherV2::default().ignore_case(),
            live_cache: LiveCache::new(),
            hot: HotPaths::new(usage),
            scoped_memo: Mutex::new(None),
        }
    }

    pub fn index_progress(&self) -> IndexProgress {
        let running = self.state.indexing.load(Ordering::Relaxed);
        let progress = self.state.progress.load(Ordering::Relaxed);
        let stored = self.state.index.read().unwrap_or_else(|p| p.into_inner()).len();
        IndexProgress {
            count: if running { progress } else { stored },
            running,
            capped: self.state.capped.load(Ordering::Relaxed),
            capped_by_deep: self.state.capped_by_deep.load(Ordering::Relaxed),
            max: MAX_INDEX,
        }
    }

    pub fn rebuild_index(&self) {
        self.state.ensure_fresh();
        self.refresh_hot();
    }

    pub fn force_rebuild(&self) {
        self.state.force_rebuild();
        self.refresh_hot();
    }

    /// Mark hot set stale (e.g. after recording a path open).
    pub fn clear_live_cache(&self) {
        self.live_cache.clear();
    }

    pub fn note_usage_changed(&self) {
        self.hot.mark_dirty();
    }

    /// Hot set size (for bench / diagnostics).
    #[allow(dead_code)]
    pub fn hot_len(&self) -> usize {
        self.hot.len()
    }

    fn refresh_hot(&self) {
        let index = self.state.index.read().unwrap_or_else(|p| p.into_inner());
        self.hot.rebuild(&index);
    }

    pub fn resolve_path(&self, path: &Path) -> Option<SearchResult> {
        // One syscall (not exists() + is_dir()). Missing paths drop out.
        let meta = std::fs::symlink_metadata(path).ok()?;
        let is_dir = meta.is_dir();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let path_style = self.state.config.with(|c| c.index.path_style.clone());
        let mounts = self
            .state
            .mounts
            .read()
            .unwrap_or_else(|p| p.into_inner());
        Some(SearchResult {
            id: format!("path:{}", path.display()),
            title: name,
            subtitle: pretty_path(path, &path_style, &mounts),
            kind: if is_dir {
                ResultKind::Folder
            } else {
                ResultKind::File
            },
            score: 0,
            icon: Some(icon_for_path(path, is_dir).into()),
            action: Action::OpenPath(path.to_path_buf()),
            conversion: None,
        })
    }
}

impl FileProvider {
    /// Index-only (or sync/async deep). Main UI uses `DeepMode::Skip` + async deep.
    ///
    /// When `deep == Skip`, any live-cache hits for this query are merged in so
    /// retypes stay instant without re-walking.
    pub fn search_with(&self, query: &str, allow_fuzzy: bool, deep: DeepMode) -> Vec<SearchResult> {
        // Full cache hit for a deep mode: skip the walk entirely.
        if deep != DeepMode::Skip {
            if let Some(cached) = self.live_cache.get(query) {
                return cached.to_vec();
            }
        }

        // Cheap Arc config + mounts snapshot; index lock only for scan/plan.
        let cfg = self.state.config.snapshot();
        let mounts = self.state.mounts.read().unwrap_or_else(|p| p.into_inner()).clone();
        let excludes = ExcludeSet::from_list(&cfg.index.exclude);

        // Phase 1: index-only search under a short read lock (no WalkDir).
        let (mut results, deep_jobs) = {
            let index = self.state.index.read().unwrap_or_else(|p| p.into_inner());
            self.hot.ensure_fresh(&index);
            let hot_indices = self.hot.snapshot_indices();
            let results = search::search_index(
                &index,
                query,
                &cfg.index.path_style,
                &mounts,
                &excludes,
                &self.matcher,
                allow_fuzzy,
                &hot_indices,
            );
            let jobs = if deep != DeepMode::Skip {
                search::plan_deep_jobs(
                    &index,
                    query,
                    &results,
                    deep,
                    &cfg.index.deep_roots,
                    &mounts,
                )
            } else {
                Vec::new()
            };
            (results, jobs)
        }; // index RwLock released before any live walk

        // Phase 2: live deep walks without holding the index lock.
        if !deep_jobs.is_empty() {
            search::run_deep_jobs(
                deep_jobs,
                &cfg.index.path_style,
                &mounts,
                &excludes,
                &mut results,
            );
        }

        if deep != DeepMode::Skip {
            // Move into Arc cache once; return a Vec clone of the shared slice
            // (avoids holding two full owned Vecs like `put(results.clone())`).
            return self.live_cache.put(query, results);
        }
        if let Some(cached) = self.live_cache.get(query) {
            // UI path: merge previous live hits into index-only results.
            merge_cached(&mut results, cached.as_ref());
        }
        results
    }

    /// Whether async deep is worth scheduling for this query + current index hits.
    pub fn should_deep_search(&self, query: &str, index_results: &[SearchResult]) -> bool {
        if self.live_cache.contains(query) {
            // Cache already has live hits; no need to re-walk.
            return false;
        }
        // Soft folder hints after ` in ` — never schedule a deep walk.
        if index_results
            .iter()
            .any(|r| r.id.starts_with("scope-hint:"))
        {
            return false;
        }
        // Scoped `in` (cheap path first; memoized index-aware for bare folders).
        if is_scoped_file_query(query) || self.is_scoped_query(query) {
            return !search::index_results_are_strong(index_results);
        }
        search::should_deep_search(query, index_results)
    }

    /// Confident `name in scope` parse (uses index for bare folder scopes).
    /// Memoized for the last query so engine + deep-gating share one index parse.
    pub fn is_scoped_query(&self, query: &str) -> bool {
        let memo_key = LiveCache::key_for(query);
        if memo_key.is_empty() {
            return false;
        }
        if let Some((k, v)) = self.scoped_memo.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            if *k == memo_key {
                return *v;
            }
        }
        let v = if is_scoped_file_query(query) {
            true
        } else {
            let raw = query.trim();
            let q = raw
                .strip_prefix("f ")
                .or_else(|| raw.strip_prefix("file "))
                .or_else(|| raw.strip_prefix("folder "))
                .unwrap_or(raw)
                .trim();
            let index = self.state.index.read().unwrap_or_else(|p| p.into_inner());
            search::parse_scoped_for_query(q, &index).is_some()
        };
        *self.scoped_memo.lock().unwrap_or_else(|p| p.into_inner()) = Some((memo_key, v));
        v
    }
}

fn merge_cached(base: &mut Vec<SearchResult>, cached: &[SearchResult]) {
    if cached.is_empty() {
        return;
    }
    // Owned ids only: cannot store `&str` into base while also pushing (reallocation).
    // `contains` + conditional insert avoids cloning the id on duplicate hits.
    let mut seen: std::collections::HashSet<String> =
        base.iter().map(|r| r.id.clone()).collect();
    for r in cached {
        if !seen.contains(&r.id) {
            seen.insert(r.id.clone());
            base.push(r.clone());
        }
    }
    base.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    base.truncate(25);
}

/// Expand `~` for settings / promote UI.
pub fn expand_user_path(q: &str) -> PathBuf {
    index::expand_user(q)
}

/// Theme icon name for a path based on directory flag / file extension.
///
/// Uses FreeDesktop / Papirus-friendly names (specific first, generic fallback).
pub fn icon_for_path(path: &Path, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        // Images — specific mimetypes resolve better in Papirus
        "png" => "image-png",
        "jpg" | "jpeg" => "image-jpeg",
        "gif" => "image-gif",
        "svg" => "image-svg+xml",
        "webp" => "image-webp",
        "bmp" => "image-bmp",
        "ico" => "image-x-ico",
        "tif" | "tiff" => "image-tiff",
        "heic" | "heif" => "image-heif",
        "avif" => "image-avif",
        "jxl" => "image-jpegxl",
        // Video
        "mp4" => "video-mp4",
        "webm" => "video-webm",
        "mkv" => "video-x-matroska",
        "mov" => "video-quicktime",
        "avi" => "video-x-msvideo",
        "m4v" | "wmv" | "flv" | "mpeg" | "mpg" => "video-x-generic",
        // Audio
        "mp3" => "audio-mpeg",
        "flac" => "audio-x-flac",
        "ogg" | "opus" => "audio-x-generic",
        "wav" => "audio-x-wav",
        "m4a" | "aac" => "audio-aac",
        "wma" | "aiff" => "audio-x-generic",
        // Documents
        "pdf" => "application-pdf",
        "doc" | "docx" => "x-office-document",
        "odt" | "rtf" | "epub" => "x-office-document",
        "txt" => "text-plain",
        "md" => "text-markdown",
        "log" => "text-x-log",
        "xls" | "xlsx" | "csv" => "x-office-spreadsheet",
        "ppt" | "pptx" => "x-office-presentation",
        // Archives
        "zip" => "application-zip",
        "tar" => "application-x-tar",
        "gz" | "tgz" => "application-gzip",
        "bz2" => "application-x-bzip",
        "xz" | "7z" | "rar" | "zst" => "package-x-generic",
        // Code / config — Papirus has many of these
        "rs" => "text-x-rust",
        "py" => "text-x-python",
        "js" | "mjs" | "cjs" => "text-x-javascript",
        "ts" => "text-x-typescript",
        "tsx" | "jsx" => "text-x-javascript",
        "go" => "text-x-go",
        "c" => "text-x-csrc",
        "h" => "text-x-chdr",
        "cpp" | "cc" | "cxx" => "text-x-c++src",
        "hpp" | "hh" => "text-x-c++hdr",
        "java" => "text-x-java",
        "kt" => "text-x-kotlin",
        "swift" => "text-x-swift",
        "rb" => "text-x-ruby",
        "php" => "text-x-php",
        "sh" | "bash" | "zsh" => "text-x-script",
        "html" | "htm" => "text-html",
        "css" => "text-css",
        "scss" => "text-x-scss",
        "json" => "application-json",
        "xml" => "text-xml",
        "toml" => "text-x-toml",
        "yaml" | "yml" => "text-x-yaml",
        "vue" => "text-x-vue",
        "svelte" => "text-x-svelte",
        "lua" => "text-x-lua",
        "sql" => "text-x-sql",
        _ => "text-x-generic",
    }
}

/// Open `path`, honoring Blink per-category overrides when provided.
/// Pass `None` / empty overrides to fall back to `xdg-open`.
pub fn open_path_with(
    path: &Path,
    open_with: Option<&crate::config::OpenWithConfig>,
) -> Result<(), String> {
    if let Some(cfg) = open_with {
        if let Some(cat) = crate::config::FileOpenCategory::from_path(path) {
            if let Some(desktop_id) = cfg.get(cat) {
                if launch_with_desktop_id(desktop_id, path) {
                    return Ok(());
                }
            }
        }
    }
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("could not open {} with xdg-open: {err}", path.display()))
}

/// Launch a `.desktop` app with `path` as a file argument.
/// Returns true if a launch was attempted successfully.
pub fn launch_with_desktop_id(desktop_id: &str, path: &Path) -> bool {
    let id = desktop_id.trim();
    if id.is_empty() {
        return false;
    }
    // GDesktopAppInfo::new wants the desktop file id (usually ends with .desktop).
    let candidates = if id.ends_with(".desktop") {
        vec![id.to_string()]
    } else {
        vec![format!("{id}.desktop"), id.to_string()]
    };

    for cand in candidates {
        if let Some(info) = gio::DesktopAppInfo::new(&cand) {
            let file = gio::File::for_path(path);
            let files = [file];
            if info.launch(&files, None::<&gio::AppLaunchContext>).is_ok() {
                return true;
            }
        }
        // Fallback: absolute path to a .desktop file
        let p = Path::new(&cand);
        if p.is_file() {
            if let Some(info) = gio::DesktopAppInfo::from_filename(p) {
                let file = gio::File::for_path(path);
                let files = [file];
                if info.launch(&files, None::<&gio::AppLaunchContext>).is_ok() {
                    return true;
                }
            }
        }
    }

    // Last resort: resolve via our app list is handled by caller; try gio default
    // for the desktop id stem by scanning common dirs is expensive — fall through.
    false
}

/// Resolve a friendly display name for a stored desktop id (settings UI).
pub fn desktop_id_display_name(desktop_id: &str) -> Option<String> {
    let id = desktop_id.trim();
    if id.is_empty() {
        return None;
    }
    let candidates = if id.ends_with(".desktop") {
        vec![id.to_string()]
    } else {
        vec![format!("{id}.desktop"), id.to_string()]
    };
    for cand in candidates {
        if let Some(info) = gio::DesktopAppInfo::new(&cand) {
            let name = info.name();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
        let p = Path::new(&cand);
        if p.is_file() {
            if let Some(info) = gio::DesktopAppInfo::from_filename(p) {
                let name = info.name();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Reveal `path` in the default file manager, selecting it when the manager supports it.
pub fn reveal_in_file_manager(path: &Path) {
    let path = if path.exists() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent().filter(|p| p.exists()) {
        parent.to_path_buf()
    } else {
        eprintln!("blink: reveal: path missing: {}", path.display());
        return;
    };

    // FreeDesktop FileManager1 — works even when Dolphin is already open as a
    // daemon, and is what most desktops implement for "show in folder".
    if reveal_via_file_manager1(&path) {
        return;
    }

    // KDE Dolphin (common on this host)
    if which_bin("dolphin").is_some() {
        spawn_detached("dolphin", &["--select", &path.to_string_lossy()]);
        return;
    }
    // GNOME Nautilus
    if which_bin("nautilus").is_some() {
        spawn_detached("nautilus", &["--select", &path.to_string_lossy()]);
        return;
    }
    // Elementary Files / Pantheon
    if which_bin("io.elementary.files").is_some() {
        spawn_detached("io.elementary.files", &[&path.to_string_lossy()]);
        return;
    }
    // Fallback: open containing folder (or the folder itself).
    let target = if path.is_dir() {
        path.as_path()
    } else {
        path.parent().unwrap_or(path.as_path())
    };
    spawn_detached("xdg-open", &[&target.to_string_lossy()]);
}

/// `org.freedesktop.FileManager1.ShowItems` — select file(s) in the default manager.
fn reveal_via_file_manager1(path: &Path) -> bool {
    let file = gio::File::for_path(path);
    let uri = file.uri();
    let bus = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(b) => b,
        Err(err) => {
            eprintln!("blink: reveal: session bus: {}", err.message());
            return false;
        }
    };
    // ShowItems(as uris, s startup_id)
    let result = bus.call_sync(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        Some(&(vec![uri.to_string()], "").to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
    );
    match result {
        Ok(_) => true,
        Err(err) => {
            eprintln!("blink: reveal: FileManager1.ShowItems: {}", err.message());
            false
        }
    }
}

/// Move `path` to the FreeDesktop trash. Returns `Ok(())` on success.
pub fn trash_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("Path no longer exists".into());
    }
    // `gio trash` is the portable FreeDesktop path (gvfs).
    let status = Command::new("gio")
        .args(["trash", "--"])
        .arg(path)
        .status()
        .map_err(|e| format!("gio trash failed to start: {e}"))?;
    if status.success() {
        return Ok(());
    }
    // Older hosts may only ship gvfs-trash.
    if which_bin("gvfs-trash").is_some() {
        let status = Command::new("gvfs-trash")
            .arg(path)
            .status()
            .map_err(|e| format!("gvfs-trash failed to start: {e}"))?;
        if status.success() {
            return Ok(());
        }
    }
    Err(format!(
        "Could not trash {} (gio exit {:?})",
        path.display(),
        status.code()
    ))
}

fn spawn_detached(bin: &str, args: &[&str]) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detach so the file manager outlives blink's short-lived helper spawns.
    let quoted: Vec<String> = std::iter::once(bin.to_string())
        .chain(args.iter().map(|a| shell_quote(Path::new(a))))
        .collect();
    let shell_cmd = quoted.join(" ");
    let mut sh = Command::new("sh");
    sh.arg("-c")
        .arg(format!(
            "setsid -f {shell_cmd} >/dev/null 2>&1 || nohup {shell_cmd} >/dev/null 2>&1 &"
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if sh.spawn().is_err() && cmd.spawn().is_err() {
        eprintln!("blink: could not spawn {bin}");
    }
}

pub fn open_terminal_at(path: &Path) -> Result<(), String> {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"))
    };
    if !dir.is_dir() {
        return Err(format!("terminal: {} is not a directory", dir.display()));
    }

    let term = std::env::var("TERMINAL")
        .ok()
        .filter(|t| which_bin(t).is_some())
        .or_else(|| which_bin("alacritty").map(|_| "alacritty".into()))
        .or_else(|| which_bin("kitty").map(|_| "kitty".into()))
        .or_else(|| which_bin("foot").map(|_| "foot".into()))
        .or_else(|| which_bin("ghostty").map(|_| "ghostty".into()))
        .or_else(|| which_bin("wezterm").map(|_| "wezterm".into()))
        .unwrap_or_else(|| "xterm".into());

    // Resolve binary path; match on basename so `/usr/bin/kitty` still works.
    let term_path = which_bin(&term).unwrap_or_else(|| PathBuf::from(&term));
    let term_name = terminal_basename(&term_path, &term);
    let dir_arg = dir.to_string_lossy().into_owned();

    match term_name.as_str() {
        "alacritty" => spawn_detached_in_dir(
            &term_path,
            &["--working-directory".into(), dir_arg],
            &dir,
        ),
        "kitty" => spawn_detached_in_dir(&term_path, &["--directory".into(), dir_arg], &dir),
        "foot" => spawn_detached_in_dir(
            &term_path,
            &[format!("--working-directory={dir_arg}")],
            &dir,
        ),
        "ghostty" => spawn_detached_in_dir(
            &term_path,
            &[format!("--working-directory={dir_arg}")],
            &dir,
        ),
        "wezterm" => spawn_detached_in_dir(
            &term_path,
            &["start".into(), "--cwd".into(), dir_arg],
            &dir,
        ),
        "xterm" | "uxterm" => {
            // xterm has no cwd flag; use argv-form -e so the path is never shell-interpolated.
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            spawn_detached_in_dir(
                &term_path,
                &[
                    "-e".into(),
                    "sh".into(),
                    "-c".into(),
                    r#"cd "$1" && exec "$2""#.into(),
                    "sh".into(),
                    dir_arg,
                    shell,
                ],
                &dir,
            )
        }
        // Unknown / custom TERMINAL: honor the binary and set process cwd.
        _ => spawn_detached_in_dir(&term_path, &[], &dir),
    }
}

/// Detach `bin args...` with an optional working directory. Argv only — no `sh -c`.
fn spawn_detached_in_dir(bin: &Path, args: &[String], dir: &Path) -> Result<(), String> {
    let mut cmd = Command::new("setsid");
    cmd.arg("-f")
        .arg(bin)
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if cmd.spawn().is_ok() {
        return Ok(());
    }
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn()
        .map(|_| ())
        .map_err(|err| format!("could not launch {}: {err}", bin.display()))
}

/// Basename used to pick terminal CLI flags (`/usr/bin/kitty` → `kitty`).
fn terminal_basename(term_path: &Path, fallback: &str) -> String {
    term_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(fallback)
        .to_ascii_lowercase()
}

fn which_bin(bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        let p = PathBuf::from(bin);
        return if p.exists() { Some(p) } else { None };
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod open_terminal_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn terminal_basename_strips_path() {
        assert_eq!(
            terminal_basename(Path::new("/usr/bin/alacritty"), "unused"),
            "alacritty"
        );
        assert_eq!(
            terminal_basename(Path::new("/usr/local/bin/kitty"), "unused"),
            "kitty"
        );
        assert_eq!(terminal_basename(Path::new("foot"), "foot"), "foot");
        // Custom binary names fall through to cwd-based launch (not xterm).
        assert_eq!(
            terminal_basename(Path::new("/opt/myterm/bin/myterm"), "myterm"),
            "myterm"
        );
    }
}
