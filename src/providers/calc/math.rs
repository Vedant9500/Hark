use super::util::{format_number, result_calc};
use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) static RE_MATHISH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\d\)\]]\s*[\+\-\*/%\^]|[\+\-\*/%\^]\s*[\d\(]|[\d\.]+\s*!").unwrap());

/// `5k`, `1.5m`, `2 billion` — magnitude scales used as calc input.
pub(crate) static RE_MAGNITUDE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)\d(?:[\d.]*)\s*(?:k|m|b|t|bn|tn|mil|",
        r"hundreds?|thousands?|millions?|billions?|trillions?|",
        r"lakhs?|lacs?|crores?)\b",
    ))
    .unwrap()
});

pub(crate) fn looks_like_math(q: &str) -> bool {
    if q.chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == ' ')
    {
        return false;
    }
    RE_MATHISH.is_match(q)
        || RE_MAGNITUDE.is_match(q)
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
    let expr = expr.replace('×', "*").replace('÷', "/");
    let expr = expr.replace("π", "pi");
    // Accept both ^ and **; evaluator treats both as power.
    let value = super::expr::eval_str(&expr)?;
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

    // Numbers may include magnitude suffixes (`10% of 1.5m`, `tip 15% on 2k`).
    static RE_PCT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*([+-]?\d+(?:\.\d+)?(?:\s*(?:k|m|b|t|bn|tn|mil|",
            r"hundreds?|thousands?|millions?|billions?|trillions?|",
            r"lakhs?|lacs?|crores?)?)?)\s*%\s*of\s*",
            r"([+-]?\d+(?:\.\d+)?(?:\s*(?:k|m|b|t|bn|tn|mil|",
            r"hundreds?|thousands?|millions?|billions?|trillions?|",
            r"lakhs?|lacs?|crores?)?)?)\s*$",
        ))
        .unwrap()
    });
    if let Some(c) = RE_PCT.captures(&lower) {
        let a = super::expr::eval_str(c.get(1)?.as_str())?;
        let b = super::expr::eval_str(c.get(2)?.as_str())?;
        let v = a / 100.0 * b;
        let formatted = format_number(v);
        return Some(result_calc(
            formatted.clone(),
            format!("{}% of {}", c.get(1)?.as_str(), c.get(2)?.as_str()),
            formatted,
        ));
    }

    static RE_TIP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*tip\s+([+-]?\d+(?:\.\d+)?(?:\s*(?:k|m|b|t|bn|tn|mil|",
            r"hundreds?|thousands?|millions?|billions?|trillions?|",
            r"lakhs?|lacs?|crores?)?)?)\s*%\s*(?:on|for)\s*",
            r"([+-]?\d+(?:\.\d+)?(?:\s*(?:k|m|b|t|bn|tn|mil|",
            r"hundreds?|thousands?|millions?|billions?|trillions?|",
            r"lakhs?|lacs?|crores?)?)?)\s*$",
        ))
        .unwrap()
    });
    if let Some(c) = RE_TIP.captures(&lower) {
        let pct = super::expr::eval_str(c.get(1)?.as_str())?;
        let bill = super::expr::eval_str(c.get(2)?.as_str())?;
        let tip = bill * pct / 100.0;
        let total = bill + tip;
        let formatted = format_number(total);
        return Some(result_calc(
            format!("Total {formatted}"),
            format!("Tip {} on {}", format_number(tip), c.get(2)?.as_str()),
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

#[cfg(test)]
mod math_gate_tests {
    use super::{looks_like_math, try_math, try_natural};

    #[test]
    fn magnitude_queries_are_math() {
        assert!(looks_like_math("5k"));
        assert!(looks_like_math("1.5 million"));
        assert!(looks_like_math("2k + 3m"));
        assert!(!looks_like_math("42"));
    }

    #[test]
    fn try_math_magnitude() {
        let r = try_math("5k + 2.5k").expect("math");
        assert_eq!(r.title, "7500");
        let r = try_math("1.5m").expect("math");
        assert_eq!(r.title, "1500000");
    }

    #[test]
    fn percent_of_with_suffix() {
        let r = try_natural("10% of 2k").expect("natural");
        assert_eq!(r.title, "200");
    }
}
