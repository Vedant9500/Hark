//! Hot path set for free-text file search.
//!
//! Built from usage `path:` ids ∩ current index (cap [`HOT_CAP`]).
//! Free-text scoring may short-circuit on a strong hot hit when the query is
//! long enough; short queries use the full index only (baseline cost).
//!
//! Design: `docs/hot-path-file-search.md`.

use super::index::IndexedPath;
use crate::usage::UsageStore;
use std::collections::{HashMap, HashSet};
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

/// Thread-safe hot set + dirty flag (rebuild after opens / reindex).
pub struct HotPaths {
    usage: Arc<UsageStore>,
    set: RwLock<HotSet>,
    dirty: AtomicBool,
}

impl HotPaths {
    pub fn new(usage: Arc<UsageStore>) -> Self {
        Self {
            usage,
            set: RwLock::new(HotSet::default()),
            dirty: AtomicBool::new(true),
        }
    }

    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Rebuild from usage ∩ `index` when dirty.
    pub fn ensure_fresh(&self, index: &[IndexedPath]) {
        if !self.dirty.load(Ordering::Relaxed) {
            return;
        }
        self.rebuild(index);
    }

    /// Force rebuild (after index swap).
    pub fn rebuild(&self, index: &[IndexedPath]) {
        // Oversample: some usage paths may not be in the (shallow) index.
        let wanted = self
            .usage
            .top_path_ids(HOT_CAP.saturating_mul(2).max(HOT_CAP));
        let set = build_hot_set(index, &wanted, HOT_CAP);
        *self.set.write().unwrap() = set;
        self.dirty.store(false, Ordering::Relaxed);
    }

    pub fn snapshot_indices(&self) -> Vec<usize> {
        self.set.read().unwrap().indices.clone()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.set.read().unwrap().indices.len()
    }
}

/// Map absolute paths → first index position (`path_lower` key).
///
/// Keys borrow from `index` — no per-entry `path_lower` clone (hot cap is tiny;
/// the expensive part was cloning up to `MAX_INDEX` strings into the map).
pub(crate) fn build_hot_set(index: &[IndexedPath], wanted_paths: &[String], cap: usize) -> HotSet {
    if index.is_empty() || wanted_paths.is_empty() || cap == 0 {
        return HotSet::default();
    }

    let mut by_path: HashMap<&str, usize> = HashMap::with_capacity(index.len());
    for (idx, item) in index.iter().enumerate() {
        by_path.entry(item.path_lower.as_str()).or_insert(idx);
    }

    let mut indices = Vec::with_capacity(cap.min(wanted_paths.len()));
    let mut seen_idx = HashSet::with_capacity(cap.min(wanted_paths.len()));

    for p in wanted_paths {
        if indices.len() >= cap {
            break;
        }
        // Wanted list is small (≤ ~2× HOT_CAP); lowercasing here is fine.
        let key = PathBuf::from(p).to_string_lossy().to_lowercase();
        let Some(&idx) = by_path.get(key.as_str()) else {
            continue;
        };
        if seen_idx.insert(idx) {
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
