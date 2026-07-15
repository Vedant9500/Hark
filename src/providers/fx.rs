use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

const TTL_SECS: u64 = 12 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RatesCache {
    base: String,
    date: String,
    rates: HashMap<String, f64>,
    fetched_at: u64,
}

pub struct FxStore {
    cache: RwLock<Option<RatesCache>>,
}

impl FxStore {
    pub fn new() -> Self {
        let store = Self {
            cache: RwLock::new(load_disk()),
        };
        store
    }

    /// Force-check rates (network if stale). Not called at daemon boot —
    /// `convert` refreshes lazily for battery life.
    #[allow(dead_code)]
    pub fn ensure_fresh(&self) {
        if self.is_stale() {
            if let Some(c) = fetch_rates() {
                save_disk(&c);
                *self.cache.write().unwrap() = Some(c);
            }
        }
    }

    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Option<(f64, String)> {
        let from = from.to_uppercase();
        let to = to.to_uppercase();
        if from == to {
            return Some((amount, "same currency".into()));
        }

        // Try memory, then disk, then fetch
        if self.is_stale() {
            if let Some(c) = fetch_rates() {
                save_disk(&c);
                *self.cache.write().unwrap() = Some(c);
            }
        }

        let g = self.cache.read().unwrap();
        let cache = g.as_ref()?;
        let from_rate = rate_vs_base(cache, &from)?;
        let to_rate = rate_vs_base(cache, &to)?;
        // amount in FROM → base → TO
        let in_base = amount / from_rate;
        let out = in_base * to_rate;
        let meta = if now_secs().saturating_sub(cache.fetched_at) > 300 {
            format!("ECB {} · cached", cache.date)
        } else {
            format!("ECB {}", cache.date)
        };
        Some((out, meta))
    }

    fn is_stale(&self) -> bool {
        let g = self.cache.read().unwrap();
        match g.as_ref() {
            None => true,
            Some(c) => now_secs().saturating_sub(c.fetched_at) > TTL_SECS,
        }
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
    // Frankfurter: latest EUR-based rates
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "3",
            "https://api.frankfurter.dev/v1/latest",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    #[derive(Deserialize)]
    struct Api {
        base: String,
        date: String,
        rates: HashMap<String, f64>,
    }
    let api: Api = serde_json::from_slice(&out.stdout).ok()?;
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
    if let Ok(data) = serde_json::to_string_pretty(c) {
        let _ = fs::write(path, data);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
