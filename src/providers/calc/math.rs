use super::util::{card_result, format_number};
use crate::providers::{Action, ConversionView, ResultKind, SearchResult};
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) static RE_MATHISH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\d\)\]]\s*[\+\-\*/%\^]|[\+\-\*/%\^]\s*[\d\(]|[\d\.]+\s*!").unwrap());

/// `5k`, `1.5 million`, `2 billion` — magnitude scales used as calc input.
/// Single `m`/`b`/`t` are excluded: they read as meters/bytes/tonnes.
pub(crate) static RE_MAGNITUDE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)\d(?:[\d.]*)\s*(?:k|bn|tn|mil|",
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
    // Show interpreted grouping when it differs from the input (`-2^2` →
    // `-(2^2)`); whitespace-only differences keep the user's own spelling.
    let explained = super::expr::explain_str(&expr).unwrap_or_else(|| display_expr.clone());
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let shown = if squeeze(&explained) == squeeze(&display_expr) {
        display_expr.clone()
    } else {
        explained
    };
    Some(SearchResult {
        id: format!("calc:{formatted}:{display_expr}"),
        title: formatted.clone(),
        subtitle: format!("= {shown}"),
        kind: ResultKind::Calc,
        score: 10_000,
        icon: Some("accessories-calculator".into()),
        action: Action::Copy(formatted.clone()),
        conversion: Some(ConversionView {
            left_title: shown,
            left_badge: "expression".into(),
            right_title: formatted,
            right_badge: "result".into(),
        }),
        matched: None,
    })
}

pub(crate) fn try_natural(q: &str) -> Option<SearchResult> {
    let lower = q.to_lowercase();

    // Numbers may include magnitude suffixes (`10% of 1.5 million`,
    // `tip 15% on 2k`). Single m/b/t are units, not magnitudes.
    static RE_PCT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*([+-]?\d+(?:\.\d+)?(?:\s*(?:k|bn|tn|mil|",
            r"hundreds?|thousands?|millions?|billions?|trillions?|",
            r"lakhs?|lacs?|crores?)?)?)\s*%\s*of\s*",
            r"([+-]?\d+(?:\.\d+)?(?:\s*(?:k|bn|tn|mil|",
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
        let shown = format!("{}% of {}", c.get(1)?.as_str(), c.get(2)?.as_str());
        return Some(card_result(
            formatted.clone(),
            shown.clone(),
            formatted.clone(),
            shown,
            "percentage",
            formatted,
            "result",
        ));
    }

    static RE_TIP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(concat!(
            r"(?i)^\s*tip\s+([+-]?\d+(?:\.\d+)?(?:\s*(?:k|bn|tn|mil|",
            r"hundreds?|thousands?|millions?|billions?|trillions?|",
            r"lakhs?|lacs?|crores?)?)?)\s*%\s*(?:on|for)\s*",
            r"([+-]?\d+(?:\.\d+)?(?:\s*(?:k|bn|tn|mil|",
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
        let title = format!("Total {formatted}");
        let shown = format!("Tip {}% on {}", c.get(1)?.as_str(), c.get(2)?.as_str());
        return Some(card_result(
            title.clone(),
            format!("Tip {} on {}", format_number(tip), c.get(2)?.as_str()),
            formatted,
            shown,
            "tip",
            title,
            "result",
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
    let subtitle = format!("{original} → dec {v} · hex 0x{v:X} · bin 0b{v:b}");
    card_result(
        title.clone(),
        subtitle,
        title.clone(),
        original.to_string(),
        "base",
        title,
        "dec",
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
        let r = try_math("1.5 million").expect("math");
        assert_eq!(r.title, "1500000");
        // Single m/b/t are meters/bytes/tonnes, not magnitudes.
        assert!(try_math("1.5m").is_none());
        assert!(try_math("100m / 2").is_none());
        assert!(try_math("1m * 3").is_none());
        assert!(try_math("2t / 4").is_none());
    }

    #[test]
    fn try_math_shows_interpreted_grouping() {
        let r = try_math("-2^2").expect("math");
        assert!(r.subtitle.contains("-(2^2)"), "subtitle: {}", r.subtitle);
        let conv = r.conversion.expect("card");
        assert_eq!(conv.left_title, "-(2^2)");
        // Id stays built from the raw input.
        assert!(r.id.ends_with(":-2^2"));
        // Whitespace-only difference keeps the user's own spelling.
        let r = try_math("2+2").expect("math");
        assert_eq!(r.subtitle, "= 2+2");
    }

    #[test]
    fn percent_of_with_suffix() {
        let r = try_natural("10% of 2k").expect("natural");
        assert_eq!(r.title, "200");
    }

    #[test]
    fn natural_results_render_cards() {
        let r = try_natural("50% of 100").expect("natural");
        let conv = r.conversion.expect("card");
        assert_eq!(conv.left_title, "50% of 100");
        assert_eq!(conv.left_badge, "percentage");
        assert_eq!(conv.right_badge, "result");

        let r = try_natural("tip 15% on 2k").expect("natural");
        let conv = r.conversion.expect("card");
        assert_eq!(conv.left_title, "Tip 15% on 2k");
        assert_eq!(conv.left_badge, "tip");
        assert_eq!(conv.right_title, "Total 2300");
        assert_eq!(r.title, "Total 2300");

        let r = try_natural("0x1f").expect("natural");
        let conv = r.conversion.expect("card");
        assert_eq!(conv.left_title, "0x1f");
        assert_eq!(conv.left_badge, "base");
        assert_eq!(conv.right_title, "31");
        assert_eq!(conv.right_badge, "dec");

        let r = try_natural("0b1010").expect("natural");
        let conv = r.conversion.expect("card");
        assert_eq!(conv.right_title, "10");
        assert_eq!(conv.right_badge, "dec");
    }
}
