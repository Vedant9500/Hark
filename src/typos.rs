//! Personal typo / reformulation aliases for search.
//!
//! v1: learn `query → result id` when the user launches something that doesn't
//!     cleanly match the typed query (or is only a near-miss).
//! v2: also consider earlier queries in the same open session (backspace /
//!     rewrite patterns like `wats` → `whatsapp`).
//!
//! v3: Settings can list / add / remove aliases (and clear all).
//!
//! Storage mirrors [`crate::usage::UsageStore`]: small JSON, debounced save, cap.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Soft cap — personal aliases stay tiny.
const MAX_ALIASES: usize = 300;
const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);
/// Ignore 1–2 char noise (`w`, `wa`).
const MIN_ALIAS_LEN: usize = 3;
const MAX_ALIAS_LEN: usize = 24;
/// Require this many confirms before a strong rank boost (still weak-boost at 1).
const STRONG_COUNT: u64 = 2;
/// Score added when alias count >= STRONG_COUNT (on top of base inject).
const BOOST_STRONG: i64 = 18_000;
/// Score added for a single observation (weaker until confirmed).
const BOOST_WEAK: i64 = 8_000;
/// Floor score when injecting a missing alias hit into results.
const INJECT_FLOOR: i64 = 22_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AliasEntry {
    /// Stable result id (`app:…` / `path:…`).
    id: String,
    count: u64,
    last: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TypoFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    aliases: HashMap<String, AliasEntry>,
}

fn default_version() -> u32 {
    1
}

pub struct TypoStore {
    inner: RwLock<TypoFile>,
    path: PathBuf,
    dirty: AtomicBool,
    last_save: Mutex<Instant>,
}

impl TypoStore {
    /// Test-only: empty in-memory store (no disk read/write).
    #[cfg(test)]
    pub(crate) fn new_empty() -> Self {
        Self {
            inner: RwLock::new(TypoFile::default()),
            path: std::path::PathBuf::from("<test>"),
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now() - SAVE_DEBOUNCE),
        }
    }

    pub fn load() -> Self {
        let path = typo_path();
        let mut data = if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            TypoFile::default()
        };
        let now = now_secs();
        if data.aliases.len() > MAX_ALIASES {
            prune_aliases(&mut data.aliases, MAX_ALIASES, now);
        }
        Self {
            inner: RwLock::new(data),
            path,
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now() - SAVE_DEBOUNCE),
        }
    }

    /// O(1) lookup. Returns `(result_id, score_boost)` when an alias exists.
    pub fn lookup(&self, query: &str) -> Option<(String, i64)> {
        let key = normalize_alias(query)?;
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let e = g.aliases.get(&key)?;
        let boost = if e.count >= STRONG_COUNT {
            BOOST_STRONG
        } else {
            BOOST_WEAK
        };
        Some((e.id.clone(), boost))
    }

    /// Learn aliases from a successful launch (v1 final query + v2 session).
    pub fn learn_from_launch(
        &self,
        final_query: &str,
        recent_queries: &[String],
        result_id: &str,
        result_title: &str,
    ) {
        if !result_id.starts_with("app:") && !result_id.starts_with("path:") {
            return;
        }
        if result_id.is_empty() || result_title.is_empty() {
            return;
        }
        let title = result_title.to_lowercase();
        // Skip calc-like titles etc. already gated by id prefix.

        let mut seen = std::collections::HashSet::new();
        let mut candidates: Vec<String> = Vec::new();

        // v1 — final query at launch time
        if let Some(k) = normalize_alias(final_query) {
            if seen.insert(k.clone()) {
                candidates.push(k);
            }
        }
        // v2 — earlier spellings in this open session
        for q in recent_queries {
            if let Some(k) = normalize_alias(q) {
                if seen.insert(k.clone()) {
                    candidates.push(k);
                }
            }
        }

        let mut learned_any = false;
        for key in candidates {
            if !should_learn_alias(&key, &title) {
                continue;
            }
            self.record_alias(&key, result_id);
            learned_any = true;
        }
        if learned_any {
            self.maybe_save(false);
        }
    }

    fn record_alias(&self, key: &str, result_id: &str) {
        let now = now_secs();
        let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
        match g.aliases.get_mut(key) {
            Some(e) if e.id == result_id => {
                e.count = e.count.saturating_add(1);
                e.last = now;
            }
            Some(e) => {
                // Conflicting target: only switch after the new id "wins" once
                // more often — simple: replace if counts were low, else keep.
                if e.count <= 2 {
                    e.id = result_id.to_string();
                    e.count = 1;
                    e.last = now;
                }
                // else ignore conflicting observation
            }
            None => {
                g.aliases.insert(
                    key.to_string(),
                    AliasEntry {
                        id: result_id.to_string(),
                        count: 1,
                        last: now,
                    },
                );
            }
        }
        if g.aliases.len() > MAX_ALIASES {
            prune_aliases(&mut g.aliases, MAX_ALIASES, now);
        }
        self.dirty.store(true, Ordering::Relaxed);
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

    /// Compact JSON + atomic replace (parity with [`crate::usage::UsageStore`]).
    fn save(&self) {
        // usage-race parity: claim the flag before snapshotting; records
        // landing mid-write re-set it so the next flush retries.
        self.dirty.store(false, Ordering::Release);
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
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
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// Force flush (tests / drop).
    #[allow(dead_code)]
    pub fn flush(&self) {
        self.maybe_save(true);
    }

    /// All aliases, strongest first (for Settings).
    pub fn list(&self) -> Vec<TypoAlias> {
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let now = now_secs();
        let mut items: Vec<TypoAlias> = g
            .aliases
            .iter()
            .map(|(alias, e)| TypoAlias {
                alias: alias.clone(),
                id: e.id.clone(),
                count: e.count,
                last: e.last,
                score: alias_frecency(e.count, e.last, now),
            })
            .collect();
        items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.alias.cmp(&b.alias)));
        items
    }

    /// Remove one alias key. Returns true if it existed.
    pub fn remove(&self, alias: &str) -> bool {
        let key = alias.trim().to_lowercase();
        let removed = {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            g.aliases.remove(&key).is_some()
        };
        if removed {
            self.dirty.store(true, Ordering::Relaxed);
            self.maybe_save(true);
        }
        removed
    }

    /// Drop every learned alias.
    pub fn clear_all(&self) {
        {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            if g.aliases.is_empty() {
                return;
            }
            g.aliases.clear();
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.maybe_save(true);
    }

    /// Manual pin from Settings (v3). `alias` is normalized; overwrites target.
    pub fn set_manual(&self, alias: &str, result_id: &str) -> Result<(), String> {
        let key = normalize_alias(alias)
            .ok_or_else(|| "Typo must be 3–24 letters (single word, no paths/math)".to_string())?;
        if !result_id.starts_with("app:") && !result_id.starts_with("path:") {
            return Err("Target must be an app or file result".into());
        }
        let now = now_secs();
        {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            g.aliases.insert(
                key,
                AliasEntry {
                    id: result_id.to_string(),
                    // Manual pins start confirmed so they boost strongly.
                    count: STRONG_COUNT.max(2),
                    last: now,
                },
            );
            if g.aliases.len() > MAX_ALIASES {
                prune_aliases(&mut g.aliases, MAX_ALIASES, now);
            }
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.maybe_save(true);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .aliases
            .len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Public view of one learned alias (Settings UI).
#[derive(Debug, Clone)]
pub struct TypoAlias {
    pub alias: String,
    pub id: String,
    pub count: u64,
    #[allow(dead_code)]
    pub last: u64,
    #[allow(dead_code)]
    pub score: i64,
}

impl Drop for TypoStore {
    fn drop(&mut self) {
        self.maybe_save(true);
    }
}

fn normalize_alias(q: &str) -> Option<String> {
    let s = q.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    // Single-token free-text only — skip paths, scopes, math, modes.
    if s.contains('/')
        || s.contains('\\')
        || s.contains('~')
        || s.contains('*')
        || s.contains('%')
        || s.contains('=')
        || s.contains('+')
        || s.starts_with("0x")
        || s.starts_with("0b")
    {
        return None;
    }
    // Multi-word: take first token only for alias keys (keep simple).
    let token = s.split_whitespace().next().unwrap_or(&s);
    let token = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
    if token.chars().count() < MIN_ALIAS_LEN || token.chars().count() > MAX_ALIAS_LEN {
        return None;
    }
    // Must be mostly letters (app-name typos), allow digits lightly.
    let letters = token.chars().filter(|c| c.is_alphabetic()).count();
    if letters < MIN_ALIAS_LEN {
        return None;
    }
    Some(token.to_string())
}

/// True when `alias` looks like a typo / near-miss of `title`, not normal typing.
fn should_learn_alias(alias: &str, title_lower: &str) -> bool {
    if title_lower.starts_with(alias) {
        // Normal progressive typing: `wha` → WhatsApp
        return false;
    }
    if title_lower.contains(alias) {
        // Substring hit is already strong search — not a typo alias
        return false;
    }
    near_title_prefix(alias, title_lower)
}

fn near_title_prefix(alias: &str, title: &str) -> bool {
    let ql = alias.chars().count();
    let tchars: Vec<char> = title.chars().collect();
    if tchars.is_empty() {
        return false;
    }
    let max_d = max_edit_distance(ql);
    // Compare against title prefixes of similar length (catches `wats` ≈ `whats`).
    let lo = ql.saturating_sub(1).max(1);
    let hi = (ql + 2).min(tchars.len());
    for len in lo..=hi {
        let prefix: String = tchars[..len].iter().collect();
        if levenshtein(alias, &prefix) <= max_d {
            return true;
        }
    }
    // Near-full-title typos (`firefow` ≈ `firefox`, `wahtsapp` ≈ `whatsapp`).
    let tl = tchars.len();
    if ql + 3 >= tl && levenshtein(alias, title) <= max_edit_distance(ql.max(tl)) {
        return true;
    }
    false
}

fn max_edit_distance(len: usize) -> usize {
    match len {
        0..=2 => 0,
        3..=4 => 1,
        5..=8 => 2,
        _ => 2,
    }
}

/// Classic Wagner–Fischer; aliases are short so O(n*m) is fine.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

fn prune_aliases(map: &mut HashMap<String, AliasEntry>, keep: usize, now: u64) {
    if map.len() <= keep {
        return;
    }
    let mut items: Vec<(String, i64)> = map
        .iter()
        .map(|(k, e)| (k.clone(), alias_frecency(e.count, e.last, now)))
        .collect();
    items.sort_by_key(|a| a.1); // coldest first
    let drop_n = map.len().saturating_sub(keep);
    for (k, _) in items.into_iter().take(drop_n) {
        map.remove(&k);
    }
}

fn alias_frecency(count: u64, last: u64, now: u64) -> i64 {
    let age = now.saturating_sub(last);
    let recency = if age < 86_400 {
        500
    } else if age < 86_400 * 7 {
        200
    } else {
        0
    };
    let days = age as f64 / 86_400.0;
    let decay = (-days / 21.0).exp();
    ((count as f64) * 1000.0 * decay) as i64 + recency
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn typo_path() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("hark/typos.json")
}

/// Score floor used when injecting an alias hit that wasn't already in results.
pub fn inject_floor() -> i64 {
    INJECT_FLOOR
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn temp_store() -> TypoStore {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hark-typos-{}-{}-{}.json",
            std::process::id(),
            n,
            now_secs()
        ));
        let _ = fs::remove_file(&path);
        TypoStore {
            inner: RwLock::new(TypoFile::default()),
            path,
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(Instant::now() - SAVE_DEBOUNCE),
        }
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("wats", "whats"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "ab"), 2);
    }

    #[test]
    fn rejects_normal_prefix_typing() {
        assert!(!should_learn_alias("wha", "whatsapp"));
        assert!(!should_learn_alias("what", "whatsapp"));
        assert!(!should_learn_alias("sapp", "whatsapp")); // contains
    }

    #[test]
    fn accepts_near_miss_typo() {
        assert!(should_learn_alias("wats", "whatsapp"));
        assert!(should_learn_alias("wahtsapp", "whatsapp"));
        assert!(should_learn_alias("firefow", "firefox"));
        // Missing final letters is still a title prefix (normal typing) — not a typo.
        assert!(!should_learn_alias("whatsap", "whatsapp"));
    }

    #[test]
    fn learn_and_lookup_v1() {
        let store = temp_store();
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        let (id, boost) = store.lookup("wats").expect("alias");
        assert_eq!(id, "app:whatsapp.desktop");
        assert_eq!(boost, BOOST_WEAK);
        // second confirm → strong
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        let (_, boost2) = store.lookup("wats").unwrap();
        assert_eq!(boost2, BOOST_STRONG);
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn learn_v2_from_session_history() {
        let store = temp_store();
        // Final query is the correct spelling; earlier session had the typo.
        store.learn_from_launch(
            "whatsapp",
            &["wats".into(), "whats".into()],
            "app:whatsapp.desktop",
            "WhatsApp",
        );
        assert!(store.lookup("wats").is_some());
        // Normal prefixes must not be stored
        assert!(store.lookup("whats").is_none());
        assert!(store.lookup("whatsapp").is_none());
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn list_remove_clear_and_manual() {
        let store = temp_store();
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        assert_eq!(store.len(), 1);
        let list = store.list();
        assert_eq!(list[0].alias, "wats");
        assert!(store.remove("wats"));
        assert_eq!(store.len(), 0);
        store
            .set_manual("ffox", "app:firefox.desktop")
            .expect("manual");
        assert_eq!(
            store.lookup("ffox").map(|(id, _)| id),
            Some("app:firefox.desktop".into())
        );
        store.clear_all();
        assert_eq!(store.len(), 0);
        assert!(store.set_manual("x", "app:a.desktop").is_err());
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn normalize_skips_paths_and_short() {
        assert!(normalize_alias("wa").is_none());
        assert!(normalize_alias("~/foo").is_none());
        assert!(normalize_alias("15% of 80").is_none());
        assert_eq!(normalize_alias("  Wats  ").as_deref(), Some("wats"));
    }

    #[test]
    fn save_is_compact_json() {
        let store = temp_store();
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        store.flush();
        let raw = fs::read_to_string(&store.path).expect("read typos");
        assert!(
            !raw.contains("\n  "),
            "expected compact JSON without pretty indentation"
        );
        assert!(raw.contains("wats"));
        let _ = fs::remove_file(&store.path);
    }
}
