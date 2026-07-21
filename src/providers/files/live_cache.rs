//! Ephemeral in-memory cache for live deep-search hits.
//!
//! Never written to disk / the persistent file index. Used to skip re-walks
//! when the user retypes the same query within a short TTL.

use crate::providers::SearchResult;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 64;
const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);
/// Empty deep results (walked, no hits) — short TTL so typos don't re-walk every keystroke.
const NEGATIVE_TTL: Duration = Duration::from_secs(90);

struct Entry {
    hits: Arc<[SearchResult]>,
    expires: Instant,
    /// Insertion / last-hit order for simple LRU eviction.
    last_used: Instant,
}

/// Query → live deep hits (TTL + LRU cap).
pub struct LiveCache {
    inner: Mutex<HashMap<String, Entry>>,
}

impl LiveCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Normalize query for cache key (trim + lowercase; strip `f `/`file `/`folder `).
    pub fn key_for(query: &str) -> String {
        let raw = query.trim();
        let q = raw
            .strip_prefix("f ")
            .or_else(|| raw.strip_prefix("file "))
            .or_else(|| raw.strip_prefix("folder "))
            .unwrap_or(raw)
            .trim()
            .to_lowercase();
        q
    }

    /// True when a non-expired entry exists (no hit vector clone).
    pub fn contains(&self, query: &str) -> bool {
        let key = Self::key_for(query);
        if key.is_empty() {
            return false;
        }
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let Some(entry) = map.get_mut(&key) else {
            return false;
        };
        if entry.expires <= now {
            map.remove(&key);
            return false;
        }
        entry.last_used = now;
        true
    }

    /// Shared hits for the query (Arc clone only).
    pub fn get(&self, query: &str) -> Option<Arc<[SearchResult]>> {
        let key = Self::key_for(query);
        if key.is_empty() {
            return None;
        }
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let Some(entry) = map.get_mut(&key) else {
            return None;
        };
        if entry.expires <= now {
            map.remove(&key);
            return None;
        }
        entry.last_used = now;
        Some(entry.hits.clone())
    }

    pub fn put(&self, query: &str, hits: Vec<SearchResult>) {
        let key = Self::key_for(query);
        if key.is_empty() {
            return;
        }
        let now = Instant::now();
        let ttl = if hits.is_empty() {
            NEGATIVE_TTL
        } else {
            DEFAULT_TTL
        };
        let mut map = self.inner.lock().unwrap();
        map.insert(
            key,
            Entry {
                hits: hits.into(),
                expires: now + ttl,
                last_used: now,
            },
        );
        // Evict oldest until under cap.
        while map.len() > MAX_ENTRIES {
            let victim = map
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            if let Some(k) = victim {
                map.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Drop all cached deep-search hits (e.g. after trash / external delete).
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
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
        }
    }

    #[test]
    fn put_get_and_key_normalize() {
        let c = LiveCache::new();
        c.put("Foo.Bar", vec![hit("a")]);
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
    fn empty_hits_negative_cached() {
        let c = LiveCache::new();
        c.put("x", Vec::new());
        assert_eq!(c.len(), 1);
        assert!(c.contains("x"));
        let hits = c.get("x").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn contains_true_when_present() {
        let c = LiveCache::new();
        assert!(!c.contains("x"));
        c.put("x", vec![hit("a")]);
        assert!(c.contains("x"));
        assert!(c.contains("f x"));
    }
}
