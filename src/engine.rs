use crate::config::ConfigStore;
use crate::providers::apps::AppProvider;
use crate::providers::calc::CalcProvider;
use crate::providers::files::{FileProvider, IndexProgress};
use crate::providers::translate::TranslateProvider;
use crate::providers::{Action, ResultKind, SearchResult};
use crate::typos::TypoStore;
use crate::usage::UsageStore;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

struct PeriodicRefresh {
    /// Sending stops the thread (wakes its 45m wait immediately).
    stop: std::sync::mpsc::Sender<()>,
    thread: thread::JoinHandle<()>,
}

pub struct Engine {
    apps: Arc<AppProvider>,
    files: Arc<FileProvider>,
    calc: Arc<CalcProvider>,
    translate: Arc<TranslateProvider>,
    usage: Arc<UsageStore>,
    typos: Arc<TypoStore>,
    config: Arc<ConfigStore>,
    /// Periodic refresh thread + stop signal (daemon only; None in headless).
    periodic: Mutex<Option<PeriodicRefresh>>,
}

impl Engine {
    /// Full daemon engine: warm apps/index on a bg thread + 45m periodic refresh.
    pub fn new() -> Self {
        let mut engine = Self::new_headless();
        engine.spawn_warm();
        engine.spawn_periodic_refresh();
        engine
    }

    /// CLI / bench: same providers, **no** eternal periodic thread.
    /// Still warms apps + index once on a background thread (needed for search).
    pub fn new_headless() -> Self {
        let config = Arc::new(ConfigStore::load());
        let usage = Arc::new(UsageStore::load());
        let typos = Arc::new(TypoStore::load());
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
            typos,
            config,
            periodic: Mutex::new(None),
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
    ///
    /// Battery note: the thread sleeps in a channel `recv_timeout`, so it only
    /// wakes the process once per 45 min. `rebuild_index` delegates to
    /// `ensure_fresh`, which short-circuits when the on-disk fingerprint is
    /// unchanged, so a quiet daemon costs little beyond that single wake.
    /// The thread is stoppable via [`Engine::shutdown_periodic_refresh`] (also
    /// called on `Drop`) so in-process `Engine::new()` tests never leak it.
    pub fn spawn_periodic_refresh(&mut self) {
        let files_periodic = self.files.clone();
        let apps_periodic = self.apps.clone();
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        let thread = thread::spawn(move || loop {
            match rx.recv_timeout(Duration::from_secs(45 * 60)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    apps_periodic.reload();
                    files_periodic.rebuild_index();
                }
            }
        });
        *self.periodic.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(PeriodicRefresh { stop, thread });
    }

    /// Stop the periodic refresh thread and join it (wakes its 45m wait).
    pub fn shutdown_periodic_refresh(&self) {
        if let Some(p) = self
            .periodic
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = p.stop.send(());
            let _ = p.thread.join();
        }
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

    /// Rescan `.desktop` files. Cheap (~ms) and safe on the UI thread.
    pub fn reload_apps(&self) {
        self.apps.reload();
    }

    /// Always off the UI thread.
    pub fn force_reindex(&self) {
        let files = self.files.clone();
        let apps = self.apps.clone();
        thread::spawn(move || {
            // New installs often land while the daemon is already running;
            // reindex should pick up apps too, not only files.
            apps.reload();
            files.force_rebuild();
        });
    }

    /// Blocking rebuild for `hark --bench` only (not used by UI).
    #[cfg(feature = "bench")]
    pub fn bench_force_reindex_blocking(&self) {
        self.files.force_rebuild();
    }

    #[cfg(feature = "bench")]
    pub fn index_cache_bytes(&self) -> Option<u64> {
        crate::providers::files::cache_bytes_on_disk()
    }

    /// Loaded desktop app count (for `hark --bench` readiness).
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
            if p.capped_by_deep {
                format!(
                    "Index: {} items · cap hit by deep roots (max {}; pin fewer deep folders)",
                    format_int(p.count),
                    format_int(p.max)
                )
            } else {
                format!(
                    "Index: {} items · cap reached (max {})",
                    format_int(p.count),
                    format_int(p.max)
                )
            }
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
                title: "Hark Settings".into(),
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
        let strong_path = results
            .iter()
            .any(|r| matches!(r.kind, ResultKind::Folder | ResultKind::File) && r.score >= 30_000);
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

        // Personal typo aliases (v1/v2): boost or inject the learned target.
        if !force_files && !force_translate && !calc_hit {
            self.apply_typo_alias(q, &mut results);
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

        // Prefer apps first (no FS). Paths use a single metadata syscall and a
        // hard cap so empty-open never hammers the disk with 20× exists/is_dir.
        const MAX_PATH_RESOLVES: usize = 8;
        let mut path_resolves = 0usize;

        for (id, score) in self.usage.top(20) {
            if id.starts_with("path:") {
                if path_resolves >= MAX_PATH_RESOLVES {
                    continue;
                }
                path_resolves += 1;
            }
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

        results.sort_by_key(|b| std::cmp::Reverse(b.score));
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
                match crate::providers::apps::launch_app(exec, *terminal) {
                    Ok(()) => ExecuteOutcome::Launched,
                    Err(err) => {
                        eprintln!("hark: launch failed: {err}");
                        ExecuteOutcome::Failed
                    }
                }
            }
            Action::OpenPath(path) => {
                // Promote project roots off the UI thread (marker walks are FS-heavy).
                self.schedule_auto_promote_deep_root(path);
                let cfg = self.config.snapshot();
                match crate::providers::files::open_path_with(path, Some(&cfg.open_with)) {
                    Ok(()) => ExecuteOutcome::Launched,
                    Err(err) => {
                        eprintln!("hark: open failed: {err}");
                        ExecuteOutcome::Failed
                    }
                }
            }
            Action::OpenTerminal(path) => match crate::providers::files::open_terminal_at(path) {
                Ok(()) => ExecuteOutcome::Launched,
                Err(err) => {
                    eprintln!("hark: open terminal failed: {err}");
                    ExecuteOutcome::Failed
                }
            },
            Action::Copy(text) => match copy_to_clipboard(text) {
                Ok(()) => ExecuteOutcome::Launched,
                Err(err) => {
                    eprintln!("hark: copy failed: {err}");
                    ExecuteOutcome::Failed
                }
            },
            Action::SetQuery(q) => ExecuteOutcome::SetQuery(q.clone()),
            Action::OpenSettings => ExecuteOutcome::OpenSettings,
            Action::RevealPath(path) => {
                crate::providers::files::reveal_in_file_manager(path);
                ExecuteOutcome::Launched
            }
            Action::TrashPath(path) => match crate::providers::files::trash_path(path) {
                Ok(()) => {
                    // Stale deep-cache hits can re-surface the trashed path.
                    self.files.clear_live_cache();
                    ExecuteOutcome::Refresh
                }
                Err(err) => {
                    eprintln!("hark: trash failed: {err}");
                    ExecuteOutcome::Failed
                }
            },
            Action::OpenWith(path) => ExecuteOutcome::OpenWith(path.clone()),
            Action::TogglePreview => ExecuteOutcome::TogglePreview,
        }
    }

    /// Secondary actions for the action panel (`Ctrl+K`).
    pub fn secondary_actions(&self, item: &SearchResult) -> Vec<crate::providers::ActionSpec> {
        crate::providers::secondary_actions(item)
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

    /// Learn personal typo aliases from a launch (v1 final query + v2 session).
    pub fn learn_typos(
        &self,
        final_query: &str,
        recent_queries: &[String],
        result_id: &str,
        result_title: &str,
    ) {
        self.typos
            .learn_from_launch(final_query, recent_queries, result_id, result_title);
    }

    /// Settings: learned typo aliases (strongest first).
    pub fn list_typo_aliases(&self) -> Vec<crate::typos::TypoAlias> {
        self.typos.list()
    }

    pub fn remove_typo_alias(&self, alias: &str) -> bool {
        self.typos.remove(alias)
    }

    pub fn clear_typo_aliases(&self) {
        self.typos.clear_all();
    }

    /// Manual alias from Settings. `target` is an app name, desktop id, or path.
    pub fn add_typo_alias(&self, alias: &str, target: &str) -> Result<String, String> {
        let (id, label) = self.resolve_alias_target(target)?;
        self.typos.set_manual(alias, &id)?;
        Ok(label)
    }

    fn resolve_alias_target(&self, target: &str) -> Result<(String, String), String> {
        let t = target.trim();
        if t.is_empty() {
            return Err("Enter an app name or file path".into());
        }
        // Explicit ids
        if t.starts_with("app:") || t.starts_with("path:") {
            if let Some(r) = self.resolve_id(t) {
                return Ok((r.id, r.title));
            }
            return Err("Unknown result id".into());
        }
        // Path-shaped
        let path = crate::providers::files::expand_user_path(t);
        if path.exists() {
            let id = format!("path:{}", path.display());
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(t)
                .to_string();
            return Ok((id, name));
        }
        // App by name / desktop id
        let q = t.to_lowercase();
        // Prefer exact desktop id without app: prefix
        if let Some(r) = self.apps.resolve_id(&format!("app:{t}")) {
            return Ok((r.id, r.title));
        }
        if let Some(r) = self.apps.resolve_id(&format!("app:{t}.desktop")) {
            return Ok((r.id, r.title));
        }
        let hits = self.apps.search(t);
        // Exact name first
        if let Some(r) = hits.iter().find(|r| r.title.eq_ignore_ascii_case(t)) {
            return Ok((r.id.clone(), r.title.clone()));
        }
        // Strong prefix / high score
        if let Some(r) = hits.iter().find(|r| r.score >= 15_000) {
            return Ok((r.id.clone(), r.title.clone()));
        }
        if let Some(r) = hits.first() {
            return Ok((r.id.clone(), r.title.clone()));
        }
        let _ = q;
        Err(format!("No app or path matching “{t}”"))
    }

    /// Friendly label for a stored result id (Settings list).
    pub fn result_display_name(&self, id: &str) -> String {
        if let Some(r) = self.resolve_id(id) {
            return r.title;
        }
        if let Some(rest) = id.strip_prefix("app:") {
            return rest.trim_end_matches(".desktop").to_string();
        }
        if let Some(rest) = id.strip_prefix("path:") {
            return std::path::Path::new(rest)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(rest)
                .to_string();
        }
        id.to_string()
    }

    /// Boost / inject a learned alias target for `query`.
    fn apply_typo_alias(&self, query: &str, results: &mut Vec<SearchResult>) {
        let Some((id, boost)) = self.typos.lookup(query) else {
            return;
        };
        if let Some(r) = results.iter_mut().find(|r| r.id == id) {
            r.score = r.score.saturating_add(boost);
            return;
        }
        // Not in the current hit list — inject so the personal match still appears.
        if let Some(mut r) = self.resolve_id(&id) {
            r.score = crate::typos::inject_floor().saturating_add(boost);
            r.score = r.score.saturating_add(self.usage.boost(&r.id));
            results.push(r);
        }
    }

    /// Isolated provider search (for `hark --bench` only).
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

    /// Isolated provider search (for `hark --bench` only).
    #[cfg(feature = "bench")]
    pub fn search_calc_only(&self, query: &str) -> Vec<SearchResult> {
        self.calc.search(query)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown_periodic_refresh();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteOutcome {
    Launched,
    OpenSettings,
    /// Soft completion — keep window open and replace the search query.
    SetQuery(String),
    /// Keep the launcher open and re-run the current search (e.g. after trash).
    Refresh,
    /// Action did not complete; keep the launcher open.
    Failed,
    /// Show system Open With dialog for this path (UI-owned).
    OpenWith(std::path::PathBuf),
    /// Toggle media preview panel (UI-owned).
    TogglePreview,
}

impl Engine {
    /// Cheap UI-thread gate, then FS marker walk + promote on a background thread.
    fn schedule_auto_promote_deep_root(&self, path: &std::path::Path) {
        // Already covered by a pinned deep root — nothing to do.
        if path_under_any_deep_root(path, &self.config.snapshot().index.deep_roots) {
            return;
        }
        let path = path.to_path_buf();
        let config = self.config.clone();
        let files = self.files.clone();
        let apps = self.apps.clone();
        thread::spawn(move || {
            auto_promote_deep_root(&config, &files, &apps, &path);
        });
    }
}

/// True when `path` is equal to or nested under any configured deep root.
fn path_under_any_deep_root(path: &std::path::Path, deep_roots: &[String]) -> bool {
    for r in deep_roots {
        let root = std::path::Path::new(r);
        if root.as_os_str().is_empty() {
            continue;
        }
        if path == root || path.starts_with(root) {
            return true;
        }
    }
    false
}

/// When the user opens a file deeper than the global index depth, promote a
/// nearby project root so future deep walks prefer it. Never writes live hits
/// into the persistent index — only pins a folder as a deep root.
///
/// Never promotes `$HOME` / `/` even if a stray `package.json` (etc.) sits there.
/// Runs on a worker thread — do not call from the GTK main loop.
fn auto_promote_deep_root(
    config: &ConfigStore,
    files: &FileProvider,
    apps: &AppProvider,
    path: &std::path::Path,
) {
    // Prefer the directory containing the file; if already a dir, use it.
    // Use symlink_metadata once instead of is_dir() (which may stat again).
    let start = match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => path.to_path_buf(),
        Ok(_) | Err(_) => match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => return,
        },
    };

    // Re-check after the open path may have raced with a settings change.
    if path_under_any_deep_root(path, &config.snapshot().index.deep_roots) {
        return;
    }

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
                promote_deep_root_arcs(config, files, apps, &cur);
                return;
            }
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => break,
        }
    }
}

/// Same as [`Engine::promote_deep_root`] but for background threads (owned Arcs).
fn promote_deep_root_arcs(
    config: &ConfigStore,
    files: &FileProvider,
    apps: &AppProvider,
    path: &std::path::Path,
) {
    const MAX_DEEP_ROOTS: usize = 32;
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let abs = abs.canonicalize().unwrap_or(abs);
    if crate::config::is_forbidden_deep_root(&abs) {
        return;
    }
    let s = abs.to_string_lossy().to_string();
    if s.is_empty() {
        return;
    }
    let mut changed = false;
    config.update(|c| {
        if c.index.deep_roots.iter().any(|x| x == &s) {
            return;
        }
        c.index.deep_roots.push(s);
        if c.index.deep_roots.len() > MAX_DEEP_ROOTS {
            let drop_n = c.index.deep_roots.len() - MAX_DEEP_ROOTS;
            c.index.deep_roots.drain(0..drop_n);
        }
        changed = true;
    });
    if changed {
        // Match Engine::force_reindex — off UI by construction here.
        apps.reload();
        files.force_rebuild();
    }
}

#[cfg(test)]
mod deep_root_tests {
    use super::path_under_any_deep_root;
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
            let project = home.join("hark");
            assert!(!is_forbidden_deep_root(&project));
        }
    }

    #[test]
    fn under_deep_root_prefix() {
        let roots = vec!["/home/u/proj".into(), "/mnt/d/code".into()];
        assert!(path_under_any_deep_root(
            Path::new("/home/u/proj/src/main.rs"),
            &roots
        ));
        assert!(path_under_any_deep_root(Path::new("/home/u/proj"), &roots));
        assert!(!path_under_any_deep_root(
            Path::new("/home/u/other/file"),
            &roots
        ));
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
    if t.starts_with('/') || t.starts_with("~/") || t.starts_with("./") || t.starts_with('.') {
        return true;
    }
    // Prefix modes: `f foo`, `File foo`, `FOLDER bar` (ASCII-insensitive).
    if let Some(rest) = crate::providers::files::strip_force_files_prefix(t) {
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

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    for (prog, extra) in [
        ("wl-copy", &[][..]),
        ("xclip", &["-selection", "clipboard"][..]),
    ] {
        if let Ok(mut child) = Command::new(prog).args(extra).stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return Ok(());
        }
    }
    Err("no clipboard tool available (wl-copy / xclip not found)".into())
}

#[cfg(test)]
mod force_files_tests {
    use crate::providers::files::strip_force_files_prefix;

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

#[cfg(test)]
mod engine_search_tests {
    use super::*;
    use crate::config::{ConfigStore, HarkConfig};
    use crate::providers::apps::AppProvider;
    use crate::providers::files::FileProvider;
    use crate::providers::{Action, ResultKind};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    /// Hermetic Engine: temp-dir config (translate disabled), injected app list,
    /// seeded in-memory file index, empty usage/typos. No disk scans, no cache
    /// writes, no network, no periodic thread. T1 ranking-matrix testbed.
    struct TestEngine {
        engine: Engine,
        _dir: PathBuf,
    }

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn tmp_config_dir() -> PathBuf {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("hark-engine-test-{}-{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn base_config() -> HarkConfig {
        let mut cfg = HarkConfig::default();
        // Translate is on by default; disable for deterministic ranking.
        cfg.translate.enabled = false;
        cfg.index.include_home = false;
        cfg.index.extra_roots.clear();
        cfg
    }

    /// Build an engine with the given apps and files.
    fn build_engine(apps: &[(&str, &str)], files: &[(&str, bool)]) -> TestEngine {
        let dir = tmp_config_dir();
        let cfg = ConfigStore::with_path(base_config(), dir.join("config.json"));
        let cfg = Arc::new(cfg);

        let usage = Arc::new(UsageStore::new_empty());
        let typos = Arc::new(TypoStore::new_empty());

        let app_provider = Arc::new(AppProvider::new_empty());
        app_provider.inject(apps);

        let file_provider = Arc::new(FileProvider::new_empty(cfg.clone(), usage.clone()));
        let file_paths: Vec<(PathBuf, bool)> = files
            .iter()
            .map(|(name, is_dir)| (PathBuf::from(name), *is_dir))
            .collect();
        file_provider.seed_index(&file_paths);

        let engine = Engine {
            apps: app_provider,
            files: file_provider,
            calc: Arc::new(CalcProvider::new()),
            translate: Arc::new(TranslateProvider::new(cfg.clone())),
            usage,
            typos,
            config: cfg,
            periodic: Mutex::new(None),
        };
        TestEngine { engine, _dir: dir }
    }

    fn titles(results: &[SearchResult]) -> Vec<&str> {
        results.iter().map(|r| r.title.as_str()).collect()
    }

    fn first_kind(results: &[SearchResult]) -> Option<ResultKind> {
        results.first().map(|r| r.kind)
    }

    fn has_app(results: &[SearchResult], title: &str) -> bool {
        results
            .iter()
            .any(|r| r.kind == ResultKind::App && r.title == title)
    }

    fn find<'a>(results: &'a [SearchResult], title: &str) -> Option<&'a SearchResult> {
        results.iter().find(|r| r.title == title)
    }

    #[test]
    fn settings_command_owns_query() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        let results = te.engine.search("settings");
        assert_eq!(first_kind(&results), Some(ResultKind::Command));
        assert!(matches!(results[0].action, Action::OpenSettings));
        // "preferences", "index" also map to Settings.
        assert_eq!(te.engine.search("preferences")[0].kind, ResultKind::Command);
    }

    #[test]
    fn calc_owns_query_and_skips_apps() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        let results = te.engine.search("2+2");
        assert_eq!(first_kind(&results), Some(ResultKind::Calc));
        assert!(
            !has_app(&results, "Firefox"),
            "apps must not mix into calc hits"
        );
    }

    #[test]
    fn math_units_and_timezone() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        // Units conversion (Conversion kind).
        let r = te.engine.search("5 km to miles");
        assert_eq!(first_kind(&r), Some(ResultKind::Conversion));
        // Datetime query (e.g. "now").
        let now = te.engine.search("now");
        assert_eq!(first_kind(&now), Some(ResultKind::Calc));
    }

    #[test]
    fn t0_unit_magnitude_and_duration_steal() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        fn no_calc(r: &[SearchResult]) -> bool {
            r.iter().all(|x| !matches!(x.kind, ResultKind::Calc | ResultKind::Conversion))
        }
        fn calc_title(r: &[SearchResult]) -> String {
            r.first().map(|x| x.title.clone()).unwrap_or_default()
        }
        // Single-letter m/b/t are meters/bytes/tonnes, not magnitudes: unit
        // arithmetic returns real unit answers, never 5e7/3e6-style junk.
        assert_eq!(calc_title(&te.engine.search("100m / 2")), "50 m");
        assert_eq!(calc_title(&te.engine.search("1m * 3")), "3 m");
        assert_eq!(calc_title(&te.engine.search("100m + 5m")), "105 m");
        assert_eq!(calc_title(&te.engine.search("1b / 2")), "0.5 b");
        assert_eq!(calc_title(&te.engine.search("2t / 4")), "500 kg");
        // Bare `100m` (no arithmetic) still gets no calc answer.
        assert!(no_calc(&te.engine.search("100m")), "must not be 100 m from now");
        // `50% of 1h 30min` → 45min, not a duration echo of "1h 30min".
        let r = te.engine.search("50% of 1h 30min");
        assert_eq!(calc_title(&r), "45min");
        assert!(r[0].conversion.is_some(), "must carry a conversion card");
        // `in 1h 30min` → future timestamp, not a duration card.
        let r = te.engine.search("in 1h 30min");
        assert_eq!(first_kind(&r), Some(ResultKind::Calc), "{r:?}");
        // Magnitude words / k still work.
        let r = te.engine.search("2 million + 500k");
        assert_eq!(first_kind(&r), Some(ResultKind::Calc));
        let r = te.engine.search("10k * 3");
        assert_eq!(first_kind(&r), Some(ResultKind::Calc));
        // Duration math still works (duration provider owns time arithmetic).
        let r = te.engine.search("2h + 30m");
        assert_eq!(first_kind(&r), Some(ResultKind::Calc));
        assert_eq!(r[0].title, "2h 30min");
        // Unit math renders on the card (title = formatted result).
        let r = te.engine.search("200mb * 10");
        assert_eq!(r[0].title, "2 gb");
        let r = te.engine.search("2km / 5");
        assert_eq!(r[0].title, "400 m");
        let r = te.engine.search("50% of 2h");
        assert_eq!(r[0].title, "1h");
    }

    #[test]
    fn tier1_math_natural_renders_cards() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        let cases = ["50% of 100", "10% of 2k", "tip 15% on 2k", "0x1f", "0b1010"];
        for q in cases {
            let r = te.engine.search(q);
            assert_eq!(first_kind(&r), Some(ResultKind::Calc), "{q}");
            assert!(r[0].conversion.is_some(), "{q} must carry a conversion card");
        }
    }

    #[test]
    fn tier2_datetime_renders_cards() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        let cases = [
            "now",
            "utc",
            "tomorrow",
            "yesterday",
            "unix 1735000000",
            "1735000000",
            "to unix",
            "in 1h 30min",
            "1h 30min ago",
            "days until 2026-08-20",
            "2026-08-20",
            "15/08/2026",
            "week",
            "day of year",
            "day on 27 august 2026",
            "on 26 aug",
        ];
        for q in cases {
            let r = te.engine.search(q);
            assert_eq!(first_kind(&r), Some(ResultKind::Calc), "{q}");
            assert!(r[0].conversion.is_some(), "{q} must carry a conversion card");
        }
    }

    #[test]
    fn tier2_financial_renders_cards() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        for q in [
            "interest 1000 at 5% for 3 years",
            "20% off 500",
            "split 45 4",
            "gst 18% on 1000",
            "emi 500000 8% 5 years",
            "cagr 10000 to 20000 3 years",
            "72 at 8%",
            "100 to 150",
            "25/hr to annual",
            "60000/yr to hourly",
        ] {
            let r = te.engine.search(q);
            assert_eq!(first_kind(&r), Some(ResultKind::Calc), "{q}");
            assert!(r[0].conversion.is_some(), "{q} must carry a conversion card");
        }
    }

    #[test]
    fn tier2_fuel_economy_renders_cards() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        for q in ["12 km/l to mpg", "30 mpg to l/100km", "30 mpg to km/l", "7.84 l/100km to mpg"] {
            let r = te.engine.search(q);
            assert_eq!(first_kind(&r), Some(ResultKind::Calc), "{q}");
            assert!(r[0].conversion.is_some(), "{q} must carry a conversion card");
        }
    }

    #[test]
    fn tier3_battery_renders_cards() {
        let te = build_engine(&[("firefox.desktop", "Firefox")], &[]);
        for q in ["battery", "bat", "power", "charging", "on battery"] {
            let r = te.engine.search(q);
            assert_eq!(first_kind(&r), Some(ResultKind::Calc), "{q}");
            assert!(r[0].conversion.is_some(), "{q} must carry a conversion card");
        }
    }

    #[test]
    fn force_files_query_bypasses_apps() {
        let te = build_engine(
            &[("firefox.desktop", "Firefox")],
            &[("/home/u/doc.md", false), ("/home/u/proj", true)],
        );
        // Path-shaped query → files only.
        let results = te.engine.search("/home/u/doc");
        assert!(
            results.iter().all(|r| r.kind == ResultKind::File),
            "path query must not return apps: {results:?}"
        );
        // `f ` prefix → files only.
        let results = te.engine.search("f doc");
        assert!(
            results
                .iter()
                .all(|r| matches!(r.kind, ResultKind::File | ResultKind::Folder)),
            "f- prefix must return only file/folder: {results:?}"
        );
    }

    #[test]
    fn glob_query_files_only() {
        let te = build_engine(
            &[("firefox.desktop", "Firefox")],
            &[("/home/u/doc.md", false), ("/home/u/src/main.rs", false)],
        );
        let results = te.engine.search("*.md");
        assert!(
            results.iter().all(|r| r.kind == ResultKind::File),
            "glob must return files only: {results:?}"
        );
        assert!(titles(&results).contains(&"doc.md"));
    }

    #[test]
    fn strong_path_beats_weak_app() {
        // "glassbox" fuzzy-matches Firefox (s.score < 40? no) — use a name that
        // fuzzy-matches but has no prefix/contains band, e.g. "fixf" → Firefox.
        // An exact folder named "fixf" must outrank the weak fuzzy app.
        let te = build_engine(
            &[("firefox.desktop", "Firefox")],
            &[("/home/u/fixf", true), ("/home/u/fixf_notes.md", false)],
        );
        let results = te.engine.search("fixf");
        let folder = find(&results, "fixf");
        let firefox = find(&results, "Firefox");
        assert!(folder.is_some(), "folder must be present: {results:?}");
        if let (Some(f), Some(a)) = (folder, firefox) {
            assert!(
                f.score >= a.score,
                "exact folder should beat fuzzy app: {} vs {}",
                f.score,
                a.score
            );
        }
    }

    #[test]
    fn app_prefix_owns_query_files_name_only() {
        let te = build_engine(
            &[("firefox.desktop", "Firefox")],
            &[
                ("/home/u/firefox.md", false),
                ("/home/u/firefox_backup", true),
                ("/home/u/xyz.txt", false),
            ],
        );
        let results = te.engine.search("firef");
        assert_eq!(
            results[0].title, "Firefox",
            "strong app prefix wins: {results:?}"
        );
    }

    #[test]
    fn strong_app_exact_wins_over_folder() {
        let te = build_engine(&[("brave.desktop", "Brave")], &[("/home/u/brave", true)]);
        // Exact app (50k) beats exact folder (50k) — App kind breaks the tie.
        let results = te.engine.search("brave");
        let first = results.first().unwrap();
        assert_eq!(
            first.title, "Brave",
            "exact app beats folder at equal score"
        );
        assert_eq!(first.kind, ResultKind::App);
    }

    #[test]
    fn usage_boost_lifts_frequently_opened() {
        let te = build_engine(
            &[("firefox.desktop", "Firefox"), ("chrome.desktop", "Chrome")],
            &[],
        );
        // Record usage for Firefox so its boost outranks Chrome.
        te.engine.record_usage("app:firefox.desktop");
        te.engine.record_usage("app:firefox.desktop");
        let results = te.engine.search("firefox");
        assert!(has_app(&results, "Firefox"));
    }

    #[test]
    fn typo_alias_injects_personal_target() {
        let te = build_engine(
            &[("firefox.desktop", "Firefox"), ("chrome.desktop", "Chrome")],
            &[],
        );
        // Learn alias "ff" → firefox.
        te.engine
            .learn_typos("ff", &["ff".to_string()], "app:firefox.desktop", "Firefox");
        let results = te.engine.search("ff");
        assert!(
            has_app(&results, "Firefox"),
            "typo alias must surface Firefox for ff: {results:?}"
        );
    }

    #[test]
    fn dedup_by_id_keeps_first() {
        let te = build_engine(
            &[("firefox.desktop", "Firefox")],
            &[("/home/u/firefox", true)],
        );
        let results = te.engine.search("firefox");
        // No duplicate ids (app:firefox.desktop vs path:/home/u/firefox differ, so
        // both should appear but no id may repeat).
        let mut ids = results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            results.len(),
            "duplicate ids in results: {results:?}"
        );
    }

    #[test]
    fn results_are_sorted_desc_by_score() {
        let te = build_engine(
            &[("firefox.desktop", "Firefox")],
            &[("/home/u/doc.md", false), ("/home/u/proj", true)],
        );
        let results = te.engine.search("f");
        let mut prev = i64::MAX;
        for r in &results {
            assert!(r.score <= prev, "results not sorted desc: {results:?}");
            prev = r.score;
        }
    }
}
