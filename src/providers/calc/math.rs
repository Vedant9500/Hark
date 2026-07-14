use super::util::{format_number, result_calc};
use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) static RE_MATHISH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\d\)\]]\s*[\+\-\*/%\^]|[\+\-\*/%\^]\s*[\d\(]|[\d\.]+\s*!").unwrap());

pub(crate) fn looks_like_math(q: &str) -> bool {
    if q.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ' ') {
        return false;
    }
    RE_MATHISH.is_match(q)
        || q.contains("sqrt")
        || q.contains("sin")
        || q.contains("cos")
        || q.contains("tan")
        || q.contains("log")
        || q.contains("pi")
        || q.starts_with('=')
}

pub(crate) fn try_math(q: &str) -> Option<SearchResult> {
    let expr = q.trim().trim_start_matches('=').trim();
    let expr = expr.replace('^', "**").replace('×', "*").replace('÷', "/");
    let expr = expr.replace("π", "pi").replace("log10(", "log(");
    let value = meval::eval_str(&expr).ok()?;
    let formatted = format_number(value);
    let display_expr = q.trim().trim_start_matches('=').trim().to_string();
    Some(SearchResult {
        id: format!("calc:{formatted}:{display_expr}"),
        title: formatted.clone(),
        subtitle: format!("= {display_expr}"),
        kind: ResultKind::Calc,
        score: 10_000,
        icon: Some("accessories-calculator".into()),
        action: Action::Copy(formatted.clone()),
        conversion: Some(ConversionView {
            left_title: display_expr,
            left_badge: "expression".into(),
            right_title: formatted,
            right_badge: "result".into(),
        }),
    })
}

pub(crate) fn try_natural(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase();

    static RE_PCT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*([+-]?\d+(?:\.\d+)?)\s*%\s*of\s*([+-]?\d+(?:\.\d+)?)\s*$").unwrap()
    });
    if let Some(c) = RE_PCT.captures(&lower) {
        let a: f64 = c.get(1)?.as_str().parse().ok()?;
        let b: f64 = c.get(2)?.as_str().parse().ok()?;
        let v = a / 100.0 * b;
        let formatted = format_number(v);
        return Some(result_calc(formatted.clone(), format!("{a}% of {b}"), formatted));
    }

    static RE_TIP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?i)^\s*tip\s+([+-]?\d+(?:\.\d+)?)\s*%\s*(?:on|for)\s*([+-]?\d+(?:\.\d+)?)\s*$",
        )
        .unwrap()
    });
    if let Some(c) = RE_TIP.captures(&lower) {
        let pct: f64 = c.get(1)?.as_str().parse().ok()?;
        let bill: f64 = c.get(2)?.as_str().parse().ok()?;
        let tip = bill * pct / 100.0;
        let total = bill + tip;
        let formatted = format_number(total);
        return Some(result_calc(
            format!("Total {formatted}"),
            format!("Tip {} on {bill}", format_number(tip)),
            formatted,
        ));
    }

    if let Some(rest) = lower.strip_prefix("0x") {
        if let Ok(v) = u64::from_str_radix(rest.trim(), 16) {
            return Some(base_result(v, q));
        }
    }
    if let Some(rest) = lower.strip_prefix("0b") {
        if let Ok(v) = u64::from_str_radix(rest.trim(), 2) {
            return Some(base_result(v, q));
        }
    }

    None
}

pub(crate) fn base_result(v: u64, original: &str) -> SearchResult {
    let title = format!("{v}");
    result_calc(
        title.clone(),
        format!("{original} → dec {v} · hex 0x{v:X} · bin 0b{v:b}"),
        title,
    )
}
