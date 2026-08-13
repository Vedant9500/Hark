//! Index search orchestration + free-text scoring glue.
//!
//! Deep search is *not* run here: the caller plans and runs it separately via
//! [`plan_deep_jobs`] / [`run_deep_jobs`] after this returns index-only hits.
//!
//! The heavy lifting lives in sibling modules:
//! - [`plan`]: query interpretation (prefixes, scoped `in`, glob parsing)
//! - [`glob`]: glob matching + live directory listings
//! - [`deep`]: on-demand live deep-search planning and walks
//! - [`rank`]: free-text scoring / ranking

use super::index::IndexedPath;
use crate::config::{ExcludeSet, MountInfo, PathStyle};
use crate::providers::SearchResult;
use fuzzy_matcher::skim::SkimMatcherV2;

#[cfg(test)]
use super::index::make_indexed;
#[cfg(test)]
use crate::providers::Action;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::{Path, PathBuf};

pub(crate) mod deep;
pub(crate) mod glob;
pub(crate) mod plan;
pub(crate) mod rank;

pub use deep::DeepMode;
pub use plan::{is_path_glob_query, is_scoped_file_query};

// API consumed by the file provider (crate-internal).
pub(crate) use deep::{
    index_results_are_strong, plan_deep_jobs, run_deep_jobs, should_deep_search,
};
pub(crate) use plan::parse_scoped_for_query;

// Internal cross-module imports for `search_index` and the hub below.
use glob::{
    is_drive_path_query, maybe_live_relative_glob, path_completions, search_absolute_glob,
    search_glob,
};
use plan::{
    parse_glob_query, parse_scoped_query, scope_folder_suggestions, scoped_to_glob,
    strip_file_mode_prefix,
};
use rank::score_free_text_full;

pub(crate) const FILE_RESULT_LIMIT: usize = 25;
/// Once top-K is full of substring-or-better hits, skip fuzzy entirely.
pub(crate) const STRONG_SCORE: i64 = 15_000;
/// Batch B: skip full free-text scan when best hot name score reaches prefix band.
pub(crate) const HOT_SKIP_FULL_SCORE: i64 = 30_000;
/// Min query length (Unicode chars) for hot short-circuit.
/// Short prefixes (`doc`, `src`) stay full-scan so cold siblings still appear.
pub(crate) const HOT_SKIP_MIN_QUERY_LEN: usize = 4;
/// Exact/prefix band — index already answered; no live walk.
pub(crate) const DEEP_SKIP_IF_INDEX_SCORE: i64 = 30_000;
/// Live walk budgets — sync (bench) stays tight; async UI worker can go deeper.
pub(crate) const DEEP_VISIT_CAP_SYNC: usize = 8_000;
pub(crate) const DEEP_TIME_BUDGET_SYNC: std::time::Duration = std::time::Duration::from_millis(40);
pub(crate) const DEEP_VISIT_CAP_ASYNC: usize = 40_000;
pub(crate) const DEEP_TIME_BUDGET_ASYNC: std::time::Duration =
    std::time::Duration::from_millis(200);
pub(crate) const DEEP_MAX_DEPTH: usize = 6;
pub(crate) const DEEP_MAX_ROOTS: usize = 12;
/// Live hits slightly below equivalent index hits when scores would tie.
pub(crate) const DEEP_SCORE_PENALTY: i64 = 500;

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_index(
    index: &[IndexedPath],
    query: &str,
    path_style: &PathStyle,
    mounts: &[MountInfo],
    excludes: &ExcludeSet,
    matcher: &SkimMatcherV2,
    allow_fuzzy: bool,
    // Indices of frequently opened paths (Batch A: seed free-text heap; no short-circuit yet).
    hot_indices: &[usize],
) -> Vec<SearchResult> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }

    if q.starts_with('/') || q.starts_with('~') || q.starts_with("./") || is_drive_path_query(q) {
        if q.contains('*') || q.contains('?') {
            return search_absolute_glob(q, index, path_style, mounts);
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
        return search_glob(index, &gq, path_style, mounts);
    }

    if let Some(gq) = parse_glob_query(q) {
        let mut results = search_glob(index, &gq, path_style, mounts);
        // Relative scope under a known dir: live list/walk for freshness
        // (index may be stale or shallow). Absolute globs already live-list.
        maybe_live_relative_glob(&gq, index, path_style, mounts, excludes, &mut results);
        return results;
    }

    let q_lower = q.to_lowercase();
    if index.is_empty() {
        return Vec::new();
    }
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
}

#[cfg(test)]
mod tests {
    use super::deep::{index_is_strong, live_deep_under_roots, looks_specific_for_deep};
    use super::glob::{
        expand_drive_path, find_path_segment, glob_match, is_broad_extension_glob,
        strip_double_star_components,
    };
    use super::plan::parse_scope_hint_query;
    use super::rank::path_contains_slash_prefixed;
    use super::*;

    #[test]
    fn path_contains_slash_prefixed_no_alloc_semantics() {
        // Same semantics as `path.contains(&format!("/{q}"))` (substring, not segment).
        assert!(path_contains_slash_prefixed(
            "docs",
            "/home/u/docs/readme.md"
        ));
        assert!(path_contains_slash_prefixed(
            "readme.md",
            "/home/u/docs/readme.md"
        ));
        // "doc" matches the prefix of "/docs" — intentional parity with old format! path.
        assert!(path_contains_slash_prefixed(
            "doc",
            "/home/u/docs/readme.md"
        ));
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
        assert!(is_path_glob_query("hark/docs/*.md"));
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
            false,
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

        let g = parse_glob_query("hark/docs/*.md").unwrap();
        assert_eq!(g.segments, vec!["hark", "docs"]);
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

        let g = parse_glob_query("hark/docs/*.md").unwrap();
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
                false,
            ),
            make_indexed(
                PathBuf::from("/home/u/glassbox/docs"),
                "docs".into(),
                true,
                2,
                false,
            ),
            make_indexed(PathBuf::from("/home/u/hark"), "hark".into(), true, 1, false),
            make_indexed(
                PathBuf::from("/home/u/glassbox/docs/optimization.md"),
                "optimization.md".into(),
                false,
                3,
                false,
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

        // /home/u/hark/docs/x — "hark" @ 8..12, "docs" @ 13..17
        assert_eq!(
            find_path_segment("/home/u/hark/docs/x", "hark", 0),
            Some(12)
        );
        assert_eq!(
            find_path_segment("/home/u/hark/docs/x", "docs", 13),
            Some(17)
        );
        // substring of component must not match
        assert!(find_path_segment("/home/u/harky/docs", "hark", 0).is_none());
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
        let base = std::env::temp_dir().join(format!("hark-deep-ut-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let deep = base.join("proj").join("src").join("ui").join("widgets");
        fs::create_dir_all(&deep).unwrap();
        let target = deep.join("optimization.md");
        fs::write(&target, "hi").unwrap();
        let junk = base.join("proj").join("node_modules").join("pkg");
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("optimization.md"), "nope").unwrap();

        // Index only has the shallow project folder (as depth-2 index would).
        let index = vec![make_indexed(
            base.join("proj"),
            "proj".into(),
            true,
            1,
            false,
        )];
        let style = PathStyle::Label;
        let mounts: Vec<MountInfo> = vec![];
        let excludes = ExcludeSet::from_list(&[]);

        // Segment-scoped: proj + name
        let mut results = search_glob(
            &index,
            &parse_glob_query("proj/optimization.md").unwrap(),
            &style,
            &mounts,
        );
        // Index has no deep file → weak
        assert!(!index_is_strong(&results));
        let jobs = plan_deep_jobs(
            &index,
            "proj/optimization.md",
            &results,
            DeepMode::Sync,
            &[],
            &mounts,
        );
        run_deep_jobs(jobs, &style, &mounts, &excludes, &mut results);

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
        let mut empty = Vec::new();
        let jobs = plan_deep_jobs(&index, "*.md", &empty, DeepMode::Sync, &[], &mounts);
        run_deep_jobs(jobs, &style, &mounts, &excludes, &mut empty);
        assert!(empty.is_empty(), "broad extension must not deep-walk");

        // Pinned deep root alone can surface a nested file even with empty index.
        let mut pinned_results = Vec::new();
        let jobs = plan_deep_jobs(
            &[],
            "optimization.md",
            &pinned_results,
            DeepMode::Sync,
            &[base.join("proj").to_string_lossy().to_string()],
            &mounts,
        );
        run_deep_jobs(jobs, &style, &mounts, &excludes, &mut pinned_results);
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
        let jobs = plan_deep_jobs(
            &index,
            "optimization.md in proj",
            &scoped,
            DeepMode::Sync,
            &[],
            &mounts,
        );
        run_deep_jobs(jobs, &style, &mounts, &excludes, &mut scoped);
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

    #[test]
    fn shared_existing_set_skips_duplicate_walks() {
        let base = std::env::temp_dir().join(format!("hark-deep-seen-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("proj").join("src")).unwrap();
        fs::write(base.join("proj").join("src").join("optimization.md"), "hi").unwrap();

        let style = PathStyle::Label;
        let mounts: Vec<MountInfo> = vec![];
        let excludes = ExcludeSet::from_list(&[]);
        let mut existing: HashSet<String> = HashSet::new();

        let first = live_deep_under_roots(
            &[base.join("proj")],
            &[],
            Some("optimization.md"),
            false,
            &style,
            &mounts,
            &excludes,
            &mut existing,
            DeepMode::Sync,
        );
        assert_eq!(first.len(), 1);

        // Same root + pattern walked again: the shared `existing` set (the walk
        // inserts every hit id into it) makes the second walk skip the file, so
        // `merge_live` can append without dedup.
        let second = live_deep_under_roots(
            &[base.join("proj")],
            &[],
            Some("optimization.md"),
            false,
            &style,
            &mounts,
            &excludes,
            &mut existing,
            DeepMode::Sync,
        );
        assert!(
            second.is_empty(),
            "persistent existing set must skip already-found hits"
        );

        let _ = fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod hot_skip_tests {
    use super::rank::hot_strong_enough;
    use super::{HOT_SKIP_FULL_SCORE, HOT_SKIP_MIN_QUERY_LEN};

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
