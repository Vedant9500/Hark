use crate::providers::{Action, ResultKind, SearchResult};

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


pub(crate) fn result_calc(title: String, subtitle: String, copy: String) -> SearchResult {
    SearchResult {
        id: format!("calc:{title}:{subtitle}"),
        title,
        subtitle,
        kind: ResultKind::Calc,
        score: 10_000,
        icon: Some("accessories-calculator".into()),
        action: Action::Copy(copy),
        conversion: None,
    }
}
