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

    /// Convert using memory/disk rates only. Never blocks on network.
    /// Stale rates still convert; a background refresh is scheduled when needed.
    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Option<(f64, String)> {
        let from = from.to_uppercase();
        let to = to.to_uppercase();
        if from == to {
            return Some((amount, "same currency".into()));
        }

        if self.is_stale() {
            self.schedule_background_refresh();
        }

        let g = self.shared.cache.read().unwrap();
        let cache = g.as_ref()?;
        let from_rate = rate_vs_base(cache, &from)?;
        let to_rate = rate_vs_base(cache, &to)?;
        let in_base = amount / from_rate;
        let out = in_base * to_rate;
        let age = now_secs().saturating_sub(cache.fetched_at);
        let meta = if age > TTL_SECS {
            format!("ECB {} · stale cache", cache.date)
        } else if age > 300 {
            format!("ECB {} · cached", cache.date)
        } else {
            format!("ECB {}", cache.date)
        };
        Some((out, meta))
    }

    fn is_stale(&self) -> bool {
        let g = self.shared.cache.read().unwrap();
        match g.as_ref() {
            None => true,
            Some(c) => now_secs().saturating_sub(c.fetched_at) > TTL_SECS,
        }
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
        self.shared
            .last_attempt_secs
            .store(now, Ordering::Relaxed);

        let shared = self.shared.clone();
        thread::spawn(move || {
            if let Some(c) = fetch_rates() {
                save_disk(&c);
                *shared.cache.write().unwrap() = Some(c);
            }
            shared.inflight.store(false, Ordering::Release);
        });
    }
}

fn rate_vs_base(cache: &RatesCache, code: &str) -> Option<f64> {
    if code == cache.base {
        return Some(1.0);
    }
    cache.rates.get(code).copied()
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
        "SEK" | "NOK" | "DKK" | "PLN" | "TRY" | "RUB" | "AED" | "SAR" | "THB"
        | "IDR" | "PHP" | "MYR" | "NZD" | "TWD" | "ILS" | "CZK" | "HUF" | "RON"
        | "BGN" | "ISK" => {
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
    let body = crate::providers::http::get_bytes("https://api.frankfurter.dev/v1/latest").ok()?;
    #[derive(Deserialize)]
    struct Api {
        base: String,
        date: String,
        rates: HashMap<String, f64>,
    }
    let api: Api = serde_json::from_slice(&body).ok()?;
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
        .join("blink/fx-rates.json")
}

fn load_disk() -> Option<RatesCache> {
    let data = fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_disk(c: &RatesCache) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string(c) {
        let _ = fs::write(path, data);
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
        if store.shared.cache.read().unwrap().is_some() {
            let r = store.convert(100.0, "USD", "EUR");
            assert!(r.is_some(), "stale disk cache should still convert");
        }
    }
}
