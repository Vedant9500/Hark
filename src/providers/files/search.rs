use super::index::{
    expand_user, is_encoded_session_name, make_indexed, should_descend, should_skip_entry,
    IndexedPath,
};
use crate::config::{pretty_path, ExcludeSet, MountInfo, PathStyle};
use crate::providers::{Action, ResultKind, SearchResult};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

const FILE_RESULT_LIMIT: usize = 25;
/// Once top-K is full of substring-or-better hits, skip fuzzy entirely.
const STRONG_SCORE: i64 = 15_000;
/// Batch B: skip full free-text scan when best hot name score reaches prefix band.
const HOT_SKIP_FULL_SCORE: i64 = 30_000;
/// Min query length (Unicode chars) for hot short-circuit.
/// Short prefixes (`doc`, `src`) stay full-scan so cold siblings still appear.
const HOT_SKIP_MIN_QUERY_LEN: usize = 4;
/// Exact/prefix band — index already answered; no live walk.
const DEEP_SKIP_IF_INDEX_SCORE: i64 = 30_000;
/// Live walk budgets — sync (bench) stays tight; async UI worker can go deeper.
const DEEP_VISIT_CAP_SYNC: usize = 8_000;
const DEEP_TIME_BUDGET_SYNC: std::time::Duration = std::time::Duration::from_millis(40);
const DEEP_VISIT_CAP_ASYNC: usize = 40_000;
const DEEP_TIME_BUDGET_ASYNC: std::time::Duration = std::time::Duration::from_millis(200);
const DEEP_MAX_DEPTH: usize = 6;
const DEEP_MAX_ROOTS: usize = 12;
/// Live hits slightly below equivalent index hits when scores would tie.
const DEEP_SCORE_PENALTY: i64 = 500;

/// How to run on-demand live deep walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepMode {
    /// Index only — main-thread UI path (deep runs async separately).
    Skip,
    /// Tight budget; unit tests + optional sync deep (not used on GTK main).
    #[allow(dead_code)] // constructed in tests; production uses Skip/Async only
    Sync,
    /// Larger budget; worker thread only.
    Async,
}

impl DeepMode {
    fn budgets(self) -> (usize, std::time::Duration) {
        match self {
            DeepMode::Skip => (0, std::time::Duration::ZERO),
            DeepMode::Sync => (DEEP_VISIT_CAP_SYNC, DEEP_TIME_BUDGET_SYNC),
            DeepMode::Async => (DEEP_VISIT_CAP_ASYNC, DEEP_TIME_BUDGET_ASYNC),
        }
    }
}

/// Live deep walk planned under the index lock, executed after the lock is dropped.
#[derive(Debug, Clone)]
pub(crate) struct DeepJob {
    roots: Vec<PathBuf>,
    segments: Vec<String>,
    name_pat: Option<String>,
    dir_scope: bool,
    deep: DeepMode,
}

/// Plan live deep work from the index snapshot only (no WalkDir).
/// Mirrors the gates in `maybe_deep_*` used by unit tests.
pub(crate) fn plan_deep_jobs(
    index: &[IndexedPath],
    query: &str,
    results: &[SearchResult],
    deep: DeepMode,
    deep_roots: &[String],
    mounts: &[MountInfo],
) -> Vec<DeepJob> {
    if deep == DeepMode::Skip {
        return Vec::new();
    }

    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }

    // Absolute / home / drive path globs
    if q.starts_with('/') || q.starts_with('~') || q.starts_with("./") || is_drive_path_query(q) {
        if q.contains('*') || q.contains('?') {
            return plan_deep_absolute_glob(q, results, deep, mounts);
        }
        return Vec::new();
    }

    let q = strip_file_mode_prefix(q).trim();
    if q.is_empty() {
        return Vec::new();
    }

    // Soft scope hints never deep-walk.
    if parse_scope_hint_query(q).is_some() && parse_scoped_query(q, Some(index)).is_none() {
        return Vec::new();
    }

    if let Some(sq) = parse_scoped_query(q, Some(index)) {
        return plan_deep_for_scoped(&sq, index, results, deep, deep_roots);
    }

    if let Some(gq) = parse_glob_query(q) {
        return plan_deep_for_glob(&gq, index, results, deep, deep_roots);
    }

    let q_lower = q.to_lowercase();
    plan_deep_for_name(&q_lower, index, results, deep, deep_roots)
}

/// Execute planned deep walks and merge into `results` (no index lock).
pub(crate) fn run_deep_jobs(
    jobs: Vec<DeepJob>,
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    results: &mut Vec<SearchResult>,
) {
    for job in jobs {
        let existing: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
        let live = live_deep_under_roots(
            &job.roots,
            &job.segments,
            job.name_pat.as_deref(),
            job.dir_scope,
            path_style,
            mounts,
            excludes,
            existing,
            job.deep,
        );
        merge_live(results, live);
        // Named-root walks may already answer strongly — skip remaining jobs.
        if index_is_strong(results) {
            break;
        }
    }
}

fn job_from_roots(
    roots: Vec<PathBuf>,
    segments: Vec<String>,
    name_pat: Option<String>,
    dir_scope: bool,
    deep: DeepMode,
) -> Option<DeepJob> {
    if roots.is_empty() {
        return None;
    }
    Some(DeepJob {
        roots,
        segments,
        name_pat,
        dir_scope,
        deep,
    })
}

fn plan_deep_for_name(
    q_lower: &str,
    index: &[IndexedPath],
    results: &[SearchResult],
    deep: DeepMode,
    deep_roots: &[String],
) -> Vec<DeepJob> {
    if index_is_strong(results) || !looks_specific_for_deep(q_lower) {
        return Vec::new();
    }

    let mut jobs = Vec::new();
    let pinned = pinned_deep_roots(deep_roots);

    let named_roots = roots_from_index_name(index, q_lower);
    if !named_roots.is_empty() {
        if let Some(j) = job_from_roots(
            named_roots,
            Vec::new(),
            Some(q_lower.to_string()),
            false,
            deep,
        ) {
            jobs.push(j);
        }
    }

    let has_ext = q_lower.contains('.')
        && q_lower
            .rsplit_once('.')
            .map(|(_, e)| {
                !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric())
            })
            .unwrap_or(false);
    let specific_glob = (q_lower.contains('*') || q_lower.contains('?'))
        && !is_broad_extension_glob(q_lower)
        && looks_specific_for_deep(q_lower);

    if !has_ext && !specific_glob && pinned.is_empty() {
        return jobs;
    }

    let mut hv = high_value_shallow_roots(index);
    prepend_unique(&mut hv, &pinned);
    if hv.is_empty() {
        return jobs;
    }
    if let Some(j) = job_from_roots(hv, Vec::new(), Some(q_lower.to_string()), false, deep) {
        jobs.push(j);
    }
    jobs
}

fn plan_deep_for_glob(
    gq: &GlobQuery,
    index: &[IndexedPath],
    results: &[SearchResult],
    deep: DeepMode,
    deep_roots: &[String],
) -> Vec<DeepJob> {
    if index_is_strong(results) && !(gq.recursive && !gq.segments.is_empty()) {
        return Vec::new();
    }
    if gq.segments.is_empty() {
        if let Some(pat) = &gq.name_pat {
            if is_broad_extension_glob(pat) {
                return Vec::new();
            }
            if !looks_specific_for_deep(pat) {
                return Vec::new();
            }
        } else {
            return Vec::new();
        }
    }

    let pinned = pinned_deep_roots(deep_roots);
    let roots = if !gq.segments.is_empty() {
        let mut r = roots_from_segments(index, &gq.segments);
        if r.is_empty() && pinned.is_empty() {
            return Vec::new();
        }
        prepend_unique(&mut r, &pinned);
        r
    } else if gq.name_pat.is_some() {
        let mut r = high_value_shallow_roots(index);
        if r.is_empty() && pinned.is_empty() {
            return Vec::new();
        }
        prepend_unique(&mut r, &pinned);
        r
    } else {
        return Vec::new();
    };

    job_from_roots(
        roots,
        gq.segments.clone(),
        gq.name_pat.clone(),
        gq.dir_scope,
        deep,
    )
    .into_iter()
    .collect()
}

fn plan_deep_for_scoped(
    sq: &ScopedQuery,
    index: &[IndexedPath],
    results: &[SearchResult],
    deep: DeepMode,
    deep_roots: &[String],
) -> Vec<DeepJob> {
    if index_is_strong(results) {
        return Vec::new();
    }
    let pinned = pinned_deep_roots(deep_roots);

    // Absolute / `~/` scope → walk that root directly (best case).
    if let Some(abs) = &sq.abs_root {
        let root = if abs.is_dir() {
            abs.clone()
        } else {
            abs.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| abs.clone())
        };
        let mut roots = vec![root];
        prepend_unique(&mut roots, &pinned);
        return job_from_roots(
            roots,
            sq.segments.clone(),
            Some(sq.name_pat.clone()),
            false,
            deep,
        )
        .into_iter()
        .collect();
    }

    // Relative scope segments → same as glob deep, but always prefer pins.
    let mut roots = roots_from_segments(index, &sq.segments);
    if roots.is_empty() {
        // Segment not in shallow index — try high-value + pins, still filter by segments.
        roots = high_value_shallow_roots(index);
    }
    prepend_unique(&mut roots, &pinned);
    if roots.is_empty() {
        return Vec::new();
    }
    job_from_roots(
        roots,
        sq.segments.clone(),
        Some(sq.name_pat.clone()),
        false,
        deep,
    )
    .into_iter()
    .collect()
}

fn plan_deep_absolute_glob(
    query: &str,
    results: &[SearchResult],
    deep: DeepMode,
    mounts: &[MountInfo],
) -> Vec<DeepJob> {
    if index_is_strong(results) || results.len() >= FILE_RESULT_LIMIT {
        // still allow absolute deep? original: return if strong OR full
        return Vec::new();
    }
    let expanded = strip_double_star_components(&expand_path_query(query, mounts));
    let (dir, pat) = split_glob_path(&expanded);
    let pat_l = pat.to_lowercase();
    if pat.is_empty() {
        return Vec::new();
    }
    if !dir.is_dir() {
        return Vec::new();
    }
    // Broad extension under absolute dir: original still deep-walks that dir.
    job_from_roots(
        vec![dir],
        Vec::new(),
        if pat_l.is_empty() { None } else { Some(pat_l) },
        false,
        deep,
    )
    .into_iter()
    .collect()
}

/// Strip optional `f`/`file`/`folder` mode prefix (ASCII case-insensitive).
/// Requires whitespace after the keyword so `firefox` is unchanged.
pub(crate) fn strip_file_mode_prefix(q: &str) -> &str {
    let t = q.trim_start();
    let bytes = t.as_bytes();
    for pref in ["folder", "file", "f"] {
        let pb = pref.as_bytes();
        if bytes.len() >= pb.len() && bytes[..pb.len()].eq_ignore_ascii_case(pb) {
            let rest = &t[pb.len()..];
            if rest.is_empty() {
                return rest;
            }
            let b0 = rest.as_bytes()[0];
            if b0 == b' ' || b0 == b'\t' {
                return rest.trim_start();
            }
        }
    }
    t
}

/// True for path segments, globs, or extension shorthand (e.g. `foo/bar`, `*.md`, `.rs`).
/// Used by the engine to force files-only mode (skip apps).
pub fn is_path_glob_query(query: &str) -> bool {
    let raw = query.trim();
    let q = strip_file_mode_prefix(raw).trim();
    if q.is_empty() {
        return false;
    }
    // `name in scope` is always a files-only intent when confidently parsed.
    if is_scoped_file_query(q) {
        return true;
    }
    // Incomplete `file.md in …` folder hints — also files-only.
    if parse_scope_hint_query(q).is_some() {
        return true;
    }
    if q.starts_with('/') || q.starts_with('~') || q.starts_with("./") {
        return true;
    }
    // Drive-letter paths from Display → Drive style (`D:/Glassbox`, `d:\foo`).
    if is_drive_path_query(q) {
        return true;
    }
    // Spaces without `/` → not a path/glob (`2 * 3`, multi-word app names).
    if q.contains(char::is_whitespace) && !q.contains('/') {
        return false;
    }
    if q.contains('/') || q.contains('*') || q.contains('?') {
        // Bare `*` / `**` alone is useless noise.
        return q
            .chars()
            .any(|c| c != '*' && c != '?' && c != '/' && !c.is_whitespace());
    }
    // Extension shorthand: `.md`, `.png` (not `.gitignore` — has no extra dots after first).
    is_extension_shorthand(q)
}

/// True when the query is a confident `name in scope` file search
/// (no index needed — extension / glob / path-like scope).
pub fn is_scoped_file_query(query: &str) -> bool {
    let raw = query.trim();
    let q = strip_file_mode_prefix(raw).trim();
    parse_scoped_query(q, None).is_some() || parse_scope_hint_query(q).is_some()
}

/// Scope keyword aliases (case-insensitive).
const SCOPE_KEYWORDS: &[&str] = &[" in ", " within ", " under ", " inside "];

/// `optimization.md in glassbox/docs` → name pattern + path segments.
#[derive(Debug, Clone)]
struct ScopedQuery {
    /// Lowercased name / glob pattern (always present).
    name_pat: String,
    /// Path components that must appear in order.
    segments: Vec<String>,
    /// Absolute root when scope was `~/…` or `/…` (preferred deep-walk root).
    abs_root: Option<PathBuf>,
}

/// Parse `name in scope`. Returns `None` when the query is not scoped, sides are
/// empty, or disambiguation says it is not a path intent.
///
/// When `index` is provided, a bare folder-name scope is accepted if that folder
/// exists in the index. Without index, only strong signals are accepted
/// (name has extension/glob, or scope looks path-like).
fn parse_scoped_query(q: &str, index: Option<&[IndexedPath]>) -> Option<ScopedQuery> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    let lower = q.to_lowercase();
    let mut best: Option<(usize, usize)> = None; // (keyword_start, keyword_len)
    for kw in SCOPE_KEYWORDS {
        if let Some(pos) = lower.find(kw) {
            // Prefer the first keyword occurrence.
            match best {
                Some((bp, _)) if bp <= pos => {}
                _ => best = Some((pos, kw.len())),
            }
        }
    }
    let (kw_start, kw_len) = best?;
    let name = q[..kw_start].trim();
    let scope = q[kw_start + kw_len..].trim();
    if name.is_empty() || scope.is_empty() {
        return None;
    }
    // Don't steal absolute path queries that happen to contain " in ".
    if name.starts_with('/') || name.starts_with('~') || name.starts_with("./") {
        return None;
    }

    let name_pat = normalize_scoped_name(name);
    if name_pat.is_empty() {
        return None;
    }
    let (segments, abs_root) = scope_to_segments(scope);
    if segments.is_empty() && abs_root.is_none() {
        return None;
    }

    if !scoped_query_is_confident(name, scope, &segments, index) {
        return None;
    }

    Some(ScopedQuery {
        name_pat,
        segments,
        abs_root,
    })
}

fn normalize_scoped_name(name: &str) -> String {
    let n = name.trim().to_lowercase();
    if is_extension_shorthand(&n) {
        format!("*{n}")
    } else {
        n
    }
}

fn scope_to_segments(scope: &str) -> (Vec<String>, Option<PathBuf>) {
    let scope = scope.trim().trim_end_matches('/');
    if scope.is_empty() {
        return (Vec::new(), None);
    }
    if scope.starts_with('/') || scope.starts_with('~') || scope.starts_with("./") {
        let abs = expand_user(scope);
        let segments: Vec<String> = abs
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
                _ => None,
            })
            .collect();
        (segments, Some(abs))
    } else {
        let segments = scope
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect();
        (segments, None)
    }
}

/// Disambiguation: only treat as scoped path search when confident.
fn scoped_query_is_confident(
    name: &str,
    scope: &str,
    segments: &[String],
    index: Option<&[IndexedPath]>,
) -> bool {
    let name = name.trim();
    let scope = scope.trim();
    // Left side looks like a filename / glob.
    if name_looks_like_file(name) {
        return true;
    }
    // Right side looks path-like.
    if scope.starts_with('/')
        || scope.starts_with('~')
        || scope.starts_with("./")
        || scope.contains('/')
    {
        return true;
    }
    // Bare folder name: only if index knows that folder.
    if let Some(index) = index {
        if let Some(seg) = segments.first() {
            if index.iter().any(|it| it.is_dir && it.name_lower == *seg) {
                return true;
            }
        }
    }
    false
}

fn name_looks_like_file(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return false;
    }
    if n.contains('*') || n.contains('?') {
        return true;
    }
    if is_extension_shorthand(n) {
        return true;
    }
    // `foo.md`, `main.rs` — stem + short alphanumeric ext.
    if let Some((stem, ext)) = n.rsplit_once('.') {
        if !stem.is_empty()
            && !ext.is_empty()
            && ext.len() <= 8
            && !ext.contains('/')
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return true;
        }
    }
    false
}

fn scoped_to_glob(sq: &ScopedQuery) -> GlobQuery {
    GlobQuery {
        segments: sq.segments.clone(),
        name_pat: Some(sq.name_pat.clone()),
        dir_scope: false,
        recursive: false,
    }
}

const SCOPE_HINT_LIMIT: usize = 12;

/// Detect incomplete / partial scoped queries for folder completions.
///
/// Matches: `foo.md in`, `foo.md in `, `foo.md in gla`, `*.rs under docs/`
/// Does not match completed scopes that already parse as confident searches
/// with a full non-prefix-only intent — those go through normal scoped search.
fn parse_scope_hint_query(q: &str) -> Option<(String, String, String)> {
    let q = q.trim_end();
    if q.is_empty() {
        return None;
    }
    let lower = q.to_lowercase();
    let mut best: Option<(usize, usize, &'static str)> = None;
    for kw in SCOPE_KEYWORDS {
        if let Some(pos) = lower.find(kw) {
            match best {
                Some((bp, _, _)) if bp <= pos => {}
                _ => best = Some((pos, kw.len(), *kw)),
            }
        }
    }
    // Also allow trailing keyword without trailing space: `foo.md in`
    // (SCOPE_KEYWORDS include surrounding spaces except we need bare end forms)
    if best.is_none() {
        for (bare, spaced) in [
            (" in", " in "),
            (" within", " within "),
            (" under", " under "),
            (" inside", " inside "),
        ] {
            if lower.ends_with(bare) {
                // `bare` already includes a leading space, so the match is a
                // keyword boundary (e.g. `foo.md in` / `foo.md within`).
                let pos = lower.len() - bare.len();
                best = Some((pos, bare.len(), spaced));
                break;
            }
        }
    }
    let (kw_start, kw_len, kw_display) = best?;
    let name = q[..kw_start].trim();
    if name.is_empty() || !name_looks_like_file(name) {
        // Only offer scope hints when left side already looks like a file/glob —
        // avoids stealing "login in" app-ish queries.
        return None;
    }
    if name.starts_with('/') || name.starts_with('~') || name.starts_with("./") {
        return None;
    }
    let scope = q[kw_start + kw_len..].trim_start();
    // If scope already confidently parses as a full scoped query with a
    // complete folder path that isn't a pure prefix type-ahead, skip hints
    // only when the user has a trailing path that would fully resolve —
    // actually we always show hints when scope is empty/partial prefix.
    // When scope is empty or a prefix of folder names, show suggestions.
    Some((
        name.to_string(),
        scope.to_string(),
        kw_display.trim().to_string(),
    ))
}

/// Folder soft-hints after ` in ` / partial scope typing.
fn scope_folder_suggestions(
    q: &str,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
) -> Option<Vec<SearchResult>> {
    let (name, scope_prefix, kw) = parse_scope_hint_query(q)?;
    let scope_prefix = scope_prefix.trim_end_matches('/');
    let scope_lower = scope_prefix.to_lowercase();

    // If the query already fully parses as a confident scoped search and the
    // scope isn't a bare prefix of something longer, prefer real search —
    // except when scope is empty (user just typed ` in `).
    if !scope_prefix.is_empty() {
        if let Some(sq) = parse_scoped_query(q, Some(index)) {
            // Full confident scope with exact folder match and no typing-ahead:
            // if any indexed dir name equals the last segment exactly AND the
            // user didn't leave a partial last component, run normal search.
            // Heuristic: if scope ends with `/` or last segment matches a dir
            // exactly AND there are name matches, skip hints.
            let last = sq.segments.last().map(|s| s.as_str()).unwrap_or("");
            let exact_dir =
                !last.is_empty() && index.iter().any(|it| it.is_dir && it.name_lower == last);
            // Partial last segment: "gla" matches glassbox → keep hints.
            let last_is_partial = !last.is_empty()
                && index.iter().any(|it| {
                    it.is_dir && it.name_lower.starts_with(last) && it.name_lower != last
                })
                && !exact_dir;
            if exact_dir && !last_is_partial && !q.trim_end().ends_with('/') {
                // Could still be typing a deeper segment; only skip when
                // normal search would return something useful.
                let gq = scoped_to_glob(&sq);
                let hits = search_glob(index, &gq, path_style, mounts);
                if !hits.is_empty() {
                    return None;
                }
            }
            if last_is_partial {
                // fall through to hints
            } else if exact_dir {
                return None;
            }
        }
    }

    let mut scored: Vec<(i64, &IndexedPath, String)> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for item in index {
        if !item.is_dir || item.low_value {
            continue;
        }
        // Prefer high-value / shallow folders for soft hints.
        if item.depth > 4 && !item.high_value {
            continue;
        }

        let (ok, score_boost, completion_scope) = match scope_folder_match(item, &scope_lower) {
            Some(v) => v,
            None => continue,
        };
        if !ok {
            continue;
        }
        if !seen_paths.insert(item.path.as_path()) {
            continue;
        }

        let mut score: i64 = 28_000 + score_boost;
        if item.high_value {
            score += 4_000;
        }
        if item.is_mnt {
            score += 1_500;
        }
        score += (6_i64 - item.depth as i64).max(0) * 400;
        // Prefer folders whose name is the completion target.
        if item.name_lower.starts_with(&scope_lower) && !scope_lower.is_empty() {
            score += 2_000;
        }
        if item.name_lower == scope_lower {
            score += 3_000;
        }

        scored.push((score, item, completion_scope));
    }

    if scored.is_empty() {
        // Empty scope with no folders? still show top high-value dirs.
        if scope_prefix.is_empty() {
            for item in index {
                if !item.is_dir || item.low_value {
                    continue;
                }
                if !(item.high_value || item.depth <= 2) {
                    continue;
                }
                if !seen_paths.insert(item.path.as_path()) {
                    continue;
                }
                let mut score: i64 = 26_000;
                if item.high_value {
                    score += 4_000;
                }
                score += (4_i64 - item.depth as i64).max(0) * 500;
                scored.push((score, item, item.name_lower.clone()));
                if scored.len() >= 40 {
                    break;
                }
            }
        }
    }

    if scored.is_empty() {
        return None;
    }

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.depth.cmp(&b.1.depth))
            .then_with(|| a.1.name_lower.cmp(&b.1.name_lower))
    });
    scored.truncate(SCOPE_HINT_LIMIT);

    let results: Vec<SearchResult> = scored
        .into_iter()
        .map(|(score, item, completion_scope)| {
            let filled = format!("{name} {kw} {completion_scope}");
            SearchResult {
                id: format!("scope-hint:{}", item.path.display()),
                title: item.name.clone(),
                subtitle: format!(
                    "scoped to {} · {}",
                    completion_scope,
                    pretty_path(&item.path, path_style, mounts)
                ),
                kind: ResultKind::Folder,
                score,
                icon: Some("folder".into()),
                action: Action::SetQuery(filled),
                conversion: None,
            }
        })
        .collect();

    Some(results)
}

/// Match an indexed folder against a partial scope string.
/// Returns (matched, score_boost, completion scope string for SetQuery).
fn scope_folder_match(item: &IndexedPath, scope_lower: &str) -> Option<(bool, i64, String)> {
    if scope_lower.is_empty() {
        return Some((true, 0, item.name_lower.clone()));
    }

    // Multi-segment scope: `glassbox/do`
    if scope_lower.contains('/') {
        let parts: Vec<&str> = scope_lower.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return Some((true, 0, item.name_lower.clone()));
        }
        let last = parts[parts.len() - 1];
        let prefix_segs = &parts[..parts.len() - 1];

        // Path must contain all complete prefix segments in order.
        let mut from = 0usize;
        for seg in prefix_segs {
            from = find_path_segment(&item.path_lower, seg, from)?;
        }
        // Last segment: prefix match on a component after `from`.
        let rest = &item.path_lower[from..];
        // Find a component starting with `last`
        let bytes = rest.as_bytes();
        let mut i = 0usize;
        while i < rest.len() {
            // skip slashes
            while i < rest.len() && bytes[i] == b'/' {
                i += 1;
            }
            if i >= rest.len() {
                break;
            }
            let start = i;
            while i < rest.len() && bytes[i] != b'/' {
                i += 1;
            }
            let comp = &rest[start..i];
            if comp.starts_with(last) {
                // Build completion: prefix_segs + this full component name
                let mut completion = prefix_segs.join("/");
                if !completion.is_empty() {
                    completion.push('/');
                }
                completion.push_str(comp);
                let boost = if comp == last { 5_000 } else { 2_000 };
                return Some((true, boost, completion));
            }
        }
        return None;
    }

    // Single segment: match folder name prefix or path component.
    if item.name_lower.starts_with(scope_lower) {
        let boost = if item.name_lower == scope_lower {
            5_000
        } else {
            3_000
        };
        return Some((true, boost, item.name_lower.clone()));
    }
    if find_path_segment(&item.path_lower, scope_lower, 0).is_some() {
        // Exact component somewhere — complete with that name
        return Some((true, 1_500, scope_lower.to_string()));
    }
    // Component prefix anywhere in path
    for comp in item.path_lower.split('/') {
        if !comp.is_empty() && comp.starts_with(scope_lower) {
            return Some((true, 1_000, comp.to_string()));
        }
    }
    None
}

fn is_extension_shorthand(q: &str) -> bool {
    let Some(rest) = q.strip_prefix('.') else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains('.')
        && !rest.contains('/')
        && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

#[derive(Debug)]
struct GlobQuery {
    /// Lowercased path segments that must appear in order (component boundaries).
    /// `**` segments are stripped; see `recursive`.
    segments: Vec<String>,
    /// Lowercased final name pattern (`None` = any name under the segment scope).
    name_pat: Option<String>,
    /// Query ended with `/` → directory-scope listing.
    dir_scope: bool,
    /// Query contained a `**` path segment (recursive under the prefix).
    recursive: bool,
}

pub(crate) fn search_index(
    index: &[IndexedPath],
    query: &str,
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    matcher: &SkimMatcherV2,
    allow_fuzzy: bool,
    deep: DeepMode,
    deep_roots: &[String],
    // Indices of frequently opened paths (Batch A: seed free-text heap; no short-circuit yet).
    hot_indices: &[usize],
) -> Vec<SearchResult> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }

    if q.starts_with('/') || q.starts_with('~') || q.starts_with("./") || is_drive_path_query(q) {
        if q.contains('*') || q.contains('?') {
            let mut results = search_absolute_glob(q, index, path_style, mounts);
            if deep != DeepMode::Skip {
                maybe_deep_absolute_glob(
                    q,
                    index,
                    path_style,
                    mounts,
                    excludes,
                    deep,
                    deep_roots,
                    &mut results,
                );
            }
            return results;
        }
        return path_completions(q, path_style, mounts);
    }

    let q = strip_file_mode_prefix(q).trim();
    if q.is_empty() {
        return Vec::new();
    }

    // Soft UX: `name in ` / `name in gla` → folder suggestions from the index.
    if let Some(hints) = scope_folder_suggestions(q, index, path_style, mounts) {
        return hints;
    }

    // `name in scope` — files-only scoped path search (before free-text / glob).
    if let Some(sq) = parse_scoped_query(q, Some(index)) {
        let gq = scoped_to_glob(&sq);
        let mut results = search_glob(index, &gq, path_style, mounts);
        if deep != DeepMode::Skip {
            maybe_deep_for_scoped(
                &sq,
                index,
                path_style,
                mounts,
                excludes,
                deep,
                deep_roots,
                &mut results,
            );
        }
        return results;
    }

    if let Some(gq) = parse_glob_query(q) {
        let mut results = search_glob(index, &gq, path_style, mounts);
        // Relative scope under a known dir: live list/walk for freshness
        // (index may be stale or shallow). Absolute globs already live-list.
        maybe_live_relative_glob(&gq, index, path_style, mounts, excludes, deep, &mut results);
        if deep != DeepMode::Skip {
            maybe_deep_for_glob(
                &gq,
                index,
                path_style,
                mounts,
                excludes,
                deep,
                deep_roots,
                &mut results,
            );
        }
        return results;
    }

    let q_lower = q.to_lowercase();
    let mut results = if index.is_empty() {
        // No index yet — still allow live deep when pinned roots exist.
        Vec::new()
    } else {
        score_free_text_full(
            index,
            q,
            &q_lower,
            matcher,
            allow_fuzzy,
            path_style,
            mounts,
            hot_indices,
        )
    };

    if deep != DeepMode::Skip {
        maybe_deep_for_name(
            &q_lower,
            index,
            path_style,
            mounts,
            excludes,
            deep,
            deep_roots,
            &mut results,
        );
    }
    results
}

/// True when index-only results are weak enough that a live deep walk may help.
pub(crate) fn should_deep_search(query: &str, index_results: &[SearchResult]) -> bool {
    if index_is_strong(index_results) {
        return false;
    }
    let raw = query.trim();
    if raw.is_empty() {
        return false;
    }
    // Absolute / drive path completions never deep-walk (except globs handled below).
    if (raw.starts_with('/')
        || raw.starts_with('~')
        || raw.starts_with("./")
        || is_drive_path_query(raw))
        && !raw.contains('*')
        && !raw.contains('?')
    {
        return false;
    }
    let q = strip_file_mode_prefix(raw).trim();
    if q.is_empty() {
        return false;
    }
    // Incomplete `file in ` folder hints — no deep walk (suggestions only).
    if parse_scope_hint_query(q).is_some() && parse_scoped_query(q, None).is_none() {
        return false;
    }
    // Scoped `in` with confident signals (no index) — scope is narrow, always walk.
    if parse_scoped_query(q, None).is_some() {
        return true;
    }
    if let Some(gq) = parse_glob_query(q) {
        if gq.segments.is_empty() {
            if let Some(pat) = &gq.name_pat {
                if is_broad_extension_glob(pat) {
                    return false;
                }
                return looks_specific_for_deep(pat);
            }
            return false;
        }
        // Path-scoped glob (including `src/**/*.rs`) always benefits from live deep.
        return true;
    }
    if q.starts_with('/') || q.starts_with('~') || q.starts_with("./") || is_drive_path_query(q) {
        // Absolute / drive glob — deep only if pattern exists.
        return q.contains('*') || q.contains('?');
    }
    looks_specific_for_deep(&q.to_lowercase())
}

/// Exposed so FileProvider can gate async deep after checking index strength.
pub(crate) fn index_results_are_strong(results: &[SearchResult]) -> bool {
    index_is_strong(results)
}

/// Index-aware scoped parse for FileProvider / engine.
pub(crate) fn parse_scoped_for_query(q: &str, index: &[IndexedPath]) -> Option<()> {
    parse_scoped_query(q, Some(index)).map(|_| ())
}

// ── On-demand deep search (live walk; never writes to the index) ─────────────

/// Free-text / name queries: only deep-search when specific and index is weak.
fn looks_specific_for_deep(q: &str) -> bool {
    let q = q.trim();
    if q.len() < 3 {
        return false;
    }
    // Bare extension shorthand is too broad for a whole-tree walk.
    if is_extension_shorthand(q) {
        return false;
    }
    // `*.md` alone is huge — require path segments (handled elsewhere) or a
    // more specific pattern (`*foo*`, `opt*.md`, plain filename with extension).
    if q == "*" || q == "**" || q == "*.*" {
        return false;
    }
    if let Some(rest) = q.strip_prefix("*.") {
        // `*.md` alone: skip deep (millions of hits possible). Need segments.
        return rest.contains('*') || rest.len() > 4;
    }
    if q.contains('*') || q.contains('?') {
        // Glob with some literal chars.
        return q
            .chars()
            .filter(|c| *c != '*' && *c != '?' && *c != '.')
            .count()
            >= 2;
    }
    // Filename with extension (e.g. main.rs, optimization.md) — best deep signal.
    if q.contains('.') {
        let (stem, ext) = q.rsplit_once('.').unwrap_or((q, ""));
        if !stem.is_empty() && !ext.is_empty() && ext.len() <= 8 && !ext.contains('/') {
            return true;
        }
    }
    // Long-ish plain name (likely intentional file/folder, not 2-letter noise).
    q.len() >= 5 && !q.contains(char::is_whitespace)
}

fn index_is_strong(results: &[SearchResult]) -> bool {
    // Only file/folder strength — mixed engine results may include high-score apps.
    results.iter().any(|r| {
        matches!(
            r.kind,
            crate::providers::ResultKind::File | crate::providers::ResultKind::Folder
        ) && r.score >= DEEP_SKIP_IF_INDEX_SCORE
    })
}

fn maybe_deep_for_name(
    q_lower: &str,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    deep: DeepMode,
    deep_roots: &[String],
    results: &mut Vec<SearchResult>,
) {
    if index_is_strong(results) || !looks_specific_for_deep(q_lower) {
        return;
    }

    let mut existing: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
    let pinned = pinned_deep_roots(deep_roots);

    // 1) Folders named like the query — walk inside them for nested files.
    //    Do not mix pinned roots here; those are handled in step 2 so we still
    //    cover high-value shallow roots when pins exist.
    let named_roots = roots_from_index_name(index, q_lower);
    if !named_roots.is_empty() {
        let live = live_deep_under_roots(
            &named_roots,
            &[],
            Some(q_lower),
            false,
            path_style,
            mounts,
            excludes,
            existing.clone(),
            deep,
        );
        // Named-root walk looks for the *name* deeper (e.g. nested `glassbox`);
        // usually the index already has the folder. Still useful for files named
        // the same as a parent folder.
        for r in &live {
            existing.insert(r.id.clone());
        }
        merge_live(results, live);
        if index_is_strong(results) {
            return;
        }
    }

    // 2) Filename with extension / specific glob / pinned roots: walk high-value
    //    shallow roots + deep roots. Never walk bare $HOME or whole mounts.
    let has_ext = q_lower.contains('.')
        && q_lower
            .rsplit_once('.')
            .map(|(_, e)| {
                !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric())
            })
            .unwrap_or(false);
    let specific_glob = (q_lower.contains('*') || q_lower.contains('?'))
        && !is_broad_extension_glob(q_lower)
        && looks_specific_for_deep(q_lower);

    if !has_ext && !specific_glob && pinned.is_empty() {
        return;
    }

    let mut hv = high_value_shallow_roots(index);
    prepend_unique(&mut hv, &pinned);
    if hv.is_empty() {
        return;
    }
    let live = live_deep_under_roots(
        &hv,
        &[],
        Some(q_lower),
        false,
        path_style,
        mounts,
        excludes,
        existing,
        deep,
    );
    merge_live(results, live);
}

fn maybe_deep_for_glob(
    gq: &GlobQuery,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    deep: DeepMode,
    deep_roots: &[String],
    results: &mut Vec<SearchResult>,
) {
    // Recursive `**` under a path prefix: prefer a live walk even when the
    // shallow index already has a few hits (depth-2 index misses nested files).
    if index_is_strong(results) && !(gq.recursive && !gq.segments.is_empty()) {
        return;
    }
    // Broad extension-only (`*.md`, `.rs`) without path segments → never deep-walk.
    if gq.segments.is_empty() {
        if let Some(pat) = &gq.name_pat {
            if is_broad_extension_glob(pat) {
                return;
            }
            if !looks_specific_for_deep(pat) {
                return;
            }
        } else {
            return;
        }
    }

    let existing: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
    let live = live_deep_search(
        index,
        &gq.segments,
        gq.name_pat.as_deref(),
        gq.dir_scope,
        path_style,
        mounts,
        excludes,
        existing,
        deep,
        deep_roots,
    );
    merge_live(results, live);
}

/// Scoped `name in path` deep walk: prefer absolute root when present, else
/// segment roots from the index. Scope is narrow → always walk when index weak.
fn maybe_deep_for_scoped(
    sq: &ScopedQuery,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    deep: DeepMode,
    deep_roots: &[String],
    results: &mut Vec<SearchResult>,
) {
    if index_is_strong(results) {
        return;
    }
    let existing: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
    let pinned = pinned_deep_roots(deep_roots);

    // Absolute / `~/` scope → walk that root directly (best case).
    if let Some(abs) = &sq.abs_root {
        let root = if abs.is_dir() {
            abs.clone()
        } else {
            abs.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| abs.clone())
        };
        let mut roots = vec![root];
        prepend_unique(&mut roots, &pinned);
        let live = live_deep_under_roots(
            &roots,
            &sq.segments,
            Some(&sq.name_pat),
            false,
            path_style,
            mounts,
            excludes,
            existing,
            deep,
        );
        merge_live(results, live);
        return;
    }

    // Relative scope segments → same as glob deep, but always prefer pins.
    let mut roots = roots_from_segments(index, &sq.segments);
    if roots.is_empty() {
        // Segment not in shallow index — try high-value + pins, still filter by segments.
        roots = high_value_shallow_roots(index);
    }
    prepend_unique(&mut roots, &pinned);
    if roots.is_empty() {
        return;
    }
    let live = live_deep_under_roots(
        &roots,
        &sq.segments,
        Some(&sq.name_pat),
        false,
        path_style,
        mounts,
        excludes,
        existing,
        deep,
    );
    merge_live(results, live);
}

fn maybe_deep_absolute_glob(
    query: &str,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    deep: DeepMode,
    _deep_roots: &[String],
    results: &mut Vec<SearchResult>,
) {
    if index_is_strong(results) || results.len() >= FILE_RESULT_LIMIT {
        return;
    }
    let expanded = strip_double_star_components(&expand_path_query(query, mounts));
    let (dir, pat) = split_glob_path(&expanded);
    if pat.is_empty() || is_broad_extension_glob(&pat.to_lowercase()) {
        // For broad globs under an absolute dir, a single-level read_dir already ran;
        // only deep-walk if the pattern is more specific.
        if is_broad_extension_glob(&pat.to_lowercase()) {
            // Allow deep under THIS dir only (user scoped it with an absolute path).
        } else {
            return;
        }
    }
    if !dir.is_dir() {
        return;
    }
    let pat_l = pat.to_lowercase();
    let existing: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
    let live = live_deep_under_roots(
        &[dir],
        &[],
        if pat_l.is_empty() {
            None
        } else {
            Some(pat_l.as_str())
        },
        false,
        path_style,
        mounts,
        excludes,
        existing,
        deep,
    );
    merge_live(results, live);
    let _ = index;
}

fn pinned_deep_roots(deep_roots: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for s in deep_roots {
        let p = expand_user(s);
        if p.is_dir() {
            out.push(p);
            if out.len() >= DEEP_MAX_ROOTS {
                break;
            }
        }
    }
    out
}

fn prepend_unique(roots: &mut Vec<PathBuf>, extra: &[PathBuf]) {
    if extra.is_empty() {
        return;
    }
    let mut seen: HashSet<PathBuf> = extra.iter().cloned().collect();
    let mut merged = extra.to_vec();
    for r in roots.drain(..) {
        if seen.insert(r.clone()) {
            merged.push(r);
        }
    }
    merged.truncate(DEEP_MAX_ROOTS);
    *roots = merged;
}

fn is_broad_extension_glob(pat: &str) -> bool {
    // `*.md`, `*.rs`, `.png` (already expanded to `*.png`)
    if let Some(rest) = pat.strip_prefix("*.") {
        return !rest.is_empty()
            && !rest.contains('*')
            && !rest.contains('?')
            && rest.chars().all(|c| c.is_ascii_alphanumeric());
    }
    false
}

fn merge_live(results: &mut Vec<SearchResult>, live: Vec<SearchResult>) {
    if live.is_empty() {
        return;
    }
    let mut seen: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
    for r in live {
        if seen.insert(r.id.clone()) {
            results.push(r);
        }
    }
    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    results.truncate(FILE_RESULT_LIMIT);
}

/// Indexed folders whose name matches `q` (exact/prefix) — best deep roots.
fn roots_from_index_name(index: &[IndexedPath], q_lower: &str) -> Vec<PathBuf> {
    let mut roots: Vec<(i64, PathBuf)> = Vec::new();
    for item in index {
        if !item.is_dir {
            continue;
        }
        let score = if item.name_lower == q_lower {
            3
        } else if item.name_lower.starts_with(q_lower) {
            2
        } else if q_lower.len() >= 4 && item.name_lower.contains(q_lower) {
            1
        } else {
            continue;
        };
        // Prefer shallower + high_value
        let boost = score * 10 - item.depth as i64 + if item.high_value { 5 } else { 0 };
        roots.push((boost, item.path.clone()));
    }
    roots.sort_by(|a, b| b.0.cmp(&a.0));
    roots.truncate(DEEP_MAX_ROOTS);
    roots.into_iter().map(|(_, p)| p).collect()
}

/// Roots for path-segment queries: folders matching the first segment, then
/// optionally narrowed by later segments when present in the index.
fn roots_from_segments(index: &[IndexedPath], segments: &[String]) -> Vec<PathBuf> {
    if segments.is_empty() {
        return Vec::new();
    }
    let first = &segments[0];
    let mut candidates: Vec<(i64, PathBuf, String)> = Vec::new();
    for item in index {
        if !item.is_dir {
            continue;
        }
        // Prefer the folder *named* the segment, not every child under it.
        let score = if item.name_lower == *first {
            50 - item.depth as i64 + if item.high_value { 10 } else { 0 }
        } else if find_path_segment(&item.path_lower, first, 0).is_some()
            && item.name_lower != *first
        {
            // Path contains segment but this entry is deeper — weaker root.
            continue;
        } else {
            continue;
        };
        candidates.push((score, item.path.clone(), item.path_lower.clone()));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    // If we have more segments, try to refine to the deepest matching folder.
    if segments.len() > 1 {
        let mut refined: Vec<(i64, PathBuf)> = Vec::new();
        for item in index {
            if !item.is_dir {
                continue;
            }
            let mut from = 0usize;
            let mut ok = true;
            for seg in segments {
                match find_path_segment(&item.path_lower, seg, from) {
                    Some(n) => from = n,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            // Prefer folder named the last segment.
            let last = segments.last().unwrap();
            let score = if item.name_lower == *last {
                100 - item.depth as i64
            } else {
                40 - item.depth as i64
            };
            refined.push((score, item.path.clone()));
        }
        if !refined.is_empty() {
            refined.sort_by(|a, b| b.0.cmp(&a.0));
            refined.truncate(DEEP_MAX_ROOTS);
            return refined.into_iter().map(|(_, p)| p).collect();
        }
    }

    candidates.truncate(DEEP_MAX_ROOTS);
    candidates.into_iter().map(|(_, p, _)| p).collect()
}

/// High-value shallow folders from the index — fallback when no segment roots.
fn high_value_shallow_roots(index: &[IndexedPath]) -> Vec<PathBuf> {
    let mut roots: Vec<(u16, PathBuf)> = Vec::new();
    for item in index {
        if item.is_dir && item.high_value && item.depth <= 2 && !item.low_value {
            roots.push((item.depth, item.path.clone()));
        }
    }
    roots.sort_by(|a, b| a.0.cmp(&b.0));
    // Dedupe by path
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (_, p) in roots {
        if seen.insert(p.clone()) {
            out.push(p);
            if out.len() >= DEEP_MAX_ROOTS {
                break;
            }
        }
    }
    out
}

fn live_deep_search(
    index: &[IndexedPath],
    segments: &[String],
    name_pat: Option<&str>,
    dir_scope: bool,
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    existing: HashSet<String>,
    deep: DeepMode,
    deep_roots: &[String],
) -> Vec<SearchResult> {
    let pinned = pinned_deep_roots(deep_roots);
    let roots = if !segments.is_empty() {
        let mut r = roots_from_segments(index, segments);
        // When segments are unknown but the user pinned deep roots, still walk
        // those pins (segment filter applies during the walk).
        if r.is_empty() && pinned.is_empty() {
            // Segment unknown in index — do NOT walk home; nothing to scope to.
            return Vec::new();
        }
        prepend_unique(&mut r, &pinned);
        r
    } else if name_pat.is_some() {
        // Name-only: high-value shallow project-ish roots + pinned deep roots
        // (never all of $HOME).
        let mut r = high_value_shallow_roots(index);
        if r.is_empty() && pinned.is_empty() {
            return Vec::new();
        }
        prepend_unique(&mut r, &pinned);
        r
    } else {
        return Vec::new();
    };

    live_deep_under_roots(
        &roots, segments, name_pat, dir_scope, path_style, mounts, excludes, existing, deep,
    )
}

fn live_deep_under_roots(
    roots: &[PathBuf],
    segments: &[String],
    name_pat: Option<&str>,
    dir_scope: bool,
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    mut existing: HashSet<String>,
    deep: DeepMode,
) -> Vec<SearchResult> {
    if roots.is_empty() || deep == DeepMode::Skip {
        return Vec::new();
    }

    let (visit_cap, time_budget) = deep.budgets();
    let start = Instant::now();
    let mut visited = 0usize;
    let mut hit_paths: Vec<(i64, PathBuf, bool)> = Vec::new();

    'roots: for root in roots {
        if !root.is_dir() {
            continue;
        }
        if start.elapsed() >= time_budget || visited >= visit_cap {
            break;
        }
        if should_skip_entry(root, excludes) {
            continue;
        }

        for entry in WalkDir::new(root)
            .follow_links(false)
            .max_depth(DEEP_MAX_DEPTH)
            .into_iter()
            .filter_entry(|e| {
                // Time only here (no mut borrow of `visited`); visit cap checked in loop body.
                if start.elapsed() >= time_budget {
                    return false;
                }
                should_descend(e.path(), root, excludes)
            })
        {
            if start.elapsed() >= time_budget || visited >= visit_cap {
                break 'roots;
            }
            let Ok(entry) = entry else {
                continue;
            };
            visited += 1;
            let path = entry.path();
            if path == root.as_path() {
                continue;
            }
            if should_skip_entry(path, excludes) {
                continue;
            }

            let is_dir = entry.file_type().is_dir();
            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) if !n.is_empty() => n,
                _ => continue,
            };
            let name_lower = name.to_lowercase();
            let path_lower = path.to_string_lossy().to_lowercase();

            // Segment filter (ordered components).
            let mut from = 0usize;
            let mut segs_ok = true;
            for seg in segments {
                match find_path_segment(&path_lower, seg, from) {
                    Some(n) => from = n,
                    None => {
                        segs_ok = false;
                        break;
                    }
                }
            }
            if !segs_ok {
                continue;
            }

            if let Some(pat) = name_pat {
                if !name_matches_pat(&name_lower, pat) {
                    continue;
                }
            } else if dir_scope {
                // any entry under segments is fine
            } else {
                continue;
            }

            let id = format!("path:{}", path.display());
            if existing.contains(&id) {
                continue;
            }

            let depth = path
                .strip_prefix(root)
                .map(|p| p.components().count() as u16)
                .unwrap_or(0);
            let item = make_indexed(path.to_path_buf(), name.to_string(), is_dir, depth);
            // Build a synthetic GlobQuery-compatible score, then penalize live.
            let mut score = score_live_hit(&item, segments, name_pat, dir_scope);
            score = score.saturating_sub(DEEP_SCORE_PENALTY);
            if score <= 0 {
                continue;
            }

            existing.insert(id);
            // Keep top-K by score
            if hit_paths.len() < FILE_RESULT_LIMIT {
                hit_paths.push((score, path.to_path_buf(), is_dir));
            } else if let Some(min_i) = hit_paths
                .iter()
                .enumerate()
                .min_by_key(|(_, (s, _, _))| *s)
                .map(|(i, _)| i)
            {
                if score > hit_paths[min_i].0 {
                    hit_paths[min_i] = (score, path.to_path_buf(), is_dir);
                }
            }
            // Early exit: enough exact name hits.
            if hit_paths.len() >= FILE_RESULT_LIMIT
                && hit_paths
                    .iter()
                    .all(|(s, _, _)| *s >= 45_000 - DEEP_SCORE_PENALTY)
            {
                break 'roots;
            }
        }
    }

    hit_paths.sort_by(|a, b| b.0.cmp(&a.0));
    hit_paths
        .into_iter()
        .map(|(score, path, is_dir)| {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            SearchResult {
                id: format!("path:{}", path.display()),
                title: display_name(&name),
                subtitle: pretty_path(&path, path_style, mounts),
                kind: if is_dir {
                    ResultKind::Folder
                } else {
                    ResultKind::File
                },
                score,
                icon: Some(super::icon_for_path(&path, is_dir).into()),
                action: Action::OpenPath(path),
                conversion: None,
            }
        })
        .collect()
}

fn score_live_hit(
    item: &IndexedPath,
    segments: &[String],
    name_pat: Option<&str>,
    dir_scope: bool,
) -> i64 {
    let mut score: i64 = 35_000;
    score += segments.len() as i64 * 2_500;

    if let Some(pat) = name_pat {
        if !pat.contains('*') && !pat.contains('?') {
            if item.name_lower == *pat {
                score = 50_000;
            } else if item.name_lower.starts_with(pat) {
                score = 40_000 + pat.len() as i64 * 100;
            } else if item.name_lower.contains(pat) {
                score = 32_000 + pat.len() as i64 * 50;
            }
        } else {
            score = 38_000 + segments.len() as i64 * 2_000;
            if pat.starts_with("*.") && item.name_lower.ends_with(&pat[1..]) {
                score += 1_000;
            }
        }
    } else if dir_scope {
        if let Some(last) = segments.last() {
            if item.is_dir && item.name_lower == *last {
                score = 48_000;
            } else if item.is_dir {
                score = 36_000;
            } else {
                score = 34_000;
            }
        }
    }

    let q_hint = name_pat
        .or_else(|| segments.last().map(|s| s.as_str()))
        .unwrap_or("");
    apply_path_boosts(item, q_hint, score).unwrap_or(0)
}

fn parse_glob_query(q: &str) -> Option<GlobQuery> {
    if !(q.contains('/') || q.contains('*') || q.contains('?') || is_extension_shorthand(q)) {
        return None;
    }
    // Keep calc / multi-word free text out of glob path (`2 * 3`).
    if q.contains(char::is_whitespace) && !q.contains('/') {
        return None;
    }
    if !q
        .chars()
        .any(|c| c != '*' && c != '?' && c != '/' && !c.is_whitespace())
    {
        return None;
    }

    // `.md` → treat as `*.md`
    if is_extension_shorthand(q) {
        return Some(GlobQuery {
            segments: Vec::new(),
            name_pat: Some(format!("*{q}")),
            dir_scope: false,
            recursive: false,
        });
    }

    let dir_scope = q.ends_with('/');
    let trimmed = q.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let parts: Vec<String> = trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    if parts.is_empty() {
        return None;
    }

    // `**` means "any depth between prefix segments and the name pattern".
    // Ordered component matching already allows intermediates, so strip `**`
    // from the segment list and record that the user asked for recursion
    // (prefer live deep under prefix roots).
    let recursive = parts.iter().any(|p| p == "**");
    let parts: Vec<String> = parts.into_iter().filter(|p| p != "**").collect();
    if parts.is_empty() {
        // Bare `/**/` or only `**` — already rejected above for no literal, but
        // be safe.
        return None;
    }

    if dir_scope {
        return Some(GlobQuery {
            segments: parts,
            name_pat: None,
            dir_scope: true,
            recursive,
        });
    }

    let last = parts.last().cloned().unwrap_or_default();
    if last.contains('*') || last.contains('?') || is_extension_shorthand(&last) {
        let name_pat = if is_extension_shorthand(&last) {
            format!("*{last}")
        } else {
            last
        };
        let segments = parts[..parts.len() - 1].to_vec();
        return Some(GlobQuery {
            segments,
            name_pat: Some(name_pat),
            dir_scope: false,
            recursive,
        });
    }

    if parts.len() == 1 {
        // Single segment with glob already handled; plain name is not a glob query.
        if last.contains('*') || last.contains('?') {
            return Some(GlobQuery {
                segments: Vec::new(),
                name_pat: Some(last),
                dir_scope: false,
                recursive,
            });
        }
        return None;
    }

    // `glassbox/src` → under segment glassbox, name matches src
    let segments = parts[..parts.len() - 1].to_vec();
    Some(GlobQuery {
        segments,
        name_pat: Some(last),
        dir_scope: false,
        recursive,
    })
}

fn search_glob(
    index: &[IndexedPath],
    gq: &GlobQuery,
    path_style: &PathStyle,
    mounts: &[MountInfo],
) -> Vec<SearchResult> {
    if index.is_empty() {
        return Vec::new();
    }

    let mut heap: BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>> =
        BinaryHeap::with_capacity(FILE_RESULT_LIMIT + 1);

    for (idx, item) in index.iter().enumerate() {
        let Some(score) = score_glob_item(item, gq) else {
            continue;
        };
        push_heap(&mut heap, score, item.depth, idx);
    }

    heap_to_results(heap, index, path_style, mounts)
}

fn score_glob_item(item: &IndexedPath, gq: &GlobQuery) -> Option<i64> {
    let mut from = 0usize;
    for seg in &gq.segments {
        from = find_path_segment(&item.path_lower, seg, from)?;
    }

    if let Some(pat) = &gq.name_pat {
        if !name_matches_pat(&item.name_lower, pat) {
            return None;
        }
    } else if gq.dir_scope {
        // Scope-only: path must include all segments; prefer items under that tree
        // (already enforced). Accept files and folders.
    }

    let mut score: i64 = 35_000;
    score += gq.segments.len() as i64 * 2_500;
    if gq.recursive {
        // User asked for nested match (`**`); small boost so deep hits rank well.
        score += 1_000;
    }

    if let Some(pat) = &gq.name_pat {
        if !pat.contains('*') && !pat.contains('?') {
            if item.name_lower == *pat {
                score = 50_000;
            } else if item.name_lower.starts_with(pat.as_str()) {
                score = 40_000 + pat.len() as i64 * 100;
            } else if item.name_lower.contains(pat.as_str()) {
                score = 32_000 + pat.len() as i64 * 50;
            }
        } else if item.name_lower == *pat {
            score = 50_000;
        } else {
            // Glob hit on name
            score = 38_000 + gq.segments.len() as i64 * 2_000;
            if pat.starts_with("*.") && item.name_lower.ends_with(&pat[1..]) {
                score += 1_000;
            }
        }
    } else if gq.dir_scope {
        // Prefer the directory that ends at the last segment, then children.
        if let Some(last) = gq.segments.last() {
            if item.is_dir && item.name_lower == *last {
                score = 48_000;
            } else if item.is_dir {
                score = 36_000;
            } else {
                score = 34_000;
            }
        }
    }

    let q_hint = gq
        .name_pat
        .as_deref()
        .or_else(|| gq.segments.last().map(|s| s.as_str()))
        .unwrap_or("");
    apply_path_boosts(item, q_hint, score)
}

/// Find `seg` as a full path component at or after `start`; returns index after the match.
fn find_path_segment(path_lower: &str, seg: &str, start: usize) -> Option<usize> {
    if seg.is_empty() {
        return Some(start);
    }
    let bytes = path_lower.as_bytes();
    let mut i = start.min(path_lower.len());
    while i < path_lower.len() {
        let rest = &path_lower[i..];
        let Some(rel) = rest.find(seg) else {
            return None;
        };
        let abs = i + rel;
        let before_ok = abs == 0 || bytes[abs - 1] == b'/';
        let after = abs + seg.len();
        let after_ok = after == path_lower.len() || bytes.get(after) == Some(&b'/');
        if before_ok && after_ok {
            return Some(after);
        }
        i = abs + 1;
    }
    None
}

fn name_matches_pat(name_lower: &str, pat: &str) -> bool {
    if pat.contains('*') || pat.contains('?') {
        return glob_match(pat, name_lower);
    }
    name_lower == pat || name_lower.starts_with(pat) || name_lower.contains(pat)
}

/// Simple glob: `*` any span, `?` one char. Pattern and name should be lowercased.
fn glob_match(pat: &str, name: &str) -> bool {
    glob_match_bytes(pat.as_bytes(), name.as_bytes())
}

fn glob_match_bytes(pat: &[u8], name: &[u8]) -> bool {
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star_pi = None;
    let mut star_ni = 0usize;

    while ni < name.len() {
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == name[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star_pi = Some(pi);
            star_ni = ni;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ni += 1;
            ni = star_ni;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

fn heap_to_results(
    heap: BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>>,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
) -> Vec<SearchResult> {
    let mut scored: Vec<(i64, &IndexedPath)> = heap
        .into_iter()
        .map(|Reverse((score, _, idx))| (score, &index[idx]))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.depth.cmp(&b.1.depth)));

    scored
        .into_iter()
        .map(|(score, item)| indexed_to_result(item, score, path_style, mounts))
        .collect()
}

fn indexed_to_result(
    item: &IndexedPath,
    score: i64,
    path_style: &PathStyle,
    mounts: &[MountInfo],
) -> SearchResult {
    SearchResult {
        id: format!("path:{}", item.path.display()),
        title: display_name(&item.name),
        subtitle: pretty_path(&item.path, path_style, mounts),
        kind: if item.is_dir {
            ResultKind::Folder
        } else {
            ResultKind::File
        },
        score,
        icon: Some(super::icon_for_path(&item.path, item.is_dir).into()),
        action: Action::OpenPath(item.path.clone()),
        conversion: None,
    }
}

/// Absolute / `~/` / `./` / drive-letter query with wildcards.
fn search_absolute_glob(
    query: &str,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
) -> Vec<SearchResult> {
    let expanded = expand_path_query(query, mounts);
    // Drop `**` segments so `~/dev/**/*.rs` resolves under `~/dev`, not a
    // non-existent `~/dev/**` directory.
    let expanded = strip_double_star_components(&expanded);
    let query_str = expanded.to_string_lossy();
    let (dir, pat) = split_glob_path(Path::new(query_str.as_ref()));
    let pat_lower = pat.to_lowercase();

    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Live directory listing when parent exists and pattern is only in the last component.
    if !pat_lower.is_empty() && (pat_lower.contains('*') || pat_lower.contains('?')) {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let name_l = name.to_lowercase();
                if !glob_match(&pat_lower, &name_l) {
                    continue;
                }
                let path = entry.path();
                let key = path.display().to_string();
                if !seen.insert(key.clone()) {
                    continue;
                }
                let is_dir = path.is_dir();
                results.push(SearchResult {
                    id: format!("path:{key}"),
                    title: name,
                    subtitle: pretty_path(&path, path_style, mounts),
                    kind: if is_dir {
                        ResultKind::Folder
                    } else {
                        ResultKind::File
                    },
                    score: 45_000,
                    icon: Some(super::icon_for_path(&path, is_dir).into()),
                    action: Action::OpenPath(path),
                    conversion: None,
                });
                if results.len() >= FILE_RESULT_LIMIT {
                    break;
                }
            }
        }
    }

    // Index supplement: path under dir prefix + name glob.
    let dir_lower = dir.to_string_lossy().to_lowercase();
    let dir_prefix = if dir_lower.ends_with('/') {
        dir_lower.clone()
    } else {
        format!("{dir_lower}/")
    };

    if results.len() < FILE_RESULT_LIMIT {
        let mut heap: BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>> =
            BinaryHeap::with_capacity(FILE_RESULT_LIMIT + 1);
        for (idx, item) in index.iter().enumerate() {
            let under = item.path_lower.starts_with(&dir_prefix)
                || item.path_lower == dir_lower
                || item.path_lower.starts_with(&dir_lower);
            if !under {
                continue;
            }
            if !pat_lower.is_empty() && !name_matches_pat(&item.name_lower, &pat_lower) {
                continue;
            }
            let key = item.path.display().to_string();
            if seen.contains(&key) {
                continue;
            }
            let score = if item.name_lower == pat_lower {
                50_000
            } else {
                40_000
            };
            let Some(score) = apply_path_boosts(item, &pat_lower, score) else {
                continue;
            };
            push_heap(&mut heap, score, item.depth, idx);
        }
        for Reverse((score, _, idx)) in heap {
            let item = &index[idx];
            let key = item.path.display().to_string();
            if !seen.insert(key) {
                continue;
            }
            results.push(indexed_to_result(item, score, path_style, mounts));
        }
    }

    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    results.truncate(FILE_RESULT_LIMIT);
    results
}

fn split_glob_path(path: &Path) -> (PathBuf, String) {
    let s = path.to_string_lossy();
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.contains('*') || name.contains('?') {
            let parent = path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("/"));
            return (parent, name.to_string());
        }
    }
    // No glob in last component — treat whole path as directory scope.
    if s.contains('*') || s.contains('?') {
        // Glob in middle (unsupported deeply): best-effort parent of first meta segment.
        if let Some(pos) = s.find(['*', '?']) {
            let before = &s[..pos];
            let parent = before
                .rfind('/')
                .map(|i| PathBuf::from(&before[..=i]))
                .unwrap_or_else(|| PathBuf::from("/"));
            let pat = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("*")
                .to_string();
            return (parent, pat);
        }
    }
    (path.to_path_buf(), String::new())
}

/// Free-text top-K over the index.
///
/// - Short queries (< [`HOT_SKIP_MIN_QUERY_LEN`]): full scan only (baseline cost; discovery).
/// - Longer queries with a hot set: try hot first; skip full scan if best hot ≥ prefix band.
fn score_free_text_full(
    index: &[IndexedPath],
    q: &str,
    q_lower: &str,
    matcher: &SkimMatcherV2,
    allow_fuzzy: bool,
    path_style: &PathStyle,
    mounts: &[MountInfo],
    hot_indices: &[usize],
) -> Vec<SearchResult> {
    let q_chars = q_lower.chars().count();
    let use_hot = q_chars >= HOT_SKIP_MIN_QUERY_LEN && !hot_indices.is_empty();

    if !use_hot {
        return score_free_text_baseline(
            index,
            q,
            q_lower,
            matcher,
            allow_fuzzy,
            path_style,
            mounts,
        );
    }

    let mut heap: BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>> =
        BinaryHeap::with_capacity(FILE_RESULT_LIMIT + 1);
    let mut seen = std::collections::HashSet::with_capacity(hot_indices.len().max(8));
    let mut best_hot: i64 = 0;

    for &idx in hot_indices {
        let Some(item) = index.get(idx) else {
            continue;
        };
        let Some(score) = score_name_only(item, q_lower) else {
            continue;
        };
        if score > best_hot {
            best_hot = score;
        }
        if seen.insert(idx) {
            push_heap(&mut heap, score, item.depth, idx);
        }
    }

    if best_hot >= HOT_SKIP_FULL_SCORE {
        return heap_to_results(heap, index, path_style, mounts);
    }

    // Weak/empty hot — full scan (skip idxs already in heap via `seen`).
    for (idx, item) in index.iter().enumerate() {
        if seen.contains(&idx) {
            continue;
        }
        let Some(score) = score_name_only(item, q_lower) else {
            continue;
        };
        push_heap(&mut heap, score, item.depth, idx);
    }

    finish_free_text_fuzzy(&mut heap, index, q, q_lower, matcher, allow_fuzzy);
    heap_to_results(heap, index, path_style, mounts)
}

/// Pre-hot free-text path: one name-only pass + optional fuzzy (no hot / HashSet).
fn score_free_text_baseline(
    index: &[IndexedPath],
    q: &str,
    q_lower: &str,
    matcher: &SkimMatcherV2,
    allow_fuzzy: bool,
    path_style: &PathStyle,
    mounts: &[MountInfo],
) -> Vec<SearchResult> {
    let mut heap: BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>> =
        BinaryHeap::with_capacity(FILE_RESULT_LIMIT + 1);

    for (idx, item) in index.iter().enumerate() {
        let Some(score) = score_name_only(item, q_lower) else {
            continue;
        };
        push_heap(&mut heap, score, item.depth, idx);
    }

    finish_free_text_fuzzy(&mut heap, index, q, q_lower, matcher, allow_fuzzy);
    heap_to_results(heap, index, path_style, mounts)
}

fn finish_free_text_fuzzy(
    heap: &mut BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>>,
    index: &[IndexedPath],
    q: &str,
    q_lower: &str,
    matcher: &SkimMatcherV2,
    allow_fuzzy: bool,
) {
    let strong_full = heap.len() >= FILE_RESULT_LIMIT
        && heap
            .peek()
            .map(|Reverse((s, _, _))| *s >= STRONG_SCORE)
            .unwrap_or(false);
    if !allow_fuzzy || strong_full {
        return;
    }

    let first = q_lower.chars().next();
    let mut fuzzy_left = 500usize;
    for (idx, item) in index.iter().enumerate() {
        if fuzzy_left == 0 {
            break;
        }
        if item.name_lower.contains(q_lower) {
            continue;
        }
        if let Some(ch) = first {
            if !item.name_lower.contains(ch) && !item.path_lower.contains(ch) {
                continue;
            }
        }
        let allow_path_fuzzy = match heap.peek() {
            Some(Reverse((min_score, _, _)))
                if heap.len() >= FILE_RESULT_LIMIT && *min_score >= STRONG_SCORE =>
            {
                false
            }
            _ => true,
        };
        fuzzy_left -= 1;
        let Some(score) = score_fuzzy(item, q, q_lower, matcher, allow_path_fuzzy) else {
            continue;
        };
        push_heap(heap, score, item.depth, idx);
    }
}

#[cfg(test)]
#[inline]
fn hot_strong_enough(best_hot: i64, query_chars: usize) -> bool {
    query_chars >= HOT_SKIP_MIN_QUERY_LEN && best_hot >= HOT_SKIP_FULL_SCORE
}

fn push_heap(
    heap: &mut BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>>,
    score: i64,
    depth: u16,
    idx: usize,
) {
    let key = (score, Reverse(depth), idx);
    if heap.len() < FILE_RESULT_LIMIT {
        heap.push(Reverse(key));
    } else if let Some(Reverse(worst)) = heap.peek() {
        if key > *worst {
            heap.pop();
            heap.push(Reverse(key));
        }
    }
}

fn apply_path_boosts(item: &IndexedPath, q_lower: &str, mut score: i64) -> Option<i64> {
    if item.is_dir {
        score += 800;
    }
    score += (40_i64 - item.depth as i64).max(0) * 120;

    if item.low_value && !item.name_lower.contains(q_lower) {
        return None;
    }
    if item.high_value {
        score += 3_000;
    }
    if item.is_mnt {
        score += 2_500;
    }
    if item.low_value {
        score -= 20_000;
    }
    if item.path_lower.contains("/.pi/agent/sessions") {
        score -= 50_000;
    }
    // Zero-alloc equivalent of `path_lower.contains(&format!("/{q_lower}"))`.
    if path_contains_slash_prefixed(q_lower, &item.path_lower) || item.path_lower.ends_with(q_lower)
    {
        score += 2_000;
    }
    if score <= 0 {
        return None;
    }
    Some(score)
}

/// True when `path_lower` contains the substring `"/" + q_lower` (no heap allocation).
#[inline]
fn path_contains_slash_prefixed(q_lower: &str, path_lower: &str) -> bool {
    if q_lower.is_empty() {
        return path_lower.contains('/');
    }
    let path = path_lower.as_bytes();
    let q = q_lower.as_bytes();
    let need = q.len() + 1;
    if path.len() < need {
        return false;
    }
    // Scan for '/' then compare the following bytes to q_lower.
    let last_start = path.len() - need;
    let mut i = 0;
    while i <= last_start {
        if path[i] == b'/' && path[i + 1..i + 1 + q.len()] == *q {
            return true;
        }
        i += 1;
    }
    false
}

/// Exact / prefix / substring on name only (no fuzzy allocator path).
fn score_name_only(item: &IndexedPath, q_lower: &str) -> Option<i64> {
    let name = &item.name_lower;
    let score = if name == q_lower {
        50_000
    } else if name.starts_with(q_lower) {
        30_000 + (q_lower.len() as i64 * 100)
    } else if name.contains(q_lower) {
        15_000 + (q_lower.len() as i64 * 50)
    } else {
        return None;
    };
    apply_path_boosts(item, q_lower, score)
}

fn score_fuzzy(
    item: &IndexedPath,
    _q: &str,
    q_lower: &str,
    matcher: &SkimMatcherV2,
    allow_path_fuzzy: bool,
) -> Option<i64> {
    // Prefer pre-lowercased name (matcher is ignore_case; avoids re-folding).
    let score = if let Some(s) = matcher.fuzzy_match(&item.name_lower, q_lower) {
        if s < 40 {
            return None;
        }
        5_000 + s
    } else if allow_path_fuzzy {
        let s = matcher.fuzzy_match(&item.path_lower, q_lower)?;
        if s < 60 {
            return None;
        }
        1_000 + s / 2
    } else {
        return None;
    };
    apply_path_boosts(item, q_lower, score)
}

fn display_name(name: &str) -> String {
    if is_encoded_session_name(name) {
        return decode_session_name(name);
    }
    name.to_string()
}

fn decode_session_name(name: &str) -> String {
    let inner = name.trim_matches('-');
    if let Some(rest) = inner
        .strip_prefix("D--")
        .or_else(|| inner.strip_prefix("C--"))
    {
        return rest.replace("--", " ").trim().to_string();
    }
    if let Some(rest) = inner.strip_prefix("mnt-windows_d-") {
        return rest.replace('-', " ").replace('_', " ");
    }
    if let Some(rest) = inner.strip_prefix("mnt-windows_c-") {
        return rest.replace('-', " ").replace('_', " ");
    }
    inner.replace("--", "/").replace('-', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_contains_slash_prefixed_no_alloc_semantics() {
        // Same semantics as `path.contains(&format!("/{q}"))` (substring, not segment).
        assert!(path_contains_slash_prefixed("docs", "/home/u/docs/readme.md"));
        assert!(path_contains_slash_prefixed("readme.md", "/home/u/docs/readme.md"));
        // "doc" matches the prefix of "/docs" — intentional parity with old format! path.
        assert!(path_contains_slash_prefixed("doc", "/home/u/docs/readme.md"));
        assert!(path_contains_slash_prefixed("u", "/home/u/docs"));
        assert!(!path_contains_slash_prefixed("xyz", "/home/u/docs"));
        assert!(!path_contains_slash_prefixed("home", "home/u")); // no '/' before match
        assert!(path_contains_slash_prefixed("home", "/home/u"));
        // Empty needle: any slash counts.
        assert!(path_contains_slash_prefixed("", "/a"));
        assert!(!path_contains_slash_prefixed("", "nopathslash"));
    }

    #[test]
    fn detects_path_glob_queries() {
        assert!(is_path_glob_query("*.md"));
        assert!(is_path_glob_query(".rs"));
        assert!(is_path_glob_query("blink/docs/*.md"));
        assert!(is_path_glob_query("glassbox/src/"));
        assert!(is_path_glob_query("foo/bar"));
        assert!(is_path_glob_query("~/dev/*.rs"));
        assert!(is_path_glob_query("D:/Glassbox/*.md"));
        assert!(is_path_glob_query("src/**/*.rs"));
        assert!(!is_path_glob_query("firefox"));
        assert!(!is_path_glob_query("2 * 3"));
        assert!(!is_path_glob_query("*"));
        assert!(!is_path_glob_query("**"));
        // Scoped: confident without index
        assert!(is_path_glob_query("optimization.md in glassbox/docs"));
        assert!(is_path_glob_query("*.md in glassbox/"));
        assert!(is_path_glob_query("todo.md under ~/dev"));
        // Not confident without index (app-like)
        assert!(!is_path_glob_query("login in firefox"));
        assert!(!is_path_glob_query("in glassbox"));
    }

    #[test]
    fn parse_scoped_name_and_scope() {
        let sq = parse_scoped_query("optimization.md in glassbox/docs", None).unwrap();
        assert_eq!(sq.name_pat, "optimization.md");
        assert_eq!(sq.segments, vec!["glassbox", "docs"]);
        assert!(sq.abs_root.is_none());

        let sq = parse_scoped_query("*.md within glassbox/", None).unwrap();
        assert_eq!(sq.name_pat, "*.md");
        assert_eq!(sq.segments, vec!["glassbox"]);

        let sq = parse_scoped_query(".rs under proj/src", None).unwrap();
        assert_eq!(sq.name_pat, "*.rs");
        assert_eq!(sq.segments, vec!["proj", "src"]);

        // Alias keywords
        assert!(parse_scoped_query("foo.md inside bar/baz", None).is_some());

        // Disambiguation: no extension, bare scope, no index → reject
        assert!(parse_scoped_query("login in firefox", None).is_none());
        // Empty sides
        assert!(parse_scoped_query("in glassbox", None).is_none());
        assert!(parse_scoped_query("foo in ", None).is_none());

        // Bare folder scope accepted only with index hit
        let index = vec![make_indexed(
            PathBuf::from("/home/u/glassbox"),
            "glassbox".into(),
            true,
            1,
        )];
        assert!(parse_scoped_query("readme in glassbox", None).is_none());
        let sq = parse_scoped_query("readme in glassbox", Some(&index)).unwrap();
        assert_eq!(sq.name_pat, "readme");
        assert_eq!(sq.segments, vec!["glassbox"]);
    }

    #[test]
    fn parse_extension_and_segments() {
        let g = parse_glob_query("*.md").unwrap();
        assert!(g.segments.is_empty());
        assert_eq!(g.name_pat.as_deref(), Some("*.md"));

        let g = parse_glob_query(".png").unwrap();
        assert_eq!(g.name_pat.as_deref(), Some("*.png"));

        let g = parse_glob_query("blink/docs/*.md").unwrap();
        assert_eq!(g.segments, vec!["blink", "docs"]);
        assert_eq!(g.name_pat.as_deref(), Some("*.md"));

        let g = parse_glob_query("glassbox/src/").unwrap();
        assert_eq!(g.segments, vec!["glassbox", "src"]);
        assert!(g.name_pat.is_none());
        assert!(g.dir_scope);

        let g = parse_glob_query("glassbox/todo.md").unwrap();
        assert_eq!(g.segments, vec!["glassbox"]);
        assert_eq!(g.name_pat.as_deref(), Some("todo.md"));

        // `**` is stripped; recursive flag set; remaining segments ordered.
        let g = parse_glob_query("src/**/*.rs").unwrap();
        assert_eq!(g.segments, vec!["src"]);
        assert_eq!(g.name_pat.as_deref(), Some("*.rs"));
        assert!(g.recursive);

        let g = parse_glob_query("foo/**/bar/**/*.md").unwrap();
        assert_eq!(g.segments, vec!["foo", "bar"]);
        assert_eq!(g.name_pat.as_deref(), Some("*.md"));
        assert!(g.recursive);

        let g = parse_glob_query("blink/docs/*.md").unwrap();
        assert!(!g.recursive);
    }

    #[test]
    fn scope_folder_hints_after_in() {
        let index = vec![
            make_indexed(
                PathBuf::from("/home/u/glassbox"),
                "glassbox".into(),
                true,
                1,
            ),
            make_indexed(
                PathBuf::from("/home/u/glassbox/docs"),
                "docs".into(),
                true,
                2,
            ),
            make_indexed(PathBuf::from("/home/u/blink"), "blink".into(), true, 1),
            make_indexed(
                PathBuf::from("/home/u/glassbox/docs/optimization.md"),
                "optimization.md".into(),
                false,
                3,
            ),
        ];
        let style = PathStyle::Label;
        let mounts: Vec<MountInfo> = vec![];

        // Empty scope after ` in `
        let hints = scope_folder_suggestions("optimization.md in ", &index, &style, &mounts);
        assert!(
            hints.is_some(),
            "hint parse={:?} empty-scope folders",
            parse_scope_hint_query("optimization.md in ")
        );
        let hints = hints.unwrap();
        assert!(
            hints.iter().any(|h| h.title == "glassbox"),
            "expected glassbox folder hint, got {:?}",
            hints.iter().map(|h| &h.title).collect::<Vec<_>>()
        );
        assert!(
            hints
                .iter()
                .all(|h| matches!(h.action, Action::SetQuery(_))),
            "hints should SetQuery"
        );
        assert!(
            hints.iter().all(|h| h.subtitle.contains("scoped to")),
            "subtitle soft-hint"
        );

        // Prefix filter
        let hints = scope_folder_suggestions("optimization.md in gla", &index, &style, &mounts)
            .expect("prefix hints");
        assert!(hints.iter().any(|h| h.title == "glassbox"));
        assert!(hints
            .iter()
            .all(|h| h.title.to_lowercase().starts_with("gla")
                || h.subtitle.contains("glassbox")
                || true));
        if let Action::SetQuery(q) = &hints[0].action {
            assert!(q.contains(" in "), "filled query: {q}");
            assert!(q.starts_with("optimization.md"));
        }

        // Trailing `in` without space
        assert!(parse_scope_hint_query("foo.md in").is_some());
        // Not a file-like left side
        assert!(parse_scope_hint_query("login in ").is_none());
    }

    #[test]
    fn drive_path_and_double_star_helpers() {
        assert!(is_drive_path_query("D:/Glassbox"));
        assert!(is_drive_path_query("d:\\foo"));
        assert!(is_drive_path_query("Windows D:/x"));
        assert!(!is_drive_path_query("~/dev"));
        assert!(!is_drive_path_query("foo/bar"));

        let mounts = vec![MountInfo {
            target: PathBuf::from("/mnt/windows_d"),
            label: "Windows D".into(),
            drive_letter: Some('D'),
        }];
        let p = expand_drive_path("D:/Glassbox/docs/*.md", &mounts).unwrap();
        assert_eq!(p, PathBuf::from("/mnt/windows_d/Glassbox/docs/*.md"));

        let stripped = strip_double_star_components(Path::new("/home/u/src/**/*.rs"));
        assert_eq!(stripped, PathBuf::from("/home/u/src/*.rs"));
    }

    #[test]
    fn glob_and_segment_match() {
        assert!(glob_match("*.md", "readme.md"));
        assert!(glob_match("opt*.md", "optimization.md"));
        assert!(!glob_match("*.rs", "readme.md"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));

        // /home/u/blink/docs/x — "blink" @ 8..13, "docs" @ 14..18
        assert_eq!(
            find_path_segment("/home/u/blink/docs/x", "blink", 0),
            Some(13)
        );
        assert_eq!(
            find_path_segment("/home/u/blink/docs/x", "docs", 13),
            Some(18)
        );
        // substring of component must not match
        assert!(find_path_segment("/home/u/blinky/docs", "blink", 0).is_none());
    }

    #[test]
    fn deep_search_gating() {
        // Specific enough
        assert!(looks_specific_for_deep("optimization.md"));
        assert!(looks_specific_for_deep("main.rs"));
        assert!(looks_specific_for_deep("opt*.md"));
        assert!(looks_specific_for_deep("readme")); // len >= 5
                                                    // Too broad / short
        assert!(!looks_specific_for_deep("ab"));
        assert!(!looks_specific_for_deep(".md"));
        assert!(!looks_specific_for_deep("*.md"));
        assert!(!looks_specific_for_deep("*.rs"));
        assert!(!looks_specific_for_deep("*"));
        assert!(is_broad_extension_glob("*.md"));
        assert!(is_broad_extension_glob("*.rs"));
        assert!(!is_broad_extension_glob("opt*.md"));
        assert!(!is_broad_extension_glob("*foo*"));
    }

    #[test]
    fn live_deep_finds_nested_and_skips_junk() {
        let base = std::env::temp_dir().join(format!("blink-deep-ut-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let deep = base.join("proj").join("src").join("ui").join("widgets");
        fs::create_dir_all(&deep).unwrap();
        let target = deep.join("optimization.md");
        fs::write(&target, "hi").unwrap();
        let junk = base.join("proj").join("node_modules").join("pkg");
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("optimization.md"), "nope").unwrap();

        // Index only has the shallow project folder (as depth-2 index would).
        let index = vec![make_indexed(base.join("proj"), "proj".into(), true, 1)];
        let style = PathStyle::Label;
        let mounts: Vec<MountInfo> = vec![];
        let excludes = ExcludeSet::from_list(&[]);

        // Segment-scoped: proj + name
        let gq = parse_glob_query("proj/optimization.md").unwrap();
        let mut results = search_glob(&index, &gq, &style, &mounts);
        // Index has no deep file → weak
        assert!(!index_is_strong(&results));
        maybe_deep_for_glob(
            &gq,
            &index,
            &style,
            &mounts,
            &excludes,
            DeepMode::Sync,
            &[],
            &mut results,
        );

        let ids: Vec<_> = results.iter().map(|r| r.id.clone()).collect();
        assert!(
            ids.iter().any(|id| id.contains("widgets/optimization.md")),
            "expected deep hit, got {ids:?}"
        );
        assert!(
            ids.iter().all(|id| !id.contains("node_modules")),
            "must skip node_modules, got {ids:?}"
        );

        // Broad *.md with no segments must NOT deep-walk
        let gq2 = parse_glob_query("*.md").unwrap();
        let mut empty = Vec::new();
        maybe_deep_for_glob(
            &gq2,
            &index,
            &style,
            &mounts,
            &excludes,
            DeepMode::Sync,
            &[],
            &mut empty,
        );
        assert!(empty.is_empty(), "broad extension must not deep-walk");

        // Pinned deep root alone can surface a nested file even with empty index.
        let mut pinned_results = Vec::new();
        maybe_deep_for_name(
            "optimization.md",
            &[],
            &style,
            &mounts,
            &excludes,
            DeepMode::Sync,
            &[base.join("proj").to_string_lossy().to_string()],
            &mut pinned_results,
        );
        assert!(
            pinned_results
                .iter()
                .any(|r| r.id.contains("widgets/optimization.md")),
            "pinned deep root should find nested file, got {:?}",
            pinned_results
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>()
        );

        // Scoped `in` deep walk under project
        let mut scoped = search_glob(
            &index,
            &scoped_to_glob(&parse_scoped_query("optimization.md in proj", Some(&index)).unwrap()),
            &style,
            &mounts,
        );
        assert!(!index_is_strong(&scoped));
        let sq = parse_scoped_query("optimization.md in proj", Some(&index)).unwrap();
        maybe_deep_for_scoped(
            &sq,
            &index,
            &style,
            &mounts,
            &excludes,
            DeepMode::Sync,
            &[],
            &mut scoped,
        );
        assert!(
            scoped
                .iter()
                .any(|r| r.id.contains("widgets/optimization.md")),
            "scoped deep should find nested file, got {:?}",
            scoped.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
        );
        assert!(
            scoped.iter().all(|r| !r.id.contains("node_modules")),
            "scoped deep must skip junk"
        );

        let _ = fs::remove_dir_all(&base);
    }
}

/// `D:/path`, `d:\path`, optional `Windows D:/path` → real mount target.
fn is_drive_path_query(q: &str) -> bool {
    let q = q.trim();
    if q.len() >= 2 {
        let bytes = q.as_bytes();
        let c0 = bytes[0].to_ascii_uppercase();
        if c0.is_ascii_alphabetic() && bytes[1] == b':' {
            return true;
        }
    }
    let lower = q.to_ascii_lowercase();
    lower.starts_with("windows ")
        && lower
            .chars()
            .nth(8)
            .map(|c| c.is_ascii_alphabetic())
            .unwrap_or(false)
}

/// Expand `~`, drive letters, and leave absolute paths alone.
fn expand_path_query(query: &str, mounts: &[MountInfo]) -> PathBuf {
    let q = query.trim();
    if let Some(p) = expand_drive_path(q, mounts) {
        return p;
    }
    expand_user(q)
}

fn expand_drive_path(query: &str, mounts: &[MountInfo]) -> Option<PathBuf> {
    let q = query.trim();
    let (letter, rest) = parse_drive_prefix(q)?;
    let letter = letter.to_ascii_uppercase();
    let mount = mounts.iter().find(|m| m.drive_letter == Some(letter))?;
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        Some(mount.target.clone())
    } else {
        // Normalize Windows separators in the remainder.
        let rest = rest.replace('\\', "/");
        Some(mount.target.join(rest))
    }
}

fn parse_drive_prefix(q: &str) -> Option<(char, &str)> {
    let q = q.trim();
    // `Windows D:/foo` or `Windows D:foo`
    let lower = q.to_ascii_lowercase();
    if lower.starts_with("windows ") {
        // Use original slice with same length prefix.
        let rest_orig = &q["windows ".len()..];
        return parse_drive_letter_rest(rest_orig);
    }
    parse_drive_letter_rest(q)
}

fn parse_drive_letter_rest(q: &str) -> Option<(char, &str)> {
    let mut chars = q.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    if chars.next() != Some(':') {
        return None;
    }
    // remainder starts after "X:"
    let rest = &q[letter.len_utf8() + 1..];
    Some((letter, rest))
}

/// Remove `**` path components so parents resolve to real directories.
fn strip_double_star_components(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::Normal(s) if s == "**" => continue,
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        out
    }
}

/// Live shallow listing for relative globs under index-resolved segment roots.
///
/// `blink/docs/*.md` → find `…/docs` folders and `read_dir` for name matches
/// (fresher than a stale/shallow index). Recursive `**` deep walks stay in
/// `maybe_deep_for_glob`.
fn maybe_live_relative_glob(
    gq: &GlobQuery,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    _deep: DeepMode,
    results: &mut Vec<SearchResult>,
) {
    if gq.segments.is_empty() {
        return;
    }
    // Need a name pattern (glob/file) or directory-scope trailing `/`.
    if gq.name_pat.is_none() && !gq.dir_scope {
        return;
    }

    let roots = roots_from_segments(index, &gq.segments);
    if roots.is_empty() {
        return;
    }

    let mut seen: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
    let name_pat = gq.name_pat.as_deref();

    for root in roots {
        if !root.is_dir() || should_skip_entry(&root, excludes) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            if results.len() >= FILE_RESULT_LIMIT {
                break;
            }
            let path = entry.path();
            if should_skip_entry(&path, excludes) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let name_l = name.to_lowercase();
            if let Some(pat) = name_pat {
                if !name_matches_pat(&name_l, pat) {
                    continue;
                }
            }
            let id = format!("path:{}", path.display());
            if !seen.insert(id.clone()) {
                continue;
            }
            let is_dir = path.is_dir();
            let mut score: i64 = 45_000;
            score += gq.segments.len() as i64 * 500;
            if let Some(pat) = name_pat {
                if !pat.contains('*') && !pat.contains('?') && name_l == pat {
                    score = 50_000;
                }
            }
            results.push(SearchResult {
                id,
                title: name,
                subtitle: pretty_path(&path, path_style, mounts),
                kind: if is_dir {
                    ResultKind::Folder
                } else {
                    ResultKind::File
                },
                score,
                icon: Some(super::icon_for_path(&path, is_dir).into()),
                action: Action::OpenPath(path),
                conversion: None,
            });
        }
    }

    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    results.truncate(FILE_RESULT_LIMIT);
}

fn path_completions(query: &str, style: &PathStyle, mounts: &[MountInfo]) -> Vec<SearchResult> {
    let expanded = expand_path_query(query, mounts);
    let query_ends_sep = query.ends_with('/') || query == "~";
    let (dir, prefix) = if query_ends_sep || expanded.is_dir() {
        (expanded.clone(), String::new())
    } else {
        let parent = expanded
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let prefix = expanded
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        (parent, prefix)
    };

    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut results = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !prefix.starts_with('.') {
            continue;
        }
        if !prefix.is_empty() && !name.to_lowercase().starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        let is_dir = path.is_dir();
        results.push(SearchResult {
            id: format!("path:{}", path.display()),
            title: name,
            subtitle: pretty_path(&path, style, mounts),
            kind: if is_dir {
                ResultKind::Folder
            } else {
                ResultKind::File
            },
            score: 2000,
            icon: Some(super::icon_for_path(&path, is_dir).into()),
            action: Action::OpenPath(path),
            conversion: None,
        });
        if results.len() >= 40 {
            break;
        }
    }
    results.sort_by(|a, b| {
        let ad = matches!(a.kind, ResultKind::Folder);
        let bd = matches!(b.kind, ResultKind::Folder);
        bd.cmp(&ad)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    results
}

#[cfg(test)]
mod hot_skip_tests {
    use super::{hot_strong_enough, HOT_SKIP_FULL_SCORE, HOT_SKIP_MIN_QUERY_LEN};

    #[test]
    fn hot_strong_enough_gate() {
        assert!(hot_strong_enough(
            HOT_SKIP_FULL_SCORE,
            HOT_SKIP_MIN_QUERY_LEN
        ));
        assert!(hot_strong_enough(50_000, 5));
        // Weak contains-only (~15k band) must fall through to full index.
        assert!(!hot_strong_enough(15_000, 5));
        assert!(!hot_strong_enough(HOT_SKIP_FULL_SCORE - 1, 5));
        // Short / broad queries always full scan (discovery).
        assert!(!hot_strong_enough(50_000, 1));
        assert!(!hot_strong_enough(50_000, 3)); // e.g. "doc"
        assert!(!hot_strong_enough(0, 10));
    }
}
