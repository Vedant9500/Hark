//! Translate-on-paste provider.
//!
//! Fast path (UI thread): detection + **process-memory** cache / fail only (no disk).
//! Network path: `search_network` on a worker thread — may read durable disk cache,
//! then HTTP (never blocks GTK).
//! When `TranslateConfig.enabled` is false: zero I/O.

use crate::config::{ConfigStore, TranslateConfig};
use crate::providers::files::{is_path_glob_query, is_scoped_file_query};
use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
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
        self.config.with(|c| c.translate.clone())
    }

    pub fn is_enabled(&self) -> bool {
        self.config.with(|c| c.translate.enabled)
    }

    pub fn should_handle(&self, query: &str) -> bool {
        self.config.with(|c| {
            let cfg = &c.translate;
            cfg.enabled && is_translate_query(query, cfg)
        })
    }

    /// Auto-detect CJK (no `tr` prefix) — longer UI debounce; forced prefix stays snappy.
    pub fn is_auto_query(&self, query: &str) -> bool {
        self.config.with(|c| {
            let cfg = &c.translate;
            if !cfg.enabled || !cfg.auto_detect {
                return false;
            }
            let (forced, _) = strip_translate_prefix(query.trim());
            if forced {
                return false;
            }
            is_translate_query(query, cfg)
        })
    }

    /// True when UI should spawn a worker: enabled, matches, not already in **memory** cache.
    /// Disk is checked on the worker (`search_network`) so the UI thread never blocks on FS.
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
        if cache_get_mem(&key).is_some() {
            return false;
        }
        // Recent failure: show soft-fail from UI path, no new worker.
        if fail_get(&key).is_some() {
            return false;
        }
        true
    }

    /// Blocking network translate (worker thread only). May read disk cache.
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
        // Worker path: mem first, then disk (promotes into mem).
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

    /// UI-thread safe: memory cache hit, recent fail, or "Translating…" placeholder.
    /// **No disk I/O** — durable cache is loaded on the worker via `search_network`.
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
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
        if let Some(hit) = cache_get_mem(&key) {
            return vec![ok_result(&text, &hit.translated, &source, &target, "cache")];
        }
        if let Some(msg) = fail_get(&key) {
            return vec![fail_result(&text, &source, &target, &msg)];
        }
        vec![pending_result(&text, &source, &target)]
    }
}

/// Parse query into (text, source_lang, target_lang).
///
/// Forced forms after `tr` / `translate` / `译`:
/// - `tr en zh Hello` → source=en, target=zh, text=Hello
/// - `tr zh en 你好` → source=zh, target=en, text=你好
/// - `tr Hello` / bare non-Latin paste → auto/guessed source, config target
fn parse_job(query: &str, cfg: &TranslateConfig) -> Option<(String, String, String)> {
    let (forced, rest) = strip_translate_prefix(query.trim());
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    if forced {
        if let Some((source, target, text)) = parse_direction_and_text(rest) {
            return Some((text, normalize_lang(&source), normalize_lang(&target)));
        }
        // `tr en zh` with no payload — incomplete direction, not a job yet.
        if incomplete_direction(rest) {
            return None;
        }
    }

    let source = guess_source_lang(rest, forced);
    let mut target = normalize_lang(&cfg.target_lang);
    // Auto paste with target == source (e.g. zh→zh) is useless — flip to English.
    // Forced `tr en en …` keeps the user's explicit pair.
    if !forced && same_primary_lang(&source, &target) {
        target = if is_english_lang(&source) {
            "zh-CN".into()
        } else {
            "en".into()
        };
    }
    Some((rest.to_string(), source, target))
}

/// Two lang codes and nothing else (`tr en zh`).
fn incomplete_direction(rest: &str) -> bool {
    let mut parts = rest.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) if is_lang_code(a) && is_lang_code(b) => true,
        _ => false,
    }
}

/// True when first two whitespace-separated tokens look like language codes
/// and there is remaining text (`en zh Hello world`).
fn parse_direction_and_text(rest: &str) -> Option<(String, String, String)> {
    let mut parts = rest.split_whitespace();
    let a = parts.next()?;
    let b = parts.next()?;
    if !is_lang_code(a) || !is_lang_code(b) {
        return None;
    }
    // Remainder of original string after the two codes (preserve inner spaces).
    let mut idx = 0usize;
    let bytes = rest.as_bytes();
    // skip first code
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    idx += a.len();
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    idx += b.len();
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let text = rest[idx..].trim();
    if text.is_empty() {
        return None;
    }
    Some((a.to_string(), b.to_string(), text.to_string()))
}

/// ISO-ish language tag: `en`, `zh`, `zh-CN`, `zh_cn`, `pt-BR` (2–3 letter primary).
fn is_lang_code(tok: &str) -> bool {
    let t = tok.trim();
    if t.is_empty() || t.len() > 12 {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split(|c| c == '-' || c == '_').collect();
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    let primary = parts[0];
    if primary.len() < 2 || primary.len() > 3 || !primary.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    if let Some(region) = parts.get(1) {
        if region.is_empty()
            || region.len() > 4
            || !region.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return false;
        }
    }
    // Reject common English words mistaken as codes when alone is ok;
    // "to"/"in" are 2 letters — still valid ISO, but `tr to en foo` is rare.
    // Accept all valid-looking tags; `parse_direction` needs *two* consecutive.
    true
}

fn normalize_lang(code: &str) -> String {
    let c = code.trim().to_ascii_lowercase().replace('_', "-");
    if c.is_empty() {
        return "en".into();
    }
    // Canonical common forms + popular aliases users type in `tr xx yy …`
    match c.as_str() {
        "zh" | "zh-cn" | "zh-hans" | "cn" | "chi" | "chinese" => "zh-CN".into(),
        "zh-tw" | "zh-hant" | "zh-hk" | "tw" => "zh-TW".into(),
        "jp" | "jpn" | "japanese" => "ja".into(),
        "kr" | "kor" | "korean" => "ko".into(),
        "iw" | "hebrew" => "he".into(),
        "ua" | "ukr" => "uk".into(),
        "nb" | "nn" | "no" | "nor" => "no".into(),
        "fa" | "per" | "farsi" | "persian" => "fa".into(),
        "ur" | "urd" => "ur".into(),
        "hi" | "hin" | "hindi" => "hi".into(),
        "bn" | "ben" | "bengali" => "bn".into(),
        "ta" | "tam" | "tamil" => "ta".into(),
        "th" | "tha" | "thai" => "th".into(),
        "vi" | "vie" | "vietnamese" => "vi".into(),
        "ar" | "ara" | "arabic" => "ar".into(),
        "ru" | "rus" | "russian" => "ru".into(),
        "es" | "spa" | "spanish" => "es".into(),
        "fr" | "fra" | "fre" | "french" => "fr".into(),
        "de" | "ger" | "deu" | "german" => "de".into(),
        "pt" | "por" | "portuguese" => "pt".into(),
        "pt-br" | "br" => "pt-BR".into(),
        "it" | "ita" | "italian" => "it".into(),
        "tr" | "tur" | "turkish" => "tr".into(),
        "pl" | "pol" | "polish" => "pl".into(),
        "nl" | "dut" | "nld" | "dutch" => "nl".into(),
        "id" | "ind" | "indonesian" => "id".into(),
        "el" | "gre" | "ell" | "greek" => "el".into(),
        "en" | "eng" | "english" => "en".into(),
        other => {
            // Keep primary + optional region uppercased like en, pt-BR
            let mut parts = other.split('-');
            let p = parts.next().unwrap_or("en");
            if let Some(r) = parts.next() {
                format!("{}-{}", p, r.to_ascii_uppercase())
            } else {
                p.to_string()
            }
        }
    }
}

fn primary_lang(code: &str) -> String {
    let n = normalize_lang(code);
    n.split(['-', '_'])
        .next()
        .unwrap_or("en")
        .to_ascii_lowercase()
}

fn same_primary_lang(a: &str, b: &str) -> bool {
    let pa = primary_lang(a);
    let pb = primary_lang(b);
    if pa == "auto" || pb == "auto" {
        return false;
    }
    pa == pb
}

fn is_english_lang(code: &str) -> bool {
    primary_lang(code) == "en"
}

fn lang_badge(code: &str) -> String {
    let c = code.trim();
    if c.eq_ignore_ascii_case("auto") {
        return "AUTO".into();
    }
    // zh-CN → ZH
    let primary = c.split(['-', '_']).next().unwrap_or(c);
    primary.to_ascii_uppercase()
}

fn ok_result(
    source_text: &str,
    translated: &str,
    source: &str,
    target: &str,
    backend: &str,
) -> SearchResult {
    let src_b = lang_badge(source);
    let tgt_b = lang_badge(target);
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
    let src_b = lang_badge(source);
    let tgt_b = lang_badge(target);
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
    let src_b = lang_badge(source);
    let tgt_b = lang_badge(target);
    // Keep messages short so the conversion card stays readable.
    let short = if msg.chars().count() > 72 {
        let t: String = msg.chars().take(69).collect();
        format!("{t}…")
    } else {
        msg.to_string()
    };
    // Show a short reason on the right panel (conversion card hides title/subtitle).
    let right = if short.chars().count() > 28 {
        let t: String = short.chars().take(25).collect();
        format!("{t}…")
    } else {
        short.clone()
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
            right_title: right,
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

/// Auto-detect paste in non-Latin scripts (not Latin app queries like `firefox`).
/// Covers CJK, Cyrillic, Arabic, Indic, Thai, Hebrew, Greek — popular launcher paste cases.
pub fn looks_like_translatable_script(text: &str) -> bool {
    let mut total = 0u32;
    let mut script = 0u32;
    for ch in text.chars() {
        if ch.is_whitespace() || ch.is_ascii_punctuation() {
            continue;
        }
        total += 1;
        if is_translatable_script_char(ch) {
            script += 1;
        }
    }
    if script == 0 {
        return false;
    }
    if script >= 2 {
        return true;
    }
    total > 0 && (script as f32 / total as f32) >= 0.5
}

/// True for scripts we treat as "foreign text paste" (not ASCII Latin).
fn is_translatable_script_char(ch: char) -> bool {
    is_cjk_like(ch)
        || is_cyrillic(ch)
        || is_arabic_script(ch)
        || is_devanagari(ch)
        || is_bengali(ch)
        || is_tamil(ch)
        || is_thai(ch)
        || is_hebrew(ch)
        || is_greek(ch)
}

fn is_cjk_like(ch: char) -> bool {
    let c = ch as u32;
    // Han, CJK ext A, compatibility ideographs, hiragana, katakana, hangul, CJK punctuation
    (0x4E00..=0x9FFF).contains(&c)
        || (0x3400..=0x4DBF).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0x3040..=0x309F).contains(&c)
        || (0x30A0..=0x30FF).contains(&c)
        || (0xAC00..=0xD7AF).contains(&c)
        || (0x3000..=0x303F).contains(&c)
}

fn is_cyrillic(ch: char) -> bool {
    let c = ch as u32;
    (0x0400..=0x04FF).contains(&c) // Cyrillic
        || (0x0500..=0x052F).contains(&c) // Cyrillic Supplement
}

fn is_arabic_script(ch: char) -> bool {
    let c = ch as u32;
    (0x0600..=0x06FF).contains(&c) // Arabic
        || (0x0750..=0x077F).contains(&c) // Arabic Supplement
        || (0x08A0..=0x08FF).contains(&c) // Arabic Extended-A
        || (0xFB50..=0xFDFF).contains(&c) // Arabic Presentation Forms-A
        || (0xFE70..=0xFEFF).contains(&c) // Arabic Presentation Forms-B
}

fn is_devanagari(ch: char) -> bool {
    let c = ch as u32;
    (0x0900..=0x097F).contains(&c)
}

fn is_bengali(ch: char) -> bool {
    let c = ch as u32;
    (0x0980..=0x09FF).contains(&c)
}

fn is_tamil(ch: char) -> bool {
    let c = ch as u32;
    (0x0B80..=0x0BFF).contains(&c)
}

fn is_thai(ch: char) -> bool {
    let c = ch as u32;
    (0x0E00..=0x0E7F).contains(&c)
}

fn is_hebrew(ch: char) -> bool {
    let c = ch as u32;
    (0x0590..=0x05FF).contains(&c)
}

fn is_greek(ch: char) -> bool {
    let c = ch as u32;
    (0x0370..=0x03FF).contains(&c)
}

fn guess_source_lang(text: &str, _forced: bool) -> String {
    let mut han = 0u32;
    let mut hira_kata = 0u32;
    let mut hangul = 0u32;
    let mut cyrillic = 0u32;
    let mut arabic = 0u32;
    let mut devanagari = 0u32;
    let mut bengali = 0u32;
    let mut tamil = 0u32;
    let mut thai = 0u32;
    let mut hebrew = 0u32;
    let mut greek = 0u32;

    for ch in text.chars() {
        let c = ch as u32;
        if (0x4E00..=0x9FFF).contains(&c)
            || (0x3400..=0x4DBF).contains(&c)
            || (0xF900..=0xFAFF).contains(&c)
        {
            han += 1;
        } else if (0x3040..=0x309F).contains(&c) || (0x30A0..=0x30FF).contains(&c) {
            hira_kata += 1;
        } else if (0xAC00..=0xD7AF).contains(&c) {
            hangul += 1;
        } else if is_cyrillic(ch) {
            cyrillic += 1;
        } else if is_arabic_script(ch) {
            arabic += 1;
        } else if is_devanagari(ch) {
            devanagari += 1;
        } else if is_bengali(ch) {
            bengali += 1;
        } else if is_tamil(ch) {
            tamil += 1;
        } else if is_thai(ch) {
            thai += 1;
        } else if is_hebrew(ch) {
            hebrew += 1;
        } else if is_greek(ch) {
            greek += 1;
        }
    }

    // CJK: prefer kana → Japanese, hangul → Korean, else Han → Chinese.
    if hira_kata > 0 && hira_kata >= han {
        return "ja".into();
    }
    if hangul > 0 && hangul >= han {
        return "ko".into();
    }
    if han > 0 {
        return "zh-CN".into();
    }

    // Other scripts: pick the strongest signal (ties break by this order).
    let candidates: [(&str, u32); 8] = [
        ("ru", cyrillic),
        ("ar", arabic),
        ("hi", devanagari),
        ("bn", bengali),
        ("ta", tamil),
        ("th", thai),
        ("he", hebrew),
        ("el", greek),
    ];
    let mut best: Option<(&str, u32)> = None;
    for (lang, n) in candidates {
        if n == 0 {
            continue;
        }
        match best {
            None => best = Some((lang, n)),
            Some((_, bn)) if n > bn => best = Some((lang, n)),
            _ => {}
        }
    }
    if let Some((lang, _)) = best {
        return lang.into();
    }

    // Latin / unknown: free APIs that support it get `auto`.
    "auto".into()
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

fn free_backends_race(text: &str, source: &str, target: &str) -> Result<(String, String), String> {
    // P5: prefer sequential fallback over a concurrent race. Try Google first
    // (bounded by http::TOTAL); only if it fails do we try MyMemory. This avoids
    // orphaning a worker thread that lingers up to the 4s timeout after the first
    // backend already returned, which would pile up under paste storms. The UI
    // layer additionally single-flights translate so only one chain runs at a time.
    match google_gtx(text, source, target) {
        Ok(v) => Ok(v),
        Err(google_err) => {
            let joined = match mymemory(text, source, target) {
                Ok(v) => return Ok(v),
                Err(mem_err) => format!("{google_err}; {mem_err}"),
            };
            // Prefer a short human message when every backend is unreachable.
            let low = joined.to_ascii_lowercase();
            if low.contains("unreachable") || low.contains("timed out") {
                Err("Offline or blocked · check network".into())
            } else {
                Err(joined)
            }
        }
    }
}

fn libretranslate(
    text: &str,
    source: &str,
    target: &str,
    cfg: &TranslateConfig,
) -> Result<(String, String), String> {
    crate::config::validate_translate_endpoint(&cfg.endpoint)
        .map_err(|e| format!("LibreTranslate endpoint: {e}"))?;
    let url = format!("{}/translate", cfg.endpoint.trim_end_matches('/'));
    // LibreTranslate: ISO 639-1 primary codes; "auto" when supported
    let src = api_source_lang(source, true, LangStyle::Primary);
    let tgt = api_target_lang(target, LangStyle::Primary);
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
    let bytes = crate::providers::http::post_json(&url, &payload)
        .map_err(|e| format!("LibreTranslate {e}"))?;
    let translated = parse_libretranslate_body(&bytes)?;
    Ok((translated, "LibreTranslate".into()))
}

/// Parse LibreTranslate response. Live API returns `translatedText`
/// (and legacy `translatedtext`); accept either, prefer camelCase.
fn parse_libretranslate_body(bytes: &[u8]) -> Result<String, String> {
    #[derive(Deserialize)]
    struct LtResp {
        #[serde(default)]
        translatedtext: String,
        #[serde(default, rename = "translatedText")]
        translated_text: String,
    }
    let resp: LtResp =
        serde_json::from_slice(bytes).map_err(|_| "Bad LibreTranslate response".to_string())?;
    let translated = if !resp.translated_text.is_empty() {
        resp.translated_text
    } else {
        resp.translatedtext
    };
    if translated.trim().is_empty() {
        return Err("Empty LibreTranslate response".into());
    }
    Ok(translated)
}

fn google_gtx(text: &str, source: &str, target: &str) -> Result<(String, String), String> {
    // Unofficial free endpoint (same family as many OSS clients). No API key.
    // Google prefers region tags for Chinese (zh-CN / zh-TW).
    let sl = api_source_lang(source, true, LangStyle::Google);
    let tl = api_target_lang(target, LangStyle::Google);
    let bytes = crate::providers::http::get_bytes_query(
        "https://translate.googleapis.com/translate_a/single",
        &[
            ("client", "gtx"),
            ("sl", &sl),
            ("tl", &tl),
            ("dt", "t"),
            ("q", text),
        ],
    )
    .map_err(|e| {
        if e == "unreachable" || e == "timed out" {
            "Google translate unreachable".into()
        } else {
            format!("Google translate {e}")
        }
    })?;
    let translated = parse_google_gtx_body(&bytes)?;
    Ok((translated, "Google".into()))
}

/// Parse Google gtx response: `[[["Hello world","你好世界",...],...],...]`.
/// First element of each inner chunk is the translated segment; concatenate them.
fn parse_google_gtx_body(bytes: &[u8]) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Bad Google translate response".to_string())?;
    let mut translated = String::new();
    if let Some(arr) = v
        .as_array()
        .and_then(|a| a.first())
        .and_then(|x| x.as_array())
    {
        for part in arr {
            if let Some(s) = part
                .as_array()
                .and_then(|p| p.first())
                .and_then(|t| t.as_str())
            {
                translated.push_str(s);
            }
        }
    }
    if translated.trim().is_empty() {
        return Err("Empty Google translate response".into());
    }
    Ok(translated)
}

fn mymemory(text: &str, source: &str, target: &str) -> Result<(String, String), String> {
    // MyMemory has no reliable "auto"; fall back to en when unknown.
    // Uses region for Chinese; primary for most others.
    let src = match api_source_lang(source, false, LangStyle::MyMemory).as_str() {
        "auto" => "en".to_string(),
        s => s.to_string(),
    };
    let tgt = api_target_lang(target, LangStyle::MyMemory);
    let langpair = format!("{src}|{tgt}");
    let bytes = crate::providers::http::get_bytes_query(
        "https://api.mymemory.translated.net/get",
        &[("q", text), ("langpair", &langpair)],
    )
    .map_err(|e| {
        if e == "unreachable" || e == "timed out" {
            "MyMemory unreachable".into()
        } else {
            format!("MyMemory {e}")
        }
    })?;

    let translated = parse_mymemory_body(&bytes)?;
    Ok((translated, "MyMemory".into()))
}

/// Parse MyMemory JSON. Live API uses camelCase (`responseData.translatedText`).
fn parse_mymemory_body(bytes: &[u8]) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Bad MyMemory response".to_string())?;
    let translated = v
        .pointer("/responseData/translatedText")
        .or_else(|| v.pointer("/response_data/translated_text"))
        .or_else(|| v.pointer("/responseData/translated_text"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let upper = translated.to_ascii_uppercase();
    if translated.trim().is_empty() || upper.contains("INVALID SOURCE LANGUAGE") {
        return Err("MyMemory rejected language pair".into());
    }
    if upper.contains("QUERY LENGTH LIMIT") {
        return Err("MyMemory rate/length limit".into());
    }
    Ok(translated.trim().to_string())
}

/// How to encode language tags for a given backend.
#[derive(Clone, Copy)]
enum LangStyle {
    /// ISO 639-1 primary only (`zh`, `en`, `pt`).
    Primary,
    /// Google gtx: keep zh-CN / zh-TW / pt-BR.
    Google,
    /// MyMemory: zh-CN style for Chinese; primary otherwise.
    MyMemory,
}

fn api_target_lang(code: &str, style: LangStyle) -> String {
    encode_lang(code, style)
}

/// Source language code for HTTP APIs.
/// `allow_auto`: Google / LibreTranslate can use `auto`; MyMemory cannot.
fn api_source_lang(source: &str, allow_auto: bool, style: LangStyle) -> String {
    let s = source.trim();
    if s.eq_ignore_ascii_case("auto") {
        return if allow_auto {
            "auto".into()
        } else {
            "en".into()
        };
    }
    encode_lang(s, style)
}

fn encode_lang(code: &str, style: LangStyle) -> String {
    let n = normalize_lang(code);
    if n.eq_ignore_ascii_case("auto") {
        return "auto".into();
    }
    match style {
        LangStyle::Primary => {
            // LibreTranslate-style: zh-CN → zh, pt-BR → pt
            if n.to_ascii_lowercase().starts_with("zh") {
                return "zh".into();
            }
            primary_lang(&n)
        }
        LangStyle::Google | LangStyle::MyMemory => {
            // Preserve Chinese / Brazilian Portuguese regions; strip other regions.
            let lower = n.to_ascii_lowercase();
            if lower == "zh-cn" || lower == "zh" {
                return "zh-CN".into();
            }
            if lower == "zh-tw" || lower == "zh-hk" {
                return "zh-TW".into();
            }
            if lower == "pt-br" {
                return "pt-BR".into();
            }
            primary_lang(&n)
        }
    }
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

/// Process-memory only — safe on the GTK main thread (no FS).
fn cache_get_mem(key: &str) -> Option<CacheEntry> {
    let Ok(g) = mem_ok().lock() else {
        return None;
    };
    let e = g.get(key)?;
    if now_secs().saturating_sub(e.fetched_at) > CACHE_TTL_SECS || e.translated.trim().is_empty() {
        return None;
    }
    Some(e.clone())
}

/// Mem first, then durable disk (promotes into mem). **Worker thread only.**
fn cache_get(key: &str) -> Option<CacheEntry> {
    if let Some(e) = cache_get_mem(key) {
        return Some(e);
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
        // Bound process-local success cache (disk remains durable).
        const MAX_MEM: usize = 256;
        if g.len() > MAX_MEM {
            // Drop oldest by fetched_at until under cap.
            let mut keys: Vec<(u64, String)> =
                g.iter().map(|(k, v)| (v.fetched_at, k.clone())).collect();
            keys.sort_by_key(|(ts, _)| *ts);
            let remove_n = g.len() - MAX_MEM;
            for (_, k) in keys.into_iter().take(remove_n) {
                g.remove(&k);
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
        let now = now_secs();
        g.insert(key.to_string(), (msg.to_string(), now));
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
    fn popular_scripts_auto_detect() {
        let c = cfg_auto();
        assert!(is_translate_query("Привет мир", &c)); // Russian
        assert!(is_translate_query("مرحبا بالعالم", &c)); // Arabic
        assert!(is_translate_query("नमस्ते दुनिया", &c)); // Hindi
        assert!(is_translate_query("สวัสดี", &c)); // Thai
        assert!(is_translate_query("Γειά σου", &c)); // Greek
        assert!(is_translate_query("שלום עולם", &c)); // Hebrew
        assert!(!is_translate_query("spotify", &c));
        assert!(!is_translate_query("hello world", &c));
    }

    #[test]
    fn prefix_forced() {
        let c = cfg_prefix_only();
        assert!(is_translate_query("tr 你好", &c));
        assert!(is_translate_query("translate hello world", &c));
        assert!(!is_translate_query("你好", &c));
        assert!(!is_translate_query("Привет", &c));
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
        assert_eq!(guess_source_lang("Привет", false), "ru");
        assert_eq!(guess_source_lang("مرحبا", false), "ar");
        assert_eq!(guess_source_lang("नमस्ते", false), "hi");
        assert_eq!(guess_source_lang("สวัสดี", false), "th");
        assert_eq!(guess_source_lang("Γειά", false), "el");
        assert_eq!(guess_source_lang("שלום", false), "he");
        assert_eq!(guess_source_lang("Hello world", true), "auto");
    }

    #[test]
    fn normalize_aliases() {
        assert_eq!(normalize_lang("jp"), "ja");
        assert_eq!(normalize_lang("kr"), "ko");
        assert_eq!(normalize_lang("ua"), "uk");
        assert_eq!(normalize_lang("zh"), "zh-CN");
        assert_eq!(normalize_lang("zh-TW"), "zh-TW");
        assert_eq!(normalize_lang("pt-br"), "pt-BR");
        assert_eq!(normalize_lang("iw"), "he");
    }

    #[test]
    fn direction_parse_en_zh() {
        let c = cfg_auto();
        let job = parse_job("tr en zh Hello world", &c).expect("job");
        assert_eq!(job.0, "Hello world");
        assert_eq!(job.1.to_ascii_lowercase(), "en");
        assert!(job.2.to_ascii_lowercase().starts_with("zh"));
    }

    #[test]
    fn direction_parse_zh_en() {
        let c = cfg_auto();
        let job = parse_job("tr zh en 你好世界", &c).expect("job");
        assert_eq!(job.0, "你好世界");
        assert!(job.1.to_ascii_lowercase().starts_with("zh"));
        assert_eq!(job.2.to_ascii_lowercase(), "en");
    }

    #[test]
    fn direction_parse_popular_pairs() {
        let c = cfg_auto();
        let job = parse_job("tr en es Hello", &c).expect("job");
        assert_eq!(job.1, "en");
        assert_eq!(job.2, "es");
        assert_eq!(job.0, "Hello");

        let job = parse_job("tr ru en Привет", &c).expect("job");
        assert_eq!(job.1, "ru");
        assert_eq!(job.2, "en");

        let job = parse_job("tr en hi Hello", &c).expect("job");
        assert_eq!(job.2, "hi");
    }

    #[test]
    fn direction_requires_text() {
        let c = cfg_auto();
        // Only two codes, no payload — not a direction job.
        assert!(parse_job("tr en zh", &c).is_none());
        // Without two codes: whole remainder is text, source=auto
        let job = parse_job("tr Hello there", &c).expect("job");
        assert_eq!(job.0, "Hello there");
        assert_eq!(job.1, "auto");
    }

    #[test]
    fn bare_cjk_uses_config_target() {
        let mut c = cfg_auto();
        c.target_lang = "en".into();
        let job = parse_job("你好世界", &c).expect("job");
        assert_eq!(job.0, "你好世界");
        assert_eq!(job.1, "zh-CN");
        assert_eq!(job.2, "en");
    }

    #[test]
    fn auto_flips_same_source_target() {
        let mut c = cfg_auto();
        c.target_lang = "zh".into();
        // Chinese paste with target zh → reverse to English
        let job = parse_job("你好世界", &c).expect("job");
        assert!(job.1.to_ascii_lowercase().starts_with("zh"));
        assert_eq!(job.2, "en");
    }

    #[test]
    fn is_lang_code_basic() {
        assert!(is_lang_code("en"));
        assert!(is_lang_code("zh"));
        assert!(is_lang_code("zh-CN"));
        assert!(is_lang_code("pt-BR"));
        assert!(!is_lang_code("hello"));
        assert!(!is_lang_code("1"));
    }

    #[test]
    fn pending_id() {
        let r = pending_result("你好", "zh-CN", "en");
        assert!(is_pending_result(&r));
    }

    #[test]
    fn google_keeps_zh_region() {
        assert_eq!(encode_lang("zh-CN", LangStyle::Google), "zh-CN");
        assert_eq!(encode_lang("zh-TW", LangStyle::Google), "zh-TW");
        assert_eq!(encode_lang("zh", LangStyle::Primary), "zh");
        assert_eq!(encode_lang("pt-BR", LangStyle::Google), "pt-BR");
        assert_eq!(encode_lang("pt-BR", LangStyle::Primary), "pt");
    }

    #[test]
    fn mymemory_parses_camel_case() {
        let body =
            br#"{"responseData":{"translatedText":"Hello. ","match":0.99},"responseStatus":200}"#;
        assert_eq!(parse_mymemory_body(body).unwrap(), "Hello.");
    }

    #[test]
    fn mymemory_rejects_invalid() {
        let body = br#"{"responseData":{"translatedText":"INVALID SOURCE LANGUAGE "}}"#;
        assert!(parse_mymemory_body(body).is_err());
    }

    #[test]
    fn mymemory_rejects_rate_limit() {
        let body = br#"{"responseData":{"translatedText":"QUERY LENGTH LIMIT EXCEEDED. MAX ALLOWED QUERY : 500 CHARS"}}"#;
        assert!(parse_mymemory_body(body).is_err());
    }

    #[test]
    fn google_gtx_concatenates_segments() {
        // Real Google gtx shape: [[["Hello world","你好世界",null,null,10]],null,"en",...].
        // Chinese "你好世界" is irrelevant to the assertion — only chunk[0] is read.
        let body = b"[[[\"Hello world\",\"\xe4\xbd\xa0\xe5\xa5\xbd\xe4\xb8\x96\xe7\x95\x8c\",null,null,10]],null,\"en\"]";
        let out = parse_google_gtx_body(body).unwrap();
        assert_eq!(out, "Hello world");
    }

    #[test]
    fn google_gtx_joins_multiple_chunks() {
        // Name entity split across chunks → segments concatenate in order.
        let body = b"[[[\"Hello \",null,1],[\"world\",null,2]]]";
        let out = parse_google_gtx_body(body).unwrap();
        assert_eq!(out, "Hello world");
    }

    #[test]
    fn google_gtx_empty_rejected() {
        let body = b"[[[\" \"]]]";
        assert!(parse_google_gtx_body(body).is_err());
        assert!(parse_google_gtx_body(b"not json").is_err());
    }

    #[test]
    fn libretranslate_parses_camel_case() {
        let body = br#"{"translatedText":"Hola mundo","detectedLanguage":{"language":"es"}}"#;
        assert_eq!(parse_libretranslate_body(body).unwrap(), "Hola mundo");
    }

    #[test]
    fn libretranslate_accepts_legacy_snake_case() {
        let body = br#"{"translatedtext":"Bonjour le monde"}"#;
        assert_eq!(parse_libretranslate_body(body).unwrap(), "Bonjour le monde");
    }

    #[test]
    fn libretranslate_empty_rejected() {
        let body = br#"{"translatedText":"  "}"#;
        assert!(parse_libretranslate_body(body).is_err());
        assert!(parse_libretranslate_body(b"{}").is_err());
    }
}
