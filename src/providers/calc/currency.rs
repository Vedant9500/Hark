use super::units::{RE_CONVERT, RE_CONVERT_PARTIAL};
use crate::providers::fx::{format_money, normalize_currency, FxStore};
use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use once_cell::sync::Lazy;
use regex::Regex;
use std::borrow::Cow;

/// Rewrite `$100` / `100$` into `100 USD …` when a currency symbol is present.
/// Fast path: no symbol → borrow the input (zero alloc on typical app/file queries).
pub(crate) fn normalize_money_query(q: &str) -> Cow<'_, str> {
    if !q
        .chars()
        .any(|c| matches!(c, '$' | '€' | '£' | '¥' | '₹' | '₩' | '₽'))
    {
        return Cow::Borrowed(q);
    }

    let mut s = q.to_string();
    // $100 / €50 / £20 / ₹1000 at start
    static RE_SYM: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)^\s*([$€£¥₹₩₽])\s*([+-]?\d+(?:\.\d+)?)\s*").unwrap());
    if let Some(c) = RE_SYM.captures(&s) {
        let sym = c.get(1).unwrap().as_str();
        let num = c.get(2).unwrap().as_str();
        if let Some(code) = normalize_currency(sym) {
            let rest = &s[c.get(0).unwrap().end()..];
            s = format!("{num} {code} {rest}");
        }
    }
    // 100$ → 100 usd
    static RE_SYM_AFTER: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)([+-]?\d+(?:\.\d+)?)\s*([$€£¥₹])\b").unwrap());
    if let Some(c) = RE_SYM_AFTER.captures(&s) {
        let num = c.get(1).unwrap().as_str();
        let sym = c.get(2).unwrap().as_str();
        if let Some(code) = normalize_currency(sym) {
            s = RE_SYM_AFTER
                .replace(&s, format!("{num} {code}"))
                .to_string();
        }
    }
    Cow::Owned(s)
}

/// Currency-shaped query for the search-bar mode icon:
/// `100 usd to inr`, `10 usd to in`, `50 eur as jpy` — the source unit is a
/// currency code/symbol.
pub(crate) fn looks_like_currency_query(q: &str) -> bool {
    [RE_CONVERT.captures(q), RE_CONVERT_PARTIAL.captures(q)]
        .into_iter()
        .flatten()
        .any(|caps| {
            caps.get(2)
                .is_some_and(|from| crate::providers::fx::is_currency(from.as_str()))
        })
}

pub(crate) fn try_currency(q: &str, fx: &FxStore) -> Option<SearchResult> {
    let caps = RE_CONVERT.captures(q)?;
    let value: f64 = super::util::parse_amount(caps.get(1)?.as_str())?;
    let from_raw = caps.get(2)?.as_str();
    let to_raw = caps.get(3)?.as_str();
    if to_raw.is_empty() {
        return None;
    }
    let from = normalize_currency(from_raw)?;
    let to = normalize_currency(to_raw)?;
    fx_result(value, from, to, fx)
}

pub(crate) fn try_currency_predict(q: &str, fx: &FxStore) -> Option<SearchResult> {
    let caps = RE_CONVERT_PARTIAL.captures(q)?;
    let value: f64 = super::util::parse_amount(caps.get(1)?.as_str())?;
    let from_raw = caps.get(2)?.as_str();
    let to_prefix = caps.get(4).map(|m| m.as_str()).unwrap_or("").trim();
    let from = normalize_currency(from_raw)?;
    // Exact already handled
    if !to_prefix.is_empty() && normalize_currency(to_prefix).is_some() {
        return None;
    }
    let to = predict_currency(to_prefix, from)?;
    if to == from && !to_prefix.is_empty() {
        return None;
    }
    fx_result(value, from, to, fx)
}

pub(crate) fn try_currency_home(q: &str, fx: &FxStore) -> Option<SearchResult> {
    // Bare `10usd` / `10 usd` (no target): convert to the home currency.
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*([+-]?\d+(?:\.\d+)?(?:/\d+)?(?:\s*(?:thousands?|millions?|billions?|trillions?|hundreds?|mil|bn|tn|lakh|lac|lacs|crore|crores|k|cr|crs))?)\s*([a-zA-Z]{3})\s*$")
            .unwrap()
    });
    let caps = RE.captures(q)?;
    let value = super::util::parse_amount(caps.get(1)?.as_str())?;
    let from = normalize_currency(caps.get(2)?.as_str())?;
    let home = super::home::home_currency();
    let to = if from == home {
        if home == "USD" {
            "EUR"
        } else {
            "USD"
        }
    } else {
        home
    };
    if to == from {
        return None;
    }
    let mut r = fx_result(value, from, to, fx)?;
    r.score = 10_400;
    Some(r)
}

pub(crate) fn fx_result(value: f64, from: &str, to: &str, fx: &FxStore) -> Option<SearchResult> {
    let (result, meta) = fx.convert(value, from, to)?;
    let title = format_money(result, to);
    Some(SearchResult {
        id: format!("fx:{value}:{from}:{to}"),
        title: title.clone(),
        subtitle: format!("{value} {from} → {to} · {meta}"),
        kind: ResultKind::Conversion,
        score: 10_500,
        icon: Some("accessories-calculator".into()),
        action: Action::Copy(title.clone()),
        conversion: Some(ConversionView {
            left_title: format!("{value} {from}"),
            left_badge: from.to_string(),
            right_title: title,
            right_badge: format!("{to} · {meta}"),
        }),
        matched: None,
    })
}

pub(crate) fn predict_currency(prefix: &str, from: &str) -> Option<&'static str> {
    const CODES: &[&str] = &[
        "USD", "EUR", "GBP", "INR", "JPY", "CNY", "AUD", "CAD", "CHF", "HKD", "SGD", "KRW", "MXN",
        "BRL", "ZAR", "SEK", "NOK", "DKK", "PLN", "TRY", "RUB", "AED", "SAR", "THB", "NZD", "TWD",
        "ILS",
    ];
    // Aliases for prediction
    const ALIASES: &[(&str, &str)] = &[
        ("dollar", "USD"),
        ("dollars", "USD"),
        ("euro", "EUR"),
        ("euros", "EUR"),
        ("pound", "GBP"),
        ("pounds", "GBP"),
        ("sterling", "GBP"),
        ("rupee", "INR"),
        ("rupees", "INR"),
        ("yen", "JPY"),
        ("yuan", "CNY"),
        ("won", "KRW"),
    ];
    let p = prefix.to_lowercase();
    if p.is_empty() {
        // Bare `10usd to` (no target): convert to the home currency
        // (`10usd` → INR in India), else fall back to USD.
        let home = super::home::home_currency();
        return Some(if from == home {
            if home == "USD" {
                "EUR"
            } else {
                "USD"
            }
        } else {
            home
        });
    }
    // Prefer alias prefix match (pou → pound → GBP)
    let mut alias_hits: Vec<(&str, &str)> = ALIASES
        .iter()
        .copied()
        .filter(|(a, _)| a.starts_with(&p) || p.starts_with(a))
        .collect();
    alias_hits.sort_by_key(|(a, _)| a.len());
    if let Some((_, code)) = alias_hits.first() {
        return Some(*code);
    }
    // ISO code prefix
    let mut code_hits: Vec<&&str> = CODES
        .iter()
        .filter(|c| c.to_lowercase().starts_with(&p))
        .collect();
    code_hits.sort_by_key(|c| c.len());
    code_hits.first().copied().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn bare_currency_converts_to_home() {
        // Region-independent: target is always the home currency (or the
        // USD/EUR fallback when the source already is home).
        let home = crate::providers::calc::home::home_currency();
        let mut rates = HashMap::new();
        rates.insert("USD".into(), 1.1);
        rates.insert("EUR".into(), 1.0);
        rates.insert(home.to_string(), 90.0);
        let store = crate::providers::fx::FxStore::with_cache("EUR", "2026-08-05", rates);
        let r = try_currency_home("10usd", &store).expect("bare usd");
        let badge = r.conversion.as_ref().unwrap().right_badge.clone();
        let expected = if home == "USD" { "EUR" } else { home };
        assert!(badge.starts_with(expected), "{badge}");
        // With an explicit target the home default stays out of the way.
        assert!(try_currency_home("10usd to eur", &store).is_none());
    }
}
