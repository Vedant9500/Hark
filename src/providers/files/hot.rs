//! Hot path set for free-text file search.
//!
//! Built from usage `path:` ids ∩ current index (cap [`HOT_CAP`]).
//! Free-text scoring may short-circuit on a strong hot hit when the query is
//! long enough; short queries use the full index only (baseline cost).
//!
//! Design: `docs/hot-path-file-search.md`.

use super::index::IndexedPath;
use crate::usage::UsageStore;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// Frequently opened paths kept for free-text hot scoring.
pub const HOT_CAP: usize = 64;

/// Indices into the current in-memory file index (valid until next index rebuild).
#[derive(Debug, Clone, Default)]
pub struct HotSet {
    /// Index positions, frecency order (best first).
    indices: Vec<usize>,
}

/// #24: shared immutable hot indices — `Arc<[usize]>` clones are refcount
/// bumps, not Vec copies under the read lock nested inside the index lock.
pub struct HotPaths {
    usage: Arc<UsageStore>,
    set: RwLock<Arc<[usize]>>,
    dirty: AtomicBool,
}

impl HotPaths {
    pub fn new(usage: Arc<UsageStore>) -> Self {
        Self {
            usage,
            set: RwLock::new(Arc::from(Vec::new())),
            dirty: AtomicBool::new(true),
        }
    }

    pub fn mark_dirty(&self) {
        // Release: usage-store writes sequenced before this must be visible
        // to the thread that claims dirty and rebuilds.
        self.dirty.store(true, Ordering::Release);
    }

    /// Rebuild from usage ∩ `index` when dirty.
    pub fn ensure_fresh(&self, index: &[IndexedPath]) {
        // Swap-claim: a `mark_dirty` racing the rebuild stays true for the
        // next call instead of being overwritten by a trailing store(false)
        // (lost-dirty → stale hot set until the next usage change).
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        self.build_and_swap(index);
    }

    /// Force rebuild (after index swap).
    pub fn rebuild(&self, index: &[IndexedPath]) {
        // Clear-before-build: a concurrent usage change during the build
        // survives for the next `ensure_fresh` instead of being cleared after.
        self.dirty.store(false, Ordering::Relaxed);
        self.build_and_swap(index);
    }

    fn build_and_swap(&self, index: &[IndexedPath]) {
        // Oversample: some usage paths may not be in the (shallow) index.
        let wanted = self
            .usage
            .top_path_ids(HOT_CAP.saturating_mul(2).max(HOT_CAP));
        let set = build_hot_set(index, &wanted, HOT_CAP);
        *self.set.write().unwrap_or_else(|p| p.into_inner()) = Arc::from(set.indices);
    }

    /// #24: cheap refcount clone — no Vec copy under nested locks.
    pub fn snapshot_indices(&self) -> Arc<[usize]> {
        self.set.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.set.read().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// Map wanted paths → first index position, in frecency (`wanted_paths`) order.
///
/// Wanted-keyed (≤2×`HOT_CAP` entries): the old index-keyed map hashed up to
/// `MAX_INDEX` (100k) strings per rebuild. Single index scan, first occurrence
/// wins per wanted key.
pub(crate) fn build_hot_set(index: &[IndexedPath], wanted_paths: &[String], cap: usize) -> HotSet {
    if index.is_empty() || wanted_paths.is_empty() || cap == 0 {
        return HotSet::default();
    }

    // Normalize wanted keys, deduping while preserving frecency order.
    let mut keys: Vec<String> = Vec::with_capacity(wanted_paths.len());
    for p in wanted_paths {
        // Wanted list is small (≤ ~2× HOT_CAP); lowercasing here is fine.
        let key = PathBuf::from(p).to_string_lossy().to_lowercase();
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let mut order: HashMap<&str, usize> = HashMap::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        order.insert(k.as_str(), i);
    }

    // Single scan: first index position per wanted key.
    let mut first_idx: Vec<Option<usize>> = vec![None; keys.len()];
    let mut found = 0usize;
    for (idx, item) in index.iter().enumerate() {
        if let Some(&ord) = order.get(item.path_lower.as_str()) {
            if first_idx[ord].is_none() {
                first_idx[ord] = Some(idx);
                found += 1;
                if found == keys.len() {
                    break;
                }
            }
        }
    }

    let mut indices = Vec::with_capacity(cap.min(keys.len()));
    for opt in first_idx {
        if indices.len() >= cap {
            break;
        }
        if let Some(idx) = opt {
            indices.push(idx);
        }
    }

    HotSet { indices }
}

#[cfg(test)]
mod hot_tests {
    use super::*;
    use std::path::PathBuf;

    fn item(path: &str, name: &str) -> IndexedPath {
        let path_buf = PathBuf::from(path);
        let path_lower = path_buf.to_string_lossy().to_lowercase();
        IndexedPath {
            path: path_buf,
            name: name.into(),
            name_lower: name.to_lowercase(),
            path_lower,
            is_dir: false,
            depth: 2,
            low_value: false,
            high_value: true,
            is_mnt: false,
        }
    }

    #[test]
    fn build_preserves_frecency_order_and_cap() {
        let index = vec![
            item("/home/u/a.txt", "a.txt"),
            item("/home/u/b.txt", "b.txt"),
            item("/home/u/c.txt", "c.txt"),
            item("/home/u/d.txt", "d.txt"),
        ];
        let wanted = vec![
            "/home/u/c.txt".into(),
            "/home/u/a.txt".into(),
            "/home/u/missing.txt".into(),
            "/home/u/b.txt".into(),
        ];
        let set = build_hot_set(&index, &wanted, 2);
        assert_eq!(set.indices, vec![2, 0]);
    }

    #[test]
    fn empty_inputs() {
        assert!(build_hot_set(&[], &["/x".into()], 64).indices.is_empty());
        let index = vec![item("/x", "x")];
        assert!(build_hot_set(&index, &[], 64).indices.is_empty());
    }

    #[test]
    fn case_insensitive_path_match() {
        let index = vec![item("/Home/U/Readme.md", "Readme.md")];
        let wanted = vec!["/home/u/readme.md".into()];
        let set = build_hot_set(&index, &wanted, 8);
        assert_eq!(set.indices, vec![0]);
    }
}
