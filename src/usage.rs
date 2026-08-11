use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Soft cap: after this, drop coldest entries by frecency.
const MAX_ENTRIES: usize = 500;
/// Don't rewrite usage.json more often than this (memory still updates immediately).
const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct UsageEntry {
    count: u64,
    last: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct UsageFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, UsageEntry>,
}

fn default_version() -> u32 {
    1
}

pub struct UsageStore {
    inner: RwLock<UsageFile>,
    path: PathBuf,
    dirty: AtomicBool,
    last_save: Mutex<Instant>,
}

impl UsageStore {
    pub fn load() -> Self {
        let path = usage_path();
        let mut data = if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            UsageFile::default()
        };
        // Cap on load so old bloated files shrink once.
        let now = now_secs();
        if data.entries.len() > MAX_ENTRIES {
            prune_entries(&mut data.entries, MAX_ENTRIES, now);
        }
        Self {
            inner: RwLock::new(data),
            path,
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now() - SAVE_DEBOUNCE),
        }
    }

    /// Test-only: empty store backed by a unique temp-dir path so any `record`
    /// debounce-write lands in /tmp, never the working directory.
    #[cfg(test)]
    pub(crate) fn new_empty() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("blink-usage-empty-{}-{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&dir);
        Self {
            inner: RwLock::new(UsageFile::default()),
            path: dir.join("usage.json"),
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now() - SAVE_DEBOUNCE),
        }
    }

    pub fn record(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        let now = now_secs();
        {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            let e = g.entries.entry(id.to_string()).or_default();
            e.count = e.count.saturating_add(1);
            e.last = now;
            if g.entries.len() > MAX_ENTRIES {
                prune_entries(&mut g.entries, MAX_ENTRIES, now);
            }
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.maybe_save(false);
    }

    pub fn boost(&self, id: &str) -> i64 {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let Some(e) = g.entries.get(id) else {
            return 0;
        };
        frecency(e.count, e.last, now_secs())
    }

    /// Top ids by frecency, highest first.
    pub fn top(&self, n: usize) -> Vec<(String, i64)> {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let now = now_secs();
        let mut items: Vec<(String, i64)> = g
            .entries
            .iter()
            .map(|(id, e)| (id.clone(), frecency(e.count, e.last, now)))
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1));
        items.truncate(n);
        items
    }

    /// Absolute filesystem paths from top frecency `path:…` usage ids (no prefix).
    /// Used to build the file-search hot set (see `docs/hot-path-file-search.md`).
    pub fn top_path_ids(&self, n: usize) -> Vec<String> {
        if n == 0 {
            return Vec::new();
        }
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let now = now_secs();
        let mut items: Vec<(String, i64)> = g
            .entries
            .iter()
            .filter_map(|(id, e)| {
                let path = id.strip_prefix("path:")?;
                if path.is_empty() {
                    return None;
                }
                Some((path.to_string(), frecency(e.count, e.last, now)))
            })
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1));
        items.truncate(n);
        items.into_iter().map(|(p, _)| p).collect()
    }

    /// Flush pending writes (process exit / tests).
    pub fn flush(&self) {
        self.maybe_save(true);
    }

    fn maybe_save(&self, force: bool) {
        if !self.dirty.load(Ordering::Relaxed) {
            return;
        }
        {
            let last = self.last_save.lock().unwrap_or_else(|p| p.into_inner());
            if !force && last.elapsed() < SAVE_DEBOUNCE {
                return;
            }
        }
        self.save();
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Compact JSON — humans rarely edit usage; pretty was pure overhead.
        let data = {
            let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
            match serde_json::to_vec(&*g) {
                Ok(v) => v,
                Err(_) => return,
            }
        };
        let tmp = self.path.with_extension("json.tmp");
        if fs::write(&tmp, data).is_ok() && fs::rename(&tmp, &self.path).is_ok() {
            self.dirty.store(false, Ordering::Relaxed);
            *self.last_save.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
        }
    }
}

impl Drop for UsageStore {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Keep the `keep` highest-frecency entries; drop the rest.
fn prune_entries(entries: &mut HashMap<String, UsageEntry>, keep: usize, now: u64) {
    if entries.len() <= keep {
        return;
    }
    let mut ranked: Vec<(String, i64)> = entries
        .iter()
        .map(|(id, e)| (id.clone(), frecency(e.count, e.last, now)))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    let retain: std::collections::HashSet<String> =
        ranked.into_iter().take(keep).map(|(id, _)| id).collect();
    entries.retain(|id, _| retain.contains(id));
}

fn frecency(count: u64, last: u64, now: u64) -> i64 {
    let age = now.saturating_sub(last);
    let recency = if age < 86_400 {
        5_000
    } else if age < 86_400 * 7 {
        2_000
    } else if age < 86_400 * 30 {
        500
    } else {
        0
    };
    // Soft decay over ~2 weeks
    let days = age as f64 / 86_400.0;
    let decay = (-days / 14.0).exp();
    ((count as f64) * 1000.0 * decay) as i64 + recency
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn usage_path() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("blink/usage.json")
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn temp_store() -> (UsageStore, PathBuf) {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "blink-usage-{}-{}-{}",
            std::process::id(),
            n,
            now_secs()
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("usage.json");
        let store = UsageStore {
            inner: RwLock::new(UsageFile::default()),
            path: path.clone(),
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now() - SAVE_DEBOUNCE),
        };
        (store, dir)
    }

    #[test]
    fn debounce_skips_rapid_rewrites() {
        let (store, dir) = temp_store();
        store.record("app:a");
        store.flush(); // force first write
        let m0 = fs::metadata(&store.path).unwrap().modified().unwrap();

        // Within debounce window — dirty but no rewrite if we only record.
        // Reset last_save to now so debounce applies.
        *store.last_save.lock().unwrap() = Instant::now();
        store.dirty.store(false, Ordering::Relaxed);
        store.record("app:b");
        // Immediate second record should not pass debounce.
        store.record("app:c");
        let m1 = fs::metadata(&store.path).unwrap().modified().unwrap();
        assert_eq!(m0, m1, "debounced records must not rewrite immediately");

        // Force flush writes compact file with all three.
        store.flush();
        let m2 = fs::metadata(&store.path).unwrap().modified().unwrap();
        assert!(m2 >= m0);
        let raw = fs::read_to_string(&store.path).unwrap();
        assert!(raw.contains("app:a") && raw.contains("app:c"));
        // Compact encoding: single-line-ish (no pretty 2-space indent after newline).
        assert!(
            !raw.contains("\n  "),
            "expected compact JSON without pretty indentation"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_caps_entries() {
        let (store, dir) = temp_store();
        for i in 0..(MAX_ENTRIES + 50) {
            store.record(&format!("app:{i}"));
        }
        store.flush();
        let n = store.inner.read().unwrap().entries.len();
        assert!(n <= MAX_ENTRIES, "entries={n} > {MAX_ENTRIES}");
        let _ = fs::remove_dir_all(&dir);
    }
}
