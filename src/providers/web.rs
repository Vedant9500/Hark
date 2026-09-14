//! Web-search fallback provider.
//!
//! Zero I/O until Enter: [`WebProvider::fallback`] only builds a row
//! (no network, no disk). The browser itself renders AI Overviews /
//! definitions — Hark never scrapes result pages.
//!
//! Two shapes:
//! - **fallback** (last row): any free-text query with no path/glob/calc/
//!   translate ownership gets `Search Google for "…"`. Enter opens it.
//! - **forced** (`? foo`, `g foo`, `wiki foo`, …): owns the query like
//!   translate does — the only row, ranked first.

use crate::config::{ConfigStore, WebConfig};
use crate::providers::{Action, ResultKind, SearchResult};
use std::sync::Arc;

/// Score for the appended fallback row — sorts after every local hit.
pub(crate) const WEB_SCORE: i64 = -10_000;
/// Score for an explicitly forced web query (`g foo`) — owns the query.
pub(crate) const WEB_FORCED_SCORE: i64 = 100_000;
/// Row titles truncate the echoed query here so long pastes stay one line.
const TITLE_ECHO_MAX_CHARS: usize = 60;

pub struct WebProvider {
    config: Arc<ConfigStore>,
}

impl WebProvider {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self { config }
    }

    pub fn cfg(&self) -> WebConfig {
        self.config.with(|c| c.web.clone())
    }

    pub fn is_enabled(&self) -> bool {
        self.config.with(|c| c.web.enabled)
    }

    /// Forced web query (`? foo`, `g foo`, `wiki foo`). Returns the bare
    /// search text plus an optional engine override (`wiki` forces
    /// Wikipedia regardless of the configured engine).
    pub fn forced_query(&self, query: &str) -> Option<(String, Option<&'static str>)> {
        if !self.is_enabled() {
            return None;
        }
        strip_force_web_prefix(query)
    }

    /// Fallback row for an already-trimmed query using the configured engine.
    /// Pure construction — no I/O, safe on the GTK main thread.
    pub fn fallback(&self, query: &str) -> Option<SearchResult> {
        let cfg = self.cfg();
        if !cfg.enabled {
            return None;
        }
        let q = query.trim();
        if q.is_empty() {
            return None;
        }
        Some(web_result(q, None, &cfg, WEB_SCORE))
    }

    /// Forced row (`g foo` → search text `foo`). Pure construction.
    pub fn forced(&self, query: &str) -> Option<SearchResult> {
        let cfg = self.cfg();
        if !cfg.enabled {
            return None;
        }
        let (text, engine_override) = strip_force_web_prefix(query)?;
        if text.trim().is_empty() {
            return None;
        }
        Some(web_result(
            text.trim(),
            engine_override,
            &cfg,
            WEB_FORCED_SCORE,
        ))
    }
}

/// Build the fallback/forced row. `engine_override` (`Some("wikipedia")`
/// from a `wiki ` prefix) wins over the configured engine.
fn web_result(
    text: &str,
    engine_override: Option<&str>,
    cfg: &WebConfig,
    score: i64,
) -> SearchResult {
    let (url, label, key) = build_search_url(text, engine_override, cfg);
    let echo = truncate_echo(text);
    SearchResult {
        id: format!("web:{key}:{:016x}", fnv1a64(&normalized_key(text))),
        title: format!("Search {label} for “{echo}”"),
        subtitle: "No local match · Enter to open in browser".into(),
        kind: ResultKind::Web,
        score,
        icon: Some("applications-internet".into()),
        action: Action::OpenUrl(url),
        conversion: None,
        matched: None,
    }
}

/// Resolve `(url, display label, id key)` for `text`.
fn build_search_url(
    text: &str,
    engine_override: Option<&str>,
    cfg: &WebConfig,
) -> (String, String, String) {
    let engine = engine_override.unwrap_or(cfg.engine.as_str());
    let enc = encode_query(text);
    match engine.trim().to_ascii_lowercase().as_str() {
        "ddg" | "duckduckgo" | "duck" => (
            format!("https://duckduckgo.com/?q={enc}"),
            "DuckDuckGo".into(),
            "ddg".into(),
        ),
        "bing" => (
            format!("https://www.bing.com/search?q={enc}"),
            "Bing".into(),
            "bing".into(),
        ),
        "brave" => (
            format!("https://search.brave.com/search?q={enc}"),
            "Brave".into(),
            "brave".into(),
        ),
        "wikipedia" | "wiki" => (
            format!("https://en.wikipedia.org/wiki/Special:Search?search={enc}"),
            "Wikipedia".into(),
            "wiki".into(),
        ),
        "custom" => {
            let t = cfg.custom_url.trim();
            if is_http_template(t) {
                (t.replace("%s", &enc), "Web".into(), "custom".into())
            } else {
                // Misconfigured custom template — degrade to Google, never fail.
                (
                    format!("https://www.google.com/search?q={enc}"),
                    "Google".into(),
                    "google".into(),
                )
            }
        }
        // "google" + anything unknown (forward-compat): Google shape.
        _ => (
            format!("https://www.google.com/search?q={enc}"),
            "Google".into(),
            "google".into(),
        ),
    }
}

/// Custom templates must be http(s) and carry exactly the `%s` slot.
fn is_http_template(t: &str) -> bool {
    if !t.contains("%s") {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// Strip a forced-web prefix. Returns `(search text, engine override)`.
///
/// Supported (ASCII case-insensitive, prefix + whitespace, so `gimp` /
/// `webstorm` never trigger): `g`, `google`, `web`, `search`, `ddg`,
/// `bing`, `brave`, `wiki`, `wikipedia`. Bare `?foo` / `? foo` forces the
/// configured engine. Returns `None` when there is no prefix or the
/// remainder is empty.
pub fn strip_force_web_prefix(query: &str) -> Option<(String, Option<&'static str>)> {
    let q = query.trim();
    if let Some(rest) = q.strip_prefix('?') {
        let text = rest.trim();
        if text.is_empty() {
            return None;
        }
        return Some((text.to_string(), None));
    }
    let bytes = q.as_bytes();
    // Longest first so `wikipedia foo` doesn't match `web`… it can't
    // (whitespace required after the keyword), but order still reads safer.
    for (pref, engine) in [
        ("wikipedia", Some("wikipedia")),
        ("google", None),
        ("search", None),
        ("brave", None),
        ("bing", None),
        ("wiki", Some("wikipedia")),
        ("web", None),
        ("ddg", None),
        ("g", None),
    ] {
        let pb = pref.as_bytes();
        if bytes.len() > pb.len() && bytes[..pb.len()].eq_ignore_ascii_case(pb) {
            let rest = &q[pb.len()..];
            let b0 = rest.as_bytes()[0];
            if b0 == b' ' || b0 == b'\t' {
                let text = rest.trim();
                if text.is_empty() {
                    return None;
                }
                return Some((text.to_string(), engine));
            }
        }
    }
    None
}

/// True for any forced-web query (UI gates: mode icon, deep-search skip).
pub fn is_force_web_query(query: &str) -> bool {
    strip_force_web_prefix(query).is_some()
}

/// Percent-encode a query string (RFC 3986 unreserved set passes through,
/// everything else `%XX` over UTF-8 bytes; space → `%20` so custom
/// path-style templates also work).
pub fn encode_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for b in text.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            _ => {
                out.push('%');
                out.push(upper_hex(b >> 4));
                out.push(upper_hex(b & 0x0F));
            }
        }
    }
    out
}

fn upper_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + n - 10) as char,
    }
}

fn truncate_echo(text: &str) -> String {
    let t = text
        .trim()
        .replace(|c: char| c.is_control() || c == '\n', " ");
    let collapsed = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TITLE_ECHO_MAX_CHARS {
        return collapsed;
    }
    let head: String = collapsed.chars().take(TITLE_ECHO_MAX_CHARS - 1).collect();
    format!("{head}…")
}

/// Whitespace-normalized key input so `a  b` and `a b` share one id.
fn normalized_key(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Open `url` in the default browser.
///
/// Only `http(s)` targets are accepted — a crafted custom template must
/// never turn Enter into `file://` / `javascript:` dispatch. Prefers the
/// FreeDesktop default handler (`gio`), falling back to `xdg-open`.
pub fn open_url(url: &str) -> Result<(), String> {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("refusing to open non-http(s) URL".into());
    }
    let url = url.trim().to_string();
    // Default-browser path: respects the user's MIME handler, no child.
    if gio_open_uri(&url) {
        return Ok(());
    }
    // Fallback: detached xdg-open, reaped on a thread (no daemon zombies).
    match std::process::Command::new("xdg-open")
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(e) => Err(format!("could not open URL ({e})")),
    }
}

fn gio_open_uri(url: &str) -> bool {
    gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(engine: &str, custom_url: &str) -> WebConfig {
        WebConfig {
            enabled: true,
            engine: engine.into(),
            custom_url: custom_url.into(),
            extra: Default::default(),
        }
    }

    #[test]
    fn forced_prefixes_parse() {
        assert_eq!(
            strip_force_web_prefix("?what does SIMD mean"),
            Some(("what does SIMD mean".into(), None))
        );
        assert_eq!(strip_force_web_prefix("? AVX"), Some(("AVX".into(), None)));
        assert_eq!(
            strip_force_web_prefix("g simd full form"),
            Some(("simd full form".into(), None))
        );
        assert_eq!(
            strip_force_web_prefix("G hello"),
            Some(("hello".into(), None))
        );
        assert_eq!(
            strip_force_web_prefix("wiki SIMD"),
            Some(("SIMD".into(), Some("wikipedia")))
        );
        assert_eq!(
            strip_force_web_prefix("wikipedia AVX"),
            Some(("AVX".into(), Some("wikipedia")))
        );
        assert!(is_force_web_query("ddg hello world"));
    }

    #[test]
    fn forced_prefix_never_steals_apps() {
        assert_eq!(strip_force_web_prefix("gimp"), None);
        assert_eq!(strip_force_web_prefix("webstorm"), None);
        assert_eq!(strip_force_web_prefix("searchmonkey"), None);
        assert_eq!(strip_force_web_prefix("google"), None);
        assert_eq!(strip_force_web_prefix("g"), None);
        assert_eq!(strip_force_web_prefix("?"), None);
        assert_eq!(strip_force_web_prefix("?   "), None);
        assert_eq!(strip_force_web_prefix("wiki"), None);
        assert!(!is_force_web_query("firefox"));
    }

    #[test]
    fn engines_build_expected_hosts() {
        let (url, label, _) = build_search_url("SIMD", None, &cfg_with("google", ""));
        assert!(url.starts_with("https://www.google.com/search?q="), "{url}");
        assert_eq!(label, "Google");
        let (url, _, key) = build_search_url("SIMD", None, &cfg_with("duckduckgo", ""));
        assert!(url.starts_with("https://duckduckgo.com/?q="), "{url}");
        assert_eq!(key, "ddg");
        let (url, _, _) = build_search_url("SIMD", None, &cfg_with("bing", ""));
        assert!(url.contains("bing.com"), "{url}");
        let (url, _, _) = build_search_url("SIMD", None, &cfg_with("brave", ""));
        assert!(url.contains("search.brave.com"), "{url}");
        let (url, label, _) = build_search_url("SIMD", None, &cfg_with("wikipedia", ""));
        assert!(url.contains("wikipedia.org"), "{url}");
        assert_eq!(label, "Wikipedia");
        // wiki prefix overrides the configured engine.
        let (url, _, key) = build_search_url("SIMD", Some("wikipedia"), &cfg_with("google", ""));
        assert!(url.contains("wikipedia.org"), "{url}");
        assert_eq!(key, "wiki");
        // Unknown engine degrades to Google, never fails.
        let (url, _, _) = build_search_url("x", None, &cfg_with("kagi-future", ""));
        assert!(url.starts_with("https://www.google.com/"), "{url}");
    }

    #[test]
    fn custom_template_and_bad_template() {
        let (url, label, key) = build_search_url(
            "a b",
            None,
            &cfg_with("custom", "https://search.example/?q=%s&src=hark"),
        );
        assert_eq!(url, "https://search.example/?q=a%20b&src=hark");
        assert_eq!((label.as_str(), key.as_str()), ("Web", "custom"));
        // No %s → Google fallback.
        let (url, _, key) =
            build_search_url("a", None, &cfg_with("custom", "https://search.example/"));
        assert!(url.starts_with("https://www.google.com/"), "{url}");
        assert_eq!(key, "google");
        // Non-http template → Google fallback.
        let (url, _, _) =
            build_search_url("a", None, &cfg_with("custom", "file:///etc/passwd?q=%s"));
        assert!(url.starts_with("https://www.google.com/"), "{url}");
    }

    #[test]
    fn encoding_is_rfc3986() {
        assert_eq!(encode_query("a b"), "a%20b");
        assert_eq!(
            encode_query("what does SIMD mean?"),
            "what%20does%20SIMD%20mean%3F"
        );
        assert_eq!(encode_query("a+b"), "a%2Bb");
        // Unreserved set passes through; multibyte encodes per UTF-8 byte.
        assert_eq!(encode_query("a-z_0.9~"), "a-z_0.9~");
        assert_eq!(encode_query("héllo"), "h%C3%A9llo");
    }

    #[test]
    fn result_shape_and_scores() {
        let cfg = cfg_with("google", "");
        let r = web_result("what does SIMD mean", None, &cfg, WEB_SCORE);
        assert_eq!(r.kind, ResultKind::Web);
        assert_eq!(r.score, WEB_SCORE);
        assert!(r.title.contains("Google"));
        assert!(r.title.contains("what does SIMD mean"));
        assert!(matches!(r.action, Action::OpenUrl(_)));
        assert!(r.id.starts_with("web:google:"));
        // Long queries stay one bounded row.
        let long = "x".repeat(500);
        let r = web_result(&long, None, &cfg, WEB_SCORE);
        assert!(r.title.chars().count() < 120, "{}", r.title.len());
        assert!(r.id.len() < 40);
    }

    #[test]
    fn open_url_rejects_non_http_without_launching() {
        assert!(open_url("file:///etc/passwd").is_err());
        assert!(open_url("javascript:alert(1)").is_err());
        assert!(open_url("").is_err());
    }
}
