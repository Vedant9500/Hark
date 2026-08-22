use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// Prefer last-known rates over blocking search on network.
const TTL_SECS: u64 = 12 * 3600;
/// After a failed (or completed) background fetch, wait before trying again.
const REFRESH_BACKOFF_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RatesCache {
    base: String,
    date: String,
    rates: HashMap<String, f64>,
    fetched_at: u64,
}

struct FxShared {
    cache: RwLock<Option<RatesCache>>,
    /// Background HTTP refresh in flight.
    inflight: AtomicBool,
    /// Last spawn attempt (success or fail) — throttles retries.
    last_attempt_secs: AtomicU64,
}

pub struct FxStore {
    shared: Arc<FxShared>,
}

impl FxStore {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(FxShared {
                cache: RwLock::new(load_disk()),
                inflight: AtomicBool::new(false),
                last_attempt_secs: AtomicU64::new(0),
            }),
        }
    }

    /// Test-only: inject an in-memory rate table without touching disk or network.
    /// `last_attempt_secs` is pre-set so `convert` never schedules a bg fetch.
    #[cfg(test)]
    fn with_cache(base: &str, date: &str, rates: HashMap<String, f64>) -> Self {
        let fetched_at = now_secs();
        Self {
            shared: Arc::new(FxShared {
                cache: RwLock::new(Some(RatesCache {
                    base: base.to_string(),
                    date: date.to_string(),
                    rates,
                    fetched_at,
                })),
                inflight: AtomicBool::new(false),
                last_attempt_secs: AtomicU64::new(fetched_at),
            }),
        }
    }

    /// Convert using memory/disk rates only. Never blocks on network.
    /// Stale rates still convert; a background refresh is scheduled when needed.
    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Option<(f64, String)> {
        let from = from.to_uppercase();
        let to = to.to_uppercase();
        if from == to {
            return Some((amount, "same currency".into()));
        }

        // One read lock: compute conversion + staleness, then refresh after drop.
        let (converted, stale) = {
            let g = self.shared.cache.read().unwrap_or_else(|p| p.into_inner());
            match g.as_ref() {
                None => (None, true),
                Some(cache) => {
                    let age = now_secs().saturating_sub(cache.fetched_at);
                    let stale = age > TTL_SECS;
                    let converted = rate_vs_base(cache, &from).and_then(|from_rate| {
                        rate_vs_base(cache, &to).and_then(|to_rate| {
                            let out = convert_amount(amount, from_rate, to_rate)?;
                            let meta = if stale {
                                format!("ECB {} · stale cache", cache.date)
                            } else if age > 300 {
                                format!("ECB {} · cached", cache.date)
                            } else {
                                format!("ECB {}", cache.date)
                            };
                            Some((out, meta))
                        })
                    });
                    (converted, stale)
                }
            }
        };
        if stale {
            self.schedule_background_refresh();
        }
        converted
    }

    /// Fire-and-forget HTTP refresh. Coalesced + backoff so typing FX queries
    /// cannot spawn a worker storm when offline.
    fn schedule_background_refresh(&self) {
        let now = now_secs();
        let last = self.shared.last_attempt_secs.load(Ordering::Relaxed);
        // Always allow first attempt (last==0). After that, back off.
        if last != 0 && now.saturating_sub(last) < REFRESH_BACKOFF_SECS {
            return;
        }
        if self
            .shared
            .inflight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        // Reserve the attempt slot before spawn so concurrent converts don't all pass backoff.
        self.shared.last_attempt_secs.store(now, Ordering::Relaxed);

        let shared = self.shared.clone();
        thread::spawn(move || {
            if let Some(c) = fetch_rates() {
                save_disk(&c);
                *shared.cache.write().unwrap_or_else(|p| p.into_inner()) = Some(c);
            }
            shared.inflight.store(false, Ordering::Release);
        });
    }
}

impl Default for FxStore {
    fn default() -> Self {
        Self::new()
    }
}

fn rate_vs_base(cache: &RatesCache, code: &str) -> Option<f64> {
    if code == cache.base {
        return Some(1.0);
    }
    cache.rates.get(code).copied()
}

/// Apply ECB cross rate; reject zero / non-finite inputs so UI never shows inf/NaN.
fn convert_amount(amount: f64, from_rate: f64, to_rate: f64) -> Option<f64> {
    if !amount.is_finite() || !from_rate.is_finite() || !to_rate.is_finite() || from_rate == 0.0 {
        return None;
    }
    let out = (amount / from_rate) * to_rate;
    if out.is_finite() {
        Some(out)
    } else {
        None
    }
}

/// Normalize currency token (code or symbol) → ISO code
pub fn normalize_currency(token: &str) -> Option<&'static str> {
    let t = token.trim();
    // symbols
    match t {
        "$" | "💵" => return Some("USD"),
        "€" => return Some("EUR"),
        "£" => return Some("GBP"),
        "¥" | "￥" => return Some("JPY"),
        "₹" => return Some("INR"),
        "₩" => return Some("KRW"),
        "₽" => return Some("RUB"),
        "₺" => return Some("TRY"),
        "A$" | "AU$" => return Some("AUD"),
        "C$" | "CA$" => return Some("CAD"),
        _ => {}
    }
    let u = t.to_uppercase();
    match u.as_str() {
        "USD" | "DOLLAR" | "DOLLARS" | "BUCK" | "BUCKS" => Some("USD"),
        "EUR" | "EURO" | "EUROS" => Some("EUR"),
        "GBP" | "POUND" | "POUNDS" | "STERLING" => Some("GBP"),
        "INR" | "RUPEE" | "RUPEES" | "RS" => Some("INR"),
        "JPY" | "YEN" => Some("JPY"),
        "CNY" | "RMB" | "YUAN" => Some("CNY"),
        "AUD" => Some("AUD"),
        "CAD" => Some("CAD"),
        "CHF" => Some("CHF"),
        "HKD" => Some("HKD"),
        "SGD" => Some("SGD"),
        "KRW" | "WON" => Some("KRW"),
        "MXN" => Some("MXN"),
        "BRL" | "REAL" | "REAIS" => Some("BRL"),
        "ZAR" | "RAND" => Some("ZAR"),
        "SEK" | "NOK" | "DKK" | "PLN" | "TRY" | "RUB" | "AED" | "SAR" | "THB" | "IDR" | "PHP"
        | "MYR" | "NZD" | "TWD" | "ILS" | "CZK" | "HUF" | "RON" | "BGN" | "ISK" => {
            // return leaked static via match arms for known 3-letter
            match u.as_str() {
                "SEK" => Some("SEK"),
                "NOK" => Some("NOK"),
                "DKK" => Some("DKK"),
                "PLN" => Some("PLN"),
                "TRY" => Some("TRY"),
                "RUB" => Some("RUB"),
                "AED" => Some("AED"),
                "SAR" => Some("SAR"),
                "THB" => Some("THB"),
                "IDR" => Some("IDR"),
                "PHP" => Some("PHP"),
                "MYR" => Some("MYR"),
                "NZD" => Some("NZD"),
                "TWD" => Some("TWD"),
                "ILS" => Some("ILS"),
                "CZK" => Some("CZK"),
                "HUF" => Some("HUF"),
                "RON" => Some("RON"),
                "BGN" => Some("BGN"),
                "ISK" => Some("ISK"),
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn is_currency(token: &str) -> bool {
    normalize_currency(token).is_some()
}

pub fn format_money(amount: f64, code: &str) -> String {
    let zero_dec = matches!(code, "JPY" | "KRW" | "VND" | "IDR" | "CLP");
    if zero_dec {
        format!("{:.0} {code}", amount.round())
    } else {
        format!("{amount:.2} {code}")
    }
}

fn fetch_rates() -> Option<RatesCache> {
    // Frankfurter: latest EUR-based rates (in-process HTTP — no curl spawn).
    // Background agent: first DNS resolution in a fresh process can take 5s+,
    // which the fast UI request agent would abort.
    let body =
        crate::providers::http::get_bytes_background("https://api.frankfurter.dev/v1/latest")
            .ok()?;
    parse_rates_body(&body)
}

/// Parse Frankfurter `latest` response into a [`RatesCache`].
/// Returns None on malformed JSON / missing fields / non-finite rates.
fn parse_rates_body(body: &[u8]) -> Option<RatesCache> {
    #[derive(Deserialize)]
    struct Api {
        base: String,
        date: String,
        rates: HashMap<String, f64>,
    }
    let api: Api = serde_json::from_slice(body).ok()?;
    if !api.rates.values().all(|r| r.is_finite() && *r != 0.0) {
        return None;
    }
    Some(RatesCache {
        base: api.base,
        date: api.date,
        rates: api.rates,
        fetched_at: now_secs(),
    })
}

fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("hark/fx-rates.json")
}

fn load_disk() -> Option<RatesCache> {
    let data = fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&data).ok()
}

// O_NOFOLLOW open(2) bits (std does not expose them; libc is not a dependency).
// Other unix targets fall back to 0 — the directory ownership guard below
// still prevents an attacker from planting entries in the cache dir.
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400_000;
#[cfg(any(target_os = "macos", target_os = "ios"))]
const O_NOFOLLOW: i32 = 0o400;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
const O_NOFOLLOW: i32 = 0;

/// Effective UID of this process, parsed from procfs (`Uid:` line, 2nd field).
#[cfg(unix)]
fn current_euid() -> Option<u32> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

fn save_disk(c: &RatesCache) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // Fallback paths live under shared temp space where a local
            // attacker may have pre-created the directory or planted a
            // symlinked cache file. Force 0700, then refuse to persist
            // through any directory we do not own.
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            let trusted = fs::metadata(parent).is_ok_and(|m| {
                m.is_dir()
                    && m.mode() & 0o777 == 0o700
                    && m.uid() == current_euid().unwrap_or(u32::MAX)
            });
            if !trusted {
                eprintln!("hark: fx cache dir untrusted, skipping disk persistence");
                return;
            }
        }
    }
    if let Ok(data) = serde_json::to_string(c) {
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            // O_NOFOLLOW makes the kernel refuse a symlinked cache file.
            let _ = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .custom_flags(O_NOFOLLOW)
                .open(&path)
                .and_then(|mut f| f.write_all(data.as_bytes()));
        }
        #[cfg(not(unix))]
        {
            let _ = fs::write(&path, data);
        }
    }
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

    #[test]
    fn convert_uses_disk_without_network_when_present() {
        // If disk cache exists from the machine, convert must return Some quickly.
        // This does not assert network; only that convert does not require success.
        let store = FxStore::new();
        if store
            .shared
            .cache
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            let r = store.convert(100.0, "USD", "EUR");
            assert!(r.is_some(), "stale disk cache should still convert");
        }
    }

    #[test]
    fn convert_uses_injected_rates_without_network() {
        // EUR base, USD 1.1 → 100 USD = 90.91 EUR; GBP 0.9 cross-rate.
        let mut rates = HashMap::new();
        rates.insert("USD".into(), 1.1);
        rates.insert("GBP".into(), 0.9);
        let store = FxStore::with_cache("EUR", "2026-08-05", rates);

        let (out, meta) = store.convert(100.0, "USD", "EUR").unwrap();
        assert!((out - 90.9090).abs() < 1e-3);
        assert!(meta.contains("EUR") || meta.contains("ECB"), "meta: {meta}");

        // Cross rate USD → GBP: (100 / 1.1) * 0.9 ≈ 81.818
        let (out, _) = store.convert(100.0, "USD", "GBP").unwrap();
        assert!((out - 81.8181).abs() < 1e-3);

        // Same currency short-circuits.
        let (out, _) = store.convert(100.0, "EUR", "EUR").unwrap();
        assert_eq!(out, 100.0);
    }

    #[test]
    fn convert_missing_currency_returns_none() {
        let mut rates = HashMap::new();
        rates.insert("USD".into(), 1.1);
        let store = FxStore::with_cache("EUR", "2026-08-05", rates);
        assert!(store.convert(1.0, "USD", "JPY").is_none());
        assert!(store.convert(1.0, "JPY", "USD").is_none());
    }

    #[test]
    fn convert_supports_lowercase_codes() {
        let mut rates = HashMap::new();
        rates.insert("USD".into(), 1.1);
        let store = FxStore::with_cache("EUR", "2026-08-05", rates);
        let (out, _) = store.convert(100.0, "usd", "eur").unwrap();
        assert!((out - 90.9090).abs() < 1e-3);
    }

    #[test]
    fn parse_rates_body_ok() {
        let body = br#"{"base":"EUR","date":"2026-08-05","rates":{"USD":1.1,"GBP":0.9}}"#;
        let c = parse_rates_body(body).unwrap();
        assert_eq!(c.base, "EUR");
        assert_eq!(c.date, "2026-08-05");
        assert_eq!(c.rates["USD"], 1.1);
    }

    #[test]
    fn parse_rates_body_rejects_corrupt() {
        // Malformed JSON.
        assert!(parse_rates_body(b"not json").is_none());
        // Missing base/date.
        assert!(parse_rates_body(br#"{"rates":{"USD":1.1}}"#).is_none());
        // Zero / non-finite rates would poison conversions — reject them.
        assert!(parse_rates_body(br#"{"base":"EUR","date":"d","rates":{"USD":0.0}}"#).is_none());
    }

    #[test]
    fn convert_amount_rejects_zero_and_non_finite_rates() {
        assert_eq!(convert_amount(100.0, 1.0, 0.9), Some(90.0));
        assert!(convert_amount(100.0, 0.0, 0.9).is_none());
        assert!(convert_amount(100.0, f64::NAN, 0.9).is_none());
        assert!(convert_amount(100.0, 1.0, f64::INFINITY).is_none());
        assert!(convert_amount(f64::NAN, 1.0, 0.9).is_none());
        // Extreme values that overflow to inf.
        assert!(convert_amount(f64::MAX, 1e-300, f64::MAX).is_none());
    }
}
