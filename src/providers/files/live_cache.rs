//! Ephemeral in-memory cache for live deep-search hits.
//!
//! Never written to disk / the persistent file index. Used to skip re-walks
//! when the user retypes the same query within a short TTL.

use crate::providers::SearchResult;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 64;
const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);
/// Empty deep results (walked, no hits) — short TTL so typos don't re-walk every keystroke.
const NEGATIVE_TTL: Duration = Duration::from_secs(90);

struct Entry {
    hits: Arc<[SearchResult]>,
    expires: Instant,
    /// Monotonic recency stamp; larger = more recently used.
    last_used: u64,
}

struct Inner {
    map: HashMap<String, Entry>,
    /// key → recency stamp, ascending so the LRU victim is always the first
    /// entry (O(log n) insert/remove, O(1) eviction — no full scans).
    recency: BTreeMap<u64, String>,
    seq: u64,
}

impl Inner {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Move `key`'s entry to the most-recent position.
    fn touch(&mut self, key: &str) {
        let new_stamp = {
            self.seq += 1;
            self.seq
        };
        let old_stamp = match self.map.get_mut(key) {
            Some(e) => {
                let old = e.last_used;
                e.last_used = new_stamp;
                old
            }
            None => return,
        };
        self.recency.remove(&old_stamp);
        self.recency.insert(new_stamp, key.to_string());
    }

    fn remove(&mut self, key: &str) {
        if let Some(entry) = self.map.remove(key) {
            self.recency.remove(&entry.last_used);
        }
    }

    fn insert(&mut self, key: String, entry: Entry) {
        let stamp = entry.last_used;
        if let Some(old) = self.map.insert(key.clone(), entry) {
            self.recency.remove(&old.last_used);
        }
        self.recency.insert(stamp, key);
    }

    /// Drop the least-recently-used entry until under the cap.
    fn evict_to_cap(&mut self) {
        while self.map.len() > MAX_ENTRIES {
            let (stamp, victim) = self
                .recency
                .iter()
                .next()
                .map(|(s, k)| (*s, k.clone()))
                .expect("recency non-empty while map over cap");
            self.recency.remove(&stamp);
            self.map.remove(&victim);
        }
    }
}

/// Query → live deep hits (TTL + LRU cap).
pub struct LiveCache {
    inner: Mutex<Inner>,
}

impl LiveCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                recency: BTreeMap::new(),
                seq: 0,
            }),
        }
    }

    /// Normalize query for cache key (trim + lowercase; strip `f `/`file `/`folder `
    /// case-insensitively, matching the engine's force-files gate).
    pub fn key_for(query: &str) -> String {
        let raw = query.trim();
        crate::providers::files::strip_force_files_prefix(raw)
            .unwrap_or(raw)
            .trim()
            .to_lowercase()
    }

    /// True when a non-expired entry exists (no hit vector clone).
    pub fn contains(&self, query: &str) -> bool {
        let key = Self::key_for(query);
        if key.is_empty() {
            return false;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let expired = match inner.map.get(&key) {
            Some(e) => e.expires <= now,
            None => return false,
        };
        if expired {
            inner.remove(&key);
            return false;
        }
        inner.touch(&key);
        true
    }

    /// Shared hits for the query (Arc clone only).
    pub fn get(&self, query: &str) -> Option<Arc<[SearchResult]>> {
        let key = Self::key_for(query);
        if key.is_empty() {
            return None;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let expired = inner.map.get(&key).map(|e| e.expires <= now)?;
        if expired {
            inner.remove(&key);
            return None;
        }
        inner.touch(&key);
        inner.map.get(&key).map(|e| e.hits.clone())
    }

    /// Cache `hits` and return them for the caller.
    ///
    /// Moves into an `Arc` once, stores that Arc, then clones elements out for the
    /// return `Vec` — avoids the old `results.clone()` + `put(clone)` pattern that
    /// briefly held two full owned vectors before the Arc conversion.
    pub fn put(&self, query: &str, hits: Vec<SearchResult>) -> Vec<SearchResult> {
        let key = Self::key_for(query);
        if key.is_empty() {
            return hits;
        }
        let now = Instant::now();
        let ttl = if hits.is_empty() {
            NEGATIVE_TTL
        } else {
            DEFAULT_TTL
        };
        // Single move into shared storage; return path clones from Arc.
        let hits: Arc<[SearchResult]> = Arc::from(hits);
        let out = hits.to_vec();
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let stamp = inner.next_seq();
        inner.insert(
            key,
            Entry {
                hits,
                expires: now + ttl,
                last_used: stamp,
            },
        );
        inner.evict_to_cap();
        out
    }

    /// Drop all cached deep-search hits (e.g. after trash / external delete).
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.map.clear();
        inner.recency.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map
            .len()
    }
}

impl Default for LiveCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{Action, ResultKind};

    fn hit(id: &str) -> SearchResult {
        SearchResult {
            id: id.into(),
            title: id.into(),
            subtitle: String::new(),
            kind: ResultKind::File,
            score: 1,
            icon: None,
            action: Action::OpenPath(std::path::PathBuf::from("/tmp")),
            conversion: None,
            matched: None,
        }
    }

    #[test]
    fn put_get_and_key_normalize() {
        let c = LiveCache::new();
        let _ = c.put("Foo.Bar", vec![hit("a")]);
        assert_eq!(c.get("foo.bar").unwrap().len(), 1);
        assert_eq!(c.get("f foo.bar").unwrap().len(), 1);
        assert_eq!(c.get("file foo.bar").unwrap().len(), 1);
        assert!(c.get("other").is_none());
        // Same Arc across gets for the same key
        let a = c.get("foo.bar").unwrap();
        let b = c.get("foo.bar").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn prefix_normalization_is_case_insensitive() {
        // Engine treats `f`/`file`/`folder` prefixes ASCII case-insensitively;
        // cache keys must too, else `File foo` vs `file foo` duplicate deep walks.
        let c = LiveCache::new();
        let _ = c.put("File foo.bar", vec![hit("a")]);
        assert_eq!(c.get("file foo.bar").unwrap().len(), 1);
        assert_eq!(c.get("FOLDER foo.bar").unwrap().len(), 1);
        assert_eq!(c.get("Folder\tfoo.bar").unwrap().len(), 1);
        assert!(c.get("firefox").is_none());
    }

    #[test]
    fn empty_hits_negative_cached() {
        let c = LiveCache::new();
        let _ = c.put("x", Vec::new());
        assert_eq!(c.len(), 1);
        assert!(c.contains("x"));
        let hits = c.get("x").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn contains_true_when_present() {
        let c = LiveCache::new();
        assert!(!c.contains("x"));
        let _ = c.put("x", vec![hit("a")]);
        assert!(c.contains("x"));
        assert!(c.contains("f x"));
    }

    #[test]
    fn evicts_least_recently_used() {
        let c = LiveCache::new();
        for i in 0..MAX_ENTRIES {
            let _ = c.put(&format!("k{i}"), vec![hit(&format!("k{i}"))]);
        }
        assert_eq!(c.len(), MAX_ENTRIES);
        // Promote k0 to most-recent, making k1 the LRU.
        assert!(c.contains("k0"));
        let _ = c.put("overflow", vec![hit("overflow")]);
        assert_eq!(c.len(), MAX_ENTRIES);
        assert!(c.contains("overflow"));
        assert!(c.contains("k0"));
        assert!(!c.contains("k1"), "LRU k1 should be evicted");
    }

    #[test]
    fn reinserting_key_does_not_corrupt_recency() {
        let c = LiveCache::new();
        let _ = c.put("k", vec![hit("k")]);
        // Re-put same key: no stale recency stamp may survive.
        let _ = c.put("k", vec![hit("k2")]);
        for i in 0..62 {
            let _ = c.put(&format!("m{i}"), vec![hit(&format!("m{i}"))]);
        }
        assert_eq!(c.len(), 63);
        // Promote k to most-recent; a stale recency stamp would make k the
        // (wrong) eviction victim despite the touch.
        assert!(c.contains("k"));
        let _ = c.put("m62", vec![hit("m62")]);
        let _ = c.put("m63", vec![hit("m63")]);
        assert_eq!(c.len(), MAX_ENTRIES);
        assert!(c.contains("k"), "recently-used k must survive eviction");
        assert!(!c.contains("m0"), "oldest m0 should be evicted");
    }
}
