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
    let value: f64 = caps.get(1)?.as_str().parse().ok()?;
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
    let value: f64 = caps.get(1)?.as_str().parse().ok()?;
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
        // Suggest a sensible default different from source
        return Some(if from == "USD" { "EUR" } else { "USD" });
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
