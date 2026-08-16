//! Glob / path-segment matching primitives and single-level live listings.
//!
//! Pure string helpers (no index scans) plus `read_dir`-based results for
//! absolute / relative globs and path completions.

use super::deep::roots_from_segments;
use super::rank::{apply_path_boosts, heap_to_results, indexed_to_result, push_heap};
use super::FILE_RESULT_LIMIT;
use crate::config::{pretty_path, ExcludeSet, MountInfo, PathStyle};
use crate::providers::files::index::{expand_user, should_skip_entry, IndexedPath};
use crate::providers::{Action, ResultKind, SearchResult};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn is_extension_shorthand(q: &str) -> bool {
    let Some(rest) = q.strip_prefix('.') else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains('.')
        && !rest.contains('/')
        && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

#[derive(Debug)]
pub(super) struct GlobQuery {
    /// Lowercased path segments that must appear in order (component boundaries).
    /// `**` segments are stripped; see `recursive`.
    pub(super) segments: Vec<String>,
    /// Lowercased final name pattern (`None` = any name under the segment scope).
    pub(super) name_pat: Option<String>,
    /// Query ended with `/` → directory-scope listing.
    pub(super) dir_scope: bool,
    /// Query contained a `**` path segment (recursive under the prefix).
    pub(super) recursive: bool,
}

pub(super) fn is_broad_extension_glob(pat: &str) -> bool {
    // `*.md`, `*.rs`, `.png` (already expanded to `*.png`)
    if let Some(rest) = pat.strip_prefix("*.") {
        return !rest.is_empty()
            && !rest.contains('*')
            && !rest.contains('?')
            && rest.chars().all(|c| c.is_ascii_alphanumeric());
    }
    false
}

pub(super) fn search_glob(
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

    // Literal name patterns ("optimization.md in docs") highlight via the
    // substring helper; true globs ("*.md") contain metacharacters that never
    // appear in a filename → no highlight, which is honest.
    let needle = gq.name_pat.as_deref().unwrap_or("");
    heap_to_results(heap, index, needle, &HashMap::new(), path_style, mounts)
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
pub(super) fn find_path_segment(path_lower: &str, seg: &str, start: usize) -> Option<usize> {
    if seg.is_empty() {
        return Some(start);
    }
    let bytes = path_lower.as_bytes();
    let mut i = start.min(path_lower.len());
    while i < path_lower.len() {
        let rest = &path_lower[i..];
        let rel = rest.find(seg)?;
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

pub(super) fn name_matches_pat(name_lower: &str, pat: &str) -> bool {
    if pat.contains('*') || pat.contains('?') {
        return glob_match(pat, name_lower);
    }
    name_lower == pat || name_lower.starts_with(pat) || name_lower.contains(pat)
}

pub(super) fn glob_match(pat: &str, name: &str) -> bool {
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

pub(super) fn search_absolute_glob(
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
                    icon: Some(crate::providers::files::icon_for_path(&path, is_dir).into()),
                    action: Action::OpenPath(path),
                    conversion: None,
                    matched: None,
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
            results.push(indexed_to_result(item, score, None, path_style, mounts));
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

pub(super) fn split_glob_path(path: &Path) -> (PathBuf, String) {
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

pub(super) fn is_drive_path_query(q: &str) -> bool {
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

pub(super) fn expand_path_query(query: &str, mounts: &[MountInfo]) -> PathBuf {
    let q = query.trim();
    if let Some(p) = expand_drive_path(q, mounts) {
        return p;
    }
    expand_user(q)
}

pub(super) fn expand_drive_path(query: &str, mounts: &[MountInfo]) -> Option<PathBuf> {
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

pub(super) fn strip_double_star_components(path: &Path) -> PathBuf {
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

pub(super) fn maybe_live_relative_glob(
    gq: &GlobQuery,
    index: &[IndexedPath],
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
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
                icon: Some(crate::providers::files::icon_for_path(&path, is_dir).into()),
                action: Action::OpenPath(path),
                conversion: None,
                matched: None,
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

pub(super) fn path_completions(
    query: &str,
    style: &PathStyle,
    mounts: &[MountInfo],
) -> Vec<SearchResult> {
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
            icon: Some(crate::providers::files::icon_for_path(&path, is_dir).into()),
            action: Action::OpenPath(path),
            conversion: None,
            matched: None,
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
