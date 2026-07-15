//! Translate-on-paste provider.
//!
//! Phase 1: detection + disk cache + LibreTranslate/MyMemory via `curl`.
//! When `TranslateConfig.enabled` is false, **no** network, cache, or
//! background work runs.

use crate::config::{ConfigStore, TranslateConfig};
use crate::providers::files::{is_path_glob_query, is_scoped_file_query};
use crate::providers::{Action, ConversionView, Provider, ResultKind, SearchResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Strong hit — engine skips apps/files noise.
pub(crate) const TRANSLATE_SCORE: i64 = 100_000;
const TRANSLATE_FAIL_SCORE: i64 = 80_000;
const CACHE_TTL_SECS: u64 = 14 * 24 * 3600;

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

    /// Master switch — when false, engine must not call search or spawn work.
    pub fn is_enabled(&self) -> bool {
        self.cfg().enabled
    }

    /// True when this query should be handled as translation (prefix or auto CJK).
    pub fn should_handle(&self, query: &str) -> bool {
        let cfg = self.cfg();
        if !cfg.enabled {
            return false;
        }
        is_translate_query(query, &cfg)
    }
}

impl Provider for TranslateProvider {
    fn search(&self, query: &str) -> Vec<SearchResult> {
        let cfg = self.cfg();
        // Hard gate: disabled → zero work (no cache, no curl).
        if !cfg.enabled {
            return Vec::new();
        }
        if !is_translate_query(query, &cfg) {
            return Vec::new();
        }

        let (forced, text) = strip_translate_prefix(query.trim());
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        if text.chars().count() > cfg.max_chars {
            return vec![fail_result(
                text,
                &cfg.target_lang,
                &format!("Too long (max {} characters)", cfg.max_chars),
            )];
        }

        let source = guess_source_lang(text, forced);
        let target = cfg.target_lang.clone();
        // Avoid no-op same-language when we can tell.
        if source == target && !forced {
            // e.g. English target and pure English auto — shouldn't reach here often.
        }

        let key = cache_key(&source, &target, text);

        // Cache first — only when enabled (we're past the gate).
        if let Some(hit) = cache_get(&key) {
            return vec![ok_result(text, &hit.translated, &source, &target, "cache")];
        }

        match translate_http(text, &source, &target, &cfg) {
            Ok((translated, backend)) => {
                cache_put(&key, text, &source, &target, &translated);
                vec![ok_result(text, &translated, &source, &target, &backend)]
            }
            Err(msg) => vec![fail_result(text, &target, &msg)],
        }
    }
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

fn fail_result(source_text: &str, target: &str, msg: &str) -> SearchResult {
    let tgt_b = target.to_ascii_uppercase();
    SearchResult {
        id: format!("translate-fail:{}", simple_hash(source_text)),
        title: "Translation unavailable".into(),
        subtitle: msg.to_string(),
        kind: ResultKind::Conversion,
        score: TRANSLATE_FAIL_SCORE,
        icon: Some("dialog-warning".into()),
        action: Action::Copy(source_text.to_string()),
        conversion: Some(ConversionView {
            left_title: source_text.to_string(),
            left_badge: "SRC".into(),
            right_title: "—".into(),
            right_badge: tgt_b,
        }),
    }
}

// ── Detection ───────────────────────────────────────────────────────────────

/// Strip forced-mode prefixes. Returns `(forced, remainder)`.
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

/// Public detection used by engine and provider.
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
        // Still "a translate query" so we can show the too-long error.
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
        return "zh".into();
    }
    "en".into()
}

// ── HTTP ────────────────────────────────────────────────────────────────────

fn translate_http(
    text: &str,
    source: &str,
    target: &str,
    cfg: &TranslateConfig,
) -> Result<(String, String), String> {
    if !cfg.endpoint.is_empty() {
        libretranslate(text, source, target, cfg)
    } else {
        mymemory(text, source, target)
    }
}

fn libretranslate(
    text: &str,
    source: &str,
    target: &str,
    cfg: &TranslateConfig,
) -> Result<(String, String), String> {
    let url = format!("{}/translate", cfg.endpoint.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "q": text,
        "source": source,
        "target": target,
        "format": "text",
    });
    if let Some(key) = &cfg.api_key {
        body["api_key"] = serde_json::Value::String(key.clone());
    }
    let payload = body.to_string();

    let mut cmd = Command::new("curl");
    cmd.args([
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
    .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|_| "curl not available".to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|_| "translate request failed".to_string())?;
    if !out.status.success() {
        return Err("Translation endpoint error (check URL / network)".into());
    }

    #[derive(Deserialize)]
    struct LtResp {
        #[serde(default)]
        translatedtext: String,
        #[serde(default, alias = "translatedText")]
        translated_text: String,
    }
    let resp: LtResp = serde_json::from_slice(&out.stdout)
        .map_err(|_| "Bad response from translation endpoint".to_string())?;
    let translated = if !resp.translatedtext.is_empty() {
        resp.translatedtext
    } else {
        resp.translated_text
    };
    if translated.trim().is_empty() {
        return Err("Empty translation response".into());
    }
    Ok((translated, "LibreTranslate".into()))
}

fn mymemory(text: &str, source: &str, target: &str) -> Result<(String, String), String> {
    // Free public API — no key. Used when no custom endpoint is set.
    let langpair = format!("{source}|{target}");
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
        return Err("Translation network error (or rate limited)".into());
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
        .map_err(|_| "Bad response from free translate API".to_string())?;
    let translated = resp
        .response_data
        .and_then(|d| d.translated_text)
        .unwrap_or_default();
    // MyMemory sometimes returns "INVALID SOURCE LANGUAGE" as the text
    if translated.trim().is_empty()
        || translated.to_ascii_uppercase().contains("INVALID")
        || translated.to_ascii_uppercase().contains("MYMEMORY WARNING")
    {
        return Err("Free translate failed — set a LibreTranslate URL in Settings → Tools".into());
    }
    // Soft warning prefix from MyMemory free tier
    let cleaned = translated
        .trim()
        .trim_start_matches("MYMEMORY WARNING")
        .trim()
        .to_string();
    let cleaned = if cleaned.is_empty() {
        translated
    } else {
        cleaned
    };
    Ok((cleaned, "MyMemory".into()))
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
    let data = fs::read_to_string(cache_path(key)).ok()?;
    let e: CacheEntry = serde_json::from_str(&data).ok()?;
    if now_secs().saturating_sub(e.fetched_at) > CACHE_TTL_SECS {
        let _ = fs::remove_file(cache_path(key));
        return None;
    }
    if e.translated.trim().is_empty() {
        return None;
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
    if let Ok(data) = serde_json::to_string(&e) {
        let path = cache_path(key);
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(tmp, path);
        }
    }
    maybe_sweep_cache(&dir);
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
    // FNV-1a 64-bit, hex — stable, no extra crate
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

// ── Tests ───────────────────────────────────────────────────────────────────

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
        assert!(!is_translate_query("translate hello", &cfg_off()));
    }

    #[test]
    fn cjk_auto_detect() {
        let c = cfg_auto();
        assert!(is_translate_query("你好", &c));
        assert!(is_translate_query("你好世界", &c));
        assert!(is_translate_query("  こんにちは  ", &c));
        assert!(is_translate_query("안녕하세요", &c));
        assert!(!is_translate_query("firefox", &c));
        assert!(!is_translate_query("a", &c));
    }

    #[test]
    fn prefix_forced() {
        let c = cfg_prefix_only();
        assert!(is_translate_query("tr 你好", &c));
        assert!(is_translate_query("translate hello world", &c));
        assert!(is_translate_query("TR Hello", &c));
        assert!(!is_translate_query("你好", &c));
        assert!(!is_translate_query("tr ", &c));
    }

    #[test]
    fn does_not_steal_paths_or_files() {
        let c = cfg_auto();
        assert!(!is_translate_query("*.md", &c));
        assert!(!is_translate_query("blink/docs/*.md", &c));
        assert!(!is_translate_query("~/dev", &c));
        assert!(!is_translate_query("optimization.md in glassbox", &c));
    }

    #[test]
    fn provider_disabled_returns_empty() {
        let c = cfg_off();
        assert!(!is_translate_query("你好世界", &c));
    }

    #[test]
    fn strip_prefix() {
        assert_eq!(strip_translate_prefix("tr 你好").0, true);
        assert_eq!(strip_translate_prefix("tr 你好").1, "你好");
        assert_eq!(strip_translate_prefix("translate foo").1, "foo");
        assert_eq!(strip_translate_prefix("hello").0, false);
    }

    #[test]
    fn cache_key_stable() {
        let a = cache_key("zh", "en", "你好  世界");
        let b = cache_key("zh", "en", "你好 世界");
        assert_eq!(a, b);
        assert_ne!(cache_key("zh", "en", "你好"), cache_key("zh", "en", "您好"));
    }

    #[test]
    fn guess_langs() {
        assert_eq!(guess_source_lang("你好", false), "zh");
        assert_eq!(guess_source_lang("こんにちは", false), "ja");
        assert_eq!(guess_source_lang("안녕하세요", false), "ko");
        assert_eq!(guess_source_lang("hello", true), "en");
    }
}
