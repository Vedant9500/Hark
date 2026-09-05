//! Home-region defaults for bare conversions.
//!
//! `10usd` (no target) auto-converts to the user's home currency, `10miles`
//! to the home distance unit, etc. The region comes from the system timezone
//! (`/etc/localtime` symlink, `TZ` fallback) — no new dependencies, detected
//! once per process.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HomePrefs {
    /// ISO code bare currency amounts convert to (`10usd` → INR in India).
    pub currency: &'static str,
    /// Metric-first defaults (km, kg, l, km/h) vs US customary (mi, lb, gal, mph).
    pub metric: bool,
    /// Home temperature unit: `"c"` or `"f"`.
    pub temp: &'static str,
}

/// Cached process-wide home prefs (timezone doesn't change mid-process).
pub(crate) fn home_prefs() -> HomePrefs {
    static PREFS: OnceLock<HomePrefs> = OnceLock::new();
    *PREFS.get_or_init(|| prefs_for_zone(&local_zone_name()))
}

pub(crate) fn home_currency() -> &'static str {
    home_prefs().currency
}

pub(crate) fn home_uses_metric() -> bool {
    home_prefs().metric
}

pub(crate) fn home_temp_unit() -> &'static str {
    home_prefs().temp
}

fn local_zone_name() -> String {
    // /etc/localtime is a symlink into /usr/share/zoneinfo/<Area>/<City>.
    if let Ok(link) = std::fs::read_link("/etc/localtime") {
        let s = link.to_string_lossy();
        if let Some(pos) = s.find("zoneinfo/") {
            return s[pos + "zoneinfo/".len()..].to_string();
        }
    }
    std::env::var("TZ").unwrap_or_default()
}

pub(crate) fn prefs_for_zone(zone: &str) -> HomePrefs {
    let metric = !is_imperial_zone(zone);
    let temp = if is_us_zone(zone) { "f" } else { "c" };
    HomePrefs {
        currency: currency_for_zone(zone),
        metric,
        temp,
    }
}

fn is_us_zone(zone: &str) -> bool {
    const US: &[&str] = &[
        "America/New_York",
        "America/Detroit",
        "America/Chicago",
        "America/Denver",
        "America/Phoenix",
        "America/Boise",
        "America/Los_Angeles",
        "America/Anchorage",
        "America/Honolulu",
        "America/Juneau",
        "America/Sitka",
        "America/Yakutat",
        "America/Nome",
        "America/Adak",
        "America/Metlakatla",
        "America/Indiana",
        "America/Kentucky",
        "America/Menominee",
        "America/North_Dakota",
        "America/Shiprock",
        "Pacific/Honolulu",
        "US/",
    ];
    US.iter().any(|p| zone == *p || zone.starts_with(p))
}

fn is_imperial_zone(zone: &str) -> bool {
    // US customary plus the only other holdouts (Liberia, Myanmar).
    is_us_zone(zone) || zone == "Africa/Monrovia" || zone == "Asia/Yangon"
}

fn currency_for_zone(zone: &str) -> &'static str {
    // South Asia
    if zone == "Asia/Kolkata" {
        return "INR";
    }
    if zone == "Asia/Colombo" {
        return "LKR";
    }
    if zone == "Asia/Dhaka" {
        return "BDT";
    }
    if zone == "Asia/Kathmandu" {
        return "NPR";
    }
    if zone == "Asia/Karachi" {
        return "PKR";
    }
    // East / SE Asia
    if zone == "Asia/Tokyo" {
        return "JPY";
    }
    if zone.starts_with("Asia/Shanghai")
        || zone.starts_with("Asia/Chongqing")
        || zone.starts_with("Asia/Harbin")
        || zone.starts_with("Asia/Urumqi")
    {
        return "CNY";
    }
    if zone == "Asia/Seoul" {
        return "KRW";
    }
    if zone == "Asia/Singapore" {
        return "SGD";
    }
    if zone == "Asia/Hong_Kong" {
        return "HKD";
    }
    if zone == "Asia/Bangkok" {
        return "THB";
    }
    if zone == "Asia/Jakarta" || zone == "Asia/Makassar" || zone == "Asia/Jayapura" {
        return "IDR";
    }
    if zone == "Asia/Manila" {
        return "PHP";
    }
    if zone == "Asia/Dubai" {
        return "AED";
    }
    if zone == "Asia/Riyadh" {
        return "SAR";
    }
    if zone == "Asia/Jerusalem" {
        return "ILS";
    }
    // Europe
    if zone == "Europe/London" {
        return "GBP";
    }
    if zone == "Europe/Moscow" {
        return "RUB";
    }
    if zone == "Europe/Istanbul" {
        return "TRY";
    }
    if zone == "Europe/Kyiv" {
        return "UAH";
    }
    if zone.starts_with("Europe/") {
        return "EUR";
    }
    // Americas
    if zone.starts_with("America/Toronto")
        || zone.starts_with("America/Vancouver")
        || zone.starts_with("America/Edmonton")
        || zone.starts_with("America/Winnipeg")
        || zone.starts_with("America/Halifax")
        || zone.starts_with("Canada/")
    {
        return "CAD";
    }
    if zone.starts_with("America/Sao_Paulo")
        || zone.starts_with("America/Bahia")
        || zone.starts_with("America/Belem")
        || zone.starts_with("America/Fortaleza")
        || zone.starts_with("America/Recife")
    {
        return "BRL";
    }
    if zone.starts_with("America/Mexico_City")
        || zone.starts_with("America/Cancun")
        || zone.starts_with("America/Monterrey")
        || zone.starts_with("America/Tijuana")
    {
        return "MXN";
    }
    if zone.starts_with("America/Argentina") {
        return "ARS";
    }
    if zone == "America/Santiago" {
        return "CLP";
    }
    if zone == "America/Bogota" {
        return "COP";
    }
    if zone == "America/Lima" {
        return "PEN";
    }
    // Oceania / Africa / Middle East
    if zone.starts_with("Australia/") {
        return "AUD";
    }
    if zone == "Pacific/Auckland" {
        return "NZD";
    }
    if zone == "Africa/Cairo" {
        return "EGP";
    }
    if zone == "Africa/Johannesburg" {
        return "ZAR";
    }
    if zone == "Africa/Lagos" {
        return "NGN";
    }
    if zone == "Africa/Nairobi" {
        return "KES";
    }
    "USD"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn india_defaults() {
        let p = prefs_for_zone("Asia/Kolkata");
        assert_eq!(p.currency, "INR");
        assert!(p.metric);
        assert_eq!(p.temp, "c");
    }

    #[test]
    fn us_defaults() {
        let p = prefs_for_zone("America/New_York");
        assert_eq!(p.currency, "USD");
        assert!(!p.metric);
        assert_eq!(p.temp, "f");
    }

    #[test]
    fn europe_defaults() {
        let p = prefs_for_zone("Europe/Paris");
        assert_eq!(p.currency, "EUR");
        assert!(p.metric);
        assert_eq!(p.temp, "c");
    }

    #[test]
    fn unknown_defaults_to_usd_metric() {
        let p = prefs_for_zone("");
        assert_eq!(p.currency, "USD");
        assert!(p.metric);
    }
}
