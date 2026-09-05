use crate::providers::{Action, ConversionView, ResultKind, SearchResult};

/// Parse a numeric literal that may be a fraction (`1/2`, `2/3`) or a plain
/// decimal. Used for fraction quantities like `1/2 cup to ml`.
pub(crate) fn parse_qty_number(s: &str) -> Option<f64> {
    if let Some((a, b)) = s.split_once('/') {
        let a: f64 = a.parse().ok()?;
        let b: f64 = b.trim().parse().ok()?;
        if !a.is_finite() || !b.is_finite() || b == 0.0 {
            return None;
        }
        let out = a / b;
        return out.is_finite().then_some(out);
    }
    let out: f64 = s.parse().ok()?;
    out.is_finite().then_some(out)
}

const MAG_SUF: &[(&str, f64)] = &[
    ("trillions", 1e12),
    ("trillion", 1e12),
    ("tn", 1e12),
    ("billions", 1e9),
    ("billion", 1e9),
    ("bn", 1e9),
    ("crores", 1e7),
    ("crore", 1e7),
    ("crs", 1e7),
    ("cr", 1e7),
    ("millions", 1e6),
    ("million", 1e6),
    ("mil", 1e6),
    ("lakhs", 1e5),
    ("lakh", 1e5),
    ("lacs", 1e5),
    ("lac", 1e5),
    ("thousands", 1e3),
    ("thousand", 1e3),
    ("k", 1e3),
    ("hundreds", 1e2),
    ("hundred", 1e2),
];

/// Split a trailing magnitude word off a numeric token (`10k`, `1.5 crore`).
/// Mirrors the finance amount words minus single letters that collide with
/// units (`m` = meters, so `10m` stays meters). Returns (number, multiplier).
/// Used by unit/currency conversion so `10k kg to lb` works like finance.
pub(crate) fn split_magnitude(s: &str) -> (&str, f64) {
    let (n, m, _) = split_magnitude_word(s);
    (n, m)
}

/// Like [`split_magnitude`], additionally returning the matched suffix word
/// (for un-splitting greedy lexes like `36 kmph` → `36 k` + `mph`).
pub(crate) fn split_magnitude_word(s: &str) -> (&str, f64, Option<&'static str>) {
    let t = s.trim_end();
    let low = t.to_ascii_lowercase();
    for (w, m) in MAG_SUF {
        if let Some(num) = low.strip_suffix(w) {
            // Boundary: char before the suffix must not be a letter, so unit
            // fragments can't be mistaken for magnitudes. Slicing `t` at an
            // ASCII boundary is always a char boundary.
            if num.chars().last().is_some_and(|c| !c.is_ascii_alphabetic()) {
                return (t[..num.len()].trim_end(), *m, Some(*w));
            }
        }
    }
    (s, 1.0, None)
}

/// Parse a conversion amount: magnitude suffix × fraction/decimal quantity.
/// Rejects non-finite and overflowed results.
pub(crate) fn parse_amount(s: &str) -> Option<f64> {
    let (num, mult) = split_magnitude(s);
    let v = parse_qty_number(num)? * mult;
    v.is_finite().then_some(v)
}

pub(crate) fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let abs = v.abs();
    // Tiny nonzero magnitudes collapse to "0" through the rounding path
    // below — render scientific instead (audit P3).
    if abs > 0.0 && abs < 1e-6 {
        return format!("{v:.2e}");
    }
    // Near-integer → whole number
    if abs < 1e15 && (v - v.round()).abs() < 1e-9 * abs.max(1.0) {
        return format!("{}", v.round() as i64);
    }
    // Scale decimals: large values need fewer places; tiny need more (cap 6).
    let places: i32 = if abs >= 1000.0 {
        2
    } else if abs >= 100.0 {
        3
    } else if abs >= 1.0 {
        4
    } else if abs >= 0.01 {
        6
    } else {
        8
    };
    let factor = 10f64.powi(places);
    let rounded = (v * factor).round() / factor;
    if (rounded - rounded.round()).abs() < 1e-12 && rounded.abs() < 1e15 {
        return format!("{}", rounded.round() as i64);
    }
    let s = match places {
        2 => format!("{rounded:.2}"),
        3 => format!("{rounded:.3}"),
        4 => format!("{rounded:.4}"),
        6 => format!("{rounded:.6}"),
        _ => format!("{rounded:.8}"),
    };
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s
    }
}

pub(crate) fn relative_secs(n: f64, unit: &str) -> Option<f64> {
    let u = unit.to_lowercase();
    let mult = match u.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
        "d" | "day" | "days" => 86400.0,
        "w" | "week" | "weeks" => 604800.0,
        "mo" | "month" | "months" => 2_629_746.0,
        "y" | "yr" | "yrs" | "year" | "years" => 31_556_952.0,
        _ => return None,
    };
    Some(n * mult)
}

pub(crate) fn card_result(
    title: String,
    subtitle: String,
    copy: String,
    left_title: String,
    left_badge: &'static str,
    right_title: String,
    right_badge: &'static str,
) -> SearchResult {
    SearchResult {
        id: format!("calc:{title}:{subtitle}"),
        title,
        subtitle,
        kind: ResultKind::Calc,
        score: 10_000,
        icon: Some("accessories-calculator".into()),
        action: Action::Copy(copy),
        conversion: Some(ConversionView {
            left_title,
            left_badge: left_badge.into(),
            right_title,
            right_badge: right_badge.into(),
        }),
        matched: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{format_number, parse_amount, parse_qty_number, split_magnitude};

    #[test]
    fn tiny_values_use_scientific() {
        // Audit P3 (Pass 18): sub-5e-9 magnitudes collapsed to "0".
        assert_eq!(format_number(5e-9), "5.00e-9");
        assert_eq!(format_number(-5e-9), "-5.00e-9");
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(1.5), "1.5");
    }

    #[test]
    fn magnitude_suffix_splits() {
        assert_eq!(split_magnitude("10k"), ("10", 1e3));
        assert_eq!(split_magnitude("1.5 crore"), ("1.5", 1e7));
        assert_eq!(split_magnitude("2cr"), ("2", 1e7));
        assert_eq!(split_magnitude("2 mil"), ("2", 1e6));
        // Meters and plain numbers pass through untouched.
        assert_eq!(split_magnitude("100 m"), ("100 m", 1.0));
        assert_eq!(split_magnitude("10"), ("10", 1.0));
        assert_eq!(parse_amount("10k"), Some(10_000.0));
        assert_eq!(parse_amount("1/2k"), Some(500.0));
        assert_eq!(parse_amount("1.5k"), Some(1_500.0));
    }

    #[test]
    fn parse_qty_number_rejects_non_finite() {
        assert!(parse_qty_number("NaN/1").is_none());
        assert!(parse_qty_number("1/NaN").is_none());
        assert!(parse_qty_number("inf/2").is_none());
        assert!(parse_qty_number("2/inf").is_none());
        assert!(parse_qty_number("1e309").is_none());
        assert_eq!(parse_qty_number("1/2"), Some(0.5));
        assert_eq!(parse_qty_number("2.5"), Some(2.5));
    }
}
