//! Free-text scoring and ranking: name-only hot/full scans, fuzzy fallback,
//! path boosts, and heap→`SearchResult` conversion.

use super::FILE_RESULT_LIMIT;
use super::HOT_SKIP_FULL_SCORE;
use super::HOT_SKIP_MIN_QUERY_LEN;
use super::STRONG_SCORE;
use crate::config::{pretty_path, MountInfo, PathStyle};
use crate::providers::files::index::{is_encoded_session_name, IndexedPath};
use crate::providers::{title_match_indices, Action, ResultKind, SearchResult};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

pub(super) fn heap_to_results(
    heap: BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>>,
    index: &[IndexedPath],
    q_lower: &str,
    fuzzy_spans: &HashMap<usize, Vec<usize>>,
    path_style: &PathStyle,
    mounts: &[MountInfo],
) -> Vec<SearchResult> {
    let mut scored: Vec<(i64, usize)> = heap
        .into_iter()
        .map(|Reverse((score, _, idx))| (score, idx))
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| index[a.1].depth.cmp(&index[b.1].depth))
    });

    scored
        .into_iter()
        .map(|(score, idx)| {
            let item = &index[idx];
            // Fuzzy spans are char indices into name_lower; they map onto the
            // displayed title only when case folding kept the char count and
            // display_name didn't transform the name.
            let matched = if item.name_lower.chars().count() == item.name.chars().count() {
                fuzzy_spans.get(&idx).cloned()
            } else {
                None
            }
            .or_else(|| title_match_indices(&display_name(&item.name), q_lower));
            indexed_to_result(item, score, matched, path_style, mounts)
        })
        .collect()
}

pub(super) fn indexed_to_result(
    item: &IndexedPath,
    score: i64,
    matched: Option<Vec<usize>>,
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
        icon: Some(crate::providers::files::icon_for_path(&item.path, item.is_dir).into()),
        action: Action::OpenPath(item.path.clone()),
        conversion: None,
        matched,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn score_free_text_full(
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
    let mut fuzzy_spans: HashMap<usize, Vec<usize>> = HashMap::new();

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
        // Strong hot hits — still run the fuzzy pass so fuzzy-only candidates
        // aren't dropped and highlight spans stay consistent with the
        // non-short-circuited path.
        //
        // Exact-name sweep (audit P2): a file exactly matching the query that
        // was created after a prefix-matching file went hot would otherwise
        // never be scanned (50,000 band skipped). A linear equality pass is
        // O(n) cheap string compares — no fuzzy scoring — so the Batch-B
        // perf win survives while exact matches can't be dropped.
        if best_hot < 50_000 {
            for (idx, item) in index.iter().enumerate() {
                if item.name_lower == q_lower && seen.insert(idx) {
                    // Same score the full scan would assign (exact band +
                    // path boosts), so ordering matches the slow path.
                    if let Some(score) = apply_path_boosts(item, q_lower, 50_000) {
                        push_heap(&mut heap, score, item.depth, idx);
                    }
                }
            }
        }
        finish_free_text_fuzzy(
            &mut heap,
            index,
            q,
            q_lower,
            matcher,
            allow_fuzzy,
            &mut fuzzy_spans,
        );
        return heap_to_results(heap, index, q_lower, &fuzzy_spans, path_style, mounts);
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

    finish_free_text_fuzzy(
        &mut heap,
        index,
        q,
        q_lower,
        matcher,
        allow_fuzzy,
        &mut fuzzy_spans,
    );
    heap_to_results(heap, index, q_lower, &fuzzy_spans, path_style, mounts)
}

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
    let mut fuzzy_spans: HashMap<usize, Vec<usize>> = HashMap::new();

    for (idx, item) in index.iter().enumerate() {
        let Some(score) = score_name_only(item, q_lower) else {
            continue;
        };
        push_heap(&mut heap, score, item.depth, idx);
    }

    finish_free_text_fuzzy(
        &mut heap,
        index,
        q,
        q_lower,
        matcher,
        allow_fuzzy,
        &mut fuzzy_spans,
    );
    heap_to_results(heap, index, q_lower, &fuzzy_spans, path_style, mounts)
}

fn finish_free_text_fuzzy(
    heap: &mut BinaryHeap<Reverse<(i64, Reverse<u16>, usize)>>,
    index: &[IndexedPath],
    q: &str,
    q_lower: &str,
    matcher: &SkimMatcherV2,
    allow_fuzzy: bool,
    fuzzy_spans: &mut HashMap<usize, Vec<usize>>,
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
        let allow_path_fuzzy = !matches!(
            heap.peek(),
            Some(Reverse((min_score, _, _)))
                if heap.len() >= FILE_RESULT_LIMIT && *min_score >= STRONG_SCORE
        );
        fuzzy_left -= 1;
        if let Some((score, spans)) = score_fuzzy(item, q, q_lower, matcher, allow_path_fuzzy) {
            if let Some(sp) = spans {
                fuzzy_spans.insert(idx, sp);
            }
            push_heap(heap, score, item.depth, idx);
        } else {
            // Failed scoring didn't score — refund so it doesn't burn budget.
            fuzzy_left += 1;
        }
    }
}

#[cfg(test)]
#[inline]
pub(super) fn hot_strong_enough(best_hot: i64, query_chars: usize) -> bool {
    query_chars >= HOT_SKIP_MIN_QUERY_LEN && best_hot >= HOT_SKIP_FULL_SCORE
}

pub(super) fn push_heap(
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

pub(super) fn apply_path_boosts(item: &IndexedPath, q_lower: &str, mut score: i64) -> Option<i64> {
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
pub(super) fn path_contains_slash_prefixed(q_lower: &str, path_lower: &str) -> bool {
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

/// Name fuzzy matches carry their matcher char spans for title highlighting;
/// path fuzzy matches return `None` (the title didn't match).
fn score_fuzzy(
    item: &IndexedPath,
    _q: &str,
    q_lower: &str,
    matcher: &SkimMatcherV2,
    allow_path_fuzzy: bool,
) -> Option<(i64, Option<Vec<usize>>)> {
    // Prefer pre-lowercased name (matcher is ignore_case; avoids re-folding).
    let (score, spans) =
        if let Some((s, indices)) = matcher.fuzzy_indices(&item.name_lower, q_lower) {
            if s < 40 {
                return None;
            }
            (5_000 + s, Some(indices))
        } else if allow_path_fuzzy {
            let s = matcher.fuzzy_match(&item.path_lower, q_lower)?;
            if s < 60 {
                return None;
            }
            (1_000 + s / 2, None)
        } else {
            return None;
        };
    apply_path_boosts(item, q_lower, score).map(|s| (s, spans))
}

pub(super) fn display_name(name: &str) -> String {
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
        return rest.replace(['-', '_'], " ");
    }
    if let Some(rest) = inner.strip_prefix("mnt-windows_c-") {
        return rest.replace(['-', '_'], " ");
    }
    inner.replace("--", "/").replace('-', " ")
}
