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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Set by `set_manual` (Settings pins). The auto-learner never
    /// retargets a manual pin, regardless of conflicting launches.
    #[serde(default)]
    manual: bool,
    /// Consecutive searches where the alias target failed to resolve.
    /// Renamed/unmounted targets stop costing a resolve per keystroke once
    /// this hits `MAX_DEAD_STREAK` (non-manual aliases are dropped).
    #[serde(default)]
    fail_streak: u32,
}

/// Drop a non-manual alias after this many consecutive resolve failures.
const MAX_DEAD_STREAK: u32 = 5;
/// Minimum decayed frecency for a strong boost (see `lookup`).
const STRONG_FRECENCY: i64 = 1_000;

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

/// Load with backup + per-entry salvage (audit P2): a truncated file, an
/// empty file, or one wrong-typed entry used to wipe every learned alias
/// silently, and the next debounced save then persisted the wipe. Now the
/// corrupt file is copied aside + logged, and each intact entry survives.
fn load_typo_file(path: &Path) -> TypoFile {
    let Some(s) = crate::config::read_private_file(path) else {
        return TypoFile::default();
    };
    match serde_json::from_str::<TypoFile>(&s) {
        Ok(f) => f,
        Err(err) => {
            match crate::config::backup_invalid_config(path) {
                Some(b) => eprintln!(
                    "hark: invalid typos store {} ({err}); salvaging intact aliases (backup: {})",
                    path.display(),
                    b.display()
                ),
                None => eprintln!(
                    "hark: invalid typos store {} ({err}); salvaging intact aliases (backup failed)",
                    path.display()
                ),
            }
            salvage_typo_file(&s)
        }
    }
}

/// Best-effort rescue: keep every entry that still parses, skip the rest.
fn salvage_typo_file(s: &str) -> TypoFile {
    let mut out = TypoFile::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return out;
    };
    out.version = v
        .get("version")
        .and_then(|x| serde_json::from_value(x.clone()).ok())
        .unwrap_or_else(default_version);
    if let Some(map) = v.get("aliases").and_then(|a| a.as_object()) {
        for (k, ev) in map {
            if k.is_empty() {
                continue;
            }
            if let Ok(e) = serde_json::from_value::<AliasEntry>(ev.clone()) {
                if e.id.is_empty() {
                    continue;
                }
                out.aliases.insert(k.clone(), e);
            }
        }
    }
    out
}

pub struct TypoStore {
    inner: RwLock<TypoFile>,
    path: PathBuf,
    dirty: AtomicBool,
    last_save: Mutex<Instant>,
    /// Highest wall-clock seen — same rollback pin as `UsageStore::max_now`
    /// (audit P3): decay must keep progressing when the clock steps back.
    max_now: AtomicU64,
}

impl TypoStore {
    /// Test-only: empty in-memory store (no disk read/write).
    #[cfg(test)]
    pub(crate) fn new_empty() -> Self {
        Self {
            inner: RwLock::new(TypoFile::default()),
            path: std::path::PathBuf::from("<test>"),
            dirty: AtomicBool::new(false),
            max_now: AtomicU64::new(now_secs()),
            last_save: Mutex::new(
                Instant::now()
                    .checked_sub(SAVE_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            ),
        }
    }

    pub fn load() -> Self {
        let path = typo_path();
        // read_private_file refuses files in shared /tmp fallback space not
        // owned by this user — a planted aliases file would steer launches.
        let mut data = load_typo_file(&path);
        let now = now_secs();
        if data.aliases.len() > MAX_ALIASES {
            prune_aliases(&mut data.aliases, MAX_ALIASES, now);
        }
        Self {
            inner: RwLock::new(data),
            path,
            dirty: AtomicBool::new(false),
            max_now: AtomicU64::new(now),
            last_save: Mutex::new(
                Instant::now()
                    .checked_sub(SAVE_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            ),
        }
    }

    /// O(1) lookup. Returns `(result_id, score_boost)` when an alias exists.
    /// The strong boost is gated on decayed frecency, not raw count: a
    /// two-year-stale alias must not outrank live prefix hits forever
    /// (audit P2). Manual pins are exempt (explicit user intent).
    pub fn lookup(&self, query: &str) -> Option<(String, i64)> {
        let key = normalize_alias(query)?;
        let g = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let e = g.aliases.get(&key)?;
        let strong = e.count >= STRONG_COUNT
            && (e.manual || alias_frecency(e.count, e.last, self.now_clamped()) >= STRONG_FRECENCY);
        let boost = if strong { BOOST_STRONG } else { BOOST_WEAK };
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
        // v2 — earlier spellings in this open session, but only genuine
        // reformulations of the final query (within 2 edits). Abandoned
        // dead-ends the user backspaced away from (`wat`→`wats`→`watss`→
        // `whatsapp` minting `watss`) must not become injected aliases
        // (audit P3). If the final query itself isn't alias-shaped, there
        // is no reformulation anchor — skip v2 entirely.
        let final_norm = normalize_alias(final_query);
        if let Some(ref f) = final_norm {
            for q in recent_queries {
                let Some(k) = normalize_alias(q) else {
                    continue;
                };
                if seen.contains(&k) || levenshtein(&k, f) > 2 {
                    continue;
                }
                seen.insert(k.clone());
                candidates.push(k);
            }
        }

        // Launches the alias itself produced must not entrench it: activating
        // the injected top row re-runs learning with the same query, which
        // used to increment count monotonically with no escape (audit P2).
        // Refresh recency (keeps decay honest) but don't count it.
        let alias_driven = final_norm.as_ref().is_some_and(|f| {
            seen.contains(f)
                && self
                    .lookup(final_query)
                    .is_some_and(|(id, _)| id == result_id)
        });

        let mut learned_any = false;
        for key in candidates {
            if !should_learn_alias(&key, &title) {
                continue;
            }
            let driven = alias_driven && final_norm.as_ref().is_some_and(|f| *f == key);
            self.record_alias(&key, result_id, driven);
            learned_any = true;
        }
        if learned_any {
            self.maybe_save(false);
        }
    }

    /// `alias_driven` marks launches the alias itself produced (activating
    /// the injected top row re-runs learning with the same query). Those
    /// still refresh recency but stop incrementing once confirmed — otherwise
    /// count grows monotonically with no escape (audit P2). Unconfirmed
    /// aliases always count (a weak alias needs its confirming observation).
    fn record_alias(&self, key: &str, result_id: &str, alias_driven: bool) {
        let now = self.now_clamped();
        let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
        match g.aliases.get_mut(key) {
            Some(e) if e.id == result_id => {
                if !(alias_driven && e.count >= STRONG_COUNT) {
                    e.count = e.count.saturating_add(1);
                }
                e.last = now;
                e.fail_streak = 0;
            }
            Some(e) => {
                // Conflicting target: only switch while unconfirmed (below
                // STRONG_COUNT) — a twice-confirmed alias is never retargeted
                // by a single conflicting launch (audit P3). Manual pins
                // (Settings) are never auto-retargeted.
                if !e.manual && e.count < STRONG_COUNT {
                    e.id = result_id.to_string();
                    e.count = 1;
                    e.last = now;
                    e.fail_streak = 0;
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
                        manual: false,
                        fail_streak: 0,
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
        let now = self.now_clamped();
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
    /// `resolve` checks the id against live providers so a structurally-valid
    /// but nonexistent id (`app:ghost.desktop`, `path:/gone`) can't be pinned
    /// as a zombie alias that later launches nothing.
    pub fn set_manual(
        &self,
        alias: &str,
        result_id: &str,
        resolve: impl Fn(&str) -> bool,
    ) -> Result<(), String> {
        let key = normalize_alias(alias)
            .ok_or_else(|| "Typo must be 3–24 letters (single word, no paths/math)".to_string())?;
        if !result_id.starts_with("app:") && !result_id.starts_with("path:") {
            return Err("Target must be an app or file result".into());
        }
        if !resolve(result_id) {
            return Err("Target not found — pick an existing app or file".into());
        }
        let now = self.now_clamped();
        {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            g.aliases.insert(
                key,
                AliasEntry {
                    id: result_id.to_string(),
                    // Manual pins start confirmed so they boost strongly.
                    count: STRONG_COUNT.max(2),
                    last: now,
                    manual: true,
                    fail_streak: 0,
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

    /// Track whether an alias target resolved (called by the engine on
    /// searches that hit the alias). A renamed/unmounted target otherwise
    /// costs a filesystem resolve per keystroke forever (audit P3):
    /// non-manual aliases are dropped after `MAX_DEAD_STREAK` consecutive
    /// failures. Manual pins are never auto-removed.
    pub fn note_resolve(&self, query: &str, ok: bool) {
        let Some(key) = normalize_alias(query) else {
            return;
        };
        let mut touched = false;
        {
            let mut g = self.inner.write().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = g.aliases.get_mut(&key) {
                if ok {
                    if e.fail_streak > 0 {
                        e.fail_streak = 0;
                        touched = true;
                    }
                } else if !e.manual {
                    e.fail_streak = e.fail_streak.saturating_add(1);
                    touched = true;
                    if e.fail_streak >= MAX_DEAD_STREAK {
                        g.aliases.remove(&key);
                    }
                }
            }
        }
        if touched {
            self.dirty.store(true, Ordering::Relaxed);
            self.maybe_save(false);
        }
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
    // Both branches share the query-length budget: a 4-char alias gets 1
    // edit against 5–7-char titles, not a doubled budget (audit P3).
    let tl = tchars.len();
    if ql + 3 >= tl && levenshtein(alias, title) <= max_edit_distance(ql) {
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
        // Manual pins survive pruning — evicting a user's explicit pin
        // because it hasn't fired recently is data loss.
        .filter(|(_, e)| !e.manual)
        .map(|(k, e)| (k.clone(), alias_frecency(e.count, e.last, now)))
        .collect();
    items.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0))); // coldest first, id tie-break (deterministic)
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

impl TypoStore {
    /// Wall-clock pinned to the highest value this store has seen (audit P3).
    fn now_clamped(&self) -> u64 {
        let now = now_secs();
        self.max_now.fetch_max(now, Ordering::Relaxed).max(now)
    }
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

    /// Scratch dir satisfying write_private_file's trust rule (0700,
    /// owned): tests write real files, so they need a private subdir of
    /// /tmp rather than /tmp itself.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hark-typos-test-{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
        dir
    }

    fn temp_store() -> TypoStore {
        let path = scratch_dir("store").join("typos.json");
        let _ = fs::remove_file(&path);
        TypoStore {
            inner: RwLock::new(TypoFile::default()),
            path,
            dirty: AtomicBool::new(false),
            max_now: AtomicU64::new(now_secs()),
            last_save: Mutex::new(
                Instant::now()
                    .checked_sub(SAVE_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            ),
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
        // Final query is a near-miss; an earlier session token within 2
        // edits of it is learned as a genuine reformulation.
        store.learn_from_launch(
            "whats",
            &["wats".into(), "what".into()],
            "app:whatsapp.desktop",
            "WhatsApp",
        );
        assert!(store.lookup("wats").is_some());
        // Normal prefixes must not be stored
        assert!(store.lookup("what").is_none());
        assert!(store.lookup("whats").is_none());
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn session_dead_ends_not_learned() {
        // Audit P3 (Pass 18): `wat`→`wats`→`watss`→`whatsapp` must not mint
        // abandoned intermediates as injected aliases.
        let store = temp_store();
        store.learn_from_launch(
            "whatsapp",
            &["wat".into(), "wats".into(), "watss".into()],
            "app:whatsapp.desktop",
            "WhatsApp",
        );
        assert!(store.lookup("wats").is_none());
        assert!(store.lookup("watss").is_none());
        // The exact-title final query itself is not an alias either.
        assert_eq!(store.len(), 0);
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn stale_alias_decays_to_weak_but_resolves() {
        // Audit P2 (Pass 18): boost must follow decayed frecency, not raw count.
        let store = temp_store();
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        assert_eq!(store.lookup("wats").map(|(_, b)| b), Some(BOOST_STRONG));
        // Age it 60 days: 2000·e^(−60/21) ≈ 114 < STRONG_FRECENCY → weak.
        {
            let mut g = store.inner.write().unwrap();
            let e = g.aliases.get_mut("wats").unwrap();
            e.last = now_secs() - 60 * 86_400;
        }
        assert_eq!(store.lookup("wats").map(|(_, b)| b), Some(BOOST_WEAK));
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn manual_pin_ignores_decay() {
        let store = temp_store();
        store
            .set_manual("ffox", "app:firefox.desktop", |_| true)
            .expect("manual");
        {
            let mut g = store.inner.write().unwrap();
            let e = g.aliases.get_mut("ffox").unwrap();
            e.last = now_secs() - 400 * 86_400;
        }
        assert_eq!(store.lookup("ffox").map(|(_, b)| b), Some(BOOST_STRONG));
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn alias_driven_launch_does_not_entrench() {
        // Audit P2 (Pass 18): activating the alias-injected top row must not
        // keep incrementing count (monotone entrenchment with no escape).
        let store = temp_store();
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        let count = store.inner.read().unwrap().aliases["wats"].count;
        assert_eq!(count, 2);
        // Third launch produced by the alias itself: recency refreshes, count stays.
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        let after = store.inner.read().unwrap().aliases["wats"].clone();
        assert_eq!(after.count, 2);
        assert!(after.last >= now_secs() - 5);
        // Fresh + confirmed → still strong (no behavior change for live aliases).
        assert_eq!(store.lookup("wats").map(|(_, b)| b), Some(BOOST_STRONG));
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn confirmed_alias_survives_conflict_unconfirmed_switches() {
        // Audit P3 (Pass 13): only below-STRONG counts retarget.
        let store = temp_store();
        store.learn_from_launch("firefow", &[], "app:firefox.desktop", "Firefox");
        // Single observation → conflicting near-title launch retargets.
        store.learn_from_launch("firefow", &[], "app:firefog.desktop", "Firefog");
        assert_eq!(
            store.lookup("firefow").map(|(id, _)| id),
            Some("app:firefog.desktop".into())
        );
        // Confirm twice → a further conflict is ignored.
        store.learn_from_launch("firefow", &[], "app:firefog.desktop", "Firefog");
        store.learn_from_launch("firefow", &[], "app:firefox.desktop", "Firefox");
        assert_eq!(
            store.lookup("firefow").map(|(id, _)| id),
            Some("app:firefog.desktop".into()),
            "twice-confirmed alias must not retarget on one conflict"
        );
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn dead_alias_dropped_after_streak_manual_immune() {
        // Audit P3 (Pass 18): dead targets stop costing a resolve per keystroke.
        let store = temp_store();
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        store.learn_from_launch("wats", &[], "app:whatsapp.desktop", "WhatsApp");
        for _ in 0..4 {
            store.note_resolve("wats", false);
        }
        assert!(store.lookup("wats").is_some());
        store.note_resolve("wats", false);
        assert!(store.lookup("wats").is_none());
        // Manual pins are never auto-removed.
        store
            .set_manual("ffox", "app:firefox.desktop", |_| true)
            .expect("manual");
        for _ in 0..10 {
            store.note_resolve("ffox", false);
        }
        assert!(store.lookup("ffox").is_some());
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
            .set_manual("ffox", "app:firefox.desktop", |_| true)
            .expect("manual");
        assert_eq!(
            store.lookup("ffox").map(|(id, _)| id),
            Some("app:firefox.desktop".into())
        );
        store.clear_all();
        assert_eq!(store.len(), 0);
        assert!(store.set_manual("x", "app:a.desktop", |_| true).is_err());
        // Zombie ids: structurally valid prefix but no live result — the
        // resolver callback rejects them (audit Pass 18).
        assert!(store
            .set_manual("ffox", "app:ghost.desktop", |_| false)
            .is_err());
        assert!(store
            .set_manual("ffox", "path:/nonexistent/file", |_| false)
            .is_err());
        assert_eq!(store.len(), 0);
        store.flush();
        let _ = fs::remove_file(&store.path);
    }

    #[test]
    fn manual_pin_not_retargeted_by_learner() {
        // Audit P2 (Pass 18): set_manual writes count=2 which the learner's
        // `count <= 2` branch treated as unconfirmed — one conflicting
        // launch silently retargeted the user's pin. The manual flag now
        // makes the learner skip conflicting updates on pins entirely.
        let store = temp_store();
        store
            .set_manual("ffox", "app:firefox.desktop", |_| true)
            .expect("manual");
        // Conflicting launch of the same alias pointing elsewhere:
        store.learn_from_launch("ffox", &[], "app:ghost.desktop", "Ghost");
        assert_eq!(
            store.lookup("ffox").map(|(id, _)| id),
            Some("app:firefox.desktop".into()),
            "manual pin must survive a conflicting launch"
        );
        // Same-target launches still refresh recency (no manual check there):
        store.learn_from_launch("ffox", &[], "app:firefox.desktop", "Firefox");
        assert_eq!(
            store.lookup("ffox").map(|(id, _)| id),
            Some("app:firefox.desktop".into())
        );
        // Legacy files without the manual field still deserialize (false).
        let legacy = r#"{"version":2,"aliases":{"ffox":{"id":"app:firefox.desktop","count":2,"last":1750000000}}}"#;
        let f: TypoFile = serde_json::from_str(legacy).expect("legacy file");
        assert!(!f.aliases["ffox"].manual);
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

    #[test]
    fn corrupt_store_backs_up_and_salvages_intact_aliases() {
        // Audit P2: one wrong-typed entry must not wipe the whole file.
        let dir = scratch_dir("salvage");
        let path = dir.join("typos.json");
        fs::write(
            &path,
            r#"{"version":1,"aliases":{"good":{"id":"app:a.desktop","count":2,"last":0},"bad":{"id":"","count":"nope"}}}"#,
        )
        .unwrap();
        let f = load_typo_file(&path);
        assert!(f.aliases.contains_key("good"));
        assert!(!f.aliases.contains_key("bad"));
        assert!(
            path.with_extension("json.invalid").exists(),
            "corrupt store must be copied aside"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_store_backs_up_to_empty() {
        let dir = scratch_dir("salvage-trunc");
        let path = dir.join("typos.json");
        fs::write(&path, r#"{"version":1,"aliases":{"a":"#).unwrap();
        let f = load_typo_file(&path);
        assert!(f.aliases.is_empty());
        assert!(path.with_extension("json.invalid").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
