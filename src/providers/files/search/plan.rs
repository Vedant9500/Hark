//! Query interpretation: force-files prefixes, scoped `name in …` parsing,
//! glob-query parsing, and folder soft-hints after ` in `.

use super::glob::{
    find_path_segment, is_drive_path_query, is_extension_shorthand, search_glob, GlobQuery,
};
use crate::config::{pretty_path, MountInfo, PathStyle};
use crate::providers::files::index::{expand_user, IndexedPath};
use crate::providers::{Action, ResultKind, SearchResult};
use std::path::PathBuf;

/// Strip optional `f`/`file`/`folder` mode prefix (ASCII case-insensitive).
/// Requires whitespace after the keyword so `firefox` is unchanged.
pub(super) fn strip_file_mode_prefix(q: &str) -> &str {
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

const SCOPE_KEYWORDS: &[&str] = &[" in ", " within ", " under ", " inside "];

/// `optimization.md in glassbox/docs` → name pattern + path segments.
#[derive(Debug, Clone)]
pub(super) struct ScopedQuery {
    /// Lowercased name / glob pattern (always present).
    pub(super) name_pat: String,
    /// Path components that must appear in order.
    pub(super) segments: Vec<String>,
    /// Absolute root when scope was `~/…` or `/…` (preferred deep-walk root).
    pub(super) abs_root: Option<PathBuf>,
}

/// Parse `name in scope`. Returns `None` when the query is not scoped, sides are
/// empty, or disambiguation says it is not a path intent.
///
/// When `index` is provided, a bare folder-name scope is accepted if that folder
/// exists in the index. Without index, only strong signals are accepted
/// (name has extension/glob, or scope looks path-like).
pub(super) fn parse_scoped_query(q: &str, index: Option<&[IndexedPath]>) -> Option<ScopedQuery> {
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

pub(super) fn scoped_to_glob(sq: &ScopedQuery) -> GlobQuery {
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
pub(super) fn parse_scope_hint_query(q: &str) -> Option<(String, String, String)> {
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
pub(super) fn scope_folder_suggestions(
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

pub(crate) fn parse_scoped_for_query(q: &str, index: &[IndexedPath]) -> Option<()> {
    parse_scoped_query(q, Some(index)).map(|_| ())
}

pub(super) fn parse_glob_query(q: &str) -> Option<GlobQuery> {
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
