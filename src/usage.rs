use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Temp-file naming for saves is handled inside `config::write_private_file`.

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

/// Upper bound for a per-id count. A tampered `usage.json` can carry
/// `u64::MAX`; clamping on load keeps frecency's float→i64 cast from
/// saturating and later score additions from overflowing (panic=abort).
const MAX_COUNT: u64 = 1_000_000;

impl UsageStore {
    pub fn load() -> Self {
        let p = usage_path();
        let dir = p
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        Self::load_with_dir_impl(&dir)
    }

    /// Shared load logic (also used by the test-only `load_with_dir`).
    /// Clamps poisoned `count` values and drops empty ids.
    fn load_with_dir_impl(dir: &std::path::Path) -> Self {
        let path = dir.join("usage.json");
        // read_private_file refuses files in shared /tmp fallback space not
        // owned by this user — a planted usage table would skew ranking.
        let mut data = load_usage_file(&path);
        data = UsageFile {
            version: data.version,
            entries: data
                .entries
                .into_iter()
                .filter(|(id, _)| !id.is_empty())
                .map(|(id, mut e)| {
                    e.count = e.count.min(MAX_COUNT);
                    (id, e)
                })
                .collect(),
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
            last_save: Mutex::new(
                Instant::now()
                    .checked_sub(SAVE_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            ),
        }
    }

    /// Test-only: empty store backed by a unique temp-dir path so any `record`
    /// debounce-write lands in /tmp, never the working directory.
    #[cfg(test)]
    pub(crate) fn new_empty() -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("hark-usage-empty-{}-{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&dir);
        // write_private_file refuses non-0700 dirs; tests write real files.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Self {
            inner: RwLock::new(UsageFile::default()),
            path: dir.join("usage.json"),
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(
                Instant::now()
                    .checked_sub(SAVE_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            ),
        }
    }

    /// Test-only: real `load()` logic but from an explicit directory.
    #[cfg(test)]
    pub(crate) fn load_with_dir(dir: &std::path::Path) -> Self {
        Self::load_with_dir_impl(dir)
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
                prune_entries_pinning(&mut g.entries, MAX_ENTRIES, now, id);
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

    /// Batched `boost` under a single read guard (audit P3 Pass 14): the
    /// engine scores ~45 results per keystroke, and one guard + one clock
    /// read beats ~45 of each. Output aligns with `ids`.
    pub fn boost_many(&self, ids: &[&str]) -> Vec<i64> {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let now = now_secs();
        ids.iter()
            .map(|id| {
                g.entries
                    .get(*id)
                    .map(|e| frecency(e.count, e.last, now))
                    .unwrap_or(0)
            })
            .collect()
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
        items.sort_by_key(|b| std::cmp::Reverse(b.1));
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
        items.sort_by_key(|b| std::cmp::Reverse(b.1));
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
        // usage-race: claim the flag BEFORE snapshotting. A record landing
        // mid-write re-sets it, so the next flush retries — clearing after the
        // rename could erase a concurrent record's dirty flag (lost write).
        self.dirty.store(false, Ordering::Release);
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
        // Shared atomic write: unique 0600 tmp, fsync, rename.
        if crate::config::write_private_file(&self.path, &data) {
            *self.last_save.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
        } else {
            // Write failed — stay dirty so the next flush retries.
            self.dirty.store(true, Ordering::Release);
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
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let retain: std::collections::HashSet<String> =
        ranked.into_iter().take(keep).map(|(id, _)| id).collect();
    entries.retain(|id, _| retain.contains(id));
}

/// Prune variant for `record`: the just-recorded id is pinned so a first-use
/// entry can never be evicted by its own record call (audit P3). Keeps
/// `keep - 1` best of the rest plus the pinned id.
fn prune_entries_pinning(
    entries: &mut HashMap<String, UsageEntry>,
    keep: usize,
    now: u64,
    pin: &str,
) {
    if entries.len() <= keep {
        return;
    }
    let mut ranked: Vec<(String, i64)> = entries
        .iter()
        .filter(|(id, _)| id.as_str() != pin)
        .map(|(id, e)| (id.clone(), frecency(e.count, e.last, now)))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let retain: std::collections::HashSet<String> = ranked
        .into_iter()
        .take(keep.saturating_sub(1))
        .map(|(id, _)| id)
        .collect();
    entries.retain(|id, _| id == pin || retain.contains(id));
}

/// Load with backup + per-entry salvage (audit P2): mirrors the typos
/// store — a corrupt file is copied aside + logged, and each intact entry
/// survives instead of the whole history being silently wiped.
fn load_usage_file(path: &std::path::Path) -> UsageFile {
    let Some(s) = crate::config::read_private_file(path) else {
        return UsageFile::default();
    };
    match serde_json::from_str::<UsageFile>(&s) {
        Ok(f) => f,
        Err(err) => {
            match crate::config::backup_invalid_config(path) {
                Some(b) => eprintln!(
                    "hark: invalid usage store {} ({err}); salvaging intact entries (backup: {})",
                    path.display(),
                    b.display()
                ),
                None => eprintln!(
                    "hark: invalid usage store {} ({err}); salvaging intact entries (backup failed)",
                    path.display()
                ),
            }
            salvage_usage_file(&s)
        }
    }
}

/// Best-effort rescue: keep every entry that still parses, skip the rest.
fn salvage_usage_file(s: &str) -> UsageFile {
    let mut out = UsageFile::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return out;
    };
    out.version = v
        .get("version")
        .and_then(|x| serde_json::from_value(x.clone()).ok())
        .unwrap_or_else(default_version);
    if let Some(map) = v.get("entries").and_then(|a| a.as_object()) {
        for (k, ev) in map {
            if k.is_empty() {
                continue;
            }
            if let Ok(e) = serde_json::from_value::<UsageEntry>(ev.clone()) {
                out.entries.insert(k.clone(), e);
            }
        }
    }
    out
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
        .join("hark/usage.json")
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    fn temp_store() -> (UsageStore, PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hark-usage-{}-{}-{}",
            std::process::id(),
            n,
            now_secs()
        ));
        let _ = fs::create_dir_all(&dir);
        // write_private_file refuses non-0700 dirs; tests write real files.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
        let path = dir.join("usage.json");
        let store = UsageStore {
            inner: RwLock::new(UsageFile::default()),
            path: path.clone(),
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(
                Instant::now()
                    .checked_sub(SAVE_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            ),
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

    #[test]
    fn record_pins_new_entry() {
        // Audit P3 (Pass 13): a first-use entry must survive its own
        // record call's prune instead of being evicted immediately.
        let (store, dir) = temp_store();
        for i in 0..MAX_ENTRIES {
            store.record(&format!("app:{i}"));
        }
        store.record("app:newcomer");
        let g = store.inner.read().unwrap();
        assert!(g.entries.contains_key("app:newcomer"));
        assert!(g.entries.len() <= MAX_ENTRIES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_clamps_poisoned_counts() {
        // A corrupt-but-parseable usage.json with count: u64::MAX must not
        // survive load as-is — frecency's float→i64 cast would saturate and
        // later unsaturating adds aborted the daemon (panic=abort).
        let (store, dir) = temp_store();
        let poisoned = format!(
            r#"{{"version":1,"entries":{{"app:evil":{{"count":{},"last":0}}}}}}"#,
            u64::MAX
        );
        fs::write(&store.path, poisoned).unwrap();

        let loaded = UsageStore::load_with_dir(&dir);
        let boost = loaded.boost("app:evil");
        assert!(boost >= 0, "boost must never be negative, got {boost}");
        let g = loaded.inner.read().unwrap();
        let e = g.entries.get("app:evil").expect("entry kept");
        assert_eq!(e.count, MAX_COUNT, "poisoned count clamped on load");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn boost_many_matches_single_boosts() {
        // Audit P3 (Pass 14): batched scoring must equal per-id `boost`.
        let (store, dir) = temp_store();
        store.record("app:a");
        store.record("app:a");
        store.record("app:b");
        let ids = ["app:a", "app:b", "app:ghost"];
        let batched = store.boost_many(&ids);
        let singles: Vec<i64> = ids.iter().map(|id| store.boost(id)).collect();
        assert_eq!(batched, singles);
        assert!(batched[0] > 0 && batched[1] > 0);
        assert_eq!(batched[2], 0);
        assert!(store.boost_many(&[]).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_store_backs_up_and_salvages_intact_entries() {
        // Audit P2: one wrong-typed entry must not wipe the whole history.
        let (store, dir) = temp_store();
        let path = dir.join("usage.json");
        fs::write(
            &path,
            r#"{"version":1,"entries":{"app:good":{"count":3,"last":0},"app:bad":{"count":"lots"}}}"#,
        )
        .unwrap();
        let loaded = UsageStore::load_with_dir(&dir);
        let g = loaded.inner.read().unwrap();
        assert!(g.entries.contains_key("app:good"));
        assert!(!g.entries.contains_key("app:bad"));
        drop(g);
        assert!(
            path.with_extension("json.invalid").exists(),
            "corrupt store must be copied aside"
        );
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }
}
