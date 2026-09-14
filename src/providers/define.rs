//! Inline definitions (no LLM, no API key).
//!
//! Fast path (UI thread): pattern parse + **process-memory** cache / fail
//! only (no disk). Network path: `search_network` on a worker thread — may
//! read durable disk cache, then Wikipedia summary → DuckDuckGo Instant
//! Answer (never blocks GTK).
//! When `DefineConfig.enabled` is false: zero I/O.
//!
//! A total miss still owns the query with a fail row whose Enter opens a
//! web search for the term — the launcher never dead-ends.

use crate::config::{ConfigStore, DefineConfig};
use crate::providers::web::google_search_url;
use crate::providers::{Action, ResultKind, SearchResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const DEFINE_SCORE: i64 = 100_000;
const DEFINE_PENDING_SCORE: i64 = 95_000;
const DEFINE_FAIL_SCORE: i64 = 80_000;
/// Definitions barely change; keep them a month.
const CACHE_TTL_SECS: u64 = 30 * 24 * 3600;
/// Per-entry stored-text cap (bytes). Long extracts are truncated before
/// they can persist durably.
const CACHE_ENTRY_MAX_BYTES: usize = 2000;
/// Avoid hammering free APIs on repeated misses.
const FAIL_CACHE_SECS: u64 = 90;
const PENDING_PREFIX: &str = "define:pending:";
const OK_PREFIX: &str = "define:";
/// Single-char terms never hit the network: `define b` owns no useful
/// article and each prefix fetch used to head-of-line block the final term.
const MIN_TERM_CHARS: usize = 2;

/// A fetched definition plus optional Wikipedia article media. Image/page
/// URLs ride the durable cache so the preview can render article art
/// without a second API call.
#[derive(Debug, Clone)]
struct DefineHit {
    title: String,
    extract: String,
    source: String,
    image_url: Option<String>,
    page_url: Option<String>,
    description: Option<String>,
}

/// UI-safe article media for a resolved define row (mem cache only, no I/O).
#[derive(Debug, Clone)]
pub struct DefineMedia {
    pub image_url: String,
    pub page_url: Option<String>,
    pub description: Option<String>,
}

/// Process-local success cache (disk is the durable layer).
fn mem_ok() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static M: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Negative cache: key → (term, fetched_at). Misses own the query with a
/// web-search fail row, so remember them briefly without disk writes.
fn mem_fail() -> &'static Mutex<HashMap<String, (String, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (String, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct DefineProvider {
    config: Arc<ConfigStore>,
}

impl DefineProvider {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self { config }
    }

    pub fn cfg(&self) -> DefineConfig {
        self.config.with(|c| c.define.clone())
    }

    pub fn is_enabled(&self) -> bool {
        self.config.with(|c| c.define.enabled)
    }

    pub fn should_handle(&self, query: &str) -> bool {
        self.config.with(|c| {
            let cfg = &c.define;
            cfg.enabled && is_define_query(query, cfg)
        })
    }

    /// True when UI should spawn a worker: enabled, matches, term within
    /// budget, not already in **memory** cache or recent-fail.
    /// Disk is checked on the worker (`search_network`) so the UI thread
    /// never blocks on FS.
    pub fn needs_network(&self, query: &str) -> bool {
        let cfg = self.cfg();
        if !cfg.enabled || !is_define_query(query, &cfg) {
            return false;
        }
        let Some((term, _)) = parse_term(query) else {
            return false;
        };
        if term.chars().count() < MIN_TERM_CHARS {
            return false;
        }
        let key = cache_key(&term);
        if cache_get_mem(&key).is_some() {
            return false;
        }
        if fail_get(&key).is_some() {
            return false;
        }
        true
    }

    /// Blocking definition fetch (worker thread only). May read disk cache.
    /// Misses return a web-search fail row (never empty — the query is
    /// owned either way, so Enter always does something useful).
    pub fn search_network(&self, query: &str) -> Vec<SearchResult> {
        let cfg = self.cfg();
        if !cfg.enabled {
            return Vec::new();
        }
        if !is_define_query(query, &cfg) {
            return Vec::new();
        }
        let Some((term, _)) = parse_term(query) else {
            return Vec::new();
        };
        if term.chars().count() < MIN_TERM_CHARS {
            return Vec::new();
        }
        let key = cache_key(&term);
        if let Some(hit) = cache_get(&key) {
            return vec![ok_result(&term, &hit.title, &hit.extract, &hit.source)];
        }
        if fail_get(&key).is_some() {
            return vec![fail_result(&term)];
        }
        match fetch_definition(&term) {
            Ok(hit) => {
                cache_put(&key, &term, &hit);
                fail_clear(&key);
                vec![ok_result(&term, &hit.title, &hit.extract, &hit.source)]
            }
            Err(_) => {
                fail_put(&key, &term);
                vec![fail_result(&term)]
            }
        }
    }

    /// UI-thread safe: memory cache hit, recent fail, or a "Defining…"
    /// placeholder. **No disk I/O** — durable cache loads on the worker.
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let cfg = self.cfg();
        if !cfg.enabled {
            return Vec::new();
        }
        if !is_define_query(query, &cfg) {
            return Vec::new();
        }
        let Some((term, _)) = parse_term(query) else {
            return Vec::new();
        };
        if term.chars().count() < MIN_TERM_CHARS {
            return Vec::new();
        }
        let key = cache_key(&term);
        if let Some(hit) = cache_get_mem(&key) {
            return vec![ok_result(&term, &hit.title, &hit.extract, &hit.source)];
        }
        if fail_get(&key).is_some() {
            return vec![fail_result(&term)];
        }
        vec![pending_result(&term)]
    }
}

/// Parse a query into `(term, forced)`.
///
/// Forced prefixes (like `tr `): `def `, `define `, `dict `, `dictionary `.
/// Question shapes: `what does X mean`, `what is X`, `what's X`,
/// `X full form`, `full form of X`, `X stands for`,
/// `what does X stand for`, `meaning of X`, `X meaning`, `X means`.
pub fn parse_term(query: &str) -> Option<(String, bool)> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    // Trailing "?" is punctuation, not part of the term.
    let q = q.trim_end_matches('?').trim();
    if q.is_empty() {
        return None;
    }
    // Forced prefixes first (longest first so `define` wins over `def`).
    for prefix in ["dictionary ", "define ", "dict ", "def "] {
        if q.get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
        {
            let term = q[prefix.len()..].trim();
            return valid_term(term).map(|t| (t, true));
        }
    }
    let lower = q.to_ascii_lowercase();
    // `what's X` / `whats X` (no space after "what", so checked first).
    for verb in ["what's ", "whats "] {
        if let Some(after) = strip_prefix_word(&lower, verb) {
            let term = q[after..].trim();
            return valid_term(term).map(|t| (t, false));
        }
    }
    // `what does X mean` / `what do X mean` / `what does X stand for` …
    if let Some(rest) = strip_prefix_word(&lower, "what ") {
        let rest_orig = &q[rest..];
        let rest_lower = &lower[rest..];
        for verb in ["does ", "do "] {
            if let Some(after) = strip_prefix_word(rest_lower, verb) {
                let term = rest_orig[after..].trim();
                if let Some(t) = strip_suffix_word(term, " stand for") {
                    return valid_term(&t).map(|t| (t, false));
                }
                if let Some(t) = strip_suffix_word(term, " mean") {
                    return valid_term(&t).map(|t| (t, false));
                }
                // `what does X` with no trailing verb word is not a question.
                return None;
            }
        }
        for verb in ["is ", "are "] {
            if let Some(after) = strip_prefix_word(rest_lower, verb) {
                let term = rest_orig[after..].trim();
                return valid_term(term).map(|t| (t, false));
            }
        }
        return None;
    }
    // `full form of X`
    if let Some(after) = strip_prefix_word(&lower, "full form of ") {
        return valid_term(q[after..].trim()).map(|t| (t, false));
    }
    // `meaning of X`
    if let Some(after) = strip_prefix_word(&lower, "meaning of ") {
        return valid_term(q[after..].trim()).map(|t| (t, false));
    }
    // Suffix shapes on the whole query.
    for suffix in [" full form", " stands for", " meaning", " means"] {
        if let Some(t) = strip_suffix_word(q, suffix) {
            // Suffix match must be case-insensitive: compare on lowered ends.
            if q.len() >= suffix.len() && q[q.len() - suffix.len()..].eq_ignore_ascii_case(suffix) {
                return valid_term(&t).map(|t| (t, false));
            }
        }
    }
    None
}

/// ASCII case-insensitive prefix strip returning the byte offset of the rest.
/// `get` keeps this panic-free on multi-byte queries.
fn strip_prefix_word(lower: &str, prefix: &str) -> Option<usize> {
    if lower.get(..prefix.len()).is_some_and(|s| s == prefix) {
        Some(prefix.len())
    } else {
        None
    }
}

/// ASCII case-insensitive suffix strip returning the leading part.
fn strip_suffix_word(orig: &str, suffix: &str) -> Option<String> {
    if orig.len() >= suffix.len() && orig[orig.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    {
        Some(orig[..orig.len() - suffix.len()].to_string())
    } else {
        None
    }
}

/// A term is 1+ letters, bounded, and never a path/glob/URL.
fn valid_term(term: &str) -> Option<String> {
    let t = term.trim();
    if t.is_empty() || t.chars().count() > 80 {
        return None;
    }
    if !t.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    if t.contains('/') || t.contains('*') || t.contains("://") {
        return None;
    }
    Some(t.to_string())
}

pub fn is_define_query(query: &str, cfg: &DefineConfig) -> bool {
    if !cfg.enabled {
        return false;
    }
    let Some((term, _)) = parse_term(query) else {
        return false;
    };
    if term.chars().count() > cfg.max_chars {
        return false;
    }
    true
}

/// Article row (the full extract travels on the row's `Copy` action and
/// renders inline in the list; the preview pane shows the Wikipedia image
/// when the summary carries one). Enter copies the whole extract.
fn ok_result(term: &str, title: &str, extract: &str, source: &str) -> SearchResult {
    let para = collapse_ws(extract);
    SearchResult {
        id: format!("define:{}", cache_key(term)),
        title: format!("{title} · {source}"),
        subtitle: snippet(&para),
        kind: ResultKind::Define,
        score: DEFINE_SCORE,
        icon: Some("accessories-dictionary".into()),
        action: Action::Copy(para),
        conversion: None,
        matched: None,
    }
}

fn pending_result(term: &str) -> SearchResult {
    SearchResult {
        id: format!("{PENDING_PREFIX}{:016x}", fnv1a64(&normalized_key(term))),
        title: "Defining…".into(),
        subtitle: term.to_string(),
        kind: ResultKind::Define,
        score: DEFINE_PENDING_SCORE,
        icon: Some("accessories-dictionary".into()),
        action: Action::Copy(term.to_string()),
        conversion: None,
        matched: None,
    }
}

/// Misses never dead-end: Enter opens a web search for the term.
fn fail_result(term: &str) -> SearchResult {
    SearchResult {
        id: format!("define:fail:{:016x}", fnv1a64(&normalized_key(term))),
        title: "No definition found".into(),
        subtitle: format!("Enter to search the web for “{term}”"),
        kind: ResultKind::Command,
        score: DEFINE_FAIL_SCORE,
        icon: Some("applications-internet".into()),
        action: Action::OpenUrl(google_search_url(term)),
        conversion: None,
        matched: None,
    }
}

pub fn is_pending_result(r: &SearchResult) -> bool {
    r.id.starts_with(PENDING_PREFIX)
}

/// Single-space the extract so rows and the reader show one clean paragraph.
fn collapse_ws(extract: &str) -> String {
    extract.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One-line row snippet: word-boundary truncation with an ellipsis.
fn snippet(para: &str) -> String {
    const MAX: usize = 140;
    if para.chars().count() <= MAX {
        return para.to_string();
    }
    let mut head: String = para.chars().take(MAX - 1).collect();
    if let Some(i) = head.rfind(' ') {
        head.truncate(i);
    }
    format!("{head}…")
}

// ── HTTP (worker only) ──────────────────────────────────────────────────────

/// Wikipedia summary first, DDG Instant Answer as backup.
fn fetch_definition(term: &str) -> Result<DefineHit, ()> {
    if let Ok(hit) = wikipedia_summary(term) {
        return Ok(hit);
    }
    if let Ok(hit) = ddg_abstract(term) {
        return Ok(hit);
    }
    Err(())
}

fn wikipedia_summary(term: &str) -> Result<DefineHit, ()> {
    // Spaces → underscores (canonical article form), then path-encode.
    let title_path = crate::providers::web::encode_query(&term.replace(' ', "_"));
    let url = format!("https://en.wikipedia.org/api/rest_v1/page/summary/{title_path}");
    let bytes = crate::providers::http::get_bytes(&url).map_err(|_| ())?;
    parse_wikipedia_body(&bytes)
}

/// True for resolved define rows (excludes `Defining…` pending + miss rows).
pub fn is_ok_result(r: &SearchResult) -> bool {
    r.kind == ResultKind::Define && r.id.starts_with(OK_PREFIX) && !is_pending_result(r)
}

/// Mem-cache-only media lookup for a resolved row id (safe on GTK main).
/// Returns `None` for pending/miss rows, cache misses, and non-Wikimedia
/// image hosts (planted cache entries must never turn the preview into a
/// generic URL fetcher).
pub fn lookup_media_by_id(result_id: &str) -> Option<DefineMedia> {
    let key = result_id.strip_prefix(OK_PREFIX)?;
    // Pending ids use a different prefix so they never reach here, but
    // belt-and-braces: keys are fixed 16-hex hashes.
    if key.len() != 16 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hit = cache_get_mem(key)?;
    let image_url = hit.image_url?;
    if !is_allowed_image_url(&image_url) {
        return None;
    }
    Some(DefineMedia {
        image_url,
        page_url: hit.page_url,
        description: hit.description,
    })
}

fn is_allowed_image_url(url: &str) -> bool {
    url.starts_with("https://upload.wikimedia.org/")
        || url.starts_with("https://thumb.wikimedia.org/")
}

fn parse_wikipedia_body(bytes: &[u8]) -> Result<DefineHit, ()> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    // Disambiguation pages carry no usable definition — let DDG try.
    if v.get("type").and_then(|t| t.as_str()) == Some("disambiguation") {
        return Err(());
    }
    let extract = v.get("extract").and_then(|e| e.as_str()).unwrap_or("");
    if extract.trim().is_empty() {
        return Err(());
    }
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("").trim();
    if title.is_empty() {
        return Err(());
    }
    // Prefer the caller-sized thumbnail (330px fits the 280px preview
    // stage); fall back to the original when no thumbnail is listed.
    let image_url = v
        .get("thumbnail")
        .and_then(|t| t.get("source"))
        .and_then(|s| s.as_str())
        .or_else(|| {
            v.get("originalimage")
                .and_then(|t| t.get("source"))
                .and_then(|s| s.as_str())
        })
        .filter(|u| is_allowed_image_url(u))
        .map(|s| s.to_string());
    let page_url = v
        .get("content_urls")
        .and_then(|c| c.get("desktop"))
        .and_then(|d| d.get("page"))
        .and_then(|p| p.as_str())
        .filter(|u| {
            u.starts_with("https://en.wikipedia.org/wiki/")
                || u.starts_with("http://en.wikipedia.org/wiki/")
        })
        .map(|s| s.to_string());
    let description = v
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(DefineHit {
        title: title.to_string(),
        extract: extract.trim().to_string(),
        source: "Wikipedia".into(),
        image_url,
        page_url,
        description,
    })
}

fn ddg_abstract(term: &str) -> Result<DefineHit, ()> {
    let bytes = crate::providers::http::get_bytes_query(
        "https://api.duckduckgo.com/",
        &[
            ("q", term),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ],
    )
    .map_err(|_| ())?;
    parse_ddg_body(&bytes)
}

fn parse_ddg_body(bytes: &[u8]) -> Result<DefineHit, ()> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let text = v
        .get("AbstractText")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err(());
    }
    let source = v
        .get("AbstractSource")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    let source = if source.is_empty() {
        "DuckDuckGo".to_string()
    } else {
        source.to_string()
    };
    // Heading doubles as the display title when present.
    let heading = v
        .get("Heading")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .trim();
    let title = if heading.is_empty() {
        source.clone()
    } else {
        heading.to_string()
    };
    Ok(DefineHit {
        title,
        extract: text.to_string(),
        source,
        image_url: None,
        page_url: None,
        description: None,
    })
}

// ── Cache ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    term: String,
    title: String,
    extract: String,
    source: String,
    fetched_at: u64,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    page_url: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

fn cache_dir() -> PathBuf {
    // No `/tmp` fallback: a world-writable directory lets any local user
    // plant entries that are later copied to the clipboard.
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join(".cache/hark/define"))
        .unwrap_or_default()
}

/// True when the cache directory is owned by this user and locked to 0700
/// (mirrors the fx.rs / translate.rs disk guards).
#[cfg(unix)]
fn cache_dir_trusted(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = fs::metadata(dir) else {
        return false;
    };
    m.is_dir() && m.mode() & 0o777 == 0o700 && m.uid() == current_euid().unwrap_or(u32::MAX)
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no args, always succeeds, no side effects.
    Some(unsafe { geteuid() })
}

#[cfg(not(unix))]
fn cache_dir_trusted(_dir: &Path) -> bool {
    true
}

fn cache_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.json"))
}

/// Process-memory only — safe on the GTK main thread (no FS).
fn cache_get_mem(key: &str) -> Option<CacheEntry> {
    let Ok(g) = mem_ok().lock() else {
        return None;
    };
    let e = g.get(key)?;
    if now_secs().saturating_sub(e.fetched_at) > CACHE_TTL_SECS || e.extract.trim().is_empty() {
        return None;
    }
    Some(e.clone())
}

/// Mem first, then durable disk (promotes into mem). **Worker thread only.**
#[cfg(unix)]
fn read_cache_file(path: &Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(target_os = "linux")]
    const O_NOFOLLOW: i32 = 0o400_000;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const O_NOFOLLOW: i32 = 0o400;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
    const O_NOFOLLOW: i32 = 0;
    let mut f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .ok()?;
    let mut data = String::new();
    f.read_to_string(&mut data).ok()?;
    Some(data)
}

#[cfg(not(unix))]
fn read_cache_file(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn cache_get(key: &str) -> Option<CacheEntry> {
    if let Some(e) = cache_get_mem(key) {
        return Some(e);
    }
    let path = cache_path(key);
    if !cache_dir_trusted(path.parent()?) {
        return None;
    }
    let data = read_cache_file(&path)?;
    let e: CacheEntry = serde_json::from_str(&data).ok()?;
    // The key is a hash of the normalized term; recomputing it from the
    // entry must reproduce the same key or the entry is planted.
    if cache_key(&e.term) != key {
        return None;
    }
    if now_secs().saturating_sub(e.fetched_at) > CACHE_TTL_SECS {
        let _ = fs::remove_file(&path);
        return None;
    }
    if e.extract.trim().is_empty() {
        return None;
    }
    if e.term.len() > CACHE_ENTRY_MAX_BYTES || e.extract.len() > CACHE_ENTRY_MAX_BYTES {
        let _ = fs::remove_file(&path);
        return None;
    }
    // Planted cache files must not turn the preview into a generic URL
    // fetcher: drop oversized or off-host media, keep the text.
    let mut e = e;
    if e.image_url
        .as_deref()
        .is_some_and(|u| u.len() > 512 || !is_allowed_image_url(u))
    {
        e.image_url = None;
    }
    if e.page_url.as_deref().is_some_and(|u| {
        u.len() > 512
            || !(u.starts_with("https://en.wikipedia.org/wiki/")
                || u.starts_with("http://en.wikipedia.org/wiki/"))
    }) {
        e.page_url = None;
    }
    if e.description.as_deref().is_some_and(|d| d.len() > 256) {
        e.description = None;
    }
    if let Ok(mut g) = mem_ok().lock() {
        g.insert(key.to_string(), e.clone());
    }
    Some(e)
}

fn cache_put(key: &str, term: &str, hit: &DefineHit) {
    let dir = cache_dir();
    if dir.as_os_str().is_empty() {
        return;
    }
    let _ = fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    if !cache_dir_trusted(&dir) {
        return;
    }
    // Truncate the extract before persisting: oversized text must not linger.
    let extract: String = hit.extract.chars().take(CACHE_ENTRY_MAX_BYTES).collect();
    if term.len() > CACHE_ENTRY_MAX_BYTES || extract.len() > CACHE_ENTRY_MAX_BYTES {
        return;
    }
    // Media is metadata, not body text: tight caps + host allowlist so a
    // planted entry cannot smuggle an arbitrary fetch target to the preview.
    let image_url = hit
        .image_url
        .as_deref()
        .filter(|u| u.len() <= 512 && is_allowed_image_url(u))
        .map(|s| s.to_string());
    let page_url = hit
        .page_url
        .as_deref()
        .filter(|u| {
            u.len() <= 512
                && (u.starts_with("https://en.wikipedia.org/wiki/")
                    || u.starts_with("http://en.wikipedia.org/wiki/"))
        })
        .map(|s| s.to_string());
    let description = hit
        .description
        .as_deref()
        .map(|d| d.trim().chars().take(256).collect::<String>())
        .filter(|s| !s.is_empty());
    let e = CacheEntry {
        term: term.into(),
        title: hit.title.clone(),
        extract,
        source: hit.source.clone(),
        fetched_at: now_secs(),
        image_url,
        page_url,
        description,
    };
    if let Ok(mut g) = mem_ok().lock() {
        g.insert(key.to_string(), e.clone());
        const MAX_MEM: usize = 256;
        if g.len() > MAX_MEM {
            let mut keys: Vec<(&str, u64)> =
                g.iter().map(|(k, v)| (k.as_str(), v.fetched_at)).collect();
            keys.sort_unstable_by_key(|(_, ts)| *ts);
            let remove_n = g.len() - MAX_MEM;
            let to_remove: Vec<String> = keys
                .into_iter()
                .take(remove_n)
                .map(|(k, _)| k.to_string())
                .collect();
            for k in to_remove {
                g.remove(&k);
            }
        }
    }
    if let Ok(data) = serde_json::to_string(&e) {
        let path = cache_path(key);
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(&tmp, path);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
    maybe_sweep_cache(&dir);
}

fn fail_get(key: &str) -> Option<String> {
    let Ok(mut g) = mem_fail().lock() else {
        return None;
    };
    let (term, at) = g.get(key)?.clone();
    if now_secs().saturating_sub(at) > FAIL_CACHE_SECS {
        g.remove(key);
        return None;
    }
    Some(term)
}

fn fail_put(key: &str, term: &str) {
    if let Ok(mut g) = mem_fail().lock() {
        let now = now_secs();
        g.insert(key.to_string(), (term.to_string(), now));
        const MAX_FAIL: usize = 64;
        if g.len() > MAX_FAIL {
            let mut keys: Vec<(u64, String)> =
                g.iter().map(|(k, (_, at))| (*at, k.clone())).collect();
            keys.sort_by_key(|(ts, _)| *ts);
            let remove_n = g.len() - MAX_FAIL;
            for (_, k) in keys.into_iter().take(remove_n) {
                g.remove(&k);
            }
        }
    }
}

fn fail_clear(key: &str) {
    if let Ok(mut g) = mem_fail().lock() {
        g.remove(key);
    }
}

fn maybe_sweep_cache(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, u64, u64)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .map(|p| {
            let meta = fs::metadata(&p).ok();
            let sz = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (p, sz, mtime)
        })
        .collect();
    const MAX: usize = 500;
    const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;
    let total: u64 = files.iter().map(|(_, s, _)| *s).sum();
    if files.len() <= MAX && total <= MAX_TOTAL_BYTES {
        return;
    }
    files.sort_unstable_by_key(|(_, _, mtime)| *mtime);
    let mut remove_n = files.len().saturating_sub(MAX);
    let mut bytes_after = total;
    if total > MAX_TOTAL_BYTES {
        for (i, (_, sz, _)) in files.iter().enumerate() {
            if bytes_after <= MAX_TOTAL_BYTES {
                break;
            }
            remove_n = remove_n.max(i + 1);
            bytes_after = bytes_after.saturating_sub(*sz);
        }
    }
    for (p, _, _) in files.into_iter().take(remove_n) {
        let _ = fs::remove_file(p);
    }
}

fn normalized_key(term: &str) -> String {
    term.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cache_key(term: &str) -> String {
    // Case-insensitive: `SIMD` and `simd` are one article.
    // `v2` namespace: the image upgrade refetches pre-image cache entries
    // once (their recompute check below fails and the file is dropped).
    let norm = format!("v2:{}", normalized_key(term).to_lowercase());
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in norm.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DefineConfig;

    fn cfg() -> DefineConfig {
        DefineConfig::default()
    }

    fn term_of(q: &str) -> Option<String> {
        parse_term(q).map(|(t, _)| t)
    }

    #[test]
    fn question_shapes_parse() {
        assert_eq!(term_of("what does SIMD mean").as_deref(), Some("SIMD"));
        assert_eq!(term_of("What Does AVX Mean").as_deref(), Some("AVX"));
        assert_eq!(term_of("what do GPUs mean").as_deref(), Some("GPUs"));
        assert_eq!(term_of("what is SIMD").as_deref(), Some("SIMD"));
        assert_eq!(term_of("what are GPUs").as_deref(), Some("GPUs"));
        assert_eq!(term_of("what's SIMD?").as_deref(), Some("SIMD"));
        assert_eq!(term_of("whats AVX").as_deref(), Some("AVX"));
        assert_eq!(term_of("SIMD full form").as_deref(), Some("SIMD"));
        assert_eq!(term_of("full form of AVX").as_deref(), Some("AVX"));
        assert_eq!(term_of("SIMD stands for").as_deref(), Some("SIMD"));
        assert_eq!(term_of("what does CPU stand for").as_deref(), Some("CPU"));
        assert_eq!(term_of("meaning of SIMD").as_deref(), Some("SIMD"));
        assert_eq!(term_of("SIMD meaning").as_deref(), Some("SIMD"));
        assert_eq!(term_of("SIMD means").as_deref(), Some("SIMD"));
        assert_eq!(term_of("define SIMD").as_deref(), Some("SIMD"));
    }

    #[test]
    fn forced_prefixes_flagged() {
        let (t, forced) = parse_term("def SIMD").expect("term");
        assert_eq!(t, "SIMD");
        assert!(forced);
        let (t, forced) = parse_term("dict gyroscope").expect("term");
        assert_eq!(t, "gyroscope");
        assert!(forced);
        let (_, forced) = parse_term("what does SIMD mean").expect("term");
        assert!(!forced);
    }

    #[test]
    fn incomplete_and_junk_rejected() {
        for q in [
            "",
            "?",
            "what",
            "what does",
            "what does  mean",
            "what is",
            "define",
            "def",
            "meaning",
            "means",
            "full form",
            "full form of",
            "firefox",
            "2+2",
            "100 km to mi",
            "*.md",
            "/usr/bin",
            "notes in glassbox",
        ] {
            assert!(parse_term(q).is_none(), "must reject {q:?}");
            assert!(!is_define_query(q, &cfg()), "must not handle {q:?}");
        }
    }

    #[test]
    fn does_not_steal_paths_or_math() {
        assert!(parse_term("what is /usr/bin").is_none());
        assert!(parse_term("define a/b").is_none());
        assert!(parse_term("what is 2*3").is_none());
    }

    #[test]
    fn max_chars_gates_long_terms() {
        let mut c = cfg();
        c.max_chars = 3;
        assert!(!is_define_query("what does SIMD mean", &c));
        assert!(is_define_query("what does AVX mean", &c));
    }

    #[test]
    fn cache_key_case_insensitive_and_stable() {
        assert_eq!(cache_key("SIMD"), cache_key("simd"));
        assert_eq!(cache_key("a  b"), cache_key("a b"));
        assert_ne!(cache_key("SIMD"), cache_key("AVX"));
    }

    #[test]
    fn ok_row_is_one_line_snippet_with_full_copy() {
        let extract = "The International Space Station is a big station orbiting Earth with many more words following to push past the snippet budget for sure, plus extra trailing words.";
        let r = ok_result("iss", "International Space Station", extract, "Wikipedia");
        assert_eq!(r.kind, ResultKind::Define);
        assert!(r.conversion.is_none(), "define never renders a card");
        assert_eq!(r.title, "International Space Station · Wikipedia");
        assert!(r.subtitle.chars().count() <= 140, "{}", r.subtitle);
        assert!(r.subtitle.ends_with('…'), "{}", r.subtitle);
        match &r.action {
            Action::Copy(t) => assert_eq!(t, &collapse_ws(extract), "{t}"),
            _ => panic!("ok must copy the full extract"),
        }
    }

    #[test]
    fn snippet_short_text_untouched() {
        assert_eq!(snippet("short text"), "short text");
    }

    #[test]
    fn pending_row_is_define_kind() {
        let r = pending_result("iss");
        assert_eq!(r.kind, ResultKind::Define);
        assert!(r.conversion.is_none());
        assert!(is_pending_result(&r));
    }

    #[test]
    fn wikipedia_parses_and_rejects_disambiguation() {
        let body = br#"{"type":"standard","title":"SIMD","extract":"Single instruction, multiple data (SIMD) is a type of parallel processing.","thumbnail":{"source":"https://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/SIMD.svg/330px-SIMD.svg.png"},"originalimage":{"source":"https://upload.wikimedia.org/wikipedia/commons/a/ab/SIMD.svg"},"description":"Parallel computing term","content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/SIMD"}}}"#;
        let hit = parse_wikipedia_body(body).expect("parse");
        assert_eq!(hit.title, "SIMD");
        assert!(hit.extract.contains("parallel processing"));
        assert_eq!(hit.source, "Wikipedia");
        assert_eq!(
            hit.image_url.as_deref(),
            Some("https://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/SIMD.svg/330px-SIMD.svg.png")
        );
        assert_eq!(
            hit.page_url.as_deref(),
            Some("https://en.wikipedia.org/wiki/SIMD")
        );
        assert_eq!(hit.description.as_deref(), Some("Parallel computing term"));
        // Off-host thumbnails are dropped, text still parses.
        let evil = br#"{"type":"standard","title":"SIMD","extract":"text here","thumbnail":{"source":"https://evil.example/x.png"},"content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/SIMD"}}}"#;
        let hit = parse_wikipedia_body(evil).expect("parse");
        assert!(hit.image_url.is_none());
        let dis = br#"{"type":"disambiguation","title":"AVX","extract":"AVX may refer to:"}"#;
        assert!(parse_wikipedia_body(dis).is_err());
        let empty = br#"{"type":"standard","title":"X"}"#;
        assert!(parse_wikipedia_body(empty).is_err());
    }

    #[test]
    fn ddg_parses_abstract() {
        let body = br#"{"Heading":"SIMD","AbstractText":"SIMD is parallel processing.","AbstractSource":"Wikipedia"}"#;
        let hit = parse_ddg_body(body).expect("parse");
        assert_eq!(hit.title, "SIMD");
        assert!(hit.extract.contains("parallel"));
        assert_eq!(hit.source, "Wikipedia");
        assert!(hit.image_url.is_none());
        assert!(parse_ddg_body(br#"{"AbstractText":""}"#).is_err());
    }

    #[test]
    fn fail_row_opens_web_search() {
        let r = fail_result("SIMD");
        assert_eq!(r.kind, ResultKind::Command);
        match &r.action {
            Action::OpenUrl(u) => assert!(u.contains("SIMD"), "{u}"),
            _ => panic!("fail must carry OpenUrl"),
        }
    }

    #[test]
    fn pending_id() {
        assert!(is_pending_result(&pending_result("SIMD")));
        assert_eq!(pending_result("SIMD").kind, ResultKind::Define);
    }

    #[test]
    fn single_char_terms_skip_network_and_rows() {
        // `define b` must not own the query: no row, no worker.
        let provider =
            DefineProvider::new(std::sync::Arc::new(crate::config::ConfigStore::with_path(
                {
                    let mut c = crate::config::HarkConfig::default();
                    c.define.enabled = true;
                    c
                },
                std::env::temp_dir().join("hark-define-min-test.json"),
            )));
        assert!(provider.search("define b").is_empty());
        assert!(!provider.needs_network("define b"));
        assert!(provider.needs_network("define AI"));
    }

    #[test]
    fn ok_lookup_round_trips_media() {
        let key = cache_key("Black hole");
        cache_put(
            &key,
            "Black hole",
            &DefineHit {
                title: "Black hole".into(),
                extract: "A black hole is compact.".into(),
                source: "Wikipedia".into(),
                image_url: Some(
                    "https://upload.wikimedia.org/wikipedia/commons/thumb/x.jpg".into(),
                ),
                page_url: Some("https://en.wikipedia.org/wiki/Black_hole".into()),
                description: Some("Compact astronomical body".into()),
            },
        );
        let id = format!("define:{key}");
        let r = ok_result(
            "Black hole",
            "Black hole",
            "A black hole is compact.",
            "Wikipedia",
        );
        assert_eq!(r.id, id);
        assert!(is_ok_result(&r));
        assert!(!is_ok_result(&pending_result("Black hole")));
        let media = lookup_media_by_id(&id).expect("media");
        assert!(media.image_url.contains("upload.wikimedia.org"));
        assert_eq!(
            media.page_url.as_deref(),
            Some("https://en.wikipedia.org/wiki/Black_hole")
        );
        // Pending / malformed ids never resolve.
        assert!(lookup_media_by_id("define:pending:deadbeef").is_none());
        assert!(lookup_media_by_id("define:xyz").is_none());
    }
}
