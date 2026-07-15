//! Translate-on-paste provider.
//!
//! Fast path (UI thread): detection + disk cache only.
//! Network path: `search_network` on a worker thread (never blocks GTK).
//! When `TranslateConfig.enabled` is false: zero I/O.

use crate::config::{ConfigStore, TranslateConfig};
use crate::providers::files::{is_path_glob_query, is_scoped_file_query};
use crate::providers::{Action, ConversionView, Provider, ResultKind, SearchResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const TRANSLATE_SCORE: i64 = 100_000;
const TRANSLATE_PENDING_SCORE: i64 = 95_000;
const TRANSLATE_FAIL_SCORE: i64 = 80_000;
const CACHE_TTL_SECS: u64 = 14 * 24 * 3600;
/// Avoid hammering free APIs / spinning workers on repeated failures.
const FAIL_CACHE_SECS: u64 = 90;
const PENDING_PREFIX: &str = "translate:pending:";

/// Process-local success cache (disk is the durable layer).
fn mem_ok() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static M: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Negative cache: key → (message, fetched_at).
fn mem_fail() -> &'static Mutex<HashMap<String, (String, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (String, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct TranslateProvider {
    config: Arc<ConfigStore>,
}

impl TranslateProvider {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self { config }
    }

    pub fn cfg(&self) -> TranslateConfig {
        self.config.get().translate
    }

    pub fn is_enabled(&self) -> bool {
        self.cfg().enabled
    }

    pub fn should_handle(&self, query: &str) -> bool {
        let cfg = self.cfg();
        if !cfg.enabled {
            return false;
        }
        is_translate_query(query, &cfg)
    }

    /// True when UI should spawn a worker: enabled, matches, not already cached.
    pub fn needs_network(&self, query: &str) -> bool {
        let cfg = self.cfg();
        if !cfg.enabled || !is_translate_query(query, &cfg) {
            return false;
        }
        let Some((text, source, target)) = parse_job(query, &cfg) else {
            return false;
        };
        if text.chars().count() > cfg.max_chars {
            return false;
        }
        let key = cache_key(&source, &target, &text);
        if cache_get(&key).is_some() {
            return false;
        }
        // Recent failure: show soft-fail from UI path, no new worker.
        if fail_get(&key).is_some() {
            return false;
        }
        true
    }

    /// Blocking network translate (worker thread only).
    pub fn search_network(&self, query: &str) -> Vec<SearchResult> {
        let cfg = self.cfg();
        if !cfg.enabled {
            return Vec::new();
        }
        if !is_translate_query(query, &cfg) {
            return Vec::new();
        }
        let Some((text, source, target)) = parse_job(query, &cfg) else {
            return Vec::new();
        };
        if text.chars().count() > cfg.max_chars {
            return vec![fail_result(
                &text,
                &source,
                &target,
                &format!("Too long (max {} characters)", cfg.max_chars),
            )];
        }
        let key = cache_key(&source, &target, &text);
        if let Some(hit) = cache_get(&key) {
            return vec![ok_result(&text, &hit.translated, &source, &target, "cache")];
        }
        if let Some(msg) = fail_get(&key) {
            return vec![fail_result(&text, &source, &target, &msg)];
        }
        match translate_http(&text, &source, &target, &cfg) {
            Ok((translated, backend)) => {
                cache_put(&key, &text, &source, &target, &translated);
                fail_clear(&key);
                vec![ok_result(&text, &translated, &source, &target, &backend)]
            }
            Err(msg) => {
                fail_put(&key, &msg);
                vec![fail_result(&text, &source, &target, &msg)]
            }
        }
    }
}

impl Provider for TranslateProvider {
    /// UI-thread safe: cache hit, recent fail, or "Translating…" placeholder. **No curl.**
    fn search(&self, query: &str) -> Vec<SearchResult> {
        let cfg = self.cfg();
        if !cfg.enabled {
            return Vec::new();
        }
        if !is_translate_query(query, &cfg) {
            return Vec::new();
        }
        let Some((text, source, target)) = parse_job(query, &cfg) else {
            return Vec::new();
        };
        if text.chars().count() > cfg.max_chars {
            return vec![fail_result(
                &text,
                &source,
                &target,
                &format!("Too long (max {} characters)", cfg.max_chars),
            )];
        }
        let key = cache_key(&source, &target, &text);
        if let Some(hit) = cache_get(&key) {
            return vec![ok_result(&text, &hit.translated, &source, &target, "cache")];
        }
        if let Some(msg) = fail_get(&key) {
            return vec![fail_result(&text, &source, &target, &msg)];
        }
        vec![pending_result(&text, &source, &target)]
    }
}

fn parse_job(query: &str, cfg: &TranslateConfig) -> Option<(String, String, String)> {
    let (forced, text) = strip_translate_prefix(query.trim());
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let source = guess_source_lang(text, forced);
    let target = cfg.target_lang.clone();
    Some((text.to_string(), source, target))
}

fn ok_result(
    source_text: &str,
    translated: &str,
    source: &str,
    target: &str,
    backend: &str,
) -> SearchResult {
    let src_b = source.to_ascii_uppercase();
    let tgt_b = target.to_ascii_uppercase();
    SearchResult {
        id: format!("translate:{}", cache_key(source, target, source_text)),
        title: translated.to_string(),
        subtitle: format!("{src_b} → {tgt_b} · {backend}"),
        kind: ResultKind::Conversion,
        score: TRANSLATE_SCORE,
        icon: Some("preferences-desktop-locale".into()),
        action: Action::Copy(translated.to_string()),
        conversion: Some(ConversionView {
            left_title: source_text.to_string(),
            left_badge: src_b,
            right_title: translated.to_string(),
            right_badge: tgt_b,
        }),
    }
}

fn pending_result(source_text: &str, source: &str, target: &str) -> SearchResult {
    let src_b = source.to_ascii_uppercase();
    let tgt_b = target.to_ascii_uppercase();
    SearchResult {
        id: format!("{PENDING_PREFIX}{}", simple_hash(source_text)),
        title: "Translating…".into(),
        subtitle: format!("{src_b} → {tgt_b} · fetching"),
        kind: ResultKind::Conversion,
        score: TRANSLATE_PENDING_SCORE,
        icon: Some("preferences-desktop-locale".into()),
        action: Action::Copy(source_text.to_string()),
        conversion: Some(ConversionView {
            left_title: source_text.to_string(),
            left_badge: src_b,
            right_title: "…".into(),
            right_badge: tgt_b,
        }),
    }
}

fn fail_result(source_text: &str, source: &str, target: &str, msg: &str) -> SearchResult {
    let src_b = source.to_ascii_uppercase();
    let tgt_b = target.to_ascii_uppercase();
    // Keep subtitle short so the conversion card stays readable.
    let short = if msg.chars().count() > 72 {
        let t: String = msg.chars().take(69).collect();
        format!("{t}…")
    } else {
        msg.to_string()
    };
    SearchResult {
        id: format!("translate-fail:{}", simple_hash(source_text)),
        title: "Translation unavailable".into(),
        subtitle: short,
        kind: ResultKind::Conversion,
        score: TRANSLATE_FAIL_SCORE,
        icon: Some("dialog-warning".into()),
        action: Action::Copy(source_text.to_string()),
        conversion: Some(ConversionView {
            left_title: source_text.to_string(),
            left_badge: src_b,
            right_title: "—".into(),
            right_badge: tgt_b,
        }),
    }
}

pub fn is_pending_result(r: &SearchResult) -> bool {
    r.id.starts_with(PENDING_PREFIX)
}

// ── Detection ───────────────────────────────────────────────────────────────

pub fn strip_translate_prefix(query: &str) -> (bool, &str) {
    let q = query.trim();
    let lower = q.to_ascii_lowercase();
    for prefix in ["translate ", "tr ", "译 "] {
        let plen = prefix.len();
        if prefix.chars().all(|c| c.is_ascii()) {
            if lower.starts_with(prefix) {
                return (true, q[plen..].trim());
            }
        } else if q.starts_with(prefix) {
            return (true, q[plen..].trim());
        }
    }
    (false, q)
}

pub fn is_translate_query(query: &str, cfg: &TranslateConfig) -> bool {
    if !cfg.enabled {
        return false;
    }
    let q = query.trim();
    if q.is_empty() {
        return false;
    }

    let (forced, text) = strip_translate_prefix(q);
    if text.is_empty() {
        return false;
    }
    if text.chars().count() > cfg.max_chars {
        return forced || looks_like_translatable_script(text);
    }

    if is_path_glob_query(q) || is_scoped_file_query(q) {
        return false;
    }
    if text.starts_with('/')
        || text.starts_with("~/")
        || text.starts_with("./")
        || (text.contains('*')
            && (text.contains('/') || text.starts_with('*') || text.starts_with('.')))
    {
        return false;
    }

    if forced {
        return true;
    }
    if !cfg.auto_detect {
        return false;
    }
    looks_like_translatable_script(text)
}

pub fn looks_like_translatable_script(text: &str) -> bool {
    let mut total = 0u32;
    let mut cjk = 0u32;
    for ch in text.chars() {
        if ch.is_whitespace() || ch.is_ascii_punctuation() {
            continue;
        }
        total += 1;
        if is_cjk_like(ch) {
            cjk += 1;
        }
    }
    if cjk == 0 {
        return false;
    }
    if cjk >= 2 {
        return true;
    }
    total > 0 && (cjk as f32 / total as f32) >= 0.5
}

fn is_cjk_like(ch: char) -> bool {
    let c = ch as u32;
    (0x4E00..=0x9FFF).contains(&c)
        || (0x3400..=0x4DBF).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0x3040..=0x309F).contains(&c)
        || (0x30A0..=0x30FF).contains(&c)
        || (0xAC00..=0xD7AF).contains(&c)
        || (0x3000..=0x303F).contains(&c)
}

fn guess_source_lang(text: &str, _forced: bool) -> String {
    let mut han = 0u32;
    let mut hira_kata = 0u32;
    let mut hangul = 0u32;
    for ch in text.chars() {
        let c = ch as u32;
        if (0x4E00..=0x9FFF).contains(&c) || (0x3400..=0x4DBF).contains(&c) {
            han += 1;
        } else if (0x3040..=0x309F).contains(&c) || (0x30A0..=0x30FF).contains(&c) {
            hira_kata += 1;
        } else if (0xAC00..=0xD7AF).contains(&c) {
            hangul += 1;
        }
    }
    if hira_kata > 0 && hira_kata >= han {
        return "ja".into();
    }
    if hangul > 0 && hangul >= han {
        return "ko".into();
    }
    if han > 0 {
        return "zh-CN".into();
    }
    "en".into()
}

// ── HTTP (worker only) ──────────────────────────────────────────────────────

fn translate_http(
    text: &str,
    source: &str,
    target: &str,
    cfg: &TranslateConfig,
) -> Result<(String, String), String> {
    if !cfg.endpoint.is_empty() {
        return libretranslate(text, source, target, cfg);
    }
    // Free backends in parallel — first success wins; bound wall time ~2s.
    free_backends_race(text, source, target)
}

fn free_backends_race(
    text: &str,
    source: &str,
    target: &str,
) -> Result<(String, String), String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<(String, String), String>>();
    let t1 = text.to_string();
    let s1 = source.to_string();
    let g1 = target.to_string();
    let tx1 = tx.clone();
    thread::spawn(move || {
        let _ = tx1.send(google_gtx(&t1, &s1, &g1));
    });
    let t2 = text.to_string();
    let s2 = source.to_string();
    let g2 = target.to_string();
    thread::spawn(move || {
        let _ = tx.send(mymemory(&t2, &s2, &g2));
    });

    let mut errors = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2200);
    for _ in 0..2 {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            break;
        }
        match rx.recv_timeout(remain) {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(e)) => errors.push(e),
            Err(_) => break,
        }
    }
    if errors.is_empty() {
        Err("Offline or timed out".into())
    } else {
        // Prefer a short human message when every backend is unreachable.
        let joined = errors.join("; ");
        if joined.to_ascii_lowercase().contains("unreachable") {
            Err("Offline or blocked · check network".into())
        } else {
            Err(joined)
        }
    }
}

fn libretranslate(
    text: &str,
    source: &str,
    target: &str,
    cfg: &TranslateConfig,
) -> Result<(String, String), String> {
    let url = format!("{}/translate", cfg.endpoint.trim_end_matches('/'));
    // LibreTranslate often wants short codes: zh-CN → zh
    let src = short_lang(source);
    let tgt = short_lang(target);
    let mut body = serde_json::json!({
        "q": text,
        "source": src,
        "target": tgt,
        "format": "text",
    });
    if let Some(key) = &cfg.api_key {
        body["api_key"] = serde_json::Value::String(key.clone());
    }
    let payload = body.to_string();

    let mut child = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "1",
            "--max-time",
            "2",
            "-H",
            "Content-Type: application/json",
            "-X",
            "POST",
            "--data-binary",
            "@-",
            &url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "curl not available".to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|_| "translate request failed".to_string())?;
    if !out.status.success() {
        return Err("LibreTranslate error (check endpoint)".into());
    }

    #[derive(Deserialize)]
    struct LtResp {
        #[serde(default)]
        translatedtext: String,
        #[serde(default, rename = "translatedText")]
        translated_text: String,
    }
    let resp: LtResp = serde_json::from_slice(&out.stdout)
        .map_err(|_| "Bad LibreTranslate response".to_string())?;
    let translated = if !resp.translated_text.is_empty() {
        resp.translated_text
    } else {
        resp.translatedtext
    };
    if translated.trim().is_empty() {
        return Err("Empty LibreTranslate response".into());
    }
    Ok((translated, "LibreTranslate".into()))
}

fn google_gtx(text: &str, source: &str, target: &str) -> Result<(String, String), String> {
    // Unofficial free endpoint (same family as many OSS clients). No API key.
    let sl = if source == "en" {
        "auto".to_string()
    } else {
        short_lang(source)
    };
    let tl = short_lang(target);
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "1",
            "--max-time",
            "2",
            "-G",
            "https://translate.googleapis.com/translate_a/single",
            "--data-urlencode",
            "client=gtx",
            "--data-urlencode",
            &format!("sl={sl}"),
            "--data-urlencode",
            &format!("tl={tl}"),
            "--data-urlencode",
            "dt=t",
            "--data-urlencode",
            &format!("q={text}"),
        ])
        .output()
        .map_err(|_| "curl not available".to_string())?;
    if !out.status.success() {
        return Err("Google translate unreachable".into());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| "Bad Google translate response".to_string())?;
    // Response: [[["Hello world","你好世界",...],...],...]
    let mut translated = String::new();
    if let Some(arr) = v.as_array().and_then(|a| a.first()).and_then(|x| x.as_array()) {
        for part in arr {
            if let Some(s) = part.as_array().and_then(|p| p.first()).and_then(|t| t.as_str()) {
                translated.push_str(s);
            }
        }
    }
    if translated.trim().is_empty() {
        return Err("Empty Google translate response".into());
    }
    Ok((translated, "Google".into()))
}

fn mymemory(text: &str, source: &str, target: &str) -> Result<(String, String), String> {
    let src = match short_lang(source).as_str() {
        "zh" => "zh-CN".to_string(),
        s => s.to_string(),
    };
    let tgt = short_lang(target);
    let langpair = format!("{src}|{tgt}");
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "1",
            "--max-time",
            "2",
            "-G",
            "https://api.mymemory.translated.net/get",
            "--data-urlencode",
            &format!("q={text}"),
            "--data-urlencode",
            &format!("langpair={langpair}"),
        ])
        .output()
        .map_err(|_| "curl not available".to_string())?;
    if !out.status.success() {
        return Err("MyMemory unreachable".into());
    }

    #[derive(Deserialize)]
    struct MmResp {
        response_data: Option<MmData>,
    }
    #[derive(Deserialize)]
    struct MmData {
        translated_text: Option<String>,
    }
    let resp: MmResp = serde_json::from_slice(&out.stdout)
        .map_err(|_| "Bad MyMemory response".to_string())?;
    let translated = resp
        .response_data
        .and_then(|d| d.translated_text)
        .unwrap_or_default();
    let upper = translated.to_ascii_uppercase();
    if translated.trim().is_empty() || upper.contains("INVALID SOURCE LANGUAGE") {
        return Err("MyMemory rejected language pair".into());
    }
    // Strip free-tier warning prefix if present, keep the rest.
    let cleaned = if let Some(idx) = translated.find("QUERY LENGTH LIMIT") {
        // Entirely a warning — fail
        let _ = idx;
        return Err("MyMemory rate/length limit".into());
    } else {
        translated.trim().to_string()
    };
    Ok((cleaned, "MyMemory".into()))
}

fn short_lang(code: &str) -> String {
    let c = code.trim().to_ascii_lowercase();
    if c.starts_with("zh") {
        return "zh".into();
    }
    c.split('-').next().unwrap_or("en").to_string()
}

// ── Cache ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    source: String,
    target: String,
    q: String,
    translated: String,
    fetched_at: u64,
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("blink/translate")
}

fn cache_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.json"))
}

fn cache_get(key: &str) -> Option<CacheEntry> {
    // Hot path: process memory (UI thread).
    if let Ok(g) = mem_ok().lock() {
        if let Some(e) = g.get(key) {
            if now_secs().saturating_sub(e.fetched_at) <= CACHE_TTL_SECS
                && !e.translated.trim().is_empty()
            {
                return Some(e.clone());
            }
        }
    }
    let data = fs::read_to_string(cache_path(key)).ok()?;
    let e: CacheEntry = serde_json::from_str(&data).ok()?;
    if now_secs().saturating_sub(e.fetched_at) > CACHE_TTL_SECS {
        let _ = fs::remove_file(cache_path(key));
        return None;
    }
    if e.translated.trim().is_empty() {
        return None;
    }
    if let Ok(mut g) = mem_ok().lock() {
        g.insert(key.to_string(), e.clone());
    }
    Some(e)
}

fn cache_put(key: &str, q: &str, source: &str, target: &str, translated: &str) {
    let dir = cache_dir();
    let _ = fs::create_dir_all(&dir);
    let e = CacheEntry {
        source: source.into(),
        target: target.into(),
        q: q.into(),
        translated: translated.into(),
        fetched_at: now_secs(),
    };
    if let Ok(mut g) = mem_ok().lock() {
        g.insert(key.to_string(), e.clone());
        // Bound memory cache size.
        if g.len() > 256 {
            // Drop arbitrary older-ish half by clearing when huge (simple).
            if g.len() > 400 {
                g.clear();
                g.insert(key.to_string(), e.clone());
            }
        }
    }
    if let Ok(data) = serde_json::to_string(&e) {
        let path = cache_path(key);
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(tmp, path);
        }
    }
    maybe_sweep_cache(&dir);
}

fn fail_get(key: &str) -> Option<String> {
    let Ok(mut g) = mem_fail().lock() else {
        return None;
    };
    let (msg, at) = g.get(key)?.clone();
    if now_secs().saturating_sub(at) > FAIL_CACHE_SECS {
        g.remove(key);
        return None;
    }
    Some(msg)
}

fn fail_put(key: &str, msg: &str) {
    if let Ok(mut g) = mem_fail().lock() {
        g.insert(key.to_string(), (msg.to_string(), now_secs()));
        if g.len() > 128 {
            g.clear();
            g.insert(key.to_string(), (msg.to_string(), now_secs()));
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
    let mut files: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    const MAX: usize = 500;
    if files.len() <= MAX {
        return;
    }
    files.sort_by_key(|p| {
        fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    let remove_n = files.len() - MAX;
    for p in files.into_iter().take(remove_n) {
        let _ = fs::remove_file(p);
    }
}

fn cache_key(source: &str, target: &str, text: &str) -> String {
    let norm = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let payload = format!("{source}|{target}|{norm}");
    simple_hash(&payload)
}

fn simple_hash(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
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
    use crate::config::TranslateConfig;

    fn cfg_auto() -> TranslateConfig {
        TranslateConfig {
            enabled: true,
            auto_detect: true,
            ..TranslateConfig::default()
        }
    }

    fn cfg_prefix_only() -> TranslateConfig {
        TranslateConfig {
            enabled: true,
            auto_detect: false,
            ..TranslateConfig::default()
        }
    }

    fn cfg_off() -> TranslateConfig {
        TranslateConfig {
            enabled: false,
            auto_detect: true,
            ..TranslateConfig::default()
        }
    }

    #[test]
    fn disabled_never_matches() {
        assert!(!is_translate_query("你好", &cfg_off()));
        assert!(!is_translate_query("tr 你好", &cfg_off()));
    }

    #[test]
    fn cjk_auto_detect() {
        let c = cfg_auto();
        assert!(is_translate_query("你好", &c));
        assert!(is_translate_query("你好世界", &c));
        assert!(!is_translate_query("firefox", &c));
    }

    #[test]
    fn prefix_forced() {
        let c = cfg_prefix_only();
        assert!(is_translate_query("tr 你好", &c));
        assert!(is_translate_query("translate hello world", &c));
        assert!(!is_translate_query("你好", &c));
    }

    #[test]
    fn does_not_steal_paths_or_files() {
        let c = cfg_auto();
        assert!(!is_translate_query("*.md", &c));
        assert!(!is_translate_query("optimization.md in glassbox", &c));
    }

    #[test]
    fn strip_prefix() {
        assert_eq!(strip_translate_prefix("tr 你好").1, "你好");
        assert_eq!(strip_translate_prefix("translate foo").1, "foo");
    }

    #[test]
    fn cache_key_stable() {
        let a = cache_key("zh-CN", "en", "你好  世界");
        let b = cache_key("zh-CN", "en", "你好 世界");
        assert_eq!(a, b);
    }

    #[test]
    fn guess_langs() {
        assert_eq!(guess_source_lang("你好", false), "zh-CN");
        assert_eq!(guess_source_lang("こんにちは", false), "ja");
        assert_eq!(guess_source_lang("안녕하세요", false), "ko");
    }

    #[test]
    fn pending_id() {
        let r = pending_result("你好", "zh-CN", "en");
        assert!(is_pending_result(&r));
    }
}
