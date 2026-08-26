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

pub(crate) fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let abs = v.abs();
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
    use super::parse_qty_number;

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
