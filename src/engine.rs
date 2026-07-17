use crate::config::ConfigStore;
use crate::providers::apps::AppProvider;
use crate::providers::calc::CalcProvider;
use crate::providers::files::{FileProvider, IndexProgress};
use crate::providers::translate::TranslateProvider;
use crate::providers::{Action, ResultKind, SearchResult};
use crate::usage::UsageStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct Engine {
    apps: Arc<AppProvider>,
    files: Arc<FileProvider>,
    calc: Arc<CalcProvider>,
    translate: Arc<TranslateProvider>,
    usage: Arc<UsageStore>,
    config: Arc<ConfigStore>,
}

impl Engine {
    /// Full daemon engine: warm apps/index on a bg thread + 45m periodic refresh.
    pub fn new() -> Self {
        let engine = Self::new_headless();
        engine.spawn_warm();
        engine.spawn_periodic_refresh();
        engine
    }

    /// CLI / bench: same providers, **no** eternal periodic thread.
    /// Still warms apps + index once on a background thread (needed for search).
    pub fn new_headless() -> Self {
        let config = Arc::new(ConfigStore::load());
        let usage = Arc::new(UsageStore::load());
        let apps = Arc::new(AppProvider::new_empty());
        let files = Arc::new(FileProvider::new_empty(config.clone(), usage.clone()));
        let calc = Arc::new(CalcProvider::new());
        let translate = Arc::new(TranslateProvider::new(config.clone()));

        // Currency rates: use on-disk cache only at boot. Network fetch is deferred
        // until an FX conversion is actually requested (see FxStore::convert) so
        // idle daemons do not wake radios / burn CPU on curl.

        Self {
            apps,
            files,
            calc,
            translate,
            usage,
            config,
        }
    }

    /// One-shot warm (apps + file index). Used by headless CLI and daemon boot.
    pub fn spawn_warm(&self) {
        let apps_bg = self.apps.clone();
        let files_bg = self.files.clone();
        thread::spawn(move || {
            apps_bg.reload();
            files_bg.rebuild_index();
        });
    }

    /// Long-lived 45m apps reload + files `ensure_fresh` (daemon only).
    pub fn spawn_periodic_refresh(&self) {
        let files_periodic = self.files.clone();
        let apps_periodic = self.apps.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(45 * 60));
            apps_periodic.reload();
            files_periodic.rebuild_index();
        });
    }

    pub fn config(&self) -> Arc<ConfigStore> {
        self.config.clone()
    }

    /// Installed GUI apps for Settings pickers (excludes terminal-only).
    pub fn list_apps_for_picker(&self) -> Vec<crate::providers::apps::AppPickEntry> {
        self.apps.list_for_picker()
    }

    /// Friendly name for a stored desktop id, if resolvable.
    pub fn app_display_name(&self, desktop_id: &str) -> Option<String> {
        self.apps.display_name_for_desktop_id(desktop_id)
    }

    pub fn index_progress(&self) -> IndexProgress {
        self.files.index_progress()
    }

    /// Always off the UI thread.
    pub fn force_reindex(&self) {
        let files = self.files.clone();
        thread::spawn(move || {
            files.force_rebuild();
        });
    }

    /// Blocking rebuild for `blink --bench` only (not used by UI).
    #[cfg(feature = "bench")]
    pub fn bench_force_reindex_blocking(&self) {
        self.files.force_rebuild();
    }

    #[cfg(feature = "bench")]
    pub fn index_cache_bytes(&self) -> Option<u64> {
        crate::providers::files::cache_bytes_on_disk()
    }

    /// Loaded desktop app count (for `blink --bench` readiness).
    #[cfg(feature = "bench")]
    pub fn apps_len(&self) -> usize {
        self.apps.len()
    }

    /// Isolated provider search — index only (no live deep / live cache).
    #[cfg(feature = "bench")]
    pub fn search_files_index_only(&self, query: &str) -> Vec<SearchResult> {
        use crate::providers::files::DeepMode;
        self.files.search_with(query, true, DeepMode::Skip)
    }

    pub fn format_index_status(&self) -> String {
        let p = self.index_progress();
        if p.running {
            format!("Indexing… {} files", format_int(p.count))
        } else if p.capped {
            format!(
                "Index: {} items · cap reached (max {})",
                format_int(p.count),
                format_int(p.max)
            )
        } else {
            format!("Index: {} items · done", format_int(p.count))
        }
    }

    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.trim();
        if q.is_empty() {
            return self.empty_results();
        }

        let mut results = Vec::new();

        let ql = q.to_lowercase();
        if "settings".starts_with(&ql)
            || "preferences".starts_with(&ql)
            || "index".starts_with(&ql)
            || ql == "config"
        {
            results.push(SearchResult {
                id: "cmd:settings".into(),
                title: "Blink Settings".into(),
                subtitle: "Indexing · mounts · excludes".into(),
                kind: ResultKind::Command,
                score: 20_000,
                icon: Some("preferences-system".into()),
                action: Action::OpenSettings,
                conversion: None,
            });
        }

        let calc = self.calc.search(q);
        let calc_hit = calc
            .iter()
            .any(|r| matches!(r.kind, ResultKind::Calc | ResultKind::Conversion));
        results.extend(calc);

        // Translate: only when enabled. Disabled → zero I/O / no background work.
        let mut force_translate = false;
        if !calc_hit && self.translate.is_enabled() && self.translate.should_handle(q) {
            let tr = self.translate.search(q);
            // Any translate row (success or soft-fail) owns the query.
            force_translate = !tr.is_empty();
            results.extend(tr);
        }

        let force_files = is_force_files_query(q, &self.files);

        // Skip apps when calc already answered (unless path/file/glob).
        // UI path: DeepMode::Skip — live deep runs async via `search_files_deep`.
        use crate::providers::files::DeepMode;
        if force_files {
            // Path/glob queries: files only (no Chrome for `*.md`).
            results.extend(self.files.search_with(q, true, DeepMode::Skip));
        } else if force_translate {
            // Strong translation hit — do not mix in apps/files noise.
        } else if !calc_hit {
            let apps = self.apps.search(q);
            // App score bands: exact 50k, prefix 30k+, contains 15k+, fuzzy often <1k.
            let app_prefix = apps.iter().any(|r| r.score >= 30_000);
            let any_apps = !apps.is_empty();
            results.extend(apps);
            if q.len() >= 2 {
                if app_prefix {
                    // Strong prefix (e.g. "firef" → Firefox) — apps only.
                } else if any_apps {
                    // Apps already useful — name-only files (no path fuzzy).
                    results.extend(self.files.search_with(q, false, DeepMode::Skip));
                } else {
                    // No apps — full file search including fuzzy.
                    results.extend(self.files.search_with(q, true, DeepMode::Skip));
                }
            }
        }

        // Exact/prefix path names beat weak app fuzzy (e.g. "glassbox" folder vs
        // Flatseal/Chrome letter soup). Drop apps that only fuzzy-matched.
        let strong_path = results.iter().any(|r| {
            matches!(r.kind, ResultKind::Folder | ResultKind::File) && r.score >= 30_000
        });
        if strong_path {
            results.retain(|r| !matches!(r.kind, ResultKind::App) || r.score >= 15_000);
        }

        for r in &mut results {
            if !matches!(
                r.kind,
                ResultKind::Calc | ResultKind::Conversion | ResultKind::Command
            ) {
                r.score += self.usage.boost(&r.id);
            }
        }

        // Dedup by id without cloning id Strings (first occurrence wins).
        {
            let mut seen = std::collections::HashSet::with_capacity(results.len());
            let mut keep = Vec::with_capacity(results.len());
            for r in &results {
                keep.push(seen.insert(r.id.as_str()));
            }
            let mut i = 0;
            results.retain(|_| {
                let k = keep[i];
                i += 1;
                k
            });
        }

        // Score first so exact folder/file (50k+) outranks weak apps. Kind only
        // breaks ties (app named X still preferred over folder X at equal score).
        results.sort_unstable_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
                .then_with(|| a.title.cmp(&b.title))
        });
        results.truncate(25);
        results
    }

    fn empty_results(&self) -> Vec<SearchResult> {
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Frecency top items
        for (id, score) in self.usage.top(20) {
            if let Some(mut r) = self.resolve_id(&id) {
                r.score = 50_000 + score;
                if seen.insert(r.id.clone()) {
                    results.push(r);
                }
            }
        }

        // Fill with apps
        if results.len() < 15 {
            for mut app in self.apps.all_results(40) {
                if seen.insert(app.id.clone()) {
                    app.score = 1_000 + self.usage.boost(&app.id);
                    results.push(app);
                }
                if results.len() >= 15 {
                    break;
                }
            }
        }

        results.sort_by(|a, b| b.score.cmp(&a.score));
        results.truncate(15);
        results
    }

    fn resolve_id(&self, id: &str) -> Option<SearchResult> {
        if let Some(path) = id.strip_prefix("path:") {
            return self.files.resolve_path(&PathBuf::from(path));
        }
        if id.starts_with("app:") {
            // Exact desktop id lookup — never run fuzzy search on empty-state resolve.
            return self.apps.resolve_id(id);
        }
        None
    }

    pub fn execute(&self, action: &Action) -> ExecuteOutcome {
        match action {
            Action::LaunchApp { exec, terminal, .. } => {
                crate::providers::apps::launch_app(exec, *terminal);
                ExecuteOutcome::Launched
            }
            Action::OpenPath(path) => {
                // Auto-promote parent project folder so future deep walks prefer it.
                maybe_auto_promote_deep_root(self, path);
                let cfg = self.config.snapshot();
                crate::providers::files::open_path_with(path, Some(&cfg.open_with));
                ExecuteOutcome::Launched
            }
            Action::OpenTerminal(path) => {
                crate::providers::files::open_terminal_at(path);
                ExecuteOutcome::Launched
            }
            Action::Copy(text) => {
                copy_to_clipboard(text);
                ExecuteOutcome::Launched
            }
            Action::SetQuery(q) => ExecuteOutcome::SetQuery(q.clone()),
            Action::OpenSettings => ExecuteOutcome::OpenSettings,
        }
    }

    /// Open terminal at selected result (folder, or parent of file).
    pub fn open_terminal_for(&self, item: &SearchResult) -> bool {
        let path = match &item.action {
            Action::OpenPath(p) | Action::OpenTerminal(p) => p.clone(),
            _ => return false,
        };
        self.execute(&Action::OpenTerminal(path));
        self.record_usage(&item.id);
        true
    }

    pub fn record_usage(&self, id: &str) {
        self.usage.record(id);
        if id.starts_with("path:") {
            self.files.note_usage_changed();
        }
    }

    /// Isolated provider search (for `blink --bench` only).
    #[cfg(feature = "bench")]
    pub fn search_apps_only(&self, query: &str) -> Vec<SearchResult> {
        self.apps.search(query)
    }

    /// Async deep walk (worker thread). Larger budget than sync; fills live cache.
    pub fn search_files_deep(&self, query: &str) -> Vec<SearchResult> {
        use crate::providers::files::DeepMode;
        self.files.search_with(query, true, DeepMode::Async)
    }

    /// Blocking network translate for worker threads only (never call on GTK main).
    pub fn search_translate_network(&self, query: &str) -> Vec<SearchResult> {
        if !self.translate.is_enabled() {
            return Vec::new();
        }
        self.translate.search_network(query)
    }

    /// Whether UI should schedule async translate (enabled + needs network).
    pub fn should_translate_network(&self, query: &str) -> bool {
        self.translate.is_enabled() && self.translate.needs_network(query)
    }

    /// Whether this query is a translate candidate (UI gates / diagnostics).
    #[allow(dead_code)]
    pub fn translate_should_handle(&self, query: &str) -> bool {
        self.translate.is_enabled() && self.translate.should_handle(query)
    }

    /// Auto-detect translate (no forced `tr` prefix) — use longer debounce.
    pub fn translate_is_auto_query(&self, query: &str) -> bool {
        self.translate.is_enabled() && self.translate.is_auto_query(query)
    }

    /// Whether the UI should schedule an async deep walk for this query.
    pub fn should_deep_search(&self, query: &str, current: &[SearchResult]) -> bool {
        // Translate owns CJK / `tr ` queries — never deep-walk those (was a major stutter).
        if self.translate.is_enabled() && self.translate.should_handle(query) {
            return false;
        }
        // Calc/conversion already answered (battery, math, `now`, …) and Engine::search
        // skipped the file index for this query — don't bury it under a deep walk.
        let calc_hit = current
            .iter()
            .any(|r| matches!(r.kind, ResultKind::Calc | ResultKind::Conversion));
        if calc_hit {
            let force_files = is_force_files_query(query, &self.files);
            if !force_files {
                return false;
            }
        }
        // Files provider only uses score / scope-hint ids; pass mixed results
        // (no clone of file/folder rows).
        self.files.should_deep_search(query, current)
    }

    /// Pin a folder as a deep root (always indexed to depth 6). Triggers reindex.
    /// Cap is small — deep roots are intentional project pins, not an open-ended list.
    /// Refuses `$HOME`, `/`, and other overly broad roots (see `is_forbidden_deep_root`).
    pub fn promote_deep_root(&self, path: &std::path::Path) {
        const MAX_DEEP_ROOTS: usize = 32;
        // Prefer absolute path so config is stable across shells.
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        };
        // Canonicalize when possible so `/home/foo/../foo` matches home checks.
        let abs = abs.canonicalize().unwrap_or(abs);
        if crate::config::is_forbidden_deep_root(&abs) {
            return;
        }
        let s = abs.to_string_lossy().to_string();
        if s.is_empty() {
            return;
        }
        let mut changed = false;
        self.config.update(|c| {
            if c.index.deep_roots.iter().any(|x| x == &s) {
                return;
            }
            c.index.deep_roots.push(s);
            // Drop oldest pins if over cap (keep most recent).
            if c.index.deep_roots.len() > MAX_DEEP_ROOTS {
                let drop_n = c.index.deep_roots.len() - MAX_DEEP_ROOTS;
                c.index.deep_roots.drain(0..drop_n);
            }
            changed = true;
        });
        if changed {
            self.force_reindex();
        }
    }

    /// Remove a pinned deep root.
    pub fn remove_deep_root(&self, path: &str) {
        let mut changed = false;
        self.config.update(|c| {
            let before = c.index.deep_roots.len();
            c.index.deep_roots.retain(|x| x != path);
            changed = c.index.deep_roots.len() != before;
        });
        if changed {
            self.force_reindex();
        }
    }

    /// Isolated provider search (for `blink --bench` only).
    #[cfg(feature = "bench")]
    pub fn search_calc_only(&self, query: &str) -> Vec<SearchResult> {
        self.calc.search(query)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteOutcome {
    Launched,
    OpenSettings,
    /// Soft completion — keep window open and replace the search query.
    SetQuery(String),
}

/// When the user opens a file deeper than the global index depth, promote a
/// nearby project root so future deep walks prefer it. Never writes live hits
/// into the persistent index — only pins a folder as a deep root.
///
/// Never promotes `$HOME` / `/` even if a stray `package.json` (etc.) sits there.
fn maybe_auto_promote_deep_root(engine: &Engine, path: &std::path::Path) {
    // Prefer the directory containing the file; if already a dir, use it.
    let start = if path.is_dir() {
        path.to_path_buf()
    } else {
        match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => return,
        }
    };

    // Walk up a few levels looking for a project marker.
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "CMakeLists.txt",
        "Makefile",
        "meson.build",
    ];
    let mut cur = start;
    for _ in 0..6 {
        // Stop before promoting home / filesystem root even if they have markers.
        if crate::config::is_forbidden_deep_root(&cur) {
            break;
        }
        for m in MARKERS {
            if cur.join(m).exists() {
                engine.promote_deep_root(&cur);
                return;
            }
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => break,
        }
    }
}

#[cfg(test)]
mod deep_root_tests {
    use crate::config::is_forbidden_deep_root;
    use std::path::Path;

    #[test]
    fn forbids_slash_and_home() {
        assert!(is_forbidden_deep_root(Path::new("/")));
        if let Some(home) = dirs::home_dir() {
            assert!(is_forbidden_deep_root(&home));
        }
    }

    #[test]
    fn allows_project_subdir() {
        if let Some(home) = dirs::home_dir() {
            let project = home.join("blink");
            assert!(!is_forbidden_deep_root(&project));
        }
    }
}

/// Path / file-forced queries: case-insensitive `f`/`file`/`folder` prefixes,
/// absolute/home/dot paths, globs, and scoped (`name in folder`) forms.
fn is_force_files_query(q: &str, files: &FileProvider) -> bool {
    let t = q.trim_start();
    if t.is_empty() {
        return false;
    }
    // Path-shaped (keep case: `/`, `~/`, `./`, `.ext` / `.hidden`).
    if t.starts_with('/')
        || t.starts_with("~/")
        || t.starts_with("./")
        || t.starts_with('.')
    {
        return true;
    }
    // Prefix modes: `f foo`, `File foo`, `FOLDER bar` (ASCII-insensitive).
    if let Some(rest) = strip_force_files_prefix(t) {
        // Bare `f` / `file` / `folder` still count as force (browse mode).
        let _ = rest;
        return true;
    }
    if crate::providers::files::is_path_glob_query(t) {
        return true;
    }
    // `name in scope` (incl. bare folder scopes known to the index).
    files.is_scoped_query(t)
}

/// If `q` starts with `f`/`file`/`folder` + whitespace (ASCII case-insensitive),
/// return the remainder (may be empty).
fn strip_force_files_prefix(q: &str) -> Option<&str> {
    let bytes = q.as_bytes();
    // Match longest prefix first.
    for pref in ["folder", "file", "f"] {
        let pb = pref.as_bytes();
        if bytes.len() >= pb.len()
            && bytes[..pb.len()].eq_ignore_ascii_case(pb)
        {
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

fn kind_rank(k: ResultKind) -> u8 {
    match k {
        ResultKind::Calc | ResultKind::Conversion => 0,
        ResultKind::Command => 1,
        ResultKind::App => 2,
        ResultKind::Folder => 3,
        ResultKind::File => 4,
    }
}

fn format_int(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        return;
    }

    if let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}


#[cfg(test)]
mod force_files_tests {
    use super::strip_force_files_prefix;

    #[test]
    fn prefix_case_insensitive() {
        assert!(strip_force_files_prefix("f doc").is_some());
        assert!(strip_force_files_prefix("F doc").is_some());
        assert!(strip_force_files_prefix("File doc").is_some());
        assert!(strip_force_files_prefix("FILE doc").is_some());
        assert!(strip_force_files_prefix("folder x").is_some());
        assert!(strip_force_files_prefix("Folder x").is_some());
        // Must not steal normal app names.
        assert!(strip_force_files_prefix("firefox").is_none());
        assert!(strip_force_files_prefix("files").is_none());
        assert!(strip_force_files_prefix("folderish").is_none());
    }
}
