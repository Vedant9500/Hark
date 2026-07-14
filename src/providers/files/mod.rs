mod index;
mod live_cache;
mod search;

use crate::config::{pretty_path, ConfigStore};
use crate::providers::{Action, Provider, ResultKind, SearchResult};
use fuzzy_matcher::skim::SkimMatcherV2;
pub use index::{cache_bytes_on_disk, MAX_INDEX};
use index::IndexState;
use live_cache::LiveCache;
pub use search::{is_path_glob_query, is_scoped_file_query, DeepMode};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct FileProvider {
    state: IndexState,
    matcher: SkimMatcherV2,
    live_cache: LiveCache,
}

#[derive(Debug, Clone, Copy)]
pub struct IndexProgress {
    pub count: usize,
    pub running: bool,
    pub capped: bool,
    pub max: usize,
}

impl FileProvider {
    pub fn new_empty(config: Arc<ConfigStore>) -> Self {
        Self {
            state: IndexState::new(config),
            matcher: SkimMatcherV2::default().ignore_case(),
            live_cache: LiveCache::new(),
        }
    }

    pub fn index_progress(&self) -> IndexProgress {
        let running = self.state.indexing.load(Ordering::Relaxed);
        let progress = self.state.progress.load(Ordering::Relaxed);
        let stored = self.state.index.read().unwrap().len();
        IndexProgress {
            count: if running { progress } else { stored },
            running,
            capped: self.state.capped.load(Ordering::Relaxed),
            max: MAX_INDEX,
        }
    }

    pub fn rebuild_index(&self) {
        self.state.ensure_fresh();
    }

    pub fn force_rebuild(&self) {
        self.state.force_rebuild();
    }

    pub fn resolve_path(&self, path: &Path) -> Option<SearchResult> {
        if !path.exists() {
            return None;
        }
        let is_dir = path.is_dir();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let cfg = self.state.config.get();
        let mounts = self.state.mounts.read().unwrap();
        Some(SearchResult {
            id: format!("path:{}", path.display()),
            title: name,
            subtitle: pretty_path(path, &cfg.index.path_style, &mounts),
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
    pub fn search_with(
        &self,
        query: &str,
        allow_fuzzy: bool,
        deep: DeepMode,
    ) -> Vec<SearchResult> {
        // Full cache hit for a deep mode: skip the walk entirely.
        if deep != DeepMode::Skip {
            if let Some(cached) = self.live_cache.get(query) {
                return cached;
            }
        }

        let index = self.state.index.read().unwrap();
        let cfg = self.state.config.get();
        let mounts = self.state.mounts.read().unwrap();
        let mut results = search::search_index(
            &index,
            query,
            &cfg.index.path_style,
            &mounts,
            &cfg.index.exclude,
            &self.matcher,
            allow_fuzzy,
            deep,
            &cfg.index.deep_roots,
        );
        drop(mounts);
        drop(index);

        if deep != DeepMode::Skip {
            // Store full file-result set so retypes / async re-runs skip the walk.
            self.live_cache.put(query, results.clone());
        } else if let Some(cached) = self.live_cache.get(query) {
            // UI path: merge previous live hits into index-only results.
            merge_cached(&mut results, cached);
        }
        results
    }

    /// Whether async deep is worth scheduling for this query + current index hits.
    pub fn should_deep_search(&self, query: &str, index_results: &[SearchResult]) -> bool {
        if self.live_cache.get(query).is_some() {
            // Cache already has live hits; no need to re-walk.
            return false;
        }
        // Index-aware scoped `in` queries (folder-name scopes need the index).
        if self.is_scoped_query(query) {
            return !search::index_results_are_strong(index_results);
        }
        search::should_deep_search(query, index_results)
    }

    /// Confident `name in scope` parse (uses index for bare folder scopes).
    pub fn is_scoped_query(&self, query: &str) -> bool {
        if is_scoped_file_query(query) {
            return true;
        }
        let raw = query.trim();
        let q = raw
            .strip_prefix("f ")
            .or_else(|| raw.strip_prefix("file "))
            .or_else(|| raw.strip_prefix("folder "))
            .unwrap_or(raw)
            .trim();
        let index = self.state.index.read().unwrap();
        search::parse_scoped_for_query(q, &index).is_some()
    }
}

impl Provider for FileProvider {
    fn search(&self, query: &str) -> Vec<SearchResult> {
        // Default provider path: sync deep (bench / isolated). UI uses Engine.
        self.search_with(query, true, DeepMode::Sync)
    }
}

fn merge_cached(base: &mut Vec<SearchResult>, cached: Vec<SearchResult>) {
    if cached.is_empty() {
        return;
    }
    let mut seen: std::collections::HashSet<String> =
        base.iter().map(|r| r.id.clone()).collect();
    for r in cached {
        if seen.insert(r.id.clone()) {
            base.push(r);
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

pub fn open_path(path: &Path) {
    let _ = Command::new("xdg-open").arg(path).spawn();
}

pub fn open_terminal_at(path: &Path) {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"))
    };
    if !dir.is_dir() {
        return;
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

    let shell_cmd = match term.as_str() {
        "alacritty" => format!("alacritty --working-directory {}", shell_quote(&dir)),
        "kitty" => format!("kitty --directory {}", shell_quote(&dir)),
        "foot" => format!("foot --working-directory={}", shell_quote(&dir)),
        "ghostty" => format!("ghostty --working-directory={}", shell_quote(&dir)),
        "wezterm" => format!("wezterm start --cwd {}", shell_quote(&dir)),
        _ => format!(
            "xterm -e sh -c 'cd {} && exec $SHELL'",
            shell_quote(&dir)
        ),
    };

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "setsid -f {shell_cmd} >/dev/null 2>&1 || nohup {shell_cmd} >/dev/null 2>&1 &"
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = cmd.spawn();
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
