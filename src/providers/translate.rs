//! Translate-on-paste provider (Phase 0 scaffold).
//!
//! Detection + config gating only. Phase 1 adds cache + HTTP.
//! When `TranslateConfig.enabled` is false, **no** network, cache, or
//! background work runs — callers must short-circuit before any I/O.

use crate::config::TranslateConfig;
use crate::providers::files::{is_path_glob_query, is_scoped_file_query};
use crate::providers::{Provider, SearchResult};
use std::sync::Arc;

use crate::config::ConfigStore;

/// High score reserved for a real translation hit (Phase 1).
#[allow(dead_code)]
pub(crate) const TRANSLATE_SCORE: i64 = 100_000;

pub struct TranslateProvider {
    config: Arc<ConfigStore>,
}

impl TranslateProvider {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self { config }
    }

    /// Live config snapshot (sanitized on store update).
    pub fn cfg(&self) -> TranslateConfig {
        self.config.get().translate
    }

    /// Master switch — when false, engine must not call search or spawn work.
    pub fn is_enabled(&self) -> bool {
        self.cfg().enabled
    }

    /// True when this query should be handled as translation (prefix or auto CJK).
    ///
    /// Returns **false** immediately if the feature is disabled — no script scan
    /// cost beyond reading the bool, and no I/O.
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
        // Hard gate: disabled → zero work (no cache, no curl, no rows).
        let cfg = self.cfg();
        if !cfg.enabled {
            return Vec::new();
        }
        if !is_translate_query(query, &cfg) {
            return Vec::new();
        }
        // Phase 0: detection only. Phase 1 returns ConversionView + Copy.
        Vec::new()
    }
}

/// Strip forced-mode prefixes. Returns `(forced, remainder)`.
pub fn strip_translate_prefix(query: &str) -> (bool, &str) {
    let q = query.trim();
    let lower = q.to_ascii_lowercase();
    for prefix in ["tr ", "translate ", "译 "] {
        if lower.starts_with(prefix) || q.starts_with(prefix) {
            // Prefix length in bytes matches for ASCII; 译 is 3 bytes UTF-8.
            let plen = if prefix == "译 " {
                "译 ".len()
            } else {
                prefix.len()
            };
            // Case-insensitive for ASCII prefixes
            if prefix.chars().all(|c| c.is_ascii()) {
                if lower.starts_with(prefix) {
                    return (true, q[plen..].trim());
                }
            } else if q.starts_with(prefix) {
                return (true, q[plen..].trim());
            }
        }
    }
    // Also allow bare `tr` / `translate` with no trailing space only if more text follows via unicode space
    if let Some(rest) = lower.strip_prefix("tr\t").or_else(|| lower.strip_prefix("translate\t")) {
        let offset = q.len() - rest.len();
        return (true, q[offset..].trim());
    }
    (false, q)
}

/// Public detection used by engine short-circuit tests and provider.
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
        return false;
    }

    // Never steal path / glob / scoped file queries.
    if is_path_glob_query(q) || is_scoped_file_query(q) {
        return false;
    }
    // Remainder itself path-like
    if text.starts_with('/')
        || text.starts_with("~/")
        || text.starts_with("./")
        || text.contains('*')
            && (text.contains('/') || text.starts_with('*') || text.starts_with('.'))
    {
        return false;
    }

    if forced {
        // Prefix mode: any non-empty text (Phase 1 will translate).
        return text.chars().count() >= 1;
    }

    if !cfg.auto_detect {
        return false;
    }

    looks_like_translatable_script(text)
}

/// CJK / Japanese / Korean script density heuristic (no network).
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
    // ≥2 CJK chars, or single CJK with almost no Latin (e.g. "你")
    if cjk >= 2 {
        return true;
    }
    // one CJK ideograph alone or with few non-script chars
    total > 0 && (cjk as f32 / total as f32) >= 0.5
}

fn is_cjk_like(ch: char) -> bool {
    let c = ch as u32;
    // CJK Unified Ideographs + extensions (common blocks)
    (0x4E00..=0x9FFF).contains(&c)
        || (0x3400..=0x4DBF).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        // Hiragana / Katakana
        || (0x3040..=0x309F).contains(&c)
        || (0x30A0..=0x30FF).contains(&c)
        // Hangul syllables
        || (0xAC00..=0xD7AF).contains(&c)
        // CJK punctuation sometimes appears in pure CJK paste
        || (0x3000..=0x303F).contains(&c)
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
        assert!(!is_translate_query("你好", &c)); // auto off
        assert!(!is_translate_query("tr ", &c)); // empty remainder
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
        // Use a real ConfigStore is heavy; unit-test the gate via is_translate_query.
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
}
