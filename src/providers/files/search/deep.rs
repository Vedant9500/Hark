//! On-demand live deep search: plan jobs from the index snapshot, then walk
//! bounded roots on a worker thread (never writes to the index).

use super::glob::{
    expand_path_query, find_path_segment, is_broad_extension_glob, is_drive_path_query,
    is_extension_shorthand, name_matches_pat, split_glob_path, strip_double_star_components,
    GlobQuery,
};
use super::plan::{
    parse_glob_query, parse_scope_hint_query, parse_scoped_query, strip_file_mode_prefix,
    ScopedQuery,
};
use super::rank::{apply_path_boosts, display_name};
use super::DEEP_MAX_DEPTH;
use super::DEEP_MAX_ROOTS;
use super::DEEP_SCORE_PENALTY;
use super::DEEP_SKIP_IF_INDEX_SCORE;
use super::DEEP_TIME_BUDGET_ASYNC;
use super::DEEP_TIME_BUDGET_SYNC;
use super::DEEP_VISIT_CAP_ASYNC;
use super::DEEP_VISIT_CAP_SYNC;
use super::FILE_RESULT_LIMIT;
use crate::config::{pretty_path, ExcludeSet, MountInfo, PathStyle};
use crate::providers::files::index::{
    expand_user, make_indexed, should_descend, should_skip_entry, IndexedPath,
};
use crate::providers::{Action, ResultKind, SearchResult};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;
use walkdir::WalkDir;

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
    // One id set for the whole batch, reused across every job + merge instead of
    // rebuilding from `results` per job (which is O(jobs²) on result count).
    let mut existing: HashSet<String> = results.iter().map(|r| r.id.clone()).collect();
    for job in jobs {
        let live = live_deep_under_roots(
            &job.roots,
            &job.segments,
            job.name_pat.as_deref(),
            job.dir_scope,
            path_style,
            mounts,
            excludes,
            &mut existing,
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

pub(super) fn looks_specific_for_deep(q: &str) -> bool {
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

pub(super) fn index_is_strong(results: &[SearchResult]) -> bool {
    // Only file/folder strength — mixed engine results may include high-score apps.
    results.iter().any(|r| {
        matches!(
            r.kind,
            crate::providers::ResultKind::File | crate::providers::ResultKind::Folder
        ) && r.score >= DEEP_SKIP_IF_INDEX_SCORE
    })
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

fn merge_live(results: &mut Vec<SearchResult>, live: Vec<SearchResult>) {
    if live.is_empty() {
        return;
    }
    results.extend(live);
    // Rank once with a precomputed lowercase title key instead of lowercasing
    // per comparison (O(n log n) allocs on the ranking path).
    let mut keyed: Vec<(i64, String, SearchResult)> = results
        .drain(..)
        .map(|r| (r.score, r.title.to_lowercase(), r))
        .collect();
    keyed.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    results.extend(keyed.into_iter().map(|(_, _, r)| r));
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
    roots.sort_by_key(|b| std::cmp::Reverse(b.0));
    roots.truncate(DEEP_MAX_ROOTS);
    roots.into_iter().map(|(_, p)| p).collect()
}

/// Roots for path-segment queries: folders matching the first segment, then
/// optionally narrowed by later segments when present in the index.
pub(super) fn roots_from_segments(index: &[IndexedPath], segments: &[String]) -> Vec<PathBuf> {
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
    candidates.sort_by_key(|b| std::cmp::Reverse(b.0));

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
            refined.sort_by_key(|b| std::cmp::Reverse(b.0));
            refined.truncate(DEEP_MAX_ROOTS);
            return refined.into_iter().map(|(_, p)| p).collect();
        }
    }

    candidates.truncate(DEEP_MAX_ROOTS);
    candidates.into_iter().map(|(_, p, _)| p).collect()
}

fn high_value_shallow_roots(index: &[IndexedPath]) -> Vec<PathBuf> {
    let mut roots: Vec<(u16, PathBuf)> = Vec::new();
    for item in index {
        if item.is_dir && item.high_value && item.depth <= 2 && !item.low_value {
            roots.push((item.depth, item.path.clone()));
        }
    }
    roots.sort_by_key(|a| a.0);
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

#[allow(clippy::too_many_arguments)]
pub(super) fn live_deep_under_roots(
    roots: &[PathBuf],
    segments: &[String],
    name_pat: Option<&str>,
    dir_scope: bool,
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    existing: &mut HashSet<String>,
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
            let item = make_indexed(path.to_path_buf(), name.to_string(), is_dir, depth, false);
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

    hit_paths.sort_by_key(|b| std::cmp::Reverse(b.0));
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
                icon: Some(crate::providers::files::icon_for_path(&path, is_dir).into()),
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
